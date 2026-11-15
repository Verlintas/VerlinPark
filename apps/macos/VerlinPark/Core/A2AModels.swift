import Foundation
import SwiftUI

// MARK: - Copy and display bounds

enum A2AText {
    static let maxItemChars = 200
    static let maxReferenceChars = 400
    static let maxErrorChars = 320
    static let maxTaskIDChars = 80
    static let maxContextIDChars = 120

    static func bounded(_ value: String, limit: Int = maxItemChars) -> String {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard limit > 0, trimmed.count > limit else { return trimmed }
        let end = trimmed.index(trimmed.startIndex, offsetBy: limit - 1)
        return String(trimmed[..<end]) + "…"
    }

    static func optionalText(_ value: String?, limit: Int = maxReferenceChars) -> String? {
        guard let value else { return nil }
        let bounded = bounded(value, limit: limit)
        return bounded.isEmpty ? nil : bounded
    }

    /// Display-only timestamp normalization. No `Date` parsing is performed.
    static func timestamp(_ value: String?) -> String? {
        guard let value else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let normalized = trimmed.replacingOccurrences(of: "T", with: " ")
        return String(normalized.prefix(19))
    }

    static func durationLabel(seconds: Int) -> String {
        guard seconds > 0 else { return "off" }
        if seconds % 3600 == 0 {
            return "\(seconds / 3600)h"
        }
        if seconds % 60 == 0 {
            return "\(seconds / 60)m"
        }
        return "\(seconds)s"
    }

    static func pluralized(_ count: Int, singular: String, plural: String? = nil) -> String {
        "\(count) \(count == 1 ? singular : plural ?? singular + "s")"
    }
}

enum A2ACopy {
    static let loopbackBadgeText = "loopback only"
    static let loopbackWarning =
        "Loopback only: the facade binds only to 127.0.0.1 or ::1, has no public push endpoint, and cannot exceed the session's runtime contract."
    static let inboundWarning =
        "Inbound A2A is enabled. Requests stay loopback-only and must still pass the session contract, approvals, sandbox, budget, and Verifier gates. Inbound work cannot exceed the parent contract."
    static let inboundDisabledNote =
        "Inbound A2A is disabled. The task lifecycle is still available to explicit local callers when a policy enables it."
    static let noTokenNotice =
        "Only token presence is reported. The token value and hash are never fetched or displayed."
    static let unavailableTitle = "A2A status unavailable"
    static let unavailableDetail =
        "Refresh to load the local loopback facade state. No network endpoint is contacted by this card."
    static let disabledTitle = "A2A facade disabled"
    static let disabledDetail =
        "The project policy disables the loopback facade. Start is unavailable until the policy enables it."
    static let loadingTitle = "Loading A2A status"
    static let emptyTasksTitle = "No A2A tasks"
    static let emptyTasksDetail =
        "No loopback task summaries are retained for the current window."
}

// MARK: - Tone and task state

enum A2ATone: String, CaseIterable, Hashable, Sendable {
    case running
    case waiting
    case positive
    case danger
    case neutral

    var tint: Color {
        switch self {
        case .running: VPTheme.accent
        case .waiting: VPTheme.warning
        case .positive: VPTheme.success
        case .danger: VPTheme.danger
        case .neutral: VPTheme.textTertiary
        }
    }
}

/// A2A task lifecycle state. Unknown values decode conservatively instead of
/// failing the whole status response.
enum A2ATaskState: String, Codable, CaseIterable, Identifiable, Hashable, Sendable {
    case submitted
    case working
    case inputRequired = "input_required"
    case authRequired = "auth_required"
    case completed
    case failed
    case canceled
    case rejected
    case unknown

    var id: String {
        rawValue
    }

    var label: String {
        switch self {
        case .submitted: L10n.t("Submitted")
        case .working: L10n.t("Working")
        case .inputRequired: L10n.t("Input required")
        case .authRequired: L10n.t("Auth required")
        case .completed: L10n.t("Completed")
        case .failed: L10n.t("Failed")
        case .canceled: L10n.t("Canceled")
        case .rejected: L10n.t("Rejected")
        case .unknown: L10n.t("Unknown")
        }
    }

    var tone: A2ATone {
        switch self {
        case .submitted: .waiting
        case .working: .running
        case .inputRequired, .authRequired: .waiting
        case .completed: .positive
        case .failed, .rejected: .danger
        case .canceled, .unknown: .neutral
        }
    }

    var tint: Color {
        tone.tint
    }

