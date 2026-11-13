//! Identity, department, and project agent memory (S5).
//!
//! These rows are durable project state, not session state. An agent memory
//! belongs to one project identity, department memory is shared by a
//! department, and project memory is shared by the project. The table is
//! deliberately separate from the session-scoped `memories` projection: S5
//! memory has explicit scope, evidence, TTL, supersede edges, and an append-only
//! project event stream.
//!
//! Every mutation commits its projection row and one `agent_memory_events` row
//! in the same transaction. Events are project-scoped because memory outlives
//! any session. The event `id` is portable across databases while `seq` is the
//! per-project replay order.
//!
//! This module never calls a provider, never renders prompt text, and never
//! decides truth. Memory is untrusted reference material with provenance, not
//! authorization, instructions, or governance.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vp_protocol::types::{
    AGENT_MEMORY_EVENT_KINDS, AGENT_MEMORY_MAX_LIST, AGENT_MEMORY_MAX_SWEEP,
    AGENT_MEMORY_MAX_TTL_DAYS, AGENT_MEMORY_STATUSES, AgentMemory, AgentMemoryEraseResult,
    AgentMemoryError, AgentMemoryEvent, CreateAgentMemoryParams, EraseAgentMemoryParams,
    ForgetAgentMemoryParams, SupersedeAgentMemoryParams, SweepAgentMemoriesParams,
    UpdateAgentMemoryParams, VerifyAgentMemoryParams, agent_memory_codes, agent_memory_scope,
    agent_memory_source, normalize_agent_memory_component, normalize_agent_memory_content,
    normalize_agent_memory_department, validate_agent_memory_evidence,
    validate_agent_memory_scope_shape,
};

use super::{Store, now_rfc3339, validate_storage_id};

/// Hard upper bound on one replay read. It matches the S2/S4 project streams.
pub const AGENT_MEMORY_EVENT_LIMIT: u32 = 100_000;

const AGENT_MEMORY_COLUMNS: &str = "id, project_id, identity_id, department, component, scope, \
     content, content_hash, evidence_json, source, status, pinned, created_at, updated_at, \
     expires_at, verified_at, superseded_by, stale_reason, origin";

/// Result of one sweep. The events are already persisted; the caller may log
/// or broadcast them after the transaction commits.
#[derive(Debug, Clone, Default)]
pub struct MemorySweep {
    pub expired: u64,
    pub pruned: u64,
    pub remaining: u64,
    pub events: Vec<AgentMemoryEvent>,
}

/// Result of an atomic supersede. `events` contains the replacement creation
/// event (when a new memory was created) followed by the supersede event.
#[derive(Debug, Clone)]
pub struct MemorySupersede {
    pub previous: AgentMemory,
    pub replacement: AgentMemory,
    pub events: Vec<AgentMemoryEvent>,
}

fn memory_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentMemoryError::new(code, message).into()
}

fn short_hash(value: &str) -> String {
    value.trim().chars().take(8).collect()
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn format_time(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .map_err(|error| anyhow!("formatting agent memory time: {error}"))
}

fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// Deterministic expiry normalization. A bounded TTL is not a security
/// boundary; it keeps stale model prose from being trusted indefinitely.
fn normalize_expiry(value: Option<&str>, now: OffsetDateTime) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let expires = parse_time(value).ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_INVALID,
            "expires_at must be an RFC3339 timestamp",
        )
    })?;
    if expires <= now {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            "expires_at must be in the future",
        ));
    }
    let max = now + time::Duration::days(i64::from(AGENT_MEMORY_MAX_TTL_DAYS));
    if expires > max {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("expires_at must be at most {AGENT_MEMORY_MAX_TTL_DAYS} days from now"),
        ));
    }
    Ok(Some(format_time(expires)?))
}

fn normalize_source(value: Option<&str>, local_create: bool) -> Result<String> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("operator");
    let source = agent_memory_source(value).ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!(
                "source must be one of operator, agent, task_result, verifier, integration: {value}"
            ),
        )
    })?;
    if local_create && source == "imported" {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            "the imported source is reserved for archive provenance",
        ));
    }
    Ok(source.to_string())
}

fn require_evidence_for_source(
    source: &str,
    evidence: &[vp_protocol::types::MemoryEvidence],
) -> Result<()> {
    if matches!(source, "task_result" | "verifier" | "integration") && evidence.is_empty() {
        return Err(memory_error(
            agent_memory_codes::MEMORY_EVIDENCE_INVALID,
            format!("{source} memory requires at least one bounded evidence pointer"),
        ));
    }
    Ok(())
}

struct MemoryIdentityRow {
    project_id: String,
    department: Option<String>,
    component: Option<String>,
    status: String,
    origin: String,
}

fn ensure_project_tx(tx: &Transaction<'_>, project_id: &str) -> Result<()> {
    let exists = tx
        .query_row(
            "SELECT 1 FROM projects WHERE id = ?1",
            params![project_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !exists {
        return Err(memory_error(
            agent_memory_codes::MEMORY_NOT_FOUND,
            format!("project not found: {project_id}"),
        ));
    }
    Ok(())
}

fn memory_identity_tx(tx: &Transaction<'_>, identity_id: &str) -> Result<MemoryIdentityRow> {
    tx.query_row(
        "SELECT project_id, department, component, status, origin
         FROM agent_identities WHERE id = ?1",
        params![identity_id],
        |row| {
            Ok(MemoryIdentityRow {
                project_id: row.get(0)?,
                department: row.get(1)?,
                component: row.get(2)?,
                status: row.get(3)?,
                origin: row.get(4)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_NOT_FOUND,
            format!("agent identity not found: {identity_id}"),
        )
    })
}

/// Resolve scope ownership from trusted rows. Agent memory always inherits the
/// identity's department/component so a caller cannot mislabel a cross-department
/// claim. Department and project memory never carry an identity id.
fn resolve_scope_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    scope: &str,
    identity_id: Option<&str>,
    department: Option<&str>,
    component: Option<&str>,
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    validate_agent_memory_scope_shape(scope, identity_id, department)?;
    match scope {
        "agent" => {
            let identity_id = identity_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    memory_error(
                        agent_memory_codes::MEMORY_IDENTITY_REQUIRED,
                        "agent memory requires a project identity",
                    )
                })?;
            let identity = memory_identity_tx(tx, identity_id)?;
            if identity.project_id != project_id {
                return Err(memory_error(
                    agent_memory_codes::MEMORY_PROJECT_MISMATCH,
                    "agent memory identity belongs to another project",
                ));
            }
            if identity.origin != "local" {
                return Err(memory_error(
                    agent_memory_codes::MEMORY_IDENTITY_UNAVAILABLE,
                    "an imported identity is read-only until an explicit rehome",
                ));
            }
            if identity.status == "retired" {
                return Err(memory_error(
                    agent_memory_codes::MEMORY_IDENTITY_RETIRED,
                    "a retired identity's agent memory is read-only; supersede or expire it instead",
                ));
            }
            let resolved_department = identity.department.clone();
            if let Some(requested) = department.map(str::trim).filter(|value| !value.is_empty()) {
                let matches = resolved_department
                    .as_deref()
                    .is_some_and(|current| current.eq_ignore_ascii_case(requested));
                if !matches {
                    return Err(memory_error(
                        agent_memory_codes::MEMORY_SCOPE_INVALID,
                        "agent memory department must match the identity department",
                    ));
                }
            }
            let resolved_component = match (identity.component.as_deref(), component) {
                (Some(current), Some(requested)) => {
                    let requested = normalize_agent_memory_component(requested)?;
                    if !current.eq_ignore_ascii_case(&requested) {
                        return Err(memory_error(
                            agent_memory_codes::MEMORY_SCOPE_INVALID,
                            "agent memory component must match the identity component",
                        ));
                    }
                    Some(current.to_string())
                }
                (Some(current), None) => Some(current.to_string()),
                (None, Some(requested)) => Some(normalize_agent_memory_component(requested)?),
                (None, None) => None,
            };
            Ok((
                Some(identity_id.to_string()),
                resolved_department,
                resolved_component,
            ))
        }
        "department" => {
            let department = department
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    memory_error(
                        agent_memory_codes::MEMORY_DEPARTMENT_REQUIRED,
                        "department memory requires a department",
                    )
                })?;
            let department = normalize_agent_memory_department(department)?;
            let component = component
                .map(normalize_agent_memory_component)
                .transpose()?;
            Ok((None, Some(department), component))
        }
        "project" => {
            let component = component
                .map(normalize_agent_memory_component)
                .transpose()?;
            Ok((None, None, component))
        }
        _ => Err(memory_error(
            agent_memory_codes::MEMORY_SCOPE_INVALID,
            format!("scope must be agent, department, or project: {scope}"),
        )),
    }
}

