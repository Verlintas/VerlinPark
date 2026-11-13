import Foundation

// The S7 org console is a projection over the S1–S6 identity stores. These
// models are decode-only for read surfaces; mutations are explicit drafts that
// AppModel sends through real RPCs. No prompt, tool argument, tool output, or
// secret is represented here.

/// Bounded project identity policy mirrored for approval-state copy.
struct AgentIdentityPolicy: Codable, Hashable, Sendable {
    let enabled: Bool
    let maxManagers: Int
    let maxWorkers: Int
    let hireRequiresApproval: Bool
    let retirementRequiresApproval: Bool

    enum CodingKeys: String, CodingKey {
        case enabled
        case maxManagers = "max_managers"
        case maxWorkers = "max_workers"
        case hireRequiresApproval = "hire_requires_approval"
        case retirementRequiresApproval = "retirement_requires_approval"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        maxManagers = try container.decodeIfPresent(Int.self, forKey: .maxManagers) ?? 4
        maxWorkers = try container.decodeIfPresent(Int.self, forKey: .maxWorkers) ?? 10
        hireRequiresApproval =
            try container.decodeIfPresent(Bool.self, forKey: .hireRequiresApproval) ?? true
        retirementRequiresApproval =
            try container.decodeIfPresent(Bool.self, forKey: .retirementRequiresApproval) ?? true
    }

    static let disabledDefault = AgentIdentityPolicy(
        enabled: false,
        maxManagers: 4,
        maxWorkers: 10,
        hireRequiresApproval: true,
        retirementRequiresApproval: true
    )

    init(
        enabled: Bool,
        maxManagers: Int,
        maxWorkers: Int,
        hireRequiresApproval: Bool,
        retirementRequiresApproval: Bool
    ) {
        self.enabled = enabled
        self.maxManagers = maxManagers
        self.maxWorkers = maxWorkers
        self.hireRequiresApproval = hireRequiresApproval
        self.retirementRequiresApproval = retirementRequiresApproval
    }
}

/// Project retirement policy mirrored for the read-only console.
struct AgentRetirementPolicy: Codable, Hashable, Sendable {
    let enabled: Bool
    let lookbackDays: Int
    let minCompletedTasks: Int
    let maxConsecutiveFailures: Int
    let maxVerifierRejections: Int
    let maxUnsubstantiatedHandoffs: Int
    let maxFalseConsensus: Int
    let cooldownSecs: Int
    let requireApproval: Bool

    enum CodingKeys: String, CodingKey {
        case enabled
        case lookbackDays = "lookback_days"
        case minCompletedTasks = "min_completed_tasks"
        case maxConsecutiveFailures = "max_consecutive_failures"
        case maxVerifierRejections = "max_verifier_rejections"
        case maxUnsubstantiatedHandoffs = "max_unsubstantiated_handoffs"
        case maxFalseConsensus = "max_false_consensus"
        case cooldownSecs = "cooldown_secs"
        case requireApproval = "require_approval"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        lookbackDays = try container.decodeIfPresent(Int.self, forKey: .lookbackDays) ?? 30
        minCompletedTasks =
            try container.decodeIfPresent(Int.self, forKey: .minCompletedTasks) ?? 3
        maxConsecutiveFailures =
            try container.decodeIfPresent(Int.self, forKey: .maxConsecutiveFailures) ?? 3
        maxVerifierRejections =
            try container.decodeIfPresent(Int.self, forKey: .maxVerifierRejections) ?? 2
        maxUnsubstantiatedHandoffs =
            try container.decodeIfPresent(Int.self, forKey: .maxUnsubstantiatedHandoffs) ?? 2
        maxFalseConsensus =
            try container.decodeIfPresent(Int.self, forKey: .maxFalseConsensus) ?? 2
        cooldownSecs = try container.decodeIfPresent(Int.self, forKey: .cooldownSecs) ?? 86400
        requireApproval =
            try container.decodeIfPresent(Bool.self, forKey: .requireApproval) ?? true
    }

    static let disabledDefault = AgentRetirementPolicy(
        enabled: false,
        lookbackDays: 30,
        minCompletedTasks: 3,
        maxConsecutiveFailures: 3,
        maxVerifierRejections: 2,
        maxUnsubstantiatedHandoffs: 2,
        maxFalseConsensus: 2,
        cooldownSecs: 86400,
        requireApproval: true
    )

    init(
        enabled: Bool,
        lookbackDays: Int,
        minCompletedTasks: Int,
        maxConsecutiveFailures: Int,
        maxVerifierRejections: Int,
        maxUnsubstantiatedHandoffs: Int,
        maxFalseConsensus: Int,
        cooldownSecs: Int,
        requireApproval: Bool
    ) {
        self.enabled = enabled
        self.lookbackDays = lookbackDays
        self.minCompletedTasks = minCompletedTasks
        self.maxConsecutiveFailures = maxConsecutiveFailures
        self.maxVerifierRejections = maxVerifierRejections
        self.maxUnsubstantiatedHandoffs = maxUnsubstantiatedHandoffs
        self.maxFalseConsensus = maxFalseConsensus
        self.cooldownSecs = cooldownSecs
        self.requireApproval = requireApproval
    }
}

