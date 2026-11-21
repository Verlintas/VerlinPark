use super::*;

impl Engine {
    pub async fn create_session(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
    ) -> Result<Session> {
        self.create_session_with_limits(project_id, goal, token_budget, None, None)
            .await
    }

    pub async fn create_session_with_limits(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
    ) -> Result<Session> {
        self.create_session_full(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            0,
            false,
            None,
            None,
        )
        .await
    }

    pub async fn create_session_with_priority(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        priority: i32,
    ) -> Result<Session> {
        self.create_session_full(
            project_id,
            goal,
            token_budget,
            None,
            None,
            priority,
            false,
            None,
            None,
        )
        .await
    }

    /// Create a session with every admission and budget option. This is the
    /// full option path used by the RPC layer; the narrower helpers above are
    /// convenience wrappers for tests and older call sites.
    pub async fn create_session_with_options(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
    ) -> Result<Session> {
        self.create_session_full(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            priority,
            false,
            None,
            None,
        )
        .await
    }

    /// Full session creation path that can also select unlimited budget mode.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_session_with_options_and_unlimited(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
        unlimited: bool,
    ) -> Result<Session> {
        self.create_session_full(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            priority,
            unlimited,
            None,
            None,
        )
        .await
    }

    /// Create a session with an explicit budget mode and optional session-level
    /// auto policy. Auto creation fails closed unless the resolved policy
    /// declares at least one hard cap; there is no silent unlimited fallback.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_session_with_budget_options(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        budget_mode: Option<BudgetMode>,
        auto_budget: Option<AutoBudgetPolicy>,
        priority: i32,
        unlimited: bool,
    ) -> Result<Session> {
        let budget_mode = budget_mode.or_else(|| auto_budget.as_ref().map(|_| BudgetMode::Auto));
        self.create_session_full(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            priority,
            unlimited,
            budget_mode,
            auto_budget,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session_full(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
        unlimited: bool,
        budget_mode: Option<BudgetMode>,
        auto_budget: Option<AutoBudgetPolicy>,
    ) -> Result<Session> {
        if !(-100..=100).contains(&priority) {
            anyhow::bail!("session priority must be between -100 and 100");
        }
        let mode = budget_mode.unwrap_or(if unlimited {
            BudgetMode::Unlimited
        } else {
            BudgetMode::Fixed
        });
        let unlimited = unlimited || mode == BudgetMode::Unlimited;
        if unlimited && mode != BudgetMode::Unlimited {
            return Err(BudgetValidationError::new(
                auto_budget_codes::BUDGET_MODE_CONFLICT,
                "an unlimited session cannot also select a fixed or auto budget mode",
            )
            .into());
        }
        if matches!(mode, BudgetMode::Fixed | BudgetMode::Unlimited) && auto_budget.is_some() {
            return Err(BudgetValidationError::new(
                auto_budget_codes::BUDGET_POLICY_NOT_APPLICABLE,
                "a session auto-budget policy requires budget mode auto",
            )
            .into());
        }
        if mode == BudgetMode::Unlimited
            && (token_budget.is_some() || cost_budget_usd.is_some() || time_budget_secs.is_some())
        {
            return Err(BudgetValidationError::new(
                auto_budget_codes::BUDGET_MODE_CONFLICT,
                "an unlimited session cannot also declare token, cost, or time budgets",
            )
            .into());
        }
        if token_budget == Some(0) {
            return Err(BudgetValidationError::new(
                auto_budget_codes::INVALID_BUDGET,
                "token budget must be greater than zero",
            )
            .into());
        }
        if cost_budget_usd.is_some_and(|value| !value.is_finite() || value <= 0.0) {
            return Err(BudgetValidationError::new(
                auto_budget_codes::INVALID_BUDGET,
                "cost budget must be a positive finite number",
            )
            .into());
        }
        if time_budget_secs == Some(0) {
            return Err(BudgetValidationError::new(
                auto_budget_codes::INVALID_BUDGET,
                "time budget must be greater than zero",
            )
            .into());
        }
        let store = self.store.clone();
        let project_id_for_load = project_id.clone();
        let (project, project_auto_policy) = tokio::task::spawn_blocking(move || {
            let project = store
                .get_project(&project_id_for_load)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {project_id_for_load}"))?;
            let policy = store.get_project_auto_budget_policy(&project.id)?;
            Ok::<_, anyhow::Error>((project, policy))
        })
        .await??;

        let mut token_budget = token_budget;
        let mut cost_budget_usd = cost_budget_usd;
        let mut time_budget_secs = time_budget_secs;
        let mut auto_budget_record = None;
        let session_id = Uuid::now_v7().to_string();
        if mode == BudgetMode::Auto {
            let policy = match auto_budget {
                Some(policy) => policy,
                None => {
                    if !project_auto_policy.enabled {
                        return Err(BudgetValidationError::new(
                            auto_budget_codes::AUTO_BUDGET_POLICY_REQUIRED,
                            "auto budget requires an enabled project policy or an explicit session policy",
                        )
                        .into());
                    }
                    project_auto_policy.clone()
                }
            };
            policy.validate()?;
            policy.require_hard_cap()?;
            let caps = policy.hard_caps();
            token_budget = token_budget.or(policy.resolved_initial_token_budget()).or(caps.token);
            cost_budget_usd = cost_budget_usd
                .or(policy.resolved_initial_cost_budget_usd())
                .or(caps.cost_usd);
            time_budget_secs = time_budget_secs
                .or(policy.resolved_initial_wall_clock_secs())
                .or(caps.wall_clock_secs);
            if let (Some(value), Some(cap)) = (token_budget, caps.token) {
                token_budget = Some(value.min(cap));
            }
            if let (Some(value), Some(cap)) = (cost_budget_usd, caps.cost_usd) {
                cost_budget_usd = Some(value.min(cap));
            }
            if let (Some(value), Some(cap)) = (time_budget_secs, caps.wall_clock_secs) {
                time_budget_secs = Some(value.min(cap));
            }
            let now = vp_protocol::now_rfc3339();
            auto_budget_record = Some(SessionAutoBudgetRecord {
                session_id: session_id.clone(),
                mode: BudgetMode::Auto,
                policy,
                state: SessionAutoBudgetState::default(),
                caps,
                created_at: now.clone(),
                updated_at: now,
            });
        }
        let session_id_for_worktree = session_id.clone();
        let project_id_for_worktree = project.id.clone();
        let root = PathBuf::from(&project.root);
        let data_dir = self.store.data_dir().to_path_buf();
        let git_lock = self.git_lock.clone();
        let worktree = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::create(
                &root,
                &data_dir,
                &project_id_for_worktree,
                &session_id_for_worktree,
            )
        })
        .await??;

        let store = self.store.clone();
        let worktree_path = worktree.path.to_string_lossy().to_string();
        let branch = worktree.branch.clone();
        let base_commit = worktree.base_commit.clone();
        let session = tokio::task::spawn_blocking({
            let session_id = session_id.clone();
            let project_id = project.id.clone();
            let goal = goal.clone();
            let worktree_path = worktree_path.clone();
            let branch = branch.clone();
            let base_commit = base_commit.clone();
            let auto_budget_record = auto_budget_record.clone();
            move || {
                // The session row, its `session.created` event, and the auto
                // budget row commit in one transaction; `db-rebuild` therefore
                // never skips the session or its hard caps while an orphan
                // worktree remains on disk.
                store.create_session_with_worktree_and_event_budget(
                    &session_id,
                    &project_id,
                    &goal,
                    &worktree_path,
                    &branch,
                    token_budget,
                    cost_budget_usd,
                    time_budget_secs,
                    unlimited,
                    &base_commit,
                    priority,
                    mode,
                    auto_budget_record.as_ref(),
                    None,
                )
            }
        })
        .await;

        let (session, event) = match session {
            Ok(Ok(created)) => created,
            Ok(Err(error)) => {
                worktree::remove(
                    &PathBuf::from(&project.root),
                    &worktree.path,
                    &worktree.branch,
                );
                return Err(error);
            }
            Err(error) => {
                worktree::remove(
                    &PathBuf::from(&project.root),
                    &worktree.path,
                    &worktree.branch,
                );
                return Err(error.into());
            }
        };
        // The store committed the event with the row; subscribers still need
        // the same broadcast that `append_event` used to provide.
        self.broadcast_event(event);

        Ok(session)
    }

    /// Snapshot the active conversation and committed session branch. The
    /// worktree must be clean so the checkpoint commit is reproducible.
    /// Snapshot a preempted Worker/Labour task worktree under a daemon-owned
    /// ref. This preserves incomplete work for inspection and an explicit
    /// retry; it never merges, commits to the task branch, or enters the merge
    /// queue.
    pub(crate) async fn snapshot_preempted_task_worktree(
        &self,
        session_id: &str,
        task_id: &str,
        worktree_path: &Path,
        label: &str,
    ) -> Result<String> {
        let data_dir = self.store.data_dir().to_path_buf();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let worktree_path = worktree_path.to_path_buf();
        let checkpoint_id = format!("preempt-{}", Uuid::now_v7());
        let label = format!(
            "{} [task {}]",
            label.chars().take(80).collect::<String>(),
            task_id.chars().take(80).collect::<String>()
        );
        let git_lock = self.git_lock.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            if !worktree_path.is_dir() {
                anyhow::bail!("task worktree is missing: {}", worktree_path.display());
            }
            worktree::snapshot_worktree(
                &worktree_path,
                &data_dir,
                &session_id,
                &checkpoint_id,
                &label,
            )
        })
        .await?
    }

    pub async fn ensure_session_worktree(&self, session_id: &str) -> Result<Session> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found"))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;

        if let Some(path) = &session.worktree
            && Path::new(path).join(".git").is_file()
        {
            return Ok(session);
        }

        let data_dir = self.store.data_dir().to_path_buf();
        let root = PathBuf::from(&project.root);
        let project_id = project.id.clone();
        let session_id = session.id.clone();
        let git_lock = self.git_lock.clone();
        let info = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::create(&root, &data_dir, &project_id, &session_id)
        })
        .await??;

        let worktree_path = info.path.to_string_lossy().to_string();
        let store = self.store.clone();
        let id = session.id.clone();
        let branch = info.branch.clone();
        let worktree_for_store = worktree_path.clone();
        tokio::task::spawn_blocking(move || {
            store.set_session_worktree(&id, &worktree_for_store, &branch)
        })
        .await??;

        let session = Session {
            worktree: Some(worktree_path),
            branch: Some(info.branch),
            ..session
        };
        self.append_event(
            &session.id,
            "session.worktree",
            serde_json::json!({
                "worktree": session.worktree,
                "branch": session.branch,
            }),
        )
        .await?;
        Ok(session)
    }

    pub async fn list_sessions(&self, project_id: Option<String>) -> Result<Vec<Session>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_sessions(project_id.as_deref())).await?
    }

    pub async fn session_diff(&self, session_id: &str) -> Result<SessionDiff> {
        let store = self.store.clone();
        let id = session_id.to_string();
        if let Some(session) = tokio::task::spawn_blocking(move || store.get_session(&id)).await??
            && session.status == "imported"
        {
            return Ok(SessionDiff {
                session_id: session.id,
                branch: session.branch.unwrap_or_default(),
                worktree: session.worktree.unwrap_or_default(),
                base_commit: session.base_commit,
                status: "imported archive · worktree not restored".to_string(),
                patch: String::new(),
            });
        }
        let session = self.ensure_session_worktree(session_id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let branch = session.branch.clone().unwrap_or_default();
        let base_commit = session.base_commit.clone();
        let session_id = session.id.clone();
        let diff_base = base_commit.clone().unwrap_or_else(|| "HEAD".to_string());
        let worktree_for_git = PathBuf::from(&worktree);
        let (status, patch) = tokio::task::spawn_blocking(move || {
            let status = git_output(&worktree_for_git, &["status", "--short"])?;
            let stat = git_output(&worktree_for_git, &["diff", "--stat", &diff_base])?;
            let diff = git_output(&worktree_for_git, &["diff", "--no-color", &diff_base])?;
            let mut patch = String::new();
            if !stat.is_empty() {
                patch.push_str(&stat);
                patch.push('\n');
            }
            if !diff.is_empty() {
                patch.push_str(&diff);
            }
            const MAX_DIFF: usize = 200_000;
            if patch.len() > MAX_DIFF {
                patch.truncate(MAX_DIFF);
                patch.push_str("\n… diff truncated");
            }
            Ok::<_, anyhow::Error>((status, patch))
        })
        .await??;
        Ok(SessionDiff {
            session_id,
            branch,
            worktree,
            base_commit,
            status,
            patch,
        })
    }

    pub async fn session_transcript(&self, session_id: &str) -> Result<SessionTranscriptResult> {
        use std::fmt::Write as _;

        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project, plan, messages) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let session = store
                    .get_session(&id)?
                    .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
                let project = store
                    .get_project(&session.project_id)?
                    .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
                let plan = store
                    .get_team_plan(&id)?
                    .and_then(|raw| serde_json::from_str::<TeamPlan>(&raw).ok());
                let messages = store.list_messages(&id)?;
                Ok((session, project, plan, messages))
            })
            .await??;

        let mut out = String::new();
        let _ = writeln!(out, "# {}\n", session.goal);
        let _ = writeln!(out, "- **Session:** `{}`", session.id);
        let _ = writeln!(out, "- **Project:** {} (`{}`)", project.name, project.root);
        let _ = writeln!(out, "- **Status:** {}", session.status);
        let _ = writeln!(out, "- **Created:** {}", session.created_at);
        if let Some(branch) = &session.branch {
            let _ = writeln!(out, "- **Branch:** `{branch}`");
        }
        if let Some(worktree) = &session.worktree {
            let _ = writeln!(out, "- **Worktree:** `{worktree}`");
        }
        let _ = writeln!(
            out,
            "- **Tokens:** {} in / {} out",
            session.tokens_in, session.tokens_out
        );
        let _ = writeln!(out, "- **Cost:** ${:.4}", session.cost_spent_usd);
        if session.unlimited {
            let _ = writeln!(out, "- **Budget:** unlimited (economic gates bypassed)");
        }

        if let Some(plan) = &plan {
            let _ = writeln!(out, "\n## Team Plan\n");
            for department in &plan.departments {
                let _ = writeln!(out, "### {} — {}", department.name, department.objective);
                for task in plan
                    .tasks
                    .iter()
                    .filter(|task| task.department == department.name)
                {
                    let mut details = vec![task.kind.clone()];
                    if let Some(profile) = &task.profile {
                        details.push(profile.clone());
                    }
                    if task.priority != 0 {
                        details.push(format!("priority {}", task.priority));
                    }
                    if !task.deps.is_empty() {
                        details.push(format!("deps: {}", task.deps.join(", ")));
                    }
                    if !task.write_scope.is_empty() {
                        details.push(format!("scope: {}", task.write_scope.join(", ")));
                    }
                    let _ = writeln!(
                        out,
                        "- **{}** {} · {}",
                        task.id,
                        task.title,
                        details.join(" · ")
                    );
                }
            }
        }

        let store = self.store.clone();
        let checkpoint_session = session.id.clone();
        let context_checkpoints = tokio::task::spawn_blocking(move || {
            store.list_context_checkpoints(&checkpoint_session, 200)
        })
        .await??;
        if !context_checkpoints.is_empty() {
            let _ = writeln!(out, "\n## Context Checkpoints\n");
            let _ = writeln!(
                out,
                "Context restores rewind the active summary boundary only. They do not reset the worktree or create a run.\n"
            );
            let _ = writeln!(
                out,
                "| id | mode | plan | tokens | active? | label | created |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|---|");
            let active_id = {
                let events = self.all_events(&session.id).await?;
                crate::context::active_context_checkpoint_id(&events)
            };
            for checkpoint in &context_checkpoints {
                let label = checkpoint
                    .label
                    .replace('|', "\\|")
                    .replace(['\n', '\r'], " ");
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} | {} → {} | {} | {} | {} |",
                    checkpoint.id,
                    checkpoint.mode,
                    checkpoint.plan_revision,
                    checkpoint.tokens_before,
                    checkpoint.tokens_after,
                    if active_id.as_deref() == Some(checkpoint.id.as_str()) {
                        "yes"
                    } else {
                        ""
                    },
                    label,
                    checkpoint.created_at
                );
            }
        }

        let _ = writeln!(out, "\n## Conversation\n");
        if messages.is_empty() {
            let _ = writeln!(out, "_No messages yet._");
        }
        for message in messages {
            let _ = writeln!(out, "### {} · {}\n", message.role, message.created_at);
            let _ = writeln!(out, "{}", clip_transcript(&message.content, 8_000));
            if let Some(calls) = message.tool_calls.as_array() {
                for call in calls {
                    let name = call["name"].as_str().unwrap_or("tool");
                    let arguments = call["arguments"].as_str().unwrap_or("");
                    let _ = writeln!(
                        out,
                        "\n- `{}` `{}`",
                        name,
                        clip_transcript(arguments, 1_000)
                    );
                }
            }
            let _ = writeln!(out);
        }
        Ok(SessionTranscriptResult {
            session_id: session.id,
            markdown: out,
        })
    }

    pub async fn search(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<SearchResult> {
        let query = query.trim();
        if query.is_empty() {
            anyhow::bail!("search query is required");
        }
        if query.chars().count() > 200 {
            anyhow::bail!("search query must be at most 200 characters");
        }
        let limit = limit.clamp(1, 100);
        let store = self.store.clone();
        let query_owned = query.to_string();
        let project_owned = project_id.map(str::to_string);
        let hits = tokio::task::spawn_blocking(move || {
            store.search(
                &query_owned,
                project_owned.as_deref(),
                limit.saturating_add(1),
            )
        })
        .await??;
        let truncated = hits.len() > limit as usize;
        let hits = hits.into_iter().take(limit as usize).collect();
        Ok(SearchResult {
            query: query.to_string(),
            hits,
            truncated,
        })
    }

    pub async fn latest_event_of_kind(
        &self,
        session_id: &str,
        kind: &str,
    ) -> Result<Option<Event>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || store.latest_event_by_kind(&session_id, &kind)).await?
    }

    pub async fn count_events_of_kind(&self, session_id: &str, kind: &str) -> Result<u64> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || store.event_count_by_kind(&session_id, &kind)).await?
    }

    pub async fn session_context_status(&self, session_id: &str) -> Result<SessionContextStatus> {
        let usage_event = self
            .latest_event_of_kind(session_id, "context.usage")
            .await?;
        let compaction_event = self
            .latest_event_of_kind(session_id, "context.compacted")
            .await?;
        let usage = usage_event.and_then(|event| {
            serde_json::from_value::<ContextUsage>(event.payload.clone())
                .ok()
                .map(|mut usage| {
                    usage.created_at = event.created_at;
                    usage
                })
        });
        let last_compaction = compaction_event.and_then(|event| {
            serde_json::from_value::<ContextCompactionSummary>(event.payload.clone())
                .ok()
                .map(|mut summary| {
                    summary.created_at = event.created_at;
                    summary
                })
        });
        let measurement_event = self
            .latest_event_of_kind(session_id, "context.measured")
            .await?;
        let last_measurement = measurement_event.and_then(|event| {
            serde_json::from_value::<ContextMeasurement>(event.payload.clone())
                .ok()
                .map(|mut measurement| {
                    measurement.created_at = event.created_at;
                    measurement
                })
        });
        let compaction_count = last_compaction
            .as_ref()
            .map(|summary| summary.compaction_count)
            .unwrap_or(0);
        let last_compaction_latency_ms = last_compaction
            .as_ref()
            .map(|summary| summary.latency_ms)
            .unwrap_or(0);
        let last_compaction_tokens_before = last_compaction
            .as_ref()
            .map(|summary| summary.estimated_tokens_before)
            .unwrap_or(0);
        let last_compaction_tokens_after = last_compaction
            .as_ref()
            .map(|summary| summary.estimated_tokens_after)
            .unwrap_or(0);
        let compaction_background_reuse = last_compaction
            .as_ref()
            .is_some_and(|summary| summary.background_reuse);
        let compaction_fallback_to_sync = last_compaction
            .as_ref()
            .is_some_and(|summary| summary.fallback_to_sync);
        let store = self.store.clone();
        let checkpoint_session = session_id.to_string();
        let (checkpoint_count, latest_checkpoint) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                store.context_checkpoint_count(&checkpoint_session)?,
                store.latest_context_checkpoint(&checkpoint_session)?,
            ))
        })
        .await??;
        Ok(SessionContextStatus {
            session_id: session_id.to_string(),
            usage,
            last_compaction,
            last_measurement,
            compaction_count,
            last_compaction_latency_ms,
            last_compaction_tokens_before,
            last_compaction_tokens_after,
            compaction_background_reuse,
            compaction_fallback_to_sync,
            checkpoint_count,
            latest_checkpoint_id: latest_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.id.clone()),
            latest_checkpoint_at: latest_checkpoint.map(|checkpoint| checkpoint.created_at),
        })
    }

    pub async fn list_events(
        &self,
        session_id: String,
        since_seq: u64,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_events(&session_id, since_seq, limit))
            .await?
    }

    /// Read-only, bounded tool-activity projection. It reads `tool.finished`
    /// events only; provider content and raw structured payloads are not
    /// copied into the response. Legacy events without the additive metrics
    /// fields decode with zeroes and a derived `TOOL_FAILED` code.
    pub async fn session_tool_activity(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<ToolActivityResult> {
        let limit = limit.clamp(1, 200);
        let store = self.store.clone();
        let id = session_id.to_string();
        let (events, total) = tokio::task::spawn_blocking(move || -> Result<(Vec<Event>, u64)> {
            if store.get_session(&id)?.is_none() {
                anyhow::bail!("session not found: {id}");
            }
            let events = store.list_recent_events_by_kind(&id, "tool.finished", limit + 1)?;
            let total = store.event_count_by_kind(&id, "tool.finished")?;
            Ok((events, total))
        })
        .await??;
        let truncated = events.len() > limit as usize;
        let mut entries = events
            .into_iter()
            .take(limit as usize)
            .map(tool_activity_entry)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.seq));
        let summary = tool_activity_summary(&entries);
        Ok(ToolActivityResult {
            session_id: session_id.to_string(),
            entries,
            total,
            limit,
            truncated,
            summary,
        })
    }

    pub async fn set_session_priority(
        &self,
        session_id: &str,
        priority: i32,
    ) -> Result<SessionPriorityResult> {
        if !(-100..=100).contains(&priority) {
            anyhow::bail!("session priority must be between -100 and 100");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.set_session_priority_with_event(&id, priority)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(SessionPriorityResult {
            session_id: session_id.to_string(),
            priority,
        })
    }

    /// Toggle unlimited budget mode. The persisted budget values are retained,
    /// so disabling unlimited restores the previous limits. The shared runtime
    /// flag changes immediately for any active agent loop.
    pub async fn set_session_unlimited(
        &self,
        session_id: &str,
        unlimited: bool,
    ) -> Result<SessionUnlimitedResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = id.clone();
            move || store.get_session(&id)
        })
        .await??
        .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let store = self.store.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.set_session_unlimited_with_event(&id, unlimited)
        })
        .await??;
        let _ = self.events.send(event);
        self.update_unlimited_flag(session_id, unlimited);
        Ok(SessionUnlimitedResult {
            session_id: session_id.to_string(),
            unlimited,
        })
    }

    pub async fn update_session_status(&self, session_id: &str, status: &str) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = status.to_string();
        let release_status = status.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.update_session_status_with_event(&id, &status)
        })
        .await??;
        let _ = self.events.send(event);
        if matches!(
            release_status.as_str(),
            "idle" | "failed" | "cancelled" | "landed" | "interrupted"
        ) {
            let _ = self
                .release_session_workspaces(session_id, "session_ended")
                .await;
        }
        Ok(())
    }

    pub async fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_calls: serde_json::Value,
        tool_call_id: Option<String>,
    ) -> Result<StoredMessage> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let role = role.to_string();
        let content = content.to_string();
        let (message, event) = tokio::task::spawn_blocking(move || {
            store.append_message_with_event(
                &session_id,
                &role,
                &content,
                &tool_calls,
                tool_call_id.as_deref(),
            )
        })
        .await??;
        let _ = self.events.send(event);
        Ok(message)
    }

    pub async fn list_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_messages(&session_id)).await?
    }
}
