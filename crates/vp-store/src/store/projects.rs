//! Project CRUD, project manifest snapshots, and project-scoped policies.

use super::*;

impl Store {
    pub fn add_project(&self, root: &str, name: Option<&str>) -> Result<Project> {
        let root_path = PathBuf::from(root);
        if !root_path.is_dir() {
            return Err(anyhow!("project root is not a directory: {root}"));
        }
        let root_path = root_path
            .canonicalize()
            .with_context(|| format!("canonicalizing {root}"))?;
        let root = root_path.to_string_lossy().to_string();

        let name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                root_path
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "Untitled project".to_string());

        let default_branch = detect_default_branch(&root_path);
        let project = Project {
            id: Uuid::now_v7().to_string(),
            name,
            root: root.clone(),
            default_branch,
            created_at: now_rfc3339(),
        };

        let conn = self.lock()?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM projects WHERE root = ?1",
                params![project.root],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Err(anyhow!("project already added: {id}"));
        }
        conn.execute(
            "INSERT INTO projects (id, name, root, default_branch, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.id,
                project.name,
                project.root,
                project.default_branch,
                project.created_at
            ],
        )?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, root, default_branch, created_at
             FROM projects ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                root: row.get(2)?,
                default_branch: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, root, default_branch, created_at FROM projects WHERE id = ?1",
            params![id],
            |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    root: row.get(2)?,
                    default_branch: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Append a full project/policy snapshot to `projects.jsonl`. The latest
    /// line per project is used by `read_project_manifest` for rebuilds.
    pub fn record_project_manifest(&self) -> Result<()> {
        let projects = self.list_projects()?;
        if projects.is_empty() {
            return Ok(());
        }
        let path = self.data_dir.join("projects.jsonl");
        let _guard = self
            .manifest_lock
            .lock()
            .map_err(|_| anyhow!("project manifest mutex poisoned"))?;
        let mut line_buffer = Vec::new();
        for project in projects {
            let retirement_policy =
                crate::store::employment::get_retirement_policy(self, &project.id)?.policy;
            // The performance projection is derived. Refresh it before
            // recording the manifest so the snapshot is evidence-current rather
            // than a stale read of the last explicit replay.
            crate::store::employment::rebuild_agent_performance(
                self,
                &project.id,
                retirement_policy.lookback_days,
                OffsetDateTime::now_utc(),
            )?;
            let (team_templates, project_team_templates, team_template_events) =
                crate::store::team_template::manifest_records(self, &project.id)?;
            let snapshot = ProjectManifestSnapshot {
                version: 1,
                recorded_at: now_rfc3339(),
                shell_policy: self.get_shell_policy(&project.id)?,
                contract_policy: self.get_contract_policy(&project.id)?,
                agent_identity_policy: self.get_agent_identity_policy(&project.id)?,
                agent_identities: self.list_agent_identities(&project.id, None, None)?,
                agent_identity_events: self
                    .list_agent_identity_events_for_project(&project.id, Some(100_000))?,
                session_agent_assignments: self.list_agent_assignments_for_replay(&project.id)?,
                agent_inbox: self.list_agent_inbox_for_replay(&project.id)?,
                agent_coordination_events: self
                    .list_agent_coordination_events_for_project(&project.id, None)?,
                agent_workspaces: self.list_agent_workspaces_for_replay(&project.id)?,
                session_worker_workspaces: self.list_session_workspaces_for_replay(&project.id)?,
                agent_workspace_events: self
                    .list_agent_workspace_events_for_project(&project.id, None)?,
                agent_memories: crate::store::identity_memory::list_agent_memories_for_replay(
                    self,
                    &project.id,
                )?,
                agent_memory_events:
                    crate::store::identity_memory::list_agent_memory_events_for_project(
                        self,
                        &project.id,
                        None,
                    )?,
                agent_retirement_policy: retirement_policy,
                agent_retirement_proposals:
                    crate::store::employment::list_retirement_proposals_for_replay(
                        self,
                        &project.id,
                    )?,
                agent_employment_events: crate::store::employment::list_employment_events(
                    self,
                    &project.id,
                    Some(100_000),
                )?,
                agent_performance: crate::store::employment::list_agent_performance(
                    self,
                    &project.id,
                    None,
                )?,
                agent_hire_requests: crate::store::employment::list_hire_requests_for_replay(
                    self,
                    &project.id,
                )?,
                team_templates,
                project_team_templates,
                team_template_events,
                effect_policy: self.get_effect_policy(&project.id)?,
                gate_policy: self.get_gate_policy(&project.id)?,
                otel_policy: self.get_otel_policy(&project.id)?,
                policy_snapshots: self.list_policy_snapshots(&project.id, 10_000)?,
                mcp_tool_pins: self.list_mcp_tool_pins(&project.id, None)?,
                cache_policy: self.get_cache_policy(&project.id)?,
                execution_host: self.get_execution_host(&project.id)?,
                remote_integration_policy: self.get_remote_integration_policy(&project.id)?,
                remote_test_policy: self.get_remote_test_policy(&project.id)?,
                remote_bootstrap_records: self.list_remote_bootstrap_records(&project.id)?,
                integration_policy: self.get_integration_policy(&project.id)?,
                gui_policy: self.get_gui_policy(&project.id)?,
                artifact_policy: self.get_artifact_policy(&project.id)?,
                tool_output_policy: self.get_tool_output_policy(&project.id)?,
                test_cache_policy: self.get_test_cache_policy(&project.id)?,
                mailbox_policy: self.get_mailbox_policy(&project.id)?,
                worktree_policy: self.get_worktree_policy(&project.id)?,
                checkpoint_policy: self.get_checkpoint_policy(&project.id)?,
                context_checkpoint_policy: self.get_context_checkpoint_policy(&project.id)?,
                process_policy: self.get_process_policy(&project.id)?,
                preemption_policy: self.get_preemption_policy(&project.id)?,
                budget: self.get_project_budget(&project.id)?,
                task_scheduler_policy: self.get_task_scheduler_policy(&project.id)?,
                scaling_policy: self.get_scaling_policy(&project.id)?,
                session_run_policy: self.get_session_run_policy(&project.id)?,
                memory_policy: self.get_memory_policy(&project.id)?,
                source_embedding_policy: self.get_source_embedding_policy(&project.id)?,
                source_retrieval_policy: self.get_source_retrieval_policy(&project.id)?,
                orchestration_policy: self.get_orchestration_policy(&project.id)?,
                critical_path_policy: self.get_critical_path_policy(&project.id)?,
                approval_grants: self.list_approval_grants(&project.id)?,
                approval_templates: self.list_approval_templates(&project.id)?,
                attention_policy: self.get_attention_policy(&project.id)?,
                approval_audit: self.list_approval_audit(&project.id, Some(100_000))?,
                intake_tasks: self.list_intake_tasks(Some(&project.id), None, None)?,
                programs: self.list_programs(Some(&project.id), None, None)?,
                program_milestones: self.list_program_milestones_for_project(&project.id)?,
                program_events: self.list_program_events_for_project(&project.id, Some(100_000))?,
                skills: self.list_skills(Some(&project.id), true)?,
                project,
            };
            serde_json::to_writer(&mut line_buffer, &snapshot)?;
            line_buffer.push(b'\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening project manifest {}", path.display()))?;
        file.write_all(&line_buffer)?;
        file.flush()?;
        paths::ensure_private_file(&path)?;
        Ok(())
    }

    pub fn insert_project_snapshot(&self, project: &Project) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO projects (id, name, root, default_branch, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.id,
                project.name,
                project.root,
                project.default_branch,
                project.created_at
            ],
        )?;
        Ok(())
    }

    pub fn insert_session_snapshot(&self, session: &Session) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sessions
             (id, project_id, goal, status, priority, worktree, branch, token_budget,
              cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit,
              team_plan, tokens_in, tokens_out, prompt_layout, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?14, ?15, ?16, ?17)",
            params![
                session.id,
                session.project_id,
                session.goal,
                session.status,
                session.priority,
                session.worktree,
                session.branch,
                session.token_budget,
                session.cost_budget_usd,
                session.time_budget_secs,
                if session.unlimited { 1 } else { 0 },
                session.cost_spent_usd,
                session.base_commit,
                session.tokens_in as i64,
                session.tokens_out as i64,
                session.prompt_layout as i64,
                session.created_at
            ],
        )?;
        Ok(())
    }

    pub fn get_shell_policy(&self, project_id: &str) -> Result<ShellPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT network, read_deny, restrict_home, read_jail
                 FROM project_shell_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, i64>(3)? != 0,
                    ))
                },
            )
            .optional()?;
        match stored {
            Some((network, read_deny, restrict_home, read_jail)) => Ok(ShellPolicy {
                network,
                read_deny: serde_json::from_str(&read_deny)
                    .context("parsing stored shell read-deny policy")?,
                restrict_home,
                read_jail,
            }),
            None => Ok(ShellPolicy::default()),
        }
    }

    pub fn set_shell_policy(&self, project_id: &str, policy: &ShellPolicy) -> Result<()> {
        let read_deny = serde_json::to_string(&policy.read_deny)
            .context("serializing shell read-deny policy")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_shell_policies
             (project_id, network, read_deny, restrict_home, read_jail, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                network = excluded.network,
                read_deny = excluded.read_deny,
                restrict_home = excluded.restrict_home,
                read_jail = excluded.read_jail,
                updated_at = excluded.updated_at",
            params![
                project_id,
                policy.network,
                read_deny,
                if policy.restrict_home { 1 } else { 0 },
                if policy.read_jail { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn get_contract_policy(&self, project_id: &str) -> Result<ContractPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_contract_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match stored {
            Some(policy) => {
                let policy: ContractPolicy =
                    serde_json::from_str(&policy).context("parsing stored contract policy")?;
                policy
                    .normalized()
                    .map_err(|error| anyhow!("stored contract policy is invalid: {error}"))
            }
            None => Ok(ContractPolicy::default()),
        }
    }

    pub fn set_contract_policy(&self, project_id: &str, policy: &ContractPolicy) -> Result<()> {
        let policy = policy
            .normalized()
            .map_err(|error| anyhow!("invalid contract policy: {error}"))?;
        let encoded = serde_json::to_string(&policy)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_contract_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_effect_policy(&self, project_id: &str) -> Result<EffectPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_effect_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match stored {
            Some(policy) => {
                let policy: EffectPolicy =
                    serde_json::from_str(&policy).context("parsing stored effect policy")?;
                policy
                    .normalized()
                    .map_err(|error| anyhow!("stored effect policy is invalid: {error}"))
            }
            None => Ok(EffectPolicy::default()),
        }
    }

    pub fn set_effect_policy(&self, project_id: &str, policy: &EffectPolicy) -> Result<()> {
        let policy = policy
            .normalized()
            .map_err(|error| anyhow!("invalid effect policy: {error}"))?;
        let encoded = serde_json::to_string(&policy)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_effect_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_gate_policy(&self, project_id: &str) -> Result<GatePolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_gate_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match stored {
            Some(policy) => serde_json::from_str(&policy).context("parsing stored gate policy"),
            None => Ok(GatePolicy::default()),
        }
    }

    pub fn set_gate_policy(&self, project_id: &str, policy: &GatePolicy) -> Result<()> {
        let encoded = serde_json::to_string(policy)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_gate_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_otel_policy(&self, project_id: &str) -> Result<OtelPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_otel_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match stored {
            Some(policy) => {
                let policy: OtelPolicy =
                    serde_json::from_str(&policy).context("parsing stored otel policy")?;
                policy
                    .normalized()
                    .map_err(|error| anyhow!("stored otel policy is invalid: {error}"))
            }
            None => Ok(OtelPolicy::default()),
        }
    }

    pub fn set_otel_policy(&self, project_id: &str, policy: &OtelPolicy) -> Result<()> {
        let policy = policy
            .normalized()
            .map_err(|error| anyhow!("invalid otel policy: {error}"))?;
        let encoded = serde_json::to_string(&policy)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_otel_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }
}
