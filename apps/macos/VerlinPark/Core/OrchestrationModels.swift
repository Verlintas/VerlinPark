import Foundation

enum CollabTaskCommand: String, CaseIterable, Identifiable, Sendable {
    case retry
    case pause
    case resume
    case skip
    case cancel

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .retry: L10n.t("Retry")
        case .pause: L10n.t("Pause")
        case .resume: L10n.t("Resume")
        case .skip: L10n.t("Skip")
        case .cancel: L10n.t("Cancel")
        }
    }

    var icon: String {
        switch self {
        case .retry: "arrow.clockwise"
        case .pause: "pause.fill"
        case .resume: "play.fill"
        case .skip: "forward.end.fill"
        case .cancel: "xmark.octagon"
        }
    }
}

/// Deterministic orchestration policy exposed by the daemon. Model routing is
/// never derived here; the policy only controls retries, reviews, and dependency
/// handling.
struct OrchestrationPolicy: Codable, Equatable, Sendable {
    var autoRetryFailed: Bool
    var maxAttempts: Int
    var retryBackoffSecs: Int
    var retryPriorityBoost: Int
    var dependencyFailure: String
    var escalateAfterRetries: Bool
    var autoResolveBlockers: Bool

    static let standard = OrchestrationPolicy(
        autoRetryFailed: true,
        maxAttempts: 2,
        retryBackoffSecs: 30,
        retryPriorityBoost: 10,
        dependencyFailure: "block",
        escalateAfterRetries: true,
        autoResolveBlockers: true
    )

    enum CodingKeys: String, CodingKey {
        case autoRetryFailed = "auto_retry_failed"
        case maxAttempts = "max_attempts"
        case retryBackoffSecs = "retry_backoff_secs"
        case retryPriorityBoost = "retry_priority_boost"
        case dependencyFailure = "dependency_failure"
        case escalateAfterRetries = "escalate_after_retries"
        case autoResolveBlockers = "auto_resolve_blockers"
    }
}

struct OrchestrationPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: OrchestrationPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct OrchestrationTask: Codable, Identifiable, Hashable, Sendable {
    let taskId: String
    let department: String
    let title: String
    let kind: String
    let priority: Int
    let status: String
    let deps: [String]
    let writeScope: [String]
    let acceptance: [String]
    let attempts: Int
    let maxAttempts: Int
    let ownerAgentId: String?
    let ownerAgentName: String?
    let lastError: String?
    let resultSummary: String?
    let verifierStatus: String?
    let nextAttemptAt: String?
    let ready: Bool
    let blockedBy: [String]
    let startedAt: String?
    let finishedAt: String?

    var id: String {
        taskId
    }

    enum CodingKeys: String, CodingKey {
        case taskId = "task_id"
        case department
        case title
        case kind
        case priority
        case status
        case deps
        case writeScope = "write_scope"
        case acceptance
        case attempts
        case maxAttempts = "max_attempts"
        case ownerAgentId = "owner_agent_id"
        case ownerAgentName = "owner_agent_name"
        case lastError = "last_error"
        case resultSummary = "result_summary"
        case verifierStatus = "verifier_status"
        case nextAttemptAt = "next_attempt_at"
        case ready
        case blockedBy = "blocked_by"
        case startedAt = "started_at"
        case finishedAt = "finished_at"
    }
}

struct OrchestrationDepartment: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let managerTitle: String
    let profile: String
    let objective: String
    let totalTasks: Int
    let completedTasks: Int
    let activeTasks: Int
    let blockedTasks: Int
    let failedTasks: Int
    let managerAgentId: String?

    var id: String {
        name
    }

    enum CodingKeys: String, CodingKey {
        case name
        case managerTitle = "manager_title"
        case profile
        case objective
        case totalTasks = "total_tasks"
        case completedTasks = "completed_tasks"
        case activeTasks = "active_tasks"
        case blockedTasks = "blocked_tasks"
        case failedTasks = "failed_tasks"
        case managerAgentId = "manager_agent_id"
    }
}

