import Foundation

// MARK: - Bounded Collab agent inspector

/// Read-only projection behind the Collab Agent Inspector. The daemon owns
/// bounds, redaction, and event ordering; the client only decodes and displays
/// what the RPC returned.
struct AgentInspectResult: Codable, Hashable, Sendable {
    var agent: AgentRecord
    var task: AgentTaskView?
    var context: AgentContextView
    var timeline: [AgentTimelineEntry]
    var steering: [SteeringMessage]
    var inbox: [AgentInboxView]
    var inboxCounts: AgentInboxCountsView
    var truncated: Bool

    enum CodingKeys: String, CodingKey {
        case agent
        case task
        case context
        case timeline
        case steering
        case inbox
        case inboxCounts = "inbox_counts"
        case truncated
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        agent = try container.decode(AgentRecord.self, forKey: .agent)
        task = try? container.decodeIfPresent(AgentTaskView.self, forKey: .task)
        context = (try? container.decodeIfPresent(AgentContextView.self, forKey: .context)) ??
            AgentContextView()
        timeline = (try? container.decodeIfPresent([AgentTimelineEntry].self, forKey: .timeline)) ?? []
        steering = (try? container.decodeIfPresent([SteeringMessage].self, forKey: .steering)) ?? []
        inbox = (try? container.decodeIfPresent([AgentInboxView].self, forKey: .inbox)) ?? []
        inboxCounts = (try? container.decodeIfPresent(AgentInboxCountsView.self, forKey: .inboxCounts))
            ?? AgentInboxCountsView()
        truncated = (try? container.decodeIfPresent(Bool.self, forKey: .truncated)) ?? false
    }
}

/// Plan-task metadata scoped to the inspected agent. Fields are additive; a
/// missing value decodes to an empty default rather than failing the whole
/// inspector.
struct AgentTaskView: Codable, Identifiable, Hashable, Sendable {
    var taskID: String
    var title: String
    var department: String
    var kind: String
    var status: String
    var acceptance: [String]
    var writeScope: [String]
    var deps: [String]
    var attempts: Int
    var maxAttempts: Int
    var verifierStatus: String?

    var id: String {
        taskID
    }

    enum CodingKeys: String, CodingKey {
        case taskID = "task_id"
        case title
        case department
        case kind
        case status
        case acceptance
        case writeScope = "write_scope"
        case deps
        case attempts
        case maxAttempts = "max_attempts"
        case verifierStatus = "verifier_status"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        taskID = (try? container.decodeIfPresent(String.self, forKey: .taskID)) ?? ""
        title = (try? container.decodeIfPresent(String.self, forKey: .title)) ?? ""
        department = (try? container.decodeIfPresent(String.self, forKey: .department)) ?? ""
        kind = (try? container.decodeIfPresent(String.self, forKey: .kind)) ?? ""
        status = (try? container.decodeIfPresent(String.self, forKey: .status)) ?? ""
        acceptance = inspectStringList(container, key: .acceptance)
        writeScope = inspectStringList(container, key: .writeScope)
        deps = inspectStringList(container, key: .deps)
        attempts = (try? container.decodeIfPresent(Int.self, forKey: .attempts)) ?? 0
        maxAttempts = (try? container.decodeIfPresent(Int.self, forKey: .maxAttempts)) ?? 0
        verifierStatus = try? container.decodeIfPresent(String.self, forKey: .verifierStatus)
    }
}

/// Bounded context/cache telemetry for the inspected agent. Hashes and counts
/// only; the daemon never sends raw hidden prompts.
struct AgentContextView: Codable, Hashable, Sendable {
    var objective: String?
    var model: String
    var profile: String?
    var estimatedTokens: Int
    var contextWindow: Int
    var thresholdTokens: Int
    var hardLimitTokens: Int
    var inputTokens: Int
    var outputTokens: Int
    var cacheReadTokens: Int
    var cacheWriteTokens: Int
    var costUSD: Double
    var toolCount: Int
    var toolBundle: String?
    var prefixHash: String?
    var lastCompaction: String?
    var memoryBriefChars: Int
    var memoryBriefHash: String?
    var generatedAt: String

