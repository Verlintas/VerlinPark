//! Tool-output economy: deterministic spilling, bounded provider previews and
//! daemon-owned paging handles.
//!
//! Full tool output is operational state. It is stored under the daemon data
//! directory as a content-addressed blob (`tool-outputs/<sha256>.txt`) with a
//! SQLite metadata row. It is deliberately:
//!
//! - not a replay projection: replay/repair never compare it;
//! - not bundle content: `session.export` does not copy bodies or metadata,
//!   and an imported archive cannot resolve a source handle;
//! - not a workspace path: agents receive an opaque `out_…` handle and the
//!   daemon resolves it against metadata only.
//!
//! The provider-facing text is always bounded and ends with an explicit
//! continuation marker. When several results arrive in one assistant turn,
//! shaping is deterministic: forced spills are decided by the per-tool
//! threshold, then the largest remaining results are spilled first until the
//! turn preview budget is met. No shaping step splits a UTF-8 scalar.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde_json::json;
use vp_protocol::types::{
    ToolOutputEntry, ToolOutputListResult, ToolOutputPolicy, ToolOutputPruneResult,
    ToolOutputStatus,
};
use vp_store::Store;
use vp_store::store::{StoredToolOutput, ToolOutputPruneRequest};

use crate::tools::ToolOutput;

/// The legacy provider-facing tool cap. It remains the fallback when spilling
/// is disabled or unavailable, so a policy change can never make provider
/// requests unbounded.
pub const MAX_PROVIDER_TOOL_OUTPUT_BYTES: usize = 40_000;

/// Hard cap for one body stored in the tool-output store. Tool results larger
/// than this fall back to the legacy bounded preview and do not get a handle.
pub const MAX_STORED_TOOL_OUTPUT_BYTES: usize = vp_store::store::MAX_TOOL_OUTPUT_BLOB_BYTES;

/// Hard cap for one `read_tool_output` response body, excluding the short
/// header and continuation hint. Callers cannot raise it through arguments.
pub const MAX_READ_TOOL_OUTPUT_BYTES: u64 = 64 * 1024;
pub const DEFAULT_READ_TOOL_OUTPUT_BYTES: u64 = 32 * 1024;
pub const MAX_READ_TOOL_OUTPUT_LINES: u64 = 2_000;
pub const DEFAULT_READ_TOOL_OUTPUT_LINES: u64 = 200;

/// Tools whose result is more useful with the tail retained. A build log,
/// diagnostics report, or shell command usually puts the error at the end.
const HEAD_TAIL_TOOLS: &[&str] = &[
    "shell",
    "run_project_task",
    "remote_exec",
    "code_diagnostics",
    "browser",
    "computer",
];

const TOOL_THRESHOLD_MIN: u64 = 64;
const TOOL_THRESHOLD_MAX: u64 = 64 * 1024 * 1024;

const MIN_POLICY_SPILL: u64 = 256;
const MAX_POLICY_SPILL: u64 = 16 * 1024 * 1024;
const MIN_POLICY_PREVIEW: u64 = 64;
const MAX_POLICY_PREVIEW: u64 = 1024 * 1024;
const MIN_POLICY_TURN: u64 = 1024;
const MAX_POLICY_TURN: u64 = 64 * 1024 * 1024;
const MIN_RETENTION: u64 = 60;
const MAX_RETENTION: u64 = 31_536_000;
const MIN_ENTRIES: u64 = 1;
const MAX_ENTRIES: u64 = 1_000_000;

/// A handle returned by the store plus the metadata needed to build a bounded
/// provider preview. It intentionally does not contain the full body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpilledToolOutput {
    pub handle: String,
    pub sha256: String,
    pub bytes: u64,
    pub lines: u64,
    pub preview: String,
    pub preview_bytes: u64,
    pub created_at: String,
    pub expires_at: Option<String>,
}

impl SpilledToolOutput {
    pub fn entry(
        &self,
        session_id: &str,
        tool_call_id: Option<&str>,
        tool_name: &str,
    ) -> ToolOutputEntry {
        ToolOutputEntry {
            id: self.handle.clone(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.map(str::to_string),
            tool_name: tool_name.to_string(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
            lines: self.lines,
            preview_bytes: self.preview_bytes,
            created_at: self.created_at.clone(),
            expires_at: self.expires_at.clone(),
        }
    }
}

/// Thin façade over the SQLite metadata registry and the content-addressed
/// blob directory. All methods are blocking; the runtime calls them inside
/// `spawn_blocking`.
#[derive(Clone)]
pub struct ToolOutputStore {
    store: Store,
}

impl ToolOutputStore {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub fn policy(&self, project_id: &str) -> Result<ToolOutputPolicy> {
        self.store.get_tool_output_policy(project_id)
    }

    pub fn session_project_id(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self
            .store
            .get_session(session_id)?
            .map(|session| session.project_id))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spill(
        &self,
        project_id: &str,
        session_id: &str,
        tool_call_id: Option<&str>,
        tool_name: &str,
        content: &str,
        preview: &str,
        retention_secs: u64,
    ) -> Result<SpilledToolOutput> {
        if tool_name.trim().is_empty() || tool_name.len() > 64 {
            bail!("tool-output tool name must be 1..=64 bytes");
        }
        if tool_call_id.is_some_and(|id| id.len() > 200) {
            bail!("tool-output call id must be at most 200 bytes");
        }
        if content.len() > MAX_STORED_TOOL_OUTPUT_BYTES {
            bail!(
                "tool output is {} bytes; the store limit is {} bytes",
                content.len(),
                MAX_STORED_TOOL_OUTPUT_BYTES
            );
        }
        let stored = self.store.spill_tool_output(
            project_id,
            session_id,
            tool_call_id,
            tool_name,
            content,
            preview,
            retention_secs,
        )?;
        Ok(SpilledToolOutput {
            handle: stored.entry.id,
            sha256: stored.entry.sha256,
            bytes: stored.entry.bytes,
            lines: stored.entry.lines,
            preview: stored.preview,
            preview_bytes: stored.entry.preview_bytes,
            created_at: stored.entry.created_at,
            expires_at: stored.entry.expires_at,
        })
    }

    /// Resolve one handle for a session. Foreign handles are reported as
    /// invalid, not as an existence oracle.
    pub fn read_for_session(&self, handle: &str, session_id: &str) -> Result<StoredToolOutput> {
        validate_handle(handle)?;
        let Some(record) = self.store.get_tool_output(handle)? else {
            bail!("unknown tool output handle");
        };
        if record.entry.session_id != session_id {
            bail!("unknown tool output handle");
        }
        if record
            .entry
            .expires_at
            .as_deref()
            .is_some_and(|expires| expires <= vp_protocol::now_rfc3339().as_str())
        {
            // Expired handles are operationally gone even if the sweep has not
            // removed the row yet. Do not extend a handle's life by reading it.
            bail!("unknown tool output handle");
        }
        Ok(record)
    }

    pub fn read_text(&self, record: &StoredToolOutput) -> Result<String> {
        self.store.read_tool_output_text(record)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn list(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
        tool_name: Option<&str>,
        before_created_at: Option<&str>,
        include_expired: bool,
        limit: usize,
    ) -> Result<ToolOutputListResult> {
        let limit = limit.clamp(1, 500) as u32;
        let total = self.store.tool_output_total(project_id, session_id)?;
        // Fetch one extra row so the cursor is honest even when the total is
        // filtered by tool, time, or expiry.
        let mut entries = self
            .store
            .list_tool_outputs(
                project_id,
                session_id,
                tool_name,
                before_created_at,
                include_expired,
                limit as usize + 1,
            )?
            .into_iter()
            .map(|stored| stored.entry)
            .collect::<Vec<_>>();
        let truncated = entries.len() > limit as usize;
        if truncated {
            entries.truncate(limit as usize);
        }
        let next_before = if truncated {
            entries.last().map(|entry| entry.created_at.clone())
        } else {
            None
        };
        Ok(ToolOutputListResult {
            truncated,
            entries,
            total,
            next_before,
        })
    }

    pub fn status(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
        policy: ToolOutputPolicy,
    ) -> Result<ToolOutputStatus> {
        let stats = self.store.tool_output_stats(project_id, session_id)?;
        let by_tool = self.store.tool_output_tool_stats(project_id, session_id)?;
        Ok(ToolOutputStatus {
            project_id: project_id.map(str::to_string),
            session_id: session_id.map(str::to_string),
            policy,
            entries: stats.entries,
            total_bytes: stats.total_bytes,
            preview_bytes: stats.preview_bytes,
            active_entries: stats.active_entries,
            expired_entries: stats.expired_entries,
            oldest_created_at: stats.oldest_created_at,
            newest_created_at: stats.newest_created_at,
            by_tool,
        })
    }

    pub fn prune(&self, request: &ToolOutputPruneRequest) -> Result<ToolOutputPruneResult> {
        self.store.prune_tool_outputs(request)
    }

    pub fn audit(
        &self,
        project_id: Option<&str>,
        verify_hashes: bool,
        clean_orphans: bool,
    ) -> Result<vp_protocol::types::ToolOutputAuditResult> {
        self.store
            .audit_tool_outputs(project_id, verify_hashes, clean_orphans)
    }

    /// Opportunistic cap enforcement after a spill batch. Active sessions are
    /// protected by the store; a project can temporarily exceed `max_entries`
    /// when every candidate belongs to a live run. The daemon sweep retries
    /// later.
    pub fn enforce_limits(
        &self,
        project_id: &str,
        policy: &ToolOutputPolicy,
    ) -> Result<ToolOutputPruneResult> {
        self.store.prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project_id.to_string()),
            session_id: None,
            older_than_secs: Some(policy.retention_secs),
            max_entries: Some(policy.max_entries),
            dry_run: false,
        })
    }

