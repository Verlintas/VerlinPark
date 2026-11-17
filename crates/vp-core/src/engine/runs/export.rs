use super::*;

impl Engine {
    pub async fn export_session(&self, session_id: &str) -> Result<SessionBundle> {
        let data = self.load_replay_data(session_id).await?;
        let store = self.store.clone();
        let project_id = data.session.project_id.clone();
        let project_store = store.clone();
        let project = tokio::task::spawn_blocking(move || project_store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let session_id = data.session.id.clone();
        let message_store = store.clone();
        let messages_session = session_id.clone();
        let agent_messages = tokio::task::spawn_blocking(move || {
            message_store.list_agent_messages(&messages_session, None, false)
        })
        .await??;
        let artifact_store = store.clone();
        let artifacts_session = session_id.clone();
        let mut artifacts =
            tokio::task::spawn_blocking(move || artifact_store.list_artifacts(&artifacts_session))
                .await??;
        let usage_store = store.clone();
        let usage_session = session_id.clone();
        let usage =
            tokio::task::spawn_blocking(move || usage_store.list_usage_entries(&usage_session))
                .await??;
        let program_store = store.clone();
        let program_session = session_id.clone();
        let programs = tokio::task::spawn_blocking(move || {
            program_store.list_programs_for_session(&program_session)
        })
        .await??;
        let mut program_milestones = Vec::new();
        let mut program_events = Vec::new();
        for program in &programs {
            let milestone_store = store.clone();
            let program_id = program.id.clone();
            program_milestones.extend(
                tokio::task::spawn_blocking(move || {
                    milestone_store.list_program_milestones(&program_id)
                })
                .await??,
            );
            let event_store = store.clone();
            let program_id = program.id.clone();
            program_events.extend(
                tokio::task::spawn_blocking(move || {
                    event_store.list_program_events(&program_id, Some(10_000))
                })
                .await??,
            );
        }
        for artifact in &mut artifacts {
            if artifact.storage == "file"
                && let Some(path) = &artifact.path
            {
                let mut data = tokio::fs::read(path)
                    .await
                    .with_context(|| format!("reading artifact blob {path}"))?;
                if artifact.compression == "zstd" {
                    data = zstd::stream::decode_all(&data[..])
                        .with_context(|| format!("decompressing artifact {}", artifact.id))?;
                }
                if !artifact.sha256.is_empty() && sha256_hex(&data) != artifact.sha256 {
                    anyhow::bail!(
                        "artifact {} content does not match its SHA-256",
                        artifact.id
                    );
                }
                use base64::Engine as _;
                artifact.data_base64 = Some(base64::engine::general_purpose::STANDARD.encode(data));
            }
        }
        let team_plan = data
            .plan
            .as_deref()
            .and_then(|plan| serde_json::from_str::<TeamPlan>(plan).ok());
        let blob_store = store.clone();
        let instruction_hashes = data
            .instructions
            .iter()
            .map(|instruction| instruction.hash.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let instruction_blobs = tokio::task::spawn_blocking(move || {
            let mut blobs = Vec::with_capacity(instruction_hashes.len());
            for hash in instruction_hashes {
                let blob = blob_store.get_instruction_blob(&hash)?.ok_or_else(|| {
                    anyhow!("instruction state references missing blob {hash}; export refused")
                })?;
                if crate::instruction::hash_instruction_text(&blob.text) != hash {
                    anyhow::bail!(
                        "instruction blob {hash} does not match its text; export refused"
                    );
                }
                blobs.push(blob);
            }
            Ok::<_, anyhow::Error>(blobs)
        })
        .await??;
        let policy_store = store.clone();
        let policy_project = data.session.project_id.clone();
        let policy_session = data.session.id.clone();
        let mut policy_hashes = data
            .events
            .iter()
            .filter(|event| event.kind == crate::runtime_pin::POLICY_SNAPSHOT_EVENT)
            .filter_map(|event| {
                event
                    .payload
                    .get("hash")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect::<std::collections::BTreeSet<_>>();
        for run in &data.runs {
            if let Some(snapshot) = &run.runtime_snapshot
                && !snapshot.policy_hash.is_empty()
            {
                policy_hashes.insert(snapshot.policy_hash.clone());
            }
        }
        let policy_snapshots = tokio::task::spawn_blocking(move || {
            let mut policy_hashes = policy_hashes;
            if let Some(raw) = policy_store.get_plan_runtime_snapshot(&policy_session)?
                && let Some(snapshot) = crate::runtime_pin::parse_snapshot(&raw)
                && !snapshot.policy_hash.is_empty()
            {
                policy_hashes.insert(snapshot.policy_hash);
            }
            let mut rows = Vec::new();
            for hash in policy_hashes {
                if let Some(row) = policy_store.get_policy_snapshot(&policy_project, &hash)? {
                    rows.push(row);
                }
            }
            Ok::<_, anyhow::Error>(rows)
        })
        .await??;
        let identity_store = store.clone();
        let identity_project = project.id.clone();
        let (agent_identities, agent_identity_events) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                identity_store.list_agent_identities(&identity_project, None, None)?,
                identity_store
                    .list_agent_identity_events_for_project(&identity_project, Some(100_000))?,
            ))
        })
        .await??;
        let coordination_store = store.clone();
        let coordination_project = project.id.clone();
        let (session_agent_assignments, agent_inbox, agent_coordination_events) =
            tokio::task::spawn_blocking(move || {
                Ok::<_, anyhow::Error>((
                    coordination_store.list_agent_assignments_for_replay(&coordination_project)?,
                    coordination_store.list_agent_inbox_for_replay(&coordination_project)?,
                    coordination_store
                        .list_agent_coordination_events_for_project(&coordination_project, None)?,
                ))
            })
            .await??;
        let workspace_store = store.clone();
        let workspace_project = project.id.clone();
        let (agent_workspaces, session_worker_workspaces, agent_workspace_events) =
            tokio::task::spawn_blocking(move || {
                Ok::<_, anyhow::Error>((
                    workspace_store.list_agent_workspaces_for_replay(&workspace_project)?,
                    workspace_store.list_session_workspaces_for_replay(&workspace_project)?,
                    workspace_store
                        .list_agent_workspace_events_for_project(&workspace_project, None)?,
                ))
            })
            .await??;
        let memory_store = store.clone();
        let memory_project = project.id.clone();
        let (agent_memories, agent_memory_events) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                vp_store::store::identity_memory::list_agent_memories_for_replay(
                    &memory_store,
                    &memory_project,
                )?,
                vp_store::store::identity_memory::list_agent_memory_events_for_project(
                    &memory_store,
                    &memory_project,
                    None,
                )?,
            ))
        })
        .await??;
        let employment_store = store.clone();
        let employment_project = project.id.clone();
        // Materialize the derived ledger so an export always carries the
        // evidence-current rows rather than a stale explicit replay.
        self.rebuild_agent_performance_projection(&employment_project)
            .await?;
        let (
            agent_performance,
            agent_retirement_proposals,
            agent_employment_events,
            agent_hire_requests,
        ) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                vp_store::store::employment::list_agent_performance(
                    &employment_store,
                    &employment_project,
                    None,
                )?,
                vp_store::store::employment::list_retirement_proposals_for_replay(
                    &employment_store,
                    &employment_project,
                )?,
                vp_store::store::employment::list_employment_events(
                    &employment_store,
                    &employment_project,
                    Some(100_000),
                )?,
                vp_store::store::employment::list_hire_requests_for_replay(
                    &employment_store,
                    &employment_project,
                )?,
            ))
        })
        .await??;
        let template_store = store.clone();
        let template_project = project.id.clone();
        let (team_templates, project_team_templates, team_template_events) =
            tokio::task::spawn_blocking(move || {
                vp_store::store::team_template::manifest_records(&template_store, &template_project)
            })
            .await??;
        Ok(SessionBundle {
            version: 1,
            exported_at: vp_protocol::now_rfc3339(),
            project,
            session: data.session,
            events: data.events,
            agents: data.agents,
            messages: data.messages,
            team_plan,
            todos: data.todos,
            approvals: data.approvals,
            merge_queue: data.queue,
            agent_messages,
            artifacts,
            usage,
            memories: data.memories,
            memory_conflicts: data.memory_conflicts,
            questions: data.questions,
            tasks: data.tasks,
            steering: data.steering,
            continuations: data.continuations,
            gates: data.gates,
            policy_snapshots,
            recoveries: data.recoveries,
            programs,
            program_milestones,
            program_events,
            session_runs: data.runs,
            run_steps: data.run_steps,
            run_checkpoints: data.run_checkpoints,
            context_checkpoints: data.context_checkpoints,
            supervised_processes: data.processes,
            test_results: data.test_results,
            instruction_blobs,
            instructions: data.instructions,
            prompt_prefix_observations: data.prompt_prefix_observations,
            effects: data.effects,
            agent_identities,
            agent_identity_events,
            session_agent_assignments,
            agent_inbox,
            agent_coordination_events,
            agent_workspaces,
            session_worker_workspaces,
            agent_workspace_events,
            agent_memories,
            agent_memory_events,
            agent_performance,
            agent_retirement_proposals,
            agent_employment_events,
            agent_hire_requests,
            team_templates,
            project_team_templates,
            team_template_events,
        })
    }
}
