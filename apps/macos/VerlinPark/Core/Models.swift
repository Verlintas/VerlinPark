import Foundation

struct ShellPolicy: Codable, Hashable, Sendable {
    var network: String
    var readDeny: [String]
    var restrictHome: Bool
    var readJail: Bool

    static let allow = ShellPolicy(
        network: "allow",
        readDeny: [],
        restrictHome: false,
        readJail: false
    )

    enum CodingKeys: String, CodingKey {
        case network
        case readDeny = "read_deny"
        case restrictHome = "restrict_home"
        case readJail = "read_jail"
    }

    init(network: String, readDeny: [String], restrictHome: Bool, readJail: Bool) {
        self.network = network
        self.readDeny = readDeny
        self.restrictHome = restrictHome
        self.readJail = readJail
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        network = try container.decode(String.self, forKey: .network)
        readDeny = try container.decodeIfPresent([String].self, forKey: .readDeny) ?? []
        restrictHome = try container.decodeIfPresent(Bool.self, forKey: .restrictHome) ?? false
        readJail = try container.decodeIfPresent(Bool.self, forKey: .readJail) ?? false
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
    let transport: String?
    let command: String
    let args: [String]
    let env: [String: String]
    let url: String?
    let headers: [String: String]?
    let enabled: Bool
    let createdAt: String

    var transportKind: String {
        transport ?? "stdio"
    }

    var headerCount: Int {
        headers?.count ?? 0
    }

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case transport
        case command
        case args
        case env
        case url
        case headers
        case enabled
        case createdAt = "created_at"
    }
}

struct AcpAgent: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let command: String
    let args: [String]
    let env: [String: String]
    let enabled: Bool
    let autoApprove: Bool
    let timeoutSecs: Int
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case command
        case args
        case env
        case enabled
        case autoApprove = "auto_approve"
        case timeoutSecs = "timeout_secs"
        case createdAt = "created_at"
    }
}

struct AcpAgentListResult: Codable, Sendable {
    let agents: [AcpAgent]
}

struct AcpAgentResult: Codable, Sendable {
    let agent: AcpAgent
}

struct AcpProbeResult: Codable, Sendable {
    let agentId: String
    let agentName: String
    let version: String
    let protocolVersion: Int
    let sessionCreated: Bool

    enum CodingKeys: String, CodingKey {
        case agentId = "agent_id"
        case agentName = "agent_name"
        case version
        case protocolVersion = "protocol_version"
        case sessionCreated = "session_created"
    }
}

struct AcpRemoveResult: Codable, Sendable {
    let removed: Bool
}

struct McpRemoveResult: Codable, Sendable {
    let removed: Bool
}

struct McpServerListResult: Codable, Sendable {
    let servers: [McpServer]
}

struct McpProbeResult: Codable, Sendable {
    let serverID: String
    let name: String
    let transport: String
    let serverName: String
    let serverVersion: String
    let protocolVersion: String
    let tools: Int
    let resources: Int
    let prompts: Int
    let resourcesSupported: Bool
    let promptsSupported: Bool

    enum CodingKeys: String, CodingKey {
        case serverID = "server_id"
        case name
        case transport
        case serverName = "server_name"
        case serverVersion = "server_version"
        case protocolVersion = "protocol_version"
        case tools
        case resources
        case prompts
        case resourcesSupported = "resources_supported"
        case promptsSupported = "prompts_supported"
    }
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

struct McpResource: Codable, Identifiable, Hashable, Sendable {
    let uri: String
    let name: String
    let description: String
    let mimeType: String?

    var id: String {
        uri
    }

    enum CodingKeys: String, CodingKey {
        case uri
        case name
        case description
        case mimeType = "mime_type"
    }
}

struct McpResourceTemplate: Codable, Identifiable, Hashable, Sendable {
    let uriTemplate: String
    let name: String
    let description: String
    let mimeType: String?

