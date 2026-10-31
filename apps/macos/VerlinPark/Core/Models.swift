import Foundation

struct ShellPolicy: Codable, Hashable, Sendable {
    var network: String
    var readDeny: [String]
    var restrictHome: Bool

    static let allow = ShellPolicy(network: "allow", readDeny: [], restrictHome: false)

    enum CodingKeys: String, CodingKey {
        case network
        case readDeny = "read_deny"
        case restrictHome = "restrict_home"
    }

    init(network: String, readDeny: [String], restrictHome: Bool) {
        self.network = network
        self.readDeny = readDeny
        self.restrictHome = restrictHome
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        network = try container.decode(String.self, forKey: .network)
        readDeny = try container.decodeIfPresent([String].self, forKey: .readDeny) ?? []
        restrictHome = try container.decodeIfPresent(Bool.self, forKey: .restrictHome) ?? false
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

struct ProjectUsageSession: Codable, Identifiable, Sendable {
    let sessionId: String
    let goal: String
    let status: String
    let calls: Int
    let inputTokens: Int
    let outputTokens: Int
    let costUsd: Double

    var id: String {
        sessionId
    }

    var totalTokens: Int {
        inputTokens + outputTokens
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case goal
        case status
        case calls
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case costUsd = "cost_usd"
    }
}

struct ProjectUsageResult: Codable, Sendable {
    let projectId: String
    let sessions: [ProjectUsageSession]
    let totals: UsageTotals

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case sessions
        case totals
    }
}

struct McpServer: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let command: String
    let args: [String]
    let env: [String: String]
    let enabled: Bool
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case command
        case args
        case env
        case enabled
        case createdAt = "created_at"
    }
}

struct McpRemoveResult: Codable, Sendable {
    let removed: Bool
}

struct McpServerListResult: Codable, Sendable {
    let servers: [McpServer]
}

struct McpServerResult: Codable, Sendable {
    let server: McpServer
}

struct McpTool: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let description: String
    let inputSchema: JSONValue?

    var id: String {
        name
    }

    enum CodingKeys: String, CodingKey {
        case name
        case description
        case inputSchema = "input_schema"
    }
}

struct McpToolListResult: Codable, Sendable {
    let serverId: String
    let tools: [McpTool]

    enum CodingKeys: String, CodingKey {
        case serverId = "server_id"
        case tools
    }
}

struct McpCallResult: Codable, Sendable {
    let serverId: String
    let tool: String
    let result: JSONValue

    enum CodingKeys: String, CodingKey {
        case serverId = "server_id"
        case tool
        case result
    }
}

struct MemoryDeleteResult: Codable, Sendable {
    let memoryId: String
    let removed: Bool

    enum CodingKeys: String, CodingKey {
        case memoryId = "memory_id"
        case removed
    }
}

struct MemoryPruneResult: Codable, Sendable {
    let candidates: Int
    let removed: Int
    let dryRun: Bool

    enum CodingKeys: String, CodingKey {
        case candidates
        case removed
        case dryRun = "dry_run"
    }
}

struct MemoryItem: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let projectId: String
    let agentId: String
    let agentName: String
    let kind: String
    let title: String
    let body: String
    let tags: [String]
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case projectId = "project_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case kind
        case title
        case body
        case tags
        case createdAt = "created_at"
    }
}

struct Question: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let agentId: String
    let agentName: String
    let question: String
    let options: [String]
    let status: String
    let answer: String?
    let createdAt: String
    let answeredAt: String?

    var isPending: Bool {
        status == "pending"
    }

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case question
        case options
        case status
        case answer
        case createdAt = "created_at"
        case answeredAt = "answered_at"
    }
}

struct QuestionListResult: Codable, Sendable {
    let questions: [Question]
}

struct AnswerQuestionResult: Codable, Sendable {
    let answered: Bool
}

struct MemoryListResult: Codable, Sendable {
    let memories: [MemoryItem]
}

struct MemoryCreateResult: Codable, Sendable {
    let memory: MemoryItem
}

struct UsageCsvResult: Codable, Sendable {
    let sessionId: String
    let csv: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case csv
    }
}

struct SessionPlanResumeResult: Codable, Sendable {
    let sessionId: String
    let started: Bool

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case started
    }
}

struct SessionTranscriptResult: Codable, Sendable {
    let sessionId: String
    let markdown: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case markdown
    }
}

