use super::*;

use vp_protocol::types::{
    AgentContextView, AgentInboxCounts, AgentInboxView, AgentInspectParams, AgentInspectResult,
    AgentTaskView, AgentTimelineEntry, AgentUsageTotals, TeamPlan,
};

const AGENT_INSPECT_DEFAULT_TIMELINE: u32 = 80;
const AGENT_INSPECT_MAX_TIMELINE: u32 = 200;
const AGENT_INSPECT_TEXT_CHARS: usize = 1_200;
const AGENT_INSPECT_INBOX_LIMIT: u32 = 50;
const AGENT_INSPECT_STEERING_LIMIT: usize = 50;

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub async fn create_agent(
        &self,
        session_id: &str,
        role: &str,
        name: &str,
        title: Option<String>,
        parent_id: Option<String>,
        model: &str,
    ) -> Result<AgentRecord> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let role = role.to_string();
        let name = name.to_string();
        let model = model.to_string();
        let (agent, event) = tokio::task::spawn_blocking(move || {
            store.create_agent_with_snapshot(
                &session_id,
                &role,
                &name,
                title.as_deref(),
                parent_id.as_deref(),
                &model,
            )
        })
        .await??;
        let _ = self.events.send(event);
        Ok(agent)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_agent_message(
        &self,
        session_id: &str,
        from_agent_id: &str,
        from_name: &str,
        to_agent_id: &str,
        to_name: &str,
        kind: &str,
        body: &str,
        payload: serde_json::Value,
    ) -> Result<AgentMessage> {
        if kind.trim().is_empty() || kind.len() > 64 {
            anyhow::bail!("message kind must be 1-64 characters");
        }
        if body.len() > 20_000 {
            anyhow::bail!("message body exceeds 20000 bytes");
        }
        if serde_json::to_string(&payload)
            .map(|encoded| encoded.len() > 20_000)
            .unwrap_or(true)
        {
            anyhow::bail!("message payload exceeds 20000 bytes");
        }
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let gc_session_id = session_id.clone();
        let from_agent_id = from_agent_id.to_string();
        let from_name = from_name.to_string();
        let to_agent_id = to_agent_id.to_string();
        let to_name = to_name.to_string();
        let kind = kind.to_string();
        let body = body.to_string();
        let (message, event) = tokio::task::spawn_blocking(move || {
            store.create_agent_message_with_event(
                &session_id,
                &from_agent_id,
                &from_name,
                &to_agent_id,
                &to_name,
                &kind,
                &body,
                &payload,
            )
        })
        .await??;
        let _ = self.events.send(event);
        self.auto_gc_mailbox_for_session(&gc_session_id).await;
        Ok(message)
    }

    pub async fn list_agent_messages(
        &self,
        session_id: &str,
        to_agent_id: Option<String>,
        unread_only: bool,
    ) -> Result<Vec<AgentMessage>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            store.list_agent_messages(&session_id, to_agent_id.as_deref(), unread_only)
        })
        .await?
    }

    pub async fn mark_agent_message_read(&self, message_id: &str) -> Result<Option<AgentMessage>> {
        let store = self.store.clone();
        let message_id = message_id.to_string();
        let marked =
            tokio::task::spawn_blocking(move || store.mark_agent_message_read(&message_id))
                .await??;
        match marked {
            Some((message, event)) => {
                if let Some(event) = event {
                    let _ = self.events.send(event);
                }
                self.auto_gc_mailbox_for_session(&message.session_id).await;
                Ok(Some(message))
            }
            None => Ok(None),
        }
    }

    pub async fn create_task_worktree(
        &self,
        project_root: &str,
        project_id: &str,
        session_id: &str,
        task_id: &str,
        base_branch: &str,
    ) -> Result<worktree::WorktreeInfo> {
        let data_dir = self.store.data_dir().to_path_buf();
        let root = PathBuf::from(project_root);
        let project_id = project_id.to_string();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let base_branch = base_branch.to_string();
        let git_lock = self.git_lock.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::create_task(
                &root,
                &data_dir,
                &project_id,
                &session_id,
                &task_id,
                &base_branch,
            )
        })
        .await?
    }

    pub async fn set_agent_worktree(
        &self,
        agent_id: &str,
        worktree: &str,
        branch: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let worktree = worktree.to_string();
        let branch = branch.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.set_agent_worktree_with_snapshot(&id, &worktree, &branch)
        })
        .await??;
        if let Some((_, event)) = updated {
            let _ = self.events.send(event);
        }
        Ok(())
    }

    pub async fn set_agent_workspace_binding(
        &self,
        agent_id: &str,
        workspace_id: Option<&str>,
        parent_workspace_id: Option<&str>,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let workspace_id = workspace_id.map(str::to_string);
        let parent_workspace_id = parent_workspace_id.map(str::to_string);
        let updated = tokio::task::spawn_blocking(move || {
            store.set_agent_workspace_binding_with_snapshot(
                &id,
                workspace_id.as_deref(),
                parent_workspace_id.as_deref(),
            )
        })
        .await??;
        if let Some((_, event)) = updated {
            let _ = self.events.send(event);
        }
        Ok(())
    }

    pub async fn set_agent_profile(&self, agent_id: &str, profile: &str) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let profile = profile.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.set_agent_profile_with_snapshot(&id, &profile)
        })
        .await??;
        if let Some((_, event)) = updated {
            let _ = self.events.send(event);
        }
        Ok(())
    }

    /// Persist the run→identity binding resolved by the run loop.
    pub async fn set_agent_identity_binding(
        &self,
        agent_id: &str,
        identity_id: Option<&str>,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let identity_id = identity_id.map(str::to_string);
        let updated = tokio::task::spawn_blocking(move || {
            store.set_agent_identity_with_snapshot(&id, identity_id.as_deref())
        })
        .await??;
        if let Some((_, event)) = updated {
            let _ = self.events.send(event);
        }
        Ok(())
    }

    /// Heartbeat the S2 assignment and S4 workspace leases for a live run.
    ///
    /// Lease loss is reported as a warning rather than aborting the run: the
    /// run may continue on the legacy path, but the observation is event-backed
    /// so it cannot silently switch execution modes. Fatal store errors still
    /// propagate.
    pub async fn heartbeat_run_leases(
        &self,
        session_id: &str,
        identity_id: &str,
    ) -> Result<RunLeaseHeartbeat> {
        if identity_id.trim().is_empty() {
            return Ok(RunLeaseHeartbeat::default());
        }
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let identity_id_owned = identity_id.to_string();
        tokio::task::spawn_blocking(move || {
            let mut report = RunLeaseHeartbeat::default();
            match store.renew_session_agent_assignment(
                &session_id_owned,
                &identity_id_owned,
                Some(3_600),
                Some("engine:run-heartbeat"),
            ) {
                Ok(_) => report.assignment_renewed = true,
                Err(error) => {
                    if let Some(typed) =
                        error.downcast_ref::<vp_protocol::types::AgentCoordinationError>()
                        && matches!(
                            typed.code,
                            vp_protocol::types::agent_coordination_codes::ASSIGNMENT_NOT_FOUND
                                | vp_protocol::types::agent_coordination_codes::ASSIGNMENT_LEASE_EXPIRED
                        )
                    {
                        report
                            .warnings
                            .push(format!("{}: {}", typed.code, typed.message));
                    } else {
                        return Err(error);
                    }
                }
            }
            match store.renew_session_worker_workspaces(
                &session_id_owned,
                Some(&identity_id_owned),
                Some(3_600),
                Some("engine:run-heartbeat"),
            ) {
                Ok(mutations) => report.workspaces_renewed = mutations.len(),
                Err(error) => {
                    if let Some(typed) =
                        error.downcast_ref::<vp_protocol::types::AgentWorkspaceError>()
                        && matches!(
                            typed.code,
                            vp_protocol::types::agent_workspace_codes::WORKSPACE_LEASE_EXPIRED
                                | vp_protocol::types::agent_workspace_codes::WORKSPACE_CONFLICT
                                | vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_FOUND
                        )
                    {
                        report
                            .warnings
                            .push(format!("{}: {}", typed.code, typed.message));
                    } else {
                        return Err(error);
                    }
                }
            }
            Ok(report)
        })
        .await?
    }

    pub async fn update_agent_status(&self, agent_id: &str, status: &str) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let status = status.to_string();
        let event =
            tokio::task::spawn_blocking(move || store.update_agent_status_with_event(&id, &status))
                .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn set_agent_task(
        &self,
        agent_id: &str,
        task_id: Option<String>,
        department: Option<String>,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.set_agent_task_with_event(&id, task_id.as_deref(), department.as_deref())
        })
        .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn list_agents(&self, session_id: &str) -> Result<Vec<AgentRecord>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_agents(&id)).await?
    }

    // ---------------------------------------------------------------------
    // V1 Collab Agent Inspector
    // ---------------------------------------------------------------------

    /// Bounded, read-only Agent inspection for the Collab inspector.
    ///
    /// The result is event-derived and deliberately excludes raw prompts, tool
    /// input/output, unbounded compaction summaries, and raw inbox payloads.
    /// It never appends an event, changes agent status, or approves anything.
    pub async fn inspect_agent(&self, params: AgentInspectParams) -> Result<AgentInspectResult> {
        let session_id = params.session_id.trim().to_string();
        let agent_id = params.agent_id.trim().to_string();
        if session_id.is_empty() || agent_id.is_empty() {
            return Err(operator_error(
                vp_protocol::types::agent_operator_codes::AGENT_TARGET_INVALID,
                "session_id and agent_id are required",
            ));
        }

        let store = self.store.clone();
        let lookup_session = session_id.clone();
        let lookup_agent = agent_id.clone();
        let (session, agent, agents) = tokio::task::spawn_blocking(move || {
            let session = store.get_session(&lookup_session)?.ok_or_else(|| {
                operator_error(
                    vp_protocol::types::agent_operator_codes::AGENT_NOT_FOUND,
                    "session not found",
                )
            })?;
            let agents = store.list_agents(&lookup_session)?;
            let agent = agents
                .iter()
                .find(|agent| agent.id == lookup_agent)
                .cloned()
                .ok_or_else(|| {
                    operator_error(
                        vp_protocol::types::agent_operator_codes::AGENT_NOT_FOUND,
                        "agent is not a member of this session",
                    )
                })?;
            Ok::<_, anyhow::Error>((session, agent, agents))
        })
        .await??;

        let timeline_limit = params
            .timeline_limit
            .unwrap_or(AGENT_INSPECT_DEFAULT_TIMELINE)
            .clamp(1, AGENT_INSPECT_MAX_TIMELINE);

        let store = self.store.clone();
        let timeline_session = session_id.clone();
        let timeline_agent = agent_id.clone();
        let (mut events, truncated) = tokio::task::spawn_blocking(move || {
            // For the common case read one extra row so `truncated` is proven
            // rather than guessed. At the hard ceiling the store clamps the
            // probe, so an indexed count supplies the same answer.
            if timeline_limit < AGENT_INSPECT_MAX_TIMELINE {
                let events = store.list_agent_events(
                    &timeline_session,
                    &timeline_agent,
                    timeline_limit + 1,
                )?;
                let truncated = events.len() > timeline_limit as usize;
                Ok::<_, anyhow::Error>((events, truncated))
            } else {
                let events =
                    store.list_agent_events(&timeline_session, &timeline_agent, timeline_limit)?;
                let truncated = store.agent_event_count(&timeline_session, &timeline_agent)?
                    > u64::from(timeline_limit);
                Ok::<_, anyhow::Error>((events, truncated))
            }
        })
        .await??;
        if events.len() > timeline_limit as usize {
            let excess = events.len() - timeline_limit as usize;
            events.drain(0..excess);
        }
        let timeline = events.iter().map(timeline_entry).collect::<Vec<_>>();

        let task = if let Some(task_id) = agent.task_id.clone() {
            let store = self.store.clone();
            let task_session = session_id.clone();
            let task_id_for_read = task_id.clone();
            let agents_for_verifier = agents.clone();
            let (plan, scheduled, status, verifier_status) =
                tokio::task::spawn_blocking(move || {
                    let plan = store
                        .get_team_plan(&task_session)?
                        .and_then(|raw| serde_json::from_str::<TeamPlan>(&raw).ok());
                    let scheduled = store
                        .list_plan_tasks(None, Some(&task_session))?
                        .into_iter()
                        .find(|task| task.task_id == task_id_for_read);
                    let status = store.plan_task_status(&task_session, &task_id_for_read)?;
                    let verifier_status = latest_verifier_status(
                        &store,
                        &task_session,
                        &agents_for_verifier,
                        &task_id_for_read,
                    )?;
                    Ok::<_, anyhow::Error>((plan, scheduled, status, verifier_status))
                })
                .await??;
            let max_attempts = self
                .orchestration_policy(&session.project_id)
                .await?
                .max_attempts;
            let plan_task = plan
                .as_ref()
                .and_then(|plan| plan.tasks.iter().find(|task| task.id == task_id));
            Some(AgentTaskView {
                task_id: task_id.clone(),
                title: plan_task.map(|task| task.title.clone()).unwrap_or_default(),
                department: plan_task
                    .map(|task| task.department.clone())
                    .or_else(|| agent.department.clone())
                    .unwrap_or_default(),
                kind: plan_task.map(|task| task.kind.clone()).unwrap_or_default(),
                status,
                acceptance: plan_task
                    .map(|task| task.acceptance.clone())
                    .unwrap_or_default(),
                write_scope: plan_task
                    .map(|task| task.write_scope.clone())
                    .unwrap_or_default(),
                deps: plan_task.map(|task| task.deps.clone()).unwrap_or_default(),
                attempts: scheduled.as_ref().map(|task| task.attempts).unwrap_or(0),
                max_attempts,
                verifier_status,
            })
        } else {
            None
        };

        let store = self.store.clone();
        let context_session = session_id.clone();
        let context_agent = agent_id.clone();
        let (started, prompt_prefix, usage_event, compacted, totals) =
            tokio::task::spawn_blocking(move || {
                let started = store.latest_agent_event_of_kind(
                    &context_session,
                    &context_agent,
                    "agent.started",
                )?;
                let prompt_prefix = store.latest_agent_event_of_kind(
                    &context_session,
                    &context_agent,
                    "context.prompt_prefix",
                )?;
                let usage_event = store.latest_agent_event_of_kind(
                    &context_session,
                    &context_agent,
                    "context.usage",
                )?;
                let compacted = store.latest_agent_event_of_kind(
                    &context_session,
                    &context_agent,
                    "context.compacted",
                )?;
                let totals = store.agent_usage_totals(&context_session, &context_agent)?;
                Ok::<_, anyhow::Error>((started, prompt_prefix, usage_event, compacted, totals))
            })
            .await??;
        let context = build_agent_context(
            &agent,
            started.as_ref(),
            prompt_prefix.as_ref(),
            usage_event.as_ref(),
            compacted.as_ref(),
            &totals,
        );

        let steering = self
            .session_steering(&session_id, None, Some(2_000))
            .await?
            .steering
            .into_iter()
            .filter(|steering| steering.agent_id == agent.id)
            .collect::<Vec<_>>();
        let steering = tail_bounded(steering, AGENT_INSPECT_STEERING_LIMIT);

        let (inbox, inbox_counts) = match agent.identity_id.clone() {
            Some(identity_id) => {
                let store = self.store.clone();
                let project_id = session.project_id.clone();
                let inbox_session = session_id.clone();
                let (messages, counts) = tokio::task::spawn_blocking(move || {
                    store.list_agent_inbox_messages(
                        Some(&project_id),
                        Some(&identity_id),
                        Some(&inbox_session),
                        None,
                        false,
                        Some(AGENT_INSPECT_INBOX_LIMIT),
                    )
                })
                .await??;
                (messages.into_iter().map(agent_inbox_view).collect(), counts)
            }
            None => (Vec::new(), AgentInboxCounts::default()),
        };

        Ok(AgentInspectResult {
            agent,
            task,
            context,
            timeline,
            steering,
            inbox,
            inbox_counts,
            truncated,
        })
    }

    // ---------------------------------------------------------------------
    // Project-scoped agent identities
    // ---------------------------------------------------------------------

    pub async fn get_agent_identity_policy(&self, project_id: &str) -> Result<AgentIdentityPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_agent_identity_policy(&project_id)).await?
    }

    pub async fn set_agent_identity_policy(
        &self,
        project_id: &str,
        policy: AgentIdentityPolicy,
    ) -> Result<AgentIdentityPolicy> {
        let store = self.store.clone();
        let project_id_for_check = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&project_id_for_check))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = crate::identity::normalize_policy(&policy)?;
        let for_store = policy.clone();
        let project_id = project_id.to_string();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            store.set_agent_identity_policy(&project_id, &for_store)
        })
        .await??;
        Ok(policy)
    }

    /// Explicit identity creation. No session, run, inbox, workspace, or memory
    /// row is created as a side effect.
    pub async fn create_agent_identity(
        &self,
        params: CreateAgentIdentityParams,
    ) -> Result<AgentIdentityResult> {
        let store = self.store.clone();
        let (identity, _event) =
            tokio::task::spawn_blocking(move || store.create_agent_identity(&params)).await??;
        Ok(AgentIdentityResult {
            identity,
            changed: true,
        })
    }

    pub async fn get_agent_identity(&self, identity_id: &str) -> Result<AgentIdentityResult> {
        let store = self.store.clone();
        let identity_id = identity_id.to_string();
        let identity = tokio::task::spawn_blocking(move || store.get_agent_identity(&identity_id))
            .await??
            .ok_or_else(|| {
                vp_protocol::types::AgentIdentityError::new(
                    crate::identity::codes::IDENTITY_NOT_FOUND,
                    "agent identity not found",
                )
            })?;
        Ok(AgentIdentityResult {
            identity,
            changed: false,
        })
    }

    pub async fn list_agent_identities_for_project(
        &self,
        project_id: &str,
        role: Option<&str>,
        status: Option<&str>,
    ) -> Result<AgentIdentitiesResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let role = role.map(str::to_string);
        let status = status.map(str::to_string);
        let identities = tokio::task::spawn_blocking(move || {
            store.list_agent_identities(&project_id, role.as_deref(), status.as_deref())
        })
        .await??;
        Ok(AgentIdentitiesResult { identities })
    }

    pub async fn update_agent_identity(
        &self,
        params: UpdateAgentIdentityParams,
    ) -> Result<AgentIdentityResult> {
        let store = self.store.clone();
        let identity_id = params.identity_id.clone();
        match tokio::task::spawn_blocking(move || store.update_agent_identity(&params)).await?? {
            Some((identity, _event)) => Ok(AgentIdentityResult {
                identity,
                changed: true,
            }),
            None => {
                let current = self.get_agent_identity(&identity_id).await?;
                Ok(AgentIdentityResult {
                    identity: current.identity,
                    changed: false,
                })
            }
        }
    }

    pub async fn pause_agent_identity(
        &self,
        params: PauseAgentIdentityParams,
    ) -> Result<AgentIdentityResult> {
        let store = self.store.clone();
        let identity_id = params.identity_id.clone();
        match tokio::task::spawn_blocking(move || store.pause_agent_identity(&params.identity_id))
            .await??
        {
            Some((identity, _event)) => Ok(AgentIdentityResult {
                identity,
                changed: true,
            }),
            None => {
                let current = self.get_agent_identity(&identity_id).await?;
                Ok(AgentIdentityResult {
                    identity: current.identity,
                    changed: false,
                })
            }
        }
    }

    pub async fn resume_agent_identity(
        &self,
        params: ResumeAgentIdentityParams,
    ) -> Result<AgentIdentityResult> {
        let store = self.store.clone();
        let identity_id = params.identity_id.clone();
        match tokio::task::spawn_blocking(move || store.resume_agent_identity(&params.identity_id))
            .await??
        {
            Some((identity, _event)) => Ok(AgentIdentityResult {
                identity,
                changed: true,
            }),
            None => {
                let current = self.get_agent_identity(&identity_id).await?;
                Ok(AgentIdentityResult {
                    identity: current.identity,
                    changed: false,
                })
            }
        }
    }

    pub async fn retire_agent_identity(
        &self,
        params: RetireAgentIdentityParams,
    ) -> Result<AgentIdentityResult> {
        let store = self.store.clone();
        let identity_id = params.identity_id.clone();
        match tokio::task::spawn_blocking(move || store.retire_agent_identity(&params)).await?? {
            Some((identity, _event)) => Ok(AgentIdentityResult {
                identity,
                changed: true,
            }),
            None => {
                let current = self.get_agent_identity(&identity_id).await?;
                Ok(AgentIdentityResult {
                    identity: current.identity,
                    changed: false,
                })
            }
        }
    }

    /// Read-only identity replay, or explicit projection repair. It never
    /// appends lifecycle events and never assigns identities to sessions.
    pub async fn replay_agent_identities(
        &self,
        params: AgentIdentityReplayParams,
    ) -> Result<AgentIdentityReplayReport> {
        let store = self.store.clone();
        let project_id = params.project_id;
        let repair = params.repair;
        tokio::task::spawn_blocking(move || {
            let events =
                store.list_agent_identity_events_for_project(&project_id, Some(100_000))?;
            let expected = crate::identity::identities_from_events(&events)?;
            let mut actual = store.list_agent_identities(&project_id, None, None)?;
            let mut mismatches = crate::identity::compare_identity_projection(&expected, &actual);
            let mut repaired = false;
            if repair && !mismatches.is_empty() {
                store.replace_agent_identity_projection(&project_id, &expected)?;
                actual = store.list_agent_identities(&project_id, None, None)?;
                mismatches = crate::identity::compare_identity_projection(&expected, &actual);
                repaired = true;
            }
            Ok(crate::identity::replay_report(
                &project_id,
                &expected,
                events.len(),
                repaired,
                mismatches,
            ))
        })
        .await?
    }

    // -----------------------------------------------------------------
    // S3 identity prompts and role contracts
    // -----------------------------------------------------------------

    /// Resolve the project identity that drives one run.
    ///
    /// Only Leader, Manager, and Worker may have a prompt identity. Labour,
    /// Verifier, and Scout return `None` even if a caller passes one of those
    /// role names: they are task/session gates, not project employees. A legacy
    /// session with no active assignment also returns `None`, preserving the
    /// pre-S3 prompt and cache bytes exactly.
    ///
    /// Selection is deterministic: exact role, then exact title, then exact
    /// department, then the oldest active assignment with an identity-id
    /// tie-break. The lease's wall-clock expiry is not consulted here; the
    /// persisted assignment status is authoritative, and S2 reclaims an expired
    /// lease before another session can use it. This keeps a live session's
    /// identity head section stable across runs.
    pub async fn prompt_identity_for_run(
        &self,
        session_id: &str,
        role: &str,
        title: Option<&str>,
        department: Option<&str>,
    ) -> Result<Option<AgentIdentity>> {
        let role = role.trim().to_ascii_lowercase();
        if !crate::identity_prompt::identity_role_applies(&role) {
            return Ok(None);
        }
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let title = title.map(str::to_string);
        let department = department.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&session_id)?
                .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
            let assignments = store.list_session_agent_assignments(
                Some(&session_id),
                None,
                None,
                Some("active"),
                None,
            )?;
            if assignments.is_empty() {
                return Ok(None);
            }
            let mut identities = HashMap::new();
            for assignment in &assignments {
                if let Some(identity) = store.get_agent_identity(&assignment.identity_id)? {
                    identities.insert(identity.id.clone(), identity);
                }
            }
            // `status = active` alone is not a live lease: the 30s sweep can
            // lag behind `lease_expires_at`, and an expired row must not keep
            // rendering an identity prompt. Live runs renew before expiry.
            let assignments = assignments
                .into_iter()
                .filter(|assignment| assignment.lease_active)
                .collect::<Vec<_>>();
            if assignments.is_empty() {
                return Ok(None);
            }
            let Some(assignment) = crate::identity_prompt::prefer_identity_assignment(
                &assignments,
                &identities,
                &role,
                title.as_deref(),
                department.as_deref(),
            ) else {
                return Ok(None);
            };
            let Some(identity) = identities.get(&assignment.identity_id) else {
                return Ok(None);
            };
            if identity.project_id != session.project_id
                || !identity.role.eq_ignore_ascii_case(&role)
                || identity.origin == "imported"
                || !matches!(identity.status.as_str(), "active" | "idle")
            {
                return Ok(None);
            }
            Ok(Some(identity.clone()))
        })
        .await?
    }

    /// Bounded identity/department/project memory brief (S5).
    ///
    /// The primary source is `agent_memories` scoped to the active identity,
    /// then its department, then project memory. Only local, active,
    /// non-expired rows are eligible. If the project has no S5 memory yet, the
    /// S3 legacy project-memory brief is used as a bounded compatibility
    /// fallback; it is labelled separately in telemetry. An empty result omits
    /// the `agent/memory` tail section entirely.
    pub async fn identity_memory_brief(
        &self,
        session_id: &str,
        identity: &AgentIdentity,
    ) -> Result<crate::identity_memory::IdentityMemoryBrief> {
        if !crate::identity_prompt::identity_role_applies(&identity.role) {
            return Ok(crate::identity_memory::IdentityMemoryBrief::default());
        }
        let store = self.store.clone();
        let lookup = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&lookup))
            .await??
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.project_id != identity.project_id {
            return Ok(crate::identity_memory::IdentityMemoryBrief::default());
        }
        let memory_store = self.store.clone();
        let project_id = session.project_id.clone();
        let viewer_identity_id = identity.id.clone();
        let viewer_department = identity.department.clone();
        let candidates = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::list_agent_memories_for_brief(
                &memory_store,
                &project_id,
                Some(viewer_identity_id.as_str()),
                viewer_department.as_deref(),
                Some(128),
            )
        })
        .await??;
        let visible = candidates
            .into_iter()
            .filter(|memory| crate::identity_memory::memory_visible_to(memory, Some(identity)))
            .collect::<Vec<_>>();
        let brief = crate::identity_memory::select_identity_memory_brief(
            &visible,
            crate::identity_memory::DEFAULT_BRIEF_BYTES,
            time::OffsetDateTime::now_utc(),
        );
        if !brief.is_empty() {
            return Ok(brief);
        }
        // S3 compatibility fallback. It is read-only and never marked used;
        // the ordinary run-memory brief retains that accounting.
        let query = identity
            .component
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                identity
                    .department
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| {
                identity
                    .title
                    .as_deref()
                    .and_then(|title| title.split_whitespace().next())
            })
            .unwrap_or(identity.role.as_str())
            .to_string();
        let matches = self
            .memory_search(MemorySearchParams {
                project_id: session.project_id,
                query,
                kind: None,
                kinds: Vec::new(),
                task_id: None,
                department: identity.department.clone(),
                paths: Vec::new(),
                limit: Some(16),
                include_stale: false,
                max_chars: None,
                lexical_only: true,
            })
            .await?
            .memories;
        if matches.is_empty() {
            return Ok(crate::identity_memory::IdentityMemoryBrief::default());
        }
        let selection = crate::memory::select_brief(
            &matches,
            crate::identity_prompt::IDENTITY_MEMORY_BRIEF_CHARS,
        );
        if selection.text.trim().is_empty() {
            return Ok(crate::identity_memory::IdentityMemoryBrief::default());
        }
        let ids = selection
            .memories
            .iter()
            .map(|matched| matched.memory.id.clone())
            .collect();
        let hashes = selection
            .memories
            .iter()
            .map(|matched| crate::sha256_hex(matched.memory.body.as_bytes()))
            .collect();
        Ok(crate::identity_memory::legacy_brief(
            selection.text,
            ids,
            hashes,
        ))
    }

    // -----------------------------------------------------------------
    // S5 identity, department, and project memory
    // -----------------------------------------------------------------

    /// Bounded project memory read. When `identity_id` is present it is a
    /// viewer and scope isolation applies; an operator read may omit it.
    pub async fn list_agent_memories(
        &self,
        params: AgentMemoryListParams,
    ) -> Result<AgentMemoryListResult> {
        let store = self.store.clone();
        let viewer = match params.identity_id.as_deref() {
            Some(identity_id) => {
                let viewer = store.get_agent_identity(identity_id)?.ok_or_else(|| {
                    vp_protocol::types::AgentMemoryError::new(
                        vp_protocol::types::agent_memory_codes::MEMORY_NOT_FOUND,
                        format!("agent identity not found: {identity_id}"),
                    )
                })?;
                if viewer.project_id != params.project_id {
                    return Err(vp_protocol::types::AgentMemoryError::new(
                        vp_protocol::types::agent_memory_codes::MEMORY_PROJECT_MISMATCH,
                        "viewer identity belongs to another project",
                    )
                    .into());
                }
                Some(viewer)
            }
            None => None,
        };
        let project_id = params.project_id.clone();
        let scope = params.scope.clone();
        let status = params.status.clone();
        let department = params.department.clone();
        let query = params.query.clone();
        let include_expired = params.include_expired;
        let limit = params.limit;
        let has_viewer = viewer.is_some();
        let memory_store = self.store.clone();
        let viewer_identity_id = viewer.as_ref().map(|viewer| viewer.id.clone());
        let viewer_department = viewer.as_ref().and_then(|viewer| viewer.department.clone());
        let (mut memories, total) = tokio::task::spawn_blocking(move || {
            let rows = vp_store::store::identity_memory::list_agent_memories(
                &memory_store,
                &project_id,
                scope.as_deref(),
                status.as_deref(),
                department.as_deref(),
                query.as_deref(),
                include_expired,
                limit,
                viewer_identity_id.as_deref(),
                viewer_department.as_deref(),
            )?;
            let total = if has_viewer {
                0
            } else {
                vp_store::store::identity_memory::count_agent_memories(
                    &memory_store,
                    &project_id,
                    scope.as_deref(),
                    status.as_deref(),
                    department.as_deref(),
                    query.as_deref(),
                    include_expired,
                )?
            };
            Ok::<_, anyhow::Error>((rows, total))
        })
        .await??;
        if let Some(viewer) = viewer.as_ref() {
            memories
                .retain(|memory| crate::identity_memory::memory_visible_to(memory, Some(viewer)));
        }
        let total = if viewer.is_some() {
            memories.len() as u64
        } else {
            total
        };
        Ok(AgentMemoryListResult { memories, total })
    }

    pub async fn session_assignments(
        &self,
        params: SessionAssignmentsParams,
    ) -> Result<SessionAssignmentsResult> {
        let store = self.store.clone();
        let assignments = tokio::task::spawn_blocking(move || {
            store.list_session_agent_assignments(
                params.session_id.as_deref(),
                params.identity_id.as_deref(),
                params.project_id.as_deref(),
                params.status.as_deref(),
                params.limit,
            )
        })
        .await??;
        Ok(SessionAssignmentsResult { assignments })
    }

    /// Assign one project identity to a session. Assignment is a durable
    /// coordination record, not a run: it creates no agent row, worktree, or
    /// provider call. `wake_session` is an optional continuation.
    pub async fn assign_session_identity(
        &self,
        params: SessionAssignParams,
    ) -> Result<SessionAssignResult> {
        let store = self.store.clone();
        let session_id = params.session_id.clone();
        let identity_id = params.identity_id.clone();
        let wake_session = params.wake_session;
        let mutation = tokio::task::spawn_blocking(move || {
            store.create_session_agent_assignment(
                &params.session_id,
                &params.identity_id,
                params.role.as_deref(),
                params.lease_secs,
                params.operator.as_deref(),
            )
        })
        .await??;
        let mut wake = AgentCoordinationWake {
            requested: wake_session,
            reason: if wake_session {
                "not_attempted".to_string()
            } else {
                String::new()
            },
            ..AgentCoordinationWake::default()
        };
        if wake_session {
            let body = crate::coordination::render_assignment_wake_body(&mutation.assignment);
            let operation_id = format!(
                "coordination-wake:assignment:{}:{}:{}",
                session_id, identity_id, mutation.assignment.assigned_at
            );
            wake = self
                .wake_session_for_coordination(&session_id, &operation_id, &body, true)
                .await;
        }
        self.record_project_manifest_best_effort().await;
        Ok(SessionAssignResult {
            assignment: mutation.assignment,
            created: mutation.changed,
            wake,
        })
    }

    pub async fn renew_session_assignment(
        &self,
        params: SessionAssignmentRenewParams,
    ) -> Result<SessionAssignmentRenewResult> {
        let store = self.store.clone();
        let mutation = tokio::task::spawn_blocking(move || {
            store.renew_session_agent_assignment(
                &params.session_id,
                &params.identity_id,
                params.lease_secs,
                params.operator.as_deref(),
            )
        })
        .await??;
        self.record_project_manifest_best_effort().await;
        Ok(SessionAssignmentRenewResult {
            assignment: mutation.assignment,
            renewed: mutation.changed,
        })
    }

    pub async fn release_session_identity(
        &self,
        params: SessionReleaseParams,
    ) -> Result<SessionReleaseResult> {
        let store = self.store.clone();
        let mutation = tokio::task::spawn_blocking(move || {
            store.release_session_agent_assignment(
                &params.session_id,
                &params.identity_id,
                params.operator,
                params.reason.as_deref(),
            )
        })
        .await??;
        self.record_project_manifest_best_effort().await;
        Ok(SessionReleaseResult {
            assignment: mutation.assignment,
            released: mutation.changed,
        })
    }

    /// Read-only project coordination replay, or explicit projection repair.
    /// Repair derives both projections from the append-only stream and never
    /// appends an event.
    pub async fn replay_agent_coordination(
        &self,
        params: AgentCoordinationReplayParams,
    ) -> Result<AgentCoordinationReplayReport> {
        let store = self.store.clone();
        let project_id = params.project_id;
        let repair = params.repair;
        tokio::task::spawn_blocking(move || {
            let events = store.list_agent_coordination_events_for_project(&project_id, None)?;
            let (expected_assignments, expected_inbox) =
                crate::coordination::project_from_events(&events)?;
            let mut actual_assignments = store.list_agent_assignments_for_replay(&project_id)?;
            let mut actual_inbox = store.list_agent_inbox_for_replay(&project_id)?;
            let mut mismatches = crate::coordination::compare_coordination_projection(
                &expected_assignments,
                &actual_assignments,
                &expected_inbox,
                &actual_inbox,
            );
            let mut repaired = false;
            if repair && !mismatches.is_empty() {
                store.replace_agent_coordination_projection(
                    &project_id,
                    &expected_assignments,
                    &expected_inbox,
                )?;
                actual_assignments = store.list_agent_assignments_for_replay(&project_id)?;
                actual_inbox = store.list_agent_inbox_for_replay(&project_id)?;
                mismatches = crate::coordination::compare_coordination_projection(
                    &expected_assignments,
                    &actual_assignments,
                    &expected_inbox,
                    &actual_inbox,
                );
                repaired = true;
            }
            Ok(crate::coordination::replay_report(
                &project_id,
                expected_assignments.len(),
                expected_inbox.len(),
                events.len(),
                repaired,
                mismatches,
            ))
        })
        .await?
    }

    /// Send one project-scoped inbox row. The optional wake is a deferred
    /// continuation; it never starts a provider call by itself and never holds
    /// a Worker/Labour slot.
    pub async fn send_agent_inbox(
        &self,
        params: AgentInboxSendParams,
    ) -> Result<AgentInboxSendResult> {
        let store = self.store.clone();
        let params_for_store = params.clone();
        let message = tokio::task::spawn_blocking(move || {
            let project_id = match params_for_store.project_id.as_deref() {
                Some(project_id) => project_id.to_string(),
                None => store
                    .get_agent_identity(&params_for_store.identity_id)?
                    .map(|identity| identity.project_id)
                    .ok_or_else(|| {
                        vp_protocol::types::AgentCoordinationError::new(
                            vp_protocol::types::agent_coordination_codes::INBOX_RECIPIENT_INVALID,
                            format!("agent identity not found: {}", params_for_store.identity_id),
                        )
                    })?,
            };
            let payload = match (&params_for_store.payload, &params_for_store.body) {
                (Some(_), Some(_)) => {
                    return Err(vp_protocol::types::AgentCoordinationError::new(
                        vp_protocol::types::agent_coordination_codes::INBOX_PAYLOAD_INVALID,
                        "pass either payload or body, not both",
                    )
                    .into());
                }
                (Some(payload), None) => payload.clone(),
                (None, Some(body)) => {
                    let body = body.trim();
                    if body.is_empty() || body.chars().count() > 4_000 {
                        return Err(vp_protocol::types::AgentCoordinationError::new(
                            vp_protocol::types::agent_coordination_codes::INBOX_PAYLOAD_INVALID,
                            "body must be 1-4000 characters",
                        )
                        .into());
                    }
                    serde_json::json!({"body": body})
                }
                (None, None) => serde_json::json!({}),
            };
            store.send_agent_inbox_message(
                &project_id,
                &params_for_store.identity_id,
                params_for_store.session_id.as_deref(),
                params_for_store.from_identity_id.as_deref(),
                &params_for_store.kind,
                &payload,
                params_for_store.expires_in_secs,
            )
        })
        .await??;
        let mut wake = AgentCoordinationWake {
            requested: params.wake_session,
            reason: if params.wake_session {
                "not_attempted".to_string()
            } else {
                String::new()
            },
            ..AgentCoordinationWake::default()
        };
        if params.wake_session {
            let target_session = match params.session_id.clone() {
                Some(session_id) => Some(session_id),
                None => {
                    let store = self.store.clone();
                    let identity_id = message.identity_id.clone();
                    tokio::task::spawn_blocking(move || {
                        store
                            .list_session_agent_assignments(
                                None,
                                Some(&identity_id),
                                None,
                                Some("active"),
                                Some(1),
                            )
                            .map(|rows| rows.into_iter().next().map(|row| row.session_id))
                    })
                    .await??
                }
            };
            if let Some(session_id) = target_session {
                let store = self.store.clone();
                let identity_id = message.identity_id.clone();
                let session_for_list = session_id.clone();
                let unread = tokio::task::spawn_blocking(move || {
                    store
                        .list_agent_inbox_messages(
                            None,
                            Some(&identity_id),
                            Some(&session_for_list),
                            None,
                            true,
                            Some(20),
                        )
                        .map(|(messages, _)| messages)
                })
                .await??;
                let body = if unread.is_empty() {
                    crate::coordination::render_inbox_wake_body(std::slice::from_ref(&message))
                } else {
                    crate::coordination::render_inbox_wake_body(&unread)
                };
                let operation_id = format!("coordination-wake:inbox:{}", message.id);
                wake = self
                    .wake_session_for_coordination(&session_id, &operation_id, &body, true)
                    .await;
            } else {
                wake.reason = "no_active_session".to_string();
            }
        }
        self.record_project_manifest_best_effort().await;
        Ok(AgentInboxSendResult { message, wake })
    }

    pub async fn list_agent_inbox(
        &self,
        params: AgentInboxListParams,
    ) -> Result<AgentInboxListResult> {
        let store = self.store.clone();
        let (messages, counts) = tokio::task::spawn_blocking(move || {
            store.list_agent_inbox_messages(
                params.project_id.as_deref(),
                params.identity_id.as_deref(),
                params.session_id.as_deref(),
                params.status.as_deref(),
                params.unread_only,
                params.limit,
            )
        })
        .await??;
        Ok(AgentInboxListResult { messages, counts })
    }

    pub async fn read_agent_inbox(
        &self,
        params: AgentInboxMessageParams,
    ) -> Result<AgentInboxReadResult> {
        let store = self.store.clone();
        let (message, changed) = tokio::task::spawn_blocking(move || {
            store.mark_agent_inbox_read(
                &params.message_id,
                params.project_id.as_deref(),
                params.identity_id.as_deref(),
            )
        })
        .await??;
        Ok(AgentInboxReadResult { message, changed })
    }

    pub async fn ack_agent_inbox(
        &self,
        params: AgentInboxMessageParams,
    ) -> Result<AgentInboxReadResult> {
        let store = self.store.clone();
        let (message, changed) = tokio::task::spawn_blocking(move || {
            store.ack_agent_inbox_message(
                &params.message_id,
                params.project_id.as_deref(),
                params.identity_id.as_deref(),
            )
        })
        .await??;
        self.record_project_manifest_best_effort().await;
        Ok(AgentInboxReadResult { message, changed })
    }

    /// Startup/periodic lease reconciliation. Expired leases become `released`
    /// and emit one bounded `agent.assignment.expired` event each. Active
    /// identities are never reassigned by this sweep.
    pub async fn reconcile_session_agent_assignments(&self) -> Result<usize> {
        let store = self.store.clone();
        let events = tokio::task::spawn_blocking(move || {
            store.expire_session_agent_assignments(Some(1_000))
        })
        .await??;
        if !events.is_empty() {
            self.record_project_manifest_best_effort().await;
        }
        Ok(events.len())
    }

    pub async fn sweep_agent_inbox_expiry(&self) -> Result<usize> {
        let store = self.store.clone();
        let events = tokio::task::spawn_blocking(move || store.expire_due_agent_inbox(Some(1_000)))
            .await??;
        if !events.is_empty() {
            self.record_project_manifest_best_effort().await;
        }
        Ok(events.len())
    }

    /// Create a deferred continuation for a coordination wake. It is
    /// deliberately best-effort: a busy session or a continuation failure must
    /// never fail the durable assignment/inbox write or hold a Worker slot.
    async fn wake_session_for_coordination(
        &self,
        session_id: &str,
        operation_id: &str,
        body: &str,
        requested: bool,
    ) -> AgentCoordinationWake {
        let mut wake = AgentCoordinationWake {
            requested,
            created: false,
            continuation_id: None,
            reason: String::new(),
        };
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = match tokio::task::spawn_blocking(move || store.get_session(&id)).await {
            Ok(Ok(Some(session))) => session,
            Ok(Ok(None)) => {
                wake.reason = "session_missing".to_string();
                return wake;
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "coordination wake could not read the session");
                wake.reason = "session_read_failed".to_string();
                return wake;
            }
            Err(error) => {
                tracing::warn!(%error, "coordination wake session read task failed");
                wake.reason = "session_read_failed".to_string();
                return wake;
            }
        };
        if !crate::coordination::wake_eligible_status(&session.status) {
            wake.reason = format!("session_{}", session.status);
            return wake;
        }
        match self
            .defer_session_watching_gate(
                session_id,
                body,
                Vec::new(),
                None,
                None,
                Some(1),
                None,
                None,
                None,
                None,
                Some(operation_id),
            )
            .await
        {
            Ok(result) => {
                wake.created = true;
                wake.continuation_id = Some(result.continuation.id);
                wake.reason = "deferred_continuation".to_string();
            }
            Err(error) => {
                tracing::warn!(%error, "coordination wake could not create a continuation");
                wake.reason = "wake_failed".to_string();
            }
        }
        wake
    }

    pub async fn create_leader_agent(&self, session_id: &str, model: &str) -> Result<String> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let model = model.to_string();
        let (agent, event) = tokio::task::spawn_blocking(move || {
            store.create_leader_agent_with_snapshot(&session_id, &model)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(agent.id)
    }
}

