//! Identity, department, and project memory (S5): pure visibility, ranking,
//! bounded brief selection, and event replay.
//!
//! This module deliberately performs no store access and no provider call. The
//! store owns transactions and the append-only event stream; the engine passes
//! already-loaded rows in. Ranking is deterministic and depends only on
//! trusted row metadata plus an injected `now`, so two runs over the same rows
//! always produce the same brief bytes.
//!
//! Scope isolation has three levels:
//!
//! - `agent`: visible only to the exact identity that owns the row;
//! - `department`: visible to identities in the same project and department;
//! - `project`: visible to every identity in the project.
//!
//! An operator read that omits a viewer identity is project-wide. It is a local
//! operator surface, not something an agent can reach through `recall`.
//!
//! The brief is tail-only prompt material. It is bounded by bytes, never
//! splits an item, excludes imported/stale/superseded/expired rows, and carries
//! no claim of truth: memory is untrusted reference material, not
//! authorization, instructions, or governance.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::types::{
    AGENT_MEMORY_BRIEF_MAX_BYTES, AGENT_MEMORY_EVENT_KINDS, AGENT_MEMORY_MAX_CONTENT_CHARS,
    AGENT_MEMORY_STATUSES, AgentIdentity, AgentMemory, AgentMemoryError, AgentMemoryEvent,
    AgentMemoryReplayReport, IdentityMemoryTelemetry, agent_memory_codes,
    validate_agent_memory_evidence, validate_agent_memory_scope_shape,
};

pub use vp_protocol::types::agent_memory_codes as codes;

const SCOPE_AGENT_WEIGHT: f64 = 3.0;
const SCOPE_DEPARTMENT_WEIGHT: f64 = 2.0;
const SCOPE_PROJECT_WEIGHT: f64 = 1.0;
const PINNED_WEIGHT: f64 = 0.50;
const VERIFIED_WEIGHT: f64 = 0.25;
const RECENCY_WEIGHT: f64 = 0.25;
const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;
/// Upper bound on candidates considered before ranking. The store applies its
/// own SQL limit; this is a second, deterministic bound.
pub const MAX_BRIEF_CANDIDATES: usize = 128;
/// Upper bound on rendered brief items. The byte budget normally binds first.
pub const MAX_BRIEF_ITEMS: usize = 24;

/// One deterministic, bounded `agent/memory` brief plus metadata-only
/// telemetry. `text` is the item body; the section renderer adds the fixed
/// header. `source` distinguishes S5 memory from the S3 legacy fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityMemoryBrief {
    pub text: String,
    pub source: String,
    pub memory_ids: Vec<String>,
    pub memory_hashes: Vec<String>,
    pub memory_count: u32,
    pub brief_chars: u64,
    pub brief_bytes: usize,
    pub brief_hash: String,
    pub scope_counts: BTreeMap<String, u32>,
    pub total_candidates: usize,
}

impl IdentityMemoryBrief {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Counts and hashes only. Memory content never enters prompt telemetry.
    pub fn telemetry(&self) -> Option<IdentityMemoryTelemetry> {
        if self.is_empty() {
            return None;
        }
        Some(IdentityMemoryTelemetry {
            source: self.source.clone(),
            memory_count: self.memory_count,
            brief_chars: self.brief_chars,
            brief_hash: self.brief_hash.clone(),
            memory_hashes: self.memory_hashes.clone(),
            scope_counts: self.scope_counts.clone(),
        })
    }
}

/// True when `viewer` may read `memory`. A viewer from another project never
/// matches. Omitted viewer means the trusted local operator view.
pub fn memory_visible_to(memory: &AgentMemory, viewer: Option<&AgentIdentity>) -> bool {
    let Some(viewer) = viewer else {
        return true;
    };
    if viewer.project_id != memory.project_id {
        return false;
    }
    match memory.scope.as_str() {
        "agent" => memory.identity_id.as_deref() == Some(viewer.id.as_str()),
        "department" => {
            let Some(viewer_department) = viewer.department.as_deref() else {
                return false;
            };
            memory
                .department
                .as_deref()
                .is_some_and(|department| department.eq_ignore_ascii_case(viewer_department.trim()))
        }
        "project" => true,
        _ => false,
    }
}

