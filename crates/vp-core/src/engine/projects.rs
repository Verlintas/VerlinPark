use super::*;

impl Engine {
    /// Release-facing Git preflight for a project root. This is separate from
    /// `add_project` so store-level tests and fixture builders can still create
    /// synthetic project rows.
    pub async fn preflight_project(&self, root: &str) -> Result<()> {
        let root = PathBuf::from(root);
        tokio::task::spawn_blocking(move || preflight_project_root(&root)).await?
    }

    pub async fn add_project(&self, root: String, name: Option<String>) -> Result<Project> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.add_project(&root, name.as_deref())).await?
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_projects()).await?
    }

    pub async fn get_cache_policy(&self, project_id: &str) -> Result<CachePolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        // One bounded lookup on the hot prompt-construction path. A missing row
        // decodes as the default policy; `set_cache_policy` still validates the
        // project and the values before writing.
        let policy =
            tokio::task::spawn_blocking(move || store.get_cache_policy(&project_id)).await??;
        Ok(policy.normalized().unwrap_or_default())
    }

    pub async fn set_cache_policy(
        &self,
        project_id: &str,
        policy: CachePolicy,
    ) -> Result<CachePolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = policy
            .normalized()
            .map_err(|error| anyhow::anyhow!("invalid cache policy: {error}"))?;
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_cache_policy(&project_id, &policy_for_store))
            .await??;
        Ok(policy)
    }

    pub async fn get_shell_policy(&self, project_id: &str) -> Result<ShellPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        tokio::task::spawn_blocking(move || store.get_shell_policy(&project_id)).await?
    }

    pub async fn get_process_policy(&self, project_id: &str) -> Result<ProcessPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id}"));
            }
            store.get_process_policy(&project_id)
        })
        .await?
    }

    pub async fn set_process_policy(
        &self,
        project_id: &str,
        policy: ProcessPolicy,
    ) -> Result<ProcessPolicy> {
        policy
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid process policy: {error}"))?;
        let store = self.store.clone();
        let project_id_for_store = project_id.to_string();
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_for_store)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_for_store}"));
            }
            store.set_process_policy(&project_id_for_store, &policy_for_store)
        })
        .await??;
        Ok(policy)
    }

    /// Project-scoped hard-preemption policy. This never changes cooperative
    /// pause/cancel and never widens a grace past its hard bound.
    pub async fn get_preemption_policy(&self, project_id: &str) -> Result<PreemptionPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id}"));
            }
            store.get_preemption_policy(&project_id)
        })
        .await?
    }

    pub async fn set_preemption_policy(
        &self,
        project_id: &str,
        policy: PreemptionPolicy,
    ) -> Result<PreemptionPolicy> {
        let policy = policy
            .normalized()
            .map_err(|error| anyhow::anyhow!("invalid preemption policy: {error}"))?;
        let store = self.store.clone();
        let project_id_for_store = project_id.to_string();
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_for_store)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_for_store}"));
            }
            store.set_preemption_policy(&project_id_for_store, &policy_for_store)
        })
        .await??;
        Ok(policy)
    }

    /// Event-derived preemption history. This intentionally reads the session
    /// event stream rather than a mutable projection, so replay, repair, bundle
    /// import, and `db-rebuild` all reproduce the same records.
    pub async fn session_preemption(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<SessionPreemptionResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let policy = self.get_preemption_policy(&session.project_id).await?;
        let events = self
            .recent_events_of_kind(session_id, "session.preempted", 1_000)
            .await?;
        let provider_events = self
            .recent_events_of_kind(session_id, "provider.preempted", 1_000)
            .await?;
        let tool_events = self
            .recent_events_of_kind(session_id, "tool.preempted", 1_000)
            .await?;
        let mut records = Vec::new();
        for event in provider_events.into_iter().chain(tool_events).chain(events) {
            if let Ok(record) = serde_json::from_value::<PreemptionRecord>(event.payload) {
                records.push(record);
            }
        }
        records.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.phase.cmp(&right.phase))
        });
        let cap = limit.clamp(1, 1_000) as usize;
        if records.len() > cap {
            records = records.split_off(records.len() - cap);
        }
        let mut phases: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        let mut reasons: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        let mut terminal = false;
        for record in &records {
            if !record.terminal {
                continue;
            }
            terminal = true;
            *phases.entry(record.phase.clone()).or_default() += 1;
            *reasons.entry(record.reason.clone()).or_default() += 1;
        }
        Ok(SessionPreemptionResult {
            session_id: session_id.to_string(),
            policy,
            active: false,
            preempted: terminal,
            records,
            by_phase: phases
                .into_iter()
                .map(|(key, count)| MetricCount { key, count })
                .collect(),
            by_reason: reasons
                .into_iter()
                .map(|(key, count)| MetricCount { key, count })
                .collect(),
        })
    }

    /// Event-derived project overview for Manage Dash and operational audit.
    pub async fn preemption_overview(
        &self,
        project_id: &str,
        limit: u32,
    ) -> Result<PreemptionOverviewResult> {
        let policy = self.get_preemption_policy(project_id).await?;
        let store = self.store.clone();
        let project_id_string = project_id.to_string();
        let (sessions, by_phase, by_reason) = tokio::task::spawn_blocking(move || {
            store.preemption_overview(&project_id_string, limit)
        })
        .await??;
        let total = sessions
            .iter()
            .map(|summary| summary.terminal_count)
            .sum::<u64>();
        Ok(PreemptionOverviewResult {
            project_id: project_id.to_string(),
            policy,
            total,
            sessions,
            by_phase,
            by_reason,
        })
    }

    /// Read-only language-server availability for a project checkout. This
    /// never spawns a server; it only checks PATH and the in-process registry.
    pub async fn lsp_status(&self, project_id: &str) -> Result<LspStatusResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let project = tokio::task::spawn_blocking({
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let report = tokio::task::spawn_blocking(move || {
            crate::tooling::lsp::status_report(Path::new(&project.root))
        })
        .await?;
        Ok(LspStatusResult {
            project_id,
            root: report.root,
            languages: report
                .languages
                .into_iter()
                .map(|language| LspLanguageStatus {
                    language: language.language,
                    display: language.display,
                    server: language.server,
                    installed: language.installed,
                    running: language.running,
                    capabilities: language.capabilities,
                    install_hint: language.install_hint,
                    idle_secs: language.idle_secs,
                })
                .collect(),
            running: report.running,
            installed: report.installed,
            supported_languages: crate::tooling::lsp::all_languages()
                .iter()
                .map(|language| language.id.to_string())
                .collect(),
        })
    }

    pub async fn get_task_scheduler_policy(
        &self,
        project_id: &str,
    ) -> Result<TaskSchedulerPolicyResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let id = project_id.clone();
        let policy =
            tokio::task::spawn_blocking(move || store.get_task_scheduler_policy(&id)).await??;
        Ok(TaskSchedulerPolicyResult { project_id, policy })
    }

    pub async fn set_task_scheduler_policy(
        &self,
        project_id: &str,
        policy: TaskSchedulerPolicy,
    ) -> Result<TaskSchedulerPolicyResult> {
        if !(1..=86_400).contains(&policy.cooldown_secs) {
            anyhow::bail!("task scheduler cooldown must be between 1 and 86400 seconds");
        }
        if !(1..=10).contains(&policy.max_intake_parallel) {
            anyhow::bail!("max intake parallelism must be between 1 and 10");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let stored = policy.clone();
        let id = project_id.clone();
        tokio::task::spawn_blocking(move || store.set_task_scheduler_policy(&id, &stored))
            .await??;
        Ok(TaskSchedulerPolicyResult { project_id, policy })
    }

    pub async fn get_tool_output_policy(&self, project_id: &str) -> Result<ToolOutputPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let id = project_id.clone();
            move || store.get_project(&id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        tokio::task::spawn_blocking(move || store.get_tool_output_policy(&project_id)).await?
    }

    pub async fn set_tool_output_policy(
        &self,
        project_id: &str,
        policy: ToolOutputPolicy,
    ) -> Result<ToolOutputPolicyResult> {
        crate::tool_output::validate_tool_output_policy(&policy)?;
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let id = project_id.clone();
            move || store.get_project(&id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let stored = policy.clone();
        let id = project_id.clone();
        tokio::task::spawn_blocking(move || store.set_tool_output_policy(&id, &stored)).await??;
        Ok(ToolOutputPolicyResult { project_id, policy })
    }

    pub async fn tool_output_status(
        &self,
        params: ToolOutputStatusParams,
    ) -> Result<ToolOutputStatus> {
        let project_id = params.project_id.clone();
        let session_id = params.session_id.clone();
        let store = self.store.clone();
        let project_for_policy = project_id.clone();
        let session_for_policy = session_id.clone();
        let policy = tokio::task::spawn_blocking(move || match project_for_policy {
            Some(project_id) => store.get_tool_output_policy(&project_id),
            None => match session_for_policy {
                Some(session_id) => match store.get_session(&session_id)? {
                    Some(session) => store.get_tool_output_policy(&session.project_id),
                    None => Ok(ToolOutputPolicy::default()),
                },
                None => Ok(ToolOutputPolicy::default()),
            },
        })
        .await??;
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            output_store.status(project_id.as_deref(), session_id.as_deref(), policy)
        })
        .await?
    }

    pub async fn list_tool_outputs(
        &self,
        params: ToolOutputListParams,
    ) -> Result<ToolOutputListResult> {
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            let limit = params.limit.unwrap_or(100) as usize;
            output_store.list(
                params.project_id.as_deref(),
                params.session_id.as_deref(),
                params.tool_name.as_deref(),
                params.before_created_at.as_deref(),
                params.include_expired,
                limit,
            )
        })
        .await?
    }

    pub async fn prune_tool_outputs(
        &self,
        params: ToolOutputPruneParams,
    ) -> Result<ToolOutputPruneResult> {
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            let policy_project = match params.project_id.as_deref() {
                Some(project_id) => Some(project_id.to_string()),
                None => match params.session_id.as_deref() {
                    Some(session_id) => output_store.session_project_id(session_id)?,
                    None => None,
                },
            };
            let policy = match policy_project.as_deref() {
                Some(project_id) => output_store.policy(project_id)?,
                None => ToolOutputPolicy::default(),
            };
            let older_than_secs = Some(params.older_than_secs.unwrap_or(policy.retention_secs));
            output_store.prune(&vp_store::store::ToolOutputPruneRequest {
                project_id: params.project_id,
                session_id: params.session_id,
                older_than_secs,
                max_entries: params.max_entries.or(Some(policy.max_entries)),
                dry_run: params.dry_run,
            })
        })
        .await?
    }

    pub async fn audit_tool_outputs(
        &self,
        params: ToolOutputAuditParams,
    ) -> Result<ToolOutputAuditResult> {
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            output_store.audit(
                params.project_id.as_deref(),
                params.verify_hashes,
                params.clean_orphans,
            )
        })
        .await?
    }

    /// Daemon retention sweep. It runs per project so each project policy can
    /// protect active sessions and enforce its own entry cap.
    pub async fn sweep_tool_output_retention(&self) -> Result<u64> {
        let projects = self.list_projects().await?;
        let mut removed = 0u64;
        for project in projects {
            let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
            let project_id = project.id.clone();
            let report = tokio::task::spawn_blocking(move || {
                let policy = output_store.policy(&project_id)?;
                output_store.prune(&vp_store::store::ToolOutputPruneRequest {
                    project_id: Some(project_id),
                    session_id: None,
                    older_than_secs: Some(policy.retention_secs),
                    max_entries: Some(policy.max_entries),
                    dry_run: false,
                })
            })
            .await??;
            removed = removed.saturating_add(report.removed);
        }
        // Clean crash-left orphan blobs after metadata pruning. This never
        // removes a referenced body and never touches metadata.
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || output_store.audit(None, false, true)).await??;
        Ok(removed)
    }

    pub async fn set_shell_policy(
        &self,
        project_id: &str,
        policy: ShellPolicy,
    ) -> Result<ShellPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = normalize_shell_policy(policy)?;
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_shell_policy(&project_id, &policy_for_store))
            .await??;
        Ok(policy)
    }

    pub async fn get_integration_policy(&self, project_id: &str) -> Result<IntegrationPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        tokio::task::spawn_blocking(move || store.get_integration_policy(&project_id)).await?
    }

    pub async fn set_integration_policy(
        &self,
        project_id: &str,
        policy: IntegrationPolicy,
    ) -> Result<IntegrationPolicy> {
        if policy.max_auto_attempts > 5 {
            anyhow::bail!("max_auto_attempts must be between 0 and 5");
        }
        if let Some(tokens) = policy.max_resolve_tokens
            && !(1_000..=10_000_000).contains(&tokens)
        {
            anyhow::bail!("max_resolve_tokens must be between 1000 and 10000000");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_integration_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    pub async fn auto_resolve_attempts(&self, entry_id: &str) -> Result<u32> {
        let store = self.store.clone();
        let entry_id = entry_id.to_string();
        tokio::task::spawn_blocking(move || store.auto_resolve_attempts(&entry_id)).await?
    }

    pub async fn bump_auto_resolve_attempts(&self, entry_id: &str) -> Result<u32> {
        let store = self.store.clone();
        let entry_id = entry_id.to_string();
        tokio::task::spawn_blocking(move || store.bump_auto_resolve_attempts(&entry_id)).await?
    }

    pub async fn list_model_presets(&self) -> Result<ModelPresetListResult> {
        let store = self.store.clone();
        let presets = tokio::task::spawn_blocking(move || store.list_model_presets()).await??;
        Ok(ModelPresetListResult { presets })
    }

    pub async fn create_model_preset(
        &self,
        params: CreateModelPresetParams,
    ) -> Result<ModelPreset> {
        validate_model_preset(
            &params.name,
            &params.provider,
            &params.model,
            params.context_window,
            params.input_usd_per_million,
            params.output_usd_per_million,
            &params.notes,
        )?;
        let preset = ModelPreset {
            id: Uuid::now_v7().to_string(),
            name: params.name.trim().to_string(),
            provider: params.provider.trim().to_string(),
            model: params.model.trim().to_string(),
            context_window: params.context_window,
            input_usd_per_million: params.input_usd_per_million,
            output_usd_per_million: params.output_usd_per_million,
            notes: params.notes.trim().to_string(),
            created_at: vp_protocol::now_rfc3339(),
        };
        let store = self.store.clone();
        let saved = preset.clone();
        tokio::task::spawn_blocking(move || store.create_model_preset(&saved)).await??;
        Ok(preset)
    }

    pub async fn update_model_preset(
        &self,
        params: UpdateModelPresetParams,
    ) -> Result<ModelPreset> {
        validate_model_preset(
            &params.name,
            &params.provider,
            &params.model,
            params.context_window,
            params.input_usd_per_million,
            params.output_usd_per_million,
            &params.notes,
        )?;
        let store = self.store.clone();
        let id = params.id.clone();
        let existing = tokio::task::spawn_blocking(move || store.get_model_preset(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("model preset not found: {}", params.id))?;
        let preset = ModelPreset {
            id: existing.id,
            name: params.name.trim().to_string(),
            provider: params.provider.trim().to_string(),
            model: params.model.trim().to_string(),
            context_window: params.context_window,
            input_usd_per_million: params.input_usd_per_million,
            output_usd_per_million: params.output_usd_per_million,
            notes: params.notes.trim().to_string(),
            created_at: existing.created_at,
        };
        let store = self.store.clone();
        let saved = preset.clone();
        tokio::task::spawn_blocking(move || store.update_model_preset(&saved)).await??;
        Ok(preset)
    }

    pub async fn delete_model_preset(&self, id: &str) -> Result<bool> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.delete_model_preset(&id)).await?
    }

    pub async fn get_project_budget(&self, project_id: &str) -> Result<ProjectBudgetResult> {
        let store = self.store.clone();
        let id = project_id.to_string();
        let (budget, usage) = tokio::task::spawn_blocking(move || {
            let budget = store.get_project_budget(&id)?;
            let usage = store.project_usage(&id)?;
            Ok::<_, anyhow::Error>((budget, usage))
        })
        .await??;
        Ok(ProjectBudgetResult {
            project_id: project_id.to_string(),
            budget,
            spent_tokens: usage.0,
            spent_cost_usd: usage.1,
        })
    }

    pub async fn set_project_budget(
        &self,
        project_id: &str,
        budget: ProjectBudget,
    ) -> Result<ProjectBudgetResult> {
        if budget.token_budget.is_some_and(|value| value == 0) {
            anyhow::bail!("project token budget must be greater than zero");
        }
        if budget
            .cost_budget_usd
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            anyhow::bail!("project cost budget must be a positive finite number");
        }
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id_owned.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = budget.clone();
        tokio::task::spawn_blocking(move || {
            store.set_project_budget(&project_id_owned, &for_store)
        })
        .await??;
        self.budget_registry.clear_project(project_id);
        self.get_project_budget(project_id).await
    }

    /// Project-level Run Contract defaults. A missing row reports the derived
    /// balanced defaults with no `updated_at`, so callers can distinguish it
    /// from an explicit project policy.
    pub async fn get_project_run_policy(&self, project_id: &str) -> Result<ProjectRunPolicyResult> {
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let (policy, updated_at) = tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_owned)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_owned}"));
            }
            store.get_project_run_policy_record(&project_id_owned)
        })
        .await??;
        Ok(ProjectRunPolicyResult {
            project_id: project_id.to_string(),
            policy,
            updated_at,
        })
    }

    /// Persist a validated project Run Contract policy. This only changes
    /// effective defaults; it cannot widen approvals, sandbox, contracts,
    /// budgets, capacity, Verifier/Integrator gates, or model routing.
    pub async fn set_project_run_policy(
        &self,
        params: SetProjectRunPolicyParams,
    ) -> Result<ProjectRunPolicyResult> {
        params.policy.validate()?;
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let policy = params.policy.clone();
        let updated_at =
            tokio::task::spawn_blocking(move || store.set_project_run_policy(&project_id, &policy))
                .await??;
        self.record_project_manifest_best_effort().await;
        Ok(ProjectRunPolicyResult {
            project_id: params.project_id,
            policy: params.policy,
            updated_at: Some(updated_at),
        })
    }

    pub async fn record_project_manifest(&self) -> Result<()> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.record_project_manifest()).await?
    }

    pub async fn project_usage_report(&self, project_id: &str) -> Result<ProjectUsageResult> {
        let store = self.store.clone();
        let id = project_id.to_string();
        let sessions = tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.project_usage_by_session(&id)
        })
        .await??;
        let mut totals = UsageTotals::default();
        for session in &sessions {
            totals.calls = totals.calls.saturating_add(session.calls);
            totals.input_tokens = totals.input_tokens.saturating_add(session.input_tokens);
            totals.output_tokens = totals.output_tokens.saturating_add(session.output_tokens);
            totals.cache_read_tokens = totals
                .cache_read_tokens
                .saturating_add(session.cache_read_tokens);
            totals.cache_write_tokens = totals
                .cache_write_tokens
                .saturating_add(session.cache_write_tokens);
            totals.cost_usd += session.cost_usd;
        }
        totals.cache_hit_ratio = vp_protocol::types::cache_hit_ratio(
            totals.input_tokens,
            totals.cache_read_tokens,
            totals.cache_write_tokens,
        );
        Ok(ProjectUsageResult {
            project_id: project_id.to_string(),
            sessions,
            totals,
        })
    }

    pub async fn project_usage(&self, project_id: &str) -> Result<(u64, f64)> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.project_usage(&id)).await?
    }
}

