//! Control plane + signaling server.
//!
//! Phase 1 scope: in-memory device registry, password-gated sessions, consent
//! handshake and opaque WebRTC signal relay. Postgres-backed devices, OIDC and
//! mTLS enrollment arrive in Phase 3 (see docs/PLAN.md).

mod agent;
mod password;
mod ratelimit;
mod state;
mod viewer;

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    extract::{ConnectInfo, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use proto::IceServer;
use tower_http::{services::ServeDir, trace::TraceLayer};

pub use state::AppState;

/// Largest signaling frame accepted (SDP offers are typically < 10 KiB).
const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    pub ice_servers: Vec<IceServer>,
    /// Failed viewer logins allowed per IP within `rate_window`.
    pub max_failed_logins: usize,
    pub rate_window: Duration,
    /// Directory with the built web viewer (`web/dist`), served at `/`.
    pub web_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ice_servers: vec![IceServer {
                urls: vec!["stun:stun.l.google.com:19302".into()],
                username: None,
                credential: None,
            }],
            max_failed_logins: 5,
            rate_window: Duration::from_secs(300),
            web_dir: None,
        }
    }
}

pub fn app(config: Config) -> Router {
    let web_dir = config.web_dir.clone();
    let state = Arc::new(AppState::new(config));
    let mut router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws/agent", get(agent_ws))
        .route("/ws/viewer", get(viewer_ws))
        .with_state(state);
    if let Some(dir) = web_dir {
        router = router.fallback_service(ServeDir::new(dir));
    }
    router.layer(TraceLayer::new_for_http())
}

async fn agent_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.max_message_size(MAX_FRAME)
        .on_upgrade(move |socket| agent::handle(socket, state, addr))
}

async fn viewer_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.max_message_size(MAX_FRAME)
        .on_upgrade(move |socket| viewer::handle(socket, state, addr))
}

/// Bind and serve until the process is stopped.
pub async fn serve(listener: tokio::net::TcpListener, config: Config) -> std::io::Result<()> {
    axum::serve(
        listener,
        app(config).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// Shared helpers for sending typed JSON over an axum WebSocket.
pub(crate) mod wire {
    use std::time::Duration;

    use axum::extract::ws::{Message, WebSocket};
    use futures_util::{
        stream::{SplitSink, SplitStream},
        SinkExt, StreamExt,
    };
    use serde::{de::DeserializeOwned, Serialize};
    use tokio::sync::mpsc::UnboundedReceiver;

    pub type Tx = SplitSink<WebSocket, Message>;
    pub type Rx = SplitStream<WebSocket>;

    /// How long a fresh connection may take to send its first message.
    pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

    pub enum Incoming<T> {
        Msg(T),
        Invalid,
        Closed,
    }

    pub async fn send<T: Serialize>(tx: &mut Tx, msg: &T) -> bool {
        match serde_json::to_string(msg) {
            Ok(s) => tx.send(Message::Text(s.into())).await.is_ok(),
            Err(_) => false,
        }
    }

    pub async fn recv<T: DeserializeOwned>(rx: &mut Rx) -> Incoming<T> {
        loop {
            match rx.next().await {
                Some(Ok(Message::Text(t))) => {
                    return match serde_json::from_str(&t) {
                        Ok(m) => Incoming::Msg(m),
                        Err(_) => Incoming::Invalid,
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Ok(Message::Binary(_))) => return Incoming::Invalid,
                Some(Ok(Message::Close(_)) | Err(_)) | None => return Incoming::Closed,
            }
        }
    }

    pub async fn recv_hello<T: DeserializeOwned>(rx: &mut Rx) -> Incoming<T> {
        tokio::time::timeout(HELLO_TIMEOUT, recv(rx))
            .await
            .unwrap_or(Incoming::Closed)
    }

    /// Drains `rx` into the socket until the channel closes, the peer goes away, or
    /// `is_final` says the message just sent ends the conversation.
    pub async fn pump<T: Serialize>(
        mut tx: Tx,
        mut rx: UnboundedReceiver<T>,
        is_final: impl Fn(&T) -> bool,
    ) {
        while let Some(m) = rx.recv().await {
            if !send(&mut tx, &m).await || is_final(&m) {
                break;
            }
        }
        let _ = tx.close().await;
    }
}
