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
        case "tests_passed": "TESTS PASSED"
        case "committed": "COMMITTED"
        case "dirty": "DIRTY"
        case "partial": "PARTIAL"
        case "blocked": "BLOCKED"
        default: "NONE"
        }
    }

    var needsAttention: Bool {
        level == "blocked" || level == "dirty" || level == "partial"
            || level == "tests_passed" || !blockers.isEmpty
    }

    /// Explicit, operator-facing caveat for a delivery that is not complete.
    /// `nil` means the daemon-reported level does not need a partial-delivery
    /// warning; the card must not infer mergeability or completion from it.
    var deliveryCaveat: String? {
        switch level {
        case "partial":
            "Partial delivery — remaining work is not complete; this is not mergeable."
        case "committed":
            "Committed locally — tests, Verifier approval, and integration are still pending."
        case "dirty":
            "Uncommitted changes — nothing is shippable or mergeable yet."
        case "blocked":
            "Blocked — resolve the blocker before this can be merged."
        case "tests_passed":
            "Tests passed locally — Verifier approval and integration are still pending."
        default:
            nil
        }
    }

    /// True for levels that must show remaining work/blockers on the card,
    /// even when the snapshot does not otherwise need attention.
    var showsDeliveryDetail: Bool {
        deliveryCaveat != nil
    }

    var shippabilityLabel: String {
        shippable ? "shippable" : "not shippable"
    }

    var mergeabilityLabel: String {
        mergeable ? "mergeable" : "not mergeable"
    }

    var shortCommit: String? {
        guard let latestCommit, !latestCommit.isEmpty else { return nil }
        return String(latestCommit.prefix(12))
    }

    /// Bounded plain-text snapshot for the "Copy readiness report" action.
    /// Uses only daemon-provided fields; it never upgrades a level or claims a
    /// merge that the snapshot did not report.
    func boundedReport(maxItems: Int = 8, maxCharacters: Int = 2400) -> String {
        let itemLimit = max(0, maxItems)
        var lines = [
            "VerlinPark readiness report",
            "Level: \(levelLabel) (\(level))",
            "Shippable: \(shippable ? "yes" : "no")",
            "Mergeable: \(mergeable ? "yes" : "no")",
        ]
        if let latestCommit, !latestCommit.isEmpty {
            lines.append("Commit: \(latestCommit)")
        }
        if let latestTest, !latestTest.isEmpty {
            lines.append("Latest test: \(latestTest)")
        }
        if let verifier, !verifier.isEmpty {
            lines.append("Verifier: \(verifier)")
        }
        if let merge, !merge.isEmpty {
            lines.append("Merge: \(merge)")
        }
        if let caveat = deliveryCaveat {
            lines.append("Note: \(caveat)")
        }
        if !changedPaths.isEmpty {
            lines.append("Changed paths (\(changedPaths.count)):")
            lines.append(contentsOf: changedPaths.prefix(itemLimit).map { "- \($0)" })
            appendOverflow(changedPaths.count, limit: itemLimit, to: &lines)
        }
        lines.append("Remaining work (\(remainingWork.count)):")
        if remainingWork.isEmpty {
            lines.append("- none reported")
        } else {
            lines.append(contentsOf: remainingWork.prefix(itemLimit).map { "- \($0)" })
            appendOverflow(remainingWork.count, limit: itemLimit, to: &lines)
        }
        lines.append("Blockers (\(blockers.count)):")
        if blockers.isEmpty {
            lines.append("- none reported")
        } else {
            lines.append(contentsOf: blockers.prefix(itemLimit).map { "- \($0)" })
            appendOverflow(blockers.count, limit: itemLimit, to: &lines)
        }
        let text = lines.joined(separator: "\n")
        let suffix = "\n… (truncated)"
        guard text.count > maxCharacters else { return text }
        let prefixCount = max(0, maxCharacters - suffix.count)
        return String(text.prefix(prefixCount)) + suffix
    }

    private func appendOverflow(
        _ total: Int,
        limit: Int,
        to lines: inout [String]
    ) {
        guard total > limit else { return }
        lines.append("- … \(total - limit) more")
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

    static let standard = RunStopPolicyView(
        requireEvidence: true,
        noProgressTurns: 6,
        marginalValueRatio: 0.2,
        partialDelivery: true
    )
}

