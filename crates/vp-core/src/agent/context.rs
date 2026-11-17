use super::*;

/// Marker for the authoritative structured task state block. It lives in the
/// parent because compaction reuse and tests both refer to it by name.
pub(super) const STRUCTURED_STATE_START: &str = "[structured task state - authoritative]";

pub(super) const STRUCTURED_STATE_END: &str = "[/structured task state]";

/// Flatten multi-line task text into one bounded display line.
pub(super) fn structured_state_line(value: &str) -> String {
    value.replace(['\n', '\r'], " ").trim().to_string()
}

mod bundles;
mod compaction;
mod prompts;
mod state;
mod support;

// Flat sibling visibility, unchanged from the pre-split single module. The
// agent parent glob-imports this module, so these re-exports keep the existing
// `crate::agent::...` call sites working without widening any API.
pub(super) use bundles::*;
pub(super) use compaction::*;
pub(super) use prompts::*;
pub(super) use state::{
    LoadedConversation, context_worktree_head, coordination_brief_for_agent,
    load_compaction_structured_state, load_conversation, load_session_project, mailbox_brief,
    parse_todos, project_memory_brief, refresh_structured_state, render_structured_state,
    restored_active_messages, task_dependency_brief,
};
pub(super) use support::*;

// `conversation_from_events` was `pub(crate)` before the split and is
// re-exported by the agent parent, so keep that exact visibility.
pub(crate) use state::conversation_from_events;
