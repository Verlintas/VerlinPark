//! Higher-level agent tools and shared helpers.
//!
//! The base seven tools stay small and universally useful. This module contains
//! project-aware tools that absorb deterministic work instead of forcing the
//! model to compose long shell/search/read chains.

pub mod code;
pub mod diff;
pub mod files;
pub mod fs;
pub mod git;
pub mod http;
pub mod process;
pub mod project;