    var id: String {
        uriTemplate
    }

    enum CodingKeys: String, CodingKey {
        case uriTemplate = "uri_template"
        case name
        case description
        case mimeType = "mime_type"
    }
}

struct McpResourceListResult: Codable, Sendable {
    let serverId: String
    let resources: [McpResource]
    let templates: [McpResourceTemplate]

    enum CodingKeys: String, CodingKey {
        case serverId = "server_id"
        case resources
        case templates
    }
}

struct McpPromptArgument: Codable, Hashable, Sendable {
    let name: String
    let description: String
    let required: Bool
}

struct McpPrompt: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let description: String
    let arguments: [McpPromptArgument]

    var id: String {
        name
    }
}

struct McpPromptListResult: Codable, Sendable {
    let serverId: String
    let prompts: [McpPrompt]

    enum CodingKeys: String, CodingKey {
        case serverId = "server_id"
        case prompts
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

struct MemoryPolicy: Codable, Equatable, Sendable {
    let enabled: Bool
    let maxItems: Int?
    let maxAgeDays: Int?

    enum CodingKeys: String, CodingKey {
        case enabled
        case maxItems = "max_items"
        case maxAgeDays = "max_age_days"
    }
}

struct MemoryPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: MemoryPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
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

struct SessionPriorityResult: Codable, Sendable {
    let sessionId: String
    let priority: Int

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case priority
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

struct ModelPreset: Codable, Sendable, Identifiable {
    let id: String
    let name: String
    let provider: String
    let model: String
    let contextWindow: Int?
    let inputUsdPerMillion: Double?
    let outputUsdPerMillion: Double?
    let notes: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case provider
        case model
        case contextWindow = "context_window"
        case inputUsdPerMillion = "input_usd_per_million"
        case outputUsdPerMillion = "output_usd_per_million"
        case notes
        case createdAt = "created_at"
    }
}

struct ModelPresetListResult: Codable, Sendable {
    let presets: [ModelPreset]
}

struct DeleteModelPresetResult: Codable, Sendable {
    let removed: Bool
}

struct CheckpointRetentionPolicy: Codable, Hashable, Sendable {
    var enabled: Bool
    var olderThanSecs: Int
    var keepLatest: Int

    static let disabled = CheckpointRetentionPolicy(
        enabled: false,
        olderThanSecs: 30 * 86400,
        keepLatest: 5
    )

    enum CodingKeys: String, CodingKey {
        case enabled
        case olderThanSecs = "older_than_secs"
        case keepLatest = "keep_latest"
    }
}

struct CheckpointPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: CheckpointRetentionPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
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
    let firstTokenMs: Int?
    let chunks: Int?
    let done: Bool?
    let inputTokens: Int
    let outputTokens: Int
    let text: String

    enum CodingKeys: String, CodingKey {
        case provider
        case model
        case latencyMs = "latency_ms"
        case firstTokenMs = "first_token_ms"
        case chunks
        case done
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
    let priority: Int?
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
        case priority
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
    let approvalRisk: String?
    let approvalReason: String?

    var isDenied: Bool {
        status == "denied"
    }

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
        case approvalRisk = "approval_risk"
        case approvalReason = "approval_reason"
    }
}

struct ApprovalListResult: Codable, Sendable {
    let approvals: [Approval]
}

struct RoleModelConfig: Codable, Hashable, Sendable {
    var model: String? = nil
    var inputUsdPerMillion: Double? = nil
    var outputUsdPerMillion: Double? = nil
    var contextWindow: Int? = nil

    enum CodingKeys: String, CodingKey {
        case model
        case inputUsdPerMillion = "input_usd_per_million"
        case outputUsdPerMillion = "output_usd_per_million"
        case contextWindow = "context_window"
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
    let contextWindow: Int?
    let approvalMode: String?

