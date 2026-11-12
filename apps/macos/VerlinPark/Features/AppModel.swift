import AppKit
import Observation

struct ArtifactPreviewData {
    let kind: ArtifactPreviewKind
    let sourceURL: URL?
    let inlineData: Data?
    let image: NSImage?
    let text: String?
    let totalBytes: Int
    let truncated: Bool
    let error: String?

    var hexSource: ArtifactHexSource? {
        if let sourceURL {
            return .file(sourceURL)
        }
        if let inlineData {
            return .inline(inlineData)
        }
        return nil
    }
}

struct WorkspaceDirectoryState: Equatable {
    var entries: [WorkspaceEntry] = []
    var total = 0
    var hasMore = false
    var truncated = false
    var loading = false
    var loadingMore = false
    var error: String?
}

@MainActor
@Observable
final class AppModel {
    private let noticesKey = "verlinpark.notices"

    enum ConnectionState: Equatable {
        case idle
        case connecting
        case connected
        case failed(String)
    }

    enum WorkspaceMode: Equatable {
        case resident
        case project
    }

    var connection: ConnectionState = .idle
    var mode: WorkspaceMode = .resident
    var projects: [Project] = []
    var sessions: [Session] = []
    var allSessions: [Session] = []
    var allSessionsComplete = false
    var events: [Event] = []
    var selectedProjectID: String?
    var selectedSessionID: String?
    var isBusy = false
    var lastError: String?
    var agentInfo: AgentInfo?
    var toasts: [VPToast] = []
    var streamingText = ""
    var diffFiles: [SessionDiffFile] = []
    var searchQuery = ""
    var searchHits: [SearchHit] = []
    var searchTruncated = false
    var searchBusy = false
    var notices: [AppNotice] = []
    var sessionDirectory: [String: SessionDirectoryEntry] = [:]
    var todos: [TodoItem] = []
    var agents: [AgentRecord] = []
    var agentIdentities: [AgentIdentity] = []
    var agentIdentitiesStatus: String?
    var agentIdentitiesBusy = false
    var sessionAssignments: [AgentAssignment] = []
    var sessionAssignmentsStatus: String?
    var sessionAssignmentsBusy = false
    var agentWorkspaces: [AgentWorkspace] = []
    var agentWorkspacesStatus: String?
    var agentWorkspacesBusy = false
    var sessionWorkspaces: [SessionWorkerWorkspace] = []
    var sessionWorkspacesStatus: String?
    var sessionWorkspacesBusy = false
    var coordinationInbox: [AgentInboxMessage] = []
    var coordinationInboxCounts: AgentInboxCounts?
    var coordinationInboxStatus: String?
    var coordinationInboxBusy = false
    var mergeQueue: [MergeQueueEntry] = []
    var mailbox: [AgentMessage] = []
    var artifacts: [Artifact] = []
    var artifactVersions: [ArtifactVersion] = []
    var artifactVersionsArtifactID: String?
    var artifactVersionsBusy = false
    var artifactVersionsError: String?
    var artifactDiff: ArtifactDiffResult?
    var artifactDiffBusy = false
    var artifactDiffError: String?
    var sessionCitations: [MessageCitation] = []
    var sessionCitationsBusy = false
    var sessionCitationsError: String?
    var citationAudit: CitationAudit?
    var usageLedger: UsageLedgerResult?
    var usageStatus: String?
    var sessionCacheStatus: SessionCacheStatus?
    var sessionCacheCalls: [SessionCacheCall] = []
    var sessionCacheCallsTruncated = false
    var sessionInstructions: SessionInstructionsReport?
    var sessionContract: SessionContractReport?
    var sessionEffects: SessionEffectsReport?
    var sessionGates: SessionGatesReport?
    var sessionCompatibility: CompatibilityReportView?
    var otelStatus: OtelStatusReport?
    var otelMessage: String?
    var otelBusy = false
    var governorStatus: GovernorSessionStatus?
    var governorStatusMessage: String?
    var efficiencyBusy = false
    var efficiencyStatus: String?
    var shellPolicy = ShellPolicy.allow
    var shellPolicyNetwork = "allow"
    var shellPolicyRestrictHome = false
    var shellPolicyReadJail = false
    var shellPolicyReadDenyText = ""
    var shellPolicyBusy = false
    var shellPolicyStatus: String?
    var integrationPolicy = IntegrationPolicy.disabled
    var integrationAutoResolve = false
    var integrationMaxAutoAttempts = 1
    var integrationMaxTokensText = ""
    var integrationPolicyBusy = false
    var integrationPolicyStatus: String?
    var remoteIntegrationStatus: RemoteIntegrationStatusResult?
    var remoteIntegrationBusy = false
    var remoteIntegrationStatusMessage: String?
    var remoteBootstrapStatus: RemoteBootstrapStatusResult?
    var remoteTestStatus: RemoteTestStatusResult?
    var remoteOperationsBusy = false
    var remoteOperationsMessage: String?
    var guiPolicy = GuiPolicy.standard
    var guiGraceSecs = 45
    var guiWaitMs = 15000
    var guiStatusTtlSecs = 30
    var guiSensitiveMode: GuiSensitiveMode = .deny
    var guiSensitiveBundlesText = ""
    var guiSensitiveTitlesText = ""
    var guiTimelineRetentionDays = 30
    var guiTimelinePruneResult: GuiTimelinePruneResult?
    var guiTimelineRetention: GuiTimelineRetentionResult?
    var guiTimelineAudit: GuiTimelineAuditResult?
    var guiTimelinePruneBusy = false
    var daemonAgentStatus: DaemonAgentStatus?
    var daemonAgentBusy = false
    var guiPolicyBusy = false
    var guiPolicyStatus: String?
    var guiStatus: GuiStatusResult?
    var guiTimeline: [GuiActionInfo] = []
    var guiStatusBusy = false
    var guiStatusError: String?
    var toolOutputPolicy = ToolOutputPolicy.default
    var toolOutputEnabled = true
    var toolOutputSpillThresholdText = "16384"
    var toolOutputPreviewBytesText = "4096"
    var toolOutputMaxTurnBytesText = "65536"
    var toolOutputRetentionSecsText = "604800"
    var toolOutputMaxEntriesText = "2000"
    var toolOutputToolThresholds: [String: Int] = [:]
    var toolOutputPolicyBusy = false
    var toolOutputPolicyStatus: String?
    var testCacheStats: TestCacheStats?
    var testCacheEntries: [TestCacheEntry] = []
    var testCachePolicy = TestCachePolicy.default
    var testCacheEnabled = true
    var testCacheMaxEntriesText = "512"
    var testCacheRetentionDaysText = "14"
    var testCacheMaxOutputBytesText = "262144"
    var testCacheReuseLint = true
    var testCacheIncludeIntegration = false
    var testCacheFailedRetentionText = "900"
    var testCacheExtraEnvText = ""
    var testCacheExtraPathsText = ""
    var testCacheScopePreview: TestCacheScopePreview?
    var testCacheBusy = false
    var testCacheStatus: String?
    var testImpactResult: TestCacheImpactResult?
    var testImpactBusy = false
    var testImpactStatus: String?
    var artifactPolicy = ArtifactPolicy.disabled
    var artifactAutoGc = false
    var artifactMaxBytesText = ""
    var artifactMaxAgeText = ""
    var artifactMaxCountText = ""
    var artifactPolicyBusy = false
    var artifactPolicyStatus: String?
    var projectBudget = ProjectBudget.empty
    var projectSpentTokens = 0
    var projectSpentCostUsd = 0.0
    var projectUsage: ProjectUsageResult?
    var projectUsageBusy = false
    var projectUsageStatus: String?
    var pendingApprovals: [Approval] = []
    var approvalSessionGoals: [String: String] = [:]
    var approvalProjectIDs: [String: String] = [:]
    var approvalCenterBusy = false
    var approvalCenterStatus: String?
    var approvalSelection: Set<String> = []
    var approvalBatchBusy = false
    var approvalBatchStatus: String?
    var approvalTemplates: [ApprovalTemplate] = []
    var approvalTemplateBusy = false
    var approvalTemplateStatus: String?
    var attentionPolicy: AttentionPolicy?
    var attentionPolicyBusy = false
    var attentionPolicyStatus: String?
    var questions: [Question] = []
    var pendingQuestions: [Question] = []
    var questionCenterBusy = false
    var questionCenterStatus: String?
    var acpAgents: [AcpAgent] = []
    var acpBusy = false
    var acpStatus: String?
    var acpProbeStatus: [String: String] = [:]
    var mcpServers: [McpServer] = []
    var mcpBusy = false
    var mcpStatus: String?
    var mcpProbeBusy: Set<String> = []
    var mcpProbeStatus: [String: String] = [:]
    var mcpTools: [String: [McpTool]] = [:]
    var mcpResources: [String: McpResourceListResult] = [:]
    var mcpPrompts: [String: McpPromptListResult] = [:]
    var mcpToolStatus: [String: String] = [:]
    var memories: [MemoryItem] = []
    var memoryQuery = ""
    var memoryBusy = false
    var memoryStatus: String?
    var gitCommits: [GitCommit] = []
    var gitBranch: String?
    var gitBaseCommit: String?
    var gitLogBusy = false
    var gitLogError: String?
    var gitSelectedCommit: String?
    var gitShow: GitShowResult?
    var gitShowBusy = false
    var gitShowError: String?
    var gitHistorySessionID: String?
    var workspaceEntries: [WorkspaceEntry] = []
    var workspaceTreeBusy = false
    var workspaceTreeError: String?
    var workspaceDirectories: [String: WorkspaceDirectoryState] = [:]
    var workspaceSearchResults: [WorkspaceEntry] = []
    var workspaceSearchBusy = false
    var workspaceSearchError: String?
    var workspaceSearchTruncated = false
    var workspaceSearchQuery = ""
    var workspaceSearchMode: WorkspaceSearchMode = .names
    var workspaceContentMatches: [WorkspaceGrepMatch] = []
    var workspaceContentSearchBusy = false
    var workspaceContentSearchError: String?
    var workspaceContentSearchTruncated = false
    var workspaceContentFilesScanned = 0
    var workspaceContentSearchQuery = ""
    var workspaceFilter = ""
    var workspaceSelectedPath: String?
    var workspaceFile: WorkspaceFileResult?
    var workspaceDraft = ""
    var workspaceDraftPath: String?
    var workspaceFileBusy = false
    var workspaceFileError: String?
    var workspaceSaveBusy = false
    var workspaceSaveStatus: String?
    var workspaceSessionID: String?
    var terminalID: String?
    var terminalWorktree = ""
    var terminalData = Data()
    var terminalSeq: UInt64 = 0
    var terminalStatus = "closed"
    var terminalExitCode: Int?
    var terminalCols = 80
    var terminalRows = 24
    var terminalBusy = false
    var terminalError: String?
    var terminalSessionID: String?
    var consoleID: String?
    var consoleWorktree = ""
    var consoleOutput = ""
    var consoleSeq: UInt64 = 0
    var consoleStatus = "closed"
    var consoleExitCode: Int?
    var consoleInput = ""
    var consoleBusy = false
    var consoleError: String?
    var supervisedProcesses: [SupervisedProcess] = []
    var processSummary: [SupervisedProcess] = []
    var selectedProcessID: String?
    var processLogs = ""
    var processesBusy = false
    var processesError: String?
    var processStopBusy = false
    var memoryPolicyEnabled = false
    var memoryPolicyMaxItems = "1000"
    var memoryPolicyMaxAgeDays = "180"
    var memoryPolicyTaskResultVerifyDays = "90"
    var memoryPolicyDefaultVerifyDays = "365"
    var memoryPolicyAutoSupersede = true
    var memoryPolicyBriefMaxChars = "4000"
    var memoryPolicySweepStale = false
    var memoryPolicyBusy = false
    var memoryPolicyStatus: String?
    var memoryEmbeddingEnabled = false
    var memoryEmbeddingEndpoint = ""
    var memoryEmbeddingModel = ""
    var memoryEmbeddingDimensions = "1536"
    var memoryEmbeddingBatchSize = "16"
    var memoryEmbeddingTimeoutSecs = "30"
    var memoryEmbeddingMaxCandidates = "64"
    var memoryEmbeddingMinSimilarity = "0.20"
    var memoryEmbeddingAPIKeyEnv = ""
    var memoryEmbeddingIndex: MemoryIndexMode = .scan
    var memoryEmbeddingIVFNlist = "64"
    var memoryEmbeddingIVFNprobe = "8"
    var memoryEmbeddingIVFMinVectors = "256"
    var memoryEmbeddingIVFRebuildVectors = "4096"
    var memoryEmbeddingsStatus: MemoryEmbeddingsStatusResult?
    var memoryEmbeddingIndexStatus: MemoryEmbeddingIndexStatus?
    var memoryEmbeddingBusy = false
    var memoryEmbeddingStatusMessage: String?
    var memoryEmbeddingTestResult: MemoryEmbeddingsTestResult?
    var memoryEmbeddingRebuildResult: MemoryEmbeddingsRebuildResult?
    var memoryEmbeddingIndexRebuildResult: MemoryEmbeddingsIndexRebuildResult?

    /// Mirrors the daemon gates for the explicit ANN rebuild action: a project
    /// must be selected, no policy operation may be in flight, the semantic
    /// lane must be enabled and configured, and the operator must have chosen
    /// IVF. This is UI gating only; the daemon re-validates everything.
    var canRebuildMemoryEmbeddingIndex: Bool {
        guard selectedProjectID != nil,
              !memoryEmbeddingBusy,
              !memoryPolicyBusy,
              memoryEmbeddingEnabled,
              memoryEmbeddingIndex == .ivf
        else { return false }
        let endpoint = memoryEmbeddingEndpoint
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let model = memoryEmbeddingModel
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return !endpoint.isEmpty && !model.isEmpty
    }

    var memoryNewKind = "note"
    var memoryNewTitle = ""
    var memoryNewBody = ""
    var memoryIncludeStale = false
    var memoryStats: MemoryStatsResult?
    var memoryStatsStatus: String?
    var memoryConflicts: [MemoryConflict] = []
    var memoryHealth: MemoryHealthResult?
    var memoryConflictBusy = false
    var memoryConflictStatus: String?
    var memoryActionBusyID: String?
    var memorySweepBusy = false
    var projectTokenBudgetText = ""
    var projectCostBudgetText = ""
    var projectBudgetBusy = false
    var projectBudgetStatus: String?
    var mailboxPolicy = MailboxPolicy.disabled
    var mailboxMaxAgeText = ""
    var mailboxMaxCountText = ""
    var mailboxDeleteUnread = false
    var mailboxAutoGc = false
    var mailboxPolicyBusy = false
    var mailboxPolicyStatus: String?
    var teamPlan: TeamPlan?
    var planTaskStatuses: [String: String] = [:]
    var taskQueue: TaskQueueResult?
    var taskQueueBusy = false
    var taskQueueStatus: String?
    /// Task rows for the selected session. Collab uses these for authoritative
    /// status, attempts, dependencies, and failure reasons.
    var scheduledSessionTasks: [SchedulerTask] = []
    var scheduledSessionTasksSessionID: String?
    var orchestrationSnapshot: OrchestrationSnapshot?
    var orchestrationPolicy = OrchestrationPolicy.standard
    var orchestrationPolicyBusy = false
    var orchestrationPolicyStatus: String?
    var collabTaskActionStatus: String?
    var collabTaskActionBusyID: String?
    var planRevisionBusy = false
    var planRevisionStatus: String?
    var planRevisionDetail: PlanRevisionDetailResult?
    var planRevisionDetailBusy = false
    var planRevisionDetailStatus: String?
    var planRestoreBusy = false
    var planRestoreStatus: String?
    var orchestrationPreview: OrchestrationPreviewResult?
    var orchestrationPreviewBusy = false
    var orchestrationPreviewStatus: String?
    var runtimeLeases: [RuntimeLease] = []
    var runtimeLeaseOwnerID: String?
    var runtimeLeasesBusy = false
    var runtimeLeasesStatus: String?
    var orchestrationOverview: OrchestrationOverviewResult?
    var orchestrationOverviewBusy = false
    var orchestrationOverviewStatus: String?
    var steeringMessages: [SteeringMessage] = []
    var continuations: [SessionContinuation] = []
    var recoveries: [OrchestrationRecovery] = []
    var taskProgress: [PlanTaskProgress] = []
    var taskStalls: [TaskStall] = []
    var sessionPreemption: SessionPreemptionResult?
    var preemptionOverview: PreemptionOverviewResult?
    var preemptionBusy = false
    var preemptionStatus: String?
    var preemptionPolicyDraft = PreemptionPolicy()
    var preemptionPolicyBusy = false
    var preemptionPolicyStatus: String?
    var stallThresholdSecs: UInt64 = 0
    var longTaskBusy = false
    var longTaskStatus: String?
    var steeringSubmitBusy = false
    var steeringCancelBusyID: String?
    var continuationSubmitBusy = false
    var continuationActionBusyID: String?
    private var orchestrationRefreshTask: Task<Void, Never>?
    private var longTaskRefreshTask: Task<Void, Never>?
    private var efficiencyRefreshTask: Task<Void, Never>?
    private var toolActivityRefreshTask: Task<Void, Never>?
    var taskActionBusyID: String?
    var taskActionStatus: String?
    var taskSchedulerPolicy = TaskSchedulerPolicy.disabled
    var taskSchedulerEnabled = false
    var taskSchedulerDispatchTasks = false
    var taskSchedulerIncludeIntake = true
    var taskSchedulerMaxIntakeParallelText = "2"
    var taskSchedulerCooldownText = "60"
    var taskSchedulerPolicyBusy = false
    var taskSchedulerPolicyStatus: String?
    var schedulerStatus: SchedulerStatusResult?
    var schedulerStatusBusy = false
    var schedulerStatusError: String?
    var scalingPolicy = ScalingPolicy.disabled
    var scalingEnabled = false
    var scalingMinWorkersText = "1"
    var scalingMaxWorkersText = "4"
    var scalingMinLabourText = "1"
    var scalingMaxLabourText = "8"
    var scalingReadyThresholdText = "2"
    var scalingCriticalDelayText = "120"
    var scalingIdleSecsText = "600"
    var scalingCooldownText = "120"
    var scalingMaxStepText = "2"
    var scalingBudgetRatioText = ""
    var scalingPolicyBusy = false
    var scalingPolicyStatus: String?
    var scalingStatus: ScalingStatusResult?
    var scalingStatusBusy = false
    var scalingStatusError: String?
    var scalingPreview: ScalingPreviewResult?
    var scalingPreviewBusy = false
    var sources: [ProjectSource] = []
    var sourceStats: SourceStats?
    var sourceCollections: [SourceCollection] = []
    var sourceCollectionFilter: String?
    var sourcesBusy = false
    var sourceStatus: String?
    var sourceSelectedID: String?
    var sourceSelected: SourceGetResult?
    var sourceSelectedChunk: UInt32?
    var sourceSelectedBusy = false
    var sourceSearchQuery = ""
    var sourceSearchResults: [SourceSearchHit] = []
    var sourceSearchDegraded = false
    var sourceSearchBusy = false
    var sourceSearchError: String?
    var sourceSearchMode: SourceSearchMode = .automatic
    var sourceSearchEffectiveMode = ""
    var sourceSearchFallbackReason: String?
    var sourceEmbeddingStatus: SourceEmbeddingStatusResult?
    var sourceEmbeddingBusy = false
    var sourceEmbeddingMessage: String?
    var sourceRetrievalPolicy: SourceRetrievalPolicy?
    var sourceIndexStatus: SourceIndexStatusResult?
    var sourceRetrievalBusy = false
    var sourceRetrievalMessage: String?
    var sourceDraftTitle = ""
    var sourceDraftContent = ""
    var sourceDraftCollection = ""
    var sourceDraftPath = ""
    var sourceDraftURL = ""
    var sourceActionBusyID: String?
    var skills: [ProjectSkill] = []
    var skillsBusy = false
    var skillsStatus: String?
    var skillActivations: [SkillActivation] = []
    var skillActivationsBusy = false
    var skillActionBusyID: String?
    var intakeTasks: [IntakeTask] = []
    var intakeBusy = false
    var intakeStatus: String?
    var intakeActionBusyID: String?
    var intakeDraftTitle = ""
    var intakeDraftInstructions = ""
    var intakeDraftProfile = "coding"
    var intakeDraftKind = ""
    var intakeDraftPriorityText = "0"
    var intakeDraftScope = ""
    var intakeDraftAcceptance = ""
    var intakeDraftMaxAttemptsText = "2"
    var intakeDraftTokenBudget = ""
    var intakeDraftCostBudget = ""
    var intakeDraftTimeBudget = ""
    var intakeDraftLabels = ""
    var intakeDraftDeps: Set<String> = []
    var intakeEditingID: String?
    var programEntries: [ProgramListEntry] = []
    var programBusy = false
    var programStatus: String?
    var programActionBusyID: String?
    var selectedProgramID: String?
    var programDetail: ProgramStatusResult?
    var programDetailBusy = false
    var programDraftGoal = ""
    var programDraftActivate = true
    var programDraftAutoDispatch = true
    var programDraftTokenBudget = ""
    var programDraftCostBudget = ""
    var programDraftMaxAttemptsText = "2"
    var programDraftBackoffText = "60"
    var programDraftParallelText = "2"
    var programDraftDependencyFailure = "block"
    var programMilestoneDraftTitle = ""
    var programMilestoneDraftObjective = ""
    var programMilestoneDraftAcceptance = ""
    var programMilestoneDraftScope = "src"
    var programMilestoneDraftPriorityText = "0"
    var programMilestoneDraftMaxAttemptsText = "2"
    var programMilestoneDraftProfile = "coding"
    var programMilestoneDraftTokenBudget = ""
    var programMilestoneDraftCostBudget = ""
    var programMilestoneDraftDeps: Set<String> = []
    var contextStatus: SessionContextStatus?
    var toolOutputStatus: ToolOutputStatus?
    var toolActivity: ToolActivityResult?
    var toolActivityStatus: String?
    var toolActivityBusy = false
    var compactBusy = false
    var compactNotice: String?
    var contextCheckpoints: [ContextCheckpoint] = []
    var contextCheckpointDetails: [String: ContextCheckpointDetailResult] = [:]
    var contextCheckpointPolicy: ContextCheckpointPolicy?
    var contextCheckpointBusy = false
    var contextCheckpointNotice: String?
    var forkBusy = false
    var checkpoints: [SessionCheckpoint] = []
    var checkpointBusy = false
    var checkpointNotice: String?
    var checkpointDiff: SessionCheckpointDiffResult?
    var checkpointDiffBusy = false
    var checkpointDiffNotice: String?
    var checkpointStats: SessionCheckpointStatsResult?
    var checkpointStatsBusy = false
    var sessionLineage: SessionLineageResult?
    var lineageBusy = false
    var forkTree: SessionForkTreeResult?
    var forkTreeBusy = false
    var dashboardSnapshot: DashboardResult?
    var dashboardBusy = false
    var dashboardStatus: String?
    var projectForkForest: ProjectForkForestResult?
    var projectForkForestBusy = false
    var forkAudit: ForkAuditResult?
    var forkAuditBusy = false
    var forkRepairNotice: String?
    var compareResult: SessionCompareResult?
    var compareBusy = false
    var compareError: String?
    var forkMergeBusy = false
    var forkMergeNotice: String?
    var sessionDiff: SessionDiff?
    var diffLoading = false
    var diffError: String?
    var lastCommit: SessionCommit?
    var commitBusy = false
    var landingBusy = false
    var lastLanding: SessionLandResult?
    var pullRequestBusy = false
    var lastPullRequest: PullRequestResult?
    var forgeStatus: ForgeStatus?
    var forgeStatusBusy = false
    var forgeStatusError: String?
    var forgeCheckResult: ForgePreflightResult?
    var forgeCheckBusy = false
    var forgeCheckError: String?
    var providerKind =
        UserDefaults.standard.string(forKey: "provider.kind") ?? "openai-compatible"
    var providerBaseURL =
        UserDefaults.standard.string(forKey: "provider.baseURL")
            ?? "https://api.openai.com/v1"
    var providerModel = UserDefaults.standard.string(forKey: "provider.model") ?? ""
    var providerContextWindowText =
        UserDefaults.standard.string(forKey: "provider.contextWindow") ?? "128000"
    var roleModels = AppModel.loadRoleModels()
    var modelPresets: [ModelPreset] = []
    var modelPresetsBusy = false
    var modelPresetsStatus: String?
    var selectedModelPresetID: String?
    var modelPresetName = ""
    var modelPresetProvider = "openai-compatible"
    var modelPresetModel = ""
    var modelPresetContextText = ""
    var modelPresetInputPriceText = ""
    var modelPresetOutputPriceText = ""
    var modelPresetNotes = ""
    var orgManagers = 4
    var orgWorkers = 6
    var orgLabour = 12
    var autoExpandWorkers = false
    /// Newly typed secret. Stored secrets are read lazily so launching the app
    /// with a local provider never opens a Keychain password dialog.
    var providerAPIKey = ""
    var providerHasStoredKey =
        UserDefaults.standard.bool(forKey: "provider.hasAPIKey")
    var approvalMode = AgentApprovalMode.stored
    var approvalGrants: [ApprovalGrant] = []
    var approvalGrantBusy = false
    var approvalGrantStatus: String?
    var providerStatus: String?
    var providerBusy = false
    var providerProbeBusy = false
    var providerProbeStatus: String?
    var planResumeBusy = false
    var cleanupOlderDaysText = "7"
    var cleanupDryRun = true
    var cleanupDeleteBranches = false
    var cleanupKeepFailed = false
    var cleanupBusy = false
    var cleanupStatus: String?
    var cleanupResult: WorktreeCleanupResult?
    var checkpointPolicy = CheckpointRetentionPolicy.disabled
    var checkpointPolicyEnabled = false
    var checkpointPolicyOlderDaysText = "30"
    var checkpointPolicyKeepLatestText = "5"
    var checkpointPolicyBusy = false
    var checkpointPolicyStatus: String?
    var worktreePolicy = WorktreeRetentionPolicy.disabled
    var worktreePolicyEnabled = false
    var worktreePolicyOlderDaysText = "7"
    var worktreePolicyDeleteBranches = false
    var worktreePolicyKeepFailed = true
    var worktreePolicyBusy = false
    var worktreePolicyStatus: String?
    var updateResult: UpdateCheckResult?
    var updateBusy = false
    var updateError: String?
    var updateDownloadBusy = false
    var updateDownloadResult: UpdateDownloadResult?
    var updateDownloadError: String?
    var backupStatus: BackupStatusResult?
    var backupStatusBusy = false
    var backupStatusError: String?
    var doctorReport: DoctorReport?
    var doctorBusy = false
    var doctorStatus: String?

    private var parsedContextWindow: Int? {
        let trimmed = providerContextWindowText.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, let value = Int(trimmed) else { return nil }
        return max(0, min(value, 10_000_000))
    }

    private static func loadRoleModels() -> RoleModels {
        guard let data = UserDefaults.standard.data(forKey: "provider.roleModels"),
              let models = try? JSONDecoder().decode(RoleModels.self, from: data)
        else {
            return RoleModels()
        }
        return models
    }

    private let core = CoreClient()
    private let daemon = DaemonController()
    private var eventTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var reconnectAttempt = 0
    private var started = false
    private var selectionGeneration = 0
    private var searchGeneration = 0

    var unreadNoticeCount: Int {
        notices.count(where: { !$0.isRead })
    }

    var pendingContinuationCount: Int {
        continuations.count(where: \.isPending)
    }

    var activeRecoveryCount: Int {
        recoveries.count(where: \.isActiveRecovery)
    }

    var hasLongTaskAttention: Bool {
        pendingContinuationCount > 0 || activeRecoveryCount > 0
    }

    func clearLongTaskStatus() {
        longTaskStatus = nil
    }

    var selectedProject: Project? {
        projects.first { $0.id == selectedProjectID }
    }

    var roundtableRounds: [RoundtableRound] {
        var grouped: [Int: [RoundtablePosition]] = [:]
        for event in events where event.type == "manager.roundtable" {
            let payload = event.payload.objectValue
            guard let position = payload?["position"]?.stringValue else { continue }
            let round = payload?["round"]?.intValue ?? 0
            let title = payload?["title"]?.stringValue
                ?? payload?["agent_title"]?.stringValue
                ?? payload?["agent_name"]?.stringValue
                ?? "Manager"
            grouped[round, default: []].append(
                RoundtablePosition(
                    id: event.id,
                    title: title,
                    profile: payload?["profile"]?.stringValue,
                    position: position,
                    agentName: payload?["agent_name"]?.stringValue
                )
            )
        }
        return grouped
            .sorted { $0.key < $1.key }
            .map { RoundtableRound(round: $0.key, positions: $0.value) }
    }

    var selectedSession: Session? {
        sessions.first { $0.id == selectedSessionID }
    }

    /// The model the daemon will actually use for the session Leader.
    ///
    /// This is `agent.info.role_models.leader.model` when a role override is
    /// configured, otherwise `agent.info.model`. It deliberately never falls
    /// back to the local `providerModel` UserDefaults value: that value can be
    /// stale after a daemon restart or a settings edit and must not be shown as
    /// the live run's model.
    var daemonModelLabel: String? {
        agentInfo?.configuredModelLabel
    }

    var daemonModelSource: String? {
        guard let info = agentInfo, info.configured else { return nil }
        let leader = info.roleModels.leader.model?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return leader.isEmpty ? "agent.info.model" : "agent.info.role_models.leader"
    }

    /// Dashboard/popover view of the selected run, assembled from real RPC
    /// results. `session.cache` is optional until the daemon ships it.
    var efficiencySnapshot: EfficiencySnapshot? {
        EfficiencyMetrics.makeSnapshot(
            projectUsage: projectUsage,
            usage: usageLedger,
            context: contextStatus,
            cache: sessionCacheStatus,
            toolOutputs: toolOutputStatus,
            eventTypes: events.map(\.type),
            latestPrefixHash: latestPromptPrefixHash
        )
    }

    /// Per-call table rows. The dedicated `session.cache.calls` RPC is newest
    /// first and filterable; older daemons fall back to the bounded list on
    /// `session.cache`.
    var cacheCallRows: [SessionCacheCall] {
        if !sessionCacheCalls.isEmpty {
            return sessionCacheCalls
        }
        return sessionCacheStatus?.recentCalls ?? []
    }

    var latestPromptPrefixHash: String? {
        guard let selectedSessionID else { return nil }
        for event in events.reversed() where event.sessionId == selectedSessionID {
            guard event.type == "context.prompt_prefix" else { continue }
            let payload = event.payload.objectValue
            if let hash = payload?["prefix_hash"]?.stringValue {
                return hash
            }
            if let hash = payload?["cache_generation"]?.stringValue {
                return hash
            }
        }
        return nil
    }

    private func isCurrent(sessionID: String, generation: Int) -> Bool {
        selectedSessionID == sessionID && selectionGeneration == generation
    }

    private func resetLongTaskState() {
        longTaskRefreshTask?.cancel()
        steeringMessages = []
        continuations = []
        recoveries = []
        taskProgress = []
        taskStalls = []
        stallThresholdSecs = 0
        sessionPreemption = nil
        preemptionOverview = nil
        preemptionBusy = false
        preemptionStatus = nil
        preemptionPolicyDraft = PreemptionPolicy()
        preemptionPolicyBusy = false
        preemptionPolicyStatus = nil
        longTaskBusy = false
        longTaskStatus = nil
        steeringSubmitBusy = false
        steeringCancelBusyID = nil
        continuationSubmitBusy = false
        continuationActionBusyID = nil
    }

    private func resetEfficiencyState() {
        efficiencyRefreshTask?.cancel()
        toolActivityRefreshTask?.cancel()
        sessionCacheStatus = nil
        sessionCacheCalls = []
        sessionCacheCallsTruncated = false
        sessionInstructions = nil
        sessionContract = nil
        otelStatus = nil
        otelMessage = nil
        otelBusy = false
        governorStatus = nil
        governorStatusMessage = nil
        toolOutputStatus = nil
        toolActivity = nil
        toolActivityStatus = nil
        toolActivityBusy = false
        efficiencyBusy = false
        efficiencyStatus = nil
    }

    func start() async {
        guard !started else { return }
        started = true
        loadNotices()
        loadOrgCaps()
        ArtifactPreviewCache.sweepStaleFiles()
        SystemNotifier.shared.requestAuthorization()
        SystemNotifier.shared.onResolveApproval = { [weak self] approvalID, approved in
            guard let self else { return }
            Task { @MainActor in
                await self.resolveGlobalApproval(approvalID, approved: approved)
            }
        }
        SystemNotifier.shared.onOpenSession = { [weak self] sessionID in
            guard let self else { return }
            Task { @MainActor in
                if let projectID = self.sessionDirectory[sessionID]?.projectId {
                    await self.openSession(sessionID: sessionID, projectID: projectID)
                } else {
                    await self.refresh()
                    if let projectID = self.sessionDirectory[sessionID]?.projectId {
                        await self.openSession(sessionID: sessionID, projectID: projectID)
                    }
                }
            }
        }
        await connectIfNeeded()
        if connection != .connected {
            scheduleReconnect()
        }
    }

    private func connectIfNeeded() async {
        guard started, connection != .connected else { return }
        connection = .connecting
        do {
            let discovery = try await daemon.ensureRunning()
            try await core.connect(to: discovery)
            await core.setDisconnectHandler { [weak self] error in
                Task { @MainActor in
                    guard let self else { return }
                    self.connection = .failed(error.localizedDescription)
                    self.lastError = error.localizedDescription
                    self.scheduleReconnect()
                }
            }
            connection = .connected
            reconnectAttempt = 0
            await configureStoredProvider()
            agentInfo = try? await core.agentInfo()
            if let info = agentInfo {
                roleModels = info.roleModels
                adoptApprovalMode(from: info)
            }
            consumeEvents()
            await refresh()
        } catch {
            reconnectAttempt += 1
            connection = .failed(error.localizedDescription)
            lastError = error.localizedDescription
        }
    }

    private func scheduleReconnect() {
        guard started, reconnectTask == nil, connection != .connected else { return }
        reconnectTask = Task { [weak self] in
            guard let self else { return }
            defer {
                self.reconnectTask = nil
            }
            while started, connection != .connected, !Task.isCancelled {
                let delays = [1, 2, 4, 8, 16, 30, 30, 30]
                let delay = delays[min(reconnectAttempt, delays.count - 1)]
                try? await Task.sleep(for: .seconds(delay))
                if Task.isCancelled {
                    return
                }
                await connectIfNeeded()
            }
        }
    }