    enum CodingKeys: String, CodingKey {
        case objective
        case model
        case profile
        case estimatedTokens = "estimated_tokens"
        case contextWindow = "context_window"
        case thresholdTokens = "threshold_tokens"
        case hardLimitTokens = "hard_limit_tokens"
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
        case costUSD = "cost_usd"
        case toolCount = "tool_count"
        case toolBundle = "tool_bundle"
        case prefixHash = "prefix_hash"
        case lastCompaction = "last_compaction"
        case memoryBriefChars = "memory_brief_chars"
        case memoryBriefHash = "memory_brief_hash"
        case generatedAt = "generated_at"
    }

    init() {
        objective = nil
        model = ""
        profile = nil
        estimatedTokens = 0
        contextWindow = 0
        thresholdTokens = 0
        hardLimitTokens = 0
        inputTokens = 0
        outputTokens = 0
        cacheReadTokens = 0
        cacheWriteTokens = 0
        costUSD = 0
        toolCount = 0
        toolBundle = nil
        prefixHash = nil
        lastCompaction = nil
        memoryBriefChars = 0
        memoryBriefHash = nil
        generatedAt = ""
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        objective = try? container.decodeIfPresent(String.self, forKey: .objective)
        model = (try? container.decodeIfPresent(String.self, forKey: .model)) ?? ""
        profile = try? container.decodeIfPresent(String.self, forKey: .profile)
        estimatedTokens = (try? container.decodeIfPresent(Int.self, forKey: .estimatedTokens)) ?? 0
        contextWindow = (try? container.decodeIfPresent(Int.self, forKey: .contextWindow)) ?? 0
        thresholdTokens = (try? container.decodeIfPresent(Int.self, forKey: .thresholdTokens)) ?? 0
        hardLimitTokens = (try? container.decodeIfPresent(Int.self, forKey: .hardLimitTokens)) ?? 0
        inputTokens = (try? container.decodeIfPresent(Int.self, forKey: .inputTokens)) ?? 0
        outputTokens = (try? container.decodeIfPresent(Int.self, forKey: .outputTokens)) ?? 0
        cacheReadTokens = (try? container.decodeIfPresent(Int.self, forKey: .cacheReadTokens)) ?? 0
        cacheWriteTokens = (try? container.decodeIfPresent(Int.self, forKey: .cacheWriteTokens)) ?? 0
        costUSD = (try? container.decodeIfPresent(Double.self, forKey: .costUSD)) ?? 0
        toolCount = (try? container.decodeIfPresent(Int.self, forKey: .toolCount)) ?? 0
        toolBundle = try? container.decodeIfPresent(String.self, forKey: .toolBundle)
        prefixHash = try? container.decodeIfPresent(String.self, forKey: .prefixHash)
        lastCompaction = inspectFlexibleString(container, key: .lastCompaction)
        memoryBriefChars = (try? container.decodeIfPresent(Int.self, forKey: .memoryBriefChars)) ?? 0
        memoryBriefHash = try? container.decodeIfPresent(String.self, forKey: .memoryBriefHash)
        generatedAt = (try? container.decodeIfPresent(String.self, forKey: .generatedAt)) ?? ""
    }
}

/// One bounded visible conversation/tool/activity row. `isError` is decoded
/// leniently so legacy rows without the field are still displayable.
struct AgentTimelineEntry: Codable, Identifiable, Hashable, Sendable {
    var seq: UInt64
    var kind: String
    var category: String
    var createdAt: String
    var text: String?
    var tool: String?
    var toolCallID: String?
    var status: String?
    var isError: Bool

    var id: String {
        "\(seq)-\(kind)-\(toolCallID ?? "")"
    }

    enum CodingKeys: String, CodingKey {
        case seq
        case kind
        case category
        case createdAt = "created_at"
        case text
        case tool
        case toolCallID = "tool_call_id"
        case status
        case isError = "is_error"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        seq = (try? container.decodeIfPresent(UInt64.self, forKey: .seq)) ?? 0
        kind = (try? container.decodeIfPresent(String.self, forKey: .kind)) ?? ""
        category = (try? container.decodeIfPresent(String.self, forKey: .category)) ?? ""
        createdAt = (try? container.decodeIfPresent(String.self, forKey: .createdAt)) ?? ""
        text = try? container.decodeIfPresent(String.self, forKey: .text)
        tool = try? container.decodeIfPresent(String.self, forKey: .tool)
        toolCallID = try? container.decodeIfPresent(String.self, forKey: .toolCallID)
        status = try? container.decodeIfPresent(String.self, forKey: .status)
        isError = (try? container.decodeIfPresent(Bool.self, forKey: .isError)) ?? false
    }
}

