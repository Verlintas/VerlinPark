import Foundation

// Read-only Run Contract/resource/readiness models. Every value comes from
// `session.resources` / `session.readiness` / `session.run_contract.get`; the
// client never derives a budget or readiness claim from local state.

enum RunProfile: String, Codable, CaseIterable, Sendable, Identifiable {
    case quality
    case balanced
    case fast
    case cheap
    case unattended

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .quality: "Quality"
        case .balanced: "Balanced"
        case .fast: "Fast"
        case .cheap: "Cheap"
        case .unattended: "Unattended"
        }
    }

    static func fromWire(_ value: String) -> RunProfile {
        RunProfile(rawValue: value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased())
            ?? .balanced
    }
}

struct ResourceDimensionView: Codable, Sendable, Identifiable, Equatable {
    let key: String
    let label: String
    let unit: String
    let used: Double
    let remaining: Double?
    let softTarget: Double?
    let hardCap: Double?
    let forecastP50: Double?
    let forecastP90: Double?
    let state: String
    let source: String
    let note: String?

    var id: String {
        key
    }

    enum CodingKeys: String, CodingKey {
        case key, label, unit, used, remaining, state, source, note
        case softTarget = "soft_target"
        case hardCap = "hard_cap"
        case forecastP50 = "forecast_p50"
        case forecastP90 = "forecast_p90"
    }

    var stateLabel: String {
        switch state {
        case "hard_limit": "LIMIT"
        case "soft_limit": "SOFT"
        case "watch": "WATCH"
        case "unavailable": "N/A"
        default: "OK"
        }
    }

    var isAttention: Bool {
        matches(state, "soft_limit", "hard_limit", "watch")
    }
}

struct ResourceCountView: Codable, Sendable, Equatable {
    let key: String
    let count: UInt32
    let weight: UInt32
}

struct ResourceAttentionView: Codable, Sendable, Equatable {
    let pending: UInt32
    let approved: UInt32
    let denied: UInt32
    let expired: UInt32
    let interrupts: UInt32
    let byRisk: [ResourceCountView]

    enum CodingKeys: String, CodingKey {
        case pending, approved, denied, expired, interrupts
        case byRisk = "by_risk"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        pending = try container.decodeIfPresent(UInt32.self, forKey: .pending) ?? 0
        approved = try container.decodeIfPresent(UInt32.self, forKey: .approved) ?? 0
        denied = try container.decodeIfPresent(UInt32.self, forKey: .denied) ?? 0
        expired = try container.decodeIfPresent(UInt32.self, forKey: .expired) ?? 0
        interrupts = try container.decodeIfPresent(UInt32.self, forKey: .interrupts) ?? 0
        byRisk = try container.decodeIfPresent([ResourceCountView].self, forKey: .byRisk) ?? []
    }
}

struct ReadinessSnapshotView: Codable, Sendable, Equatable {
    let sessionID: String
    let level: String
    let shippable: Bool
    let mergeable: Bool
    let latestCommit: String?
    let latestTest: String?
    let verifier: String?
    let merge: String?
    let changedPaths: [String]
    let remainingWork: [String]
    let blockers: [String]
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case level, shippable, mergeable, verifier, merge, blockers
        case sessionID = "session_id"
        case latestCommit = "latest_commit"
        case latestTest = "latest_test"
        case changedPaths = "changed_paths"
        case remainingWork = "remaining_work"
        case generatedAt = "generated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        level = try container.decodeIfPresent(String.self, forKey: .level) ?? "none"
        shippable = try container.decodeIfPresent(Bool.self, forKey: .shippable) ?? false
        mergeable = try container.decodeIfPresent(Bool.self, forKey: .mergeable) ?? false
        latestCommit = try container.decodeIfPresent(String.self, forKey: .latestCommit)
        latestTest = try container.decodeIfPresent(String.self, forKey: .latestTest)
        verifier = try container.decodeIfPresent(String.self, forKey: .verifier)
        merge = try container.decodeIfPresent(String.self, forKey: .merge)
        changedPaths = try container.decodeIfPresent([String].self, forKey: .changedPaths) ?? []
        remainingWork = try container.decodeIfPresent([String].self, forKey: .remainingWork) ?? []
        blockers = try container.decodeIfPresent([String].self, forKey: .blockers) ?? []
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
    }

    var levelLabel: String {
        switch level {
        case "merged": "MERGED"
        case "verified": "VERIFIED"
        case "committed": "COMMITTED"
        case "dirty": "DIRTY"
        case "partial": "PARTIAL"
        case "blocked": "BLOCKED"
        default: "NONE"
        }
    }

    var needsAttention: Bool {
        level == "blocked" || level == "dirty" || !blockers.isEmpty
    }
}

struct ResourceSnapshotView: Codable, Sendable, Equatable {
    let sessionID: String
    let generatedAt: String
    let profile: String
    let dimensions: [ResourceDimensionView]
    let bottleneck: String?
    let attention: ResourceAttentionView
    let readiness: ReadinessSnapshotView?
    let forecast: BudgetForecast?
    let warnings: [String]
    let sampleCalls: UInt32
    let pricingSource: String