    pub fn data_dir(&self) -> &Path {
        self.store.data_dir()
    }
}

/// Validate all policy bounds in one place so RPC, CLI, manifest import, and
/// direct store writes agree. Unknown per-tool names are retained for forward
/// compatibility but their values are still bounded.
pub fn validate_tool_output_policy(policy: &ToolOutputPolicy) -> Result<()> {
    if !(MIN_POLICY_SPILL..=MAX_POLICY_SPILL).contains(&policy.spill_threshold_bytes) {
        bail!(
            "tool-output spill_threshold_bytes must be between {} and {}",
            MIN_POLICY_SPILL,
            MAX_POLICY_SPILL
        );
    }
    if !(MIN_POLICY_PREVIEW..=MAX_POLICY_PREVIEW).contains(&policy.preview_bytes) {
        bail!(
            "tool-output preview_bytes must be between {} and {}",
            MIN_POLICY_PREVIEW,
            MAX_POLICY_PREVIEW
        );
    }
    if policy.preview_bytes > policy.spill_threshold_bytes {
        bail!("tool-output preview_bytes must not exceed spill_threshold_bytes");
    }
    if !(MIN_POLICY_TURN..=MAX_POLICY_TURN).contains(&policy.max_total_bytes_per_turn) {
        bail!(
            "tool-output max_total_bytes_per_turn must be between {} and {}",
            MIN_POLICY_TURN,
            MAX_POLICY_TURN
        );
    }
    if policy.max_total_bytes_per_turn < policy.preview_bytes {
        bail!("tool-output max_total_bytes_per_turn must not be smaller than preview_bytes");
    }
    if !(MIN_RETENTION..=MAX_RETENTION).contains(&policy.retention_secs) {
        bail!(
            "tool-output retention_secs must be between {} and {}",
            MIN_RETENTION,
            MAX_RETENTION
        );
    }
    if !(MIN_ENTRIES..=MAX_ENTRIES).contains(&policy.max_entries) {
        bail!(
            "tool-output max_entries must be between {} and {}",
            MIN_ENTRIES,
            MAX_ENTRIES
        );
    }
    for (tool, threshold) in &policy.tool_thresholds {
        if tool.trim().is_empty() {
            bail!("tool-output per-tool threshold names must not be empty");
        }
        if !(TOOL_THRESHOLD_MIN..=TOOL_THRESHOLD_MAX).contains(threshold) {
            bail!(
                "tool-output threshold for '{tool}' must be between {} and {} bytes",
                TOOL_THRESHOLD_MIN,
                TOOL_THRESHOLD_MAX
            );
        }
    }
    Ok(())
}

pub fn effective_spill_threshold(policy: &ToolOutputPolicy, tool_name: &str) -> u64 {
    policy
        .tool_thresholds
        .get(tool_name)
        .copied()
        .unwrap_or(policy.spill_threshold_bytes)
}

pub fn effective_preview_bytes(policy: &ToolOutputPolicy, tool_name: &str) -> u64 {
    let threshold = effective_spill_threshold(policy, tool_name);
    policy
        .preview_bytes
        .min(threshold)
        .min(policy.max_total_bytes_per_turn)
}

fn uses_head_tail(tool_name: &str) -> bool {
    HEAD_TAIL_TOOLS.contains(&tool_name)
}

/// True if an output is large enough for the store path. The comparison is
/// strictly greater-than so an output exactly at the threshold stays inline.
pub fn exceeds_spill_threshold(policy: &ToolOutputPolicy, tool_name: &str, bytes: usize) -> bool {
    policy.enabled && bytes as u64 > effective_spill_threshold(policy, tool_name)
}

fn valid_handle_shape(handle: &str) -> bool {
    let Some(rest) = handle.strip_prefix("out_") else {
        return false;
    };
    if rest.len() != 36 {
        return false;
    }
    rest.bytes().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    })
}

pub fn validate_handle(handle: &str) -> Result<()> {
    if !valid_handle_shape(handle) {
        bail!("invalid tool output handle");
    }
    Ok(())
}

