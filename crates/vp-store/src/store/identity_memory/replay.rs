use super::*;

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
        .map(|value| value.clamp(1, AGENT_MEMORY_EVENT_LIMIT) as i64)
        .unwrap_or(-1);
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT id, project_id, seq, kind, payload, created_at
         FROM agent_memory_events WHERE project_id = ?1
         ORDER BY seq ASC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_id, limit], |row| {
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