/// Typed visibility failure for an explicit read.
pub fn ensure_memory_visible(memory: &AgentMemory, viewer: Option<&AgentIdentity>) -> Result<()> {
    if memory_visible_to(memory, viewer) {
        return Ok(());
    }
    Err(AgentMemoryError::new(
        agent_memory_codes::MEMORY_SCOPE_FORBIDDEN,
        format!(
            "memory {} is not visible to this identity under {} scope",
            memory.id,
            memory_scope_label(&memory.scope)
        ),
    )
    .into())
}

fn memory_scope_label(scope: &str) -> &str {
    match scope {
        "agent" | "department" | "project" => scope,
        _ => "unknown",
    }
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn recency_factor(updated_at: &str, now: OffsetDateTime) -> f64 {
    let Some(updated_at) = parse_time(updated_at) else {
        return 0.0;
    };
    let age_secs = (now - updated_at).whole_seconds().max(0) as f64;
    let age_days = age_secs / 86_400.0;
    (0.5_f64)
        .powf(age_days / RECENCY_HALF_LIFE_DAYS)
        .clamp(0.0, 1.0)
}

/// A memory is brief-eligible only when it is live local evidence. Stale,
/// superseded, expired, and imported rows remain readable through `recall` and
/// the operator RPC but never enter a prompt automatically.
pub fn brief_eligible(memory: &AgentMemory, now: OffsetDateTime) -> bool {
    memory.status == "active"
        && memory.origin == "local"
        && !memory_is_expired(memory, now)
        && !memory.content.trim().is_empty()
}

pub fn memory_is_expired(memory: &AgentMemory, now: OffsetDateTime) -> bool {
    memory
        .expires_at
        .as_deref()
        .and_then(parse_time)
        .is_some_and(|expires| expires <= now)
}

fn scope_rank(scope: &str) -> u32 {
    match scope {
        "agent" => 3,
        "department" => 2,
        "project" => 1,
        _ => 0,
    }
}

fn scope_weight(scope: &str) -> f64 {
    match scope {
        "agent" => SCOPE_AGENT_WEIGHT,
        "department" => SCOPE_DEPARTMENT_WEIGHT,
        "project" => SCOPE_PROJECT_WEIGHT,
        _ => 0.0,
    }
}

/// Deterministic score. This is a display-selection heuristic, not a
/// correctness signal and not a model judgment.
pub fn memory_score(memory: &AgentMemory, now: OffsetDateTime) -> f64 {
    let mut score = scope_weight(&memory.scope);
    if memory.pinned {
        score += PINNED_WEIGHT;
    }
    if memory.verified_at.is_some() {
        score += VERIFIED_WEIGHT;
    }
    score += recency_factor(&memory.updated_at, now) * RECENCY_WEIGHT;
    score
}

fn compare_memories(
    left: &AgentMemory,
    right: &AgentMemory,
    now: OffsetDateTime,
) -> std::cmp::Ordering {
    memory_score(right, now)
        .total_cmp(&memory_score(left, now))
        .then_with(|| right.pinned.cmp(&left.pinned))
        .then_with(|| scope_rank(&right.scope).cmp(&scope_rank(&left.scope)))
        .then_with(|| right.updated_at.cmp(&left.updated_at))
        .then_with(|| left.content_hash.cmp(&right.content_hash))
        .then_with(|| left.id.cmp(&right.id))
}

/// Deterministic display order of all provided rows. Filtering is the caller's
/// responsibility; this helper does not mutate or persist anything.
pub fn rank_memories(memories: &[AgentMemory], now: OffsetDateTime) -> Vec<AgentMemory> {
    let mut rows = memories
        .iter()
        .filter(|memory| brief_eligible(memory, now))
        .cloned()
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| compare_memories(left, right, now));
    rows.truncate(MAX_BRIEF_CANDIDATES);
    rows
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip_chars(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut clipped = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    clipped.push('…');
    clipped
}

/// Human-readable provenance for one identity memory. The raw evidence JSON is
/// never exposed by the brief or a tool response.
pub fn memory_provenance_label(memory: &AgentMemory) -> String {
    let mut parts = Vec::new();
    if let Some(task) = memory
        .evidence
        .iter()
        .find(|evidence| evidence.kind == "task" && !evidence.reference.trim().is_empty())
    {
        parts.push(format!("task {}", task.reference.trim()));
    }
    if let Some(commit) = memory
        .evidence
        .iter()
        .find(|evidence| evidence.kind == "commit" && !evidence.reference.trim().is_empty())
    {
        parts.push(format!("commit {}", short_commit(&commit.reference)));
    }
    if parts.is_empty()
        && let Some(session) = memory
            .evidence
            .iter()
            .find(|evidence| evidence.kind == "session" && !evidence.reference.trim().is_empty())
    {
        parts.push(format!("session {}", short_commit(&session.reference)));
    }
    parts.join(", ")
}

fn short_commit(reference: &str) -> String {
    reference.trim().chars().take(7).collect()
}

fn render_item(memory: &AgentMemory) -> String {
    let content = clip_chars(
        &collapse_whitespace(&memory.content),
        AGENT_MEMORY_MAX_CONTENT_CHARS,
    );
    let provenance = memory_provenance_label(memory);
    if provenance.is_empty() {
        format!("[{}·{}] {content}", memory.scope, memory.source)
    } else {
        format!(
            "[{}·{}] {content} ({provenance})",
            memory.scope, memory.source
        )
    }
}

/// Build the bounded brief. Items are considered in rank order and a low-ranked
/// item is dropped rather than exceeding the byte budget. An item is never
/// split: if even the next whole item does not fit, selection stops. An empty
/// result means the caller omits the `agent/memory` section entirely.
pub fn select_identity_memory_brief(
    memories: &[AgentMemory],
    max_bytes: usize,
    now: OffsetDateTime,
) -> IdentityMemoryBrief {
    let budget = max_bytes.max(1);
    let ranked = rank_memories(memories, now);
    let total_candidates = ranked.len();
    let mut text = String::new();
    let mut selected = Vec::new();
    for memory in ranked {
        if selected.len() >= MAX_BRIEF_ITEMS {
            break;
        }
        let item = render_item(&memory);
        if item.trim().is_empty() {
            continue;
        }
        let separator = usize::from(!text.is_empty());
        let needed = separator + item.len();
        if text.len().saturating_add(needed) > budget {
            break;
        }
        if separator == 1 {
            text.push('\n');
        }
        text.push_str(&item);
        selected.push(memory);
    }
    let mut scope_counts = BTreeMap::new();
    for memory in &selected {
        *scope_counts.entry(memory.scope.clone()).or_insert(0) += 1;
    }
    IdentityMemoryBrief {
        brief_chars: text.chars().count() as u64,
        brief_bytes: text.len(),
        brief_hash: crate::sha256_hex(text.as_bytes()),
        memory_ids: selected.iter().map(|memory| memory.id.clone()).collect(),
        memory_hashes: selected
            .iter()
            .map(|memory| memory.content_hash.clone())
            .collect(),
        memory_count: selected.len() as u32,
        source: "agent_memory".to_string(),
        total_candidates,
        text,
        scope_counts,
    }
}

/// Legacy S3 fallback. The caller supplies an already selected project-memory
/// brief and its memory ids/hashes; this wrapper keeps the metadata shape the
/// same as the S5 path without pretending legacy rows are identity memory.
pub fn legacy_brief(
    text: String,
    memory_ids: Vec<String>,
    memory_hashes: Vec<String>,
) -> IdentityMemoryBrief {
    let scope_counts = if text.trim().is_empty() {
        BTreeMap::new()
    } else {
        BTreeMap::from([("project".to_string(), memory_ids.len() as u32)])
    };
    IdentityMemoryBrief {
        brief_chars: text.chars().count() as u64,
        brief_bytes: text.len(),
        brief_hash: crate::sha256_hex(text.as_bytes()),
        memory_ids,
        memory_hashes,
        memory_count: scope_counts.get("project").copied().unwrap_or(0),
        source: if text.trim().is_empty() {
            String::new()
        } else {
            "legacy_project_memory".to_string()
        },
        total_candidates: 0,
        text,
        scope_counts,
    }
}

/// `AGENT_MEMORY_BRIEF_MAX_BYTES` is re-exported so callers do not duplicate
/// the prompt budget constant.
pub const DEFAULT_BRIEF_BYTES: usize = AGENT_MEMORY_BRIEF_MAX_BYTES;

/// Validate one event snapshot and return the bounded memory row it carries.
/// Malformed, cross-project, hash-mismatched, or scope-invalid payloads fail
/// closed; the event stream is authoritative state, not free-form prose.
pub fn memory_from_event(event: &AgentMemoryEvent) -> Result<AgentMemory> {
    if !AGENT_MEMORY_EVENT_KINDS.contains(&event.kind.as_str()) {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_INVALID_EVENT,
            format!(
                "memory event {} has an unknown kind {}",
                event.id, event.kind
            ),
        )
        .into());
    }
    let mut memory: AgentMemory =
        serde_json::from_value(event.payload.get("memory").cloned().ok_or_else(|| {
            AgentMemoryError::new(
                agent_memory_codes::MEMORY_INVALID_EVENT,
                format!("memory event {} has no memory snapshot", event.id),
            )
        })?)?;
    if memory.id.trim().is_empty() || memory.project_id.trim().is_empty() {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_INVALID_EVENT,
            format!("memory event {} has an empty id or project id", event.id),
        )
        .into());
    }
    if memory.project_id != event.project_id {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_PROJECT_MISMATCH,
            format!(
                "memory event {} carries a snapshot for another project",
                event.id
            ),
        )
        .into());
    }
    if !AGENT_MEMORY_STATUSES.contains(&memory.status.as_str()) {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_INVALID_EVENT,
            format!("memory event {} has an invalid status", event.id),
        )
        .into());
    }
    validate_agent_memory_scope_shape(
        &memory.scope,
        memory.identity_id.as_deref(),
        memory.department.as_deref(),
    )?;
    let normalized = vp_protocol::types::normalize_agent_memory_content(&memory.content)?;
    if normalized != memory.content {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_INVALID_EVENT,
            format!("memory event {} content is not normalized", event.id),
        )
        .into());
    }
    let expected_hash = crate::sha256_hex(memory.content.as_bytes());
    if !memory.content_hash.is_empty() && memory.content_hash != expected_hash {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_INVALID_EVENT,
            format!(
                "memory event {} content hash does not match content",
                event.id
            ),
        )
        .into());
    }
    memory.content_hash = expected_hash;
    if memory.origin != "local" && memory.origin != "imported" {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_INVALID_EVENT,
            format!("memory event {} has an invalid origin", event.id),
        )
        .into());
    }
    validate_agent_memory_evidence(&memory.evidence)?;
    // `expired` is derived at read time; an event never carries authority for
    // it. Normalizing here keeps replay deterministic across wall clocks.
    memory.expired = false;
    match event.kind.as_str() {
        "agent.memory.created" | "agent.memory.updated" if memory.status != "active" => {
            return Err(AgentMemoryError::new(
                agent_memory_codes::MEMORY_INVALID_EVENT,
                format!(
                    "memory event {} does not carry an active snapshot",
                    event.id
                ),
            )
            .into());
        }
        "agent.memory.superseded"
            if memory.status != "superseded" || memory.superseded_by.is_none() =>
        {
            return Err(AgentMemoryError::new(
                agent_memory_codes::MEMORY_INVALID_EVENT,
                format!(
                    "memory superseded event {} is missing its replacement edge",
                    event.id
                ),
            )
            .into());
        }
        "agent.memory.expired"
            if memory.status != "stale" || memory.stale_reason.as_deref() != Some("expired") =>
        {
            return Err(AgentMemoryError::new(
                agent_memory_codes::MEMORY_INVALID_EVENT,
                format!("memory expired event {} is not an expiry", event.id),
            )
            .into());
        }
        "agent.memory.deleted" => match deleted_mode(event) {
            Some("soft") if memory.status != "stale" => {
                return Err(AgentMemoryError::new(
                    agent_memory_codes::MEMORY_INVALID_EVENT,
                    format!("memory soft-delete event {} is not stale", event.id),
                )
                .into());
            }
            Some("prune") if !matches!(memory.status.as_str(), "stale" | "superseded") => {
                return Err(AgentMemoryError::new(
                    agent_memory_codes::MEMORY_INVALID_EVENT,
                    format!("memory prune event {} is not stale or superseded", event.id),
                )
                .into());
            }
            Some("soft") | Some("prune") => {}
            _ => {
                return Err(AgentMemoryError::new(
                    agent_memory_codes::MEMORY_INVALID_EVENT,
                    format!("memory delete event {} has no mode", event.id),
                )
                .into());
            }
        },
        _ => {}
    }
    Ok(memory)
}