struct AgentOrgCounts: Codable, Hashable, Sendable {
    let identities: Int
    let leaders: Int
    let managers: Int
    let workers: Int
    let active: Int
    let idle: Int
    let paused: Int
    let retired: Int
    let imported: Int
    let activeAssignments: Int
    let activeWorkspaces: Int
    let unreadInbox: Int
    let memoryCount: Int
    let pendingHireRequests: Int
    let pendingRetirementProposals: Int
    let retirementCandidates: Int

    enum CodingKeys: String, CodingKey {
        case identities, leaders, managers, workers, active, idle, paused, retired, imported
        case activeAssignments = "active_assignments"
        case activeWorkspaces = "active_workspaces"
        case unreadInbox = "unread_inbox"
        case memoryCount = "memory_count"
        case pendingHireRequests = "pending_hire_requests"
        case pendingRetirementProposals = "pending_retirement_proposals"
        case retirementCandidates = "retirement_candidates"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        identities = try container.decodeIfPresent(Int.self, forKey: .identities) ?? 0
        leaders = try container.decodeIfPresent(Int.self, forKey: .leaders) ?? 0
        managers = try container.decodeIfPresent(Int.self, forKey: .managers) ?? 0
        workers = try container.decodeIfPresent(Int.self, forKey: .workers) ?? 0
        active = try container.decodeIfPresent(Int.self, forKey: .active) ?? 0
        idle = try container.decodeIfPresent(Int.self, forKey: .idle) ?? 0
        paused = try container.decodeIfPresent(Int.self, forKey: .paused) ?? 0
        retired = try container.decodeIfPresent(Int.self, forKey: .retired) ?? 0
        imported = try container.decodeIfPresent(Int.self, forKey: .imported) ?? 0
        activeAssignments =
            try container.decodeIfPresent(Int.self, forKey: .activeAssignments) ?? 0
        activeWorkspaces =
            try container.decodeIfPresent(Int.self, forKey: .activeWorkspaces) ?? 0
        unreadInbox = try container.decodeIfPresent(Int.self, forKey: .unreadInbox) ?? 0
        memoryCount = try container.decodeIfPresent(Int.self, forKey: .memoryCount) ?? 0
        pendingHireRequests =
            try container.decodeIfPresent(Int.self, forKey: .pendingHireRequests) ?? 0
        pendingRetirementProposals =
            try container.decodeIfPresent(Int.self, forKey: .pendingRetirementProposals) ?? 0
        retirementCandidates =
            try container.decodeIfPresent(Int.self, forKey: .retirementCandidates) ?? 0
    }

    static let empty = AgentOrgCounts(
        identities: 0,
        leaders: 0,
        managers: 0,
        workers: 0,
        active: 0,
        idle: 0,
        paused: 0,
        retired: 0,
        imported: 0,
        activeAssignments: 0,
        activeWorkspaces: 0,
        unreadInbox: 0,
        memoryCount: 0,
        pendingHireRequests: 0,
        pendingRetirementProposals: 0,
        retirementCandidates: 0
    )

    init(
        identities: Int,
        leaders: Int,
        managers: Int,
        workers: Int,
        active: Int,
        idle: Int,
        paused: Int,
        retired: Int,
        imported: Int,
        activeAssignments: Int,
        activeWorkspaces: Int,
        unreadInbox: Int,
        memoryCount: Int,
        pendingHireRequests: Int,
        pendingRetirementProposals: Int,
        retirementCandidates: Int
    ) {
        self.identities = identities
        self.leaders = leaders
        self.managers = managers
        self.workers = workers
        self.active = active
        self.idle = idle
        self.paused = paused
        self.retired = retired
        self.imported = imported
        self.activeAssignments = activeAssignments
        self.activeWorkspaces = activeWorkspaces
        self.unreadInbox = unreadInbox
        self.memoryCount = memoryCount
        self.pendingHireRequests = pendingHireRequests
        self.pendingRetirementProposals = pendingRetirementProposals
        self.retirementCandidates = retirementCandidates
    }

    var liveIdentities: Int {
        max(0, identities - retired)
    }

    var pendingOrgActions: Int {
        pendingHireRequests + pendingRetirementProposals
    }
}

struct AgentOrgAssignmentState: Codable, Hashable, Sendable {
    let sessionId: String
    let role: String
    let assignedAt: String
    let leaseExpiresAt: String?
    let leaseActive: Bool
    let assignmentCount: Int
    let sessionCount: Int

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case role
        case assignedAt = "assigned_at"
        case leaseExpiresAt = "lease_expires_at"
        case leaseActive = "lease_active"
        case assignmentCount = "assignment_count"
        case sessionCount = "session_count"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decodeIfPresent(String.self, forKey: .sessionId) ?? ""
        role = try container.decodeIfPresent(String.self, forKey: .role) ?? ""
        assignedAt = try container.decodeIfPresent(String.self, forKey: .assignedAt) ?? ""
        leaseExpiresAt = try container.decodeIfPresent(String.self, forKey: .leaseExpiresAt)
        leaseActive = try container.decodeIfPresent(Bool.self, forKey: .leaseActive) ?? false
        assignmentCount = try container.decodeIfPresent(Int.self, forKey: .assignmentCount) ?? 0
        sessionCount = try container.decodeIfPresent(Int.self, forKey: .sessionCount) ?? 0
    }
}

