//! Read-only CUSTOS Advanced inspection CLI.
//!
//! This binary never installs, writes, signals, starts a service, or requires
//! root. `status` reads the root-owned marker/audit; `plan` prints the
//! installer contract for review.

use std::process::ExitCode;

use vp_custos_root::installer::InstallerPlan;
use vp_custos_root::layout::{ROOT_UID, RootLayout};
use vp_custos_root::status::read_status;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("status") => status_command(&mut args),
        Some("plan") => plan_command(&mut args),
        Some("-h" | "--help" | "help") | None => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn status_command(args: &mut impl Iterator<Item = String>) -> ExitCode {
    let root = args.next();
    let uid = match args.next() {
        Some(raw) => match raw.parse::<u32>() {
            Ok(uid) => uid,
            Err(_) => {
                eprintln!("uid must be a u32");
                return ExitCode::FAILURE;
            }
        },
        None => ROOT_UID,
    };
    let layout = match root.as_deref() {
        Some(path) => RootLayout::new(path),
        None => Ok(RootLayout::advanced_default()),
    };
    let layout = match layout {
        Ok(layout) => layout,
        Err(error) => {
            eprintln!("invalid root: {error}");
            return ExitCode::FAILURE;
        }
    };
    let status = read_status(&layout, uid);
    match serde_json::to_string_pretty(&status) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("status serialization failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn plan_command(args: &mut impl Iterator<Item = String>) -> ExitCode {
    let root = args.next();
    let layout = match root.as_deref() {
        Some(path) => RootLayout::new(path),
        None => Ok(RootLayout::advanced_default()),
    };
    let layout = match layout {
        Ok(layout) => layout,
        Err(error) => {
            eprintln!("invalid root: {error}");
            return ExitCode::FAILURE;
        }
    };
    match InstallerPlan::advanced(&layout).and_then(|plan| {
        serde_json::to_string_pretty(&plan).map_err(|error| {
            vp_custos_root::layout::LayoutError::Io {
                path: layout.root().to_path_buf(),
                source: std::io::Error::other(error),
            }
        })
    }) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("plan failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "custos-root — read-only CUSTOS Advanced inspection\n\
         \n\
         usage:\n\
           custos-root status [ROOT] [UID]   read tier/marker/audit status\n\
           custos-root plan [ROOT]           print the installer plan\n\
         \n\
         ROOT defaults to /Library/Application Support/VerlinParkCustos.\n\
         UID defaults to 0 (the production root owner).\n\
         This tool never installs, writes, signals, or starts services."
    );
}