// MARK: - Project RunPolicy

/// Project-scoped default Run Contract policy. These values only supply
/// defaults for sessions that have no explicit override; they cannot widen
/// approvals, sandbox, task contracts, budgets/capacity, Verifier, Integrator,
/// or model routing. `updated_at` is absent when the daemon is reporting
/// built-in defaults instead of a persisted explicit policy.
struct ProjectRunPolicyView: Codable, Sendable, Equatable {
    let defaultProfile: RunProfile
    let completionReserveRatio: Double
    let attentionCap: UInt32
    let stopPolicy: RunStopPolicyView

    enum CodingKeys: String, CodingKey {
        case defaultProfile = "default_profile"
        case completionReserveRatio = "completion_reserve_ratio"
        case attentionCap = "attention_cap"
        case stopPolicy = "stop_policy"
    }

    init(
        defaultProfile: RunProfile = .balanced,
        completionReserveRatio: Double = 0.20,
        attentionCap: UInt32 = 20,
        stopPolicy: RunStopPolicyView = .standard
    ) {
        self.defaultProfile = defaultProfile
        self.completionReserveRatio = completionReserveRatio
        self.attentionCap = attentionCap
        self.stopPolicy = stopPolicy
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        defaultProfile =
            try container.decodeIfPresent(RunProfile.self, forKey: .defaultProfile) ?? .balanced
        completionReserveRatio =
            try container.decodeIfPresent(Double.self, forKey: .completionReserveRatio) ?? 0.20
        attentionCap =
            try container.decodeIfPresent(UInt32.self, forKey: .attentionCap) ?? 20
        stopPolicy =
            try container.decodeIfPresent(RunStopPolicyView.self, forKey: .stopPolicy)
                ?? .standard
    }

    var profileTitle: String {
        defaultProfile.title
    }

    var reservePercentLabel: String {
        let percent = completionReserveRatio * 100
        if percent.rounded() == percent {
            return "\(Int(percent))%"
        }
        return String(format: "%.1f%%", percent)
    }

    var attentionLabel: String {
        "\(attentionCap)"
    }

    var noProgressLabel: String {
        "\(stopPolicy.noProgressTurns) turns"
    }

    var stopPolicySummary: String {
        var parts: [String] = []
        parts.append(stopPolicy.requireEvidence ? "evidence required" : "evidence optional")
        parts.append(stopPolicy.partialDelivery ? "partial delivery allowed" : "no partial delivery")
        return parts.joined(separator: " · ")
    }

    /// Client-side mirror of the daemon validation. The daemon remains
    /// authoritative; this only prevents an empty/invalid request.
    func validate() throws {
        guard completionReserveRatio.isFinite,
              (0.0 ... 0.50).contains(completionReserveRatio)
        else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "Completion reserve must be between 0% and 50%."
            )
        }
        guard attentionCap > 0, attentionCap <= 1000 else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "Attention cap must be between 1 and 1000 interrupts."
            )
        }
        guard stopPolicy.noProgressTurns <= 1000 else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "No-progress turns must be at most 1000."
            )
        }
        guard stopPolicy.marginalValueRatio.isFinite,
              (0.0 ... 1.0).contains(stopPolicy.marginalValueRatio)
        else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "The stop policy marginal-value ratio must be between 0 and 1."
            )
        }
    }

    func jsonValue() throws -> JSONValue {
        let data = try JSONEncoder().encode(self)
        return try JSONDecoder().decode(JSONValue.self, from: data)
    }
}

struct ProjectRunPolicyResultView: Codable, Sendable, Equatable {
    let projectID: String
    let policy: ProjectRunPolicyView
    /// `nil` means the daemon is reporting built-in defaults, not an explicit
    /// project policy row. The UI must not present those defaults as saved.
    let updatedAt: String?

    enum CodingKeys: String, CodingKey {
        case policy
        case projectID = "project_id"
        case updatedAt = "updated_at"
    }

    init(projectID: String, policy: ProjectRunPolicyView, updatedAt: String? = nil) {
        self.projectID = projectID
        self.policy = policy
        self.updatedAt = updatedAt
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decode(String.self, forKey: .projectID)
        policy = try container.decode(ProjectRunPolicyView.self, forKey: .policy)
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt)
    }

    var isExplicit: Bool {
        guard let updatedAt else { return false }
        return !updatedAt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var sourceLabel: String {
        isExplicit ? "Explicit project policy" : "Built-in defaults"
    }

    var updatedLabel: String? {
        guard isExplicit, let updatedAt else { return nil }
        return "saved \(updatedAt)"
    }
}