    enum CodingKeys: String, CodingKey {
        case configured
        case provider
        case model
        case baseUrl = "base_url"
        case roleModels = "role_models"
        case capacity
        case contextWindow = "context_window"
        case approvalMode = "approval_mode"
    }
}

struct ContextUsage: Codable, Sendable {
    let estimatedTokens: Int
    let contextWindow: Int
    let thresholdTokens: Int
    let hardLimitTokens: Int
    let messages: Int
    let calibration: Double?
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case estimatedTokens = "estimated_tokens"
        case contextWindow = "context_window"
        case thresholdTokens = "threshold_tokens"
        case hardLimitTokens = "hard_limit_tokens"
        case messages
        case calibration
        case createdAt = "created_at"
    }
}

struct ContextMeasurement: Codable, Sendable {
    let estimatedTokens: Int
    let actualInputTokens: Int
    let actualOutputTokens: Int
    let ratio: Double
    let calibration: Double
    let samples: Int
    let model: String
    let messages: Int
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case estimatedTokens = "estimated_tokens"
        case actualInputTokens = "actual_input_tokens"
        case actualOutputTokens = "actual_output_tokens"
        case ratio
        case calibration
        case samples
        case model
        case messages
        case createdAt = "created_at"
    }
}

struct ContextCompactionSummary: Codable, Sendable {
    let mode: String
    let summarizedMessages: Int
    let droppedMessages: Int
    let keptMessages: Int
    let estimatedTokensBefore: Int
    let estimatedTokensAfter: Int
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case mode
        case summarizedMessages = "summarized_messages"
        case droppedMessages = "dropped_messages"
        case keptMessages = "kept_messages"
        case estimatedTokensBefore = "estimated_tokens_before"
        case estimatedTokensAfter = "estimated_tokens_after"
        case createdAt = "created_at"
    }
}

struct SessionContextStatus: Codable, Sendable {
    let sessionID: String
    let usage: ContextUsage?
    let lastCompaction: ContextCompactionSummary?
    let lastMeasurement: ContextMeasurement?

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case usage
        case lastCompaction = "last_compaction"
        case lastMeasurement = "last_measurement"
    }
}

struct SessionCheckpoint: Codable, Sendable, Identifiable {
    let id: String
    let sessionID: String
    let label: String
    let commit: String
    let branch: String
    let startMessageID: String?
    let throughMessageID: String?
    let messageCount: Int
    let summary: String?
    let dirty: Bool?
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case sessionID = "session_id"
        case label
        case commit
        case branch
        case startMessageID = "start_message_id"
        case throughMessageID = "through_message_id"
        case messageCount = "message_count"
        case summary
        case dirty
        case createdAt = "created_at"
    }
}

struct SessionCheckpointListResult: Codable, Sendable {
    let checkpoints: [SessionCheckpoint]
}

struct SessionCheckpointRestoreResult: Codable, Sendable {
    let sessionID: String
    let checkpointID: String
    let previousCommit: String
    let restoredCommit: String

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case checkpointID = "checkpoint_id"
        case previousCommit = "previous_commit"
        case restoredCommit = "restored_commit"
    }
}

struct SessionCheckpointDiffResult: Codable, Sendable {
    let sessionID: String
    let checkpointID: String
    let currentCommit: String
    let checkpointCommit: String
    let currentBranch: String
    let clean: Bool
    let files: [SessionCompareFile]
    let patch: String
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case checkpointID = "checkpoint_id"
        case currentCommit = "current_commit"
        case checkpointCommit = "checkpoint_commit"
        case currentBranch = "current_branch"
        case clean
        case files
        case patch
        case truncated
    }
}

struct SessionCheckpointStatsResult: Codable, Sendable {
    let sessionID: String
    let checkpoints: Int
    let dirty: Int
    let refs: Int
    let commitBytes: Int
    let snapshotBytes: Int
    let uniqueBlobs: Int

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case checkpoints
        case dirty
        case refs
        case commitBytes = "commit_bytes"
        case snapshotBytes = "snapshot_bytes"
        case uniqueBlobs = "unique_blobs"
    }
}

