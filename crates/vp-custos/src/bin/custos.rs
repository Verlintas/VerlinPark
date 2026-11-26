use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::Value;

use vp_custos::client;

#[derive(Debug, Parser)]
#[command(name = "custos", version, about = "Read-only ◇CUSTOS status client")]
struct Args {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum Command {
    /// Sealed health summary.
    Status,
    /// Bounded probe details.
    Health,
    /// Pinned charter view.
    Charter,
    /// Server liveness.
    Ping,
    /// Generate local standard + forensic diagnostic bundles.
    Diagnostics,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("custos: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> anyhow::Result<()> {
    let data_dir = args.data_dir.unwrap_or_else(client::default_data_dir);
    let socket = client::resolve_socket(&data_dir, args.socket.as_deref());
    let value = match args.command {
        Command::Status => client::status(&socket).await?,
        Command::Health => client::health(&socket).await?,
        Command::Charter => client::charter(&socket).await?,
        Command::Ping => client::ping(&socket).await?,
        Command::Diagnostics => client::diagnostics(&socket).await?,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_human(&args.command, &value);
    }
    Ok(())
}

fn print_human(command: &Command, value: &Value) {
    match command {
        Command::Status => {
            let state = value
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let role = value.get("role").and_then(Value::as_str).unwrap_or("?");
            let charter = value
                .get("charter_hash")
                .and_then(Value::as_str)
                .unwrap_or("?");
            println!("◇CUSTOS  {state}  role={role}");
            println!("charter {charter}");
            if let Some(dimensions) = value.get("dimensions").and_then(Value::as_array) {
                for dimension in dimensions {
                    let key = dimension.get("key").and_then(Value::as_str).unwrap_or("?");
                    let state = dimension
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    let detail = dimension
                        .get("detail")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    println!("  {key:<10} {state:<9} {detail}");
                }
            }
        }
        Command::Health | Command::Charter => {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_default()
            );
        }
        Command::Ping => {
            let version = value.get("version").and_then(Value::as_str).unwrap_or("?");
            let protocol = value
                .get("protocol")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let pid = value.get("pid").and_then(Value::as_u64).unwrap_or_default();
            let role = value.get("role").and_then(Value::as_str).unwrap_or("?");
            println!("◇CUSTOS v{version} protocol={protocol} role={role} pid={pid}");
        }
        Command::Diagnostics => {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_default()
            );
        }
    }
}
