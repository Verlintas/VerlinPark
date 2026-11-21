//! Human-readable, JSON, and CSV renderers for CLI command results.

use anyhow::Result;
use serde_json::{Value, json};

mod activity;
mod budget;
mod effects;
mod forges;
mod human;
mod identity;
mod instructions;
mod repository;
mod runtime;
mod support;

#[allow(unused_imports)]
pub(crate) use activity::*;
#[allow(unused_imports)]
pub(crate) use budget::*;
#[allow(unused_imports)]
pub(crate) use effects::*;
#[allow(unused_imports)]
pub(crate) use forges::*;
#[allow(unused_imports)]
pub(crate) use human::*;
#[allow(unused_imports)]
pub(crate) use identity::*;
#[allow(unused_imports)]
pub(crate) use instructions::*;
#[allow(unused_imports)]
pub(crate) use repository::*;
#[allow(unused_imports)]
pub(crate) use runtime::*;
#[allow(unused_imports)]
pub(crate) use support::*;