struct SessionCheckpointDeleteResult: Codable, Sendable {
    let sessionID: String
    let checkpointID: String
    let removed: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case checkpointID = "checkpoint_id"
        case removed
    }
}

struct SessionCheckpointPruneResult: Codable, Sendable {
    let sessionID: String
    let candidates: Int
    let removed: Int
    let dryRun: Bool
    let checkpoints: [SessionCheckpoint]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case candidates
        case removed
        case dryRun = "dry_run"
        case checkpoints
    }
}

struct UpdateCheckResult: Codable, Sendable {
    let currentVersion: String
    let latestVersion: String
    let updateAvailable: Bool
    let repo: String
    let releaseName: String?
    let releaseUrl: String?
    let publishedAt: String?
    let dmgName: String?
    let dmgUrl: String?
    let sha256Name: String?
    let sha256Url: String?

    enum CodingKeys: String, CodingKey {
        case currentVersion = "current_version"
        case latestVersion = "latest_version"
        case updateAvailable = "update_available"
        case repo
        case releaseName = "release_name"
        case releaseUrl = "release_url"
        case publishedAt = "published_at"
        case dmgName = "dmg_name"
        case dmgUrl = "dmg_url"
        case sha256Name = "sha256_name"
        case sha256Url = "sha256_url"
    }
}

struct UpdateDownloadResult: Codable, Sendable {
    let version: String
    let path: String
    let sha256: String
    let size: Int
    let verified: Bool
}

struct SessionForkTreeNode: Codable, Sendable, Identifiable {
    let session: Session
    let checkpointID: String?
    let commit: String?
    let children: [SessionForkTreeNode]

    var id: String {
        session.id
    }

    enum CodingKeys: String, CodingKey {
        case session
        case checkpointID = "checkpoint_id"
        case commit
        case children
    }
}

struct DashboardCount: Codable, Sendable, Identifiable {
    var id: String {
        label
    }

    let label: String
    let count: Int
}

struct DashboardProjectSummary: Codable, Sendable, Identifiable {
    var id: String {
        projectID
    }

    let projectID: String
    let name: String
    let sessions: Int
    let runningSessions: Int
    let queuedSessions: Int
    let failedSessions: Int
    let pendingApprovals: Int
    let pendingQuestions: Int
    let tasks: Int
    let runningTasks: Int
    let checkpoints: Int
    let dirtyCheckpoints: Int
    let forkRoots: Int
    let forkSessions: Int

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case name
        case sessions
        case runningSessions = "running_sessions"
        case queuedSessions = "queued_sessions"
        case failedSessions = "failed_sessions"
        case pendingApprovals = "pending_approvals"
        case pendingQuestions = "pending_questions"
        case tasks
        case runningTasks = "running_tasks"
        case checkpoints
        case dirtyCheckpoints = "dirty_checkpoints"
        case forkRoots = "fork_roots"
        case forkSessions = "fork_sessions"
    }
}

struct DashboardSummary: Codable, Sendable {
    let generatedAt: String
    let projectID: String?
    let projects: Int
    let sessions: Int
    let sessionsByStatus: [DashboardCount]
    let pendingApprovals: Int
    let pendingQuestions: Int
    let runningSessions: Int
    let queuedSessions: Int
    let tasks: Int
    let tasksByStatus: [DashboardCount]
    let runningTasks: Int
    let writeLeases: Int
    let checkpoints: Int
    let dirtyCheckpoints: Int
    let forkRoots: Int
    let forkSessions: Int
    let projectSummaries: [DashboardProjectSummary]
    let agent: AgentInfo?

