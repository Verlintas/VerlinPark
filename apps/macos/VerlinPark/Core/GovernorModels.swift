import Foundation

/// Project-scoped critical-path budget policy. The daemon normalizes stored
/// values; the client keeps the same defaults so a missing field never changes
/// admission semantics.
struct CriticalPathPolicy: Codable, Sendable, Equatable {
    var enabled: Bool = false
    var reserveRatio: Double = 0.25
    var throttleAtRatio: Double = 0.35
    var criticalBoost: Int = 25
    var minTaskBudget: Int = 1000

    enum CodingKeys: String, CodingKey {
        case enabled
        case reserveRatio = "reserve_ratio"
        case throttleAtRatio = "throttle_at_ratio"
        case criticalBoost = "critical_boost"
        case minTaskBudget = "min_task_budget"
    }

    init() {}

    init(
        enabled: Bool,
        reserveRatio: Double,
        throttleAtRatio: Double,
        criticalBoost: Int,
        minTaskBudget: Int
    ) {
        self.enabled = enabled
        self.reserveRatio = reserveRatio
        self.throttleAtRatio = throttleAtRatio
        self.criticalBoost = criticalBoost
        self.minTaskBudget = minTaskBudget
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        reserveRatio = try container.decodeIfPresent(Double.self, forKey: .reserveRatio) ?? 0.25
        throttleAtRatio = try container.decodeIfPresent(Double.self, forKey: .throttleAtRatio) ?? 0.35
        criticalBoost = try container.decodeIfPresent(Int.self, forKey: .criticalBoost) ?? 25
        minTaskBudget = try container.decodeIfPresent(Int.self, forKey: .minTaskBudget) ?? 1000
    }

    func jsonValue() -> JSONValue {
        .object([
            "enabled": .bool(enabled),
            "reserve_ratio": .number(reserveRatio),
            "throttle_at_ratio": .number(throttleAtRatio),
            "critical_boost": .number(Double(criticalBoost)),
            "min_task_budget": .number(Double(minTaskBudget)),
        ])
    }
}

struct GovernorCriticalPath: Decodable, Sendable, Equatable {
    var planRevision: Int = 0
    var taskIDs: [String] = []
    var weightTokens: Int = 0
    var remainingBudgetTokens: Int?
    var complete: Bool = false
    var taskCount: Int = 0
    var edgeCount: Int = 0
    var inputsHash: String = ""

    enum CodingKeys: String, CodingKey {
        case planRevision = "plan_revision"
        case taskIDs = "task_ids"
        case weightTokens = "weight_tokens"
        case remainingBudgetTokens = "remaining_budget_tokens"
        case complete
        case taskCount = "task_count"
        case edgeCount = "edge_count"
        case inputsHash = "inputs_hash"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        taskIDs = try container.decodeIfPresent([String].self, forKey: .taskIDs) ?? []
        weightTokens = try container.decodeIfPresent(Int.self, forKey: .weightTokens) ?? 0
        remainingBudgetTokens = try container.decodeIfPresent(Int.self, forKey: .remainingBudgetTokens)
        complete = try container.decodeIfPresent(Bool.self, forKey: .complete) ?? false
        taskCount = try container.decodeIfPresent(Int.self, forKey: .taskCount) ?? 0
        edgeCount = try container.decodeIfPresent(Int.self, forKey: .edgeCount) ?? 0
        inputsHash = try container.decodeIfPresent(String.self, forKey: .inputsHash) ?? ""
    }
}

struct GovernorReserve: Decodable, Sendable, Equatable {
    var state: String = "disabled"
    var reserveRatio: Double = 0.25
    var throttleAtRatio: Double = 0.35
    var criticalBoost: Int = 25
    var minTaskBudget: Int = 0
    var limitTokens: Int?
    var spentTokens: Int = 0
    var reservedTokens: Int = 0
    var remainingTokens: Int?
    var pathWeightTokens: Int = 0
    var ratioReserveTokens: Int = 0
    var protectedTokens: Int = 0
    var headroomTokens: Int?
    var throttled: Bool = false

