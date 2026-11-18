use super::*;

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