fn map_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMemory> {
    let evidence_json: String = row.get(8)?;
    let evidence = serde_json::from_str(&evidence_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let expires_at: Option<String> = row.get(14)?;
    let expired = expires_at
        .as_deref()
        .and_then(parse_time)
        .is_some_and(|expires| expires <= OffsetDateTime::now_utc());
    Ok(AgentMemory {
        id: row.get(0)?,
        project_id: row.get(1)?,
        identity_id: row.get(2)?,
        department: row.get(3)?,
        component: row.get(4)?,
        scope: row.get(5)?,
        content: row.get(6)?,
        content_hash: row.get(7)?,
        evidence,
        source: row.get(9)?,
        status: row.get(10)?,
        pinned: row.get::<_, i64>(11)? != 0,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        expires_at,
        verified_at: row.get(15)?,
        superseded_by: row.get(16)?,
        stale_reason: row.get(17)?,
        origin: row.get(18)?,
        expired,
    })
}

pub(crate) fn get_memory_tx(tx: &Transaction<'_>, memory_id: &str) -> Result<Option<AgentMemory>> {
    tx.query_row(
        &format!("SELECT {AGENT_MEMORY_COLUMNS} FROM agent_memories WHERE id = ?1"),
        params![memory_id],
        map_memory,
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn insert_memory_row_tx(tx: &Transaction<'_>, memory: &AgentMemory) -> Result<()> {
    tx.execute(
        "INSERT INTO agent_memories
         (id, project_id, identity_id, department, component, scope, content, content_hash,
          evidence_json, source, status, pinned, created_at, updated_at, expires_at, verified_at,
          superseded_by, stale_reason, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            memory.id,
            memory.project_id,
            memory.identity_id,
            memory.department,
            memory.component,
            memory.scope,
            memory.content,
            memory.content_hash,
            serde_json::to_string(&memory.evidence)?,
            memory.source,
            memory.status,
            if memory.pinned { 1 } else { 0 },
            memory.created_at,
            memory.updated_at,
            memory.expires_at,
            memory.verified_at,
            memory.superseded_by,
            memory.stale_reason,
            memory.origin
        ],
    )?;
    Ok(())
}

fn update_memory_row_tx(tx: &Transaction<'_>, memory: &AgentMemory) -> Result<()> {
    tx.execute(
        "UPDATE agent_memories SET
            identity_id = ?2, department = ?3, component = ?4, scope = ?5, content = ?6,
            content_hash = ?7, evidence_json = ?8, source = ?9, status = ?10, pinned = ?11,
            created_at = ?12, updated_at = ?13, expires_at = ?14, verified_at = ?15,
            superseded_by = ?16, stale_reason = ?17, origin = ?18
         WHERE id = ?1",
        params![
            memory.id,
            memory.identity_id,
            memory.department,
            memory.component,
            memory.scope,
            memory.content,
            memory.content_hash,
            serde_json::to_string(&memory.evidence)?,
            memory.source,
            memory.status,
            if memory.pinned { 1 } else { 0 },
            memory.created_at,
            memory.updated_at,
            memory.expires_at,
            memory.verified_at,
            memory.superseded_by,
            memory.stale_reason,
            memory.origin
        ],
    )?;
    Ok(())
}

fn next_memory_seq_tx(tx: &Transaction<'_>, project_id: &str) -> Result<u64> {
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_memory_events WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(seq as u64)
}

pub(crate) fn insert_memory_event_tx(
    tx: &Transaction<'_>,
    kind: &str,
    memory: &AgentMemory,
    extra: serde_json::Value,
) -> Result<AgentMemoryEvent> {
    let seq = next_memory_seq_tx(tx, &memory.project_id)?;
    let mut payload = serde_json::json!({"memory": memory});
    if let (Some(object), serde_json::Value::Object(extra)) = (payload.as_object_mut(), extra) {
        for (key, value) in extra {
            object.insert(key, value);
        }
    }
    let event = AgentMemoryEvent {
        id: format!("mev-{}", Uuid::new_v4().simple()),
        project_id: memory.project_id.clone(),
        seq,
        kind: kind.to_string(),
        payload,
        created_at: now_rfc3339(),
    };
    tx.execute(
        "INSERT INTO agent_memory_events (id, project_id, seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.kind,
            serde_json::to_string(&event.payload)?,
            event.created_at
        ],
    )?;
    Ok(event)
}

fn ensure_local(memory: &AgentMemory) -> Result<()> {
    if memory.origin != "local" {
        return Err(memory_error(
            agent_memory_codes::MEMORY_IMPORTED_READONLY,
            "an imported memory is read-only evidence until an explicit rehome",
        ));
    }
    Ok(())
}

fn ensure_active(memory: &AgentMemory) -> Result<()> {
    if memory.status != "active" {
        return Err(memory_error(
            agent_memory_codes::MEMORY_CONFLICT,
            format!(
                "memory {} is {}; only active memory can be edited",
                memory.id, memory.status
            ),
        ));
    }
    Ok(())
}

/// Retirement makes agent memory read-only for content/evidence/expiry edits.
/// Lifecycle transitions (supersede, expire, forget) remain available.
fn ensure_identity_writable_tx(tx: &Transaction<'_>, memory: &AgentMemory) -> Result<()> {
    if memory.scope != "agent" {
        return Ok(());
    }
    let Some(identity_id) = memory.identity_id.as_deref() else {
        return Err(memory_error(
            agent_memory_codes::MEMORY_IDENTITY_REQUIRED,
            "agent memory requires a project identity",
        ));
    };
    let identity = memory_identity_tx(tx, identity_id)?;
    if identity.project_id != memory.project_id {
        return Err(memory_error(
            agent_memory_codes::MEMORY_PROJECT_MISMATCH,
            "agent memory identity belongs to another project",
        ));
    }
    if identity.origin != "local" {
        return Err(memory_error(
            agent_memory_codes::MEMORY_IMPORTED_READONLY,
            "an imported identity's agent memory is read-only until an explicit rehome",
        ));
    }
    if identity.status == "retired" {
        return Err(memory_error(
            agent_memory_codes::MEMORY_IDENTITY_RETIRED,
            "a retired identity's agent memory is read-only; supersede or expire it instead",
        ));
    }
    Ok(())
}

fn check_expected_hash(memory: &AgentMemory, expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    if memory.content_hash != expected {
        return Err(memory_error(
            agent_memory_codes::MEMORY_CONFLICT,
            format!(
                "memory {} changed since it was read (expected content hash {})",
                memory.id,
                short_hash(expected)
            ),
        ));
    }
    Ok(())
}

/// Create one memory row and its `created` event atomically. This never calls a
/// provider, starts a session, or touches the session `memories` table.
pub fn create_agent_memory_with_event(
    store: &Store,
    params: &CreateAgentMemoryParams,
) -> Result<(AgentMemory, AgentMemoryEvent)> {
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = OffsetDateTime::now_utc();
    let now_text = format_time(now)?;
    ensure_project_tx(&tx, &params.project_id)?;
    let scope = agent_memory_scope(params.scope.trim()).ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_SCOPE_INVALID,
            format!(
                "scope must be agent, department, or project: {}",
                params.scope
            ),
        )
    })?;
    let content = normalize_agent_memory_content(&params.content)?;
    let source = normalize_source(params.source.as_deref(), true)?;
    validate_agent_memory_evidence(&params.evidence)?;
    require_evidence_for_source(&source, &params.evidence)?;
    let expires_at = normalize_expiry(params.expires_at.as_deref(), now)?;
    let (identity_id, department, component) = resolve_scope_tx(
        &tx,
        &params.project_id,
        scope,
        params.identity_id.as_deref(),
        params.department.as_deref(),
        params.component.as_deref(),
    )?;
    let memory = AgentMemory {
        id: format!("mem-{}", Uuid::new_v4().simple()),
        project_id: params.project_id.clone(),
        identity_id,
        department,
        component,
        scope: scope.to_string(),
        content_hash: content_hash(&content),
        content,
        evidence: params.evidence.clone(),
        source,
        status: "active".to_string(),
        pinned: params.pinned,
        created_at: now_text.clone(),
        updated_at: now_text,
        expires_at,
        verified_at: None,
        superseded_by: None,
        stale_reason: None,
        origin: "local".to_string(),
        expired: false,
    };
    validate_storage_id("agent memory", &memory.id)?;
    insert_memory_row_tx(&tx, &memory)?;
    let event = insert_memory_event_tx(
        &tx,
        "agent.memory.created",
        &memory,
        serde_json::json!({"changed_fields": []}),
    )?;
    tx.commit()?;
    Ok((memory, event))
}

