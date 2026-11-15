//! Host agent.
//!
//! Phase 1: registers with the control plane, keeps a stable device ID, gates
//! sessions behind a password + consent prompt and logs WebRTC signaling.
//! Phase 2 adds capture, hardware encode, the WebRTC peer and input injection.

mod config;

use std::{path::PathBuf, time::Duration};

use anyhow::{bail, Context};
use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHasher,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use proto::{AgentInfo, AgentToServer, ErrorCode, ServerToAgent, SessionId, PROTOCOL_VERSION};
use rand::{distributions::Alphanumeric, Rng};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

#[derive(Parser)]
#[command(about = "Remote Desk host agent")]
struct Args {
    /// Control plane base URL (ws:// for local dev, wss:// otherwise).
    #[arg(long, env = "RD_SERVER", default_value = "ws://localhost:8080")]
    server: String,
    /// Set (and persist) the unattended-access password, then continue running.
    #[arg(long, env = "RD_SET_PASSWORD", hide_env_values = true)]
    set_password: Option<String>,
    /// Accept sessions without a local consent prompt (requires a set password).
    #[arg(long)]
    unattended: bool,
    /// Seconds to wait for local consent before rejecting.
    #[arg(long, default_value_t = 30)]
    consent_timeout: u64,
    #[arg(long, env = "RD_AGENT_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let cfg_path = args.config.clone().unwrap_or_else(config::default_path);
    let mut identity = config::load(&cfg_path)?;

    if let Some(pw) = &args.set_password {
        if pw.len() < 8 {
            bail!("password must be at least 8 characters");
        }
        identity.password_hash = Some(hash_password(pw)?);
        config::save(&cfg_path, &identity)?;
        info!(path = %cfg_path.display(), "unattended password saved");
    }
    if args.unattended && identity.password_hash.is_none() {
        bail!("--unattended requires a password; run once with --set-password");
    }

    // Without an unattended password, use a fresh one-time password per run (AnyDesk-style).
    let password_hash = match &identity.password_hash {
        Some(h) => h.clone(),
        None => {
            let otp: String = rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(10)
                .map(char::from)
                .collect();
            println!("\n  One-time password: {otp}\n");
            hash_password(&otp)?
        }
    };

    let consent = Consent::spawn(args.unattended, Duration::from_secs(args.consent_timeout));
    let mut backoff = Duration::from_secs(1);
    loop {
        match run_once(&args, &cfg_path, &mut identity, &password_hash, &consent).await {
            Ok(()) => backoff = Duration::from_secs(1),
            Err(e) => warn!("connection error: {e:#}"),
        }
        info!("reconnecting in {}s", backoff.as_secs());
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn run_once(
    args: &Args,
    cfg_path: &PathBuf,
    identity: &mut config::Identity,
    password_hash: &str,
    consent: &Consent,
) -> anyhow::Result<()> {
    let url = format!("{}/ws/agent", args.server.trim_end_matches('/'));
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("connecting to {url}"))?;
    let (mut sink, mut stream) = ws.split();

    let register = AgentToServer::Register {
        protocol_version: PROTOCOL_VERSION,
        device_id: identity.device_id.clone(),
        device_token: identity.device_token.clone(),
        password_hash: password_hash.to_owned(),
        info: AgentInfo {
            hostname: hostname(),
            os: std::env::consts::OS.into(),
            agent_version: env!("CARGO_PKG_VERSION").into(),
        },
    };
    sink.send(Message::Text(serde_json::to_string(&register)?.into()))
        .await?;

    // Outbound queue so consent prompts don't block the read loop.
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

    let mut active: Option<SessionId> = None;
    while let Some(frame) = stream.next().await {
        let text = match frame? {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let msg: ServerToAgent = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                warn!("ignoring unparseable server message: {e}");
                continue;
            }
        };
        match msg {
            ServerToAgent::Registered {
                device_id,
                device_token,
                ..
            } => {
                if identity.device_id.as_deref() != Some(&device_id)
                    || identity.device_token.as_deref() != Some(&device_token)
                {
                    identity.device_id = Some(device_id.clone());
                    identity.device_token = Some(device_token);
                    config::save(cfg_path, identity)?;
                }
                println!("\n  Device ID: {}\n", format_id(&device_id));
            }
            ServerToAgent::Error(e) => {
                if e.code == ErrorCode::DeviceIdTaken {
                    warn!("saved device ID is owned by another install; requesting a new one");
                    identity.device_id = None;
                    identity.device_token = None;
                    config::save(cfg_path, identity)?;
                }
                bail!("server error {:?}: {}", e.code, e.message);
            }
            ServerToAgent::SessionRequest {
                session_id,
                viewer_addr,
            } => {
                info!(%session_id, %viewer_addr, "incoming session request");
                let out = out_tx.clone();
                let consent = consent.clone();
                tokio::spawn(async move {
                    let accept = consent.ask(&viewer_addr).await;
                    let _ = out.send(AgentToServer::SessionDecision { session_id, accept });
                });
                active = Some(session_id);
            }
            ServerToAgent::Signal {
                session_id,
                payload,
            } if active == Some(session_id) => {
                // Phase 2: feed into the WebRTC peer connection.
                info!(%session_id, ?payload, "signal received (media pipeline not implemented yet)");
            }
            ServerToAgent::Signal { .. } => {}
            ServerToAgent::SessionEnded { session_id } => {
                info!(%session_id, "session ended");
                if active == Some(session_id) {
                    active = None;
                }
            }
        }
    }
    writer.abort();
    Ok(())
}

/// Serializes consent prompts onto the terminal (one reader of stdin for the process).
#[derive(Clone)]
struct Consent {
    unattended: bool,
    timeout: Duration,
    lines: std::sync::Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>>,
}

impl Consent {
    fn spawn(unattended: bool, timeout: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Self {
            unattended,
            timeout,
            lines: std::sync::Arc::new(tokio::sync::Mutex::new(rx)),
        }
    }

    async fn ask(&self, from: &str) -> bool {
        if self.unattended {
            info!(%from, "unattended mode: session accepted");
            return true;
        }
        let mut lines = self.lines.lock().await;
        while lines.try_recv().is_ok() {} // drop stale keystrokes
        println!(
            "Allow remote session from {from}? [y/N] ({}s)",
            self.timeout.as_secs()
        );
        match tokio::time::timeout(self.timeout, lines.recv()).await {
            Ok(Some(l)) => l.trim().eq_ignore_ascii_case("y"),
            _ => {
                println!("No answer; session rejected.");
                false
            }
        }
    }
}

fn hash_password(pw: &str) -> anyhow::Result<String> {
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

fn format_id(id: &str) -> String {
    id.as_bytes()
        .chunks(3)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
}