/// Number of logical lines. Empty output has zero lines; a trailing newline
/// does not create a phantom line for paging purposes.
pub fn line_count(text: &str) -> u64 {
    text.lines().count() as u64
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Produce a bounded preview body. Head-only tools keep the beginning.
/// Head+tail tools keep both ends and insert a short elision marker. The
/// returned string is always valid UTF-8 because every cut is moved to a
/// scalar boundary.
fn preview_body(text: &str, budget: u64, head_tail: bool) -> String {
    let budget = budget as usize;
    if text.len() <= budget {
        return text.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    if !head_tail || budget < 16 {
        let end = floor_char_boundary(text, budget);
        return text[..end].to_string();
    }
    const SEPARATOR: &str = "\n…\n";
    let split = (budget.saturating_sub(SEPARATOR.len())) / 2;
    let head_end = floor_char_boundary(text, split);
    let tail_budget = budget
        .saturating_sub(SEPARATOR.len())
        .saturating_sub(head_end);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(tail_budget));
    let mut preview = String::with_capacity(head_end + tail_budget + SEPARATOR.len());
    preview.push_str(&text[..head_end]);
    preview.push_str(SEPARATOR);
    preview.push_str(&text[tail_start..]);
    preview
}

fn continuation_marker(handle: &str, preview_bytes: u64, total_bytes: u64, lines: u64) -> String {
    format!(
        "[output truncated: showing {preview_bytes} of {total_bytes} bytes ({lines} lines); use read_tool_output handle={handle}]"
    )
}

fn legacy_marker(shown: u64, total: u64) -> String {
    format!(
        "[output truncated: showing {shown} of {total} bytes; request a bounded section with the tool's offset/limit parameters]"
    )
}

/// One event-shaped observation produced by shaping. It contains sizes and
/// hashes only; never output content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutputNoticeKind {
    Spilled,
    Truncated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputNotice {
    pub kind: ToolOutputNoticeKind,
    pub tool_call_id: String,
    pub tool_name: String,
    pub handle: Option<String>,
    pub sha256: String,
    pub total_bytes: u64,
    pub returned_bytes: u64,
    pub total_lines: u64,
    pub returned_lines: u64,
    pub preview_bytes: u64,
    pub reason: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShapeReport {
    pub notices: Vec<ToolOutputNotice>,
    pub provider_bytes: u64,
}

/// A tool result waiting for shaping. `output` is moved through the shaper so
/// a large full body is not cloned unnecessarily.
#[derive(Debug)]
pub struct PendingToolOutput {
    pub tool_call_id: String,
    pub tool_name: String,
    pub output: ToolOutput,
}

fn combined_sha(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Shape a batch of tool results for one assistant turn.
///
/// `prior_turn_bytes` is the provider-facing byte total already emitted for
/// earlier tool results in the same assistant turn. The function mutates each
/// `ToolOutput` in place and returns the notices the caller must emit after
/// persistence. Store failures degrade to the legacy bounded preview rather
/// than failing the agent run; the notice records the error.
pub fn shape_tool_outputs(
    store: Option<&ToolOutputStore>,
    policy: &ToolOutputPolicy,
    session_id: &str,
    project_id: &str,
    prior_turn_bytes: u64,
    pending: &mut [PendingToolOutput],
) -> Result<ShapeReport> {
    if pending.is_empty() {
        return Ok(ShapeReport::default());
    }
    if !policy.enabled {
        return Ok(shape_legacy(pending, prior_turn_bytes));
    }

    let mut full_texts = pending
        .iter_mut()
        .map(|item| {
            item.output
                .full_content
                .take()
                .unwrap_or_else(|| item.output.content.clone())
        })
        .collect::<Vec<_>>();
    let sizes = full_texts
        .iter()
        .map(|text| text.len() as u64)
        .collect::<Vec<_>>();
    let preview_caps = pending
        .iter()
        .enumerate()
        .map(|(index, item)| effective_preview_bytes(policy, &item.tool_name).min(sizes[index]))
        .collect::<Vec<_>>();

    let mut spilled = vec![false; pending.len()];
    for (index, item) in pending.iter().enumerate() {
        if item.output.output_ref.is_some() {
            continue;
        }
        if exceeds_spill_threshold(policy, &item.tool_name, sizes[index] as usize) {
            spilled[index] = true;
        }
    }

    let retained_bytes = |spilled: &[bool]| -> u64 {
        sizes
            .iter()
            .enumerate()
            .map(|(index, size)| {
                if spilled[index] {
                    preview_caps[index].min(*size)
                } else {
                    *size
                }
            })
            .fold(prior_turn_bytes, |total, value| total.saturating_add(value))
    };
    let mut retained = retained_bytes(&spilled);
    while retained > policy.max_total_bytes_per_turn {
        let candidate = pending
            .iter()
            .enumerate()
            .filter(|(index, item)| !spilled[*index] && item.output.output_ref.is_none())
            .max_by(|(left_index, _), (right_index, _)| {
                sizes[*left_index]
                    .cmp(&sizes[*right_index])
                    // Largest first; earlier tool call wins a size tie.
                    .then_with(|| right_index.cmp(left_index))
            })
            .map(|(index, _)| index);
        let Some(candidate) = candidate else {
            break;
        };
        spilled[candidate] = true;
        retained = retained_bytes(&spilled);
    }

    // Allocate preview bodies in tool-call order from the remaining turn
    // budget. Forced spills already account for size; preview space is shared
    // fairly and deterministically.
    let mut remaining_turn = policy
        .max_total_bytes_per_turn
        .saturating_sub(prior_turn_bytes);
    let mut report = ShapeReport::default();
    for index in 0..pending.len() {
        let item = &mut pending[index];
        if item.output.output_ref.is_some() {
            report.provider_bytes = report
                .provider_bytes
                .saturating_add(item.output.content.len() as u64);
            continue;
        }
        if !spilled[index] {
            report.provider_bytes = report
                .provider_bytes
                .saturating_add(full_texts[index].len() as u64);
            continue;
        }
        let full = std::mem::take(&mut full_texts[index]);
        let sha256 = combined_sha(&full);
        let total_bytes = full.len() as u64;
        let total_lines = line_count(&full);
        let desired = preview_caps[index].min(total_bytes);
        let allocated = desired.min(remaining_turn);
        let body = preview_body(&full, allocated, uses_head_tail(&item.tool_name));
        let preview_bytes = body.len() as u64;
        let spill_result = match store {
            Some(store) => store.spill(
                project_id,
                session_id,
                Some(item.tool_call_id.as_str()),
                &item.tool_name,
                &full,
                &body,
                policy.retention_secs,
            ),
            None => Err(anyhow!("tool-output store is not available")),
        };
        match spill_result {
            Err(error) => {
                // Fail open to the legacy bounded rendering, but keep an
                // explicit no-handle marker so the model knows the content is
                // reduced. The run is not aborted by a full disk or a
                // pathological result larger than the store cap.
                let body = preview_body(&full, MAX_PROVIDER_TOOL_OUTPUT_BYTES as u64, false);
                let content = if body.is_empty() {
                    legacy_marker(0, total_bytes)
                } else {
                    format!(
                        "{body}\n\n{}",
                        legacy_marker(body.len() as u64, total_bytes)
                    )
                };
                item.output.content = content;
                item.output.output_ref = None;
                item.output.truncated = true;
                item.output.total_bytes = total_bytes;
                item.output.preview_bytes = body.len() as u64;
                item.output.lines = total_lines;
                report.provider_bytes = report
                    .provider_bytes
                    .saturating_add(item.output.content.len() as u64);
                report.notices.push(ToolOutputNotice {
                    kind: ToolOutputNoticeKind::Truncated,
                    tool_call_id: item.tool_call_id.clone(),
                    tool_name: item.tool_name.clone(),
                    handle: None,
                    sha256,
                    total_bytes,
                    returned_bytes: item.output.content.len() as u64,
                    total_lines,
                    returned_lines: line_count(&item.output.content),
                    preview_bytes: item.output.preview_bytes,
                    reason: "store_error".to_string(),
                    error: Some(format!("{error:#}")),
                });
            }
            Ok(spill) => {
                let marker =
                    continuation_marker(&spill.handle, preview_bytes, total_bytes, total_lines);
                let content = if body.is_empty() {
                    marker
                } else {
                    format!("{body}\n\n{marker}")
                };
                item.output.content = content;
                item.output.output_ref = Some(spill.handle.clone());
                item.output.truncated = true;
                item.output.total_bytes = total_bytes;
                item.output.preview_bytes = preview_bytes;
                item.output.lines = total_lines;
                remaining_turn = remaining_turn.saturating_sub(preview_bytes);
                report.provider_bytes = report
                    .provider_bytes
                    .saturating_add(item.output.content.len() as u64);
                let below_threshold =
                    total_bytes <= effective_spill_threshold(policy, &item.tool_name);
                let reason = if below_threshold {
                    "turn_budget"
                } else {
                    "threshold"
                };
                let returned_bytes = item.output.content.len() as u64;
                let returned_lines = line_count(&item.output.content);
                report.notices.push(ToolOutputNotice {
                    kind: ToolOutputNoticeKind::Spilled,
                    tool_call_id: item.tool_call_id.clone(),
                    tool_name: item.tool_name.clone(),
                    handle: Some(spill.handle.clone()),
                    sha256: spill.sha256.clone(),
                    total_bytes,
                    returned_bytes,
                    total_lines,
                    returned_lines,
                    preview_bytes,
                    reason: reason.to_string(),
                    error: None,
                });
                report.notices.push(ToolOutputNotice {
                    kind: ToolOutputNoticeKind::Truncated,
                    tool_call_id: item.tool_call_id.clone(),
                    tool_name: item.tool_name.clone(),
                    handle: Some(spill.handle),
                    sha256: spill.sha256,
                    total_bytes,
                    returned_bytes,
                    total_lines,
                    returned_lines,
                    preview_bytes,
                    reason: "spilled".to_string(),
                    error: None,
                });
            }
        }
    }
    if spilled.iter().any(|spilled| *spilled)
        && let Some(store) = store
    {
        // Best-effort cap enforcement. A full disk or a lock error must not
        // turn a successful tool call into a run failure.
        let _ = store.enforce_limits(project_id, policy);
    }
    Ok(report)
}

fn shape_legacy(pending: &mut [PendingToolOutput], prior_turn_bytes: u64) -> ShapeReport {
    let mut report = ShapeReport::default();
    let mut used = prior_turn_bytes;
    for item in pending {
        if item.output.output_ref.is_some() {
            used = used.saturating_add(item.output.content.len() as u64);
            report.provider_bytes = used.saturating_sub(prior_turn_bytes);
            continue;
        }
        let full = item
            .output
            .full_content
            .take()
            .unwrap_or_else(|| item.output.content.clone());
        let total_bytes = full.len() as u64;
        let total_lines = line_count(&full);
        if full.len() > MAX_PROVIDER_TOOL_OUTPUT_BYTES {
            let body = preview_body(&full, MAX_PROVIDER_TOOL_OUTPUT_BYTES as u64, false);
            let content = format!(
                "{body}\n\n{}",
                legacy_marker(body.len() as u64, total_bytes)
            );
            item.output.content = content;
            item.output.truncated = true;
            item.output.total_bytes = total_bytes;
            item.output.preview_bytes = body.len() as u64;
            item.output.lines = total_lines;
            report.notices.push(ToolOutputNotice {
                kind: ToolOutputNoticeKind::Truncated,
                tool_call_id: item.tool_call_id.clone(),
                tool_name: item.tool_name.clone(),
                handle: None,
                sha256: combined_sha(&full),
                total_bytes,
                returned_bytes: item.output.content.len() as u64,
                total_lines,
                returned_lines: line_count(&item.output.content),
                preview_bytes: body.len() as u64,
                reason: "policy_disabled".to_string(),
                error: None,
            });
        }
        used = used.saturating_add(item.output.content.len() as u64);
        report.provider_bytes = used.saturating_sub(prior_turn_bytes);
    }
    report
}

// ---------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadToolOutputParams {
    pub handle: String,
    /// Zero-based line offset in the filtered line set.
    pub offset: Option<u64>,
    pub limit: Option<u64>,
    pub max_bytes: Option<u64>,
    /// Offset from the end instead of the beginning. The returned lines are
    /// still rendered in chronological order.
    pub from_end: bool,
    /// Literal, case-sensitive substring filter.
    pub pattern: Option<String>,
}

fn parse_u64(input: &serde_json::Value, key: &str) -> Result<Option<u64>> {
    match input.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow!("{key} must be a non-negative integer")),
    }
}

