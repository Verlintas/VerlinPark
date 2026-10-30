import Foundation

struct ShellPolicy: Codable, Hashable, Sendable {
    var network: String
    var readDeny: [String]

    static let allow = ShellPolicy(network: "allow", readDeny: [])

    enum CodingKeys: String, CodingKey {
        case network
        case readDeny = "read_deny"
    }
}

struct IntegrationPolicy: Codable, Hashable, Sendable {
    var autoResolve: Bool
    var maxAutoAttempts: Int
    var maxResolveTokens: Int?

    static let disabled = IntegrationPolicy(
        autoResolve: false,
        maxAutoAttempts: 1,
        maxResolveTokens: nil
    )

    enum CodingKeys: String, CodingKey {
        case autoResolve = "auto_resolve"
        case maxAutoAttempts = "max_auto_attempts"
        case maxResolveTokens = "max_resolve_tokens"
    }
}

struct IntegrationPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: IntegrationPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct ArtifactPolicy: Codable, Hashable, Sendable {
    var autoGc: Bool
    var maxBytes: Int?
    var maxAgeSecs: Int?
    var maxCount: Int?

    static let disabled = ArtifactPolicy(
        autoGc: false,
        maxBytes: nil,
        maxAgeSecs: nil,
        maxCount: nil
    )

    enum CodingKeys: String, CodingKey {
        case autoGc = "auto_gc"
        case maxBytes = "max_bytes"
        case maxAgeSecs = "max_age_secs"
        case maxCount = "max_count"
    }
}

struct MailboxPolicy: Codable, Hashable, Sendable {
    var maxAgeSecs: Int?
    var maxCount: Int?
    var deleteUnread: Bool

    static let disabled = MailboxPolicy(
        maxAgeSecs: nil,
        maxCount: nil,
        deleteUnread: false
    )

    enum CodingKeys: String, CodingKey {
        case maxAgeSecs = "max_age_secs"
        case maxCount = "max_count"
        case deleteUnread = "delete_unread"
    }
}

struct MailboxPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: MailboxPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct MailboxGcResult: Codable, Sendable {
    let removed: Int
}

struct ArtifactPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: ArtifactPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct ArtifactGcResult: Codable, Sendable {
    let removed: Int
    let bytesFreed: Int

    enum CodingKeys: String, CodingKey {
        case removed
        case bytesFreed = "bytes_freed"
    }
}

struct UsageEntry: Codable, Sendable, Identifiable {
    let id: String
    let sessionId: String
    let agentId: String
    let agentName: String
    let role: String
    let taskId: String?
    let model: String
    let inputTokens: Int
    let outputTokens: Int
    let estimated: Bool
    let costUsd: Double
    let createdAt: String

    var totalTokens: Int {
        inputTokens + outputTokens
    }

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case role
        case taskId = "task_id"
        case model
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case estimated
        case costUsd = "cost_usd"
        case createdAt = "created_at"
    }
}

struct UsageBucket: Codable, Sendable, Identifiable {
    let key: String
    let calls: Int
    let inputTokens: Int
    let outputTokens: Int
    let costUsd: Double

    var id: String {
        key
    }

    enum CodingKeys: String, CodingKey {
        case key
        case calls
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case costUsd = "cost_usd"
    }
}

struct UsageTotals: Codable, Sendable {
    let calls: Int
    let inputTokens: Int
    let outputTokens: Int
    let costUsd: Double
    let byRole: [UsageBucket]
    let byModel: [UsageBucket]
    let byAgent: [UsageBucket]
    let byTask: [UsageBucket]
    let byDepartment: [UsageBucket]

    enum CodingKeys: String, CodingKey {
        case calls
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case costUsd = "cost_usd"
        case byRole = "by_role"
        case byModel = "by_model"
        case byAgent = "by_agent"
        case byTask = "by_task"
        case byDepartment = "by_department"
    }
}

struct UsageLedgerResult: Codable, Sendable {
    let sessionId: String
    let entries: [UsageEntry]
    let totals: UsageTotals

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case entries
        case totals
    }
}

struct ShellPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: ShellPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct Project: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let root: String
    let defaultBranch: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case root
        case defaultBranch = "default_branch"
        case createdAt = "created_at"
    }
}

struct Session: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let goal: String
    let status: String
    let worktree: String?
    let branch: String?
    let tokenBudget: Int?
    let costBudgetUsd: Double?
    let timeBudgetSecs: Int?
    let costSpentUsd: Double
    let baseCommit: String?
    let tokensIn: Int
    let tokensOut: Int
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case goal
        case status
        case worktree
        case branch
        case tokenBudget = "token_budget"
        case costBudgetUsd = "cost_budget_usd"
        case timeBudgetSecs = "time_budget_secs"
        case costSpentUsd = "cost_spent_usd"
        case baseCommit = "base_commit"
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case createdAt = "created_at"
    }

    var tokensUsed: Int {
        tokensIn + tokensOut
    }
}

