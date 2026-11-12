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
use vp_remote::{MAX_FRAME_BYTES, MAX_LEDGER_ENTRIES, REMOTE_PROTOCOL_VERSION};

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
    describe: bool,
}

fn usage() -> &'static str {
    "vp-remote-agent remote-agent --root <DIR> [--token-env NAME | --token-file FILE]\n\
     \n\
     Options:\n\
       --root <DIR>            Canonical workspace root (required)\n\
       --token-env <NAME>      Read the auth token from this environment variable\n\
       --token-file <FILE>     Read the auth token from a 0600 file\n\
                               Default fallback: ~/.verlinpark/remote-token (0600)\n\
       --ledger <FILE>         Operation ledger path (default: ~/.verlinpark/remote-agent-ledger.jsonl)\n\
       --no-ledger             Keep the operation ledger in memory only\n\
       --env-allow <NAMES>     Comma-separated extra exec environment names\n\
       --max-frame-bytes <N>   Frame limit (1024..16777216)\n\
       --ledger-entries <N>    Retained ledger entries (1..8192)\n\
       --version               Print the helper version\n\
       --describe              Print machine-readable helper metadata as JSON\n\
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
            "--describe" => args.describe = true,
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
    read_token_from(args, |name| std::env::var(name).ok(), default_token_file())
}

/// The helper's private default token path. Operators can provision the same
/// token on the remote host without setting a remote environment variable,
/// which OpenSSH does not forward by default.
fn default_token_file() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".verlinpark").join("remote-token"))
}

fn read_token_from<F>(args: &Args, env: F, default_path: Option<PathBuf>) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(path) = args.token_file.as_ref() {
        return read_token_file(path);
    }
    if let Some(name) = args.token_env.as_deref() {
        if let Some(token) = env(name) {
            let token = token.trim().to_string();
            if token.chars().count() < 16 {
                bail!("auth token environment variable {name} must contain at least 16 characters");
            }
            return Ok(token);
        }
        // An SSH-launched helper normally cannot see the daemon's environment
        // because OpenSSH forwards only SendEnv names. Fall back to the
        // standard private token file before failing.
        if let Some(path) = default_path.as_ref()
            && path.exists()
        {
            return read_token_file(path);
        }
        bail!(
            "auth token environment variable {name} is not set and {} does not exist; \
             provision a 0600 token file or configure SSH SendEnv/AcceptEnv",
            default_path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "the default token file".to_string())
        );
    }
    if let Some(path) = default_path.as_ref()
        && path.exists()
    {
        return read_token_file(path);
    }
    bail!(
        "no auth token configured; pass --token-env or --token-file, or create \
         ~/.verlinpark/remote-token with mode 600\n\n{}",
        usage()
    );
}

fn read_token_file(path: &std::path::Path) -> Result<String> {
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
    Ok(token)
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
    if args.describe {
        let describe = serde_json::json!({
            "helper_version": env!("CARGO_PKG_VERSION"),
            "protocol_version": REMOTE_PROTOCOL_VERSION,
            "target_os": std::env::consts::OS,
            "target_arch": std::env::consts::ARCH,
            "capabilities": vp_remote::protocol::helper_capabilities(),
        });
        println!("{}", serde_json::to_string(&describe)?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn write_token(path: &std::path::Path, token: &str, mode: u32) {
        std::fs::write(path, token).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn private_token_file_is_read_and_trimmed() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("token");
        write_token(&path, "  token-from-file-1234\n", 0o600);
        let args = Args {
            token_file: Some(path),
            ..Args::default()
        };
        let token = read_token_from(&args, |_| None, None).unwrap();
        assert_eq!(token, "token-from-file-1234");
    }

    #[test]
    fn group_readable_token_file_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("token");
        write_token(&path, "token-from-file-1234", 0o644);
        let args = Args {
            token_file: Some(path),
            ..Args::default()
        };
        let error = read_token_from(&args, |_| None, None).unwrap_err();
        assert!(
            error.to_string().contains("chmod 600"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn environment_token_wins_over_the_default_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("token");
        write_token(&path, "token-from-default-1234", 0o600);
        let args = Args {
            token_env: Some("VP_TEST_TOKEN".into()),
            ..Args::default()
        };
        let token = read_token_from(
            &args,
            |name| (name == "VP_TEST_TOKEN").then(|| " token-from-env-123456 ".into()),
            Some(path),
        )
        .unwrap();
        assert_eq!(token, "token-from-env-123456");
    }

    #[test]
    fn missing_environment_token_falls_back_to_the_default_private_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("token");
        write_token(&path, "token-from-default-1234", 0o600);
        let args = Args {
            token_env: Some("VP_TEST_TOKEN".into()),
            ..Args::default()
        };
        let token = read_token_from(&args, |_| None, Some(path)).unwrap();
        assert_eq!(token, "token-from-default-1234");
    }

    #[test]
    fn short_environment_token_is_rejected_instead_of_falling_back() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("token");
        write_token(&path, "token-from-default-1234", 0o600);
        let args = Args {
            token_env: Some("VP_TEST_TOKEN".into()),
            ..Args::default()
        };
        let error = read_token_from(&args, |_| Some("short".into()), Some(path)).unwrap_err();
        assert!(
            error.to_string().contains("at least 16 characters"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_all_token_sources_reports_the_expected_locations() {
        let args = Args {
            token_env: Some("VP_TEST_TOKEN".into()),
            ..Args::default()
        };
        let error = read_token_from(&args, |_| None, None).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("VP_TEST_TOKEN"));
        assert!(message.contains("0600"));
    }
}