struct OrchestrationNote: Codable, Identifiable, Hashable, Sendable {
    let noteId: String
    let action: String
    let severity: String
    let status: String
    let taskId: String?
    let targetAgentId: String?
    let agentId: String
    let agentName: String
    let message: String
    let createdAt: String
    let resolvedAt: String?

    var id: String {
        noteId
    }

    enum CodingKeys: String, CodingKey {
        case noteId = "note_id"
        case action
        case severity
        case status
        case taskId = "task_id"
        case targetAgentId = "target_agent_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case message
        case createdAt = "created_at"
        case resolvedAt = "resolved_at"
    }
}

struct OrchestrationDecision: Codable, Identifiable, Hashable, Sendable {
    let decisionId: String
    let action: String
    let taskId: String?
    let detail: String
    let attempt: Int
    let agentId: String
    let createdAt: String

    var id: String {
        decisionId
    }

    var icon: String {
        switch action {
        case "retry": "arrow.clockwise"
        case "block": "hand.raised"
        case "unblock": "lock.open"
        case "cancel": "xmark.octagon"
        case "escalate": "exclamationmark.bubble"
        case "resolve_blockers": "checkmark.bubble"
        default: "gearshape"
        }
    }

    enum CodingKeys: String, CodingKey {
        case decisionId = "decision_id"
        case action
        case taskId = "task_id"
        case detail
        case attempt
        case agentId = "agent_id"
        case createdAt = "created_at"
    }
}

struct OrchestrationBlocker: Codable, Identifiable, Hashable, Sendable {
    let kind: String
    let noteId: String?
    let taskId: String?
    let agentId: String?
    let agentName: String?
    let message: String
    let createdAt: String

    var id: String {
        "\(kind)-\(taskId ?? agentId ?? createdAt)-\(message.prefix(24))"
    }

    enum CodingKeys: String, CodingKey {
        case kind
        case noteId = "note_id"
        case taskId = "task_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case message
        case createdAt = "created_at"
    }
}

struct OrchestrationMetrics: Codable, Equatable, Sendable {
    let totalAgents: Int
    let runningAgents: Int
    let queuedAgents: Int
    let failedAgents: Int
    let totalTasks: Int
    let completedTasks: Int
    let activeTasks: Int
    let readyTasks: Int
    let blockedTasks: Int
    let failedTasks: Int
    let retryingTasks: Int
    let openBlockers: Int
    let mergeQueueDepth: Int
    let mergeConflicts: Int
    let attentionCount: Int

    enum CodingKeys: String, CodingKey {
        case totalAgents = "total_agents"
        case runningAgents = "running_agents"
        case queuedAgents = "queued_agents"
        case failedAgents = "failed_agents"
        case totalTasks = "total_tasks"
        case completedTasks = "completed_tasks"
        case activeTasks = "active_tasks"
        case readyTasks = "ready_tasks"
        case blockedTasks = "blocked_tasks"
        case failedTasks = "failed_tasks"
        case retryingTasks = "retrying_tasks"
        case openBlockers = "open_blockers"
        case mergeQueueDepth = "merge_queue_depth"
        case mergeConflicts = "merge_conflicts"
        case attentionCount = "attention_count"
    }
}

struct PlanRevisionInfo: Codable, Identifiable, Hashable, Sendable {
    let revision: Int
    let seq: UInt64
    let kind: String
    let reason: String?
    let author: String
    let taskCount: Int
    let added: [String]
    let removed: [String]
    let changed: [String]
    let createdAt: String

    var id: UInt64 {
        seq
    }

    var kindLabel: String {
        kind == "team_plan.revised" ? "Revision" : "Submitted"
    }

    var diffLabel: String {
        var parts: [String] = []
        if !added.isEmpty {
            parts.append("+\(added.joined(separator: ","))")
        }
        if !removed.isEmpty {
            parts.append("-\(removed.joined(separator: ","))")
        }
        if !changed.isEmpty {
            parts.append("~\(changed.joined(separator: ","))")
        }
        return parts.isEmpty ? "no task changes" : parts.joined(separator: " ")
    }

    enum CodingKeys: String, CodingKey {
        case revision
        case seq
        case kind
        case reason
        case author
        case taskCount = "task_count"
        case added
        case removed
        case changed
        case createdAt = "created_at"
    }
}