struct AgentOrgWorkspaceState: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let identityId: String
    let component: String
    let branch: String
    let status: String
    let leaseActive: Bool
    let leaseExpiresAt: String?
    let lastSessionId: String?
    let workspaceCount: Int

    enum CodingKeys: String, CodingKey {
        case id
        case identityId = "identity_id"
        case component, branch, status
        case leaseActive = "lease_active"
        case leaseExpiresAt = "lease_expires_at"
        case lastSessionId = "last_session_id"
        case workspaceCount = "workspace_count"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        identityId = try container.decodeIfPresent(String.self, forKey: .identityId) ?? ""
        component = try container.decodeIfPresent(String.self, forKey: .component) ?? ""
        branch = try container.decodeIfPresent(String.self, forKey: .branch) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        leaseActive = try container.decodeIfPresent(Bool.self, forKey: .leaseActive) ?? false
        leaseExpiresAt = try container.decodeIfPresent(String.self, forKey: .leaseExpiresAt)
        lastSessionId = try container.decodeIfPresent(String.self, forKey: .lastSessionId)
        workspaceCount = try container.decodeIfPresent(Int.self, forKey: .workspaceCount) ?? 1
    }
}

struct AgentOrgPerformanceSummary: Codable, Hashable, Sendable {
    let completedTasks: Int
    let failedTasks: Int
    let cancelledTasks: Int
    let blockedTasks: Int
    let skippedTasks: Int
    let consecutiveFailures: Int
    let retries: Int
    let verifierApprovals: Int
    let verifierRejections: Int
    let inputTokens: Int
    let outputTokens: Int
    let costUsd: Double
    let lastActiveAt: String?
    let evidenceCount: Int

    enum CodingKeys: String, CodingKey {
        case completedTasks = "completed_tasks"
        case failedTasks = "failed_tasks"
        case cancelledTasks = "cancelled_tasks"
        case blockedTasks = "blocked_tasks"
        case skippedTasks = "skipped_tasks"
        case consecutiveFailures = "consecutive_failures"
        case retries
        case verifierApprovals = "verifier_approvals"
        case verifierRejections = "verifier_rejections"
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case costUsd = "cost_usd"
        case lastActiveAt = "last_active_at"
        case evidenceCount = "evidence_count"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        completedTasks = try container.decodeIfPresent(Int.self, forKey: .completedTasks) ?? 0
        failedTasks = try container.decodeIfPresent(Int.self, forKey: .failedTasks) ?? 0
        cancelledTasks = try container.decodeIfPresent(Int.self, forKey: .cancelledTasks) ?? 0
        blockedTasks = try container.decodeIfPresent(Int.self, forKey: .blockedTasks) ?? 0
        skippedTasks = try container.decodeIfPresent(Int.self, forKey: .skippedTasks) ?? 0
        consecutiveFailures =
            try container.decodeIfPresent(Int.self, forKey: .consecutiveFailures) ?? 0
        retries = try container.decodeIfPresent(Int.self, forKey: .retries) ?? 0
        verifierApprovals =
            try container.decodeIfPresent(Int.self, forKey: .verifierApprovals) ?? 0
        verifierRejections =
            try container.decodeIfPresent(Int.self, forKey: .verifierRejections) ?? 0
        inputTokens = try container.decodeIfPresent(Int.self, forKey: .inputTokens) ?? 0
        outputTokens = try container.decodeIfPresent(Int.self, forKey: .outputTokens) ?? 0
        costUsd = try container.decodeIfPresent(Double.self, forKey: .costUsd) ?? 0
        lastActiveAt = try container.decodeIfPresent(String.self, forKey: .lastActiveAt)
        evidenceCount = try container.decodeIfPresent(Int.self, forKey: .evidenceCount) ?? 0
    }

    static let empty = AgentOrgPerformanceSummary(
        completedTasks: 0,
        failedTasks: 0,
        cancelledTasks: 0,
        blockedTasks: 0,
        skippedTasks: 0,
        consecutiveFailures: 0,
        retries: 0,
        verifierApprovals: 0,
        verifierRejections: 0,
        inputTokens: 0,
        outputTokens: 0,
        costUsd: 0,
        lastActiveAt: nil,
        evidenceCount: 0
    )

    init(
        completedTasks: Int,
        failedTasks: Int,
        cancelledTasks: Int,
        blockedTasks: Int,
        skippedTasks: Int,
        consecutiveFailures: Int,
        retries: Int,
        verifierApprovals: Int,
        verifierRejections: Int,
        inputTokens: Int,
        outputTokens: Int,
        costUsd: Double,
        lastActiveAt: String?,
        evidenceCount: Int
    ) {
        self.completedTasks = completedTasks
        self.failedTasks = failedTasks
        self.cancelledTasks = cancelledTasks
        self.blockedTasks = blockedTasks
        self.skippedTasks = skippedTasks
        self.consecutiveFailures = consecutiveFailures
        self.retries = retries
        self.verifierApprovals = verifierApprovals
        self.verifierRejections = verifierRejections
        self.inputTokens = inputTokens
        self.outputTokens = outputTokens
        self.costUsd = costUsd
        self.lastActiveAt = lastActiveAt
        self.evidenceCount = evidenceCount
    }

    var hasEvidence: Bool {
        completedTasks > 0 || failedTasks > 0 || verifierApprovals > 0
            || verifierRejections > 0 || evidenceCount > 0
    }

    var taskSummary: String {
        "\(completedTasks)✓ \(failedTasks)×"
    }
}

