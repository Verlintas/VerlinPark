use super::*;

impl Engine {
    pub async fn orchestration_policy(&self, project_id: &str) -> Result<OrchestrationPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_orchestration_policy(&project_id)).await?
    }

    pub async fn set_orchestration_policy(
        &self,
        project_id: &str,
        policy: &OrchestrationPolicy,
    ) -> Result<OrchestrationPolicy> {
        crate::orchestration::validate_policy(policy)?;
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let policy_owned = policy.clone();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_owned)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_owned}"));
            }
            store.set_orchestration_policy(&project_id_owned, &policy_owned)
        })
        .await??;
        self.record_project_manifest().await?;
        self.orchestration_policy(project_id).await
    }

    pub async fn plan_revisions(&self, session_id: &str) -> Result<Vec<PlanRevisionInfo>> {
        let events = self.recent_events(session_id, 5_000).await?;
        Ok(crate::orchestration::revisions_from_events(&events))
    }

    pub async fn critical_path_policy(&self, project_id: &str) -> Result<CriticalPathPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_critical_path_policy(&project_id)).await?
    }

    pub async fn set_critical_path_policy(
        &self,
        project_id: &str,
        policy: &CriticalPathPolicy,
    ) -> Result<CriticalPathPolicy> {
        policy
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid critical-path policy: {error}"))?;
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let policy_owned = policy.clone();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_owned)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_owned}"));
            }
            store.set_critical_path_policy(&project_id_owned, &policy_owned)
        })
        .await??;
        self.record_project_manifest().await?;
        self.critical_path_policy(project_id).await
    }

    /// Observed input+output tokens per task. The read only happens for
    /// multi-path policies that actually blend observed spend; single mode
    /// never touches the ledger here.
    async fn governor_observed_tokens(
        &self,
        session_id: &str,
        policy: &CriticalPathPolicy,
    ) -> Result<HashMap<String, u64>> {
        if policy.mode != vp_protocol::types::CriticalPathMode::Multi
            || policy.observed_weight_ratio <= 0.0
        {
            return Ok(HashMap::new());
        }
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.usage_tokens_by_task(&session_id)).await?
    }

    /// Bounded newest `budget.governor.decision` history. The events remain the
    /// source of truth; this is a read-only projection for the CLI/Swift UI.
    pub async fn governor_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<BudgetGovernorDecision>> {
        let events = self.recent_events(session_id, 5_000).await?;
        Ok(crate::budget_governor::decisions_from_events(
            &events,
            limit.clamp(1, 100) as usize,
        ))
    }

    /// Live critical-path governor status for one session. It evaluates the
    /// same pure function as scheduler admission; `governor.preview` uses the
    /// same builder with an optional remaining-token override.
    pub async fn budget_governor_status(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<GovernorSessionStatus> {
        self.build_governor_status(session_id, None, limit, None)
            .await
    }

    pub async fn preview_budget_governor(
        &self,
        session_id: &str,
        remaining_tokens: Option<u64>,
        limit: u32,
    ) -> Result<GovernorSessionStatus> {
        self.preview_budget_governor_with_mode(session_id, remaining_tokens, limit, None)
            .await
    }

    /// Preview with an explicit `single`/`multi` override. This changes only
    /// the preview evaluation; the project policy is not persisted or
    /// rewritten. The response's `policy` reflects the effective preview mode.
    pub async fn preview_budget_governor_with_mode(
        &self,
        session_id: &str,
        remaining_tokens: Option<u64>,
        limit: u32,
        mode_override: Option<vp_protocol::types::CriticalPathMode>,
    ) -> Result<GovernorSessionStatus> {
        self.build_governor_status(session_id, remaining_tokens, limit, mode_override)
            .await
    }

    async fn build_governor_status(
        &self,
        session_id: &str,
        remaining_override: Option<u64>,
        limit: u32,
        mode_override: Option<vp_protocol::types::CriticalPathMode>,
    ) -> Result<GovernorSessionStatus> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let mut policy = self.critical_path_policy(&session.project_id).await?;
        if let Some(mode) = mode_override {
            // Preview override only. `normalized` keeps the rest of the policy
            // exactly as persisted; no write path is touched.
            policy.mode = mode;
            policy = policy.normalized();
        }
        let plan = self.get_team_plan(session_id).await?;
        let queue = self.task_queue(None, Some(session_id.to_string())).await?;
        let tasks = queue.tasks;
        let mut statuses = tasks
            .iter()
            .map(|task| (task.task_id.clone(), task.status.clone()))
            .collect::<HashMap<_, _>>();
        if let Some(plan) = plan.as_ref() {
            for task in &plan.tasks {
                statuses
                    .entry(task.id.clone())
                    .or_insert_with(|| "pending".to_string());
            }
        }
        let plan_revision = self.current_plan_revision(session_id).await?;
        let reserved = self.budget_registry.reserved_for_session(&session.id);
        let spent = session.tokens_in.saturating_add(session.tokens_out);
        let budget = GovernorBudgetView::new(session.token_budget, spent, reserved);
        let remaining = remaining_override.or_else(|| budget.remaining_tokens());
        let observed_tokens = self.governor_observed_tokens(&session.id, &policy).await?;
        let evaluated_at = now_rfc3339();
        let last_decisions = self.governor_decisions(session_id, limit).await?;
        let mut status = GovernorSessionStatus {
            session_id: session.id.clone(),
            project_id: session.project_id.clone(),
            session_status: session.status.clone(),
            plan_revision,
            enabled: policy.enabled && !session.unlimited,
            unlimited: session.unlimited,
            plan_present: plan.is_some(),
            policy: policy.clone(),
            critical_path: None,
            reserve: crate::budget_governor::reserve_math(&policy, budget, remaining, 0, true)
                .info(),
            inputs_hash: String::new(),
            decisions: Vec::new(),
            admitted: Vec::new(),
            deferred: Vec::new(),
            last_decisions,
            paths: Vec::new(),
            departments: Vec::new(),
            deferred_order: Vec::new(),
            error: None,
            generated_at: evaluated_at.clone(),
        };
        let Some(plan) = plan else {
            return Ok(status);
        };
        let orchestration = self.orchestration_policy(&session.project_id).await?;
        let infos = governor_candidate_infos(&session, &plan, &tasks, &orchestration);
        let candidates = infos
            .iter()
            .map(|info| crate::budget_governor::GovernorCandidate {
                task: &info.task,
                effective_priority: info.effective_priority,
                enqueued_at: info.enqueued_at.as_str(),
            })
            .collect::<Vec<_>>();
        if session.unlimited {
            status.critical_path = crate::critical_path::compute_critical_path(
                crate::critical_path::CriticalPathInput {
                    session_id: &session.id,
                    project_id: &session.project_id,
                    plan_revision,
                    plan: &plan,
                    statuses: &statuses,
                    remaining_budget_tokens: None,
                },
            )
            .ok()
            .map(|path| path.info());
            if policy.mode == vp_protocol::types::CriticalPathMode::Multi {
                status.paths =
                    crate::multi_path::compute_multi_path(crate::multi_path::MultiPathInput {
                        session_id: &session.id,
                        project_id: &session.project_id,
                        plan_revision,
                        plan: &plan,
                        statuses: &statuses,
                        remaining_budget_tokens: None,
                        policy: &policy,
                        observed_tokens: &observed_tokens,
                    })
                    .ok()
                    .map(|allocation| allocation.path_infos())
                    .unwrap_or_default();
            }
            return Ok(status);
        }
        match crate::budget_governor::evaluate(crate::budget_governor::GovernorEvaluation {
            session_id: &session.id,
            project_id: &session.project_id,
            plan_revision,
            plan: &plan,
            statuses: &statuses,
            policy: &policy,
            candidates: &candidates,
            budget,
            remaining_override,
            observed_tokens: &observed_tokens,
            evaluated_at: &evaluated_at,
        }) {
            Ok(admission) => {
                status.enabled = admission.enabled;
                status.critical_path = Some(admission.path.info());
                status.reserve = admission.reserve.info();
                status.inputs_hash = admission.inputs_hash.clone();
                status.decisions = admission.decision_infos();
                status.admitted = admission.admitted.clone();
                status.deferred = admission.deferral_infos();
                status.paths = admission.path_infos();
                status.departments = admission.department_infos();
                status.deferred_order = admission.deferred_order();
            }
            Err(failure) => {
                status.enabled = policy.enabled;
                status.error = Some(failure.info());
            }
        }
        Ok(status)
    }

    /// Static governor context for one provider run. It is computed once at
    /// run start; a plan revision or completion changes the next run or
    /// scheduler tick, never an in-flight provider request.
    pub async fn governor_run_context(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<Option<crate::budget_governor::GovernorRunContext>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.unlimited {
            return Ok(None);
        }
        let policy = self.critical_path_policy(&session.project_id).await?;
        if !policy.enabled {
            return Ok(None);
        }
        let Some(plan) = self.get_team_plan(session_id).await? else {
            return Ok(None);
        };
        let queue = self.task_queue(None, Some(session_id.to_string())).await?;
        let statuses = queue
            .tasks
            .iter()
            .map(|task| (task.task_id.clone(), task.status.clone()))
            .collect::<HashMap<_, _>>();
        let plan_revision = self.current_plan_revision(session_id).await?;
        let reserved = self.budget_registry.reserved_for_session(&session.id);
        let remaining = session.token_budget.map(|limit| {
            limit
                .saturating_sub(session.tokens_in)
                .saturating_sub(session.tokens_out)
                .saturating_sub(reserved)
        });
        let path =
            crate::critical_path::compute_critical_path(crate::critical_path::CriticalPathInput {
                session_id: &session.id,
                project_id: &session.project_id,
                plan_revision,
                plan: &plan,
                statuses: &statuses,
                remaining_budget_tokens: remaining,
            })?;
        if policy.mode == vp_protocol::types::CriticalPathMode::Multi {
            let observed_tokens = self.governor_observed_tokens(&session.id, &policy).await?;
            let allocation =
                crate::multi_path::compute_multi_path(crate::multi_path::MultiPathInput {
                    session_id: &session.id,
                    project_id: &session.project_id,
                    plan_revision,
                    plan: &plan,
                    statuses: &statuses,
                    remaining_budget_tokens: remaining,
                    policy: &policy,
                    observed_tokens: &observed_tokens,
                })?;
            let critical = task_id.is_some_and(|task| {
                allocation
                    .paths
                    .iter()
                    .any(|path| path.task_ids.iter().any(|id| id == task))
            });
            let runner_path = task_id
                .map(|task| allocation.task_path_id(task))
                .unwrap_or(None);
            let runner_allocation = runner_path
                .and_then(|path_id| allocation.paths.get(path_id as usize))
                .map(|path| path.allocation_tokens);
            let mut path_task_ids = allocation
                .paths
                .iter()
                .flat_map(|path| path.task_ids.iter().cloned())
                .collect::<Vec<_>>();
            path_task_ids.sort();
            path_task_ids.dedup();
            return Ok(Some(crate::budget_governor::GovernorRunContext {
                enabled: true,
                policy,
                task_id: task_id.map(str::to_string),
                critical,
                path_task_ids,
                path_weight_tokens: allocation.path_weight_tokens,
                path_count: allocation.paths.len(),
                path_allocation_tokens: runner_allocation,
                path_deficit_tokens: task_id
                    .map(|task| allocation.task_deficit(task))
                    .unwrap_or(0),
                plan_revision,
                inputs_hash: allocation.inputs_hash,
            }));
        }
        let critical = task_id.is_some_and(|task| path.task_ids.iter().any(|id| id == task));
        Ok(Some(crate::budget_governor::GovernorRunContext {
            enabled: true,
            policy,
            task_id: task_id.map(str::to_string),
            critical,
            path_task_ids: path.task_ids.clone(),
            path_weight_tokens: path.weight_tokens,
            path_count: usize::from(!path.task_ids.is_empty()),
            path_allocation_tokens: None,
            path_deficit_tokens: 0,
            plan_revision,
            inputs_hash: path.inputs_hash,
        }))
    }

    /// Persist one scheduler admission decision. Preview never calls this.
    pub async fn record_budget_governor_admission(
        &self,
        session_id: &str,
        admission: &crate::budget_governor::GovernorAdmission,
    ) -> Result<()> {
        if !admission.enabled {
            return Ok(());
        }
        let decision_id = format!("gov-{}", Uuid::now_v7());
        let now = now_rfc3339();
        let payload =
            crate::budget_governor::admission_payload(session_id, &decision_id, admission, &now);
        self.append_event(
            session_id,
            crate::budget_governor::DECISION_EVENT_KIND,
            payload,
        )
        .await?;
        Ok(())
    }

    pub async fn revise_team_plan(
        &self,
        session_id: &str,
        plan: TeamPlan,
        reason: &str,
        author: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 1_000 {
            anyhow::bail!("revision reason must be 1-1000 chars");
        }
        let author = author.trim();
        if author.is_empty() || author.chars().count() > 80 {
            anyhow::bail!("revision author must be 1-80 chars");
        }
        let session_id = session_id.to_string();
        let store = self.store.clone();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.get_session(&session_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        crate::ensure_session_quiescent(&session, "revise the team plan")?;
        let current_raw = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.get_team_plan(&session_id)
        })
        .await??;
        let current: TeamPlan = current_raw
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .ok_or_else(|| anyhow::anyhow!("session has no TeamPlan to revise"))?;
        let tasks = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.list_plan_tasks(None, Some(&session_id))
        })
        .await??;
        let events = self.recent_events(&session_id, 5_000).await?;
        let current_revision = crate::orchestration::plan_revision(&events);
        if let Some(expected) = expected_revision
            && expected != current_revision
        {
            anyhow::bail!(
                "plan revision conflict: expected {expected}, current {current_revision}"
            );
        }
        crate::agent::validate_team_plan(&plan, max_managers)?;
        let statuses = crate::orchestration::status_map(&current, &tasks, &events);
        crate::orchestration::ensure_revision_safe(&current, &plan, &statuses)?;
        let diff = crate::orchestration::diff_plan(&current, &plan);
        let revision = current_revision.saturating_add(1);
        let snapshot = self.pin_plan_snapshot(&session, &plan, revision).await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let payload = serde_json::json!({
            "plan": plan,
            "revision": revision,
            "reason": reason,
            "author": author,
            "task_count": plan.tasks.len(),
            "added": diff.added,
            "removed": diff.removed,
            "changed": diff.changed,
            "runtime_snapshot": snapshot,
        });
        let plan_json = serde_json::to_string(&plan)?;
        let event = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            let plan_json = plan_json.clone();
            let payload = payload.clone();
            let snapshot_json = snapshot_json.clone();
            move || {
                store.set_team_plan_revision_with_runtime_snapshot_and_event(
                    &session_id,
                    &plan_json,
                    "team_plan.revised",
                    payload,
                    Some(&snapshot_json),
                )
            }
        })
        .await??;
        let _ = self.events.send(event.clone());
        self.scheduler.sync_plan(&session, &plan).await?;
        Ok(ReviseTeamPlanResult {
            revision: PlanRevisionInfo {
                revision,
                seq: event.seq,
                kind: event.kind,
                reason: Some(reason.to_string()),
                author: author.to_string(),
                task_count: plan.tasks.len(),
                added: diff.added,
                removed: diff.removed,
                changed: diff.changed,
                created_at: event.created_at,
            },
            plan,
        })
    }

    pub async fn coordination_notes(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_coordination_notes(&session_id, limit))
            .await?
    }

    pub async fn orchestration_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationDecision>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_orchestration_decisions(&session_id, limit))
            .await?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_coordination_note(
        &self,
        session_id: &str,
        action: &str,
        severity: &str,
        task_id: Option<&str>,
        target_agent_id: Option<&str>,
        target_note_id: Option<&str>,
        agent_id: &str,
        agent_name: &str,
        agent_role: Option<&str>,
        message: &str,
    ) -> Result<Event> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let action = action.to_string();
        let severity = severity.to_string();
        let task_id = task_id.map(str::to_string);
        let target_agent_id = target_agent_id.map(str::to_string);
        let target_note_id = target_note_id.map(str::to_string);
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let agent_role = agent_role.map(str::to_string);
        let message = message.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.record_coordination_note_with_event(
                &session_id_owned,
                &action,
                &severity,
                task_id.as_deref(),
                target_agent_id.as_deref(),
                target_note_id.as_deref(),
                &agent_id,
                &agent_name,
                agent_role.as_deref(),
                &message,
            )
        })
        .await??;
        let _ = self.events.send(event.clone());
        // A targeted note is also delivered to the durable mailbox so the
        // recipient still sees it after the recent-coordination window moves on.
        let target = event
            .payload
            .get("target_agent_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let note_action = event
            .payload
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("note");
        if let Some(target) = target
            && !matches!(note_action, "unblock" | "list")
        {
            let agents = self.list_agents(session_id).await?;
            if let Some(agent) = agents.iter().find(|agent| agent.id == target) {
                let _ = self
                    .send_agent_message(
                        session_id,
                        event
                            .payload
                            .get("agent_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("orchestrator"),
                        event
                            .payload
                            .get("agent_name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Orchestrator"),
                        &agent.id,
                        &agent.name,
                        "coordination",
                        event
                            .payload
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(""),
                        serde_json::json!({
                            "note_id": event.payload.get("note_id"),
                            "action": note_action,
                            "task_id": event.payload.get("task_id"),
                            "severity": event.payload.get("severity"),
                        }),
                    )
                    .await;
            }
        }
        Ok(event)
    }

    pub async fn coordination_notes_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let task_id = task_id.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.list_coordination_notes_for_agent(
                &session_id,
                &agent_id,
                task_id.as_deref(),
                limit,
            )
        })
        .await?
    }

    pub async fn plan_revision_detail(
        &self,
        session_id: &str,
        revision: u32,
    ) -> Result<PlanRevisionDetailResult> {
        let events = self.all_events(session_id).await?;
        let revisions = crate::orchestration::revisions_from_events(&events);
        let info = revisions
            .iter()
            .find(|candidate| candidate.revision == revision)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("plan revision {revision} not found"))?;
        let plan = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "team_plan.submitted" | "team_plan.revised"
                )
            })
            .filter(|event| {
                event
                    .payload
                    .get("revision")
                    .and_then(serde_json::Value::as_u64)
                    .map(|value| value as u32)
                    .unwrap_or_else(|| {
                        revisions
                            .iter()
                            .find(|candidate| candidate.seq == event.seq)
                            .map(|candidate| candidate.revision)
                            .unwrap_or(0)
                    })
                    == revision
            })
            .find_map(crate::orchestration::plan::plan_from_event)
            .ok_or_else(|| anyhow::anyhow!("plan revision {revision} has no stored plan"))?;
        Ok(PlanRevisionDetailResult {
            revision: info,
            plan,
        })
    }

    pub async fn restore_team_plan_revision(
        &self,
        session_id: &str,
        revision: u32,
        reason: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let detail = self.plan_revision_detail(session_id, revision).await?;
        self.revise_team_plan(
            session_id,
            detail.plan,
            reason,
            "user",
            expected_revision,
            max_managers,
        )
        .await
    }

    pub async fn edit_plan_task(
        &self,
        session_id: &str,
        task: vp_protocol::types::PlanTask,
        reason: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let current = self
            .get_team_plan(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session has no TeamPlan to edit"))?;
        let mut plan = current.clone();
        let index = plan
            .tasks
            .iter()
            .position(|candidate| candidate.id == task.id)
            .ok_or_else(|| anyhow::anyhow!("task not found in plan: {}", task.id))?;
        plan.tasks[index] = task;
        self.revise_team_plan(
            session_id,
            plan,
            reason,
            "user",
            expected_revision,
            max_managers,
        )
        .await
    }

    pub async fn all_events(&self, session_id: &str) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_all_events(&session_id)).await?
    }

    /// Read-only controller replay. This never calls a provider, executes a
    /// tool, appends an event, or mutates a projection.
    pub async fn replay_orchestration(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::replay::ControllerReplayReport> {
        crate::orchestration::replay::replay_orchestration(self, session_id, limit).await
    }

    pub async fn orchestration_snapshot(&self, session_id: &str) -> Result<OrchestrationSnapshot> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let plan = self.get_team_plan(session_id).await?;
        let queue = self.task_queue(None, Some(session_id.to_string())).await?;
        let tasks = queue.tasks;
        let progress = queue.progress;
        let store = self.store.clone();
        let id = session_id.to_string();
        let recoveries = tokio::task::spawn_blocking(move || {
            store.list_current_orchestration_recoveries(None, Some(&id))
        })
        .await??;
        let stall_threshold_secs = match task_stall_threshold_secs() {
            0 => None,
            secs => Some(secs),
        };
        let agents = self.list_agents(session_id).await?;
        let merge_queue = self.list_merge_queue(session_id).await?;
        let questions = self
            .list_questions(Some(session_id.to_string()))
            .await?
            .questions;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let decisions = self.orchestration_decisions(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        Ok(crate::orchestration::build_snapshot(
            crate::orchestration::OrchestrationInput {
                session: &session,
                plan: plan.as_ref(),
                scheduled: &tasks,
                agents: &agents,
                events: &events,
                merge_queue: &merge_queue,
                questions: &questions,
                notes: &notes,
                decisions: &decisions,
                progress: &progress,
                recoveries: &recoveries,
                stall_threshold_secs,
                policy: &policy,
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_user_coordination_note(
        &self,
        session_id: &str,
        action: &str,
        task_id: Option<&str>,
        target_agent_id: Option<&str>,
        target_note_id: Option<&str>,
        message: &str,
        severity: Option<&str>,
    ) -> Result<String> {
        if !matches!(
            action,
            "note" | "blocker" | "unblock" | "decision" | "escalate"
        ) {
            anyhow::bail!("unsupported coordination action: {action}");
        }
        let message = message.trim();
        if message.is_empty() || message.chars().count() > 2_000 {
            anyhow::bail!("coordination message must be 1-2000 chars");
        }
        let task_id = task_id.map(str::trim).filter(|task| !task.is_empty());
        if let Some(task_id) = task_id {
            crate::worktree::validate_task_id(task_id)?;
        }
        let severity = severity.unwrap_or(if matches!(action, "blocker" | "escalate") {
            "warning"
        } else {
            "info"
        });
        if !matches!(severity, "info" | "warning" | "critical") {
            anyhow::bail!("severity must be info, warning, or critical");
        }
        if let Some(target) = target_agent_id {
            let agents = self.list_agents(session_id).await?;
            if !agents.iter().any(|agent| agent.id == target) {
                anyhow::bail!("target agent is not in this session: {target}");
            }
        }
        if let Some(note) = target_note_id {
            let notes = self.coordination_notes(session_id, 2_000).await?;
            if !notes.iter().any(|candidate| candidate.note_id == note) {
                anyhow::bail!("coordination note not found: {note}");
            }
        }
        let event = self
            .record_coordination_note(
                session_id,
                action,
                severity,
                task_id,
                target_agent_id,
                target_note_id,
                "user",
                "User",
                Some("user"),
                message,
            )
            .await?;
        Ok(event
            .payload
            .get("note_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Recent TeamPlan sessions across the selected project (or all projects)
    /// with their live metrics and top blockers. The daemon exposes this for
    /// Manage Dash; the per-session snapshot remains the source of truth.
    pub async fn orchestration_overview(
        &self,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<OrchestrationOverviewResult> {
        let sessions = self.list_sessions(project_id.map(str::to_string)).await?;
        let projects = self.list_projects().await?;
        let project_names = projects
            .into_iter()
            .map(|project| (project.id, project.name))
            .collect::<HashMap<_, _>>();
        let limit = limit.clamp(1, 50) as usize;
        let mut summaries = Vec::new();
        for session in sessions.into_iter().take(limit.saturating_mul(4)) {
            let Ok(snapshot) = self.orchestration_snapshot(&session.id).await else {
                continue;
            };
            if snapshot.plan.is_none() {
                continue;
            }
            let blockers = snapshot
                .blockers
                .iter()
                .take(3)
                .map(|blocker| format!("{}: {}", blocker.kind, blocker.message))
                .collect::<Vec<_>>();
            let last = snapshot.decisions.last().cloned();
            let active_recoveries = snapshot
                .recoveries
                .iter()
                .filter(|recovery| {
                    matches!(recovery.status.as_str(), "requested" | "waiting_cancel")
                })
                .count();
            let failed_recoveries = snapshot
                .recoveries
                .iter()
                .filter(|recovery| recovery.status == "failed")
                .count();
            let last_recovery = snapshot
                .recoveries
                .iter()
                .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
                .map(|recovery| {
                    format!(
                        "{}: {} ({})",
                        recovery.task_id,
                        recovery.status,
                        recovery
                            .last_error
                            .as_deref()
                            .unwrap_or(recovery.action.as_str())
                    )
                });
            summaries.push(OrchestrationSessionSummary {
                session_id: session.id.clone(),
                project_id: session.project_id.clone(),
                project_name: project_names
                    .get(&session.project_id)
                    .cloned()
                    .unwrap_or_else(|| session.project_id.clone()),
                goal: session.goal.clone(),
                status: session.status.clone(),
                priority: session.priority,
                plan_revision: snapshot.plan_revision,
                metrics: snapshot.metrics.clone(),
                blockers,
                last_decision: last.as_ref().map(|decision| decision.detail.clone()),
                last_decision_at: last.map(|decision| decision.created_at),
                recoveries: snapshot.recoveries.len(),
                active_recoveries,
                failed_recoveries,
                last_recovery,
            });
        }
        summaries.sort_by(|lhs, rhs| {
            let left_attention = lhs.metrics.attention_count;
            let right_attention = rhs.metrics.attention_count;
            if left_attention != right_attention {
                return right_attention.cmp(&left_attention);
            }
            let left_running = lhs.status == "running" || lhs.status == "queued";
            let right_running = rhs.status == "running" || rhs.status == "queued";
            if left_running != right_running {
                return right_running.cmp(&left_running);
            }
            rhs.priority.cmp(&lhs.priority)
        });
        summaries.truncate(limit);
        let totals = OrchestrationOverviewTotals {
            sessions: summaries.len(),
            running: summaries
                .iter()
                .filter(|summary| matches!(summary.status.as_str(), "running" | "queued"))
                .count(),
            attention: summaries
                .iter()
                .filter(|summary| summary.metrics.attention_count > 0)
                .count(),
            blocked_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.blocked_tasks)
                .sum(),
            failed_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.failed_tasks)
                .sum(),
            retrying_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.retrying_tasks)
                .sum(),
            open_blockers: summaries
                .iter()
                .map(|summary| summary.metrics.open_blockers)
                .sum(),
            merge_conflicts: summaries
                .iter()
                .map(|summary| summary.metrics.merge_conflicts)
                .sum(),
            stalled_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.stalled_tasks)
                .sum(),
            recoveries: summaries.iter().map(|summary| summary.recoveries).sum(),
            active_recoveries: summaries
                .iter()
                .map(|summary| summary.active_recoveries)
                .sum(),
            failed_recoveries: summaries
                .iter()
                .map(|summary| summary.failed_recoveries)
                .sum(),
        };
        Ok(OrchestrationOverviewResult {
            sessions: summaries,
            totals,
            generated_at: vp_protocol::now_rfc3339(),
        })
    }

    /// Preview the controller decisions without mutating task rows or notes.
    pub async fn preview_orchestration(
        &self,
        session_id: &str,
    ) -> Result<OrchestrationPreviewResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let Some(plan) = self.get_team_plan(session_id).await? else {
            return Ok(OrchestrationPreviewResult {
                session_id: session_id.to_string(),
                actions: Vec::new(),
            });
        };
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        let actions = crate::orchestration::plan_actions(crate::orchestration::ControllerInput {
            session: &session,
            plan: &plan,
            scheduled: &tasks,
            events: &events,
            notes: &notes,
            policy: &policy,
            budget_exhausted: session_budget_exhausted(&session),
        });
        Ok(OrchestrationPreviewResult {
            session_id: session_id.to_string(),
            actions: actions
                .into_iter()
                .map(|action| OrchestrationPreviewAction {
                    action: action.action().to_string(),
                    task_id: action.task_id().to_string(),
                    detail: action.detail(),
                    attempt: action.attempt(),
                })
                .collect(),
        })
    }

    /// Run one deterministic controller pass for a session. Actions are
    /// idempotent: retries are gated by `next_attempt_at`, blocked rows are
    /// skipped once moved, and escalations are deduplicated by attempt.
    pub async fn advance_orchestration(
        &self,
        session_id: &str,
    ) -> Result<Vec<OrchestrationDecision>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let Some(plan) = self.get_team_plan(session_id).await? else {
            return Ok(Vec::new());
        };
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        let actions = crate::orchestration::plan_actions(crate::orchestration::ControllerInput {
            session: &session,
            plan: &plan,
            scheduled: &tasks,
            events: &events,
            notes: &notes,
            policy: &policy,
            budget_exhausted: session_budget_exhausted(&session),
        });
        let mut decisions = Vec::new();
        for action in actions {
            let task_id = action.task_id().to_string();
            let Some(row) = tasks.iter().find(|row| row.task_id == task_id) else {
                continue;
            };
            let plan_seq = row.plan_seq;
            match &action {
                crate::orchestration::ControllerAction::Retry {
                    task_id,
                    delay_secs,
                    reason,
                } => {
                    if let Err(error) = self
                        .schedule_plan_task_retry(
                            session_id,
                            plan_seq,
                            task_id,
                            std::time::Duration::from_secs(*delay_secs),
                            reason,
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration retry failed");
                        continue;
                    }
                }
                crate::orchestration::ControllerAction::Block {
                    task_id,
                    dependency,
                    reason,
                } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let dependency = dependency.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.block_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                            &dependency,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Unblock { task_id, reason } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.unblock_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Cancel {
                    task_id,
                    dependency,
                    reason,
                } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let dependency = dependency.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.cancel_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                            &dependency,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Escalate {
                    task_id,
                    attempt: _,
                    message,
                } => {
                    if let Err(error) = self
                        .record_coordination_note(
                            session_id,
                            "escalate",
                            "critical",
                            Some(task_id),
                            None,
                            None,
                            "orchestrator",
                            "Orchestrator",
                            Some("orchestrator"),
                            message,
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration escalation failed");
                        continue;
                    }
                }
                crate::orchestration::ControllerAction::ResolveBlockers { task_id, count } => {
                    if let Err(error) = self
                        .record_coordination_note(
                            session_id,
                            "unblock",
                            "info",
                            Some(task_id),
                            None,
                            None,
                            "orchestrator",
                            "Orchestrator",
                            Some("orchestrator"),
                            &format!(
                                "{task_id} completed; {count} coordination blocker(s) resolved automatically"
                            ),
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration blocker resolution failed");
                        continue;
                    }
                }
            }
            decisions.push(
                self.record_orchestration_decision(
                    session_id,
                    action.action(),
                    Some(action.task_id()),
                    &action.detail(),
                    action.attempt(),
                )
                .await?,
            );
        }
        Ok(decisions)
    }

    pub async fn record_orchestration_decision(
        &self,
        session_id: &str,
        action: &str,
        task_id: Option<&str>,
        detail: &str,
        attempt: u32,
    ) -> Result<OrchestrationDecision> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let action_owned = action.to_string();
        let task_id_owned = task_id.map(str::to_string);
        let detail_owned = detail.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.record_orchestration_decision_with_event(
                &session_id_owned,
                &action_owned,
                task_id_owned.as_deref(),
                &detail_owned,
                attempt,
                "orchestrator",
            )
        })
        .await??;
        let _ = self.events.send(event.clone());
        Ok(OrchestrationDecision {
            decision_id: event.payload["decision_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            action: action.to_string(),
            task_id: task_id.map(str::to_string),
            detail: detail.to_string(),
            attempt,
            agent_id: "orchestrator".to_string(),
            created_at: event.created_at,
        })
    }

    pub async fn schedule_plan_task_retry(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        delay: std::time::Duration,
        reason: &str,
    ) -> Result<ScheduledTask> {
        let next = time::OffsetDateTime::now_utc()
            + time::Duration::seconds(delay.as_secs().min(i64::MAX as u64) as i64);
        let next = next
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting retry time")?;
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let task_id_owned = task_id.to_string();
        let reason_owned = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.schedule_plan_task_retry_with_event(
                &session_id_owned,
                plan_seq,
                &task_id_owned,
                &next,
                &reason_owned,
            )
        })
        .await??;
        let (task, event) =
            result.ok_or_else(|| anyhow::anyhow!("task {task_id} cannot be retried"))?;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn set_team_plan(&self, session_id: &str, plan: &TeamPlan) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let json = serde_json::to_string(plan)?;
        let loaded = {
            let store = store.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || store.get_session(&id)).await??
        }
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let revision = self
            .current_plan_revision(session_id)
            .await?
            .saturating_add(1);
        let snapshot = self.pin_plan_snapshot(&loaded, plan, revision).await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let event = tokio::task::spawn_blocking(move || {
            store.set_team_plan_with_runtime_snapshot_and_event(&id, &json, Some(&snapshot_json))
        })
        .await??;
        let _ = self.events.send(event);
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        self.scheduler.sync_plan(&session, plan).await?;
        Ok(())
    }

    pub async fn task_queue(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<TaskQueueResult> {
        self.scheduler.queue(project_id, session_id).await
    }

    /// Create an operator steering request for one running agent/run. When
    /// `agent_id` is omitted the session Leader is targeted; there is no
    /// implicit broadcast to Workers. An optional operation id replays the
    /// first steering row instead of creating a duplicate.
    pub async fn steer_session(
        &self,
        session_id: &str,
        body: &str,
        agent_id: Option<&str>,
        task_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> Result<SessionSteerResult> {
        let payload = serde_json::json!({
            "session_id": session_id,
            "body": body,
            "agent_id": agent_id,
            "task_id": task_id,
        });
        let receipt = match crate::operation::begin(
            self.store(),
            session_id,
            operation_id,
            "session.steer",
            &payload,
        )
        .await?
        {
            crate::operation::OperationStart::Replay(result) => {
                return crate::operation::replay(&result);
            }
            crate::operation::OperationStart::Proceed(receipt) => receipt,
        };
        let result = self
            .steer_session_inner(session_id, body, agent_id, task_id)
            .await;
        match &result {
            Ok(value) => {
                if let Err(error) = crate::operation::complete(&receipt, value).await {
                    return Err(error.context(
                        "steering was created but the session operation receipt could not be completed",
                    ));
                }
            }
            Err(error) => {
                crate::operation::fail(&receipt, &format!("{error:#}")).await;
            }
        }
        result
    }

    async fn steer_session_inner(
        &self,
        session_id: &str,
        body: &str,
        agent_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<SessionSteerResult> {
        let body = body.trim();
        if body.is_empty() || body.chars().count() > 4_000 {
            anyhow::bail!("steering body must be 1-4000 chars");
        }
        let task_id = task_id.map(str::trim).filter(|task| !task.is_empty());
        if let Some(task_id) = task_id {
            crate::worktree::validate_task_id(task_id)?;
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let agents = self.list_agents(session_id).await?;
        let target = match agent_id {
            Some(agent_id) => agents
                .iter()
                .find(|agent| agent.id == agent_id)
                .ok_or_else(|| anyhow!("target agent is not in this session: {agent_id}"))?,
            None => agents
                .iter()
                .find(|agent| {
                    agent.role == "leader"
                        && matches!(agent.status.as_str(), "running" | "waiting_input")
                })
                .ok_or_else(|| {
                    anyhow!(
                        "no running Leader in this session; pass --agent <id> or start the session"
                    )
                })?,
        };
        if !matches!(target.status.as_str(), "running" | "waiting_input") {
            anyhow::bail!(
                "target agent {} is {}; steering requires a running or waiting agent",
                target.id,
                target.status
            );
        }
        if let Some(task_id) = task_id {
            if target.task_id.as_deref() != Some(task_id) {
                anyhow::bail!(
                    "target agent {} is not working on task {task_id}",
                    target.id
                );
            }
            if let Some(plan) = self.get_team_plan(session_id).await?
                && !plan.tasks.iter().any(|task| task.id == task_id)
            {
                anyhow::bail!("task {task_id} is not in the stored TeamPlan");
            }
        }
        let target_id = target.id.clone();
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let body = body.to_string();
        let task_id = task_id.map(str::to_string);
        let (steering, event) = tokio::task::spawn_blocking(move || {
            store.create_steering_with_event(&session_id, &target_id, task_id.as_deref(), &body)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(SessionSteerResult { steering })
    }

    pub async fn session_steering(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<SessionSteeringListResult> {
        if let Some(status) = status
            && !matches!(status, "pending" | "delivered" | "cancelled")
        {
            anyhow::bail!("steering status must be pending, delivered, or cancelled");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = status.map(str::to_string);
        let steering =
            tokio::task::spawn_blocking(move || store.list_steering(&id, status.as_deref(), limit))
                .await??;
        Ok(SessionSteeringListResult {
            session_id: session_id.to_string(),
            steering,
        })
    }

    pub async fn cancel_steering(&self, id: &str) -> Result<SessionSteeringCancelResult> {
        let store = self.store.clone();
        let id = id.to_string();
        let (steering, event) =
            tokio::task::spawn_blocking(move || store.cancel_steering_with_event(&id)).await??;
        let _ = self.events.send(event);
        Ok(SessionSteeringCancelResult {
            cancelled: true,
            steering,
        })
    }

    /// Defer an operator prompt until watched plan tasks settle or a deadline
    /// arrives. This is a persisted session-level continuation, not a plan
    /// dependency: it never changes task ordering, approvals, budgets, or
    /// write scopes.
    #[allow(clippy::too_many_arguments)]
    pub async fn defer_session(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        operation_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        self.defer_session_watching_gate(
            session_id,
            body,
            watch_task_ids,
            watch_mode,
            deadline_at,
            after_secs,
            max_attempts,
            every_secs,
            max_runs,
            None,
            operation_id,
        )
        .await
    }

    /// Defer an operator prompt until watched plan tasks settle, a deadline
    /// arrives, or an existing external gate resolves, expires, or is
    /// cancelled. A gate watch is additive; it does not change approvals,
    /// budgets, write scopes, the Verifier, or the merge queue.
    #[allow(clippy::too_many_arguments)]
    pub async fn defer_session_watching_gate(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        watch_gate_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        let mut payload = serde_json::json!({
            "session_id": session_id,
            "body": body,
            "watch_task_ids": &watch_task_ids,
            "watch_mode": watch_mode,
            "deadline_at": deadline_at,
            "after_secs": after_secs,
            "max_attempts": max_attempts,
            "every_secs": every_secs,
            "max_runs": max_runs,
        });
        if let Some(watch_gate_id) = watch_gate_id {
            payload["watch_gate_id"] = serde_json::json!(watch_gate_id);
        }
        let receipt = match crate::operation::begin(
            self.store(),
            session_id,
            operation_id,
            "session.defer",
            &payload,
        )
        .await?
        {
            crate::operation::OperationStart::Replay(result) => {
                return crate::operation::replay(&result);
            }
            crate::operation::OperationStart::Proceed(receipt) => receipt,
        };
        let result = self
            .defer_session_inner(
                session_id,
                body,
                watch_task_ids,
                watch_mode,
                deadline_at,
                after_secs,
                max_attempts,
                every_secs,
                max_runs,
                watch_gate_id,
            )
            .await;
        match &result {
            Ok(value) => {
                if let Err(error) = crate::operation::complete(&receipt, value).await {
                    return Err(error.context(
                        "continuation was created but the session operation receipt could not be completed",
                    ));
                }
            }
            Err(error) => {
                crate::operation::fail(&receipt, &format!("{error:#}")).await;
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn defer_session_inner(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        watch_gate_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        let body = crate::continuation::validate_body(body)?;
        let watch_mode = crate::continuation::normalize_watch_mode(watch_mode)?;
        let recurrence = crate::continuation::normalize_recurrence(every_secs, max_runs)?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if session.status == "landed" {
            anyhow::bail!("landed sessions are final; create a new session or fork to continue");
        }

        let mut seen = std::collections::HashSet::new();
        let mut watch_task_ids = watch_task_ids
            .into_iter()
            .map(|task_id| task_id.trim().to_string())
            .filter(|task_id| !task_id.is_empty())
            .collect::<Vec<_>>();
        watch_task_ids.retain(|task_id| seen.insert(task_id.clone()));
        if watch_task_ids.len() > crate::continuation::MAX_WATCH_TASKS {
            anyhow::bail!(
                "a continuation may watch at most {} tasks",
                crate::continuation::MAX_WATCH_TASKS
            );
        }
        for task_id in &watch_task_ids {
            crate::worktree::validate_task_id(task_id)?;
        }
        let watch_gate_id = watch_gate_id
            .map(str::trim)
            .filter(|gate_id| !gate_id.is_empty())
            .map(str::to_string);
        if let Some(gate_id) = &watch_gate_id {
            if gate_id.chars().count() > 200 || gate_id.chars().any(char::is_control) {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_INVALID,
                    "gate_id is invalid",
                ));
            }
            let store = self.store.clone();
            let lookup = gate_id.clone();
            let record = tokio::task::spawn_blocking(move || store.get_gate(&lookup))
                .await??
                .ok_or_else(|| {
                    crate::gate::gate_error(
                        crate::gate::codes::GATE_NOT_FOUND,
                        format!("gate {gate_id} does not exist"),
                    )
                })?;
            if record.gate.session_id != session_id {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_NOT_FOUND,
                    "the gate does not belong to this session",
                ));
            }
            if !record.gate.is_open() {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_INVALID,
                    format!(
                        "gate {gate_id} is already {}; read its outcome instead of waiting",
                        record.gate.state.as_str()
                    ),
                ));
            }
            let store = self.store.clone();
            let lookup = gate_id.clone();
            if tokio::task::spawn_blocking(move || store.has_open_gate_watch(&lookup)).await?? {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_ALREADY_WATCHED,
                    format!("gate {gate_id} already has an open continuation watch"),
                ));
            }
        }
        let deadline_at = crate::continuation::resolve_deadline(
            after_secs,
            deadline_at,
            time::OffsetDateTime::now_utc(),
        )?;
        if watch_task_ids.is_empty() && deadline_at.is_none() && watch_gate_id.is_none() {
            anyhow::bail!("a continuation needs a watch task, a deadline, or a gate");
        }

        if !watch_task_ids.is_empty() {
            let plan = self.get_team_plan(session_id).await?;
            let Some(plan) = plan else {
                anyhow::bail!("session has no stored TeamPlan to watch");
            };
            let plan_ids = plan
                .tasks
                .iter()
                .map(|task| task.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            if let Some(missing) = watch_task_ids
                .iter()
                .find(|task_id| !plan_ids.contains(task_id.as_str()))
            {
                anyhow::bail!("task {missing} is not in the stored TeamPlan");
            }
            let store = self.store.clone();
            let id = session_id.to_string();
            let tasks = tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&id)))
                .await??;
            for task_id in &watch_task_ids {
                let Some(task) = tasks
                    .iter()
                    .filter(|task| &task.task_id == task_id)
                    .max_by_key(|task| task.plan_seq)
                else {
                    anyhow::bail!("task {task_id} has no scheduling state yet");
                };
                if crate::continuation::is_terminal_task_status(&task.status) {
                    anyhow::bail!(
                        "task {task_id} is already {}; read its outcome instead of watching it",
                        task.status
                    );
                }
            }
        }

        let store = self.store.clone();
        let session_id = session_id.to_string();
        let project_id = session.project_id.clone();
        let max_attempts = crate::continuation::normalize_max_attempts(max_attempts);
        let gate_for_error = watch_gate_id.clone();
        let created = tokio::task::spawn_blocking(move || {
            store.create_session_continuation_with_event(&NewSessionContinuation {
                session_id,
                project_id,
                body,
                watch_task_ids,
                watch_mode,
                watch_gate_id,
                deadline_at,
                max_attempts,
                series_id: None,
                parent_continuation_id: None,
                recurrence_every_secs: recurrence.every_secs,
                recurrence_max_runs: recurrence.max_runs,
                run_count: 0,
            })
        })
        .await?;
        let (continuation, event) = match created {
            Ok(created) => created,
            Err(error) => {
                if let Some(gate_id) = gate_for_error
                    && format!("{error:#}").contains("UNIQUE constraint failed")
                {
                    return Err(crate::gate::gate_error(
                        crate::gate::codes::GATE_ALREADY_WATCHED,
                        format!("gate {gate_id} already has an open continuation watch"),
                    ));
                }
                return Err(error);
            }
        };
        let _ = self.events.send(event);
        Ok(SessionDeferResult { continuation })
    }

    pub async fn session_continuations(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<SessionContinuationsResult> {
        if let Some(status) = status
            && !matches!(
                status,
                "pending" | "ready" | "delivering" | "delivered" | "cancelled" | "failed"
            )
        {
            anyhow::bail!(
                "continuation status must be pending, ready, delivering, delivered, cancelled, or failed"
            );
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = status.map(str::to_string);
        let continuations = tokio::task::spawn_blocking(move || {
            store.list_session_continuations(&id, status.as_deref(), limit)
        })
        .await??;
        Ok(SessionContinuationsResult {
            session_id: session_id.to_string(),
            continuations,
        })
    }

    /// Token-free status for one continuation delivery claim. The scheduler
    /// and operators use it to distinguish a live foreign claim from an
    /// expired row that is safe to reclaim.
    pub async fn session_continuation_status(
        &self,
        id: &str,
    ) -> Result<SessionContinuationStatusResult> {
        let store = self.store.clone();
        let id_owned = id.to_string();
        let continuation =
            tokio::task::spawn_blocking(move || store.get_session_continuation(&id_owned))
                .await??
                .ok_or_else(|| anyhow!("continuation not found: {id}"))?;
        let store = self.store.clone();
        let id_owned = id.to_string();
        let claim =
            tokio::task::spawn_blocking(move || store.session_continuation_claim_status(&id_owned))
                .await??;
        Ok(SessionContinuationStatusResult {
            continuation,
            claim: claim.map(|claim| SessionContinuationClaimInfo {
                status: claim.status,
                owner_id: claim.owner_id,
                expires_at: claim.expires_at,
                delivery_attempt: claim.delivery_attempt,
                live: claim.live,
            }),
        })
    }

    /// Explicitly reclaim an expired delivery claim. This is the same
    /// operation startup recovery and the scheduler use; it cannot reclaim a
    /// live claim or a terminal row.
    pub async fn reclaim_session_continuation(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<SessionContinuationReclaimResult> {
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("claim expired before acknowledgement");
        let reclaimed = self
            .reclaim_expired_session_continuation(id, reason)
            .await?;
        Ok(SessionContinuationReclaimResult {
            reclaimed: reclaimed.is_some(),
            continuation: reclaimed,
        })
    }

    pub async fn cancel_session_continuation(
        &self,
        id: &str,
    ) -> Result<SessionContinuationCancelResult> {
        let store = self.store.clone();
        let continuation_id = id.to_string();
        let reason = "cancelled by operator".to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.cancel_session_continuation_with_event(&continuation_id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            anyhow::bail!("continuation {id} is not pending or ready");
        };
        let _ = self.events.send(event);
        Ok(SessionContinuationCancelResult {
            cancelled: true,
            continuation,
        })
    }

    /// Transition a pending row to ready and broadcast the event. Scheduler
    /// internals use this; replay reconstructs the same row from the payload.
    pub async fn mark_session_continuation_ready(
        &self,
        id: &str,
        reason: &str,
        outcomes: &[ContinuationOutcome],
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let outcomes = outcomes.to_vec();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_session_continuation_ready_with_event(&id, &reason, &outcomes)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Atomically claim a ready row, or reclaim an expired delivery claim,
    /// with a new process owner and token. The scheduler uses this path and
    /// acknowledges with the returned claim. A live foreign claim returns
    /// `None` and is never touched.
    pub async fn claim_session_continuation_for_delivery(
        &self,
        id: &str,
        ttl_secs: u64,
    ) -> Result<Option<SessionContinuationClaim>> {
        let store = self.store.clone();
        let id = id.to_string();
        let owner_id = self.runtime_id.clone();
        let claim = tokio::task::spawn_blocking(move || {
            store.claim_session_continuation_for_delivery_with_event(&id, &owner_id, ttl_secs)
        })
        .await??;
        let Some(claim) = claim else {
            return Ok(None);
        };
        let _ = self.events.send(claim.event.clone());
        Ok(Some(claim))
    }

    /// Backwards-compatible claim used by tests and older internal callers. It
    /// records the token in the Engine's in-memory claim map so the un-fenced
    /// acknowledgement helpers stay safe within this process. New code should
    /// call `claim_session_continuation_for_delivery` and pass the claim.
    pub async fn claim_session_continuation(
        &self,
        id: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self
            .claim_session_continuation_for_delivery(id, 120)
            .await?
        else {
            return Ok(None);
        };
        let continuation = claim.continuation.clone();
        if let Ok(mut claims) = self.continuation_claims.lock() {
            claims.insert(id.to_string(), claim);
        }
        Ok(Some(continuation))
    }

    fn take_continuation_claim(&self, id: &str) -> Option<SessionContinuationClaim> {
        self.continuation_claims
            .lock()
            .ok()
            .and_then(|mut claims| claims.remove(id))
    }

    /// Fenced delivery acknowledgement. A stale owner receives a typed
    /// `SessionContinuationAckError::Lost` and no terminal row is written.
    pub async fn ack_session_continuation_delivered(
        &self,
        claim: &SessionContinuationClaim,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_session_continuation_delivered_with_event(&id, &owner, &token)
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some(delivery) = result else {
            return Ok(None);
        };
        for event in delivery.events {
            let _ = self.events.send(event);
        }
        Ok(Some(delivery.delivered))
    }

    pub async fn ack_session_continuation_retry(
        &self,
        claim: &SessionContinuationClaim,
        error: &str,
        next_attempt_at: &str,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let error = error.to_string();
        let next_attempt_at = next_attempt_at.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.retry_session_continuation_with_event(
                &id,
                &owner,
                &token,
                &error,
                &next_attempt_at,
            )
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    pub async fn ack_session_continuation_failed(
        &self,
        claim: &SessionContinuationClaim,
        error: &str,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let error = error.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_session_continuation_with_event(&id, &owner, &token, &error)
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Idempotent legacy acknowledgement. A repeated terminal write or a stale
    /// in-memory claim returns `Ok(None)` instead of rewriting the row.
    pub async fn mark_session_continuation_delivered(
        &self,
        id: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self.take_continuation_claim(id) else {
            return Ok(None);
        };
        match self.ack_session_continuation_delivered(&claim).await {
            Ok(delivered) => Ok(delivered),
            Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
            Err(error) => Err(anyhow!(error)),
        }
    }

    pub async fn retry_session_continuation(
        &self,
        id: &str,
        error: &str,
        next_attempt_at: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self.take_continuation_claim(id) else {
            return Ok(None);
        };
        match self
            .ack_session_continuation_retry(&claim, error, next_attempt_at)
            .await
        {
            Ok(retried) => Ok(retried),
            Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
            Err(error) => Err(anyhow!(error)),
        }
    }

    pub async fn fail_session_continuation(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<SessionContinuation>> {
        if let Some(claim) = self.take_continuation_claim(id) {
            return match self.ack_session_continuation_failed(&claim, error).await {
                Ok(failed) => Ok(failed),
                Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
                Err(error) => Err(anyhow!(error)),
            };
        }
        self.fail_unclaimed_session_continuation(id, error).await
    }

    /// Fail a pending/ready row, or an expired delivery claim, without a token.
    /// A live foreign claim is left untouched.
    pub async fn fail_unclaimed_session_continuation(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let error = error.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_unclaimed_session_continuation_with_event(&id, &error)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Reclaim one expired claim at startup or from an operator drill. The
    /// delivery operation receipt makes a retry safe when the old process may
    /// already have started a run.
    pub async fn reclaim_expired_session_continuation(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.reclaim_expired_session_continuation_with_event(&id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    pub async fn cancel_session_continuation_with_reason(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.cancel_session_continuation_with_event(&id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Drain pending steering for the current turn. Called by the agent loop
    /// before the next provider request; top-level runs also persist a visible
    /// user message while child runs keep the injected turn in memory and the
    /// event log.
    pub async fn deliver_pending_steering(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        persist_messages: bool,
    ) -> Result<Vec<DeliveredSteering>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let task_id = task_id.map(str::to_string);
        let (delivered, events) = tokio::task::spawn_blocking(move || {
            store.deliver_pending_steering_with_events(
                &session_id,
                &agent_id,
                task_id.as_deref(),
                persist_messages,
            )
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(delivered)
    }

    /// Bounded turn/tool progress observation for a task-scoped run.
    pub async fn touch_task_progress(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        turn: u32,
        tool: Option<&str>,
    ) -> Result<bool> {
        let Some(task_id) = task_id else {
            return Ok(false);
        };
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let tool = tool.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.touch_plan_task_progress(&session_id, &task_id, turn, tool.as_deref())
        })
        .await?
    }

    /// Terminal-run cleanup: cancel anything still pending for this agent.
    pub async fn cancel_pending_steering_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let reason = reason.to_string();
        let events = tokio::task::spawn_blocking(move || {
            store.cancel_pending_steering_for_agent_with_events(&session_id, &agent_id, &reason)
        })
        .await??;
        let count = events.len();
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(count)
    }

    /// Run one stall sweep. Observation is always recorded; when the project
    /// policy opts into `escalate` or `retry`, the same fresh observations may
    /// advance a bounded recovery. Zero disables the sweep and recovery.
    pub async fn sweep_task_stalls(&self, threshold: std::time::Duration) -> Result<usize> {
        self.sweep_task_stalls_at(threshold, time::OffsetDateTime::now_utc())
            .await
    }

    /// Deterministic-time variant used by tests and replay repairs.
    pub async fn sweep_task_stalls_at(
        &self,
        threshold: std::time::Duration,
        now: time::OffsetDateTime,
    ) -> Result<usize> {
        if threshold.is_zero() {
            return Ok(0);
        }
        let store = self.store.clone();
        let (stalls, events) = tokio::task::spawn_blocking(move || {
            store.sweep_task_stalls_with_events_at(threshold, now)
        })
        .await??;
        for event in events {
            self.broadcast_event(event);
        }
        self.evaluate_stall_recoveries(&stalls).await?;
        Ok(stalls.len())
    }

    async fn evaluate_stall_recoveries(&self, stalls: &[TaskStall]) -> Result<()> {
        for stall in stalls {
            let policy = self.orchestration_policy(&stall.project_id).await?;
            let store = self.store.clone();
            let session_id = stall.session_id.clone();
            let plan_seq = stall.plan_seq;
            let task_id = stall.task_id.clone();
            let recovery = tokio::task::spawn_blocking(move || {
                store.get_orchestration_recovery(&session_id, plan_seq, &task_id)
            })
            .await??;
            let decision =
                crate::orchestration::decide_stall_recovery(&policy, recovery.as_ref(), stall);
            match decision {
                crate::orchestration::StallRecoveryDecision::None => {}
                crate::orchestration::StallRecoveryDecision::Escalate => {
                    let reason = format!(
                        "{} produced {} stall observation(s); latest observation was {}s old",
                        stall.task_id, stall.stall_count, stall.age_secs
                    );
                    let session_id = stall.session_id.clone();
                    let task_id = stall.task_id.clone();
                    let store = self.store.clone();
                    let owned_stall = stall.clone();
                    let reason_for_store = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.escalate_stall_recovery(&owned_stall, &reason_for_store)
                    })
                    .await??;
                    let Some((_recovery, event)) = result else {
                        continue;
                    };
                    self.broadcast_event(event);
                    self.record_stall_escalation(&session_id, &task_id, &reason)
                        .await?;
                }
                crate::orchestration::StallRecoveryDecision::Retry => {
                    self.request_stall_recovery_retry(stall, &policy).await?;
                }
                crate::orchestration::StallRecoveryDecision::Exhausted { reason } => {
                    self.record_stall_recovery_failure(stall, &reason).await?;
                }
            }
        }
        Ok(())
    }

    async fn request_stall_recovery_retry(
        &self,
        stall: &TaskStall,
        policy: &OrchestrationPolicy,
    ) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let session = tokio::task::spawn_blocking(move || store.get_session(&session_id))
            .await??
            .ok_or_else(|| anyhow!("session not found: {}", stall.session_id))?;
        if session_budget_exhausted(&session) {
            self.record_stall_recovery_failure(
                stall,
                "session budget is exhausted; automatic stall recovery did not start",
            )
            .await?;
            return Ok(());
        }
        let reason = format!(
            "{} produced {} stall observation(s); latest observation was {}s old",
            stall.task_id, stall.stall_count, stall.age_secs
        );
        let store = self.store.clone();
        let owned = stall.clone();
        let reason_for_store = reason.clone();
        let max_attempts = policy.stall_max_attempts;
        let grace_secs = policy.stall_grace_secs;
        let requested = tokio::task::spawn_blocking(move || {
            store.begin_stall_retry_recovery(&owned, max_attempts, grace_secs, &reason_for_store)
        })
        .await??;
        let Some((_recovery, event)) = requested else {
            return Ok(());
        };
        self.broadcast_event(event);
        match self
            .request_plan_task_cancel(&stall.session_id, &stall.task_id)
            .await
        {
            Ok(_) => {
                self.finish_stall_recovery_cancel(
                    stall,
                    "cancellation requested; waiting for the task and session to settle",
                )
                .await?;
            }
            Err(error) => {
                let store = self.store.clone();
                let session_id = stall.session_id.clone();
                let task_id = stall.task_id.clone();
                let status = tokio::task::spawn_blocking(move || {
                    store.plan_task_status(&session_id, &task_id)
                })
                .await??;
                match status.as_deref() {
                    Some("completed" | "skipped") | None => {
                        self.finish_stall_recovery_unnecessary(
                            stall,
                            "task completed before the cancellation request took effect",
                        )
                        .await?;
                    }
                    Some("cancelled" | "interrupted" | "failed" | "cancelling") => {
                        self.finish_stall_recovery_cancel(
                            stall,
                            "the task ended before cancellation was delivered; waiting for the session to settle",
                        )
                        .await?;
                    }
                    Some(_) => {
                        self.record_stall_recovery_failure(
                            stall,
                            &format!("cooperative cancellation failed: {error:#}"),
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn finish_stall_recovery_cancel(&self, stall: &TaskStall, reason: &str) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let task_id = stall.task_id.clone();
        let plan_seq = stall.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_stall_recovery_waiting_cancel(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
        }
        Ok(())
    }

    async fn finish_stall_recovery_unnecessary(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let task_id = stall.task_id.clone();
        let plan_seq = stall.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.finish_stall_recovery_unnecessary(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
        }
        Ok(())
    }

    pub(crate) async fn record_stall_recovery_failure(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let owned = stall.clone();
        let reason_for_store = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_stall_recovery_for_stall(&owned, &reason_for_store)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
            self.record_stall_escalation(&stall.session_id, &stall.task_id, reason)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn record_stall_escalation(
        &self,
        session_id: &str,
        task_id: &str,
        reason: &str,
    ) -> Result<()> {
        self.record_coordination_note(
            session_id,
            "escalate",
            "critical",
            Some(task_id),
            None,
            None,
            "orchestrator",
            "Orchestrator",
            Some("orchestrator"),
            &format!("Automatic stall recovery escalation for {task_id}: {reason}"),
        )
        .await?;
        Ok(())
    }

    pub async fn orchestration_recoveries(
        &self,
        session_id: &str,
    ) -> Result<OrchestrationRecoveriesResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let recoveries = tokio::task::spawn_blocking(move || {
            store.list_current_orchestration_recoveries(None, Some(&id))
        })
        .await??;
        Ok(OrchestrationRecoveriesResult {
            session_id: session_id.to_string(),
            recoveries,
        })
    }

    pub async fn session_stalls(&self, session_id: &str) -> Result<SessionStallsResult> {
        let threshold_secs = task_stall_threshold_secs();
        let store = self.store.clone();
        let id = session_id.to_string();
        let progress =
            tokio::task::spawn_blocking(move || store.list_plan_task_progress(None, Some(&id)))
                .await??;
        let events = self.recent_events(session_id, 5_000).await?;
        let mut stalls = events
            .into_iter()
            .filter(|event| event.kind == "orchestration.stall")
            .filter_map(|event| serde_json::from_value::<TaskStall>(event.payload).ok())
            .collect::<Vec<_>>();
        stalls.truncate(200);
        Ok(SessionStallsResult {
            session_id: session_id.to_string(),
            threshold_secs,
            progress,
            stalls,
        })
    }

    pub async fn write_leases(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<Vec<WriteLease>> {
        self.scheduler.leases(project_id, session_id).await
    }

    pub async fn retry_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.retry_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn skip_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.skip_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn request_plan_task_cancel(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.request_plan_task_cancel_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn pause_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.pause_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn mark_plan_task_paused(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<ScheduledTask>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_plan_task_paused_with_event(&session_id, &task_id)
        })
        .await??;
        let Some((task, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(Some(task))
    }

    pub async fn resume_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.resume_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn plan_task_status(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<String>> {
        self.scheduler.plan_task_status(session_id, task_id).await
    }

    pub async fn plan_task_cancel_requested(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<bool> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let status =
            tokio::task::spawn_blocking(move || store.plan_task_status(&session_id, &task_id))
                .await??;
        Ok(matches!(
            status.as_deref(),
            Some("cancelling" | "cancelled")
        ))
    }

    pub async fn interrupt_running_plan_tasks(&self, session_id: &str) -> Result<usize> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let tasks = tokio::task::spawn_blocking(move || {
            store.interrupt_session_plan_tasks(&id, "the session ended before the task finished")
        })
        .await??;
        let count = tasks.len();
        for task in tasks {
            self.append_event(
                session_id,
                "plan_task.interrupted",
                serde_json::json!({
                    "task_id": task.task_id,
                    "task_status": "interrupted",
                    "reason": "the session ended before the task finished",
                }),
            )
            .await?;
        }
        Ok(count)
    }

    pub async fn get_team_plan(&self, session_id: &str) -> Result<Option<TeamPlan>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let raw = tokio::task::spawn_blocking(move || store.get_team_plan(&id)).await??;
        Ok(raw.and_then(|json| serde_json::from_str(&json).ok()))
    }

    pub async fn set_todos(&self, session_id: &str, items: Vec<TodoItem>) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let items_for_store = items.clone();
        let event =
            tokio::task::spawn_blocking(move || store.set_todos_with_event(&id, &items_for_store))
                .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn list_todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_todos(&id)).await?
    }

    pub async fn current_plan_revision(&self, session_id: &str) -> Result<u32> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.plan_revision_count(&session_id)).await?
    }
}

struct GovernorCandidateInfo {
    task: vp_protocol::types::PlanTask,
    effective_priority: i64,
    enqueued_at: String,
}

/// Build the same deterministic ready-candidate set used by governor status and
/// preview. The live scheduler additionally applies `next_attempt_at` gating;
/// this read-only view is intentionally simpler and never mutates a task.
fn governor_candidate_infos(
    session: &Session,
    plan: &TeamPlan,
    scheduled: &[ScheduledTask],
    orchestration: &OrchestrationPolicy,
) -> Vec<GovernorCandidateInfo> {
    let by_task = scheduled
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let statuses = scheduled
        .iter()
        .map(|task| (task.task_id.clone(), task.status.clone()))
        .collect::<HashMap<_, _>>();
    crate::orchestration::ready_tasks(plan, &statuses, plan.tasks.len())
        .into_iter()
        .map(|task| {
            let row = by_task.get(task.id.as_str());
            let retry_boost = row
                .filter(|row| {
                    row.attempts > 0 && matches!(row.status.as_str(), "failed" | "pending")
                })
                .map(|_row| i64::from(orchestration.retry_priority_boost))
                .unwrap_or(0);
            let base = session
                .priority
                .saturating_add(task.priority)
                .clamp(-100, 100);
            GovernorCandidateInfo {
                task: task.clone(),
                effective_priority: i64::from(base).saturating_add(retry_boost),
                enqueued_at: row
                    .map(|row| row.enqueued_at.clone())
                    .unwrap_or_else(|| session.created_at.clone()),
            }
        })
        .collect()
}
