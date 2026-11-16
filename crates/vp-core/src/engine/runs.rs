use super::*;

struct ReplayData {
    session: Session,
    messages: Vec<StoredMessage>,
    plan: Option<String>,
    todos: Vec<TodoItem>,
    agents: Vec<AgentRecord>,
    approvals: Vec<Approval>,
    queue: Vec<MergeQueueEntry>,
    agent_messages: Vec<AgentMessage>,
    artifacts: Vec<Artifact>,
    usage: Vec<UsageEntry>,
    memories: Vec<MemoryItem>,
    memory_conflicts: Vec<MemoryConflict>,
    questions: Vec<Question>,
    progress: Vec<(u64, ProgressRound)>,
    tasks: Vec<ScheduledTask>,
    steering: Vec<SteeringMessage>,
    continuations: Vec<SessionContinuation>,
    gates: Vec<GateRecord>,
    recoveries: Vec<OrchestrationRecovery>,
    notes: Vec<OrchestrationNote>,
    decisions: Vec<OrchestrationDecision>,
    scaling_decisions: Vec<vp_protocol::scaling::ScalingDecision>,
    runs: Vec<vp_protocol::types::SessionRun>,
    run_steps: Vec<vp_protocol::types::RunStep>,
    run_checkpoints: Vec<vp_protocol::types::RunCheckpoint>,
    context_checkpoints: Vec<vp_protocol::types::ContextCheckpoint>,
    processes: Vec<vp_protocol::types::SupervisedProcess>,
    test_results: Vec<vp_protocol::test_cache::TestCacheEntry>,
    remote_test_evidence: Vec<RemoteTestEvidence>,
    instructions: Vec<vp_protocol::types::SessionInstructionState>,
    contracts: Vec<StoredContractRow>,
    contract_usage: Vec<ContractUsageRow>,
    effects: Vec<EffectReceipt>,
    policy_snapshots: Vec<vp_protocol::runtime::PolicySnapshot>,
    plan_runtime_snapshot: Option<String>,
    prompt_prefix_observations: Vec<PromptPrefixObservation>,
    prompt_prefix_aggregates: Vec<PromptPrefixAggregate>,
    prompt_prefix_inconsistent_epochs: Vec<String>,
    events: Vec<Event>,
}

struct ExpectedArtifact {
    artifact: Artifact,
    full: bool,
    has_storage: bool,
    has_size: bool,
}

