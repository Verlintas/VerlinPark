use super::*;

impl Engine {
    pub async fn get_agent_memory(&self, params: AgentMemoryParams) -> Result<AgentMemoryResult> {
        let store = self.store.clone();
        let viewer = match params.identity_id.as_deref() {
            Some(identity_id) => Some(store.get_agent_identity(identity_id)?.ok_or_else(|| {
                vp_protocol::types::AgentMemoryError::new(
                    vp_protocol::types::agent_memory_codes::MEMORY_NOT_FOUND,
                    format!("agent identity not found: {identity_id}"),
                )
            })?),
            None => None,
        };
        let memory_id = params.memory_id.clone();
        let memory = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::get_agent_memory(&store, &memory_id)
        })
        .await??
        .ok_or_else(|| {
            vp_protocol::types::AgentMemoryError::new(
                vp_protocol::types::agent_memory_codes::MEMORY_NOT_FOUND,
                format!("agent memory not found: {}", params.memory_id),
            )
        })?;
        if let Some(project_id) = params.project_id.as_deref()
            && project_id != memory.project_id
        {
            return Err(vp_protocol::types::AgentMemoryError::new(
                vp_protocol::types::agent_memory_codes::MEMORY_PROJECT_MISMATCH,
                "agent memory belongs to another project",
            )
            .into());
        }
        if let Some(viewer) = viewer.as_ref() {
            if viewer.project_id != memory.project_id {
                return Err(vp_protocol::types::AgentMemoryError::new(
                    vp_protocol::types::agent_memory_codes::MEMORY_PROJECT_MISMATCH,
                    "viewer identity belongs to another project",
                )
                .into());
            }
            crate::identity_memory::ensure_memory_visible(&memory, Some(viewer))?;
        }
        Ok(AgentMemoryResult {
            memory,
            changed: false,
        })
    }

    pub async fn remember_agent_memory(
        &self,
        params: CreateAgentMemoryParams,
    ) -> Result<AgentMemoryResult> {
        let store = self.store.clone();
        let (memory, _event) = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::create_agent_memory_with_event(&store, &params)
        })
        .await??;
        Ok(AgentMemoryResult {
            memory,
            changed: true,
        })
    }

    pub async fn update_agent_memory(
        &self,
        params: UpdateAgentMemoryParams,
    ) -> Result<AgentMemoryResult> {
        let store = self.store.clone();
        let memory_id = params.memory_id.clone();
        match tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::update_agent_memory_with_event(&store, &params)
        })
        .await??
        {
            Some((memory, _event)) => Ok(AgentMemoryResult {
                memory,
                changed: true,
            }),
            None => {
                let current = self
                    .get_agent_memory(AgentMemoryParams {
                        memory_id,
                        project_id: None,
                        identity_id: None,
                    })
                    .await?;
                Ok(AgentMemoryResult {
                    memory: current.memory,
                    changed: false,
                })
            }
        }
    }

    pub async fn supersede_agent_memory(
        &self,
        params: SupersedeAgentMemoryParams,
    ) -> Result<AgentMemorySupersedeResult> {
        let store = self.store.clone();
        let result = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::supersede_agent_memory_with_events(&store, &params)
        })
        .await??;
        Ok(AgentMemorySupersedeResult {
            previous: result.previous,
            replacement: result.replacement,
        })
    }

    pub async fn forget_agent_memory(
        &self,
        params: ForgetAgentMemoryParams,
    ) -> Result<AgentMemoryForgetResult> {
        let store = self.store.clone();
        let memory_id = params.memory_id.clone();
        let prune = params.prune;
        let outcome = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::forget_agent_memory_with_event(&store, &params)
        })
        .await??;
        Ok(AgentMemoryForgetResult {
            memory_id,
            pruned: prune && outcome.is_some(),
            changed: outcome.is_some(),
        })
    }

    /// Logically erase one memory and its prior event payloads. Destructive,
    /// explicit, and local-only; imported rows fail closed.
    pub async fn erase_agent_memory(
        &self,
        params: vp_protocol::types::EraseAgentMemoryParams,
    ) -> Result<vp_protocol::types::AgentMemoryEraseResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::erase_agent_memory(&store, &params)
        })
        .await?
    }

    pub async fn verify_agent_memory(
        &self,
        params: VerifyAgentMemoryParams,
    ) -> Result<AgentMemoryResult> {
        let store = self.store.clone();
        let memory_id = params.memory_id.clone();
        match tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::verify_agent_memory_with_event(&store, &params)
        })
        .await??
        {
            Some((memory, _event)) => Ok(AgentMemoryResult {
                memory,
                changed: true,
            }),
            None => {
                let current = self
                    .get_agent_memory(AgentMemoryParams {
                        memory_id,
                        project_id: None,
                        identity_id: None,
                    })
                    .await?;
                Ok(AgentMemoryResult {
                    memory: current.memory,
                    changed: false,
                })
            }
        }
    }

    pub async fn sweep_agent_memories(
        &self,
        params: SweepAgentMemoriesParams,
    ) -> Result<AgentMemorySweepResult> {
        let store = self.store.clone();
        let sweep = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::sweep_agent_memories_with_events(&store, &params)
        })
        .await??;
        Ok(AgentMemorySweepResult {
            expired: sweep.expired,
            pruned: sweep.pruned,
            remaining: sweep.remaining,
        })
    }

    /// Project-scoped memory replay, or explicit projection repair. It never
    /// appends memory events and never crosses project boundaries.
    pub async fn replay_agent_memories(
        &self,
        params: AgentMemoryReplayParams,
    ) -> Result<AgentMemoryReplayReport> {
        let store = self.store.clone();
        let project_id = params.project_id;
        let repair = params.repair;
        tokio::task::spawn_blocking(move || {
            let events = store.list_agent_memory_events_for_project(&project_id, None)?;
            if repair {
                let total = vp_store::store::identity_memory::count_agent_memory_events(
                    &store,
                    &project_id,
                )?;
                if total > events.len() as u64 {
                    return Err(vp_protocol::types::AgentMemoryError::new(
                        vp_protocol::types::agent_memory_codes::MEMORY_REPLAY_TRUNCATED,
                        format!(
                            "project has {total} memory events but replay reads at most {}; refusing to repair from a truncated stream",
                            events.len()
                        ),
                    )
                    .into());
                }
            }
            let expected = crate::identity_memory::memories_from_events(&events)?;
            let mut actual = store.list_agent_memories_for_replay(&project_id)?;
            let mut mismatches =
                crate::identity_memory::compare_memory_projection(&expected, &actual);
            let mut repaired = false;
            if repair && !mismatches.is_empty() {
                store.replace_agent_memory_projection(&project_id, &expected)?;
                actual = store.list_agent_memories_for_replay(&project_id)?;
                mismatches = crate::identity_memory::compare_memory_projection(&expected, &actual);
                repaired = true;
            }
            Ok(crate::identity_memory::replay_report(
                &project_id,
                &expected,
                events.len(),
                repaired,
                mismatches,
            ))
        })
        .await?
    }
}
