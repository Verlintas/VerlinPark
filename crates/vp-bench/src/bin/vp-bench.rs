use std::process::ExitCode;

use clap::Parser;
use vp_bench::cli::{BenchCli, execute};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = BenchCli::parse();
    match execute(cli.command, cli.json).await {
        Ok(outcome) => {
            if cli.json {
                if let Some(value) = outcome.json {
                    match serde_json::to_string_pretty(&value) {
                        Ok(text) => println!("{text}"),
                        Err(error) => {
                            eprintln!("vp-bench: failed to print JSON: {error}");
                            return ExitCode::from(2);
                        }
                    }
                }
            } else {
                print!("{}", outcome.human);
            }
            ExitCode::from(outcome.exit_code as u8)
        }
        Err(error) => {
            eprintln!("vp-bench: {error:#}");
            ExitCode::from(2)
        }
    }
}
