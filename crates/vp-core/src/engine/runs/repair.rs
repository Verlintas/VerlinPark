use super::*;

impl Engine {
    pub(super) fn build_session_repair(data: &ReplayData) -> Result<SessionRepair> {
        let status = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.updated")
            .and_then(|event| event.payload.get("status"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                data.events
                    .iter()
                    .any(|event| event.kind == "session.created")
                    .then_some("draft")
            })
            .unwrap_or(data.session.status.as_str())
            .to_string();
        let (tokens_in, tokens_out) = data
            .events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .fold((0u64, 0u64), |(input, output), event| {
                (
                    input.saturating_add(event.payload["input_tokens"].as_u64().unwrap_or(0)),
                    output.saturating_add(event.payload["output_tokens"].as_u64().unwrap_or(0)),
                )
            });
        let budget_projection = crate::auto_budget::budget_projection_from_events(&data.events)?;
        let auto_budget = budget_projection
            .as_ref()
            .and_then(|projection| projection.auto_record(&data.session.id));

        let plan_event = data.events.iter().rev().find(|event| {
            matches!(
                event.kind.as_str(),
                "team_plan.submitted" | "team_plan.revised"
            )
        });
        let replace_plan = plan_event.is_some() || data.plan.is_none();
        let plan = plan_event
            // A revision wraps the plan in `{plan, revision, ...}` while the
            // session column stores the bare plan. Serialize the resolved plan
            // value, not the event envelope, so repair never corrupts
            // `sessions.team_plan`. A payload that cannot be resolved leaves
            // the existing projection untouched rather than wiping it.
            .and_then(crate::orchestration::plan::plan_from_event)
            .map(|plan| serde_json::to_string(&plan).map_err(anyhow::Error::from))
            .transpose()?
            .or_else(|| data.plan.clone());

        let todos = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "todo.updated")
            .map(|event| serde_json::from_value::<Vec<TodoItem>>(event.payload["items"].clone()))
            .transpose()?
            .or_else(|| data.todos.is_empty().then(Vec::new));

