use super::*;

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
