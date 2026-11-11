//! Durable active-context checkpoints.
//!
//! A context checkpoint is a bounded snapshot of the boundary produced by
//! `context.compacted`: mode, prefixed summary, through-message cursor, plan
//! revision, provider/model/prefix identity, worktree head, and the
//! authoritative structured task state. Rows are event-backed projections:
//! `context.checkpointed` carries the full row, `context.checkpoint.deleted`
//! tombstones it, and replay/repair rebuilds the table from those events.
//!
//! Restore is intentionally not a run resume. It appends a reconstructed
//! `context.compacted` event plus `context.restored`, so the ordinary
//! conversation loaders rewind to the checkpoint boundary. Original
//! `message.created` rows and worktree content are never modified.

use anyhow::{Result, anyhow, bail};
use vp_protocol::types::{ContextCheckpoint, ContextStructuredState, Event};

/// A checkpoint summary is the already-bounded compaction summary. This is a
/// defense-in-depth cap for hand-edited or third-party events.
pub const MAX_CONTEXT_CHECKPOINT_SUMMARY_BYTES: usize = 128 * 1024;
/// Serialized structured state is authoritative task metadata, not a prompt
/// transcript. Sixteen KiB is far above the practical plan-task fields.
pub const MAX_CONTEXT_CHECKPOINT_STRUCTURED_BYTES: usize = 16 * 1024;
pub const MAX_CONTEXT_CHECKPOINT_LABEL_CHARS: usize = 120;
pub const MAX_CONTEXT_CHECKPOINT_GOAL_CHARS: usize = 1_000;
pub const MAX_CONTEXT_CHECKPOINT_ACCEPTANCE_ITEMS: usize = 20;
pub const MAX_CONTEXT_CHECKPOINT_WRITE_SCOPE_ITEMS: usize = 50;
pub const MAX_CONTEXT_CHECKPOINT_LIST_LIMIT: u32 = 200;
pub const MAX_CONTEXT_CHECKPOINT_POLICY_ITEMS: u32 = 100;
pub const MAX_CONTEXT_CHECKPOINT_POLICY_AGE_DAYS: u32 = 365;

pub const CONTEXT_CHECKPOINT_CREATED_EVENT: &str = "context.checkpointed";
pub const CONTEXT_CHECKPOINT_DELETED_EVENT: &str = "context.checkpoint.deleted";
pub const CONTEXT_RESTORED_EVENT: &str = "context.restored";

pub(crate) fn truncate_at_char_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Clamp one structured-state block to bounded, deterministic values. The
/// model summary is advisory, but this block is authoritative for recovery, so
/// truncation is explicit and stable rather than silent JSON loss.
pub(crate) fn bound_structured_state(mut state: ContextStructuredState) -> ContextStructuredState {
    state.goal =
        truncate_at_char_boundary(state.goal.trim(), MAX_CONTEXT_CHECKPOINT_GOAL_CHARS).to_string();
    if let Some(task_id) = state.active_task_id.take() {
        let task_id = task_id.trim();
        state.active_task_id =
            (!task_id.is_empty()).then(|| truncate_at_char_boundary(task_id, 200).to_string());
    }
    state.active_task_title = state
        .active_task_title
        .map(|value| truncate_at_char_boundary(value.trim(), 300).to_string());
    state.active_task_department = state
        .active_task_department
        .map(|value| truncate_at_char_boundary(value.trim(), 200).to_string());
    state.active_task_acceptance = state
        .active_task_acceptance
        .into_iter()
        .take(MAX_CONTEXT_CHECKPOINT_ACCEPTANCE_ITEMS)
        .map(|item| truncate_at_char_boundary(item.trim(), 500).to_string())
        .filter(|item| !item.is_empty())
        .collect();
    state.active_task_write_scope = state
        .active_task_write_scope
        .into_iter()
        .take(MAX_CONTEXT_CHECKPOINT_WRITE_SCOPE_ITEMS)
        .map(|item| truncate_at_char_boundary(item.trim(), 300).to_string())
        .filter(|item| !item.is_empty())
        .collect();
    state.recent_completed_task_ids = state
        .recent_completed_task_ids
        .into_iter()
        .take(16)
        .map(|item| truncate_at_char_boundary(item.trim(), 200).to_string())
        .filter(|item| !item.is_empty())
        .collect();
    state.generated_at = truncate_at_char_boundary(state.generated_at.trim(), 64).to_string();
    state
}