    func refresh() async {
        guard connection == .connected else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            projects = try await core.listProjects()
            if selectedProjectID == nil ||
                !projects.contains(where: { $0.id == selectedProjectID })
            {
                selectedProjectID = projects.first?.id
            }
            await loadSessions()
            await refreshSessionDirectory()
            await loadShellPolicy()
            await loadIntegrationPolicy()
            await loadArtifactPolicy()
            await loadMailboxPolicy()
            await loadProjectBudget()
            await loadAcpAgents()
            await loadMcpServers()
            await loadMemoryPolicy()
            await loadOrchestrationPolicy()
            await loadApprovalGrants()
        } catch {
            handle(error)
        }
    }

    func selectProject(_ id: String?) {
        guard let id else {
            selectionGeneration &+= 1
            selectedProjectID = nil
            selectedSessionID = nil
            events = []
            contextStatus = nil
            compactNotice = nil
            contextCheckpoints = []
            contextCheckpointDetails = [:]
            contextCheckpointPolicy = nil
            contextCheckpointNotice = nil
            checkpoints = []
            checkpointNotice = nil
            checkpointStats = nil
            checkpointDiff = nil
            checkpointDiffNotice = nil
            sessionLineage = nil
            forkTree = nil
            projectForkForest = nil
            dashboardSnapshot = nil
            dashboardStatus = nil
            forkAudit = nil
            forkRepairNotice = nil
            compareResult = nil
            compareError = nil
            forkMergeNotice = nil
            sessionDiff = nil
            todos = []
            agents = []
            mergeQueue = []
            mailbox = []
            artifacts = []
            usageLedger = nil
            usageStatus = nil
            resetEfficiencyState()
            shellPolicy = .allow
            shellPolicyNetwork = "allow"
            shellPolicyRestrictHome = false
            shellPolicyReadJail = false
            shellPolicyReadDenyText = ""
            shellPolicyStatus = nil
            integrationPolicy = .disabled
            integrationAutoResolve = false
            integrationMaxAutoAttempts = 1
            integrationMaxTokensText = ""
            integrationPolicyStatus = nil
            guiPolicy = .standard
            guiGraceSecs = 45
            guiWaitMs = 15000
            guiStatusTtlSecs = 30
            guiSensitiveMode = .deny
            guiSensitiveBundlesText = ""
            guiSensitiveTitlesText = ""
            guiTimelineRetentionDays = 30
            guiTimelinePruneResult = nil
            guiTimelineRetention = nil
            guiTimelineAudit = nil
            guiPolicyStatus = nil
            daemonAgentStatus = nil
            guiStatus = nil
            guiTimeline = []
            guiStatusError = nil
            artifactPolicy = .disabled
            artifactAutoGc = false
            artifactMaxBytesText = ""
            artifactMaxAgeText = ""
            artifactMaxCountText = ""
            artifactPolicyStatus = nil
            mailboxPolicy = .disabled
            mailboxMaxAgeText = ""
            mailboxMaxCountText = ""
            mailboxDeleteUnread = false
            mailboxAutoGc = false
            mailboxPolicyStatus = nil
            projectBudget = .empty
            projectSpentTokens = 0
            projectSpentCostUsd = 0
            projectTokenBudgetText = ""
            projectCostBudgetText = ""
            projectBudgetStatus = nil
            projectUsage = nil
            projectUsageStatus = nil
            teamPlan = nil
            planTaskStatuses = [:]
            resetLongTaskState()
            lastCommit = nil
            lastLanding = nil
            lastPullRequest = nil
            mode = .resident
            return
        }
        mode = .project
        guard selectedProjectID != id else { return }
        selectionGeneration &+= 1
        selectedProjectID = id
        selectedSessionID = nil
        events = []
        contextStatus = nil
        compactNotice = nil
        contextCheckpoints = []
        contextCheckpointDetails = [:]
        contextCheckpointPolicy = nil
        contextCheckpointNotice = nil
        checkpoints = []
        checkpointNotice = nil
        checkpointStats = nil
        checkpointDiff = nil
        checkpointDiffNotice = nil
        sessionLineage = nil
        forkTree = nil
        forkRepairNotice = nil
        compareResult = nil
        compareError = nil
        forkMergeNotice = nil
        sessionDiff = nil
        diffFiles = []
        todos = []
        agents = []
        mergeQueue = []
        scheduledSessionTasks = []
        scheduledSessionTasksSessionID = nil
        orchestrationSnapshot = nil
        collabTaskActionStatus = nil
        planRevisionStatus = nil
        planRevisionDetail = nil
        planRevisionDetailStatus = nil
        planRestoreStatus = nil
        orchestrationPreview = nil
        orchestrationPreviewStatus = nil
        mailbox = []
        artifacts = []
        usageLedger = nil
        usageStatus = nil
        resetEfficiencyState()
        shellPolicy = .allow
        shellPolicyNetwork = "allow"
        shellPolicyRestrictHome = false
        shellPolicyReadJail = false
        shellPolicyReadDenyText = ""
        shellPolicyStatus = nil
        integrationPolicy = .disabled
        integrationAutoResolve = false
        integrationMaxAutoAttempts = 1
        integrationMaxTokensText = ""
        integrationPolicyStatus = nil
        artifactPolicy = .disabled
        artifactAutoGc = false
        artifactMaxBytesText = ""
        artifactMaxAgeText = ""
        artifactMaxCountText = ""
        artifactPolicyStatus = nil
        mailboxPolicy = .disabled
        mailboxMaxAgeText = ""
        mailboxMaxCountText = ""
        mailboxDeleteUnread = false
        mailboxAutoGc = false
        mailboxPolicyStatus = nil
        projectBudget = .empty
        projectSpentTokens = 0
        projectSpentCostUsd = 0
        projectTokenBudgetText = ""
        projectCostBudgetText = ""
        projectBudgetStatus = nil
        projectUsage = nil
        projectUsageStatus = nil
        memories = []
        taskQueue = nil
        schedulerStatus = nil
        schedulerStatusError = nil
        sources = []
        sourceStats = nil
        sourceCollections = []
        sourceCollectionFilter = nil
        sourceStatus = nil
        sourceSelectedID = nil
        sourceSelected = nil
        sourceSelectedChunk = nil
        sourceSearchQuery = ""
        sourceSearchResults = []
        sourceSearchDegraded = false
        sourceSearchError = nil
        sourceSearchEffectiveMode = ""
        sourceSearchFallbackReason = nil
        sourceEmbeddingStatus = nil
        sourceEmbeddingBusy = false
        sourceEmbeddingMessage = nil
        sourceRetrievalPolicy = nil
        sourceIndexStatus = nil
        sourceRetrievalBusy = false
        sourceRetrievalMessage = nil
        sourceDraftTitle = ""
        sourceDraftContent = ""
        sourceDraftCollection = ""
        sourceDraftPath = ""
        sourceDraftURL = ""
        skills = []
        skillsStatus = nil
        skillActivations = []
        intakeTasks = []
        intakeStatus = nil
        intakeEditingID = nil
        intakeDraftDeps = []
        programEntries = []
        programStatus = nil
        programActionBusyID = nil
        selectedProgramID = nil
        programDetail = nil
        programDetailBusy = false
        programMilestoneDraftDeps = []
        scheduledSessionTasks = []
        scheduledSessionTasksSessionID = nil
        orchestrationSnapshot = nil
        orchestrationPolicy = .standard
        orchestrationPolicyStatus = nil
        collabTaskActionStatus = nil
        gitCommits = []
        gitBranch = nil
        gitBaseCommit = nil
        gitSelectedCommit = nil
        gitShow = nil
        guiTimeline = []
        guiStatusError = nil
        workspaceEntries = []
        resetWorkspaceBrowser()
        workspaceSelectedPath = nil
        workspaceFile = nil
        workspaceFileError = nil
        Task { await loadShellPolicy() }
        Task { await loadIntegrationPolicy() }
        Task { await loadGuiPolicy() }
        Task { await loadGuiStatus() }
        Task { await loadArtifactPolicy() }
        Task { await loadMailboxPolicy() }
        Task { await loadProjectBudget() }
        Task { await loadEfficiency() }
        Task { await loadProjectForkForest() }
        Task { await loadDashboardSnapshot() }
        Task { await loadMemories() }
        Task { await loadCheckpointPolicy() }
        Task { await loadWorktreePolicy() }
        teamPlan = nil
        planTaskStatuses = [:]
        resetLongTaskState()
        lastCommit = nil
        lastLanding = nil
        lastPullRequest = nil
        Task { await loadSessions() }
    }

    func selectResident() {
        selectionGeneration &+= 1
        mode = .resident
    }

    func selectSession(_ id: String?) {
        if selectedSessionID == id {
            mode = .project
            return
        }
        selectionGeneration &+= 1
        mode = .project
        selectedSessionID = id
        Task {
            await closeTerminal()
            await closeConsole()
        }
        streamingText = ""
        contextStatus = nil
        compactNotice = nil
        contextCheckpoints = []
        contextCheckpointDetails = [:]
        contextCheckpointPolicy = nil
        contextCheckpointNotice = nil
        checkpoints = []
        checkpointNotice = nil
        checkpointStats = nil
        checkpointDiff = nil
        checkpointDiffNotice = nil
        sessionLineage = nil
        forkTree = nil
        projectForkForest = nil
        forkAudit = nil
        forkRepairNotice = nil
        compareResult = nil
        compareError = nil
        forkMergeNotice = nil
        sessionDiff = nil
        diffFiles = []
        todos = []
        agents = []
        mergeQueue = []
        mailbox = []
        artifacts = []
        usageLedger = nil
        usageStatus = nil
        resetEfficiencyState()
        questions = []
        pendingQuestions = []
        gitCommits = []
        gitBranch = nil
        gitBaseCommit = nil
        gitSelectedCommit = nil
        gitShow = nil
        guiTimeline = []
        guiStatusError = nil
        workspaceEntries = []
        resetWorkspaceBrowser()
        workspaceSelectedPath = nil
        workspaceFile = nil
        workspaceFileError = nil
        teamPlan = nil
        planTaskStatuses = [:]
        resetLongTaskState()
        lastCommit = nil
        lastLanding = nil
        lastPullRequest = nil
        Task {
            await loadEvents()
            await loadEfficiency()
            await loadCheckpoints()
            await loadLineage()
            await loadTodos()
            await loadAgents()
            await loadMergeQueue()
            await loadScheduledSessionTasks()
            await loadOrchestrationSnapshot()
            await refreshLongTaskState()
            await loadMailbox()
            await loadTeamPlan()
            await loadQuestions()
            await loadDiff()
            await loadGuiTimeline()
        }
    }

    func resumePlan() async {
        guard let sessionID = selectedSessionID else { return }
        planResumeBusy = true
        defer { planResumeBusy = false }
        do {
            _ = try await core.resumePlan(sessionID: sessionID)
            await loadTeamPlan()
        } catch {
            handle(error)
        }
    }

    func exportUsageCsv() async {
        guard let session = selectedSession else { return }
        do {
            let result = try await core.usageCsv(sessionID: session.id)
            let panel = NSSavePanel()
            panel.canCreateDirectories = true
            panel.nameFieldStringValue = "usage-\(session.id.prefix(8)).csv"
            guard panel.runModal() == .OK, let url = panel.url else { return }
            try result.csv.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            handle(error)
        }
    }

    func exportTranscript() async {
        guard let session = selectedSession else { return }
        do {
            let result = try await core.transcript(sessionID: session.id)
            let panel = NSSavePanel()
            panel.canCreateDirectories = true
            let sanitized = session.goal
                .replacingOccurrences(of: "/", with: "-")
                .replacingOccurrences(of: ":", with: "-")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let stem = String(sanitized.prefix(60))
            panel.nameFieldStringValue = (stem.isEmpty ? session.id : stem) + ".md"
            guard panel.runModal() == .OK, let url = panel.url else { return }
            try result.markdown.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            handle(error)
        }
    }

    func runSearch(projectID: String?) async {
        let trimmed = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            searchGeneration &+= 1
            searchHits = []
            searchTruncated = false
            searchBusy = false
            return
        }
        searchGeneration &+= 1
        let generation = searchGeneration
        searchBusy = true
        defer {
            if generation == searchGeneration {
                searchBusy = false
            }
        }
        do {
            let result = try await core.search(query: trimmed, projectID: projectID)
            guard generation == searchGeneration,
                  trimmed == searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
            else {
                return
            }
            searchHits = result.hits
            searchTruncated = result.truncated
        } catch {
            guard generation == searchGeneration else { return }
            searchHits = []
            searchTruncated = false
            handle(error)
        }
    }

    func openSearchHit(_ hit: SearchHit) async {
        await openSession(sessionID: hit.sessionId, projectID: hit.projectId)
    }

    func openSession(sessionID: String, projectID: String) async {
        if selectedProjectID != projectID {
            selectProject(projectID)
        }
        do {
            let loaded = try await core.listSessions(projectID: projectID)
            guard selectedProjectID == projectID else { return }
            sessions = loaded
        } catch {
            if selectedProjectID == projectID {
                handle(error)
            }
            return
        }
        for session in sessions {
            sessionDirectory[session.id] = SessionDirectoryEntry(
                projectId: session.projectId,
                goal: session.goal
            )
        }
        guard sessions.contains(where: { $0.id == sessionID }) else {
            lastError = "That session is no longer available."
            return
        }
        if selectedSessionID != sessionID {
            selectedSessionID = nil
            selectSession(sessionID)
        }
    }

    func openNotice(_ notice: AppNotice) async {
        markNoticeRead(notice.id)
        guard let projectID = notice.projectId ?? sessionDirectory[notice.sessionId]?.projectId
        else {
            await refreshSessionDirectory()
            if let projectID = sessionDirectory[notice.sessionId]?.projectId {
                await openSession(sessionID: notice.sessionId, projectID: projectID)
            }
            return
        }
        await openSession(sessionID: notice.sessionId, projectID: projectID)
    }

    func markNoticeRead(_ id: String) {
        guard let index = notices.firstIndex(where: { $0.id == id }) else { return }
        notices[index].isRead = true
        saveNotices()
        updateDockBadge()
    }

    func markAllNoticesRead() {
        for index in notices.indices {
            notices[index].isRead = true
        }
        saveNotices()
        updateDockBadge()
    }

    func clearNotices() {
        notices = []
        saveNotices()
        updateDockBadge()
    }

    func clearReadNotices() {
        notices.removeAll { $0.isRead }
        saveNotices()
        updateDockBadge()
    }

    func removeNotice(_ id: String) {
        notices.removeAll { $0.id == id }
        saveNotices()
        updateDockBadge()
    }

    func loadAllSessions() async {
        guard connection == .connected else { return }
        do {
            allSessions = try await core.listSessions(projectID: nil)
            allSessionsComplete = true
            sessionDirectory = Dictionary(
                uniqueKeysWithValues: allSessions.map {
                    ($0.id, SessionDirectoryEntry(projectId: $0.projectId, goal: $0.goal))
                }
            )
        } catch {
            allSessions = sessions
            allSessionsComplete = false
        }
    }

    func refreshSessionDirectory() async {
        guard connection == .connected,
              let allSessions = try? await core.listSessions(projectID: nil) else { return }
        sessionDirectory = Dictionary(
            uniqueKeysWithValues: allSessions.map {
                ($0.id, SessionDirectoryEntry(projectId: $0.projectId, goal: $0.goal))
            }
        )
        var patched = false
        for index in notices.indices where notices[index].projectId == nil {
            if let projectID = sessionDirectory[notices[index].sessionId]?.projectId {
                notices[index].projectId = projectID
                patched = true
            }
        }
        if patched {
            saveNotices()
        }
    }

    func addProject() async {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Add Project"
        panel.message = "Choose a Git repository or project folder."
        if panel.runModal() == .OK, let url = panel.url {
            await addProject(at: url)
        }
    }

    func addProject(at url: URL) async {
        guard connection == .connected else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            let project = try await core.addProject(root: url.path, name: nil)
            projects = try await core.listProjects()
            selectedProjectID = project.id
            selectedSessionID = nil
            events = []
            mode = .project
            await loadSessions()
        } catch {
            handle(error)
        }
    }

    var defaultTokenBudget: Int {
        UserDefaults.standard.object(forKey: "verlinpark.session.tokenBudget") as? Int
            ?? 200_000
    }

    var defaultUnlimitedBudget: Bool {
        UserDefaults.standard.bool(forKey: "verlinpark.session.unlimitedBudget")
    }

    func createSession(
        goal: String,
        tokenBudget: Int? = nil,
        costBudgetUSD: Double? = nil,
        timeBudgetSecs: Int? = nil,
        priority: Int = 0,
        unlimited: Bool? = nil
    ) async -> Bool {
        guard let projectID = selectedProjectID else {
            lastError = "Choose a project first."
            return false
        }
        let trimmed = goal.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }
        let isUnlimited = unlimited ?? defaultUnlimitedBudget
        let budget: Int? = if isUnlimited {
            nil
        } else {
            (tokenBudget ?? defaultTokenBudget).clampedTokenBudget
        }
        if let budget {
            UserDefaults.standard.set(budget, forKey: "verlinpark.session.tokenBudget")
        }

        isBusy = true
        defer { isBusy = false }
        do {
            let session = try await core.createSession(
                projectID: projectID,
                goal: trimmed,
                tokenBudget: budget,
                costBudgetUSD: isUnlimited ? nil : costBudgetUSD,
                timeBudgetSecs: isUnlimited ? nil : timeBudgetSecs,
                priority: priority,
                unlimited: isUnlimited
            )
            let loaded = try await core.listSessions(projectID: projectID)
            guard selectedProjectID == projectID else { return false }
            sessions = loaded
            selectSession(session.id)
            showToast("Session created", detail: session.goal, kind: .success)
            return true
        } catch {
            handle(error)
            return false
        }
    }

    func sendMessage(_ text: String) async {
        guard let sessionID = selectedSessionID else {
            lastError = "Choose or create a session first."
            return
        }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        do {
            try await core.prompt(sessionID: sessionID, content: trimmed)
        } catch {
            handle(error)
        }
    }

    func setSessionPriority(_ priority: Int) async {
        guard let sessionID = selectedSessionID else { return }
        do {
            let result = try await core.setSessionPriority(
                sessionID: sessionID,
                priority: priority
            )
            if let index = sessions.firstIndex(where: { $0.id == sessionID }) {
                let old = sessions[index]
                sessions[index] = Session(
                    id: old.id,
                    projectId: old.projectId,
                    goal: old.goal,
                    status: old.status,
                    priority: result.priority,
                    worktree: old.worktree,
                    branch: old.branch,
                    tokenBudget: old.tokenBudget,
                    costBudgetUsd: old.costBudgetUsd,
                    timeBudgetSecs: old.timeBudgetSecs,
                    unlimited: old.unlimited,
                    costSpentUsd: old.costSpentUsd,
                    baseCommit: old.baseCommit,
                    tokensIn: old.tokensIn,
                    tokensOut: old.tokensOut,
                    createdAt: old.createdAt
                )
            }
            lastError = nil
        } catch {
            handle(error)
        }
    }

    func setSessionUnlimited(_ unlimited: Bool) async {
        guard let sessionID = selectedSessionID else { return }
        do {
            let result = try await core.setSessionUnlimited(
                sessionID: sessionID,
                unlimited: unlimited
            )
            if let index = sessions.firstIndex(where: { $0.id == sessionID }) {
                let old = sessions[index]
                sessions[index] = Session(
                    id: old.id,
                    projectId: old.projectId,
                    goal: old.goal,
                    status: old.status,
                    priority: old.priority,
                    worktree: old.worktree,
                    branch: old.branch,
                    tokenBudget: old.tokenBudget,
                    costBudgetUsd: old.costBudgetUsd,
                    timeBudgetSecs: old.timeBudgetSecs,
                    unlimited: result.unlimited,
                    costSpentUsd: old.costSpentUsd,
                    baseCommit: old.baseCommit,
                    tokensIn: old.tokensIn,
                    tokensOut: old.tokensOut,
                    createdAt: old.createdAt
                )
            }
            showToast(
                unlimited ? "Unlimited budget enabled" : "Stored budget limits restored",
                detail: unlimited
                    ? "Session, plan, department, and project budget gates are bypassed."
                    : "Budget gates use the session's stored limits again.",
                kind: unlimited ? .warning : .success
            )
        } catch {
            handle(error)
        }
    }

    func cancelSession() async {
        guard let sessionID = selectedSessionID else { return }
        do {
            _ = try await core.cancelSession(sessionID: sessionID)
        } catch {
            handle(error)
        }
    }

    func pauseSession() async {
        guard let sessionID = selectedSessionID else { return }
        do {
            _ = try await core.pauseSession(sessionID: sessionID)
        } catch {
            handle(error)
        }
    }

    func resumeSession() async {
        guard let sessionID = selectedSessionID else { return }
        do {
            _ = try await core.resumeSession(sessionID: sessionID)
        } catch {
            handle(error)
        }
    }

    func loadShellPolicy() async {
        guard let projectID = selectedProjectID else {
            shellPolicy = .allow
            shellPolicyNetwork = "allow"
            shellPolicyReadDenyText = ""
            shellPolicyStatus = nil
            return
        }
        shellPolicyBusy = true
        defer { shellPolicyBusy = false }
        do {
            let policy = try await core.shellPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            shellPolicy = policy
            shellPolicyNetwork = policy.network
            shellPolicyRestrictHome = policy.restrictHome
            shellPolicyReadJail = policy.readJail
            shellPolicyReadDenyText = policy.readDeny.joined(separator: "\n")
            shellPolicyStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveShellPolicy() async {
        guard let projectID = selectedProjectID else {
            shellPolicyStatus = "Select a project first."
            return
        }
        let readDeny = shellPolicyReadDenyText
            .split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        shellPolicyBusy = true
        defer { shellPolicyBusy = false }
        do {
            let policy = try await core.setShellPolicy(
                projectID: projectID,
                policy: ShellPolicy(
                    network: shellPolicyNetwork,
                    readDeny: readDeny,
                    restrictHome: shellPolicyRestrictHome,
                    readJail: shellPolicyReadJail
                )
            )
            guard projectID == selectedProjectID else { return }
            shellPolicy = policy
            shellPolicyNetwork = policy.network
            shellPolicyRestrictHome = policy.restrictHome
            shellPolicyReadJail = policy.readJail
            shellPolicyReadDenyText = policy.readDeny.joined(separator: "\n")
            shellPolicyStatus =
                "Saved · network \(policy.network) · \(policy.readDeny.count) read-deny paths · read jail \(policy.readJail ? "on" : "off")"
        } catch {
            shellPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetShellPolicy() async {
        shellPolicyNetwork = "allow"
        shellPolicyRestrictHome = false
        shellPolicyReadJail = false
        shellPolicyReadDenyText = ""
        await saveShellPolicy()
    }

    func loadIntegrationPolicy() async {
        guard let projectID = selectedProjectID else {
            integrationPolicy = .disabled
            integrationAutoResolve = false
            integrationMaxAutoAttempts = 1
            integrationMaxTokensText = ""
            integrationPolicyStatus = nil
            return
        }
        integrationPolicyBusy = true
        defer { integrationPolicyBusy = false }
        do {
            let policy = try await core.integrationPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            integrationPolicy = policy
            integrationAutoResolve = policy.autoResolve
            integrationMaxAutoAttempts = policy.maxAutoAttempts
            integrationMaxTokensText = policy.maxResolveTokens.map(String.init) ?? ""
            integrationPolicyStatus = nil
        } catch {
            handle(error)
        }
    }

    func loadRemoteOperations() async {
        guard let projectID = selectedProjectID else {
            remoteBootstrapStatus = nil
            remoteTestStatus = nil
            remoteOperationsMessage = nil
            return
        }
        remoteOperationsBusy = true
        defer { remoteOperationsBusy = false }
        do {
            async let bootstrap = core.remoteBootstrapStatus(projectID: projectID)
            async let tests = core.remoteTestStatus(
                projectID: projectID,
                sessionID: selectedSessionID,
                entryID: nil
            )
            let (bootstrapStatus, testStatus) = try await (bootstrap, tests)
            guard projectID == selectedProjectID else { return }
            remoteBootstrapStatus = bootstrapStatus
            remoteTestStatus = testStatus
            remoteOperationsMessage = nil
        } catch {
            guard projectID == selectedProjectID else { return }
            remoteBootstrapStatus = nil
            remoteTestStatus = nil
            remoteOperationsMessage = error.localizedDescription
        }
    }

    func loadRemoteIntegrationStatus() async {
        guard let projectID = selectedProjectID else {
            remoteIntegrationStatus = nil
            remoteIntegrationStatusMessage = nil
            remoteBootstrapStatus = nil
            remoteTestStatus = nil
            remoteOperationsMessage = nil
            return
        }
        remoteIntegrationBusy = true
        defer { remoteIntegrationBusy = false }
        do {
            let status = try await core.remoteIntegrationStatus(
                projectID: projectID,
                sessionID: selectedSessionID
            )
            guard projectID == selectedProjectID else { return }
            remoteIntegrationStatus = status
            remoteIntegrationStatusMessage = nil
        } catch {
            guard projectID == selectedProjectID else { return }
            remoteIntegrationStatus = nil
            remoteIntegrationStatusMessage = error.localizedDescription
        }
    }

    func cleanupRemoteIntegrationRefs(force: Bool = false) async {
        guard let projectID = selectedProjectID else {
            remoteIntegrationStatusMessage = "Select a project first."
            return
        }
        remoteIntegrationBusy = true
        defer { remoteIntegrationBusy = false }
        do {
            let result = try await core.cleanupRemoteIntegration(
                projectID: projectID,
                sessionID: selectedSessionID,
                entryID: nil,
                force: force
            )
            remoteIntegrationStatusMessage =
                "Removed \(result.removedRefs.count) ref(s); \(result.skipped.count) skipped."
            await loadRemoteIntegrationStatus()
        } catch {
            remoteIntegrationStatusMessage = error.localizedDescription
        }
    }

    func saveIntegrationPolicy() async {
        guard let projectID = selectedProjectID else {
            integrationPolicyStatus = "Select a project first."
            return
        }
        let rawTokens = integrationMaxTokensText.trimmingCharacters(in: .whitespaces)
        let tokens: Int?
        if rawTokens.isEmpty {
            tokens = nil
        } else if let value = Int(rawTokens), value > 0 {
            tokens = value
        } else {
            integrationPolicyStatus = "Max tokens must be empty or a positive integer."
            return
        }
        integrationPolicyBusy = true
        defer { integrationPolicyBusy = false }
        do {
            let policy = try await core.setIntegrationPolicy(
                projectID: projectID,
                policy: IntegrationPolicy(
                    autoResolve: integrationAutoResolve,
                    maxAutoAttempts: integrationMaxAutoAttempts,
                    maxResolveTokens: tokens
                )
            )
            guard projectID == selectedProjectID else { return }
            integrationPolicy = policy
            integrationAutoResolve = policy.autoResolve
            integrationMaxAutoAttempts = policy.maxAutoAttempts
            integrationMaxTokensText = policy.maxResolveTokens.map(String.init) ?? ""
            integrationPolicyStatus = policy.autoResolve
                ? "Auto-resolve enabled · attempts \(policy.maxAutoAttempts)"
                : "Auto-resolve disabled"
        } catch {
            integrationPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadPendingApprovals() async {
        approvalCenterBusy = true
        defer { approvalCenterBusy = false }
        do {
            let queue = try await core.approvalQueue(sessionID: nil, projectID: nil)
            let allSessions = try await core.listSessions(projectID: nil)
            pendingApprovals = queue.approvals
            approvalSelection = approvalSelection.intersection(Set(queue.approvals.map(\.id)))
            approvalSessionGoals = Dictionary(
                uniqueKeysWithValues: allSessions.map { ($0.id, $0.goal) }
            )
            approvalProjectIDs = Dictionary(
                uniqueKeysWithValues: allSessions.map { ($0.id, $0.projectId) }
            )
            approvalCenterStatus = pendingApprovals.isEmpty ? "No pending approvals." : nil
        } catch {
            approvalCenterStatus = error.localizedDescription
        }
    }

    func resolveGlobalApproval(_ id: String, approved: Bool) async {
        approvalCenterBusy = true
        defer { approvalCenterBusy = false }
        do {
            let resolved = try await core.resolveApproval(id: id, approved: approved)
            if resolved {
                pendingApprovals.removeAll { $0.id == id }
                approvalSelection.remove(id)
            }
            approvalCenterStatus = pendingApprovals.isEmpty ? "No pending approvals." : nil
        } catch {
            approvalCenterStatus = error.localizedDescription
            handle(error)
        }
    }

    func toggleApprovalSelection(_ id: String) {
        if approvalSelection.contains(id) {
            approvalSelection.remove(id)
        } else {
            approvalSelection.insert(id)
        }
    }

    func selectAllPendingApprovals() {
        approvalSelection = Set(pendingApprovals.map(\.id))
    }

    func clearApprovalSelection() {
        approvalSelection.removeAll()
    }

    func resolveSelectedApprovals(approved: Bool) async {
        let ids = pendingApprovals
            .map(\.id)
            .filter { approvalSelection.contains($0) }
        guard !ids.isEmpty else {
            approvalBatchStatus = "Select at least one approval."
            return
        }
        approvalBatchBusy = true
        defer { approvalBatchBusy = false }
        do {
            let result = try await core.resolveApprovals(ids.map { ($0, approved) })
            let resolved = Set(
                result.results
                    .filter(\.resolved)
                    .map(\.approvalId)
            )
            pendingApprovals.removeAll { resolved.contains($0.id) }
            approvalSelection.subtract(resolved)
            let failures = result.results.filter { !$0.resolved }
            if failures.isEmpty {
                approvalBatchStatus = "\(result.resolvedCount) resolved."
            } else {
                let labels = failures.prefix(3).map { item in
                    "\(item.approvalId.prefix(8)): \(item.message ?? item.code ?? "failed")"
                }
                approvalBatchStatus =
                    "\(result.resolvedCount) resolved, \(result.failedCount) failed — "
                        + labels.joined(separator: " · ")
            }
            if pendingApprovals.isEmpty {
                approvalCenterStatus = "No pending approvals."
            }
        } catch {
            approvalBatchStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadApprovalTemplates(projectID: String) async {
        approvalTemplateBusy = true
        defer { approvalTemplateBusy = false }
        do {
            approvalTemplates = try await core.listApprovalTemplates(projectID: projectID)
            approvalTemplateStatus = approvalTemplates.isEmpty ? "No templates." : nil
        } catch {
            approvalTemplateStatus = error.localizedDescription
        }
    }

    func createApprovalTemplateFromApproval(
        _ approval: Approval,
        name: String,
        maxUses: Int,
        expiresAt: String?
    ) async {
        approvalTemplateBusy = true
        defer { approvalTemplateBusy = false }
        do {
            let template = try await core.createApprovalTemplateFromApproval(
                approvalID: approval.id,
                name: name.isEmpty ? nil : name,
                maxUses: maxUses,
                expiresAt: expiresAt
            )
            approvalTemplates.insert(template, at: 0)
            approvalTemplateStatus = "Created \(template.name)."
            if let projectID = approvalProjectIDs[approval.sessionId] {
                await loadApprovalTemplates(projectID: projectID)
            }
        } catch {
            approvalTemplateStatus = error.localizedDescription
            handle(error)
        }
    }

    func deleteApprovalTemplate(_ template: ApprovalTemplate) async {
        approvalTemplateBusy = true
        defer { approvalTemplateBusy = false }
        do {
            let deleted = try await core.deleteApprovalTemplate(id: template.id)
            if deleted {
                approvalTemplates.removeAll { $0.id == template.id }
            }
            approvalTemplateStatus = deleted ? "Deleted \(template.name)." : "Template was already gone."
        } catch {
            approvalTemplateStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadAttentionPolicy(projectID: String) async {
        attentionPolicyBusy = true
        defer { attentionPolicyBusy = false }
        do {
            attentionPolicy = try await core.attentionPolicy(projectID: projectID).policy
            attentionPolicyStatus = nil
        } catch {
            attentionPolicyStatus = error.localizedDescription
        }
    }

    func saveAttentionPolicy(_ policy: AttentionPolicy, projectID: String) async {
        attentionPolicyBusy = true
        defer { attentionPolicyBusy = false }
        do {
            attentionPolicy = try await core.setAttentionPolicy(
                projectID: projectID,
                policy: policy
            ).policy
            attentionPolicyStatus = "Attention policy saved."
            await loadPendingApprovals()
        } catch {
            attentionPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadAcpAgents() async {
        acpBusy = true
        defer { acpBusy = false }
        do {
            acpAgents = try await core.listAcpAgents()
            acpStatus = nil
        } catch {
            acpStatus = error.localizedDescription
        }
    }

    func addAcpAgent(
        name: String,
        command: String,
        args: [String],
        env: [String: String],
        autoApprove: Bool,
        timeoutSecs: Int,
        transport: AcpTransport = .stdio,
        endpoint: String? = nil,
        authHeaderEnv: String? = nil,
        headers: [String: String] = [:],
        connectTimeoutSecs: Int = 30,
        allowRemote: Bool = false
    ) async -> Bool {
        acpBusy = true
        defer { acpBusy = false }
        do {
            _ = try await core.addAcpAgent(
                name: name,
                command: command,
                args: args,
                env: env,
                autoApprove: autoApprove,
                timeoutSecs: timeoutSecs,
                transport: transport,
                endpoint: endpoint,
                authHeaderEnv: authHeaderEnv,
                headers: headers,
                connectTimeoutSecs: connectTimeoutSecs,
                allowRemote: allowRemote
            )
            acpStatus = "ACP agent added."
            await loadAcpAgents()
            showToast("ACP agent added", detail: name, kind: .success)
            return true
        } catch {
            acpStatus = error.localizedDescription
            handle(error)
            return false
        }
    }

    func updateAcpAgentSettings(
        id: String,
        name: String,
        command: String,
        args: [String],
        env: [String: String],
        transport: AcpTransport,
        endpoint: String?,
        authHeaderEnv: String?,
        headers: [String: String],
        connectTimeoutSecs: Int,
        allowRemote: Bool,
        autoApprove: Bool,
        timeoutSecs: Int,
        enabled: Bool
    ) async -> Bool {
        acpBusy = true
        defer { acpBusy = false }
        do {
            _ = try await core.updateAcpAgentSettings(
                id: id,
                name: name,
                command: command,
                args: args,
                env: env,
                transport: transport,
                endpoint: endpoint,
                authHeaderEnv: authHeaderEnv,
                headers: headers,
                connectTimeoutSecs: connectTimeoutSecs,
                allowRemote: allowRemote,
                autoApprove: autoApprove,
                timeoutSecs: timeoutSecs,
                enabled: enabled
            )
            acpStatus = "ACP agent updated."
            await loadAcpAgents()
            showToast("ACP agent updated", detail: name, kind: .success)
            return true
        } catch {
            acpStatus = error.localizedDescription
            handle(error)
            return false
        }
    }

    func setAcpAgentEnabled(_ agent: AcpAgent, enabled: Bool) async {
        acpBusy = true
        defer { acpBusy = false }
        do {
            _ = try await core.updateAcpAgent(
                agent,
                enabled: enabled,
                autoApprove: agent.autoApprove
            )
            await loadAcpAgents()
        } catch {
            acpStatus = error.localizedDescription
            handle(error)
        }
    }

    func setAcpAgentAutoApprove(_ agent: AcpAgent, autoApprove: Bool) async {
        acpBusy = true
        defer { acpBusy = false }
        do {
            _ = try await core.updateAcpAgent(
                agent,
                enabled: agent.enabled,
                autoApprove: autoApprove
            )
            await loadAcpAgents()
        } catch {
            acpStatus = error.localizedDescription
            handle(error)
        }
    }

    func removeAcpAgent(id: String) async {
        acpBusy = true
        defer { acpBusy = false }
        do {
            _ = try await core.removeAcpAgent(id: id)
            acpProbeStatus[id] = nil
            await loadAcpAgents()
        } catch {
            acpStatus = error.localizedDescription
            handle(error)
        }
    }

    func probeAcpAgent(id: String) async {
        acpProbeStatus[id] = "probing…"
        do {
            let result = try await core.probeAcpAgent(id: id, projectID: selectedProjectID)
            let load = result.loadSession ? " · session/load" : ""
            let endpoint = result.remote ? " · \(result.endpoint ?? "remote")" : ""
            let http = result.httpVersion.map { " · \($0)" } ?? ""
            acpProbeStatus[id] =
                "ok · \(result.transport.displayName)\(http) · \(result.agentName) \(result.version) · protocol \(result.protocolVersion)\(load)\(endpoint)"
        } catch {
            acpProbeStatus[id] = error.localizedDescription
        }
    }

    func probeMcpServer(id: String) async {
        mcpProbeBusy.insert(id)
        mcpProbeStatus[id] = "probing…"
        defer { mcpProbeBusy.remove(id) }
        do {
            let result = try await core.probeMcpServer(serverID: id)
            let server = [result.serverName, result.serverVersion]
                .filter { !$0.isEmpty }
                .joined(separator: " ")
            mcpProbeStatus[id] =
                "ok · \(server.isEmpty ? "server" : server) · \(result.tools) tools · \(result.resources) resources · \(result.prompts) prompts · \(result.protocolVersion)"
        } catch {
            mcpProbeStatus[id] = error.localizedDescription
        }
    }

    func loadMcpServers() async {
        mcpBusy = true
        defer { mcpBusy = false }
        do {
            mcpServers = try await core.listMcpServers()
            mcpStatus = nil
        } catch {
            mcpStatus = error.localizedDescription
        }
    }

    @discardableResult
    func addMcpServer(
        name: String,
        transport: String,
        command: String,
        args: [String],
        env: [String: String],
        url: String?,
        headers: [String: String]
    ) async -> Bool {
        mcpBusy = true
        defer { mcpBusy = false }
        do {
            _ = try await core.addMcpServer(
                name: name,
                transport: transport,
                command: command,
                args: args,
                env: env,
                url: url,
                headers: headers
            )
            mcpStatus = "MCP server added."
            await loadMcpServers()
            showToast("MCP server added", detail: name, kind: .success)
            return true
        } catch {
            mcpStatus = error.localizedDescription
            handle(error)
            return false
        }
    }

    func setMcpServerEnabled(_ server: McpServer, enabled: Bool) async {
        let updated = await updateMcpServer(
            id: server.id,
            name: server.name,
            transport: server.transportKind,
            command: server.command,
            args: server.args,
            env: server.env,
            url: server.url,
            headers: server.headers ?? [:],
            enabled: enabled
        )
        if !updated {
            await loadMcpServers()
        }
    }

    @discardableResult
    func updateMcpServer(
        id: String,
        name: String,
        transport: String,
        command: String,
        args: [String],
        env: [String: String],
        url: String?,
        headers: [String: String],
        enabled: Bool
    ) async -> Bool {
        mcpBusy = true
        defer { mcpBusy = false }
        do {
            _ = try await core.updateMcpServer(
                id: id,
                name: name,
                transport: transport,
                command: command,
                args: args,
                env: env,
                url: url,
                headers: headers,
                enabled: enabled
            )
            mcpStatus = "MCP server updated."
            showToast("MCP server updated", detail: name, kind: .success)
            mcpProbeStatus[id] = nil
            mcpTools[id] = nil
            mcpResources[id] = nil
            mcpPrompts[id] = nil
            mcpToolStatus[id] = nil
            await loadMcpServers()
            return true
        } catch {
            mcpStatus = error.localizedDescription
            handle(error)
            return false
        }
    }

    func removeMcpServer(id: String) async {
        mcpBusy = true
        defer { mcpBusy = false }
        do {
            _ = try await core.removeMcpServer(id: id)
            mcpProbeStatus[id] = nil
            mcpTools[id] = nil
            mcpResources[id] = nil
            mcpPrompts[id] = nil
            mcpToolStatus[id] = nil
            await loadMcpServers()
        } catch {
            mcpStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadMcpTools(serverID: String) async {
        mcpToolStatus[serverID] = "loading…"
        do {
            let tools = try await core.listMcpTools(serverID: serverID)
            mcpTools[serverID] = tools
            mcpToolStatus[serverID] = "\(tools.count) tools"
        } catch {
            mcpToolStatus[serverID] = error.localizedDescription
        }
    }

    func loadMcpResources(serverID: String) async {
        mcpToolStatus[serverID] = "loading resources…"
        do {
            let result = try await core.listMcpResources(serverID: serverID)
            mcpResources[serverID] = result
            mcpToolStatus[serverID] =
                "\(result.resources.count) resources · \(result.templates.count) templates"
        } catch {
            mcpToolStatus[serverID] = error.localizedDescription
        }
    }

    func loadMcpPrompts(serverID: String) async {
        mcpToolStatus[serverID] = "loading prompts…"
        do {
            let result = try await core.listMcpPrompts(serverID: serverID)
            mcpPrompts[serverID] = result
            mcpToolStatus[serverID] = "\(result.prompts.count) prompts"
        } catch {
            mcpToolStatus[serverID] = error.localizedDescription
        }
    }

    func loadQuestions() async {
        guard let sessionID = selectedSessionID else {
            questions = []
            return
        }
        let generation = selectionGeneration
        do {
            let loaded = try await core.listQuestions(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                questions = loaded
                pendingQuestions = loaded.filter(\.isPending)
            }
        } catch {
            // The event stream will retry on question.requested.
        }
    }

    func loadPendingQuestions() async {
        questionCenterBusy = true
        defer { questionCenterBusy = false }
        do {
            pendingQuestions = try await core.listAllQuestions().filter(\.isPending)
            questionCenterStatus = pendingQuestions.isEmpty ? "No pending questions." : nil
        } catch {
            questionCenterStatus = error.localizedDescription
        }
    }

    func answerGlobalQuestion(_ questionID: String, answer: String) async {
        let answer = answer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !answer.isEmpty else { return }
        questionCenterBusy = true
        defer { questionCenterBusy = false }
        do {
            _ = try await core.answerQuestion(id: questionID, answer: answer)
            await loadPendingQuestions()
            if selectedSessionID == questions.first(where: { $0.id == questionID })?.sessionId {
                await loadQuestions()
            }
        } catch {
            questionCenterStatus = error.localizedDescription
            handle(error)
        }
    }

    func answerQuestion(_ questionID: String, answer: String) async {
        let answer = answer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !answer.isEmpty else { return }
        do {
            _ = try await core.answerQuestion(id: questionID, answer: answer)
            await loadQuestions()
        } catch {
            handle(error)
        }
    }

    func loadMemoryPolicy() async {
        guard let projectID = selectedProjectID else {
            memoryPolicyEnabled = false
            memoryPolicyMaxItems = "1000"
            memoryPolicyMaxAgeDays = "180"
            memoryPolicyTaskResultVerifyDays = "90"
            memoryPolicyDefaultVerifyDays = "365"
            memoryPolicyAutoSupersede = true
            memoryPolicyBriefMaxChars = "4000"
            memoryPolicySweepStale = false
            memoryPolicyStatus = nil
            memoryEmbeddingEnabled = false
            memoryEmbeddingEndpoint = ""
            memoryEmbeddingModel = ""
            memoryEmbeddingDimensions = "1536"
            memoryEmbeddingBatchSize = "16"
            memoryEmbeddingTimeoutSecs = "30"
            memoryEmbeddingMaxCandidates = "64"
            memoryEmbeddingMinSimilarity = "0.20"
            memoryEmbeddingAPIKeyEnv = ""
            memoryEmbeddingIndex = .scan
            memoryEmbeddingIVFNlist = "64"
            memoryEmbeddingIVFNprobe = "8"
            memoryEmbeddingIVFMinVectors = "256"
            memoryEmbeddingIVFRebuildVectors = "4096"
            memoryEmbeddingsStatus = nil
            memoryEmbeddingIndexStatus = nil
            memoryEmbeddingStatusMessage = nil
            memoryEmbeddingTestResult = nil
            memoryEmbeddingRebuildResult = nil
            memoryEmbeddingIndexRebuildResult = nil
            return
        }
        memoryPolicyBusy = true
        defer { memoryPolicyBusy = false }
        do {
            let policy = try await core.memoryPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            applyMemoryPolicy(policy)
            memoryPolicyStatus = nil
        } catch {
            memoryPolicyStatus = error.localizedDescription
        }
    }

    func saveMemoryPolicy() async {
        guard let projectID = selectedProjectID else {
            memoryPolicyStatus = "Select a project first."
            return
        }
        let itemsText = memoryPolicyMaxItems.trimmingCharacters(in: .whitespacesAndNewlines)
        let daysText = memoryPolicyMaxAgeDays.trimmingCharacters(in: .whitespacesAndNewlines)
        let taskResultText = memoryPolicyTaskResultVerifyDays
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let defaultVerifyText = memoryPolicyDefaultVerifyDays
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let briefText = memoryPolicyBriefMaxChars.trimmingCharacters(in: .whitespacesAndNewlines)
        let maxItems = itemsText.isEmpty ? nil : Int(itemsText)
        let maxAgeDays = daysText.isEmpty ? nil : Int(daysText)
        let taskResultVerifyDays = taskResultText.isEmpty ? nil : Int(taskResultText)
        let defaultVerifyDays = defaultVerifyText.isEmpty ? nil : Int(defaultVerifyText)
        let briefMaxChars = briefText.isEmpty ? nil : Int(briefText)
        if !itemsText.isEmpty, maxItems == nil {
            memoryPolicyStatus = "Max items must be a whole number."
            return
        }
        if !daysText.isEmpty, maxAgeDays == nil {
            memoryPolicyStatus = "Max age days must be a whole number."
            return
        }
        if !taskResultText.isEmpty, taskResultVerifyDays == nil {
            memoryPolicyStatus = "Task-result verification days must be a whole number."
            return
        }
        if !defaultVerifyText.isEmpty, defaultVerifyDays == nil {
            memoryPolicyStatus = "Default verification days must be a whole number."
            return
        }
        if !briefText.isEmpty, briefMaxChars == nil {
            memoryPolicyStatus = "Brief character budget must be a whole number."
            return
        }
        if memoryPolicyEnabled, maxItems == nil, maxAgeDays == nil {
            memoryPolicyStatus = "Enable requires a max item count or max age."
            return
        }
        let endpoint = memoryEmbeddingEndpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        let model = memoryEmbeddingModel.trimmingCharacters(in: .whitespacesAndNewlines)
        let apiKeyEnv = memoryEmbeddingAPIKeyEnv
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let dimensions = Int(
            memoryEmbeddingDimensions.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let batchSize = Int(
            memoryEmbeddingBatchSize.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let timeoutSecs = Int(
            memoryEmbeddingTimeoutSecs.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let maxCandidates = Int(
            memoryEmbeddingMaxCandidates.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let minSimilarity = Double(
            memoryEmbeddingMinSimilarity.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let ivfNlist = Int(
            memoryEmbeddingIVFNlist.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let ivfNprobe = Int(
            memoryEmbeddingIVFNprobe.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let ivfMinVectors = Int(
            memoryEmbeddingIVFMinVectors.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let ivfRebuildVectors = Int(
            memoryEmbeddingIVFRebuildVectors.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        if memoryEmbeddingEnabled || !endpoint.isEmpty || !model.isEmpty {
            guard !endpoint.isEmpty, !model.isEmpty else {
                memoryPolicyStatus = "Embedding endpoint and model are both required."
                return
            }
            guard let dimensions, (8 ... 8192).contains(dimensions) else {
                memoryPolicyStatus = "Embedding dimensions must be between 8 and 8192."
                return
            }
            guard let batchSize, (1 ... 64).contains(batchSize) else {
                memoryPolicyStatus = "Embedding batch size must be between 1 and 64."
                return
            }
            guard let timeoutSecs, (1 ... 300).contains(timeoutSecs) else {
                memoryPolicyStatus = "Embedding timeout must be between 1 and 300 seconds."
                return
            }
            guard let maxCandidates, (1 ... 256).contains(maxCandidates) else {
                memoryPolicyStatus = "Embedding candidates must be between 1 and 256."
                return
            }
            guard let minSimilarity, minSimilarity.isFinite, (0 ... 1).contains(minSimilarity) else {
                memoryPolicyStatus = "Minimum similarity must be between 0 and 1."
                return
            }
            guard let ivfNlist, (4 ... 1024).contains(ivfNlist) else {
                memoryPolicyStatus = "IVF nlist must be between 4 and 1024."
                return
            }
            guard let ivfNprobe, (1 ... ivfNlist).contains(ivfNprobe) else {
                memoryPolicyStatus = "IVF nprobe must be between 1 and nlist."
                return
            }
            guard let ivfMinVectors, (1 ... 10_000_000).contains(ivfMinVectors) else {
                memoryPolicyStatus = "IVF minimum vectors must be between 1 and 10000000."
                return
            }
            guard let ivfRebuildVectors,
                  (1 ... 10_000_000).contains(ivfRebuildVectors),
                  ivfRebuildVectors >= ivfMinVectors
            else {
                memoryPolicyStatus = "IVF rebuild threshold must be at least the minimum and at most 10000000."
                return
            }
        }
        let embedding: MemoryEmbeddingPolicy? = if endpoint.isEmpty, model.isEmpty {
            nil
        } else {
            MemoryEmbeddingPolicy(
                enabled: memoryEmbeddingEnabled,
                endpoint: endpoint,
                model: model,
                dimensions: dimensions ?? 1536,
                batchSize: batchSize ?? 16,
                timeoutSecs: timeoutSecs ?? 30,
                maxCandidates: maxCandidates ?? 64,
                minSimilarity: minSimilarity ?? 0.20,
                apiKeyEnv: apiKeyEnv.isEmpty ? nil : apiKeyEnv,
                index: memoryEmbeddingIndex,
                ivfNlist: ivfNlist ?? 64,
                ivfNprobe: ivfNprobe ?? 8,
                ivfMinVectors: ivfMinVectors ?? 256,
                ivfRebuildVectors: ivfRebuildVectors ?? 4096
            )
        }
        memoryPolicyBusy = true
        defer { memoryPolicyBusy = false }
        do {
            let policy = try await core.setMemoryPolicy(
                projectID: projectID,
                policy: MemoryPolicy(
                    enabled: memoryPolicyEnabled,
                    maxItems: maxItems,
                    maxAgeDays: maxAgeDays,
                    taskResultVerifyDays: taskResultVerifyDays,
                    defaultVerifyDays: defaultVerifyDays,
                    autoSupersede: memoryPolicyAutoSupersede,
                    briefMaxChars: briefMaxChars,
                    sweepStale: memoryPolicySweepStale,
                    embedding: embedding
                )
            )
            applyMemoryPolicy(policy)
            memoryPolicyStatus = "Memory policy saved."
            await loadMemoryEmbeddingStatus()
        } catch {
            memoryPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    private func applyMemoryPolicy(_ policy: MemoryPolicy) {
        memoryPolicyEnabled = policy.enabled
        memoryPolicyMaxItems = policy.maxItems.map(String.init) ?? ""
        memoryPolicyMaxAgeDays = policy.maxAgeDays.map(String.init) ?? ""
        memoryPolicyTaskResultVerifyDays = policy.taskResultVerifyDays.map(String.init) ?? "90"
        memoryPolicyDefaultVerifyDays = policy.defaultVerifyDays.map(String.init) ?? "365"
        memoryPolicyAutoSupersede = policy.autoSupersede ?? true
        memoryPolicyBriefMaxChars = policy.briefMaxChars.map(String.init) ?? "4000"
        memoryPolicySweepStale = policy.sweepStale ?? false
        let embedding = policy.embedding ?? MemoryEmbeddingPolicy.defaults
        memoryEmbeddingEnabled = embedding.enabled
        memoryEmbeddingEndpoint = embedding.endpoint ?? ""
        memoryEmbeddingModel = embedding.model ?? ""
        memoryEmbeddingDimensions = String(embedding.dimensions)
        memoryEmbeddingBatchSize = String(embedding.batchSize)
        memoryEmbeddingTimeoutSecs = String(embedding.timeoutSecs)
        memoryEmbeddingMaxCandidates = String(embedding.maxCandidates)
        memoryEmbeddingMinSimilarity = String(format: "%.2f", embedding.minSimilarity)
        memoryEmbeddingAPIKeyEnv = embedding.apiKeyEnv ?? ""
        memoryEmbeddingIndex = embedding.index
        memoryEmbeddingIVFNlist = String(embedding.ivfNlist)
        memoryEmbeddingIVFNprobe = String(embedding.ivfNprobe)
        memoryEmbeddingIVFMinVectors = String(embedding.ivfMinVectors)
        memoryEmbeddingIVFRebuildVectors = String(embedding.ivfRebuildVectors)
    }

    func loadMemoryEmbeddingStatus() async {
        guard let projectID = selectedProjectID else {
            memoryEmbeddingsStatus = nil
            return
        }
        do {
            let status = try await core.memoryEmbeddingsStatus(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            memoryEmbeddingsStatus = status
            memoryEmbeddingIndexStatus = status.index
            memoryEmbeddingStatusMessage = nil
            let index = try await core.memoryEmbeddingIndexStatus(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            memoryEmbeddingIndexStatus = index.index
        } catch {
            guard projectID == selectedProjectID else { return }
            memoryEmbeddingStatusMessage = error.localizedDescription
        }
    }

    func testMemoryEmbeddings() async {
        guard let projectID = selectedProjectID else {
            memoryEmbeddingStatusMessage = "Select a project first."
            return
        }
        guard !memoryEmbeddingBusy else { return }
        memoryEmbeddingBusy = true
        defer { memoryEmbeddingBusy = false }
        do {
            let result = try await core.testMemoryEmbeddings(projectID: projectID, text: nil)
            guard projectID == selectedProjectID else { return }
            memoryEmbeddingTestResult = result
            memoryEmbeddingStatusMessage = result.ok
                ? "Embedding endpoint responded in \(result.latencyMs) ms."
                : (result.error ?? "Embedding endpoint probe failed.")
            await loadMemoryEmbeddingStatus()
        } catch {
            memoryEmbeddingStatusMessage = error.localizedDescription
            handle(error)
        }
    }

    func rebuildMemoryEmbeddings() async {
        guard let projectID = selectedProjectID else {
            memoryEmbeddingStatusMessage = "Select a project first."
            return
        }
        guard !memoryEmbeddingBusy else { return }
        memoryEmbeddingBusy = true
        defer { memoryEmbeddingBusy = false }
        do {
            let result = try await core.rebuildMemoryEmbeddings(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            memoryEmbeddingRebuildResult = result
            memoryEmbeddingStatusMessage =
                "Queued \(result.enqueued) memories and cleared \(result.cleared) derived rows."
            await loadMemoryEmbeddingStatus()
        } catch {
            memoryEmbeddingStatusMessage = error.localizedDescription
            handle(error)
        }
    }

    /// Explicit derived ANN rebuild. No embedding request is made; the daemon
    /// rebuilds centroids from stored vectors and re-assigns them. The action
    /// is gated on an enabled, configured IVF policy.
    func rebuildMemoryEmbeddingIndex() async {
        guard let projectID = selectedProjectID else {
            memoryEmbeddingStatusMessage = "Select a project first."
            return
        }
        guard canRebuildMemoryEmbeddingIndex else { return }
        memoryEmbeddingBusy = true
        defer { memoryEmbeddingBusy = false }
        do {
            let result = try await core.rebuildMemoryEmbeddingIndex(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            memoryEmbeddingIndexRebuildResult = result
            memoryEmbeddingStatusMessage = result.summaryLabel
            await loadMemoryEmbeddingStatus()
        } catch {
            memoryEmbeddingStatusMessage = error.localizedDescription
            handle(error)
        }
    }

    func loadGitHistory() async {
        guard let sessionID = selectedSessionID else {
            gitCommits = []
            gitSelectedCommit = nil
            gitShow = nil
            gitHistorySessionID = nil
            return
        }
        let generation = selectionGeneration
        let sessionChanged = gitHistorySessionID != sessionID
        gitHistorySessionID = sessionID
        if sessionChanged {
            gitSelectedCommit = nil
            gitShow = nil
        }
        gitLogBusy = true
        defer { gitLogBusy = false }
        do {
            let result = try await core.gitLog(sessionID: sessionID)
            guard sessionID == selectedSessionID,
                  generation == selectionGeneration
            else { return }
            gitCommits = result.commits
            gitBranch = result.branch
            gitBaseCommit = result.baseCommit
            gitLogError = nil
            if let selected = gitSelectedCommit,
               !result.commits.contains(where: { $0.hash == selected })
            {
                gitSelectedCommit = nil
                gitShow = nil
            }
            if let selected = gitSelectedCommit {
                await loadGitCommit(selected)
            } else if let first = result.commits.first {
                await loadGitCommit(first.hash)
            }
        } catch {
            gitLogError = error.localizedDescription
        }
    }

    func loadGitCommit(_ hash: String) async {
        guard let sessionID = selectedSessionID else { return }
        gitSelectedCommit = hash
        gitShowBusy = true
        gitShowError = nil
        defer { gitShowBusy = false }
        do {
            let show = try await core.gitShow(sessionID: sessionID, commit: hash)
            guard sessionID == selectedSessionID, gitSelectedCommit == hash else { return }
            gitShow = show
        } catch {
            guard sessionID == selectedSessionID, gitSelectedCommit == hash else { return }
            gitShow = nil
            gitShowError = error.localizedDescription
        }
    }

    func loadWorkspaceTree() async {
        guard let sessionID = selectedSessionID else {
            workspaceEntries = []
            resetWorkspaceBrowser()
            workspaceSelectedPath = nil
            workspaceFile = nil
            workspaceDraft = ""
            workspaceDraftPath = nil
            workspaceSessionID = nil
            return
        }
        let sessionChanged = workspaceSessionID != sessionID
        workspaceSessionID = sessionID
        if sessionChanged {
            workspaceFilter = ""
            workspaceSelectedPath = nil
            workspaceFile = nil
            workspaceDraft = ""
            workspaceDraftPath = nil
        }
        workspaceTreeBusy = true
        defer { workspaceTreeBusy = false }
        do {
            let result = try await core.workspaceTree(sessionID: sessionID)
            guard sessionID == selectedSessionID else { return }
            workspaceEntries = result.entries
            workspaceTreeError = result.truncated ? "File list truncated to 5000 entries." : nil
            if let selected = workspaceSelectedPath,
               !result.entries.contains(where: { $0.path == selected && !$0.isDirectory })
            {
                workspaceSelectedPath = nil
                workspaceFile = nil
                workspaceDraft = ""
                workspaceDraftPath = nil
            }
        } catch {
            workspaceTreeError = error.localizedDescription
        }
    }

    func resetWorkspaceBrowser() {
        workspaceDirectories = [:]
        workspaceSearchResults = []
        workspaceSearchBusy = false
        workspaceSearchError = nil
        workspaceSearchTruncated = false
        workspaceSearchQuery = ""
        workspaceSearchMode = .names
        workspaceContentMatches = []
        workspaceContentSearchBusy = false
        workspaceContentSearchError = nil
        workspaceContentSearchTruncated = false
        workspaceContentFilesScanned = 0
        workspaceContentSearchQuery = ""
    }

    func loadWorkspaceRoot() async {
        if workspaceDirectories[""] == nil {
            await loadWorkspaceDirectory("", reset: true)
        }
    }

    func loadWorkspaceDirectory(_ path: String, reset: Bool = false) async {
        guard let sessionID = selectedSessionID else { return }
        let generation = selectionGeneration
        if !reset, workspaceDirectories[path]?.loading == true {
            return
        }
        var state = workspaceDirectories[path] ?? WorkspaceDirectoryState()
        state.loading = true
        state.error = nil
        workspaceDirectories[path] = state
        do {
            let result = try await core.workspaceList(
                sessionID: sessionID,
                path: path,
                offset: 0,
                limit: 500
            )
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            workspaceDirectories[path] = WorkspaceDirectoryState(
                entries: result.entries,
                total: result.total,
                hasMore: result.hasMore,
                truncated: result.truncated
            )
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            var failed = workspaceDirectories[path] ?? WorkspaceDirectoryState()
            failed.loading = false
            failed.error = error.localizedDescription
            workspaceDirectories[path] = failed
        }
    }

    func loadMoreWorkspaceDirectory(_ path: String) async {
        guard let sessionID = selectedSessionID else { return }
        var state = workspaceDirectories[path] ?? WorkspaceDirectoryState()
        guard state.hasMore, !state.loading, !state.loadingMore else { return }
        state.loadingMore = true
        workspaceDirectories[path] = state
        let generation = selectionGeneration
        do {
            let result = try await core.workspaceList(
                sessionID: sessionID,
                path: path,
                offset: state.entries.count,
                limit: 500
            )
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            var updated = workspaceDirectories[path] ?? state
            updated.entries.append(contentsOf: result.entries)
            updated.total = result.total
            updated.hasMore = result.hasMore
            updated.truncated = result.truncated
            updated.loadingMore = false
            workspaceDirectories[path] = updated
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            var failed = workspaceDirectories[path] ?? state
            failed.loadingMore = false
            failed.error = error.localizedDescription
            workspaceDirectories[path] = failed
        }
    }

    func refreshWorkspaceBrowser() async {
        guard let sessionID = selectedSessionID else {
            resetWorkspaceBrowser()
            return
        }
        let generation = selectionGeneration
        workspaceDirectories = [:]
        workspaceSearchResults = []
        workspaceSearchError = nil
        workspaceSearchTruncated = false
        workspaceSearchQuery = ""
        workspaceContentMatches = []
        workspaceContentSearchError = nil
        workspaceContentSearchTruncated = false
        workspaceContentFilesScanned = 0
        workspaceContentSearchQuery = ""
        await loadWorkspaceDirectory("", reset: true)
        guard isCurrent(sessionID: sessionID, generation: generation) else { return }
    }

    func searchWorkspace(_ rawQuery: String) async {
        guard let sessionID = selectedSessionID else { return }
        let query = rawQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        workspaceSearchQuery = query
        workspaceSearchError = nil
        if query.isEmpty {
            workspaceSearchResults = []
            workspaceSearchTruncated = false
            workspaceSearchBusy = false
            return
        }
        workspaceSearchBusy = true
        let generation = selectionGeneration
        do {
            let result = try await core.workspaceSearch(sessionID: sessionID, query: query)
            guard isCurrent(sessionID: sessionID, generation: generation),
                  workspaceSearchQuery == query
            else { return }
            workspaceSearchResults = result.entries
            workspaceSearchTruncated = result.truncated
            workspaceSearchBusy = false
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation),
                  workspaceSearchQuery == query
            else { return }
            workspaceSearchError = error.localizedDescription
            workspaceSearchBusy = false
        }
    }

    func searchWorkspaceContents(_ rawQuery: String) async {
        guard let sessionID = selectedSessionID else { return }
        let query = rawQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        workspaceContentSearchQuery = query
        workspaceContentSearchError = nil
        if query.isEmpty {
            workspaceContentMatches = []
            workspaceContentSearchTruncated = false
            workspaceContentFilesScanned = 0
            workspaceContentSearchBusy = false
            return
        }
        workspaceContentSearchBusy = true
        let generation = selectionGeneration
        do {
            let result = try await core.workspaceGrep(sessionID: sessionID, query: query)
            guard selectionGeneration == generation,
                  workspaceContentSearchQuery == query
            else { return }
            workspaceContentMatches = result.matches
            workspaceContentFilesScanned = result.filesScanned
            workspaceContentSearchTruncated = result.truncated
            workspaceContentSearchBusy = false
        } catch {
            guard selectionGeneration == generation,
                  workspaceContentSearchQuery == query
            else { return }
            workspaceContentSearchError = error.localizedDescription
            workspaceContentSearchBusy = false
        }
    }

    func setWorkspaceSearchMode(_ mode: WorkspaceSearchMode) {
        if workspaceSearchMode != mode {
            workspaceSearchMode = mode
            workspaceContentSearchError = nil
        }
    }

    func fetchWorkspaceFile(path: String) async -> WorkspaceFileResult? {
        guard let sessionID = selectedSessionID else { return nil }
        do {
            return try await core.workspaceFile(sessionID: sessionID, path: path)
        } catch {
            handle(error)
            return nil
        }
    }

    func writeWorkspaceFile(
        path: String,
        content: String,
        expectedSha256: String?
    ) async -> WorkspaceWriteResult? {
        guard let sessionID = selectedSessionID else { return nil }
        do {
            return try await core.workspaceWrite(
                sessionID: sessionID,
                path: path,
                content: content,
                expectedSha256: expectedSha256
            )
        } catch {
            handle(error)
            return nil
        }
    }

    func openWorkspaceFile(_ entry: WorkspaceEntry) async {
        guard !entry.isDirectory, let sessionID = selectedSessionID else { return }
        workspaceSelectedPath = entry.path
        workspaceFileBusy = true
        workspaceFileError = nil
        workspaceSaveStatus = nil
        defer { workspaceFileBusy = false }
        do {
            let file = try await core.workspaceFile(sessionID: sessionID, path: entry.path)
            guard sessionID == selectedSessionID,
                  workspaceSelectedPath == entry.path
            else { return }
            workspaceFile = file
            workspaceDraft = file.content
            workspaceDraftPath = file.path
        } catch {
            guard sessionID == selectedSessionID,
                  workspaceSelectedPath == entry.path
            else { return }
            workspaceFile = nil
            workspaceDraft = ""
            workspaceDraftPath = nil
            workspaceFileError = error.localizedDescription
        }
    }

    func saveWorkspaceFile(content: String) async -> Bool {
        guard let sessionID = selectedSessionID,
              let file = workspaceFile,
              file.editable
        else {
            workspaceSaveStatus = "This file is read-only."
            return false
        }
        let savingPath = file.path
        workspaceSaveBusy = true
        workspaceSaveStatus = nil
        defer { workspaceSaveBusy = false }
        do {
            let result = try await core.workspaceWrite(
                sessionID: sessionID,
                path: file.path,
                content: content,
                expectedSha256: file.sha256
            )
            guard sessionID == selectedSessionID,
                  workspaceDraftPath == savingPath,
                  workspaceFile?.path == savingPath
            else { return false }
            workspaceFile = WorkspaceFileResult(
                path: result.path,
                size: result.size,
                truncated: false,
                binary: false,
                content: content,
                sha256: result.sha256,
                editable: true
            )
            workspaceDraft = content
            workspaceDraftPath = result.path
            workspaceFileError = nil
            workspaceSaveStatus = "Saved \(result.path) (\(result.size) bytes)."
            await loadDiff()
            return true
        } catch {
            workspaceSaveStatus = error.localizedDescription
            return false
        }
    }

    func reloadWorkspaceFile() async {
        guard let path = workspaceSelectedPath,
              let entry = workspaceEntries.first(where: { $0.path == path })
        else { return }
        await openWorkspaceFile(entry)
    }

    func openTerminal() async {
        guard let sessionID = selectedSessionID else {
            terminalError = "Select a session before opening a terminal."
            return
        }
        guard terminalID == nil else { return }
        terminalBusy = true
        defer { terminalBusy = false }
        do {
            let opened = try await core.openTerminal(
                sessionID: sessionID,
                cols: terminalCols,
                rows: terminalRows
            )
            terminalID = opened.terminalId
            terminalWorktree = opened.worktree
            terminalData = Data()
            terminalSeq = 0
            terminalStatus = "running"
            terminalExitCode = nil
            terminalError = nil
            terminalSessionID = sessionID
        } catch {
            terminalError = error.localizedDescription
            handle(error)
        }
    }

    func sendTerminalInput(_ data: Data) async {
        guard let terminalID else { return }
        do {
            _ = try await core.writeTerminal(terminalID: terminalID, data: data)
        } catch {
            terminalError = error.localizedDescription
        }
    }

    func pollTerminal() async {
        guard let terminalID else { return }
        do {
            let result = try await core.readTerminal(
                terminalID: terminalID,
                afterSeq: terminalSeq
            )
            for chunk in result.chunks {
                if let data = Data(base64Encoded: chunk.data) {
                    terminalData.append(data)
                }
            }
            if terminalData.count > 2_000_000 {
                terminalData = terminalData.suffix(1_500_000)
            }
            terminalSeq = result.nextSeq
            if result.status != terminalStatus {
                terminalStatus = result.status
                if result.status != "running" {
                    terminalExitCode = result.exitCode
                }
            }
            if result.dropped > 0 {
                terminalError = "terminal output buffer dropped \(result.dropped) chunks"
            }
        } catch {
            terminalError = error.localizedDescription
        }
    }

    func resizeTerminal(cols: Int, rows: Int) async {
        guard let terminalID else { return }
        let cols = cols.clamped(to: 1 ... 500)
        let rows = rows.clamped(to: 1 ... 500)
        guard cols != terminalCols || rows != terminalRows else { return }
        terminalCols = cols
        terminalRows = rows
        do {
            _ = try await core.resizeTerminal(
                terminalID: terminalID,
                cols: cols,
                rows: rows
            )
        } catch {
            terminalError = error.localizedDescription
        }
    }

    func closeTerminal() async {
        guard let terminalID else { return }
        terminalBusy = true
        defer { terminalBusy = false }
        do {
            let result = try await core.closeTerminal(terminalID: terminalID)
            terminalExitCode = result.exitCode ?? terminalExitCode
            self.terminalID = nil
            terminalWorktree = ""
            terminalStatus = "closed"
            terminalSessionID = nil
        } catch {
            terminalError = error.localizedDescription
        }
    }

    func openConsole() async {
        guard let sessionID = selectedSessionID else {
            consoleError = "Select a session before opening a console."
            return
        }
        guard consoleID == nil else { return }
        consoleBusy = true
        defer { consoleBusy = false }
        do {
            let opened = try await core.openConsole(sessionID: sessionID)
            consoleID = opened.consoleId
            consoleWorktree = opened.worktree
            consoleOutput = "Console ready in \(opened.worktree)\n"
            consoleSeq = 0
            consoleStatus = "running"
            consoleExitCode = nil
            consoleInput = ""
            consoleError = nil
        } catch {
            consoleError = error.localizedDescription
            handle(error)
        }
    }

    func sendConsole() async {
        guard let consoleID else { return }
        let command = consoleInput
        guard !command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        consoleInput = ""
        consoleOutput += "$ \(command)\n"
        do {
            _ = try await core.writeConsole(
                consoleID: consoleID,
                data: command + "\n"
            )
        } catch {
            consoleError = error.localizedDescription
        }
    }

    func pollConsole() async {
        guard let consoleID else { return }
        do {
            let result = try await core.readConsole(
                consoleID: consoleID,
                afterSeq: consoleSeq
            )
            if !result.chunks.isEmpty {
                consoleOutput += result.chunks.map(\.text).joined()
                if consoleOutput.count > 400_000 {
                    consoleOutput = String(consoleOutput.suffix(300_000))
                }
            }
            consoleSeq = result.nextSeq
            if result.status != consoleStatus {
                consoleStatus = result.status
                if result.status != "running" {
                    consoleExitCode = result.exitCode
                }
            }
            if result.dropped > 0 {
                consoleError = "console output buffer dropped \(result.dropped) chunks"
            }
        } catch {
            consoleError = error.localizedDescription
        }
    }

    func interruptConsole() async {
        guard let consoleID else { return }
        do {
            _ = try await core.signalConsole(consoleID: consoleID, signal: "int")
        } catch {
            consoleError = error.localizedDescription
        }
    }

    func closeConsole() async {
        guard let consoleID else { return }
        consoleBusy = true
        defer { consoleBusy = false }
        do {
            let result = try await core.closeConsole(consoleID: consoleID)
            consoleExitCode = result.exitCode ?? consoleExitCode
            consoleStatus = "closed"
            self.consoleID = nil
            consoleWorktree = ""
        } catch {
            consoleError = error.localizedDescription
        }
    }

    func clearConsole() {
        consoleOutput = ""
    }

    func loadProcesses() async {
        guard let sessionID = selectedSessionID
        else {
            supervisedProcesses = []
            selectedProcessID = nil
            processLogs = ""
            return
        }
        processesBusy = true
        defer { processesBusy = false }
        do {
            let result = try await core.listProcesses(sessionID: sessionID)
            supervisedProcesses = result.processes
            if let selectedProcessID,
               !result.processes.contains(where: { $0.id == selectedProcessID })
            {
                self.selectedProcessID = nil
                processLogs = ""
            }
            if selectedProcessID == nil {
                selectedProcessID = result.processes.first?.id
            }
            processesError = nil
            if let selectedProcessID {
                await loadProcessLogs(processID: selectedProcessID)
            }
        } catch {
            processesError = error.localizedDescription
            handle(error)
        }
    }

    func loadProcessLogs(processID: String) async {
        guard let sessionID = selectedSessionID else { return }
        do {
            let result = try await core.processLogs(
                sessionID: sessionID,
                processID: processID,
                maxBytes: 65536,
                fromEnd: true
            )
            var text = result.content
            if result.rotated {
                text = "[older output was dropped by bounded rotation]\n" + text
            }
            if text.count > 200_000 {
                text = String(text.suffix(160_000))
            }
            processLogs = text
        } catch {
            processesError = error.localizedDescription
        }
    }

    func refreshProcessSummary() async {
        guard let sessionID = selectedSessionID else {
            processSummary = []
            return
        }
        do {
            let result = try await core.listProcesses(
                sessionID: sessionID,
                includeFinished: false,
                limit: 8
            )
            processSummary = result.processes
        } catch {
            // A transient summary failure must not replace the Stage with an
            // error banner; the Preview card owns detailed diagnostics.
            processSummary = []
        }
    }

    var activeProcessCount: Int {
        processSummary.count
    }

    func stopSupervisedProcess(_ processID: String) async {
        guard let sessionID = selectedSessionID else { return }
        processStopBusy = true
        defer { processStopBusy = false }
        do {
            let result = try await core.stopProcess(sessionID: sessionID, processID: processID)
            if let index = supervisedProcesses.firstIndex(where: { $0.id == processID }) {
                supervisedProcesses[index] = result.process
            }
            if result.process.state != "running", result.process.state != "starting" {
                processSummary.removeAll { $0.id == processID }
            }
            processesError = nil
            await loadProcessLogs(processID: processID)
        } catch {
            processesError = error.localizedDescription
        }
    }

    func loadMemories() async {
        guard let projectID = selectedProjectID else {
            memories = []
            return
        }
        memoryBusy = true
        defer { memoryBusy = false }
        do {
            let query = memoryQuery.trimmingCharacters(in: .whitespacesAndNewlines)
            let items = try await core.listMemories(
                projectID: projectID,
                query: query.isEmpty ? nil : query,
                kind: nil,
                includeStale: memoryIncludeStale,
                limit: 50
            )
            guard projectID == selectedProjectID else { return }
            memories = items
            memoryStatus = nil
        } catch {
            memoryStatus = error.localizedDescription
        }
        await loadMemoryStats()
        await loadMemoryHealth()
        await loadMemoryConflicts()
    }

    func loadMemoryStats() async {
        guard let projectID = selectedProjectID else {
            memoryStats = nil
            return
        }
        do {
            let stats = try await core.memoryStats(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            memoryStats = stats
            memoryStatsStatus = nil
        } catch {
            memoryStatsStatus = error.localizedDescription
        }
    }

    func loadMemoryHealth() async {
        guard let projectID = selectedProjectID else {
            memoryHealth = nil
            return
        }
        do {
            let health = try await core.memoryHealth(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            memoryHealth = health
            await loadMemoryEmbeddingStatus()
        } catch {
            memoryConflictStatus = error.localizedDescription
        }
    }

    func loadMemoryConflicts() async {
        guard let projectID = selectedProjectID else {
            memoryConflicts = []
            return
        }
        do {
            let conflicts = try await core.memoryConflicts(
                projectID: projectID,
                status: nil,
                limit: 100
            )
            guard projectID == selectedProjectID else { return }
            memoryConflicts = conflicts
        } catch {
            memoryConflictStatus = error.localizedDescription
        }
    }

    func scanMemoryConflicts() async {
        guard let projectID = selectedProjectID else {
            memoryConflictStatus = "Select a project before scanning."
            return
        }
        memoryConflictBusy = true
        defer { memoryConflictBusy = false }
        do {
            let result = try await core.scanMemoryConflicts(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            if result.detected > 0 {
                memoryConflictStatus =
                    "Detected \(result.detected) new conflict(s); \(result.open) open total."
            } else {
                memoryConflictStatus =
                    "No new same-scope conflicts. \(result.open) open total."
            }
            await loadMemoryHealth()
            await loadMemoryConflicts()
        } catch {
            memoryConflictStatus = error.localizedDescription
            handle(error)
        }
    }

    func resolveMemoryConflict(
        _ conflict: MemoryConflict,
        action: MemoryConflictResolution
    ) async {
        memoryConflictBusy = true
        defer { memoryConflictBusy = false }
        do {
            let result = try await core.resolveMemoryConflict(
                id: conflict.id,
                action: action,
                note: nil
            )
            memoryConflictStatus =
                "Conflict resolved with \(action.title.lowercased()): \(result.conflict.status)."
            await loadMemories()
        } catch {
            memoryConflictStatus = error.localizedDescription
            handle(error)
        }
    }

    func toggleMemoryPin(_ memory: MemoryItem) async {
        memoryActionBusyID = memory.id
        defer { memoryActionBusyID = nil }
        do {
            _ = try await core.memoryUpdate(
                id: memory.id,
                status: nil,
                pinned: !memory.isPinned,
                tags: nil
            )
            memoryStatus = memory.isPinned ? "Memory unpinned." : "Memory pinned."
            await loadMemories()
        } catch {
            memoryStatus = error.localizedDescription
            handle(error)
        }
    }

    func verifyMemory(_ memory: MemoryItem) async {
        memoryActionBusyID = memory.id
        defer { memoryActionBusyID = nil }
        do {
            let result = try await core.memoryVerify(id: memory.id)
            let checked = result.checks.filter { $0.status != "unchecked" }.count
            let missing = result.checks.filter { $0.status == "missing" }.count
            if missing > 0 {
                memoryStatus =
                    "Verification found \(missing) missing evidence reference(s); the memory is now stale."
            } else if checked == 0 {
                memoryStatus =
                    "No locally checkable evidence. The memory was left unchanged rather than treated as verified."
            } else {
                memoryStatus = "Verified \(checked) evidence reference(s)."
            }
            await loadMemories()
        } catch {
            memoryStatus = error.localizedDescription
            handle(error)
        }
    }

    func sweepStaleMemories() async {
        guard let projectID = selectedProjectID else { return }
        memorySweepBusy = true
        defer { memorySweepBusy = false }
        do {
            let preview = try await core.memorySweep(
                projectID: projectID,
                olderThanDays: nil,
                dryRun: true
            )
            guard preview.candidates > 0 else {
                memoryStatus = "No memories are past their verification window."
                return
            }
            let result = try await core.memorySweep(
                projectID: projectID,
                olderThanDays: nil,
                dryRun: false
            )
            memoryStatus =
                "Marked \(result.markedStale) of \(result.candidates) expired memories stale."
            await loadMemories()
        } catch {
            memoryStatus = error.localizedDescription
            handle(error)
        }
    }

    func deleteMemory(_ memory: MemoryItem) async {
        memoryBusy = true
        defer { memoryBusy = false }
        do {
            let result = try await core.deleteMemory(id: memory.id)
            memoryStatus = result.removed ? "Memory deleted." : "Memory no longer exists."
            await loadMemories()
        } catch {
            memoryStatus = error.localizedDescription
            handle(error)
        }
    }

    func pruneMemories(olderThanDays: Int? = nil, keepLatest: Int? = nil) async {
        guard let projectID = selectedProjectID else { return }
        memoryBusy = true
        defer { memoryBusy = false }
        do {
            let preview = try await core.pruneMemories(
                projectID: projectID,
                olderThanDays: olderThanDays,
                keepLatest: keepLatest,
                dryRun: true
            )
            guard preview.candidates > 0 else {
                memoryStatus = "No memories matched the prune policy."
                return
            }
            let result = try await core.pruneMemories(
                projectID: projectID,
                olderThanDays: olderThanDays,
                keepLatest: keepLatest,
                dryRun: false
            )
            memoryStatus = "Pruned \(result.removed) memories."
            await loadMemories()
        } catch {
            memoryStatus = error.localizedDescription
            handle(error)
        }
    }

    func createMemory() async {
        guard let sessionID = selectedSessionID else {
            memoryStatus = "Select a session before adding a memory."
            return
        }
        let kind = memoryNewKind.trimmingCharacters(in: .whitespacesAndNewlines)
        let title = memoryNewTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = memoryNewBody.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !kind.isEmpty, !title.isEmpty, !body.isEmpty else {
            memoryStatus = "Kind, title, and body are required."
            return
        }
        memoryBusy = true
        defer { memoryBusy = false }
        do {
            _ = try await core.createMemory(
                sessionID: sessionID,
                kind: kind,
                title: title,
                body: body,
                tags: []
            )
            memoryNewTitle = ""
            memoryNewBody = ""
            memoryStatus = "Memory added."
            await loadMemories()
        } catch {
            memoryStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadProjectUsage() async {
        guard let projectID = selectedProjectID else {
            projectUsage = nil
            return
        }
        projectUsageBusy = true
        defer { projectUsageBusy = false }
        do {
            let usage = try await core.projectUsage(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            projectUsage = usage
            projectUsageStatus = nil
        } catch {
            projectUsageStatus = error.localizedDescription
        }
    }

    /// Loads the read-only efficiency surface: project totals, the selected
    /// session ledger, context usage, and the optional `session.cache` status.
    /// Older daemons do not implement `session.cache`; the dashboard then falls
    /// back to ratios derived from `session.usage` instead of failing.
    func loadEfficiency() async {
        guard let projectID = selectedProjectID else {
            projectUsage = nil
            projectUsageStatus = nil
            usageLedger = nil
            usageStatus = nil
            contextStatus = nil
            toolOutputStatus = nil
            resetEfficiencyState()
            return
        }
        let generation = selectionGeneration
        let sessionID = selectedSessionID
        efficiencyBusy = true
        efficiencyStatus = nil
        defer {
            if generation == selectionGeneration {
                efficiencyBusy = false
            }
        }

        do {
            let usage = try await core.projectUsage(projectID: projectID)
            guard projectID == selectedProjectID, generation == selectionGeneration else {
                return
            }
            projectUsage = usage
            projectUsageStatus = nil
        } catch {
            guard projectID == selectedProjectID, generation == selectionGeneration else {
                return
            }
            projectUsageStatus = error.localizedDescription
            efficiencyStatus = error.localizedDescription
        }

        do {
            let status = try await core.toolOutputStatus(projectID: projectID, sessionID: nil)
            guard projectID == selectedProjectID, generation == selectionGeneration else {
                return
            }
            toolOutputStatus = status
        } catch {
            guard projectID == selectedProjectID, generation == selectionGeneration else {
                return
            }
            toolOutputStatus = nil
        }

        do {
            let status = try await core.otelStatus(projectID: projectID, sessionID: nil)
            guard projectID == selectedProjectID, generation == selectionGeneration else {
                return
            }
            otelStatus = status
            otelMessage = nil
        } catch {
            guard projectID == selectedProjectID, generation == selectionGeneration else {
                return
            }
            otelStatus = nil
            otelMessage = "telemetry.otel.status is unavailable: \(error.localizedDescription)"
        }

        guard let sessionID,
              selectedSessionID == sessionID,
              generation == selectionGeneration
        else {
            usageLedger = nil
            usageStatus = nil
            contextStatus = nil
            sessionCacheStatus = nil
            sessionInstructions = nil
            sessionContract = nil
            sessionEffects = nil
            sessionGates = nil
            sessionCompatibility = nil
            otelStatus = nil
            governorStatus = nil
            governorStatusMessage = nil
            toolActivity = nil
            toolActivityStatus = nil
            return
        }

        do {
            let ledger = try await core.usageLedger(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            usageLedger = ledger
            usageStatus = nil
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                usageStatus = error.localizedDescription
            }
        }

        do {
            let status = try await core.contextStatus(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            contextStatus = status
        } catch {
            // Context status is diagnostic; keep the last successful snapshot.
        }

        do {
            let status = try await core.toolOutputStatus(
                projectID: projectID,
                sessionID: sessionID
            )
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            toolOutputStatus = status
        } catch {
            // Tool-output status is diagnostic; keep the project-level value.
        }

        do {
            let instructions = try await core.sessionInstructions(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionInstructions = instructions
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionInstructions = nil
        }

        do {
            let contract = try await core.sessionContract(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionContract = contract
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionContract = nil
        }

        do {
            let effects = try await core.sessionEffects(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionEffects = effects
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionEffects = nil
        }

        do {
            let gates = try await core.sessionGates(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionGates = gates
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionGates = nil
        }

        do {
            let compatibility = try await core.sessionCompatibility(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionCompatibility = compatibility
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionCompatibility = nil
        }

        do {
            let status = try await core.otelStatus(projectID: projectID, sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            otelStatus = status
            otelMessage = nil
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                otelMessage = "telemetry.otel.status is unavailable: \(error.localizedDescription)"
            }
        }

        do {
            let cache = try await core.sessionCache(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionCacheStatus = cache
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionCacheStatus = nil
            if usageLedger == nil, let usageStatus {
                efficiencyStatus = usageStatus
            } else if efficiencyStatus == nil {
                efficiencyStatus =
                    "session.cache is unavailable; cache values are derived from session.usage."
            }
        }

        do {
            let calls = try await core.sessionCacheCalls(sessionID: sessionID, limit: 20)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionCacheCalls = calls.calls
            sessionCacheCallsTruncated = calls.truncated
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionCacheCalls = []
            sessionCacheCallsTruncated = false
        }

        do {
            let governor = try await core.budgetGovernor(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            governorStatus = governor
            governorStatusMessage = nil
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            governorStatus = nil
            // The RPC is newer than some daemons. A missing governor is a
            // diagnostic, not a reason to fail the efficiency surface.
            governorStatusMessage = "session.budget.governor is unavailable: \(error.localizedDescription)"
        }

        await loadToolActivity()
    }

    /// Explicit one-shot OTel projection export for the selected session. This
    /// never runs automatically; the policy only enables the operator action.
    func exportOtelTraces() async {
        guard let sessionID = selectedSessionID, !otelBusy else { return }
        let generation = selectionGeneration
        otelBusy = true
        otelMessage = nil
        defer {
            if generation == selectionGeneration {
                otelBusy = false
            }
        }
        do {
            let result = try await core.exportOtel(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            if result.exported {
                otelMessage =
                    "Exported \(result.spanCount) span(s) to the local collector (HTTP \(result.httpStatus ?? 0))."
            } else {
                otelMessage = result.message ?? "OTel export failed (\(result.errorCode ?? "unknown"))."
            }
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            otelMessage = error.localizedDescription
        }
    }

    /// Explicit connectivity test. It sends one synthetic span and never
    /// exports session content.
    func testOtelCollector() async {
        guard let projectID = selectedProjectID, !otelBusy else { return }
        let generation = selectionGeneration
        otelBusy = true
        otelMessage = nil
        defer {
            if generation == selectionGeneration {
                otelBusy = false
            }
        }
        do {
            let result = try await core.testOtel(projectID: projectID)
            guard projectID == selectedProjectID, generation == selectionGeneration else {
                return
            }
            otelMessage = result.ok
                ? "Local collector reachable (HTTP \(result.httpStatus ?? 0))."
                : (result.message ?? "OTel collector test failed (\(result.errorCode ?? "unknown")).")
        } catch {
            guard projectID == selectedProjectID, generation == selectionGeneration else {
                return
            }
            otelMessage = error.localizedDescription
        }
    }

    /// Load the bounded newest-first tool activity window. Older daemons do not
    /// implement `session.tool_activity`; the dashboard then keeps the current
    /// snapshot and shows a diagnostic status instead of failing the whole
    /// efficiency surface.
    func loadToolActivity() async {
        guard let sessionID = selectedSessionID else {
            toolActivity = nil
            toolActivityStatus = nil
            return
        }
        let generation = selectionGeneration
        toolActivityBusy = true
        defer {
            if generation == selectionGeneration {
                toolActivityBusy = false
            }
        }
        do {
            let activity = try await core.toolActivity(sessionID: sessionID, limit: 50)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            toolActivity = activity
            toolActivityStatus = nil
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            toolActivityStatus = error.localizedDescription
        }
    }

    private func refreshToolActivitySoon() {
        toolActivityRefreshTask?.cancel()
        toolActivityRefreshTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            await self?.loadToolActivity()
        }
    }

    private func refreshEfficiencySoon() {
        efficiencyRefreshTask?.cancel()
        efficiencyRefreshTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            await self?.loadEfficiency()
        }
    }

    func loadProjectBudget() async {
        guard let projectID = selectedProjectID else {
            projectBudget = .empty
            projectSpentTokens = 0
            projectSpentCostUsd = 0
            projectTokenBudgetText = ""
            projectCostBudgetText = ""
            projectBudgetStatus = nil
            return
        }
        projectBudgetBusy = true
        defer { projectBudgetBusy = false }
        do {
            let result = try await core.projectBudget(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            projectBudget = result.budget
            projectSpentTokens = result.spentTokens
            projectSpentCostUsd = result.spentCostUsd
            projectTokenBudgetText = result.budget.tokenBudget.map { String($0) } ?? ""
            projectCostBudgetText = result.budget.costBudgetUsd.map { String($0) } ?? ""
            projectBudgetStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveProjectBudget() async {
        guard let projectID = selectedProjectID else {
            projectBudgetStatus = "Select a project first."
            return
        }
        func optionalInt(_ text: String) -> Int? {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? nil : Int(trimmed)
        }
        func optionalDouble(_ text: String) -> Double? {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? nil : Double(trimmed)
        }
        let tokens = optionalInt(projectTokenBudgetText)
        let cost = optionalDouble(projectCostBudgetText)
        if (!projectTokenBudgetText.trimmingCharacters(in: .whitespaces).isEmpty && tokens == nil)
            || (!projectCostBudgetText.trimmingCharacters(in: .whitespaces).isEmpty && cost == nil)
        {
            projectBudgetStatus = "Budgets must be empty or positive numbers."
            return
        }
        projectBudgetBusy = true
        defer { projectBudgetBusy = false }
        do {
            let result = try await core.setProjectBudget(
                projectID: projectID,
                budget: ProjectBudget(tokenBudget: tokens, costBudgetUsd: cost)
            )
            guard projectID == selectedProjectID else { return }
            projectBudget = result.budget
            projectSpentTokens = result.spentTokens
            projectSpentCostUsd = result.spentCostUsd
            projectBudgetStatus = "Project budget saved"
        } catch {
            projectBudgetStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetProjectBudget() async {
        projectTokenBudgetText = ""
        projectCostBudgetText = ""
        await saveProjectBudget()
    }

    func loadMailboxPolicy() async {
        guard let projectID = selectedProjectID else {
            mailboxPolicy = .disabled
            mailboxMaxAgeText = ""
            mailboxMaxCountText = ""
            mailboxDeleteUnread = false
            mailboxAutoGc = false
            mailboxPolicyStatus = nil
            projectBudget = .empty
            projectSpentTokens = 0
            projectSpentCostUsd = 0
            projectTokenBudgetText = ""
            projectCostBudgetText = ""
            projectBudgetStatus = nil
            return
        }
        mailboxPolicyBusy = true
        defer { mailboxPolicyBusy = false }
        do {
            let policy = try await core.mailboxPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            mailboxPolicy = policy
            mailboxMaxAgeText = policy.maxAgeSecs.map(String.init) ?? ""
            mailboxMaxCountText = policy.maxCount.map(String.init) ?? ""
            mailboxDeleteUnread = policy.deleteUnread
            mailboxAutoGc = policy.autoGc
            mailboxPolicyStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveMailboxPolicy() async {
        guard let projectID = selectedProjectID else {
            mailboxPolicyStatus = "Select a project first."
            return
        }
        func optionalInt(_ text: String) -> Int? {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? nil : Int(trimmed)
        }
        let maxAge = optionalInt(mailboxMaxAgeText)
        let maxCount = optionalInt(mailboxMaxCountText)
        if (!mailboxMaxAgeText.trimmingCharacters(in: .whitespaces).isEmpty && maxAge == nil)
            || (!mailboxMaxCountText.trimmingCharacters(in: .whitespaces).isEmpty && maxCount == nil)
        {
            mailboxPolicyStatus = "Limits must be empty or positive integers."
            return
        }
        mailboxPolicyBusy = true
        defer { mailboxPolicyBusy = false }
        do {
            let policy = try await core.setMailboxPolicy(
                projectID: projectID,
                policy: MailboxPolicy(
                    maxAgeSecs: maxAge,
                    maxCount: maxCount,
                    deleteUnread: mailboxDeleteUnread,
                    autoGc: mailboxAutoGc
                )
            )
            guard projectID == selectedProjectID else { return }
            mailboxPolicy = policy
            mailboxPolicyStatus = "Mailbox policy saved"
        } catch {
            mailboxPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetMailboxPolicy() async {
        mailboxMaxAgeText = ""
        mailboxMaxCountText = ""
        mailboxDeleteUnread = false
        mailboxAutoGc = false
        await saveMailboxPolicy()
    }

    func gcMailbox() async {
        guard let projectID = selectedProjectID else { return }
        mailboxPolicyBusy = true
        defer { mailboxPolicyBusy = false }
        do {
            let result = try await core.gcMailbox(projectID: projectID)
            mailboxPolicyStatus = "Removed \(result.removed) message(s)"
            await loadMailbox()
        } catch {
            mailboxPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadToolOutputPolicy() async {
        guard let projectID = selectedProjectID else {
            toolOutputPolicy = .default
            toolOutputEnabled = true
            toolOutputSpillThresholdText = "16384"
            toolOutputPreviewBytesText = "4096"
            toolOutputMaxTurnBytesText = "65536"
            toolOutputRetentionSecsText = "604800"
            toolOutputMaxEntriesText = "2000"
            toolOutputToolThresholds = [:]
            toolOutputPolicyStatus = nil
            return
        }
        toolOutputPolicyBusy = true
        defer { toolOutputPolicyBusy = false }
        do {
            let policy = try await core.toolOutputPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            toolOutputPolicy = policy
            toolOutputEnabled = policy.enabled
            toolOutputSpillThresholdText = String(policy.spillThresholdBytes)
            toolOutputPreviewBytesText = String(policy.previewBytes)
            toolOutputMaxTurnBytesText = String(policy.maxTotalBytesPerTurn)
            toolOutputRetentionSecsText = String(policy.retentionSecs)
            toolOutputMaxEntriesText = String(policy.maxEntries)
            toolOutputToolThresholds = policy.toolThresholds ?? [:]
            toolOutputPolicyStatus = nil
        } catch {
            toolOutputPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func saveToolOutputPolicy() async {
        guard let projectID = selectedProjectID else {
            toolOutputPolicyStatus = "Select a project first."
            return
        }
        func requiredInt(_ title: String, _ text: String) -> Int? {
            guard let value = Int(text.trimmingCharacters(in: .whitespaces)), value > 0 else {
                toolOutputPolicyStatus = "\(title) must be a positive integer."
                return nil
            }
            return value
        }
        guard let spill = requiredInt("Spill threshold", toolOutputSpillThresholdText),
              let preview = requiredInt("Preview bytes", toolOutputPreviewBytesText),
              let maxTurn = requiredInt("Turn budget", toolOutputMaxTurnBytesText),
              let retention = requiredInt("Retention", toolOutputRetentionSecsText),
              let maxEntries = requiredInt("Max entries", toolOutputMaxEntriesText)
        else { return }
        if preview > spill {
            toolOutputPolicyStatus = "Preview bytes must not exceed the spill threshold."
            return
        }
        if maxTurn < preview {
            toolOutputPolicyStatus = "Turn budget must not be smaller than preview bytes."
            return
        }
        toolOutputPolicyBusy = true
        defer { toolOutputPolicyBusy = false }
        do {
            let policy = try await core.setToolOutputPolicy(
                projectID: projectID,
                policy: ToolOutputPolicy(
                    enabled: toolOutputEnabled,
                    spillThresholdBytes: spill,
                    previewBytes: preview,
                    maxTotalBytesPerTurn: maxTurn,
                    retentionSecs: retention,
                    maxEntries: maxEntries,
                    toolThresholds: toolOutputToolThresholds.isEmpty
                        ? nil
                        : toolOutputToolThresholds
                )
            )
            guard projectID == selectedProjectID else { return }
            toolOutputPolicy = policy
            toolOutputPolicyStatus = policy.enabled
                ? "Tool-output policy saved"
                : "Tool-output spilling disabled"
        } catch {
            toolOutputPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetToolOutputPolicy() async {
        toolOutputEnabled = true
        toolOutputSpillThresholdText = "16384"
        toolOutputPreviewBytesText = "4096"
        toolOutputMaxTurnBytesText = "65536"
        toolOutputRetentionSecsText = "604800"
        toolOutputMaxEntriesText = "2000"
        toolOutputToolThresholds = [:]
        await saveToolOutputPolicy()
    }

    func auditToolOutputs() async {
        guard let projectID = selectedProjectID else { return }
        toolOutputPolicyBusy = true
        defer { toolOutputPolicyBusy = false }
        do {
            let audit = try await core.auditToolOutputs(
                projectID: projectID,
                verifyHashes: false,
                cleanOrphans: true
            )
            toolOutputPolicyStatus =
                "Audit: \(audit.rows) handles, \(audit.missingBlobs) missing, "
                    + "\(audit.hashMismatches) mismatched, \(audit.cleanedOrphans) orphan(s) cleaned"
        } catch {
            toolOutputPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadTestCache(includeEntries: Bool = true) async {
        guard let projectID = selectedProjectID else {
            testCacheStats = nil
            testCacheEntries = []
            applyTestCachePolicy(.default)
            testCacheStatus = nil
            return
        }
        testCacheBusy = true
        defer { testCacheBusy = false }
        do {
            let stats = try await core.testCacheStats(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            testCacheStats = stats
            testCachePolicy = stats.policy
            applyTestCachePolicy(stats.policy)
            testCacheScopePreview = try? await core.testCacheScopePreview(
                projectID: projectID
            )
            if includeEntries {
                let result = try await core.testCacheEntries(
                    projectID: projectID,
                    sessionID: nil,
                    includeExpired: false,
                    limit: 200
                )
                guard projectID == selectedProjectID else { return }
                testCacheEntries = result.entries
            }
            testCacheStatus = nil
        } catch {
            testCacheStatus = error.localizedDescription
            handle(error)
        }
    }

    private func applyTestCachePolicy(_ policy: TestCachePolicy) {
        testCachePolicy = policy
        testCacheEnabled = policy.enabled
        testCacheMaxEntriesText = String(policy.maxEntries)
        testCacheRetentionDaysText = String(policy.retentionDays)
        testCacheMaxOutputBytesText = String(policy.maxOutputBytes)
        testCacheReuseLint = policy.reuseLint
        testCacheIncludeIntegration = policy.includeIntegration
        testCacheFailedRetentionText = String(policy.failedRetentionSecs)
        testCacheExtraEnvText = policy.extraEnv.joined(separator: "\n")
        testCacheExtraPathsText = policy.extraPaths.joined(separator: "\n")
    }

    func saveTestCachePolicy() async {
        guard let projectID = selectedProjectID else {
            testCacheStatus = "Select a project first."
            return
        }
        guard let maxEntries = Int(testCacheMaxEntriesText.trimmingCharacters(in: .whitespaces)),
              let retentionDays = Int(
                  testCacheRetentionDaysText.trimmingCharacters(in: .whitespaces)
              ),
              let maxOutputBytes = Int(
                  testCacheMaxOutputBytesText.trimmingCharacters(in: .whitespaces)
              ),
              let failedRetention = Int(
                  testCacheFailedRetentionText.trimmingCharacters(in: .whitespaces)
              )
        else {
            testCacheStatus = "All test-cache policy fields must be integers."
            return
        }
        guard (1 ... 10000).contains(maxEntries) else {
            testCacheStatus = "Max entries must be between 1 and 10,000."
            return
        }
        guard (1 ... 365).contains(retentionDays) else {
            testCacheStatus = "Retention must be between 1 and 365 days."
            return
        }
        guard (4096 ... 8_388_608).contains(maxOutputBytes) else {
            testCacheStatus = "Output bound must be between 4 KiB and 8 MiB."
            return
        }
        guard (60 ... 86400).contains(failedRetention) else {
            testCacheStatus = "Failed-result TTL must be between 60 and 86,400 seconds."
            return
        }
        testCacheBusy = true
        defer { testCacheBusy = false }
        do {
            let policy = try await core.setTestCachePolicy(
                projectID: projectID,
                policy: TestCachePolicy(
                    enabled: testCacheEnabled,
                    maxEntries: maxEntries,
                    retentionDays: retentionDays,
                    maxOutputBytes: maxOutputBytes,
                    reuseLint: testCacheReuseLint,
                    includeIntegration: testCacheIncludeIntegration,
                    failedRetentionSecs: failedRetention,
                    extraEnv: testCacheExtraEnvText
                        .split(whereSeparator: \.isNewline)
                        .map { $0.trimmingCharacters(in: .whitespaces) }
                        .filter { !$0.isEmpty },
                    extraPaths: testCacheExtraPathsText
                        .split(whereSeparator: \.isNewline)
                        .map { $0.trimmingCharacters(in: .whitespaces) }
                        .filter { !$0.isEmpty }
                )
            )
            guard projectID == selectedProjectID else { return }
            applyTestCachePolicy(policy)
            testCacheStatus = policy.enabled ? "Test cache policy saved" : "Test cache disabled"
            await loadTestCache()
        } catch {
            testCacheStatus = error.localizedDescription
            handle(error)
        }
    }

    func pruneTestCache(dryRun: Bool) async {
        guard let projectID = selectedProjectID else { return }
        testCacheBusy = true
        defer { testCacheBusy = false }
        do {
            let result = try await core.pruneTestCache(projectID: projectID, dryRun: dryRun)
            testCacheStatus = dryRun
                ? "Prune preview: \(result.pruned) row(s) would be removed"
                : "Pruned \(result.pruned) row(s)"
            if !dryRun {
                await loadTestCache()
            }
        } catch {
            testCacheStatus = error.localizedDescription
            handle(error)
        }
    }

    func clearTestCache() async {
        guard let projectID = selectedProjectID else { return }
        testCacheBusy = true
        defer { testCacheBusy = false }
        do {
            let result = try await core.clearTestCache(projectID: projectID, confirm: true)
            testCacheStatus = "Cleared \(result.cleared) row(s)"
            await loadTestCache()
        } catch {
            testCacheStatus = error.localizedDescription
            handle(error)
        }
    }

    func previewTestImpact() async {
        guard let sessionID = selectedSessionID else {
            testImpactStatus = "Select a session first."
            return
        }
        testImpactBusy = true
        defer { testImpactBusy = false }
        do {
            let result = try await core.testImpact(
                sessionID: sessionID,
                changedPaths: []
            )
            guard sessionID == selectedSessionID else { return }
            testImpactResult = result
            testImpactStatus = nil
        } catch {
            testImpactStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadArtifactPolicy() async {
        guard let projectID = selectedProjectID else {
            artifactPolicy = .disabled
            artifactAutoGc = false
            artifactMaxBytesText = ""
            artifactMaxAgeText = ""
            artifactMaxCountText = ""
            artifactPolicyStatus = nil
            mailboxPolicy = .disabled
            mailboxMaxAgeText = ""
            mailboxMaxCountText = ""
            mailboxDeleteUnread = false
            mailboxAutoGc = false
            mailboxPolicyStatus = nil
            projectBudget = .empty
            projectSpentTokens = 0
            projectSpentCostUsd = 0
            projectTokenBudgetText = ""
            projectCostBudgetText = ""
            projectBudgetStatus = nil
            return
        }
        artifactPolicyBusy = true
        defer { artifactPolicyBusy = false }
        do {
            let policy = try await core.artifactPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            artifactPolicy = policy
            artifactAutoGc = policy.autoGc
            artifactMaxBytesText = policy.maxBytes.map(String.init) ?? ""
            artifactMaxAgeText = policy.maxAgeSecs.map(String.init) ?? ""
            artifactMaxCountText = policy.maxCount.map(String.init) ?? ""
            artifactPolicyStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveArtifactPolicy() async {
        guard let projectID = selectedProjectID else {
            artifactPolicyStatus = "Select a project first."
            return
        }
        func optionalInt(_ text: String) -> Int? {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? nil : Int(trimmed)
        }
        let maxBytes = optionalInt(artifactMaxBytesText)
        let maxAge = optionalInt(artifactMaxAgeText)
        let maxCount = optionalInt(artifactMaxCountText)
        if (!artifactMaxBytesText.trimmingCharacters(in: .whitespaces).isEmpty && maxBytes == nil)
            || (!artifactMaxAgeText.trimmingCharacters(in: .whitespaces).isEmpty && maxAge == nil)
            || (!artifactMaxCountText.trimmingCharacters(in: .whitespaces).isEmpty && maxCount == nil)
        {
            artifactPolicyStatus = "Limits must be empty or positive integers."
            return
        }
        artifactPolicyBusy = true
        defer { artifactPolicyBusy = false }
        do {
            let policy = try await core.setArtifactPolicy(
                projectID: projectID,
                policy: ArtifactPolicy(
                    autoGc: artifactAutoGc,
                    maxBytes: maxBytes,
                    maxAgeSecs: maxAge,
                    maxCount: maxCount
                )
            )
            guard projectID == selectedProjectID else { return }
            artifactPolicy = policy
            artifactPolicyStatus = policy.autoGc ? "Automatic GC enabled" : "Artifact policy saved"
        } catch {
            artifactPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetArtifactPolicy() async {
        artifactAutoGc = false
        artifactMaxBytesText = ""
        artifactMaxAgeText = ""
        artifactMaxCountText = ""
        await saveArtifactPolicy()
    }

    func gcArtifacts() async {
        guard let projectID = selectedProjectID else { return }
        artifactPolicyBusy = true
        defer { artifactPolicyBusy = false }
        do {
            let result = try await core.gcArtifacts(projectID: projectID)
            artifactPolicyStatus =
                "Removed \(result.removed) artifact(s), freed \(result.bytesFreed) bytes"
            await loadMailbox()
        } catch {
            artifactPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadGuiPolicy() async {
        guard let projectID = selectedProjectID else {
            guiPolicy = .standard
            guiGraceSecs = 45
            guiWaitMs = 15000
            guiStatusTtlSecs = 30
            guiSensitiveMode = .deny
            guiSensitiveBundlesText = ""
            guiSensitiveTitlesText = ""
            guiTimelineRetentionDays = 30
            guiPolicyStatus = nil
            return
        }
        guiPolicyBusy = true
        defer { guiPolicyBusy = false }
        do {
            let policy = try await core.guiPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            applyGuiPolicy(policy, status: nil)
        } catch {
            handle(error)
        }
    }

    func saveGuiPolicy() async {
        guard let projectID = selectedProjectID else {
            guiPolicyStatus = "Select a project first."
            return
        }
        guard guiGraceSecs >= 0, guiGraceSecs <= 600 else {
            guiPolicyStatus = "Grace must be between 0 and 600 seconds."
            return
        }
        guard guiWaitMs >= 0, guiWaitMs <= 60000 else {
            guiPolicyStatus = "Wait must be between 0 and 60000 ms."
            return
        }
        guard guiStatusTtlSecs >= 5, guiStatusTtlSecs <= 600 else {
            guiPolicyStatus = "Status cache TTL must be between 5 and 600 seconds."
            return
        }
        guard guiTimelineRetentionDays >= 0, guiTimelineRetentionDays <= 3650 else {
            guiPolicyStatus = "Timeline retention must be 0 (forever) or 1-3650 days."
            return
        }
        let bundles = guiSensitiveBundlesText
            .split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        let titles = guiSensitiveTitlesText
            .split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        guiPolicyBusy = true
        defer { guiPolicyBusy = false }
        do {
            let policy = try await core.setGuiPolicy(
                projectID: projectID,
                policy: GuiPolicy(
                    graceSecs: guiGraceSecs,
                    waitMs: guiWaitMs,
                    statusTtlSecs: guiStatusTtlSecs,
                    sensitiveMode: guiSensitiveMode,
                    sensitiveBundles: bundles,
                    sensitiveTitles: titles,
                    timelineRetentionDays: guiTimelineRetentionDays
                )
            )
            guard projectID == selectedProjectID else { return }
            applyGuiPolicy(
                policy,
                status: "Saved · grace \(policy.graceSecs)s · wait \(policy.waitMs)ms"
            )
            await loadGuiStatus()
        } catch {
            guiPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetGuiPolicy() async {
        guiGraceSecs = 45
        guiWaitMs = 15000
        guiStatusTtlSecs = 30
        guiSensitiveMode = .deny
        guiSensitiveBundlesText = ""
        guiSensitiveTitlesText = ""
        guiTimelineRetentionDays = 30
        await saveGuiPolicy()
    }

    private func applyGuiPolicy(_ policy: GuiPolicy, status: String?) {
        guiPolicy = policy
        guiGraceSecs = policy.graceSecs
        guiWaitMs = policy.waitMs
        guiStatusTtlSecs = policy.statusTtlSecs
        guiSensitiveMode = policy.sensitiveMode
        guiSensitiveBundlesText = policy.sensitiveBundles.joined(separator: "\n")
        guiSensitiveTitlesText = policy.sensitiveTitles.joined(separator: "\n")
        guiTimelineRetentionDays = policy.timelineRetentionDays
        guiPolicyStatus = status
    }

    func pruneGuiTimeline(dryRun: Bool) async {
        guard let projectID = selectedProjectID else {
            guiPolicyStatus = "Select a project first."
            return
        }
        guiTimelinePruneBusy = true
        defer { guiTimelinePruneBusy = false }
        do {
            guiTimelinePruneResult = try await core.guiTimelinePrune(
                projectID: projectID,
                olderThanDays: guiTimelineRetentionDays == 0 ? nil : guiTimelineRetentionDays,
                dryRun: dryRun,
                confirm: !dryRun
            )
            if let result = guiTimelinePruneResult {
                guiPolicyStatus = result.dryRun
                    ? "Dry run: \(result.pruned) event(s) would be pruned"
                    : "Pruned \(result.pruned) gui.action event(s); \(result.remaining) remain"
            }
            await loadGuiTimeline()
            await loadGuiTimelineRetention()
        } catch {
            guiPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadGuiTimelineRetention() async {
        guard let projectID = selectedProjectID else {
            guiTimelineRetention = nil
            return
        }
        do {
            guiTimelineRetention = try await core.guiTimelineRetention(
                projectID: projectID
            )
            guiTimelineAudit = try? await core.guiTimelineAudit(projectID: projectID)
        } catch {
            guiTimelineRetention = nil
            guiTimelineAudit = nil
        }
    }

    func loadDaemonAgentStatus() async {
        daemonAgentBusy = true
        defer { daemonAgentBusy = false }
        do {
            daemonAgentStatus = try await core.daemonAgentStatus()
        } catch {
            daemonAgentStatus = nil
        }
    }

    func loadGuiStatus() async {
        guiStatusBusy = true
        defer { guiStatusBusy = false }
        do {
            let status = try await core.guiStatus(projectID: selectedProjectID)
            guard selectedProjectID != nil || status.runtimeId.isEmpty == false else { return }
            guiStatus = status
            guiStatusError = nil
        } catch {
            guiStatusError = error.localizedDescription
        }
    }

    func releaseGuiLease() async {
        guard let leaseID = guiStatus?.holder?.leaseId else { return }
        do {
            _ = try await core.releaseComputerLease(leaseID: leaseID)
            guiPolicyStatus = "Released GUI lease \(leaseID.prefix(8))"
            await loadGuiStatus()
            await loadGuiTimeline()
        } catch {
            guiStatusError = error.localizedDescription
            handle(error)
        }
    }

    func loadGuiTimeline() async {
        guard let sessionID = selectedSessionID else {
            guiTimeline = []
            return
        }
        do {
            let result = try await core.guiTimeline(sessionID: sessionID, limit: 80)
            guard sessionID == selectedSessionID else { return }
            guiTimeline = result.actions
        } catch {
            guiStatusError = error.localizedDescription
        }
    }

    var guiHeldBySelectedSession: Bool {
        guard let selectedSessionID, let holder = guiStatus?.holder else { return false }
        return holder.sessionId == selectedSessionID
    }

    var guiLatestActivity: GuiActionInfo? {
        guiTimeline.last
    }

    var guiStageLabel: String? {
        if let holder = guiStatus?.holder {
            if holder.sessionId == selectedSessionID {
                return "GUI \(holder.surface) · gen \(holder.leaseGeneration)"
            }
            let agent = holder.agentName.isEmpty ? holder.sessionId.prefix(8) : Substring(holder.agentName)
            return "GUI busy · \(agent)"
        }
        guard let action = guiTimeline.last else { return nil }
        return "GUI \(action.action) · gen \(action.leaseGeneration)"
    }

    func resetIntegrationPolicy() async {
        integrationAutoResolve = false
        integrationMaxAutoAttempts = 1
        integrationMaxTokensText = ""
        await saveIntegrationPolicy()
    }

    func loadApprovalGrants() async {
        guard let projectID = selectedProjectID else {
            approvalGrants = []
            return
        }
        do {
            approvalGrants = try await core.listApprovalGrants(projectID: projectID)
        } catch {
            approvalGrantStatus = error.localizedDescription
        }
    }

    @discardableResult
    func allowApprovalPath(
        _ approval: Approval,
        path: String,
        includeChildren: Bool
    ) async -> Bool {
        guard let projectID = selectedProjectID else { return false }
        approvalGrantBusy = true
        defer { approvalGrantBusy = false }
        do {
            let grant = try await core.createApprovalGrant(
                projectID: projectID,
                tool: approval.tool,
                pathPrefix: path,
                includeChildren: includeChildren
            )
            approvalGrants.insert(grant, at: 0)
            approvalGrantStatus = includeChildren
                ? "Always allowing \(path) and subfolders."
                : "Always allowing \(path)."
            return await resolveApproval(approval.id, approved: true)
        } catch {
            approvalGrantStatus = error.localizedDescription
            return false
        }
    }

    func deleteApprovalGrant(_ grant: ApprovalGrant) async {
        approvalGrantBusy = true
        defer { approvalGrantBusy = false }
        do {
            _ = try await core.deleteApprovalGrant(id: grant.id)
            approvalGrants.removeAll { $0.id == grant.id }
        } catch {
            approvalGrantStatus = error.localizedDescription
        }
    }

    func resolveApproval(_ id: String, approved: Bool) async -> Bool {
        do {
            let resolved = try await core.resolveApproval(id: id, approved: approved)
            if resolved {
                pendingApprovals.removeAll { $0.id == id }
            }
            return resolved
        } catch {
            handle(error)
            return false
        }
    }

    private var orgCaps: (managers: Int, workers: Int, labour: Int) {
        (orgManagers, orgWorkers, orgLabour)
    }

    func loadOrgCaps() {
        let defaults = UserDefaults.standard
        orgManagers = (defaults.object(forKey: "verlinpark.org.managers") as? Int ?? 4)
            .clamped(to: 1 ... 5)
        orgWorkers = (defaults.object(forKey: "verlinpark.org.workers") as? Int ?? 6)
            .clamped(to: 1 ... 10)
        orgLabour = (defaults.object(forKey: "verlinpark.org.labour") as? Int ?? 12)
            .clamped(to: 1 ... 40)
        autoExpandWorkers = defaults.bool(forKey: "verlinpark.org.autoExpandWorkers")
    }

    func saveOrgCaps() async {
        orgManagers = orgManagers.clamped(to: 1 ... 5)
        orgWorkers = orgWorkers.clamped(to: 1 ... 10)
        orgLabour = orgLabour.clamped(to: 1 ... 40)
        let defaults = UserDefaults.standard
        defaults.set(orgManagers, forKey: "verlinpark.org.managers")
        defaults.set(orgWorkers, forKey: "verlinpark.org.workers")
        defaults.set(orgLabour, forKey: "verlinpark.org.labour")
        defaults.set(autoExpandWorkers, forKey: "verlinpark.org.autoExpandWorkers")
        guard agentInfo?.configured == true else {
            providerStatus = "Team capacity saved. Configure a provider to apply it."
            return
        }
        await saveProviderSettings()
    }

    func saveProviderSettings() async {
        let baseURL = providerBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        let model = providerModel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !model.isEmpty else {
            providerStatus = "Enter a model name."
            return
        }
        guard !baseURL.isEmpty else {
            providerStatus = "Enter a base URL."
            return
        }
        providerBusy = true
        defer { providerBusy = false }
        do {
            let caps = orgCaps
            let apiKey = resolvedProviderAPIKey()
            let info = try await core.configureAgent(
                kind: providerKind,
                baseURL: baseURL,
                model: model,
                roleModels: roleModels,
                apiKey: apiKey,
                maxManagers: caps.managers,
                maxWorkers: caps.workers,
                maxLabour: caps.labour,
                autoExpandWorkers: autoExpandWorkers,
                contextWindow: parsedContextWindow,
                approvalMode: approvalMode.rawValue
            )
            agentInfo = info
            roleModels = info.roleModels
            let defaults = UserDefaults.standard
            defaults.set(orgManagers, forKey: "verlinpark.org.managers")
            defaults.set(orgWorkers, forKey: "verlinpark.org.workers")
            defaults.set(orgLabour, forKey: "verlinpark.org.labour")
            defaults.set(autoExpandWorkers, forKey: "verlinpark.org.autoExpandWorkers")
            UserDefaults.standard.set(providerKind, forKey: "provider.kind")
            UserDefaults.standard.set(baseURL, forKey: "provider.baseURL")
            UserDefaults.standard.set(model, forKey: "provider.model")
            UserDefaults.standard.set(providerContextWindowText, forKey: "provider.contextWindow")
            if let data = try? JSONEncoder().encode(roleModels) {
                UserDefaults.standard.set(data, forKey: "provider.roleModels")
            }
            if !providerAPIKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                try Keychain.save(providerAPIKey, account: "provider.apiKey")
                providerHasStoredKey = true
                UserDefaults.standard.set(true, forKey: "provider.hasAPIKey")
                providerAPIKey = ""
            }
            providerStatus = "Connected · \(model)"
            showToast("Provider connected", detail: model, kind: .success)
        } catch {
            providerStatus = error.localizedDescription
        }
    }

    private var providerNeedsAPIKey: Bool {
        let lower = providerBaseURL.lowercased()
        return !(lower.contains("127.0.0.1")
            || lower.contains("localhost")
            || lower.contains("[::1]")
            || lower.contains("0.0.0.0"))
    }

    private func resolvedProviderAPIKey() -> String {
        let typed = providerAPIKey.trimmingCharacters(in: .whitespacesAndNewlines)
        if !typed.isEmpty {
            return typed
        }
        guard providerHasStoredKey, providerNeedsAPIKey else { return "" }
        return Keychain.read(account: "provider.apiKey") ?? ""
    }

    func clearStoredProviderKey() {
        Keychain.delete(account: "provider.apiKey")
        providerAPIKey = ""
        providerHasStoredKey = false
        UserDefaults.standard.set(false, forKey: "provider.hasAPIKey")
        providerStatus = "Stored API key removed."
    }

    private func configureStoredProvider() async {
        let baseURL = providerBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        let model = providerModel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !baseURL.isEmpty, !model.isEmpty else { return }
        do {
            let caps = orgCaps
            _ = try await core.configureAgent(
                kind: providerKind,
                baseURL: baseURL,
                model: model,
                roleModels: roleModels,
                apiKey: resolvedProviderAPIKey(),
                maxManagers: caps.managers,
                maxWorkers: caps.workers,
                maxLabour: caps.labour,
                autoExpandWorkers: autoExpandWorkers,
                contextWindow: parsedContextWindow,
                approvalMode: approvalMode.rawValue
            )
        } catch {
            providerStatus = error.localizedDescription
        }
    }

    func applyOrgSettings() async {
        await configureStoredProvider()
    }

    func applyApprovalMode() async {
        approvalMode.store()
        await configureStoredProvider()
        showToast(
            "Permission policy updated",
            detail: approvalMode.title,
            kind: .success
        )
    }

    func commitSession(message: String) async {
        guard let sessionID = selectedSessionID else { return }
        let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            lastError = "Enter a commit message."
            return
        }
        commitBusy = true
        defer { commitBusy = false }
        do {
            lastCommit = try await core.commitSession(sessionID: sessionID, message: trimmed)
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    func discardChanges() async {
        guard let sessionID = selectedSessionID else { return }
        commitBusy = true
        defer { commitBusy = false }
        do {
            try await core.discardSession(sessionID: sessionID)
            lastCommit = nil
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    func loadTodos() async {
        guard let sessionID = selectedSessionID else {
            todos = []
            return
        }
        let generation = selectionGeneration
        do {
            let items = try await core.sessionTodos(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                todos = items
            }
        } catch {
            // The event stream will retry on the next todo.updated.
        }
    }

    func loadTeamPlan() async {
        guard let sessionID = selectedSessionID else {
            teamPlan = nil
            return
        }
        let generation = selectionGeneration
        do {
            let plan = try await core.getTeamPlan(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                teamPlan = plan
            }
        } catch {
            // The event stream will retry on team_plan.submitted.
        }
    }

    func loadScheduledSessionTasks() async {
        guard let sessionID = selectedSessionID else {
            scheduledSessionTasks = []
            scheduledSessionTasksSessionID = nil
            return
        }
        let generation = selectionGeneration
        do {
            let result = try await core.taskQueue(projectID: nil, sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            scheduledSessionTasks = result.tasks
            scheduledSessionTasksSessionID = sessionID
        } catch {
            // Event-driven plan updates still keep the tree usable; the next
            // plan or task event retries this snapshot.
        }
    }

    func loadTaskQueue() async {
        taskQueueBusy = true
        defer { taskQueueBusy = false }
        do {
            taskQueue = try await core.taskQueue(projectID: selectedProjectID, sessionID: nil)
            taskQueueStatus = nil
        } catch {
            taskQueueStatus = error.localizedDescription
        }
    }

    func loadOrchestrationPolicy() async {
        guard let projectID = selectedProjectID else {
            orchestrationPolicy = .standard
            orchestrationPolicyStatus = nil
            return
        }
        orchestrationPolicyBusy = true
        defer { orchestrationPolicyBusy = false }
        do {
            let policy = try await core.orchestrationPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            orchestrationPolicy = policy
            orchestrationPolicyStatus = nil
        } catch {
            orchestrationPolicyStatus = error.localizedDescription
        }
    }

    @discardableResult
    func saveOrchestrationPolicy(_ policy: OrchestrationPolicy) async -> Bool {
        guard let projectID = selectedProjectID else {
            orchestrationPolicyStatus = "Select a project first."
            return false
        }
        orchestrationPolicyBusy = true
        defer { orchestrationPolicyBusy = false }
        do {
            let saved = try await core.setOrchestrationPolicy(projectID: projectID, policy: policy)
            guard projectID == selectedProjectID else { return false }
            orchestrationPolicy = saved
            orchestrationPolicyStatus = "Orchestration policy saved."
            return true
        } catch {
            orchestrationPolicyStatus = error.localizedDescription
            return false
        }
    }

    func loadOrchestrationSnapshot() async {
        guard let sessionID = selectedSessionID else {
            orchestrationSnapshot = nil
            return
        }
        let generation = selectionGeneration
        do {
            let snapshot = try await core.orchestrationSnapshot(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            orchestrationSnapshot = snapshot
            if snapshot.policy != orchestrationPolicy {
                orchestrationPolicy = snapshot.policy
            }
        } catch {
            // The graph still uses events and task rows if the snapshot fails;
            // the next plan/task event retries it.
        }
    }

    private func refreshOrchestrationSnapshotSoon() {
        orchestrationRefreshTask?.cancel()
        orchestrationRefreshTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(180))
            guard !Task.isCancelled else { return }
            await self?.loadOrchestrationSnapshot()
        }
    }

    // MARK: - Long-task operator controls

    func loadSteering() async {
        guard let sessionID = selectedSessionID else {
            steeringMessages = []
            return
        }
        await loadSteering(sessionID: sessionID, generation: selectionGeneration)
    }

    private func loadSteering(sessionID: String, generation: Int) async {
        do {
            let result = try await core.sessionSteering(
                sessionID: sessionID,
                status: nil,
                limit: 50
            )
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            steeringMessages = result.steering
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            longTaskStatus = error.localizedDescription
        }
    }

    func loadContinuations() async {
        guard let sessionID = selectedSessionID else {
            continuations = []
            return
        }
        await loadContinuations(sessionID: sessionID, generation: selectionGeneration)
    }

    private func loadContinuations(sessionID: String, generation: Int) async {
        do {
            let result = try await core.sessionContinuations(
                sessionID: sessionID,
                status: nil,
                limit: 50
            )
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            continuations = result.continuations
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            longTaskStatus = error.localizedDescription
        }
    }

    func loadRecoveries() async {
        guard let sessionID = selectedSessionID else {
            recoveries = []
            return
        }
        await loadRecoveries(sessionID: sessionID, generation: selectionGeneration)
    }

    private func loadRecoveries(sessionID: String, generation: Int) async {
        do {
            let result = try await core.orchestrationRecoveries(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            recoveries = result.recoveries
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            longTaskStatus = error.localizedDescription
        }
    }

    func loadStalls() async {
        guard let sessionID = selectedSessionID else {
            taskProgress = []
            taskStalls = []
            stallThresholdSecs = 0
            return
        }
        await loadStalls(sessionID: sessionID, generation: selectionGeneration)
    }

    private func loadStalls(sessionID: String, generation: Int) async {
        do {
            let result = try await core.sessionStalls(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            taskProgress = result.progress
            taskStalls = result.stalls
            stallThresholdSecs = result.thresholdSecs
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            longTaskStatus = error.localizedDescription
        }
    }

    private func loadPreemption(sessionID: String, generation: Int) async {
        do {
            let result = try await core.sessionPreemption(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionPreemption = result
            preemptionPolicyDraft = result.policy
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            // Preemption history is diagnostics, not a reason to blank the stage.
            preemptionStatus = error.localizedDescription
        }
        if let project = selectedProject,
           let overview = try? await core.preemptionOverview(projectID: project.id)
        {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            preemptionOverview = overview
        }
    }

    func loadPreemption() async {
        guard let sessionID = selectedSessionID else {
            sessionPreemption = nil
            return
        }
        await loadPreemption(sessionID: sessionID, generation: selectionGeneration)
    }

    @discardableResult
    func savePreemptionPolicy() async -> String? {
        guard let project = selectedProject else {
            let message = L10n.t("Select a project first.")
            preemptionPolicyStatus = message
            return message
        }
        guard !preemptionPolicyBusy else { return nil }
        preemptionPolicyBusy = true
        defer { preemptionPolicyBusy = false }
        do {
            let saved = try await core.setPreemptionPolicy(
                projectID: project.id,
                policy: preemptionPolicyDraft
            )
            preemptionPolicyDraft = saved
            if let sessionID = selectedSessionID {
                await loadPreemption(sessionID: sessionID, generation: selectionGeneration)
            }
            preemptionPolicyStatus = L10n.t("Preemption policy saved.")
            return nil
        } catch {
            let message = error.localizedDescription
            preemptionPolicyStatus = message
            return message
        }
    }

    @discardableResult
    func preemptActiveRun(reason: String? = nil) async -> String? {
        guard let sessionID = selectedSessionID else {
            let message = L10n.t("Select a session first.")
            preemptionStatus = message
            return message
        }
        guard !preemptionBusy else { return nil }
        let generation = selectionGeneration
        preemptionBusy = true
        defer { preemptionBusy = false }
        do {
            let result = try await core.preemptSession(sessionID: sessionID, reason: reason)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return nil }
            preemptionStatus = result.detail
            await loadPreemption(sessionID: sessionID, generation: generation)
            await loadSessions()
            return result.detail
        } catch {
            let message = error.localizedDescription
            if isCurrent(sessionID: sessionID, generation: generation) {
                preemptionStatus = message
            }
            return message
        }
    }

    func refreshRecoveries() async {
        await loadRecoveries()
    }

    func refreshStalls() async {
        await loadStalls()
    }

    func refreshLongTaskState() async {
        guard let sessionID = selectedSessionID else {
            steeringMessages = []
            continuations = []
            recoveries = []
            taskProgress = []
            taskStalls = []
            stallThresholdSecs = 0
            return
        }
        let generation = selectionGeneration
        longTaskBusy = true
        defer {
            if generation == selectionGeneration {
                longTaskBusy = false
            }
        }
        await loadSteering(sessionID: sessionID, generation: generation)
        await loadContinuations(sessionID: sessionID, generation: generation)
        await loadRecoveries(sessionID: sessionID, generation: generation)
        await loadStalls(sessionID: sessionID, generation: generation)
        await loadPreemption(sessionID: sessionID, generation: generation)
    }

    private func refreshLongTaskStateSoon() {
        longTaskRefreshTask?.cancel()
        longTaskRefreshTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            await self?.refreshLongTaskState()
        }
    }

    @discardableResult
    func steerSession(body: String, agentID: String?, taskID: String?) async -> String? {
        guard let sessionID = selectedSessionID else {
            let message = L10n.t("Select a session first.")
            longTaskStatus = message
            return message
        }
        let trimmed: String
        switch LongTaskRules.validateSteeringBody(body) {
        case let .success(value):
            trimmed = value
        case let .failure(error):
            longTaskStatus = error.message
            return error.message
        }
        guard !steeringSubmitBusy else { return nil }
        let generation = selectionGeneration
        steeringSubmitBusy = true
        defer { steeringSubmitBusy = false }
        do {
            _ = try await core.steerSession(
                sessionID: sessionID,
                body: trimmed,
                agentID: agentID,
                taskID: taskID
            )
            guard isCurrent(sessionID: sessionID, generation: generation) else { return nil }
            await loadSteering(sessionID: sessionID, generation: generation)
            longTaskStatus =
                L10n.t("Steering queued. It is delivered at the target run's next provider boundary.")
            return nil
        } catch {
            let message = error.localizedDescription
            if isCurrent(sessionID: sessionID, generation: generation) {
                longTaskStatus = message
            }
            return message
        }
    }

    func cancelSteering(id: String) async {
        guard let sessionID = selectedSessionID else { return }
        let generation = selectionGeneration
        steeringCancelBusyID = id
        defer {
            if generation == selectionGeneration {
                steeringCancelBusyID = nil
            }
        }
        do {
            _ = try await core.cancelSteering(id: id)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            await loadSteering(sessionID: sessionID, generation: generation)
            longTaskStatus = L10n.t("Steering cancelled.")
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                longTaskStatus = error.localizedDescription
            }
        }
    }

    @discardableResult
    func deferContinuation(
        body: String,
        watchTaskIDs: [String],
        watchMode: String,
        afterSecs: UInt64?,
        deadlineAt: String?
    ) async -> String? {
        guard let sessionID = selectedSessionID else {
            let message = L10n.t("Select a session first.")
            longTaskStatus = message
            return message
        }
        let trimmed: String
        switch LongTaskRules.validateSteeringBody(body) {
        case let .success(value):
            trimmed = value
        case let .failure(error):
            longTaskStatus = error.message
            return error.message
        }
        guard !watchTaskIDs.isEmpty || afterSecs != nil || deadlineAt != nil else {
            let error = LongTaskValidationError.emptyWakeCondition
            longTaskStatus = error.message
            return error.message
        }
        let mode: String
        switch LongTaskRules.normalizeWatchMode(watchMode) {
        case let .success(value):
            mode = value
        case let .failure(error):
            longTaskStatus = error.message
            return error.message
        }
        guard !continuationSubmitBusy else { return nil }
        let generation = selectionGeneration
        continuationSubmitBusy = true
        defer { continuationSubmitBusy = false }
        do {
            _ = try await core.deferSession(
                sessionID: sessionID,
                body: trimmed,
                watchTaskIDs: watchTaskIDs,
                watchMode: mode,
                afterSecs: afterSecs,
                deadlineAt: deadlineAt
            )
            guard isCurrent(sessionID: sessionID, generation: generation) else { return nil }
            await loadContinuations(sessionID: sessionID, generation: generation)
            longTaskStatus = L10n.t(
                "Continuation created. The daemon starts it only after the watch condition or deadline, and only when the session is idle."
            )
            return nil
        } catch {
            let message = error.localizedDescription
            if isCurrent(sessionID: sessionID, generation: generation) {
                longTaskStatus = message
            }
            return message
        }
    }

    func cancelContinuation(id: String) async {
        guard let sessionID = selectedSessionID else { return }
        let generation = selectionGeneration
        continuationActionBusyID = id
        defer {
            if generation == selectionGeneration {
                continuationActionBusyID = nil
            }
        }
        do {
            _ = try await core.cancelContinuation(id: id)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            await loadContinuations(sessionID: sessionID, generation: generation)
            longTaskStatus = L10n.t("Continuation cancelled.")
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                longTaskStatus = error.localizedDescription
            }
        }
    }

    func loadOrchestrationOverview() async {
        orchestrationOverviewBusy = true
        defer { orchestrationOverviewBusy = false }
        do {
            orchestrationOverview = try await core.orchestrationOverview(
                projectID: selectedProjectID,
                limit: 20
            )
            orchestrationOverviewStatus = nil
        } catch {
            orchestrationOverviewStatus = error.localizedDescription
        }
    }

    func loadRuntimeLeases() async {
        runtimeLeasesBusy = true
        defer { runtimeLeasesBusy = false }
        do {
            let result = try await core.runtimeLeases()
            runtimeLeaseOwnerID = result.runtimeId
            runtimeLeases = result.leases
            runtimeLeasesStatus = nil
        } catch {
            runtimeLeasesStatus = error.localizedDescription
        }
    }

    func sweepRuntimeLeases() async {
        runtimeLeasesBusy = true
        defer { runtimeLeasesBusy = false }
        do {
            let removed = try await core.sweepRuntimeLeases()
            runtimeLeasesStatus = "Removed \(removed) expired lease(s)."
            await loadRuntimeLeases()
        } catch {
            runtimeLeasesStatus = error.localizedDescription
        }
    }

    func previewOrchestration() async {
        guard let sessionID = selectedSessionID else { return }
        orchestrationPreviewBusy = true
        defer { orchestrationPreviewBusy = false }
        do {
            let preview = try await core.previewOrchestration(sessionID: sessionID)
            orchestrationPreview = preview
            orchestrationPreviewStatus = preview.actions.isEmpty
                ? "No orchestration action is currently needed."
                : "\(preview.actions.count) action(s) would be applied."
        } catch {
            orchestrationPreviewStatus = error.localizedDescription
        }
    }

    @discardableResult
    func advanceOrchestration() async -> Bool {
        guard let sessionID = selectedSessionID else { return false }
        do {
            let actions = try await core.advanceOrchestration(sessionID: sessionID)
            await loadScheduledSessionTasks()
            await loadOrchestrationSnapshot()
            collabTaskActionStatus = actions.isEmpty
                ? "No orchestration action was needed."
                : "Applied \(actions.count) orchestration action(s)."
            return true
        } catch {
            collabTaskActionStatus = error.localizedDescription
            return false
        }
    }

    func dispatchNextOrchestrationTask() async {
        guard let sessionID = selectedSessionID else { return }
        collabTaskActionBusyID = "__dispatch__"
        defer { collabTaskActionBusyID = nil }
        do {
            let started = try await core.dispatchTask(sessionID: sessionID)
            collabTaskActionStatus = started
                ? "Next ready plan task dispatched."
                : "Session is already running."
            await loadScheduledSessionTasks()
            await loadOrchestrationSnapshot()
        } catch {
            collabTaskActionStatus = error.localizedDescription
        }
    }

    func collabTaskAction(_ task: OrchestrationTask, action: CollabTaskCommand) async {
        collabTaskActionBusyID = task.taskId
        defer { collabTaskActionBusyID = nil }
        do {
            let updated: SchedulerTask = switch action {
            case .retry:
                try await core.retryTask(sessionID: selectedSessionID ?? "", taskID: task.taskId)
            case .skip:
                try await core.skipTask(sessionID: selectedSessionID ?? "", taskID: task.taskId)
            case .pause:
                try await core.pauseTask(sessionID: selectedSessionID ?? "", taskID: task.taskId)
            case .resume:
                try await core.resumeTask(sessionID: selectedSessionID ?? "", taskID: task.taskId)
            case .cancel:
                try await core.cancelTask(sessionID: selectedSessionID ?? "", taskID: task.taskId)
            }
            collabTaskActionStatus = "\(updated.taskID) -> \(updated.status)"
            await loadScheduledSessionTasks()
            await loadOrchestrationSnapshot()
        } catch {
            collabTaskActionStatus = error.localizedDescription
        }
    }

    @discardableResult
    func addCoordinationNote(
        action: String,
        taskID: String?,
        targetAgentID: String?,
        targetNoteID: String? = nil,
        message: String,
        severity: String
    ) async -> Bool {
        guard let sessionID = selectedSessionID else { return false }
        let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            collabTaskActionStatus = "Coordination message must not be empty."
            return false
        }
        do {
            _ = try await core.coordinationNote(
                sessionID: sessionID,
                action: action,
                taskID: taskID,
                targetAgentID: targetAgentID,
                targetNoteID: targetNoteID,
                message: trimmed,
                severity: severity
            )
            await loadOrchestrationSnapshot()
            collabTaskActionStatus = "Coordination note recorded."
            return true
        } catch {
            collabTaskActionStatus = error.localizedDescription
            return false
        }
    }

    @discardableResult
    func resolveOrchestrationBlocker(_ blocker: OrchestrationBlocker) async -> Bool {
        guard blocker.taskId != nil || blocker.noteId != nil else {
            collabTaskActionStatus = "This blocker has no task or note to resolve."
            return false
        }
        return await addCoordinationNote(
            action: "unblock",
            taskID: blocker.taskId,
            targetAgentID: nil,
            targetNoteID: blocker.noteId,
            message: "Resolved from Collab: \(blocker.message)",
            severity: "info"
        )
    }

    func loadPlanRevisionDetail(_ revision: Int) async {
        guard let sessionID = selectedSessionID else { return }
        planRevisionDetailBusy = true
        planRevisionDetailStatus = nil
        defer { planRevisionDetailBusy = false }
        do {
            planRevisionDetail = try await core.planRevisionDetail(
                sessionID: sessionID,
                revision: revision
            )
        } catch {
            planRevisionDetailStatus = error.localizedDescription
        }
    }

    @discardableResult
    func restorePlanRevision(
        _ revision: Int,
        reason: String,
        expectedRevision: Int
    ) async -> Bool {
        guard let sessionID = selectedSessionID else { return false }
        let trimmed = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            planRestoreStatus = "A restore reason is required."
            return false
        }
        planRestoreBusy = true
        defer { planRestoreBusy = false }
        do {
            let result = try await core.restoreTeamPlanRevision(
                sessionID: sessionID,
                revision: revision,
                reason: trimmed,
                expectedRevision: expectedRevision
            )
            planRestoreStatus = "Restored r\(result.revision.revision)."
            await loadTeamPlan()
            await loadScheduledSessionTasks()
            await loadOrchestrationSnapshot()
            planRevisionDetail = try? await core.planRevisionDetail(
                sessionID: sessionID,
                revision: result.revision.revision
            )
            return true
        } catch {
            planRestoreStatus = error.localizedDescription
            return false
        }
    }

    @discardableResult
    func editPlanTask(_ task: PlanTask, reason: String) async -> Bool {
        guard let sessionID = selectedSessionID else { return false }
        let trimmed = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            planRevisionStatus = "An edit reason is required."
            return false
        }
        planRevisionBusy = true
        defer { planRevisionBusy = false }
        do {
            let result = try await core.editPlanTask(
                sessionID: sessionID,
                task: task,
                reason: trimmed,
                expectedRevision: orchestrationSnapshot?.planRevision
            )
            planRevisionStatus = "Task \(task.id) updated in r\(result.revision.revision)."
            await loadTeamPlan()
            await loadScheduledSessionTasks()
            await loadOrchestrationSnapshot()
            return true
        } catch {
            planRevisionStatus = error.localizedDescription
            return false
        }
    }

    @discardableResult
    func reviseSelectedTeamPlan(
        _ plan: TeamPlan,
        reason: String,
        expectedRevision: Int
    ) async -> Bool {
        guard let sessionID = selectedSessionID else { return false }
        let trimmed = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            planRevisionStatus = "A revision reason is required."
            return false
        }
        planRevisionBusy = true
        defer { planRevisionBusy = false }
        do {
            let result = try await core.reviseTeamPlan(
                sessionID: sessionID,
                plan: plan,
                reason: trimmed,
                expectedRevision: expectedRevision
            )
            planRevisionStatus = "Plan revision \(result.revision.revision) recorded."
            await loadTeamPlan()
            await loadScheduledSessionTasks()
            await loadOrchestrationSnapshot()
            return true
        } catch {
            planRevisionStatus = error.localizedDescription
            return false
        }
    }

    func loadTaskSchedulerPolicy() async {
        guard let projectID = selectedProjectID else {
            taskSchedulerPolicy = .disabled
            taskSchedulerEnabled = false
            taskSchedulerDispatchTasks = false
            taskSchedulerIncludeIntake = true
            taskSchedulerMaxIntakeParallelText = "2"
            taskSchedulerCooldownText = "60"
            return
        }
        do {
            let policy = try await core.taskSchedulerPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            taskSchedulerPolicy = policy
            taskSchedulerEnabled = policy.enabled
            taskSchedulerDispatchTasks = policy.dispatchTasks
            taskSchedulerIncludeIntake = policy.includeIntake
            taskSchedulerMaxIntakeParallelText = String(policy.maxIntakeParallel)
            taskSchedulerCooldownText = String(policy.cooldownSecs)
            taskSchedulerPolicyStatus = nil
        } catch {
            taskSchedulerPolicyStatus = error.localizedDescription
        }
    }

    func saveTaskSchedulerPolicy() async {
        guard let projectID = selectedProjectID else {
            taskSchedulerPolicyStatus = "Select a project first."
            return
        }
        let cooldownText = taskSchedulerCooldownText.trimmingCharacters(in: .whitespaces)
        guard let cooldown = UInt64(cooldownText), (1 ... 86400).contains(cooldown) else {
            taskSchedulerPolicyStatus = "Cooldown must be between 1 and 86400 seconds."
            return
        }
        let parallelText = taskSchedulerMaxIntakeParallelText
            .trimmingCharacters(in: .whitespaces)
        guard let parallel = Int(parallelText), (1 ... 10).contains(parallel) else {
            taskSchedulerPolicyStatus = "Intake parallelism must be between 1 and 10."
            return
        }
        taskSchedulerPolicyBusy = true
        defer { taskSchedulerPolicyBusy = false }
        do {
            let policy = try await core.setTaskSchedulerPolicy(
                projectID: projectID,
                policy: TaskSchedulerPolicy(
                    enabled: taskSchedulerEnabled,
                    cooldownSecs: cooldown,
                    dispatchTasks: taskSchedulerDispatchTasks,
                    includeIntake: taskSchedulerIncludeIntake,
                    maxIntakeParallel: parallel
                )
            )
            guard projectID == selectedProjectID else { return }
            taskSchedulerPolicy = policy
            taskSchedulerEnabled = policy.enabled
            taskSchedulerDispatchTasks = policy.dispatchTasks
            taskSchedulerIncludeIntake = policy.includeIntake
            taskSchedulerMaxIntakeParallelText = String(policy.maxIntakeParallel)
            taskSchedulerCooldownText = String(policy.cooldownSecs)
            if policy.enabled {
                let mode = policy.dispatchTasks ? "per-task dispatch" : "full plan resume"
                let intake = policy.includeIntake ? "intake on" : "intake off"
                taskSchedulerPolicyStatus =
                    "Scheduler enabled · \(mode) · \(intake) · cooldown \(policy.cooldownSecs)s"
            } else {
                taskSchedulerPolicyStatus = "Autonomous dispatch disabled"
            }
            await loadTaskQueue()
            await loadIntakeTasks()
            await loadSchedulerStatus()
        } catch {
            taskSchedulerPolicyStatus = error.localizedDescription
        }
    }

    func loadSchedulerStatus(silent: Bool = false) async {
        let projectID = selectedProjectID
        let generation = selectionGeneration
        if !silent {
            schedulerStatusBusy = true
        }
        defer {
            if !silent {
                schedulerStatusBusy = false
            }
        }
        do {
            let status = try await core.schedulerStatus(projectID: projectID)
            guard generation == selectionGeneration, projectID == selectedProjectID else {
                return
            }
            schedulerStatus = status
            schedulerStatusError = nil
        } catch {
            guard generation == selectionGeneration, projectID == selectedProjectID else {
                return
            }
            if !silent {
                schedulerStatusError = error.localizedDescription
            }
        }
    }

    func loadScalingPolicy() async {
        scalingPreview = nil
        guard let projectID = selectedProjectID else {
            scalingPolicy = .disabled
            applyScalingPolicyFields(.disabled)
            return
        }
        do {
            let policy = try await core.scalingPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            scalingPolicy = policy
            applyScalingPolicyFields(policy)
            scalingPolicyStatus = nil
        } catch {
            scalingPolicyStatus = error.localizedDescription
        }
    }

    private func applyScalingPolicyFields(_ policy: ScalingPolicy) {
        scalingEnabled = policy.enabled
        scalingMinWorkersText = String(policy.minWorkers)
        scalingMaxWorkersText = String(policy.maxWorkers)
        scalingMinLabourText = String(policy.minLabour)
        scalingMaxLabourText = String(policy.maxLabour)
        scalingReadyThresholdText = String(policy.expandReadyThreshold)
        scalingCriticalDelayText = String(policy.expandCriticalDelaySecs)
        scalingIdleSecsText = String(policy.scaleDownIdleSecs)
        scalingCooldownText = String(policy.cooldownSecs)
        scalingMaxStepText = String(policy.maxStep)
        scalingBudgetRatioText = policy.budgetHeadroomRatio.map {
            String(format: "%.2f", $0)
        } ?? ""
    }

    func saveScalingPolicy() async {
        guard let projectID = selectedProjectID else {
            scalingPolicyStatus = "Select a project first."
            return
        }
        guard let minWorkers = Int(scalingMinWorkersText.trimmingCharacters(in: .whitespaces)),
              let maxWorkers = Int(scalingMaxWorkersText.trimmingCharacters(in: .whitespaces)),
              let minLabour = Int(scalingMinLabourText.trimmingCharacters(in: .whitespaces)),
              let maxLabour = Int(scalingMaxLabourText.trimmingCharacters(in: .whitespaces)),
              let threshold = Int(scalingReadyThresholdText.trimmingCharacters(in: .whitespaces)),
              let criticalDelay = UInt64(
                  scalingCriticalDelayText.trimmingCharacters(in: .whitespaces)
              ),
              let idleSecs = UInt64(scalingIdleSecsText.trimmingCharacters(in: .whitespaces)),
              let cooldown = UInt64(scalingCooldownText.trimmingCharacters(in: .whitespaces)),
              let maxStep = Int(scalingMaxStepText.trimmingCharacters(in: .whitespaces))
        else {
            scalingPolicyStatus = "All scaling fields except the budget ratio must be integers."
            return
        }
        guard (1 ... 10).contains(minWorkers), (minWorkers ... 10).contains(maxWorkers) else {
            scalingPolicyStatus = "Worker bounds must satisfy 1 ≤ min ≤ max ≤ 10."
            return
        }
        guard (1 ... 40).contains(minLabour), (minLabour ... 40).contains(maxLabour) else {
            scalingPolicyStatus = "Labour bounds must satisfy 1 ≤ min ≤ max ≤ 40."
            return
        }
        guard (1 ... 100).contains(threshold) else {
            scalingPolicyStatus = "Ready threshold must be between 1 and 100."
            return
        }
        guard criticalDelay <= 86400 else {
            scalingPolicyStatus = "Critical delay must be at most 86400 seconds."
            return
        }
        guard idleSecs <= 604_800 else {
            scalingPolicyStatus = "Scale-down idle must be at most 604800 seconds."
            return
        }
        guard cooldown <= 86400 else {
            scalingPolicyStatus = "Cooldown must be at most 86400 seconds."
            return
        }
        guard (1 ... 10).contains(maxStep) else {
            scalingPolicyStatus = "Max step must be between 1 and 10."
            return
        }
        let ratioText = scalingBudgetRatioText.trimmingCharacters(in: .whitespaces)
        let ratio: Double?
        if ratioText.isEmpty {
            ratio = nil
        } else if let parsed = Double(ratioText), (0.0 ... 1.0).contains(parsed) {
            ratio = parsed
        } else {
            scalingPolicyStatus = "Budget headroom ratio must be between 0.0 and 1.0, or empty."
            return
        }
        let policy = ScalingPolicy(
            enabled: scalingEnabled,
            minWorkers: minWorkers,
            maxWorkers: maxWorkers,
            minLabour: minLabour,
            maxLabour: maxLabour,
            expandReadyThreshold: threshold,
            expandCriticalDelaySecs: criticalDelay,
            scaleDownIdleSecs: idleSecs,
            cooldownSecs: cooldown,
            maxStep: maxStep,
            budgetHeadroomRatio: ratio
        )
        scalingPolicyBusy = true
        defer { scalingPolicyBusy = false }
        do {
            let saved = try await core.setScalingPolicy(projectID: projectID, policy: policy)
            guard projectID == selectedProjectID else { return }
            scalingPolicy = saved
            applyScalingPolicyFields(saved)
            scalingPolicyStatus = saved.enabled
                ? "Scaling enabled · Workers \(saved.minWorkers)-\(saved.maxWorkers) · Labour \(saved.minLabour)-\(saved.maxLabour)"
                : "Scaling disabled; no automatic team-size changes"
            await loadScalingStatus()
        } catch {
            scalingPolicyStatus = error.localizedDescription
        }
    }

    func loadScalingStatus(silent: Bool = false) async {
        let projectID = selectedProjectID
        let generation = selectionGeneration
        if !silent {
            scalingStatusBusy = true
        }
        defer {
            if !silent {
                scalingStatusBusy = false
            }
        }
        do {
            let status = try await core.scalingStatus(projectID: projectID)
            guard generation == selectionGeneration, projectID == selectedProjectID else {
                return
            }
            scalingStatus = status
            scalingStatusError = nil
        } catch {
            guard generation == selectionGeneration, projectID == selectedProjectID else {
                return
            }
            if !silent {
                scalingStatusError = error.localizedDescription
            }
        }
    }

    func previewScalingNow() async {
        guard let sessionID = selectedSessionID else {
            scalingPolicyStatus = "Select a session first."
            return
        }
        scalingPreviewBusy = true
        defer { scalingPreviewBusy = false }
        do {
            scalingPreview = try await core.scalingPreview(sessionID: sessionID)
            scalingPolicyStatus = "Preview: \(scalingPreview?.decision.summary ?? "hold")"
        } catch {
            scalingPolicyStatus = error.localizedDescription
        }
    }

    var scalingSelectedSession: ScalingSessionStatus? {
        guard let sessionID = selectedSessionID, let status = scalingStatus else {
            return nil
        }
        return status.projects.flatMap(\.sessions).first { $0.sessionID == sessionID }
    }

    var scalingStageLabel: String? {
        guard let entry = scalingSelectedSession else { return nil }
        let latest = entry.latest?.action
        let label = "team \(entry.currentWorkers)W/\(entry.currentLabour)L · \(entry.readyTasks) ready"
        if let latest, latest != "hold" {
            return "\(label) · \(latest)"
        }
        return label
    }

    func loadSkills(silent: Bool = false) async {
        guard let projectID = selectedProjectID else {
            skills = []
            return
        }
        let generation = selectionGeneration
        if !silent {
            skillsBusy = true
        }
        defer {
            if !silent {
                skillsBusy = false
            }
        }
        do {
            let result = try await core.skills(projectID: projectID, includeDisabled: true)
            guard generation == selectionGeneration, projectID == selectedProjectID else {
                return
            }
            skills = result.skills
            if !silent {
                skillsStatus = nil
            }
        } catch {
            guard generation == selectionGeneration, projectID == selectedProjectID else {
                return
            }
            if !silent {
                skillsStatus = error.localizedDescription
            }
        }
    }

    func createSkill(
        name: String,
        description: String,
        whenToUse: String,
        version: String,
        instructions: String,
        toolAllow: [String],
        toolDeny: [String],
        profiles: [String],
        roles: [String]
    ) async -> ProjectSkill? {
        guard let projectID = selectedProjectID else {
            skillsStatus = "Select a project first."
            return nil
        }
        skillsBusy = true
        defer { skillsBusy = false }
        do {
            let skill = try await core.createSkill(
                projectID: projectID,
                name: name,
                description: description,
                whenToUse: whenToUse,
                version: version.isEmpty ? nil : version,
                instructions: instructions,
                toolAllow: toolAllow,
                toolDeny: toolDeny,
                profiles: profiles,
                roles: roles
            )
            skillsStatus = "Created \(skill.name)."
            await loadSkills(silent: true)
            return skill
        } catch {
            skillsStatus = error.localizedDescription
            return nil
        }
    }

    func updateSkill(
        id: String,
        name: String,
        description: String,
        whenToUse: String,
        version: String,
        instructions: String,
        toolAllow: [String],
        toolDeny: [String],
        profiles: [String],
        roles: [String]
    ) async -> ProjectSkill? {
        skillsBusy = true
        defer { skillsBusy = false }
        do {
            let skill = try await core.updateSkill(
                id: id,
                name: name,
                description: description,
                whenToUse: whenToUse,
                version: version.isEmpty ? nil : version,
                instructions: instructions,
                toolAllow: toolAllow,
                toolDeny: toolDeny,
                profiles: profiles,
                roles: roles
            )
            skillsStatus = "Updated \(skill.name)."
            await loadSkills(silent: true)
            return skill
        } catch {
            skillsStatus = error.localizedDescription
            return nil
        }
    }

    func loadSkillActivations() async {
        guard let sessionID = selectedSessionID else {
            skillActivations = []
            return
        }
        let generation = selectionGeneration
        skillActivationsBusy = true
        skillActivations = []
        defer { skillActivationsBusy = false }
        do {
            let result = try await core.skillActivations(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            skillActivations = result.activations
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            skillActivations = []
        }
    }

    func importSkill(path: String) async -> ProjectSkill? {
        guard let projectID = selectedProjectID else {
            skillsStatus = "Select a project first."
            return nil
        }
        let path = path.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !path.isEmpty else {
            skillsStatus = "Enter a SKILL.md path relative to the project root."
            return nil
        }
        skillsBusy = true
        defer { skillsBusy = false }
        do {
            let skill = try await core.importSkill(projectID: projectID, path: path)
            skillsStatus = "Imported \(skill.name)."
            await loadSkills(silent: true)
            return skill
        } catch {
            skillsStatus = error.localizedDescription
            return nil
        }
    }

    func setSkillEnabled(_ skill: ProjectSkill, enabled: Bool) async {
        skillActionBusyID = skill.id
        defer { skillActionBusyID = nil }
        do {
            _ = try await core.setSkillEnabled(id: skill.id, enabled: enabled)
            skillsStatus = enabled ? "Enabled \(skill.name)." : "Disabled \(skill.name)."
            await loadSkills(silent: true)
        } catch {
            skillsStatus = error.localizedDescription
        }
    }

    func deleteSkill(_ skill: ProjectSkill) async {
        skillActionBusyID = skill.id
        defer { skillActionBusyID = nil }
        do {
            _ = try await core.deleteSkill(id: skill.id)
            skillsStatus = "Deleted \(skill.name)."
            await loadSkills(silent: true)
        } catch {
            skillsStatus = error.localizedDescription
        }
    }

    func loadSources(silent: Bool = false) async {
        guard let projectID = selectedProjectID else {
            sources = []
            sourceStats = nil
            return
        }
        let generation = selectionGeneration
        if !silent {
            sourcesBusy = true
        }
        defer {
            if !silent {
                sourcesBusy = false
            }
        }
        do {
            async let sourcesTask = core.sources(
                projectID: projectID,
                query: nil,
                status: nil,
                limit: 200
            )
            async let collectionsTask = core.sourceCollections(projectID: projectID)
            async let embeddingStatusTask = try? core.sourceEmbeddingStatus(projectID: projectID)
            async let retrievalPolicyTask = try? core.sourceRetrievalPolicy(projectID: projectID)
            async let indexStatusTask = try? core.sourceIndexStatus(projectID: projectID)
            let (result, collections, embeddingStatus, retrievalPolicy, indexStatus) = try await (
                sourcesTask,
                collectionsTask,
                embeddingStatusTask,
                retrievalPolicyTask,
                indexStatusTask
            )
            guard generation == selectionGeneration, projectID == selectedProjectID else {
                return
            }
            sources = result.sources
            sourceStats = result.stats
            sourceCollections = collections.collections
            sourceEmbeddingStatus = embeddingStatus
            sourceRetrievalPolicy = retrievalPolicy?.policy
            sourceIndexStatus = indexStatus
            if let filter = sourceCollectionFilter,
               !filter.isEmpty,
               !collections.collections.contains(where: { $0.name == filter })
            {
                sourceCollectionFilter = nil
            }
            if !silent {
                sourceStatus = nil
            }
            if let selectedID = sourceSelectedID,
               !result.sources.contains(where: { $0.id == selectedID })
            {
                sourceSelectedID = nil
                sourceSelected = nil
            }
        } catch {
            guard generation == selectionGeneration, projectID == selectedProjectID else {
                return
            }
            if !silent {
                sourceStatus = error.localizedDescription
            }
        }
    }

    func loadSource(_ sourceID: String, chunkNo: UInt32? = nil) async {
        if sourceSelectedID != sourceID {
            sourceSelected = nil
        }
        sourceSelectedID = sourceID
        sourceSelectedChunk = chunkNo
        sourceSelectedBusy = true
        defer { sourceSelectedBusy = false }
        let offset = chunkNo.map { max(0, Int($0) - 4) } ?? 0
        do {
            let result = try await core.sourceGet(
                sourceID: sourceID,
                offset: offset,
                limit: 60
            )
            guard sourceSelectedID == sourceID else { return }
            sourceSelected = result
            sourceSearchError = nil
        } catch {
            guard sourceSelectedID == sourceID else { return }
            sourceStatus = error.localizedDescription
        }
    }

    func searchSourceContents(_ rawQuery: String? = nil) async {
        guard let projectID = selectedProjectID else { return }
        let query = (rawQuery ?? sourceSearchQuery)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        sourceSearchQuery = query
        sourceSearchError = nil
        if query.isEmpty {
            sourceSearchResults = []
            sourceSearchDegraded = false
            sourceSearchEffectiveMode = ""
            sourceSearchFallbackReason = nil
            sourceSearchBusy = false
            return
        }
        sourceSearchBusy = true
        let generation = selectionGeneration
        do {
            let filters = sourceCollectionFilter.map { [$0] } ?? []
            let result = try await core.searchSources(
                projectID: projectID,
                query: query,
                collections: filters,
                limit: 12,
                mode: sourceSearchMode
            )
            guard generation == selectionGeneration,
                  sourceSearchQuery == query
            else { return }
            sourceSearchResults = result.hits
            sourceSearchDegraded = result.degraded
            sourceSearchEffectiveMode = result.mode
            sourceSearchFallbackReason = result.semanticUnavailable
            sourceSearchBusy = false
        } catch {
            guard generation == selectionGeneration,
                  sourceSearchQuery == query
            else { return }
            sourceSearchError = error.localizedDescription
            sourceSearchBusy = false
        }
    }

    /// Reload the bounded source embedding status card. A status failure never
    /// hides the lexical source library; it is surfaced on the card only.
    func loadSourceEmbeddingStatus(silent: Bool = false) async {
        guard let projectID = selectedProjectID else {
            sourceEmbeddingStatus = nil
            return
        }
        if !silent {
            sourceEmbeddingBusy = true
        }
        defer {
            if !silent {
                sourceEmbeddingBusy = false
            }
        }
        do {
            let status = try await core.sourceEmbeddingStatus(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            sourceEmbeddingStatus = status
            if !silent {
                sourceEmbeddingMessage = nil
            }
        } catch {
            guard projectID == selectedProjectID else { return }
            if !silent {
                sourceEmbeddingMessage = error.localizedDescription
            }
        }
    }

    /// Explicit derived-index rebuild. No embedding request is made by this
    /// call; the daemon clears vectors/jobs and queues enabled sources.
    func reindexSourceEmbeddings() async {
        guard let projectID = selectedProjectID else {
            sourceEmbeddingMessage = "Select a project first."
            return
        }
        guard !sourceEmbeddingBusy else { return }
        sourceEmbeddingBusy = true
        defer { sourceEmbeddingBusy = false }
        do {
            let result = try await core.reindexSourceEmbeddings(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            if result.enabled {
                sourceEmbeddingMessage =
                    "Cleared \(result.clearedVectors) vectors and queued \(result.enqueuedSources) sources."
            } else {
                sourceEmbeddingMessage =
                    "Cleared \(result.clearedVectors) derived vectors; the lane is disabled."
            }
            await loadSourceEmbeddingStatus(silent: true)
            await loadSources(silent: true)
        } catch {
            sourceEmbeddingMessage = error.localizedDescription
            handle(error)
        }
    }

    /// Reload retrieval policy and derived-index status. Failures surface on
    /// the retrieval card only and never hide lexical source search.
    func loadSourceRetrievalStatus(silent: Bool = false) async {
        guard let projectID = selectedProjectID else {
            sourceRetrievalPolicy = nil
            sourceIndexStatus = nil
            return
        }
        if !silent {
            sourceRetrievalBusy = true
        }
        defer {
            if !silent {
                sourceRetrievalBusy = false
            }
        }
        async let policyTask = try? core.sourceRetrievalPolicy(projectID: projectID)
        async let statusTask = try? core.sourceIndexStatus(projectID: projectID)
        let (policy, status) = await (policyTask, statusTask)
        guard projectID == selectedProjectID else { return }
        if let policy {
            sourceRetrievalPolicy = policy.policy
        }
        sourceIndexStatus = status
        if !silent {
            sourceRetrievalMessage = nil
        }
    }

    /// Persist retrieval controls. The daemon clamps numeric fields and owns
    /// the derived index; this client never touches stored centroids/vectors.
    func updateSourceRetrievalPolicy(_ policy: SourceRetrievalPolicy) async {
        guard let projectID = selectedProjectID else {
            sourceRetrievalMessage = "Select a project first."
            return
        }
        guard !sourceRetrievalBusy else { return }
        sourceRetrievalBusy = true
        defer { sourceRetrievalBusy = false }
        do {
            let result = try await core.setSourceRetrievalPolicy(
                projectID: projectID,
                policy: policy
            )
            guard projectID == selectedProjectID else { return }
            sourceRetrievalPolicy = result.policy
            sourceRetrievalMessage = result.policy.annEnabled
                ? "ANN enabled; rebuild the derived index if status reports not_built."
                : "ANN disabled; semantic search uses the exact scan."
            await loadSourceRetrievalStatus(silent: true)
            await loadSourceEmbeddingStatus(silent: true)
        } catch {
            sourceRetrievalMessage = error.localizedDescription
            handle(error)
        }
    }

    /// Explicit derived-index rebuild. No embedding request is made.
    func rebuildSourceIndex() async {
        guard let projectID = selectedProjectID else {
            sourceRetrievalMessage = "Select a project first."
            return
        }
        guard !sourceRetrievalBusy else { return }
        sourceRetrievalBusy = true
        defer { sourceRetrievalBusy = false }
        do {
            let result = try await core.rebuildSourceIndex(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            if result.rebuilt {
                sourceRetrievalMessage =
                    "Rebuilt generation \(result.generation ?? 0) over \(result.assignedVectors) vectors."
            } else {
                sourceRetrievalMessage =
                    "Index not built: \(result.reason ?? "unavailable")."
            }
            await loadSourceRetrievalStatus(silent: true)
        } catch {
            sourceRetrievalMessage = error.localizedDescription
            handle(error)
        }
    }

    private var sourceCollection: String? {
        let value = sourceDraftCollection.trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }

    func setSourceCollectionFilter(_ value: String?) async {
        sourceCollectionFilter = value
        if !sourceSearchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            await searchSourceContents(sourceSearchQuery)
        }
    }

    func updateSourceMetadata(
        _ source: ProjectSource,
        title: String,
        collection: String?,
        labels: [String]
    ) async -> Bool {
        sourceActionBusyID = source.id
        defer { sourceActionBusyID = nil }
        do {
            let updated = try await core.updateSourceMetadata(
                sourceID: source.id,
                title: title,
                collection: collection,
                labels: labels
            )
            if let index = sources.firstIndex(where: { $0.id == updated.id }) {
                sources[index] = updated
            }
            if sourceSelectedID == updated.id {
                await loadSource(updated.id, chunkNo: sourceSelectedChunk)
            }
            sourceStatus = "Updated \(updated.title)."
            await loadSources(silent: true)
            return true
        } catch {
            sourceStatus = error.localizedDescription
            return false
        }
    }

    func addSourceNote() async -> Bool {
        guard let projectID = selectedProjectID else {
            sourceStatus = "Select a project first."
            return false
        }
        let title = sourceDraftTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        let content = sourceDraftContent.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty, !content.isEmpty else {
            sourceStatus = "A source note needs a title and content."
            return false
        }
        sourcesBusy = true
        defer { sourcesBusy = false }
        do {
            let source = try await core.addSourceNote(
                projectID: projectID,
                collection: sourceCollection,
                title: title,
                content: content
            )
            sourceDraftTitle = ""
            sourceDraftContent = ""
            sourceStatus = "Indexed \(source.title) (\(source.chunkCount) chunks)."
            await loadSources(silent: true)
            await loadSource(source.id)
            return true
        } catch {
            sourceStatus = error.localizedDescription
            return false
        }
    }

    func addSourceFile() async -> Bool {
        guard let projectID = selectedProjectID else { return false }
        let path = sourceDraftPath.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !path.isEmpty else {
            sourceStatus = "Enter a project-relative file path."
            return false
        }
        sourcesBusy = true
        defer { sourcesBusy = false }
        do {
            let source = try await core.addSourceFile(
                projectID: projectID,
                collection: sourceCollection,
                path: path
            )
            sourceDraftPath = ""
            sourceStatus = "Indexed \(source.title)."
            await loadSources(silent: true)
            await loadSource(source.id)
            return true
        } catch {
            sourceStatus = error.localizedDescription
            return false
        }
    }

    func addSourceURL() async -> Bool {
        guard let projectID = selectedProjectID else { return false }
        let url = sourceDraftURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty else {
            sourceStatus = "Enter a public http(s) URL."
            return false
        }
        sourcesBusy = true
        defer { sourcesBusy = false }
        do {
            let source = try await core.addSourceURL(
                projectID: projectID,
                collection: sourceCollection,
                url: url
            )
            sourceDraftURL = ""
            sourceStatus = "Fetched and indexed \(source.title)."
            await loadSources(silent: true)
            await loadSource(source.id)
            return true
        } catch {
            sourceStatus = error.localizedDescription
            return false
        }
    }

    func importProjectSources() async -> Bool {
        guard let projectID = selectedProjectID else { return false }
        sourcesBusy = true
        defer { sourcesBusy = false }
        do {
            let result = try await core.importSources(
                projectID: projectID,
                collection: sourceCollection,
                path: sourceDraftPath.isEmpty ? nil : sourceDraftPath,
                maxFiles: 100
            )
            sourceStatus = "Imported \(result.imported.count) sources; skipped \(result.skipped)."
            if let first = result.imported.first {
                await loadSource(first.id)
            }
            await loadSources(silent: true)
            return true
        } catch {
            sourceStatus = error.localizedDescription
            return false
        }
    }

    func deleteSource(_ source: ProjectSource) async {
        sourceActionBusyID = source.id
        defer { sourceActionBusyID = nil }
        do {
            _ = try await core.deleteSource(sourceID: source.id)
            if sourceSelectedID == source.id {
                sourceSelectedID = nil
                sourceSelected = nil
            }
            sourceStatus = "Removed \(source.title)."
            await loadSources(silent: true)
        } catch {
            sourceStatus = error.localizedDescription
        }
    }

    func refreshSource(_ source: ProjectSource) async {
        sourceActionBusyID = source.id
        defer { sourceActionBusyID = nil }
        do {
            let result = try await core.refreshSource(sourceID: source.id)
            sourceStatus = "Refreshed \(result.source.title) to revision \(result.source.revision)."
            await loadSources(silent: true)
            await loadSource(source.id)
        } catch {
            sourceStatus = error.localizedDescription
        }
    }

    func reindexSource(_ source: ProjectSource) async {
        sourceActionBusyID = source.id
        defer { sourceActionBusyID = nil }
        do {
            let result = try await core.reindexSource(sourceID: source.id)
            sourceStatus = "Reindexed \(result.source.title) (\(result.chunks) chunks)."
            await loadSources(silent: true)
            await loadSource(source.id)
        } catch {
            sourceStatus = error.localizedDescription
        }
    }

    func loadIntakeTasks(silent: Bool = false) async {
        guard let projectID = selectedProjectID else {
            intakeTasks = []
            return
        }
        let generation = selectionGeneration
        if !silent {
            intakeBusy = true
        }
        defer {
            if !silent {
                intakeBusy = false
            }
        }
        do {
            let result = try await core.intakeTasks(projectID: projectID, limit: 200)
            guard selectionGeneration == generation, projectID == selectedProjectID else {
                return
            }
            intakeTasks = result.tasks
            if !silent {
                intakeStatus = nil
            }
        } catch {
            guard projectID == selectedProjectID else { return }
            if !silent {
                intakeStatus = error.localizedDescription
            }
        }
    }

    func createIntakeTask() async {
        guard let projectID = selectedProjectID else {
            intakeStatus = "Select a project first."
            return
        }
        let title = intakeDraftTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        let instructions = intakeDraftInstructions
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else {
            intakeStatus = "Give the task a title."
            return
        }
        guard !instructions.isEmpty else {
            intakeStatus = "Describe what the task should accomplish."
            return
        }
        let priorityText = intakeDraftPriorityText.trimmingCharacters(in: .whitespaces)
        guard let priority = Int(priorityText), (-100 ... 100).contains(priority) else {
            intakeStatus = "Priority must be a whole number between -100 and 100."
            return
        }
        let attemptsText = intakeDraftMaxAttemptsText.trimmingCharacters(in: .whitespaces)
        guard let maxAttempts = UInt32(attemptsText), (1 ... 10).contains(maxAttempts) else {
            intakeStatus = "Max attempts must be a whole number between 1 and 10."
            return
        }
        let tokenText = intakeDraftTokenBudget.trimmingCharacters(in: .whitespaces)
        let tokenBudget = tokenText.isEmpty ? nil : UInt64(tokenText)
        if !tokenText.isEmpty, tokenBudget == nil || tokenBudget == 0 {
            intakeStatus = "Token budget must be a positive whole number."
            return
        }
        let timeText = intakeDraftTimeBudget.trimmingCharacters(in: .whitespaces)
        let timeBudget = timeText.isEmpty ? nil : UInt64(timeText)
        if !timeText.isEmpty, timeBudget == nil || timeBudget == 0 {
            intakeStatus = "Time budget must be a positive number of seconds."
            return
        }
        let costText = intakeDraftCostBudget.trimmingCharacters(in: .whitespaces)
        let costBudget = costText.isEmpty ? nil : Double(costText)
        if !costText.isEmpty, costBudget == nil || costBudget! <= 0 {
            intakeStatus = "Cost budget must be a positive number."
            return
        }
        let scope = parseIntakeItems(intakeDraftScope)
        let acceptance = parseIntakeItems(intakeDraftAcceptance)
        let labels = parseIntakeItems(intakeDraftLabels)
        let deps = intakeDraftDeps.sorted()
        intakeBusy = true
        defer { intakeBusy = false }
        do {
            if let editingID = intakeEditingID {
                _ = try await core.updateIntakeTask(
                    id: editingID,
                    title: title,
                    instructions: instructions,
                    profile: intakeDraftProfile,
                    kind: intakeDraftKind.isEmpty ? nil : intakeDraftKind,
                    priority: priority,
                    writeScope: scope,
                    acceptance: acceptance,
                    deps: deps,
                    tokenBudget: tokenBudget,
                    costBudgetUsd: costBudget,
                    timeBudgetSecs: timeBudget,
                    maxAttempts: maxAttempts,
                    labels: labels
                )
                intakeStatus = "Task updated and re-queued."
            } else {
                _ = try await core.createIntakeTask(
                    projectID: projectID,
                    title: title,
                    instructions: instructions,
                    profile: intakeDraftProfile,
                    kind: intakeDraftKind.isEmpty ? nil : intakeDraftKind,
                    priority: priority,
                    writeScope: scope,
                    acceptance: acceptance,
                    deps: deps,
                    tokenBudget: tokenBudget,
                    costBudgetUsd: costBudget,
                    timeBudgetSecs: timeBudget,
                    maxAttempts: maxAttempts,
                    labels: labels
                )
                intakeStatus = "Task queued. The autonomous dispatcher will pick it up."
            }
            clearIntakeDraft()
            await loadIntakeTasks()
        } catch {
            intakeStatus = error.localizedDescription
        }
    }

    func beginIntakeEdit(_ task: IntakeTask) {
        intakeEditingID = task.id
        intakeDraftTitle = task.title
        intakeDraftInstructions = task.instructions
        intakeDraftProfile = task.profile
        intakeDraftKind = task.kind
        intakeDraftPriorityText = String(task.priority)
        intakeDraftScope = task.writeScope.joined(separator: ", ")
        intakeDraftAcceptance = task.acceptance.joined(separator: ", ")
        intakeDraftMaxAttemptsText = String(task.maxAttempts)
        intakeDraftTokenBudget = task.tokenBudget.map(String.init) ?? ""
        intakeDraftCostBudget = task.costBudgetUsd.map { String(format: "%.4f", $0) } ?? ""
        intakeDraftTimeBudget = task.timeBudgetSecs.map(String.init) ?? ""
        intakeDraftLabels = task.labels.joined(separator: ", ")
        intakeDraftDeps = Set(task.deps)
        intakeStatus = "Editing \(task.title). Saving re-queues the task."
    }

    func cancelIntakeEdit() {
        clearIntakeDraft()
        intakeStatus = nil
    }

    private func clearIntakeDraft() {
        intakeEditingID = nil
        intakeDraftTitle = ""
        intakeDraftInstructions = ""
        intakeDraftKind = ""
        intakeDraftPriorityText = "0"
        intakeDraftScope = ""
        intakeDraftAcceptance = ""
        intakeDraftMaxAttemptsText = "2"
        intakeDraftTokenBudget = ""
        intakeDraftCostBudget = ""
        intakeDraftTimeBudget = ""
        intakeDraftLabels = ""
        intakeDraftDeps = []
    }

    func toggleIntakeDependency(_ id: String) {
        if intakeDraftDeps.contains(id) {
            intakeDraftDeps.remove(id)
        } else {
            intakeDraftDeps.insert(id)
        }
    }

    func dispatchIntakeTask(_ task: IntakeTask) async {
        intakeActionBusyID = task.id
        defer { intakeActionBusyID = nil }
        do {
            let result = try await core.dispatchIntakeTask(id: task.id)
            if result.started {
                if let sessionID = result.sessionID {
                    intakeStatus = "Task dispatched in session \(sessionID.prefix(8))."
                } else {
                    intakeStatus = "Task dispatched."
                }
            } else {
                intakeStatus = result.task.lastError ?? "The task could not start."
            }
            await loadIntakeTasks()
            await loadSessions()
        } catch {
            intakeStatus = error.localizedDescription
        }
    }

    func cancelIntakeTask(_ task: IntakeTask) async {
        intakeActionBusyID = task.id
        defer { intakeActionBusyID = nil }
        do {
            _ = try await core.cancelIntakeTask(id: task.id)
            intakeStatus = "Task cancelled."
            await loadIntakeTasks()
            if task.sessionID == selectedSessionID {
                await loadSessions()
            }
        } catch {
            intakeStatus = error.localizedDescription
        }
    }

    func retryIntakeTask(_ task: IntakeTask) async {
        intakeActionBusyID = task.id
        defer { intakeActionBusyID = nil }
        do {
            _ = try await core.retryIntakeTask(id: task.id)
            intakeStatus = "Task queued for another attempt."
            await loadIntakeTasks()
        } catch {
            intakeStatus = error.localizedDescription
        }
    }

    func deleteIntakeTask(_ task: IntakeTask) async {
        intakeActionBusyID = task.id
        defer { intakeActionBusyID = nil }
        do {
            _ = try await core.deleteIntakeTask(id: task.id)
            intakeStatus = "Task removed."
            await loadIntakeTasks()
        } catch {
            intakeStatus = error.localizedDescription
        }
    }

    func openIntakeSession(_ task: IntakeTask) {
        guard let sessionID = task.sessionID else { return }
        selectSession(sessionID)
    }

    func loadPrograms(silent: Bool = false) async {
        guard let projectID = selectedProjectID else {
            programEntries = []
            programDetail = nil
            selectedProgramID = nil
            return
        }
        let generation = selectionGeneration
        if !silent {
            programBusy = true
        }
        defer {
            if !silent {
                programBusy = false
            }
        }
        do {
            let result = try await core.programs(projectID: projectID, limit: 100)
            guard selectionGeneration == generation, projectID == selectedProjectID else {
                return
            }
            programEntries = result.programs
            if let selected = selectedProgramID,
               !result.programs.contains(where: { $0.program.id == selected })
            {
                selectedProgramID = nil
                programDetail = nil
            }
            if !silent {
                programStatus = nil
            }
        } catch {
            guard selectionGeneration == generation, projectID == selectedProjectID else {
                return
            }
            if !silent {
                programStatus = error.localizedDescription
            }
        }
    }

    func loadProgramDetail(silent: Bool = false) async {
        guard let programID = selectedProgramID else {
            programDetail = nil
            return
        }
        let generation = selectionGeneration
        if !silent {
            programDetailBusy = true
        }
        defer {
            if !silent {
                programDetailBusy = false
            }
        }
        do {
            let result = try await core.programStatus(id: programID, eventLimit: 50)
            guard selectionGeneration == generation,
                  programID == selectedProgramID else { return }
            programDetail = result
            if !silent {
                programStatus = nil
            }
        } catch {
            guard selectionGeneration == generation,
                  programID == selectedProgramID else { return }
            if !silent {
                programStatus = error.localizedDescription
            }
        }
    }

    func selectProgram(_ programID: String) async {
        selectedProgramID = programID
        await loadProgramDetail()
    }

    func createProgram() async {
        guard let projectID = selectedProjectID else {
            programStatus = "Select a project first."
            return
        }
        let goal = programDraftGoal.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !goal.isEmpty else {
            programStatus = "Give the program a goal."
            return
        }
        let attemptsText = programDraftMaxAttemptsText.trimmingCharacters(in: .whitespaces)
        guard let maxAttempts = UInt32(attemptsText), (1 ... 5).contains(maxAttempts) else {
            programStatus = "Max attempts must be between 1 and 5."
            return
        }
        let backoffText = programDraftBackoffText.trimmingCharacters(in: .whitespaces)
        guard let backoff = UInt64(backoffText), backoff <= 86400 else {
            programStatus = "Retry backoff must be between 0 and 86400 seconds."
            return
        }
        let parallelText = programDraftParallelText.trimmingCharacters(in: .whitespaces)
        guard let parallel = Int(parallelText), (1 ... 10).contains(parallel) else {
            programStatus = "Parallel milestones must be between 1 and 10."
            return
        }
        let tokenText = programDraftTokenBudget.trimmingCharacters(in: .whitespaces)
        let tokenBudget = tokenText.isEmpty ? nil : UInt64(tokenText)
        if !tokenText.isEmpty, tokenBudget == nil || tokenBudget == 0 {
            programStatus = "Token budget must be a positive whole number."
            return
        }
        let costText = programDraftCostBudget.trimmingCharacters(in: .whitespaces)
        let costBudget = costText.isEmpty ? nil : Double(costText)
        if !costText.isEmpty, costBudget == nil || costBudget! <= 0 {
            programStatus = "Cost budget must be a positive number."
            return
        }
        let policy = ProgramPolicy(
            autoDispatch: programDraftAutoDispatch,
            maxAttempts: maxAttempts,
            retryBackoffSecs: backoff,
            dependencyFailure: programDraftDependencyFailure,
            tokenBudget: nil,
            costBudgetUsd: nil,
            maxParallelMilestones: parallel
        )
        programBusy = true
        defer { programBusy = false }
        do {
            let program = try await core.createProgram(
                projectID: projectID,
                goal: goal,
                policy: policy,
                tokenBudget: tokenBudget,
                costBudgetUsd: costBudget,
                activate: programDraftActivate
            )
            programDraftGoal = ""
            programDraftTokenBudget = ""
            programDraftCostBudget = ""
            selectedProgramID = program.id
            programStatus = programDraftActivate
                ? "Program activated; ready milestones dispatch deterministically."
                : "Program saved as a draft."
            await loadPrograms(silent: true)
            await loadProgramDetail(silent: true)
        } catch {
            programStatus = error.localizedDescription
        }
    }

    func createProgramMilestone() async {
        guard let programID = selectedProgramID else {
            programStatus = "Select a program first."
            return
        }
        let title = programMilestoneDraftTitle
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let objective = programMilestoneDraftObjective
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else {
            programStatus = "Give the milestone a title."
            return
        }
        guard !objective.isEmpty else {
            programStatus = "Describe the milestone objective."
            return
        }
        let acceptance = parseIntakeItems(programMilestoneDraftAcceptance)
        guard !acceptance.isEmpty else {
            programStatus = "Add at least one acceptance criterion."
            return
        }
        let priorityText = programMilestoneDraftPriorityText
            .trimmingCharacters(in: .whitespaces)
        guard let priority = Int(priorityText), (-100 ... 100).contains(priority) else {
            programStatus = "Milestone priority must be between -100 and 100."
            return
        }
        let attemptsText = programMilestoneDraftMaxAttemptsText
            .trimmingCharacters(in: .whitespaces)
        guard let maxAttempts = UInt32(attemptsText), (1 ... 5).contains(maxAttempts) else {
            programStatus = "Milestone max attempts must be between 1 and 5."
            return
        }
        let tokenText = programMilestoneDraftTokenBudget
            .trimmingCharacters(in: .whitespaces)
        let tokenBudget = tokenText.isEmpty ? nil : UInt64(tokenText)
        if !tokenText.isEmpty, tokenBudget == nil || tokenBudget == 0 {
            programStatus = "Milestone token budget must be a positive whole number."
            return
        }
        let costText = programMilestoneDraftCostBudget
            .trimmingCharacters(in: .whitespaces)
        let costBudget = costText.isEmpty ? nil : Double(costText)
        if !costText.isEmpty, costBudget == nil || costBudget! <= 0 {
            programStatus = "Milestone cost budget must be a positive number."
            return
        }
        let scope = parseIntakeItems(programMilestoneDraftScope)
        let deps = programMilestoneDraftDeps.sorted()
        programBusy = true
        defer { programBusy = false }
        do {
            _ = try await core.createProgramMilestone(
                programID: programID,
                key: nil,
                title: title,
                objective: objective,
                acceptance: acceptance,
                deps: deps,
                priority: priority,
                maxAttempts: maxAttempts,
                profile: programMilestoneDraftProfile,
                writeScope: scope,
                tokenBudget: tokenBudget,
                costBudgetUsd: costBudget
            )
            clearProgramMilestoneDraft()
            programStatus = "Milestone added."
            await loadPrograms(silent: true)
            await loadProgramDetail(silent: true)
        } catch {
            programStatus = error.localizedDescription
        }
    }

    func toggleProgramMilestoneDependency(_ id: String) {
        if programMilestoneDraftDeps.contains(id) {
            programMilestoneDraftDeps.remove(id)
        } else {
            programMilestoneDraftDeps.insert(id)
        }
    }

    func clearProgramMilestoneDraft() {
        programMilestoneDraftTitle = ""
        programMilestoneDraftObjective = ""
        programMilestoneDraftAcceptance = ""
        programMilestoneDraftScope = "src"
        programMilestoneDraftPriorityText = "0"
        programMilestoneDraftMaxAttemptsText = "2"
        programMilestoneDraftProfile = "coding"
        programMilestoneDraftTokenBudget = ""
        programMilestoneDraftCostBudget = ""
        programMilestoneDraftDeps = []
    }

    func pauseProgram(_ program: ProjectProgram) async {
        programActionBusyID = program.id
        defer { programActionBusyID = nil }
        do {
            _ = try await core.pauseProgram(id: program.id, reason: "paused from Manage Dash")
            programStatus = "Program paused; in-flight work continues."
            await loadPrograms(silent: true)
            await loadProgramDetail(silent: true)
        } catch {
            programStatus = error.localizedDescription
        }
    }

    func resumeProgram(_ program: ProjectProgram) async {
        programActionBusyID = program.id
        defer { programActionBusyID = nil }
        do {
            _ = try await core.resumeProgram(id: program.id)
            programStatus = "Program resumed."
            await loadPrograms(silent: true)
            await loadProgramDetail(silent: true)
        } catch {
            programStatus = error.localizedDescription
        }
    }

    func cancelProgram(_ program: ProjectProgram) async {
        programActionBusyID = program.id
        defer { programActionBusyID = nil }
        do {
            _ = try await core.cancelProgram(
                id: program.id,
                reason: "cancelled from Manage Dash"
            )
            programStatus = "Program cancelled; all non-terminal milestones were cancelled."
            await loadPrograms(silent: true)
            await loadProgramDetail(silent: true)
        } catch {
            programStatus = error.localizedDescription
        }
    }

    func deleteProgram(_ program: ProjectProgram) async {
        programActionBusyID = program.id
        defer { programActionBusyID = nil }
        do {
            _ = try await core.deleteProgram(id: program.id)
            if selectedProgramID == program.id {
                selectedProgramID = nil
                programDetail = nil
            }
            programStatus = "Program deleted."
            await loadPrograms(silent: true)
        } catch {
            programStatus = error.localizedDescription
        }
    }

    func retryProgramMilestone(_ milestone: ProgramMilestone) async {
        programActionBusyID = milestone.id
        defer { programActionBusyID = nil }
        do {
            _ = try await core.retryProgramMilestone(id: milestone.id)
            programStatus = "Milestone reset for another attempt."
            await loadProgramDetail(silent: true)
            await loadPrograms(silent: true)
        } catch {
            programStatus = error.localizedDescription
        }
    }

    func skipProgramMilestone(_ milestone: ProgramMilestone) async {
        programActionBusyID = milestone.id
        defer { programActionBusyID = nil }
        do {
            _ = try await core.skipProgramMilestone(
                id: milestone.id,
                reason: "skipped from Manage Dash"
            )
            programStatus = "Milestone skipped."
            await loadProgramDetail(silent: true)
            await loadPrograms(silent: true)
        } catch {
            programStatus = error.localizedDescription
        }
    }

    func startProgramMilestone(_ milestone: ProgramMilestone) async {
        programActionBusyID = milestone.id
        defer { programActionBusyID = nil }
        do {
            _ = try await core.startProgramMilestone(id: milestone.id)
            programStatus = "Milestone dispatched through the normal verifier path."
            await loadProgramDetail(silent: true)
            await loadPrograms(silent: true)
            await loadSessions()
        } catch {
            programStatus = error.localizedDescription
        }
    }

    private func parseIntakeItems(_ text: String) -> [String] {
        var items: [String] = []
        for raw in text.split(whereSeparator: { $0 == "," || $0 == "\n" }) {
            let item = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if !item.isEmpty, !items.contains(item) {
                items.append(item)
            }
        }
        return items
    }

    func dispatchScheduledTaskNow() async {
        guard let task = taskQueue?.tasks.first(where: {
            ["pending", "interrupted", "failed", "blocked", "cancelled"].contains($0.status)
        }) else {
            taskActionStatus = "No dispatchable task."
            return
        }
        taskActionBusyID = task.id
        defer { taskActionBusyID = nil }
        do {
            let started = try await core.dispatchTask(sessionID: task.sessionID)
            taskActionStatus = started
                ? "Dispatch queued for \(task.sessionID)"
                : "Session is already running."
            await loadTaskQueue()
        } catch {
            taskActionStatus = error.localizedDescription
            handle(error)
        }
    }

    func retryScheduledTask(_ task: SchedulerTask) async {
        taskActionBusyID = task.id
        defer { taskActionBusyID = nil }
        do {
            let updated = try await core.retryTask(sessionID: task.sessionID, taskID: task.taskID)
            taskActionStatus = "\(updated.taskID) -> \(updated.status)"
            await loadTaskQueue()
        } catch {
            taskActionStatus = error.localizedDescription
        }
    }

    func pauseRunningTask(_ task: SchedulerTask) async {
        taskActionBusyID = task.id
        defer { taskActionBusyID = nil }
        do {
            let updated = try await core.pauseTask(sessionID: task.sessionID, taskID: task.taskID)
            taskActionStatus = "\(updated.taskID) -> \(updated.status)"
            await loadTaskQueue()
        } catch {
            taskActionStatus = error.localizedDescription
        }
    }

    func resumePausedTask(_ task: SchedulerTask) async {
        taskActionBusyID = task.id
        defer { taskActionBusyID = nil }
        do {
            let updated = try await core.resumeTask(sessionID: task.sessionID, taskID: task.taskID)
            taskActionStatus = "\(updated.taskID) -> \(updated.status)"
            await loadTaskQueue()
        } catch {
            taskActionStatus = error.localizedDescription
        }
    }

    func cancelRunningTask(_ task: SchedulerTask) async {
        taskActionBusyID = task.id
        defer { taskActionBusyID = nil }
        do {
            let updated = try await core.cancelTask(sessionID: task.sessionID, taskID: task.taskID)
            taskActionStatus = "\(updated.taskID) -> \(updated.status)"
            await loadTaskQueue()
        } catch {
            taskActionStatus = error.localizedDescription
        }
    }

    func skipScheduledTask(_ task: SchedulerTask) async {
        taskActionBusyID = task.id
        defer { taskActionBusyID = nil }
        do {
            let updated = try await core.skipTask(sessionID: task.sessionID, taskID: task.taskID)
            taskActionStatus = "\(updated.taskID) -> \(updated.status)"
            await loadTaskQueue()
        } catch {
            taskActionStatus = error.localizedDescription
        }
    }

    func artifactArchiveList(
        artifactID: String,
        maxEntries: Int? = nil
    ) async throws -> ArtifactArchiveListResult {
        try await core.artifactArchiveList(
            artifactID: artifactID,
            maxEntries: maxEntries
        )
    }

    func artifactArchiveExtract(
        artifactID: String,
        entries: [String],
        all: Bool,
        maxEntries: Int? = nil,
        includePreviews: Bool = true
    ) async throws -> ArtifactArchiveExtractResult {
        let result = try await core.artifactArchiveExtract(
            artifactID: artifactID,
            entries: entries,
            all: all,
            maxEntries: maxEntries,
            includePreviews: includePreviews
        )
        await loadMailbox()
        return result
    }

    func mediaInfo(artifactID: String) async throws -> MediaInfoResult {
        try await core.mediaInfo(artifactID: artifactID)
    }

    func mediaTranscode(
        artifactID: String,
        format: String,
        maxDurationSecs: Int,
        maxBytes: Int? = nil
    ) async throws -> MediaTranscodeResult {
        let result = try await core.mediaTranscode(
            artifactID: artifactID,
            format: format,
            maxDurationSecs: maxDurationSecs,
            maxBytes: maxBytes,
            approved: true
        )
        await loadMailbox()
        return result
    }

    func artifactPreviewData(_ artifact: Artifact) async -> ArtifactPreviewData {
        // File-backed artifacts are read from the daemon path only; their
        // `content` field is intentionally empty and is not a fallback source.
        let inlineData: Data?
        if artifact.storage == "file" {
            inlineData = nil
        } else {
            do {
                inlineData = try ArtifactInlineContent.bytes(
                    content: artifact.content,
                    dataBase64: artifact.dataBase64
                )
            } catch {
                return ArtifactPreviewData(
                    kind: .other,
                    sourceURL: nil,
                    inlineData: nil,
                    image: nil,
                    text: nil,
                    totalBytes: artifact.sizeBytes,
                    truncated: false,
                    error: error.localizedDescription
                )
            }
        }

        var sourceURL: URL?
        var loadError: String?
        if artifact.storage == "file" {
            if artifact.compression != "zstd",
               let stored = ArtifactLocalFile.storedURL(for: artifact)
            {
                sourceURL = stored
            } else {
                do {
                    let materialized = try await core.materializeArtifact(artifactID: artifact.id)
                    if (materialized.path as NSString).isAbsolutePath {
                        sourceURL = URL(fileURLWithPath: materialized.path)
                    } else {
                        loadError = L10n.t("Preview unavailable.")
                    }
                } catch {
                    if artifact.compression != "zstd",
                       let stored = ArtifactLocalFile.storedURL(for: artifact)
                    {
                        sourceURL = stored
                    } else {
                        loadError = error.localizedDescription
                    }
                }
            }
        }

        let totalBytes = artifactTotalBytes(
            artifact: artifact,
            sourceURL: sourceURL,
            inlineData: inlineData
        )
        let sample = await artifactBytes(
            sourceURL: sourceURL,
            inlineData: inlineData,
            limit: 4096
        )
        let kind = ArtifactPreviewClassifier.classify(
            mimeType: artifact.mimeType,
            fileName: artifact.name,
            contentSample: sample
        )

        switch kind {
        case .image:
            let data = await artifactBytes(
                sourceURL: sourceURL,
                inlineData: inlineData,
                limit: 64 * 1024 * 1024
            )
            if let data, let image = NSImage(data: data) {
                return ArtifactPreviewData(
                    kind: .image,
                    sourceURL: sourceURL,
                    inlineData: inlineData,
                    image: image,
                    text: nil,
                    totalBytes: totalBytes,
                    truncated: false,
                    error: loadError
                )
            }
            if let data,
               let text = String(data: data.prefix(64 * 1024), encoding: .utf8)
            {
                return ArtifactPreviewData(
                    kind: .text,
                    sourceURL: sourceURL,
                    inlineData: inlineData,
                    image: nil,
                    text: text,
                    totalBytes: totalBytes,
                    truncated: totalBytes > 64 * 1024,
                    error: loadError
                )
            }
            return ArtifactPreviewData(
                kind: .binary,
                sourceURL: sourceURL,
                inlineData: inlineData,
                image: nil,
                text: nil,
                totalBytes: totalBytes,
                truncated: false,
                error: loadError
            )
        case .text:
            guard let data = await artifactBytes(
                sourceURL: sourceURL,
                inlineData: inlineData,
                limit: 64 * 1024
            ) else {
                return ArtifactPreviewData(
                    kind: .text,
                    sourceURL: sourceURL,
                    inlineData: inlineData,
                    image: nil,
                    text: nil,
                    totalBytes: totalBytes,
                    truncated: false,
                    error: loadError ?? L10n.t("Preview unavailable.")
                )
            }
            return ArtifactPreviewData(
                kind: .text,
                sourceURL: sourceURL,
                inlineData: inlineData,
                image: nil,
                text: String(decoding: data, as: UTF8.self),
                totalBytes: totalBytes,
                truncated: totalBytes > 64 * 1024 || (totalBytes == 0 && data.count >= 64 * 1024),
                error: loadError
            )
        case .binary:
            return ArtifactPreviewData(
                kind: .binary,
                sourceURL: sourceURL,
                inlineData: inlineData,
                image: nil,
                text: nil,
                totalBytes: totalBytes,
                truncated: false,
                error: loadError
            )
        case .pdf, .video, .audio, .archive, .other:
            let error = loadError
                ?? (sourceURL == nil && inlineData == nil
                    ? L10n.t("Preview unavailable.")
                    : nil)
            return ArtifactPreviewData(
                kind: kind,
                sourceURL: sourceURL,
                inlineData: inlineData,
                image: nil,
                text: nil,
                totalBytes: totalBytes,
                truncated: false,
                error: error
            )
        }
    }

    private func artifactBytes(
        sourceURL: URL?,
        inlineData: Data?,
        limit: Int
    ) async -> Data? {
        if let sourceURL {
            return try? await Task.detached(priority: .userInitiated) {
                try ArtifactFileReader.readPrefix(limit, from: sourceURL)
            }.value
        }
        return inlineData.map { Data($0.prefix(limit)) }
    }

    private func artifactTotalBytes(
        artifact: Artifact,
        sourceURL: URL?,
        inlineData: Data?
    ) -> Int {
        if artifact.sizeBytes > 0 {
            return artifact.sizeBytes
        }
        if let inlineData {
            return inlineData.count
        }
        if let sourceURL,
           let attributes = try? FileManager.default.attributesOfItem(atPath: sourceURL.path),
           let size = attributes[.size] as? NSNumber
        {
            return size.intValue
        }
        return 0
    }

    func loadSessionCitations() async {
        guard let sessionID = selectedSessionID else {
            sessionCitations = []
            return
        }
        let generation = selectionGeneration
        sessionCitationsBusy = true
        sessionCitations = []
        citationAudit = nil
        defer { sessionCitationsBusy = false }
        do {
            async let citationsTask = core.sessionCitations(sessionID: sessionID)
            async let auditTask = core.citationAudit(sessionID: sessionID)
            let (result, auditResult) = try await (citationsTask, auditTask)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionCitations = result.citations
            citationAudit = auditResult.audit
            sessionCitationsError = nil
        } catch {
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionCitationsError = error.localizedDescription
        }
    }

    func loadArtifactDiff(
        artifactID: String,
        fromVersion: Int,
        toVersion: Int? = nil
    ) async {
        artifactDiffBusy = true
        artifactDiff = nil
        artifactDiffError = nil
        defer { artifactDiffBusy = false }
        do {
            artifactDiff = try await core.artifactDiff(
                artifactID: artifactID,
                fromVersion: fromVersion,
                toVersion: toVersion
            )
        } catch {
            artifactDiff = nil
            artifactDiffError = error.localizedDescription
        }
    }

    func loadArtifactVersions(_ artifact: Artifact) async {
        if artifactVersionsArtifactID != artifact.id {
            artifactVersions = []
        }
        artifactVersionsArtifactID = artifact.id
        artifactVersionsBusy = true
        defer { artifactVersionsBusy = false }
        do {
            let result = try await core.artifactVersions(artifactID: artifact.id)
            guard artifactVersionsArtifactID == artifact.id else { return }
            artifactVersions = result.versions
            artifactVersionsError = nil
        } catch {
            guard artifactVersionsArtifactID == artifact.id else { return }
            artifactVersionsError = error.localizedDescription
        }
    }

    func restoreArtifactVersion(_ version: ArtifactVersion) async -> Bool {
        do {
            let artifact = try await core.restoreArtifactVersion(
                artifactID: version.artifactId,
                version: version.version
            )
            if let index = artifacts.firstIndex(where: { $0.id == artifact.id }) {
                artifacts[index] = artifact
            }
            await loadArtifactVersions(artifact)
            return true
        } catch {
            artifactVersionsError = error.localizedDescription
            return false
        }
    }

    func openArtifact(_ artifact: Artifact) async {
        do {
            let url: URL
            if artifact.storage == "file" {
                let materialized = try await core.materializeArtifact(artifactID: artifact.id)
                guard (materialized.path as NSString).isAbsolutePath else {
                    throw ArtifactPreviewError.invalidDaemonPath
                }
                let source = URL(fileURLWithPath: materialized.path)
                if source.pathExtension.isEmpty {
                    url = try ArtifactPreviewCache.writeCached(
                        data: Data(contentsOf: source),
                        artifactID: artifact.id,
                        mimeType: artifact.mimeType
                    )
                } else {
                    url = source
                }
            } else {
                let data = try ArtifactInlineContent.bytes(
                    content: artifact.content,
                    dataBase64: artifact.dataBase64
                )
                url = try ArtifactPreviewCache.writeCached(
                    data: data,
                    artifactID: artifact.id,
                    mimeType: artifact.mimeType
                )
            }
            NSWorkspace.shared.open(url)
        } catch {
            handle(error)
        }
    }

    private func optionalPresetInt(_ text: String) -> Int? {
        let value = text.trimmingCharacters(in: .whitespaces)
        guard !value.isEmpty else { return nil }
        return Int(value)
    }

    private func optionalPresetDouble(_ text: String) -> Double? {
        let value = text.trimmingCharacters(in: .whitespaces)
        guard !value.isEmpty else { return nil }
        return Double(value)
    }

    func loadModelPresets() async {
        modelPresetsBusy = true
        defer { modelPresetsBusy = false }
        do {
            modelPresets = try await core.listModelPresets()
            if let selectedModelPresetID,
               !modelPresets.contains(where: { $0.id == selectedModelPresetID })
            {
                self.selectedModelPresetID = nil
            }
        } catch {
            modelPresetsStatus = error.localizedDescription
        }
    }

    func newModelPresetDraft() {
        selectedModelPresetID = nil
        modelPresetName = ""
        modelPresetProvider = providerKind
        modelPresetModel = providerModel
        modelPresetContextText = providerContextWindowText
        modelPresetInputPriceText = ""
        modelPresetOutputPriceText = ""
        modelPresetNotes = ""
        modelPresetsStatus = nil
    }

    func selectModelPreset(_ preset: ModelPreset) {
        selectedModelPresetID = preset.id
        modelPresetName = preset.name
        modelPresetProvider = preset.provider
        modelPresetModel = preset.model
        modelPresetContextText = preset.contextWindow.map(String.init) ?? ""
        modelPresetInputPriceText = preset.inputUsdPerMillion.map { String($0) } ?? ""
        modelPresetOutputPriceText = preset.outputUsdPerMillion.map { String($0) } ?? ""
        modelPresetNotes = preset.notes
        modelPresetsStatus = nil
    }

    func saveModelPreset() async {
        let name = modelPresetName.trimmingCharacters(in: .whitespacesAndNewlines)
        let model = modelPresetModel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !model.isEmpty else {
            modelPresetsStatus = "Name and model are required."
            return
        }
        let context = optionalPresetInt(modelPresetContextText)
        let input = optionalPresetDouble(modelPresetInputPriceText)
        let output = optionalPresetDouble(modelPresetOutputPriceText)
        modelPresetsBusy = true
        defer { modelPresetsBusy = false }
        do {
            let saved: ModelPreset = if let selectedModelPresetID {
                try await core.updateModelPreset(
                    id: selectedModelPresetID,
                    name: name,
                    provider: modelPresetProvider,
                    model: model,
                    contextWindow: context,
                    inputPrice: input,
                    outputPrice: output,
                    notes: modelPresetNotes
                )
            } else {
                try await core.createModelPreset(
                    name: name,
                    provider: modelPresetProvider,
                    model: model,
                    contextWindow: context,
                    inputPrice: input,
                    outputPrice: output,
                    notes: modelPresetNotes
                )
            }
            selectedModelPresetID = saved.id
            await loadModelPresets()
            selectModelPreset(saved)
            modelPresetsStatus = "Saved model preset."
        } catch {
            modelPresetsStatus = error.localizedDescription
        }
    }

    func deleteModelPreset() async {
        guard let id = selectedModelPresetID else { return }
        modelPresetsBusy = true
        defer { modelPresetsBusy = false }
        do {
            let result = try await core.deleteModelPreset(id: id)
            await loadModelPresets()
            newModelPresetDraft()
            modelPresetsStatus = result.removed ? "Deleted preset." : "Preset was already removed."
        } catch {
            modelPresetsStatus = error.localizedDescription
        }
    }

    func applyModelPreset(_ preset: ModelPreset, to role: String) {
        guard preset.provider == providerKind else {
            modelPresetsStatus =
                "Preset provider \(preset.provider) does not match the configured provider \(providerKind)."
            return
        }
        var config = RoleModelConfig()
        config.model = role == "leader" ? nil : preset.model
        config.contextWindow = preset.contextWindow
        config.inputUsdPerMillion = preset.inputUsdPerMillion
        config.outputUsdPerMillion = preset.outputUsdPerMillion
        if role == "leader" {
            providerModel = preset.model
        }
        switch role {
        case "leader": roleModels.leader = config
        case "manager": roleModels.manager = config
        case "worker": roleModels.worker = config
        case "verifier": roleModels.verifier = config
        case "labour": roleModels.labour = config
        default:
            modelPresetsStatus = "Unknown role: \(role)"
            return
        }
        modelPresetsStatus = "Applied \(preset.name) to \(role). Save provider settings to persist."
    }

    func checkForUpdates() async {
        updateBusy = true
        updateError = nil
        defer { updateBusy = false }
        do {
            updateResult = try await core.checkForUpdates()
        } catch {
            updateError = error.localizedDescription
        }
    }

    func refreshBackupStatus() async {
        backupStatusBusy = true
        backupStatusError = nil
        defer { backupStatusBusy = false }
        do {
            backupStatus = try await core.backupStatus()
        } catch {
            backupStatusError = error.localizedDescription
        }
    }

    func downloadUpdate() async {
        updateDownloadBusy = true
        updateDownloadError = nil
        updateDownloadResult = nil
        defer { updateDownloadBusy = false }
        do {
            updateDownloadResult = try await core.downloadUpdate()
        } catch {
            updateDownloadError = error.localizedDescription
        }
    }

    func loadCheckpointPolicy() async {
        guard let projectID = selectedProjectID else {
            checkpointPolicy = .disabled
            return
        }
        checkpointPolicyBusy = true
        defer { checkpointPolicyBusy = false }
        do {
            let result = try await core.checkpointPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            checkpointPolicy = result.policy
            checkpointPolicyEnabled = result.policy.enabled
            checkpointPolicyOlderDaysText = "\(max(0, result.policy.olderThanSecs / 86400))"
            checkpointPolicyKeepLatestText = "\(result.policy.keepLatest)"
            checkpointPolicyStatus = nil
        } catch {
            checkpointPolicyStatus = error.localizedDescription
        }
    }

    func saveCheckpointPolicy() async {
        guard let projectID = selectedProjectID else {
            checkpointPolicyStatus = "Select a project first."
            return
        }
        let daysText = checkpointPolicyOlderDaysText.trimmingCharacters(in: .whitespaces)
        let keepText = checkpointPolicyKeepLatestText.trimmingCharacters(in: .whitespaces)
        guard let days = Int(daysText), days >= 0,
              let keepLatest = Int(keepText), keepLatest >= 0
        else {
            checkpointPolicyStatus = "Retention must be non-negative whole days and a whole keep count."
            return
        }
        let policy = CheckpointRetentionPolicy(
            enabled: checkpointPolicyEnabled,
            olderThanSecs: max(0, days) * 86400,
            keepLatest: max(0, keepLatest)
        )
        checkpointPolicyBusy = true
        defer { checkpointPolicyBusy = false }
        do {
            let result = try await core.setCheckpointPolicy(projectID: projectID, policy: policy)
            checkpointPolicy = result.policy
            checkpointPolicyStatus = result.policy.enabled
                ? "Automatic checkpoint retention enabled"
                : "Automatic checkpoint retention disabled"
        } catch {
            checkpointPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadWorktreePolicy() async {
        guard let projectID = selectedProjectID else {
            worktreePolicy = .disabled
            return
        }
        worktreePolicyBusy = true
        defer { worktreePolicyBusy = false }
        do {
            let result = try await core.worktreePolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            worktreePolicy = result.policy
            worktreePolicyEnabled = result.policy.enabled
            worktreePolicyOlderDaysText = "\(max(0, result.policy.olderThanSecs / 86400))"
            worktreePolicyDeleteBranches = result.policy.deleteBranches
            worktreePolicyKeepFailed = result.policy.keepFailed
            worktreePolicyStatus = nil
        } catch {
            worktreePolicyStatus = error.localizedDescription
        }
    }

    func saveWorktreePolicy() async {
        guard let projectID = selectedProjectID else {
            worktreePolicyStatus = "Select a project first."
            return
        }
        let daysText = worktreePolicyOlderDaysText.trimmingCharacters(in: .whitespaces)
        guard let days = Int(daysText), days >= 0 else {
            worktreePolicyStatus = "Retention must be a non-negative whole number of days."
            return
        }
        let policy = WorktreeRetentionPolicy(
            enabled: worktreePolicyEnabled,
            olderThanSecs: max(0, days) * 86400,
            deleteBranches: worktreePolicyDeleteBranches,
            keepFailed: worktreePolicyKeepFailed
        )
        worktreePolicyBusy = true
        defer { worktreePolicyBusy = false }
        do {
            let result = try await core.setWorktreePolicy(projectID: projectID, policy: policy)
            worktreePolicy = result.policy
            worktreePolicyStatus = result.policy.enabled
                ? "Automatic cleanup enabled"
                : "Automatic cleanup disabled"
        } catch {
            worktreePolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func runWorktreeCleanup() async {
        guard let projectID = selectedProjectID else {
            cleanupStatus = "Select a project first."
            return
        }
        let daysText = cleanupOlderDaysText.trimmingCharacters(in: .whitespaces)
        guard let days = Int(daysText), days >= 0 else {
            cleanupStatus = "Retention must be a non-negative whole number of days."
            return
        }
        cleanupBusy = true
        defer { cleanupBusy = false }
        do {
            let result = try await core.cleanupWorktrees(
                projectID: projectID,
                olderThanDays: max(0, days),
                dryRun: cleanupDryRun,
                force: false,
                deleteBranches: cleanupDeleteBranches,
                keepFailed: cleanupKeepFailed
            )
            cleanupResult = result
            cleanupStatus =
                "scanned \(result.scanned), removed \(result.removed), skipped \(result.skipped)"
        } catch {
            cleanupStatus = error.localizedDescription
            handle(error)
        }
    }

    func runDoctor() async {
        doctorBusy = true
        defer { doctorBusy = false }
        do {
            doctorReport = try await core.doctor()
            doctorStatus = doctorReport?.ok == true ? "All checks passed" : "Warnings or errors found"
        } catch {
            doctorStatus = error.localizedDescription
            handle(error)
        }
    }

    func probeProvider() async {
        providerProbeBusy = true
        defer { providerProbeBusy = false }
        do {
            let result = try await core.probeProvider()
            let streamDetail = if let chunks = result.chunks, let firstToken = result.firstTokenMs {
                " · \(chunks) chunks · first token \(firstToken) ms"
            } else {
                ""
            }
            providerProbeStatus =
                "\(result.provider) / \(result.model) · \(result.latencyMs) ms\(streamDetail) · \(result.text)"
        } catch {
            providerProbeStatus = error.localizedDescription
            handle(error)
        }
    }

    func refreshAgentInfo() async {
        do {
            let info = try await core.agentInfo()
            agentInfo = info
            adoptApprovalMode(from: info)
            if let contextWindow = info.contextWindow, contextWindow > 0 {
                providerContextWindowText = String(contextWindow)
            }
        } catch {
            handle(error)
        }
    }

    private func adoptApprovalMode(from info: AgentInfo) {
        guard let raw = info.approvalMode,
              let mode = AgentApprovalMode(rawValue: raw)
        else {
            return
        }
        approvalMode = mode
        mode.store()
    }

    func loadAgents() async {
        guard let sessionID = selectedSessionID else {
            agents = []
            return
        }
        let generation = selectionGeneration
        do {
            let records = try await core.listAgents(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                agents = records
            }
        } catch {
            // The event stream will retry on the next agent event.
        }
    }

    /// Read-only project identity roster. There is intentionally no hire,
    /// assign, pause, or retire action in the S1 client.
    func loadAgentIdentities() async {
        guard let projectID = selectedProjectID else {
            agentIdentities = []
            agentIdentitiesStatus = nil
            return
        }
        agentIdentitiesBusy = true
        defer { agentIdentitiesBusy = false }
        do {
            let rows = try await core.agentIdentities(projectID: projectID)
            if projectID == selectedProjectID {
                agentIdentities = rows
                agentIdentitiesStatus = rows.isEmpty
                    ? "No project identities."
                    : "\(rows.count) identities"
            }
        } catch {
            if projectID == selectedProjectID {
                agentIdentitiesStatus = error.localizedDescription
            }
        }
    }

    func loadMailbox() async {
        guard let sessionID = selectedSessionID else {
            mailbox = []
            artifacts = []
            usageLedger = nil
            usageStatus = nil
            return
        }
        let generation = selectionGeneration
        do {
            let messages = try await core.listMailbox(sessionID: sessionID)
            let published = try await core.listArtifacts(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            mailbox = messages
            artifacts = published
        } catch {
            // The event stream will retry on mailbox/artifact events.
        }
    }

    /// Read-only session assignment metadata for the coordination card.
    func loadSessionAssignments() async {
        guard let sessionID = selectedSessionID else {
            sessionAssignments = []
            sessionAssignmentsStatus = nil
            return
        }
        sessionAssignmentsBusy = true
        defer { sessionAssignmentsBusy = false }
        let generation = selectionGeneration
        do {
            let rows = try await core.sessionAssignments(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionAssignments = rows
            sessionAssignmentsStatus = rows.isEmpty
                ? "No identities assigned to this session."
                : "\(rows.count) assignments"
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                sessionAssignmentsStatus = error.localizedDescription
            }
        }
    }

    /// Read-only project workspace metadata for the team card.
    func loadAgentWorkspaces() async {
        guard let projectID = selectedProjectID else {
            agentWorkspaces = []
            agentWorkspacesStatus = nil
            return
        }
        agentWorkspacesBusy = true
        defer { agentWorkspacesBusy = false }
        do {
            let result = try await core.agentWorkspaces(projectID: projectID)
            if projectID == selectedProjectID {
                agentWorkspaces = result.workspaces
                agentWorkspacesStatus = result.workspaces.isEmpty
                    ? "No Worker workspaces."
                    : "\(result.workspaces.count) workspaces"
            }
        } catch {
            if projectID == selectedProjectID {
                agentWorkspacesStatus = error.localizedDescription
            }
        }
    }

    /// Read-only session workspace bindings for the Stage inspector.
    func loadSessionWorkspaces() async {
        guard let sessionID = selectedSessionID else {
            sessionWorkspaces = []
            sessionWorkspacesStatus = nil
            return
        }
        sessionWorkspacesBusy = true
        defer { sessionWorkspacesBusy = false }
        let generation = selectionGeneration
        do {
            let rows = try await core.sessionWorkspaces(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            sessionWorkspaces = rows
            sessionWorkspacesStatus = rows.isEmpty
                ? "No Worker workspaces bound to this session."
                : "\(rows.count) workspace binding(s)"
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                sessionWorkspacesStatus = error.localizedDescription
            }
        }
    }

    /// Read-only project inbox metadata. This never marks a message read; the
    /// CLI/operator owns lifecycle changes.
    func loadCoordinationInbox() async {
        guard let projectID = selectedProjectID else {
            coordinationInbox = []
            coordinationInboxCounts = nil
            coordinationInboxStatus = nil
            return
        }
        coordinationInboxBusy = true
        defer { coordinationInboxBusy = false }
        do {
            let result = try await core.agentInbox(projectID: projectID)
            if projectID == selectedProjectID {
                coordinationInbox = result.messages
                coordinationInboxCounts = result.counts
                coordinationInboxStatus = result.messages.isEmpty
                    ? "No project inbox rows."
                    : "\(result.counts.unread) unread"
            }
        } catch {
            if projectID == selectedProjectID {
                coordinationInboxStatus = error.localizedDescription
            }
        }
    }

    func loadUsage() async {
        guard let sessionID = selectedSessionID else {
            usageLedger = nil
            usageStatus = nil
            return
        }
        let generation = selectionGeneration
        do {
            let ledger = try await core.usageLedger(sessionID: sessionID)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            usageLedger = ledger
            usageStatus = nil
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                usageStatus = error.localizedDescription
            }
        }
    }

    func markMessageRead(_ messageID: String) async {
        do {
            _ = try await core.markMessageRead(messageID: messageID)
            await loadMailbox()
        } catch {
            handle(error)
        }
    }

    func loadMergeQueue() async {
        guard let sessionID = selectedSessionID else {
            mergeQueue = []
            return
        }
        let generation = selectionGeneration
        do {
            let queue = try await core.listMergeQueue(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                mergeQueue = queue
            }
        } catch {
            // The event stream will retry on the next merge queue event.
        }
    }

    func retryMergeEntry(_ entryID: String) async {
        do {
            _ = try await core.retryMerge(entryID: entryID)
            await loadMergeQueue()
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    func resolveMergeEntry(_ entryID: String) async {
        do {
            _ = try await core.resolveMerge(entryID: entryID)
            await loadMergeQueue()
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    func landSession() async {
        guard let sessionID = selectedSessionID else { return }
        landingBusy = true
        defer { landingBusy = false }
        do {
            let landing = try await core.landSession(sessionID: sessionID)
            lastLanding = landing
            await loadSessions()
            await loadDiff()
            await loadMergeQueue()
            showToast(
                "Session landed",
                detail: "\(landing.commit) → \(landing.targetBranch)",
                kind: .success
            )
        } catch {
            handle(error)
        }
    }

    func openPullRequest() async {
        guard let sessionID = selectedSessionID else { return }
        pullRequestBusy = true
        defer { pullRequestBusy = false }
        do {
            let pullRequest = try await core.openPullRequest(sessionID: sessionID)
            lastPullRequest = pullRequest
            await loadSessions()
            let provider = pullRequest.provider.map { "\($0) · " } ?? ""
            showToast(
                "\(pullRequest.change ?? "PR") opened",
                detail: provider + (pullRequest.url ?? pullRequest.branch),
                kind: .success
            )
        } catch {
            handle(error)
        }
    }

    /// Read-only forge inventory for Settings. Never triggers a provider call or
    /// a push; credentials stay in the daemon environment.
    func loadForgeStatus() async {
        forgeStatusBusy = true
        defer { forgeStatusBusy = false }
        do {
            forgeStatus = try await core.forgeStatus(projectID: selectedProjectID)
            forgeStatusError = nil
        } catch {
            forgeStatusError = error.localizedDescription
        }
    }

    /// Explicit read-only provider probe. This can contact the forge, but it
    /// never pushes a branch or creates a pull request.
    func checkForgeCredentials() async {
        guard let projectID = selectedProjectID else {
            forgeCheckError = "Select a project before checking forge credentials."
            return
        }
        forgeCheckBusy = true
        defer { forgeCheckBusy = false }
        do {
            forgeCheckResult = try await core.forgePreflight(projectID: projectID)
            forgeCheckError = nil
        } catch {
            forgeCheckResult = nil
            forgeCheckError = error.localizedDescription
        }
    }

    func loadContextStatus() async {
        guard let sessionID = selectedSessionID else {
            contextStatus = nil
            contextCheckpoints = []
            contextCheckpointDetails = [:]
            contextCheckpointPolicy = nil
            return
        }
        let generation = selectionGeneration
        do {
            let status = try await core.contextStatus(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                contextStatus = status
            }
        } catch {
            // Context status is diagnostic; do not surface a transient RPC error.
        }
        await loadContextCheckpoints()
    }

    func loadContextCheckpoints() async {
        guard let sessionID = selectedSessionID else {
            contextCheckpoints = []
            contextCheckpointDetails = [:]
            contextCheckpointPolicy = nil
            return
        }
        let generation = selectionGeneration
        do {
            let result = try await core.contextCheckpoints(sessionID: sessionID, limit: 50)
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            contextCheckpoints = result.checkpoints
            contextCheckpointPolicy = result.policy
        } catch {
            // Context checkpoints are diagnostic; keep the last successful list.
        }
    }

    func loadContextCheckpointDetail(_ checkpointID: String) async -> ContextCheckpointDetailResult? {
        guard let sessionID = selectedSessionID else { return nil }
        let generation = selectionGeneration
        do {
            let detail = try await core.contextCheckpointDetail(
                sessionID: sessionID,
                checkpointID: checkpointID
            )
            guard isCurrent(sessionID: sessionID, generation: generation) else { return nil }
            contextCheckpointDetails[checkpointID] = detail
            return detail
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                contextCheckpointNotice = error.localizedDescription
            }
            return nil
        }
    }

    func createContextCheckpoint() async {
        guard let sessionID = selectedSessionID else { return }
        contextCheckpointBusy = true
        contextCheckpointNotice = nil
        defer { contextCheckpointBusy = false }
        do {
            let checkpoint = try await core.createContextCheckpoint(
                sessionID: sessionID,
                label: nil
            )
            guard sessionID == selectedSessionID else { return }
            contextCheckpointNotice = "Captured \(checkpoint.mode) · \(checkpoint.label)"
            await loadContextStatus()
            await loadEvents()
        } catch {
            if sessionID == selectedSessionID {
                contextCheckpointNotice = "Checkpoint failed: \(error.localizedDescription)"
            }
            handle(error)
        }
    }

    func restoreContextCheckpoint(_ checkpointID: String) async {
        guard let sessionID = selectedSessionID else { return }
        contextCheckpointBusy = true
        contextCheckpointNotice = nil
        defer { contextCheckpointBusy = false }
        do {
            let detail = try await core.contextCheckpointDetail(
                sessionID: sessionID,
                checkpointID: checkpointID
            )
            guard sessionID == selectedSessionID else { return }
            contextCheckpointDetails[checkpointID] = detail
            if let error = detail.preflight.errors.first {
                contextCheckpointNotice = "Restore refused: \(error)"
                return
            }
            // A threshold warning is advisory: the model still asks for an
            // explicit restore, and the daemon records `forced=true`.
            let force = !detail.preflight.warnings.isEmpty
            let result = try await core.restoreContextCheckpoint(
                sessionID: sessionID,
                checkpointID: checkpointID,
                force: force
            )
            guard sessionID == selectedSessionID else { return }
            let warning = result.warning.map { " · \($0)" } ?? ""
            contextCheckpointNotice =
                "Restored \(result.mode) boundary · \(result.activeMessages) messages\(warning)"
            await loadContextStatus()
            await loadEvents()
        } catch {
            if sessionID == selectedSessionID {
                contextCheckpointNotice = "Restore failed: \(error.localizedDescription)"
            }
            handle(error)
        }
    }

    func deleteContextCheckpoint(_ checkpointID: String) async {
        guard let sessionID = selectedSessionID else { return }
        contextCheckpointBusy = true
        contextCheckpointNotice = nil
        defer { contextCheckpointBusy = false }
        do {
            let result = try await core.deleteContextCheckpoint(
                sessionID: sessionID,
                checkpointID: checkpointID
            )
            guard sessionID == selectedSessionID else { return }
            contextCheckpointNotice = result.removed ? "Checkpoint deleted" : "Checkpoint already gone"
            contextCheckpointDetails.removeValue(forKey: checkpointID)
            await loadContextStatus()
        } catch {
            if sessionID == selectedSessionID {
                contextCheckpointNotice = "Delete failed: \(error.localizedDescription)"
            }
            handle(error)
        }
    }

    func setContextCheckpointRetention(
        enabled: Bool? = nil,
        maxCheckpoints: Int? = nil,
        maxAgeDays: Int? = nil
    ) async {
        guard let projectID = selectedProjectID else { return }
        let current = contextCheckpointPolicy
            ?? ContextCheckpointPolicy(enabled: false, maxCheckpoints: 20, maxAgeDays: 30)
        let next = ContextCheckpointPolicy(
            enabled: enabled ?? current.enabled,
            maxCheckpoints: maxCheckpoints ?? current.maxCheckpoints,
            maxAgeDays: maxAgeDays ?? current.maxAgeDays
        )
        contextCheckpointBusy = true
        contextCheckpointNotice = nil
        defer { contextCheckpointBusy = false }
        do {
            let result = try await core.setContextCheckpointPolicy(
                projectID: projectID,
                policy: next
            )
            guard projectID == selectedProjectID else { return }
            contextCheckpointPolicy = result.policy
            contextCheckpointNotice =
                "Retention: sweep \(result.policy.enabled ? "on" : "off") · max \(result.policy.maxCheckpoints) · age \(result.policy.maxAgeDays)d"
        } catch {
            if projectID == selectedProjectID {
                contextCheckpointNotice = "Retention update failed: \(error.localizedDescription)"
            }
            handle(error)
        }
    }

    func pruneContextCheckpoints(keepLatest: Int = 1) async {
        guard let sessionID = selectedSessionID else { return }
        contextCheckpointBusy = true
        contextCheckpointNotice = nil
        defer { contextCheckpointBusy = false }
        do {
            let result = try await core.pruneContextCheckpoints(
                sessionID: sessionID,
                olderThanDays: 0,
                keepLatest: keepLatest,
                dryRun: false
            )
            guard sessionID == selectedSessionID else { return }
            contextCheckpointNotice = "Pruned \(result.removed) of \(result.candidates)"
            await loadContextStatus()
        } catch {
            if sessionID == selectedSessionID {
                contextCheckpointNotice = "Prune failed: \(error.localizedDescription)"
            }
            handle(error)
        }
    }

    func forkSession(goal: String, checkpointID: String? = nil) async -> Bool {
        guard let sourceSessionID = selectedSessionID else { return false }
        forkBusy = true
        defer { forkBusy = false }
        do {
            let result = try await core.forkSession(
                sessionID: sourceSessionID,
                checkpointID: checkpointID,
                goal: goal
            )
            await loadSessions()
            if selectedSessionID == sourceSessionID {
                selectSession(result.session.id)
            }
            showToast("Session forked", detail: result.session.goal, kind: .success)
            return true
        } catch {
            handle(error)
            return false
        }
    }

    func loadCheckpoints() async {
        guard let sessionID = selectedSessionID else {
            checkpoints = []
            return
        }
        let generation = selectionGeneration
        do {
            let loaded = try await core.listCheckpoints(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                checkpoints = loaded
            }
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                checkpoints = []
            }
        }
    }

    func loadCheckpointStats() async {
        guard let sessionID = selectedSessionID else {
            checkpointStats = nil
            return
        }
        checkpointStatsBusy = true
        defer { checkpointStatsBusy = false }
        do {
            let result = try await core.checkpointStats(sessionID: sessionID)
            if sessionID == selectedSessionID {
                checkpointStats = result
            }
        } catch {
            checkpointStats = nil
        }
    }

    func loadCheckpointDiff(_ checkpointID: String) async {
        guard let sessionID = selectedSessionID else { return }
        checkpointDiffBusy = true
        checkpointDiff = nil
        checkpointDiffNotice = nil
        defer { checkpointDiffBusy = false }
        do {
            let result = try await core.checkpointDiff(
                sessionID: sessionID,
                checkpointID: checkpointID
            )
            if sessionID == selectedSessionID {
                checkpointDiff = result
            }
        } catch {
            checkpointDiffNotice = error.localizedDescription
            handle(error)
        }
    }

    func restoreCheckpoint(_ checkpointID: String) async {
        guard let sessionID = selectedSessionID else { return }
        checkpointBusy = true
        checkpointNotice = nil
        defer { checkpointBusy = false }
        do {
            let result = try await core.restoreCheckpoint(
                sessionID: sessionID,
                checkpointID: checkpointID
            )
            guard sessionID == selectedSessionID else { return }
            await loadCheckpoints()
            await loadCheckpointStats()
            await loadEvents()
            await loadDiff()
            checkpointDiff = nil
            checkpointDiffNotice = nil
            checkpointNotice =
                "Restored \(String(result.restoredCommit.prefix(8))) from \(String(result.previousCommit.prefix(8)))"
            showToast(
                "Checkpoint restored",
                detail: String(result.restoredCommit.prefix(8)),
                kind: .success
            )
        } catch {
            checkpointNotice = error.localizedDescription
            handle(error)
        }
    }

    func deleteCheckpoint(_ checkpointID: String) async {
        guard let sessionID = selectedSessionID else { return }
        checkpointBusy = true
        checkpointNotice = nil
        defer { checkpointBusy = false }
        do {
            let result = try await core.deleteCheckpoint(
                sessionID: sessionID,
                checkpointID: checkpointID
            )
            guard sessionID == selectedSessionID else { return }
            await loadCheckpoints()
            checkpointNotice = result.removed ? "Checkpoint deleted" : "Checkpoint was already removed"
            if result.removed {
                showToast("Checkpoint deleted", detail: "Snapshot ref released", kind: .success)
            }
        } catch {
            handle(error)
        }
    }

    func pruneCheckpoints() async {
        guard let sessionID = selectedSessionID else { return }
        checkpointBusy = true
        checkpointNotice = nil
        defer { checkpointBusy = false }
        do {
            let result = try await core.pruneCheckpoints(
                sessionID: sessionID,
                olderThanDays: 30,
                keepLatest: 5,
                dryRun: false
            )
            await loadCheckpoints()
            checkpointNotice = "Removed \(result.removed) of \(result.candidates) candidates"
        } catch {
            handle(error)
        }
    }

    func loadLineage() async {
        guard let sessionID = selectedSessionID else {
            sessionLineage = nil
            return
        }
        let generation = selectionGeneration
        lineageBusy = true
        defer { lineageBusy = false }
        do {
            let result = try await core.sessionLineage(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                sessionLineage = result
            }
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                sessionLineage = nil
            }
        }
    }

    func loadForkTree() async {
        guard let sessionID = selectedSessionID else {
            forkTree = nil
            return
        }
        let generation = selectionGeneration
        forkTreeBusy = true
        defer { forkTreeBusy = false }
        do {
            let result = try await core.sessionForkTree(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                forkTree = result
            }
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                forkTree = nil
            }
        }
    }

    func loadDashboardSnapshot() async {
        let projectID = selectedProjectID
        dashboardBusy = true
        defer { dashboardBusy = false }
        do {
            let snapshot = try await core.dashboardSnapshot(projectID: projectID)
            if projectID == selectedProjectID {
                dashboardSnapshot = snapshot
                dashboardStatus = snapshot.alerts.isEmpty
                    ? "No outstanding alerts."
                    : "\(snapshot.alerts.count) alert(s)\(snapshot.alertsTruncated ? " (truncated)" : "")"
            }
        } catch {
            if projectID == selectedProjectID {
                dashboardStatus = error.localizedDescription
            }
        }
    }

    func loadProjectForkForest() async {
        guard let projectID = selectedProjectID else {
            projectForkForest = nil
            return
        }
        projectForkForestBusy = true
        defer { projectForkForestBusy = false }
        do {
            let result = try await core.projectForkForest(projectID: projectID)
            if projectID == selectedProjectID {
                projectForkForest = result
            }
        } catch {
            projectForkForest = nil
        }
    }

    func auditProjectForks() async {
        guard let projectID = selectedProjectID else { return }
        forkAuditBusy = true
        forkRepairNotice = nil
        defer { forkAuditBusy = false }
        do {
            let result = try await core.auditProjectForks(projectID: projectID)
            if projectID == selectedProjectID {
                forkAudit = result
            }
        } catch {
            forkAudit = nil
            forkRepairNotice = error.localizedDescription
        }
    }

    func repairProjectForks() async {
        guard let projectID = selectedProjectID else { return }
        forkAuditBusy = true
        forkRepairNotice = nil
        defer { forkAuditBusy = false }
        do {
            let result = try await core.repairProjectForks(projectID: projectID)
            forkRepairNotice = "Repaired \(result.repaired), failed \(result.failed)"
            await auditProjectForks()
            await loadProjectForkForest()
        } catch {
            forkRepairNotice = error.localizedDescription
        }
    }

    func compareSessions(otherID: String) async {
        guard let sessionID = selectedSessionID else { return }
        compareBusy = true
        compareError = nil
        compareResult = nil
        defer { compareBusy = false }
        do {
            let result = try await core.compareSessions(
                sessionID: sessionID,
                otherSessionID: otherID
            )
            if sessionID == selectedSessionID {
                compareResult = result
            }
        } catch {
            if sessionID == selectedSessionID {
                compareError = error.localizedDescription
            }
            handle(error)
        }
    }

    func mergeFork(_ forkID: String) async {
        guard let sourceID = selectedSessionID else { return }
        forkMergeBusy = true
        forkMergeNotice = nil
        defer { forkMergeBusy = false }
        do {
            let result = try await core.mergeFork(
                sourceSessionID: sourceID,
                forkSessionID: forkID
            )
            guard sourceID == selectedSessionID else { return }
            await loadLineage()
            await loadEvents()
            await loadDiff()
            switch result.status {
            case "merged":
                forkMergeNotice =
                    "Merged \(String(result.forkCommit.prefix(8))) → \(String(result.mergeCommit?.prefix(8) ?? "?"))"
            case "conflict":
                forkMergeNotice = "Conflict: \(result.conflicts.joined(separator: ", "))"
            default:
                forkMergeNotice = "Merge failed: \(result.detail)"
            }
        } catch {
            forkMergeNotice = "Merge failed: \(error.localizedDescription)"
            handle(error)
        }
    }

    func compactSession() async {
        guard let sessionID = selectedSessionID else { return }
        compactBusy = true
        compactNotice = nil
        defer { compactBusy = false }
        do {
            let result = try await core.compactSession(sessionID: sessionID)
            guard sessionID == selectedSessionID else { return }
            await loadContextStatus()
            await loadEvents()
            if result.compacted {
                if let compaction = result.compaction {
                    compactNotice =
                        "\(compaction.mode): \(compaction.summarizedMessages) summarized, \(compaction.droppedMessages) dropped"
                } else {
                    compactNotice = "Compacted"
                }
            } else {
                compactNotice = "No change"
            }
        } catch {
            if sessionID == selectedSessionID {
                compactNotice = "Compaction failed"
            }
            handle(error)
        }
    }

    func loadDiff() async {
        guard let sessionID = selectedSessionID else {
            sessionDiff = nil
            return
        }
        let generation = selectionGeneration
        diffLoading = true
        defer {
            if generation == selectionGeneration {
                diffLoading = false
            }
        }
        do {
            let diff = try await core.sessionDiff(sessionID: sessionID)
            if isCurrent(sessionID: sessionID, generation: generation) {
                sessionDiff = diff
                diffFiles = SessionDiffParser.parse(diff.patch)
                diffError = nil
            }
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                diffError = error.localizedDescription
            }
        }
    }

    func clearError() {
        lastError = nil
    }

    private func loadSessions() async {
        guard let projectID = selectedProjectID else {
            sessions = []
            selectedSessionID = nil
            events = []
            return
        }
        let generation = selectionGeneration
        do {
            let loaded = try await core.listSessions(projectID: projectID)
            guard selectedProjectID == projectID,
                  generation == selectionGeneration
            else {
                return
            }
            sessions = loaded
            if let selectedSessionID,
               !sessions.contains(where: { $0.id == selectedSessionID })
            {
                self.selectedSessionID = nil
            }
            if selectedSessionID == nil {
                selectedSessionID = sessions.first?.id
            }
            await loadEvents()
            await loadTodos()
            await loadAgents()
            await loadMergeQueue()
            await loadTeamPlan()
            await loadDiff()
            await refreshLongTaskState()
        } catch {
            if selectedProjectID == projectID, generation == selectionGeneration {
                handle(error)
            }
        }
    }

    private func loadEvents() async {
        guard let sessionID = selectedSessionID else {
            events = []
            return
        }
        let generation = selectionGeneration
        do {
            var loaded: [Event] = []
            var since: UInt64 = 0
            while true {
                if Task.isCancelled || !isCurrent(sessionID: sessionID, generation: generation) {
                    return
                }
                let page = try await core.listEvents(
                    sessionID: sessionID,
                    since: since,
                    limit: 500
                )
                loaded.append(contentsOf: page)
                guard page.count == 500, let last = page.last else { break }
                let next = last.seq &+ 1
                guard next > since else { break }
                since = next
                if loaded.count > 100_000 {
                    break
                }
            }
            guard isCurrent(sessionID: sessionID, generation: generation) else { return }
            // Keep events that arrived while the history was loading; dedupe by
            // sequence and restore the canonical order.
            let live = events.filter { $0.sessionId == sessionID }
            events = mergeEvents(loaded, live)
            refreshPlanTaskStatuses()
        } catch {
            if isCurrent(sessionID: sessionID, generation: generation) {
                handle(error)
            }
        }
    }

    private func mergeEvents(_ first: [Event], _ second: [Event]) -> [Event] {
        var bySeq: [UInt64: Event] = [:]
        for event in first {
            bySeq[event.seq] = event
        }
        for event in second {
            bySeq[event.seq] = event
        }
        return bySeq.values.sorted { $0.seq < $1.seq }
    }

    private func refreshPlanTaskStatuses() {
        var statuses: [String: String] = [:]
        for event in events {
            guard let taskID = event.payload.objectValue?["task_id"]?.stringValue,
                  let status = planTaskStatus(for: event)
            else {
                continue
            }
            statuses[taskID] = status
        }
        planTaskStatuses = statuses
    }

    private func planTaskStatus(for event: Event) -> String? {
        if let status = event.payload.objectValue?["task_status"]?.stringValue {
            return status
        }
        return switch event.type {
        case "plan_task.started", "plan_task.resumed": "running"
        case "plan_task.skipped": "skipped"
        case "plan_task.blocked": "blocked"
        case "plan_task.unblocked": "pending"
        case "plan_task.cancelled": "cancelled"
        case "plan_task.retried", "plan_task.retry_scheduled": "pending"
        case "plan_task.cancelling": "cancelling"
        case "plan_task.pausing": "pausing"
        case "plan_task.paused": "paused"
        case "plan_task.interrupted": "interrupted"
        default: nil
        }
    }

    private func consumeEvents() {
        eventTask?.cancel()
        eventTask = Task { [weak self] in
            guard let self else { return }
            let stream = await core.events()
            for await event in stream {
                apply(event)
            }
        }
    }

    private func apply(_ event: Event) {
        let transientDelta = event.type == "agent.message_delta" || event.type == "agent.thought_delta"
        if event.sessionId == selectedSessionID,
           !transientDelta,
           !events.contains(where: { $0.id == event.id })
        {
            events.append(event)
            // Context diagnostics can add one event per provider call; keep a
            // deeper window so a long tool-heavy session does not push the
            // visible conversation out of the in-memory event buffer.
            if events.count > 4000 {
                events.removeFirst(events.count - 4000)
            }
        }

        if event.sessionId == selectedSessionID {
            let role = event.payload.objectValue?["agent_role"]?.stringValue
            let isLeader = role == nil || role == "leader"
            switch event.type {
            case "agent.started":
                if isLeader {
                    streamingText = ""
                }
            case "agent.message_delta":
                if isLeader, let text = event.payload.objectValue?["text"]?.stringValue {
                    streamingText += text
                }
            case "agent.message", "agent.turn_finished", "agent.failed", "agent.cancelled",
                 "agent.budget_exceeded":
                if isLeader {
                    streamingText = ""
                }
            default:
                break
            }
        }

        if event.type == "gui.action",
           event.sessionId == selectedSessionID,
           let data = try? JSONEncoder().encode(event.payload),
           let decoded = try? JSONDecoder().decode(GuiActionInfo.self, from: data)
        {
            guiTimeline.append(decoded)
            if guiTimeline.count > 80 {
                guiTimeline.removeFirst(guiTimeline.count - 80)
            }
        }

        if event.type.hasPrefix("computer.lease.") {
            Task { [weak self] in
                await self?.loadGuiStatus()
            }
        }

        if event.type == "todo.updated",
           event.sessionId == selectedSessionID,
           let items = event.payload.objectValue?["items"],
           let data = try? JSONEncoder().encode(items),
           let decoded = try? JSONDecoder().decode([TodoItem].self, from: data)
        {
            todos = decoded
        }

        if event.type == "agent.usage",
           let input = event.payload.objectValue?["input_tokens"]?.intValue,
           let output = event.payload.objectValue?["output_tokens"]?.intValue,
           let index = sessions.firstIndex(where: { $0.id == event.sessionId })
        {
            let old = sessions[index]
            sessions[index] = Session(
                id: old.id,
                projectId: old.projectId,
                goal: old.goal,
                status: old.status,
                priority: old.priority,
                worktree: old.worktree,
                branch: old.branch,
                tokenBudget: old.tokenBudget,
                costBudgetUsd: old.costBudgetUsd,
                timeBudgetSecs: old.timeBudgetSecs,
                unlimited: old.unlimited,
                costSpentUsd: old.costSpentUsd,
                baseCommit: old.baseCommit,
                tokensIn: old.tokensIn + input,
                tokensOut: old.tokensOut + output,
                createdAt: old.createdAt
            )
        }

        if event.type == "agent.usage" || event.type == "agent.cost" {
            Task { await loadUsage() }
        }

        if event.type == "agent.cost",
           let total = event.payload.objectValue?["total_usd"]?.doubleValue,
           let index = sessions.firstIndex(where: { $0.id == event.sessionId })
        {
            let old = sessions[index]
            sessions[index] = Session(
                id: old.id,
                projectId: old.projectId,
                goal: old.goal,
                status: old.status,
                priority: old.priority,
                worktree: old.worktree,
                branch: old.branch,
                tokenBudget: old.tokenBudget,
                costBudgetUsd: old.costBudgetUsd,
                timeBudgetSecs: old.timeBudgetSecs,
                unlimited: old.unlimited,
                costSpentUsd: total,
                baseCommit: old.baseCommit,
                tokensIn: old.tokensIn,
                tokensOut: old.tokensOut,
                createdAt: old.createdAt
            )
        }

        if event.type == "session.updated",
           let status = event.payload.objectValue?["status"]?.stringValue,
           let index = sessions.firstIndex(where: { $0.id == event.sessionId })
        {
            let old = sessions[index]
            sessions[index] = Session(
                id: old.id,
                projectId: old.projectId,
                goal: old.goal,
                status: status,
                priority: old.priority,
                worktree: old.worktree,
                branch: old.branch,
                tokenBudget: old.tokenBudget,
                costBudgetUsd: old.costBudgetUsd,
                timeBudgetSecs: old.timeBudgetSecs,
                unlimited: old.unlimited,
                costSpentUsd: old.costSpentUsd,
                baseCommit: old.baseCommit,
                tokensIn: old.tokensIn,
                tokensOut: old.tokensOut,
                createdAt: old.createdAt
            )
        }

        if event.type == "session.created"
            || event.type == "session.budget_mode_changed"
            || event.type == "session.worktree"
            || event.type == "session.imported"
            || event.type == "session.rehydrated"
        {
            Task {
                await self.loadSessions()
                await self.refreshSessionDirectory()
                await self.loadDiff()
            }
        }

        if event.sessionId == selectedSessionID,
           ["agent.spawned", "agent.finished", "agent.updated"].contains(event.type)
        {
            Task { await self.loadAgents() }
        }

        if event.type.hasPrefix("intake.") {
            Task {
                await self.loadIntakeTasks(silent: true)
                await self.loadSchedulerStatus(silent: true)
            }
        }

        if event.type == "scaling.decision" {
            Task { await self.loadScalingStatus(silent: true) }
        }

        if event.sessionId == selectedSessionID,
           ["context.usage", "context.compacted", "context.measured"].contains(event.type)
        {
            Task { await self.loadContextStatus() }
        }

        if ["usage.recorded", "agent.usage", "agent.cost"].contains(event.type) {
            refreshEfficiencySoon()
        }
        if event.sessionId == selectedSessionID,
           ["context.usage", "context.measured", "context.compacted", "context.cache",
            "context.prompt_prefix"].contains(event.type)
        {
            refreshEfficiencySoon()
        }

        if event.sessionId == selectedSessionID,
           ["session.checkpoint", "session.checkpoint.deleted"].contains(event.type)
        {
            Task { await self.loadCheckpoints() }
        }

        if event.sessionId == selectedSessionID, event.type == "session.forked" {
            Task {
                await self.loadLineage()
                if self.forkTree != nil {
                    await self.loadForkTree()
                }
                await self.loadProjectForkForest()
            }
        }

        if event.sessionId == selectedSessionID, event.type == "session.fork.merge" {
            Task {
                await self.loadLineage()
                await self.loadEvents()
                await self.loadDiff()
            }
        }

        if event.sessionId == selectedSessionID {
            if event.type == "team_plan.submitted" {
                planTaskStatuses = [:]
                Task {
                    await self.loadTeamPlan()
                    await self.loadScheduledSessionTasks()
                }
            }
            if event.type.hasPrefix("plan_task.") || event.type == "team_plan.revised" {
                Task { await self.loadScheduledSessionTasks() }
            }
            if event.type.hasPrefix("plan_task.")
                || event.type.hasPrefix("coordination.")
                || event.type.hasPrefix("orchestration.")
                || event.type.hasPrefix("merge_queue.")
                || event.type == "team_plan.submitted"
                || event.type == "team_plan.revised"
                || event.type == "session.updated"
                || event.type.hasPrefix("agent.")
            {
                refreshOrchestrationSnapshotSoon()
            }
            if let taskID = event.payload.objectValue?["task_id"]?.stringValue,
               let status = planTaskStatus(for: event)
            {
                planTaskStatuses[taskID] = status
            }
            if event.type.hasPrefix("steering.")
                || event.type.hasPrefix("session.continuation.")
                || event.type.hasPrefix("orchestration.recovery_")
                || event.type == "orchestration.stall"
                || event.type.hasPrefix("plan_task.")
            {
                refreshLongTaskStateSoon()
            }
        }

        if event.sessionId == selectedSessionID,
           event.type.hasPrefix("merge_queue.") || event.type.hasPrefix("integrator.")
        {
            Task {
                await self.loadMergeQueue()
                await self.loadDiff()
            }
        }

        if event.sessionId == selectedSessionID,
           event.type.hasPrefix("mailbox.") || event.type.hasPrefix("artifact.")
        {
            Task { await self.loadMailbox() }
        }

        if event.type.hasPrefix("memory.conflict.") {
            Task {
                await self.loadMemoryHealth()
                await self.loadMemoryConflicts()
            }
        } else if event.type.hasPrefix("memory.") {
            // Create, delete, prune, verify, stale, supersede, and pin all
            // change the memory projection or health counters. Reloading the
            // list also refreshes stats, health, and conflicts.
            Task { await self.loadMemories() }
        }

        if ["approval.requested", "approval.created"].contains(event.type),
           let approval = try? event.payload.decode(Approval.self),
           !pendingApprovals.contains(where: { $0.id == approval.id })
        {
            pendingApprovals.append(approval)
        }
        if ["approval.resolved", "approval.expired"].contains(event.type),
           let approvalID = event.payload.objectValue?["approval_id"]?.stringValue
        {
            pendingApprovals.removeAll { $0.id == approvalID }
            approvalSelection.remove(approvalID)
        }
        if event.type == "approval.coalesced" {
            Task { await self.loadPendingApprovals() }
        }

        if ["question.requested", "question.answered", "question.cancelled", "question.expired"]
            .contains(event.type),
           let question = try? event.payload.decode(Question.self)
        {
            if let index = questions.firstIndex(where: { $0.id == question.id }) {
                questions[index] = question
            } else {
                questions.append(question)
            }
            if let index = pendingQuestions.firstIndex(where: { $0.id == question.id }) {
                if question.isPending {
                    pendingQuestions[index] = question
                } else {
                    pendingQuestions.remove(at: index)
                }
            } else if question.isPending {
                pendingQuestions.append(question)
            }
        }

        if event.sessionId == selectedSessionID,
           ["tool.finished", "agent.turn_finished", "session.worktree"].contains(event.type)
        {
            Task { await self.loadDiff() }
        }

        if event.sessionId == selectedSessionID, event.type == "tool.finished" {
            refreshToolActivitySoon()
        }

        recordNotice(for: event)
    }

    private func recordNotice(for event: Event) {
        let directory = sessionDirectory[event.sessionId]
        let fallbackProject = event.sessionId == selectedSessionID ? selectedProjectID : nil
        let goal = directory?.goal ?? String(event.sessionId.prefix(8))
        switch event.type {
        case "question.requested":
            guard let question = try? event.payload.decode(Question.self) else { return }
            addNotice(
                id: "question-\(question.id)",
                event: event,
                projectId: directory?.projectId ?? fallbackProject,
                kind: "question",
                title: "Input needed · \(goal)",
                detail: question.question,
                createdAt: event.createdAt
            )
        case "approval.requested", "approval.created":
            guard let approval = try? event.payload.decode(Approval.self) else { return }
            // The daemon sets `notify=false` for rows inside the same
            // session/tool/risk coalescing window. Older events have no field
            // and remain notifiable.
            let notify = event.payload.objectValue?["notify"]?.boolValue ?? true
            guard notify else { return }
            addNotice(
                id: "approval-\(approval.id)",
                event: event,
                projectId: directory?.projectId ?? fallbackProject,
                kind: "approval",
                title: "Approval needed · \(goal)",
                detail: "\(approval.tool) · \(approval.agentName ?? approval.agentRole ?? "agent")",
                createdAt: event.createdAt
            )
        case "plan_task.finished":
            let status = event.payload.objectValue?["task_status"]?.stringValue ?? ""
            guard status == "failed" || status == "blocked" else { return }
            let task = event.payload.objectValue?["task_id"]?.stringValue ?? "task"
            addNotice(
                id: "task-\(event.sessionId)-\(task)-\(status)-\(event.seq)",
                event: event,
                projectId: directory?.projectId ?? fallbackProject,
                kind: "task",
                title: "Plan task \(status) · \(task)",
                detail: goal,
                createdAt: event.createdAt
            )
        case "integrator.finished":
            let status = event.payload.objectValue?["status"]?.stringValue ?? ""
            guard status == "failed" || status == "conflict" else { return }
            addNotice(
                id: "integrator-\(event.sessionId)-\(status)-\(event.seq)",
                event: event,
                projectId: directory?.projectId ?? fallbackProject,
                kind: "integration",
                title: "Integration \(status)",
                detail: goal,
                createdAt: event.createdAt
            )
        case "session.updated":
            let status = event.payload.objectValue?["status"]?.stringValue ?? ""
            guard status == "failed" || status == "cancelled" else { return }
            addNotice(
                id: "session-\(event.sessionId)-\(status)",
                event: event,
                projectId: directory?.projectId ?? fallbackProject,
                kind: "session",
                title: "Session \(status)",
                detail: goal,
                createdAt: event.createdAt
            )
        default:
            break
        }
    }

    private func addNotice(
        id: String,
        event: Event,
        projectId: String?,
        kind: String,
        title: String,
        detail: String,
        createdAt: String
    ) {
        guard !notices.contains(where: { $0.id == id }) else { return }
        let notice = AppNotice(
            id: id,
            sessionId: event.sessionId,
            projectId: projectId,
            kind: kind,
            title: title,
            detail: detail,
            createdAt: createdAt,
            isRead: false
        )
        notices.insert(notice, at: 0)
        if notices.count > 100 {
            notices.removeLast(notices.count - 100)
        }
        saveNotices()
        updateDockBadge()
        SystemNotifier.shared.notify(
            notice: notice,
            sessionGoal: sessionDirectory[event.sessionId]?.goal
        )
    }

    private func loadNotices() {
        guard let data = UserDefaults.standard.data(forKey: noticesKey),
              let decoded = try? JSONDecoder().decode([AppNotice].self, from: data)
        else { return }
        notices = Array(decoded.prefix(100))
        updateDockBadge()
    }

    private func saveNotices() {
        guard let data = try? JSONEncoder().encode(notices) else { return }
        UserDefaults.standard.set(data, forKey: noticesKey)
    }

    private func updateDockBadge() {
        NSApp.dockTile.badgeLabel = unreadNoticeCount > 0 ? "\(unreadNoticeCount)" : nil
    }

    func showToast(
        _ title: String,
        detail: String,
        kind: VPToastKind = .info,
        duration: TimeInterval = 3.4
    ) {
        let toast = VPToast(
            id: UUID().uuidString,
            title: title,
            detail: detail,
            kind: kind,
            duration: duration
        )
        toasts.append(toast)
        if toasts.count > 4 {
            toasts.removeFirst(toasts.count - 4)
        }
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(duration))
            self?.toasts.removeAll { $0.id == toast.id }
        }
    }

    func dismissToast(_ id: String) {
        toasts.removeAll { $0.id == id }
    }

    private func handle(_ error: Error) {
        lastError = error.localizedDescription
        if case .connecting = connection {
            connection = .failed(error.localizedDescription)
        }
    }
}