    var icon: String {
        switch self {
        case .submitted: "paperplane"
        case .working: "gearshape.2"
        case .inputRequired: "questionmark.bubble"
        case .authRequired: "lock"
        case .completed: "checkmark.seal"
        case .failed: "xmark.octagon"
        case .canceled: "slash.circle"
        case .rejected: "hand.raised"
        case .unknown: "questionmark.circle"
        }
    }

    var isTerminal: Bool {
        switch self {
        case .completed, .failed, .canceled, .rejected: true
        default: false
        }
    }

    var isWaitingForInput: Bool {
        self == .inputRequired || self == .authRequired
    }

    var helpText: String {
        switch self {
        case .submitted: "The task was accepted but has not started."
        case .working: "The task is running."
        case .inputRequired: "The task is waiting for a bounded input-required response."
        case .authRequired: "The task is waiting for an authentication challenge."
        case .completed: "The task completed and reported its result."
        case .failed: "The task failed and reported an error."
        case .canceled: "The task was canceled."
        case .rejected: "The task was rejected before execution."
        case .unknown: "The daemon returned a state this client does not recognize."
        }
    }

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .unknown
    }
}

enum A2ARuntimeState: String, CaseIterable, Identifiable, Hashable, Sendable {
    case running
    case stopped
    case disabled
    case unavailable

    var id: String {
        rawValue
    }

    var label: String {
        switch self {
        case .running: L10n.t("Running")
        case .stopped: L10n.t("Enabled · stopped")
        case .disabled: L10n.t("Disabled")
        case .unavailable: L10n.t("Unavailable")
        }
    }

    var tone: A2ATone {
        switch self {
        case .running: .running
        case .stopped: .waiting
        case .disabled, .unavailable: .neutral
        }
    }

    var tint: Color {
        tone.tint
    }

    var icon: String {
        switch self {
        case .running: "dot.radiowaves.left.and.right"
        case .stopped: "power"
        case .disabled: "pause.circle"
        case .unavailable: "questionmark.circle"
        }
    }
}

// MARK: - Task summary

/// Bounded task summary from the loopback facade. It intentionally carries no
/// prompt text, result body, token, or credential.
struct A2ATaskSummary: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let contextID: String
    let state: A2ATaskState
    let updatedAt: String?
    let messageCount: Int
    let artifactCount: Int

    init(
        id: String,
        contextID: String = "",
        state: A2ATaskState = .unknown,
        updatedAt: String? = nil,
        messageCount: Int = 0,
        artifactCount: Int = 0
    ) {
        self.id = A2AText.bounded(id, limit: A2AText.maxTaskIDChars)
        self.contextID = A2AText.bounded(contextID, limit: A2AText.maxContextIDChars)
        self.state = state
        self.updatedAt = A2AText.timestamp(updatedAt)
        self.messageCount = max(0, messageCount)
        self.artifactCount = max(0, artifactCount)
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        let contextID = try container.decodeIfPresent(String.self, forKey: .contextID) ?? ""
        let state = try container.decodeIfPresent(A2ATaskState.self, forKey: .state) ?? .unknown
        let updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt)
        let messageCount = try container.decodeIfPresent(Int.self, forKey: .messageCount) ?? 0
        let artifactCount = try container.decodeIfPresent(Int.self, forKey: .artifactCount) ?? 0
        self.init(
            id: id,
            contextID: contextID,
            state: state,
            updatedAt: updatedAt,
            messageCount: messageCount,
            artifactCount: artifactCount
        )
    }

    var displayID: String {
        id.isEmpty ? "unidentified task" : id
    }

    var displayContextID: String? {
        contextID.isEmpty ? nil : contextID
    }

    var timestampLabel: String? {
        updatedAt
    }

    var countsLabel: String {
        let messages = A2AText.pluralized(messageCount, singular: "message")
        let artifacts = A2AText.pluralized(artifactCount, singular: "artifact")
        return "\(messages) · \(artifacts)"
    }

    var isActive: Bool {
        !state.isTerminal
    }

    enum CodingKeys: String, CodingKey {
        case id
        case contextID = "context_id"
        case state
        case updatedAt = "updated_at"
        case messageCount = "message_count"
        case artifactCount = "artifact_count"
    }
}

// MARK: - Status

/// Bounded task-list response from the loopback facade. The card accepts the
/// two fields directly so callers that already split the response remain simple.
struct A2ATaskListResult: Codable, Hashable, Sendable {
    let tasks: [A2ATaskSummary]
    let truncated: Bool

