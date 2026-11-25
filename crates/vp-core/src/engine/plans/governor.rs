use super::*;

impl Engine {
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
        // A scheduler cooldown can re-evaluate the same deferred admission.
        // Keep one event while its deterministic inputs and decisions do not
        // change; a real budget/task/policy change produces a new inputs hash.
        if let Some(previous) = self
            .latest_event_of_kind(session_id, crate::budget_governor::DECISION_EVENT_KIND)
            .await?
            && previous.payload["action"] == "admission"
            && previous.payload["inputs_hash"].as_str() == Some(admission.inputs_hash.as_str())
        {
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