struct SearchHit: Codable, Sendable, Identifiable {
    let id: String
    let sessionId: String
    let projectId: String
    let sessionGoal: String
    let source: String
    let role: String?
    let kind: String?
    let seq: Int?
    let snippet: String
    let createdAt: String

    var detail: String {
        role ?? kind ?? source
    }

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case projectId = "project_id"
        case sessionGoal = "session_goal"
        case source
        case role
        case kind
        case seq
        case snippet
        case createdAt = "created_at"
    }
}

struct SearchResult: Codable, Sendable {
    let query: String
    let hits: [SearchHit]
    let truncated: Bool
}

struct WorktreeRetentionPolicy: Codable, Hashable, Sendable {
    var enabled: Bool
    var olderThanSecs: Int
    var deleteBranches: Bool
    var keepFailed: Bool

    static let disabled = WorktreeRetentionPolicy(
        enabled: false,
        olderThanSecs: 7 * 86400,
        deleteBranches: false,
        keepFailed: true
    )

    enum CodingKeys: String, CodingKey {
        case enabled
        case olderThanSecs = "older_than_secs"
        case deleteBranches = "delete_branches"
        case keepFailed = "keep_failed"
    }
}

struct WorktreePolicyResult: Codable, Sendable {
    let projectId: String
    let policy: WorktreeRetentionPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct WorktreeCleanupEntry: Codable, Sendable, Identifiable {
    let sessionId: String
    let status: String
    let worktree: String
    let branch: String
    let removed: Bool
    let reason: String

    var id: String {
        sessionId
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case status
        case worktree
        case branch
        case removed
        case reason
    }
}

struct WorktreeCleanupResult: Codable, Sendable {
    let projectId: String
    let scanned: Int
    let removed: Int
    let skipped: Int
    let entries: [WorktreeCleanupEntry]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case scanned
        case removed
        case skipped
        case entries
    }
}

struct DoctorCheck: Codable, Sendable, Identifiable {
    let name: String
    let status: String
    let detail: String

    var id: String {
        name
    }
}

struct DoctorReport: Codable, Sendable {
    let checks: [DoctorCheck]
    let ok: Bool
}

struct AgentProbeResult: Codable, Sendable {
    let provider: String
    let model: String
    let latencyMs: Int
    let inputTokens: Int
    let outputTokens: Int
    let text: String

    enum CodingKeys: String, CodingKey {
        case provider
        case model
        case latencyMs = "latency_ms"
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case text
    }
}

struct ProjectBudget: Codable, Hashable, Sendable {
    var tokenBudget: Int?
    var costBudgetUsd: Double?

    static let empty = ProjectBudget(tokenBudget: nil, costBudgetUsd: nil)

    enum CodingKeys: String, CodingKey {
        case tokenBudget = "token_budget"
        case costBudgetUsd = "cost_budget_usd"
    }
}

struct ProjectBudgetResult: Codable, Sendable {
    let projectId: String
    let budget: ProjectBudget
    let spentTokens: Int
    let spentCostUsd: Double

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case budget
        case spentTokens = "spent_tokens"
        case spentCostUsd = "spent_cost_usd"
    }
}

struct MailboxPolicy: Codable, Hashable, Sendable {
    var maxAgeSecs: Int?
    var maxCount: Int?
    var deleteUnread: Bool
    var autoGc: Bool

    static let disabled = MailboxPolicy(
        maxAgeSecs: nil,
        maxCount: nil,
        deleteUnread: false,
        autoGc: false
    )

    enum CodingKeys: String, CodingKey {
        case maxAgeSecs = "max_age_secs"
        case maxCount = "max_count"
        case deleteUnread = "delete_unread"
        case autoGc = "auto_gc"
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

struct ArtifactMaterialization: Codable, Sendable {
    let artifactId: String
    let path: String

    enum CodingKeys: String, CodingKey {
        case artifactId = "artifact_id"
        case path
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
    let byDepartment: [UsageBucket]?

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
    let compression: String?
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
        case compression
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

struct AppNotice: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    var projectId: String?
    let kind: String
    let title: String
    let detail: String
    let createdAt: String
    var isRead: Bool
}

struct SessionDirectoryEntry: Hashable, Sendable {
    let projectId: String
    let goal: String
}

struct RoundtablePosition: Identifiable, Hashable, Sendable {
    let id: String
    let title: String
    let profile: String?
    let position: String
    let agentName: String?
}

struct RoundtableRound: Identifiable, Hashable, Sendable {
    let round: Int
    let positions: [RoundtablePosition]

    var id: Int {
        round
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
    let priority: Int?
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
        case priority
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