/// Editable form state for the project policy sheet. Parsing/validation stays
/// here so views own bindings only and the daemon is never asked to store an
/// empty or out-of-range policy.
struct ProjectRunPolicyDraft: Equatable, Sendable {
    var profile: RunProfile = .balanced
    var completionReservePercentText = "20"
    var attentionCapText = "20"
    var noProgressTurnsText = "6"
    var requireEvidence = true
    var partialDelivery = true
    var marginalValueRatio = 0.2

    init() {}

    init(policy: ProjectRunPolicyView) {
        profile = policy.defaultProfile
        completionReservePercentText = Self.percentText(policy.completionReserveRatio)
        attentionCapText = String(policy.attentionCap)
        noProgressTurnsText = String(policy.stopPolicy.noProgressTurns)
        requireEvidence = policy.stopPolicy.requireEvidence
        partialDelivery = policy.stopPolicy.partialDelivery
        marginalValueRatio = policy.stopPolicy.marginalValueRatio
    }

    var validationIssue: BudgetValidationIssue? {
        do {
            _ = try makePolicy()
            return nil
        } catch {
            return error as? BudgetValidationIssue
                ?? BudgetValidationIssue(
                    code: "INVALID_PROJECT_RUN_POLICY",
                    message: error.localizedDescription
                )
        }
    }

    func makePolicy() throws -> ProjectRunPolicyView {
        let percent = try Self.requiredDecimal(
            completionReservePercentText,
            label: "Completion reserve percent"
        )
        guard percent >= 0, percent <= 50 else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "Completion reserve must be between 0% and 50%."
            )
        }
        let attentionCap = try Self.requiredInteger(
            attentionCapText,
            label: "Attention cap"
        )
        guard attentionCap >= 1, attentionCap <= 1000 else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "Attention cap must be between 1 and 1000 interrupts."
            )
        }
        let noProgressTurns = try Self.requiredInteger(
            noProgressTurnsText,
            label: "No-progress turns"
        )
        guard noProgressTurns <= 1000 else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "No-progress turns must be at most 1000."
            )
        }
        let policy = ProjectRunPolicyView(
            defaultProfile: profile,
            completionReserveRatio: percent / 100,
            attentionCap: attentionCap,
            stopPolicy: RunStopPolicyView(
                requireEvidence: requireEvidence,
                noProgressTurns: noProgressTurns,
                marginalValueRatio: marginalValueRatio,
                partialDelivery: partialDelivery
            )
        )
        try policy.validate()
        return policy
    }

    private static func percentText(_ ratio: Double) -> String {
        let percent = ratio * 100
        if percent.rounded() == percent {
            return String(Int(percent))
        }
        return String(
            format: "%.1f",
            locale: Locale(identifier: "en_US_POSIX"),
            percent
        )
    }

    private static func requiredDecimal(
        _ raw: String,
        label: String
    ) throws -> Double {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value = Double(text), value.isFinite else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "\(label) must be a number."
            )
        }
        return value
    }

    private static func requiredInteger(
        _ raw: String,
        label: String
    ) throws -> UInt32 {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value = UInt32(text) else {
            throw BudgetValidationIssue(
                code: "INVALID_PROJECT_RUN_POLICY",
                message: "\(label) must be a whole number."
            )
        }
        return value
    }
}

// MARK: - Resource pause event

struct AttentionBatchItemView: Codable, Equatable, Sendable, Identifiable {
    let approvalID: String
    let tool: String
    let risk: String?
    let taskID: String?
    let priority: Int
    let waiters: UInt32
    let reason: String?

    var id: String {
        approvalID
    }

    enum CodingKeys: String, CodingKey {
        case tool, risk, priority, waiters, reason
        case approvalID = "approval_id"
        case taskID = "task_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        approvalID =
            try container.decodeIfPresent(String.self, forKey: .approvalID) ?? ""
        tool = try container.decodeIfPresent(String.self, forKey: .tool) ?? "unknown"
        risk = try container.decodeIfPresent(String.self, forKey: .risk)
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        priority = try container.decodeIfPresent(Int.self, forKey: .priority) ?? 0
        waiters = try container.decodeIfPresent(UInt32.self, forKey: .waiters) ?? 0
        reason = try container.decodeIfPresent(String.self, forKey: .reason)
    }

