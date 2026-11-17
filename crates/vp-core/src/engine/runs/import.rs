use super::*;

impl Engine {
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
}