struct AgentOrgRetirementState: Codable, Hashable, Sendable {
    let candidate: Bool
    let severity: Int
    let reasons: [AgentRetirementReason]
    let pendingProposals: Int
    let latestProposalId: String?
    let latestProposalStatus: String?
    let latestProposalAt: String?

    enum CodingKeys: String, CodingKey {
        case candidate, severity, reasons
        case pendingProposals = "pending_proposals"
        case latestProposalId = "latest_proposal_id"
        case latestProposalStatus = "latest_proposal_status"
        case latestProposalAt = "latest_proposal_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        candidate = try container.decodeIfPresent(Bool.self, forKey: .candidate) ?? false
        severity = try container.decodeIfPresent(Int.self, forKey: .severity) ?? 0
        reasons =
            try container.decodeIfPresent([AgentRetirementReason].self, forKey: .reasons) ?? []
        pendingProposals =
            try container.decodeIfPresent(Int.self, forKey: .pendingProposals) ?? 0
        latestProposalId =
            try container.decodeIfPresent(String.self, forKey: .latestProposalId)
        latestProposalStatus =
            try container.decodeIfPresent(String.self, forKey: .latestProposalStatus)
        latestProposalAt =
            try container.decodeIfPresent(String.self, forKey: .latestProposalAt)
    }

    static let empty = AgentOrgRetirementState(
        candidate: false,
        severity: 0,
        reasons: [],
        pendingProposals: 0,
        latestProposalId: nil,
        latestProposalStatus: nil,
        latestProposalAt: nil
    )

    init(
        candidate: Bool,
        severity: Int,
        reasons: [AgentRetirementReason],
        pendingProposals: Int,
        latestProposalId: String?,
        latestProposalStatus: String?,
        latestProposalAt: String?
    ) {
        self.candidate = candidate
        self.severity = severity
        self.reasons = reasons
        self.pendingProposals = pendingProposals
        self.latestProposalId = latestProposalId
        self.latestProposalStatus = latestProposalStatus
        self.latestProposalAt = latestProposalAt
    }

    var reasonSummary: String {
        reasons.map { "\($0.code) \($0.count)/\($0.threshold)" }.joined(separator: " · ")
    }

    var hasPendingProposal: Bool {
        pendingProposals > 0 || latestProposalStatus == "pending"
    }
}

struct AgentOrgNode: Codable, Identifiable, Hashable, Sendable {
    let identity: AgentIdentity
    let activeAssignment: AgentOrgAssignmentState?
    let workspace: AgentOrgWorkspaceState?
    let unreadInbox: Int
    let memoryCount: Int
    let performance: AgentOrgPerformanceSummary
    let retirement: AgentOrgRetirementState
    let reports: [AgentOrgNode]
    let reportCount: Int

    var id: String {
        identity.id
    }

    enum CodingKeys: String, CodingKey {
        case identity
        case activeAssignment = "active_assignment"
        case workspace
        case unreadInbox = "unread_inbox"
        case memoryCount = "memory_count"
        case performance, retirement, reports
        case reportCount = "report_count"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        identity = try container.decode(AgentIdentity.self, forKey: .identity)
        activeAssignment =
            try container.decodeIfPresent(AgentOrgAssignmentState.self, forKey: .activeAssignment)
        workspace =
            try container.decodeIfPresent(AgentOrgWorkspaceState.self, forKey: .workspace)
        unreadInbox = try container.decodeIfPresent(Int.self, forKey: .unreadInbox) ?? 0
        memoryCount = try container.decodeIfPresent(Int.self, forKey: .memoryCount) ?? 0
        performance =
            try container.decodeIfPresent(AgentOrgPerformanceSummary.self, forKey: .performance)
                ?? .empty
        retirement =
            try container.decodeIfPresent(AgentOrgRetirementState.self, forKey: .retirement)
                ?? .empty
        reports = try container.decodeIfPresent([AgentOrgNode].self, forKey: .reports) ?? []
        reportCount =
            try container.decodeIfPresent(Int.self, forKey: .reportCount) ?? reports.count
    }

    var displayTitle: String {
        identity.displayName
    }

    var statusLabel: String {
        if identity.isImported {
            return "imported"
        }
        return identity.status
    }

    var isLiveAssignment: Bool {
        activeAssignment?.leaseActive == true
    }

    var hasWorkspace: Bool {
        workspace != nil
    }

    var hasPerformanceEvidence: Bool {
        performance.hasEvidence
    }

    var needsAttention: Bool {
        identity.status == "paused" || identity.isImported
            || retirement.candidate || retirement.hasPendingProposal
            || identity.role == "manager" && !isLiveAssignment
    }
}

struct AgentOrgDepartment: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let managerCount: Int
    let workerCount: Int
    let activeCount: Int
    let activeWorkers: Int
    let unreadInbox: Int
    let managerIds: [String]
    let workerIds: [String]

    var id: String {
        name
    }

    enum CodingKeys: String, CodingKey {
        case name
        case managerCount = "manager_count"
        case workerCount = "worker_count"
        case activeCount = "active_count"
        case activeWorkers = "active_workers"
        case unreadInbox = "unread_inbox"
        case managerIds = "manager_ids"
        case workerIds = "worker_ids"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? "(unassigned)"
        managerCount = try container.decodeIfPresent(Int.self, forKey: .managerCount) ?? 0
        workerCount = try container.decodeIfPresent(Int.self, forKey: .workerCount) ?? 0
        activeCount = try container.decodeIfPresent(Int.self, forKey: .activeCount) ?? 0
        activeWorkers = try container.decodeIfPresent(Int.self, forKey: .activeWorkers) ?? 0
        unreadInbox = try container.decodeIfPresent(Int.self, forKey: .unreadInbox) ?? 0
        managerIds =
            try container.decodeIfPresent([String].self, forKey: .managerIds) ?? []
        workerIds = try container.decodeIfPresent([String].self, forKey: .workerIds) ?? []
    }
}

