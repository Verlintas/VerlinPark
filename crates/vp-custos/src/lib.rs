//! ◇CUSTOS — deterministic, observe-only integrity guardian.
//!
//! This crate is the first implementation slice of the design in
//! `docs/CUSTOS_CHARTER_V0.md`. It is deliberately read-only:
//!
//! - no language model or provider key;
//! - no approval, merge, budget, routing, identity, or source mutation;
//! - no agent-facing tool or mailbox surface;
//! - one core process, a 0600 Unix socket, and read-only SQLite probes.
//!
//! The four-role ring and mutation classes land in later slices. The charter,
//! capacity, score, ring-heartbeat, and probe modules are shared by those
//! slices so the policy does not fork.

#![forbid(unsafe_code)]

pub mod audit;
pub mod capacity;
pub mod charter;
pub mod client;
pub mod diagnostics;
pub mod discovery;
pub mod gating;
pub mod launchd;
pub mod mode;
pub mod notice;
pub mod probe;
pub mod process_probe;
pub mod protocol;
pub mod ring;
pub mod score;
pub mod server;
pub mod vault;

pub use capacity::{EffectiveCaps, MachineCapacity};
pub use charter::{ActionClass, ActionSpec, CharterView};
pub use gating::{evaluate, evaluate_plan};
pub use mode::{advanced_marker_path, detect_tier, detect_tier_at};
pub use probe::{ProbeReport, database_path, probe};
pub use protocol::RingView;
pub use protocol::{
    CUSTOS_PROTOCOL_VERSION, CUSTOS_VERSION, Counts, CustosRole, CustosState, Health, Status,
};
pub use ring::Heartbeat;
pub use score::{ProcessSample, ProcessState, ProcessVerdict, ProcessWindow};