fn clamp_list(items: &mut Vec<String>, max_items: usize, max_chars: usize) {
    items.truncate(max_items);
    for item in items.iter_mut() {
        *item = truncate_at_char_boundary(item.trim(), max_chars).to_string();
    }
    items.retain(|item| !item.is_empty());
}

/// Keep the authoritative block within its storage cap by dropping the least
/// important tail entries first. A pathological plan must not make compaction
/// fail or silently exceed the event-size budget.
fn shrink_structured_state(mut state: ContextStructuredState) -> Result<ContextStructuredState> {
    let mut encoded = serde_json::to_vec(&state)?;
    while encoded.len() > MAX_CONTEXT_CHECKPOINT_STRUCTURED_BYTES {
        let removed = state.active_task_write_scope.pop().is_some()
            || state.active_task_acceptance.pop().is_some()
            || state.recent_completed_task_ids.pop().is_some()
            || state.active_task_title.take().is_some()
            || state.active_task_department.take().is_some()
            || state.active_task_id.take().is_some();
        if !removed {
            if state.goal.is_empty() {
                break;
            }
            let keep = state.goal.len() / 2;
            state.goal = truncate_at_char_boundary(&state.goal, keep).to_string();
            if state.goal.is_empty() {
                break;
            }
        }
        encoded = serde_json::to_vec(&state)?;
    }
    if encoded.len() > MAX_CONTEXT_CHECKPOINT_STRUCTURED_BYTES {
        state = ContextStructuredState {
            plan_revision: state.plan_revision,
            generated_at: state.generated_at,
            ..ContextStructuredState::default()
        };
    }
    Ok(state)
}

fn bound_label(label: &str, checkpoint_id: &str) -> String {
    let normalized = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = truncate_at_char_boundary(&normalized, MAX_CONTEXT_CHECKPOINT_LABEL_CHARS);
    if label.trim().is_empty() {
        let suffix = checkpoint_id.chars().take(8).collect::<String>();
        format!("context checkpoint {suffix}")
    } else {
        label.to_string()
    }
}

/// Validate and bound a checkpoint before it reaches the event table. The
/// caller may pass an unbounded summary from an event; truncation is recorded
/// in the stored payload so the projection matches replay exactly.
pub(crate) fn bound_context_checkpoint(
    mut checkpoint: ContextCheckpoint,
) -> Result<ContextCheckpoint> {
    if checkpoint.session_id.trim().is_empty() {
        bail!("context checkpoint session id is required");
    }
    if checkpoint.project_id.trim().is_empty() {
        bail!("context checkpoint project id is required");
    }
    if checkpoint.id.trim().is_empty() {
        bail!("context checkpoint id is required");
    }
    checkpoint.id = checkpoint.id.trim().to_string();
    checkpoint.session_id = checkpoint.session_id.trim().to_string();
    checkpoint.project_id = checkpoint.project_id.trim().to_string();
    checkpoint.label = bound_label(&checkpoint.label, &checkpoint.id);
    if checkpoint.mode.trim().is_empty() {
        checkpoint.mode = "manual".to_string();
    }
    if !matches!(checkpoint.mode.as_str(), "summary" | "trim" | "manual") {
        bail!(
            "context checkpoint mode must be summary, trim, or manual; got {}",
            checkpoint.mode
        );
    }
    checkpoint.summary = checkpoint.summary.and_then(|summary| {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return None;
        }
        let bounded = truncate_at_char_boundary(trimmed, MAX_CONTEXT_CHECKPOINT_SUMMARY_BYTES);
        Some(bounded.to_string())
    });
    if checkpoint.mode == "summary" && checkpoint.summary.is_none() {
        bail!("a summary checkpoint requires a non-empty summary");
    }
    if let Some(through) = checkpoint.through_message_id.as_mut() {
        *through = through.trim().to_string();
        if through.is_empty() {
            checkpoint.through_message_id = None;
        }
    }
    if checkpoint.message_cursor > 10_000_000 {
        bail!("context checkpoint message cursor is out of range");
    }
    checkpoint.provider = truncate_at_char_boundary(checkpoint.provider.trim(), 200).to_string();
    checkpoint.model = truncate_at_char_boundary(checkpoint.model.trim(), 200).to_string();
    checkpoint.prompt_prefix_hash =
        truncate_at_char_boundary(checkpoint.prompt_prefix_hash.trim(), 200).to_string();
    checkpoint.worktree_head =
        truncate_at_char_boundary(checkpoint.worktree_head.trim(), 200).to_string();
    if let Some(state) = checkpoint.structured_state.take() {
        let mut state = bound_structured_state(state);
        clamp_list(
            &mut state.active_task_acceptance,
            MAX_CONTEXT_CHECKPOINT_ACCEPTANCE_ITEMS,
            500,
        );
        clamp_list(
            &mut state.active_task_write_scope,
            MAX_CONTEXT_CHECKPOINT_WRITE_SCOPE_ITEMS,
            300,
        );
        checkpoint.structured_state = Some(shrink_structured_state(state)?);
    }
    if checkpoint.created_at.trim().is_empty() {
        checkpoint.created_at = vp_protocol::now_rfc3339();
    }
    Ok(checkpoint)
}