/// Safe read used by the RPC/tool surface. Visibility is enforced by the caller
/// with the loaded identity row.
pub fn get_agent_memory(store: &Store, memory_id: &str) -> Result<Option<AgentMemory>> {
    let conn = store.lock()?;
    conn.query_row(
        &format!("SELECT {AGENT_MEMORY_COLUMNS} FROM agent_memories WHERE id = ?1"),
        params![memory_id],
        map_memory,
    )
    .optional()
    .map_err(Into::into)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Bounded, filterable project memory read. Viewer scope isolation is applied
/// in SQL so a busy project cannot starve an identity's own memory before the
/// engine filters the result.
#[allow(clippy::too_many_arguments)]
pub fn list_agent_memories(
    store: &Store,
    project_id: &str,
    scope: Option<&str>,
    status: Option<&str>,
    department: Option<&str>,
    query: Option<&str>,
    include_expired: bool,
    limit: Option<u32>,
    viewer_identity_id: Option<&str>,
    viewer_department: Option<&str>,
) -> Result<Vec<AgentMemory>> {
    if let Some(scope) = scope
        && agent_memory_scope(scope).is_none()
    {
        return Err(memory_error(
            agent_memory_codes::MEMORY_SCOPE_INVALID,
            format!("scope must be agent, department, or project: {scope}"),
        ));
    }
    if let Some(status) = status
        && !AGENT_MEMORY_STATUSES.contains(&status)
    {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("status must be active, superseded, or stale: {status}"),
        ));
    }
    let limit = limit.unwrap_or(50).clamp(1, AGENT_MEMORY_MAX_LIST);
    let department = department.map(str::trim).filter(|value| !value.is_empty());
    let query = query.map(str::trim).filter(|value| !value.is_empty());
    let like = query.map(|value| format!("%{}%", escape_like(value)));
    let now = now_rfc3339();
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {AGENT_MEMORY_COLUMNS}
         FROM agent_memories
         WHERE project_id = ?1
           AND (?2 IS NULL OR scope = ?2)
           AND (?3 IS NULL OR status = ?3)
           AND (?4 IS NULL OR department = ?4 COLLATE NOCASE)
           AND (?5 IS NULL OR content LIKE ?5 ESCAPE '\\')
           AND (?6 = 1 OR expires_at IS NULL OR expires_at > ?7)
           AND (
             ?9 IS NULL
             OR (scope = 'agent' AND identity_id = ?9)
             OR (scope = 'department' AND ?10 IS NOT NULL
                 AND department = ?10 COLLATE NOCASE)
             OR scope = 'project'
           )
         ORDER BY created_at DESC, id ASC
         LIMIT ?8"
    ))?;
    let rows = statement.query_map(
        params![
            project_id,
            scope,
            status,
            department,
            like,
            if include_expired { 1 } else { 0 },
            now,
            limit as i64,
            viewer_identity_id,
            viewer_department
        ],
        map_memory,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Active agent-scope memory counts grouped by identity. This is the org-chart
/// count, not a content read; expired and superseded rows are excluded.
pub fn active_agent_memory_counts_by_identity(
    store: &Store,
    project_id: &str,
) -> Result<HashMap<String, u64>> {
    let now = now_rfc3339();
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT identity_id, COUNT(*) FROM agent_memories
         WHERE project_id = ?1
           AND scope = 'agent'
           AND status = 'active'
           AND identity_id IS NOT NULL
           AND (expires_at IS NULL OR expires_at > ?2)
         GROUP BY identity_id",
    )?;
    let rows = statement.query_map(params![project_id, now], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut counts = HashMap::new();
    for row in rows {
        let (identity_id, count) = row?;
        if count > 0 {
            counts.insert(identity_id, count as u64);
        }
    }
    Ok(counts)
}

/// Count the memory rows visible to one identity through the S5 scope rules:
/// its own agent memory, its department memory, and project memory. The count
/// excludes superseded and expired rows.
pub fn visible_agent_memory_count(
    store: &Store,
    project_id: &str,
    identity_id: &str,
    department: Option<&str>,
) -> Result<u64> {
    let now = now_rfc3339();
    let department = department.map(str::trim).filter(|value| !value.is_empty());
    let conn = store.lock()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_memories
         WHERE project_id = ?1
           AND status = 'active'
           AND (expires_at IS NULL OR expires_at > ?2)
           AND (
             (scope = 'agent' AND identity_id = ?3)
             OR (scope = 'department' AND ?4 IS NOT NULL AND department = ?4 COLLATE NOCASE)
             OR scope = 'project'
           )",
        params![project_id, now, identity_id, department],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

/// Bounded memory read for one identity using the S5 scope rules: its own agent
/// memory, its department memory, and project memory. This is an operator
/// projection, not a permission bypass; it still excludes superseded/expired
/// rows unless the caller explicitly asks for them.
pub fn list_visible_agent_memories(
    store: &Store,
    project_id: &str,
    identity_id: &str,
    department: Option<&str>,
    status: Option<&str>,
    include_expired: bool,
    limit: Option<u32>,
) -> Result<Vec<AgentMemory>> {
    if let Some(status) = status
        && !AGENT_MEMORY_STATUSES.contains(&status)
    {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("status must be active, superseded, or stale: {status}"),
        ));
    }
    let limit = limit.unwrap_or(50).clamp(1, AGENT_MEMORY_MAX_LIST);
    let department = department.map(str::trim).filter(|value| !value.is_empty());
    let now = now_rfc3339();
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {AGENT_MEMORY_COLUMNS}
         FROM agent_memories
         WHERE project_id = ?1
           AND (?2 IS NULL OR status = ?2)
           AND (?3 = 1 OR expires_at IS NULL OR expires_at > ?4)
           AND (
             (scope = 'agent' AND identity_id = ?5)
             OR (scope = 'department' AND ?6 IS NOT NULL AND department = ?6 COLLATE NOCASE)
             OR scope = 'project'
           )
         ORDER BY created_at DESC, id ASC
         LIMIT ?7"
    ))?;
    let rows = statement.query_map(
        params![
            project_id,
            status,
            if include_expired { 1 } else { 0 },
            now,
            identity_id,
            department,
            limit as i64
        ],
        map_memory,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn count_agent_memories(
    store: &Store,
    project_id: &str,
    scope: Option<&str>,
    status: Option<&str>,
    department: Option<&str>,
    query: Option<&str>,
    include_expired: bool,
) -> Result<u64> {
    let department = department.map(str::trim).filter(|value| !value.is_empty());
    let query = query.map(str::trim).filter(|value| !value.is_empty());
    let like = query.map(|value| format!("%{}%", escape_like(value)));
    let now = now_rfc3339();
    let conn = store.lock()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_memories
         WHERE project_id = ?1
           AND (?2 IS NULL OR scope = ?2)
           AND (?3 IS NULL OR status = ?3)
           AND (?4 IS NULL OR department = ?4 COLLATE NOCASE)
           AND (?5 IS NULL OR content LIKE ?5 ESCAPE '\\')
           AND (?6 = 1 OR expires_at IS NULL OR expires_at > ?7)",
        params![
            project_id,
            scope,
            status,
            department,
            like,
            if include_expired { 1 } else { 0 },
            now
        ],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

/// Candidates for the bounded prompt brief. Only local, active, non-expired
/// rows are returned. Scope filtering and deterministic ranking happen in
/// `vp_core::identity_memory`.
pub fn list_agent_memories_for_brief(
    store: &Store,
    project_id: &str,
    viewer_identity_id: Option<&str>,
    viewer_department: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<AgentMemory>> {
    let limit = limit.unwrap_or(128).clamp(1, 256);
    let now = now_rfc3339();
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {AGENT_MEMORY_COLUMNS}
         FROM agent_memories
         WHERE project_id = ?1
           AND status = 'active'
           AND origin = 'local'
           AND (expires_at IS NULL OR expires_at > ?2)
           AND (
             ?3 IS NULL
             OR (scope = 'agent' AND identity_id = ?3)
             OR (scope = 'department' AND ?4 IS NOT NULL
                 AND department = ?4 COLLATE NOCASE)
             OR scope = 'project'
           )
         ORDER BY updated_at DESC, content_hash ASC, id ASC
         LIMIT ?5"
    ))?;
    let rows = statement.query_map(
        params![
            project_id,
            now,
            viewer_identity_id,
            viewer_department,
            limit as i64
        ],
        map_memory,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn list_agent_memories_for_replay(store: &Store, project_id: &str) -> Result<Vec<AgentMemory>> {
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {AGENT_MEMORY_COLUMNS} FROM agent_memories
         WHERE project_id = ?1 ORDER BY id ASC"
    ))?;
    let rows = statement.query_map(params![project_id], map_memory)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Update bounded content, evidence, pin state, or expiry. A content change
/// invalidates prior verification. `None` means no field changed and no event
/// was appended.
pub fn update_agent_memory_with_event(
    store: &Store,
    params: &UpdateAgentMemoryParams,
) -> Result<Option<(AgentMemory, AgentMemoryEvent)>> {
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_memory_tx(&tx, &params.memory_id)?.ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_NOT_FOUND,
            format!("agent memory not found: {}", params.memory_id),
        )
    })?;
    ensure_local(&existing)?;
    ensure_active(&existing)?;
    ensure_identity_writable_tx(&tx, &existing)?;
    check_expected_hash(&existing, params.expected_content_hash.as_deref())?;
    let now = OffsetDateTime::now_utc();
    let now_text = format_time(now)?;
    let mut next = existing.clone();
    let mut changed_fields = Vec::new();
    if let Some(content) = params.content.as_deref() {
        let content = normalize_agent_memory_content(content)?;
        if content != next.content {
            next.content_hash = content_hash(&content);
            next.content = content;
            next.verified_at = None;
            changed_fields.push("content");
            changed_fields.push("content_hash");
            changed_fields.push("verified_at");
        }
    }
    if let Some(evidence) = params.evidence.as_ref()
        && *evidence != next.evidence
    {
        validate_agent_memory_evidence(evidence)?;
        require_evidence_for_source(&next.source, evidence)?;
        next.evidence = evidence.clone();
        changed_fields.push("evidence");
    }
    if let Some(pinned) = params.pinned
        && pinned != next.pinned
    {
        next.pinned = pinned;
        changed_fields.push("pinned");
    }
    if let Some(expires_at) = params.expires_at.as_ref() {
        let normalized = normalize_expiry(expires_at.as_deref(), now)?;
        if normalized != next.expires_at {
            next.expires_at = normalized;
            changed_fields.push("expires_at");
        }
    }
    if changed_fields.is_empty() {
        return Ok(None);
    }
    next.updated_at = now_text;
    update_memory_row_tx(&tx, &next)?;
    let event = insert_memory_event_tx(
        &tx,
        "agent.memory.updated",
        &next,
        serde_json::json!({"changed_fields": changed_fields}),
    )?;
    tx.commit()?;
    Ok(Some((next, event)))
}