/// Bounded agent-inbox row shown in the Messages tab. Delivery stays
/// pull-based: rendering or decoding this row never marks it read.
struct AgentInboxView: Codable, Identifiable, Hashable, Sendable {
    var id: String
    var fromLabel: String
    var kind: String
    var status: String
    var body: String
    var createdAt: String
    var readAt: String?
    var expiresAt: String?

    var isUnread: Bool {
        status == "unread"
    }

    enum CodingKeys: String, CodingKey {
        case id
        case fromLabel = "from_label"
        case kind
        case status
        case body
        case createdAt = "created_at"
        case readAt = "read_at"
        case expiresAt = "expires_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? container.decodeIfPresent(String.self, forKey: .id)) ?? ""
        fromLabel = (try? container.decodeIfPresent(String.self, forKey: .fromLabel)) ?? ""
        kind = (try? container.decodeIfPresent(String.self, forKey: .kind)) ?? ""
        status = (try? container.decodeIfPresent(String.self, forKey: .status)) ?? ""
        body = (try? container.decodeIfPresent(String.self, forKey: .body)) ?? ""
        createdAt = (try? container.decodeIfPresent(String.self, forKey: .createdAt)) ?? ""
        readAt = try? container.decodeIfPresent(String.self, forKey: .readAt)
        expiresAt = try? container.decodeIfPresent(String.self, forKey: .expiresAt)
    }
}

/// Local inspector counts. `total` is derived when the daemon does not send a
/// total explicitly, so additive metrics never invalidate the whole result.
struct AgentInboxCountsView: Codable, Hashable, Sendable {
    var unread: Int
    var read: Int
    var done: Int
    var expired: Int
    var total: Int

    init(unread: Int = 0, read: Int = 0, done: Int = 0, expired: Int = 0, total: Int? = nil) {
        self.unread = unread
        self.read = read
        self.done = done
        self.expired = expired
        self.total = total ?? (unread + read + done + expired)
    }

    enum CodingKeys: String, CodingKey {
        case unread
        case read
        case done
        case expired
        case total
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let unread = (try? container.decodeIfPresent(Int.self, forKey: .unread)) ?? 0
        let read = (try? container.decodeIfPresent(Int.self, forKey: .read)) ?? 0
        let done = (try? container.decodeIfPresent(Int.self, forKey: .done)) ?? 0
        let expired = (try? container.decodeIfPresent(Int.self, forKey: .expired)) ?? 0
        self.unread = unread
        self.read = read
        self.done = done
        self.expired = expired
        total = (try? container.decodeIfPresent(Int.self, forKey: .total))
            ?? (unread + read + done + expired)
    }
}

/// Delivery result of `agent.message.send`. Exactly one of `steering`/`inbox`
/// is expected today; both stay optional so additive delivery kinds decode.
struct AgentMessageDelivery: Codable, Hashable, Sendable {
    var kind: String
    var status: String
    var message: String?
    var agentID: String
    var identityID: String?
    var steering: SteeringMessage?
    var inbox: AgentInboxView?
    var wake: AgentCoordinationWake?

    enum CodingKeys: String, CodingKey {
        case kind
        case status
        case message
        case agentID = "agent_id"
        case identityID = "identity_id"
        case steering
        case inbox
        case wake
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = (try? container.decodeIfPresent(String.self, forKey: .kind)) ?? ""
        status = (try? container.decodeIfPresent(String.self, forKey: .status)) ?? ""
        message = try? container.decodeIfPresent(String.self, forKey: .message)
        agentID = (try? container.decodeIfPresent(String.self, forKey: .agentID)) ?? ""
        identityID = try? container.decodeIfPresent(String.self, forKey: .identityID)
        steering = try? container.decodeIfPresent(SteeringMessage.self, forKey: .steering)
        inbox = try? container.decodeIfPresent(AgentInboxView.self, forKey: .inbox)
        wake = try? container.decodeIfPresent(AgentCoordinationWake.self, forKey: .wake)
    }
}

struct AgentMessageSendResult: Codable, Hashable, Sendable {
    var delivery: AgentMessageDelivery

    enum CodingKeys: String, CodingKey {
        case delivery
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        delivery = try container.decode(AgentMessageDelivery.self, forKey: .delivery)
    }
}

// MARK: - Client-side rules

