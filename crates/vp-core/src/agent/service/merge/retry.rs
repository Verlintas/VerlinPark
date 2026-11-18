use super::*;

impl AgentService {
    pub async fn retry_merge(&self, entry_id: &str) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        let (session, _) =
            load_session_project(self.engine.store().clone(), &entry.session_id).await?;
        crate::ensure_session_quiescent(&session, "retry a merge")?;
        if session.status == "landed" {
            anyhow::bail!("session is already landed");
        }
        self.begin_operation(&entry.session_id)?;
        let result = self.retry_merge_inner(entry_id).await;
        self.end_operation(&entry.session_id);
        result
    }

    pub(in crate::agent) async fn retry_merge_inner(
        &self,
        entry_id: &str,
    ) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        if !matches!(entry.status.as_str(), "conflict" | "failed" | "interrupted") {
            return Err(anyhow!(
                "merge queue entry is {}, only conflict/failed/interrupted can be retried",
                entry.status
            ));
        }
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), &entry.session_id).await?;
        let agents = self.engine.list_agents(&entry.session_id).await?;
        let agent = agents
            .iter()
            .find(|agent| agent.id == entry.agent_id)
            .ok_or_else(|| anyhow!("worker agent not found: {}", entry.agent_id))?;
        let agent_worktree = agent
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("worker has no worktree: {}", entry.agent_id))?;
        let session_worktree = session
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("session has no worktree"))?;
        let session_branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow!("session has no branch"))?;

        let rebase_worktree = agent_worktree.clone();
        let rebase_onto = session_branch.clone();
        let rebase = tokio::task::spawn_blocking(move || {
            worktree::rebase_onto(&rebase_worktree, &rebase_onto)
        })
        .await?;
        if let Err(error) = rebase {
            let detail = truncate(&format!("rebase failed: {error:#}"));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let commit_worktree = agent_worktree.clone();
        let commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&commit_worktree)).await??;
        let base_worktree = session_worktree.clone();
        let base_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&base_worktree)).await??;
        if entry.source == MERGE_SOURCE_REMOTE
            && let Some(reference) = entry.imported_ref.clone()
        {
            let project_root = PathBuf::from(&project.root);
            let commit_for_ref = commit.clone();
            tokio::task::spawn_blocking(move || {
                crate::remote_integration::update_import_ref(
                    &project_root,
                    &reference,
                    &commit_for_ref,
                )
            })
            .await??;
        }

        let profile = agent
            .profile
            .as_deref()
            .and_then(AgentProfile::parse)
            .unwrap_or(AgentProfile::Coding);
        let plan = self.engine.get_team_plan(&session.id).await.ok().flatten();
        let task = entry.task_id.as_deref().and_then(|task_id| {
            plan.as_ref()
                .and_then(|plan| plan.tasks.iter().find(|task| task.id == task_id))
        });
        let objective = task
            .map(|task| task.title.clone())
            .unwrap_or_else(|| format!("Retry integration for queue entry {}", entry.id));
        let acceptance = task.map(|task| task.acceptance.join("; "));
        let control = Arc::new(RunControl::new_operation());
        let budget = SessionBudget::new(
            session.token_budget,
            session.tokens_in + session.tokens_out,
            AgentCaps {
                managers: config.max_managers,
                workers: config.max_workers,
                labour: config.max_labour,
            },
        )
        .with_cost(session.cost_budget_usd, session.cost_spent_usd)
        .with_time(session.time_budget_secs)
        .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited))
        .with_scaling_slots(self.engine.scaling_slots(
            &session.id,
            &session.project_id,
            config.max_workers,
            config.max_labour,
        ));
        let manager_spec = AgentSpec {
            id: agent
                .parent_id
                .clone()
                .unwrap_or_else(|| "integrator".to_string()),
            role: AgentRole::Manager,
            name: "Integrator".to_string(),
            depth: 1,
            parent_name: None,
            title: Some("Integration".to_string()),
            task_id: None,
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Generic,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: session_worktree.clone(),
            branch: session_branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let worker_spec = AgentSpec {
            id: agent.id.clone(),
            role: AgentRole::Worker,
            name: agent.name.clone(),
            depth: 2,
            parent_name: Some("Integrator".to_string()),
            title: agent.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: agent_worktree.clone(),
            branch: entry.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let verification = verify_worker(
            &self.engine,
            &config,
            &session,
            &project,
            &control,
            &budget,
            &manager_spec,
            &worker_spec,
            &commit,
            &objective,
            "Rebased onto the current session branch after a previous integration attempt.",
            acceptance.as_deref(),
            true,
            &verifier_provenance_for_entry(&entry),
        )
        .await?;
        let verifier_summary = match verification {
            VerificationOutcome::Approved(verdict) => verdict.summary,
            VerificationOutcome::Rejected(verdict) => {
                let detail = truncate(&format!("retry rejected by Verifier: {}", verdict.summary));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Cancelled => {
                let detail = "retry verification cancelled".to_string();
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::BudgetExceeded(stop) => {
                let detail = truncate(&format!("retry verification stopped: {}", stop.summary()));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Error(error) => {
                let detail = truncate(&format!("retry verification failed: {error}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
        };
        let Some(entry) = self
            .engine
            .retry_merge_queue_entry(
                &entry.id,
                &commit,
                &base_commit,
                Some(format!(
                    "rebased onto the current session branch; verified: {verifier_summary}"
                )),
            )
            .await?
        else {
            return Err(anyhow!("merge queue entry can no longer be retried"));
        };

        let spec = AgentSpec {
            id: agent.id.clone(),
            role: AgentRole::Worker,
            name: agent.name.clone(),
            depth: 2,
            parent_name: None,
            title: agent.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile,
            allow_delegation: true,
            task_kind: None,
            auto_approve: false,
            worktree: agent_worktree,
            branch: entry.branch.clone(),
            session_worktree,
            session_branch,
            write_scope: Vec::new(),
            base_commit: entry.base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        process_merge_entry(
            &self.engine,
            &session,
            &spec,
            &entry,
            config.integration_command.as_deref(),
        )
        .await?;
        let merged = self
            .engine
            .get_merge_queue_entry(&entry.id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry disappeared after retry"))?;
        if merged.status == "merged"
            && let Some(warning) = self.prune_remote_refs_after_merge(&project, &merged).await
        {
            emit(
                &self.engine,
                &session.id,
                &spec,
                "remote.integration.cleanup_failed",
                json!({"entry_id": merged.id, "detail": warning}),
            )
            .await?;
        }
        Ok(merged)
    }
}