fn deleted_mode(event: &AgentMemoryEvent) -> Option<&str> {
    event
        .payload
        .get("mode")
        .and_then(serde_json::Value::as_str)
}

/// Validate one logical-erasure tombstone and return the erased memory id.
///
/// An erasure is content-free: it may carry the old content hash, scope, and a
/// bounded reason, but never a full memory snapshot or the erased content.
/// `None` means this is not an erasure event.
pub fn erased_memory_id(event: &AgentMemoryEvent) -> Result<Option<&str>> {
    if event.kind != vp_protocol::types::AGENT_MEMORY_ERASED_EVENT {
        return Ok(None);
    }
    if event.payload.get("memory").is_some() || event.payload.get("content").is_some() {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_INVALID_EVENT,
            format!(
                "erasure event {} must not carry a memory snapshot or content",
                event.id
            ),
        )
        .into());
    }
    let encoded = serde_json::to_vec(&event.payload).unwrap_or_default();
    if encoded.len() > 4 * 1024 {
        return Err(AgentMemoryError::new(
            agent_memory_codes::MEMORY_INVALID_EVENT,
            format!("erasure event {} payload is oversized", event.id),
        )
        .into());
    }
    let memory_id = event
        .payload
        .get("memory_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|memory_id| !memory_id.is_empty())
        .ok_or_else(|| {
            AgentMemoryError::new(
                agent_memory_codes::MEMORY_INVALID_EVENT,
                format!("erasure event {} has no memory id", event.id),
            )
        })?;
    Ok(Some(memory_id))
}