    enum CodingKeys: String, CodingKey {
        case generatedAt = "generated_at"
        case projectID = "project_id"
        case projects
        case sessions
        case sessionsByStatus = "sessions_by_status"
        case pendingApprovals = "pending_approvals"
        case pendingQuestions = "pending_questions"
        case runningSessions = "running_sessions"
        case queuedSessions = "queued_sessions"
        case tasks
        case tasksByStatus = "tasks_by_status"
        case runningTasks = "running_tasks"
        case writeLeases = "write_leases"
        case checkpoints
        case dirtyCheckpoints = "dirty_checkpoints"
        case forkRoots = "fork_roots"
        case forkSessions = "fork_sessions"
        case projectSummaries = "project_summaries"
        case agent
    }
}

struct DashboardAlert: Codable, Sendable, Identifiable {
    let id: String
    let kind: String
    let severity: String
    let title: String
    let detail: String
    let projectID: String?
    let sessionID: String?
    let action: String

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case severity
        case title
        case detail
        case projectID = "project_id"
        case sessionID = "session_id"
        case action
    }
}

struct DashboardResult: Codable, Sendable {
    let summary: DashboardSummary
    let alerts: [DashboardAlert]
    let alertsTruncated: Bool

    enum CodingKeys: String, CodingKey {
        case summary
        case alerts
        case alertsTruncated = "alerts_truncated"
    }
}

struct ProjectForkForestResult: Codable, Sendable {
    let projectID: String
    let roots: [SessionForkTreeNode]
    let total: Int
    let checkpoints: Int
    let dirtyCheckpoints: Int
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case roots
        case total
        case checkpoints
        case dirtyCheckpoints = "dirty_checkpoints"
        case truncated
    }
}

struct ForkAuditIssue: Codable, Sendable, Identifiable {
    let kind: String
    let sessionID: String
    let relatedSessionID: String?
    let checkpointID: String?
    let commit: String?
    let detail: String
    let repairable: Bool

    var id: String {
        "\(kind)-\(sessionID)-\(checkpointID ?? "")-\(relatedSessionID ?? "")"
    }

    enum CodingKeys: String, CodingKey {
        case kind
        case sessionID = "session_id"
        case relatedSessionID = "related_session_id"
        case checkpointID = "checkpoint_id"
        case commit
        case detail
        case repairable
    }
}

struct ForkAuditResult: Codable, Sendable {
    let projectID: String
    let sessions: Int
    let checkpoints: Int
    let issues: [ForkAuditIssue]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case sessions
        case checkpoints
        case issues
    }
}

struct ForkRepairResult: Codable, Sendable {
    let projectID: String
    let repaired: Int
    let failed: Int
    let details: [String]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case repaired
        case failed
        case details
    }
}

struct SessionForkTreeResult: Codable, Sendable {
    let sessionID: String
    let root: SessionForkTreeNode
    let total: Int
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case root
        case total
        case truncated
    }
}

struct SessionLineageEntry: Codable, Sendable, Identifiable {
    let session: Session
    let checkpointID: String?
    let commit: String?

    var id: String {
        session.id
    }

    enum CodingKeys: String, CodingKey {
        case session
        case checkpointID = "checkpoint_id"
        case commit
    }
}

struct SessionLineageResult: Codable, Sendable {
    let sessionID: String
    let parent: SessionLineageEntry?
    let siblings: [SessionLineageEntry]
    let children: [SessionLineageEntry]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case parent
        case siblings
        case children
    }
}

struct SessionCompareFile: Codable, Sendable, Identifiable {
    let path: String
    let additions: Int
    let deletions: Int
    let binary: Bool

    var id: String {
        path
    }
}

struct SessionCompareResult: Codable, Sendable {
    let sessionID: String
    let otherSessionID: String
    let leftCommit: String
    let rightCommit: String
    let files: [SessionCompareFile]
    let patch: String
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case otherSessionID = "other_session_id"
        case leftCommit = "left_commit"
        case rightCommit = "right_commit"
        case files
        case patch
        case truncated
    }
}