    enum CodingKeys: String, CodingKey {
        case state
        case reserveRatio = "reserve_ratio"
        case throttleAtRatio = "throttle_at_ratio"
        case criticalBoost = "critical_boost"
        case minTaskBudget = "min_task_budget"
        case limitTokens = "limit_tokens"
        case spentTokens = "spent_tokens"
        case reservedTokens = "reserved_tokens"
        case remainingTokens = "remaining_tokens"
        case pathWeightTokens = "path_weight_tokens"
        case ratioReserveTokens = "ratio_reserve_tokens"
        case protectedTokens = "protected_tokens"
        case headroomTokens = "headroom_tokens"
        case throttled
    }

    init() {}

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        state = try container.decodeIfPresent(String.self, forKey: .state) ?? "disabled"
        reserveRatio = try container.decodeIfPresent(Double.self, forKey: .reserveRatio) ?? 0.25
        throttleAtRatio = try container.decodeIfPresent(Double.self, forKey: .throttleAtRatio) ?? 0.35
        criticalBoost = try container.decodeIfPresent(Int.self, forKey: .criticalBoost) ?? 25
        minTaskBudget = try container.decodeIfPresent(Int.self, forKey: .minTaskBudget) ?? 0
        limitTokens = try container.decodeIfPresent(Int.self, forKey: .limitTokens)
        spentTokens = try container.decodeIfPresent(Int.self, forKey: .spentTokens) ?? 0
        reservedTokens = try container.decodeIfPresent(Int.self, forKey: .reservedTokens) ?? 0
        remainingTokens = try container.decodeIfPresent(Int.self, forKey: .remainingTokens)
        pathWeightTokens = try container.decodeIfPresent(Int.self, forKey: .pathWeightTokens) ?? 0
        ratioReserveTokens = try container.decodeIfPresent(Int.self, forKey: .ratioReserveTokens) ?? 0
        protectedTokens = try container.decodeIfPresent(Int.self, forKey: .protectedTokens) ?? 0
        headroomTokens = try container.decodeIfPresent(Int.self, forKey: .headroomTokens)
        throttled = try container.decodeIfPresent(Bool.self, forKey: .throttled) ?? false
    }
}

struct GovernorTaskDecision: Decodable, Sendable, Equatable {
    var taskID: String = ""
    var department: String = ""
    var critical: Bool = false
    var weightTokens: Int = 0
    var order: Int = 0
    var action: String = "defer"
    var reason: String = ""
    var code: String = ""
    var remainingAfterTokens: Int?

    enum CodingKeys: String, CodingKey {
        case taskID = "task_id"
        case department
        case critical
        case weightTokens = "weight_tokens"
        case order
        case action
        case reason
        case code
        case remainingAfterTokens = "remaining_after_tokens"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID) ?? ""
        department = try container.decodeIfPresent(String.self, forKey: .department) ?? ""
        critical = try container.decodeIfPresent(Bool.self, forKey: .critical) ?? false
        weightTokens = try container.decodeIfPresent(Int.self, forKey: .weightTokens) ?? 0
        order = try container.decodeIfPresent(Int.self, forKey: .order) ?? 0
        action = try container.decodeIfPresent(String.self, forKey: .action) ?? "defer"
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        code = try container.decodeIfPresent(String.self, forKey: .code) ?? ""
        remainingAfterTokens = try container.decodeIfPresent(Int.self, forKey: .remainingAfterTokens)
    }
}

struct GovernorDeferral: Decodable, Sendable, Equatable {
    var taskID: String = ""
    var department: String = ""
    var critical: Bool = false
    var weightTokens: Int = 0
    var reason: String = ""
    var code: String = ""
    var remainingTokens: Int?
    var protectedTokens: Int = 0

