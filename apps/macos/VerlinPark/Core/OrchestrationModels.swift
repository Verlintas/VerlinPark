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
