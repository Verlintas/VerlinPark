use super::*;

impl Store {
    pub fn repair_session_projections(
        &self,
        session_id: &str,
        repair: &SessionRepair,
    ) -> Result<()> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET status = ?1, priority = COALESCE(?2, priority),
                 tokens_in = ?3, tokens_out = ?4, cost_spent_usd = ?5,
                 unlimited = COALESCE(?7, unlimited),
                 prompt_layout = COALESCE(?8, prompt_layout)
             WHERE id = ?6",
            params![
                repair.status,
                repair.priority,
                repair.tokens_in as i64,
                repair.tokens_out as i64,
                repair.cost_spent_usd,
                session_id,
                repair.unlimited.map(|value| if value { 1 } else { 0 }),
                repair.prompt_layout.map(|value| value as i64)
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        if repair.replace_plan {
            tx.execute(
                "UPDATE sessions SET team_plan = ?1 WHERE id = ?2",
                params![repair.plan, session_id],
            )?;
            // Runtime snapshots are event-backed too. Reconstruct the newest
            // plan snapshot from its lifecycle payload; a legacy event has no
            // snapshot and leaves the column untouched.
            let latest_plan_payload: Option<String> = tx
                .query_row(
                    "SELECT payload FROM events
                     WHERE session_id = ?1
                       AND type IN ('team_plan.submitted', 'team_plan.revised', 'plan.migrated')
                     ORDER BY seq DESC LIMIT 1",
                    params![session_id],
                    |row| row.get(0),
                )
                .optional()?;
            // `plan.migrated` carries the updated snapshot under `snapshot`;
            // plan lifecycle events use `runtime_snapshot`. Ignoring the
            // migration event let a routine repair reintroduce the old
            // (incompatible) snapshot.
            let snapshot_json = latest_plan_payload
                .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
                .and_then(|payload| {
                    payload
                        .get("runtime_snapshot")
                        .or_else(|| payload.get("snapshot"))
                        .cloned()
                })
                .and_then(|snapshot| {
                    serde_json::from_value::<vp_protocol::runtime::RuntimeSnapshot>(snapshot).ok()
                })
                .and_then(|snapshot| serde_json::to_string(&snapshot).ok());
            if let Some(snapshot_json) = snapshot_json {
                tx.execute(
                    "UPDATE sessions SET plan_runtime_snapshot = ?1 WHERE id = ?2",
                    params![snapshot_json, session_id],
                )?;
            }
        }
        if let Some(todos) = &repair.todos {
            tx.execute(
                "DELETE FROM todos WHERE session_id = ?1",
                params![session_id],
            )?;
            for (position, item) in todos.iter().enumerate() {
                tx.execute(
                    "INSERT INTO todos (session_id, position, title, status, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![session_id, position as i64, item.title, item.status, now],
                )?;
            }
        }
        if let Some(messages) = &repair.messages {
            tx.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                params![session_id],
            )?;
            for message in messages {
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
        }
        if let Some(agent_messages) = &repair.agent_messages {
            tx.execute(
                "DELETE FROM agent_messages WHERE session_id = ?1",
                params![session_id],
            )?;
            for message in agent_messages {
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
        }
        if let Some(agents) = &repair.agents {
            tx.execute(
                "DELETE FROM agents WHERE session_id = ?1",
                params![session_id],
            )?;
            for agent in agents {
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
        }
        if let Some(queue) = &repair.queue {
            tx.execute(
                "DELETE FROM merge_queue WHERE session_id = ?1",
                params![session_id],
            )?;
            for entry in queue {
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
        }
        if let Some(evidence) = &repair.remote_test_evidence {
            tx.execute(
                "DELETE FROM remote_test_evidence WHERE session_id = ?1",
                params![session_id],
            )?;
            for row in evidence {
                tx.execute(
                    "INSERT INTO remote_test_evidence
                     (id, project_id, session_id, entry_id, profile, kind, host,
                      commit_hash, command, cwd, status, exit_code, duration_ms,
                      output_ref, output_sha256, output_bytes, output_truncated,
                      timed_out, detail, digest, environment_caveat, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                             ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                    params![
                        row.id,
                        row.project_id,
                        row.session_id,
                        row.entry_id,
                        row.profile,
                        row.kind,
                        row.host,
                        row.commit,
                        row.command,
                        row.cwd,
                        row.status,
                        row.exit_code,
                        row.duration_ms as i64,
                        row.output_ref,
                        row.output_sha256,
                        row.output_bytes as i64,
                        if row.output_truncated { 1 } else { 0 },
                        if row.timed_out { 1 } else { 0 },
                        row.detail,
                        row.digest,
                        row.environment_caveat,
                        row.created_at,
                    ],
                )?;
            }
        }
        if let Some(memories) = &repair.memories {
            replace_session_memories_tx(&tx, session_id, memories)?;
        }
        if let Some(conflicts) = &repair.memory_conflicts {
            replace_session_memory_conflicts_tx(&tx, session_id, conflicts)?;
        }
        if let Some(questions) = &repair.questions {
            tx.execute(
                "DELETE FROM questions WHERE session_id = ?1",
                params![session_id],
            )?;
            for question in questions {
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
        }
        if let Some(tasks) = &repair.tasks {
            tx.execute(
                "DELETE FROM plan_tasks WHERE session_id = ?1",
                params![session_id],
            )?;
            for task in tasks {
                tx.execute(
                    "INSERT INTO plan_tasks
                     (session_id, plan_seq, task_id, project_id, department, title, priority,
                      status, deps, write_scope, attempts, last_error, enqueued_at, started_at,
                      finished_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                        task.enqueued_at,
                        task.started_at,
                        task.finished_at,
                        now
                    ],
                )?;
            }
        }
        if let Some(steering) = &repair.steering {
            tx.execute(
                "DELETE FROM steering_messages WHERE session_id = ?1",
                params![session_id],
            )?;
            for message in steering {
                tx.execute(
                    "INSERT INTO steering_messages
                     (id, session_id, agent_id, task_id, body, status, created_at, delivered_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        message.id,
                        message.session_id,
                        message.agent_id,
                        message.task_id,
                        message.body,
                        message.status,
                        message.created_at,
                        message.delivered_at
                    ],
                )?;
            }
        }
        if let Some(continuations) = &repair.continuations {
            tx.execute(
                "DELETE FROM session_continuations WHERE session_id = ?1",
                params![session_id],
            )?;
            for continuation in continuations {
                insert_session_continuation_tx(&tx, continuation)?;
            }
        }
        if let Some(gates) = &repair.gates {
            tx.execute(
                "DELETE FROM external_gates WHERE session_id = ?1",
                params![session_id],
            )?;
            for record in gates {
                if record.gate.session_id != session_id {
                    return Err(anyhow!(
                        "replayed external gate {} does not belong to session {}",
                        record.gate.id,
                        session_id
                    ));
                }
                crate::store::gate::validate_gate_record(record)?;
                crate::store::gate::insert_gate_tx(&tx, record)?;
            }
        }
        if let Some(recoveries) = &repair.recoveries {
            tx.execute(
                "DELETE FROM orchestration_recoveries WHERE session_id = ?1",
                params![session_id],
            )?;
            for recovery in recoveries {
                upsert_orchestration_recovery(&tx, recovery)?;
            }
        }
        if let Some(artifacts) = &repair.artifacts {
            tx.execute(
                "DELETE FROM artifacts WHERE session_id = ?1",
                params![session_id],
            )?;
            for artifact in artifacts {
                tx.execute(
                    "INSERT INTO artifacts
                     (id, session_id, agent_id, agent_name, task_id, name, kind, mime_type,
                      content, storage, compression, size_bytes, sha256, path, provenance,
                      created_at)
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
                        artifact.content,
                        artifact.storage,
                        artifact.compression,
                        artifact.size_bytes as i64,
                        artifact.sha256,
                        artifact.path,
                        encode_artifact_provenance(artifact.provenance.as_ref())?,
                        artifact.created_at
                    ],
                )?;
            }
        }
        if let Some(approvals) = &repair.approvals {
            tx.execute(
                "DELETE FROM approvals WHERE session_id = ?1",
                params![session_id],
            )?;
            for approval in approvals {
                insert_approval_tx(&tx, approval)?;
            }
        }
        if let Some(runs) = &repair.runs {
            replace_run_journal_projections_tx(
                &tx,
                session_id,
                runs,
                repair.run_steps.as_deref().unwrap_or(&[]),
                repair.run_checkpoints.as_deref().unwrap_or(&[]),
            )?;
        }
        if let Some(context_checkpoints) = &repair.context_checkpoints {
            replace_context_checkpoints_tx(&tx, session_id, context_checkpoints)?;
        }
        if let Some(processes) = &repair.supervised_processes {
            replace_supervised_processes_tx(&tx, session_id, processes)?;
        }
        if let Some(test_results) = &repair.test_results {
            tx.execute(
                "DELETE FROM test_results WHERE session_id = ?1",
                params![session_id],
            )?;
            for entry in test_results {
                insert_test_result_tx(&tx, entry, "{}")?;
            }
        }
        if let Some(observations) = &repair.prompt_prefix_observations {
            let project_id: String = tx.query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )?;
            replace_prompt_prefix_observations_tx(&tx, &project_id, session_id, observations)?;
        }
        if let Some(instructions) = &repair.instructions {
            tx.execute(
                "DELETE FROM session_instruction_state
                 WHERE session_id = ?1 OR session_id LIKE ?2",
                params![session_id, format!("{session_id}#%")],
            )?;
            for row in instructions {
                if row.session_id != session_id
                    && !row.session_id.starts_with(&format!("{session_id}#"))
                {
                    return Err(anyhow!(
                        "replayed instruction state {} does not belong to session {}",
                        row.session_id,
                        session_id
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
                        row.session_id,
                        row.key,
                        row.hash,
                        row.ordinal as i64,
                        if row.tail { 1 } else { 0 },
                        row.updated_at
                    ],
                )?;
            }
        }
        if let Some(contracts) = &repair.contracts {
            tx.execute(
                "DELETE FROM contracts WHERE session_id = ?1",
                params![session_id],
            )?;
            for row in contracts {
                if row.session_id != session_id {
                    return Err(anyhow!(
                        "replayed contract {} does not belong to session {}",
                        row.contract_id,
                        session_id
                    ));
                }
                tx.execute(
                    "INSERT INTO contracts
                     (contract_id, project_id, session_id, task_id, plan_revision, role,
                      hash, source_hash, contract, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        row.contract_id,
                        row.project_id,
                        row.session_id,
                        row.task_id,
                        row.plan_revision as i64,
                        row.role,
                        row.hash,
                        row.source_hash,
                        row.contract,
                        row.created_at
                    ],
                )?;
            }
        }
        if let Some(usage) = &repair.contract_usage {
            tx.execute(
                "DELETE FROM contract_usage WHERE session_id = ?1",
                params![session_id],
            )?;
            for row in usage {
                if row.session_id != session_id {
                    return Err(anyhow!(
                        "replayed contract usage {} does not belong to session {}",
                        row.contract_id,
                        session_id
                    ));
                }
                tx.execute(
                    "INSERT INTO contract_usage
                     (contract_id, project_id, session_id, task_id, writes, bytes,
                      commits, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        row.contract_id,
                        row.project_id,
                        row.session_id,
                        row.task_id,
                        row.writes as i64,
                        row.bytes as i64,
                        row.commits as i64,
                        row.updated_at
                    ],
                )?;
            }
        }
        if let Some(effects) = &repair.effects {
            tx.execute(
                "DELETE FROM effect_receipts WHERE session_id = ?1",
                params![session_id],
            )?;
            for row in effects {
                if row.session_id != session_id {
                    return Err(anyhow!(
                        "replayed effect receipt {} does not belong to session {}",
                        row.id,
                        session_id
                    ));
                }
                validate_effect_receipt(row)?;
                insert_effect_tx(&tx, row)?;
            }
        }
        if let Some(progress) = &repair.progress {
            tx.execute(
                "DELETE FROM progress_rounds WHERE session_id = ?1",
                params![session_id],
            )?;
            for (plan_revision, round) in progress {
                tx.execute(
                    "INSERT INTO progress_rounds
                     (session_id, plan_revision, round_id, agent_id, agent_name, task_id,
                      satisfied_json, in_loop_json, progressing_json, blockers_json,
                      next_task, instruction, evidence_json, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                    params![
                        session_id,
                        *plan_revision as i64,
                        round.round_id,
                        round.agent_id,
                        round.agent_name,
                        round.task_id,
                        serde_json::to_string(&round.satisfied)?,
                        serde_json::to_string(&round.in_loop)?,
                        serde_json::to_string(&round.progressing)?,
                        serde_json::to_string(&round.blockers)?,
                        round.next_task,
                        round.instruction,
                        serde_json::to_string(&round.evidence)?,
                        round.created_at.clone().unwrap_or_else(now_rfc3339),
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
