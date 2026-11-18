use super::*;

impl Engine {
    pub(super) async fn load_replay_data(&self, session_id: &str) -> Result<ReplayData> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<ReplayData> {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let messages = store.list_messages(&id)?;
            let plan = store.get_team_plan(&id)?;
            let todos = store.list_todos(&id)?;
            let agents = store.list_agents(&id)?;
            let approvals = store.list_approvals(Some(&id))?;
            let queue = store.list_merge_queue(&id)?;
            let agent_messages = store.list_agent_messages(&id, None, false)?;
            let artifacts = store.list_artifacts(&id)?;
            let usage = store.list_usage_entries(&id)?;
            let memories = store.list_session_memories(&id)?;
            let memory_conflicts = store.list_session_memory_conflicts(&id)?;
            let questions = store.list_session_questions(&id)?;
            let progress = store.list_progress_rows(&id)?;
            let tasks = store.list_plan_tasks(None, Some(&id))?;
            let steering = store.list_steering(&id, None, Some(2_000))?;
            let continuations = store.list_session_continuations(&id, None, Some(2_000))?;
            let gates = store.list_gates(&id, None, 500)?;
            let recoveries = store.list_orchestration_recoveries(None, Some(&id))?;
            let notes = store.list_coordination_notes(&id, 2_000)?;
            let decisions = store.list_orchestration_decisions(&id, 2_000)?;
            let scaling_decisions = store.list_all_scaling_decisions(&id)?;
            let runs = store.list_all_session_runs(&id)?;
            let run_steps = store.list_session_run_steps(&id)?;
            let run_checkpoints = store.list_session_run_checkpoints(&id)?;
            let context_checkpoints = store.list_context_checkpoints(&id, 10_000)?;
            let processes = store.list_supervised_processes(&id, true, 500)?;
            let test_results = store.list_test_results(
                &session.project_id,
                Some(&id),
                None,
                None,
                true,
                10_000,
                &vp_protocol::now_rfc3339(),
            )?;
            let remote_test_evidence = store.list_remote_test_evidence_for_session(&id)?;
            let instructions = store.list_instruction_states_for_session(&id)?;
            let contracts = store.list_contracts_for_session(&id)?;
            let contract_usage = store.list_contract_usage_for_session(&id)?;
            let effects = store.list_effect_receipts(&id, None, true, 500)?;
            let policy_snapshots = store.list_policy_snapshots(&session.project_id, 10_000)?;
            let plan_runtime_snapshot = store.get_plan_runtime_snapshot(&id)?;
            let prompt_prefix_observations =
                store.list_prompt_prefix_observations(&session.project_id, &id)?;
            let prompt_prefix_aggregates =
                store.list_prompt_prefix_aggregates(&session.project_id, 1_000)?;
            let epochs = prompt_prefix_observations
                .iter()
                .map(|row| row.cache_epoch.clone())
                .collect::<Vec<_>>();
            let prompt_prefix_inconsistent_epochs =
                store.prompt_prefix_inconsistent_epochs(&session.project_id, &epochs)?;
            let events = store.list_all_events(&id)?;
            Ok(ReplayData {
                session,
                messages,
                plan,
                todos,
                agents,
                approvals,
                queue,
                agent_messages,
                artifacts,
                usage,
                memories,
                memory_conflicts,
                questions,
                progress,
                tasks,
                steering,
                continuations,
                gates,
                recoveries,
                notes,
                decisions,
                scaling_decisions,
                runs,
                run_steps,
                run_checkpoints,
                context_checkpoints,
                processes,
                test_results,
                remote_test_evidence,
                instructions,
                contracts,
                contract_usage,
                effects,
                policy_snapshots,
                plan_runtime_snapshot,
                prompt_prefix_observations,
                prompt_prefix_aggregates,
                prompt_prefix_inconsistent_epochs,
                events,
            })
        })
        .await?
    }

    /// Reconstruct task-DAG rows from the active plan event and the plan
    /// lifecycle events after it. The active plan is the latest submitted or
    /// revised plan; the task-event window keeps the latest **submitted**
    /// sequence, matching `Store::sync_plan_tasks` (a revision replaces the
    /// plan body but does not move `plan_seq`). Timestamps are derived from
    /// event times; `same_expected_task` deliberately compares only durable
    /// scheduling state so a live claim/finish timestamp cannot create false
    /// drift.
    pub(super) fn expected_tasks(data: &ReplayData) -> Option<Vec<ScheduledTask>> {
        let plan_event = data.events.iter().rev().find(|event| {
            matches!(
                event.kind.as_str(),
                "team_plan.submitted" | "team_plan.revised"
            )
        });
        let submitted_event = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "team_plan.submitted");
        let (plan_seq, plan, enqueued_at) = match plan_event {
            Some(event) => {
                // Revisions wrap the plan in `{plan, revision, ...}`; submitted
                // events carry the plan fields at the top level. Both resolve
                // through the same helper so replay/repair never compare the
                // revision envelope to the stored bare plan.
                let plan = crate::orchestration::plan::plan_from_event(event)?;
                let (plan_seq, enqueued_at) = submitted_event
                    .map(|event| (event.seq, event.created_at.clone()))
                    .unwrap_or_else(|| (0, data.session.created_at.clone()));
                (plan_seq, plan, enqueued_at)
            }
            None => {
                let Some(plan) = data
                    .plan
                    .as_deref()
                    .and_then(|plan| serde_json::from_str::<TeamPlan>(plan).ok())
                else {
                    return data.tasks.is_empty().then(Vec::new);
                };
                (0, plan, data.session.created_at.clone())
            }
        };
        let mut tasks = plan
            .tasks
            .iter()
            .map(|task| {
                (
                    task.id.clone(),
                    ScheduledTask {
                        session_id: data.session.id.clone(),
                        project_id: data.session.project_id.clone(),
                        plan_seq,
                        task_id: task.id.clone(),
                        department: task.department.clone(),
                        title: task.title.clone(),
                        priority: task.priority,
                        status: "pending".to_string(),
                        deps: task.deps.clone(),
                        write_scope: task.write_scope.clone(),
                        attempts: 0,
                        enqueued_at: enqueued_at.clone(),
                        started_at: None,
                        finished_at: None,
                        last_error: None,
                        next_attempt_at: None,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        for event in data.events.iter().filter(|event| event.seq > plan_seq) {
            if !matches!(
                event.kind.as_str(),
                "plan_task.started"
                    | "plan_task.finished"
                    | "plan_task.interrupted"
                    | "plan_task.retried"
                    | "plan_task.skipped"
                    | "plan_task.cancelling"
                    | "plan_task.pausing"
                    | "plan_task.paused"
                    | "plan_task.resumed"
                    | "plan_task.retry_scheduled"
                    | "plan_task.blocked"
                    | "plan_task.unblocked"
                    | "plan_task.cancelled"
            ) {
                continue;
            }
            let Some(task_id) = event.payload["task_id"].as_str() else {
                continue;
            };
            let Some(task) = tasks.get_mut(task_id) else {
                continue;
            };
            match event.kind.as_str() {
                "plan_task.started" => {
                    if task.status != "completed" {
                        task.status = if matches!(
                            data.session.status.as_str(),
                            "queued" | "running" | "paused" | "waiting_input"
                        ) {
                            "running".to_string()
                        } else {
                            "interrupted".to_string()
                        };
                        task.finished_at = None;
                        task.last_error = None;
                    }
                    task.attempts = task.attempts.saturating_add(1);
                    task.started_at = Some(event.created_at.clone());
                    task.next_attempt_at = None;
                }
                "plan_task.finished" => {
                    let status = event.payload["task_status"]
                        .as_str()
                        .unwrap_or("failed")
                        .to_string();
                    task.last_error = if status == "completed" {
                        None
                    } else {
                        event.payload["reason"]
                            .as_str()
                            .or_else(|| event.payload["summary"].as_str())
                            .map(str::to_string)
                    };
                    task.status = status;
                    task.finished_at = Some(event.created_at.clone());
                    task.next_attempt_at = None;
                }
                "plan_task.retry_scheduled" => {
                    task.status = "pending".to_string();
                    task.finished_at = None;
                    task.next_attempt_at = event.payload["next_attempt_at"]
                        .as_str()
                        .map(str::to_string);
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.interrupted" => {
                    task.status = "interrupted".to_string();
                    task.finished_at = Some(event.created_at.clone());
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.retried" => {
                    task.status = "pending".to_string();
                    task.finished_at = None;
                    task.last_error = None;
                    task.next_attempt_at = None;
                }
                "plan_task.skipped" => {
                    task.status = "skipped".to_string();
                    task.finished_at = Some(event.created_at.clone());
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.blocked" => {
                    task.status = "blocked".to_string();
                    task.finished_at = None;
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                    task.next_attempt_at = None;
                }
                "plan_task.unblocked" => {
                    task.status = "pending".to_string();
                    task.finished_at = None;
                    task.last_error = None;
                    task.next_attempt_at = None;
                }
                "plan_task.cancelled" => {
                    task.status = "cancelled".to_string();
                    task.finished_at = Some(event.created_at.clone());
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                    task.next_attempt_at = None;
                }
                "plan_task.cancelling" => {
                    task.status = "cancelling".to_string();
                    task.finished_at = None;
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.pausing" => {
                    task.status = "pausing".to_string();
                    task.finished_at = None;
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.paused" => {
                    task.status = "paused".to_string();
                    task.finished_at = None;
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.resumed" => {
                    task.status = "running".to_string();
                    task.finished_at = None;
                    task.last_error = None;
                }
                _ => {}
            }
        }
        let mut tasks = tasks.into_values().collect::<Vec<_>>();
        tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        Some(tasks)
    }

    pub(super) fn same_expected_task(left: &ScheduledTask, right: &ScheduledTask) -> bool {
        left.session_id == right.session_id
            && left.project_id == right.project_id
            && left.plan_seq == right.plan_seq
            && left.task_id == right.task_id
            && left.department == right.department
            && left.title == right.title
            && left.priority == right.priority
            && left.status == right.status
            && left.deps == right.deps
            && left.write_scope == right.write_scope
            && left.attempts == right.attempts
            && left.enqueued_at == right.enqueued_at
            && left.next_attempt_at == right.next_attempt_at
    }

    /// Reconstruct the merge-queue projection from its own mutation events.
    /// Only `merge_queue.enqueued` / `merge_queue.updated` carry queue rows;
    /// unrelated payloads with an `entry` object (for example
    /// `test_cache.stored`) must never be folded into this projection.
    pub(super) fn expected_merge_queue(data: &ReplayData) -> Option<Vec<MergeQueueEntry>> {
        let mut entries = HashMap::<String, MergeQueueEntry>::new();
        for event in data.events.iter().filter(|event| {
            matches!(
                event.kind.as_str(),
                "merge_queue.enqueued" | "merge_queue.updated"
            )
        }) {
            let entry =
                serde_json::from_value::<MergeQueueEntry>(event.payload.get("entry")?.clone())
                    .ok()?;
            entries.insert(entry.id.clone(), entry);
        }
        let mut entries = entries.into_values().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Some(entries)
    }

    /// Compare the durable merge-queue fields that determine what the
    /// Integrator merges and whether verification was recorded.
    /// `pre_merge_commit` is an operational rollback target and is deliberately
    /// not reconstructed by repair, so it is excluded.
    pub(super) fn same_merge_queue_entry(left: &MergeQueueEntry, right: &MergeQueueEntry) -> bool {
        left.id == right.id
            && left.session_id == right.session_id
            && left.agent_id == right.agent_id
            && left.agent_name == right.agent_name
            && left.task_id == right.task_id
            && left.branch == right.branch
            && left.commit == right.commit
            && left.base_commit == right.base_commit
            && left.status == right.status
            && left.attempt == right.attempt
            && left.detail == right.detail
            && left.source == right.source
            && left.remote_host == right.remote_host
            && left.remote_branch == right.remote_branch
            && left.bundle_sha256 == right.bundle_sha256
            && left.imported_ref == right.imported_ref
            && left.verifier_approved == right.verifier_approved
            && left.verifier_summary == right.verifier_summary
            && left.environment_caveat == right.environment_caveat
            && left.remote_test_evidence_id == right.remote_test_evidence_id
            && left.remote_test_digest == right.remote_test_digest
            && left.remote_test_status == right.remote_test_status
            && left.created_at == right.created_at
            && left.updated_at == right.updated_at
    }

    /// Event-derived contract rows. `contract.derived` payloads carry the
    /// hashed contract; a malformed payload returns `None` so repair fails
    /// closed instead of inventing a row.
    pub(super) fn expected_contract_rows(data: &ReplayData) -> Option<Vec<StoredContractRow>> {
        Self::contract_rows_from_events(&data.events)
    }

    pub(super) fn contract_rows_from_events(events: &[Event]) -> Option<Vec<StoredContractRow>> {
        let contracts = crate::contract::contracts_from_events(events)?;
        Some(
            contracts
                .iter()
                .map(|contract| StoredContractRow {
                    contract_id: contract.contract_id.clone(),
                    project_id: contract.project_id.clone(),
                    session_id: contract.session_id.clone(),
                    task_id: contract.task_id.clone(),
                    plan_revision: contract.plan_revision,
                    role: contract.role.clone(),
                    hash: contract.hash.clone(),
                    source_hash: contract.source_hash.clone(),
                    contract: serde_json::to_string(contract).unwrap_or_default(),
                    created_at: contract.created_at.clone(),
                })
                .collect(),
        )
    }

    /// Event-derived mutation usage rows. The ledger only tracks counts; the
    /// projection timestamp is repair-local and never compared by replay.
    pub(super) fn expected_contract_usage_rows(data: &ReplayData) -> Option<Vec<ContractUsageRow>> {
        Self::contract_usage_rows_from_events(
            &data.events,
            &data.session.project_id,
            &data.session.id,
        )
    }

    pub(super) fn contract_usage_rows_from_events(
        events: &[Event],
        project_id: &str,
        session_id: &str,
    ) -> Option<Vec<ContractUsageRow>> {
        let usage = crate::contract::usage_from_events(events)?;
        Some(
            usage
                .into_iter()
                .map(|(contract_id, usage)| ContractUsageRow {
                    contract_id,
                    project_id: project_id.to_string(),
                    session_id: session_id.to_string(),
                    task_id: None,
                    writes: usage.writes,
                    bytes: usage.bytes,
                    commits: usage.commits,
                    updated_at: vp_protocol::now_rfc3339(),
                })
                .collect(),
        )
    }

    /// A lost C4 runtime pin is projection drift even though `runs_match`
    /// (owned by the run journal) compares the rest of the durable run row.
    pub(super) fn runs_runtime_pins_match(
        projected: &[vp_protocol::types::SessionRun],
        expected: &[vp_protocol::types::SessionRun],
    ) -> bool {
        expected.iter().all(|expected| {
            projected
                .iter()
                .find(|run| run.run_id == expected.run_id)
                .is_some_and(|run| run.runtime_snapshot == expected.runtime_snapshot)
        })
    }

    pub(super) fn expected_approvals(data: &ReplayData) -> Vec<Approval> {
        let mut approvals = HashMap::<String, Approval>::new();
        for event in &data.events {
            match event.kind.as_str() {
                "approval.requested" | "approval.created" => {
                    if let Ok(approval) = serde_json::from_value::<Approval>(event.payload.clone())
                    {
                        approvals.insert(approval.id.clone(), approval);
                    }
                }
                "approval.resolved" => {
                    if let Some(approval_id) = event
                        .payload
                        .get("approval_id")
                        .and_then(serde_json::Value::as_str)
                        && let Some(approval) = approvals.get_mut(approval_id)
                    {
                        approval.status = if event
                            .payload
                            .get("approved")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                        {
                            "approved".to_string()
                        } else {
                            "denied".to_string()
                        };
                        approval.resolved_at = event
                            .payload
                            .get("resolved_at")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or_else(|| Some(event.created_at.clone()));
                    }
                }
                "approval.expired" => {
                    if let Some(approval_id) = event
                        .payload
                        .get("approval_id")
                        .and_then(serde_json::Value::as_str)
                        && let Some(approval) = approvals.get_mut(approval_id)
                    {
                        approval.status = "expired".to_string();
                        approval.resolved_at = event
                            .payload
                            .get("resolved_at")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or_else(|| Some(event.created_at.clone()));
                    }
                }
                "approval.coalesced" => {
                    if let Some(approval_id) = event
                        .payload
                        .get("approval_id")
                        .and_then(serde_json::Value::as_str)
                        && let Some(approval) = approvals.get_mut(approval_id)
                        && let Some(count) = event
                            .payload
                            .get("waiter_count")
                            .and_then(serde_json::Value::as_u64)
                    {
                        approval.waiter_count = count.min(u64::from(u32::MAX)) as u32;
                    }
                }
                _ => {}
            }
        }
        let mut approvals = approvals.into_values().collect::<Vec<_>>();
        approvals.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        approvals
    }

    pub(super) fn expected_artifacts(data: &ReplayData) -> Vec<ExpectedArtifact> {
        let mut artifacts = HashMap::<String, ExpectedArtifact>::new();
        for event in &data.events {
            match event.kind.as_str() {
                "artifact.published" => {
                    let Some(id) = event
                        .payload
                        .get("artifact_id")
                        .and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    let full = event.payload.get("mime_type").is_some()
                        && event.payload.get("sha256").is_some()
                        && event.payload.get("content").is_some();
                    let has_storage = event.payload.get("storage").is_some();
                    let has_size = event.payload.get("size_bytes").is_some();
                    let artifact = Artifact {
                        id: id.to_string(),
                        session_id: data.session.id.clone(),
                        agent_id: event
                            .payload
                            .get("agent_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        agent_name: event
                            .payload
                            .get("agent_name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        task_id: event
                            .payload
                            .get("task_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        name: event
                            .payload
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        kind: event
                            .payload
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        mime_type: event
                            .payload
                            .get("mime_type")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        content: event
                            .payload
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        storage: event
                            .payload
                            .get("storage")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("inline")
                            .to_string(),
                        compression: event
                            .payload
                            .get("compression")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("none")
                            .to_string(),
                        size_bytes: event
                            .payload
                            .get("size_bytes")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        sha256: event
                            .payload
                            .get("sha256")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        path: event
                            .payload
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        data_base64: None,
                        version: event
                            .payload
                            .get("version")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(1)
                            .clamp(1, u32::MAX as u64) as u32,
                        provenance: event
                            .payload
                            .get("provenance")
                            .filter(|value| !value.is_null())
                            .and_then(|value| {
                                serde_json::from_value::<vp_protocol::types::ArtifactProvenance>(
                                    value.clone(),
                                )
                                .ok()
                            }),
                        created_at: event.created_at.clone(),
                    };
                    artifacts.insert(
                        id.to_string(),
                        ExpectedArtifact {
                            artifact,
                            full,
                            has_storage,
                            has_size,
                        },
                    );
                }
                "artifact.pruned" => {
                    if let Some(id) = event
                        .payload
                        .get("artifact_id")
                        .and_then(serde_json::Value::as_str)
                    {
                        artifacts.remove(id);
                    }
                }
                _ => {}
            }
        }
        let mut values = artifacts.into_values().collect::<Vec<_>>();
        values.sort_by(|left, right| {
            left.artifact
                .created_at
                .cmp(&right.artifact.created_at)
                .then_with(|| left.artifact.id.cmp(&right.artifact.id))
        });
        values
    }

    pub(super) fn same_expected_artifact(
        expected: &ExpectedArtifact,
        projected: &Artifact,
    ) -> bool {
        let full = expected.full;
        let has_storage = expected.has_storage;
        let has_size = expected.has_size;
        let expected = &expected.artifact;
        expected.id == projected.id
            && expected.session_id == projected.session_id
            && expected.agent_id == projected.agent_id
            && expected.agent_name == projected.agent_name
            && expected.name == projected.name
            && expected.kind == projected.kind
            && (!has_storage || expected.storage == projected.storage)
            && (!has_size || expected.size_bytes == projected.size_bytes)
            && (!full
                || (expected.task_id == projected.task_id
                    && expected.mime_type == projected.mime_type
                    && expected.content == projected.content
                    && expected.sha256 == projected.sha256
                    && (expected.provenance.is_none()
                        || expected.provenance == projected.provenance)))
    }

    pub(super) fn expected_agent_messages(data: &ReplayData) -> Vec<AgentMessage> {
        let mut mailbox = HashMap::<String, AgentMessage>::new();
        for event in &data.events {
            match event.kind.as_str() {
                "mailbox.message_sent" => {
                    if let Ok(message) =
                        serde_json::from_value::<AgentMessage>(event.payload.clone())
                    {
                        mailbox.insert(message.id.clone(), message);
                    }
                }
                "mailbox.message_read" => {
                    if let Some(message_id) = event
                        .payload
                        .get("message_id")
                        .and_then(serde_json::Value::as_str)
                        && let Some(message) = mailbox.get_mut(message_id)
                    {
                        message.status = "read".to_string();
                        message.read_at = event
                            .payload
                            .get("read_at")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or_else(|| Some(event.created_at.clone()));
                    }
                }
                "mailbox.pruned" => {
                    if let Some(message_id) = event
                        .payload
                        .get("message_id")
                        .and_then(serde_json::Value::as_str)
                    {
                        mailbox.remove(message_id);
                    }
                }
                _ => {}
            }
        }
        let mut messages = mailbox.into_values().collect::<Vec<_>>();
        messages.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        messages
    }

    pub(super) fn expected_usage_entries(data: &ReplayData) -> Vec<UsageEntry> {
        data.events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .map(|event| UsageEntry {
                id: event
                    .payload
                    .get("usage_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("event-{}", event.seq)),
                session_id: data.session.id.clone(),
                agent_id: event
                    .payload
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                agent_name: event
                    .payload
                    .get("agent_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                role: event
                    .payload
                    .get("agent_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                task_id: event
                    .payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                department: event
                    .payload
                    .get("department")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                model: event
                    .payload
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                input_tokens: event
                    .payload
                    .get("input_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                output_tokens: event
                    .payload
                    .get("output_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                cache_read_tokens: event
                    .payload
                    .get("cache_read_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                cache_write_tokens: event
                    .payload
                    .get("cache_write_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                estimated: event
                    .payload
                    .get("estimated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                cost_usd: event
                    .payload
                    .get("cost_usd")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0),
                created_at: event.created_at.clone(),
            })
            .collect()
    }

    pub(super) fn same_usage(left: &UsageEntry, right: &UsageEntry) -> bool {
        left.id == right.id
            && left.agent_id == right.agent_id
            && left.agent_name == right.agent_name
            && left.role == right.role
            && left.task_id == right.task_id
            && left.department == right.department
            && left.model == right.model
            && left.input_tokens == right.input_tokens
            && left.output_tokens == right.output_tokens
            && left.cache_read_tokens == right.cache_read_tokens
            && left.cache_write_tokens == right.cache_write_tokens
            && left.estimated == right.estimated
            && left.cost_usd.to_bits() == right.cost_usd.to_bits()
    }

    /// Rebuild supplemental evidence rows from `remote_test.completed`
    /// events. A malformed event returns `None` rather than silently
    /// dropping evidence.
    pub(super) fn expected_remote_test_evidence(
        data: &ReplayData,
    ) -> Option<Vec<RemoteTestEvidence>> {
        Self::remote_test_evidence_from_events(&data.events)
    }

    pub(super) fn remote_test_evidence_from_events(
        events: &[Event],
    ) -> Option<Vec<RemoteTestEvidence>> {
        let mut rows = Vec::new();
        for event in events
            .iter()
            .filter(|event| event.kind == "remote_test.completed")
        {
            match serde_json::from_value::<RemoteTestEvidence>(
                event.payload.get("evidence")?.clone(),
            ) {
                Ok(row) => rows.push(row),
                Err(_) => return None,
            }
        }
        rows.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Some(rows)
    }

    pub(super) fn expected_progress(data: &ReplayData) -> Option<Vec<(u64, ProgressRound)>> {
        Self::progress_from_events(&data.events)
    }

    pub(super) fn progress_from_events(events: &[Event]) -> Option<Vec<(u64, ProgressRound)>> {
        let mut items: Vec<(u64, ProgressRound)> = Vec::new();
        for event in events.iter().filter(|event| event.kind == "progress.round") {
            let plan_revision = event
                .payload
                .get("plan_revision")
                .and_then(serde_json::Value::as_u64)?;
            let round = event
                .payload
                .get("round")
                .cloned()
                .and_then(|value| serde_json::from_value::<ProgressRound>(value).ok())?;
            if let Some(existing) = items.iter_mut().find(|(revision, item)| {
                *revision == plan_revision && item.round_id == round.round_id
            }) {
                existing.1 = round;
            } else {
                items.push((plan_revision, round));
            }
        }
        // Preserve event order. Sorting by `round_id` would turn r10/r11
        // lexicographically ahead of r2 and corrupt the seq-ordered window
        // used by `session.progress` after a repair.
        Some(items)
    }
}
