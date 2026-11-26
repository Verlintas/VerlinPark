use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use vp_custos::client::default_data_dir;
use vp_custos::protocol::CustosRole;
use vp_custos::server::{ServerConfig, run};

#[derive(Debug, Parser)]
#[command(
    name = "custosd",
    version,
    about = "◇CUSTOS deterministic observe-only integrity guardian"
)]
struct Args {
    /// VerlinPark data directory. Defaults to VERLINPARK_DATA_DIR or the app data dir.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Override the read-only Unix socket path.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Role label for the heartbeat. The observe-only slice runs `core`.
    #[arg(long, default_value = "core")]
    role: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let args = Args::parse();
    let role =
        CustosRole::parse(&args.role).ok_or_else(|| anyhow::anyhow!("unknown Custos role"))?;
    let data_dir = args.data_dir.unwrap_or_else(default_data_dir);
    let mut config = ServerConfig::new(data_dir, role, true);
    if let Some(socket) = args.socket {
        config = config.with_socket(socket);
    }
    run(config).await
}
