//! `vp-remote-agent` — a dependency-light remote workspace helper.
//!
//! The helper speaks bounded newline-delimited JSON-RPC 2.0 over stdin/stdout.
//! It is designed to be copied to a remote host and launched by `ssh`, so it
//! requires only POSIX, Git, and this binary. It never reads provider keys and
//! never opens a network port.
//!
//! Authentication is mandatory. The token comes from an environment variable
//! or a private file; the file must not be group/world readable.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use vp_remote::server::{ServerConfig, serve_stdio};
use vp_remote::{MAX_FRAME_BYTES, MAX_LEDGER_ENTRIES};

#[derive(Debug, Default)]
struct Args {
    root: Option<PathBuf>,
    token_env: Option<String>,
    token_file: Option<PathBuf>,
    ledger: Option<PathBuf>,
    no_ledger: bool,
    env_allow: Vec<String>,
    max_frame_bytes: Option<u64>,
    ledger_entries: Option<usize>,
    version: bool,
}

fn usage() -> &'static str {
    "vp-remote-agent remote-agent --root <DIR> [--token-env NAME | --token-file FILE]\n\
     \n\
     Options:\n\
       --root <DIR>            Canonical workspace root (required)\n\
       --token-env <NAME>      Read the auth token from this environment variable\n\
       --token-file <FILE>     Read the auth token from a 0600 file\n\
       --ledger <FILE>         Operation ledger path (default: ~/.verlinpark/remote-agent-ledger.jsonl)\n\
       --no-ledger             Keep the operation ledger in memory only\n\
       --env-allow <NAMES>     Comma-separated extra exec environment names\n\
       --max-frame-bytes <N>   Frame limit (1024..16777216)\n\
       --ledger-entries <N>    Retained ledger entries (1..8192)\n\
       --version               Print the helper version\n\
     \n\
     The helper writes protocol frames to stdout and logs only to stderr."
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut raw = std::env::args().skip(1).collect::<Vec<_>>();
    if raw.first().map(String::as_str) == Some("remote-agent") {
        raw.remove(0);
    }
    let mut index = 0;
    while index < raw.len() {
        let argument = &raw[index];
        let mut next = |label: &str| -> Result<String> {
            index += 1;
            raw.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{label} requires a value"))
        };
        match argument.as_str() {
            "--root" => args.root = Some(PathBuf::from(next("--root")?)),
            "--token-env" => args.token_env = Some(next("--token-env")?),
            "--token-file" => args.token_file = Some(PathBuf::from(next("--token-file")?)),
            "--ledger" => args.ledger = Some(PathBuf::from(next("--ledger")?)),
            "--no-ledger" => args.no_ledger = true,
            "--env-allow" => {
                let value = next("--env-allow")?;
                args.env_allow.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string),
                );
            }
            "--max-frame-bytes" => {
                args.max_frame_bytes = Some(
                    next("--max-frame-bytes")?
                        .parse()
                        .context("--max-frame-bytes must be an integer")?,
                );
            }
            "--ledger-entries" => {
                args.ledger_entries = Some(
                    next("--ledger-entries")?
                        .parse()
                        .context("--ledger-entries must be an integer")?,
                );
            }
            "--version" | "-V" => args.version = true,
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}\n\n{}", usage()),
        }
        index += 1;
    }
    Ok(args)
}

fn read_token(args: &Args) -> Result<String> {
    if let Some(path) = args.token_file.as_ref() {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("reading token file metadata {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                bail!(
                    "token file {} is accessible by group/others (mode {:o}); chmod 600 it",
                    path.display(),
                    mode
                );
            }
        }
        let token = std::fs::read_to_string(path)
            .with_context(|| format!("reading token file {}", path.display()))?;
        let token = token.trim().to_string();
        if token.chars().count() < 16 {
            bail!("token file must contain at least 16 characters");
        }
        return Ok(token);
    }
    if let Some(name) = args.token_env.as_deref() {
        let token = std::env::var(name)
            .with_context(|| format!("reading auth token from environment variable {name}"))?;
        let token = token.trim().to_string();
        if token.chars().count() < 16 {
            bail!("auth token environment variable must contain at least 16 characters");
        }
        return Ok(token);
    }
    bail!(
        "no auth token configured; pass --token-env or --token-file\n\n{}",
        usage()
    )
}

fn default_ledger() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".verlinpark")
            .join("remote-agent-ledger.jsonl")
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vp_remote_agent=info,vp_remote=info".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .compact()
        .init();
    let result = run().await;
    match result {
        Ok(()) => {}
        Err(error) => {
            tracing::error!(%error, "vp-remote-agent stopped");
            eprintln!("vp-remote-agent: {error}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<()> {
    let args = parse_args()?;
    if args.version {
        println!("vp-remote-agent {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let root = args
        .root
        .clone()
        .ok_or_else(|| anyhow!("--root is required\n\n{}", usage()))?;
    if !root.is_dir() {
        bail!("remote root is not a directory: {}", root.display());
    }
    let token = read_token(&args)?;
    let mut config = ServerConfig::new(root, token)?;
    if args.no_ledger {
        config.ledger_path = None;
    } else if let Some(path) = args.ledger.clone().or_else(default_ledger) {
        config.ledger_path = Some(path);
    }
    if let Some(max_frame) = args.max_frame_bytes {
        if !(1024..=MAX_FRAME_BYTES).contains(&max_frame) {
            bail!("--max-frame-bytes must be 1024..={MAX_FRAME_BYTES}");
        }
        config.max_frame_bytes = max_frame as usize;
    }
    if let Some(entries) = args.ledger_entries {
        if !(1..=MAX_LEDGER_ENTRIES).contains(&entries) {
            bail!("--ledger-entries must be 1..={MAX_LEDGER_ENTRIES}");
        }
        config.ledger_entries = entries;
    }
    config.env_allow = args.env_allow.clone();
    tracing::info!(
        root = %config.root.display(),
        ledger = ?config.ledger_path,
        "vp-remote-agent ready on stdio"
    );
    serve_stdio(config).await?;
    Ok(())
}