    enum CodingKeys: String, CodingKey {
        case profile, dimensions, bottleneck, attention, readiness, forecast, warnings
        case sessionID = "session_id"
        case generatedAt = "generated_at"
        case sampleCalls = "sample_calls"
        case pricingSource = "pricing_source"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        profile = try container.decodeIfPresent(String.self, forKey: .profile) ?? "balanced"
        dimensions =
            try container.decodeIfPresent([ResourceDimensionView].self, forKey: .dimensions) ?? []
        bottleneck = try container.decodeIfPresent(String.self, forKey: .bottleneck)
        attention =
            try container.decodeIfPresent(ResourceAttentionView.self, forKey: .attention)
                ?? ResourceAttentionView.empty
        readiness = try container.decodeIfPresent(ReadinessSnapshotView.self, forKey: .readiness)
        forecast = try container.decodeIfPresent(BudgetForecast.self, forKey: .forecast)
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
        sampleCalls = try container.decodeIfPresent(UInt32.self, forKey: .sampleCalls) ?? 0
        pricingSource =
            try container.decodeIfPresent(String.self, forKey: .pricingSource) ?? "unconfigured"
    }

    func dimension(_ key: String) -> ResourceDimensionView? {
        dimensions.first { $0.key == key }
    }

    var profileValue: RunProfile {
        RunProfile.fromWire(profile)
    }

    var needsAttention: Bool {
        bottleneck != nil || readiness?.needsAttention == true
            || dimensions.contains(where: \.isAttention)
    }
}

extension ResourceAttentionView {
    static let empty = ResourceAttentionView(
        pending: 0,
        approved: 0,
        denied: 0,
        expired: 0,
        interrupts: 0,
        byRisk: []
    )

    init(
        pending: UInt32,
        approved: UInt32,
        denied: UInt32,
        expired: UInt32,
        interrupts: UInt32,
        byRisk: [ResourceCountView]
    ) {
        self.pending = pending
        self.approved = approved
        self.denied = denied
        self.expired = expired
        self.interrupts = interrupts
        self.byRisk = byRisk
    }
}

struct ResourceSoftTargetsView: Codable, Sendable, Equatable {
    let costUSD: Double?
    let timeSecs: UInt64?
    let attention: UInt32?

    enum CodingKeys: String, CodingKey {
        case costUSD = "cost_usd"
        case timeSecs = "time_secs"
        case attention
    }
}

struct RunStopPolicyView: Codable, Sendable, Equatable {
    let requireEvidence: Bool
    let noProgressTurns: UInt32
    let marginalValueRatio: Double
    let partialDelivery: Bool

    enum CodingKeys: String, CodingKey {
        case requireEvidence = "require_evidence"
        case noProgressTurns = "no_progress_turns"
        case marginalValueRatio = "marginal_value_ratio"
        case partialDelivery = "partial_delivery"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        requireEvidence =
            try container.decodeIfPresent(Bool.self, forKey: .requireEvidence) ?? true
        noProgressTurns =
            try container.decodeIfPresent(UInt32.self, forKey: .noProgressTurns) ?? 6
        marginalValueRatio =
            try container.decodeIfPresent(Double.self, forKey: .marginalValueRatio) ?? 0.2
        partialDelivery =
            try container.decodeIfPresent(Bool.self, forKey: .partialDelivery) ?? true
    }
}

struct RunContractView: Codable, Sendable, Equatable {
    let sessionID: String
    let projectID: String
    let profile: String
    let source: String
    let goal: String
    let acceptance: [String]
    let riskCeiling: String
    let autonomy: String
    let hardCaps: AutoBudgetCaps
    let softTargets: ResourceSoftTargetsView
    let completionReserveRatio: Double
    let attentionCap: UInt32
    let stopPolicy: RunStopPolicyView
    let warnings: [String]
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case profile, source, goal, acceptance, autonomy, warnings
        case sessionID = "session_id"
        case projectID = "project_id"
        case riskCeiling = "risk_ceiling"
        case hardCaps = "hard_caps"
        case softTargets = "soft_targets"
        case completionReserveRatio = "completion_reserve_ratio"
        case attentionCap = "attention_cap"
        case stopPolicy = "stop_policy"
        case generatedAt = "generated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        profile = try container.decodeIfPresent(String.self, forKey: .profile) ?? "balanced"
        source = try container.decodeIfPresent(String.self, forKey: .source) ?? "derived"
        goal = try container.decodeIfPresent(String.self, forKey: .goal) ?? ""
        acceptance = try container.decodeIfPresent([String].self, forKey: .acceptance) ?? []
        riskCeiling = try container.decodeIfPresent(String.self, forKey: .riskCeiling) ?? ""
        autonomy = try container.decodeIfPresent(String.self, forKey: .autonomy) ?? ""
        hardCaps = try container.decodeIfPresent(AutoBudgetCaps.self, forKey: .hardCaps)
            ?? AutoBudgetCaps()
        softTargets =
            try container.decodeIfPresent(ResourceSoftTargetsView.self, forKey: .softTargets)
                ?? ResourceSoftTargetsView(costUSD: nil, timeSecs: nil, attention: nil)
        completionReserveRatio =
            try container.decodeIfPresent(Double.self, forKey: .completionReserveRatio) ?? 0.2
        attentionCap = try container.decodeIfPresent(UInt32.self, forKey: .attentionCap) ?? 20
        stopPolicy =
            try container.decodeIfPresent(RunStopPolicyView.self, forKey: .stopPolicy)
                ?? RunStopPolicyView(
                    requireEvidence: true,
                    noProgressTurns: 6,
                    marginalValueRatio: 0.2,
                    partialDelivery: true
                )
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
    }
}

extension RunStopPolicyView {
    init(
        requireEvidence: Bool,
        noProgressTurns: UInt32,
        marginalValueRatio: Double,
        partialDelivery: Bool
    ) {
        self.requireEvidence = requireEvidence
        self.noProgressTurns = noProgressTurns
        self.marginalValueRatio = marginalValueRatio
        self.partialDelivery = partialDelivery
    }
}

private func matches(_ value: String, _ options: String...) -> Bool {
    options.contains(value)
}