struct Event: Codable, Identifiable, Hashable, Sendable {
    let sessionId: String
    let seq: UInt64
    let type: String
    let payload: JSONValue
    let createdAt: String

    var id: String {
        "\(sessionId)-\(seq)"
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case seq
        case type
        case payload
        case createdAt = "created_at"
    }
}

struct Discovery: Codable, Sendable {
    let pid: Int
    let port: UInt16
    let token: String
    let `protocol`: Int
    let version: String
    let startedAt: String

    enum CodingKeys: String, CodingKey {
        case pid
        case port
        case token
        case `protocol`
        case version
        case startedAt = "started_at"
    }
}

struct ServerInfo: Codable, Sendable {
    let pid: Int
    let version: String
    let `protocol`: Int
    let dataDir: String
    let startedAt: String
    let sessions: Int
    let projects: Int

    enum CodingKeys: String, CodingKey {
        case pid
        case version
        case `protocol`
        case dataDir = "data_dir"
        case startedAt = "started_at"
        case sessions
        case projects
    }
}

struct AuthResult: Codable, Sendable {
    let ok: Bool
    let server: ServerInfo
}

struct ProjectListResult: Codable, Sendable {
    let projects: [Project]
}

struct SessionListResult: Codable, Sendable {
    let sessions: [Session]
}

struct EventListResult: Codable, Sendable {
    let events: [Event]
}

struct RPCErrorPayload: Codable, Sendable {
    let code: Int
    let message: String
}

struct RPCEnvelope: Codable, Sendable {
    let jsonrpc: String?
    let id: JSONValue?
    let result: JSONValue?
    let error: RPCErrorPayload?
    let method: String?
    let params: JSONValue?
}

struct AgentMessage: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let fromAgentId: String
    let fromName: String
    let toAgentId: String
    let toName: String
    let kind: String
    let body: String
    let payload: JSONValue
    let status: String
    let createdAt: String
    let readAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case fromAgentId = "from_agent_id"
        case fromName = "from_name"
        case toAgentId = "to_agent_id"
        case toName = "to_name"
        case kind
        case body
        case payload
        case status
        case createdAt = "created_at"
        case readAt = "read_at"
    }
}

struct Artifact: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let agentId: String
    let agentName: String
    let taskId: String?
    let name: String
    let kind: String
    let mimeType: String
    let content: String
    let storage: String
    let sizeBytes: Int
    let sha256: String
    let path: String?
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case taskId = "task_id"
        case name
        case kind
        case mimeType = "mime_type"
        case content
        case storage
        case sizeBytes = "size_bytes"
        case sha256
        case path
        case createdAt = "created_at"
    }
}

struct MailboxListResult: Codable, Sendable {
    let messages: [AgentMessage]
}

struct ArtifactListResult: Codable, Sendable {
    let artifacts: [Artifact]
}

struct ArtifactResult: Codable, Sendable {
    let artifact: Artifact
}

struct AgentMessageResult: Codable, Sendable {
    let message: AgentMessage
}

struct Approval: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let toolCallId: String
    let tool: String
    let input: JSONValue
    let status: String
    let createdAt: String
    let resolvedAt: String?
    let agentName: String?
    let agentRole: String?
    let agentTitle: String?

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case toolCallId = "tool_call_id"
        case tool
        case input
        case status
        case createdAt = "created_at"
        case resolvedAt = "resolved_at"
        case agentName = "agent_name"
        case agentRole = "agent_role"
        case agentTitle = "agent_title"
    }
}

struct ApprovalListResult: Codable, Sendable {
    let approvals: [Approval]
}

struct RoleModelConfig: Codable, Hashable, Sendable {
    var model: String? = nil
    var inputUsdPerMillion: Double? = nil
    var outputUsdPerMillion: Double? = nil

    enum CodingKeys: String, CodingKey {
        case model
        case inputUsdPerMillion = "input_usd_per_million"
        case outputUsdPerMillion = "output_usd_per_million"
    }
}

struct RoleModels: Codable, Hashable, Sendable {
    var leader = RoleModelConfig()
    var manager = RoleModelConfig()
    var worker = RoleModelConfig()
    var verifier = RoleModelConfig()
    var labour = RoleModelConfig()

    enum CodingKeys: String, CodingKey {
        case leader
        case manager
        case worker
        case verifier
        case labour
    }
}

struct CapacityInfo: Codable, Sendable {
    let workerLimit: Int
    let workerAvailable: Int
    let labourLimit: Int
    let labourAvailable: Int