    init(tasks: [A2ATaskSummary] = [], truncated: Bool = false) {
        self.tasks = tasks
        self.truncated = truncated
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        tasks = try container.decodeIfPresent([A2ATaskSummary].self, forKey: .tasks) ?? []
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }

    var count: Int {
        tasks.count
    }

    enum CodingKeys: String, CodingKey {
        case tasks
        case truncated
    }
}

/// Loopback-only A2A status. There is deliberately no token or token-hash field;
/// only `tokenPresent` crosses the protocol boundary.
struct A2AStatus: Codable, Identifiable, Hashable, Sendable {
    let running: Bool
    let enabled: Bool
    let inboundEnabled: Bool
    let url: String?
    let cardURL: String?
    let taskCount: Int
    let maxTasks: Int
    let tokenPresent: Bool
    let lastError: String?
    /// Optional policy copied from the daemon status. The card can also receive
    /// a separately refreshed policy; this keeps the nested JSON shape usable.
    let policy: A2APolicy?

    var id: String {
        "a2a|\(url ?? "")|\(cardURL ?? "")"
    }

    init(
        running: Bool = false,
        enabled: Bool = false,
        inboundEnabled: Bool = false,
        url: String? = nil,
        cardURL: String? = nil,
        taskCount: Int = 0,
        maxTasks: Int = 0,
        tokenPresent: Bool = false,
        lastError: String? = nil,
        policy: A2APolicy? = nil
    ) {
        self.running = running
        self.enabled = enabled
        self.inboundEnabled = inboundEnabled
        self.url = A2AText.optionalText(url, limit: A2AText.maxReferenceChars)
        self.cardURL = A2AText.optionalText(cardURL, limit: A2AText.maxReferenceChars)
        self.taskCount = max(0, taskCount)
        self.maxTasks = max(0, maxTasks)
        self.tokenPresent = tokenPresent
        self.lastError = A2AText.optionalText(lastError, limit: A2AText.maxErrorChars)
        self.policy = policy
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let running = try container.decodeIfPresent(Bool.self, forKey: .running) ?? false
        let enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? running
        let inboundEnabled = try container.decodeIfPresent(Bool.self, forKey: .inboundEnabled)
            ?? false
        let url = try container.decodeIfPresent(String.self, forKey: .url)
        let cardURL = try container.decodeIfPresent(String.self, forKey: .cardURL)
        let taskCount = try container.decodeIfPresent(Int.self, forKey: .taskCount) ?? 0
        let maxTasks = try container.decodeIfPresent(Int.self, forKey: .maxTasks) ?? 128
        let tokenPresent = try container.decodeIfPresent(Bool.self, forKey: .tokenPresent)
            ?? false
        let lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
        let policy = try container.decodeIfPresent(A2APolicy.self, forKey: .policy)
        self.init(
            running: running,
            enabled: enabled,
            inboundEnabled: inboundEnabled,
            url: url,
            cardURL: cardURL,
            taskCount: taskCount,
            maxTasks: maxTasks,
            tokenPresent: tokenPresent,
            lastError: lastError,
            policy: policy
        )
    }

    static let disabled = A2AStatus()

    var runtimeState: A2ARuntimeState {
        if running {
            return .running
        }
        return enabled ? .stopped : .disabled
    }

    var stateLabel: String {
        runtimeState.label
    }

    var stateTint: Color {
        runtimeState.tint
    }

    var stateIcon: String {
        runtimeState.icon
    }

    var canAcceptTasks: Bool {
        running && !isCapacityFull
    }

    var isCapacityFull: Bool {
        maxTasks > 0 && taskCount >= maxTasks
    }

    var freeSlots: Int? {
        guard maxTasks > 0 else { return nil }
        return max(0, maxTasks - taskCount)
    }

    var taskCountLabel: String {
        A2AText.pluralized(taskCount, singular: "task")
    }

    var capacityLabel: String {
        guard maxTasks > 0 else {
            return "\(taskCount)"
        }
        return "\(taskCount) / \(maxTasks)"
    }

    var capacityDetail: String {
        guard let freeSlots else {
            return "no task cap reported"
        }
        if freeSlots == 0 {
            return "at capacity"
        }
        return A2AText.pluralized(freeSlots, singular: "free slot")
    }

    var capacityFraction: Double? {
        guard maxTasks > 0 else { return nil }
        let fraction = Double(taskCount) / Double(maxTasks)
        return min(1, max(0, fraction))
    }

    var tokenBadgeText: String {
        tokenPresent ? "token present" : "no token"
    }

    var inboundStatusText: String {
        inboundEnabled ? "inbound on" : "inbound off"
    }

