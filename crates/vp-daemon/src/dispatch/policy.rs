use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "runtime.leases" => match state.engine.list_runtime_leases().await {
            Ok(leases) => {
                let identity = state.engine.daemon_identity();
                let result = RuntimeLeaseListResult {
                    runtime_id: state.engine.runtime_id().to_string(),
                    daemon_instance: Some(DaemonInstanceInfo {
                        instance_id: identity.instance_id.clone(),
                        pid: identity.pid,
                        started_at: identity.started_at.clone(),
                    }),
                    leases: leases
                        .into_iter()
                        .map(|lease| RuntimeLeaseInfo {
                            state: lease.state().as_str().to_string(),
                            lease_key: lease.lease_key,
                            owner_id: lease.owner_id,
                            owner_instance: lease.owner_instance,
                            kind: lease.kind,
                            session_id: lease.session_id,
                            fencing_token: lease.fencing_token,
                            acquired_at: lease.acquired_at,
                            heartbeat_at: lease.heartbeat_at,
                            renewed_at: lease.renewed_at,
                            expires_at: lease.expires_at,
                            released_at: lease.released_at,
                        })
                        .collect(),
                };
                RpcResponse::success(id, result)
            }
            Err(error) => internal_error(id, error),
        },
        "runtime.leases.sweep" => match state.engine.sweep_runtime_leases().await {
            Ok(removed) => RpcResponse::success(id, RuntimeLeaseSweepResult { removed }),
            Err(error) => internal_error(id, error),
        },
        "runtime.leases.history" => {
            match parse_params::<RuntimeLeaseHistoryParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .runtime_lease_audit(params.lease_key.as_deref(), params.limit)
                    .await
                {
                    Ok(entries) => RpcResponse::success(
                        id,
                        RuntimeLeaseHistoryResult {
                            lease_key: params.lease_key,
                            entries: entries
                                .into_iter()
                                .map(|entry| RuntimeLeaseAuditInfo {
                                    id: entry.id,
                                    lease_key: entry.lease_key,
                                    kind: entry.kind,
                                    action: entry.action,
                                    owner_id: entry.owner_id,
                                    owner_instance: entry.owner_instance,
                                    fencing_token: entry.fencing_token,
                                    detail: entry.detail,
                                    occurred_at: entry.occurred_at,
                                })
                                .collect(),
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "effect.reconcile" => match parse_params::<EffectReconcileParams>(request.params) {
            Ok(params) => match state.engine.reconcile_effect(params).await {
                Ok(result) => {
                    let _ = state.engine.record_project_manifest().await;
                    RpcResponse::success(
                        id,
                        EffectReconcileResult {
                            receipt: result.receipt,
                            changed: result.changed,
                        },
                    )
                }
                Err(error) => effect_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "effect.sweep" => {
            #[derive(serde::Deserialize)]
            struct EffectSweepParams {
                #[serde(default)]
                limit: Option<u32>,
            }
            match parse_params::<EffectSweepParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .sweep_stale_effect_intents(params.limit.unwrap_or(100))
                    .await
                {
                    Ok(swept) => RpcResponse::success(id, json!({"swept": swept})),
                    Err(error) => effect_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "gate.open" => match parse_params::<GateOpenParams>(request.params) {
            Ok(params) => match state.engine.open_gate(params).await {
                Ok(outcome) => RpcResponse::success(id, vp_core::gate::open_result(outcome)),
                Err(error) => gate_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.resolve" => match parse_params::<GateResolveParams>(request.params) {
            Ok(params) => match state
                .engine
                .resolve_gate(
                    &params.gate_id,
                    &params.token,
                    params.outcome.as_deref(),
                    params.note.as_deref(),
                    params.resolved_by.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => gate_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.cancel" => match parse_params::<GateCancelParams>(request.params) {
            Ok(params) => match state
                .engine
                .cancel_gate(
                    &params.gate_id,
                    params.reason.as_deref(),
                    params.cancelled_by.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => gate_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.list" => match parse_params::<GateListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_gates(&params.session_id, params.state.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => gate_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.status" => match parse_params::<GateStatusParams>(request.params) {
            Ok(params) => match state.engine.gate_status(&params.gate_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => gate_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.sweep" => match parse_params::<GateSweepParams>(request.params) {
            Ok(params) => match state
                .engine
                .sweep_expired_gates(params.limit.unwrap_or(100))
                .await
            {
                Ok(expired) => RpcResponse::success(
                    id,
                    vp_protocol::types::GateSweepResult {
                        expired_gates: expired as u64,
                    },
                ),
                Err(error) => gate_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "telemetry.otel.status" => match parse_params::<OtelStatusParams>(request.params) {
            Ok(params) => match state
                .engine
                .otel_status(&params.project_id, params.session_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "telemetry.otel.export" => match parse_params::<OtelExportParams>(request.params) {
            Ok(params) => match state
                .engine
                .otel_export(&params.session_id, params.limit, params.offset)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "telemetry.otel.test" => match parse_params::<OtelTestParams>(request.params) {
            Ok(params) => match state.engine.otel_test(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "preemption.overview" => match parse_params::<PreemptionOverviewParams>(request.params) {
            Ok(params) => match state
                .engine
                .preemption_overview(&params.project_id, params.limit.unwrap_or(100))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_bundle.catalog" => RpcResponse::success(id, state.agent.tool_bundle_catalog()),
        "tool_bundle.matrix" => match parse_params::<ToolBundleMatrixParams>(request.params) {
            Ok(params) => match state.agent.tool_bundle_matrix(&params.task_kinds) {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_bundle.diff" => match parse_params::<ToolBundleDiffParams>(request.params) {
            Ok(params) => match state.agent.tool_bundle_diff(
                &params.from_bundle,
                &params.to_bundle,
                params.role.as_deref(),
                params.profile.as_deref(),
                params.task_kind.as_deref(),
            ) {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "model.preset.list" => match state.engine.list_model_presets().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "model.preset.create" => match parse_params::<CreateModelPresetParams>(request.params) {
            Ok(params) => match state.engine.create_model_preset(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "model.preset.update" => match parse_params::<UpdateModelPresetParams>(request.params) {
            Ok(params) => match state.engine.update_model_preset(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "model.preset.delete" => match parse_params::<DeleteModelPresetParams>(request.params) {
            Ok(params) => match state.engine.delete_model_preset(&params.id).await {
                Ok(removed) => RpcResponse::success(
                    id,
                    vp_protocol::types::DeleteModelPresetResult { removed },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "question.list" => match parse_params::<QuestionListParams>(request.params) {
            Ok(params) => match state.engine.list_questions(params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "question.answer" => match parse_params::<AnswerQuestionParams>(request.params) {
            Ok(params) => match state
                .engine
                .answer_question(&params.question_id, &params.answer)
                .await
            {
                Ok(answered) => RpcResponse::success(id, AnswerQuestionResult { answered }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.scope.preview" => {
            match parse_params::<TestCacheScopePreviewParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .preview_test_cache_scopes(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "test_cache.policy.get" => match parse_params::<TestCachePolicyParams>(request.params) {
            Ok(params) => match state.engine.get_test_cache_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    vp_protocol::test_cache::TestCachePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.policy.set" => match parse_params::<SetTestCachePolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_test_cache_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.stats" => match parse_params::<TestCacheStatsParams>(request.params) {
            Ok(params) => match state.engine.test_cache_stats(&params.project_id).await {
                Ok(stats) => RpcResponse::success(id, stats),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.entries" => match parse_params::<TestCacheEntriesParams>(request.params) {
            Ok(params) => match state.engine.list_test_cache_entries(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.prune" => match parse_params::<TestCachePruneParams>(request.params) {
            Ok(params) => match state.engine.prune_test_cache(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.clear" => match parse_params::<TestCacheClearParams>(request.params) {
            Ok(params) => match state.engine.clear_test_cache(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.impact" => match parse_params::<TestCacheImpactParams>(request.params) {
            Ok(params) => match state.engine.test_cache_impact(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.grants" => match parse_params::<ApprovalGrantListParams>(request.params) {
            Ok(params) => match state.engine.list_approval_grants(&params.project_id).await {
                Ok(grants) => RpcResponse::success(id, ApprovalGrantListResult { grants }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.grant.create" => {
            match parse_params::<ApprovalGrantCreateParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .create_approval_grant(
                        &params.project_id,
                        &params.tool,
                        &params.path_prefix,
                        params.include_children,
                        "user",
                    )
                    .await
                {
                    Ok(grant) => RpcResponse::success(id, ApprovalGrantCreateResult { grant }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.grant.delete" => {
            match parse_params::<ApprovalGrantDeleteParams>(request.params) {
                Ok(params) => match state.engine.delete_approval_grant(&params.id).await {
                    Ok(deleted) => RpcResponse::success(id, ApprovalGrantDeleteResult { deleted }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.list" => {
            let params: ListApprovalsParams = if request.params.is_null() {
                ListApprovalsParams { session_id: None }
            } else {
                match serde_json::from_value(request.params) {
                    Ok(params) => params,
                    Err(error) => {
                        return RpcResponse::error(
                            id,
                            RpcError::new(codes::INVALID_PARAMS, error.to_string()),
                        );
                    }
                }
            };
            match state.engine.list_approvals(params.session_id).await {
                Ok(approvals) => RpcResponse::success(id, ApprovalListResult { approvals }),
                Err(error) => internal_error(id, error),
            }
        }
        "approval.resolve" => match parse_params::<ResolveApprovalParams>(request.params) {
            Ok(params) => match state
                .engine
                .resolve_approval(&params.approval_id, params.approved)
                .await
            {
                Ok(resolved) => RpcResponse::success(id, ResolveApprovalResult { resolved }),
                Err(error) => approval_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.queue" => match parse_params::<ApprovalQueueParams>(request.params) {
            Ok(params) => match state
                .engine
                .approval_queue(params.session_id, params.project_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => approval_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.batch.resolve" => {
            match parse_params::<ApprovalBatchResolveParams>(request.params) {
                Ok(params) => {
                    let decisions = params
                        .decisions
                        .into_iter()
                        .map(|decision| (decision.approval_id, decision.approved))
                        .collect::<Vec<_>>();
                    match state.engine.resolve_approvals(decisions).await {
                        Ok(result) => RpcResponse::success(
                            id,
                            ApprovalBatchResolveResult {
                                results: result.results,
                                resolved_count: result.resolved_count,
                                failed_count: result.failed_count,
                            },
                        ),
                        Err(error) => approval_error(id, error),
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.templates" | "approval.template.list" => {
            match parse_params::<ApprovalTemplateListParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .list_approval_templates(&params.project_id)
                    .await
                {
                    Ok(templates) => {
                        RpcResponse::success(id, ApprovalTemplateListResult { templates })
                    }
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.template.create" => {
            match parse_params::<ApprovalTemplateCreateParams>(request.params) {
                Ok(params) => match state.engine.create_approval_template(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.template.update" => {
            match parse_params::<ApprovalTemplateUpdateParams>(request.params) {
                Ok(params) => match state.engine.update_approval_template(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.template.delete" => {
            match parse_params::<ApprovalTemplateDeleteParams>(request.params) {
                Ok(params) => match state.engine.delete_approval_template(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.template.from_approval" => {
            match parse_params::<ApprovalTemplateFromApprovalParams>(request.params) {
                Ok(params) => match state.engine.approval_template_from_approval(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.policy" => match parse_params::<AttentionPolicyGetParams>(request.params) {
            Ok(params) => match state.engine.get_attention_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => approval_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.policy.set" => match parse_params::<AttentionPolicySetParams>(request.params) {
            Ok(params) => match state.engine.set_attention_policy(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => approval_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.audit" => match parse_params::<ApprovalAuditParams>(request.params) {
            Ok(params) => {
                match state
                    .engine
                    .approval_audit(&params.project_id, params.limit)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}
