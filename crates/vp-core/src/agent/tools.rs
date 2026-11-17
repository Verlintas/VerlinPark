use super::*;

mod artifacts;
mod coordination;
mod discovery;
mod integrations;

// Flat sibling visibility, unchanged from the pre-split single module. The
// agent parent glob-imports this module, so these re-exports keep the existing
// `crate::agent::...` call sites working without widening any API.
pub(super) use artifacts::*;
pub(super) use coordination::*;
pub(super) use discovery::*;
pub(super) use integrations::*;