    var inboundWarning: String? {
        inboundEnabled ? A2ACopy.inboundWarning : nil
    }

    var hasError: Bool {
        lastError != nil
    }

    var displayURL: String? {
        url
    }

    var displayCardURL: String? {
        cardURL
    }

    var endpointLabel: String {
        url ?? "not reported"
    }

    var cardEndpointLabel: String {
        cardURL ?? "not reported"
    }

    enum CodingKeys: String, CodingKey {
        case running
        case enabled
        case inboundEnabled = "inbound_enabled"
        case url
        case cardURL = "card_url"
        case taskCount = "task_count"
        case maxTasks = "max_tasks"
        case tokenPresent = "token_present"
        case lastError = "last_error"
        case policy
    }
}

// MARK: - Policy

/// Project-scoped loopback A2A policy. It never includes a token or secret.
struct A2APolicy: Codable, Identifiable, Hashable, Sendable {
    let enabled: Bool
    let allowInbound: Bool
    let port: Int
    let tokenRequired: Bool
    let maxTasks: Int
    let taskTTLSecs: Int

    var id: String {
        "a2a-policy|\(port)|\(maxTasks)|\(taskTTLSecs)"
    }

    init(
        enabled: Bool = false,
        allowInbound: Bool = false,
        port: Int = 0,
        tokenRequired: Bool = true,
        maxTasks: Int = 128,
        taskTTLSecs: Int = 3600
    ) {
        self.enabled = enabled
        self.allowInbound = allowInbound
        self.port = max(0, port)
        self.tokenRequired = tokenRequired
        self.maxTasks = max(0, maxTasks)
        self.taskTTLSecs = max(0, taskTTLSecs)
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        let allowInbound = try container.decodeIfPresent(Bool.self, forKey: .allowInbound)
            ?? false
        let port = try container.decodeIfPresent(Int.self, forKey: .port) ?? 0
        let tokenRequired = try container.decodeIfPresent(Bool.self, forKey: .tokenRequired)
            ?? true
        let maxTasks = try container.decodeIfPresent(Int.self, forKey: .maxTasks) ?? 0
        let taskTTLSecs = try container.decodeIfPresent(Int.self, forKey: .taskTTLSecs) ?? 3600
        self.init(
            enabled: enabled,
            allowInbound: allowInbound,
            port: port,
            tokenRequired: tokenRequired,
            maxTasks: maxTasks,
            taskTTLSecs: taskTTLSecs
        )
    }

    static let disabled = A2APolicy()

    var stateLabel: String {
        enabled ? "enabled" : "disabled"
    }

    var stateTint: Color {
        enabled ? VPTheme.accent : VPTheme.textTertiary
    }

    var inboundLabel: String {
        allowInbound ? "inbound allowed" : "inbound denied"
    }

    var portLabel: String {
        port > 0 ? "\(port)" : "ephemeral"
    }

    var endpointHint: String {
        port > 0 ? "http://127.0.0.1:\(port)" : "loopback · ephemeral port"
    }

    var tokenLabel: String {
        tokenRequired ? "token required" : "token optional"
    }

    var capacityLabel: String {
        maxTasks > 0 ? "\(maxTasks) tasks" : "no task cap"
    }

    var ttlLabel: String {
        A2AText.durationLabel(seconds: taskTTLSecs)
    }

    var ttlDetail: String {
        taskTTLSecs > 0
            ? "Tasks expire \(A2AText.durationLabel(seconds: taskTTLSecs)) after creation."
            : "Task expiry is disabled or managed by the daemon."
    }

    enum CodingKeys: String, CodingKey {
        case enabled
        case allowInbound = "allow_inbound"
        case port
        case tokenRequired = "token_required"
        case maxTasks = "max_tasks"
        case taskTTLSecs = "task_ttl_secs"
    }
}

/// One-time start response. The token is present only in this response and
/// must never be stored in a view model or displayed in a table.
struct A2AStartResult: Codable, Sendable {
    let status: A2AStatus
    let token: String?

    enum CodingKeys: String, CodingKey {
        case status
        case token
    }
}

extension A2APolicy {
    /// Wire shape for `a2a.start`. The token is never part of this value.
    var jsonValue: JSONValue {
        .object([
            "enabled": .bool(enabled),
            "allow_inbound": .bool(allowInbound),
            "bind_host": .string("127.0.0.1"),
            "port": .number(Double(port)),
            "max_tasks": .number(Double(maxTasks)),
            "task_ttl_secs": .number(Double(taskTTLSecs)),
            "token_required": .bool(tokenRequired),
        ])
    }
}