fn operator_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    vp_protocol::types::AgentIdentityError::new(code, message).into()
}

/// Bound one display string and remove common credential shapes. The inspector
/// is a read surface, not a secret store: callers pass only allowlisted fields,
/// and this is the final defense-in-depth filter.
fn inspect_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let redacted = crate::tooling::errors::redact_secrets_for_display(trimmed);
    truncate_chars(&redacted, max_chars)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut output = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

fn tail_bounded<T>(items: Vec<T>, limit: usize) -> Vec<T> {
    if items.len() <= limit {
        return items;
    }
    let skip = items.len() - limit;
    items.into_iter().skip(skip).collect()
}

fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn payload_u32(payload: &serde_json::Value, key: &str) -> Option<u32> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .map(|value| value.min(u64::from(u32::MAX)) as u32)
}

fn payload_u64(payload: &serde_json::Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(serde_json::Value::as_u64)
}

fn payload_status(payload: &serde_json::Value) -> Option<String> {
    for key in ["status", "task_status", "agent_status"] {
        if let Some(value) = payload_string(payload, key) {
            return Some(inspect_text(&value, 80));
        }
    }
    None
}

fn latest_verifier_status(
    store: &Store,
    session_id: &str,
    agents: &[AgentRecord],
    task_id: &str,
) -> Result<Option<String>> {
    let mut latest: Option<(u64, String)> = None;
    for verifier in agents
        .iter()
        .filter(|agent| agent.role == "verifier" && agent.task_id.as_deref() == Some(task_id))
    {
        let Some(event) =
            store.latest_agent_event_of_kind(session_id, &verifier.id, "agent.finished")?
        else {
            continue;
        };
        let status = match event
            .payload
            .get("approved")
            .and_then(serde_json::Value::as_bool)
        {
            Some(true) => "approved".to_string(),
            Some(false) => "rejected".to_string(),
            None => payload_status(&event.payload).unwrap_or_else(|| "finished".to_string()),
        };
        if latest.as_ref().is_none_or(|(seq, _)| event.seq > *seq) {
            latest = Some((event.seq, status));
        }
    }
    Ok(latest.map(|(_, status)| status))
}