struct AgentOrgSnapshot: Codable, Sendable {
    let projectId: String
    let generatedAt: String
    let enabled: Bool
    let policy: AgentIdentityPolicy
    let retirementPolicy: AgentRetirementPolicy
    let counts: AgentOrgCounts
    let leader: AgentOrgNode?
    let unassigned: [AgentOrgNode]
    let departments: [AgentOrgDepartment]
    let activeAssignments: [AgentAssignment]
    let workspaceSummaries: [AgentOrgWorkspaceState]
    let pendingHireRequests: [AgentHireRequest]
    let pendingRetirementProposals: [AgentRetirementProposal]
    let truncated: Bool
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case generatedAt = "generated_at"
        case enabled, policy
        case retirementPolicy = "retirement_policy"
        case counts, leader, unassigned, departments
        case activeAssignments = "active_assignments"
        case workspaceSummaries = "workspace_summaries"
        case pendingHireRequests = "pending_hire_requests"
        case pendingRetirementProposals = "pending_retirement_proposals"
        case truncated, warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decodeIfPresent(String.self, forKey: .projectId) ?? ""
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        policy =
            try container.decodeIfPresent(AgentIdentityPolicy.self, forKey: .policy)
                ?? .disabledDefault
        retirementPolicy =
            try container.decodeIfPresent(AgentRetirementPolicy.self, forKey: .retirementPolicy)
                ?? .disabledDefault
        counts =
            try container.decodeIfPresent(AgentOrgCounts.self, forKey: .counts) ?? .empty
        leader = try container.decodeIfPresent(AgentOrgNode.self, forKey: .leader)
        unassigned =
            try container.decodeIfPresent([AgentOrgNode].self, forKey: .unassigned) ?? []
        departments =
            try container.decodeIfPresent([AgentOrgDepartment].self, forKey: .departments) ?? []
        activeAssignments =
            try container.decodeIfPresent([AgentAssignment].self, forKey: .activeAssignments)
                ?? []
        workspaceSummaries =
            try container.decodeIfPresent(
                [AgentOrgWorkspaceState].self,
                forKey: .workspaceSummaries
            ) ?? []
        pendingHireRequests =
            try container.decodeIfPresent([AgentHireRequest].self, forKey: .pendingHireRequests)
                ?? []
        pendingRetirementProposals =
            try container.decodeIfPresent(
                [AgentRetirementProposal].self,
                forKey: .pendingRetirementProposals
            ) ?? []
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }

    var isEmpty: Bool {
        leader == nil && unassigned.isEmpty
    }

    var liveNodes: [AgentOrgNode] {
        var rows: [AgentOrgNode] = []
        if let leader {
            rows.append(leader)
            rows.append(contentsOf: leader.reports)
            rows.append(contentsOf: leader.reports.flatMap(\.reports))
        }
        for node in unassigned {
            rows.append(node)
            rows.append(contentsOf: node.reports)
        }
        return rows
    }

    func node(identityID: String) -> AgentOrgNode? {
        if let leader,
           let found = Self.find(leader, identityID: identityID)
        {
            return found
        }
        for node in unassigned {
            if let found = Self.find(node, identityID: identityID) {
                return found
            }
        }
        return nil
    }

    private static func find(_ node: AgentOrgNode, identityID: String) -> AgentOrgNode? {
        if node.identity.id == identityID {
            return node
        }
        for report in node.reports {
            if let found = find(report, identityID: identityID) {
                return found
            }
        }
        return nil
    }

    var managerChoices: [AgentIdentity] {
        liveNodes.map(\.identity).filter { $0.role == "manager" && $0.status != "retired" }
    }

    var leaderChoices: [AgentIdentity] {
        liveNodes.map(\.identity).filter { $0.role == "leader" && $0.status != "retired" }
    }

    var parentChoices: [AgentIdentity] {
        liveNodes.map(\.identity).filter {
            ($0.role == "leader" || $0.role == "manager") && $0.status != "retired"
                && !$0.isImported
        }
    }
}

/// One inbox row in the detail console. `summary` is present only when the
/// caller explicitly requested content; metadata is always available.
struct AgentIdentityInboxEntry: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let kind: String
    let status: String
    let sessionId: String?
    let fromIdentityId: String?
    let fromLabel: String
    let createdAt: String
    let readAt: String?
    let expiresAt: String?
    let summary: String?

    enum CodingKeys: String, CodingKey {
        case id, kind, status
        case sessionId = "session_id"
        case fromIdentityId = "from_identity_id"
        case fromLabel = "from_label"
        case createdAt = "created_at"
        case readAt = "read_at"
        case expiresAt = "expires_at"
        case summary
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        sessionId = try container.decodeIfPresent(String.self, forKey: .sessionId)
        fromIdentityId = try container.decodeIfPresent(String.self, forKey: .fromIdentityId)
        fromLabel = try container.decodeIfPresent(String.self, forKey: .fromLabel) ?? ""
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        readAt = try container.decodeIfPresent(String.self, forKey: .readAt)
        expiresAt = try container.decodeIfPresent(String.self, forKey: .expiresAt)
        summary = try container.decodeIfPresent(String.self, forKey: .summary)
    }

    var isUnread: Bool {
        status == "unread"
    }
}