impl ReadToolOutputParams {
    pub fn from_input(input: &serde_json::Value) -> Result<Self> {
        let handle = input
            .get("handle")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|handle| !handle.is_empty())
            .ok_or_else(|| anyhow!("handle is required"))?
            .to_string();
        let pattern = input
            .get("pattern")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .filter(|pattern| !pattern.is_empty());
        if let Some(pattern) = &pattern
            && pattern.len() > 500
        {
            bail!("pattern is limited to 500 bytes");
        }
        Ok(Self {
            handle,
            offset: parse_u64(input, "offset")?,
            limit: parse_u64(input, "limit")?,
            max_bytes: parse_u64(input, "max_bytes")?,
            from_end: input
                .get("from_end")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            pattern,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputPage {
    pub content: String,
    pub structured: serde_json::Value,
}

/// Page one stored output. The returned content is bounded by the hard
/// `max_bytes` cap, never splits a UTF-8 scalar, and includes an explicit
/// continuation hint when more matching lines remain.
pub fn page_tool_output(
    store: &ToolOutputStore,
    session_id: &str,
    params: &ReadToolOutputParams,
) -> Result<ToolOutput> {
    validate_handle(&params.handle)?;
    let record = store.read_for_session(&params.handle, session_id)?;
    let text = store.read_text(&record)?;
    let total_lines = line_count(&text);
    let total_bytes = text.len() as u64;
    let max_bytes = params
        .max_bytes
        .unwrap_or(DEFAULT_READ_TOOL_OUTPUT_BYTES)
        .clamp(256, MAX_READ_TOOL_OUTPUT_BYTES);
    let limit = params
        .limit
        .unwrap_or(DEFAULT_READ_TOOL_OUTPUT_LINES)
        .clamp(1, MAX_READ_TOOL_OUTPUT_LINES);
    let offset = params.offset.unwrap_or(0);
    let pattern = params.pattern.as_deref();
    let lines = if text.is_empty() {
        Vec::new()
    } else {
        text.lines().collect::<Vec<_>>()
    };
    let matches = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| pattern.is_none_or(|needle| line.contains(needle)))
        .map(|(index, line)| (index, *line))
        .collect::<Vec<_>>();
    let selected: Vec<(usize, &str)> = if params.from_end {
        let end = matches.len().saturating_sub(offset as usize);
        let start = end.saturating_sub(limit as usize);
        matches.get(start..end).unwrap_or(&[]).to_vec()
    } else {
        matches
            .iter()
            .skip(offset as usize)
            .take(limit as usize)
            .copied()
            .collect()
    };
    let header = format!(
        "tool output {} · {} · {} bytes / {} lines{}\n",
        record.entry.id,
        record.entry.tool_name,
        total_bytes,
        total_lines,
        pattern
            .map(|pattern| format!(" · pattern {pattern:?} · {} matching lines", matches.len()))
            .unwrap_or_default()
    );
    let mut content = header;
    let body_budget = max_bytes.saturating_sub(content.len() as u64 + 320);
    let mut body_used = 0u64;
    let mut emitted = 0usize;
    for (line_index, line) in &selected {
        let rendered = format!("{:>6} | {line}\n", line_index + 1);
        let rendered_len = rendered.len() as u64;
        if body_used.saturating_add(rendered_len) > body_budget {
            if emitted == 0 && body_budget > 0 {
                let available = body_budget.saturating_sub(20) as usize;
                let end = floor_char_boundary(&rendered, available);
                content.push_str(&rendered[..end]);
                content.push_str("…\n");
                emitted += 1;
            }
            break;
        }
        content.push_str(&rendered);
        body_used = body_used.saturating_add(rendered_len);
        emitted += 1;
    }
    if selected.is_empty() {
        if matches.is_empty() && pattern.is_some() {
            content.push_str("No lines matched the literal pattern.\n");
        } else if text.is_empty() {
            content.push_str("The stored tool output is empty (0 bytes).\n");
        } else {
            content.push_str("No lines selected at this offset.\n");
        }
    }
    let consumed = offset as usize + emitted;
    let more_after = consumed < matches.len();
    let next_offset = offset + emitted as u64;
    if more_after {
        if params.from_end {
            content.push_str(&format!(
                "\n[more: {} earlier matching line(s) remain; continue with read_tool_output handle={} from_end=true offset={} limit={}]\n",
                matches.len().saturating_sub(offset as usize + emitted),
                record.entry.id,
                next_offset,
                limit
            ));
        } else {
            content.push_str(&format!(
                "\n[more: {} matching line(s) remain; continue with read_tool_output handle={} offset={} limit={}]\n",
                matches.len().saturating_sub(offset as usize + emitted),
                record.entry.id,
                next_offset,
                limit
            ));
        }
    }
    let returned_lines = emitted as u64;
    let returned_bytes = content.len() as u64;
    let structured = json!({
        "handle": record.entry.id,
        "tool_name": record.entry.tool_name,
        "total_bytes": total_bytes,
        "total_lines": total_lines,
        "matching_lines": matches.len(),
        "offset": offset,
        "from_end": params.from_end,
        "pattern": pattern,
        "returned_lines": returned_lines,
        "returned_bytes": returned_bytes,
        "next_offset": more_after.then_some(next_offset),
        "truncated": more_after,
        "max_bytes": max_bytes,
    });
    Ok(ToolOutput::ok_structured(content, structured))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_store::Store;

    fn fixture() -> (TempDir, TempDir, Store, String, String) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("tool-output"))
            .unwrap();
        let session = store.create_session(&project.id, "inspect").unwrap();
        (data, repo, store, project.id, session.id)
    }

