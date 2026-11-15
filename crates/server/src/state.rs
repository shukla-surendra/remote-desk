//! In-memory device registry and session bookkeeping.
//!
//! Every connection owns an unbounded mpsc channel drained by its WebSocket writer
//! task; the registry only holds senders, so no lock is held across an `.await`.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use proto::{
    AgentInfo, EndReason, ErrorCode, ServerToAgent, ServerToViewer, SessionId, SignalPayload,
};
use rand::{distributions::Alphanumeric, Rng};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use crate::{ratelimit::FailureLimiter, Config};

pub type AgentTx = UnboundedSender<ServerToAgent>;
pub type ViewerTx = UnboundedSender<ServerToViewer>;

pub struct AppState {
    pub config: Config,
    pub limiter: FailureLimiter,
    registry: Mutex<Registry>,
    next_conn: AtomicU64,
}

#[derive(Default)]
struct Registry {
    devices: HashMap<String, Device>,
}

struct Device {
    token: String,
    online: Option<OnlineAgent>,
}

struct OnlineAgent {
    conn_id: u64,
    tx: AgentTx,
    password_hash: Arc<str>,
    info: AgentInfo,
    session: Option<Session>,
}

struct Session {
    id: SessionId,
    viewer_tx: ViewerTx,
    accepted: bool,
}

pub struct Registration {
    pub device_id: String,
    pub device_token: String,
    pub conn_id: u64,
}

