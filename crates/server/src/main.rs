use std::{path::PathBuf, time::Duration};

use clap::Parser;
use proto::IceServer;
use remote_desk_server::{serve, Config};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(about = "Remote Desk control plane + signaling server")]
struct Args {
    #[arg(long, env = "RD_BIND", default_value = "0.0.0.0:8080")]
    bind: String,
    /// Comma-separated STUN/TURN URLs handed to peers.
    #[arg(
        long,
        env = "RD_ICE_URLS",
        value_delimiter = ',',
        default_value = "stun:stun.l.google.com:19302"
    )]
    ice_urls: Vec<String>,
    #[arg(long, env = "RD_TURN_USERNAME")]
    turn_username: Option<String>,
    #[arg(long, env = "RD_TURN_CREDENTIAL", hide_env_values = true)]
    turn_credential: Option<String>,
    /// Built web viewer to serve at `/` (e.g. web/dist).
    #[arg(long, env = "RD_WEB_DIR")]
    web_dir: Option<PathBuf>,
    #[arg(long, env = "RD_MAX_FAILED_LOGINS", default_value_t = 5)]
    max_failed_logins: usize,
    #[arg(long, env = "RD_LOG_JSON")]
    log_json: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn".into());
    if args.log_json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let (stun, turn): (Vec<_>, Vec<_>) = args
        .ice_urls
        .into_iter()
        .partition(|u| u.starts_with("stun:"));
    let mut ice_servers = vec![];
    if !stun.is_empty() {
        ice_servers.push(IceServer {
            urls: stun,
            username: None,
            credential: None,
        });
    }
    if !turn.is_empty() {
        ice_servers.push(IceServer {
            urls: turn,
            username: args.turn_username,
            credential: args.turn_credential,
        });
    }

    let config = Config {
        ice_servers,
        max_failed_logins: args.max_failed_logins,
        rate_window: Duration::from_secs(300),
        web_dir: args.web_dir,
    };
    let listener = tokio::net::TcpListener::bind(&args.bind).await?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    serve(listener, config).await?;
    Ok(())
}
