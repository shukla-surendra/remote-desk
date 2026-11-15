use std::{
    net::SocketAddr,
    sync::{Arc, OnceLock},
};

use axum::extract::ws::WebSocket;
use futures_util::StreamExt;
use proto::{ErrorBody, ErrorCode, ServerToViewer, ViewerToServer, PROTOCOL_VERSION};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    password,
    state::AppState,
    wire::{self, Incoming},
};

pub async fn handle(socket: WebSocket, state: Arc<AppState>, addr: SocketAddr) {
    let (mut sink, mut stream) = socket.split();
    let ip = addr.ip();

    macro_rules! fail {
        ($code:expr, $msg:expr) => {{
            let e = ErrorBody::new($code, $msg);
            wire::send(&mut sink, &ServerToViewer::Error(e)).await;
            return;
        }};
    }

    if state.limiter.is_limited(ip) {
        fail!(
            ErrorCode::RateLimited,
            "too many failed attempts; try again later"
        );
    }

    let (device_id, password) = match wire::recv_hello(&mut stream).await {
        Incoming::Msg(ViewerToServer::Connect {
            protocol_version,
            device_id,
            password,
        }) => {
            if protocol_version != PROTOCOL_VERSION {
                fail!(
                    ErrorCode::UnsupportedVersion,
                    format!("server speaks protocol {PROTOCOL_VERSION}")
                );
            }
            (device_id.trim().replace(' ', ""), password)
        }
        _ => fail!(ErrorCode::BadRequest, "expected connect"),
    };

    // Verify off the async runtime. Unknown devices are checked against a dummy hash so
    // response timing doesn't reveal which IDs are online.
    let target = state.login_target(&device_id);
    let hash = target
        .as_ref()
        .map(|t| t.password_hash.to_string())
        .unwrap_or_else(|| dummy_hash().to_owned());
    let ok = tokio::task::spawn_blocking(move || password::verify(&hash, &password))
        .await
        .unwrap_or(false);
    let Some(target) = target.filter(|_| ok) else {
        state.limiter.record_failure(ip);
        warn!(%ip, "viewer login failed");
        fail!(
            ErrorCode::InvalidCredentials,
            "invalid device ID or password"
        );
    };

    let (tx, rx) = mpsc::unbounded_channel();
    let session_id = match state.start_session(&device_id, &target, tx, ip.to_string()) {
        Ok(s) => s,
        Err(ErrorCode::DeviceBusy) => {
            fail!(ErrorCode::DeviceBusy, "device is already in a session")
        }
        Err(code) => fail!(code, "could not start session"),
    };
    info!(%device_id, %session_id, %ip, "session requested");
    if !wire::send(&mut sink, &ServerToViewer::AwaitingConsent { session_id }).await {
        state.viewer_left(&device_id, session_id);
        return;
    }

    let mut writer = tokio::spawn(wire::pump(sink, rx, |m| {
        matches!(
            m,
            ServerToViewer::SessionEnded { .. } | ServerToViewer::Error(_)
        )
    }));
    loop {
        tokio::select! {
            incoming = wire::recv::<ViewerToServer>(&mut stream) => match incoming {
                Incoming::Msg(ViewerToServer::Signal { payload }) => {
                    if !state.viewer_signal(&device_id, session_id, payload) {
                        break;
                    }
                }
                Incoming::Msg(ViewerToServer::EndSession) | Incoming::Closed => break,
                Incoming::Msg(ViewerToServer::Connect { .. }) | Incoming::Invalid => {
                    warn!(%session_id, "viewer protocol violation; closing");
                    break;
                }
            },
            _ = &mut writer => break,
        }
    }
    state.viewer_left(&device_id, session_id);
    writer.abort();
    info!(%device_id, %session_id, "session closed");
}

fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| {
        use argon2::{
            password_hash::{rand_core::OsRng, SaltString},
            Argon2, PasswordHasher,
        };
        Argon2::default()
            .hash_password(b"dummy", &SaltString::generate(&mut OsRng))
            .map(|h| h.to_string())
            .unwrap_or_default()
    })
}