    enum CodingKeys: String, CodingKey {
        case taskID = "task_id"
        case department
        case critical
        case weightTokens = "weight_tokens"
        case reason
        case code
        case remainingTokens = "remaining_tokens"
        case protectedTokens = "protected_tokens"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID) ?? ""
        department = try container.decodeIfPresent(String.self, forKey: .department) ?? ""
        critical = try container.decodeIfPresent(Bool.self, forKey: .critical) ?? false
        weightTokens = try container.decodeIfPresent(Int.self, forKey: .weightTokens) ?? 0
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        code = try container.decodeIfPresent(String.self, forKey: .code) ?? ""
        remainingTokens = try container.decodeIfPresent(Int.self, forKey: .remainingTokens)
        protectedTokens = try container.decodeIfPresent(Int.self, forKey: .protectedTokens) ?? 0
    }
}

struct GovernorDecision: Decodable, Sendable, Identifiable, Equatable {
    var decisionID: String = ""
    var sessionID: String = ""
    var planRevision: Int = 0
    var action: String = ""
    var taskID: String?
    var critical: Bool = false
    var reason: String = ""
    var code: String = ""
    var detail: String = ""
    var path: [String] = []
    var pathWeightTokens: Int = 0
    var limitTokens: Int?
    var spentTokens: Int = 0
    var reservedTokens: Int = 0
    var remainingTokens: Int?
    var protectedTokens: Int = 0
    var requestedTokens: Int?
    var admitted: [String] = []
    var deferred: [GovernorDeferral] = []
    var inputsHash: String = ""
    var createdAt: String = ""

    var id: String {
        decisionID
    }

    enum CodingKeys: String, CodingKey {
        case decisionID = "decision_id"
        case sessionID = "session_id"
        case planRevision = "plan_revision"
        case action
        case taskID = "task_id"
        case critical
        case reason
        case code
        case detail
        case path
        case pathWeightTokens = "path_weight_tokens"
        case limitTokens = "limit_tokens"
        case spentTokens = "spent_tokens"
        case reservedTokens = "reserved_tokens"
        case remainingTokens = "remaining_tokens"
        case protectedTokens = "protected_tokens"
        case requestedTokens = "requested_tokens"
        case admitted
        case deferred
        case inputsHash = "inputs_hash"
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        decisionID = try container.decodeIfPresent(String.self, forKey: .decisionID) ?? ""
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        action = try container.decodeIfPresent(String.self, forKey: .action) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        critical = try container.decodeIfPresent(Bool.self, forKey: .critical) ?? false
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        code = try container.decodeIfPresent(String.self, forKey: .code) ?? ""
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
        path = try container.decodeIfPresent([String].self, forKey: .path) ?? []
        pathWeightTokens = try container.decodeIfPresent(Int.self, forKey: .pathWeightTokens) ?? 0
        limitTokens = try container.decodeIfPresent(Int.self, forKey: .limitTokens)
        spentTokens = try container.decodeIfPresent(Int.self, forKey: .spentTokens) ?? 0
        reservedTokens = try container.decodeIfPresent(Int.self, forKey: .reservedTokens) ?? 0
        remainingTokens = try container.decodeIfPresent(Int.self, forKey: .remainingTokens)
        protectedTokens = try container.decodeIfPresent(Int.self, forKey: .protectedTokens) ?? 0
        requestedTokens = try container.decodeIfPresent(Int.self, forKey: .requestedTokens)
        admitted = try container.decodeIfPresent([String].self, forKey: .admitted) ?? []
        deferred = try container.decodeIfPresent([GovernorDeferral].self, forKey: .deferred) ?? []
        inputsHash = try container.decodeIfPresent(String.self, forKey: .inputsHash) ?? ""
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

struct GovernorFailure: Decodable, Sendable, Equatable {
    var code: String = ""
    var message: String = ""
    var retryable: Bool = false
    var hint: String = ""

