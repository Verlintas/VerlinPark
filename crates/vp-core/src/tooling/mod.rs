//! Higher-level agent tools and shared helpers.
//!
//! The base seven tools stay small and universally useful. This module contains
//! project-aware tools that absorb deterministic work instead of forcing the
//! model to compose long shell/search/read chains.

pub mod archive;
pub mod code;
pub mod code_intel;
pub mod data;
pub mod diff;
pub mod errors;
pub mod files;
pub mod fs;
pub mod git;
pub mod http;
pub mod lsp;
pub mod media;
pub mod office;
pub mod process;
pub mod project;
pub mod recipes;
pub mod remote;
pub mod search;
pub mod web;
