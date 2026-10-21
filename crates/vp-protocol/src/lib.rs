//! Shared wire types for the VerlinPark daemon, app, and CLI.
//!
//! This crate is deliberately dependency-light: it defines the JSON-RPC
//! envelope, discovery file, and domain DTOs without pulling in storage or
//! runtime concerns.

pub mod discovery;
pub mod rpc;
pub mod types;

pub const PROTOCOL_VERSION: u32 = 1;

/// The version of the workspace crate. Useful in discovery/info responses.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
