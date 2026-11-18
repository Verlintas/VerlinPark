use serde_json::json;

use vp_protocol::diagnosis::{
    SessionAttributionsParams, SessionDiagnosisParams, SessionDiagnosisRecordParams,
};
use vp_protocol::lsp::LspStatusParams;
use vp_protocol::otel::{
    OtelExportParams, OtelPolicyParams, OtelStatusParams, OtelTestParams, SetOtelPolicyParams,
};
use vp_protocol::remote::{
    HostListParams, HostListResult, HostProbeParams, HostProjectParams, HostSetParams,
    RemoteBootstrapParams, RemoteBootstrapStatusParams, RemoteIntegrationCleanupParams,
    RemoteIntegrationParams, RemoteIntegrationPolicyParams, RemoteIntegrationStatusParams,
    RemoteLedgerParams, RemoteStatusParams, RemoteTestOutputParams, RemoteTestParams,
    RemoteTestPolicyParams, RemoteTestStatusParams, RemoteWorktreeListParams,
    SetRemoteIntegrationPolicyParams, SetRemoteTestPolicyParams,
};
use vp_protocol::replay::ControllerReplayParams;
use vp_protocol::rpc::{RpcError, RpcRequest, RpcResponse, codes};
use vp_protocol::runtime::{PlanMigrateParams, SessionCompatibilityParams};
use vp_protocol::scaling::{
    ScalingPolicyParams, ScalingPreviewParams, ScalingStatusParams, SetScalingPolicyParams,
};
use vp_protocol::test_cache::{
    SetTestCachePolicyParams, TestCacheClearParams, TestCacheEntriesParams, TestCacheImpactParams,
    TestCachePolicyParams, TestCachePruneParams, TestCacheScopePreviewParams, TestCacheStatsParams,
};
use vp_protocol::types::{
    AcpAgentConfig, AcpAgentIdParams, AcpProbeParams, AddAcpAgentParams, AddMcpServerParams,
    AddProjectParams, AgentCoordinationReplayParams, AgentEmploymentReplayParams,
    AgentHireApproveParams, AgentHireListParams, AgentHireParams, AgentHireRejectParams,
    AgentIdentityDetailParams, AgentIdentityListParams, AgentIdentityParams,
    AgentIdentityPolicyParams, AgentIdentityPolicyResult, AgentIdentityReplayParams,
    AgentInboxListParams, AgentInboxMessageParams, AgentInboxSendParams, AgentListParams,
    AgentListResult, AgentMemoryListParams, AgentMemoryParams, AgentMemoryReplayParams,
    AgentMessageResult, AgentOrgSnapshotParams, AgentPerformanceParams,
    AgentPerformanceReplayParams, AgentPerformanceSummaryParams, AgentPromoteParams,
    AgentRetireParams, AgentRetirementCandidatesParams, AgentRetirementPolicyParams,
    AgentRetirementProposalListParams, AgentRetirementProposalParams, AgentRetirementProposeParams,
    AgentRetirementRejectParams, AgentTransferParams, AgentWorkspaceArchiveParams,
    AgentWorkspaceEnsureParams, AgentWorkspaceLandParams, AgentWorkspaceListParams,
    AgentWorkspaceParams, AgentWorkspaceReplayParams, AgentWorkspaceRetireParams,
    AnswerQuestionParams, AnswerQuestionResult, ApprovalAuditParams, ApprovalBatchResolveParams,
    ApprovalBatchResolveResult, ApprovalGrantCreateParams, ApprovalGrantCreateResult,
    ApprovalGrantDeleteParams, ApprovalGrantDeleteResult, ApprovalGrantListParams,
    ApprovalGrantListResult, ApprovalListResult, ApprovalQueueParams, ApprovalTemplateCreateParams,
    ApprovalTemplateDeleteParams, ApprovalTemplateFromApprovalParams, ApprovalTemplateListParams,
    ApprovalTemplateListResult, ApprovalTemplateUpdateParams, ArtifactArchiveExtractParams,
    ArtifactArchiveListParams, ArtifactBackfillParams, ArtifactDiffParams, ArtifactGcParams,
    ArtifactGetParams, ArtifactListParams, ArtifactListResult, ArtifactMaterializeParams,
    ArtifactPolicyParams, ArtifactPolicyResult, ArtifactRestoreParams, ArtifactResult,
    ArtifactVersionsParams, AttentionPolicyGetParams, AttentionPolicySetParams, CachePolicyParams,
    CachePolicyResult, CancelSessionParams, CancelSessionResult, CheckpointPolicyParams,
    CleanupWorktreesParams, CommitSessionParams, ConfigureAgentParams, ConsoleCloseParams,
    ConsoleOpenParams, ConsoleReadParams, ConsoleSignalParams, ConsoleWriteParams,
    ContextCheckpointCreateParams, ContextCheckpointDeleteParams, ContextCheckpointDetailParams,
    ContextCheckpointListParams, ContextCheckpointPolicyParams, ContextCheckpointPruneParams,
    ContextRestoreParams, ContractPolicyParams, ContractPolicyResult, CreateAgentIdentityParams,
    CreateAgentMemoryParams, CreateModelPresetParams, CreateSessionParams, CriticalPathMode,
    CriticalPathPolicyParams, CriticalPathPolicyResult, DaemonMetricsResult, DashboardParams,
    DeleteModelPresetParams, DiscardSessionParams, DoctorCheck, DoctorReport, EffectPolicyParams,
    EffectPolicyResult, EffectReconcileParams, EffectReconcileResult, EventListResult,
    ForgePreflightParams, ForgeStatusParams, ForgetAgentMemoryParams, ForkAuditParams,
    ForkRepairParams, GateCancelParams, GateListParams, GateOpenParams, GatePolicyParams,
    GatePolicyResult, GateResolveParams, GateStatusParams, GateSweepParams, GitLogParams,
    GitShowParams, GovernorPreviewParams, GovernorStatusParams, IntakeCreateParams,
    IntakeDeleteResult, IntakeListParams, IntakeTaskParams, IntakeUpdateParams,
    IntegrationPolicyParams, IntegrationPolicyResult, LandSessionParams, ListApprovalsParams,
    ListEventsParams, ListSessionsParams, MailboxGcParams, MailboxListParams, MailboxListResult,
    MailboxPolicyParams, MailboxPolicyResult, MarkMessageReadParams, McpCallParams,
    McpGetPromptParams, McpPinApproveParams, McpPinRejectParams, McpProbeParams,
    McpReadResourceParams, McpServerIdParams, MediaInfoParams, MediaTranscodeParams,
    MemoryBriefParams, MemoryConflictResolveParams, MemoryConflictScanParams,
    MemoryConflictsParams, MemoryCreateParams, MemoryCreateResult, MemoryDeleteParams,
    MemoryEmbeddingsIndexRebuildParams, MemoryEmbeddingsIndexStatusParams,
    MemoryEmbeddingsRebuildParams, MemoryEmbeddingsStatusParams, MemoryEmbeddingsTestParams,
    MemoryHealthParams, MemoryListParams, MemoryPolicyParams, MemoryPruneParams,
    MemorySearchParams, MemoryStatsParams, MemorySweepParams, MemoryUpdateParams,
    MemoryVerifyParams, MergeQueueListParams, MergeQueueListResult, OpenPullRequestParams,
    PauseAgentIdentityParams, PingResult, PreemptionOverviewParams, PreemptionPolicyParams,
    PreemptionPolicyResult, ProcessListParams, ProcessLogsParams, ProcessPolicyParams,
    ProcessPolicyResult, ProcessRestartParams, ProcessStopParams, ProjectBudgetParams,
    ProjectCacheEpochParams, ProjectCacheEpochPruneParams, ProjectCacheParams,
    ProjectForkForestParams, ProjectListResult, ProjectMcpPinsParams, ProjectUsageParams,
    PromptParams, PromptResult, QuestionListParams, RehydrateSessionParams, ReplaySessionParams,
    ResolveApprovalParams, ResolveApprovalResult, ResolveMergeParams, ResolveMergeResult,
    ResumeAgentIdentityParams, RetireAgentIdentityParams, RetryMergeParams, RetryMergeResult,
    SchedulerStatusParams, SearchParams, SessionAssignParams, SessionAssignmentRenewParams,
    SessionAssignmentsParams, SessionCacheAuditParams, SessionCacheBreaksParams,
    SessionCacheCallsParams, SessionCacheEpochParams, SessionCacheParams, SessionCachePlanParams,
    SessionCacheWarmParams, SessionCheckpointDeleteParams, SessionCheckpointDiffParams,
    SessionCheckpointListParams, SessionCheckpointParams, SessionCheckpointPruneParams,
    SessionCheckpointRestoreParams, SessionCheckpointStatsParams, SessionCitationAuditParams,
    SessionCitationsParams, SessionCompactParams, SessionCompareParams, SessionContextParams,
    SessionContinuationCancelParams, SessionContinuationReclaimParams,
    SessionContinuationStatusParams, SessionContinuationsParams, SessionContractParams,
    SessionDeferParams, SessionDiffParams, SessionEffectsParams, SessionExportParams,
    SessionForkParams, SessionForkTreeParams, SessionImportParams, SessionInstructionsParams,
    SessionLineageParams, SessionListResult, SessionLogParams, SessionMergeForkParams,
    SessionPauseParams, SessionPauseResult, SessionPlanResumeParams, SessionPlanResumeResult,
    SessionPreemptParams, SessionPreemptionParams, SessionReleaseParams, SessionResumeParams,
    SessionResumeResult, SessionRunCancelParams, SessionRunDetailParams, SessionRunPolicyParams,
    SessionRunResumeParams, SessionRunsParams, SessionSkillActivationsParams, SessionStallsParams,
    SessionSteerParams, SessionSteeringCancelParams, SessionSteeringParams, SessionTodosParams,
    SessionTodosResult, SessionTranscriptParams, SessionWorkspaceParams,
    SetAgentIdentityPolicyParams, SetArtifactPolicyParams, SetCachePolicyParams,
    SetCheckpointPolicyParams, SetContextCheckpointPolicyParams, SetContractPolicyParams,
    SetCriticalPathPolicyParams, SetGatePolicyParams, SetIntegrationPolicyParams,
    SetMailboxPolicyParams, SetMemoryPolicyParams, SetPreemptionPolicyParams,
    SetProcessPolicyParams, SetProjectBudgetParams, SetProjectPromptLayoutParams,
    SetSessionPriorityParams, SetSessionPromptLayoutParams, SetSessionRunPolicyParams,
    SetSessionUnlimitedParams, SetShellPolicyParams, SetSourceCollectionParams,
    SetSourceEmbeddingPolicyParams, SetSourceLabelsParams, SetSourceRetrievalPolicyParams,
    SetTaskSchedulerPolicyParams, SetToolOutputPolicyParams, SetWorktreePolicyParams,
    ShellPolicyParams, ShellPolicyResult, SkillCreateParams, SkillEnabledParams, SkillIdParams,
    SkillImportParams, SkillListParams, SkillUpdateParams, SourceActionParams,
    SourceCollectionsParams, SourceCreateFileParams, SourceCreateNoteParams, SourceCreateUrlParams,
    SourceEmbeddingPolicyParams, SourceEmbeddingReindexParams, SourceEmbeddingStatusParams,
    SourceGetParams, SourceImportParams, SourceIndexRebuildParams, SourceIndexStatusParams,
    SourceListParams, SourceRetrievalPolicyParams, SourceSearchParams, SupersedeAgentMemoryParams,
    SweepAgentMemoriesParams, TaskActionParams, TaskActionResult, TaskDispatchParams,
    TaskDispatchResult, TaskQueueParams, TaskSchedulerPolicyParams, TeamPlanParams, TeamPlanResult,
    TeamTemplateDiffParams, TeamTemplateExportParams, TeamTemplateGetParams,
    TeamTemplateImportParams, TeamTemplateListParams, TeamTemplateReplayParams,
    TerminalCloseParams, TerminalOpenParams, TerminalReadParams, TerminalResizeParams,
    TerminalWriteParams, ToolActivityParams, ToolBundleDiffParams, ToolBundleHistoryParams,
    ToolBundleMatrixParams, ToolBundleParams, ToolOutputAuditParams, ToolOutputListParams,
    ToolOutputPolicyParams, ToolOutputPolicyResult, ToolOutputPruneParams, ToolOutputStatusParams,
    UpdateAcpAgentParams, UpdateAgentIdentityParams, UpdateAgentMemoryParams, UpdateCheckParams,
    UpdateDownloadParams, UpdateMcpServerParams, UpdateModelPresetParams,
    UpdateSourceMetadataParams, UsageLedgerParams, VerifyAgentMemoryParams, WorkspaceFileParams,
    WorkspaceGrepParams, WorkspaceListParams, WorkspaceSearchParams, WorkspaceTreeParams,
    WorkspaceWriteParams, WorktreePolicyParams,
};
use vp_protocol::types::{
    ComputerLeaseInfo, ComputerLeaseListResult, ComputerLeaseReleaseParams,
    ComputerLeaseReleaseResult, ComputerLeaseSweepResult, ComputerStatusParams,
    DaemonAgentStatusParams, DaemonAgentStatusResult, DaemonInstanceInfo, GuiPolicyParams,
    GuiPolicyResult, GuiTimelineAuditParams, GuiTimelineParams, GuiTimelinePruneParams,
    GuiTimelineResult, GuiTimelineRetentionParams, RuntimeLeaseAuditInfo,
    RuntimeLeaseHistoryParams, RuntimeLeaseHistoryResult, RuntimeLeaseInfo, RuntimeLeaseListResult,
    RuntimeLeaseSweepResult, SetGuiPolicyParams,
};
use vp_protocol::types::{
    CoordinationNoteParams, CoordinationNoteResult, OrchestrationAdvanceParams,
    OrchestrationAdvanceResult, OrchestrationOverviewParams, OrchestrationPolicyParams,
    OrchestrationPolicyResult, OrchestrationPreviewParams, OrchestrationRecoveriesParams,
    OrchestrationSnapshotParams, OrchestrationSnapshotResult, PlanRevisionGetParams,
    PlanRevisionListParams, PlanRevisionListResult, PlanRevisionRestoreParams,
    ReviseTeamPlanParams, SetOrchestrationPolicyParams, TaskEditParams,
};
use vp_protocol::types::{
    ProgramActionParams, ProgramCreateParams, ProgramDeleteParams, ProgramDeleteResult,
    ProgramListParams, ProgramMilestoneActionParams, ProgramMilestoneCreateParams,
    ProgramMilestoneParams, ProgramMilestoneResult, ProgramMilestoneUpdateParams, ProgramParams,
    ProgramReplayParams, ProgramResult, ProgramStatusParams, ProgramTickParams,
    ProgramUpdateParams,
};
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION};

