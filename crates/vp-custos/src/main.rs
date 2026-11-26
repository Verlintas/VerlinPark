use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use vp_custos::audit;
use vp_custos::client::default_data_dir;
use vp_custos::launchd::{self, LaunchdPaths, SystemLaunchctl};
use vp_custos::protocol::CustosRole;
use vp_custos::server::{ServerConfig, run};
use vp_custos::vault;

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
    /// Override the read-only Unix socket path (core role only).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Role label for the heartbeat. The observe-only server runs `core`.
    #[arg(long, default_value = "core")]
    role: String,
    /// Explicitly install the user LaunchAgent for this role.
    #[arg(long)]
    install: bool,
    /// Explicitly remove the user LaunchAgent for this role.
    #[arg(long)]
    uninstall: bool,
    /// Read-only LaunchAgent status for this role.
    #[arg(long)]
    agent_status: bool,
    /// Apply install/uninstall/status to all four roles.
    #[arg(long)]
    all: bool,
    /// Explicitly initialize the Custos-owned hash vault.
    #[arg(long)]
    vault_init: bool,
    /// Generate a 0600 local Ed25519 vault-signing key and exit.
    #[arg(long)]
    vault_keygen: Option<PathBuf>,
    /// Local Ed25519 seed used to sign a new vault manifest.
    #[arg(long)]
    signing_key: Option<PathBuf>,
    /// Verify the Custos-owned hash vault against the current installation.
    #[arg(long)]
    vault_verify: bool,
    /// Verify the Custos audit hash chain.
    #[arg(long)]
    audit_verify: bool,
    /// Opt in to automatic repair of drifted plists from a valid vault.
    #[arg(long)]
    vault_auto_repair: bool,
    /// Also allow automatic binary repair and a launchd restart.
    #[arg(long)]
    vault_auto_repair_binary: bool,
    /// Explicitly repair drifted/missing Custos-owned targets from the vault.
    #[arg(long)]
    vault_repair: bool,
    /// Required confirmation for mutating commands.
    #[arg(long)]
    yes: bool,
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
    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    let actions = [
        args.install,
        args.uninstall,
        args.agent_status,
        args.vault_init,
        args.vault_verify,
        args.vault_repair,
        args.vault_keygen.is_some(),
        args.audit_verify,
    ]
    .into_iter()
    .filter(|value| *value)
    .count();
    if actions > 1 {
        bail!("choose only one lifecycle or vault action");
    }
    if args.audit_verify {
        println!(
            "{}",
            serde_json::to_string_pretty(&audit::verify(&data_dir)?)?
        );
        return Ok(());
    }
    if let Some(path) = args.vault_keygen.as_deref() {
        if !args.yes {
            bail!("--vault-keygen requires --yes");
        }
        if args.signing_key.is_some() {
            bail!("--signing-key is only valid with --vault-init");
        }
        println!("{}", vault::keygen(path, true)?);
        return Ok(());
    }
    if args.vault_init || args.vault_verify || args.vault_repair {
        let binary = std::env::current_exe()?;
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home directory unavailable"))?;
        let paths = LaunchdPaths::new(home, &data_dir, &binary);
        let report = if args.vault_init {
            if !args.yes {
                bail!("--vault-init requires --yes");
            }
            if let Some(key) = args.signing_key.as_deref() {
                vault::init_signed(&data_dir, &paths, &binary, key)?
            } else {
                vault::init(&data_dir, &paths, &binary)?
            }
        } else if args.vault_repair {
            if !args.yes {
                bail!("--vault-repair requires --yes");
            }
            vault::repair(&data_dir, &paths, &binary, true)?
        } else {
            vault::verify(&data_dir, &paths, &binary)?
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if args.install || args.uninstall || args.agent_status {
        let roles = if args.all {
            launchd::ALL_ROLES.to_vec()
        } else {
            vec![parse_role(&args.role)?]
        };
        let binary = std::env::current_exe()?;
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home directory unavailable"))?;
        let paths = LaunchdPaths::new(home, &data_dir, binary);
        let runner = SystemLaunchctl;
        if args.install {
            if !args.yes {
                bail!("--install requires --yes");
            }
            let mut outcomes = Vec::new();
            for role in roles {
                outcomes.push(launchd::install(role, &paths, &runner, true)?);
            }
            println!("{}", serde_json::to_string_pretty(&outcomes)?);
            return Ok(());
        }
        if args.uninstall {
            if !args.yes {
                bail!("--uninstall requires --yes");
            }
            for role in roles {
                launchd::uninstall(role, &paths, &runner, true)?;
            }
            println!("uninstalled");
            return Ok(());
        }
        let mut statuses = Vec::new();
        for role in roles {
            statuses.push(launchd::status(role, &paths, &runner)?);
        }
        println!("{}", serde_json::to_string_pretty(&statuses)?);
        return Ok(());
    }
    let role = parse_role(&args.role)?;
    if role != CustosRole::Core {
        if args.socket.is_some() {
            bail!("--socket is only valid for the core role");
        }
        return vp_custos::ring::run_support_role(data_dir, role).await;
    }
    let mut config = ServerConfig::new(data_dir, role, true);
    if let Some(socket) = args.socket {
        config = config.with_socket(socket);
    }
    if args.vault_auto_repair_binary && !args.vault_auto_repair {
        bail!("--vault-auto-repair-binary requires --vault-auto-repair");
    }
    if args.vault_auto_repair {
        config = config.with_auto_repair(true);
    }
    if args.vault_auto_repair_binary {
        config = config.with_auto_repair_binary(true);
    }
    run(config).await
}

fn parse_role(value: &str) -> Result<CustosRole> {
    CustosRole::parse(value).ok_or_else(|| anyhow::anyhow!("unknown Custos role: {value}"))
}