struct OrchestrationSnapshot: Codable, Sendable {
    let sessionId: String
    let projectId: String
    let goal: String
    let status: String
    let plan: TeamPlan?
    let planRevision: Int
    let policy: OrchestrationPolicy
    let departments: [OrchestrationDepartment]
    let tasks: [OrchestrationTask]
    let agents: [AgentRecord]
    let notes: [OrchestrationNote]
    let decisions: [OrchestrationDecision]
    let revisions: [PlanRevisionInfo]
    let blockers: [OrchestrationBlocker]
    let metrics: OrchestrationMetrics
    let generatedAt: String

    var openBlockerNotes: [OrchestrationNote] {
        notes.filter { $0.status == "open" }
    }

    var recentNotes: [OrchestrationNote] {
        notes.sorted { $0.createdAt > $1.createdAt }
    }

    var recentDecisions: [OrchestrationDecision] {
        decisions.sorted { $0.createdAt > $1.createdAt }
    }

    func task(_ taskID: String) -> OrchestrationTask? {
        tasks.first { $0.taskId == taskID }
    }

    func blockers(for taskID: String) -> [OrchestrationBlocker] {
        blockers.filter { $0.taskId == taskID }
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case projectId = "project_id"
        case goal
        case status
        case plan
        case planRevision = "plan_revision"
        case policy
        case departments
        case tasks
        case agents
        case notes
        case decisions
        case revisions
        case blockers
        case metrics
        case generatedAt = "generated_at"
    }
}

struct OrchestrationSnapshotResult: Codable, Sendable {
    let snapshot: OrchestrationSnapshot
}

struct OrchestrationPreviewAction: Codable, Identifiable, Hashable, Sendable {
    let action: String
    let taskId: String
    let detail: String
    let attempt: Int

    enum CodingKeys: String, CodingKey {
        case action
        case taskId = "task_id"
        case detail
        case attempt
    }

    var id: String {
        "\(action)-\(taskId)-\(attempt)"
    }

    var icon: String {
        switch action {
        case "retry": "arrow.clockwise"
        case "block": "hand.raised"
        case "unblock": "lock.open"
        case "cancel": "xmark.octagon"
        case "escalate": "exclamationmark.bubble"
        case "resolve_blockers": "checkmark.bubble"
        default: "gearshape"
        }
    }
}

struct OrchestrationPreviewResult: Codable, Sendable {
    let sessionId: String
    let actions: [OrchestrationPreviewAction]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case actions
    }
}

struct OrchestrationAdvanceResult: Codable, Sendable {
    let sessionId: String
    let actions: [OrchestrationDecision]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case actions
    }
}

struct PlanRevisionDetailResult: Codable, Sendable {
    let revision: PlanRevisionInfo
    let plan: TeamPlan
}

struct PlanRevisionListResult: Codable, Sendable {
    let revisions: [PlanRevisionInfo]
}

struct ReviseTeamPlanResult: Codable, Sendable {
    let revision: PlanRevisionInfo
    let plan: TeamPlan
}

struct CoordinationNoteResult: Codable, Sendable {
    let noteId: String

    enum CodingKeys: String, CodingKey {
        case noteId = "note_id"
    }
}

struct RuntimeLease: Codable, Identifiable, Hashable, Sendable {
    let leaseKey: String
    let ownerId: String
    let kind: String
    let sessionId: String?
    let acquiredAt: String
    let heartbeatAt: String
    let expiresAt: String

    var id: String {
        leaseKey
    }

    var icon: String {
        kind == "session" ? "person.badge.clock" : "lock.shield"
    }

    enum CodingKeys: String, CodingKey {
        case leaseKey = "lease_key"
        case ownerId = "owner_id"
        case kind
        case sessionId = "session_id"
        case acquiredAt = "acquired_at"
        case heartbeatAt = "heartbeat_at"
        case expiresAt = "expires_at"
    }
}

struct RuntimeLeaseListResult: Codable, Sendable {
    let runtimeId: String
    let leases: [RuntimeLease]