fn create_replacement_tx(
    tx: &Transaction<'_>,
    old: &AgentMemory,
    content: &str,
    evidence: &[vp_protocol::types::MemoryEvidence],
    source: Option<&str>,
) -> Result<(AgentMemory, AgentMemoryEvent)> {
    let content = normalize_agent_memory_content(content)?;
    let source = normalize_source(source.or(Some(old.source.as_str())), true)?;
    validate_agent_memory_evidence(evidence)?;
    require_evidence_for_source(&source, evidence)?;
    let now = OffsetDateTime::now_utc();
    let now_text = format_time(now)?;
    let memory = AgentMemory {
        id: format!("mem-{}", Uuid::new_v4().simple()),
        project_id: old.project_id.clone(),
        identity_id: old.identity_id.clone(),
        department: old.department.clone(),
        component: old.component.clone(),
        scope: old.scope.clone(),
        content_hash: content_hash(&content),
        content,
        evidence: evidence.to_vec(),
        source,
        status: "active".to_string(),
        pinned: false,
        created_at: now_text.clone(),
        updated_at: now_text,
        expires_at: None,
        verified_at: None,
        superseded_by: None,
        stale_reason: None,
        origin: "local".to_string(),
        expired: false,
    };
    validate_storage_id("agent memory", &memory.id)?;
    insert_memory_row_tx(tx, &memory)?;
    let event = insert_memory_event_tx(
        tx,
        "agent.memory.created",
        &memory,
        serde_json::json!({"changed_fields": [], "supersedes": old.id}),
    )?;
    Ok((memory, event))
}

