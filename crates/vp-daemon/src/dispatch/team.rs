use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "scaling.policy.get" => match parse_params::<ScalingPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_scaling_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "scaling.policy.set" => match parse_params::<SetScalingPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_scaling_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "scaling.preview" => match parse_params::<ScalingPreviewParams>(request.params) {
            Ok(params) => match state.agent.scaling_preview(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "scaling.status" => match parse_params::<ScalingStatusParams>(request.params) {
            Ok(params) => match state
                .agent
                .scaling_status(
                    params.project_id.as_deref(),
                    params.session_id.as_deref(),
                    params.limit,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "orchestration.policy.get" => {
            match parse_params::<OrchestrationPolicyParams>(request.params) {
                Ok(params) => match state.engine.orchestration_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        OrchestrationPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.policy.set" => {
            match parse_params::<SetOrchestrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_orchestration_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        OrchestrationPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "critical_path.policy.get" => {
            match parse_params::<CriticalPathPolicyParams>(request.params) {
                Ok(params) => match state.engine.critical_path_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        CriticalPathPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "critical_path.policy.set" => {
            match parse_params::<SetCriticalPathPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_critical_path_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        CriticalPathPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "governor.preview" => match parse_params::<GovernorPreviewParams>(request.params) {
            Ok(params) => {
                let mode = match params.mode.as_deref() {
                    None => None,
                    Some(value) => match CriticalPathMode::parse(value) {
                        Some(mode) => Some(mode),
                        None => {
                            return RpcResponse::error(
                                id,
                                RpcError::new(
                                    codes::INVALID_PARAMS,
                                    format!(
                                        "invalid governor preview mode {value:?}; use `single` or `multi`"
                                    ),
                                ),
                            );
                        }
                    },
                };
                match state
                    .engine
                    .preview_budget_governor_with_mode(
                        &params.session_id,
                        params.remaining_tokens,
                        params.limit.unwrap_or(20),
                        mode,
                    )
                    .await
                {
                    Ok(status) => RpcResponse::success(id, status),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "coordination.note" => match parse_params::<CoordinationNoteParams>(request.params) {
            Ok(params) => match state
                .engine
                .record_user_coordination_note(
                    &params.session_id,
                    &params.action,
                    params.task_id.as_deref(),
                    params.target_agent_id.as_deref(),
                    params.target_note_id.as_deref(),
                    &params.message,
                    params.severity.as_deref(),
                )
                .await
            {
                Ok(note_id) => RpcResponse::success(id, CoordinationNoteResult { note_id }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "orchestration.overview" => {
            match parse_params::<OrchestrationOverviewParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .orchestration_overview(
                        params.project_id.as_deref(),
                        params.limit.unwrap_or(20),
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.preview" => {
            match parse_params::<OrchestrationPreviewParams>(request.params) {
                Ok(params) => match state.engine.preview_orchestration(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.advance" => {
            match parse_params::<OrchestrationAdvanceParams>(request.params) {
                Ok(params) => match state.engine.advance_orchestration(&params.session_id).await {
                    Ok(actions) => RpcResponse::success(
                        id,
                        OrchestrationAdvanceResult {
                            session_id: params.session_id,
                            actions,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.replay" => match parse_params::<ControllerReplayParams>(request.params) {
            Ok(params) => match state
                .engine
                .replay_orchestration(&params.session_id, params.limit)
                .await
            {
                Ok(report) => RpcResponse::success(id, report),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "orchestration.snapshot" => {
            match parse_params::<OrchestrationSnapshotParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .orchestration_snapshot(&params.session_id)
                    .await
                {
                    Ok(snapshot) => {
                        RpcResponse::success(id, OrchestrationSnapshotResult { snapshot })
                    }
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.recoveries" => {
            match parse_params::<OrchestrationRecoveriesParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .orchestration_recoveries(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "team_plan.revisions" => match parse_params::<PlanRevisionListParams>(request.params) {
            Ok(params) => match state.engine.plan_revisions(&params.session_id).await {
                Ok(revisions) => RpcResponse::success(id, PlanRevisionListResult { revisions }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.revision" => match parse_params::<PlanRevisionGetParams>(request.params) {
            Ok(params) => match state
                .agent
                .plan_revision_detail(&params.session_id, params.revision)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.restore" => match parse_params::<PlanRevisionRestoreParams>(request.params) {
            Ok(params) => match state
                .agent
                .restore_team_plan_revision(
                    &params.session_id,
                    params.revision,
                    &params.reason,
                    params.expected_revision,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.task_edit" => match parse_params::<TaskEditParams>(request.params) {
            Ok(params) => match state
                .agent
                .edit_plan_task(
                    &params.session_id,
                    params.task,
                    &params.reason,
                    params.expected_revision,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "plan.fanout" => match parse_params::<vp_protocol::types::PlanFanoutParams>(request.params)
        {
            Ok(params) => match state.engine.fanout_plan(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => fanout_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.revise" => match parse_params::<ReviseTeamPlanParams>(request.params) {
            Ok(params) => match state
                .agent
                .revise_team_plan(
                    &params.session_id,
                    params.plan,
                    &params.reason,
                    params.expected_revision,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "scheduler.status" => match parse_params::<SchedulerStatusParams>(request.params) {
            Ok(params) => match state.agent.scheduler_status(params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.list" => match parse_params::<IntakeListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_intake_tasks(params.project_id, params.status, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.create" => match parse_params::<IntakeCreateParams>(request.params) {
            Ok(params) => match state.engine.create_intake_task(params).await {
                Ok(task) => RpcResponse::success(id, task),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.update" => match parse_params::<IntakeUpdateParams>(request.params) {
            Ok(params) => match state.engine.update_intake_task(params).await {
                Ok(task) => RpcResponse::success(id, task),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.dispatch" => match parse_params::<IntakeTaskParams>(request.params) {
            Ok(params) => match state.agent.dispatch_intake_task(&params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.cancel" => match parse_params::<IntakeTaskParams>(request.params) {
            Ok(params) => match state.agent.cancel_intake_task(&params.id).await {
                Ok(task) => RpcResponse::success(id, task),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.retry" => match parse_params::<IntakeTaskParams>(request.params) {
            Ok(params) => match state.agent.retry_intake_task(&params.id).await {
                Ok(task) => RpcResponse::success(id, task),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.delete" => match parse_params::<IntakeTaskParams>(request.params) {
            Ok(params) => match state.agent.delete_intake_task(&params.id).await {
                Ok(()) => RpcResponse::success(id, IntakeDeleteResult { deleted: true }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.list" => match parse_params::<ProgramListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_programs(params.project_id, params.status, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.create" => match parse_params::<ProgramCreateParams>(request.params) {
            Ok(params) => match state.engine.create_program(params).await {
                Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.update" => match parse_params::<ProgramUpdateParams>(request.params) {
            Ok(params) => match state.engine.update_program(params).await {
                Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.pause" => match parse_params::<ProgramActionParams>(request.params) {
            Ok(params) => match state.engine.pause_program(params).await {
                Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.resume" => match parse_params::<ProgramActionParams>(request.params) {
            Ok(params) => match state.engine.resume_program(params).await {
                Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.cancel" => match parse_params::<ProgramActionParams>(request.params) {
            Ok(params) => {
                let reason = params
                    .reason
                    .unwrap_or_else(|| "cancelled from program.cancel".to_string());
                match state
                    .agent
                    .cancel_program(&params.program_id, &reason)
                    .await
                {
                    Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "program.delete" => match parse_params::<ProgramDeleteParams>(request.params) {
            Ok(params) => match state.engine.delete_program(params).await {
                Ok(deleted) => RpcResponse::success(id, ProgramDeleteResult { deleted }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.status" => match parse_params::<ProgramStatusParams>(request.params) {
            Ok(params) => match state.engine.program_status(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.milestone.list" => match parse_params::<ProgramParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_program_milestones(params.program_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.milestone.create" => {
            match parse_params::<ProgramMilestoneCreateParams>(request.params) {
                Ok(params) => match state.engine.create_program_milestone(params).await {
                    Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "program.milestone.update" => {
            match parse_params::<ProgramMilestoneUpdateParams>(request.params) {
                Ok(params) => match state.engine.update_program_milestone(params).await {
                    Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "program.milestone.retry" => {
            match parse_params::<ProgramMilestoneActionParams>(request.params) {
                Ok(params) => match state.engine.retry_program_milestone(params).await {
                    Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "program.milestone.skip" => {
            match parse_params::<ProgramMilestoneActionParams>(request.params) {
                Ok(params) => match state.engine.skip_program_milestone(params).await {
                    Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "program.milestone.start" => match parse_params::<ProgramMilestoneParams>(request.params) {
            Ok(params) => match state
                .agent
                .start_program_milestone(&params.milestone_id)
                .await
            {
                Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.replay" => match parse_params::<ProgramReplayParams>(request.params) {
            Ok(params) => match state
                .engine
                .replay_program(&params.program_id, params.repair)
                .await
            {
                Ok(report) => RpcResponse::success(id, report),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.tick" => match parse_params::<ProgramTickParams>(request.params) {
            Ok(params) => match state
                .agent
                .reconcile_programs(params.project_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.queue" => match parse_params::<TaskQueueParams>(request.params) {
            Ok(params) => match state
                .engine
                .task_queue(params.project_id, params.session_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.dispatch" => match parse_params::<TaskDispatchParams>(request.params) {
            Ok(params) => match state.agent.dispatch_next_task(&params.session_id).await {
                Ok(started) => RpcResponse::success(id, TaskDispatchResult { started }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.retry" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .retry_plan_task(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.pause" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .pause_plan_task(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.resume" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .resume_plan_task(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.cancel" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .request_plan_task_cancel(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.skip" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .skip_plan_task(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "a2a.status" => match parse_params::<vp_protocol::a2a::A2aStatusParams>(request.params) {
            Ok(_) => RpcResponse::success(id, state.engine.a2a_status()),
            Err(response) => response_with_id(id, response),
        },
        "a2a.policy.set" => {
            match parse_params::<vp_protocol::a2a::A2aPolicySetParams>(request.params) {
                Ok(params) => match state.engine.a2a_set_policy(params.policy) {
                    Ok(status) => RpcResponse::success(id, status),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "a2a.card" => match parse_params::<vp_protocol::a2a::A2aCardParams>(request.params) {
            Ok(_) => RpcResponse::success(id, serde_json::json!({"card": state.engine.a2a_card()})),
            Err(response) => response_with_id(id, response),
        },
        "a2a.tasks" => match parse_params::<vp_protocol::a2a::A2aTaskListParams>(request.params) {
            Ok(params) => RpcResponse::success(
                id,
                state
                    .engine
                    .a2a_tasks(params.limit.unwrap_or(50), params.state.as_deref()),
            ),
            Err(response) => response_with_id(id, response),
        },
        "a2a.start" => match parse_params::<vp_protocol::a2a::A2aStartParams>(request.params) {
            Ok(params) => match state.engine.a2a_start(params.approved, params.policy).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "a2a.stop" => match parse_params::<vp_protocol::a2a::A2aStopParams>(request.params) {
            Ok(params) => match state.engine.a2a_stop(params.reason.as_deref()).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "plan.migrate" => match parse_params::<PlanMigrateParams>(request.params) {
            Ok(params) => match state.engine.migrate_plan(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => runtime_pin_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.get" => match parse_params::<TeamPlanParams>(request.params) {
            Ok(params) => match state.engine.get_team_plan(&params.session_id).await {
                Ok(plan) => RpcResponse::success(id, TeamPlanResult { plan }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team.template.export" => match parse_params::<TeamTemplateExportParams>(request.params) {
            Ok(params) => match state.engine.export_team_template(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team.template.preview" => match parse_params::<TeamTemplateImportParams>(request.params) {
            Ok(params) => {
                let known_models = template_known_models(state).await;
                match state
                    .engine
                    .preview_team_template(params, known_models)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "team.template.import" => match parse_params::<TeamTemplateImportParams>(request.params) {
            Ok(params) => {
                let known_models = template_known_models(state).await;
                match state
                    .engine
                    .import_team_template(params, known_models)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "team.template.list" => match parse_params::<TeamTemplateListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_team_templates(params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team.template.get" => match parse_params::<TeamTemplateGetParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_team_template(&params.template_id, params.project_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team.template.diff" => match parse_params::<TeamTemplateDiffParams>(request.params) {
            Ok(params) => {
                let known_models = template_known_models(state).await;
                match state
                    .engine
                    .diff_team_template(&params.project_id, &params.template_id, known_models)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "team.template.replay" => match parse_params::<TeamTemplateReplayParams>(request.params) {
            Ok(params) => match state
                .engine
                .replay_team_templates(&params.project_id, params.repair)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}