/// Credentials needed to check a viewer login without holding the lock.
pub struct LoginTarget {
    pub password_hash: Arc<str>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let limiter = FailureLimiter::new(config.max_failed_logins, config.rate_window);
        Self {
            config,
            limiter,
            registry: Mutex::new(Registry::default()),
            next_conn: AtomicU64::new(1),
        }
    }

    pub fn register_agent(
        &self,
        requested: Option<(String, Option<String>)>,
        password_hash: String,
        info: AgentInfo,
        tx: AgentTx,
    ) -> Result<Registration, ErrorCode> {
        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let mut reg = self.registry.lock().unwrap();

        let (device_id, token) = match requested {
            Some((id, token)) if is_valid_device_id(&id) => match reg.devices.get_mut(&id) {
                Some(dev) => {
                    // Known ID: only the holder of its token may reclaim it. A valid token
                    // also takes over a stale (half-open) connection.
                    if !token.as_deref().is_some_and(|t| ct_eq(t, &dev.token)) {
                        return Err(ErrorCode::DeviceIdTaken);
                    }
                    if let Some(old) = dev.online.take() {
                        end_session(old.session, &old.tx, EndReason::AgentDisconnected);
                    }
                    (id, dev.token.clone())
                }
                // Unknown ID (e.g. server restarted): adopt it along with its token.
                // Phase 3 replaces this with persistent enrollment.
                None => {
                    let token = token.filter(|t| t.len() >= 32).unwrap_or_else(new_token);
                    (id, token)
                }
            },
            Some(_) => return Err(ErrorCode::BadRequest),
            None => (new_device_id(&reg.devices), new_token()),
        };

        reg.devices.insert(
            device_id.clone(),
            Device {
                token: token.clone(),
                online: Some(OnlineAgent {
                    conn_id,
                    tx,
                    password_hash: password_hash.into(),
                    info,
                    session: None,
                }),
            },
        );
        Ok(Registration {
            device_id,
            device_token: token,
            conn_id,
        })
    }

    pub fn unregister_agent(&self, device_id: &str, conn_id: u64) {
        let mut reg = self.registry.lock().unwrap();
        if let Some(dev) = reg.devices.get_mut(device_id) {
            if dev.online.as_ref().is_some_and(|a| a.conn_id == conn_id) {
                let agent = dev.online.take().expect("checked above");
                end_session(agent.session, &agent.tx, EndReason::AgentDisconnected);
            }
        }
    }

    pub fn login_target(&self, device_id: &str) -> Option<LoginTarget> {
        let reg = self.registry.lock().unwrap();
        let agent = reg.devices.get(device_id)?.online.as_ref()?;
        Some(LoginTarget {
            password_hash: agent.password_hash.clone(),
        })
    }

    /// Opens a session after the password was verified against `target`. Fails if the
    /// agent reconnected (with a possibly different password) in the meantime.
    pub fn start_session(
        &self,
        device_id: &str,
        target: &LoginTarget,
        viewer_tx: ViewerTx,
        viewer_addr: String,
    ) -> Result<SessionId, ErrorCode> {
        let mut reg = self.registry.lock().unwrap();
        let agent = reg
            .devices
            .get_mut(device_id)
            .and_then(|d| d.online.as_mut())
            .filter(|a| Arc::ptr_eq(&a.password_hash, &target.password_hash))
            .ok_or(ErrorCode::InvalidCredentials)?;
        if agent.session.is_some() {
            return Err(ErrorCode::DeviceBusy);
        }
        let id = Uuid::new_v4();
        agent.session = Some(Session {
            id,
            viewer_tx,
            accepted: false,
        });
        let _ = agent.tx.send(ServerToAgent::SessionRequest {
            session_id: id,
            viewer_addr,
        });
        Ok(id)
    }

    pub fn agent_decision(
        &self,
        device_id: &str,
        conn_id: u64,
        session_id: SessionId,
        accept: bool,
    ) {
        let mut reg = self.registry.lock().unwrap();
        let Some(agent) = online_mut(&mut reg, device_id, conn_id) else {
            return;
        };
        let Some(s) = agent
            .session
            .as_mut()
            .filter(|s| s.id == session_id && !s.accepted)
        else {
            return;
        };
        if accept {
            s.accepted = true;
            let _ = s.viewer_tx.send(ServerToViewer::SessionStarted {
                session_id,
                ice_servers: self.config.ice_servers.clone(),
                info: agent.info.clone(),
            });
        } else {
            let s = agent.session.take().expect("checked above");
            let _ = s.viewer_tx.send(ServerToViewer::SessionEnded {
                reason: EndReason::Rejected,
            });
        }
    }

    pub fn agent_signal(
        &self,
        device_id: &str,
        conn_id: u64,
        session_id: SessionId,
        payload: SignalPayload,
    ) {
        let mut reg = self.registry.lock().unwrap();
        if let Some(s) = online_mut(&mut reg, device_id, conn_id)
            .and_then(|a| a.session.as_ref())
            .filter(|s| s.id == session_id && s.accepted)
        {
            let _ = s.viewer_tx.send(ServerToViewer::Signal { payload });
        }
    }

    pub fn agent_end(&self, device_id: &str, conn_id: u64, session_id: SessionId) {
        let mut reg = self.registry.lock().unwrap();
        if let Some(agent) = online_mut(&mut reg, device_id, conn_id) {
            if agent.session.as_ref().is_some_and(|s| s.id == session_id) {
                let s = agent.session.take().expect("checked above");
                let _ = s.viewer_tx.send(ServerToViewer::SessionEnded {
                    reason: EndReason::AgentEnded,
                });
            }
        }
    }

    /// Returns `false` if the session no longer exists.
    pub fn viewer_signal(
        &self,
        device_id: &str,
        session_id: SessionId,
        payload: SignalPayload,
    ) -> bool {
        let reg = self.registry.lock().unwrap();
        match reg.devices.get(device_id).and_then(|d| d.online.as_ref()) {
            Some(a) if a.session.as_ref().is_some_and(|s| s.id == session_id) => {
                if a.session.as_ref().is_some_and(|s| s.accepted) {
                    let _ = a.tx.send(ServerToAgent::Signal {
                        session_id,
                        payload,
                    });
                }
                true
            }
            _ => false,
        }
    }

    pub fn viewer_left(&self, device_id: &str, session_id: SessionId) {
        let mut reg = self.registry.lock().unwrap();
        if let Some(agent) = reg
            .devices
            .get_mut(device_id)
            .and_then(|d| d.online.as_mut())
        {
            if agent.session.as_ref().is_some_and(|s| s.id == session_id) {
                agent.session = None;
                let _ = agent.tx.send(ServerToAgent::SessionEnded { session_id });
            }
        }
    }
}

fn online_mut<'a>(
    reg: &'a mut Registry,
    device_id: &str,
    conn_id: u64,
) -> Option<&'a mut OnlineAgent> {
    reg.devices
        .get_mut(device_id)
        .and_then(|d| d.online.as_mut())
        .filter(|a| a.conn_id == conn_id)
}

fn end_session(session: Option<Session>, agent_tx: &AgentTx, reason: EndReason) {
    if let Some(s) = session {
        let _ = s.viewer_tx.send(ServerToViewer::SessionEnded { reason });
        let _ = agent_tx.send(ServerToAgent::SessionEnded { session_id: s.id });
    }
}

pub fn is_valid_device_id(id: &str) -> bool {
    id.len() == 9 && id.bytes().all(|b| b.is_ascii_digit()) && !id.starts_with('0')
}

fn new_device_id(existing: &HashMap<String, Device>) -> String {
    let mut rng = rand::thread_rng();
    loop {
        let id = rng.gen_range(100_000_000u32..1_000_000_000).to_string();
        if !existing.contains_key(&id) {
            return id;
        }
    }
}

fn new_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(43)
        .map(char::from)
        .collect()
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}
