//! Host agent.
//!
//! Two frontends over one core:
//! - **CLI** (`rd-agent`): prints ID/password, consent on stdin.
//! - **Menu-bar app** (macOS, `Remote Desk.app`): chosen automatically when running
//!   from an app bundle, or with `--gui`. By default it also runs the signaling server
//!   and web viewer in-process, so one Mac is self-contained.
//!
//! Phase 1: registration, password + consent gating, signaling. Phase 2 adds capture,
//! hardware encode, the WebRTC peer and input injection.

mod cli;
mod config;
mod core;
#[cfg(target_os = "macos")]
mod tray;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context};
use clap::Parser;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::core::{hash_password, Agent, MIN_PASSWORD_LEN};

const DEFAULT_SERVER_BIND: &str = "0.0.0.0:8080";

#[derive(Parser, Debug, Default)]
#[command(about = "Remote Desk host agent")]
struct Args {
    /// Control plane base URL (ws:// for local dev, wss:// otherwise).
    /// Ignored with --with-server.
    #[arg(long, env = "RD_SERVER", default_value = "ws://localhost:8080")]
    server: String,
    /// Also run the signaling server in this process, listening on this address.
    #[arg(long, num_args = 0..=1, default_missing_value = DEFAULT_SERVER_BIND)]
    with_server: Option<String>,
    /// Built web viewer to serve from the embedded server (e.g. web/dist).
    #[arg(long)]
    web_dir: Option<PathBuf>,
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
    /// Run as a menu-bar app (default when launched from Remote Desk.app).
    #[arg(long)]
    gui: bool,
}

fn main() {
    let bundle = app_bundle_resources();
    // Finder may pass extra arguments to bundled apps; don't refuse to start over them.
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) if bundle.is_none() => e.exit(),
        Err(_) => Args {
            consent_timeout: 30,
            server: "ws://localhost:8080".into(),
            ..Default::default()
        },
    };
    let gui = args.gui || bundle.is_some();
    if let Err(e) = if gui {
        run_gui(args, bundle)
    } else {
        run_cli(args)
    } {
        error!("{e:#}");
        #[cfg(target_os = "macos")]
        if gui {
            tray::alert(&format!("{e:#}"));
        }
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run_cli(args: Args) -> anyhow::Result<()> {
    init_logging(None);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let cfg_path = args.config.clone().unwrap_or_else(config::default_path);
        let _lock = config::lock_single_instance(&cfg_path)?;
        let mut identity = config::load(&cfg_path)?;
        if let Some(pw) = &args.set_password {
            if pw.chars().count() < MIN_PASSWORD_LEN {
                bail!("password must be at least {MIN_PASSWORD_LEN} characters");
            }
            identity.password_hash = Some(hash_password(pw)?);
            config::save(&cfg_path, &identity)?;
            info!(path = %cfg_path.display(), "unattended password saved");
        }
        if args.unattended && identity.password_hash.is_none() {
            bail!("--unattended requires a password; run once with --set-password");
        }
        let server_url = match &args.with_server {
            Some(bind) => start_embedded_server(bind, args.web_dir.clone()).await?,
            None => args.server.clone(),
        };
        let frontend = Arc::new(cli::CliFrontend::new(Duration::from_secs(
            args.consent_timeout,
        )));
        let agent = Agent::new(server_url, cfg_path, identity, args.unattended, frontend)?;
        // Keep the sender alive: the CLI has no commands but the core selects on them.
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        agent.run(cmd_rx).await;
        Ok(())
    })
}

#[cfg(target_os = "macos")]
fn run_gui(args: Args, bundle: Option<PathBuf>) -> anyhow::Result<()> {
    let logs = dirs::home_dir()
        .map(|h| h.join("Library/Logs/Remote Desk"))
        .unwrap_or_else(|| PathBuf::from("."));
    init_logging(Some(&logs));

    let cfg_path = args.config.clone().unwrap_or_else(config::default_path);
    let lock = config::lock_single_instance(&cfg_path)?;
    let identity = config::load(&cfg_path)?;

    // Remote control plane if configured; otherwise run our own (with the viewer).
    let embedded = identity.server_url.is_none().then(|| {
        let bind = args
            .with_server
            .clone()
            .or_else(|| identity.server_bind.clone())
            .unwrap_or_else(|| DEFAULT_SERVER_BIND.into());
        let web_dir = args
            .web_dir
            .clone()
            .or_else(|| bundle.as_ref().map(|r| r.join("web")));
        (bind, web_dir)
    });
    let viewer_url = embedded
        .as_ref()
        .filter(|(_, web)| web.is_some())
        .and_then(|(bind, _)| bind.parse::<std::net::SocketAddr>().ok())
        .map(|a| format!("http://localhost:{}", a.port()));

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let start = move |frontend: Arc<dyn core::Frontend>| -> futures_util::future::BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            let _lock = lock;
            let server_url = match embedded {
                Some((bind, web_dir)) => start_embedded_server(&bind, web_dir).await?,
                None => identity.server_url.clone().unwrap_or_default(),
            };
            let agent = Agent::new(server_url, cfg_path, identity, false, frontend)?;
            agent.run(cmd_rx).await;
            Ok(())
        })
    };
    tray::run(start, cmd_tx, viewer_url)
}

#[cfg(not(target_os = "macos"))]
fn run_gui(_args: Args, _bundle: Option<PathBuf>) -> anyhow::Result<()> {
    bail!("the menu-bar app is only available on macOS for now; run without --gui")
}

/// Starts the signaling server in-process; returns the URL the agent should use.
async fn start_embedded_server(bind: &str, web_dir: Option<PathBuf>) -> anyhow::Result<String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("starting built-in server on {bind} (port in use?)"))?;
    let port = listener.local_addr()?.port();
    info!(%bind, web = ?web_dir, "built-in server listening");
    let config = remote_desk_server::Config {
        web_dir,
        ..Default::default()
    };
    tokio::spawn(async move {
        if let Err(e) = remote_desk_server::serve(listener, config).await {
            error!("built-in server stopped: {e}");
        }
    });
    Ok(format!("ws://127.0.0.1:{port}"))
}

/// `Some(Contents/Resources)` when running as `Foo.app/Contents/MacOS/<exe>`.
fn app_bundle_resources() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let macos_dir = exe.parent()?;
    let contents = macos_dir.parent()?;
    let is_bundle = macos_dir.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && contents.parent()?.extension()? == "app";
    is_bundle.then(|| contents.join("Resources"))
}

fn init_logging(log_dir: Option<&Path>) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let file = log_dir.and_then(|d| {
        std::fs::create_dir_all(d).ok()?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(d.join("agent.log"))
            .ok()
    });
    match file {
        Some(f) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}