    enum CodingKeys: String, CodingKey {
        case runtimeId = "runtime_id"
        case leases
    }
}

struct RuntimeLeaseSweepResult: Codable, Sendable {
    let removed: Int
}

struct OrchestrationSessionSummary: Codable, Identifiable, Sendable {
    let sessionId: String
    let projectId: String
    let projectName: String
    let goal: String
    let status: String
    let priority: Int
    let planRevision: Int
    let metrics: OrchestrationMetrics
    let blockers: [String]
    let lastDecision: String?
    let lastDecisionAt: String?

    var id: String {
        sessionId
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case projectId = "project_id"
        case projectName = "project_name"
        case goal
        case status
        case priority
        case planRevision = "plan_revision"
        case metrics
        case blockers
        case lastDecision = "last_decision"
        case lastDecisionAt = "last_decision_at"
    }
}

struct OrchestrationOverviewTotals: Codable, Hashable, Sendable {
    let sessions: Int
    let running: Int
    let attention: Int
    let blockedTasks: Int
    let failedTasks: Int
    let retryingTasks: Int
    let openBlockers: Int
    let mergeConflicts: Int

    enum CodingKeys: String, CodingKey {
        case sessions
        case running
        case attention
        case blockedTasks = "blocked_tasks"
        case failedTasks = "failed_tasks"
        case retryingTasks = "retrying_tasks"
        case openBlockers = "open_blockers"
        case mergeConflicts = "merge_conflicts"
    }
}

struct OrchestrationOverviewResult: Codable, Sendable {
    let sessions: [OrchestrationSessionSummary]
    let totals: OrchestrationOverviewTotals
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case sessions
        case totals
        case generatedAt = "generated_at"
    }
}

// MARK: - Long-task operator controls

/// One persisted operator steering request. Delivery is cooperative: the agent
/// loop drains pending rows immediately before its next provider request.
struct SteeringMessage: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let agentId: String
    let taskId: String?
    let body: String
    let status: String
    let createdAt: String
    let deliveredAt: String?

    var isPending: Bool {
        status == "pending"
    }

    var isTerminal: Bool {
        status == "delivered" || status == "cancelled"
    }

    var displayStatus: String {
        switch status {
        case "pending": L10n.t("Pending")
        case "delivered": L10n.t("Delivered")
        case "cancelled": L10n.t("Cancelled")
        default: status
        }
    }

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case agentId = "agent_id"
        case taskId = "task_id"
        case body
        case status
        case createdAt = "created_at"
        case deliveredAt = "delivered_at"
    }
}

struct SessionSteerResult: Codable, Sendable {
    let steering: SteeringMessage
}

struct SessionSteeringResult: Codable, Sendable {
    let sessionId: String
    let steering: [SteeringMessage]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case steering
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        steering = try container.decodeIfPresent([SteeringMessage].self, forKey: .steering) ?? []
    }
}

struct SessionSteeringCancelResult: Codable, Sendable {
    let cancelled: Bool
    let steering: SteeringMessage
}

/// Compact outcome captured when a deferred continuation wakes. It is frozen
/// at wake time so later plan revisions cannot rewrite the wake context.
struct ContinuationOutcome: Codable, Identifiable, Hashable, Sendable {
    let taskId: String
    let title: String
    let status: String

    var id: String {
        taskId
    }

    enum CodingKeys: String, CodingKey {
        case taskId = "task_id"
        case title
        case status
    }
}