/// Project a full `context.checkpointed` event. Direct-row payloads are
/// preferred; the nested `checkpoint` form is accepted for forward/backward
/// compatibility with future producers.
pub(crate) fn context_checkpoint_from_event(event: &Event) -> Option<ContextCheckpoint> {
    if event.kind != CONTEXT_CHECKPOINT_CREATED_EVENT {
        return None;
    }
    serde_json::from_value::<ContextCheckpoint>(event.payload.clone())
        .ok()
        .or_else(|| {
            event
                .payload
                .get("checkpoint")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
        })
}

/// Rebuild the active checkpoint projection by applying checkpoint/deletion
/// events in sequence order. `None` means the stream contains a malformed
/// checkpoint event and the projection cannot be trusted.
pub(crate) fn checkpoints_from_events(events: &[Event]) -> Option<Vec<ContextCheckpoint>> {
    let mut rows: Vec<ContextCheckpoint> = Vec::new();
    for event in events {
        match event.kind.as_str() {
            CONTEXT_CHECKPOINT_CREATED_EVENT => {
                let checkpoint = context_checkpoint_from_event(event)?;
                if let Some(existing) = rows.iter_mut().find(|row| row.id == checkpoint.id) {
                    *existing = checkpoint;
                } else {
                    rows.push(checkpoint);
                }
            }
            CONTEXT_CHECKPOINT_DELETED_EVENT => {
                if let Some(checkpoint_id) = event
                    .payload
                    .get("checkpoint_id")
                    .and_then(serde_json::Value::as_str)
                {
                    rows.retain(|row| row.id != checkpoint_id);
                }
            }
            _ => {}
        }
    }
    rows.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Some(rows)
}

pub(crate) fn context_checkpoints_match(
    projected: &[ContextCheckpoint],
    expected: &[ContextCheckpoint],
) -> bool {
    projected.len() == expected.len()
        && projected
            .iter()
            .zip(expected.iter())
            .all(|(left, right)| left == right)
}

/// The checkpoint whose boundary is currently active, if the latest
/// compaction event carries a checkpoint reference. Older compactions predate
/// checkpoints and return `None`.
/// The compaction mode a restore must append. A `trim` checkpoint can still
/// carry the authoritative structured-state block in `summary`; the loader
/// deliberately does not inject a summary for trim mode, so the checkpoint
/// must restore the same way.
pub(crate) fn context_checkpoint_restore_mode(checkpoint: &ContextCheckpoint) -> &'static str {
    match checkpoint.mode.as_str() {
        "summary" => "summary",
        "trim" => "trim",
        "manual" if checkpoint.summary.is_some() => "summary",
        _ => "trim",
    }
}

