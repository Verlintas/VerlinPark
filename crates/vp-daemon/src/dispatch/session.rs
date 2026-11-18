use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "session.budget.governor" => match parse_params::<GovernorStatusParams>(request.params) {
            Ok(params) => match state
                .engine
                .budget_governor_status(&params.session_id, params.limit.unwrap_or(20))
                .await
            {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.contract" => match parse_params::<SessionContractParams>(request.params) {
            Ok(params) => {
                let task = params.task_id.as_deref();
                match state
                    .engine
                    .session_contract_status(&params.session_id, task)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "session.effects" => match parse_params::<SessionEffectsParams>(request.params) {
            Ok(params) => {
                let include_settled = params.include_settled.unwrap_or(true);
                match state
                    .engine
                    .session_effects(
                        &params.session_id,
                        params.task_id.as_deref(),
                        include_settled,
                        params.limit.unwrap_or(100),
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => effect_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "session.create" => match parse_params::<CreateSessionParams>(request.params) {
            Ok(params) => {
                let created = state
                    .engine
                    .create_session_with_options_and_unlimited(
                        params.project_id,
                        params.goal.clone(),
                        params.token_budget,
                        params.cost_budget_usd,
                        params.time_budget_secs,
                        params.priority.unwrap_or(0),
                        params.unlimited,
                    )
                    .await;
                match created {
                    Ok(session) => {
                        let info = state.agent.info();
                        let model = info
                            .role_models
                            .leader
                            .model
                            .clone()
                            .or(info.model)
                            .unwrap_or_else(|| "unconfigured".to_string());
                        // Do not report success for a session that has no
                        // Leader roster row or whose run could not start. The
                        // created session is kept and named in the error so
                        // the caller can inspect it.
                        if let Err(error) =
                            state.engine.create_leader_agent(&session.id, &model).await
                        {
                            return session_start_error(id, &session, "leader agent", error);
                        }
                        if let Err(error) = state.agent.start(&session.id, &params.goal).await {
                            tracing::warn!(%error, session_id = %session.id, "agent did not start");
                            return session_start_error(id, &session, "agent.start", error);
                        }
                        RpcResponse::success(id, session)
                    }
                    Err(error) => validation_or_internal(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "session.processes" => match parse_params::<ProcessListParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_processes(&params.session_id, params.include_finished, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.process.logs" => match parse_params::<ProcessLogsParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_process_logs(
                    &params.session_id,
                    &params.process_id,
                    params.offset,
                    params.max_bytes,
                    params.max_lines,
                    params.from_end,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.process.stop" => match parse_params::<ProcessStopParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_process_stop(
                    &params.session_id,
                    &params.process_id,
                    params.signal,
                    params.grace_secs,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.process.restart" => match parse_params::<ProcessRestartParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_process_restart(&params.session_id, &params.process_id, params.confirm)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.budget.set" => match parse_params::<SetSessionUnlimitedParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_session_unlimited(&params.session_id, params.unlimited)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.priority.set" => match parse_params::<SetSessionPriorityParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_session_priority(&params.session_id, params.priority)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => validation_or_internal(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.list" => match parse_params::<ListSessionsParams>(request.params) {
            Ok(params) => match state.engine.list_sessions(params.project_id).await {
                Ok(sessions) => RpcResponse::success(id, SessionListResult { sessions }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.citation_audit" => {
            match parse_params::<SessionCitationAuditParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .session_citation_audit(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.citations" => match parse_params::<SessionCitationsParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_citations(&params.session_id, params.message_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.transcript" => match parse_params::<SessionTranscriptParams>(request.params) {
            Ok(params) => match state.engine.session_transcript(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.search" => match parse_params::<SearchParams>(request.params) {
            Ok(params) => match state
                .engine
                .search(
                    &params.query,
                    params.project_id.as_deref(),
                    params.limit.unwrap_or(50),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.discard" => match parse_params::<DiscardSessionParams>(request.params) {
            Ok(params) => match state.engine.discard_session(&params.session_id).await {
                Ok(()) => RpcResponse::success(id, json!({"discarded": true})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.preempt" => match parse_params::<SessionPreemptParams>(request.params) {
            Ok(params) => match state
                .agent
                .preempt(&params.session_id, params.reason.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.preemption" => match parse_params::<SessionPreemptionParams>(request.params) {
            Ok(params) => match state
                .agent
                .preemption(&params.session_id, params.limit.unwrap_or(100))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cancel" => match parse_params::<CancelSessionParams>(request.params) {
            Ok(params) => RpcResponse::success(
                id,
                CancelSessionResult {
                    cancelled: state.agent.cancel(&params.session_id),
                },
            ),
            Err(response) => response_with_id(id, response),
        },
        "session.pause" => match parse_params::<SessionPauseParams>(request.params) {
            Ok(params) => match state.agent.pause(&params.session_id).await {
                Ok(paused) => RpcResponse::success(
                    id,
                    SessionPauseResult {
                        session_id: params.session_id,
                        paused,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.steer" => match parse_params::<SessionSteerParams>(request.params) {
            Ok(params) => match state
                .engine
                .steer_session(
                    &params.session_id,
                    &params.body,
                    params.agent_id.as_deref(),
                    params.task_id.as_deref(),
                    params.operation_id.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => operation_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.steering" => match parse_params::<SessionSteeringParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_steering(&params.session_id, params.status.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.steering.cancel" => {
            match parse_params::<SessionSteeringCancelParams>(request.params) {
                Ok(params) => match state.engine.cancel_steering(&params.id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.defer" => match parse_params::<SessionDeferParams>(request.params) {
            Ok(params) => match state
                .engine
                .defer_session_watching_gate(
                    &params.session_id,
                    &params.body,
                    params.watch_task_ids,
                    params.watch_mode.as_deref(),
                    params.deadline_at.as_deref(),
                    params.after_secs,
                    params.max_attempts,
                    params.every_secs,
                    params.max_runs,
                    params.watch_gate_id.as_deref(),
                    params.operation_id.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => operation_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.continuations" => match parse_params::<SessionContinuationsParams>(request.params)
        {
            Ok(params) => match state
                .engine
                .session_continuations(&params.session_id, params.status.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.continuation.cancel" => {
            match parse_params::<SessionContinuationCancelParams>(request.params) {
                Ok(params) => match state.engine.cancel_session_continuation(&params.id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.continuation.status" => {
            match parse_params::<SessionContinuationStatusParams>(request.params) {
                Ok(params) => match state.engine.session_continuation_status(&params.id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.continuation.reclaim" => {
            match parse_params::<SessionContinuationReclaimParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .reclaim_session_continuation(&params.id, params.reason.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.stalls" | "task.stalls" => {
            match parse_params::<SessionStallsParams>(request.params) {
                Ok(params) => match state.engine.session_stalls(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.progress.report" => {
            match parse_params::<vp_protocol::progress::ReportProgressParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .record_progress_round(&params.session_id, params.round)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.progress" => {
            match parse_params::<vp_protocol::progress::SessionProgressParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .session_progress(&params.session_id, params.plan_revision, params.limit)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context" => match parse_params::<SessionContextParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_context_status(&params.session_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.context.checkpoints" => {
            match parse_params::<ContextCheckpointListParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .list_context_checkpoints(&params.session_id, params.limit)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.checkpoint.detail" => {
            match parse_params::<ContextCheckpointDetailParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .context_checkpoint_detail(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.checkpoint" => {
            match parse_params::<ContextCheckpointCreateParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .context_checkpoint_now(&params.session_id, params.label)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.restore" => match parse_params::<ContextRestoreParams>(request.params) {
            Ok(params) => match state
                .agent
                .restore_context_checkpoint_now(
                    &params.session_id,
                    &params.checkpoint_id,
                    params.force,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.context.checkpoint.delete" => {
            match parse_params::<ContextCheckpointDeleteParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .delete_context_checkpoint(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.checkpoint.prune" => {
            match parse_params::<ContextCheckpointPruneParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .prune_context_checkpoints(
                        &params.session_id,
                        params.older_than_days,
                        params.keep_latest,
                        params.dry_run,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.policy" => {
            match parse_params::<ContextCheckpointPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_context_checkpoint_policy(&params.project_id)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        vp_protocol::types::ContextCheckpointPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.policy.set" => {
            match parse_params::<SetContextCheckpointPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_context_checkpoint_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.instructions" => match parse_params::<SessionInstructionsParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_instructions(&params.session_id, params.agent_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.prompt_layout.set" => {
            match parse_params::<SetSessionPromptLayoutParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_session_prompt_layout(&params.session_id, params.prompt_layout)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.cache" => match parse_params::<SessionCacheParams>(request.params) {
            Ok(params) => match state.engine.session_cache_status(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.epoch" => match parse_params::<SessionCacheEpochParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_cache_epoch(&params.session_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.calls" => match parse_params::<SessionCacheCallsParams>(request.params) {
            Ok(params) => match state.engine.session_cache_calls(&params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.audit" => match parse_params::<SessionCacheAuditParams>(request.params) {
            Ok(params) => match state.engine.session_cache_audit(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.breaks" => match parse_params::<SessionCacheBreaksParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_cache_breaks(&params.session_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.plan" => match parse_params::<SessionCachePlanParams>(request.params) {
            Ok(params) => match state
                .agent
                .cache_plan_now(&params.session_id, &params.roles)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.tool_bundle.history" => {
            match parse_params::<ToolBundleHistoryParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .tool_bundle_history(&params.session_id, params.role.as_deref(), params.limit)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.tool_bundle" => match parse_params::<ToolBundleParams>(request.params) {
            Ok(params) => match state
                .agent
                .tool_bundle_now(
                    &params.session_id,
                    &params.roles,
                    params.task_kind.as_deref(),
                    params.include_tools.unwrap_or(true),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.warm" => match parse_params::<SessionCacheWarmParams>(request.params) {
            Ok(params) => match state
                .agent
                .warm_cache_now(&params.session_id, params.role.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.compact" => match parse_params::<SessionCompactParams>(request.params) {
            Ok(params) => match state.agent.compact_now(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.checkpoint" => match parse_params::<SessionCheckpointParams>(request.params) {
            Ok(params) => match state
                .agent
                .checkpoint_now(&params.session_id, params.label)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.checkpoints" => {
            match parse_params::<SessionCheckpointListParams>(request.params) {
                Ok(params) => match state.engine.list_checkpoints(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.stats" => {
            match parse_params::<SessionCheckpointStatsParams>(request.params) {
                Ok(params) => match state.engine.checkpoint_stats(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.diff" => {
            match parse_params::<SessionCheckpointDiffParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .diff_checkpoint_now(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.restore" => {
            match parse_params::<SessionCheckpointRestoreParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .restore_checkpoint_now(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.delete" => {
            match parse_params::<SessionCheckpointDeleteParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .delete_checkpoint(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.prune" => {
            match parse_params::<SessionCheckpointPruneParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .prune_checkpoints(
                        &params.session_id,
                        params.older_than_days,
                        params.keep_latest,
                        params.dry_run,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.fork.tree" => match parse_params::<SessionForkTreeParams>(request.params) {
            Ok(params) => match state.engine.session_fork_tree(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.lineage" => match parse_params::<SessionLineageParams>(request.params) {
            Ok(params) => match state.engine.session_lineage(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.compare" => match parse_params::<SessionCompareParams>(request.params) {
            Ok(params) => match state
                .engine
                .compare_sessions(&params.session_id, &params.other_session_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.fork.merge" => match parse_params::<SessionMergeForkParams>(request.params) {
            Ok(params) => match state
                .agent
                .merge_fork_now(&params.source_session_id, &params.fork_session_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.fork" => match parse_params::<SessionForkParams>(request.params) {
            Ok(params) => match state
                .agent
                .fork_now(
                    &params.session_id,
                    params.checkpoint_id.as_deref(),
                    params.goal,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.plan_resume" => match parse_params::<SessionPlanResumeParams>(request.params) {
            Ok(params) => match state.agent.resume_plan(&params.session_id).await {
                Ok(started) => RpcResponse::success(
                    id,
                    SessionPlanResumeResult {
                        session_id: params.session_id,
                        started,
                    },
                ),
                Err(error) => runtime_pin_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.resume" => match parse_params::<SessionResumeParams>(request.params) {
            Ok(params) => match state.agent.resume(&params.session_id).await {
                Ok(resumed) => RpcResponse::success(
                    id,
                    SessionResumeResult {
                        session_id: params.session_id,
                        resumed,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.runs" => match parse_params::<SessionRunsParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_runs(&params.session_id, params.limit.unwrap_or(50))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.detail" => match parse_params::<SessionRunDetailParams>(request.params) {
            Ok(params) => match state.engine.session_run_detail(&params.run_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.compatibility" => {
            match parse_params::<SessionCompatibilityParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .session_compatibility(&params.session_id, params.run_id.as_deref())
                    .await
                {
                    Ok(report) => RpcResponse::success(id, report),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.diagnosis" => match parse_params::<SessionDiagnosisParams>(request.params) {
            Ok(params) => match state.engine.session_diagnosis(params).await {
                Ok(report) => RpcResponse::success(id, report),
                Err(error) => diagnosis_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.diagnosis.record" => {
            match parse_params::<SessionDiagnosisRecordParams>(request.params) {
                Ok(params) => match state.engine.record_session_diagnosis(params).await {
                    Ok(report) => RpcResponse::success(id, report),
                    Err(error) => diagnosis_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.attributions" => match parse_params::<SessionAttributionsParams>(request.params) {
            Ok(params) => match state.engine.session_attributions(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => diagnosis_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.resume" => match parse_params::<SessionRunResumeParams>(request.params) {
            Ok(params) => match state
                .agent
                .resume_session_run(
                    &params.session_id,
                    params.run_id.as_deref(),
                    params.operation_id.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => runtime_pin_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.cancel" => match parse_params::<SessionRunCancelParams>(request.params) {
            Ok(params) => match state
                .agent
                .cancel_session_run(&params.session_id, params.run_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.policy.get" => match parse_params::<SessionRunPolicyParams>(request.params) {
            Ok(params) => match state.engine.session_run_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.policy.set" => {
            match parse_params::<SetSessionRunPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_session_run_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.commit" => match parse_params::<CommitSessionParams>(request.params) {
            Ok(params) => match state
                .engine
                .commit_session(&params.session_id, &params.message)
                .await
            {
                Ok(commit) => RpcResponse::success(id, commit),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.todos" => match parse_params::<SessionTodosParams>(request.params) {
            Ok(params) => match state.engine.list_todos(&params.session_id).await {
                Ok(items) => RpcResponse::success(
                    id,
                    SessionTodosResult {
                        session_id: params.session_id,
                        items,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.log_status" => match parse_params::<SessionLogParams>(request.params) {
            Ok(params) => match state.engine.session_log_status(&params.session_id).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.restore_log" => match parse_params::<SessionLogParams>(request.params) {
            Ok(params) => match state.engine.restore_session_log(&params.session_id).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.tool_activity" => match parse_params::<ToolActivityParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_tool_activity(&params.session_id, params.limit.unwrap_or(50))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.usage" => match parse_params::<UsageLedgerParams>(request.params) {
            Ok(params) => match state.engine.usage_ledger(&params.session_id).await {
                Ok(ledger) => RpcResponse::success(id, ledger),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.usage_csv" => match parse_params::<UsageLedgerParams>(request.params) {
            Ok(params) => match state.engine.usage_csv(&params.session_id).await {
                Ok(csv) => RpcResponse::success(id, csv),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.diff" => match parse_params::<SessionDiffParams>(request.params) {
            Ok(params) => match state.engine.session_diff(&params.session_id).await {
                Ok(diff) => RpcResponse::success(id, diff),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.assignments" => match parse_params::<SessionAssignmentsParams>(request.params) {
            Ok(params) => match state.engine.session_assignments(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.assign" => match parse_params::<SessionAssignParams>(request.params) {
            Ok(params) => match state.engine.assign_session_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.assignment.renew" => {
            match parse_params::<SessionAssignmentRenewParams>(request.params) {
                Ok(params) => match state.engine.renew_session_assignment(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => coordination_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.release" => match parse_params::<SessionReleaseParams>(request.params) {
            Ok(params) => match state.engine.release_session_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.workspace" => match parse_params::<SessionWorkspaceParams>(request.params) {
            Ok(params) => match state.engine.session_workspace(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => workspace_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.land" => match parse_params::<LandSessionParams>(request.params) {
            Ok(params) => match state.engine.land_session(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => effect_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.pr" => match parse_params::<OpenPullRequestParams>(request.params) {
            Ok(params) => match state
                .engine
                .open_pull_request_with_provider(&params.session_id, params.provider.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => forge_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.export" => match parse_params::<SessionExportParams>(request.params) {
            Ok(params) => match state.engine.export_session(&params.session_id).await {
                Ok(bundle) => RpcResponse::success(id, bundle),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.import" => match parse_params::<SessionImportParams>(request.params) {
            Ok(params) => match state.engine.import_session(params.bundle).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.rehydrate" => match parse_params::<RehydrateSessionParams>(request.params) {
            Ok(params) => match state
                .engine
                .rehydrate_session(&params.session_id, params.worktree)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.replay" => match parse_params::<ReplaySessionParams>(request.params) {
            Ok(params) => {
                let result = if params.rebuild {
                    state.engine.rebuild_session(&params.session_id).await
                } else if params.repair {
                    state.engine.repair_session(&params.session_id).await
                } else {
                    state.engine.replay_session(&params.session_id).await
                };
                match result {
                    Ok(report) => RpcResponse::success(id, report),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "session.prompt" => match parse_params::<PromptParams>(request.params) {
            Ok(params) => match state
                .agent
                .start_with_operation(
                    &params.session_id,
                    &params.content,
                    params.operation_id.as_deref(),
                )
                .await
            {
                Ok(()) => RpcResponse::success(id, PromptResult { accepted: true }),
                Err(error) => operation_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}