    enum CodingKeys: String, CodingKey {
        case workerLimit = "worker_limit"
        case workerAvailable = "worker_available"
        case labourLimit = "labour_limit"
        case labourAvailable = "labour_available"
    }
}

struct AgentInfo: Codable, Sendable {
    let configured: Bool
    let provider: String?
    let model: String?
    let baseUrl: String?
    let roleModels: RoleModels
    let capacity: CapacityInfo?

    enum CodingKeys: String, CodingKey {
        case configured
        case provider
        case model
        case baseUrl = "base_url"
        case roleModels = "role_models"
        case capacity
    }
}

struct PromptResult: Codable, Sendable {
    let accepted: Bool
}

struct ResolveApprovalResult: Codable, Sendable {
    let resolved: Bool
}

struct SessionDiff: Codable, Sendable {
    let sessionId: String
    let branch: String
    let worktree: String
    let baseCommit: String?
    let status: String
    let patch: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case branch
        case worktree
        case baseCommit = "base_commit"
        case status
        case patch
    }

    var hasUncommittedChanges: Bool {
        !status.isEmpty
    }

    var isEmpty: Bool {
        status.isEmpty && patch.isEmpty
    }
}

struct SessionCommit: Codable, Sendable {
    let sessionId: String
    let commit: String
    let message: String
    let summary: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case commit
        case message
        case summary
    }
}

struct CancelResult: Codable, Sendable {
    let cancelled: Bool
}

extension Int {
    var clampedTokenBudget: Int {
        Swift.max(1000, Swift.min(self, 2_000_000))
    }
}

struct TodoItem: Codable, Identifiable, Hashable, Sendable {
    let title: String
    let status: String

    var id: String {
        title
    }
}

struct SessionTodosResult: Codable, Sendable {
    let sessionId: String
    let items: [TodoItem]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case items
    }
}

struct AgentRecord: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let role: String
    let name: String
    let title: String?
    let profile: String?
    let parentId: String?
    let worktree: String?
    let branch: String?
    let status: String
    let model: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case role
        case name
        case title
        case profile
        case parentId = "parent_id"
        case worktree
        case branch
        case status
        case model
        case createdAt = "created_at"
    }
}

struct AgentListResult: Codable, Sendable {
    let agents: [AgentRecord]
}

struct MergeQueueEntry: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let agentId: String
    let agentName: String
    let taskId: String?
    let branch: String
    let commit: String
    let baseCommit: String
    let status: String
    let attempt: Int
    let detail: String?
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case taskId = "task_id"
        case branch
        case commit
        case baseCommit = "base_commit"
        case status
        case attempt
        case detail
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct MergeQueueListResult: Codable, Sendable {
    let entries: [MergeQueueEntry]
}

struct MergeQueueRetryResult: Codable, Sendable {
    let entry: MergeQueueEntry
}

struct PullRequestResult: Codable, Sendable {
    let sessionId: String
    let branch: String
    let baseBranch: String
    let pushed: Bool
    let url: String?
    let detail: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case branch
        case baseBranch = "base_branch"
        case pushed
        case url
        case detail
    }
}

struct SessionLandResult: Codable, Sendable {
    let sessionId: String
    let branch: String
    let targetBranch: String
    let commit: String
    let detail: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case branch
        case targetBranch = "target_branch"
        case commit
        case detail
    }
}

struct SessionPauseResult: Codable, Sendable {
    let sessionId: String
    let paused: Bool

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case paused
    }
}

struct SessionResumeResult: Codable, Sendable {
    let sessionId: String
    let resumed: Bool

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case resumed
    }
}

struct PlanDepartment: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let managerTitle: String
    let profile: String
    let objective: String
    let tokenBudget: Int?
    let costBudgetUsd: Double?

    var id: String {
        name
    }

    enum CodingKeys: String, CodingKey {
        case name
        case managerTitle = "manager_title"
        case profile
        case objective
        case tokenBudget = "token_budget"
        case costBudgetUsd = "cost_budget_usd"
    }
}

struct PlanTask: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let department: String
    let title: String
    let kind: String
    let profile: String?
    let deps: [String]
    let writeScope: [String]
    let acceptance: [String]
    let tokenBudget: Int?
    let costBudgetUsd: Double?

    enum CodingKeys: String, CodingKey {
        case id
        case department
        case title
        case kind
        case profile
        case deps
        case writeScope = "write_scope"
        case acceptance
        case tokenBudget = "token_budget"
        case costBudgetUsd = "cost_budget_usd"
    }
}

struct TeamPlan: Codable, Sendable {
    let goal: String
    let departments: [PlanDepartment]
    let tasks: [PlanTask]
}

struct TeamPlanResult: Codable, Sendable {
    let plan: TeamPlan?
}