    enum CodingKeys: String, CodingKey {
        case code
        case message
        case retryable
        case hint
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        code = try container.decodeIfPresent(String.self, forKey: .code) ?? ""
        message = try container.decodeIfPresent(String.self, forKey: .message) ?? ""
        retryable = try container.decodeIfPresent(Bool.self, forKey: .retryable) ?? false
        hint = try container.decodeIfPresent(String.self, forKey: .hint) ?? ""
    }
}

struct GovernorSessionStatus: Decodable, Sendable, Equatable {
    var sessionID: String = ""
    var projectID: String = ""
    var sessionStatus: String = ""
    var planRevision: Int = 0
    var enabled: Bool = false
    var unlimited: Bool = false
    var planPresent: Bool = false
    var policy: CriticalPathPolicy = .init()
    var criticalPath: GovernorCriticalPath?
    var reserve: GovernorReserve = .init()
    var inputsHash: String = ""
    var decisions: [GovernorTaskDecision] = []
    var admitted: [String] = []
    var deferred: [GovernorDeferral] = []
    var lastDecisions: [GovernorDecision] = []
    var error: GovernorFailure?
    var generatedAt: String = ""

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case projectID = "project_id"
        case sessionStatus = "session_status"
        case planRevision = "plan_revision"
        case enabled
        case unlimited
        case planPresent = "plan_present"
        case policy
        case criticalPath = "critical_path"
        case reserve
        case inputsHash = "inputs_hash"
        case decisions
        case admitted
        case deferred
        case lastDecisions = "last_decisions"
        case error
        case generatedAt = "generated_at"
    }

    init() {}

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        sessionStatus = try container.decodeIfPresent(String.self, forKey: .sessionStatus) ?? ""
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        unlimited = try container.decodeIfPresent(Bool.self, forKey: .unlimited) ?? false
        planPresent = try container.decodeIfPresent(Bool.self, forKey: .planPresent) ?? false
        policy = try container.decodeIfPresent(CriticalPathPolicy.self, forKey: .policy) ?? .init()
        criticalPath = try container.decodeIfPresent(GovernorCriticalPath.self, forKey: .criticalPath)
        reserve = try container.decodeIfPresent(GovernorReserve.self, forKey: .reserve) ?? .init()
        inputsHash = try container.decodeIfPresent(String.self, forKey: .inputsHash) ?? ""
        decisions = try container.decodeIfPresent([GovernorTaskDecision].self, forKey: .decisions) ?? []
        admitted = try container.decodeIfPresent([String].self, forKey: .admitted) ?? []
        deferred = try container.decodeIfPresent([GovernorDeferral].self, forKey: .deferred) ?? []
        lastDecisions = try container.decodeIfPresent([GovernorDecision].self, forKey: .lastDecisions) ?? []
        error = try container.decodeIfPresent(GovernorFailure.self, forKey: .error)
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
    }
}

/// Formatting and derived values for the Efficiency card and Stage popover.
enum GovernorMetrics {
    static func percent(_ value: Double?) -> String {
        guard let value, value.isFinite else { return "—" }
        return String(format: "%.0f%%", min(1, max(0, value)) * 100)
    }

    static func pathLabel(_ ids: [String], limit: Int = 6) -> String {
        if ids.isEmpty {
            return "complete"
        }
        let head = ids.prefix(limit).joined(separator: " → ")
        if ids.count > limit {
            return head + " → +\(ids.count - limit)"
        }
        return head
    }

    static func reserveFraction(_ reserve: GovernorReserve) -> Double? {
        guard let remaining = reserve.remainingTokens, remaining > 0 else { return nil }
        return min(1, max(0, Double(reserve.protectedTokens) / Double(remaining)))
    }

    static func lastDecision(_ status: GovernorSessionStatus?) -> GovernorDecision? {
        status?.lastDecisions.last
    }

    static func isPressure(_ reserve: GovernorReserve) -> Bool {
        reserve.throttled
    }
}

struct CriticalPathPolicyResult: Decodable, Sendable {
    var projectID: String = ""
    var policy: CriticalPathPolicy = .init()

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case policy
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        policy = try container.decodeIfPresent(CriticalPathPolicy.self, forKey: .policy) ?? .init()
    }
}