/// A persisted, operator-created deferred continuation. It never adds a plan
/// task or bypasses a dependency, approval, budget, or verification gate.
struct SessionContinuation: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let projectId: String
    let body: String
    let watchTaskIds: [String]
    let watchMode: String
    let deadlineAt: String?
    let status: String
    let attempts: Int
    let maxAttempts: Int
    let wakeReason: String?
    let outcomes: [ContinuationOutcome]
    let lastError: String?
    let nextAttemptAt: String?
    let createdAt: String
    let readyAt: String?
    let deliveredAt: String?
    let updatedAt: String

    /// Waiting continuations are `pending` plus `ready` rows that the scheduler
    /// has not claimed for delivery yet.
    var isPending: Bool {
        status == "pending" || status == "ready"
    }

    var isTerminal: Bool {
        status == "delivered" || status == "cancelled" || status == "failed"
    }

    var isCancelable: Bool {
        status == "pending" || status == "ready" || status == "delivering"
    }

    var displayStatus: String {
        switch status {
        case "pending": L10n.t("Pending")
        case "ready": L10n.t("Ready")
        case "delivering": L10n.t("Delivering")
        case "delivered": L10n.t("Delivered")
        case "cancelled": L10n.t("Cancelled")
        case "failed": L10n.t("Failed")
        default: status
        }
    }

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case projectId = "project_id"
        case body
        case watchTaskIds = "watch_task_ids"
        case watchMode = "watch_mode"
        case deadlineAt = "deadline_at"
        case status
        case attempts
        case maxAttempts = "max_attempts"
        case wakeReason = "wake_reason"
        case outcomes
        case lastError = "last_error"
        case nextAttemptAt = "next_attempt_at"
        case createdAt = "created_at"
        case readyAt = "ready_at"
        case deliveredAt = "delivered_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        projectId = try container.decode(String.self, forKey: .projectId)
        body = try container.decode(String.self, forKey: .body)
        watchTaskIds = try container.decodeIfPresent([String].self, forKey: .watchTaskIds) ?? []
        watchMode = try container.decodeIfPresent(String.self, forKey: .watchMode) ?? "all"
        deadlineAt = try container.decodeIfPresent(String.self, forKey: .deadlineAt)
        status = try container.decode(String.self, forKey: .status)
        attempts = try container.decodeIfPresent(Int.self, forKey: .attempts) ?? 0
        maxAttempts = try container.decodeIfPresent(Int.self, forKey: .maxAttempts) ?? 3
        wakeReason = try container.decodeIfPresent(String.self, forKey: .wakeReason)
        outcomes = try container.decodeIfPresent([ContinuationOutcome].self, forKey: .outcomes) ?? []
        lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
        nextAttemptAt = try container.decodeIfPresent(String.self, forKey: .nextAttemptAt)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        readyAt = try container.decodeIfPresent(String.self, forKey: .readyAt)
        deliveredAt = try container.decodeIfPresent(String.self, forKey: .deliveredAt)
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? createdAt
    }

    /// The daemon only starts a continuation when the session is quiescent and
    /// startable through the ordinary `start` path.
    func isStartable(sessionStatus: String) -> Bool {
        LongTaskRules.isSessionStartable(sessionStatus)
    }
}

struct SessionDeferResult: Codable, Sendable {
    let continuation: SessionContinuation
}

struct SessionContinuationsResult: Codable, Sendable {
    let sessionId: String
    let continuations: [SessionContinuation]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case continuations
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        continuations =
            try container.decodeIfPresent([SessionContinuation].self, forKey: .continuations) ?? []
    }
}

struct SessionContinuationCancelResult: Codable, Sendable {
    let cancelled: Bool
    let continuation: SessionContinuation
}

/// One policy-gated stall recovery row. `observe` never writes one; only a
/// project policy set to `escalate` or `retry` creates recoveries.
struct OrchestrationRecovery: Codable, Identifiable, Hashable, Sendable {
    let sessionId: String
    let projectId: String
    let planSeq: UInt64
    let taskId: String
    let department: String
    let action: String
    let status: String
    let attempts: Int
    let lastStallCount: Int
    let requestedAt: String
    let updatedAt: String
    let lastError: String?
    let nextActionAt: String?

    var id: String {
        "\(sessionId)/\(planSeq)/\(taskId)"
    }

    var isActiveRecovery: Bool {
        status == "requested" || status == "waiting_cancel"
    }

    var isTerminal: Bool {
        !isActiveRecovery
    }

    var displayAction: String {
        switch action {
        case "retry": L10n.t("Retry")
        case "escalate": L10n.t("Escalate")
        default: action
        }
    }