    fn pending(name: &str, text: &str) -> PendingToolOutput {
        PendingToolOutput {
            tool_call_id: format!("call_{name}"),
            tool_name: name.to_string(),
            output: ToolOutput::ok(text),
        }
    }

    #[tokio::test]
    async fn engine_policy_status_list_prune_and_audit_round_trip() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let engine = crate::Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_str().unwrap().to_string(),
                Some("rpc".to_string()),
            )
            .await
            .unwrap();
        let session = engine.store().create_session(&project.id, "rpc").unwrap();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 8_192,
            preview_bytes: 2_048,
            retention_secs: 3_600,
            max_entries: 10,
            ..ToolOutputPolicy::default()
        };
        let stored = engine
            .set_tool_output_policy(&project.id, policy.clone())
            .await
            .unwrap();
        assert_eq!(stored.policy, policy);
        let loaded = engine.get_tool_output_policy(&project.id).await.unwrap();
        assert_eq!(loaded, policy);
        let output_store = ToolOutputStore::new(engine.store().clone());
        let spill = output_store
            .spill(
                &project.id,
                &session.id,
                Some("call_1"),
                "shell",
                "engine body",
                "",
                3_600,
            )
            .unwrap();
        let status = engine
            .tool_output_status(vp_protocol::types::ToolOutputStatusParams {
                project_id: Some(project.id.clone()),
                session_id: Some(session.id.clone()),
            })
            .await
            .unwrap();
        assert_eq!(status.entries, 1);
        assert_eq!(status.total_bytes, "engine body".len() as u64);
        assert_eq!(status.by_tool.len(), 1);
        assert_eq!(status.by_tool[0].tool_name, "shell");
        let listed = engine
            .list_tool_outputs(vp_protocol::types::ToolOutputListParams {
                project_id: Some(project.id.clone()),
                session_id: Some(session.id.clone()),
                tool_name: Some("shell".into()),
                before_created_at: None,
                include_expired: true,
                limit: Some(10),
            })
            .await
            .unwrap();
        assert_eq!(listed.entries.len(), 1);
        assert_eq!(listed.entries[0].id, spill.handle);
        let audit = engine
            .audit_tool_outputs(vp_protocol::types::ToolOutputAuditParams {
                project_id: Some(project.id.clone()),
                verify_hashes: true,
                clean_orphans: false,
            })
            .await
            .unwrap();
        assert_eq!(audit.rows, 1);
        assert_eq!(audit.missing_blobs, 0);
        assert_eq!(audit.hash_mismatches, 0);
        let pruned = engine
            .prune_tool_outputs(vp_protocol::types::ToolOutputPruneParams {
                project_id: Some(project.id),
                session_id: None,
                older_than_secs: Some(0),
                max_entries: None,
                dry_run: false,
            })
            .await
            .unwrap();
        assert_eq!(pruned.removed, 1);
    }

    #[test]
    fn exact_spill_threshold_is_strictly_greater_than() {
        let policy = ToolOutputPolicy::default();
        assert!(!exceeds_spill_threshold(&policy, "shell", 16 * 1024));
        assert!(exceeds_spill_threshold(&policy, "shell", 16 * 1024 + 1));
        let mut per_tool = policy.clone();
        per_tool.tool_thresholds.insert("shell".into(), 1024);
        assert!(!exceeds_spill_threshold(&per_tool, "shell", 1024));
        assert!(exceeds_spill_threshold(&per_tool, "shell", 1025));
        assert!(!exceeds_spill_threshold(&per_tool, "grep", 1024));
    }

    #[test]
    fn line_count_matches_paging_semantics() {
        assert_eq!(line_count(""), 0);
        assert_eq!(line_count("one"), 1);
        assert_eq!(line_count("one\n"), 1);
        assert_eq!(line_count("one\ntwo"), 2);
        assert_eq!(line_count("one\ntwo\n"), 2);
        assert_eq!(line_count("\n"), 1);
    }

    #[test]
    fn per_tool_overrides_control_threshold_and_preview() {
        let mut policy = ToolOutputPolicy::default();
        policy.tool_thresholds.insert("shell".into(), 32 * 1024);
        policy.tool_thresholds.insert("grep".into(), 1024);
        assert_eq!(effective_spill_threshold(&policy, "shell"), 32 * 1024);
        assert_eq!(effective_spill_threshold(&policy, "grep"), 1024);
        assert_eq!(
            effective_spill_threshold(&policy, "read_file"),
            policy.spill_threshold_bytes
        );
        assert_eq!(
            effective_preview_bytes(&policy, "shell"),
            policy.preview_bytes
        );
    }

    #[test]
    fn preserved_full_content_is_what_gets_spilled() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 1024,
            preview_bytes: 256,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store.clone());
        let full = format!("HEAD\n{}\nTAIL", "x".repeat(10_000));
        let output =
            ToolOutput::ok_structured_preserving(full.clone(), json!({"summary": "large"}), false);
        assert_eq!(output.total_bytes, full.len() as u64);
        let mut outputs = vec![PendingToolOutput {
            tool_call_id: "call_preserved".into(),
            tool_name: "shell".into(),
            output,
        }];
        shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        let stored = store
            .get_tool_output(outputs[0].output.output_ref.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            store.read_tool_output_text(&stored).unwrap(),
            full,
            "the store must receive the pre-truncation body"
        );
        assert!(outputs[0].output.content.contains("HEAD"));
        assert!(outputs[0].output.content.contains("TAIL"));
    }

    #[test]
    fn prior_turn_budget_forces_zero_byte_preview_but_keeps_a_handle() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 1024 * 1024,
            preview_bytes: 4096,
            max_total_bytes_per_turn: 4096,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store);
        let mut outputs = vec![pending("shell", &"y".repeat(2000))];
        let report = shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            4096,
            &mut outputs,
        )
        .unwrap();
        assert!(outputs[0].output.output_ref.is_some());
        assert_eq!(outputs[0].output.preview_bytes, 0);
        assert!(outputs[0].output.content.starts_with("[output truncated:"));
        assert_eq!(report.notices[0].preview_bytes, 0);
    }

    #[test]
    fn provider_content_always_ends_with_the_continuation_marker() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 1024,
            preview_bytes: 128,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store);
        for text in ["x".repeat(2000), "🙂".repeat(2000)] {
            let mut outputs = vec![pending("shell", &text)];
            shape_tool_outputs(
                Some(&output_store),
                &policy,
                &session,
                &project,
                0,
                &mut outputs,
            )
            .unwrap();
            assert!(
                outputs[0].output.content.ends_with(&format!(
                    "use read_tool_output handle={}]",
                    outputs[0].output.output_ref.as_deref().unwrap()
                )),
                "marker missing: {}",
                outputs[0].output.content
            );
        }
    }

    #[test]
    fn disabled_policy_never_creates_a_store_row() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            enabled: false,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store.clone());
        let mut outputs = vec![pending("shell", &"z".repeat(50_000))];
        shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        assert!(outputs[0].output.output_ref.is_none());
        assert!(
            store
                .list_tool_outputs(Some(&project), Some(&session), None, None, true, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn spill_rejects_unsafe_tool_metadata() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        assert!(
            store
                .spill(&project, &session, None, "", "body", "", 3600)
                .is_err()
        );
        assert!(
            store
                .spill(
                    &project,
                    &session,
                    Some(&"c".repeat(201)),
                    "shell",
                    "body",
                    "",
                    3600
                )
                .is_err()
        );
        assert!(
            store
                .spill(&project, &session, None, &"t".repeat(65), "body", "", 3600)
                .is_err()
        );
    }

    #[test]
    fn handle_shape_rejects_paths_and_near_misses() {
        for invalid in [
            "",
            "out_",
            "out_123",
            "out_00000000-0000-0000-0000-00000000000g",
            "out_000000000000000000000000000000000000",
            "../out_00000000-0000-0000-0000-000000000000",
            "OUT_00000000-0000-0000-0000-000000000000",
        ] {
            assert!(
                validate_handle(invalid).is_err(),
                "{invalid} must be rejected"
            );
        }
        assert!(validate_handle("out_00000000-0000-0000-0000-000000000000").is_ok());
    }

    #[test]
    fn policy_validation_accepts_the_inclusive_bounds() {
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 256,
            preview_bytes: 64,
            max_total_bytes_per_turn: 1024,
            retention_secs: 60,
            max_entries: 1,
            ..ToolOutputPolicy::default()
        };
        assert!(validate_tool_output_policy(&policy).is_ok());
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 16 * 1024 * 1024,
            preview_bytes: 1024 * 1024,
            max_total_bytes_per_turn: 64 * 1024 * 1024,
            retention_secs: 31_536_000,
            max_entries: 1_000_000,
            ..ToolOutputPolicy::default()
        };
        assert!(validate_tool_output_policy(&policy).is_ok());
    }

    #[test]
    fn small_outputs_are_not_reshaped() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy::default();
        let output_store = ToolOutputStore::new(store);
        let mut outputs = vec![pending("grep", "short result\n")];
        let report = shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        assert!(report.notices.is_empty());
        assert_eq!(outputs[0].output.content, "short result\n");
        assert!(!outputs[0].output.truncated);
        assert!(outputs[0].output.output_ref.is_none());
        assert_eq!(outputs[0].output.lines, 1);
    }

    #[test]
    fn duplicate_bodies_get_distinct_handles_and_share_one_hash() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 512,
            preview_bytes: 128,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store.clone());
        let body = "duplicate body ".repeat(100);
        let mut outputs = vec![pending("shell", &body), pending("shell", &body)];
        shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        let first = outputs[0].output.output_ref.clone().unwrap();
        let second = outputs[1].output.output_ref.clone().unwrap();
        assert_ne!(first, second);
        let one = store.get_tool_output(&first).unwrap().unwrap();
        let two = store.get_tool_output(&second).unwrap().unwrap();
        assert_eq!(one.entry.sha256, two.entry.sha256);
    }

    #[test]
    fn paging_empty_output_is_a_successful_explanation() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let spill = store
            .spill(&project, &session, None, "shell", "", "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!page.content.is_empty());
        assert!(page.content.contains("0 bytes"));
        assert!(page.content.contains("empty"));
    }

    #[test]
    fn paging_offset_past_end_is_a_successful_empty_page() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let spill = store
            .spill(&project, &session, None, "shell", "a\nb\n", "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                offset: Some(99),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.structured.as_ref().unwrap()["returned_lines"], 0);
        assert!(page.content.contains("No lines selected"));
    }

    #[test]
    fn paging_pattern_limit_is_applied_after_filtering() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let content = (0..100)
            .map(|index| {
                if index % 10 == 0 {
                    format!("MATCH {index}")
                } else {
                    format!("other {index}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let spill = store
            .spill(&project, &session, None, "grep", &content, "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                pattern: Some("MATCH".into()),
                offset: Some(1),
                limit: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(page.content.contains("MATCH 10"));
        assert!(page.content.contains("MATCH 20"));
        assert!(page.content.contains("MATCH 30"));
        assert!(!page.content.contains("MATCH 0\n"));
        assert_eq!(page.structured.as_ref().unwrap()["matching_lines"], 10);
        assert_eq!(page.structured.as_ref().unwrap()["next_offset"], 4);
    }

    #[test]
    fn preview_separator_never_exceeds_a_tiny_head_tail_budget() {
        for budget in [0u64, 1, 2, 8, 15] {
            let preview = preview_body("abcdefghijklmnopqrstuvwxyz", budget, true);
            assert!(preview.len() as u64 <= budget.max(3));
        }
    }

    #[test]
    fn empty_batch_is_a_noop() {
        let report = shape_tool_outputs(
            None,
            &ToolOutputPolicy::default(),
            "ses",
            "proj",
            0,
            &mut [],
        )
        .unwrap();
        assert!(report.notices.is_empty());
        assert_eq!(report.provider_bytes, 0);
    }

    #[test]
    fn paging_unknown_handle_is_an_error() {
        let (_data, _repo, store, _project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let error = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: "out_00000000-0000-0000-0000-000000000001".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown tool output handle"));
    }

    #[test]
    fn paging_from_end_with_one_line_and_no_offset() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let spill = store
            .spill(&project, &session, None, "shell", "first\nlast\n", "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                from_end: true,
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(page.content.contains("last"));
        assert!(!page.content.contains("first"));
    }

    #[test]
    fn max_bytes_hard_upper_bound_is_enforced() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let spill = store
            .spill(
                &project,
                &session,
                None,
                "shell",
                &"x".repeat(200_000),
                "",
                3600,
            )
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                max_bytes: Some(u64::MAX),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            page.structured.as_ref().unwrap()["max_bytes"],
            MAX_READ_TOOL_OUTPUT_BYTES
        );
    }

    #[test]
    fn policy_validation_rejects_unsafe_bounds() {
        let policy = ToolOutputPolicy::default();
        assert!(validate_tool_output_policy(&policy).is_ok());
        let invalid_preview = ToolOutputPolicy {
            preview_bytes: policy.spill_threshold_bytes + 1,
            ..ToolOutputPolicy::default()
        };
        assert!(validate_tool_output_policy(&invalid_preview).is_err());
        let invalid_retention = ToolOutputPolicy {
            retention_secs: 1,
            ..ToolOutputPolicy::default()
        };
        assert!(validate_tool_output_policy(&invalid_retention).is_err());
        let invalid_entries = ToolOutputPolicy {
            max_entries: 0,
            ..ToolOutputPolicy::default()
        };
        assert!(validate_tool_output_policy(&invalid_entries).is_err());
        let mut policy = ToolOutputPolicy::default();
        policy.tool_thresholds.insert("shell".into(), 0);
        assert!(validate_tool_output_policy(&policy).is_err());
    }

    #[test]
    fn preview_never_splits_utf8() {
        let text = "aé漢🙂".repeat(3000);
        for budget in [1u64, 7, 64, 4096, 8192] {
            let preview = preview_body(&text, budget, false);
            assert!(preview.len() <= budget as usize);
            assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
        }
        let preview = preview_body(&text, 4096, true);
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn empty_output_is_not_spilled() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy::default();
        let output_store = ToolOutputStore::new(store);
        let mut outputs = vec![pending("shell", "")];
        let report = shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        assert!(report.notices.is_empty());
        assert!(outputs[0].output.output_ref.is_none());
        assert_eq!(outputs[0].output.content, "");
    }

    #[test]
    fn spills_the_largest_results_first_under_turn_budget() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 1024 * 1024,
            preview_bytes: 4096,
            max_total_bytes_per_turn: 20 * 1024,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store);
        let mut outputs = vec![
            pending("shell", &"a".repeat(12 * 1024)),
            pending("grep", &"b".repeat(10 * 1024)),
            pending("read_file", &"c".repeat(9 * 1024)),
        ];
        let report = shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        let spilled = outputs
            .iter()
            .filter(|item| item.output.output_ref.is_some())
            .count();
        assert_eq!(spilled, 2, "12 KiB and 10 KiB should spill first");
        assert!(outputs[0].output.output_ref.is_some());
        assert!(outputs[1].output.output_ref.is_some());
        assert!(outputs[2].output.output_ref.is_none());
        assert!(
            outputs[0]
                .output
                .content
                .contains("read_tool_output handle=")
        );
        assert_eq!(outputs[0].output.total_bytes, 12 * 1024);
        assert!(outputs[0].output.preview_bytes <= 4096);
        assert!(report.provider_bytes <= policy.max_total_bytes_per_turn);
        assert!(report.notices.iter().any(|notice| {
            notice.kind == ToolOutputNoticeKind::Spilled && notice.tool_call_id == "call_shell"
        }));
    }

    #[test]
    fn head_only_preview_respects_the_exact_byte_budget() {
        let text = "a".repeat(100);
        for budget in [1u64, 2, 15, 16, 63, 64] {
            let preview = preview_body(&text, budget, false);
            assert!(preview.len() as u64 <= budget);
            assert_eq!(preview.len() as u64, budget);
        }
        let preview = preview_body(&text, 25, true);
        assert!(preview.len() as u64 <= 25);
        assert!(preview.contains('…'));
    }

    #[test]
    fn spill_notice_precedes_its_truncation_notice() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 256,
            preview_bytes: 64,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store);
        let mut outputs = vec![pending("shell", &"x".repeat(1_000))];
        let report = shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        let kinds = report
            .notices
            .iter()
            .map(|notice| notice.kind.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                ToolOutputNoticeKind::Spilled,
                ToolOutputNoticeKind::Truncated
            ]
        );
        assert_eq!(
            report.notices[0].tool_call_id,
            report.notices[1].tool_call_id
        );
    }

    #[test]
    fn aggregate_ties_spill_earlier_tool_calls_first() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 1024 * 1024,
            preview_bytes: 1024,
            max_total_bytes_per_turn: 8 * 1024,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store);
        let mut outputs = vec![
            pending("shell", &"a".repeat(5000)),
            pending("grep", &"b".repeat(5000)),
            pending("read_file", &"c".repeat(5000)),
        ];
        let report = shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        assert!(outputs[0].output.output_ref.is_some());
        assert!(outputs[1].output.output_ref.is_some());
        assert!(outputs[2].output.output_ref.is_none());
        let spilled_ids = report
            .notices
            .iter()
            .filter(|notice| notice.kind == ToolOutputNoticeKind::Spilled)
            .map(|notice| notice.tool_call_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(spilled_ids, vec!["call_shell", "call_grep"]);
    }

    #[test]
    fn paging_combines_pattern_from_end_and_offset() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let content = (0..20)
            .map(|index| format!("row {index:02} target={}", index % 2))
            .collect::<Vec<_>>()
            .join("\n");
        let spill = store
            .spill(&project, &session, None, "grep", &content, "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle.clone(),
                pattern: Some("target=1".into()),
                from_end: true,
                offset: Some(1),
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(page.content.contains("row 15"));
        assert!(page.content.contains("row 17"));
        assert!(!page.content.contains("row 19"));
        assert!(!page.content.contains("row 13"));
        assert!(!page.content.contains("target=0"));
    }

    #[test]
    fn paging_clamps_legacy_max_bytes_and_reports_no_matches() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let spill = store
            .spill(&project, &session, None, "grep", "alpha\nbeta\n", "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle.clone(),
                max_bytes: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.structured.as_ref().unwrap()["max_bytes"], 256);
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                pattern: Some("missing".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(page.content.contains("No lines matched"));
    }

    #[test]
    fn expired_handles_are_not_readable_before_the_sweep_runs() {
        let (_data, _repo, store, project, session) = fixture();
        let raw = store.clone();
        let output_store = ToolOutputStore::new(store);
        let spill = output_store
            .spill(&project, &session, None, "shell", "expired body", "", 60)
            .unwrap();
        // Move expiry into the past while preserving the row, as if the
        // daemon had not swept yet.
        raw.set_tool_output_expiry(&spill.handle, "2000-01-01T00:00:00Z")
            .unwrap();
        let error = page_tool_output(
            &output_store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown tool output handle"));
    }

    #[test]
    fn head_tail_preview_keeps_the_error_at_the_end() {
        let text = format!(
            "{}\nBUILD FAILED: missing symbol at the end",
            "x".repeat(8000)
        );
        let preview = preview_body(&text, 1000, true);
        assert!(preview.contains("BUILD FAILED"));
        assert!(preview.contains('x'));
    }

    #[test]
    fn store_failure_degrades_to_a_bounded_legacy_preview() {
        let (data, _repo, store, project, session) = fixture();
        let body = "failed store body ".repeat(2_000);
        let sha = combined_sha(&body);
        let dir = data.path().join("tool-outputs");
        std::fs::create_dir_all(&dir).unwrap();
        let outside = data.path().join("outside-escape");
        std::fs::write(&outside, "outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join(format!("{sha}.txt"))).unwrap();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 1024,
            preview_bytes: 256,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store);
        let mut outputs = vec![pending("shell", &body)];
        let report = shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        assert!(outputs[0].output.output_ref.is_none());
        assert!(outputs[0].output.truncated);
        assert!(outputs[0].output.content.len() < MAX_PROVIDER_TOOL_OUTPUT_BYTES + 512);
        assert!(outputs[0].output.content.contains("output truncated"));
        assert!(
            !outputs[0]
                .output
                .content
                .contains("read_tool_output handle=")
        );
        let notice = report
            .notices
            .iter()
            .find(|notice| notice.kind == ToolOutputNoticeKind::Truncated)
            .expect("truncation notice");
        assert_eq!(notice.reason, "store_error");
        assert!(notice.error.is_some());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside");
    }

    #[test]
    fn batch_shape_preserves_tool_call_order() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 1024,
            preview_bytes: 256,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store);
        let mut outputs = vec![
            pending("grep", &"a".repeat(2_000)),
            pending("shell", &"b".repeat(3_000)),
            pending("read_file", "short"),
        ];
        let report = shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut outputs,
        )
        .unwrap();
        let spilled = report
            .notices
            .iter()
            .filter(|notice| notice.kind == ToolOutputNoticeKind::Spilled)
            .map(|notice| notice.tool_call_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(spilled, vec!["call_grep", "call_shell"]);
        assert_eq!(outputs[0].tool_name, "grep");
        assert_eq!(outputs[1].tool_name, "shell");
        assert!(outputs[2].output.output_ref.is_none());
    }

    #[test]
    fn threshold_boundary_is_exact_end_to_end() {
        let (_data, _repo, store, project, session) = fixture();
        let policy = ToolOutputPolicy {
            spill_threshold_bytes: 1_024,
            preview_bytes: 256,
            ..ToolOutputPolicy::default()
        };
        let output_store = ToolOutputStore::new(store);
        let mut exact = vec![pending("grep", &"e".repeat(1_024))];
        shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut exact,
        )
        .unwrap();
        assert!(exact[0].output.output_ref.is_none());
        assert!(!exact[0].output.truncated);
        let mut over = vec![pending("grep", &"o".repeat(1_025))];
        shape_tool_outputs(
            Some(&output_store),
            &policy,
            &session,
            &project,
            0,
            &mut over,
        )
        .unwrap();
        assert!(over[0].output.output_ref.is_some());
        assert!(over[0].output.truncated);
        assert_eq!(over[0].output.total_bytes, 1_025);
    }

    #[test]
    fn paging_pattern_is_literal_not_a_regex() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let spill = store
            .spill(
                &project,
                &session,
                None,
                "grep",
                "alpha.* literal\nalphaZZ literal\n",
                "",
                3600,
            )
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                pattern: Some(".*".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(page.content.contains("alpha.* literal"));
        assert!(!page.content.contains("alphaZZ literal"));
    }

    #[test]
    fn paging_reports_structured_continuation_metadata() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let content = (0..10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let spill = store
            .spill(&project, &session, None, "shell", &content, "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                offset: Some(3),
                limit: Some(4),
                ..Default::default()
            },
        )
        .unwrap();
        let structured = page.structured.as_ref().unwrap();
        assert_eq!(structured["returned_lines"], 4);
        assert_eq!(structured["next_offset"], 7);
        assert_eq!(structured["truncated"], true);
        assert_eq!(structured["matching_lines"], 10);
        assert_eq!(structured["total_lines"], 10);
    }

    #[test]
    fn paging_handles_multibyte_lines_without_splitting_utf8() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let content = "漢字🙂".repeat(4_000);
        let spill = store
            .spill(&project, &session, None, "shell", &content, "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                max_bytes: Some(512),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(std::str::from_utf8(page.content.as_bytes()).is_ok());
        assert!(page.content.len() as u64 <= 512 + 512);
    }

    #[test]
    fn legacy_path_bounds_oversized_output_without_a_handle() {
        let policy = ToolOutputPolicy {
            enabled: false,
            ..ToolOutputPolicy::default()
        };
        let mut outputs = vec![pending("shell", &"z".repeat(50_000))];
        let report = shape_tool_outputs(None, &policy, "ses", "proj", 0, &mut outputs).unwrap();
        assert!(outputs[0].output.output_ref.is_none());
        assert!(outputs[0].output.truncated);
        assert!(outputs[0].output.content.contains("output truncated"));
        assert!(outputs[0].output.content.len() < 50_000);
        assert_eq!(report.notices.len(), 1);
        assert_eq!(report.notices[0].reason, "policy_disabled");
    }

    #[tokio::test]
    async fn engine_list_reports_a_stable_next_cursor() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let engine = crate::Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_str().unwrap().to_string(),
                Some("cursor".into()),
            )
            .await
            .unwrap();
        let session = engine
            .store()
            .create_session(&project.id, "cursor")
            .unwrap();
        let output_store = ToolOutputStore::new(engine.store().clone());
        for index in 0..2 {
            output_store
                .spill(
                    &project.id,
                    &session.id,
                    None,
                    "shell",
                    &format!("cursor body {index}"),
                    "",
                    3_600,
                )
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let first = engine
            .list_tool_outputs(vp_protocol::types::ToolOutputListParams {
                project_id: Some(project.id.clone()),
                limit: Some(1),
                include_expired: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(first.entries.len(), 1);
        assert!(first.truncated);
        let cursor = first.next_before.expect("next cursor");
        let second = engine
            .list_tool_outputs(vp_protocol::types::ToolOutputListParams {
                project_id: Some(project.id),
                before_created_at: Some(cursor),
                limit: Some(1),
                include_expired: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(second.entries.len(), 1);
        assert_ne!(first.entries[0].id, second.entries[0].id);
        assert!(!second.truncated);
    }

    #[test]
    fn paging_handles_offset_limit_from_end_and_pattern() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let content = (0..50)
            .map(|index| format!("line {index:02} value={}", index * 2))
            .collect::<Vec<_>>()
            .join("\n");
        let spill = store
            .spill(
                &project,
                &session,
                Some("call_1"),
                "shell",
                &content,
                "",
                3600,
            )
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle.clone(),
                offset: Some(10),
                limit: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(page.content.contains("line 10 value=20"));
        assert!(page.content.contains("line 12 value=24"));
        assert!(!page.content.contains("line 13"));
        assert!(page.content.contains("read_tool_output"));
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle.clone(),
                limit: Some(2),
                from_end: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(page.content.contains("line 49"));
        assert!(page.content.contains("earlier matching line"));
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                pattern: Some("value=24".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(page.content.contains("line 12"));
        assert!(!page.content.contains("line 11"));
    }

    #[test]
    fn paging_rejects_foreign_and_malformed_handles() {
        let (_data, _repo, store, project, session) = fixture();
        let other = store.create_session(&project, "other").unwrap();
        let store = ToolOutputStore::new(store);
        let spill = store
            .spill(&project, &session, None, "shell", "secret", "", 3600)
            .unwrap();
        let error = page_tool_output(
            &store,
            &other.id,
            &ReadToolOutputParams {
                handle: spill.handle.clone(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown tool output handle"));
        let error = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: "../../etc/passwd".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid tool output handle"));
    }

    #[test]
    fn paging_enforces_a_hard_byte_cap() {
        let (_data, _repo, store, project, session) = fixture();
        let store = ToolOutputStore::new(store);
        let content = "x".repeat(100_000);
        let spill = store
            .spill(&project, &session, None, "shell", &content, "", 3600)
            .unwrap();
        let page = page_tool_output(
            &store,
            &session,
            &ReadToolOutputParams {
                handle: spill.handle,
                max_bytes: Some(1_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            page.structured.as_ref().unwrap()["max_bytes"]
                .as_u64()
                .unwrap()
                <= MAX_READ_TOOL_OUTPUT_BYTES
        );
        assert!(page.content.len() as u64 <= MAX_READ_TOOL_OUTPUT_BYTES + 512);
    }

    #[test]
    fn store_refuses_to_follow_a_symlinked_blob() {
        let (data, _repo, store, project, session) = fixture();
        let content = "symlink test";
        let sha = combined_sha(content);
        let dir = data.path().join("tool-outputs");
        std::fs::create_dir_all(&dir).unwrap();
        let outside = data.path().join("outside.txt");
        std::fs::write(&outside, "do not touch").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join(format!("{sha}.txt"))).unwrap();
        let store = ToolOutputStore::new(store);
        let error = store
            .spill(&project, &session, None, "shell", content, "", 3600)
            .unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "do not touch");
    }

    #[test]
    fn gc_never_prunes_unexpired_handles_of_active_sessions() {
        let (_data, _repo, store, project, session) = fixture();
        let other = store.create_session(&project, "other").unwrap();
        let output_store = ToolOutputStore::new(store.clone());
        for index in 0..3 {
            store
                .create_session(&project, &format!("old-{index}"))
                .unwrap();
        }
        let active = output_store
            .spill(&project, &session, None, "shell", "active", "", 3600)
            .unwrap();
        let idle = output_store
            .spill(&project, &other.id, None, "shell", "idle", "", 3600)
            .unwrap();
        store.update_session_status(&session, "running").unwrap();
        let report = output_store
            .prune(&ToolOutputPruneRequest {
                project_id: Some(project.clone()),
                session_id: None,
                older_than_secs: None,
                max_entries: Some(1),
                dry_run: false,
            })
            .unwrap();
        assert!(report.protected_active >= 1);
        assert!(store.get_tool_output(&active.handle).unwrap().is_some());
        // The idle handle may be kept as the newest entry; the test asserts
        // that active protection is real, not that idle handles always go.
        assert!(store.get_tool_output(&idle.handle).unwrap().is_some() || report.removed > 0);
    }
}
