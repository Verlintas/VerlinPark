//! Cohesive service modules split out of the crate-root `Engine` implementation.
//!
//! Shared state types stay in the crate root; every module is a descendant of
//! the crate root, so `use super::*;` keeps the parent's imports and private
//! `Engine` fields visible without widening the public API.

use super::*;

pub(crate) mod agents;
pub(crate) mod approvals;
pub(crate) mod artifacts;
pub(crate) mod auto_budget;
pub(crate) mod checkpoints;
pub(crate) mod collaboration;
pub(crate) mod delivery;
pub(crate) mod effects;
pub(crate) mod gui;
pub(crate) mod intake;
pub(crate) mod integrations;
pub(crate) mod leases;
pub(crate) mod memory;
pub(crate) mod merge;
pub(crate) mod operator;
pub(crate) mod plans;
pub(crate) mod programs;
pub(crate) mod projects;
pub(crate) mod rescue;
pub(crate) mod resources;
pub(crate) mod runs;
pub(crate) mod sessions;
pub(crate) mod sources;
pub(crate) mod support;
pub(crate) mod surfaces;
pub(crate) mod usage;
pub(crate) mod worktrees;