    var riskLabel: String {
        (risk ?? "unknown").uppercased()
    }

    var taskShortLabel: String? {
        guard let taskID, !taskID.isEmpty else { return nil }
        return String(taskID.prefix(18))
    }

    var reasonLabel: String? {
        guard let reason else { return nil }
        let trimmed = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : String(trimmed.prefix(160))
    }
}

struct AttentionBatchRiskView: Codable, Equatable, Sendable, Identifiable {
    let risk: String
    let count: UInt32
    let weight: UInt32

    var id: String {
        risk
    }

    enum CodingKeys: String, CodingKey {
        case risk, count, weight
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        risk = try container.decodeIfPresent(String.self, forKey: .risk) ?? "unknown"
        count = try container.decodeIfPresent(UInt32.self, forKey: .count) ?? 0
        weight = try container.decodeIfPresent(UInt32.self, forKey: .weight) ?? 0
    }
}

/// Bounded attention prompt attached to a turn-boundary pause. It is metadata
/// only: the approval queue remains the decision surface.
struct AttentionBatchView: Codable, Equatable, Sendable {
    static let schema = "verlinpark.attention_batch/v1"

    let schema: String
    let pending: UInt32
    let interrupts: UInt32
    let cap: UInt32
    let byRisk: [AttentionBatchRiskView]
    let items: [AttentionBatchItemView]
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case schema, pending, interrupts, cap, items, truncated
        case byRisk = "by_risk"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schema = try container.decodeIfPresent(String.self, forKey: .schema) ?? ""
        pending = try container.decodeIfPresent(UInt32.self, forKey: .pending) ?? 0
        interrupts = try container.decodeIfPresent(UInt32.self, forKey: .interrupts) ?? 0
        cap = try container.decodeIfPresent(UInt32.self, forKey: .cap) ?? 0
        byRisk =
            try container.decodeIfPresent([AttentionBatchRiskView].self, forKey: .byRisk) ?? []
        items =
            try container.decodeIfPresent([AttentionBatchItemView].self, forKey: .items) ?? []
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }

    var summaryLabel: String {
        L10n.format(
            "%lld pending · %lld/%lld interrupts",
            pending,
            interrupts,
            cap
        )
    }

    var remainingCount: Int {
        max(0, Int(pending) - items.count)
    }

    var riskSummary: String? {
        let parts = byRisk.prefix(3).map { "\($0.risk) \($0.count)" }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// Bounded projection of the newest `session.resource.paused` event for the
/// selected session. A later resume/status event clears it; a bottleneck alone
/// never creates one. If the event stream is unavailable the card must show
/// nothing rather than infer a pause.
struct ResourcePauseInfo: Equatable, Sendable {
    static let schema = "verlinpark.resource_pause/v1"
    static let eventKind = "session.resource.paused"

    let sequence: UInt64
    let phase: String?
    let action: String?
    let reason: String
    let bottleneck: String?
    let role: String?
    let agentID: String?
    let taskID: String?
    let generatedAt: String?
    let attentionBatch: AttentionBatchView?

    init?(event: Event) {
        guard event.type == Self.eventKind,
              let payload = event.payload.objectValue,
              payload["schema"]?.stringValue == Self.schema,
              let reason = payload["reason"]?.stringValue,
              !reason.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else {
            return nil
        }
        sequence = event.seq
        phase = payload["phase"]?.stringValue
        action = payload["action"]?.stringValue
        self.reason = reason
        bottleneck = payload["bottleneck"]?.stringValue
        role = payload["role"]?.stringValue
        agentID = payload["agent_id"]?.stringValue
        taskID = payload["task_id"]?.stringValue
        generatedAt = payload["generated_at"]?.stringValue
        attentionBatch = payload["details"]?
            .objectValue?["attention_batch"]
            .flatMap { value in
                guard let batch = try? value.decode(AttentionBatchView.self),
                      batch.schema == AttentionBatchView.schema
                else { return nil }
                return batch
            }
    }

    var reasonLabel: String {
        String(reason.trimmingCharacters(in: .whitespacesAndNewlines).prefix(240))
    }

    var roleLabel: String {
        Self.bounded(role) ?? "role not reported"
    }

    var bottleneckLabel: String {
        Self.bounded(bottleneck) ?? "bottleneck not reported"
    }

    var phaseActionLabel: String {
        let parts = [phase, action]
            .compactMap { Self.bounded($0) }
        return parts.isEmpty ? "phase not reported" : parts.joined(separator: " · ")
    }

    var taskShortLabel: String? {
        guard let taskID = Self.bounded(taskID) else { return nil }
        return String(taskID.prefix(24))
    }

    var agentShortLabel: String? {
        guard let agentID = Self.bounded(agentID) else { return nil }
        return String(agentID.prefix(12))
    }

    static func resumeOutcomeNotice(resumed: Bool) -> String {
        if resumed {
            return "The daemon reported the session resumed. The pause clears when its resume event arrives."
        }
        return "The daemon did not resume the session. It may re-pause while the resource cap is unchanged."
    }

    /// Newest relevant event wins. Only real pause and clearing events are
    /// considered; a `bottleneck` value on a resource snapshot is not a pause.
    static func activePause(
        in events: [Event],
        sessionID: String,
        eventStreamAvailable: Bool
    ) -> ResourcePauseInfo? {
        guard eventStreamAvailable else { return nil }
        var latest: Event?
        for event in events where event.sessionId == sessionID {
            guard event.type == eventKind || clearsPause(event) else { continue }
            if let current = latest {
                if event.seq >= current.seq {
                    latest = event
                }
            } else {
                latest = event
            }
        }
        guard let latest else { return nil }
        if clearsPause(latest) {
            return nil
        }
        return ResourcePauseInfo(event: latest)
    }

    /// A later resume or non-paused session status means the pause event is no
    /// longer current. A `session.updated` with `paused` does not clear it.
    static func clearsPause(_ event: Event) -> Bool {
        switch event.type {
        case "agent.resumed", "session.resumed":
            return true
        case "session.updated":
            let status = (event.payload.objectValue?["status"]?.stringValue ?? "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .lowercased()
            return [
                "running", "resumed", "idle", "failed", "cancelled", "landed",
                "interrupted", "completed",
            ].contains(status)
        default:
            return false
        }
    }

    private static func bounded(_ value: String?) -> String? {
        guard let value else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(160))
    }
}

// MARK: - Operator resource top-up

/// Additive operator top-up. The daemon clamps the resulting effective budget
/// to existing hard caps; this request can never remove a cap, select
/// Unlimited, or resume approval/verification/merge gates.
struct TopUpSessionResourcesRequest: Equatable, Sendable {
    let addTokens: UInt64?
    let addCostUSD: Double?
    let addTimeSecs: UInt64?
    let resume: Bool
    let reason: String?

    init(
        addTokens: UInt64? = nil,
        addCostUSD: Double? = nil,
        addTimeSecs: UInt64? = nil,
        resume: Bool = false,
        reason: String? = nil
    ) {
        self.addTokens = addTokens
        self.addCostUSD = addCostUSD
        self.addTimeSecs = addTimeSecs
        self.resume = resume
        self.reason = reason
    }

    var hasIncrement: Bool {
        addTokens != nil || addCostUSD != nil || addTimeSecs != nil
    }

    /// Nil means valid. The daemon repeats these checks; the client validates
    /// first so an empty or non-positive request never leaves the app.
    var validationIssue: BudgetValidationIssue? {
        do {
            try validate()
            return nil
        } catch {
            return error as? BudgetValidationIssue
                ?? BudgetValidationIssue(
                    code: "INVALID_RESOURCE_TOP_UP",
                    message: error.localizedDescription
                )
        }
    }

    func validate() throws {
        guard hasIncrement else {
            throw BudgetValidationIssue(
                code: "INVALID_RESOURCE_TOP_UP",
                message: "Enter at least one positive token, cost, or time increment."
            )
        }
        if let addTokens, addTokens == 0 {
            throw BudgetValidationIssue(
                code: "INVALID_RESOURCE_TOP_UP",
                message: "Token top-up must be greater than zero."
            )
        }
        if let addCostUSD, !addCostUSD.isFinite || addCostUSD <= 0 {
            throw BudgetValidationIssue(
                code: "INVALID_RESOURCE_TOP_UP",
                message: "Cost top-up must be a positive finite USD amount."
            )
        }
        if let addTimeSecs, addTimeSecs == 0 {
            throw BudgetValidationIssue(
                code: "INVALID_RESOURCE_TOP_UP",
                message: "Time top-up must be greater than zero."
            )
        }
    }
}

/// Pure text-field parsing for the Stage top-up sheet. Views own bindings only;
/// numeric interpretation and validation stay here so they are testable.
struct TopUpSessionResourcesDraft: Equatable, Sendable {
    var tokensText = ""
    var costUSDText = ""
    var timeSecsText = ""
    var resume = false

    var hasAnyInput: Bool {
        [tokensText, costUSDText, timeSecsText].contains {
            !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    var validationIssue: BudgetValidationIssue? {
        do {
            _ = try validatedRequest()
            return nil
        } catch {
            return error as? BudgetValidationIssue
                ?? BudgetValidationIssue(
                    code: "INVALID_RESOURCE_TOP_UP",
                    message: error.localizedDescription
                )
        }
    }

    var request: TopUpSessionResourcesRequest? {
        try? validatedRequest()
    }

    func validatedRequest(reason: String? = nil) throws -> TopUpSessionResourcesRequest {
        let request = try TopUpSessionResourcesRequest(
            addTokens: Self.positiveInteger(tokensText, label: "Token top-up"),
            addCostUSD: Self.positiveCost(costUSDText),
            addTimeSecs: Self.positiveInteger(timeSecsText, label: "Time top-up"),
            resume: resume,
            reason: reason
        )
        try request.validate()
        return request
    }

    mutating func resetIncrements() {
        tokensText = ""
        costUSDText = ""
        timeSecsText = ""
    }

    private static func positiveInteger(
        _ raw: String,
        label: String
    ) throws -> UInt64? {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        guard let value = UInt64(text), value > 0 else {
            throw BudgetValidationIssue(
                code: "INVALID_RESOURCE_TOP_UP",
                message: "\(label) must be a whole number greater than zero."
            )
        }
        return value
    }

    private static func positiveCost(_ raw: String) throws -> Double? {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        guard let value = Double(text), value.isFinite, value > 0 else {
            throw BudgetValidationIssue(
                code: "INVALID_RESOURCE_TOP_UP",
                message: "Cost top-up must be a positive USD amount."
            )
        }
        return value
    }
}

/// Exact wire result of `session.resources.top_up`. `budget` and `resources`
/// are required: a partial response is a decode error, never a fabricated
/// success. `resumed`/`capped` are conservative defaults for additive
/// compatibility with an older daemon.
struct TopUpSessionResourcesResult: Codable, Sendable, Equatable {
    let sessionID: String
    let budget: SessionBudgetStatus
    let resumed: Bool
    let capped: Bool
    let resources: ResourceSnapshotView

    enum CodingKeys: String, CodingKey {
        case budget, resumed, capped, resources
        case sessionID = "session_id"
    }

    init(
        sessionID: String,
        budget: SessionBudgetStatus,
        resumed: Bool,
        capped: Bool,
        resources: ResourceSnapshotView
    ) {
        self.sessionID = sessionID
        self.budget = budget
        self.resumed = resumed
        self.capped = capped
        self.resources = resources
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        budget = try container.decode(SessionBudgetStatus.self, forKey: .budget)
        resumed = try container.decodeIfPresent(Bool.self, forKey: .resumed) ?? false
        capped = try container.decodeIfPresent(Bool.self, forKey: .capped) ?? false
        resources = try container.decode(ResourceSnapshotView.self, forKey: .resources)
    }

    /// Truthful bounded outcome text for the sheet/toast. It never claims a
    /// resume that the daemon did not report.
    func operatorNotice(resumeRequested: Bool) -> String {
        var parts: [String] = []
        if budget.unlimited {
            parts.append("Session is Unlimited; no cap was added or removed")
        } else if capped {
            parts.append("Budget clamped to the existing hard cap")
        } else {
            parts.append("Budget updated within existing caps")
        }
        if resumeRequested {
            parts.append(resumed ? "session resumed" : "session not resumed")
        } else if resumed {
            parts.append("session resumed")
        }
        return parts.joined(separator: " · ")
    }
}

private func matches(_ value: String, _ options: String...) -> Bool {
    options.contains(value)
}