/// Narrow transport seam for AppModel tests and for keeping the direct-channel
/// view free of JSON-RPC details. Production uses `CoreClient`.
protocol CollabAgentRPC: Sendable {
    func agentInspect(
        sessionID: String,
        agentID: String,
        timelineLimit: Int?
    ) async throws -> AgentInspectResult

    func sendAgentMessage(
        sessionID: String,
        agentID: String,
        body: String,
        taskID: String?,
        wakeSession: Bool,
        operationID: String?
    ) async throws -> AgentMessageSendResult

    func sessionSteering(
        sessionID: String,
        status: String?,
        limit: Int?
    ) async throws -> SessionSteeringResult
}

enum AgentInspectRules {
    static let defaultTimelineLimit = 80
    static let maxTimelineLimit = 200
    static let notActiveCode = "AGENT_NOT_ACTIVE"

    static func normalizedTimelineLimit(_ value: Int?) -> Int? {
        guard let value else { return nil }
        return min(max(value, 1), maxTimelineLimit)
    }

    static func isAgentNotActive(_ error: Error) -> Bool {
        guard let core = error as? CoreError else { return false }
        return core.typedCode == notActiveCode
    }

    /// Preserve the daemon's stable Q3 code and hint instead of flattening an
    /// `AGENT_NOT_ACTIVE` rejection into a generic failure.
    static func errorText(_ error: Error) -> String {
        guard let core = error as? CoreError else {
            return error.localizedDescription
        }
        var parts = [core.errorDescription ?? "The daemon rejected the request."]
        if let code = core.typedCode, !code.isEmpty {
            parts.append("[\(code)]")
        }
        if let hint = core.typedHint, !hint.isEmpty {
            parts.append(hint)
        }
        return parts.joined(separator: " · ")
    }

    /// Daemon-provided delivery copy wins. The fallback still describes queued
    /// delivery rather than claiming the target has read the message.
    static func deliveryNotice(_ delivery: AgentMessageDelivery) -> String {
        if let message = delivery.message?.trimmingCharacters(in: .whitespacesAndNewlines),
           !message.isEmpty
        {
            return message
        }
        if delivery.kind == "rejected" || delivery.status == "rejected" {
            return L10n.t("The daemon rejected the message; no delivery was queued.")
        }
        switch delivery.kind.lowercased() {
        case "steering":
            return L10n.t(
                "Steering queued. It is delivered at the target agent's next provider boundary."
            )
        case "inbox":
            if delivery.wake?.created == true {
                return L10n.t("Message queued to the agent inbox. A wake continuation was created.")
            }
            if delivery.wake?.requested == true {
                return L10n.t("Message queued to the agent inbox. No wake continuation was created.")
            }
            return L10n.t("Message queued to the agent inbox.")
        default:
            return L10n.t("The daemon accepted the message for delivery.")
        }
    }
}

// MARK: - Lenient decoding helpers

private func inspectStringList<K: CodingKey>(
    _ container: KeyedDecodingContainer<K>,
    key: K
) -> [String] {
    if let values = try? container.decodeIfPresent([String].self, forKey: key) {
        return values
    }
    if let value = try? container.decodeIfPresent(String.self, forKey: key) {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? [] : [trimmed]
    }
    return []
}

/// Accepts the current scalar shape and a legacy structured compaction object,
/// rendering the object as bounded display text instead of failing the tab.
private func inspectFlexibleString<K: CodingKey>(
    _ container: KeyedDecodingContainer<K>,
    key: K
) -> String? {
    if let value = try? container.decodeIfPresent(String.self, forKey: key) {
        return value
    }
    guard let object = try? container.decodeIfPresent([String: JSONValue].self, forKey: key) else {
        return nil
    }
    if let summary = object["summary"]?.stringValue ?? object["text"]?.stringValue,
       !summary.isEmpty
    {
        return summary
    }
    let mode = object["mode"]?.stringValue
    let before = object["estimated_tokens_before"]?.intValue
    let after = object["estimated_tokens_after"]?.intValue
    let created = object["created_at"]?.stringValue
    switch (mode, before, after, created) {
    case let (mode?, before?, after?, _):
        return "\(mode) · \(before) → \(after) tokens"
    case let (mode?, _, _, created?):
        return "\(mode) · \(created)"
    case let (mode?, _, _, _):
        return mode
    case let (_, _, _, created?):
        return created
    default:
        return nil
    }
}