fn normalize_shell_policy(mut policy: ShellPolicy) -> Result<ShellPolicy> {
    let network = policy.network.trim().to_ascii_lowercase();
    if network != "allow" && network != "deny" {
        anyhow::bail!("shell policy network must be 'allow' or 'deny'");
    }
    if policy.read_deny.len() > 64 {
        anyhow::bail!("shell policy supports at most 64 read-deny paths");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut normalized = Vec::new();
    for raw in policy.read_deny {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        if raw.len() > 4096 {
            anyhow::bail!("shell read-deny path is too long");
        }
        let path = if raw == "~" {
            home.clone()
                .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot expand ~"))?
        } else if let Some(rest) = raw.strip_prefix("~/") {
            home.clone()
                .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot expand ~"))?
                .join(rest)
        } else if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            anyhow::bail!("shell read-deny paths must be absolute or start with ~/: {raw}");
        };
        let path = path.canonicalize().unwrap_or(path);
        let path = path.to_string_lossy().to_string();
        if !normalized.contains(&path) {
            normalized.push(path);
        }
    }
    policy.network = network;
    policy.read_deny = normalized;
    Ok(policy)
}

pub(crate) fn preflight_project_root(root: &Path) -> Result<()> {
    if !root.is_dir() {
        bail!("project root is not a directory: {}", root.display());
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving {}", root.display()))?;
    let git = |args: &[&str]| -> Result<std::process::Output> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .with_context(|| "Git is required for a VerlinPark project; install Git or the Xcode command line tools")?;
        Ok(output)
    };
    let inside = git(&["rev-parse", "--show-toplevel"])?;
    if !inside.status.success() {
        bail!(
            "project root must be a Git repository; run `git init` and create an initial commit first"
        );
    }
    let top = String::from_utf8_lossy(&inside.stdout).trim().to_string();
    let top = PathBuf::from(top)
        .canonicalize()
        .with_context(|| "resolving the Git repository root")?;
    if top != root {
        bail!(
            "project root must be the repository root, not a subdirectory: {}",
            top.display()
        );
    }
    let head = git(&["rev-parse", "--verify", "HEAD"])?;
    if !head.status.success() {
        bail!("repository has no commits; create an initial commit before adding it");
    }
    for (key, value) in [
        ("user.name", "Git user.name"),
        ("user.email", "Git user.email"),
    ] {
        let output = git(&["config", "--get", key])?;
        if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim().is_empty() {
            bail!("{value} is not configured; run `git config {key} \"...\"`");
        }
    }
    Ok(())
}

#[cfg(test)]
mod project_preflight_tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "initial"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        dir
    }

    #[test]
    fn accepts_a_non_empty_git_repository() {
        let dir = repo();
        preflight_project_root(dir.path()).unwrap();
    }

    #[test]
    fn rejects_a_non_git_directory() {
        let dir = TempDir::new().unwrap();
        let error = preflight_project_root(dir.path()).unwrap_err().to_string();
        assert!(error.contains("Git repository"));
    }

    #[test]
    fn rejects_a_git_subdirectory() {
        let dir = repo();
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let error = preflight_project_root(&nested).unwrap_err().to_string();
        assert!(error.contains("repository root"));
    }

    #[test]
    fn rejects_a_repository_without_commits() {
        let dir = TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        let error = preflight_project_root(dir.path()).unwrap_err().to_string();
        assert!(error.contains("no commits"));
    }
}