/// Fold the append-only project stream into the latest row per memory id.
/// Events must be in project `seq` order. A prune or erase event removes the
/// row; every other event replaces its snapshot.
pub fn memories_from_events(events: &[AgentMemoryEvent]) -> Result<Vec<AgentMemory>> {
    let mut latest: BTreeMap<String, AgentMemory> = BTreeMap::new();
    let mut previous_seq = None;
    for event in events {
        if event.seq == 0 {
            return Err(AgentMemoryError::new(
                agent_memory_codes::MEMORY_INVALID_EVENT,
                format!("memory event {} has seq 0", event.id),
            )
            .into());
        }
        if let Some(previous) = previous_seq
            && event.seq <= previous
        {
            return Err(AgentMemoryError::new(
                agent_memory_codes::MEMORY_INVALID_EVENT,
                format!("memory event stream is out of order at seq {}", event.seq),
            )
            .into());
        }
        previous_seq = Some(event.seq);
        if let Some(memory_id) = erased_memory_id(event)? {
            latest.remove(memory_id);
            continue;
        }
        let memory = memory_from_event(event)?;
        if event.kind == "agent.memory.deleted" && deleted_mode(event) == Some("prune") {
            latest.remove(&memory.id);
            continue;
        }
        latest.insert(memory.id.clone(), memory);
    }
    Ok(latest.into_values().collect())
}