fn build_agent_context(
    agent: &AgentRecord,
    started: Option<&Event>,
    prompt_prefix: Option<&Event>,
    usage_event: Option<&Event>,
    compacted: Option<&Event>,
    totals: &AgentUsageTotals,
) -> AgentContextView {
    let mut context = AgentContextView {
        model: agent.model.clone(),
        profile: agent.profile.clone(),
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        cache_read_tokens: totals.cache_read_tokens,
        cache_write_tokens: totals.cache_write_tokens,
        cost_usd: totals.cost_usd,
        generated_at: vp_protocol::now_rfc3339(),
        ..AgentContextView::default()
    };

    if let Some(event) = started {
        context.objective = payload_string(&event.payload, "goal")
            .or_else(|| payload_string(&event.payload, "objective"))
            .map(|goal| inspect_text(&goal, AGENT_INSPECT_TEXT_CHARS));
    }

    if let Some(event) = prompt_prefix {
        let payload = &event.payload;
        if let Some(model) = payload_string(payload, "model") {
            context.model = inspect_text(&model, 200);
        }
        if let Some(profile) = payload_string(payload, "agent_profile") {
            context.profile = Some(inspect_text(&profile, 80));
        }
        context.tool_count = payload_u32(payload, "tool_bundle_total_tool_count")
            .or_else(|| payload_u32(payload, "tool_count"));
        context.tool_bundle =
            payload_string(payload, "tool_bundle").map(|bundle| inspect_text(&bundle, 80));
        context.prefix_hash =
            payload_string(payload, "prefix_hash").map(|hash| inspect_text(&hash, 128));
        if let Some(memory) = payload
            .get("identity_memory")
            .filter(|value| value.is_object())
        {
            context.memory_brief_chars = payload_u32(memory, "brief_chars");
            context.memory_brief_hash =
                payload_string(memory, "brief_hash").map(|hash| inspect_text(&hash, 128));
        }
    }

    if let Some(event) = usage_event {
        let payload = &event.payload;
        context.estimated_tokens = payload_u64(payload, "estimated_tokens");
        context.context_window = payload_u64(payload, "context_window");
        context.threshold_tokens = payload_u64(payload, "threshold_tokens");
        context.hard_limit_tokens = payload_u64(payload, "hard_limit_tokens");
    }

    if let Some(event) = compacted {
        let mode = payload_string(&event.payload, "mode").unwrap_or_else(|| "compacted".into());
        let label = match payload_u64(&event.payload, "compaction_count") {
            Some(count) => format!("{mode} #{count}"),
            None => mode,
        };
        context.last_compaction = Some(inspect_text(&label, 120));
    }

    context
}