/// Atomically point one active memory at a replacement. The replacement is
/// either an existing active row in the exact same scope or a newly created
/// row. The old row becomes `superseded` with a terminal `superseded_by` edge.
pub fn supersede_agent_memory_with_events(
    store: &Store,
    params: &SupersedeAgentMemoryParams,
) -> Result<MemorySupersede> {
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_memory_tx(&tx, &params.memory_id)?.ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_NOT_FOUND,
            format!("agent memory not found: {}", params.memory_id),
        )
    })?;
    ensure_local(&existing)?;
    ensure_active(&existing)?;
    check_expected_hash(&existing, params.expected_content_hash.as_deref())?;
    let mut events = Vec::new();
    let replacement = if let Some(replacement_id) = params
        .replacement_memory_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        if replacement_id == existing.id {
            return Err(memory_error(
                agent_memory_codes::MEMORY_INVALID,
                "a memory cannot supersede itself",
            ));
        }
        let replacement = get_memory_tx(&tx, replacement_id)?.ok_or_else(|| {
            memory_error(
                agent_memory_codes::MEMORY_NOT_FOUND,
                format!("replacement memory not found: {replacement_id}"),
            )
        })?;
        ensure_local(&replacement)?;
        ensure_active(&replacement)?;
        if replacement.project_id != existing.project_id
            || replacement.scope != existing.scope
            || replacement.identity_id != existing.identity_id
            || replacement.department != existing.department
        {
            return Err(memory_error(
                agent_memory_codes::MEMORY_SCOPE_FORBIDDEN,
                "replacement memory must have the exact same project and scope",
            ));
        }
        replacement
    } else {
        let content = params.content.as_deref().ok_or_else(|| {
            memory_error(
                agent_memory_codes::MEMORY_INVALID,
                "supersede requires replacement_memory_id or bounded content",
            )
        })?;
        let evidence = if params.evidence.is_empty() {
            &existing.evidence
        } else {
            &params.evidence
        };
        let (replacement, event) =
            create_replacement_tx(&tx, &existing, content, evidence, params.source.as_deref())?;
        events.push(event);
        replacement
    };
    let now = OffsetDateTime::now_utc();
    let mut previous = existing.clone();
    previous.status = "superseded".to_string();
    previous.superseded_by = Some(replacement.id.clone());
    previous.updated_at = format_time(now)?;
    update_memory_row_tx(&tx, &previous)?;
    events.push(insert_memory_event_tx(
        &tx,
        "agent.memory.superseded",
        &previous,
        serde_json::json!({"superseded_by": replacement.id}),
    )?);
    tx.commit()?;
    Ok(MemorySupersede {
        previous,
        replacement,
        events,
    })
}

/// Soft-forget (`prune=false`) or hard-prune one memory. Soft forget keeps the
/// row and its evidence as stale; prune is only allowed after a row has left
/// active state. Imported evidence is never mutated.
pub fn forget_agent_memory_with_event(
    store: &Store,
    params: &ForgetAgentMemoryParams,
) -> Result<Option<(AgentMemory, AgentMemoryEvent)>> {
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_memory_tx(&tx, &params.memory_id)?.ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_NOT_FOUND,
            format!("agent memory not found: {}", params.memory_id),
        )
    })?;
    ensure_local(&existing)?;
    if params.prune {
        if existing.status == "active" {
            return Err(memory_error(
                agent_memory_codes::MEMORY_CONFLICT,
                "forget an active memory before pruning it",
            ));
        }
        tx.execute(
            "DELETE FROM agent_memories WHERE id = ?1",
            params![existing.id],
        )?;
        let event = insert_memory_event_tx(
            &tx,
            "agent.memory.deleted",
            &existing,
            serde_json::json!({"mode": "prune"}),
        )?;
        tx.commit()?;
        return Ok(Some((existing, event)));
    }
    if existing.status != "active" {
        return Ok(None);
    }
    let now = OffsetDateTime::now_utc();
    let mut next = existing;
    next.status = "stale".to_string();
    next.stale_reason = Some("forgotten".to_string());
    next.updated_at = format_time(now)?;
    update_memory_row_tx(&tx, &next)?;
    let event = insert_memory_event_tx(
        &tx,
        "agent.memory.deleted",
        &next,
        serde_json::json!({"mode": "soft"}),
    )?;
    tx.commit()?;
    Ok(Some((next, event)))
}

/// Logically erase one local memory and every prior event for it.
///
/// The projection row and matching `agent_memory_events` rows are deleted, and
/// one content-free `agent.memory.erased` tombstone is appended with the old
/// content hash. Replay therefore cannot reconstruct the erased content. This
/// is logical erasure from the live SQLite rows, not a secure wipe of database
/// free pages, WAL segments, or external backups.
pub fn erase_agent_memory(
    store: &Store,
    params: &EraseAgentMemoryParams,
) -> Result<AgentMemoryEraseResult> {
    if !params.approved {
        return Err(memory_error(
            agent_memory_codes::MEMORY_APPROVAL_REQUIRED,
            "logical memory erasure requires explicit approved: true",
        ));
    }
    let reason = params
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| reason.chars().take(500).collect::<String>());
    if let Some(reason) = &reason
        && reason.chars().any(char::is_control)
    {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            "erasure reason must not contain control characters",
        ));
    }
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_memory_tx(&tx, &params.memory_id)?.ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_NOT_FOUND,
            format!("agent memory not found: {}", params.memory_id),
        )
    })?;
    ensure_local(&existing)?;
    if let Some(project_id) = params
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|project| !project.is_empty())
        && project_id != existing.project_id
    {
        return Err(memory_error(
            agent_memory_codes::MEMORY_PROJECT_MISMATCH,
            "memory belongs to another project",
        ));
    }
    let events_removed = tx.execute(
        "DELETE FROM agent_memory_events
         WHERE project_id = ?1
           AND (json_extract(payload, '$.memory.id') = ?2
                OR json_extract(payload, '$.memory_id') = ?2)",
        params![existing.project_id, existing.id],
    )? as u64;
    tx.execute(
        "DELETE FROM agent_memories
         WHERE id = ?1 AND project_id = ?2 AND origin = 'local'",
        params![existing.id, existing.project_id],
    )?;
    let now = now_rfc3339();
    let event = AgentMemoryEvent {
        id: format!("mev-{}", Uuid::new_v4().simple()),
        project_id: existing.project_id.clone(),
        seq: next_memory_seq_tx(&tx, &existing.project_id)?,
        kind: vp_protocol::types::AGENT_MEMORY_ERASED_EVENT.to_string(),
        payload: serde_json::json!({
            "memory_id": existing.id,
            "content_hash": existing.content_hash,
            "scope": existing.scope,
            "identity_id": existing.identity_id,
            "erased_at": now,
            "reason": reason,
            "approval_risk": "host_state",
        }),
        created_at: now,
    };
    tx.execute(
        "INSERT INTO agent_memory_events (id, project_id, seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.kind,
            serde_json::to_string(&event.payload)?,
            event.created_at
        ],
    )?;
    tx.commit()?;
    Ok(AgentMemoryEraseResult {
        memory_id: existing.id,
        erased: true,
        events_removed,
    })
}

/// Record explicit verification. Verification is evidence, not truth: it only
/// records that an operator/agent re-checked the claim and optionally extends
/// its bounded TTL. It never mints governance authority.
pub fn verify_agent_memory_with_event(
    store: &Store,
    params: &VerifyAgentMemoryParams,
) -> Result<Option<(AgentMemory, AgentMemoryEvent)>> {
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_memory_tx(&tx, &params.memory_id)?.ok_or_else(|| {
        memory_error(
            agent_memory_codes::MEMORY_NOT_FOUND,
            format!("agent memory not found: {}", params.memory_id),
        )
    })?;
    ensure_local(&existing)?;
    ensure_active(&existing)?;
    // Verification extends trust/TTL; a retired identity's agent memory is
    // read-only for content and lifecycle edits. Supersede or expire instead.
    ensure_identity_writable_tx(&tx, &existing)?;
    let now = OffsetDateTime::now_utc();
    let now_text = format_time(now)?;
    let mut next = existing;
    let mut changed_fields = Vec::new();
    if next.verified_at.as_deref() != Some(now_text.as_str()) {
        next.verified_at = Some(now_text.clone());
        changed_fields.push("verified_at");
    }
    if let Some(days) = params.extend_days {
        if !(1..=AGENT_MEMORY_MAX_TTL_DAYS).contains(&days) {
            return Err(memory_error(
                agent_memory_codes::MEMORY_INVALID,
                format!("extend_days must be 1..={AGENT_MEMORY_MAX_TTL_DAYS}"),
            ));
        }
        let extended = format_time(now + time::Duration::days(i64::from(days)))?;
        let should_extend = next
            .expires_at
            .as_deref()
            .and_then(parse_time)
            .is_none_or(|current| {
                parse_time(&extended).is_some_and(|candidate| candidate > current)
            });
        if should_extend && next.expires_at.as_deref() != Some(extended.as_str()) {
            next.expires_at = Some(extended);
            changed_fields.push("expires_at");
        }
    }
    if changed_fields.is_empty() {
        return Ok(None);
    }
    next.updated_at = now_text;
    update_memory_row_tx(&tx, &next)?;
    let event = insert_memory_event_tx(
        &tx,
        "agent.memory.updated",
        &next,
        serde_json::json!({"changed_fields": changed_fields, "reason": "verify"}),
    )?;
    tx.commit()?;
    Ok(Some((next, event)))
}