/// Bounded employment stream row for the Lifecycle tab. `payload` is retained as
/// arbitrary JSON metadata for display but is never treated as instructions or
/// credentials.
struct AgentIdentityEmploymentEvent: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let seq: Int
    let identityId: String
    let kind: String
    let payload: JSONValue?
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case seq
        case identityId = "identity_id"
        case kind
        case payload
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        projectId = try container.decodeIfPresent(String.self, forKey: .projectId) ?? ""
        seq = try container.decodeIfPresent(Int.self, forKey: .seq) ?? 0
        identityId = try container.decodeIfPresent(String.self, forKey: .identityId) ?? ""
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        payload = try container.decodeIfPresent(JSONValue.self, forKey: .payload)
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }

    var shortKind: String {
        kind.replacingOccurrences(of: "agent.", with: "")
    }
}

struct AgentIdentityDetail: Codable, Sendable {
    let projectId: String
    let generatedAt: String
    let identity: AgentIdentity
    let policy: AgentIdentityPolicy
    let retirementPolicy: AgentRetirementPolicy
    let node: AgentOrgNode
    let assignments: [AgentAssignment]
    let inbox: [AgentIdentityInboxEntry]
    let inboxCounts: AgentInboxCounts
    let memories: [AgentMemory]
    let memoryTotal: Int
    let performance: AgentPerformance
    let retirementCandidates: [AgentRetirementCandidate]
    let retirementProposals: [AgentRetirementProposal]
    let hireRequests: [AgentHireRequest]
    let workspaces: [AgentOrgWorkspaceState]
    let parentIdentity: AgentIdentity?
    let successor: AgentIdentity?
    let reports: [AgentIdentity]
    let employmentEvents: [AgentIdentityEmploymentEvent]
    let readOnly: Bool
    let readOnlyReason: String?
    let truncated: Bool
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case generatedAt = "generated_at"
        case identity, policy
        case retirementPolicy = "retirement_policy"
        case node, assignments, inbox
        case inboxCounts = "inbox_counts"
        case memories
        case memoryTotal = "memory_total"
        case performance
        case retirementCandidates = "retirement_candidates"
        case retirementProposals = "retirement_proposals"
        case hireRequests = "hire_requests"
        case workspaces
        case parentIdentity = "parent"
        case successor, reports
        case employmentEvents = "employment_events"
        case readOnly = "read_only"
        case readOnlyReason = "read_only_reason"
        case truncated, warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decodeIfPresent(String.self, forKey: .projectId) ?? ""
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        identity = try container.decode(AgentIdentity.self, forKey: .identity)
        policy =
            try container.decodeIfPresent(AgentIdentityPolicy.self, forKey: .policy)
                ?? .disabledDefault
        retirementPolicy =
            try container.decodeIfPresent(AgentRetirementPolicy.self, forKey: .retirementPolicy)
                ?? .disabledDefault
        node =
            try container.decodeIfPresent(AgentOrgNode.self, forKey: .node)
                ?? AgentOrgNode.fallback(identity: identity)
        assignments =
            try container.decodeIfPresent([AgentAssignment].self, forKey: .assignments) ?? []
        inbox =
            try container.decodeIfPresent([AgentIdentityInboxEntry].self, forKey: .inbox) ?? []
        inboxCounts =
            try container.decodeIfPresent(AgentInboxCounts.self, forKey: .inboxCounts)
                ?? AgentInboxCounts(unread: 0, read: 0, done: 0, expired: 0)
        memories =
            try container.decodeIfPresent([AgentMemory].self, forKey: .memories) ?? []
        memoryTotal = try container.decodeIfPresent(Int.self, forKey: .memoryTotal) ?? memories.count
        performance =
            try container.decodeIfPresent(AgentPerformance.self, forKey: .performance)
                ?? AgentPerformance.fallback(projectId: projectId, identity: identity)
        retirementCandidates =
            try container.decodeIfPresent(
                [AgentRetirementCandidate].self,
                forKey: .retirementCandidates
            ) ?? []
        retirementProposals =
            try container.decodeIfPresent(
                [AgentRetirementProposal].self,
                forKey: .retirementProposals
            ) ?? []
        hireRequests =
            try container.decodeIfPresent([AgentHireRequest].self, forKey: .hireRequests) ?? []
        workspaces =
            try container.decodeIfPresent([AgentOrgWorkspaceState].self, forKey: .workspaces)
                ?? []
        parentIdentity =
            try container.decodeIfPresent(AgentIdentity.self, forKey: .parentIdentity)
        successor = try container.decodeIfPresent(AgentIdentity.self, forKey: .successor)
        reports =
            try container.decodeIfPresent([AgentIdentity].self, forKey: .reports) ?? []
        employmentEvents =
            try container.decodeIfPresent(
                [AgentIdentityEmploymentEvent].self,
                forKey: .employmentEvents
            ) ?? []
        readOnly = try container.decodeIfPresent(Bool.self, forKey: .readOnly) ?? false
        readOnlyReason =
            try container.decodeIfPresent(String.self, forKey: .readOnlyReason)
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }

    var canPromote: Bool {
        !readOnly && (identity.role == "worker" || identity.role == "manager")
    }

    var canTransfer: Bool {
        !readOnly && identity.role != "leader"
    }

    var canRetire: Bool {
        !readOnly && identity.status != "retired"
    }

    var pendingHireRequest: AgentHireRequest? {
        hireRequests.first(where: { $0.status == "pending" })
    }

    var pendingRetirementProposal: AgentRetirementProposal? {
        retirementProposals.first(where: { $0.status == "pending" })
    }

    var memoryPreview: [AgentMemory] {
        Array(memories.prefix(5))
    }
}