pub(crate) fn active_context_checkpoint_id(events: &[Event]) -> Option<String> {
    let compaction = events
        .iter()
        .rev()
        .find(|event| event.kind == "context.compacted")?;
    compaction
        .payload
        .get("context_checkpoint_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            compaction
                .payload
                .get("restored_from_checkpoint")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_string)
}

/// Summary preview for UI/CLI. It keeps the first line and a bounded suffix
/// without exposing more prompt text than the stored summary already contains.
pub(crate) fn context_checkpoint_preview(summary: Option<&str>, max_chars: usize) -> String {
    let Some(summary) = summary else {
        return String::new();
    };
    let normalized = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut preview = normalized.chars().take(max_chars).collect::<String>();
    preview.push('…');
    preview
}

pub(crate) fn validate_context_checkpoint_policy(
    policy: &vp_protocol::types::ContextCheckpointPolicy,
) -> Result<()> {
    if policy.max_checkpoints == 0 || policy.max_checkpoints > MAX_CONTEXT_CHECKPOINT_POLICY_ITEMS {
        bail!(
            "context checkpoint max_checkpoints must be between 1 and {}",
            MAX_CONTEXT_CHECKPOINT_POLICY_ITEMS
        );
    }
    if policy.max_age_days > MAX_CONTEXT_CHECKPOINT_POLICY_AGE_DAYS {
        bail!(
            "context checkpoint max_age_days must be at most {}",
            MAX_CONTEXT_CHECKPOINT_POLICY_AGE_DAYS
        );
    }
    Ok(())
}