use crate::AppState;
use crate::backup_status::backup_status;
use crate::errors::{
    approval_error, archive_media_error, coordination_error, diagnosis_error, effect_error,
    fanout_error, forge_error, gate_error, identity_error, internal_error, memory_error,
    otel_error, remote_bootstrap_error, remote_integration_error, remote_test_error,
    retrieval_policy_error, runtime_pin_error, source_index_error, workspace_error,
};
use crate::http::collect_daemon_metrics;
use crate::metrics;
use crate::params::{parse_params, response_with_id};
use crate::server::server_info;

pub(crate) async fn dispatch(state: &AppState, request: RpcRequest) -> RpcResponse {
    let id = request.id.clone();
    let method = request.method.clone();
    let response = match request.method.as_str() {
        "ping" => RpcResponse::success(
            id,
            PingResult {
                pong: true,
                version: APP_VERSION.to_string(),
                protocol: PROTOCOL_VERSION,
                uptime_secs: state.started.elapsed().as_secs(),
            },
        ),
        "server.info" => match server_info(state).await {
            Ok(info) => RpcResponse::success(id, info),
            Err(error) => internal_error(id, error),
        },
        "daemon.metrics" => match collect_daemon_metrics(state).await {
            Ok(metrics) => {
                let prometheus = metrics::render_prometheus(&metrics);
                RpcResponse::success(
                    id,
                    DaemonMetricsResult {
                        metrics,
                        prometheus,
                    },
                )
            }
            Err(error) => internal_error(id, error),
        },
        "project.add" => match parse_params::<AddProjectParams>(request.params) {
            Ok(params) => match state.engine.add_project(params.root, params.name).await {
                Ok(project) => RpcResponse::success(id, project),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "server.doctor" => match state.engine.doctor_checks().await {
            Ok(mut checks) => {
                let info = state.agent.info();
                checks.push(DoctorCheck {
                    name: "provider".into(),
                    status: if info.configured { "ok" } else { "warn" }.into(),
                    detail: if info.configured {
                        format!(
                            "{} / {}",
                            info.provider.as_deref().unwrap_or("?"),
                            info.model.as_deref().unwrap_or("?")
                        )
                    } else {
                        "no provider configured".to_string()
                    },
                });
                let ok = !checks.iter().any(|check| check.status == "error");
                RpcResponse::success(id, DoctorReport { checks, ok })
            }
            Err(error) => internal_error(id, error),
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
        "computer.leases" => match state.engine.list_computer_leases().await {
            Ok(leases) => RpcResponse::success(
                id,
                ComputerLeaseListResult {
                    runtime_id: state.engine.runtime_id().to_string(),
                    leases: leases
                        .into_iter()
                        .map(|lease| ComputerLeaseInfo {
                            lease_id: lease.lease_id,
                            surface: lease.surface,
                            owner_id: lease.owner_id,
                            session_id: lease.session_id,
                            agent_id: lease.agent_id,
                            agent_name: lease.agent_name,
                            acquired_at: lease.acquired_at,
                            heartbeat_at: lease.heartbeat_at,
                            expires_at: lease.expires_at,
                            generation: lease.generation,
                        })
                        .collect(),
                },
            ),
            Err(error) => internal_error(id, error),
        },
        "computer.leases.sweep" => match state.engine.sweep_computer_leases().await {
            Ok(removed) => RpcResponse::success(id, ComputerLeaseSweepResult { removed }),
            Err(error) => internal_error(id, error),
        },
        "computer.leases.release" => {
            match parse_params::<ComputerLeaseReleaseParams>(request.params) {
                Ok(params) => match state.engine.release_computer_lease(&params.lease_id).await {
                    Ok(released) => {
                        RpcResponse::success(id, ComputerLeaseReleaseResult { released })
                    }
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
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
        "mcp.pin.approve" => match parse_params::<McpPinApproveParams>(request.params) {
            Ok(params) => match state.engine.approve_mcp_pin(&params, None).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.pin.reject" => match parse_params::<McpPinRejectParams>(request.params) {
            Ok(params) => match state.engine.reject_mcp_pin(&params, None).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
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
                        let _ = state.engine.create_leader_agent(&session.id, &model).await;
                        if let Err(error) = state.agent.start(&session.id, &params.goal).await {
                            tracing::warn!(%error, session_id = %session.id, "agent did not start");
                        }
                        RpcResponse::success(id, session)
                    }
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.diff" => match parse_params::<ArtifactDiffParams>(request.params) {
            Ok(params) => match state.engine.artifact_diff(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.versions" => match parse_params::<ArtifactVersionsParams>(request.params) {
            Ok(params) => match state.engine.artifact_versions(&params.artifact_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.restore" => match parse_params::<ArtifactRestoreParams>(request.params) {
            Ok(params) => match state
                .engine
                .restore_artifact_version(&params.artifact_id, params.version)
                .await
            {
                Ok(artifact) => {
                    let provenance = artifact.provenance.clone();
                    RpcResponse::success(
                        id,
                        ArtifactResult {
                            artifact,
                            provenance,
                        },
                    )
                }
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.list" => match parse_params::<SkillListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_skills(params.project_id, params.include_disabled.unwrap_or(true))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.create" => match parse_params::<SkillCreateParams>(request.params) {
            Ok(params) => match state.engine.create_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.import" => match parse_params::<SkillImportParams>(request.params) {
            Ok(params) => match state.engine.import_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.update" => match parse_params::<SkillUpdateParams>(request.params) {
            Ok(params) => match state.engine.update_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.set_enabled" => match parse_params::<SkillEnabledParams>(request.params) {
            Ok(params) => match state.engine.set_skill_enabled(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.delete" => match parse_params::<SkillIdParams>(request.params) {
            Ok(params) => match state.engine.delete_skill(params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.list" => match parse_params::<SourceListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_sources(params.project_id, params.query, params.status, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.update" => match parse_params::<UpdateSourceMetadataParams>(request.params) {
            Ok(params) => match state.engine.update_source_metadata(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.collections" => match parse_params::<SourceCollectionsParams>(request.params) {
            Ok(params) => match state.engine.source_collections(params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.set_collection" => {
            match parse_params::<SetSourceCollectionParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_collection(params.source_id, params.collection)
                    .await
                {
                    Ok(source) => RpcResponse::success(id, source),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.set_labels" => match parse_params::<SetSourceLabelsParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_source_labels(params.source_id, params.labels)
                .await
            {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.get" => match parse_params::<SourceGetParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_source(params.source_id, params.offset, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_note" => match parse_params::<SourceCreateNoteParams>(request.params) {
            Ok(params) => match state.engine.add_source_note(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_file" => match parse_params::<SourceCreateFileParams>(request.params) {
            Ok(params) => match state.engine.add_source_file(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_url" => match parse_params::<SourceCreateUrlParams>(request.params) {
            Ok(params) => match state.engine.add_source_url(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.import" => match parse_params::<SourceImportParams>(request.params) {
            Ok(params) => match state.engine.import_sources(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.delete" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.delete_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.refresh" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.refresh_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.reindex" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.reindex_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.search" => match parse_params::<SourceSearchParams>(request.params) {
            Ok(params) => match state
                .engine
                .search_sources_with_mode(
                    params.project_id,
                    params.query,
                    params.source_ids,
                    params.collections,
                    params.labels,
                    params.limit,
                    params.mode,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.embedding.status" => {
            match parse_params::<SourceEmbeddingStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .source_embedding_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.reindex" => {
            match parse_params::<SourceEmbeddingReindexParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .reindex_source_embeddings(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.policy.get" => {
            match parse_params::<SourceEmbeddingPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_source_embedding_policy(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.policy.set" => {
            match parse_params::<SetSourceEmbeddingPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_embedding_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.retrieval.policy.get" => {
            match parse_params::<SourceRetrievalPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_source_retrieval_policy(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.retrieval.policy.set" => {
            match parse_params::<SetSourceRetrievalPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_retrieval_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => retrieval_policy_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.index_status" => {
            match parse_params::<SourceIndexStatusParams>(request.params) {
                Ok(params) => match state.engine.source_index_status(&params.project_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.index_rebuild" => {
            match parse_params::<SourceIndexRebuildParams>(request.params) {
                Ok(params) => match state.engine.rebuild_source_index(&params.project_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => source_index_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
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
        "terminal.open" => match parse_params::<TerminalOpenParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_open(
                    &params.session_id,
                    params.cols.unwrap_or(80),
                    params.rows.unwrap_or(24),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.write" => match parse_params::<TerminalWriteParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_write(&params.terminal_id, &params.data)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.read" => match parse_params::<TerminalReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_read(&params.terminal_id, params.after_seq)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.resize" => match parse_params::<TerminalResizeParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_resize(&params.terminal_id, params.cols, params.rows)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.close" => match parse_params::<TerminalCloseParams>(request.params) {
            Ok(params) => match state.engine.terminal_close(&params.terminal_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.open" => match parse_params::<ConsoleOpenParams>(request.params) {
            Ok(params) => match state.engine.console_open(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.write" => match parse_params::<ConsoleWriteParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_write(&params.console_id, &params.data)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.read" => match parse_params::<ConsoleReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_read(&params.console_id, params.after_seq)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.signal" => match parse_params::<ConsoleSignalParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_signal(&params.console_id, &params.signal)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.close" => match parse_params::<ConsoleCloseParams>(request.params) {
            Ok(params) => match state.engine.console_close(&params.console_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
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
        "process.policy.get" => match parse_params::<ProcessPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_process_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    ProcessPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "process.policy.set" => match parse_params::<SetProcessPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_process_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ProcessPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "git.log" => match parse_params::<GitLogParams>(request.params) {
            Ok(params) => match state
                .engine
                .git_log(&params.session_id, params.limit.unwrap_or(50))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "git.show" => match parse_params::<GitShowParams>(request.params) {
            Ok(params) => match state
                .engine
                .git_show(&params.session_id, &params.commit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
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
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.list" => {
            let params: ListSessionsParams = if request.params.is_null() {
                ListSessionsParams { project_id: None }
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
            match state.engine.list_sessions(params.project_id).await {
                Ok(sessions) => RpcResponse::success(id, SessionListResult { sessions }),
                Err(error) => internal_error(id, error),
            }
        }
        "event.list" => match parse_params::<ListEventsParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_events(params.session_id, params.since_seq, params.limit)
                .await
            {
                Ok(events) => RpcResponse::success(id, EventListResult { events }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.activations" => {
            match parse_params::<SessionSkillActivationsParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .session_skill_activations(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
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
        "agent.info" => RpcResponse::success(id, state.agent.info()),
        "computer.status" => match parse_params::<ComputerStatusParams>(request.params) {
            Ok(params) => RpcResponse::success(
                id,
                state
                    .engine
                    .computer_status_for(params.session_id.as_deref(), params.refresh)
                    .await,
            ),
            Err(response) => response_with_id(id, response),
        },
        "gui.status" => match parse_params::<vp_protocol::types::GuiStatusParams>(request.params) {
            Ok(params) => match state.engine.gui_status(params.project_id.as_deref()).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline.audit" => match parse_params::<GuiTimelineAuditParams>(request.params) {
            Ok(params) => match state
                .engine
                .gui_timeline_audit(&params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline.retention" => {
            match parse_params::<GuiTimelineRetentionParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .gui_timeline_retention(&params.project_id, params.older_than_days)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "gui.timeline.prune" => match parse_params::<GuiTimelinePruneParams>(request.params) {
            Ok(params) if !params.dry_run.unwrap_or(false) && !params.confirm => {
                RpcResponse::error(
                    id,
                    vp_protocol::rpc::RpcError::new(
                        vp_protocol::rpc::codes::INVALID_PARAMS,
                        "gui.timeline.prune requires confirm=true for a non-dry-run prune",
                    ),
                )
            }
            Ok(params) => match state
                .engine
                .prune_gui_timeline(
                    &params.project_id,
                    params.older_than_days,
                    params.dry_run.unwrap_or(false),
                    params.limit,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "daemon.agent.status" => match parse_params::<DaemonAgentStatusParams>(request.params) {
            Ok(_) => {
                let data_dir = state.data_dir.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let paths =
                        vp_core::daemon_agent::DaemonAgentPaths::from_env(Some(data_dir), None)?;
                    let runner = vp_core::daemon_agent::SystemLaunchctl;
                    Ok::<_, anyhow::Error>(
                        vp_core::daemon_agent::DaemonAgent::new(paths, &runner).status(),
                    )
                })
                .await;
                match status {
                    Ok(Ok(status)) => {
                        RpcResponse::success(id, DaemonAgentStatusResult { ..status })
                    }
                    Ok(Err(error)) => internal_error(id, error),
                    Err(error) => internal_error(id, anyhow::anyhow!(error)),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline" => match parse_params::<GuiTimelineParams>(request.params) {
            Ok(params) => {
                let limit = params.limit.unwrap_or(100).clamp(1, 500);
                match state
                    .engine
                    .gui_timeline(params.session_id.as_deref(), limit)
                    .await
                {
                    Ok(actions) => RpcResponse::success(id, GuiTimelineResult { actions, limit }),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "gui.policy.get" => match parse_params::<GuiPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_gui_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    GuiPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.policy.set" => match parse_params::<SetGuiPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_gui_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    GuiPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.probe" => match state.agent.probe().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
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
                Err(error) => internal_error(id, error),
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
                Err(error) => internal_error(id, error),
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
        "plan.migrate" => match parse_params::<PlanMigrateParams>(request.params) {
            Ok(params) => match state.engine.migrate_plan(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => runtime_pin_error(id, error),
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
        "team_plan.get" => match parse_params::<TeamPlanParams>(request.params) {
            Ok(params) => match state.engine.get_team_plan(&params.session_id).await {
                Ok(plan) => RpcResponse::success(id, TeamPlanResult { plan }),
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
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
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
        "update.download" => match parse_params::<UpdateDownloadParams>(request.params) {
            Ok(params) => match vp_core::update::download_update(params.repo.as_deref()).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "update.check" => match parse_params::<UpdateCheckParams>(request.params) {
            Ok(params) => match vp_core::update::check_for_update(params.repo.as_deref()).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "backup.status" => {
            match parse_params::<vp_protocol::types::BackupStatusParams>(request.params) {
                Ok(_) => match backup_status(&state.data_dir) {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "dashboard.snapshot" => match parse_params::<DashboardParams>(request.params) {
            Ok(params) => match state
                .engine
                .dashboard_snapshot(params.project_id.as_deref())
                .await
            {
                Ok(mut result) => {
                    result.summary.agent = Some(state.agent.info());
                    RpcResponse::success(id, result)
                }
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
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
        "session.workspace" => match parse_params::<SessionWorkspaceParams>(request.params) {
            Ok(params) => match state.engine.session_workspace(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => workspace_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
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
        "mailbox.list" => match parse_params::<MailboxListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_agent_messages(&params.session_id, params.agent_id, params.unread_only)
                .await
            {
                Ok(messages) => RpcResponse::success(id, MailboxListResult { messages }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.read" => match parse_params::<MarkMessageReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .mark_agent_message_read(&params.message_id)
                .await
            {
                Ok(Some(message)) => RpcResponse::success(id, AgentMessageResult { message }),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "message not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.list" => match state.engine.list_mcp_servers().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "mcp.server.add" => match parse_params::<AddMcpServerParams>(request.params) {
            Ok(params) => match state.engine.add_mcp_server(params.config).await {
                Ok(server) => RpcResponse::success(id, json!({"server": server})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.update" => match parse_params::<UpdateMcpServerParams>(request.params) {
            Ok(params) => match state
                .engine
                .update_mcp_server(&params.id, params.config)
                .await
            {
                Ok(Some(server)) => RpcResponse::success(id, json!({"server": server})),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "MCP server not found"),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.probe" => match parse_params::<McpProbeParams>(request.params) {
            Ok(params) => match state.engine.probe_mcp_server(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.remove" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.remove_mcp_server(&params.server_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.list" => match state.engine.list_acp_agents().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "acp.agent.add" => match parse_params::<AddAcpAgentParams>(request.params) {
            Ok(params) => match state
                .engine
                .add_acp_agent_config(AcpAgentConfig {
                    name: params.name,
                    command: params.command,
                    args: params.args,
                    env: params.env,
                    enabled: params.enabled,
                    auto_approve: params.auto_approve,
                    timeout_secs: params.timeout_secs,
                    transport: params.transport,
                    endpoint: params.endpoint,
                    auth_header_env: params.auth_header_env,
                    headers: params.headers,
                    connect_timeout_secs: params.connect_timeout_secs,
                    allow_remote: params.allow_remote,
                })
                .await
            {
                Ok(agent) => RpcResponse::success(id, json!({"agent": agent})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.update" => match parse_params::<UpdateAcpAgentParams>(request.params) {
            Ok(params) => match state
                .engine
                .update_acp_agent_config(
                    &params.id,
                    AcpAgentConfig {
                        name: params.name,
                        command: params.command,
                        args: params.args,
                        env: params.env,
                        enabled: params.enabled,
                        auto_approve: params.auto_approve,
                        timeout_secs: params.timeout_secs,
                        transport: params.transport,
                        endpoint: params.endpoint,
                        auth_header_env: params.auth_header_env,
                        headers: params.headers,
                        connect_timeout_secs: params.connect_timeout_secs,
                        allow_remote: params.allow_remote,
                    },
                )
                .await
            {
                Ok(Some(agent)) => RpcResponse::success(id, json!({"agent": agent})),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "ACP agent not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.remove" => match parse_params::<AcpAgentIdParams>(request.params) {
            Ok(params) => match state.engine.remove_acp_agent(&params.agent_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.probe" => match parse_params::<AcpProbeParams>(request.params) {
            Ok(params) => match state
                .engine
                .probe_acp_agent_options(&params.agent_id, params.project_id, params.create_session)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.tools" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_tools(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.call" => match parse_params::<McpCallParams>(request.params) {
            Ok(params) => match state.engine.mcp_call_for_params(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.task.list" => {
            match parse_params::<vp_protocol::types::McpTaskListParams>(request.params) {
                Ok(params) => match state.engine.mcp_task_list(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "mcp.task.get" => match parse_params::<vp_protocol::types::McpTaskParams>(request.params) {
            Ok(params) => match state.engine.mcp_task_get(&params.task_id, true).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.task.result" => {
            match parse_params::<vp_protocol::types::McpTaskResultParams>(request.params) {
                Ok(params) => match state.engine.mcp_task_result(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "mcp.task.cancel" => {
            match parse_params::<vp_protocol::types::McpTaskCancelParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .mcp_task_cancel(&params.task_id, params.reason.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "mcp.resources" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_resources(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.resource.read" => match parse_params::<McpReadResourceParams>(request.params) {
            Ok(params) => match state
                .engine
                .mcp_read_resource(&params.server_id, &params.uri)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.prompts" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_prompts(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.prompt.get" => match parse_params::<McpGetPromptParams>(request.params) {
            Ok(params) => match state
                .engine
                .mcp_prompt(&params.server_id, &params.name, params.arguments)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
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
        "memory.list" => match parse_params::<MemoryListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_memories_filtered(
                    &params.project_id,
                    params.query,
                    params.kind,
                    params.status,
                    params.include_stale,
                    params.limit.unwrap_or(100),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.search" => match parse_params::<MemorySearchParams>(request.params) {
            Ok(params) => match state.engine.memory_search(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.brief" => match parse_params::<MemoryBriefParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_brief(&params.session_id, params.task_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.update" => match parse_params::<MemoryUpdateParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_update(
                    &params.id,
                    params.status.as_deref(),
                    params.pinned,
                    params.tags,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.verify" => match parse_params::<MemoryVerifyParams>(request.params) {
            Ok(params) => match state.engine.memory_verify(&params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.sweep" => match parse_params::<MemorySweepParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_sweep(&params.project_id, params.older_than_days, params.dry_run)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.stats" => match parse_params::<MemoryStatsParams>(request.params) {
            Ok(params) => match state.engine.memory_stats(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflicts" => match parse_params::<MemoryConflictsParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_conflicts(&params.project_id, params.status.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflict.scan" => match parse_params::<MemoryConflictScanParams>(request.params) {
            Ok(params) => match state.engine.memory_conflict_scan(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflict.resolve" => {
            match parse_params::<MemoryConflictResolveParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_conflict_resolve(&params.id, &params.action, params.note.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.health" => match parse_params::<MemoryHealthParams>(request.params) {
            Ok(params) => match state.engine.memory_health(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.embeddings.status" => {
            match parse_params::<MemoryEmbeddingsStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.rebuild" => {
            match parse_params::<MemoryEmbeddingsRebuildParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_rebuild(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.test" => {
            match parse_params::<MemoryEmbeddingsTestParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_test(&params.project_id, params.text.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.index.status" => {
            match parse_params::<MemoryEmbeddingsIndexStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_index_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.index.rebuild" => {
            match parse_params::<MemoryEmbeddingsIndexRebuildParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_index_rebuild(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.create" => match parse_params::<MemoryCreateParams>(request.params) {
            Ok(params) => match state
                .engine
                .create_memory(
                    &params.session_id,
                    &params.kind,
                    &params.title,
                    &params.body,
                    params.tags,
                )
                .await
            {
                Ok(memory) => RpcResponse::success(id, MemoryCreateResult { memory }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.delete" => match parse_params::<MemoryDeleteParams>(request.params) {
            Ok(params) => match state.engine.delete_memory(&params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.prune" => match parse_params::<MemoryPruneParams>(request.params) {
            Ok(params) => match state
                .engine
                .prune_memories(
                    &params.project_id,
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
        },
        "memory.policy.get" => match parse_params::<MemoryPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_memory_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.policy.set" => match parse_params::<SetMemoryPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_memory_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.list" => match parse_params::<ArtifactListParams>(request.params) {
            Ok(params) => match state.engine.list_artifacts(&params.session_id).await {
                Ok(artifacts) => RpcResponse::success(id, ArtifactListResult { artifacts }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.policy.get" => match parse_params::<MailboxPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_mailbox_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    MailboxPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.policy.set" => match parse_params::<SetMailboxPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_mailbox_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    MailboxPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.gc" => match parse_params::<MailboxGcParams>(request.params) {
            Ok(params) => {
                let policy = match state.engine.get_mailbox_policy(&params.project_id).await {
                    Ok(policy) => policy,
                    Err(error) => return internal_error(id, error),
                };
                match state.engine.gc_mailbox(&params.project_id, &policy).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.backfill" => match parse_params::<ArtifactBackfillParams>(request.params) {
            Ok(params) => {
                match state
                    .engine
                    .backfill_artifact_events(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.materialize" => match parse_params::<ArtifactMaterializeParams>(request.params) {
            Ok(params) => match state.engine.materialize_artifact(&params.artifact_id).await {
                Ok(Some(materialized)) => RpcResponse::success(id, materialized),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "artifact is not a file artifact"),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.policy.get" => match parse_params::<ToolOutputPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_tool_output_policy(&params.project_id)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ToolOutputPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.policy.set" => {
            match parse_params::<SetToolOutputPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_tool_output_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "tool_output.status" => match parse_params::<ToolOutputStatusParams>(request.params) {
            Ok(params) => match state.engine.tool_output_status(params).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.list" => match parse_params::<ToolOutputListParams>(request.params) {
            Ok(params) => match state.engine.list_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.prune" => match parse_params::<ToolOutputPruneParams>(request.params) {
            Ok(params) => match state.engine.prune_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.audit" => match parse_params::<ToolOutputAuditParams>(request.params) {
            Ok(params) => match state.engine.audit_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
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
        "artifact.policy.get" => match parse_params::<ArtifactPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_artifact_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    ArtifactPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.policy.set" => match parse_params::<SetArtifactPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_artifact_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ArtifactPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.gc" => match parse_params::<ArtifactGcParams>(request.params) {
            Ok(params) => {
                let policy = match state.engine.get_artifact_policy(&params.project_id).await {
                    Ok(policy) => policy,
                    Err(error) => return internal_error(id, error),
                };
                match state.engine.gc_artifacts(&params.project_id, &policy).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.get" => match parse_params::<ArtifactGetParams>(request.params) {
            Ok(params) => match state.engine.get_artifact(&params.artifact_id).await {
                Ok(Some(artifact)) => {
                    let provenance = artifact.provenance.clone();
                    RpcResponse::success(
                        id,
                        ArtifactResult {
                            artifact,
                            provenance,
                        },
                    )
                }
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "artifact not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.archive.list" => {
            match parse_params::<ArtifactArchiveListParams>(request.params) {
                Ok(params) => match state.engine.artifact_archive_list(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => archive_media_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "artifact.archive.extract" => {
            match parse_params::<ArtifactArchiveExtractParams>(request.params) {
                Ok(params) => match state.engine.artifact_archive_extract(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => archive_media_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "media.info" => match parse_params::<MediaInfoParams>(request.params) {
            Ok(params) => match state.engine.media_info(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => archive_media_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "media.transcode" => match parse_params::<MediaTranscodeParams>(request.params) {
            Ok(params) => match state.engine.media_transcode(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => archive_media_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "merge_queue.list" => match parse_params::<MergeQueueListParams>(request.params) {
            Ok(params) => match state.engine.list_merge_queue(&params.session_id).await {
                Ok(entries) => RpcResponse::success(id, MergeQueueListResult { entries }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "merge_queue.retry" => match parse_params::<RetryMergeParams>(request.params) {
            Ok(params) => match state.agent.retry_merge(&params.entry_id).await {
                Ok(entry) => RpcResponse::success(id, RetryMergeResult { entry }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "merge_queue.resolve" => match parse_params::<ResolveMergeParams>(request.params) {
            Ok(params) => match state.agent.resolve_merge(&params.entry_id).await {
                Ok(entry) => RpcResponse::success(id, ResolveMergeResult { entry }),
                Err(error) => internal_error(id, error),
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
        "forge.status" => match parse_params::<ForgeStatusParams>(request.params) {
            Ok(params) => match state.engine.forge_status(params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "forge.preflight" => match parse_params::<ForgePreflightParams>(request.params) {
            Ok(params) => match state
                .engine
                .forge_preflight(&params.project_id, params.provider.as_deref())
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
        "event.subscribe" => RpcResponse::success(id, json!({"subscribed": true})),
        "auth" => RpcResponse::success(id, json!({"ok": true})),
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    };
    if matches!(
        method.as_str(),
        "project.add"
            | "project.shell_policy.set"
            | "project.contract_policy.set"
            | "project.agent_identity_policy.set"
            | "agent.identity.create"
            | "team.template.export"
            | "team.template.import"
            | "team.template.replay"
            | "agent.identity.update"
            | "agent.identity.pause"
            | "agent.identity.resume"
            | "agent.identity.retire"
            | "agent.hire"
            | "agent.hire.approve"
            | "agent.hire.reject"
            | "agent.promote"
            | "agent.transfer"
            | "agent.retire"
            | "agent.retirement.propose"
            | "agent.retirement.approve"
            | "agent.retirement.reject"
            | "agent.performance.replay"
            | "agent.employment.replay"
            | "project.retirement_policy.set"
            | "agent.identities.replay"
            | "agent.memory.remember"
            | "agent.memory.update"
            | "agent.memory.supersede"
            | "agent.memory.forget"
            | "agent.memory.verify"
            | "agent.memory.sweep"
            | "agent.memory.replay"
            | "session.assign"
            | "session.assignment.renew"
            | "session.release"
            | "agent.inbox.send"
            | "agent.inbox.read"
            | "agent.inbox.ack"
            | "agent.coordination.replay"
            | "agent.workspace.ensure"
            | "agent.workspace.land"
            | "agent.workspace.archive"
            | "agent.workspace.retire"
            | "agent.workspace.replay"
            | "project.effect_policy.set"
            | "project.cache_policy.set"
            | "project.integration_policy.set"
            | "project.budget.set"
            | "project.worktree_policy.set"
            | "project.checkpoint_policy.set"
            | "project.task_scheduler.set"
            | "scaling.policy.set"
            | "host.add"
            | "host.update"
            | "host.remove"
            | "orchestration.policy.set"
            | "memory.policy.set"
            | "source.embedding.policy.set"
            | "source.embedding.reindex"
            | "source.retrieval.policy.set"
            | "process.policy.set"
            | "mailbox.policy.set"
            | "artifact.policy.set"
            | "tool_output.policy.set"
            | "session.import"
            | "mcp.pin.approve"
            | "mcp.pin.reject"
    ) && let Err(error) = state.engine.record_project_manifest().await
    {
        tracing::warn!(%error, "failed to update project manifest");
    }
    response
}

/// Known model references are daemon configuration, never client input. A
/// template binding to anything outside this set fails closed on import.
async fn template_known_models(state: &AppState) -> Vec<String> {
    let mut models = vp_core::team_template::configured_model_references(&state.agent.info());
    if let Ok(presets) = state.engine.list_model_presets().await {
        for preset in presets.presets {
            let model = preset.model.trim();
            if !model.is_empty() {
                models.push(model.to_string());
            }
        }
    }
    models.sort();
    models.dedup();
    models
}