impl Engine {
    pub async fn set_session_run_policy(
        &self,
        project_id: &str,
        policy: vp_protocol::types::SessionRunPolicy,
    ) -> Result<vp_protocol::types::SessionRunPolicyResult> {
        let policy = crate::run_journal::set_policy(self, project_id, &policy).await?;
        Ok(vp_protocol::types::SessionRunPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn session_run_policy(
        &self,
        project_id: &str,
    ) -> Result<vp_protocol::types::SessionRunPolicyResult> {
        let policy = crate::run_journal::policy(self, project_id).await?;
        Ok(vp_protocol::types::SessionRunPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn session_run_detail(
        &self,
        run_id: &str,
    ) -> Result<vp_protocol::types::SessionRunDetailResult> {
        let (run, steps, tool_calls, checkpoints) =
            crate::run_journal::run_detail(self, run_id).await?;
        Ok(vp_protocol::types::SessionRunDetailResult {
            run,
            steps,
            tool_calls,
            checkpoints,
        })
    }

    pub async fn session_runs(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<vp_protocol::types::SessionRunsResult> {
        let runs = crate::run_journal::session_runs(self, session_id, limit).await?;
        Ok(vp_protocol::types::SessionRunsResult {
            session_id: session_id.to_string(),
            runs,
        })
    }

    pub async fn recent_events(&self, session_id: &str, limit: u32) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.recent_events(&id, limit)).await?
    }

    pub async fn recent_events_of_kind(
        &self,
        session_id: &str,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || store.recent_events_by_kind(&id, &kind, limit)).await?
    }

    /// Reconstruct the top-level conversation from the durable event stream,
    /// not from a live in-memory vector. Malformed message events fail closed.
    pub async fn conversation_from_events(
        &self,
        session_id: &str,
    ) -> Result<(Vec<vp_providers::ChatMessage>, Vec<Option<String>>)> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let events = tokio::task::spawn_blocking(move || store.list_all_events(&id)).await??;
        crate::agent::conversation_from_events(&events, session_id)
    }

    pub async fn recover_interrupted_sessions(&self) -> Result<usize> {
        let store = self.store.clone();
        let (sessions, events) =
            tokio::task::spawn_blocking(move || store.recover_state()).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        // Durable run journal recovery runs after session/agent status changes:
        // every run left `running` by the previous process becomes
        // `interrupted`, and its tool intents become unknown.
        let interrupted_runs = crate::run_journal::interrupt_running_runs(self).await?;
        if interrupted_runs > 0 {
            tracing::warn!(
                runs = interrupted_runs,
                "marked durable runs interrupted after daemon recovery"
            );
        }
        // An operation receipt that is still `pending` belongs to the previous
        // daemon process. Its side effect may or may not have landed, so it is
        // failed closed as `unknown` instead of being replayed.
        let operation_store = self.store.clone();
        let unknown_operations = tokio::task::spawn_blocking(move || {
            operation_store.mark_pending_session_operations_unknown()
        })
        .await??;
        if unknown_operations > 0 {
            tracing::warn!(
                operations = unknown_operations,
                "marked pending session operations unknown after daemon recovery"
            );
        }
        // A crash may land after `git merge` but before the queue status is
        // finalized. If the session worktree is clean, reset it to the
        // recorded pre-merge HEAD so retry starts from a known state.
        let recovery_store = self.store.clone();
        let recoveries =
            tokio::task::spawn_blocking(move || recovery_store.interrupted_merge_recoveries())
                .await??;
        let git_lock = self.git_lock.clone();
        for entry in recoveries {
            let Some(pre_merge) = entry.pre_merge_commit.clone() else {
                continue;
            };
            let session_store = self.store.clone();
            let session_id = entry.session_id.clone();
            let session =
                tokio::task::spawn_blocking(move || session_store.get_session(&session_id))
                    .await??;
            let Some(session) = session else {
                continue;
            };
            let Some(worktree_path) = session.worktree.clone() else {
                continue;
            };
            let path = PathBuf::from(worktree_path);
            let reset = {
                let git_lock = git_lock.clone();
                let path = path.clone();
                let pre_merge = pre_merge.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let _guard = git_lock
                        .lock()
                        .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                    if !worktree::is_clean(&path)? {
                        anyhow::bail!("session worktree is not clean");
                    }
                    worktree::reset_hard(&path, &pre_merge)
                })
                .await?
            };
            let (kind, payload) = match reset {
                Ok(()) => (
                    "merge_queue.recovered",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "pre_merge_commit": pre_merge,
                        "detail": "reset clean session worktree to the pre-merge HEAD",
                    }),
                ),
                Err(error) => (
                    "merge_queue.recovery_skipped",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "pre_merge_commit": pre_merge,
                        "detail": format!("{error:#}"),
                    }),
                ),
            };
            if let Err(error) = self.append_event(&session.id, kind, payload).await {
                tracing::warn!(%error, entry_id = %entry.id, "failed to record merge recovery");
            }
        }
        Ok(sessions.len())
    }

    async fn load_replay_data(&self, session_id: &str) -> Result<ReplayData> {
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
    /// lifecycle events after it. Timestamps are derived from event times;
    /// `same_expected_task` deliberately compares only durable scheduling state
    /// so a live claim/finish timestamp cannot create false drift.
    fn expected_tasks(data: &ReplayData) -> Option<Vec<ScheduledTask>> {
        let plan_event = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "team_plan.submitted");
        let (plan_seq, plan, enqueued_at) = match plan_event {
            Some(event) => {
                let plan = serde_json::from_value::<TeamPlan>(event.payload.clone()).ok()?;
                (event.seq, plan, event.created_at.clone())
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

    fn same_expected_task(left: &ScheduledTask, right: &ScheduledTask) -> bool {
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
    }

    fn expected_approvals(data: &ReplayData) -> Vec<Approval> {
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

    fn expected_artifacts(data: &ReplayData) -> Vec<ExpectedArtifact> {
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

    fn same_expected_artifact(expected: &ExpectedArtifact, projected: &Artifact) -> bool {
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

    fn expected_agent_messages(data: &ReplayData) -> Vec<AgentMessage> {
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

    fn expected_usage_entries(data: &ReplayData) -> Vec<UsageEntry> {
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

    fn same_usage(left: &UsageEntry, right: &UsageEntry) -> bool {
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
    fn expected_remote_test_evidence(data: &ReplayData) -> Option<Vec<RemoteTestEvidence>> {
        let mut rows = Vec::new();
        for event in data
            .events
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

    fn replay_report(data: &ReplayData, repaired: bool) -> Result<SessionReplayReport> {
        let mut mismatches = Vec::new();
        let replay_status = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.updated")
            .and_then(|event| event.payload.get("status"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                data.events
                    .iter()
                    .any(|event| event.kind == "session.created")
                    .then_some("draft")
            });
        let status_matches = replay_status == Some(data.session.status.as_str());
        if !status_matches {
            mismatches.push(format!(
                "session status: projection={}, replay={:?}",
                data.session.status, replay_status
            ));
        }
        let replay_unlimited = replay_session_unlimited(&data.events);
        if replay_unlimited != data.session.unlimited {
            mismatches.push(format!(
                "session budget mode: projection={}, replay={}",
                data.session.unlimited, replay_unlimited
            ));
        }
        let prompt_layout_matches = replay_session_prompt_layout(&data.events)
            .map(|replay_layout| replay_layout == data.session.prompt_layout)
            .unwrap_or(true);
        if !prompt_layout_matches {
            mismatches.push(format!(
                "session prompt layout: projection={}, replay={:?}",
                data.session.prompt_layout,
                replay_session_prompt_layout(&data.events)
            ));
        }

        let (replay_in, replay_out) = data
            .events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .fold((0u64, 0u64), |(input, output), event| {
                (
                    input.saturating_add(event.payload["input_tokens"].as_u64().unwrap_or(0)),
                    output.saturating_add(event.payload["output_tokens"].as_u64().unwrap_or(0)),
                )
            });
        let replay_cost = replay_session_cost(&data.events);
        let usage_matches = replay_in == data.session.tokens_in
            && replay_out == data.session.tokens_out
            && (replay_cost - data.session.cost_spent_usd).abs() < 1e-9;
        if !usage_matches {
            mismatches.push(format!(
                "usage: projection={}/{}/{:.6}, replay={replay_in}/{replay_out}/{replay_cost:.6}",
                data.session.tokens_in, data.session.tokens_out, data.session.cost_spent_usd
            ));
        }

        let expected_usage = Self::expected_usage_entries(data);
        let has_usage_id = data
            .events
            .iter()
            .any(|event| event.kind == "agent.usage" && event.payload.get("usage_id").is_some());
        let projected_usage = data
            .usage
            .iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect::<HashMap<_, _>>();
        let ledger_matches = if data.usage.is_empty() && !has_usage_id {
            // Legacy sessions predate the ledger; their events have no usage_id
            // and no projection to compare against.
            true
        } else {
            expected_usage.len() == data.usage.len()
                && expected_usage.iter().all(|expected| {
                    projected_usage
                        .get(&expected.id)
                        .is_some_and(|projected| Self::same_usage(projected, expected))
                })
        };
        if !ledger_matches {
            mismatches.push(format!(
                "usage ledger: projection={}, replay={}",
                data.usage.len(),
                expected_usage.len()
            ));
        }

        let expected_mailbox = Self::expected_agent_messages(data);
        let expected_mailbox_values = expected_mailbox
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_mailbox_values = data
            .agent_messages
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let mailbox_matches = expected_mailbox_values == projected_mailbox_values;
        if !mailbox_matches {
            mismatches.push(format!(
                "mailbox: projection={}, replay={}",
                projected_mailbox_values.len(),
                expected_mailbox_values.len()
            ));
        }

        let expected_notes = crate::orchestration::coordination_notes(&data.events);
        let expected_note_values = expected_notes
            .iter()
            .filter_map(|note| Some((note.note_id.clone(), serde_json::to_value(note).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_note_values = data
            .notes
            .iter()
            .filter_map(|note| Some((note.note_id.clone(), serde_json::to_value(note).ok()?)))
            .collect::<HashMap<_, _>>();
        let notes_match = expected_note_values == projected_note_values;
        if !notes_match {
            mismatches.push(format!(
                "coordination notes: projection={}, replay={}",
                projected_note_values.len(),
                expected_note_values.len()
            ));
        }
        let expected_decisions = crate::orchestration::decisions_from_events(&data.events);
        let expected_decision_values = expected_decisions
            .iter()
            .filter_map(|decision| {
                Some((
                    decision.decision_id.clone(),
                    serde_json::to_value(decision).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let projected_decision_values = data
            .decisions
            .iter()
            .filter_map(|decision| {
                Some((
                    decision.decision_id.clone(),
                    serde_json::to_value(decision).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let decisions_match = expected_decision_values == projected_decision_values;
        if !decisions_match {
            mismatches.push(format!(
                "orchestration decisions: projection={}, replay={}",
                projected_decision_values.len(),
                expected_decision_values.len()
            ));
        }
        let expected_scaling = crate::scaling::decisions_from_events(&data.events);
        let scaling_decisions_match =
            crate::scaling::decisions_match(&data.scaling_decisions, &expected_scaling);
        if !scaling_decisions_match {
            mismatches.push(format!(
                "scaling decisions: projection={}, replay={}",
                data.scaling_decisions.len(),
                expected_scaling.len()
            ));
        }
        let expected_recoveries = crate::orchestration::recoveries_from_events(&data.events);
        let expected_recovery_values = expected_recoveries
            .iter()
            .filter_map(|recovery| {
                Some((recovery_key(recovery), serde_json::to_value(recovery).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_recovery_values = data
            .recoveries
            .iter()
            .filter_map(|recovery| {
                Some((recovery_key(recovery), serde_json::to_value(recovery).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let recoveries_match = expected_recovery_values == projected_recovery_values;
        if !recoveries_match {
            mismatches.push(format!(
                "orchestration recoveries: projection={}, replay={}",
                projected_recovery_values.len(),
                expected_recovery_values.len()
            ));
        }
        let expected_steering = replay_steering(&data.events);
        let expected_steering_values = expected_steering
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_steering_values = data
            .steering
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let steering_matches = expected_steering_values == projected_steering_values;
        if !steering_matches {
            mismatches.push(format!(
                "steering: projection={}, replay={}",
                projected_steering_values.len(),
                expected_steering_values.len()
            ));
        }
        let expected_continuations = replay_continuations(&data.events);
        let expected_continuation_values = expected_continuations
            .iter()
            .filter_map(|continuation| {
                Some((
                    continuation.id.clone(),
                    serde_json::to_value(continuation).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let projected_continuation_values = data
            .continuations
            .iter()
            .filter_map(|continuation| {
                Some((
                    continuation.id.clone(),
                    serde_json::to_value(continuation).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let continuations_match = expected_continuation_values == projected_continuation_values;
        if !continuations_match {
            mismatches.push(format!(
                "continuations: projection={}, replay={}",
                projected_continuation_values.len(),
                expected_continuation_values.len()
            ));
        }
        let expected_gates = crate::gate::gates_from_events(&data.events);
        let has_gate_events = data.events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                crate::gate::OPENED_EVENT
                    | crate::gate::RESOLVED_EVENT
                    | crate::gate::EXPIRED_EVENT
                    | crate::gate::CANCELLED_EVENT
            )
        });
        let gates_match = if !has_gate_events {
            // Legacy/imported archives may carry a gate projection without a
            // gate event stream. Preserve it rather than reporting drift.
            true
        } else {
            match expected_gates {
                Some(expected) => {
                    let matches = data.gates.len() == expected.len()
                        && data.gates.iter().all(|projected| {
                            expected.iter().any(|candidate| {
                                candidate.gate.id == projected.gate.id
                                    && candidate.gate.state == projected.gate.state
                                    && candidate.gate.outcome == projected.gate.outcome
                                    && candidate.gate.resolution_note
                                        == projected.gate.resolution_note
                                    && candidate.gate.resolved_at == projected.gate.resolved_at
                                    && candidate.gate.resolved_by == projected.gate.resolved_by
                                    && candidate.token_hash == projected.token_hash
                            })
                        });
                    if !matches {
                        mismatches.push(format!(
                            "external gates: projection={}, replay={}",
                            data.gates.len(),
                            expected.len()
                        ));
                    }
                    matches
                }
                None => {
                    mismatches.push("external gates: malformed replay event".to_string());
                    false
                }
            }
        };

        let expected_journal = crate::run_journal::projections_from_events(&data.events);
        let (runs_match, run_steps_match, run_checkpoints_match) = match &expected_journal {
            Some((runs, steps, checkpoints)) => (
                crate::run_journal::runs_match(&data.runs, runs),
                crate::run_journal::steps_match(&data.run_steps, steps),
                crate::run_journal::checkpoints_match(&data.run_checkpoints, checkpoints),
            ),
            None => (false, false, false),
        };
        if !runs_match {
            mismatches.push(format!(
                "session runs: projection={}, replay={}",
                data.runs.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.0.len())
                    .unwrap_or(0)
            ));
        }
        if !run_steps_match {
            mismatches.push(format!(
                "run steps: projection={}, replay={}",
                data.run_steps.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.1.len())
                    .unwrap_or(0)
            ));
        }
        if !run_checkpoints_match {
            mismatches.push(format!(
                "run checkpoints: projection={}, replay={}",
                data.run_checkpoints.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.2.len())
                    .unwrap_or(0)
            ));
        }

        let expected_checkpoints = crate::context::checkpoints_from_events(&data.events);
        let context_checkpoints_match = expected_checkpoints.as_ref().is_some_and(|expected| {
            crate::context::context_checkpoints_match(&data.context_checkpoints, expected)
        });
        if !context_checkpoints_match {
            mismatches.push(format!(
                "context checkpoints: projection={}, replay={}",
                data.context_checkpoints.len(),
                expected_checkpoints.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let expected_processes = crate::process_supervisor::processes_from_events(&data.events);
        let processes_match = expected_processes.as_ref().is_some_and(|expected| {
            crate::process_supervisor::processes_match(&data.processes, expected)
        });
        if !processes_match {
            mismatches.push(format!(
                "supervised processes: projection={}, replay={}",
                data.processes.len(),
                expected_processes.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let expected_test_results = crate::test_cache::results_from_events(&data.events);
        if !crate::test_cache::results_match(&data.test_results, &expected_test_results) {
            mismatches.push(format!(
                "test cache: projection={}, replay={}",
                data.test_results.len(),
                expected_test_results.len()
            ));
        }

        let has_instruction_events = data
            .events
            .iter()
            .any(|event| event.kind == "context.instruction");
        let instructions_match = match crate::instruction::states_from_events(&data.events) {
            Some(expected_instructions) => {
                let matches = crate::instruction::instruction_rows_match(
                    &data.instructions,
                    &expected_instructions,
                );
                if !matches {
                    mismatches.push(format!(
                        "instruction state: projection={}, replay={}",
                        data.instructions.len(),
                        expected_instructions.len()
                    ));
                }
                matches
            }
            None if has_instruction_events => {
                mismatches.push("instruction state: malformed replay event".to_string());
                false
            }
            None => {
                // Legacy/imported archives may carry instruction rows without
                // an instruction event stream. There is no replay authority in
                // that case, so the projection is preserved rather than
                // reported as drift.
                true
            }
        };

        let has_contract_events = data
            .events
            .iter()
            .any(|event| event.kind == "contract.derived");
        let expected_contracts = crate::contract::contracts_from_events(&data.events);
        let contracts_projection = data
            .contracts
            .iter()
            .map(|row| {
                serde_json::from_str::<crate::contract::TaskContract>(&row.contract).map_err(|_| ())
            })
            .collect::<std::result::Result<Vec<_>, ()>>();
        let contracts_match = if !has_contract_events {
            // Legacy/imported archives may carry rows without a contract event
            // stream. The projection is preserved rather than treated as drift.
            true
        } else {
            match (&expected_contracts, contracts_projection) {
                (Some(expected), Ok(projected)) => {
                    let rows_match = crate::contract::contracts_match(&projected, expected);
                    if !rows_match {
                        mismatches.push(format!(
                            "task contracts: projection={}, replay={}",
                            data.contracts.len(),
                            expected.len()
                        ));
                    }
                    let expected_usage =
                        crate::contract::usage_from_events(&data.events).unwrap_or_default();
                    let mut usage_delta = false;
                    if expected_usage.len() != data.contract_usage.len() {
                        usage_delta = true;
                    }
                    for row in &data.contract_usage {
                        let expected = expected_usage
                            .get(&row.contract_id)
                            .copied()
                            .unwrap_or_default();
                        if expected.writes != row.writes
                            || expected.bytes != row.bytes
                            || expected.commits != row.commits
                        {
                            usage_delta = true;
                        }
                    }
                    if usage_delta {
                        mismatches.push(format!(
                            "contract usage: projection={}, replay={}",
                            data.contract_usage.len(),
                            expected_usage.len()
                        ));
                    }
                    rows_match && !usage_delta
                }
                (None, _) if has_contract_events => {
                    mismatches.push("task contracts: malformed replay event".to_string());
                    false
                }
                (None, _) => {
                    // Legacy/imported archives may carry no contract event stream.
                    // Preserve the projection rather than reporting drift.
                    true
                }
                (Some(_), Err(())) => {
                    mismatches.push("task contracts: malformed projection row".to_string());
                    false
                }
            }
        };

        let expected_prefix_observations = crate::cache_epoch::observations_from_events(
            &data.session.project_id,
            &data.session.id,
            &data.events,
        );
        let (prefix_observations_match, prefix_aggregates_match) = if let Some(expected) =
            expected_prefix_observations
        {
            let observations_match =
                crate::cache_epoch::observations_match(&data.prompt_prefix_observations, &expected);
            if !observations_match {
                mismatches.push(format!(
                    "prompt prefix observations: projection={}, replay={}",
                    data.prompt_prefix_observations.len(),
                    expected.len()
                ));
            }
            let mut aggregates_match = data.prompt_prefix_inconsistent_epochs.is_empty();
            if !aggregates_match {
                mismatches.push(format!(
                    "prompt prefix aggregates: {} epoch(s) diverge from observations",
                    data.prompt_prefix_inconsistent_epochs.len()
                ));
            }
            let missing_aggregate = expected.iter().any(|row| {
                !data
                    .prompt_prefix_aggregates
                    .iter()
                    .any(|aggregate| aggregate.cache_epoch == row.cache_epoch)
            });
            if missing_aggregate {
                aggregates_match = false;
                mismatches.push(format!(
                    "prompt prefix aggregates: missing an epoch row for {} session epoch(s)",
                    expected.len()
                ));
            }
            (observations_match, aggregates_match)
        } else {
            // No epoch event stream means a legacy session; its projection
            // is preserved rather than reported as drift.
            (true, true)
        };

        let expected_artifacts = Self::expected_artifacts(data);
        let projected_artifacts = data
            .artifacts
            .iter()
            .map(|artifact| (artifact.id.clone(), artifact))
            .collect::<HashMap<_, _>>();
        let artifacts_matches = expected_artifacts.len() == data.artifacts.len()
            && expected_artifacts.iter().all(|expected| {
                projected_artifacts
                    .get(&expected.artifact.id)
                    .is_some_and(|projected| Self::same_expected_artifact(expected, projected))
            });
        if !artifacts_matches {
            mismatches.push(format!(
                "artifacts: projection={}, replay={}",
                data.artifacts.len(),
                expected_artifacts.len()
            ));
        }

        let replay_questions = data
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "question.requested"
                        | "question.answered"
                        | "question.cancelled"
                        | "question.expired"
                )
            })
            .filter_map(|event| {
                let question = serde_json::from_value::<Question>(event.payload.clone()).ok()?;
                Some((question.id.clone(), serde_json::to_value(question).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_questions = data
            .questions
            .iter()
            .filter_map(|question| {
                Some((question.id.clone(), serde_json::to_value(question).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let questions_matches = replay_questions == projected_questions;
        if !questions_matches {
            mismatches.push(format!(
                "questions: projection={}, replay={}",
                projected_questions.len(),
                replay_questions.len()
            ));
        }

        let replay_progress = data
            .events
            .iter()
            .filter(|event| event.kind == "progress.round")
            .filter_map(|event| {
                let plan_revision = event.payload.get("plan_revision")?.as_u64()?;
                let round =
                    serde_json::from_value::<ProgressRound>(event.payload.get("round")?.clone())
                        .ok()?;
                Some((
                    (plan_revision, round.round_id.clone()),
                    serde_json::to_value(round).ok()?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let projected_progress = data
            .progress
            .iter()
            .filter_map(|(plan_revision, round)| {
                Some((
                    (*plan_revision, round.round_id.clone()),
                    serde_json::to_value(round).ok()?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let progress_matches = replay_progress == projected_progress;
        if !progress_matches {
            mismatches.push(format!(
                "progress: projection={}, replay={}",
                projected_progress.len(),
                replay_progress.len()
            ));
        }

        let expected_tasks = Self::expected_tasks(data);
        let task_queue_matches = match &expected_tasks {
            Some(expected) => {
                expected.len() == data.tasks.len()
                    && expected.iter().all(|expected| {
                        data.tasks
                            .iter()
                            .find(|task| task.task_id == expected.task_id)
                            .is_some_and(|projected| Self::same_expected_task(expected, projected))
                    })
            }
            None => false,
        };
        if !task_queue_matches {
            mismatches.push(format!(
                "task queue: projection={}, replay={}",
                data.tasks.len(),
                expected_tasks.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let mut replay_memories = HashMap::new();
        for event in &data.events {
            match event.kind.as_str() {
                "memory.created" | "memory.updated" | "memory.verified" | "memory.stale"
                | "memory.superseded" | "memory.used" => {
                    if let Ok(memory) = serde_json::from_value::<MemoryItem>(event.payload.clone())
                    {
                        replay_memories.insert(memory.id.clone(), serde_json::to_value(memory)?);
                    }
                }
                "memory.deleted" => {
                    if let Ok(memory) = serde_json::from_value::<MemoryItem>(event.payload.clone())
                    {
                        replay_memories.remove(&memory.id);
                    }
                }
                _ => {}
            }
        }
        let projected_memories = data
            .memories
            .iter()
            .filter_map(|memory| Some((memory.id.clone(), serde_json::to_value(memory).ok()?)))
            .collect::<HashMap<_, _>>();
        let memories_matches = replay_memories == projected_memories;
        if !memories_matches {
            mismatches.push(format!(
                "memories: projection={}, replay={}",
                projected_memories.len(),
                replay_memories.len()
            ));
        }

        let replay_conflicts = replay_memory_conflicts(&data.events)
            .into_iter()
            .filter_map(|conflict| {
                Some((conflict.id.clone(), serde_json::to_value(conflict).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_conflicts = data
            .memory_conflicts
            .iter()
            .filter_map(|conflict| {
                Some((conflict.id.clone(), serde_json::to_value(conflict).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let memory_conflicts_match = replay_conflicts == projected_conflicts;
        if !memory_conflicts_match {
            mismatches.push(format!(
                "memory conflicts: projection={}, replay={}",
                projected_conflicts.len(),
                replay_conflicts.len()
            ));
        }

        let expected_approvals = Self::expected_approvals(data);
        let expected_approval_values = expected_approvals
            .iter()
            .filter_map(|approval| {
                Some((approval.id.clone(), serde_json::to_value(approval).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_approval_values = data
            .approvals
            .iter()
            .filter_map(|approval| {
                Some((approval.id.clone(), serde_json::to_value(approval).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let approvals_matches = expected_approval_values == projected_approval_values;
        if !approvals_matches {
            mismatches.push(format!(
                "approvals: projection={}, replay={}",
                projected_approval_values.len(),
                expected_approval_values.len()
            ));
        }

        let replay_plan = data
            .events
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.kind.as_str(),
                    "team_plan.submitted" | "team_plan.revised"
                )
            })
            .map(|event| {
                let mut payload = event.payload.clone();
                if let Some(object) = payload.as_object_mut() {
                    object.remove("runtime_snapshot");
                }
                payload
            });
        let projected_plan = data
            .plan
            .as_deref()
            .and_then(|plan| serde_json::from_str::<serde_json::Value>(plan).ok());
        let plan_matches = replay_plan == projected_plan;
        if !plan_matches {
            mismatches.push("team plan projection differs from event stream".to_string());
        }

        let replay_todos = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "todo.updated")
            .map(|event| event.payload["items"].clone())
            .unwrap_or_else(|| serde_json::json!([]));
        let projected_todos = serde_json::to_value(&data.todos)?;
        let todos_matches = replay_todos == projected_todos;
        if !todos_matches {
            mismatches.push("todo projection differs from event stream".to_string());
        }

        let replay_messages = data
            .events
            .iter()
            .filter(|event| event.kind == "message.created")
            .filter_map(|event| {
                let message =
                    serde_json::from_value::<StoredMessage>(event.payload.clone()).ok()?;
                Some((message.id.clone(), serde_json::to_value(message).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_messages = data
            .messages
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let messages_matches = replay_messages == projected_messages;
        if !messages_matches {
            mismatches.push(format!(
                "messages: projection={}, replay={}",
                projected_messages.len(),
                replay_messages.len()
            ));
        }

        let replay_agents = data
            .events
            .iter()
            .filter_map(|event| {
                let agent = if event.kind == "agent.snapshot" {
                    serde_json::from_value::<AgentRecord>(event.payload.clone()).ok()?
                } else if event.kind == "agent.updated" {
                    serde_json::from_value::<AgentRecord>(event.payload.get("agent")?.clone())
                        .ok()?
                } else {
                    return None;
                };
                Some((agent.id.clone(), serde_json::to_value(agent).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_agents = data
            .agents
            .iter()
            .filter_map(|agent| Some((agent.id.clone(), serde_json::to_value(agent).ok()?)))
            .collect::<HashMap<_, _>>();
        let agents_matches = replay_agents == projected_agents;
        if !agents_matches {
            mismatches.push(format!(
                "agents: projection={}, replay={}",
                projected_agents.len(),
                replay_agents.len()
            ));
        }

        let replay_queue = data
            .events
            .iter()
            .filter_map(|event| {
                let entry = event.payload.get("entry")?;
                Some((
                    entry.get("id")?.as_str()?.to_string(),
                    entry.get("status")?.as_str()?.to_string(),
                ))
            })
            .fold(HashMap::new(), |mut map, (id, status)| {
                map.insert(id, status);
                map
            });
        let projected_queue = data
            .queue
            .iter()
            .map(|entry| (entry.id.clone(), entry.status.clone()))
            .collect::<HashMap<_, _>>();
        let queue_matches = replay_queue == projected_queue;
        if !queue_matches {
            mismatches.push("merge queue projection differs from event stream".to_string());
        }

        let replay_evidence = Self::expected_remote_test_evidence(data).unwrap_or_default();
        let mut projected_evidence = data.remote_test_evidence.clone();
        projected_evidence.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let remote_test_evidence_matches = replay_evidence == projected_evidence;
        if !remote_test_evidence_matches {
            mismatches
                .push("remote test evidence projection differs from event stream".to_string());
        }

        let has_effect_events = data.events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                crate::effect::EFFECT_INTENT_EVENT
                    | crate::effect::EFFECT_SETTLED_EVENT
                    | crate::effect::EFFECT_FAILED_EVENT
                    | crate::effect::EFFECT_UNKNOWN_EVENT
                    | crate::effect::EFFECT_RECONCILED_EVENT
            )
        });
        let effects_match = if !has_effect_events {
            // Legacy/imported archives may carry rows without effect events.
            true
        } else {
            match crate::effect::receipts_from_events(&data.events) {
                Some(expected) => {
                    let rows_match = crate::effect::receipts_match(&data.effects, &expected);
                    if !rows_match {
                        mismatches.push(format!(
                            "effect receipts: projection={}, replay={}",
                            data.effects.len(),
                            expected.len()
                        ));
                    }
                    rows_match
                }
                None => {
                    mismatches.push("effect receipts: malformed replay event".to_string());
                    false
                }
            }
        };

        let expected_policy = crate::runtime_pin::policy_snapshots_from_events(&data.events);
        let mut referenced_policy_hashes = expected_policy
            .iter()
            .map(|row| row.hash.clone())
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(raw) = &data.plan_runtime_snapshot
            && let Some(snapshot) = crate::runtime_pin::parse_snapshot(raw)
            && !snapshot.policy_hash.is_empty()
        {
            referenced_policy_hashes.insert(snapshot.policy_hash);
        }
        for run in &data.runs {
            if let Some(snapshot) = &run.runtime_snapshot
                && !snapshot.policy_hash.is_empty()
            {
                referenced_policy_hashes.insert(snapshot.policy_hash.clone());
            }
        }
        let projected_policy = data
            .policy_snapshots
            .iter()
            .map(|row| (row.hash.clone(), row.json.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        let policy_snapshots_match = expected_policy.iter().all(|row| {
            projected_policy
                .get(&row.hash)
                .is_some_and(|json| json == &row.json)
        }) && referenced_policy_hashes
            .iter()
            .all(|hash| projected_policy.contains_key(hash));
        if !policy_snapshots_match {
            mismatches.push(format!(
                "policy snapshots: projection={}, replay={}, referenced={}",
                data.policy_snapshots.len(),
                expected_policy.len(),
                referenced_policy_hashes.len()
            ));
        }

        let consistent = status_matches
            && prompt_layout_matches
            && instructions_match
            && prefix_observations_match
            && prefix_aggregates_match
            && usage_matches
            && ledger_matches
            && mailbox_matches
            && artifacts_matches
            && approvals_matches
            && memories_matches
            && memory_conflicts_match
            && questions_matches
            && task_queue_matches
            && plan_matches
            && todos_matches
            && messages_matches
            && agents_matches
            && queue_matches
            && notes_match
            && decisions_match
            && scaling_decisions_match
            && recoveries_match
            && steering_matches
            && continuations_match
            && gates_match
            && runs_match
            && run_steps_match
            && run_checkpoints_match
            && context_checkpoints_match
            && processes_match
            && remote_test_evidence_matches
            && contracts_match
            && effects_match
            && policy_snapshots_match;
        Ok(SessionReplayReport {
            session_id: data.session.id.clone(),
            event_count: data.events.len(),
            status_matches,
            usage_matches,
            plan_matches,
            todos_matches,
            messages_matches,
            agents_matches,
            queue_matches,
            mailbox_matches,
            artifacts_matches,
            approvals_matches,
            ledger_matches,
            memories_matches,
            memory_conflicts_match,
            questions_matches,
            task_queue_matches,
            notes_match,
            decisions_match,
            scaling_decisions_match,
            recoveries_match,
            steering_matches,
            continuations_match,
            gates_match,
            runs_match,
            run_steps_match,
            run_checkpoints_match,
            context_checkpoints_match,
            processes_match,
            remote_test_evidence_matches,
            contracts_match,
            effects_match,
            policy_snapshots_match,
            consistent,
            repaired,
            rebuilt: false,
            mismatches,
        })
    }

    fn expected_progress(data: &ReplayData) -> Option<Vec<(u64, ProgressRound)>> {
        let mut items: Vec<(u64, ProgressRound)> = Vec::new();
        for event in data
            .events
            .iter()
            .filter(|event| event.kind == "progress.round")
        {
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

    fn build_session_repair(data: &ReplayData) -> Result<SessionRepair> {
        let status = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.updated")
            .and_then(|event| event.payload.get("status"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                data.events
                    .iter()
                    .any(|event| event.kind == "session.created")
                    .then_some("draft")
            })
            .unwrap_or(data.session.status.as_str())
            .to_string();
        let (tokens_in, tokens_out) = data
            .events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .fold((0u64, 0u64), |(input, output), event| {
                (
                    input.saturating_add(event.payload["input_tokens"].as_u64().unwrap_or(0)),
                    output.saturating_add(event.payload["output_tokens"].as_u64().unwrap_or(0)),
                )
            });

        let plan_event = data.events.iter().rev().find(|event| {
            matches!(
                event.kind.as_str(),
                "team_plan.submitted" | "team_plan.revised"
            )
        });
        let replace_plan = plan_event.is_some() || data.plan.is_none();
        let plan = plan_event
            .map(|event| {
                let mut payload = event.payload.clone();
                if let Some(object) = payload.as_object_mut() {
                    object.remove("runtime_snapshot");
                }
                serde_json::to_string(&payload).map_err(anyhow::Error::from)
            })
            .transpose()?
            .or_else(|| data.plan.clone());

        let todos = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "todo.updated")
            .map(|event| serde_json::from_value::<Vec<TodoItem>>(event.payload["items"].clone()))
            .transpose()?
            .or_else(|| data.todos.is_empty().then(Vec::new));

        let messages = {
            let mut event_messages = Vec::new();
            let mut valid = true;
            for event in data
                .events
                .iter()
                .filter(|event| event.kind == "message.created")
            {
                match serde_json::from_value::<StoredMessage>(event.payload.clone()) {
                    Ok(message) => event_messages.push(message),
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            let projected_ids = data
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            let event_ids = event_messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            (valid && projected_ids.is_subset(&event_ids)).then_some(event_messages)
        };

        let agents = {
            let mut event_agents = HashMap::new();
            let mut valid = true;
            for event in data
                .events
                .iter()
                .filter(|event| event.kind == "agent.snapshot" || event.kind == "agent.updated")
            {
                let agent = if event.kind == "agent.snapshot" {
                    serde_json::from_value::<AgentRecord>(event.payload.clone())
                } else {
                    serde_json::from_value::<AgentRecord>(event.payload["agent"].clone())
                };
                match agent {
                    Ok(agent) => {
                        event_agents.insert(agent.id.clone(), agent);
                    }
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            let projected_ids = data
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            let event_ids = event_agents
                .keys()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            if valid && projected_ids.is_subset(&event_ids) {
                let mut agents = event_agents.into_values().collect::<Vec<_>>();
                agents.sort_by(|left, right| {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Some(agents)
            } else {
                None
            }
        };

        let queue = {
            let mut entries = HashMap::new();
            let mut valid = true;
            for event in data.events.iter().filter(|event| {
                event.kind == "merge_queue.enqueued" || event.kind == "merge_queue.updated"
            }) {
                match serde_json::from_value::<MergeQueueEntry>(event.payload["entry"].clone()) {
                    Ok(entry) => {
                        entries.insert(entry.id.clone(), entry);
                    }
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid && (!entries.is_empty() || data.queue.is_empty()) {
                let mut entries = entries.into_values().collect::<Vec<_>>();
                entries.sort_by(|left, right| {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Some(entries)
            } else {
                None
            }
        };

        let questions = {
            let mut latest = HashMap::new();
            let mut valid = true;
            for event in data.events.iter().filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "question.requested"
                        | "question.answered"
                        | "question.cancelled"
                        | "question.expired"
                )
            }) {
                match serde_json::from_value::<Question>(event.payload.clone()) {
                    Ok(question) => {
                        latest.insert(question.id.clone(), question);
                    }
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid {
                let mut items = latest.into_values().collect::<Vec<_>>();
                items.sort_by(|left, right| left.created_at.cmp(&right.created_at));
                Some(items)
            } else {
                None
            }
        };

        let memories = {
            let mut items: Vec<MemoryItem> = Vec::new();
            let mut valid = true;
            for event in &data.events {
                match event.kind.as_str() {
                    "memory.created" | "memory.updated" | "memory.verified" | "memory.stale"
                    | "memory.superseded" | "memory.used" => {
                        match serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                            Ok(memory) => {
                                if let Some(existing) =
                                    items.iter_mut().find(|item| item.id == memory.id)
                                {
                                    *existing = memory;
                                } else {
                                    items.push(memory);
                                }
                            }
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    "memory.deleted" => {
                        match serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                            Ok(memory) => items.retain(|item| item.id != memory.id),
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            valid.then_some(items)
        };

        let expected_artifacts = Self::expected_artifacts(data);
        let all_full = expected_artifacts.iter().all(|expected| expected.full);
        let mut artifact_warnings = Vec::new();
        let artifacts = if all_full {
            Some(
                expected_artifacts
                    .iter()
                    .map(|expected| {
                        let artifact = expected.artifact.clone();
                        if artifact.storage == "file"
                            && artifact
                                .path
                                .as_deref()
                                .is_some_and(|path| !Path::new(path).is_file())
                        {
                            artifact_warnings.push(format!(
                                "artifact {} blob is missing at {}",
                                artifact.id,
                                artifact.path.as_deref().unwrap_or("(no path)")
                            ));
                        }
                        artifact
                    })
                    .collect(),
            )
        } else {
            artifact_warnings.extend(
                expected_artifacts
                    .iter()
                    .filter(|expected| !expected.full)
                    .map(|expected| {
                        format!(
                            "artifact {} predates full publish metadata and was not rebuilt",
                            expected.artifact.id
                        )
                    }),
            );
            None
        };
        let cost_spent_usd = replay_session_cost(&data.events);
        let priority = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.priority")
            .and_then(|event| event.payload.get("priority"))
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        let journal = crate::run_journal::projections_from_events(&data.events);
        let context_checkpoints = crate::context::checkpoints_from_events(&data.events);
        let processes = crate::process_supervisor::processes_from_events(&data.events);
        Ok(SessionRepair {
            status,
            priority,
            unlimited: Some(replay_session_unlimited(&data.events)),
            prompt_layout: replay_session_prompt_layout(&data.events),
            tokens_in,
            tokens_out,
            cost_spent_usd,
            replace_plan,
            plan,
            todos,
            messages,
            agent_messages: Some(Self::expected_agent_messages(data)),
            agents,
            queue,
            artifacts,
            approvals: Some(Self::expected_approvals(data)),
            memories,
            memory_conflicts: Some(replay_memory_conflicts(&data.events)),
            questions,
            progress: Self::expected_progress(data),
            tasks: Self::expected_tasks(data),
            steering: Some(replay_steering(&data.events)),
            continuations: Some(replay_continuations(&data.events)),
            gates: if !data.events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::gate::OPENED_EVENT
                        | crate::gate::RESOLVED_EVENT
                        | crate::gate::EXPIRED_EVENT
                        | crate::gate::CANCELLED_EVENT
                )
            }) {
                // Legacy/imported archives may carry gate rows without an
                // event stream. Their projection is preserved rather than
                // treated as drift.
                if data.gates.is_empty() {
                    Some(Vec::new())
                } else {
                    None
                }
            } else {
                match crate::gate::gates_from_events(&data.events) {
                    Some(rows) => Some(rows),
                    None if data.gates.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            recoveries: Some(crate::orchestration::recoveries_from_events(&data.events)),
            runs: journal.as_ref().map(|value| value.0.clone()),
            run_steps: journal.as_ref().map(|value| value.1.clone()),
            run_checkpoints: journal.as_ref().map(|value| value.2.clone()),
            context_checkpoints,
            supervised_processes: processes,
            test_results: Some(crate::test_cache::results_from_events(&data.events)),
            remote_test_evidence: {
                let expected = Self::expected_remote_test_evidence(data);
                expected.filter(|rows| !rows.is_empty() || data.remote_test_evidence.is_empty())
            },
            instructions: match crate::instruction::states_from_events(&data.events) {
                Some(rows) => Some(rows),
                None if data.instructions.is_empty() => Some(Vec::new()),
                None => None,
            },
            contracts: if !data
                .events
                .iter()
                .any(|event| event.kind == "contract.derived")
            {
                None
            } else {
                match crate::contract::contracts_from_events(&data.events) {
                    Some(contracts) => Some(
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
                            .collect::<Vec<_>>(),
                    ),
                    None if data.contracts.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            contract_usage: if !data
                .events
                .iter()
                .any(|event| event.kind == "intent.mutation")
            {
                None
            } else {
                match crate::contract::usage_from_events(&data.events) {
                    Some(usage) => Some(
                        usage
                            .into_iter()
                            .map(|(contract_id, usage)| ContractUsageRow {
                                contract_id,
                                project_id: data.session.project_id.clone(),
                                session_id: data.session.id.clone(),
                                task_id: None,
                                writes: usage.writes,
                                bytes: usage.bytes,
                                commits: usage.commits,
                                updated_at: vp_protocol::now_rfc3339(),
                            })
                            .collect::<Vec<_>>(),
                    ),
                    None if data.contract_usage.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            effects: if !data.events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::effect::EFFECT_INTENT_EVENT
                        | crate::effect::EFFECT_SETTLED_EVENT
                        | crate::effect::EFFECT_FAILED_EVENT
                        | crate::effect::EFFECT_UNKNOWN_EVENT
                        | crate::effect::EFFECT_RECONCILED_EVENT
                )
            }) {
                None
            } else {
                match crate::effect::receipts_from_events(&data.events) {
                    Some(rows) => Some(rows),
                    None if data.effects.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            prompt_prefix_observations: match crate::cache_epoch::observations_from_events(
                &data.session.project_id,
                &data.session.id,
                &data.events,
            ) {
                Some(rows) => Some(rows),
                None if data.prompt_prefix_observations.is_empty() => Some(Vec::new()),
                None => None,
            },
            artifact_warnings,
        })
    }

    pub async fn rehydrate_session(
        &self,
        session_id: &str,
        worktree: Option<String>,
    ) -> Result<SessionRehydrateResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status != "imported" {
            anyhow::bail!("only imported archive sessions can be rehydrated");
        }
        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let project_root = PathBuf::from(&project.root);
        if !project_root.is_dir() {
            anyhow::bail!("project root does not exist: {}", project.root);
        }
        let branch = session
            .branch
            .clone()
            .unwrap_or_else(|| format!("verlinpark/{session_id}"));
        let base_commit = session.base_commit.clone().unwrap_or_default();
        let target = match worktree {
            Some(path) => PathBuf::from(path),
            None => self
                .store
                .data_dir()
                .join("worktrees")
                .join(&project.id)
                .join(&session.id)
                .join("leader"),
        };
        let canonical_root = project_root.canonicalize()?;
        let target_abs = if target.is_absolute() {
            target
        } else {
            std::env::current_dir()?.join(target)
        };
        let target_for_check = target_abs.clone();
        if target_abs.starts_with(&canonical_root) {
            anyhow::bail!("worktree path must be outside the project root");
        }
        let git_lock = self.git_lock.clone();
        let branch_for_worktree = branch.clone();
        let base_for_worktree = base_commit.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::rehydrate(
                &project_root,
                &target_for_check,
                &branch_for_worktree,
                &base_for_worktree,
            )
        })
        .await??;
        let worktree_path = target_abs.clone();
        let commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&worktree_path)).await??;
        let store = self.store.clone();
        let id = session_id.to_string();
        let worktree_string = target_abs.to_string_lossy().to_string();
        let branch_for_store = branch.clone();
        let events = tokio::task::spawn_blocking(move || {
            store.rehydrate_session_with_events(&id, &worktree_string, &branch_for_store)
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }

        let agents = self.list_agents(&session.id).await?;
        let data_dir = self.store.data_dir().to_path_buf();
        let project_root_for_agents = PathBuf::from(&project.root);
        let canonical_root_for_agents = project_root_for_agents.canonicalize()?;
        let session_worktree = target_abs.clone();
        let session_branch = branch.clone();
        let session_id_for_agents = session.id.clone();
        let project_id_for_agents = project.id.clone();
        let mut branch_paths: HashMap<String, PathBuf> = HashMap::new();
        let mut agents_remapped = 0usize;
        let mut warnings = Vec::new();

        for agent in agents {
            let desired = if agent.role == "leader" || agent.role == "manager" {
                Some((session_worktree.clone(), session_branch.clone()))
            } else {
                let Some(agent_branch) = agent.branch.clone() else {
                    warnings.push(format!(
                        "{} has no branch; task worktree not remapped",
                        agent.name
                    ));
                    continue;
                };
                if agent_branch == session_branch {
                    Some((session_worktree.clone(), session_branch.clone()))
                } else if let Some(path) = branch_paths.get(&agent_branch) {
                    Some((path.clone(), agent_branch))
                } else {
                    let target = data_dir
                        .join("worktrees")
                        .join(&project_id_for_agents)
                        .join(&session_id_for_agents)
                        .join("agents")
                        .join(&agent.id);
                    if target.starts_with(&canonical_root_for_agents) {
                        warnings.push(format!(
                            "{} task worktree target is inside the project root; skipped",
                            agent.name
                        ));
                        continue;
                    }
                    let root = project_root_for_agents.clone();
                    let branch_for_worktree = agent_branch.clone();
                    let target_for_worktree = target.clone();
                    let git_lock = self.git_lock.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        let _guard = git_lock
                            .lock()
                            .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                        if !worktree::branch_exists(&root, &branch_for_worktree)? {
                            anyhow::bail!("branch does not exist: {branch_for_worktree}");
                        }
                        worktree::rehydrate(&root, &target_for_worktree, &branch_for_worktree, "")
                    })
                    .await?;
                    match result {
                        Ok(()) => {
                            branch_paths.insert(agent_branch.clone(), target.clone());
                            Some((target, agent_branch))
                        }
                        Err(error) => {
                            warnings.push(format!(
                                "{} task worktree not remapped: {error:#}",
                                agent.name
                            ));
                            None
                        }
                    }
                }
            };
            if let Some((worktree_path, branch_name)) = desired {
                self.set_agent_worktree(&agent.id, &worktree_path.to_string_lossy(), &branch_name)
                    .await?;
                agents_remapped += 1;
            }
        }

        Ok(SessionRehydrateResult {
            session_id: session.id,
            worktree: target_abs.to_string_lossy().to_string(),
            branch,
            commit,
            status: "idle".to_string(),
            agents_remapped,
            warnings,
        })
    }

    pub async fn import_session(&self, bundle: SessionBundle) -> Result<SessionImportResult> {
        let root = PathBuf::from(&bundle.project.root);
        if !root.is_dir() {
            anyhow::bail!(
                "bundle project root does not exist locally: {}",
                bundle.project.root
            );
        }
        if bundle.session.project_id != bundle.project.id {
            anyhow::bail!("bundle session/project ids do not match");
        }
        // A self-consistent hash is not enough: validate the portable shape
        // before a tampered archive can persist a malformed template record.
        for record in &bundle.team_templates {
            crate::team_template::validate_team_template_shape(&record.template)?;
        }
        let project_id = bundle.project.id.clone();
        let session_id = bundle.session.id.clone();
        let event_count = bundle.events.len() + 2;
        let bundle_test_results = bundle.test_results.clone();
        let store = self.store.clone();
        let event =
            tokio::task::spawn_blocking(move || store.import_session_bundle(&bundle)).await??;
        let _ = self.events.send(event);
        // Imported archives keep their raw events; rebuild the derived
        // coordination and decision projections so the archive is inspectable.
        let projection_store = self.store.clone();
        let projection_id = session_id.clone();
        let events = {
            let store = self.store.clone();
            let id = session_id.clone();
            tokio::task::spawn_blocking(move || store.list_all_events(&id)).await??
        };
        let notes = crate::orchestration::coordination_notes(&events);
        let decisions = crate::orchestration::decisions_from_events(&events);
        let scaling_decisions = crate::scaling::decisions_from_events(&events);
        let recoveries = crate::orchestration::recoveries_from_events(&events);
        tokio::task::spawn_blocking(move || -> Result<()> {
            projection_store.replace_coordination_notes(&projection_id, &notes, &events)?;
            projection_store.replace_orchestration_decisions(
                &projection_id,
                &decisions,
                &events,
            )?;
            projection_store.replace_scaling_decisions(
                &projection_id,
                &scaling_decisions,
                &events,
            )?;
            projection_store.replace_orchestration_recoveries(&projection_id, &recoveries)?;
            if let Some((runs, steps, checkpoints)) =
                crate::run_journal::projections_from_events(&events)
            {
                projection_store.replace_run_journal_projections(
                    &projection_id,
                    &runs,
                    &steps,
                    &checkpoints,
                )?;
            }
            let has_context_checkpoint_events = events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::context::CONTEXT_CHECKPOINT_CREATED_EVENT
                        | crate::context::CONTEXT_CHECKPOINT_DELETED_EVENT
                )
            });
            // A legacy projection-only bundle may carry rows without the
            // matching audit events. Preserve those rows; when events exist
            // they are authoritative and rebuild (or tombstone) the projection.
            if has_context_checkpoint_events
                && let Some(context_checkpoints) = crate::context::checkpoints_from_events(&events)
            {
                projection_store
                    .replace_context_checkpoints(&projection_id, &context_checkpoints)?;
            }
            let replayed_test_results = crate::test_cache::results_from_events(&events);
            let test_results = if replayed_test_results.is_empty() {
                bundle_test_results
            } else {
                replayed_test_results
            };
            projection_store.replace_test_results(&projection_id, &test_results)?;
            // Effect events are authoritative when the archive carries them;
            // a legacy projection-only bundle keeps the rows inserted above.
            if events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::effect::EFFECT_INTENT_EVENT
                        | crate::effect::EFFECT_SETTLED_EVENT
                        | crate::effect::EFFECT_FAILED_EVENT
                        | crate::effect::EFFECT_UNKNOWN_EVENT
                        | crate::effect::EFFECT_RECONCILED_EVENT
                )
            }) && let Some(effects) = crate::effect::receipts_from_events(&events)
            {
                projection_store.replace_effect_receipts(&projection_id, &effects)?;
            }
            Ok(())
        })
        .await??;
        Ok(SessionImportResult {
            project_id,
            session_id,
            events: event_count,
            status: "imported".to_string(),
        })
    }

    pub async fn export_session(&self, session_id: &str) -> Result<SessionBundle> {
        let data = self.load_replay_data(session_id).await?;
        let store = self.store.clone();
        let project_id = data.session.project_id.clone();
        let project_store = store.clone();
        let project = tokio::task::spawn_blocking(move || project_store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let session_id = data.session.id.clone();
        let message_store = store.clone();
        let messages_session = session_id.clone();
        let agent_messages = tokio::task::spawn_blocking(move || {
            message_store.list_agent_messages(&messages_session, None, false)
        })
        .await??;
        let artifact_store = store.clone();
        let artifacts_session = session_id.clone();
        let mut artifacts =
            tokio::task::spawn_blocking(move || artifact_store.list_artifacts(&artifacts_session))
                .await??;
        let usage_store = store.clone();
        let usage_session = session_id.clone();
        let usage =
            tokio::task::spawn_blocking(move || usage_store.list_usage_entries(&usage_session))
                .await??;
        let program_store = store.clone();
        let program_session = session_id.clone();
        let programs = tokio::task::spawn_blocking(move || {
            program_store.list_programs_for_session(&program_session)
        })
        .await??;
        let mut program_milestones = Vec::new();
        let mut program_events = Vec::new();
        for program in &programs {
            let milestone_store = store.clone();
            let program_id = program.id.clone();
            program_milestones.extend(
                tokio::task::spawn_blocking(move || {
                    milestone_store.list_program_milestones(&program_id)
                })
                .await??,
            );
            let event_store = store.clone();
            let program_id = program.id.clone();
            program_events.extend(
                tokio::task::spawn_blocking(move || {
                    event_store.list_program_events(&program_id, Some(10_000))
                })
                .await??,
            );
        }
        for artifact in &mut artifacts {
            if artifact.storage == "file"
                && let Some(path) = &artifact.path
            {
                let mut data = tokio::fs::read(path)
                    .await
                    .with_context(|| format!("reading artifact blob {path}"))?;
                if artifact.compression == "zstd" {
                    data = zstd::stream::decode_all(&data[..])
                        .with_context(|| format!("decompressing artifact {}", artifact.id))?;
                }
                if !artifact.sha256.is_empty() && sha256_hex(&data) != artifact.sha256 {
                    anyhow::bail!(
                        "artifact {} content does not match its SHA-256",
                        artifact.id
                    );
                }
                use base64::Engine as _;
                artifact.data_base64 = Some(base64::engine::general_purpose::STANDARD.encode(data));
            }
        }
        let team_plan = data
            .plan
            .as_deref()
            .and_then(|plan| serde_json::from_str::<TeamPlan>(plan).ok());
        let blob_store = store.clone();
        let instruction_hashes = data
            .instructions
            .iter()
            .map(|instruction| instruction.hash.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let instruction_blobs = tokio::task::spawn_blocking(move || {
            let mut blobs = Vec::with_capacity(instruction_hashes.len());
            for hash in instruction_hashes {
                let blob = blob_store.get_instruction_blob(&hash)?.ok_or_else(|| {
                    anyhow!("instruction state references missing blob {hash}; export refused")
                })?;
                if crate::instruction::hash_instruction_text(&blob.text) != hash {
                    anyhow::bail!(
                        "instruction blob {hash} does not match its text; export refused"
                    );
                }
                blobs.push(blob);
            }
            Ok::<_, anyhow::Error>(blobs)
        })
        .await??;
        let policy_store = store.clone();
        let policy_project = data.session.project_id.clone();
        let policy_session = data.session.id.clone();
        let mut policy_hashes = data
            .events
            .iter()
            .filter(|event| event.kind == crate::runtime_pin::POLICY_SNAPSHOT_EVENT)
            .filter_map(|event| {
                event
                    .payload
                    .get("hash")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect::<std::collections::BTreeSet<_>>();
        for run in &data.runs {
            if let Some(snapshot) = &run.runtime_snapshot
                && !snapshot.policy_hash.is_empty()
            {
                policy_hashes.insert(snapshot.policy_hash.clone());
            }
        }
        let policy_snapshots = tokio::task::spawn_blocking(move || {
            let mut policy_hashes = policy_hashes;
            if let Some(raw) = policy_store.get_plan_runtime_snapshot(&policy_session)?
                && let Some(snapshot) = crate::runtime_pin::parse_snapshot(&raw)
                && !snapshot.policy_hash.is_empty()
            {
                policy_hashes.insert(snapshot.policy_hash);
            }
            let mut rows = Vec::new();
            for hash in policy_hashes {
                if let Some(row) = policy_store.get_policy_snapshot(&policy_project, &hash)? {
                    rows.push(row);
                }
            }
            Ok::<_, anyhow::Error>(rows)
        })
        .await??;
        let identity_store = store.clone();
        let identity_project = project.id.clone();
        let (agent_identities, agent_identity_events) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                identity_store.list_agent_identities(&identity_project, None, None)?,
                identity_store
                    .list_agent_identity_events_for_project(&identity_project, Some(100_000))?,
            ))
        })
        .await??;
        let coordination_store = store.clone();
        let coordination_project = project.id.clone();
        let (session_agent_assignments, agent_inbox, agent_coordination_events) =
            tokio::task::spawn_blocking(move || {
                Ok::<_, anyhow::Error>((
                    coordination_store.list_agent_assignments_for_replay(&coordination_project)?,
                    coordination_store.list_agent_inbox_for_replay(&coordination_project)?,
                    coordination_store
                        .list_agent_coordination_events_for_project(&coordination_project, None)?,
                ))
            })
            .await??;
        let workspace_store = store.clone();
        let workspace_project = project.id.clone();
        let (agent_workspaces, session_worker_workspaces, agent_workspace_events) =
            tokio::task::spawn_blocking(move || {
                Ok::<_, anyhow::Error>((
                    workspace_store.list_agent_workspaces_for_replay(&workspace_project)?,
                    workspace_store.list_session_workspaces_for_replay(&workspace_project)?,
                    workspace_store
                        .list_agent_workspace_events_for_project(&workspace_project, None)?,
                ))
            })
            .await??;
        let memory_store = store.clone();
        let memory_project = project.id.clone();
        let (agent_memories, agent_memory_events) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                vp_store::store::identity_memory::list_agent_memories_for_replay(
                    &memory_store,
                    &memory_project,
                )?,
                vp_store::store::identity_memory::list_agent_memory_events_for_project(
                    &memory_store,
                    &memory_project,
                    None,
                )?,
            ))
        })
        .await??;
        let employment_store = store.clone();
        let employment_project = project.id.clone();
        // Materialize the derived ledger so an export always carries the
        // evidence-current rows rather than a stale explicit replay.
        self.rebuild_agent_performance_projection(&employment_project)
            .await?;
        let (
            agent_performance,
            agent_retirement_proposals,
            agent_employment_events,
            agent_hire_requests,
        ) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                vp_store::store::employment::list_agent_performance(
                    &employment_store,
                    &employment_project,
                    None,
                )?,
                vp_store::store::employment::list_retirement_proposals_for_replay(
                    &employment_store,
                    &employment_project,
                )?,
                vp_store::store::employment::list_employment_events(
                    &employment_store,
                    &employment_project,
                    Some(100_000),
                )?,
                vp_store::store::employment::list_hire_requests_for_replay(
                    &employment_store,
                    &employment_project,
                )?,
            ))
        })
        .await??;
        let template_store = store.clone();
        let template_project = project.id.clone();
        let (team_templates, project_team_templates, team_template_events) =
            tokio::task::spawn_blocking(move || {
                vp_store::store::team_template::manifest_records(&template_store, &template_project)
            })
            .await??;
        Ok(SessionBundle {
            version: 1,
            exported_at: vp_protocol::now_rfc3339(),
            project,
            session: data.session,
            events: data.events,
            agents: data.agents,
            messages: data.messages,
            team_plan,
            todos: data.todos,
            approvals: data.approvals,
            merge_queue: data.queue,
            agent_messages,
            artifacts,
            usage,
            memories: data.memories,
            memory_conflicts: data.memory_conflicts,
            questions: data.questions,
            tasks: data.tasks,
            steering: data.steering,
            continuations: data.continuations,
            gates: data.gates,
            policy_snapshots,
            recoveries: data.recoveries,
            programs,
            program_milestones,
            program_events,
            session_runs: data.runs,
            run_steps: data.run_steps,
            run_checkpoints: data.run_checkpoints,
            context_checkpoints: data.context_checkpoints,
            supervised_processes: data.processes,
            test_results: data.test_results,
            instruction_blobs,
            instructions: data.instructions,
            prompt_prefix_observations: data.prompt_prefix_observations,
            effects: data.effects,
            agent_identities,
            agent_identity_events,
            session_agent_assignments,
            agent_inbox,
            agent_coordination_events,
            agent_workspaces,
            session_worker_workspaces,
            agent_workspace_events,
            agent_memories,
            agent_memory_events,
            agent_performance,
            agent_retirement_proposals,
            agent_employment_events,
            agent_hire_requests,
            team_templates,
            project_team_templates,
            team_template_events,
        })
    }

    pub async fn rebuild_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        ensure_session_not_active(&self.store, session_id, "rebuild a session").await?;
        let data = self.load_replay_data(session_id).await?;
        self.restore_policy_snapshots_from_events(&data.events)
            .await?;
        let repair = Self::build_session_repair(&data)?;
        for warning in &repair.artifact_warnings {
            tracing::warn!(session_id, %warning, "artifact replay warning");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.repair_session_projections(&id, &repair))
            .await??;
        let expected_usage = Self::expected_usage_entries(&data);
        let usage_store = self.store.clone();
        let usage_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            usage_store.replace_usage_ledger(&usage_id, &expected_usage)
        })
        .await??;
        let expected_notes = crate::orchestration::coordination_notes(&data.events);
        let expected_decisions = crate::orchestration::decisions_from_events(&data.events);
        let expected_scaling = crate::scaling::decisions_from_events(&data.events);
        let expected_recoveries = crate::orchestration::recoveries_from_events(&data.events);
        let projection_store = self.store.clone();
        let projection_id = session_id.to_string();
        let projection_events = data.events.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            projection_store.replace_coordination_notes(
                &projection_id,
                &expected_notes,
                &projection_events,
            )?;
            projection_store.replace_orchestration_decisions(
                &projection_id,
                &expected_decisions,
                &projection_events,
            )?;
            projection_store.replace_scaling_decisions(
                &projection_id,
                &expected_scaling,
                &projection_events,
            )?;
            projection_store
                .replace_orchestration_recoveries(&projection_id, &expected_recoveries)?;
            Ok(())
        })
        .await??;
        self.append_event(
            session_id,
            "session.rebuilt",
            serde_json::json!({"event_count": data.events.len()}),
        )
        .await?;
        let data = self.load_replay_data(session_id).await?;
        let mut report = Self::replay_report(&data, true)?;
        report.rebuilt = true;
        Ok(report)
    }

    pub async fn doctor_checks(&self) -> Result<Vec<DoctorCheck>> {
        let store = self.store.clone();
        let data_dir = store.data_dir().to_path_buf();
        let (integrity, projects, sessions) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                store.integrity_check()?,
                store.list_projects()?,
                store.list_sessions(None)?,
            ))
        })
        .await??;
        let mut checks = Vec::new();
        checks.push(DoctorCheck {
            name: "database".into(),
            status: if integrity == "ok" { "ok" } else { "error" }.into(),
            detail: integrity,
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&data_dir)
                .map(|metadata| metadata.permissions().mode() & 0o777)
                .unwrap_or(0);
            checks.push(DoctorCheck {
                name: "data directory".into(),
                status: if mode & 0o077 == 0 { "ok" } else { "warn" }.into(),
                detail: format!("{} (mode {mode:o})", data_dir.display()),
            });
        }
        let missing_projects = projects
            .iter()
            .filter(|project| !Path::new(&project.root).is_dir())
            .count();
        checks.push(DoctorCheck {
            name: "project roots".into(),
            status: if missing_projects == 0 { "ok" } else { "warn" }.into(),
            detail: format!(
                "{} project(s), {missing_projects} missing root(s)",
                projects.len()
            ),
        });
        let missing_worktrees = sessions
            .iter()
            .filter(|session| session.status != "imported")
            .filter(|session| {
                session
                    .worktree
                    .as_deref()
                    .is_some_and(|path| !Path::new(path).exists())
            })
            .count();
        let active = sessions
            .iter()
            .filter(|session| matches!(session.status.as_str(), "running" | "queued" | "paused"))
            .count();
        checks.push(DoctorCheck {
            name: "sessions".into(),
            status: if active > 0 { "warn" } else { "ok" }.into(),
            detail: format!(
                "{} session(s), {active} active/queued/paused, {missing_worktrees} missing worktree(s)",
                sessions.len()
            ),
        });
        let mut mirror_mismatches = Vec::new();
        for session in &sessions {
            match self.session_log_status(&session.id).await {
                Ok(status) if !status.matches => mirror_mismatches.push(status.session_id),
                Ok(_) => {}
                Err(error) => mirror_mismatches.push(format!("{}: {error}", session.id)),
            }
        }
        checks.push(DoctorCheck {
            name: "event mirrors".into(),
            status: if mirror_mismatches.is_empty() {
                "ok"
            } else {
                "warn"
            }
            .into(),
            detail: if mirror_mismatches.is_empty() {
                "all mirrors match".to_string()
            } else {
                format!(
                    "{} mirror mismatch(es): {}",
                    mirror_mismatches.len(),
                    mirror_mismatches
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        });
        let sandbox = Path::new("/usr/bin/sandbox-exec").is_file();
        checks.push(DoctorCheck {
            name: "shell sandbox".into(),
            status: if sandbox { "ok" } else { "warn" }.into(),
            detail: if sandbox {
                "/usr/bin/sandbox-exec is available (deprecated by macOS)".to_string()
            } else {
                "sandbox-exec is missing; shell execution fails closed".to_string()
            },
        });
        Ok(checks)
    }

    pub async fn session_log_status(&self, session_id: &str) -> Result<SessionLogStatus> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (mirror, database) = tokio::task::spawn_blocking(move || {
            let mirror = store.read_event_mirror(&id)?;
            let database = store.list_all_events(&id)?;
            Ok::<_, anyhow::Error>((mirror, database))
        })
        .await??;
        let mirror_count = mirror.as_ref().map_or(0, Vec::len);
        let matches = match &mirror {
            None => database.is_empty(),
            Some(mirror) => {
                mirror.len() == database.len()
                    && mirror.iter().zip(database.iter()).all(|(left, right)| {
                        left.seq == right.seq
                            && left.kind == right.kind
                            && left.payload == right.payload
                            && left.created_at == right.created_at
                    })
            }
        };
        let detail = if matches {
            "event mirror matches the database".to_string()
        } else if mirror.is_none() {
            format!(
                "event mirror is missing; database has {} event(s)",
                database.len()
            )
        } else if mirror_count != database.len() {
            format!(
                "event mirror has {} event(s); database has {}",
                mirror_count,
                database.len()
            )
        } else {
            "event mirror differs from the database".to_string()
        };
        Ok(SessionLogStatus {
            session_id: session_id.to_string(),
            mirror_events: mirror_count,
            database_events: database.len(),
            matches,
            detail,
        })
    }

    pub async fn restore_session_log(&self, session_id: &str) -> Result<SessionLogStatus> {
        ensure_session_not_active(&self.store, session_id, "restore an event log").await?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let mirror = tokio::task::spawn_blocking(move || store.read_event_mirror(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("event mirror is missing for {session_id}"))?;
        if mirror.is_empty() {
            anyhow::bail!("event mirror is empty for {session_id}");
        }
        let mut expected_seq = 1u64;
        for event in &mirror {
            if event.session_id != session_id {
                anyhow::bail!("event mirror contains an event for a different session");
            }
            if event.seq != expected_seq {
                anyhow::bail!(
                    "event mirror sequence is not contiguous at {} (expected {}, found {})",
                    event.seq,
                    expected_seq,
                    event.seq
                );
            }
            expected_seq = expected_seq.saturating_add(1);
        }
        let store = self.store.clone();
        let events = mirror.clone();
        let log_session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.replace_events(&log_session_id, &events))
            .await??;
        self.rebuild_session(session_id).await?;
        self.session_log_status(session_id).await
    }

    pub async fn replay_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        let data = self.load_replay_data(session_id).await?;
        Self::replay_report(&data, false)
    }

    /// Rebuild the immutable policy projection from `policy.snapshot` events.
    /// Rows are content-addressed and idempotent; a malformed row fails closed.
    async fn restore_policy_snapshots_from_events(&self, events: &[Event]) -> Result<()> {
        let rows = crate::runtime_pin::policy_snapshots_from_events(events);
        if rows.is_empty() {
            return Ok(());
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            for row in &rows {
                store.insert_policy_snapshot_snapshot(row)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    pub async fn repair_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        ensure_session_not_active(&self.store, session_id, "repair a session").await?;
        let data = self.load_replay_data(session_id).await?;
        self.restore_policy_snapshots_from_events(&data.events)
            .await?;
        let report = Self::replay_report(&data, false)?;
        if report.consistent {
            return Ok(report);
        }
        let repair = Self::build_session_repair(&data)?;
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.repair_session_projections(&id, &repair))
            .await??;
        let expected_usage = Self::expected_usage_entries(&data);
        let usage_store = self.store.clone();
        let usage_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            usage_store.replace_usage_ledger(&usage_id, &expected_usage)
        })
        .await??;
        let expected_notes = crate::orchestration::coordination_notes(&data.events);
        let expected_decisions = crate::orchestration::decisions_from_events(&data.events);
        let expected_scaling = crate::scaling::decisions_from_events(&data.events);
        let expected_recoveries = crate::orchestration::recoveries_from_events(&data.events);
        let projection_store = self.store.clone();
        let projection_id = session_id.to_string();
        let projection_events = data.events.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            projection_store.replace_coordination_notes(
                &projection_id,
                &expected_notes,
                &projection_events,
            )?;
            projection_store.replace_orchestration_decisions(
                &projection_id,
                &expected_decisions,
                &projection_events,
            )?;
            projection_store.replace_scaling_decisions(
                &projection_id,
                &expected_scaling,
                &projection_events,
            )?;
            projection_store
                .replace_orchestration_recoveries(&projection_id, &expected_recoveries)?;
            Ok(())
        })
        .await??;
        self.append_event(
            session_id,
            "session.repaired",
            serde_json::json!({
                "mismatches": report.mismatches,
            }),
        )
        .await?;
        let data = self.load_replay_data(session_id).await?;
        Self::replay_report(&data, true)
    }
}