    var displayStatus: String {
        switch status {
        case "requested": L10n.t("Requested")
        case "waiting_cancel": L10n.t("Waiting for cancellation")
        case "retried": L10n.t("Retried")
        case "escalated": L10n.t("Escalated")
        case "failed": L10n.t("Failed")
        case "unnecessary": L10n.t("Unnecessary")
        default: status
        }
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case projectId = "project_id"
        case planSeq = "plan_seq"
        case taskId = "task_id"
        case department
        case action
        case status
        case attempts
        case lastStallCount = "last_stall_count"
        case requestedAt = "requested_at"
        case updatedAt = "updated_at"
        case lastError = "last_error"
        case nextActionAt = "next_action_at"
    }
}

struct OrchestrationRecoveriesResult: Codable, Sendable {
    let sessionId: String
    let recoveries: [OrchestrationRecovery]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case recoveries
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        recoveries =
            try container.decodeIfPresent([OrchestrationRecovery].self, forKey: .recoveries) ?? []
    }
}

/// Runtime progress telemetry for a task-scoped agent run. This is
/// observation state, not a replayed projection.
struct PlanTaskProgress: Codable, Identifiable, Hashable, Sendable {
    let sessionId: String
    let projectId: String
    let planSeq: UInt64
    let taskId: String
    let lastProgressAt: String?
    let stallCount: Int
    let lastTurn: Int?
    let lastTool: String?
    let updatedAt: String

    var id: String {
        "\(sessionId)/\(planSeq)/\(taskId)"
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case projectId = "project_id"
        case planSeq = "plan_seq"
        case taskId = "task_id"
        case lastProgressAt = "last_progress_at"
        case stallCount = "stall_count"
        case lastTurn = "last_turn"
        case lastTool = "last_tool"
        case updatedAt = "updated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        projectId = try container.decode(String.self, forKey: .projectId)
        planSeq = try container.decode(UInt64.self, forKey: .planSeq)
        taskId = try container.decode(String.self, forKey: .taskId)
        lastProgressAt = try container.decodeIfPresent(String.self, forKey: .lastProgressAt)
        stallCount = try container.decodeIfPresent(Int.self, forKey: .stallCount) ?? 0
        lastTurn = try container.decodeIfPresent(Int.self, forKey: .lastTurn)
        lastTool = try container.decodeIfPresent(String.self, forKey: .lastTool)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
    }
}

/// One observability-only stall observation. The sweep never cancels or
/// retries a task by itself.
struct TaskStall: Codable, Identifiable, Hashable, Sendable {
    let sessionId: String
    let projectId: String
    let planSeq: UInt64
    let taskId: String
    let department: String
    let title: String
    let status: String
    let attempts: Int
    let ageSecs: UInt64
    let stallCount: Int
    let lastProgressAt: String
    let lastTurn: Int?
    let lastTool: String?
    let observedAt: String

    var id: String {
        "\(sessionId)/\(planSeq)/\(taskId)/\(observedAt)"
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case projectId = "project_id"
        case planSeq = "plan_seq"
        case taskId = "task_id"
        case department
        case title
        case status
        case attempts
        case ageSecs = "age_secs"
        case stallCount = "stall_count"
        case lastProgressAt = "last_progress_at"
        case lastTurn = "last_turn"
        case lastTool = "last_tool"
        case observedAt = "observed_at"
    }
}

struct SessionStallsResult: Codable, Sendable {
    let sessionId: String
    let thresholdSecs: UInt64
    let progress: [PlanTaskProgress]
    let stalls: [TaskStall]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case thresholdSecs = "threshold_secs"
        case progress
        case stalls
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        thresholdSecs = try container.decodeIfPresent(UInt64.self, forKey: .thresholdSecs) ?? 0
        progress = try container.decodeIfPresent([PlanTaskProgress].self, forKey: .progress) ?? []
        stalls = try container.decodeIfPresent([TaskStall].self, forKey: .stalls) ?? []
    }
}

enum LongTaskValidationError: Error, Equatable, Sendable {
    case emptyBody
    case bodyTooLong
    case emptyWakeCondition
    case invalidWatchMode
    case tooManyWatchTasks
    case invalidAfterMinutes
    case deadlineInPast
    case deadlineTooFar

