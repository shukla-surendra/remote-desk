use std::{net::SocketAddr, sync::Arc};

use axum::extract::ws::WebSocket;
use futures_util::StreamExt;
use proto::{AgentToServer, ErrorBody, ErrorCode, ServerToAgent, PROTOCOL_VERSION};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    password,
    state::AppState,
    wire::{self, Incoming},
};

pub async fn handle(socket: WebSocket, state: Arc<AppState>, addr: SocketAddr) {
    let (mut sink, mut stream) = socket.split();

    let (requested, password_hash, info) = match wire::recv_hello(&mut stream).await {
        Incoming::Msg(AgentToServer::Register {
            protocol_version,
            device_id,
            device_token,
            password_hash,
            info,
        }) => {
            if protocol_version != PROTOCOL_VERSION {
                let e = ErrorBody::new(
                    ErrorCode::UnsupportedVersion,
                    format!("server speaks protocol {PROTOCOL_VERSION}"),
                );
                wire::send(&mut sink, &ServerToAgent::Error(e)).await;
                return;
            }
            if let Err(why) = password::validate_hash(&password_hash) {
                let e = ErrorBody::new(ErrorCode::BadRequest, why);
                wire::send(&mut sink, &ServerToAgent::Error(e)).await;
                return;
            }
            (device_id.map(|id| (id, device_token)), password_hash, info)
        }
        _ => {
            let e = ErrorBody::new(ErrorCode::BadRequest, "expected register");
            wire::send(&mut sink, &ServerToAgent::Error(e)).await;
            return;
        }
    };

    let (tx, rx) = mpsc::unbounded_channel();
    let hostname = info.hostname.clone();
    let reg = match state.register_agent(requested, password_hash, info, tx) {
        Ok(r) => r,
        Err(code) => {
            let e = ErrorBody::new(code, "registration refused");
            wire::send(&mut sink, &ServerToAgent::Error(e)).await;
            return;
        }
    };
    let registered = ServerToAgent::Registered {
        device_id: reg.device_id.clone(),
        device_token: reg.device_token,
        ice_servers: state.config.ice_servers.clone(),
    };
    if !wire::send(&mut sink, &registered).await {
        state.unregister_agent(&reg.device_id, reg.conn_id);
        return;
    }
    info!(device_id = %reg.device_id, %hostname, %addr, "agent online");

    let mut writer = tokio::spawn(wire::pump(sink, rx, |_| false));
    let (id, conn) = (reg.device_id.as_str(), reg.conn_id);
    loop {
        tokio::select! {
            incoming = wire::recv::<AgentToServer>(&mut stream) => match incoming {
                Incoming::Msg(AgentToServer::SessionDecision { session_id, accept }) => {
                    info!(device_id = id, %session_id, accept, "consent decision");
                    state.agent_decision(id, conn, session_id, accept);
                }
                Incoming::Msg(AgentToServer::Signal { session_id, payload }) => {
                    state.agent_signal(id, conn, session_id, payload);
                }
                Incoming::Msg(AgentToServer::EndSession { session_id }) => {
                    state.agent_end(id, conn, session_id);
                }
                Incoming::Msg(AgentToServer::Register { .. }) | Incoming::Invalid => {
                    warn!(device_id = id, "protocol violation; closing");
                    break;
                }
                Incoming::Closed => break,
            },
            // Writer stops if the socket breaks or a newer connection took over.
            _ = &mut writer => break,
        }
    }
    state.unregister_agent(id, conn);
    writer.abort();
    info!(device_id = id, "agent offline");
}