extension AgentOrgNode {
    static func fallback(identity: AgentIdentity) -> AgentOrgNode {
        AgentOrgNode(
            identity: identity,
            activeAssignment: nil,
            workspace: nil,
            unreadInbox: 0,
            memoryCount: 0,
            performance: .empty,
            retirement: .empty,
            reports: [],
            reportCount: 0
        )
    }

    init(
        identity: AgentIdentity,
        activeAssignment: AgentOrgAssignmentState?,
        workspace: AgentOrgWorkspaceState?,
        unreadInbox: Int,
        memoryCount: Int,
        performance: AgentOrgPerformanceSummary,
        retirement: AgentOrgRetirementState,
        reports: [AgentOrgNode],
        reportCount: Int
    ) {
        self.identity = identity
        self.activeAssignment = activeAssignment
        self.workspace = workspace
        self.unreadInbox = unreadInbox
        self.memoryCount = memoryCount
        self.performance = performance
        self.retirement = retirement
        self.reports = reports
        self.reportCount = reportCount
    }
}

extension AgentPerformance {
    static func fallback(projectId: String, identity: AgentIdentity) -> AgentPerformance {
        AgentPerformance(
            projectId: projectId,
            identityId: identity.id,
            role: identity.role,
            status: identity.status,
            lookbackDays: 30,
            completedTasks: 0,
            failedTasks: 0,
            cancelledTasks: 0,
            blockedTasks: 0,
            skippedTasks: 0,
            consecutiveFailures: 0,
            retries: 0,
            verifierApprovals: 0,
            verifierRejections: 0,
            unsubstantiatedHandoffs: 0,
            falseConsensus: 0,
            diagnosisFindings: 0,
            assignmentCount: 0,
            sessionCount: 0,
            inputTokens: 0,
            outputTokens: 0,
            costUsd: 0,
            lastActiveAt: nil,
            computedAt: nil,
            reasonCounts: [],
            evidence: []
        )
    }
}

// MARK: - Console selection and mutations

/// The identity detail sheet selection. A tiny Identifiable wrapper lets
/// SwiftUI present the sheet without deriving business state in the view.
struct ProjectOrgIdentitySelection: Identifiable, Equatable, Sendable {
    let identityID: String

    var id: String {
        identityID
    }
}

/// One action sheet at a time. The identity subject is separate so the hire
/// sheet (no subject) and lifecycle sheets share one presentation path.
enum ProjectOrgSheet: String, Identifiable, Equatable, Sendable {
    case hire
    case promote
    case transfer
    case retire

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .hire: "Hire identity"
        case .promote: "Promote identity"
        case .transfer: "Transfer identity"
        case .retire: "Retire identity"
        }
    }
}

struct ProjectOrgHireDraft: Equatable, Sendable {
    var role = "worker"
    var department = ""
    var component = ""
    var title = ""
    var profile = "coding"
    var model = ""
    var parentIdentityID = ""
    var approved = false

    var normalizedRole: String {
        role.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    var normalizedTitle: String {
        title.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    func validationError() -> String? {
        if normalizedRole != "manager", normalizedRole != "worker" {
            return "Role must be Manager or Worker. The Leader is bootstrapped, not hired."
        }
        if normalizedTitle.isEmpty {
            return "A title is required for the org chart."
        }
        if profile.isEmpty {
            return "Select an explicit role profile."
        }
        return nil
    }

    var approvalNote: String {
        approved
            ? "The request will be approved immediately and create the identity."
            : "The request will stay pending until an operator approves it."
    }
}

struct ProjectOrgPromoteDraft: Equatable, Sendable {
    var identityID = ""
    var currentRole = "worker"
    var targetRole = "manager"
    var parentIdentityID = ""
    var title = ""
    var force = false

    func validationError() -> String? {
        let allowed: [String] = switch currentRole {
        case "worker": ["manager"]
        case "manager": ["leader"]
        default: []
        }
        if allowed.isEmpty {
            return "Only Worker→Manager and Manager→Leader are supported."
        }
        if !allowed.contains(targetRole) {
            return "Invalid promotion target for \(currentRole)."
        }
        return nil
    }

    var warning: String {
        targetRole == "leader"
            ? "Leader promotion requires that no live Leader exists and is approval-classified."
            : "Promotion increments the generation and is approval-classified. Active work is a conflict unless force is set."
    }
}

/// One field of a transfer. `unchanged` is omitted from the JSON-RPC params,
/// `clear` sends an explicit JSON null, and `value` sends the new string. This
/// keeps an ordinary edit from accidentally clearing an unrelated field.
enum ProjectOrgPatchField: Equatable, Sendable {
    case unchanged
    case clear
    case value(String)
}

struct ProjectOrgTransferDraft: Equatable, Sendable {
    var identityID = ""
    var department = ""
    var component = ""
    var title = ""
    var parentIdentityID = ""
    var clearParent = false
    var force = false

    func patch(
        current: String,
        draft: String
    ) -> ProjectOrgPatchField {
        let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let currentTrimmed = current.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed == currentTrimmed {
            return .unchanged
        }
        if trimmed.isEmpty {
            return .clear
        }
        return .value(trimmed)
    }

    func hasChanges(
        currentDepartment: String,
        currentComponent: String,
        currentTitle: String,
        currentParent: String
    ) -> Bool {
        patch(current: currentDepartment, draft: department) != .unchanged
            || patch(current: currentComponent, draft: component) != .unchanged
            || patch(current: currentTitle, draft: title) != .unchanged
            || clearParent
            || patch(current: currentParent, draft: parentIdentityID) != .unchanged
    }

    var approvalNote: String {
        "Transfer is approval-classified and increments the generation. Active work is a conflict unless force is set."
    }
}

struct ProjectOrgRetireDraft: Equatable, Sendable {
    var identityID = ""
    var successorID = ""
    var proposalID = ""
    var approved = false
    var force = false
    var reason = ""

    var requiresApproval = true

    func validationError() -> String? {
        if requiresApproval, !approved, proposalID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return "Retirement requires explicit approval unless an approved proposal is attached."
        }
        return nil
    }

    var warning: String {
        "Retirement is terminal and irreversible from the console. Evidence, branches, memory, and performance are preserved; rehire under the same id is impossible."
    }
}

/// Typed UI error surface for the org console. It preserves the daemon's stable
/// Q3 code and bounded hint instead of flattening every failure to a string.
struct ProjectOrgErrorSurface: Equatable, Identifiable, Sendable {
    let code: String?
    let message: String
    let hint: String?

