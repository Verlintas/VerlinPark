use super::*;

impl Engine {
    pub(super) fn replay_report(data: &ReplayData, repaired: bool) -> Result<SessionReplayReport> {
        let mut mismatches = Vec::new();
        let replay_status = data
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
            });
        let status_matches = replay_status == Some(data.session.status.as_str());
        if !status_matches {
            mismatches.push(format!(
                "session status: projection={}, replay={:?}",
                data.session.status, replay_status
            ));
        }
        let replay_unlimited = replay_session_unlimited(&data.events);
        if replay_unlimited != data.session.unlimited {
            mismatches.push(format!(
                "session budget mode: projection={}, replay={}",
                data.session.unlimited, replay_unlimited
            ));
        }
        let prompt_layout_matches = replay_session_prompt_layout(&data.events)
            .map(|replay_layout| replay_layout == data.session.prompt_layout)
            .unwrap_or(true);
        if !prompt_layout_matches {
            mismatches.push(format!(
                "session prompt layout: projection={}, replay={:?}",
                data.session.prompt_layout,
                replay_session_prompt_layout(&data.events)
            ));
        }

        let (replay_in, replay_out) = data
            .events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .fold((0u64, 0u64), |(input, output), event| {
                (
                    input.saturating_add(event.payload["input_tokens"].as_u64().unwrap_or(0)),
                    output.saturating_add(event.payload["output_tokens"].as_u64().unwrap_or(0)),
                )
            });
        let replay_cost = replay_session_cost(&data.events);
        let usage_matches = replay_in == data.session.tokens_in
            && replay_out == data.session.tokens_out
            && (replay_cost - data.session.cost_spent_usd).abs() < 1e-9;
        if !usage_matches {
            mismatches.push(format!(
                "usage: projection={}/{}/{:.6}, replay={replay_in}/{replay_out}/{replay_cost:.6}",
                data.session.tokens_in, data.session.tokens_out, data.session.cost_spent_usd
            ));
        }

        let expected_usage = Self::expected_usage_entries(data);
        let has_usage_id = data
            .events
            .iter()
            .any(|event| event.kind == "agent.usage" && event.payload.get("usage_id").is_some());
        let projected_usage = data
            .usage
            .iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect::<HashMap<_, _>>();
        let ledger_matches = if data.usage.is_empty() && !has_usage_id {
            // Legacy sessions predate the ledger; their events have no usage_id
            // and no projection to compare against.
            true
        } else {
            expected_usage.len() == data.usage.len()
                && expected_usage.iter().all(|expected| {
                    projected_usage
                        .get(&expected.id)
                        .is_some_and(|projected| Self::same_usage(projected, expected))
                })
        };
        if !ledger_matches {
            mismatches.push(format!(
                "usage ledger: projection={}, replay={}",
                data.usage.len(),
                expected_usage.len()
            ));
        }

        let expected_mailbox = Self::expected_agent_messages(data);
        let expected_mailbox_values = expected_mailbox
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_mailbox_values = data
            .agent_messages
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let mailbox_matches = expected_mailbox_values == projected_mailbox_values;
        if !mailbox_matches {
            mismatches.push(format!(
                "mailbox: projection={}, replay={}",
                projected_mailbox_values.len(),
                expected_mailbox_values.len()
            ));
        }

        let expected_notes = crate::orchestration::coordination_notes(&data.events);
        let expected_note_values = expected_notes
            .iter()
            .filter_map(|note| Some((note.note_id.clone(), serde_json::to_value(note).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_note_values = data
            .notes
            .iter()
            .filter_map(|note| Some((note.note_id.clone(), serde_json::to_value(note).ok()?)))
            .collect::<HashMap<_, _>>();
        let notes_match = expected_note_values == projected_note_values;
        if !notes_match {
            mismatches.push(format!(
                "coordination notes: projection={}, replay={}",
                projected_note_values.len(),
                expected_note_values.len()
            ));
        }
        let expected_decisions = crate::orchestration::decisions_from_events(&data.events);
        let expected_decision_values = expected_decisions
            .iter()
            .filter_map(|decision| {
                Some((
                    decision.decision_id.clone(),
                    serde_json::to_value(decision).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let projected_decision_values = data
            .decisions
            .iter()
            .filter_map(|decision| {
                Some((
                    decision.decision_id.clone(),
                    serde_json::to_value(decision).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let decisions_match = expected_decision_values == projected_decision_values;
        if !decisions_match {
            mismatches.push(format!(
                "orchestration decisions: projection={}, replay={}",
                projected_decision_values.len(),
                expected_decision_values.len()
            ));
        }
        let expected_scaling = crate::scaling::decisions_from_events(&data.events);
        let scaling_decisions_match =
            crate::scaling::decisions_match(&data.scaling_decisions, &expected_scaling);
        if !scaling_decisions_match {
            mismatches.push(format!(
                "scaling decisions: projection={}, replay={}",
                data.scaling_decisions.len(),
                expected_scaling.len()
            ));
        }
        let expected_recoveries = crate::orchestration::recoveries_from_events(&data.events);
        let expected_recovery_values = expected_recoveries
            .iter()
            .filter_map(|recovery| {
                Some((recovery_key(recovery), serde_json::to_value(recovery).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_recovery_values = data
            .recoveries
            .iter()
            .filter_map(|recovery| {
                Some((recovery_key(recovery), serde_json::to_value(recovery).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let recoveries_match = expected_recovery_values == projected_recovery_values;
        if !recoveries_match {
            mismatches.push(format!(
                "orchestration recoveries: projection={}, replay={}",
                projected_recovery_values.len(),
                expected_recovery_values.len()
            ));
        }
        let expected_steering = replay_steering(&data.events);
        let expected_steering_values = expected_steering
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_steering_values = data
            .steering
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let steering_matches = expected_steering_values == projected_steering_values;
        if !steering_matches {
            mismatches.push(format!(
                "steering: projection={}, replay={}",
                projected_steering_values.len(),
                expected_steering_values.len()
            ));
        }
        let expected_continuations = replay_continuations(&data.events);
        let expected_continuation_values = expected_continuations
            .iter()
            .filter_map(|continuation| {
                Some((
                    continuation.id.clone(),
                    serde_json::to_value(continuation).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let projected_continuation_values = data
            .continuations
            .iter()
            .filter_map(|continuation| {
                Some((
                    continuation.id.clone(),
                    serde_json::to_value(continuation).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let continuations_match = expected_continuation_values == projected_continuation_values;
        if !continuations_match {
            mismatches.push(format!(
                "continuations: projection={}, replay={}",
                projected_continuation_values.len(),
                expected_continuation_values.len()
            ));
        }
        let expected_gates = crate::gate::gates_from_events(&data.events);
        let has_gate_events = data.events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                crate::gate::OPENED_EVENT
                    | crate::gate::RESOLVED_EVENT
                    | crate::gate::EXPIRED_EVENT
                    | crate::gate::CANCELLED_EVENT
            )
        });
        let gates_match = if !has_gate_events {
            // Legacy/imported archives may carry a gate projection without a
            // gate event stream. Preserve it rather than reporting drift.
            true
        } else {
            match expected_gates {
                Some(expected) => {
                    let matches = data.gates.len() == expected.len()
                        && data.gates.iter().all(|projected| {
                            expected.iter().any(|candidate| {
                                candidate.gate.id == projected.gate.id
                                    && candidate.gate.state == projected.gate.state
                                    && candidate.gate.outcome == projected.gate.outcome
                                    && candidate.gate.resolution_note
                                        == projected.gate.resolution_note
                                    && candidate.gate.resolved_at == projected.gate.resolved_at
                                    && candidate.gate.resolved_by == projected.gate.resolved_by
                                    && candidate.token_hash == projected.token_hash
                            })
                        });
                    if !matches {
                        mismatches.push(format!(
                            "external gates: projection={}, replay={}",
                            data.gates.len(),
                            expected.len()
                        ));
                    }
                    matches
                }
                None => {
                    mismatches.push("external gates: malformed replay event".to_string());
                    false
                }
            }
        };

        let expected_journal = crate::run_journal::projections_from_events(&data.events);
        let (runs_match, run_steps_match, run_checkpoints_match) = match &expected_journal {
            Some((runs, steps, checkpoints)) => (
                crate::run_journal::runs_match(&data.runs, runs),
                crate::run_journal::steps_match(&data.run_steps, steps),
                crate::run_journal::checkpoints_match(&data.run_checkpoints, checkpoints),
            ),
            None => (false, false, false),
        };
        if !runs_match {
            mismatches.push(format!(
                "session runs: projection={}, replay={}",
                data.runs.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.0.len())
                    .unwrap_or(0)
            ));
        }
        if !run_steps_match {
            mismatches.push(format!(
                "run steps: projection={}, replay={}",
                data.run_steps.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.1.len())
                    .unwrap_or(0)
            ));
        }
        if !run_checkpoints_match {
            mismatches.push(format!(
                "run checkpoints: projection={}, replay={}",
                data.run_checkpoints.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.2.len())
                    .unwrap_or(0)
            ));
        }

        let expected_checkpoints = crate::context::checkpoints_from_events(&data.events);
        let context_checkpoints_match = expected_checkpoints.as_ref().is_some_and(|expected| {
            crate::context::context_checkpoints_match(&data.context_checkpoints, expected)
        });
        if !context_checkpoints_match {
            mismatches.push(format!(
                "context checkpoints: projection={}, replay={}",
                data.context_checkpoints.len(),
                expected_checkpoints.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let expected_processes = crate::process_supervisor::processes_from_events(&data.events);
        let processes_match = expected_processes.as_ref().is_some_and(|expected| {
            crate::process_supervisor::processes_match(&data.processes, expected)
        });
        if !processes_match {
            mismatches.push(format!(
                "supervised processes: projection={}, replay={}",
                data.processes.len(),
                expected_processes.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let expected_test_results = crate::test_cache::results_from_events(&data.events);
        if !crate::test_cache::results_match(&data.test_results, &expected_test_results) {
            mismatches.push(format!(
                "test cache: projection={}, replay={}",
                data.test_results.len(),
                expected_test_results.len()
            ));
        }

        let has_instruction_events = data
            .events
            .iter()
            .any(|event| event.kind == "context.instruction");
        let instructions_match = match crate::instruction::states_from_events(&data.events) {
            Some(expected_instructions) => {
                let matches = crate::instruction::instruction_rows_match(
                    &data.instructions,
                    &expected_instructions,
                );
                if !matches {
                    mismatches.push(format!(
                        "instruction state: projection={}, replay={}",
                        data.instructions.len(),
                        expected_instructions.len()
                    ));
                }
                matches
            }
            None if has_instruction_events => {
                mismatches.push("instruction state: malformed replay event".to_string());
                false
            }
            None => {
                // Legacy/imported archives may carry instruction rows without
                // an instruction event stream. There is no replay authority in
                // that case, so the projection is preserved rather than
                // reported as drift.
                true
            }
        };

        let has_contract_events = data
            .events
            .iter()
            .any(|event| event.kind == "contract.derived");
        let expected_contracts = crate::contract::contracts_from_events(&data.events);
        let contracts_projection = data
            .contracts
            .iter()
            .map(|row| {
                serde_json::from_str::<crate::contract::TaskContract>(&row.contract).map_err(|_| ())
            })
            .collect::<std::result::Result<Vec<_>, ()>>();
        let contracts_match = if !has_contract_events {
            // Legacy/imported archives may carry rows without a contract event
            // stream. The projection is preserved rather than treated as drift.
            true
        } else {
            match (&expected_contracts, contracts_projection) {
                (Some(expected), Ok(projected)) => {
                    let rows_match = crate::contract::contracts_match(&projected, expected);
                    if !rows_match {
                        mismatches.push(format!(
                            "task contracts: projection={}, replay={}",
                            data.contracts.len(),
                            expected.len()
                        ));
                    }
                    let expected_usage =
                        crate::contract::usage_from_events(&data.events).unwrap_or_default();
                    let mut usage_delta = false;
                    if expected_usage.len() != data.contract_usage.len() {
                        usage_delta = true;
                    }
                    for row in &data.contract_usage {
                        let expected = expected_usage
                            .get(&row.contract_id)
                            .copied()
                            .unwrap_or_default();
                        if expected.writes != row.writes
                            || expected.bytes != row.bytes
                            || expected.commits != row.commits
                        {
                            usage_delta = true;
                        }
                    }
                    if usage_delta {
                        mismatches.push(format!(
                            "contract usage: projection={}, replay={}",
                            data.contract_usage.len(),
                            expected_usage.len()
                        ));
                    }
                    rows_match && !usage_delta
                }
                (None, _) if has_contract_events => {
                    mismatches.push("task contracts: malformed replay event".to_string());
                    false
                }
                (None, _) => {
                    // Legacy/imported archives may carry no contract event stream.
                    // Preserve the projection rather than reporting drift.
                    true
                }
                (Some(_), Err(())) => {
                    mismatches.push("task contracts: malformed projection row".to_string());
                    false
                }
            }
        };

        let expected_prefix_observations = crate::cache_epoch::observations_from_events(
            &data.session.project_id,
            &data.session.id,
            &data.events,
        );
        let (prefix_observations_match, prefix_aggregates_match) = if let Some(expected) =
            expected_prefix_observations
        {
            let observations_match =
                crate::cache_epoch::observations_match(&data.prompt_prefix_observations, &expected);
            if !observations_match {
                mismatches.push(format!(
                    "prompt prefix observations: projection={}, replay={}",
                    data.prompt_prefix_observations.len(),
                    expected.len()
                ));
            }
            let mut aggregates_match = data.prompt_prefix_inconsistent_epochs.is_empty();
            if !aggregates_match {
                mismatches.push(format!(
                    "prompt prefix aggregates: {} epoch(s) diverge from observations",
                    data.prompt_prefix_inconsistent_epochs.len()
                ));
            }
            let missing_aggregate = expected.iter().any(|row| {
                !data
                    .prompt_prefix_aggregates
                    .iter()
                    .any(|aggregate| aggregate.cache_epoch == row.cache_epoch)
            });
            if missing_aggregate {
                aggregates_match = false;
                mismatches.push(format!(
                    "prompt prefix aggregates: missing an epoch row for {} session epoch(s)",
                    expected.len()
                ));
            }
            (observations_match, aggregates_match)
        } else {
            // No epoch event stream means a legacy session; its projection
            // is preserved rather than reported as drift.
            (true, true)
        };

        let expected_artifacts = Self::expected_artifacts(data);
        let projected_artifacts = data
            .artifacts
            .iter()
            .map(|artifact| (artifact.id.clone(), artifact))
            .collect::<HashMap<_, _>>();
        let artifacts_matches = expected_artifacts.len() == data.artifacts.len()
            && expected_artifacts.iter().all(|expected| {
                projected_artifacts
                    .get(&expected.artifact.id)
                    .is_some_and(|projected| Self::same_expected_artifact(expected, projected))
            });
        if !artifacts_matches {
            mismatches.push(format!(
                "artifacts: projection={}, replay={}",
                data.artifacts.len(),
                expected_artifacts.len()
            ));
        }

        let replay_questions = data
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "question.requested"
                        | "question.answered"
                        | "question.cancelled"
                        | "question.expired"
                )
            })
            .filter_map(|event| {
                let question = serde_json::from_value::<Question>(event.payload.clone()).ok()?;
                Some((question.id.clone(), serde_json::to_value(question).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_questions = data
            .questions
            .iter()
            .filter_map(|question| {
                Some((question.id.clone(), serde_json::to_value(question).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let questions_matches = replay_questions == projected_questions;
        if !questions_matches {
            mismatches.push(format!(
                "questions: projection={}, replay={}",
                projected_questions.len(),
                replay_questions.len()
            ));
        }

        let replay_progress = data
            .events
            .iter()
            .filter(|event| event.kind == "progress.round")
            .filter_map(|event| {
                let plan_revision = event.payload.get("plan_revision")?.as_u64()?;
                let round =
                    serde_json::from_value::<ProgressRound>(event.payload.get("round")?.clone())
                        .ok()?;
                Some((
                    (plan_revision, round.round_id.clone()),
                    serde_json::to_value(round).ok()?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let projected_progress = data
            .progress
            .iter()
            .filter_map(|(plan_revision, round)| {
                Some((
                    (*plan_revision, round.round_id.clone()),
                    serde_json::to_value(round).ok()?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let progress_matches = replay_progress == projected_progress;
        if !progress_matches {
            mismatches.push(format!(
                "progress: projection={}, replay={}",
                projected_progress.len(),
                replay_progress.len()
            ));
        }

        let expected_tasks = Self::expected_tasks(data);
        let task_queue_matches = match &expected_tasks {
            Some(expected) => {
                expected.len() == data.tasks.len()
                    && expected.iter().all(|expected| {
                        data.tasks
                            .iter()
                            .find(|task| task.task_id == expected.task_id)
                            .is_some_and(|projected| Self::same_expected_task(expected, projected))
                    })
            }
            None => false,
        };
        if !task_queue_matches {
            mismatches.push(format!(
                "task queue: projection={}, replay={}",
                data.tasks.len(),
                expected_tasks.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let mut replay_memories = HashMap::new();
        for event in &data.events {
            match event.kind.as_str() {
                "memory.created" | "memory.updated" | "memory.verified" | "memory.stale"
                | "memory.superseded" | "memory.used" => {
                    if let Ok(memory) = serde_json::from_value::<MemoryItem>(event.payload.clone())
                    {
                        replay_memories.insert(memory.id.clone(), serde_json::to_value(memory)?);
                    }
                }
                "memory.deleted" => {
                    if let Ok(memory) = serde_json::from_value::<MemoryItem>(event.payload.clone())
                    {
                        replay_memories.remove(&memory.id);
                    }
                }
                _ => {}
            }
        }
        let projected_memories = data
            .memories
            .iter()
            .filter_map(|memory| Some((memory.id.clone(), serde_json::to_value(memory).ok()?)))
            .collect::<HashMap<_, _>>();
        let memories_matches = replay_memories == projected_memories;
        if !memories_matches {
            mismatches.push(format!(
                "memories: projection={}, replay={}",
                projected_memories.len(),
                replay_memories.len()
            ));
        }

        let replay_conflicts = replay_memory_conflicts(&data.events)
            .into_iter()
            .filter_map(|conflict| {
                Some((conflict.id.clone(), serde_json::to_value(conflict).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_conflicts = data
            .memory_conflicts
            .iter()
            .filter_map(|conflict| {
                Some((conflict.id.clone(), serde_json::to_value(conflict).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let memory_conflicts_match = replay_conflicts == projected_conflicts;
        if !memory_conflicts_match {
            mismatches.push(format!(
                "memory conflicts: projection={}, replay={}",
                projected_conflicts.len(),
                replay_conflicts.len()
            ));
        }

        let expected_approvals = Self::expected_approvals(data);
        let expected_approval_values = expected_approvals
            .iter()
            .filter_map(|approval| {
                Some((approval.id.clone(), serde_json::to_value(approval).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_approval_values = data
            .approvals
            .iter()
            .filter_map(|approval| {
                Some((approval.id.clone(), serde_json::to_value(approval).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let approvals_matches = expected_approval_values == projected_approval_values;
        if !approvals_matches {
            mismatches.push(format!(
                "approvals: projection={}, replay={}",
                projected_approval_values.len(),
                expected_approval_values.len()
            ));
        }

        let replay_plan = data
            .events
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.kind.as_str(),
                    "team_plan.submitted" | "team_plan.revised"
                )
            })
            .map(|event| {
                let mut payload = event.payload.clone();
                if let Some(object) = payload.as_object_mut() {
                    object.remove("runtime_snapshot");
                }
                payload
            });
        let projected_plan = data
            .plan
            .as_deref()
            .and_then(|plan| serde_json::from_str::<serde_json::Value>(plan).ok());
        let plan_matches = replay_plan == projected_plan;
        if !plan_matches {
            mismatches.push("team plan projection differs from event stream".to_string());
        }

        let replay_todos = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "todo.updated")
            .map(|event| event.payload["items"].clone())
            .unwrap_or_else(|| serde_json::json!([]));
        let projected_todos = serde_json::to_value(&data.todos)?;
        let todos_matches = replay_todos == projected_todos;
        if !todos_matches {
            mismatches.push("todo projection differs from event stream".to_string());
        }

        let replay_messages = data
            .events
            .iter()
            .filter(|event| event.kind == "message.created")
            .filter_map(|event| {
                let message =
                    serde_json::from_value::<StoredMessage>(event.payload.clone()).ok()?;
                Some((message.id.clone(), serde_json::to_value(message).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_messages = data
            .messages
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let messages_matches = replay_messages == projected_messages;
        if !messages_matches {
            mismatches.push(format!(
                "messages: projection={}, replay={}",
                projected_messages.len(),
                replay_messages.len()
            ));
        }

        let replay_agents = data
            .events
            .iter()
            .filter_map(|event| {
                let agent = if event.kind == "agent.snapshot" {
                    serde_json::from_value::<AgentRecord>(event.payload.clone()).ok()?
                } else if event.kind == "agent.updated" {
                    serde_json::from_value::<AgentRecord>(event.payload.get("agent")?.clone())
                        .ok()?
                } else {
                    return None;
                };
                Some((agent.id.clone(), serde_json::to_value(agent).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_agents = data
            .agents
            .iter()
            .filter_map(|agent| Some((agent.id.clone(), serde_json::to_value(agent).ok()?)))
            .collect::<HashMap<_, _>>();
        let agents_matches = replay_agents == projected_agents;
        if !agents_matches {
            mismatches.push(format!(
                "agents: projection={}, replay={}",
                projected_agents.len(),
                replay_agents.len()
            ));
        }

        let replay_queue = data
            .events
            .iter()
            .filter_map(|event| {
                let entry = event.payload.get("entry")?;
                Some((
                    entry.get("id")?.as_str()?.to_string(),
                    entry.get("status")?.as_str()?.to_string(),
                ))
            })
            .fold(HashMap::new(), |mut map, (id, status)| {
                map.insert(id, status);
                map
            });
        let projected_queue = data
            .queue
            .iter()
            .map(|entry| (entry.id.clone(), entry.status.clone()))
            .collect::<HashMap<_, _>>();
        let queue_matches = replay_queue == projected_queue;
        if !queue_matches {
            mismatches.push("merge queue projection differs from event stream".to_string());
        }

        let replay_evidence = Self::expected_remote_test_evidence(data).unwrap_or_default();
        let mut projected_evidence = data.remote_test_evidence.clone();
        projected_evidence.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let remote_test_evidence_matches = replay_evidence == projected_evidence;
        if !remote_test_evidence_matches {
            mismatches
                .push("remote test evidence projection differs from event stream".to_string());
        }

        let has_effect_events = data.events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                crate::effect::EFFECT_INTENT_EVENT
                    | crate::effect::EFFECT_SETTLED_EVENT
                    | crate::effect::EFFECT_FAILED_EVENT
                    | crate::effect::EFFECT_UNKNOWN_EVENT
                    | crate::effect::EFFECT_RECONCILED_EVENT
            )
        });
        let effects_match = if !has_effect_events {
            // Legacy/imported archives may carry rows without effect events.
            true
        } else {
            match crate::effect::receipts_from_events(&data.events) {
                Some(expected) => {
                    let rows_match = crate::effect::receipts_match(&data.effects, &expected);
                    if !rows_match {
                        mismatches.push(format!(
                            "effect receipts: projection={}, replay={}",
                            data.effects.len(),
                            expected.len()
                        ));
                    }
                    rows_match
                }
                None => {
                    mismatches.push("effect receipts: malformed replay event".to_string());
                    false
                }
            }
        };

        let expected_policy = crate::runtime_pin::policy_snapshots_from_events(&data.events);
        let mut referenced_policy_hashes = expected_policy
            .iter()
            .map(|row| row.hash.clone())
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(raw) = &data.plan_runtime_snapshot
            && let Some(snapshot) = crate::runtime_pin::parse_snapshot(raw)
            && !snapshot.policy_hash.is_empty()
        {
            referenced_policy_hashes.insert(snapshot.policy_hash);
        }
        for run in &data.runs {
            if let Some(snapshot) = &run.runtime_snapshot
                && !snapshot.policy_hash.is_empty()
            {
                referenced_policy_hashes.insert(snapshot.policy_hash.clone());
            }
        }
        let projected_policy = data
            .policy_snapshots
            .iter()
            .map(|row| (row.hash.clone(), row.json.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        let policy_snapshots_match = expected_policy.iter().all(|row| {
            projected_policy
                .get(&row.hash)
                .is_some_and(|json| json == &row.json)
        }) && referenced_policy_hashes
            .iter()
            .all(|hash| projected_policy.contains_key(hash));
        if !policy_snapshots_match {
            mismatches.push(format!(
                "policy snapshots: projection={}, replay={}, referenced={}",
                data.policy_snapshots.len(),
                expected_policy.len(),
                referenced_policy_hashes.len()
            ));
        }

        let consistent = status_matches
            && prompt_layout_matches
            && instructions_match
            && prefix_observations_match
            && prefix_aggregates_match
            && usage_matches
            && ledger_matches
            && mailbox_matches
            && artifacts_matches
            && approvals_matches
            && memories_matches
            && memory_conflicts_match
            && questions_matches
            && task_queue_matches
            && plan_matches
            && todos_matches
            && messages_matches
            && agents_matches
            && queue_matches
            && notes_match
            && decisions_match
            && scaling_decisions_match
            && recoveries_match
            && steering_matches
            && continuations_match
            && gates_match
            && runs_match
            && run_steps_match
            && run_checkpoints_match
            && context_checkpoints_match
            && processes_match
            && remote_test_evidence_matches
            && contracts_match
            && effects_match
            && policy_snapshots_match;
        Ok(SessionReplayReport {
            session_id: data.session.id.clone(),
            event_count: data.events.len(),
            status_matches,
            usage_matches,
            plan_matches,
            todos_matches,
            messages_matches,
            agents_matches,
            queue_matches,
            mailbox_matches,
            artifacts_matches,
            approvals_matches,
            ledger_matches,
            memories_matches,
            memory_conflicts_match,
            questions_matches,
            task_queue_matches,
            notes_match,
            decisions_match,
            scaling_decisions_match,
            recoveries_match,
            steering_matches,
            continuations_match,
            gates_match,
            runs_match,
            run_steps_match,
            run_checkpoints_match,
            context_checkpoints_match,
            processes_match,
            remote_test_evidence_matches,
            contracts_match,
            effects_match,
            policy_snapshots_match,
            consistent,
            repaired,
            rebuilt: false,
            mismatches,
        })
    }

    pub async fn replay_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        let data = self.load_replay_data(session_id).await?;
        Self::replay_report(&data, false)
    }

    pub async fn rebuild_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        ensure_session_not_active(&self.store, session_id, "rebuild a session").await?;
        let data = self.load_replay_data(session_id).await?;
        self.restore_policy_snapshots_from_events(&data.events)
            .await?;
        let repair = Self::build_session_repair(&data)?;
        for warning in &repair.artifact_warnings {
            tracing::warn!(session_id, %warning, "artifact replay warning");
        }
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
            "session.rebuilt",
            serde_json::json!({"event_count": data.events.len()}),
        )
        .await?;
        let data = self.load_replay_data(session_id).await?;
        let mut report = Self::replay_report(&data, true)?;
        report.rebuilt = true;
        Ok(report)
    }
}
