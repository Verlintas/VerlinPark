use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "workspace.tree" => match parse_params::<WorkspaceTreeParams>(request.params) {
            Ok(params) => match state.engine.workspace_tree(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        // The historical `workspace.list` method is the file-browser listing.
        // The S4 agent workspace list reuses the same method name only when the
        // params carry an agent-shape selector; otherwise the file browser keeps
        // the exact legacy behavior. `agent.workspace.list` is the canonical
        // S4 name.
        "workspace.list"
            if request.params.get("project_id").is_some()
                || request.params.get("identity_id").is_some()
                || request.params.get("status").is_some() =>
        {
            match parse_params::<AgentWorkspaceListParams>(request.params) {
                Ok(params) => match state.engine.list_agent_workspaces(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "workspace.list" => match parse_params::<WorkspaceListParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_list(
                    &params.session_id,
                    &params.path,
                    params.offset,
                    params.limit,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.search" => match parse_params::<WorkspaceSearchParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_search(&params.session_id, &params.query, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.grep" => match parse_params::<WorkspaceGrepParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_grep(
                    &params.session_id,
                    &params.query,
                    params.path,
                    params.case_sensitive,
                    params.max_results,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.file" => match parse_params::<WorkspaceFileParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_file(&params.session_id, &params.path)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.write" => match parse_params::<WorkspaceWriteParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_write(
                    &params.session_id,
                    &params.path,
                    params.content,
                    params.expected_sha256,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.info" => RpcResponse::success(id, state.agent.info()),
        "agent.probe" => match state.agent.probe().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "agent.list" => match parse_params::<AgentListParams>(request.params) {
            Ok(params) => match state.engine.list_agents(&params.session_id).await {
                Ok(agents) => RpcResponse::success(id, AgentListResult { agents }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identities.list" => match parse_params::<AgentIdentityListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_agent_identities_for_project(
                    &params.project_id,
                    params.role.as_deref(),
                    params.status.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identities.get" => match parse_params::<AgentIdentityParams>(request.params) {
            Ok(params) => match state.engine.get_agent_identity(&params.identity_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.org.snapshot" => match parse_params::<AgentOrgSnapshotParams>(request.params) {
            Ok(params) => match state.engine.agent_org_snapshot(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identity.detail" => {
            match parse_params::<AgentIdentityDetailParams>(request.params) {
                Ok(params) => match state.engine.agent_identity_detail(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identity.create" => {
            match parse_params::<CreateAgentIdentityParams>(request.params) {
                Ok(params) => {
                    // S6 hires Managers/Workers through the approval-aware
                    // surface. The Leader bootstrap remains the S1 explicit
                    // creation path; there is exactly one live Leader.
                    if !params.role.trim().eq_ignore_ascii_case("leader") {
                        RpcResponse::error(
                            id,
                            RpcError::with_data(
                                codes::INVALID_PARAMS,
                                "use agent.hire for Manager/Worker identities; agent.identity.create is the Leader bootstrap path",
                                json!({"code": vp_protocol::types::agent_identity_codes::HIRE_INVALID}),
                            ),
                        )
                    } else {
                        match state.engine.create_agent_identity(params).await {
                            Ok(result) => RpcResponse::success(id, result),
                            Err(error) => identity_error(id, error),
                        }
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identity.update" => {
            match parse_params::<UpdateAgentIdentityParams>(request.params) {
                Ok(params) => {
                    // Role/department/component/parent changes are promotion or
                    // transfer and must go through their approval-classified
                    // S6 operations. The S1 update surface remains for bounded
                    // metadata, profile/model, and successor bookkeeping.
                    if params.role.is_some()
                        || params.department.is_some()
                        || params.component.is_some()
                        || params.parent_identity_id.is_some()
                    {
                        RpcResponse::error(
                            id,
                            RpcError::with_data(
                                codes::INVALID_PARAMS,
                                "role/department/component/parent changes require agent.promote or agent.transfer approval",
                                json!({"code": vp_protocol::types::agent_identity_codes::IDENTITY_APPROVAL_REQUIRED}),
                            ),
                        )
                    } else {
                        match state.engine.update_agent_identity(params).await {
                            Ok(result) => RpcResponse::success(id, result),
                            Err(error) => identity_error(id, error),
                        }
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identity.pause" => match parse_params::<PauseAgentIdentityParams>(request.params) {
            Ok(params) => match state.engine.pause_agent_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identity.resume" => {
            match parse_params::<ResumeAgentIdentityParams>(request.params) {
                Ok(params) => match state.engine.resume_agent_identity(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identity.retire" => {
            // Legacy alias. It now follows S6 approval policy and fails closed
            // unless the project has explicitly disabled retirement approval.
            match parse_params::<RetireAgentIdentityParams>(request.params) {
                Ok(params) => {
                    match state
                        .engine
                        .retire_agent(AgentRetireParams {
                            identity_id: params.identity_id,
                            successor_id: params.successor_id,
                            approved: false,
                            ..AgentRetireParams::default()
                        })
                        .await
                    {
                        Ok(result) => RpcResponse::success(
                            id,
                            vp_protocol::types::AgentIdentityResult {
                                identity: result.identity,
                                changed: result.changed,
                            },
                        ),
                        Err(error) => identity_error(id, error),
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identities.replay" => {
            match parse_params::<AgentIdentityReplayParams>(request.params) {
                Ok(params) => match state.engine.replay_agent_identities(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.hire" => match parse_params::<AgentHireParams>(request.params) {
            Ok(params) => match state.engine.hire_agent_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.hire.approve" => match parse_params::<AgentHireApproveParams>(request.params) {
            Ok(params) => match state.engine.approve_agent_hire(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.hire.reject" => match parse_params::<AgentHireRejectParams>(request.params) {
            Ok(params) => match state.engine.reject_agent_hire(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.hire.list" => match parse_params::<AgentHireListParams>(request.params) {
            Ok(params) => match state.engine.list_agent_hire_requests(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.promote" => match parse_params::<AgentPromoteParams>(request.params) {
            Ok(params) => match state.engine.promote_agent_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.transfer" => match parse_params::<AgentTransferParams>(request.params) {
            Ok(params) => match state.engine.transfer_agent_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.retire" => match parse_params::<AgentRetireParams>(request.params) {
            Ok(params) => match state.engine.retire_agent(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.retirement.propose" => {
            match parse_params::<AgentRetirementProposeParams>(request.params) {
                Ok(params) => match state.engine.propose_agent_retirement(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.retirement.approve" => {
            match parse_params::<AgentRetirementProposalParams>(request.params) {
                Ok(params) => match state.engine.approve_agent_retirement_proposal(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.retirement.reject" => {
            match parse_params::<AgentRetirementRejectParams>(request.params) {
                Ok(params) => match state.engine.reject_agent_retirement_proposal(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.retirement.proposals" => {
            match parse_params::<AgentRetirementProposalListParams>(request.params) {
                Ok(params) => match state.engine.agent_retirement_proposals(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.retirement.candidates" => {
            match parse_params::<AgentRetirementCandidatesParams>(request.params) {
                Ok(params) => match state.engine.agent_retirement_candidates(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.performance" => match parse_params::<AgentPerformanceParams>(request.params) {
            Ok(params) => match state.engine.agent_performance(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.performance.summary" => {
            match parse_params::<AgentPerformanceSummaryParams>(request.params) {
                Ok(params) => match state.engine.agent_performance_summary(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.performance.replay" => {
            match parse_params::<AgentPerformanceReplayParams>(request.params) {
                Ok(params) => match state.engine.agent_performance_replay(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.employment.replay" => {
            match parse_params::<AgentEmploymentReplayParams>(request.params) {
                Ok(params) => match state.engine.replay_agent_employment(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.memory.list" => match parse_params::<AgentMemoryListParams>(request.params) {
            Ok(params) => match state.engine.list_agent_memories(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.get" => match parse_params::<AgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.get_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.remember" => match parse_params::<CreateAgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.remember_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.update" => match parse_params::<UpdateAgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.update_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.supersede" => {
            match parse_params::<SupersedeAgentMemoryParams>(request.params) {
                Ok(params) => match state.engine.supersede_agent_memory(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => memory_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.memory.forget" => match parse_params::<ForgetAgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.forget_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.erase" => {
            match parse_params::<vp_protocol::types::EraseAgentMemoryParams>(request.params) {
                Ok(params) => match state.engine.erase_agent_memory(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => memory_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.memory.verify" => match parse_params::<VerifyAgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.verify_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.sweep" => match parse_params::<SweepAgentMemoriesParams>(request.params) {
            Ok(params) => match state.engine.sweep_agent_memories(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.replay" => match parse_params::<AgentMemoryReplayParams>(request.params) {
            Ok(params) => match state.engine.replay_agent_memories(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.inbox.list" => match parse_params::<AgentInboxListParams>(request.params) {
            Ok(params) => match state.engine.list_agent_inbox(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.inbox.send" => match parse_params::<AgentInboxSendParams>(request.params) {
            Ok(params) => match state.engine.send_agent_inbox(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.inbox.read" => match parse_params::<AgentInboxMessageParams>(request.params) {
            Ok(params) => match state.engine.read_agent_inbox(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.inbox.ack" => match parse_params::<AgentInboxMessageParams>(request.params) {
            Ok(params) => match state.engine.ack_agent_inbox(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.coordination.replay" => {
            match parse_params::<AgentCoordinationReplayParams>(request.params) {
                Ok(params) => match state.engine.replay_agent_coordination(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => coordination_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.list" => match parse_params::<AgentWorkspaceListParams>(request.params) {
            Ok(params) => match state.engine.list_agent_workspaces(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => workspace_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.workspace.get" | "workspace.get" => {
            match parse_params::<AgentWorkspaceParams>(request.params) {
                Ok(params) => match state.engine.get_agent_workspace(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.ensure" | "workspace.ensure" => {
            match parse_params::<AgentWorkspaceEnsureParams>(request.params) {
                Ok(params) => match state.engine.ensure_agent_workspace(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.land" | "workspace.land" => {
            match parse_params::<AgentWorkspaceLandParams>(request.params) {
                Ok(params) => match state.engine.land_agent_workspace(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.archive" | "workspace.archive" => {
            match parse_params::<AgentWorkspaceArchiveParams>(request.params) {
                Ok(params) => match state.engine.archive_agent_workspace(params, false).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.retire" | "workspace.retire" => {
            match parse_params::<AgentWorkspaceRetireParams>(request.params) {
                Ok(params) => match state.engine.retire_agent_workspace(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.replay" | "workspace.replay" => {
            match parse_params::<AgentWorkspaceReplayParams>(request.params) {
                Ok(params) => match state.engine.replay_agent_workspaces(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.configure" => match parse_params::<ConfigureAgentParams>(request.params) {
            Ok(params) => match state.agent.configure(params) {
                Ok(info) => {
                    let agent = state.agent.clone();
                    tokio::spawn(async move {
                        let _ = agent.resume_queued_sessions().await;
                        agent.auto_resolve_all().await;
                    });
                    RpcResponse::success(id, info)
                }
                Err(error) => {
                    RpcResponse::error(id, RpcError::new(codes::INVALID_PARAMS, error.to_string()))
                }
            },
            Err(response) => response_with_id(id, response),
        },
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}