    var id: String {
        "\(code ?? "error")#\(message)"
    }

    static func from(_ error: Error) -> ProjectOrgErrorSurface {
        if let core = error as? CoreError {
            return ProjectOrgErrorSurface(
                code: core.typedCode,
                message: core.errorDescription ?? "The daemon rejected the request.",
                hint: core.typedHint
            )
        }
        return ProjectOrgErrorSurface(
            code: nil,
            message: error.localizedDescription,
            hint: nil
        )
    }

    var displayText: String {
        var parts = [message]
        if let code, !code.isEmpty {
            parts.append("[\(code)]")
        }
        if let hint, !hint.isEmpty {
            parts.append(hint)
        }
        return parts.joined(separator: " · ")
    }
}

struct AgentPromoteResult: Codable, Sendable {
    let identity: AgentIdentity
    let previousRole: String
    let changed: Bool
    let releasedAssignments: Int
    let archivedWorkspaces: Int
    let employmentEventId: String?

    enum CodingKeys: String, CodingKey {
        case identity
        case previousRole = "previous_role"
        case changed
        case releasedAssignments = "released_assignments"
        case archivedWorkspaces = "archived_workspaces"
        case employmentEventId = "employment_event_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        identity = try container.decode(AgentIdentity.self, forKey: .identity)
        previousRole = try container.decodeIfPresent(String.self, forKey: .previousRole) ?? ""
        changed = try container.decodeIfPresent(Bool.self, forKey: .changed) ?? false
        releasedAssignments =
            try container.decodeIfPresent(Int.self, forKey: .releasedAssignments) ?? 0
        archivedWorkspaces =
            try container.decodeIfPresent(Int.self, forKey: .archivedWorkspaces) ?? 0
        employmentEventId =
            try container.decodeIfPresent(String.self, forKey: .employmentEventId)
    }
}

struct AgentTransferResult: Codable, Sendable {
    let identity: AgentIdentity
    let changed: Bool
    let releasedAssignments: Int
    let archivedWorkspaces: Int
    let employmentEventId: String?

    enum CodingKeys: String, CodingKey {
        case identity, changed
        case releasedAssignments = "released_assignments"
        case archivedWorkspaces = "archived_workspaces"
        case employmentEventId = "employment_event_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        identity = try container.decode(AgentIdentity.self, forKey: .identity)
        changed = try container.decodeIfPresent(Bool.self, forKey: .changed) ?? false
        releasedAssignments =
            try container.decodeIfPresent(Int.self, forKey: .releasedAssignments) ?? 0
        archivedWorkspaces =
            try container.decodeIfPresent(Int.self, forKey: .archivedWorkspaces) ?? 0
        employmentEventId =
            try container.decodeIfPresent(String.self, forKey: .employmentEventId)
    }
}

struct AgentRetirementDecisionResult: Codable, Sendable {
    let proposal: AgentRetirementProposal
    let changed: Bool
}

struct AgentRetirementProposeResult: Codable, Sendable {
    let proposal: AgentRetirementProposal
    let candidate: AgentRetirementCandidate?
    let alreadyPending: Bool
    let changed: Bool

    enum CodingKeys: String, CodingKey {
        case proposal, candidate
        case alreadyPending = "already_pending"
        case changed
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        proposal = try container.decode(AgentRetirementProposal.self, forKey: .proposal)
        candidate = try container.decodeIfPresent(AgentRetirementCandidate.self, forKey: .candidate)
        alreadyPending =
            try container.decodeIfPresent(Bool.self, forKey: .alreadyPending) ?? false
        changed = try container.decodeIfPresent(Bool.self, forKey: .changed) ?? false
    }
}
