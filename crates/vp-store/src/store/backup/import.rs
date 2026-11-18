use super::*;

impl Store {
    pub fn import_session_bundle(&self, bundle: &SessionBundle) -> Result<Event> {
        validate_session_bundle(bundle)?;
        for row in &bundle.prompt_prefix_observations {
            if row.project_id != bundle.project.id || row.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle prompt-prefix observation {}:{} does not belong to {}/{}",
                    row.project_id,
                    row.session_id,
                    bundle.project.id,
                    bundle.session.id
                ));
            }
            if !cache_epoch_hex(&row.cache_epoch) {
                return Err(anyhow!(
                    "bundle prompt-prefix observation has an invalid cache epoch"
                ));
            }
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let existing_root: Option<String> = tx
            .query_row(
                "SELECT root FROM projects WHERE id = ?1",
                params![bundle.project.id],
                |row| row.get(0),
            )
            .optional()?;
        match existing_root {
            Some(root) if root != bundle.project.root => {
                return Err(anyhow!(
                    "project {} already exists with a different root",
                    bundle.project.id
                ));
            }
            Some(_) => {}
            None => {
                tx.execute(
                    "INSERT INTO projects (id, name, root, default_branch, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        bundle.project.id,
                        bundle.project.name,
                        bundle.project.root,
                        bundle.project.default_branch,
                        bundle.project.created_at
                    ],
                )?;
            }
        }

        // Project identities are carried only as archive snapshots. Imported
        // rows are explicitly marked `imported` and stay read-only until a
        // rehome phase; a project id is never silently rewritten.
        for identity in &bundle.agent_identities {
            if identity.project_id != bundle.project.id {
                return Err(anyhow!(
                    "bundle identity {} belongs to project {}, not {}",
                    identity.id,
                    identity.project_id,
                    bundle.project.id
                ));
            }
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT project_id, origin FROM agent_identities WHERE id = ?1",
                    params![identity.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            match existing {
                Some((project_id, _)) if project_id != bundle.project.id => {
                    return Err(anyhow!(
                        "bundle identity {} conflicts with a local identity in another project",
                        identity.id
                    ));
                }
                Some(_) => {}
                None => {
                    let mut imported = identity.clone();
                    imported.origin = "imported".to_string();
                    insert_agent_identity_row_conn(&tx, &imported)?;
                }
            }
        }
        for event in &bundle.agent_identity_events {
            if event.project_id != bundle.project.id {
                return Err(anyhow!(
                    "bundle identity event {}:{} belongs to another project",
                    event.identity_id,
                    event.seq
                ));
            }
            let existing: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT kind, payload, created_at FROM agent_identity_events
                     WHERE identity_id = ?1 AND seq = ?2",
                    params![event.identity_id, event.seq as i64],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            match existing {
                Some((kind, payload, created_at)) => {
                    let expected_payload = serde_json::to_string(&event.payload)?;
                    if kind != event.kind
                        || payload != expected_payload
                        || created_at != event.created_at
                    {
                        return Err(anyhow!(
                            "bundle identity event {}:{} conflicts with the local event stream",
                            event.identity_id,
                            event.seq
                        ));
                    }
                }
                None => insert_agent_identity_event_snapshot_conn(&tx, event)?,
            }
        }

        // Project-scoped coordination events are portable by id. A same-id
        // event must be byte-identical; a per-project seq collision with a
        // different id fails closed rather than reordering local history.
        for event in &bundle.agent_coordination_events {
            let existing: Option<(i64, String, String, String)> = tx
                .query_row(
                    "SELECT seq, kind, payload, created_at FROM agent_coordination_events
                     WHERE id = ?1",
                    params![event.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let expected_payload = serde_json::to_string(&event.payload)?;
            match existing {
                Some((seq, kind, payload, created_at)) => {
                    if seq != event.seq as i64
                        || kind != event.kind
                        || payload != expected_payload
                        || created_at != event.created_at
                    {
                        return Err(anyhow!(
                            "bundle coordination event {} conflicts with the local event stream",
                            event.id
                        ));
                    }
                }
                None => {
                    let seq_owner: Option<String> = tx
                        .query_row(
                            "SELECT id FROM agent_coordination_events
                             WHERE project_id = ?1 AND seq = ?2",
                            params![bundle.project.id, event.seq as i64],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(owner) = seq_owner
                        && owner != event.id
                    {
                        return Err(anyhow!(
                            "bundle coordination event {} conflicts with local seq {}",
                            event.id,
                            event.seq
                        ));
                    }
                    tx.execute(
                        "INSERT INTO agent_coordination_events
                         (id, project_id, seq, kind, payload, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            event.id,
                            event.project_id,
                            event.seq as i64,
                            event.kind,
                            expected_payload,
                            event.created_at
                        ],
                    )?;
                }
            }
        }
        for assignment in &bundle.session_agent_assignments {
            let existing: Option<AssignmentProjectionRow> = tx
                .query_row(
                    "SELECT role, status, assigned_at, released_at, heartbeat_at, lease_expires_at
                     FROM session_agent_assignments
                     WHERE session_id = ?1 AND identity_id = ?2",
                    params![assignment.session_id, assignment.identity_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()?;
            match existing {
                Some((role, status, assigned_at, released_at, heartbeat_at, expires_at)) => {
                    if role != assignment.role
                        || status != assignment.status
                        || assigned_at != assignment.assigned_at
                        || released_at != assignment.released_at
                        || heartbeat_at != assignment.heartbeat_at
                        || expires_at != assignment.lease_expires_at
                    {
                        return Err(anyhow!(
                            "bundle assignment {}:{} conflicts with the local projection",
                            assignment.session_id,
                            assignment.identity_id
                        ));
                    }
                }
                None => {
                    crate::store::coordination::insert_assignment_row_tx(&tx, assignment)?;
                }
            }
        }
        for message in &bundle.agent_inbox {
            match crate::store::coordination::query_inbox_tx(&tx, &message.id)? {
                Some(existing) if &existing == message => {}
                Some(_) => {
                    return Err(anyhow!(
                        "bundle inbox message {} conflicts with the local projection",
                        message.id
                    ));
                }
                None => {
                    crate::store::coordination::insert_inbox_row_tx(&tx, message)?;
                }
            }
        }
        // Workspaces travel as archive evidence only. Imported rows are
        // read-only, carry no lease, and never attach a local worktree.
        for workspace in &bundle.agent_workspaces {
            let mut imported = workspace.clone();
            imported.origin = "imported".to_string();
            imported.lease_active = false;
            imported.lease_expires_at = None;
            match tx
                .query_row(
                    "SELECT status, updated_at FROM agent_workspaces WHERE id = ?1",
                    params![workspace.id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
            {
                Some((status, updated_at))
                    if status == imported.status && updated_at == imported.updated_at => {}
                Some(_) => {
                    return Err(anyhow!(
                        "bundle workspace {} conflicts with the local projection",
                        workspace.id
                    ));
                }
                None => {
                    crate::store::workspaces::insert_workspace_row_tx(&tx, &imported)?;
                }
            }
        }
        for event in &bundle.agent_workspace_events {
            let expected_payload = serde_json::to_string(&event.payload)?;
            match tx
                .query_row(
                    "SELECT seq, kind, payload, created_at FROM agent_workspace_events WHERE id = ?1",
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
                .optional()?
            {
                Some((seq, kind, payload, created_at)) => {
                    if seq != event.seq as i64
                        || kind != event.kind
                        || payload != expected_payload
                        || created_at != event.created_at
                    {
                        return Err(anyhow!(
                            "bundle workspace event {} conflicts with the local event stream",
                            event.id
                        ));
                    }
                }
                None => {
                    let seq_owner: Option<String> = tx
                        .query_row(
                            "SELECT id FROM agent_workspace_events
                             WHERE project_id = ?1 AND seq = ?2",
                            params![bundle.project.id, event.seq as i64],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(owner) = seq_owner
                        && owner != event.id
                    {
                        return Err(anyhow!(
                            "bundle workspace event {} conflicts with local seq {}",
                            event.id,
                            event.seq
                        ));
                    }
                    tx.execute(
                        "INSERT INTO agent_workspace_events
                         (id, project_id, seq, kind, payload, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            event.id,
                            event.project_id,
                            event.seq as i64,
                            event.kind,
                            expected_payload,
                            event.created_at
                        ],
                    )?;
                }
            }
        }

        // Memories travel as archive evidence only. Imported rows are
        // read-only, never enter a prompt brief, and never remap their project.
        let comparable_memory = |memory: &vp_protocol::types::AgentMemory| {
            let mut comparable = memory.clone();
            comparable.expired = false;
            comparable
        };
        for memory in &bundle.agent_memories {
            let mut imported = memory.clone();
            imported.origin = "imported".to_string();
            match crate::store::identity_memory::get_memory_tx(&tx, &memory.id)? {
                Some(existing) if comparable_memory(&existing) == comparable_memory(&imported) => {}
                Some(_) => {
                    return Err(anyhow!(
                        "bundle memory {} conflicts with the local projection",
                        memory.id
                    ));
                }
                None => {
                    crate::store::identity_memory::insert_memory_row_tx(&tx, &imported)?;
                }
            }
        }
        for event in &bundle.agent_memory_events {
            // Rehome the snapshot as read-only archive provenance. The rewrite
            // is deterministic, so a repeated import compares byte-for-byte
            // and a local event with the same id still conflicts.
            let mut payload = event.payload.clone();
            if let Some(memory) = payload
                .get_mut("memory")
                .and_then(serde_json::Value::as_object_mut)
            {
                memory.insert(
                    "origin".to_string(),
                    serde_json::Value::String("imported".to_string()),
                );
            }
            let expected_payload = serde_json::to_string(&payload)?;
            match tx
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
                .optional()?
            {
                Some((seq, kind, payload, created_at)) => {
                    if seq != event.seq as i64
                        || kind != event.kind
                        || payload != expected_payload
                        || created_at != event.created_at
                    {
                        return Err(anyhow!(
                            "bundle memory event {} conflicts with the local event stream",
                            event.id
                        ));
                    }
                }
                None => {
                    let seq_owner: Option<String> = tx
                        .query_row(
                            "SELECT id FROM agent_memory_events
                             WHERE project_id = ?1 AND seq = ?2",
                            params![bundle.project.id, event.seq as i64],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(owner) = seq_owner
                        && owner != event.id
                    {
                        return Err(anyhow!(
                            "bundle memory event {} conflicts with local seq {}",
                            event.id,
                            event.seq
                        ));
                    }
                    tx.execute(
                        "INSERT INTO agent_memory_events
                         (id, project_id, seq, kind, payload, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            event.id,
                            event.project_id,
                            event.seq as i64,
                            event.kind,
                            expected_payload,
                            event.created_at
                        ],
                    )?;
                }
            }
        }

        // S6 employment evidence. Derived performance rows are advisory and
        // never overwrite a local row; proposals/hire requests are imported
        // read-only evidence; employment events are portable by id.
        for row in &bundle.agent_performance {
            tx.execute(
                "INSERT OR IGNORE INTO agent_performance
                 (project_id, identity_id, role, status, lookback_days, completed_tasks,
                  failed_tasks, cancelled_tasks, blocked_tasks, skipped_tasks,
                  consecutive_failures, retries, verifier_approvals, verifier_rejections,
                  unsubstantiated_handoffs, false_consensus, diagnosis_findings,
                  assignment_count, session_count, input_tokens, output_tokens,
                  cache_read_tokens, estimated_usage_calls, cost_usd, first_active_at,
                  last_active_at, reason_counts_json, evidence_json, computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)",
                params![
                    row.project_id,
                    row.identity_id,
                    row.role,
                    row.status,
                    row.lookback_days as i64,
                    row.completed_tasks as i64,
                    row.failed_tasks as i64,
                    row.cancelled_tasks as i64,
                    row.blocked_tasks as i64,
                    row.skipped_tasks as i64,
                    row.consecutive_failures as i64,
                    row.retries as i64,
                    row.verifier_approvals as i64,
                    row.verifier_rejections as i64,
                    row.unsubstantiated_handoffs as i64,
                    row.false_consensus as i64,
                    row.diagnosis_findings as i64,
                    row.assignment_count as i64,
                    row.session_count as i64,
                    row.input_tokens as i64,
                    row.output_tokens as i64,
                    row.cache_read_tokens as i64,
                    row.estimated_usage_calls as i64,
                    row.cost_usd,
                    row.first_active_at,
                    row.last_active_at,
                    serde_json::to_string(&row.reason_counts)?,
                    serde_json::to_string(&row.evidence)?,
                    row.computed_at
                ],
            )?;
        }
        for proposal in &bundle.agent_retirement_proposals {
            let mut imported = proposal.clone();
            imported.origin = "imported".to_string();
            match crate::store::employment::get_retirement_proposal_for_import(&tx, &proposal.id)? {
                Some(existing) if existing == imported => {}
                Some(_) => {
                    return Err(anyhow!(
                        "bundle retirement proposal {} conflicts with the local projection",
                        proposal.id
                    ));
                }
                None => {
                    crate::store::employment::insert_retirement_proposal_for_import(
                        &tx, &imported,
                    )?;
                }
            }
        }
        for request in &bundle.agent_hire_requests {
            let mut imported = request.clone();
            imported.origin = "imported".to_string();
            match crate::store::employment::get_hire_request_for_import(&tx, &request.id)? {
                Some(existing) if existing == imported => {}
                Some(_) => {
                    return Err(anyhow!(
                        "bundle hire request {} conflicts with the local projection",
                        request.id
                    ));
                }
                None => {
                    crate::store::employment::insert_hire_request_for_import(&tx, &imported)?;
                }
            }
        }
        for record in &bundle.team_templates {
            let encoded = serde_json::to_string(&record.template)?;
            let existing: Option<ExistingTeamTemplateRow> = tx
                .query_row(
                    "SELECT id, name, version, schema_version, content_hash, json, created_at,
                            source_project_id
                     FROM team_templates WHERE id = ?1",
                    params![record.id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                        ))
                    },
                )
                .optional()?;
            match existing {
                Some((_, name, version, schema_version, content_hash, json, created_at, _))
                    if name == record.name
                        && version == record.version
                        && schema_version == record.schema_version as i64
                        && content_hash == record.content_hash
                        && json == encoded
                        && created_at == record.created_at => {}
                Some(_) => {
                    return Err(anyhow!(
                        "bundle template {} conflicts with the local library",
                        record.id
                    ));
                }
                None => {
                    // An archive may originate from another project. The local
                    // FK must not point at a project that is absent here; the
                    // portable body still carries the human source name.
                    let source_project_id = record
                        .source_project_id
                        .as_deref()
                        .filter(|id| *id == bundle.project.id);
                    tx.execute(
                        "INSERT INTO team_templates
                         (id, name, version, schema_version, content_hash, json, created_at,
                          source_project_id)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            record.id,
                            record.name,
                            record.version,
                            record.schema_version as i64,
                            record.content_hash,
                            encoded,
                            record.created_at,
                            source_project_id
                        ],
                    )?;
                }
            }
        }
        for link in &bundle.project_team_templates {
            // Restored links are archive evidence, never a local apply.
            tx.execute(
                "INSERT OR IGNORE INTO project_team_templates
                 (project_id, template_id, applied_at, applied_by, origin)
                 VALUES (?1, ?2, ?3, ?4, 'imported')",
                params![
                    link.project_id,
                    link.template_id,
                    link.applied_at,
                    link.applied_by
                ],
            )?;
        }
        for event in &bundle.team_template_events {
            let expected_payload = serde_json::to_string(&event.payload)?;
            let existing: Option<(String, String, String, String, String)> = tx
                .query_row(
                    "SELECT kind, template_id, name, version, content_hash
                     FROM team_template_events WHERE id = ?1",
                    params![event.id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()?;
            match existing {
                Some((kind, template_id, name, version, content_hash))
                    if kind == event.kind
                        && template_id == event.template_id
                        && name == event.name
                        && version == event.version
                        && content_hash == event.content_hash => {}
                Some(_) => {
                    return Err(anyhow!(
                        "bundle template event {} conflicts with the local audit stream",
                        event.id
                    ));
                }
                None => {
                    let seq_taken: Option<String> = tx
                        .query_row(
                            "SELECT id FROM team_template_events
                             WHERE project_id = ?1 AND seq = ?2",
                            params![bundle.project.id, event.seq as i64],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if seq_taken.is_some() {
                        return Err(anyhow!(
                            "bundle template event {} conflicts with local seq {}",
                            event.id,
                            event.seq
                        ));
                    }
                    tx.execute(
                        "INSERT INTO team_template_events
                         (id, project_id, seq, kind, template_id, name, version, content_hash,
                          payload, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            event.id,
                            event.project_id,
                            event.seq as i64,
                            event.kind,
                            event.template_id,
                            event.name,
                            event.version,
                            event.content_hash,
                            expected_payload,
                            event.created_at
                        ],
                    )?;
                }
            }
        }
        for event in &bundle.agent_employment_events {
            let expected_payload = serde_json::to_string(&event.payload)?;
            match tx
                .query_row(
                    "SELECT seq, kind, payload, created_at FROM agent_employment_events WHERE id = ?1",
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
                .optional()?
            {
                Some((seq, kind, payload, created_at)) => {
                    if seq != event.seq as i64
                        || kind != event.kind
                        || payload != expected_payload
                        || created_at != event.created_at
                    {
                        return Err(anyhow!(
                            "bundle employment event {} conflicts with the local event stream",
                            event.id
                        ));
                    }
                }
                None => {
                    let seq_owner: Option<String> = tx
                        .query_row(
                            "SELECT id FROM agent_employment_events
                             WHERE project_id = ?1 AND seq = ?2",
                            params![bundle.project.id, event.seq as i64],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(owner) = seq_owner
                        && owner != event.id
                    {
                        return Err(anyhow!(
                            "bundle employment event {} conflicts with local seq {}",
                            event.id,
                            event.seq
                        ));
                    }
                    tx.execute(
                        "INSERT INTO agent_employment_events
                         (id, project_id, seq, identity_id, kind, payload, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            event.id,
                            event.project_id,
                            event.seq as i64,
                            event.identity_id,
                            event.kind,
                            expected_payload,
                            event.created_at
                        ],
                    )?;
                }
            }
        }

        let session_exists: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![bundle.session.id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if session_exists {
            return Err(anyhow!("session already exists: {}", bundle.session.id));
        }
        for snapshot in &bundle.policy_snapshots {
            if snapshot.project_id != bundle.project.id {
                return Err(anyhow!(
                    "bundle policy snapshot {} belongs to another project",
                    snapshot.hash
                ));
            }
            let actual = format!("{:x}", Sha256::digest(snapshot.json.as_bytes()));
            if actual != snapshot.hash {
                return Err(anyhow!(
                    "bundle policy snapshot {} does not match its canonical json",
                    snapshot.hash
                ));
            }
            tx.execute(
                "INSERT INTO policy_snapshots (project_id, hash, json, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id, hash) DO NOTHING",
                params![
                    snapshot.project_id,
                    snapshot.hash,
                    snapshot.json,
                    snapshot.created_at
                ],
            )?;
        }

        // Tool-output bodies are operational and never part of a bundle. Drop
        // any stale handle rows for this session id before registering the
        // imported archive; imported event markers keep their preview text but
        // cannot resolve a source handle.
        tx.execute(
            "DELETE FROM tool_outputs WHERE session_id = ?1",
            params![bundle.session.id],
        )?;
        let plan_json = bundle
            .team_plan
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let plan_runtime_snapshot_json = bundle
            .events
            .iter()
            .rev()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "team_plan.submitted" | "team_plan.revised" | "plan.migrated"
                )
            })
            .find_map(|event| {
                event
                    .payload
                    .get("runtime_snapshot")
                    .or_else(|| event.payload.get("snapshot"))
                    .cloned()
            })
            .and_then(|snapshot| {
                serde_json::from_value::<vp_protocol::runtime::RuntimeSnapshot>(snapshot).ok()
            })
            .and_then(|snapshot| serde_json::to_string(&snapshot).ok());
        tx.execute(
            "INSERT INTO sessions
             (id, project_id, goal, status, priority, worktree, branch, token_budget,
              cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit,
              team_plan, tokens_in, tokens_out, prompt_layout, created_at,
              plan_runtime_snapshot)
             VALUES (?1, ?2, ?3, 'imported', ?4, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                bundle.session.id,
                bundle.session.project_id,
                bundle.session.goal,
                bundle.session.priority,
                bundle.session.branch,
                bundle.session.token_budget,
                bundle.session.cost_budget_usd,
                bundle.session.time_budget_secs,
                if bundle.session.unlimited { 1 } else { 0 },
                bundle.session.cost_spent_usd,
                bundle.session.base_commit,
                plan_json,
                bundle.session.tokens_in as i64,
                bundle.session.tokens_out as i64,
                bundle.session.prompt_layout as i64,
                bundle.session.created_at,
                plan_runtime_snapshot_json
            ],
        )?;
        // Session workspace bindings require the imported session row; insert
        // them after the session itself.
        for binding in &bundle.session_worker_workspaces {
            let mut imported = binding.clone();
            if imported.status == "active" {
                imported.status = "released".to_string();
            }
            imported.lease_active = false;
            imported.lease_expires_at = None;
            match tx
                .query_row(
                    "SELECT status, updated_at FROM session_worker_workspaces WHERE id = ?1",
                    params![binding.id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
            {
                Some((status, updated_at))
                    if status == imported.status && updated_at == imported.updated_at => {}
                Some(_) => {
                    return Err(anyhow!(
                        "bundle session workspace {} conflicts with the local projection",
                        binding.id
                    ));
                }
                None => {
                    crate::store::workspaces::insert_session_row_tx(&tx, &imported)?;
                }
            }
        }
        for blob in &bundle.instruction_blobs {
            let actual = format!("{:x}", Sha256::digest(blob.text.as_bytes()));
            if actual != blob.hash {
                return Err(anyhow!(
                    "bundle instruction blob {} does not match its text",
                    blob.hash
                ));
            }
            tx.execute(
                "INSERT INTO instruction_blobs (hash, text, created_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(hash) DO NOTHING",
                params![blob.hash, blob.text, blob.created_at],
            )?;
        }
        for instruction in &bundle.instructions {
            if instruction.session_id != bundle.session.id
                && !instruction
                    .session_id
                    .starts_with(&format!("{}#", bundle.session.id))
            {
                return Err(anyhow!(
                    "bundle instruction state {} does not belong to session {}",
                    instruction.session_id,
                    bundle.session.id
                ));
            }
            tx.execute(
                "INSERT INTO session_instruction_state
                 (session_id, key, hash, ordinal, tail, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(session_id, key) DO UPDATE SET
                    hash = excluded.hash,
                    ordinal = excluded.ordinal,
                    tail = excluded.tail,
                    updated_at = excluded.updated_at",
                params![
                    instruction.session_id,
                    instruction.key,
                    instruction.hash,
                    instruction.ordinal as i64,
                    if instruction.tail { 1 } else { 0 },
                    instruction.updated_at
                ],
            )?;
            let blob_exists: bool = tx
                .query_row(
                    "SELECT 1 FROM instruction_blobs WHERE hash = ?1",
                    params![instruction.hash],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !blob_exists {
                return Err(anyhow!(
                    "bundle instruction state {} references missing blob {}",
                    instruction.key,
                    instruction.hash
                ));
            }
        }
        replace_prompt_prefix_observations_tx(
            &tx,
            &bundle.project.id,
            &bundle.session.id,
            &bundle.prompt_prefix_observations,
        )?;
        for agent in &bundle.agents {
            tx.execute(
                "INSERT INTO agents
                 (id, session_id, role, name, title, profile, parent_id, worktree, branch, status, model, created_at,
                  task_id, department)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    agent.id,
                    agent.session_id,
                    agent.role,
                    agent.name,
                    agent.title,
                    agent.profile,
                    agent.parent_id,
                    agent.worktree,
                    agent.branch,
                    agent.status,
                    agent.model,
                    agent.created_at,
                    agent.task_id,
                    agent.department
                ],
            )?;
        }
        for task in &bundle.tasks {
            if task.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle task {} belongs to a different session",
                    task.task_id
                ));
            }
            if task.project_id != bundle.project.id {
                return Err(anyhow!(
                    "bundle task {} belongs to a different project",
                    task.task_id
                ));
            }
            tx.execute(
                "INSERT INTO plan_tasks
                 (session_id, plan_seq, task_id, project_id, department, title, priority,
                  status, deps, write_scope, attempts, last_error, next_attempt_at,
                  enqueued_at, started_at, finished_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                         ?14, ?15, ?16, ?17)",
                params![
                    task.session_id,
                    task.plan_seq as i64,
                    task.task_id,
                    task.project_id,
                    task.department,
                    task.title,
                    task.priority,
                    task.status,
                    serde_json::to_string(&task.deps)?,
                    serde_json::to_string(&task.write_scope)?,
                    i64::from(task.attempts),
                    task.last_error,
                    task.next_attempt_at,
                    task.enqueued_at,
                    task.started_at,
                    task.finished_at,
                    bundle.exported_at
                ],
            )?;
        }
        for message in &bundle.messages {
            tx.execute(
                "INSERT INTO messages
                 (id, session_id, role, content, tool_calls, tool_call_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    message.id,
                    message.session_id,
                    message.role,
                    message.content,
                    serde_json::to_string(&message.tool_calls)?,
                    message.tool_call_id,
                    message.created_at
                ],
            )?;
        }
        for steering in &bundle.steering {
            if steering.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle steering {} belongs to a different session",
                    steering.id
                ));
            }
            tx.execute(
                "INSERT INTO steering_messages
                 (id, session_id, agent_id, task_id, body, status, created_at, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    steering.id,
                    steering.session_id,
                    steering.agent_id,
                    steering.task_id,
                    steering.body,
                    steering.status,
                    steering.created_at,
                    steering.delivered_at
                ],
            )?;
        }
        for continuation in &bundle.continuations {
            if continuation.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle continuation {} belongs to a different session",
                    continuation.id
                ));
            }
            if continuation.project_id != bundle.project.id {
                return Err(anyhow!(
                    "bundle continuation {} belongs to a different project",
                    continuation.id
                ));
            }
            insert_session_continuation_tx(&tx, continuation)?;
        }
        for record in &bundle.gates {
            crate::store::gate::validate_gate_record(record)?;
            crate::store::gate::insert_gate_tx(&tx, record)?;
        }
        for recovery in &bundle.recoveries {
            upsert_orchestration_recovery(&tx, recovery)?;
        }
        for (position, item) in bundle.todos.iter().enumerate() {
            tx.execute(
                "INSERT INTO todos (session_id, position, title, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    bundle.session.id,
                    position as i64,
                    item.title,
                    item.status,
                    bundle.exported_at
                ],
            )?;
        }
        for approval in &bundle.approvals {
            insert_approval_tx(&tx, approval)?;
        }
        for memory in &bundle.memories {
            insert_memory_tx(&tx, memory)?;
        }
        for conflict in &bundle.memory_conflicts {
            upsert_memory_conflict_tx(&tx, conflict)?;
        }
        for question in &bundle.questions {
            tx.execute(
                "INSERT INTO questions
                 (id, session_id, agent_id, agent_name, question, options, status, answer,
                  created_at, answered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    question.id,
                    question.session_id,
                    question.agent_id,
                    question.agent_name,
                    question.question,
                    serde_json::to_string(&question.options)?,
                    question.status,
                    question.answer,
                    question.created_at,
                    question.answered_at
                ],
            )?;
        }
        for entry in &bundle.merge_queue {
            tx.execute(
                "INSERT INTO merge_queue
                 (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
                params![
                    entry.id,
                    entry.session_id,
                    entry.agent_id,
                    entry.agent_name,
                    entry.task_id,
                    entry.branch,
                    entry.commit,
                    entry.base_commit,
                    entry.status,
                    entry.attempt as i64,
                    entry.detail,
                    entry.source,
                    entry.remote_host,
                    entry.remote_branch,
                    entry.bundle_sha256,
                    entry.imported_ref,
                    entry.verifier_approved.map(i64::from),
                    entry.verifier_summary,
                    entry.environment_caveat,
                    entry.remote_test_evidence_id,
                    entry.remote_test_digest,
                    entry.remote_test_status,
                    entry.created_at,
                    entry.updated_at
                ],
            )?;
        }
        for effect in &bundle.effects {
            if effect.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle effect receipt {} belongs to a different session",
                    effect.id
                ));
            }
            validate_effect_receipt(effect)?;
            insert_effect_tx(&tx, effect)?;
        }
        for message in &bundle.agent_messages {
            tx.execute(
                "INSERT INTO agent_messages
                 (id, session_id, from_agent_id, from_name, to_agent_id, to_name,
                  kind, body, payload, status, created_at, read_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    message.id,
                    message.session_id,
                    message.from_agent_id,
                    message.from_name,
                    message.to_agent_id,
                    message.to_name,
                    message.kind,
                    message.body,
                    serde_json::to_string(&message.payload)?,
                    message.status,
                    message.created_at,
                    message.read_at
                ],
            )?;
        }
        for artifact in &bundle.artifacts {
            let (content, storage, compression, size_bytes, sha256, path) =
                if let Some(encoded) = &artifact.data_base64 {
                    use base64::Engine as _;
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .context("decoding artifact data")?;
                    use sha2::{Digest, Sha256};
                    let sha256 = format!("{:x}", Sha256::digest(&data));
                    if !artifact.sha256.is_empty() && artifact.sha256 != sha256 {
                        return Err(anyhow!(
                            "artifact {} sha256 does not match its bundle content",
                            artifact.id
                        ));
                    }
                    let dir = self.data_dir.join("artifacts").join("blobs");
                    paths::ensure_private_dir(&dir)?;
                    let file = dir.join(format!("{sha256}.zst"));
                    if !file.is_file() {
                        let compressed = zstd::stream::encode_all(&data[..], 3)?;
                        std::fs::write(&file, compressed)?;
                        paths::ensure_private_file(&file)?;
                    }
                    (
                        String::new(),
                        "file".to_string(),
                        "zstd".to_string(),
                        data.len() as u64,
                        sha256,
                        Some(file.to_string_lossy().to_string()),
                    )
                } else {
                    (
                        artifact.content.clone(),
                        artifact.storage.clone(),
                        "none".to_string(),
                        artifact.size_bytes,
                        artifact.sha256.clone(),
                        None,
                    )
                };
            tx.execute(
                "INSERT INTO artifacts
                 (id, session_id, agent_id, agent_name, task_id, name, kind,
                  mime_type, content, storage, compression, size_bytes, sha256, path,
                  provenance, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    artifact.id,
                    artifact.session_id,
                    artifact.agent_id,
                    artifact.agent_name,
                    artifact.task_id,
                    artifact.name,
                    artifact.kind,
                    artifact.mime_type,
                    content,
                    storage,
                    compression,
                    size_bytes as i64,
                    sha256,
                    path,
                    encode_artifact_provenance(artifact.provenance.as_ref())?,
                    artifact.created_at
                ],
            )?;
        }
        for event in &bundle.events {
            if event.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle event belongs to a different session: {}",
                    event.session_id
                ));
            }
            tx.execute(
                "INSERT INTO events (session_id, seq, type, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    event.session_id,
                    event.seq as i64,
                    event.kind,
                    serde_json::to_string(&event.payload)?,
                    event.created_at
                ],
            )?;
        }
        for entry in &bundle.usage {
            tx.execute(
                "INSERT INTO usage_ledger
                 (id, session_id, agent_id, agent_name, role, task_id, department, model,
                  input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                  estimated, cost_usd, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    entry.id,
                    bundle.session.id,
                    entry.agent_id,
                    entry.agent_name,
                    entry.role,
                    entry.task_id,
                    entry.department,
                    entry.model,
                    entry.input_tokens as i64,
                    entry.output_tokens as i64,
                    entry.cache_read_tokens as i64,
                    entry.cache_write_tokens as i64,
                    if entry.estimated { 1 } else { 0 },
                    entry.cost_usd,
                    entry.created_at
                ],
            )?;
        }
        // Bundle metadata is a projection fallback. Output bodies are never part
        // of an archive, so imported rows keep `output_ref` but cannot resolve a
        // local body. Engine-level import rebuilds from events when they exist.
        tx.execute(
            "DELETE FROM test_results WHERE session_id = ?1",
            params![bundle.session.id],
        )?;
        for entry in &bundle.test_results {
            insert_test_result_tx(&tx, entry, "{}")?;
        }
        for program in &bundle.programs {
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM programs WHERE id = ?1",
                    params![program.id],
                    |row| row.get(0),
                )
                .optional()?;
            if existing.is_some() {
                // Imported archives never overwrite a live program ledger.
                continue;
            }
            let mut archived = program.clone();
            archived.archived = true;
            if matches!(
                archived.status,
                ProgramStatus::Active | ProgramStatus::Draft
            ) {
                archived.status = ProgramStatus::Paused;
            }
            archived.reason = Some("imported archive".to_string());
            archived.updated_at = now_rfc3339();
            tx.execute(
                "INSERT INTO programs
                 (id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason,
                  archived, created_at, updated_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?11)",
                params![
                    archived.id,
                    archived.project_id,
                    archived.goal,
                    archived.status.as_str(),
                    serde_json::to_string(&archived.policy)?,
                    archived.token_budget.map(|value| value as i64),
                    archived.cost_budget_usd,
                    archived.reason,
                    archived.created_at,
                    archived.updated_at,
                    archived.completed_at,
                ],
            )?;
            for milestone in bundle
                .program_milestones
                .iter()
                .filter(|milestone| milestone.program_id == archived.id)
            {
                Self::insert_program_milestone_tx(&tx, milestone)?;
            }
            for event in bundle
                .program_events
                .iter()
                .filter(|event| event.program_id == archived.id)
            {
                tx.execute(
                    "INSERT OR REPLACE INTO program_events
                     (project_id, program_id, seq, kind, payload, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        event.project_id,
                        event.program_id,
                        event.seq as i64,
                        event.kind,
                        serde_json::to_string(&event.payload)?,
                        event.created_at,
                    ],
                )?;
            }
            let archive_payload = serde_json::to_value(&archived)?;
            insert_program_event_tx(
                &tx,
                &archived.project_id,
                &archived.id,
                "program.updated",
                &archive_payload,
            )?;
            for milestone in bundle
                .program_milestones
                .iter()
                .filter(|milestone| milestone.program_id == archived.id)
            {
                let payload = serde_json::to_value(milestone)?;
                insert_program_event_tx(
                    &tx,
                    &archived.project_id,
                    &archived.id,
                    "program.milestone.updated",
                    &payload,
                )?;
            }
            // Archive rows carry the full event history. Rebuild the dispatch
            // accounting ledger so imported programs keep their historical
            // spend even though imported sessions are read-only archives.
            Self::rebuild_program_session_links_tx(&tx, &archived.id)?;
        }
        replace_run_journal_projections_tx(
            &tx,
            &bundle.session.id,
            &bundle.session_runs,
            &bundle.run_steps,
            &bundle.run_checkpoints,
        )?;
        replace_context_checkpoints_tx(&tx, &bundle.session.id, &bundle.context_checkpoints)?;
        replace_supervised_processes_tx(&tx, &bundle.session.id, &bundle.supervised_processes)?;
        let has_context_checkpoint_events = bundle.events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                "context.checkpointed" | "context.checkpoint.deleted"
            )
        });
        if !has_context_checkpoint_events {
            // A pre-D1 producer could carry only the projection. Synthesize
            // the full-payload event so the imported archive remains
            // event-replayable instead of holding an orphaned row.
            for checkpoint in &bundle.context_checkpoints {
                let payload =
                    payload_with_checkpoint_id(serde_json::to_value(checkpoint)?, &checkpoint.id)?;
                insert_event_tx(&tx, &bundle.session.id, "context.checkpointed", payload)?;
            }
        }
        let status_event = insert_event_tx(
            &tx,
            &bundle.session.id,
            "session.updated",
            serde_json::json!({"status": "imported"}),
        )?;
        let imported_event = insert_event_tx(
            &tx,
            &bundle.session.id,
            "session.imported",
            serde_json::json!({
                "exported_at": bundle.exported_at,
                "source_project_id": bundle.project.id,
            }),
        )?;
        tx.commit()?;
        for event in &bundle.events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        for event in [&status_event, &imported_event] {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(imported_event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn imported_task_rows_keep_retry_backoff() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let source = Store::open(data.path()).unwrap();
        let project = source
            .add_project(repo.path().to_str().unwrap(), Some("archive"))
            .unwrap();
        let session = source.create_session(&project.id, "import retry").unwrap();
        let retry_at = "2999-01-01T00:00:00Z";
        let project_json = serde_json::to_value(&project).unwrap();
        let session_json = serde_json::to_value(&session).unwrap();
        let session_id = session.id.clone();
        let project_id = project.id.clone();
        let bundle = serde_json::json!({
            "version": 1,
            "exported_at": now_rfc3339(),
            "project": project_json,
            "session": session_json,
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": [],
            "tasks": [{
                "session_id": session_id,
                "project_id": project_id,
                "plan_seq": 1,
                "task_id": "T1",
                "department": "impl",
                "title": "Scheduled retry",
                "priority": 0,
                "status": "pending",
                "deps": [],
                "write_scope": ["src/retry"],
                "attempts": 2,
                "enqueued_at": now_rfc3339(),
                "started_at": null,
                "finished_at": null,
                "last_error": "retry later",
                "next_attempt_at": retry_at,
            }],
        });
        let bundle: SessionBundle = serde_json::from_value(bundle).unwrap();

        let imported_data = TempDir::new().unwrap();
        let imported = Store::open(imported_data.path()).unwrap();
        imported.import_session_bundle(&bundle).unwrap();
        let tasks = imported
            .list_plan_tasks(Some(&project.id), Some(&session.id))
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "pending");
        assert_eq!(tasks[0].attempts, 2);
        assert_eq!(tasks[0].next_attempt_at.as_deref(), Some(retry_at));
    }
}