fn timeline_category(kind: &str) -> &'static str {
    match kind {
        "agent.message" | "agent.message_delta" | "agent.thought_delta" => "message",
        value if value.starts_with("tool.") => "tool",
        value if value.starts_with("approval.") => "approval",
        "agent.finished"
        | "agent.budget_exceeded"
        | "plan_task.finished"
        | "task_result.stored"
        | "verification.recorded"
        | "verification.failed" => "result",
        value if value.starts_with("context.") => "context",
        "coordination.note" | "steering.created" | "steering.delivered" | "steering.cancelled" => {
            "coordination"
        }
        value if value.starts_with("merge_queue.") || value.starts_with("integration.") => "merge",
        value if value.starts_with("question.") => "question",
        _ => "other",
    }
}

fn timeline_event_text(event: &Event) -> String {
    let payload = &event.payload;
    let raw = match event.kind.as_str() {
        "agent.message" => payload_string(payload, "text").unwrap_or_default(),
        "agent.message_delta" | "agent.thought_delta" => {
            payload_string(payload, "delta").unwrap_or_default()
        }
        "tool.started" => format!(
            "{} started",
            payload_string(payload, "name")
                .or_else(|| payload_string(payload, "tool"))
                .unwrap_or_else(|| "tool".to_string())
        ),
        "tool.finished" => {
            let name = payload_string(payload, "name")
                .or_else(|| payload_string(payload, "tool"))
                .unwrap_or_else(|| "tool".to_string());
            let failed = payload
                .get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            match payload_string(payload, "error_code") {
                Some(code) => format!(
                    "{name} {} ({code})",
                    if failed { "failed" } else { "finished" }
                ),
                None => format!("{name} {}", if failed { "failed" } else { "finished" }),
            }
        }
        "approval.created" | "approval.requested" => format!(
            "approval requested for {}",
            payload_string(payload, "tool").unwrap_or_else(|| "a tool".to_string())
        ),
        "approval.resolved" => {
            let approved = payload
                .get("approved")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            format!(
                "approval {} for {}",
                if approved { "approved" } else { "denied" },
                payload_string(payload, "tool").unwrap_or_else(|| "a tool".to_string())
            )
        }
        "agent.started" => format!(
            "agent started ({})",
            payload_string(payload, "model").unwrap_or_else(|| "model unknown".to_string())
        ),
        "agent.finished" => {
            let status = payload_status(payload).unwrap_or_else(|| "finished".to_string());
            match payload_string(payload, "summary") {
                Some(summary) => format!("agent {status}: {summary}"),
                None => format!("agent {status}"),
            }
        }
        "agent.usage" => format!(
            "usage: {} input / {} output",
            payload_u64(payload, "input_tokens").unwrap_or(0),
            payload_u64(payload, "output_tokens").unwrap_or(0)
        ),
        "agent.cost" => format!(
            "cost: ${:.4}",
            payload
                .get("amount_usd")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
        ),
        "context.prompt_prefix" => format!(
            "prompt prefix {} (model {}, {} tools)",
            payload_string(payload, "prefix_hash").unwrap_or_else(|| "unknown".to_string()),
            payload_string(payload, "model").unwrap_or_else(|| "unknown".to_string()),
            payload_u64(payload, "tool_bundle_total_tool_count")
                .or_else(|| payload_u64(payload, "tool_count"))
                .unwrap_or(0)
        ),
        "context.usage" => format!(
            "context: {} / {} tokens (threshold {})",
            payload_u64(payload, "estimated_tokens").unwrap_or(0),
            payload_u64(payload, "context_window").unwrap_or(0),
            payload_u64(payload, "threshold_tokens").unwrap_or(0)
        ),
        "context.compacted" | "context.compaction" | "context.compaction.cache_reset" => {
            let mode = payload_string(payload, "mode").unwrap_or_else(|| "compacted".to_string());
            match payload_u64(payload, "compaction_count") {
                Some(count) => format!("context compacted ({mode} #{count})"),
                None => format!("context compacted ({mode})"),
            }
        }
        "question.requested" => payload_string(payload, "question")
            .map(|question| format!("question: {question}"))
            .unwrap_or_else(|| "question requested".to_string()),
        "question.answered" => payload_string(payload, "answer")
            .map(|answer| format!("answer: {answer}"))
            .unwrap_or_else(|| "question answered".to_string()),
        "coordination.note" => payload_string(payload, "message")
            .map(|message| format!("coordination: {message}"))
            .unwrap_or_else(|| "coordination note".to_string()),
        "steering.created" => payload_string(payload, "body")
            .map(|body| format!("steering queued: {body}"))
            .unwrap_or_else(|| "steering queued".to_string()),
        "steering.delivered" => "steering delivered".to_string(),
        "steering.cancelled" => "steering cancelled".to_string(),
        _ => payload_string(payload, "summary")
            .or_else(|| payload_status(payload))
            .unwrap_or_else(|| event.kind.clone()),
    };
    inspect_text(&raw, AGENT_INSPECT_TEXT_CHARS)
}