        let messages = {
            let mut event_messages = Vec::new();
            let mut valid = true;
            for event in data
                .events
                .iter()
                .filter(|event| event.kind == "message.created")
            {
                match serde_json::from_value::<StoredMessage>(event.payload.clone()) {
                    Ok(message) => event_messages.push(message),
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            let projected_ids = data
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            let event_ids = event_messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            (valid && projected_ids.is_subset(&event_ids)).then_some(event_messages)
        };

        let agents = {
            let mut event_agents = HashMap::new();
            let mut valid = true;
            for event in data
                .events
                .iter()
                .filter(|event| event.kind == "agent.snapshot" || event.kind == "agent.updated")
            {
                let agent = if event.kind == "agent.snapshot" {
                    serde_json::from_value::<AgentRecord>(event.payload.clone())
                } else {
                    serde_json::from_value::<AgentRecord>(event.payload["agent"].clone())
                };
                match agent {
                    Ok(agent) => {
                        event_agents.insert(agent.id.clone(), agent);
                    }
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            let projected_ids = data
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            let event_ids = event_agents
                .keys()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            if valid && projected_ids.is_subset(&event_ids) {
                let mut agents = event_agents.into_values().collect::<Vec<_>>();
                agents.sort_by(|left, right| {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Some(agents)
            } else {
                None
            }
        };

        let queue = {
            let entries = Self::expected_merge_queue(data);
            entries.filter(|entries| !entries.is_empty() || data.queue.is_empty())
        };

        let questions = {
            let mut latest = HashMap::new();
            let mut valid = true;
            for event in data.events.iter().filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "question.requested"
                        | "question.answered"
                        | "question.cancelled"
                        | "question.expired"
                )
            }) {
                match serde_json::from_value::<Question>(event.payload.clone()) {
                    Ok(question) => {
                        latest.insert(question.id.clone(), question);
                    }
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid {
                let mut items = latest.into_values().collect::<Vec<_>>();
                items.sort_by(|left, right| left.created_at.cmp(&right.created_at));
                Some(items)
            } else {
                None
            }
        };

        let memories = {
            let mut items: Vec<MemoryItem> = Vec::new();
            let mut valid = true;
            for event in &data.events {
                match event.kind.as_str() {
                    "memory.created" | "memory.updated" | "memory.verified" | "memory.stale"
                    | "memory.superseded" | "memory.used" => {
                        match serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                            Ok(memory) => {
                                if let Some(existing) =
                                    items.iter_mut().find(|item| item.id == memory.id)
                                {
                                    *existing = memory;
                                } else {
                                    items.push(memory);
                                }
                            }
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    "memory.deleted" => {
                        match serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                            Ok(memory) => items.retain(|item| item.id != memory.id),
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            valid.then_some(items)
        };

        let expected_artifacts = Self::expected_artifacts(data);
        let all_full = expected_artifacts.iter().all(|expected| expected.full);
        let mut artifact_warnings = Vec::new();
        let artifacts = if all_full {
            Some(
                expected_artifacts
                    .iter()
                    .map(|expected| {
                        let artifact = expected.artifact.clone();
                        if artifact.storage == "file"
                            && artifact
                                .path
                                .as_deref()
                                .is_some_and(|path| !Path::new(path).is_file())
                        {
                            artifact_warnings.push(format!(
                                "artifact {} blob is missing at {}",
                                artifact.id,
                                artifact.path.as_deref().unwrap_or("(no path)")
                            ));
                        }
                        artifact
                    })
                    .collect(),
            )
        } else {
            artifact_warnings.extend(
                expected_artifacts
                    .iter()
                    .filter(|expected| !expected.full)
                    .map(|expected| {
                        format!(
                            "artifact {} predates full publish metadata and was not rebuilt",
                            expected.artifact.id
                        )
                    }),
            );
            None
        };
        let cost_spent_usd = replay_session_cost(&data.events);
        let priority = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.priority")
            .and_then(|event| event.payload.get("priority"))
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        let journal = crate::run_journal::projections_from_events(&data.events);
        let context_checkpoints = crate::context::checkpoints_from_events(&data.events);
        let processes = crate::process_supervisor::processes_from_events(&data.events);
        Ok(SessionRepair {
            status,
            priority,
            unlimited: Some(replay_session_unlimited(&data.events)),
            prompt_layout: replay_session_prompt_layout(&data.events),
            token_budget: budget_projection
                .as_ref()
                .map(|projection| projection.token_budget),
            cost_budget_usd: budget_projection
                .as_ref()
                .map(|projection| projection.cost_budget_usd),
            time_budget_secs: budget_projection
                .as_ref()
                .map(|projection| projection.time_budget_secs),
            auto_budget,
            tokens_in,
            tokens_out,
            cost_spent_usd,
            replace_plan,
            plan,
            todos,
            messages,
            agent_messages: Some(Self::expected_agent_messages(data)),
            agents,
            queue,
            artifacts,
            approvals: Some(Self::expected_approvals(data)),
            memories,
            memory_conflicts: Some(replay_memory_conflicts(&data.events)),
            questions,
            progress: Self::expected_progress(data),
            tasks: Self::expected_tasks(data),
            steering: Some(replay_steering(&data.events)),
            continuations: Some(replay_continuations(&data.events)),
            gates: if !data.events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::gate::OPENED_EVENT
                        | crate::gate::RESOLVED_EVENT
                        | crate::gate::EXPIRED_EVENT
                        | crate::gate::CANCELLED_EVENT
                )
            }) {
                // Legacy/imported archives may carry gate rows without an
                // event stream. Their projection is preserved rather than
                // treated as drift.
                if data.gates.is_empty() {
                    Some(Vec::new())
                } else {
                    None
                }
            } else {
                match crate::gate::gates_from_events(&data.events) {
                    Some(rows) => Some(rows),
                    None if data.gates.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            recoveries: Some(crate::orchestration::recoveries_from_events(&data.events)),
            runs: journal.as_ref().map(|value| value.0.clone()),
            run_steps: journal.as_ref().map(|value| value.1.clone()),
            run_checkpoints: journal.as_ref().map(|value| value.2.clone()),
            context_checkpoints,
            supervised_processes: processes,
            test_results: Some(crate::test_cache::results_from_events(&data.events)),
            remote_test_evidence: {
                let expected = Self::expected_remote_test_evidence(data);
                expected.filter(|rows| !rows.is_empty() || data.remote_test_evidence.is_empty())
            },
            instructions: match crate::instruction::states_from_events(&data.events) {
                Some(rows) => Some(rows),
                None if data.instructions.is_empty() => Some(Vec::new()),
                None => None,
            },
            contracts: if !data
                .events
                .iter()
                .any(|event| event.kind == "contract.derived")
            {
                None
            } else {
                match Self::expected_contract_rows(data) {
                    Some(rows) => Some(rows),
                    None if data.contracts.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            contract_usage: if !data
                .events
                .iter()
                .any(|event| event.kind == "intent.mutation")
            {
                None
            } else {
                match Self::expected_contract_usage_rows(data) {
                    Some(rows) => Some(rows),
                    None if data.contract_usage.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            effects: if !data.events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::effect::EFFECT_INTENT_EVENT
                        | crate::effect::EFFECT_SETTLED_EVENT
                        | crate::effect::EFFECT_FAILED_EVENT
                        | crate::effect::EFFECT_UNKNOWN_EVENT
                        | crate::effect::EFFECT_RECONCILED_EVENT
                )
            }) {
                None
            } else {
                match crate::effect::receipts_from_events(&data.events) {
                    Some(rows) => Some(rows),
                    None if data.effects.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            prompt_prefix_observations: match crate::cache_epoch::observations_from_events(
                &data.session.project_id,
                &data.session.id,
                &data.events,
            ) {
                Some(rows) => Some(rows),
                None if data.prompt_prefix_observations.is_empty() => Some(Vec::new()),
                None => None,
            },
            artifact_warnings,
        })
    }

    /// Rebuild the immutable policy projection from `policy.snapshot` events.
    /// Rows are content-addressed and idempotent; a malformed row fails closed.
    pub(super) async fn restore_policy_snapshots_from_events(
        &self,
        events: &[Event],
    ) -> Result<()> {
        let rows = crate::runtime_pin::policy_snapshots_from_events(events);
        if rows.is_empty() {
            return Ok(());
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            for row in &rows {
                store.insert_policy_snapshot_snapshot(row)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    pub async fn repair_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        ensure_session_not_active(&self.store, session_id, "repair a session").await?;
        let data = self.load_replay_data(session_id).await?;
        self.restore_policy_snapshots_from_events(&data.events)
            .await?;
        let report = Self::replay_report(&data, false)?;
        if report.consistent {
            return Ok(report);
        }
        let repair = Self::build_session_repair(&data)?;
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.repair_session_projections(&id, &repair))
            .await??;
        let expected_usage = Self::expected_usage_entries(&data);
        let usage_store = self.store.clone();
        let usage_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            usage_store.replace_usage_ledger(&usage_id, &expected_usage)
        })
        .await??;
        let expected_notes = crate::orchestration::coordination_notes(&data.events);
        let expected_decisions = crate::orchestration::decisions_from_events(&data.events);
        let expected_scaling = crate::scaling::decisions_from_events(&data.events);
        let expected_recoveries = crate::orchestration::recoveries_from_events(&data.events);
        let projection_store = self.store.clone();
        let projection_id = session_id.to_string();
        let projection_events = data.events.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            projection_store.replace_coordination_notes(
                &projection_id,
                &expected_notes,
                &projection_events,
            )?;
            projection_store.replace_orchestration_decisions(
                &projection_id,
                &expected_decisions,
                &projection_events,
            )?;
            projection_store.replace_scaling_decisions(
                &projection_id,
                &expected_scaling,
                &projection_events,
            )?;
            projection_store
                .replace_orchestration_recoveries(&projection_id, &expected_recoveries)?;
            Ok(())
        })
        .await??;
        self.append_event(
            session_id,
            "session.repaired",
            serde_json::json!({
                "mismatches": report.mismatches,
            }),
        )
        .await?;
        let data = self.load_replay_data(session_id).await?;
        Self::replay_report(&data, true)
    }
}
