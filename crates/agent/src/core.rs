//! Frontend-independent agent logic: registration, reconnects, consent and session
//! bookkeeping. The terminal (`cli`) and menu-bar (`tray`) frontends both drive this.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHasher,
};
use futures_util::{future::BoxFuture, SinkExt, StreamExt};
use proto::{AgentInfo, AgentToServer, ErrorCode, ServerToAgent, SessionId, PROTOCOL_VERSION};
use rand::{distributions::Alphanumeric, Rng};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::config::{self, Identity};

pub const MIN_PASSWORD_LEN: usize = 8;

/// Snapshot of what the user should see.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub device_id: Option<String>,
    /// One-time password for this run; `None` once an unattended password is set.
    pub one_time_password: Option<String>,
    pub online: bool,
    pub session: Option<SessionStatus>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionStatus {
    pub peer: String,
    pub accepted: bool,
}

/// Sent by interactive frontends (currently only the macOS menu-bar app).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Command {
    EndSession,
    SetPassword(String),
}

pub trait Frontend: Send + Sync + 'static {
    fn status(&self, status: &Status);
    /// Ask the local user whether `peer` may start a session.
    fn consent(&self, peer: String) -> BoxFuture<'static, bool>;
}

pub struct Agent {
    pub server_url: String,
    pub cfg_path: PathBuf,
    pub identity: Identity,
    /// Skip the consent prompt (requires a saved password).
    pub auto_accept: bool,
    pub frontend: Arc<dyn Frontend>,
    password_hash: String,
    status: Status,
}

impl Agent {
    pub fn new(
        server_url: String,
        cfg_path: PathBuf,
        identity: Identity,
        auto_accept: bool,
        frontend: Arc<dyn Frontend>,
    ) -> anyhow::Result<Self> {
        let mut status = Status::default();
        // Without an unattended password, use a fresh one-time password per run.
        let password_hash = match &identity.password_hash {
            Some(h) => h.clone(),
            None => {
                let otp: String = rand::thread_rng()
                    .sample_iter(&Alphanumeric)
                    .take(10)
                    .map(char::from)
                    .collect();
                let h = hash_password(&otp)?;
                status.one_time_password = Some(otp);
                h
            }
        };
        status.device_id = identity.device_id.clone();
        Ok(Self {
            server_url,
            cfg_path,
            identity,
            auto_accept,
            frontend,
            password_hash,
            status,
        })
    }