fn comparable(memory: &AgentMemory) -> AgentMemory {
    let mut memory = memory.clone();
    // `origin` is archive provenance and `expired` is a read-time derivation.
    // Neither is lifecycle state, so replay compares everything else.
    memory.origin = "local".to_string();
    memory.expired = false;
    memory
}

/// Compare the event-derived projection with stored rows. The result is a
/// bounded list of human-readable mismatches; it never mutates a row.
pub fn compare_memory_projection(expected: &[AgentMemory], actual: &[AgentMemory]) -> Vec<String> {
    let expected_by_id = expected
        .iter()
        .map(comparable)
        .map(|memory| (memory.id.clone(), memory))
        .collect::<BTreeMap<_, _>>();
    let actual_by_id = actual
        .iter()
        .map(comparable)
        .map(|memory| (memory.id.clone(), memory))
        .collect::<BTreeMap<_, _>>();
    let mut mismatches = Vec::new();
    for id in expected_by_id
        .keys()
        .chain(actual_by_id.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
    {
        match (expected_by_id.get(&id), actual_by_id.get(&id)) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(expected), Some(actual)) => mismatches.push(format!(
                "memory {id}: projection status {} hash {} does not match replay status {} hash {}",
                actual.status,
                short_commit(&actual.content_hash),
                expected.status,
                short_commit(&expected.content_hash)
            )),
            (Some(_), None) => {
                mismatches.push(format!("memory {id}: event stream exists without a row"))
            }
            (None, Some(_)) => {
                mismatches.push(format!("memory {id}: row exists without a memory event"))
            }
            (None, None) => {}
        }
    }
    mismatches
}