/// Bounded expiry/prune sweep. Active rows past `expires_at` become stale with
/// an `expired` reason. Prune only removes stale/superseded local rows older
/// than the cutoff; imported evidence is never swept.
pub fn sweep_agent_memories_with_events(
    store: &Store,
    params: &SweepAgentMemoriesParams,
) -> Result<MemorySweep> {
    let limit = params.limit.unwrap_or(100).clamp(1, AGENT_MEMORY_MAX_SWEEP);
    let older_than_days = params.older_than_days.unwrap_or(30);
    if !(1..=AGENT_MEMORY_MAX_TTL_DAYS).contains(&older_than_days) {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("older_than_days must be 1..={AGENT_MEMORY_MAX_TTL_DAYS}"),
        ));
    }
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = OffsetDateTime::now_utc();
    let now_text = format_time(now)?;
    let mut events = Vec::new();
    let mut expired = 0u64;
    let due = {
        let mut statement = tx.prepare(&format!(
            "SELECT {AGENT_MEMORY_COLUMNS} FROM agent_memories
             WHERE project_id = ?1 AND status = 'active' AND origin = 'local'
               AND expires_at IS NOT NULL AND expires_at <= ?2
             ORDER BY expires_at ASC, id ASC LIMIT ?3"
        ))?;
        statement
            .query_map(
                params![params.project_id, now_text, limit as i64],
                map_memory,
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    for memory in due {
        let mut next = memory;
        next.status = "stale".to_string();
        next.stale_reason = Some("expired".to_string());
        next.updated_at = now_text.clone();
        update_memory_row_tx(&tx, &next)?;
        events.push(insert_memory_event_tx(
            &tx,
            "agent.memory.expired",
            &next,
            serde_json::json!({"changed_fields": ["status", "stale_reason"]}),
        )?);
        expired += 1;
    }
    let mut pruned = 0u64;
    if params.prune {
        let cutoff = format_time(now - time::Duration::days(i64::from(older_than_days)))?;
        let removable = {
            let mut statement = tx.prepare(&format!(
                "SELECT {AGENT_MEMORY_COLUMNS} FROM agent_memories
                 WHERE project_id = ?1 AND origin = 'local'
                   AND status IN ('stale', 'superseded')
                   AND updated_at < ?2
                 ORDER BY updated_at ASC, id ASC LIMIT ?3"
            ))?;
            statement
                .query_map(params![params.project_id, cutoff, limit as i64], map_memory)?
                .collect::<Result<Vec<_>, _>>()?
        };
        for memory in removable {
            tx.execute(
                "DELETE FROM agent_memories WHERE id = ?1",
                params![memory.id],
            )?;
            events.push(insert_memory_event_tx(
                &tx,
                "agent.memory.deleted",
                &memory,
                serde_json::json!({"mode": "prune"}),
            )?);
            pruned += 1;
        }
    }
    let remaining: i64 = tx.query_row(
        "SELECT COUNT(*) FROM agent_memories WHERE project_id = ?1",
        params![params.project_id],
        |row| row.get(0),
    )?;
    tx.commit()?;
    Ok(MemorySweep {
        expired,
        pruned,
        remaining: remaining as u64,
        events,
    })
}

/// Count the project's memory events without decoding payloads. Replay repair
/// uses this to detect a stream larger than its bounded read; repairing from a
/// truncated stream would delete rows that still have events.
pub fn count_agent_memory_events(store: &Store, project_id: &str) -> Result<u64> {
    let conn = store.lock()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_memory_events WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

pub fn list_agent_memory_events_for_project(
    store: &Store,
    project_id: &str,
    limit: Option<u32>,
) -> Result<Vec<AgentMemoryEvent>> {
    let limit = limit
        .unwrap_or(AGENT_MEMORY_EVENT_LIMIT)
        .clamp(1, AGENT_MEMORY_EVENT_LIMIT);
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT id, project_id, seq, kind, payload, created_at
         FROM agent_memory_events WHERE project_id = ?1
         ORDER BY seq ASC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_id, limit as i64], |row| {
        let payload: String = row.get(4)?;
        Ok(AgentMemoryEvent {
            id: row.get(0)?,
            project_id: row.get(1)?,
            seq: row.get::<_, i64>(2)? as u64,
            kind: row.get(3)?,
            payload: serde_json::from_str(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            created_at: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Validate a stored snapshot without touching the database. This is shared by
/// archive import and projection rebuild so an invalid row never enters SQLite.
pub fn validate_memory_snapshot(memory: &AgentMemory) -> Result<()> {
    if memory.id.trim().is_empty() || memory.project_id.trim().is_empty() {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            "memory snapshot has an empty id or project id",
        ));
    }
    if agent_memory_scope(&memory.scope).is_none() {
        return Err(memory_error(
            agent_memory_codes::MEMORY_SCOPE_INVALID,
            format!("memory {} has an invalid scope", memory.id),
        ));
    }
    validate_agent_memory_scope_shape(
        &memory.scope,
        memory.identity_id.as_deref(),
        memory.department.as_deref(),
    )?;
    if !AGENT_MEMORY_STATUSES.contains(&memory.status.as_str()) {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("memory {} has an invalid status", memory.id),
        ));
    }
    if agent_memory_source(&memory.source).is_none() {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("memory {} has an invalid source", memory.id),
        ));
    }
    if memory.origin != "local" && memory.origin != "imported" {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("memory {} has an invalid origin", memory.id),
        ));
    }
    let content = normalize_agent_memory_content(&memory.content)?;
    if content != memory.content {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("memory {} content is not normalized", memory.id),
        ));
    }
    if memory.content_hash != content_hash(&content) {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("memory {} content hash does not match content", memory.id),
        ));
    }
    validate_agent_memory_evidence(&memory.evidence)?;
    validate_storage_id("agent memory", &memory.id)?;
    Ok(())
}

pub fn insert_agent_memory_snapshot(store: &Store, memory: &AgentMemory) -> Result<()> {
    validate_memory_snapshot(memory)?;
    let conn = store.lock()?;
    conn.execute(
        "INSERT INTO agent_memories
         (id, project_id, identity_id, department, component, scope, content, content_hash,
          evidence_json, source, status, pinned, created_at, updated_at, expires_at, verified_at,
          superseded_by, stale_reason, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(id) DO NOTHING",
        params![
            memory.id,
            memory.project_id,
            memory.identity_id,
            memory.department,
            memory.component,
            memory.scope,
            memory.content,
            memory.content_hash,
            serde_json::to_string(&memory.evidence)?,
            memory.source,
            memory.status,
            if memory.pinned { 1 } else { 0 },
            memory.created_at,
            memory.updated_at,
            memory.expires_at,
            memory.verified_at,
            memory.superseded_by,
            memory.stale_reason,
            memory.origin
        ],
    )?;
    Ok(())
}

pub fn insert_agent_memory_event_snapshot(store: &Store, event: &AgentMemoryEvent) -> Result<()> {
    let conn = store.lock()?;
    conn.execute(
        "INSERT INTO agent_memory_events (id, project_id, seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO NOTHING",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.kind,
            serde_json::to_string(&event.payload)?,
            event.created_at
        ],
    )?;
    Ok(())
}