fn timeline_entry(event: &Event) -> AgentTimelineEntry {
    let payload = &event.payload;
    let status = payload_status(payload);
    let is_error = payload
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || payload.get("error").is_some_and(|value| !value.is_null())
        || payload.get("approved").and_then(serde_json::Value::as_bool) == Some(false)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "failed" | "error" | "denied" | "rejected" | "cancelled" | "timed_out"
            )
        });
    AgentTimelineEntry {
        seq: event.seq,
        kind: event.kind.clone(),
        category: timeline_category(&event.kind).to_string(),
        created_at: event.created_at.clone(),
        text: timeline_event_text(event),
        tool: payload_string(payload, "name").or_else(|| payload_string(payload, "tool")),
        tool_call_id: payload_string(payload, "tool_call_id"),
        status,
        is_error,
    }
}

fn agent_inbox_view(message: vp_protocol::types::AgentInboxMessage) -> AgentInboxView {
    AgentInboxView {
        id: message.id,
        from_label: inspect_text(&message.from_label, 200),
        kind: inspect_text(&message.kind, 80),
        status: inspect_text(&message.status, 40),
        body: inbox_body(&message.payload),
        created_at: message.created_at,
        read_at: message.read_at,
        expires_at: message.expires_at,
    }
}

fn inbox_body(payload: &serde_json::Value) -> String {
    for key in ["body", "message", "summary", "detail"] {
        if let Some(value) = payload_string(payload, key) {
            return inspect_text(&value, AGENT_INSPECT_TEXT_CHARS);
        }
    }
    if payload.is_null() || payload.as_object().is_some_and(|object| object.is_empty()) {
        return String::new();
    }
    let summary = serde_json::to_string(payload).unwrap_or_default();
    inspect_text(&summary, 600)
}