struct SessionMergeForkResult: Codable, Sendable {
    let sourceSessionID: String
    let forkSessionID: String
    let status: String
    let preMergeCommit: String
    let forkCommit: String
    let mergeCommit: String?
    let conflicts: [String]
    let detail: String

    enum CodingKeys: String, CodingKey {
        case sourceSessionID = "source_session_id"
        case forkSessionID = "fork_session_id"
        case status
        case preMergeCommit = "pre_merge_commit"
        case forkCommit = "fork_commit"
        case mergeCommit = "merge_commit"
        case conflicts
        case detail
    }
}

struct SessionForkResult: Codable, Sendable {
    let sourceSessionID: String
    let checkpointID: String
    let session: Session

    enum CodingKeys: String, CodingKey {
        case sourceSessionID = "source_session_id"
        case checkpointID = "checkpoint_id"
        case session
    }
}

struct SessionCompactResult: Codable, Sendable {
    let sessionID: String
    let compacted: Bool
    let compaction: ContextCompactionSummary?

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case compacted
        case compaction
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

    func clamped(to range: ClosedRange<Int>) -> Int {
        Swift.max(range.lowerBound, Swift.min(self, range.upperBound))
    }
}

struct GitCommit: Codable, Identifiable, Hashable, Sendable {
    let hash: String
    let shortHash: String
    let author: String
    let email: String
    let authorDate: String
    let subject: String
    let refs: String

    var id: String {
        hash
    }

    enum CodingKeys: String, CodingKey {
        case hash
        case shortHash = "short_hash"
        case author
        case email
        case authorDate = "author_date"
        case subject
        case refs
    }
}

struct GitLogResult: Codable, Sendable {
    let sessionId: String
    let branch: String?
    let baseCommit: String?
    let commits: [GitCommit]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case branch
        case baseCommit = "base_commit"
        case commits
    }
}

struct GitShowResult: Codable, Sendable {
    let sessionId: String
    let commit: String
    let output: String
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case commit
        case output
        case truncated
    }
}

struct SchedulerTask: Codable, Identifiable, Hashable, Sendable {
    let sessionID: String
    let projectID: String
    let planSeq: UInt64
    let taskID: String
    let department: String
    let title: String
    let priority: Int
    let status: String
    let deps: [String]
    let writeScope: [String]
    let attempts: UInt32
    let enqueuedAt: String
    let startedAt: String?
    let finishedAt: String?
    let lastError: String?

    var id: String {
        "\(sessionID)/\(planSeq)/\(taskID)"
    }

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case projectID = "project_id"
        case planSeq = "plan_seq"
        case taskID = "task_id"
        case department
        case title
        case priority
        case status
        case deps
        case writeScope = "write_scope"
        case attempts
        case enqueuedAt = "enqueued_at"
        case startedAt = "started_at"
        case finishedAt = "finished_at"
        case lastError = "last_error"
    }
}

struct TaskSchedulerPolicy: Codable, Hashable, Sendable {
    var enabled: Bool
    var cooldownSecs: UInt64
    var dispatchTasks: Bool

    static let disabled = TaskSchedulerPolicy(
        enabled: false,
        cooldownSecs: 60,
        dispatchTasks: false
    )

    init(enabled: Bool, cooldownSecs: UInt64, dispatchTasks: Bool) {
        self.enabled = enabled
        self.cooldownSecs = cooldownSecs
        self.dispatchTasks = dispatchTasks
    }

    enum CodingKeys: String, CodingKey {
        case enabled
        case cooldownSecs = "cooldown_secs"
        case dispatchTasks = "dispatch_tasks"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decode(Bool.self, forKey: .enabled)
        cooldownSecs = try container.decode(UInt64.self, forKey: .cooldownSecs)
        dispatchTasks = try container.decodeIfPresent(Bool.self, forKey: .dispatchTasks) ?? false
    }
}

struct TaskSchedulerPolicyResult: Codable, Sendable {
    let projectID: String
    let policy: TaskSchedulerPolicy

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case policy
    }
}