    var message: String {
        switch self {
        case .emptyBody:
            L10n.t("Enter a prompt for this action.")
        case .bodyTooLong:
            L10n.t("Keep the prompt at 4,000 characters or fewer.")
        case .emptyWakeCondition:
            L10n.t("Watch at least one task or add a deadline.")
        case .invalidWatchMode:
            L10n.t("Choose whether all watched tasks or any watched task should wake the run.")
        case .tooManyWatchTasks:
            L10n.t("A continuation may watch at most 100 tasks.")
        case .invalidAfterMinutes:
            L10n.t("Enter a deadline between 1 and 43,200 minutes.")
        case .deadlineInPast:
            L10n.t("The deadline must be in the future.")
        case .deadlineTooFar:
            L10n.t("The deadline must be within 30 days.")
        }
    }
}

/// Pure, testable validation for the long-task operator sheets. It mirrors the
/// daemon limits so the UI can explain a bad input before issuing an RPC; the
/// daemon remains the authority.
enum LongTaskRules {
    static let maxBodyChars = 4000
    static let maxWatchTasks = 100
    static let maxDeadlineSecs: TimeInterval = 30 * 24 * 60 * 60

    static func validateSteeringBody(_ body: String) -> Result<String, LongTaskValidationError> {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .failure(.emptyBody) }
        guard trimmed.count <= maxBodyChars else { return .failure(.bodyTooLong) }
        return .success(trimmed)
    }

    static func isSessionStartable(_ status: String) -> Bool {
        ["idle", "failed", "cancelled"].contains(status)
    }

    static func isTerminalTaskStatus(_ status: String) -> Bool {
        ["completed", "failed", "cancelled", "skipped", "interrupted"].contains(status)
    }

    static func normalizeWatchMode(_ mode: String) -> Result<String, LongTaskValidationError> {
        let normalized = mode.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard normalized == "all" || normalized == "any" else {
            return .failure(.invalidWatchMode)
        }
        return .success(normalized)
    }

    static func validateDefer(
        body: String,
        watchTaskIDs: [String],
        watchMode: String,
        afterMinutes: Int?,
        deadline: Date?,
        now: Date = Date()
    ) -> Result<LongTaskDeferPayload, LongTaskValidationError> {
        let trimmed: String
        switch validateSteeringBody(body) {
        case let .success(value):
            trimmed = value
        case let .failure(error):
            return .failure(error)
        }

        var seen: Set<String> = []
        let watchIDs = watchTaskIDs
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .filter { seen.insert($0).inserted }
        guard watchIDs.count <= maxWatchTasks else { return .failure(.tooManyWatchTasks) }

        let mode: String
        switch normalizeWatchMode(watchMode) {
        case let .success(value):
            mode = value
        case let .failure(error):
            return .failure(error)
        }

        if afterMinutes != nil, deadline != nil {
            return .failure(.invalidAfterMinutes)
        }
        guard !watchIDs.isEmpty || afterMinutes != nil || deadline != nil else {
            return .failure(.emptyWakeCondition)
        }

        var afterSecs: UInt64?
        var deadlineAt: String?
        if let minutes = afterMinutes {
            guard minutes > 0, Double(minutes) * 60 <= maxDeadlineSecs else {
                return .failure(.invalidAfterMinutes)
            }
            afterSecs = UInt64(minutes) * 60
        } else if let deadline {
            let interval = deadline.timeIntervalSince(now)
            guard interval > 0 else { return .failure(.deadlineInPast) }
            guard interval <= maxDeadlineSecs else { return .failure(.deadlineTooFar) }
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime]
            formatter.timeZone = TimeZone(secondsFromGMT: 0)
            deadlineAt = formatter.string(from: deadline)
        }

        return .success(
            LongTaskDeferPayload(
                body: trimmed,
                watchTaskIDs: watchIDs,
                watchMode: mode,
                afterSecs: afterSecs,
                deadlineAt: deadlineAt
            )
        )
    }
}

struct LongTaskDeferPayload: Equatable, Sendable {
    let body: String
    let watchTaskIDs: [String]
    let watchMode: String
    let afterSecs: UInt64?
    let deadlineAt: String?
}