/// Replace one project's memory projection from an authoritative snapshot.
/// Imported rows keep their read-only provenance marker across a repair.
pub fn replace_agent_memory_projection(
    store: &Store,
    project_id: &str,
    memories: &[AgentMemory],
) -> Result<()> {
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let imported = {
        let mut statement = tx.prepare(
            "SELECT id FROM agent_memories WHERE project_id = ?1 AND origin = 'imported'",
        )?;
        statement
            .query_map(params![project_id], |row| row.get::<_, String>(0))?
            .collect::<Result<BTreeSet<_>, _>>()?
    };
    // Imported rows are archive evidence. A local event stream cannot prove
    // they are wrong, so repair never deletes them; it only rebuilds local
    // rows. Expected imported rows are still upserted below with their origin
    // preserved.
    tx.execute(
        "DELETE FROM agent_memories WHERE project_id = ?1 AND origin = 'local'",
        params![project_id],
    )?;
    for memory in memories {
        let mut memory = memory.clone();
        if memory.project_id != project_id {
            return Err(memory_error(
                agent_memory_codes::MEMORY_PROJECT_MISMATCH,
                format!("memory {} belongs to another project", memory.id),
            ));
        }
        if imported.contains(&memory.id) {
            memory.origin = "imported".to_string();
        }
        validate_memory_snapshot(&memory)?;
        insert_memory_row_tx(&tx, &memory)?;
    }
    tx.commit()?;
    Ok(())
}