struct TaskDispatchResult: Codable, Sendable {
    let started: Bool
}

struct TaskActionResult: Codable, Sendable {
    let task: SchedulerTask
}

struct WriteLease: Codable, Identifiable, Hashable, Sendable {
    let projectID: String
    let sessionID: String
    let agentID: String
    let scopes: [String]
    let acquiredAt: String

    var id: String {
        agentID
    }

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case sessionID = "session_id"
        case agentID = "agent_id"
        case scopes
        case acquiredAt = "acquired_at"
    }
}

struct TaskQueueResult: Codable, Sendable {
    let tasks: [SchedulerTask]
    let leases: [WriteLease]
}

struct TerminalOpenResult: Codable, Sendable {
    let terminalId: String
    let worktree: String

    enum CodingKeys: String, CodingKey {
        case terminalId = "terminal_id"
        case worktree
    }
}

struct TerminalChunk: Codable, Identifiable, Hashable, Sendable {
    let seq: UInt64
    let data: String

    var id: UInt64 {
        seq
    }
}

struct TerminalReadResult: Codable, Sendable {
    let terminalId: String
    let chunks: [TerminalChunk]
    let nextSeq: UInt64
    let status: String
    let exitCode: Int?
    let dropped: UInt64

    enum CodingKeys: String, CodingKey {
        case terminalId = "terminal_id"
        case chunks
        case nextSeq = "next_seq"
        case status
        case exitCode = "exit_code"
        case dropped
    }
}

struct TerminalWriteResult: Codable, Sendable {
    let written: Bool
}

struct TerminalResizeResult: Codable, Sendable {
    let resized: Bool
}

struct TerminalCloseResult: Codable, Sendable {
    let closed: Bool
    let exitCode: Int?

    enum CodingKeys: String, CodingKey {
        case closed
        case exitCode = "exit_code"
    }
}

struct WorkspaceEntry: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let name: String
    let kind: String
    let size: UInt64

    var id: String {
        path
    }

    var isDirectory: Bool {
        kind == "dir"
    }
}

struct WorkspaceTreeResult: Codable, Sendable {
    let root: String
    let entries: [WorkspaceEntry]
    let truncated: Bool
}

struct WorkspaceFileResult: Codable, Sendable {
    let path: String
    let size: UInt64
    let truncated: Bool
    let binary: Bool
    let content: String
    let sha256: String
    let editable: Bool
}

struct WorkspaceWriteResult: Codable, Sendable {
    let path: String
    let size: UInt64
    let sha256: String
}

struct ConsoleOpenResult: Codable, Sendable {
    let consoleId: String
    let worktree: String

    enum CodingKeys: String, CodingKey {
        case consoleId = "console_id"
        case worktree
    }
}

struct ConsoleChunk: Codable, Identifiable, Hashable, Sendable {
    let seq: UInt64
    let stream: String
    let text: String

    var id: UInt64 {
        seq
    }
}

struct ConsoleReadResult: Codable, Sendable {
    let consoleId: String
    let chunks: [ConsoleChunk]
    let nextSeq: UInt64
    let status: String
    let exitCode: Int?
    let dropped: UInt64

    enum CodingKeys: String, CodingKey {
        case consoleId = "console_id"
        case chunks
        case nextSeq = "next_seq"
        case status
        case exitCode = "exit_code"
        case dropped
    }
}

struct ConsoleWriteResult: Codable, Sendable {
    let written: Bool
}

struct ConsoleSignalResult: Codable, Sendable {
    let signaled: Bool
}

struct ConsoleCloseResult: Codable, Sendable {
    let closed: Bool
    let exitCode: Int?

    enum CodingKeys: String, CodingKey {
        case closed
        case exitCode = "exit_code"
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
    let taskId: String?
    let department: String?
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
        case taskId = "task_id"
        case department
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