pub fn replay_report(
    project_id: &str,
    expected: &[AgentMemory],
    events: usize,
    repaired: bool,
    mismatches: Vec<String>,
) -> AgentMemoryReplayReport {
    AgentMemoryReplayReport {
        project_id: project_id.to_string(),
        memories: expected.len() as u64,
        events: events as u64,
        consistent: mismatches.is_empty(),
        repaired,
        mismatches,
    }
}

/// Deterministic content hash shared by the store and replay. The input is the
/// exact normalized content; callers must not hash a legacy body with a title
/// prefix that is not part of the stored row.
pub fn content_hash(content: &str) -> String {
    crate::sha256_hex(content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::MemoryEvidence;

    fn identity(id: &str, department: Option<&str>) -> AgentIdentity {
        AgentIdentity {
            id: id.to_string(),
            project_id: "prj-1".to_string(),
            role: "worker".to_string(),
            department: department.map(str::to_string),
            status: "active".to_string(),
            ..AgentIdentity::default()
        }
    }

    fn memory(id: &str, scope: &str, updated_at: &str) -> AgentMemory {
        let content = format!("content for {id}");
        AgentMemory {
            id: id.to_string(),
            project_id: "prj-1".to_string(),
            identity_id: (scope == "agent").then(|| "agt-a".to_string()),
            department: (scope == "department").then(|| "Platform".to_string()),
            component: None,
            scope: scope.to_string(),
            content: content.clone(),
            content_hash: content_hash(&content),
            evidence: Vec::new(),
            source: "agent".to_string(),
            status: "active".to_string(),
            pinned: false,
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            expires_at: None,
            verified_at: None,
            superseded_by: None,
            stale_reason: None,
            origin: "local".to_string(),
            expired: false,
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-11-13T00:00:00Z", &Rfc3339).unwrap()
    }

    #[test]
    fn scope_visibility_isolates_agent_and_department_rows() {
        let worker = identity("agt-a", Some("Platform"));
        let other = identity("agt-b", Some("Billing"));
        let agent = memory("m-agent", "agent", "2026-11-12T00:00:00Z");
        let department = memory("m-dept", "department", "2026-11-12T00:00:00Z");
        let project = memory("m-project", "project", "2026-11-12T00:00:00Z");
        assert!(memory_visible_to(&agent, Some(&worker)));
        assert!(!memory_visible_to(&agent, Some(&other)));
        assert!(memory_visible_to(&department, Some(&worker)));
        assert!(!memory_visible_to(&department, Some(&other)));
        assert!(memory_visible_to(&project, Some(&other)));
        assert!(memory_visible_to(&agent, None));
        let mut foreign = agent.clone();
        foreign.project_id = "prj-2".to_string();
        assert!(!memory_visible_to(&foreign, Some(&worker)));
        assert_eq!(
            ensure_memory_visible(&agent, Some(&other))
                .unwrap_err()
                .downcast_ref::<AgentMemoryError>()
                .unwrap()
                .code,
            agent_memory_codes::MEMORY_SCOPE_FORBIDDEN
        );
    }

    #[test]
    fn ranking_orders_scope_then_pinned_then_verified_then_recency() {
        let mut agent = memory("m-agent", "agent", "2026-09-01T00:00:00Z");
        agent.pinned = true;
        let mut department = memory("m-dept", "department", "2026-11-12T00:00:00Z");
        department.verified_at = Some("2026-11-12T00:00:00Z".to_string());
        let project_old = memory("m-project-old", "project", "2026-11-01T00:00:00Z");
        let project_new = memory("m-project-new", "project", "2026-11-12T00:00:00Z");
        let ranked = rank_memories(&[project_old, project_new, department, agent], now());
        let ids = ranked
            .iter()
            .map(|memory| memory.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["m-agent", "m-dept", "m-project-new", "m-project-old"]
        );
    }

    #[test]
    fn content_hash_is_the_final_tie_break() {
        let mut left = memory("m-z", "project", "2026-11-12T00:00:00Z");
        left.content_hash = "a".repeat(64);
        let mut right = memory("m-a", "project", "2026-11-12T00:00:00Z");
        right.content_hash = "b".repeat(64);
        let ranked = rank_memories(&[right, left], now());
        assert_eq!(ranked[0].id, "m-z");
        assert_eq!(ranked[1].id, "m-a");
    }

    #[test]
    fn brief_is_byte_bounded_and_never_splits_an_item() {
        let mut first = memory("m-first", "agent", "2026-11-12T00:00:00Z");
        first.content = "A".repeat(400);
        let mut second = memory("m-second", "project", "2026-11-12T00:00:00Z");
        second.content = "B".repeat(400);
        let brief = select_identity_memory_brief(&[second, first], 520, now());
        assert_eq!(brief.memory_count, 1);
        assert!(brief.text.contains("[agent·agent] AAAA"));
        assert!(!brief.text.contains("[project·agent]"));
        assert!(brief.brief_bytes <= 520);
        assert_eq!(brief.brief_hash, crate::sha256_hex(brief.text.as_bytes()));
        assert_eq!(brief.scope_counts.get("agent"), Some(&1));
    }

    #[test]
    fn brief_omits_stale_expired_superseded_and_imported_rows() {
        let fresh = memory("m-fresh", "project", "2026-11-12T00:00:00Z");
        let mut stale = memory("m-stale", "project", "2026-11-12T00:00:00Z");
        stale.status = "stale".to_string();
        let mut expired = memory("m-expired", "project", "2026-11-12T00:00:00Z");
        expired.expires_at = Some("2026-11-12T12:00:00Z".to_string());
        let mut superseded = memory("m-superseded", "project", "2026-11-12T00:00:00Z");
        superseded.status = "superseded".to_string();
        superseded.superseded_by = Some("m-new".to_string());
        let mut imported = memory("m-imported", "project", "2026-11-12T00:00:00Z");
        imported.origin = "imported".to_string();
        let brief = select_identity_memory_brief(
            &[fresh, stale, expired, superseded, imported],
            DEFAULT_BRIEF_BYTES,
            now(),
        );
        assert_eq!(brief.memory_count, 1);
        assert!(brief.text.contains("m-fresh"));
        assert!(!brief.text.contains("m-stale"));
        assert!(!brief.text.contains("m-expired"));
        assert!(!brief.text.contains("m-superseded"));
        assert!(!brief.text.contains("m-imported"));
        let empty = select_identity_memory_brief(&[], DEFAULT_BRIEF_BYTES, now());
        assert!(empty.is_empty());
        assert!(empty.telemetry().is_none());
    }

    #[test]
    fn brief_ranking_is_deterministic_across_input_order() {
        let a = memory("m-a", "department", "2026-11-11T00:00:00Z");
        let b = memory("m-b", "department", "2026-11-11T00:00:00Z");
        let mut expected = [a.clone(), b.clone()];
        expected.sort_by(|left, right| {
            left.content_hash
                .cmp(&right.content_hash)
                .then_with(|| left.id.cmp(&right.id))
        });
        let first = select_identity_memory_brief(&[a.clone(), b.clone()], 4_000, now());
        let second = select_identity_memory_brief(&[b, a], 4_000, now());
        assert_eq!(first, second);
        assert_eq!(
            first.memory_ids,
            expected
                .iter()
                .map(|memory| memory.id.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn provenance_label_exposes_pointers_without_raw_evidence_json() {
        let mut row = memory("m-prov", "agent", "2026-11-12T00:00:00Z");
        row.evidence = vec![
            MemoryEvidence {
                kind: "task".into(),
                reference: "T3".into(),
                note: Some("do not render this note".into()),
            },
            MemoryEvidence {
                kind: "commit".into(),
                reference: "abcdef1234567890".into(),
                note: None,
            },
        ];
        assert_eq!(memory_provenance_label(&row), "task T3, commit abcdef1");
        let brief = select_identity_memory_brief(&[row], 2_000, now());
        assert!(brief.text.contains("(task T3, commit abcdef1)"));
        assert!(!brief.text.contains("do not render this note"));
    }

    #[test]
    fn event_replay_folds_create_update_supersede_expire_and_prune() {
        let mut row = memory("m-1", "project", "2026-11-12T00:00:00Z");
        let created = event(&row, 1, "agent.memory.created", "local");
        row.content = "updated content".to_string();
        row.content_hash = content_hash(&row.content);
        row.updated_at = "2026-11-12T01:00:00Z".to_string();
        let updated = event(&row, 2, "agent.memory.updated", "local");
        let mut superseded = row.clone();
        superseded.status = "superseded".to_string();
        superseded.superseded_by = Some("m-2".to_string());
        let superseded_event = event(&superseded, 3, "agent.memory.superseded", "local");
        let mut expired = row.clone();
        expired.status = "stale".to_string();
        expired.stale_reason = Some("expired".to_string());
        let expired_event = event(&expired, 4, "agent.memory.expired", "local");
        let deleted = event(&expired, 5, "agent.memory.deleted", "prune");

        let folded = memories_from_events(&[
            created.clone(),
            updated,
            superseded_event.clone(),
            expired_event.clone(),
        ])
        .unwrap();
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].status, "stale");
        assert_eq!(folded[0].stale_reason.as_deref(), Some("expired"));

        let pruned = memories_from_events(&[created, expired_event, deleted]).unwrap();
        assert!(pruned.is_empty());

        // A superseded row can also be pruned; the delete event carries the
        // stale/superseded snapshot and is terminal for replay.
        let superseded_prune = event(&superseded, 4, "agent.memory.deleted", "prune");
        let pruned_superseded = memories_from_events(&[
            event(&row, 1, "agent.memory.created", "local"),
            superseded_event,
            superseded_prune,
        ])
        .unwrap();
        assert!(pruned_superseded.is_empty());
    }

    #[test]
    fn erased_tombstone_drops_the_row_and_rejects_smuggled_content() {
        let row = memory("m-erased", "project", "2026-11-12T00:00:00Z");
        let created = event(&row, 1, "agent.memory.created", "local");
        let erased = AgentMemoryEvent {
            id: "evt-2".to_string(),
            project_id: row.project_id.clone(),
            seq: 2,
            kind: vp_protocol::types::AGENT_MEMORY_ERASED_EVENT.to_string(),
            payload: serde_json::json!({
                "memory_id": row.id,
                "content_hash": row.content_hash,
                "scope": row.scope,
                "reason": "privacy request",
            }),
            created_at: "2026-11-12T01:00:00Z".to_string(),
        };
        let folded = memories_from_events(&[created, erased]).unwrap();
        assert!(folded.is_empty(), "erasure must not resurrect the row");

        let mut smuggled = event(&row, 2, "agent.memory.erased", "local");
        smuggled.payload = serde_json::json!({
            "memory_id": row.id,
            "memory": row,
        });
        let error = memories_from_events(&[smuggled]).unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_INVALID_EVENT
        );
    }

    fn event(memory: &AgentMemory, seq: u64, kind: &str, mode: &str) -> AgentMemoryEvent {
        AgentMemoryEvent {
            id: format!("evt-{seq}"),
            project_id: memory.project_id.clone(),
            seq,
            kind: kind.to_string(),
            payload: serde_json::json!({"memory": memory, "mode": mode}),
            created_at: memory.updated_at.clone(),
        }
    }

    #[test]
    fn event_replay_rejects_hash_mismatch_and_cross_project_payloads() {
        let mut row = memory("m-1", "project", "2026-11-12T00:00:00Z");
        row.content_hash = "deadbeef".to_string();
        let bad = event(&row, 1, "agent.memory.created", "local");
        let error = memories_from_events(&[bad]).unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_INVALID_EVENT
        );

        let mut row = memory("m-1", "project", "2026-11-12T00:00:00Z");
        row.project_id = "prj-2".to_string();
        let mut bad = event(&row, 1, "agent.memory.created", "local");
        bad.project_id = "prj-1".to_string();
        let error = memories_from_events(&[bad]).unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_PROJECT_MISMATCH
        );
    }

    #[test]
    fn compare_ignores_origin_and_derived_expiry_but_reports_drift() {
        let memory = memory("m-1", "project", "2026-11-12T00:00:00Z");
        let mut imported = memory.clone();
        imported.origin = "imported".to_string();
        imported.expired = true;
        assert!(compare_memory_projection(std::slice::from_ref(&memory), &[imported]).is_empty());
        let mut changed = memory.clone();
        changed.status = "stale".to_string();
        let mismatches = compare_memory_projection(&[memory], &[changed]);
        assert_eq!(mismatches.len(), 1);
        assert!(mismatches[0].contains("m-1"));
    }
}
