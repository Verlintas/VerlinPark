use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "project.add" => match parse_params::<AddProjectParams>(request.params) {
            Ok(params) => match state.engine.add_project(params.root, params.name).await {
                Ok(project) => RpcResponse::success(id, project),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.list" => match state.engine.list_projects().await {
            Ok(projects) => RpcResponse::success(id, ProjectListResult { projects }),
            Err(error) => internal_error(id, error),
        },
        "host.list" => match parse_params::<HostListParams>(request.params) {
            Ok(_) => match state.engine.list_execution_hosts().await {
                Ok(hosts) => RpcResponse::success(id, HostListResult { hosts }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "host.add" | "host.update" => match parse_params::<HostSetParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_execution_host(&params.project_id, params.config)
                .await
            {
                Ok(config) => RpcResponse::success(id, config),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "host.remove" => match parse_params::<HostProjectParams>(request.params) {
            Ok(params) => match state.engine.remove_execution_host(&params.project_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "host.probe" => match parse_params::<HostProbeParams>(request.params) {
            Ok(params) => match state
                .engine
                .probe_execution_host(&params.project_id, params.timeout_secs)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.status" => match parse_params::<RemoteStatusParams>(request.params) {
            Ok(params) => match state.engine.remote_status(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "lsp.status" => match parse_params::<LspStatusParams>(request.params) {
            Ok(params) => match state.engine.lsp_status(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.ledger" => match parse_params::<RemoteLedgerParams>(request.params) {
            Ok(params) => match state
                .engine
                .remote_ledger(&params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.worktree_list" => match parse_params::<RemoteWorktreeListParams>(request.params) {
            Ok(params) => match state.engine.remote_worktree_list(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.integrate" => match parse_params::<RemoteIntegrationParams>(request.params) {
            Ok(params) => match state.agent.integrate_remote(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_integration_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.bootstrap" => match parse_params::<RemoteBootstrapParams>(request.params) {
            Ok(params) => match state.engine.remote_bootstrap(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_bootstrap_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.bootstrap.status" => {
            match parse_params::<RemoteBootstrapStatusParams>(request.params) {
                Ok(params) => match state.engine.remote_bootstrap_status(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.test" => match parse_params::<RemoteTestParams>(request.params) {
            Ok(params) => match state.engine.run_remote_test(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_test_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.test.status" => match parse_params::<RemoteTestStatusParams>(request.params) {
            Ok(params) => match state.engine.remote_test_status(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_test_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.test.output" => match parse_params::<RemoteTestOutputParams>(request.params) {
            Ok(params) => match state.engine.remote_test_output(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_test_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.test.policy.get" => match parse_params::<RemoteTestPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .remote_test_policy_result(&params.project_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.test.policy.set" => {
            match parse_params::<SetRemoteTestPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_remote_test_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.integration.status" => {
            match parse_params::<RemoteIntegrationStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .remote_integration_status(&params.project_id, params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.integration.policy.get" => {
            match parse_params::<RemoteIntegrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .remote_integration_policy_result(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.integration.cleanup" => {
            match parse_params::<RemoteIntegrationCleanupParams>(request.params) {
                Ok(params) => match state.engine.remote_integration_cleanup(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.integration.policy.set" => {
            match parse_params::<SetRemoteIntegrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_remote_integration_policy(
                        &params.project_id,
                        params.policy,
                        params.confirm,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => remote_integration_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.task_scheduler.get" => {
            match parse_params::<TaskSchedulerPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_task_scheduler_policy(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.task_scheduler.set" => {
            match parse_params::<SetTaskSchedulerPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_task_scheduler_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.shell_policy.get" => match parse_params::<ShellPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_shell_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    ShellPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.shell_policy.set" => match parse_params::<SetShellPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_shell_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ShellPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.contract_policy.get" => {
            match parse_params::<ContractPolicyParams>(request.params) {
                Ok(params) => match state.engine.get_contract_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        ContractPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.contract_policy.set" => {
            match parse_params::<SetContractPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_contract_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        ContractPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.effect_policy.get" => match parse_params::<EffectPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_effect_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    EffectPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.effect_policy.set" => {
            match parse_params::<vp_protocol::types::SetEffectPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_effect_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        EffectPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.gate_policy.get" => match parse_params::<GatePolicyParams>(request.params) {
            Ok(params) => match state.engine.get_gate_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    GatePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.gate_policy.set" => match parse_params::<SetGatePolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_gate_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    GatePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.otel_policy.get" => match parse_params::<OtelPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_otel_policy(&params.project_id).await {
                Ok(policy) => {
                    let endpoint_valid = vp_core::otel::validate_endpoint(&policy.endpoint).is_ok();
                    RpcResponse::success(
                        id,
                        vp_protocol::otel::OtelPolicyResult {
                            project_id: params.project_id,
                            policy,
                            endpoint_valid,
                            endpoint_loopback: endpoint_valid,
                            warnings: Vec::new(),
                        },
                    )
                }
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.otel_policy.set" => match parse_params::<SetOtelPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_otel_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.mcp_pins" => match parse_params::<ProjectMcpPinsParams>(request.params) {
            Ok(params) => match state
                .engine
                .project_mcp_pins(&params.project_id, params.server_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.preemption_policy.get" => {
            match parse_params::<PreemptionPolicyParams>(request.params) {
                Ok(params) => match state.engine.get_preemption_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        PreemptionPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.preemption_policy.set" => {
            match parse_params::<SetPreemptionPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_preemption_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        PreemptionPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.cache_policy.get" => match parse_params::<CachePolicyParams>(request.params) {
            Ok(params) => match state.engine.get_cache_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    CachePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.cache_policy.set" => match parse_params::<SetCachePolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_cache_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    CachePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.integration_policy.get" => {
            match parse_params::<IntegrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_integration_policy(&params.project_id)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        IntegrationPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.integration_policy.set" => {
            match parse_params::<SetIntegrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_integration_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => {
                        let project_id = params.project_id.clone();
                        if policy.auto_resolve {
                            let agent = state.agent.clone();
                            tokio::spawn(async move {
                                agent.auto_resolve_project(&project_id).await;
                            });
                        }
                        RpcResponse::success(
                            id,
                            IntegrationPolicyResult {
                                project_id: params.project_id,
                                policy,
                            },
                        )
                    }
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.prompt_layout.set" => {
            match parse_params::<SetProjectPromptLayoutParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_project_prompt_layout(&params.project_id, params.prompt_layout)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.cache_epochs" => match parse_params::<ProjectCacheEpochParams>(request.params) {
            Ok(params) => match state
                .engine
                .project_cache_epochs(&params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.cache_epochs.prune" => {
            match parse_params::<ProjectCacheEpochPruneParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .prune_project_cache_epochs(
                        &params.project_id,
                        params.older_than_days,
                        params.keep_epochs,
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
        "project.usage" => match parse_params::<ProjectUsageParams>(request.params) {
            Ok(params) => match state.engine.project_usage_report(&params.project_id).await {
                Ok(report) => RpcResponse::success(id, report),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.cache" => match parse_params::<ProjectCacheParams>(request.params) {
            Ok(params) => match state.engine.project_cache_overview(&params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.budget.get" => match parse_params::<ProjectBudgetParams>(request.params) {
            Ok(params) => match state.engine.get_project_budget(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.budget.set" => match parse_params::<SetProjectBudgetParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_project_budget(&params.project_id, params.budget)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => validation_or_internal(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.auto_budget.get" => match parse_params::<AutoBudgetPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_project_auto_budget_policy(&params.project_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => budget_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.auto_budget.set" => {
            match parse_params::<SetAutoBudgetPolicyParams>(request.params) {
                Ok(params) => match state.engine.set_project_auto_budget_policy(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => budget_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.run_policy.get" => match parse_params::<ProjectRunPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_project_run_policy(&params.project_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => budget_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.run_policy.set" => {
            match parse_params::<SetProjectRunPolicyParams>(request.params) {
                Ok(params) => match state.engine.set_project_run_policy(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => budget_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.cleanup_worktrees" => {
            match parse_params::<CleanupWorktreesParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .cleanup_worktrees(
                        &params.project_id,
                        params.older_than_secs,
                        params.dry_run,
                        params.force,
                        params.delete_branches,
                        params.keep_failed,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.worktree_policy.get" => match parse_params::<WorktreePolicyParams>(request.params)
        {
            Ok(params) => match state.engine.get_worktree_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    vp_protocol::types::WorktreePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.worktree_policy.set" => {
            match parse_params::<SetWorktreePolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_worktree_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.checkpoint_policy.get" => {
            match parse_params::<CheckpointPolicyParams>(request.params) {
                Ok(params) => match state.engine.get_checkpoint_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        vp_protocol::types::CheckpointPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.checkpoint_policy.set" => {
            match parse_params::<SetCheckpointPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_checkpoint_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.fork.forest" => match parse_params::<ProjectForkForestParams>(request.params) {
            Ok(params) => match state.engine.project_fork_forest(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.fork.audit" => match parse_params::<ForkAuditParams>(request.params) {
            Ok(params) => match state.engine.audit_project_forks(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.fork.repair" => match parse_params::<ForkRepairParams>(request.params) {
            Ok(params) => match state.engine.repair_project_forks(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.retirement_policy.get" => {
            match parse_params::<AgentRetirementPolicyParams>(request.params) {
                Ok(params) => {
                    match state
                        .engine
                        .get_agent_retirement_policy(&params.project_id)
                        .await
                    {
                        Ok(result) => RpcResponse::success(id, result),
                        Err(error) => identity_error(id, error),
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "project.retirement_policy.set" => {
            match parse_params::<vp_protocol::types::SetAgentRetirementPolicyParams>(request.params)
            {
                Ok(params) => match state.engine.set_agent_retirement_policy(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.agent_identity_policy.get" => {
            match parse_params::<AgentIdentityPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_agent_identity_policy(&params.project_id)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        AgentIdentityPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.agent_identity_policy.set" => {
            match parse_params::<SetAgentIdentityPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_agent_identity_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        AgentIdentityPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}