pub(crate) fn empty_context_checkpoint_error(checkpoint_id: &str) -> anyhow::Error {
    anyhow!("context checkpoint not found: {checkpoint_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::Event;

    fn checkpoint(id: &str) -> ContextCheckpoint {
        ContextCheckpoint {
            id: id.into(),
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            label: format!("checkpoint {id}"),
            mode: "summary".into(),
            summary: Some("Earlier conversation summary\n\nstate".into()),
            through_message_id: Some("msg-3".into()),
            message_cursor: 3,
            plan_revision: 1,
            provider: "openai-compatible".into(),
            model: "test".into(),
            prompt_prefix_hash: "abc".into(),
            worktree_head: "deadbeef".into(),
            tokens_before: 10_000,
            tokens_after: 2_000,
            structured_state: None,
            source_compaction_seq: Some(7),
            automatic: true,
            created_at: format!("2026-11-11T00:00:0{}Z", id.len() % 10),
        }
    }

    fn event(seq: u64, kind: &str, payload: serde_json::Value) -> Event {
        Event {
            session_id: "ses-1".into(),
            seq,
            kind: kind.into(),
            payload,
            created_at: format!("2026-11-11T00:00:{seq:02}Z"),
        }
    }

    #[test]
    fn checkpoint_bounds_labels_summaries_and_structured_state() {
        let mut value = checkpoint("ctx-1");
        value.label = "  too   many\nspaces  ".into();
        value.summary = Some("x".repeat(MAX_CONTEXT_CHECKPOINT_SUMMARY_BYTES + 100));
        value.structured_state = Some(ContextStructuredState {
            goal: "g".repeat(MAX_CONTEXT_CHECKPOINT_GOAL_CHARS + 100),
            plan_revision: 2,
            active_task_acceptance: (0..100).map(|index| format!("item {index}")).collect(),
            active_task_write_scope: (0..100).map(|index| format!("path/{index}")).collect(),
            recent_completed_task_ids: (0..100).map(|index| format!("T{index}")).collect(),
            ..ContextStructuredState::default()
        });
        let bounded = bound_context_checkpoint(value).unwrap();
        assert_eq!(bounded.label, "too many spaces");
        assert_eq!(
            bounded.summary.as_ref().unwrap().len(),
            MAX_CONTEXT_CHECKPOINT_SUMMARY_BYTES
        );
        let state = bounded.structured_state.unwrap();
        assert_eq!(state.goal.len(), MAX_CONTEXT_CHECKPOINT_GOAL_CHARS);
        assert_eq!(
            state.active_task_acceptance.len(),
            MAX_CONTEXT_CHECKPOINT_ACCEPTANCE_ITEMS
        );
        assert_eq!(
            state.active_task_write_scope.len(),
            MAX_CONTEXT_CHECKPOINT_WRITE_SCOPE_ITEMS
        );
        assert_eq!(state.recent_completed_task_ids.len(), 16);
        assert!(
            serde_json::to_vec(&state).unwrap().len() <= MAX_CONTEXT_CHECKPOINT_STRUCTURED_BYTES
        );
    }

    #[test]
    fn checkpoint_validation_rejects_modes_and_empty_summaries() {
        let mut value = checkpoint("ctx-1");
        value.mode = "unknown".into();
        assert!(bound_context_checkpoint(value).is_err());

        let mut value = checkpoint("ctx-1");
        value.summary = Some("   ".into());
        assert!(bound_context_checkpoint(value).is_err());

        let mut value = checkpoint("ctx-1");
        value.mode = "trim".into();
        value.summary = None;
        assert!(bound_context_checkpoint(value).is_ok());

        let mut value = checkpoint("ctx-1");
        value.session_id.clear();
        assert!(bound_context_checkpoint(value).is_err());
    }

    #[test]
    fn checkpoint_events_rebuild_and_delete_projection() {
        let first = checkpoint("ctx-1");
        let second = checkpoint("ctx-2");
        let events = vec![
            event(
                1,
                CONTEXT_CHECKPOINT_CREATED_EVENT,
                serde_json::to_value(&first).unwrap(),
            ),
            event(
                2,
                CONTEXT_CHECKPOINT_CREATED_EVENT,
                serde_json::to_value(&second).unwrap(),
            ),
            event(
                3,
                CONTEXT_CHECKPOINT_DELETED_EVENT,
                serde_json::json!({"checkpoint_id": "ctx-1"}),
            ),
        ];
        let rebuilt = checkpoints_from_events(&events).unwrap();
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(rebuilt[0].id, "ctx-2");
        assert!(context_checkpoints_match(&rebuilt, &[second]));
    }

    #[test]
    fn malformed_checkpoint_event_fails_replay_closed() {
        let events = vec![event(
            1,
            CONTEXT_CHECKPOINT_CREATED_EVENT,
            serde_json::json!({"id": "ctx-1"}),
        )];
        assert!(checkpoints_from_events(&events).is_none());
    }

    #[test]
    fn restores_show_the_active_checkpoint_reference() {
        let events = vec![
            event(
                1,
                "context.compacted",
                serde_json::json!({"context_checkpoint_id": "ctx-1"}),
            ),
            event(
                2,
                "context.compacted",
                serde_json::json!({"restored_from_checkpoint": "ctx-2"}),
            ),
        ];
        assert_eq!(
            active_context_checkpoint_id(&events).as_deref(),
            Some("ctx-2")
        );
        assert!(
            active_context_checkpoint_id(&[event(1, "context.compacted", serde_json::json!({}))])
                .is_none()
        );
    }

    #[test]
    fn preview_is_single_line_and_bounded() {
        let preview = context_checkpoint_preview(Some("a\nb\t c"), 20);
        assert_eq!(preview, "a b c");
        let long = "x".repeat(100);
        let preview = context_checkpoint_preview(Some(&long), 10);
        assert_eq!(preview.chars().count(), 11);
        assert!(preview.ends_with('…'));
        assert!(context_checkpoint_preview(None, 10).is_empty());
    }

    #[test]
    fn policies_are_bounded() {
        let mut policy = vp_protocol::types::ContextCheckpointPolicy::default();
        assert!(validate_context_checkpoint_policy(&policy).is_ok());
        policy.max_checkpoints = 0;
        assert!(validate_context_checkpoint_policy(&policy).is_err());
        policy.max_checkpoints = 101;
        assert!(validate_context_checkpoint_policy(&policy).is_err());
        policy.max_checkpoints = 10;
        policy.max_age_days = 366;
        assert!(validate_context_checkpoint_policy(&policy).is_err());
        policy.max_age_days = 0;
        assert!(validate_context_checkpoint_policy(&policy).is_ok());
    }

    #[test]
    fn nested_checkpoint_payloads_are_accepted() {
        let value = checkpoint("ctx-9");
        let event = event(
            1,
            CONTEXT_CHECKPOINT_CREATED_EVENT,
            serde_json::json!({"checkpoint": value}),
        );
        let decoded = context_checkpoint_from_event(&event).unwrap();
        assert_eq!(decoded.id, "ctx-9");
    }

    #[test]
    fn non_checkpoint_events_are_ignored() {
        let event = event(1, "message.created", serde_json::json!({}));
        assert!(context_checkpoint_from_event(&event).is_none());
    }

    #[test]
    fn restore_mode_follows_the_recorded_boundary_not_summary_presence() {
        let mut summary = checkpoint("ctx-summary");
        assert_eq!(context_checkpoint_restore_mode(&summary), "summary");

        let mut trim = checkpoint("ctx-trim");
        trim.mode = "trim".into();
        // A trim checkpoint can carry the authoritative structured-state
        // block in `summary`; the loader must not inject it as a summary.
        assert_eq!(context_checkpoint_restore_mode(&trim), "trim");

        let mut manual = checkpoint("ctx-manual");
        manual.mode = "manual".into();
        manual.summary = None;
        assert_eq!(context_checkpoint_restore_mode(&manual), "trim");

        manual.summary = Some("summary".into());
        assert_eq!(context_checkpoint_restore_mode(&manual), "summary");
        summary.summary = None;
        assert_eq!(context_checkpoint_restore_mode(&summary), "summary");
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    fn base_checkpoint() -> ContextCheckpoint {
        ContextCheckpoint {
            id: "ctx-boundary".into(),
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            label: "boundary".into(),
            mode: "manual".into(),
            summary: None,
            through_message_id: None,
            message_cursor: 0,
            plan_revision: 0,
            provider: "queue".into(),
            model: "test".into(),
            prompt_prefix_hash: "prefix".into(),
            worktree_head: "head".into(),
            tokens_before: 0,
            tokens_after: 0,
            structured_state: None,
            source_compaction_seq: None,
            automatic: false,
            created_at: String::new(),
        }
    }

    #[test]
    fn unicode_truncation_never_splits_a_code_point() {
        let value = "aé中🎯z";
        for limit in 0..value.len() {
            let truncated = truncate_at_char_boundary(value, limit);
            assert!(value.starts_with(truncated));
            assert!(truncated.is_char_boundary(truncated.len()));
        }
        assert_eq!(truncate_at_char_boundary(value, value.len()), value);
        assert_eq!(truncate_at_char_boundary(value, value.len() + 1), value);
    }

    #[test]
    fn checkpoint_defaults_created_at_mode_and_label() {
        let mut checkpoint = base_checkpoint();
        checkpoint.label = "   ".into();
        checkpoint.created_at.clear();
        checkpoint.mode.clear();
        let bounded = bound_context_checkpoint(checkpoint).unwrap();
        assert!(bounded.label.starts_with("context checkpoint "));
        assert_eq!(bounded.mode, "manual");
        assert!(!bounded.created_at.is_empty());
    }

    #[test]
    fn checkpoint_trims_empty_through_and_identity_fields() {
        let mut checkpoint = base_checkpoint();
        checkpoint.through_message_id = Some("   ".into());
        checkpoint.provider = "  queue  ".into();
        checkpoint.model = "  test  ".into();
        checkpoint.prompt_prefix_hash = "  prefix  ".into();
        checkpoint.worktree_head = "  head  ".into();
        let bounded = bound_context_checkpoint(checkpoint).unwrap();
        assert!(bounded.through_message_id.is_none());
        assert_eq!(bounded.provider, "queue");
        assert_eq!(bounded.model, "test");
        assert_eq!(bounded.prompt_prefix_hash, "prefix");
        assert_eq!(bounded.worktree_head, "head");
    }

    #[test]
    fn message_cursor_upper_bound_is_enforced() {
        let mut checkpoint = base_checkpoint();
        checkpoint.message_cursor = 10_000_001;
        assert!(
            bound_context_checkpoint(checkpoint)
                .unwrap_err()
                .to_string()
                .contains("out of range")
        );
    }

    #[test]
    fn structured_state_lists_are_filtered_and_clamped() {
        let mut checkpoint = base_checkpoint();
        checkpoint.structured_state = Some(ContextStructuredState {
            goal: "  goal  ".into(),
            plan_revision: 4,
            active_task_id: Some("   ".into()),
            active_task_title: Some("  title  ".into()),
            active_task_department: Some(" dept ".into()),
            active_task_acceptance: vec!["  keep  ".into(), "   ".into(), "x".repeat(1_000)],
            active_task_write_scope: vec!["  src  ".into(), "   ".into()],
            recent_completed_task_ids: vec!["  T1  ".into(), " ".into()],
            generated_at: "  now  ".into(),
        });
        let bounded = bound_context_checkpoint(checkpoint).unwrap();
        let state = bounded.structured_state.unwrap();
        assert_eq!(state.goal, "goal");
        assert!(state.active_task_id.is_none());
        assert_eq!(state.active_task_title.as_deref(), Some("title"));
        assert_eq!(state.active_task_department.as_deref(), Some("dept"));
        assert_eq!(
            state.active_task_acceptance,
            vec!["keep".to_string(), "x".repeat(500)]
        );
        assert_eq!(state.active_task_write_scope, vec!["src".to_string()]);
        assert_eq!(state.recent_completed_task_ids, vec!["T1".to_string()]);
        assert_eq!(state.generated_at, "now");
    }

    #[test]
    fn checkpoint_events_apply_latest_row_and_unknown_deletes() {
        let first = base_checkpoint();
        let mut updated = first.clone();
        updated.label = "updated".into();
        let events = vec![
            Event {
                session_id: "ses-1".into(),
                seq: 1,
                kind: CONTEXT_CHECKPOINT_CREATED_EVENT.into(),
                payload: serde_json::to_value(&first).unwrap(),
                created_at: "2026-11-11T00:00:01Z".into(),
            },
            Event {
                session_id: "ses-1".into(),
                seq: 2,
                kind: CONTEXT_CHECKPOINT_CREATED_EVENT.into(),
                payload: serde_json::to_value(&updated).unwrap(),
                created_at: "2026-11-11T00:00:02Z".into(),
            },
            Event {
                session_id: "ses-1".into(),
                seq: 3,
                kind: CONTEXT_CHECKPOINT_DELETED_EVENT.into(),
                payload: serde_json::json!({"checkpoint_id": "unknown"}),
                created_at: "2026-11-11T00:00:03Z".into(),
            },
        ];
        let rows = checkpoints_from_events(&events).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "updated");
        assert!(context_checkpoints_match(&rows, &[updated]));
        assert!(!context_checkpoints_match(&rows, &[first]));
    }

    #[test]
    fn checkpoint_projection_comparison_is_order_sensitive() {
        let first = base_checkpoint();
        let mut second = first.clone();
        second.id = "ctx-2".into();
        second.created_at = "2026-11-11T00:00:01Z".into();
        assert!(!context_checkpoints_match(
            &[second.clone(), first.clone()],
            &[first.clone(), second.clone()]
        ));
        assert!(context_checkpoints_match(
            &[first.clone(), second.clone()],
            &[first, second]
        ));
    }

    #[test]
    fn active_checkpoint_reference_uses_the_latest_compaction() {
        let events = vec![
            Event {
                session_id: "ses-1".into(),
                seq: 1,
                kind: "context.compacted".into(),
                payload: serde_json::json!({"context_checkpoint_id": "ctx-1"}),
                created_at: "2026-11-11T00:00:01Z".into(),
            },
            Event {
                session_id: "ses-1".into(),
                seq: 2,
                kind: "context.compacted".into(),
                payload: serde_json::json!({"context_checkpoint_id": "ctx-2"}),
                created_at: "2026-11-11T00:00:02Z".into(),
            },
            Event {
                session_id: "ses-1".into(),
                seq: 3,
                kind: "context.restored".into(),
                payload: serde_json::json!({"checkpoint_id": "ctx-1"}),
                created_at: "2026-11-11T00:00:03Z".into(),
            },
        ];
        assert_eq!(
            active_context_checkpoint_id(&events).as_deref(),
            Some("ctx-2")
        );
    }

    #[test]
    fn empty_checkpoint_error_is_actionable() {
        let error = empty_context_checkpoint_error("ctx-missing");
        assert!(error.to_string().contains("ctx-missing"));
    }

    #[test]
    fn policy_boundaries_are_inclusive() {
        let ok = vp_protocol::types::ContextCheckpointPolicy {
            enabled: true,
            max_checkpoints: MAX_CONTEXT_CHECKPOINT_POLICY_ITEMS,
            max_age_days: MAX_CONTEXT_CHECKPOINT_POLICY_AGE_DAYS,
        };
        assert!(validate_context_checkpoint_policy(&ok).is_ok());
        let zero = vp_protocol::types::ContextCheckpointPolicy {
            enabled: true,
            max_checkpoints: 1,
            max_age_days: 0,
        };
        assert!(validate_context_checkpoint_policy(&zero).is_ok());
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn preview_collapses_whitespace_and_preserves_short_unicode() {
        assert_eq!(
            context_checkpoint_preview(Some("  one\ttwo\nthree  "), 80),
            "one two three"
        );
        assert_eq!(context_checkpoint_preview(Some("中🎯é"), 80), "中🎯é");
        let long = "中".repeat(100);
        let preview = context_checkpoint_preview(Some(&long), 10);
        assert_eq!(preview.chars().count(), 11);
        assert!(preview.starts_with("中中"));
        assert!(preview.ends_with('…'));
    }
}

#[cfg(test)]
mod structured_state_shrink_tests {
    use super::*;

    #[test]
    fn oversized_authoritative_state_is_shrunk_to_the_event_budget() {
        let checkpoint = ContextCheckpoint {
            id: "ctx-large-state".into(),
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            label: "large".into(),
            mode: "manual".into(),
            summary: None,
            through_message_id: None,
            message_cursor: 0,
            plan_revision: 7,
            provider: "queue".into(),
            model: "test".into(),
            prompt_prefix_hash: "prefix".into(),
            worktree_head: "head".into(),
            tokens_before: 0,
            tokens_after: 0,
            structured_state: Some(ContextStructuredState {
                goal: "g".repeat(MAX_CONTEXT_CHECKPOINT_GOAL_CHARS),
                plan_revision: 7,
                active_task_id: Some("T7".into()),
                active_task_title: Some("t".repeat(300)),
                active_task_department: Some("d".repeat(200)),
                active_task_acceptance: (0..MAX_CONTEXT_CHECKPOINT_ACCEPTANCE_ITEMS)
                    .map(|index| format!("{index}-{}", "a".repeat(500)))
                    .collect(),
                active_task_write_scope: (0..MAX_CONTEXT_CHECKPOINT_WRITE_SCOPE_ITEMS)
                    .map(|index| format!("{index}-{}", "w".repeat(300)))
                    .collect(),
                recent_completed_task_ids: (0..16)
                    .map(|index| format!("{index}-{}", "r".repeat(200)))
                    .collect(),
                generated_at: "2026-11-11T00:00:00Z".into(),
            }),
            source_compaction_seq: None,
            automatic: false,
            created_at: String::new(),
        };
        let bounded = bound_context_checkpoint(checkpoint).unwrap();
        let state = bounded.structured_state.unwrap();
        assert_eq!(state.plan_revision, 7);
        assert_eq!(state.generated_at, "2026-11-11T00:00:00Z");
        assert!(
            serde_json::to_vec(&state).unwrap().len() <= MAX_CONTEXT_CHECKPOINT_STRUCTURED_BYTES
        );
    }
}