    pub async fn run(mut self, mut commands: UnboundedReceiver<Command>) {
        self.publish();
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.run_once(&mut commands).await {
                Ok(()) => backoff = Duration::from_secs(1),
                Err(e) => {
                    warn!("connection error: {e:#}");
                    self.status.last_error = Some(format!("{e:#}"));
                }
            }
            self.status.online = false;
            self.status.session = None;
            self.publish();
            info!("reconnecting in {}s", backoff.as_secs());
            // Keep serving commands (e.g. a password change) while offline.
            let sleep = tokio::time::sleep(backoff);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    Some(cmd) = commands.recv() => {
                        if let Command::SetPassword(pw) = cmd {
                            if let Err(e) = self.set_password(&pw) {
                                self.status.last_error = Some(format!("{e:#}"));
                            }
                            self.publish();
                        }
                    }
                }
            }
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }

    fn publish(&self) {
        self.frontend.status(&self.status);
    }

    fn set_password(&mut self, pw: &str) -> anyhow::Result<()> {
        if pw.chars().count() < MIN_PASSWORD_LEN {
            bail!("password must be at least {MIN_PASSWORD_LEN} characters");
        }
        let hash = hash_password(pw)?;
        self.identity.password_hash = Some(hash.clone());
        config::save(&self.cfg_path, &self.identity)?;
        self.password_hash = hash;
        self.status.one_time_password = None;
        self.status.last_error = None;
        info!("unattended password saved");
        Ok(())
    }

    async fn run_once(&mut self, commands: &mut UnboundedReceiver<Command>) -> anyhow::Result<()> {
        let url = format!("{}/ws/agent", self.server_url.trim_end_matches('/'));
        let (ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        let (mut sink, mut stream) = ws.split();

        let register = AgentToServer::Register {
            protocol_version: PROTOCOL_VERSION,
            device_id: self.identity.device_id.clone(),
            device_token: self.identity.device_token.clone(),
            password_hash: self.password_hash.clone(),
            info: AgentInfo {
                hostname: hostname(),
                os: std::env::consts::OS.into(),
                agent_version: env!("CARGO_PKG_VERSION").into(),
            },
        };
        sink.send(Message::Text(serde_json::to_string(&register)?.into()))
            .await?;

        // Outbound queue so consent prompts never block the read loop.
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<AgentToServer>();
        let writer = tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                let Ok(s) = serde_json::to_string(&m) else {
                    continue;
                };
                if sink.send(Message::Text(s.into())).await.is_err() {
                    break;
                }
            }
        });
        let (decision_tx, mut decision_rx) = mpsc::unbounded_channel::<(SessionId, bool)>();

        let mut active: Option<SessionId> = None;
        let result = loop {
            tokio::select! {
                frame = stream.next() => {
                    let text = match frame {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Close(_))) | None => break Ok(()),
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => break Err(e.into()),
                    };
                    let msg: ServerToAgent = match serde_json::from_str(&text) {
                        Ok(m) => m,
                        Err(e) => {
                            warn!("ignoring unparseable server message: {e}");
                            continue;
                        }
                    };
                    if let Err(e) = self.on_server_message(msg, &mut active, &decision_tx) {
                        break Err(e);
                    }
                }
                Some((session_id, accept)) = decision_rx.recv() => {
                    if active == Some(session_id) {
                        let _ = out_tx.send(AgentToServer::SessionDecision { session_id, accept });
                        if accept {
                            if let Some(s) = self.status.session.as_mut() {
                                s.accepted = true;
                            }
                        } else {
                            active = None;
                            self.status.session = None;
                        }
                        self.publish();
                    }
                }
                Some(cmd) = commands.recv() => match cmd {
                    Command::EndSession => {
                        if let Some(session_id) = active.take() {
                            let _ = out_tx.send(AgentToServer::EndSession { session_id });
                            self.status.session = None;
                            self.publish();
                        }
                    }
                    Command::SetPassword(pw) => {
                        match self.set_password(&pw) {
                            // Re-register so the server uses the new password.
                            Ok(()) => break Ok(()),
                            Err(e) => {
                                self.status.last_error = Some(format!("{e:#}"));
                                self.publish();
                            }
                        }
                    }
                },
            }
        };
        writer.abort();
        result
    }

    fn on_server_message(
        &mut self,
        msg: ServerToAgent,
        active: &mut Option<SessionId>,
        decisions: &mpsc::UnboundedSender<(SessionId, bool)>,
    ) -> anyhow::Result<()> {
        match msg {
            ServerToAgent::Registered {
                device_id,
                device_token,
                ..
            } => {
                if self.identity.device_id.as_deref() != Some(&device_id)
                    || self.identity.device_token.as_deref() != Some(&device_token)
                {
                    self.identity.device_id = Some(device_id.clone());
                    self.identity.device_token = Some(device_token);
                    config::save(&self.cfg_path, &self.identity)?;
                }
                info!(device_id = %format_id(&device_id), "registered");
                self.status.device_id = Some(device_id);
                self.status.online = true;
                self.status.last_error = None;
                self.publish();
            }
            ServerToAgent::Error(e) => {
                if e.code == ErrorCode::DeviceIdTaken {
                    warn!("saved device ID is owned by another install; requesting a new one");
                    self.identity.device_id = None;
                    self.identity.device_token = None;
                    config::save(&self.cfg_path, &self.identity)?;
                }
                bail!("server error {:?}: {}", e.code, e.message);
            }
            ServerToAgent::SessionRequest {
                session_id,
                viewer_addr,
            } => {
                info!(%session_id, %viewer_addr, "incoming session request");
                *active = Some(session_id);
                self.status.session = Some(SessionStatus {
                    peer: viewer_addr.clone(),
                    accepted: false,
                });
                self.publish();
                let decisions = decisions.clone();
                let consent = if self.auto_accept {
                    info!("unattended mode: session accepted");
                    Box::pin(async { true })
                } else {
                    self.frontend.consent(viewer_addr)
                };
                tokio::spawn(async move {
                    let _ = decisions.send((session_id, consent.await));
                });
            }
            ServerToAgent::Signal {
                session_id,
                payload,
            } if *active == Some(session_id) => {
                // Phase 2: feed into the WebRTC peer connection.
                info!(%session_id, ?payload, "signal received (media pipeline not implemented yet)");
            }
            ServerToAgent::Signal { .. } => {}
            ServerToAgent::SessionEnded { session_id } => {
                info!(%session_id, "session ended");
                if *active == Some(session_id) {
                    *active = None;
                    self.status.session = None;
                    self.publish();
                }
            }
        }
        Ok(())
    }
}

pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(pw.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        })
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

pub fn format_id(id: &str) -> String {
    id.as_bytes()
        .chunks(3)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
}