#[cfg(test)]
mod inspector_tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;
    use vp_protocol::types::{
        AgentInboxSendParams, CreateAgentIdentityParams, PlanDepartment, PlanTask,
    };

    fn setup() -> (TempDir, TempDir, Engine, Project, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("inspect"))
            .unwrap();
        let session = store
            .create_session(&project.id, "inspect an agent")
            .unwrap();
        let engine = Engine::new(store);
        (data, repo, engine, project, session)
    }

    fn inspect_plan() -> TeamPlan {
        TeamPlan {
            goal: "inspect the widget".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build the widget".into(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks: vec![PlanTask {
                id: "T1".into(),
                department: "impl".into(),
                title: "Build the widget".into(),
                kind: "coding".into(),
                priority: 5,
                profile: Some("coding".into()),
                deps: vec!["T0".into()],
                write_scope: vec!["src/widget".into()],
                acceptance: vec!["tests pass".into()],
                token_budget: None,
                cost_budget_usd: None,
                parent_task_id: None,
                fanout_key: None,
                fanout_join_for: None,
            }],
        }
    }

    async fn worker_fixture(engine: &Engine, session: &Session) -> AgentRecord {
        let leader = engine
            .create_leader_agent(&session.id, "leader-model")
            .await
            .unwrap();
        engine
            .create_agent(
                &session.id,
                "worker",
                "Worker",
                Some("Implementation".into()),
                Some(leader),
                "worker-model",
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn inspect_agent_reports_task_context_timeline_and_redaction() {
        let (_data, _repo, engine, project, session) = setup();
        let worker = worker_fixture(&engine, &session).await;
        engine
            .submit_team_plan(&session.id, &inspect_plan())
            .await
            .unwrap();
        engine
            .set_agent_task(&worker.id, Some("T1".into()), Some("impl".into()))
            .await
            .unwrap();

        engine
            .append_event(
                &session.id,
                "agent.started",
                json!({
                    "agent_id": worker.id,
                    "agent_role": "worker",
                    "goal": "Inspect widgets safely",
                    "prompt": "PROMPT SECRET DO NOT SHOW",
                    "model": "worker-model",
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "context.prompt_prefix",
                json!({
                    "agent_id": worker.id,
                    "model": "worker-model",
                    "agent_profile": "coding",
                    "tool_bundle": "coding",
                    "tool_bundle_total_tool_count": 17,
                    "prefix_hash": "prefix-abc",
                    "prompt": "SYSTEM PROMPT SECRET",
                    "identity_memory": {
                        "brief_chars": 345,
                        "brief_hash": "memory-hash",
                        "memory_count": 3,
                    },
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "context.usage",
                json!({
                    "agent_id": worker.id,
                    "estimated_tokens": 12_000,
                    "context_window": 128_000,
                    "threshold_tokens": 96_000,
                    "hard_limit_tokens": 128_000,
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "context.compacted",
                json!({
                    "agent_id": worker.id,
                    "mode": "summary",
                    "compaction_count": 2,
                    "summary": "COMPACTION SECRET DO NOT SHOW",
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "agent.message",
                json!({
                    "agent_id": worker.id,
                    "text": "working with token=supersecret123 and bearer abc",
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "tool.started",
                json!({
                    "agent_id": worker.id,
                    "tool_call_id": "call-1",
                    "name": "shell",
                    "input": {"command": "cat TOOL INPUT SECRET"},
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "tool.finished",
                json!({
                    "agent_id": worker.id,
                    "tool_call_id": "call-1",
                    "name": "shell",
                    "is_error": false,
                    "output": "TOOL OUTPUT SECRET DO NOT SHOW",
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "approval.requested",
                json!({
                    "agent_id": worker.id,
                    "tool": "shell",
                    "status": "pending",
                    "input": {"command": "APPROVAL INPUT SECRET"},
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "agent.finished",
                json!({
                    "agent_id": worker.id,
                    "status": "completed",
                    "summary": "finished safely",
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "coordination.note",
                json!({
                    "agent_id": worker.id,
                    "message": "handoff to verifier",
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "merge_queue.enqueued",
                json!({
                    "agent_id": worker.id,
                    "status": "queued",
                }),
            )
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "question.requested",
                json!({
                    "agent_id": worker.id,
                    "question": "Should the widget be blue?",
                    "status": "pending",
                }),
            )
            .await
            .unwrap();
        engine
            .store()
            .add_usage_entry_with_events(
                &session.id,
                &worker.id,
                "Worker",
                "worker",
                Some("T1"),
                Some("impl"),
                "worker-model",
                100,
                20,
                40,
                5,
                false,
                0.25,
            )
            .unwrap();
        engine
            .store()
            .add_usage_entry_with_events(
                &session.id,
                &worker.id,
                "Worker",
                "worker",
                Some("T1"),
                Some("impl"),
                "worker-model",
                200,
                30,
                60,
                7,
                true,
                0.5,
            )
            .unwrap();

        let result = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id.clone(),
                agent_id: worker.id.clone(),
                timeline_limit: Some(80),
            })
            .await
            .unwrap();

        assert_eq!(result.agent.id, worker.id);
        let task = result.task.expect("task view");
        assert_eq!(task.task_id, "T1");
        assert_eq!(task.title, "Build the widget");
        assert_eq!(task.department, "impl");
        assert_eq!(task.kind, "coding");
        assert_eq!(task.status.as_deref(), Some("pending"));
        assert_eq!(task.acceptance, vec!["tests pass"]);
        assert_eq!(task.write_scope, vec!["src/widget"]);
        assert_eq!(task.deps, vec!["T0"]);
        assert_eq!(task.attempts, 0);
        assert_eq!(task.max_attempts, 2);
        assert!(task.verifier_status.is_none());

        assert_eq!(
            result.context.objective.as_deref(),
            Some("Inspect widgets safely")
        );
        assert_eq!(result.context.model, "worker-model");
        assert_eq!(result.context.profile.as_deref(), Some("coding"));
        assert_eq!(result.context.estimated_tokens, Some(12_000));
        assert_eq!(result.context.context_window, Some(128_000));
        assert_eq!(result.context.threshold_tokens, Some(96_000));
        assert_eq!(result.context.hard_limit_tokens, Some(128_000));
        assert_eq!(result.context.input_tokens, 300);
        assert_eq!(result.context.output_tokens, 50);
        assert_eq!(result.context.cache_read_tokens, 100);
        assert_eq!(result.context.cache_write_tokens, 12);
        assert!((result.context.cost_usd - 0.75).abs() < 1e-9);
        assert_eq!(result.context.tool_count, Some(17));
        assert_eq!(result.context.tool_bundle.as_deref(), Some("coding"));
        assert_eq!(result.context.prefix_hash.as_deref(), Some("prefix-abc"));
        assert_eq!(
            result.context.last_compaction.as_deref(),
            Some("summary #2")
        );
        assert_eq!(result.context.memory_brief_chars, Some(345));
        assert_eq!(
            result.context.memory_brief_hash.as_deref(),
            Some("memory-hash")
        );
        assert!(!result.context.generated_at.is_empty());
        assert!(!result.truncated);

        let categories = result
            .timeline
            .iter()
            .map(|entry| entry.category.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "message",
            "tool",
            "approval",
            "result",
            "context",
            "coordination",
            "merge",
            "question",
        ] {
            assert!(
                categories.contains(&expected),
                "missing timeline category {expected}: {categories:?}"
            );
        }
        let combined = result
            .timeline
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for secret in [
            "PROMPT SECRET",
            "SYSTEM PROMPT SECRET",
            "COMPACTION SECRET",
            "TOOL INPUT SECRET",
            "TOOL OUTPUT SECRET",
            "APPROVAL INPUT SECRET",
            "supersecret123",
        ] {
            assert!(!combined.contains(secret), "timeline leaked {secret}");
        }
        assert!(combined.contains("[redacted]"), "{combined}");
        assert!(result.steering.is_empty());
        assert!(result.inbox.is_empty());
        assert_eq!(project.id, session.project_id);
    }

    #[tokio::test]
    async fn inspect_agent_rejects_missing_and_cross_session_agents() {
        let (_data, _repo, engine, _project, session) = setup();
        let leader = engine
            .create_leader_agent(&session.id, "leader-model")
            .await
            .unwrap();
        let other_session = engine
            .store()
            .create_session(&session.project_id, "another session")
            .unwrap();
        let other_agent = engine
            .create_leader_agent(&other_session.id, "leader-model")
            .await
            .unwrap();

        let missing = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id.clone(),
                agent_id: "missing-agent".into(),
                timeline_limit: None,
            })
            .await
            .unwrap_err();
        assert!(
            format!("{missing}").contains("AGENT_NOT_FOUND"),
            "{missing}"
        );

        let cross_session = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id.clone(),
                agent_id: other_agent,
                timeline_limit: None,
            })
            .await
            .unwrap_err();
        assert!(
            format!("{cross_session}").contains("AGENT_NOT_FOUND"),
            "{cross_session}"
        );

        let invalid = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id.clone(),
                agent_id: "   ".into(),
                timeline_limit: None,
            })
            .await
            .unwrap_err();
        assert!(
            format!("{invalid}").contains("AGENT_TARGET_INVALID"),
            "{invalid}"
        );

        let minimal = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id,
                agent_id: leader,
                timeline_limit: None,
            })
            .await
            .unwrap();
        assert!(minimal.task.is_none());
        assert!(minimal.timeline.is_empty());
        assert_eq!(minimal.context.model, "leader-model");
        assert!(minimal.inbox.is_empty());
    }

    #[tokio::test]
    async fn inspect_timeline_is_bounded_and_reports_truncation() {
        let (_data, _repo, engine, _project, session) = setup();
        let leader = engine
            .create_leader_agent(&session.id, "leader-model")
            .await
            .unwrap();
        let mut seqs = Vec::new();
        for index in 0..5 {
            let event = engine
                .append_event(
                    &session.id,
                    "agent.message",
                    json!({
                        "agent_id": leader,
                        "text": format!("message {index}"),
                    }),
                )
                .await
                .unwrap();
            seqs.push(event.seq);
        }
        engine
            .append_event(
                &session.id,
                "agent.message",
                json!({"agent_id": "another-agent", "text": "not mine"}),
            )
            .await
            .unwrap();

        let bounded = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id.clone(),
                agent_id: leader.clone(),
                timeline_limit: Some(2),
            })
            .await
            .unwrap();
        assert_eq!(bounded.timeline.len(), 2);
        assert!(bounded.truncated);
        assert_eq!(bounded.timeline[0].seq, seqs[3]);
        assert_eq!(bounded.timeline[1].seq, seqs[4]);

        let exact = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id.clone(),
                agent_id: leader.clone(),
                timeline_limit: Some(5),
            })
            .await
            .unwrap();
        assert_eq!(exact.timeline.len(), 5);
        assert!(!exact.truncated);

        let clamped = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id.clone(),
                agent_id: leader.clone(),
                timeline_limit: Some(0),
            })
            .await
            .unwrap();
        assert_eq!(clamped.timeline.len(), 1);
        assert!(clamped.truncated);

        let oversized = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id,
                agent_id: leader,
                timeline_limit: Some(10_000),
            })
            .await
            .unwrap();
        assert_eq!(oversized.timeline.len(), 5);
        assert!(!oversized.truncated);
    }

    #[tokio::test]
    async fn inspect_agent_includes_steering_and_identity_inbox() {
        let (_data, _repo, engine, project, session) = setup();
        let worker = worker_fixture(&engine, &session).await;
        engine
            .submit_team_plan(&session.id, &inspect_plan())
            .await
            .unwrap();
        engine
            .set_agent_task(&worker.id, Some("T1".into()), Some("impl".into()))
            .await
            .unwrap();
        let identity = engine
            .create_agent_identity(CreateAgentIdentityParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("impl".into()),
                title: Some("Implementation".into()),
                profile: Some("coding".into()),
                ..CreateAgentIdentityParams::default()
            })
            .await
            .unwrap()
            .identity;
        engine
            .set_agent_identity_binding(&worker.id, Some(&identity.id))
            .await
            .unwrap();
        engine
            .steer_session(
                &session.id,
                "operator steering note",
                Some(&worker.id),
                Some("T1"),
                None,
            )
            .await
            .unwrap();
        engine
            .send_agent_inbox(AgentInboxSendParams {
                project_id: Some(project.id.clone()),
                identity_id: identity.id.clone(),
                session_id: Some(session.id.clone()),
                kind: "message".into(),
                body: Some("inbox token=inboxsecret123".into()),
                ..AgentInboxSendParams::default()
            })
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "agent.message",
                json!({"agent_id": worker.id, "text": "worker visible"}),
            )
            .await
            .unwrap();

        let result = engine
            .inspect_agent(AgentInspectParams {
                session_id: session.id,
                agent_id: worker.id.clone(),
                timeline_limit: Some(10),
            })
            .await
            .unwrap();

        assert_eq!(result.steering.len(), 1);
        assert_eq!(result.steering[0].agent_id, worker.id);
        assert_eq!(result.steering[0].body, "operator steering note");
        assert_eq!(result.inbox.len(), 1);
        assert!(!result.inbox[0].id.is_empty());
        assert_eq!(result.inbox[0].status, "unread");
        assert_eq!(result.inbox_counts.unread, 1);
        assert_eq!(result.inbox_counts.read, 0);
        assert!(!result.inbox[0].body.contains("inboxsecret123"));
        assert!(result.inbox[0].body.contains("[redacted]"));
        assert!(
            result
                .timeline
                .iter()
                .any(|entry| entry.category == "coordination")
        );
    }
}
