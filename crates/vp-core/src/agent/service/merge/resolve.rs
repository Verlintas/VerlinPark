use super::*;

impl AgentService {
    /// Resolve a merge conflict with an internal coding agent, re-verify the
    /// rebased branch, then integrate it. This is an explicit user action: it
    /// skips routine prompts for the resolver and verifier because the user
    /// already chose auto-resolution, but host-level operations are still
    /// denied and the shell sandbox still applies.
    pub async fn resolve_merge(&self, entry_id: &str) -> Result<MergeQueueEntry> {
        self.resolve_merge_with_limit(entry_id, None).await
    }

    pub(in crate::agent) async fn resolve_merge_with_limit(
        &self,
        entry_id: &str,
        max_resolve_tokens: Option<u64>,
    ) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        if entry.status != "conflict" {
            return Err(anyhow!(
                "merge queue entry is {}, only conflict entries can be auto-resolved",
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
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let control = Arc::new(RunControl::new());
        {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if runs.contains_key(&session.id) {
                return Err(anyhow!(
                    "session is running; stop or wait before auto-resolving a merge"
                ));
            }
            runs.insert(session.id.clone(), control.clone());
        }
        self.engine
            .update_session_status(&session.id, "queued")
            .await?;
        let permit = tokio::select! {
            permit = self
                .global_slots
                .acquire(&session.id, &session.project_id, session.priority, &control) => permit,
            _ = wait_for_cancel(&control) => None,
        };
        let Some(_permit) = permit else {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(&session.id);
            }
            let _ = self.engine.update_session_status(&session.id, "idle").await;
            return Err(anyhow!(
                "auto-resolution cancelled while waiting for a global slot"
            ));
        };
        if control.is_cancelled() {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(&session.id);
            }
            let _ = self.engine.update_session_status(&session.id, "idle").await;
            return Err(anyhow!("auto-resolution cancelled"));
        }
        self.engine
            .update_session_status(&session.id, "running")
            .await?;
        let result = self
            .resolve_merge_inner(
                &config,
                &session,
                &project,
                &entry,
                &control,
                max_resolve_tokens,
            )
            .await;
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&session.id);
        }
        let _ = self.engine.update_session_status(&session.id, "idle").await;
        result
    }

    pub(in crate::agent) async fn resolve_merge_inner(
        &self,
        config: &AgentConfig,
        session: &Session,
        project: &Project,
        entry: &MergeQueueEntry,
        control: &RunControl,
        max_resolve_tokens: Option<u64>,
    ) -> Result<MergeQueueEntry> {
        let agents = self.engine.list_agents(&session.id).await?;
        let worker = agents
            .iter()
            .find(|agent| agent.id == entry.agent_id)
            .ok_or_else(|| anyhow!("worker agent not found: {}", entry.agent_id))?;
        let worker_worktree = worker
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

        let clean_worktree = session_worktree.clone();
        let clean = tokio::task::spawn_blocking(move || {
            worktree::git_output(&clean_worktree, &["status", "--porcelain"])
        })
        .await??;
        if !clean.trim().is_empty() {
            anyhow::bail!("session worktree has uncommitted changes; commit or discard them first");
        }
        let clean_worker = worker_worktree.clone();
        let clean = tokio::task::spawn_blocking(move || {
            worktree::git_output(&clean_worker, &["status", "--porcelain"])
        })
        .await??;
        if !clean.trim().is_empty() {
            anyhow::bail!("worker worktree has uncommitted changes; resolve them first");
        }
        let base_worktree = session_worktree.clone();
        let base_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&base_worktree)).await??;

        let rebase_worktree = worker_worktree.clone();
        let rebase_onto = session_branch.clone();
        let rebase = tokio::task::spawn_blocking(move || {
            worktree::rebase_onto_keep_conflicts(&rebase_worktree, &rebase_onto)
        })
        .await?;
        if let Err(error) = rebase {
            let paths_worktree = worker_worktree.clone();
            let paths =
                tokio::task::spawn_blocking(move || worktree::conflicted_paths(&paths_worktree))
                    .await??;
            let in_progress_worktree = worker_worktree.clone();
            let in_progress = tokio::task::spawn_blocking(move || {
                worktree::rebase_in_progress(&in_progress_worktree)
            })
            .await?;
            if paths.is_empty() || !in_progress {
                let detail = truncate(&format!("rebase failed: {error:#}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            let detail = truncate(&format!(
                "rebase conflict in {} path(s): {}",
                paths.len(),
                paths.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
        }

        let paths_worktree = worker_worktree.clone();
        let conflict_paths =
            tokio::task::spawn_blocking(move || worktree::conflicted_paths(&paths_worktree))
                .await??;
        let resolver = self
            .engine
            .create_agent(
                &session.id,
                AgentRole::Worker.as_str(),
                "Conflict Resolver",
                Some("Integration".to_string()),
                worker.parent_id.clone(),
                &model_for(config, AgentRole::Worker),
            )
            .await?;
        self.engine
            .set_agent_task(
                &resolver.id,
                worker.task_id.clone(),
                worker.department.clone(),
            )
            .await?;
        self.engine
            .set_agent_worktree(
                &resolver.id,
                &worker_worktree.to_string_lossy(),
                &entry.branch,
            )
            .await?;
        self.engine
            .set_agent_profile(&resolver.id, AgentProfile::Coding.as_str())
            .await?;
        let resolver_spec = AgentSpec {
            id: resolver.id.clone(),
            role: AgentRole::Worker,
            name: resolver.name.clone(),
            depth: 2,
            parent_name: worker.parent_id.clone(),
            title: Some("Integration".to_string()),
            task_id: entry.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Coding,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: worker_worktree.clone(),
            branch: entry.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "agent.spawned",
            serde_json::json!({
                "parent_id": resolver.parent_id,
                "parent_name": worker.parent_id,
                "objective": "Resolve a merge conflict",
                "conflict_paths": conflict_paths,
            }),
        )
        .await?;
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "integrator.conflict_resolution_started",
            serde_json::json!({
                "entry_id": entry.id,
                "branch": entry.branch,
                "base_commit": base_commit,
                "conflict_paths": conflict_paths,
            }),
        )
        .await?;

        let spent = session.tokens_in + session.tokens_out;
        let limit = match (session.token_budget, max_resolve_tokens) {
            (Some(session_limit), Some(cap)) => Some(session_limit.min(spent.saturating_add(cap))),
            (None, Some(cap)) => Some(spent.saturating_add(cap)),
            (Some(session_limit), None) => Some(session_limit),
            (None, None) => None,
        };
        let budget = SessionBudget::new(
            limit,
            spent,
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
        let prompt = format!(
            "You are resolving a Git rebase conflict for integration queue entry {}.              The worktree is {} on branch {}. The rebase target is session branch {} at commit {}.              Conflicted paths: {}.              Resolve the conflicts in place, preserving the intent of both sides. Do not abort the rebase.              Do not commit manually; leave the resolved files in the worktree. Run relevant tests if possible.              The runtime will stage resolved files, continue the rebase, verify the result, and merge it.              Reply with a concise summary of the resolution.",
            entry.id,
            worker_worktree.to_string_lossy(),
            entry.branch,
            session_branch,
            base_commit,
            conflict_paths.join(", ")
        );
        let mut conversation = vec![ChatMessage::user(prompt.clone())];
        let result = run_loop(
            &self.engine,
            config,
            session,
            project,
            control,
            &budget,
            &resolver_spec,
            &prompt,
            &mut conversation,
            None,
            false,
            None,
        )
        .await;
        let outcome = match result {
            Ok(LoopOutcome::Final(summary)) => summary,
            Ok(LoopOutcome::Deferred(wait)) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some("auto-resolution cannot wait on an external gate".into()),
                    )
                    .await?;
                return Err(anyhow!(
                    "auto-resolution cannot defer to gate {}",
                    wait.gate_id
                ));
            }
            Ok(LoopOutcome::Cancelled) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "cancelled")
                    .await?;
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some("auto-resolution cancelled".into()),
                    )
                    .await?;
                return Err(anyhow!("auto-resolution cancelled"));
            }
            Ok(LoopOutcome::BudgetExceeded(stop)) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(stop.summary()))
                    .await?;
                return Err(anyhow!("{} during auto-resolution", stop.summary()));
            }
            Ok(LoopOutcome::Verdict(_)) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                return Err(anyhow!("resolver returned a verdict unexpectedly"));
            }
            Err(error) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                let detail = truncate(&format!("auto-resolution failed: {error:#}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
        };

        let paths_worktree = worker_worktree.clone();
        let remaining = tokio::task::spawn_blocking(move || {
            if worktree::rebase_in_progress(&paths_worktree) {
                worktree::stage_all(&paths_worktree)?;
            }
            worktree::conflicted_paths(&paths_worktree)
        })
        .await??;
        if !remaining.is_empty() {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!(
                "auto-resolution left {} conflicted path(s): {}",
                remaining.len(),
                remaining.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let marker_worktree = worker_worktree.clone();
        let markers =
            tokio::task::spawn_blocking(move || worktree::conflict_markers(&marker_worktree))
                .await??;
        if !markers.is_empty() {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!(
                "auto-resolution left conflict markers in: {}",
                markers.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let continue_worktree = worker_worktree.clone();
        let continued = tokio::task::spawn_blocking(move || -> Result<()> {
            if worktree::rebase_in_progress(&continue_worktree) {
                worktree::continue_rebase(&continue_worktree)?;
            }
            Ok(())
        })
        .await?;
        if let Err(error) = continued {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!("could not complete rebase: {error:#}"));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let commit_worktree = worker_worktree.clone();
        let commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&commit_worktree)).await??;
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
        self.engine
            .update_agent_status(&resolver.id, "idle")
            .await?;
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "agent.finished",
            serde_json::json!({
                "status": "idle",
                "summary": truncate(&outcome),
                "resolved": true,
                "commit": commit,
                "worktree": worker_worktree,
                "branch": entry.branch,
            }),
        )
        .await?;

        let manager_spec = AgentSpec {
            id: worker
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
            id: worker.id.clone(),
            role: AgentRole::Worker,
            name: worker.name.clone(),
            depth: 2,
            parent_name: Some("Integrator".to_string()),
            title: worker.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Coding,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: worker_worktree.clone(),
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
            config,
            session,
            project,
            control,
            &budget,
            &manager_spec,
            &worker_spec,
            &commit,
            &format!("Resolve integration conflict for queue entry {}", entry.id),
            &outcome,
            Some("No conflict markers; both sides' intent is preserved; relevant tests pass"),
            true,
            &verifier_provenance_for_entry(entry),
        )
        .await?;
        let verdict = match verification {
            VerificationOutcome::Approved(verdict) => verdict,
            VerificationOutcome::Rejected(verdict) => {
                let detail = truncate(&format!(
                    "auto-resolution rejected by Verifier: {}",
                    verdict.summary
                ));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Cancelled => {
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some("auto-resolution verification cancelled".into()),
                    )
                    .await?;
                return Err(anyhow!("auto-resolution verification cancelled"));
            }
            VerificationOutcome::BudgetExceeded(stop) => {
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some(format!("{} during verification", stop.summary())),
                    )
                    .await?;
                return Err(anyhow!("{} during verification", stop.summary()));
            }
            VerificationOutcome::Error(error) => {
                let detail = truncate(&format!("auto-resolution verification failed: {error}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
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
                    "auto-resolved rebase conflict; verifier: {}",
                    truncate(&verdict.summary)
                )),
            )
            .await?
        else {
            return Err(anyhow!("merge queue entry can no longer be resolved"));
        };
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "integrator.conflict_resolution_finished",
            serde_json::json!({
                "entry_id": entry.id,
                "commit": commit,
                "verdict": truncate(&verdict.summary),
            }),
        )
        .await?;
        let integration_spec = AgentSpec {
            auto_approve: true,
            ..resolver_spec
        };
        let conflict_detail = format!(
            "{} path(s): {}; resolver: {}",
            conflict_paths.len(),
            conflict_paths.join(", "),
            outcome.trim()
        );
        let integration = process_merge_entry(
            &self.engine,
            session,
            &integration_spec,
            &entry,
            config.integration_command.as_deref(),
        )
        .await;
        let integration_detail = match &integration {
            Ok(detail) => detail.clone(),
            Err(error) => format!("integration failed: {error:#}"),
        };
        record_integration_resolution_memory(
            &self.engine,
            &session.id,
            &entry.id,
            entry.task_id.as_deref(),
            &commit,
            &verdict.summary,
            &conflict_detail,
            &integration_detail,
        )
        .await;
        integration?;
        let merged = self
            .engine
            .get_merge_queue_entry(&entry.id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry disappeared after auto-resolution"))?;
        if merged.status == "merged"
            && let Some(warning) = self.prune_remote_refs_after_merge(project, &merged).await
        {
            emit(
                &self.engine,
                &session.id,
                &integration_spec,
                "remote.integration.cleanup_failed",
                json!({"entry_id": merged.id, "detail": warning}),
            )
            .await?;
        }
        Ok(merged)
    }
}