/// True when a memory event id already exists with identical bytes. Used by the
/// archive import path before inserting a project stream.
pub fn memory_event_matches(store: &Store, event: &AgentMemoryEvent) -> Result<Option<bool>> {
    let conn = store.lock()?;
    let existing = conn
        .query_row(
            "SELECT seq, kind, payload, created_at FROM agent_memory_events WHERE id = ?1",
            params![event.id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((seq, kind, payload, created_at)) = existing else {
        return Ok(None);
    };
    Ok(Some(
        seq == event.seq as i64
            && kind == event.kind
            && payload == serde_json::to_string(&event.payload)?
            && created_at == event.created_at,
    ))
}

/// True for an event kind that this store understands. Bundle validation uses
/// this without borrowing the event stream.
pub fn known_memory_event_kind(kind: &str) -> bool {
    AGENT_MEMORY_EVENT_KINDS.contains(&kind)
}

impl Store {
    pub fn insert_agent_memory_snapshot(&self, memory: &AgentMemory) -> Result<()> {
        insert_agent_memory_snapshot(self, memory)
    }

    pub fn insert_agent_memory_event_snapshot(&self, event: &AgentMemoryEvent) -> Result<()> {
        insert_agent_memory_event_snapshot(self, event)
    }

    pub fn replace_agent_memory_projection(
        &self,
        project_id: &str,
        memories: &[AgentMemory],
    ) -> Result<()> {
        replace_agent_memory_projection(self, project_id, memories)
    }

    pub fn list_agent_memories_for_replay(&self, project_id: &str) -> Result<Vec<AgentMemory>> {
        list_agent_memories_for_replay(self, project_id)
    }

    pub fn list_agent_memory_events_for_project(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AgentMemoryEvent>> {
        list_agent_memory_events_for_project(self, project_id, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{CreateAgentIdentityParams, MemoryEvidence};

    fn store() -> (TempDir, Store, String) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let repo = TempDir::new().unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("memory"))
            .unwrap();
        (dir, store, project.id)
    }

    fn identity(store: &Store, project_id: &str, department: &str) -> String {
        store
            .create_agent_identity(&CreateAgentIdentityParams {
                project_id: project_id.to_string(),
                role: "worker".to_string(),
                department: Some(department.to_string()),
                component: Some("auth".to_string()),
                ..CreateAgentIdentityParams::default()
            })
            .unwrap()
            .0
            .id
    }

    fn create_params(
        project_id: &str,
        scope: &str,
        identity_id: Option<&str>,
    ) -> CreateAgentMemoryParams {
        CreateAgentMemoryParams {
            project_id: project_id.to_string(),
            identity_id: identity_id.map(str::to_string),
            department: (scope == "department").then(|| "Platform".to_string()),
            scope: scope.to_string(),
            content: format!("{scope} memory content"),
            source: Some("operator".to_string()),
            evidence: vec![MemoryEvidence {
                kind: "task".to_string(),
                reference: "T1".to_string(),
                note: None,
            }],
            ..CreateAgentMemoryParams::default()
        }
    }

    #[test]
    fn scopes_resolve_identity_department_and_project_without_crossing() {
        let (_dir, store, project) = store();
        let worker = identity(&store, &project, "Platform");
        let (agent, _) = create_agent_memory_with_event(
            &store,
            &create_params(&project, "agent", Some(&worker)),
        )
        .unwrap();
        assert_eq!(agent.identity_id.as_deref(), Some(worker.as_str()));
        assert_eq!(agent.department.as_deref(), Some("Platform"));
        assert_eq!(agent.component.as_deref(), Some("auth"));
        let (department, _) =
            create_agent_memory_with_event(&store, &create_params(&project, "department", None))
                .unwrap();
        assert!(department.identity_id.is_none());
        assert_eq!(department.department.as_deref(), Some("Platform"));
        let (project_memory, _) =
            create_agent_memory_with_event(&store, &create_params(&project, "project", None))
                .unwrap();
        assert!(project_memory.identity_id.is_none());
        assert!(project_memory.department.is_none());

        let mut wrong_scope = create_params(&project, "agent", Some(&worker));
        wrong_scope.department = Some("Billing".to_string());
        let error = create_agent_memory_with_event(&store, &wrong_scope).unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_SCOPE_INVALID
        );
        let mut cross_project = create_params(&project, "agent", Some(&worker));
        let other_repo = TempDir::new().unwrap();
        let other_project = store
            .add_project(other_repo.path().to_str().unwrap(), Some("other"))
            .unwrap();
        let other_worker = identity(&store, &other_project.id, "Platform");
        cross_project.identity_id = Some(other_worker);
        let error = create_agent_memory_with_event(&store, &cross_project).unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_PROJECT_MISMATCH
        );
        let error = create_agent_memory_with_event(&store, &create_params(&project, "agent", None))
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_IDENTITY_REQUIRED
        );
        let error = create_agent_memory_with_event(
            &store,
            &CreateAgentMemoryParams {
                department: None,
                ..create_params(&project, "department", None)
            },
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_DEPARTMENT_REQUIRED
        );
    }

    #[test]
    fn content_hash_evidence_and_ttl_are_bounded() {
        let (_dir, store, project) = store();
        let worker = identity(&store, &project, "Platform");
        let mut params = create_params(&project, "agent", Some(&worker));
        params.content = "  padded claim  ".to_string();
        let (memory, _) = create_agent_memory_with_event(&store, &params).unwrap();
        assert_eq!(memory.content, "padded claim");
        assert_eq!(memory.content_hash, content_hash("padded claim"));

        params.content = "a".repeat(vp_protocol::types::AGENT_MEMORY_MAX_CONTENT_CHARS + 1);
        assert!(create_agent_memory_with_event(&store, &params).is_err());
        params.content = "ok".to_string();
        params.evidence = vec![MemoryEvidence {
            kind: "prompt".to_string(),
            reference: "secret".to_string(),
            note: None,
        }];
        assert!(create_agent_memory_with_event(&store, &params).is_err());
        params.evidence.clear();
        params.source = Some("operator".to_string());
        params.expires_at = Some("2020-01-01T00:00:00Z".to_string());
        assert!(create_agent_memory_with_event(&store, &params).is_err());
        params.expires_at = Some("2040-01-01T00:00:00Z".to_string());
        assert!(create_agent_memory_with_event(&store, &params).is_err());
        let source_requires_evidence = CreateAgentMemoryParams {
            source: Some("verifier".to_string()),
            evidence: Vec::new(),
            ..create_params(&project, "project", None)
        };
        assert!(create_agent_memory_with_event(&store, &source_requires_evidence).is_err());
    }

    #[test]
    fn update_requires_expected_hash_and_supersede_is_atomic() {
        let (_dir, store, project) = store();
        let worker = identity(&store, &project, "Platform");
        let (memory, _) = create_agent_memory_with_event(
            &store,
            &create_params(&project, "agent", Some(&worker)),
        )
        .unwrap();
        let error = update_agent_memory_with_event(
            &store,
            &UpdateAgentMemoryParams {
                memory_id: memory.id.clone(),
                content: Some("changed".to_string()),
                expected_content_hash: Some("deadbeef".to_string()),
                ..UpdateAgentMemoryParams::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_CONFLICT
        );
        let (updated, event) = update_agent_memory_with_event(
            &store,
            &UpdateAgentMemoryParams {
                memory_id: memory.id.clone(),
                content: Some("changed".to_string()),
                expected_content_hash: Some(memory.content_hash.clone()),
                ..UpdateAgentMemoryParams::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(updated.content, "changed");
        assert_eq!(event.kind, "agent.memory.updated");
        assert_ne!(updated.content_hash, memory.content_hash);

        let superseded = supersede_agent_memory_with_events(
            &store,
            &SupersedeAgentMemoryParams {
                memory_id: updated.id.clone(),
                content: Some("replacement".to_string()),
                source: Some("operator".to_string()),
                ..SupersedeAgentMemoryParams::default()
            },
        )
        .unwrap();
        assert_eq!(superseded.previous.status, "superseded");
        assert_eq!(
            superseded.previous.superseded_by.as_deref(),
            Some(superseded.replacement.id.as_str())
        );
        assert_eq!(superseded.events.len(), 2);
        assert_eq!(superseded.events[0].kind, "agent.memory.created");
        assert_eq!(superseded.events[1].kind, "agent.memory.superseded");
        let error = update_agent_memory_with_event(
            &store,
            &UpdateAgentMemoryParams {
                memory_id: updated.id,
                content: Some("late".to_string()),
                ..UpdateAgentMemoryParams::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_CONFLICT
        );
    }

    #[test]
    fn forget_prune_verify_and_sweep_follow_lifecycle() {
        let (_dir, store, project) = store();
        let worker = identity(&store, &project, "Platform");
        let (memory, _) = create_agent_memory_with_event(
            &store,
            &create_params(&project, "agent", Some(&worker)),
        )
        .unwrap();
        let (verified, _) = verify_agent_memory_with_event(
            &store,
            &VerifyAgentMemoryParams {
                memory_id: memory.id.clone(),
                extend_days: Some(10),
            },
        )
        .unwrap()
        .unwrap();
        assert!(verified.verified_at.is_some());
        assert!(verified.expires_at.is_some());
        let (forgotten, event) = forget_agent_memory_with_event(
            &store,
            &ForgetAgentMemoryParams {
                memory_id: memory.id.clone(),
                prune: false,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(forgotten.status, "stale");
        assert_eq!(event.kind, "agent.memory.deleted");
        assert_eq!(
            event
                .payload
                .get("mode")
                .and_then(serde_json::Value::as_str),
            Some("soft")
        );
        let (pruned, event) = forget_agent_memory_with_event(
            &store,
            &ForgetAgentMemoryParams {
                memory_id: memory.id.clone(),
                prune: true,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(pruned.id, memory.id);
        assert!(get_agent_memory(&store, &memory.id).unwrap().is_none());
        assert_eq!(event.kind, "agent.memory.deleted");
    }

    #[test]
    fn imported_rows_are_read_only_and_never_swept() {
        let (_dir, store, project) = store();
        let worker = identity(&store, &project, "Platform");
        let (memory, _) = create_agent_memory_with_event(
            &store,
            &create_params(&project, "agent", Some(&worker)),
        )
        .unwrap();
        // Replace the row with an imported copy to exercise read-only paths.
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE agent_memories SET origin = 'imported', updated_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
            params![memory.id],
        )
        .unwrap();
        drop(conn);
        let error = update_agent_memory_with_event(
            &store,
            &UpdateAgentMemoryParams {
                memory_id: memory.id.clone(),
                content: Some("should fail".to_string()),
                ..UpdateAgentMemoryParams::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentMemoryError>().unwrap().code,
            agent_memory_codes::MEMORY_IMPORTED_READONLY
        );
        let sweep = sweep_agent_memories_with_events(
            &store,
            &SweepAgentMemoriesParams {
                project_id: project,
                prune: true,
                older_than_days: Some(1),
                ..SweepAgentMemoriesParams::default()
            },
        )
        .unwrap();
        assert_eq!(sweep.pruned, 0);
        assert_eq!(sweep.remaining, 1);
    }

    #[test]
    fn logical_erasure_removes_projection_and_prior_event_payloads() {
        let (_dir, store, project) = store();
        let (memory, _) =
            create_agent_memory_with_event(&store, &create_params(&project, "project", None))
                .unwrap();
        let before = list_agent_memory_events_for_project(&store, &project, None).unwrap();
        assert!(
            before
                .iter()
                .any(|event| { event.payload.to_string().contains("project memory content") })
        );

        let error = erase_agent_memory(
            &store,
            &EraseAgentMemoryParams {
                memory_id: memory.id.clone(),
                project_id: Some(project.clone()),
                reason: None,
                approved: false,
            },
        )
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<AgentMemoryError>()
                .map(|typed| typed.code),
            Some(agent_memory_codes::MEMORY_APPROVAL_REQUIRED)
        );

        let result = erase_agent_memory(
            &store,
            &EraseAgentMemoryParams {
                memory_id: memory.id.clone(),
                project_id: Some(project.clone()),
                reason: Some("privacy request".to_string()),
                approved: true,
            },
        )
        .unwrap();
        assert!(result.erased);
        assert!(result.events_removed >= 1);
        let rows = list_agent_memories(
            &store,
            &project,
            None,
            None,
            None,
            None,
            true,
            Some(10),
            None,
            None,
        )
        .unwrap();
        assert!(rows.is_empty(), "erased projection row must be gone");
        let after = list_agent_memory_events_for_project(&store, &project, None).unwrap();
        assert_eq!(after.len(), 1, "only one content-free tombstone remains");
        assert_eq!(after[0].kind, vp_protocol::types::AGENT_MEMORY_ERASED_EVENT);
        let payload = after[0].payload.to_string();
        assert!(!payload.contains("project memory content"));
        assert!(payload.contains(&memory.content_hash));
        assert!(payload.contains("privacy request"));
    }

    #[test]
    fn imported_memory_cannot_be_erased_locally() {
        let (_dir, store, project) = store();
        let (memory, _) =
            create_agent_memory_with_event(&store, &create_params(&project, "project", None))
                .unwrap();
        let mut imported = memory.clone();
        imported.id = "mem-imported".to_string();
        imported.origin = "imported".to_string();
        let mut conn = store.lock().unwrap();
        let tx = conn.transaction().unwrap();
        insert_memory_row_tx(&tx, &imported).unwrap();
        tx.commit().unwrap();
        drop(conn);
        let memory = imported;
        let error = erase_agent_memory(
            &store,
            &EraseAgentMemoryParams {
                memory_id: memory.id.clone(),
                project_id: Some(project),
                reason: None,
                approved: true,
            },
        )
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<AgentMemoryError>()
                .map(|typed| typed.code),
            Some(agent_memory_codes::MEMORY_IMPORTED_READONLY)
        );
    }
}
