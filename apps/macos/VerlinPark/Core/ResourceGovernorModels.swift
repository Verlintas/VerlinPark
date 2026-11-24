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

// MARK: - Explicit partial delivery

/// Daemon-returned record of an explicitly labeled partial delivery. This is
/// evidence that remaining work was declared; it is never a merge, Verifier
/// approval, completion, or resumable run. Display strings are bounded and
/// missing additive fields decode to conservative defaults.
struct PartialDeliveryRecordView: Codable, Sendable, Equatable, Identifiable {
    static let schema = "verlinpark.partial_delivery/v1"
    static let eventKind = "session.partial_delivery"
    static let summaryCharacterLimit = 2000
    static let remainingItemLimit = 50
    static let remainingItemCharacterLimit = 500
    static let recordedByCharacterLimit = 120

    let schema: String
    let id: String
    let sessionID: String
    let projectID: String
    let recordedAt: String
    let recordedBy: String?
    let summary: String
    let remainingWork: [String]
    let readinessLevel: String
    let latestCommit: String?
    let changedPaths: [String]
    let blockers: [String]
    let uncommitted: Bool

    enum CodingKeys: String, CodingKey {
        case schema, id, summary, blockers, uncommitted
        case sessionID = "session_id"
        case projectID = "project_id"
        case recordedAt = "recorded_at"
        case recordedBy = "recorded_by"
        case remainingWork = "remaining_work"
        case readinessLevel = "readiness_level"
        case latestCommit = "latest_commit"
        case changedPaths = "changed_paths"
    }

    init(
        schema: String = PartialDeliveryRecordView.schema,
        id: String,
        sessionID: String,
        projectID: String = "",
        recordedAt: String = "",
        recordedBy: String? = nil,
        summary: String = "",
        remainingWork: [String] = [],
        readinessLevel: String = "partial",
        latestCommit: String? = nil,
        changedPaths: [String] = [],
        blockers: [String] = [],
        uncommitted: Bool = false
    ) {
        self.schema = schema
        self.id = id
        self.sessionID = sessionID
        self.projectID = projectID
        self.recordedAt = recordedAt
        self.recordedBy = recordedBy
        self.summary = summary
        self.remainingWork = remainingWork
        self.readinessLevel = readinessLevel
        self.latestCommit = latestCommit
        self.changedPaths = changedPaths
        self.blockers = blockers
        self.uncommitted = uncommitted
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schema =
            try container.decodeIfPresent(String.self, forKey: .schema)
                ?? PartialDeliveryRecordView.schema
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        recordedAt = try container.decodeIfPresent(String.self, forKey: .recordedAt) ?? ""
        recordedBy = try container.decodeIfPresent(String.self, forKey: .recordedBy)
        summary = try container.decodeIfPresent(String.self, forKey: .summary) ?? ""
        remainingWork =
            try container.decodeIfPresent([String].self, forKey: .remainingWork) ?? []
        readinessLevel =
            try container.decodeIfPresent(String.self, forKey: .readinessLevel) ?? ""
        latestCommit = try container.decodeIfPresent(String.self, forKey: .latestCommit)
        changedPaths = try container.decodeIfPresent([String].self, forKey: .changedPaths) ?? []
        blockers = try container.decodeIfPresent([String].self, forKey: .blockers) ?? []
        uncommitted = try container.decodeIfPresent(Bool.self, forKey: .uncommitted) ?? false
    }

    var isPartialDelivery: Bool {
        schema == Self.schema
    }

    /// The record's own label. It is intentionally not `MERGED`, `VERIFIED`,
    /// or `COMPLETED`; only the daemon readiness level may report those.
    var statusLabel: String {
        "PARTIAL DELIVERY"
    }

    /// Operator-facing truthfulness note for the compact Stage row.
    var truthfulCaveat: String {
        "Recorded partial delivery — not a merge, approval, or completion."
    }

    var recordedAtLabel: String {
        Self.bounded(recordedAt, limit: 40) ?? "time not reported"
    }

    var recordedByLabel: String? {
        Self.bounded(recordedBy, limit: 80)
    }

    var summaryLabel: String {
        Self.bounded(summary, limit: 280) ?? "No summary recorded"
    }

    var readinessLabel: String {
        let value = readinessLevel.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        switch value {
        case "none": return "NONE"
        case "dirty": return "DIRTY"
        case "committed": return "COMMITTED"
        case "tests_passed": return "TESTS PASSED"
        case "verified": return "VERIFIED"
        case "merged": return "MERGED"
        case "partial": return "PARTIAL"
        case "blocked": return "BLOCKED"
        case "": return "NOT REPORTED"
        default: return String(value.uppercased().prefix(24))
        }
    }

    var remainingCountLabel: String {
        let count = remainingWork.count
        return count == 1 ? "1 remaining item" : "\(count) remaining items"
    }

    var remainingPreview: String? {
        guard let first = remainingWork.first else { return nil }
        return Self.bounded(first, limit: 200)
    }

    var shortCommit: String? {
        guard let latestCommit, !latestCommit.isEmpty else { return nil }
        return String(latestCommit.prefix(12))
    }

    private static func bounded(_ raw: String?, limit: Int) -> String? {
        guard let raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(max(0, limit)))
    }
}

/// Result of `session.delivery`. `partial` is nil when the daemon has no
/// recorded partial delivery; that must never be presented as completion.
struct SessionDeliveryResultView: Codable, Sendable, Equatable {
    let sessionID: String
    let partial: PartialDeliveryRecordView?
    let readiness: ReadinessSnapshotView

    enum CodingKeys: String, CodingKey {
        case partial, readiness
        case sessionID = "session_id"
    }

    init(
        sessionID: String,
        partial: PartialDeliveryRecordView?,
        readiness: ReadinessSnapshotView
    ) {
        self.sessionID = sessionID
        self.partial = partial
        self.readiness = readiness
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        partial = try container.decodeIfPresent(PartialDeliveryRecordView.self, forKey: .partial)
        readiness = try container.decode(ReadinessSnapshotView.self, forKey: .readiness)
    }

    /// Selection guard used by the Stage. A record from another session (or an
    /// unrecognized schema) is never shown as this session's delivery.
    func partialRecord(for targetSessionID: String) -> PartialDeliveryRecordView? {
        guard !targetSessionID.isEmpty, sessionID == targetSessionID, let partial, partial.isPartialDelivery,
              partial.sessionID.isEmpty || partial.sessionID == targetSessionID
        else {
            return nil
        }
        return partial
    }
}

/// Result of `session.delivery.partial`. Both the record and readiness snapshot
/// are required: a partial response is a decode error, never a fabricated
/// success.
struct PartialDeliveryResultView: Codable, Sendable, Equatable {
    let sessionID: String
    let record: PartialDeliveryRecordView
    let readiness: ReadinessSnapshotView

    enum CodingKeys: String, CodingKey {
        case record, readiness
        case sessionID = "session_id"
    }

    init(
        sessionID: String,
        record: PartialDeliveryRecordView,
        readiness: ReadinessSnapshotView
    ) {
        self.sessionID = sessionID
        self.record = record
        self.readiness = readiness
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        record = try container.decode(PartialDeliveryRecordView.self, forKey: .record)
        readiness = try container.decode(ReadinessSnapshotView.self, forKey: .readiness)
    }

    var delivery: SessionDeliveryResultView {
        SessionDeliveryResultView(
            sessionID: sessionID,
            partial: record,
            readiness: readiness
        )
    }
}

/// Bounded, normalized operator input for one explicit partial-delivery
/// record. The daemon repeats validation and is authoritative.
struct PartialDeliveryRequest: Equatable, Sendable {
    let summary: String?
    let remainingWork: [String]
    let recordedBy: String?

    var hasContent: Bool {
        summary != nil || !remainingWork.isEmpty
    }

    /// Trims and client-bounds input. Returns nil when the request would be
    /// empty, which the daemon also rejects.
    static func normalized(
        summary: String?,
        remainingWork: [String],
        recordedBy: String?
    ) -> PartialDeliveryRequest? {
        let summaryText = (summary ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let normalizedSummary = summaryText.isEmpty
            ? nil
            : String(summaryText.prefix(PartialDeliveryRecordView.summaryCharacterLimit))
        let normalizedItems = PartialDeliveryLimits.items(from: remainingWork)
            .prefix(PartialDeliveryRecordView.remainingItemLimit)
            .map { String($0.prefix(PartialDeliveryRecordView.remainingItemCharacterLimit)) }
        let recordedByText = (recordedBy ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let normalizedRecordedBy = recordedByText.isEmpty
            ? nil
            : String(recordedByText.prefix(PartialDeliveryRecordView.recordedByCharacterLimit))
        guard normalizedSummary != nil || !normalizedItems.isEmpty else { return nil }
        return PartialDeliveryRequest(
            summary: normalizedSummary,
            remainingWork: Array(normalizedItems),
            recordedBy: normalizedRecordedBy
        )
    }
}

/// Editable sheet state. Views own bindings only; line parsing, normalization,
/// and bounds stay here so they are unit-testable and cannot drift per view.
struct PartialDeliveryDraft: Equatable, Sendable {
    var summary = ""
    var remainingWorkText = ""
    var recordedBy = ""

    var remainingWorkItems: [String] {
        PartialDeliveryLimits.items(from: remainingWorkText)
    }

    var summaryCharacterCount: Int {
        summary.trimmingCharacters(in: .whitespacesAndNewlines).count
    }

    var remainingItemCount: Int {
        remainingWorkItems.count
    }

    var recordedByCharacterCount: Int {
        recordedBy.trimmingCharacters(in: .whitespacesAndNewlines).count
    }

    var isOverSummaryLimit: Bool {
        summaryCharacterCount > PartialDeliveryRecordView.summaryCharacterLimit
    }

    var isOverRemainingItemLimit: Bool {
        remainingItemCount > PartialDeliveryRecordView.remainingItemLimit
    }

    var hasOverlongRemainingItem: Bool {
        remainingWorkItems.contains {
            $0.count > PartialDeliveryRecordView.remainingItemCharacterLimit
        }
    }

    var isOverRecordedByLimit: Bool {
        recordedByCharacterCount > PartialDeliveryRecordView.recordedByCharacterLimit
    }

    /// True when submission will trim one or more over-limit fields. The sheet
    /// must show this instead of silently implying the full text was sent.
    var isClientBounded: Bool {
        isOverSummaryLimit || isOverRemainingItemLimit || hasOverlongRemainingItem
            || isOverRecordedByLimit
    }

    var request: PartialDeliveryRequest? {
        PartialDeliveryRequest.normalized(
            summary: summary,
            remainingWork: remainingWorkItems,
            recordedBy: recordedBy
        )
    }

    var validationIssue: BudgetValidationIssue? {
        guard request == nil else { return nil }
        return BudgetValidationIssue(
            code: "INVALID_PARTIAL_DELIVERY",
            message: "Enter a summary or at least one remaining-work item before recording."
        )
    }

    func makeRequest() throws -> PartialDeliveryRequest {
        guard let request else {
            throw validationIssue ?? BudgetValidationIssue(
                code: "INVALID_PARTIAL_DELIVERY",
                message: "Enter a summary or at least one remaining-work item before recording."
            )
        }
        return request
    }
}

private enum PartialDeliveryLimits {
    static func items(from values: [String]) -> [String] {
        values.flatMap { items(from: $0) }
    }

    /// One non-empty item per line. CRLF/CR is normalized, whitespace is
    /// trimmed, and blank lines are dropped.
    static func items(from text: String) -> [String] {
        text
            .replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { String($0).trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
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
    /// Explicit project opt-in for at most one automatic completion-reserve
    /// rescue per session. Off unless the daemon reports it on; it can never
    /// add budget, remove a cap, change routing, or bypass Verifier/Integrator.
    let autoRescue: Bool

    enum CodingKeys: String, CodingKey {
        case defaultProfile = "default_profile"
        case completionReserveRatio = "completion_reserve_ratio"
        case attentionCap = "attention_cap"
        case stopPolicy = "stop_policy"
        case autoRescue = "auto_rescue"
    }

    init(
        defaultProfile: RunProfile = .balanced,
        completionReserveRatio: Double = 0.20,
        attentionCap: UInt32 = 20,
        stopPolicy: RunStopPolicyView = .standard,
        autoRescue: Bool = false
    ) {
        self.defaultProfile = defaultProfile
        self.completionReserveRatio = completionReserveRatio
        self.attentionCap = attentionCap
        self.stopPolicy = stopPolicy
        self.autoRescue = autoRescue
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
        autoRescue = try container.decodeIfPresent(Bool.self, forKey: .autoRescue) ?? false
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

    var autoRescueLabel: String {
        autoRescue ? "On" : "Off"
    }

    /// One operator-facing caveat shared by the card and editor so the enabled
    /// behavior cannot be described more broadly than the daemon implements.
    var autoRescueDetail: String {
        guard autoRescue else {
            return "Off · no automatic completion-reserve rescue is permitted."
        }
        return """
        On · at most one automatic completion-reserve rescue per session, only for eligible \
        fixed-budget near-completion work. It never adds budget, removes caps, changes routing, \
        or bypasses Verifier/Integrator.
        """
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
    var autoRescue = false

    init() {}

    init(policy: ProjectRunPolicyView) {
        profile = policy.defaultProfile
        completionReservePercentText = Self.percentText(policy.completionReserveRatio)
        attentionCapText = String(policy.attentionCap)
        noProgressTurnsText = String(policy.stopPolicy.noProgressTurns)
        requireEvidence = policy.stopPolicy.requireEvidence
        partialDelivery = policy.stopPolicy.partialDelivery
        marginalValueRatio = policy.stopPolicy.marginalValueRatio
        autoRescue = policy.autoRescue
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
            ),
            autoRescue: autoRescue
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

// MARK: - Completion-reserve rescue

/// Daemon-issued one-shot rescue grant. The grant record alone is never
/// "active"; only `RescueStatusView.active` from the daemon may say that.
/// Display strings are bounded and unknown additive fields are ignored.
struct RescueGrantView: Codable, Sendable, Equatable, Identifiable {
    static let schema = "verlinpark.rescue_grant/v1"
    static let reasonCharacterLimit = 240
    static let recordedByCharacterLimit = 120

    let schema: String
    let id: String
    let sessionID: String
    let projectID: String
    let recordedAt: String
    let expiresAt: String
    let recordedBy: String?
    let reason: String?
    let readinessLevel: String
    let latestCommit: String?
    let noProgressTurns: UInt32

    enum CodingKeys: String, CodingKey {
        case schema, id, reason
        case sessionID = "session_id"
        case projectID = "project_id"
        case recordedAt = "recorded_at"
        case expiresAt = "expires_at"
        case recordedBy = "recorded_by"
        case readinessLevel = "readiness_level"
        case latestCommit = "latest_commit"
        case noProgressTurns = "no_progress_turns"
    }

    init(
        schema: String = RescueGrantView.schema,
        id: String,
        sessionID: String = "",
        projectID: String = "",
        recordedAt: String = "",
        expiresAt: String = "",
        recordedBy: String? = nil,
        reason: String? = nil,
        readinessLevel: String = "",
        latestCommit: String? = nil,
        noProgressTurns: UInt32 = 0
    ) {
        self.schema = schema
        self.id = id
        self.sessionID = sessionID
        self.projectID = projectID
        self.recordedAt = recordedAt
        self.expiresAt = expiresAt
        self.recordedBy = recordedBy
        self.reason = reason
        self.readinessLevel = readinessLevel
        self.latestCommit = latestCommit
        self.noProgressTurns = noProgressTurns
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schema =
            try container.decodeIfPresent(String.self, forKey: .schema)
                ?? RescueGrantView.schema
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        recordedAt = try container.decodeIfPresent(String.self, forKey: .recordedAt) ?? ""
        expiresAt = try container.decodeIfPresent(String.self, forKey: .expiresAt) ?? ""
        recordedBy = try container.decodeIfPresent(String.self, forKey: .recordedBy)
        reason = try container.decodeIfPresent(String.self, forKey: .reason)
        readinessLevel =
            try container.decodeIfPresent(String.self, forKey: .readinessLevel) ?? ""
        latestCommit = try container.decodeIfPresent(String.self, forKey: .latestCommit)
        noProgressTurns =
            try container.decodeIfPresent(UInt32.self, forKey: .noProgressTurns) ?? 0
    }

    var isRecognized: Bool {
        schema == Self.schema
    }

    var shortID: String {
        guard !id.isEmpty else { return "unknown" }
        return String(id.prefix(12))
    }

    var shortCommit: String? {
        guard let latestCommit, !latestCommit.isEmpty else { return nil }
        return String(latestCommit.prefix(12))
    }

    var recordedAtLabel: String {
        Self.bounded(recordedAt, limit: 40) ?? "time not reported"
    }

    var expiresAtLabel: String {
        Self.bounded(expiresAt, limit: 40) ?? "expiry not reported"
    }

    var recordedByLabel: String? {
        Self.bounded(recordedBy, limit: Self.recordedByCharacterLimit)
    }

    var reasonLabel: String? {
        Self.bounded(reason, limit: Self.reasonCharacterLimit)
    }

    var noProgressLabel: String {
        noProgressTurns == 1 ? "1 no-progress turn" : "\(noProgressTurns) no-progress turns"
    }

    var readinessLabel: String {
        let value = readinessLevel.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        switch value {
        case "none": return "NONE"
        case "dirty": return "DIRTY"
        case "committed": return "COMMITTED"
        case "tests_passed": return "TESTS PASSED"
        case "verified": return "VERIFIED"
        case "merged": return "MERGED"
        case "partial": return "PARTIAL"
        case "blocked": return "BLOCKED"
        case "": return "NOT REPORTED"
        default: return String(value.uppercased().prefix(24))
        }
    }

    /// Trims and client-bounds an optional operator audit reason. The daemon
    /// repeats validation; an empty value becomes null on the wire.
    static func normalizedReason(_ raw: String?) -> String? {
        guard let raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(reasonCharacterLimit))
    }

    static func normalizedRecordedBy(_ raw: String?) -> String? {
        guard let raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(recordedByCharacterLimit))
    }

    private static func bounded(_ raw: String?, limit: Int) -> String? {
        guard let raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(max(0, limit)))
    }
}

/// One daemon-reported near-completion or looping candidate. These are
/// evidence for the operator; they are not reservations or scheduler actions.
struct RescueCandidateView: Codable, Sendable, Equatable, Identifiable {
    let kind: String
    let id: String
    let status: String
    let detail: String
    let commit: String?

    enum CodingKeys: String, CodingKey {
        case kind, id, status, detail, commit
    }

    init(
        kind: String,
        id: String,
        status: String = "",
        detail: String = "",
        commit: String? = nil
    ) {
        self.kind = kind
        self.id = id
        self.status = status
        self.detail = detail
        self.commit = commit
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
        commit = try container.decodeIfPresent(String.self, forKey: .commit)
    }

    var kindLabel: String {
        Self.bounded(kind, limit: 24)?.uppercased() ?? "UNKNOWN"
    }

    var idLabel: String {
        Self.bounded(id, limit: 24) ?? "unknown"
    }

    var statusLabel: String {
        Self.bounded(status, limit: 32)?.uppercased() ?? "UNKNOWN"
    }

    var detailLabel: String? {
        Self.bounded(detail, limit: 200)
    }

    var shortCommit: String? {
        guard let commit, !commit.isEmpty else { return nil }
        return String(commit.prefix(12))
    }

    var displayLabel: String {
        "\(kindLabel) \(idLabel) · \(statusLabel)"
    }

    private static func bounded(_ raw: String?, limit: Int) -> String? {
        guard let raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(max(0, limit)))
    }
}

/// Daemon-reported reserve headroom. Missing values stay nil; they are never
/// displayed as a zero budget or an absent cap.
struct RescueReserveView: Codable, Sendable, Equatable {
    let completionReserveRatio: Double?
    let remainingTokens: UInt64?
    let remainingCostUSD: Double?
    let remainingSecs: UInt64?
    let hardTokenCap: UInt64?
    let hardCostCapUSD: Double?
    let hardWallClockSecs: UInt64?

    enum CodingKeys: String, CodingKey {
        case completionReserveRatio = "completion_reserve_ratio"
        case remainingTokens = "remaining_tokens"
        case remainingCostUSD = "remaining_cost_usd"
        case remainingSecs = "remaining_secs"
        case hardTokenCap = "hard_token_cap"
        case hardCostCapUSD = "hard_cost_cap_usd"
        case hardWallClockSecs = "hard_wall_clock_secs"
    }

    init(
        completionReserveRatio: Double? = nil,
        remainingTokens: UInt64? = nil,
        remainingCostUSD: Double? = nil,
        remainingSecs: UInt64? = nil,
        hardTokenCap: UInt64? = nil,
        hardCostCapUSD: Double? = nil,
        hardWallClockSecs: UInt64? = nil
    ) {
        self.completionReserveRatio = completionReserveRatio
        self.remainingTokens = remainingTokens
        self.remainingCostUSD = remainingCostUSD
        self.remainingSecs = remainingSecs
        self.hardTokenCap = hardTokenCap
        self.hardCostCapUSD = hardCostCapUSD
        self.hardWallClockSecs = hardWallClockSecs
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        completionReserveRatio =
            try container.decodeIfPresent(Double.self, forKey: .completionReserveRatio)
        remainingTokens =
            try container.decodeIfPresent(UInt64.self, forKey: .remainingTokens)
        remainingCostUSD =
            try container.decodeIfPresent(Double.self, forKey: .remainingCostUSD)
        remainingSecs =
            try container.decodeIfPresent(UInt64.self, forKey: .remainingSecs)
        hardTokenCap =
            try container.decodeIfPresent(UInt64.self, forKey: .hardTokenCap)
        hardCostCapUSD =
            try container.decodeIfPresent(Double.self, forKey: .hardCostCapUSD)
        hardWallClockSecs =
            try container.decodeIfPresent(UInt64.self, forKey: .hardWallClockSecs)
    }

    var completionReservePercentLabel: String {
        guard let completionReserveRatio, completionReserveRatio.isFinite else {
            return "not reported"
        }
        let percent = completionReserveRatio * 100
        if percent.rounded() == percent {
            return "\(Int(percent))%"
        }
        return String(format: "%.1f%%", percent)
    }

    var remainingLabel: String {
        BudgetFormatting.combined(
            tokens: remainingTokens,
            cost: remainingCostUSD,
            seconds: remainingSecs,
            empty: "not reported"
        )
    }

    var hardCapLabel: String {
        BudgetFormatting.combined(
            tokens: hardTokenCap,
            cost: hardCostCapUSD,
            seconds: hardWallClockSecs,
            empty: "not reported"
        )
    }
}

/// Full `session.rescue.status` result. `active` and `eligible` are daemon
/// authority only; this client never derives either from grant presence,
/// readiness, or a local session status.
struct RescueStatusView: Codable, Sendable, Equatable {
    static let grantEventKind = "session.resource.rescue"
    static let usedEventKind = "session.resource.rescue.used"
    static let candidatePreviewLimit = 4
    static let recommendationPreviewLimit = 4
    static let recommendationCharacterLimit = 200

    let sessionID: String
    let active: Bool
    let grant: RescueGrantView?
    let usedAt: String?
    let usedGrantID: String?
    let eligible: Bool
    let reason: String
    let readiness: ReadinessSnapshotView
    let noProgressTurns: UInt32
    let nearCompletion: [RescueCandidateView]
    let looping: [RescueCandidateView]
    let recommendations: [String]
    let reserve: RescueReserveView
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case active, grant, eligible, reason, readiness, looping, recommendations, reserve
        case sessionID = "session_id"
        case usedAt = "used_at"
        case usedGrantID = "used_grant_id"
        case noProgressTurns = "no_progress_turns"
        case nearCompletion = "near_completion"
        case generatedAt = "generated_at"
    }

    init(
        sessionID: String,
        active: Bool = false,
        grant: RescueGrantView? = nil,
        usedAt: String? = nil,
        usedGrantID: String? = nil,
        eligible: Bool = false,
        reason: String = "",
        readiness: ReadinessSnapshotView,
        noProgressTurns: UInt32 = 0,
        nearCompletion: [RescueCandidateView] = [],
        looping: [RescueCandidateView] = [],
        recommendations: [String] = [],
        reserve: RescueReserveView = RescueReserveView(),
        generatedAt: String = ""
    ) {
        self.sessionID = sessionID
        self.active = active
        self.grant = grant
        self.usedAt = usedAt
        self.usedGrantID = usedGrantID
        self.eligible = eligible
        self.reason = reason
        self.readiness = readiness
        self.noProgressTurns = noProgressTurns
        self.nearCompletion = nearCompletion
        self.looping = looping
        self.recommendations = recommendations
        self.reserve = reserve
        self.generatedAt = generatedAt
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        active = try container.decodeIfPresent(Bool.self, forKey: .active) ?? false
        grant = try container.decodeIfPresent(RescueGrantView.self, forKey: .grant)
        usedAt = try container.decodeIfPresent(String.self, forKey: .usedAt)
        usedGrantID = try container.decodeIfPresent(String.self, forKey: .usedGrantID)
        eligible = try container.decodeIfPresent(Bool.self, forKey: .eligible) ?? false
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        readiness = try container.decode(ReadinessSnapshotView.self, forKey: .readiness)
        noProgressTurns =
            try container.decodeIfPresent(UInt32.self, forKey: .noProgressTurns) ?? 0
        nearCompletion =
            try container.decodeIfPresent([RescueCandidateView].self, forKey: .nearCompletion)
                ?? []
        looping =
            try container.decodeIfPresent([RescueCandidateView].self, forKey: .looping) ?? []
        recommendations =
            try container.decodeIfPresent([String].self, forKey: .recommendations) ?? []
        reserve =
            try container.decodeIfPresent(RescueReserveView.self, forKey: .reserve)
                ?? RescueReserveView()
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
    }

    /// The grant is returned only while the daemon says the rescue is active.
    /// A leftover/grant record on an inactive status is never shown as active.
    var activeGrant: RescueGrantView? {
        guard active, let grant, grant.isRecognized,
              grant.sessionID.isEmpty || grant.sessionID == sessionID else { return nil }
        return grant
    }

    var statusLabel: String {
        if active {
            return "RESCUE ACTIVE"
        }
        if usedAt != nil {
            return "RESCUE USED"
        }
        if grant != nil {
            return "RESCUE INACTIVE"
        }
        if eligible {
            return "RESCUE ELIGIBLE"
        }
        return "RESCUE UNAVAILABLE"
    }

    var usedAtLabel: String? {
        guard let usedAt else { return nil }
        let trimmed = usedAt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(40))
    }

    var usedGrantShortLabel: String? {
        guard let usedGrantID else { return nil }
        let trimmed = usedGrantID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(12))
    }

    var reasonLabel: String {
        let value = reason.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        switch value {
        case "eligible", "near_completion":
            return "Eligible: clean near-completion work with a latest commit is ready to finish."
        case "active_grant":
            return "A rescue grant is already active."
        case "dirty":
            return "The worktree has uncommitted changes."
        case "blocked":
            return "Readiness is blocked; resolve blockers before rescuing."
        case "none":
            return "No near-completion task with a clean latest commit."
        case "merged":
            return "The session is already merged."
        case "looping":
            return "No-progress turns indicate a loop; rescue is reserved for clean near-completion work."
        case "auto_budget":
            return "Rescue applies to fixed budgets only; this session uses automatic budget."
        case "unlimited":
            return "Unlimited sessions have no completion reserve to release."
        case "no_headroom":
            return "No completion-reserve headroom remains."
        case "no_commit":
            return "No latest commit was reported; commit clean near-completion work first."
        case "session_active":
            return "The session is running; pause or wait until it is idle, failed, or cancelled."
        case "imported":
            return "Imported sessions are read-only."
        case "":
            return "The daemon reports this session is not eligible for a rescue reserve."
        default:
            return "Daemon reason: \(String(value.prefix(96)))"
        }
    }

    var nearCompletionPreview: [RescueCandidateView] {
        Array(nearCompletion.prefix(Self.candidatePreviewLimit))
    }

    var loopingPreview: [RescueCandidateView] {
        Array(looping.prefix(Self.candidatePreviewLimit))
    }

    var nearCompletionOverflow: Int {
        max(0, nearCompletion.count - Self.candidatePreviewLimit)
    }

    var loopingOverflow: Int {
        max(0, looping.count - Self.candidatePreviewLimit)
    }

    var recommendationPreviews: [String] {
        Array(
            recommendations
                .compactMap { value in
                    let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !trimmed.isEmpty else { return nil }
                    return String(trimmed.prefix(Self.recommendationCharacterLimit))
                }
                .prefix(Self.recommendationPreviewLimit)
        )
    }

    var recommendationOverflow: Int {
        max(
            0,
            recommendations.filter {
                !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            }.count - Self.recommendationPreviewLimit
        )
    }
}

/// Exact wire result of `session.rescue`. Both the grant and the refreshed
/// status are required; a partial response is a decode error.
struct RescueResultView: Codable, Sendable, Equatable {
    let sessionID: String
    let grant: RescueGrantView
    let status: RescueStatusView

    enum CodingKeys: String, CodingKey {
        case grant, status
        case sessionID = "session_id"
    }

    init(sessionID: String, grant: RescueGrantView, status: RescueStatusView) {
        self.sessionID = sessionID
        self.grant = grant
        self.status = status
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        grant = try container.decode(RescueGrantView.self, forKey: .grant)
        status = try container.decode(RescueStatusView.self, forKey: .status)
    }

    /// True only when every returned object names the same selected session and
    /// the grant uses the recognized schema. A mismatch applies no local state.
    func isConsistent(for targetSessionID: String) -> Bool {
        guard !targetSessionID.isEmpty,
              status.sessionID == targetSessionID,
              grant.isRecognized,
              sessionID.isEmpty || sessionID == targetSessionID,
              grant.sessionID.isEmpty || grant.sessionID == targetSessionID
        else {
            return false
        }
        return true
    }

    var operatorNotice: String {
        if status.active {
            return "The daemon reports the completion reserve released for the next turn only."
        }
        return "The daemon recorded a rescue grant but reports it inactive; no reserve is claimed."
    }
}

/// Client-side mirror of the daemon eligibility gate. This only blocks an
/// obviously unavailable action; every allowed request is still revalidated by
/// the daemon, and no local condition can make a session eligible.
enum RescueActionAvailability {
    static let eligibleSessionStatuses: Set<String> = [
        "paused", "idle", "failed", "cancelled",
    ]

    static func blockReason(
        sessionID: String?,
        sessionStatus: String?,
        daemonConnected: Bool,
        busy: Bool,
        rescue: RescueStatusView?,
        error: String?
    ) -> String? {
        guard let sessionID, !sessionID.isEmpty else {
            return "Select a session before releasing a completion reserve."
        }
        guard daemonConnected else {
            return "The daemon is not connected; reconnect before releasing the reserve."
        }
        if busy {
            return "Rescue status is loading or a request is in progress."
        }
        guard let sessionStatus, !sessionStatus.isEmpty else {
            return "Session status is unavailable; refresh the session list."
        }
        let status = sessionStatus.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard eligibleSessionStatuses.contains(status) else {
            return "Rescue requires a paused, idle, failed, or cancelled session."
        }
        if let error {
            let trimmed = error.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                return "Rescue status unavailable — \(String(trimmed.prefix(200)))"
            }
        }
        guard let rescue else {
            return "Rescue eligibility is unavailable; refresh resources to retry."
        }
        guard rescue.sessionID.isEmpty || rescue.sessionID == sessionID else {
            return "The rescue status belongs to a different session; refresh resources to retry."
        }
        if rescue.active {
            return "A rescue grant is already active."
        }
        guard rescue.eligible else {
            return rescue.reasonLabel
        }
        return nil
    }
}

// MARK: - R3 allocation preview

/// Bounded, read-only operator view of `session.allocation`. Every value is
/// daemon-reported: the client never computes feasibility, p50/p90, reserve,
/// headroom, or recommendations locally. Missing additive fields stay nil so
/// the UI can say "not reported" instead of inventing a zero or `false`.
struct AllocationPreviewView: Codable, Sendable, Equatable {
    static let taskPreviewLimit = 6
    static let recommendationPreviewLimit = 6
    static let criticalPathPreviewLimit = 8

    let sessionID: String
    let generatedAt: String
    let profile: String
    let readiness: ReadinessSnapshotView?
    let envelope: AllocationEnvelopeView?
    let feasibility: AllocationFeasibilityView?
    let tasks: [AllocationTaskEnvelopeView]
    let recommendations: [AllocationRecommendationView]
    let metareasoning: AllocationMetareasoningView?
    let calibration: AllocationCalibrationView?
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case profile, readiness, envelope, feasibility, tasks, recommendations
        case metareasoning, calibration, truncated
        case sessionID = "session_id"
        case generatedAt = "generated_at"
    }

    init(
        sessionID: String = "",
        generatedAt: String = "",
        profile: String = "",
        readiness: ReadinessSnapshotView? = nil,
        envelope: AllocationEnvelopeView? = nil,
        feasibility: AllocationFeasibilityView? = nil,
        tasks: [AllocationTaskEnvelopeView] = [],
        recommendations: [AllocationRecommendationView] = [],
        metareasoning: AllocationMetareasoningView? = nil,
        calibration: AllocationCalibrationView? = nil,
        truncated: Bool = false
    ) {
        self.sessionID = sessionID
        self.generatedAt = generatedAt
        self.profile = profile
        self.readiness = readiness
        self.envelope = envelope
        self.feasibility = feasibility
        self.tasks = tasks
        self.recommendations = recommendations
        self.metareasoning = metareasoning
        self.calibration = calibration
        self.truncated = truncated
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        profile = try container.decodeIfPresent(String.self, forKey: .profile) ?? ""
        readiness = try container.decodeIfPresent(ReadinessSnapshotView.self, forKey: .readiness)
        envelope = try container.decodeIfPresent(AllocationEnvelopeView.self, forKey: .envelope)
        feasibility =
            try container.decodeIfPresent(AllocationFeasibilityView.self, forKey: .feasibility)
        tasks = GovernorDecoding.elements(
            AllocationTaskEnvelopeView.self,
            from: container,
            forKey: .tasks
        )
        recommendations = GovernorDecoding.recommendations(
            from: container,
            forKey: .recommendations
        )
        metareasoning =
            try container.decodeIfPresent(AllocationMetareasoningView.self, forKey: .metareasoning)
        calibration =
            try container.decodeIfPresent(AllocationCalibrationView.self, forKey: .calibration)
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }

    var profileTitle: String {
        GovernorDecoding.profileTitle(profile)
    }

    var taskPreviews: [AllocationTaskEnvelopeView] {
        Array(tasks.prefix(Self.taskPreviewLimit))
    }

    var taskOverflow: Int {
        max(0, tasks.count - Self.taskPreviewLimit)
    }

    var recommendationPreviews: [AllocationRecommendationView] {
        Array(recommendations.prefix(Self.recommendationPreviewLimit))
    }

    var recommendationOverflow: Int {
        max(0, recommendations.count - Self.recommendationPreviewLimit)
    }

    var readinessLabel: String {
        readiness?.levelLabel ?? "NOT REPORTED"
    }

    /// Strict selection guard: a preview for another session is never applied.
    func isForSession(_ targetSessionID: String) -> Bool {
        let target = targetSessionID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !target.isEmpty, !sessionID.isEmpty else { return false }
        return sessionID == target
    }
}

/// Daemon-reported budget envelope. Optional limits stay nil; they are never
/// displayed as a zero budget or a missing cap.
struct AllocationEnvelopeView: Codable, Sendable, Equatable {
    let tokenBudget: UInt64?
    let spentTokens: UInt64?
    let remainingTokens: UInt64?
    let costBudgetUSD: Double?
    let spentCostUSD: Double?
    let remainingCostUSD: Double?
    let timeBudgetSecs: UInt64?
    let remainingSecs: UInt64?
    let completionReserveRatio: Double?

    enum CodingKeys: String, CodingKey {
        case tokenBudget = "token_budget"
        case spentTokens = "spent_tokens"
        case remainingTokens = "remaining_tokens"
        case costBudgetUSD = "cost_budget_usd"
        case spentCostUSD = "spent_cost_usd"
        case remainingCostUSD = "remaining_cost_usd"
        case timeBudgetSecs = "time_budget_secs"
        case remainingSecs = "remaining_secs"
        case completionReserveRatio = "completion_reserve_ratio"
    }

    init(
        tokenBudget: UInt64? = nil,
        spentTokens: UInt64? = nil,
        remainingTokens: UInt64? = nil,
        costBudgetUSD: Double? = nil,
        spentCostUSD: Double? = nil,
        remainingCostUSD: Double? = nil,
        timeBudgetSecs: UInt64? = nil,
        remainingSecs: UInt64? = nil,
        completionReserveRatio: Double? = nil
    ) {
        self.tokenBudget = tokenBudget
        self.spentTokens = spentTokens
        self.remainingTokens = remainingTokens
        self.costBudgetUSD = costBudgetUSD
        self.spentCostUSD = spentCostUSD
        self.remainingCostUSD = remainingCostUSD
        self.timeBudgetSecs = timeBudgetSecs
        self.remainingSecs = remainingSecs
        self.completionReserveRatio = completionReserveRatio
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        tokenBudget = try container.decodeIfPresent(UInt64.self, forKey: .tokenBudget)
        spentTokens = try container.decodeIfPresent(UInt64.self, forKey: .spentTokens)
        remainingTokens = try container.decodeIfPresent(UInt64.self, forKey: .remainingTokens)
        costBudgetUSD = try container.decodeIfPresent(Double.self, forKey: .costBudgetUSD)
        spentCostUSD = try container.decodeIfPresent(Double.self, forKey: .spentCostUSD)
        remainingCostUSD = try container.decodeIfPresent(Double.self, forKey: .remainingCostUSD)
        timeBudgetSecs = try container.decodeIfPresent(UInt64.self, forKey: .timeBudgetSecs)
        remainingSecs = try container.decodeIfPresent(UInt64.self, forKey: .remainingSecs)
        completionReserveRatio =
            try container.decodeIfPresent(Double.self, forKey: .completionReserveRatio)
    }

    var budgetLabel: String {
        BudgetFormatting.combined(
            tokens: tokenBudget,
            cost: costBudgetUSD,
            seconds: timeBudgetSecs,
            empty: "no budget reported"
        )
    }

    var spentLabel: String {
        BudgetFormatting.combined(
            tokens: spentTokens,
            cost: spentCostUSD,
            seconds: nil,
            empty: "no spend reported"
        )
    }

    var remainingLabel: String {
        BudgetFormatting.combined(
            tokens: remainingTokens,
            cost: remainingCostUSD,
            seconds: remainingSecs,
            empty: "no remaining budget reported"
        )
    }

    var reservePercentLabel: String {
        guard let completionReserveRatio, completionReserveRatio.isFinite else {
            return "reserve not reported"
        }
        let percent = completionReserveRatio * 100
        if percent.rounded() == percent {
            return "\(Int(percent))%"
        }
        return String(format: "%.1f%%", percent)
    }
}

/// Daemon-computed feasibility and forecast. `feasible` is optional so a
/// missing additive field is not shown as a local "not feasible" claim.
struct AllocationFeasibilityView: Codable, Sendable, Equatable {
    let feasible: Bool?
    let reason: String
    let p50Tokens: UInt64?
    let p90Tokens: UInt64?
    let p50CostUSD: Double?
    let p90CostUSD: Double?
    let sampleCalls: UInt32?
    let source: String
    let reserveTokens: UInt64?
    let headroomTokens: UInt64?
    let criticalPathTasks: [String]
    let estimatedTasks: UInt32?
    let totalTasks: UInt32?
    let pressureDimensions: [String]
    let criticalPathAvailable: Bool?
    let criticalPathReason: String?

    enum CodingKeys: String, CodingKey {
        case feasible, reason, source
        case p50Tokens = "p50_tokens"
        case p90Tokens = "p90_tokens"
        case p50CostUSD = "p50_cost_usd"
        case p90CostUSD = "p90_cost_usd"
        case sampleCalls = "sample_calls"
        case reserveTokens = "reserve_tokens"
        case headroomTokens = "headroom_tokens"
        case criticalPathTasks = "critical_path_tasks"
        case estimatedTasks = "estimated_tasks"
        case totalTasks = "total_tasks"
        case pressureDimensions = "pressure_dimensions"
        case criticalPathAvailable = "critical_path_available"
        case criticalPathReason = "critical_path_reason"
    }

    init(
        feasible: Bool? = nil,
        reason: String = "",
        p50Tokens: UInt64? = nil,
        p90Tokens: UInt64? = nil,
        p50CostUSD: Double? = nil,
        p90CostUSD: Double? = nil,
        sampleCalls: UInt32? = nil,
        source: String = "",
        reserveTokens: UInt64? = nil,
        headroomTokens: UInt64? = nil,
        criticalPathTasks: [String] = [],
        estimatedTasks: UInt32? = nil,
        totalTasks: UInt32? = nil,
        pressureDimensions: [String] = [],
        criticalPathAvailable: Bool? = nil,
        criticalPathReason: String? = nil
    ) {
        self.feasible = feasible
        self.reason = reason
        self.p50Tokens = p50Tokens
        self.p90Tokens = p90Tokens
        self.p50CostUSD = p50CostUSD
        self.p90CostUSD = p90CostUSD
        self.sampleCalls = sampleCalls
        self.source = source
        self.reserveTokens = reserveTokens
        self.headroomTokens = headroomTokens
        self.criticalPathTasks = criticalPathTasks
        self.estimatedTasks = estimatedTasks
        self.totalTasks = totalTasks
        self.pressureDimensions = pressureDimensions
        self.criticalPathAvailable = criticalPathAvailable
        self.criticalPathReason = criticalPathReason
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        feasible = try container.decodeIfPresent(Bool.self, forKey: .feasible)
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        p50Tokens = try container.decodeIfPresent(UInt64.self, forKey: .p50Tokens)
        p90Tokens = try container.decodeIfPresent(UInt64.self, forKey: .p90Tokens)
        p50CostUSD = try container.decodeIfPresent(Double.self, forKey: .p50CostUSD)
        p90CostUSD = try container.decodeIfPresent(Double.self, forKey: .p90CostUSD)
        sampleCalls = try container.decodeIfPresent(UInt32.self, forKey: .sampleCalls)
        source = try container.decodeIfPresent(String.self, forKey: .source) ?? ""
        reserveTokens = try container.decodeIfPresent(UInt64.self, forKey: .reserveTokens)
        headroomTokens = try container.decodeIfPresent(UInt64.self, forKey: .headroomTokens)
        criticalPathTasks = GovernorDecoding.stringList(from: container, forKey: .criticalPathTasks)
        estimatedTasks = try container.decodeIfPresent(UInt32.self, forKey: .estimatedTasks)
        totalTasks = try container.decodeIfPresent(UInt32.self, forKey: .totalTasks)
        pressureDimensions = GovernorDecoding.stringList(
            from: container,
            forKey: .pressureDimensions
        )
        criticalPathAvailable =
            try container.decodeIfPresent(Bool.self, forKey: .criticalPathAvailable)
        criticalPathReason =
            try container.decodeIfPresent(String.self, forKey: .criticalPathReason)
    }

    var estimateCoverageLabel: String? {
        guard let estimatedTasks, let totalTasks else { return nil }
        return "\(estimatedTasks)/\(totalTasks) task(s) estimated"
    }

    var pressureLabel: String? {
        let values = pressureDimensions
            .compactMap { GovernorDecoding.humanizedToken($0, limit: 24) }
            .prefix(6)
            .map(\.self)
        return values.isEmpty ? nil : values.joined(separator: ", ")
    }

    var criticalPathWarning: String? {
        guard criticalPathAvailable == false,
              let reason = GovernorDecoding.bounded(criticalPathReason, limit: 80),
              reason != "no_plan"
        else { return nil }
        return "critical path unavailable (\(reason)); feasibility falls back to all remaining tasks"
    }

    var statusLabel: String {
        guard let feasible else { return "FEASIBILITY NOT REPORTED" }
        return feasible ? "FEASIBLE" : "NOT FEASIBLE"
    }

    var reasonLabel: String {
        GovernorDecoding.bounded(reason, limit: 240) ?? "No reason reported"
    }

    var forecastLabel: String {
        var parts: [String] = []
        if let p50 = Self.forecast(tokens: p50Tokens, cost: p50CostUSD) {
            parts.append("p50 \(p50)")
        }
        if let p90 = Self.forecast(tokens: p90Tokens, cost: p90CostUSD) {
            parts.append("p90 \(p90)")
        }
        if let sampleCalls {
            parts.append("\(sampleCalls) samples")
        }
        if let source = GovernorDecoding.bounded(source, limit: 40) {
            parts.append(source)
        }
        return parts.isEmpty ? "No forecast reported" : parts.joined(separator: " · ")
    }

    var reserveHeadroomLabel: String {
        var parts: [String] = []
        if let reserveTokens {
            parts.append("reserve \(BudgetFormatting.tokens(reserveTokens))")
        }
        if let headroomTokens {
            parts.append("headroom \(BudgetFormatting.tokens(headroomTokens))")
        }
        return parts.isEmpty ? "Reserve/headroom not reported" : parts.joined(separator: " · ")
    }

    var criticalPathPreviews: [String] {
        criticalPathTasks
            .compactMap { GovernorDecoding.bounded($0, limit: 32) }
            .prefix(AllocationPreviewView.criticalPathPreviewLimit)
            .map(\.self)
    }

    var criticalPathOverflow: Int {
        max(0, criticalPathTasks.count - AllocationPreviewView.criticalPathPreviewLimit)
    }

    static func forecast(tokens: UInt64?, cost: Double?) -> String? {
        let value = BudgetFormatting.combined(
            tokens: tokens,
            cost: cost,
            seconds: nil,
            empty: ""
        )
        return value.isEmpty ? nil : value
    }
}

/// One daemon-reported task envelope. Optional booleans keep an absent field
/// from being presented as a definitive ready/feasible claim.
struct AllocationTaskEnvelopeView: Codable, Sendable, Equatable {
    let taskID: String
    let title: String
    let department: String
    let status: String
    let priority: Int?
    let tokenBudget: UInt64?
    let costBudgetUSD: Double?
    let p50Tokens: UInt64?
    let p90Tokens: UInt64?
    let observedCalls: UInt32?
    let source: String
    let criticalPath: Bool?
    let ready: Bool?
    let feasible: Bool?
    let reason: String

    enum CodingKeys: String, CodingKey {
        case title, department, status, priority, source, criticalPath, ready, feasible, reason
        case taskID = "task_id"
        case tokenBudget = "token_budget"
        case costBudgetUSD = "cost_budget_usd"
        case p50Tokens = "p50_tokens"
        case p90Tokens = "p90_tokens"
        case observedCalls = "observed_calls"
    }

    init(
        taskID: String = "",
        title: String = "",
        department: String = "",
        status: String = "",
        priority: Int? = nil,
        tokenBudget: UInt64? = nil,
        costBudgetUSD: Double? = nil,
        p50Tokens: UInt64? = nil,
        p90Tokens: UInt64? = nil,
        observedCalls: UInt32? = nil,
        source: String = "",
        criticalPath: Bool? = nil,
        ready: Bool? = nil,
        feasible: Bool? = nil,
        reason: String = ""
    ) {
        self.taskID = taskID
        self.title = title
        self.department = department
        self.status = status
        self.priority = priority
        self.tokenBudget = tokenBudget
        self.costBudgetUSD = costBudgetUSD
        self.p50Tokens = p50Tokens
        self.p90Tokens = p90Tokens
        self.observedCalls = observedCalls
        self.source = source
        self.criticalPath = criticalPath
        self.ready = ready
        self.feasible = feasible
        self.reason = reason
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID) ?? ""
        title = try container.decodeIfPresent(String.self, forKey: .title) ?? ""
        department = try container.decodeIfPresent(String.self, forKey: .department) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        priority = try container.decodeIfPresent(Int.self, forKey: .priority)
        tokenBudget = try container.decodeIfPresent(UInt64.self, forKey: .tokenBudget)
        costBudgetUSD = try container.decodeIfPresent(Double.self, forKey: .costBudgetUSD)
        p50Tokens = try container.decodeIfPresent(UInt64.self, forKey: .p50Tokens)
        p90Tokens = try container.decodeIfPresent(UInt64.self, forKey: .p90Tokens)
        observedCalls = try container.decodeIfPresent(UInt32.self, forKey: .observedCalls)
        source = try container.decodeIfPresent(String.self, forKey: .source) ?? ""
        criticalPath = try container.decodeIfPresent(Bool.self, forKey: .criticalPath)
        ready = try container.decodeIfPresent(Bool.self, forKey: .ready)
        feasible = try container.decodeIfPresent(Bool.self, forKey: .feasible)
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
    }

    var titleLabel: String {
        GovernorDecoding.bounded(title, limit: 120)
            ?? GovernorDecoding.bounded(taskID, limit: 32)
            ?? "Untitled task"
    }

    var taskIDLabel: String {
        GovernorDecoding.bounded(taskID, limit: 32) ?? "no task id"
    }

    var departmentLabel: String {
        GovernorDecoding.bounded(department, limit: 64) ?? "department not reported"
    }

    var statusLabel: String {
        GovernorDecoding.bounded(status, limit: 32)?.uppercased() ?? "STATUS NOT REPORTED"
    }

    var budgetLabel: String {
        BudgetFormatting.combined(
            tokens: tokenBudget,
            cost: costBudgetUSD,
            seconds: nil,
            empty: "no task envelope reported"
        )
    }

    var forecastLabel: String {
        var parts: [String] = []
        if let p50 = AllocationFeasibilityView.forecast(tokens: p50Tokens, cost: nil) {
            parts.append("p50 \(p50)")
        }
        if let p90 = AllocationFeasibilityView.forecast(tokens: p90Tokens, cost: nil) {
            parts.append("p90 \(p90)")
        }
        if let observedCalls {
            parts.append("\(observedCalls) observed")
        }
        return parts.isEmpty ? "No task forecast reported" : parts.joined(separator: " · ")
    }

    var feasibleLabel: String? {
        guard let feasible else { return nil }
        return feasible ? "FEASIBLE" : "NOT FEASIBLE"
    }

    var readyLabel: String? {
        guard let ready else { return nil }
        return ready ? "READY" : "NOT READY"
    }

    var reasonLabel: String? {
        GovernorDecoding.bounded(reason, limit: 200)
    }

    var sourceLabel: String? {
        GovernorDecoding.bounded(source, limit: 40)
    }
}

/// One advisory recommendation. `confidence` is daemon-provided; an unknown
/// value is labeled truthfully and never upgraded to a medium/high default.
struct AllocationRecommendationView: Codable, Sendable, Equatable {
    let action: String
    let reason: String
    let confidence: String
    let taskID: String?

    enum CodingKeys: String, CodingKey {
        case action, reason, confidence
        case taskID = "task_id"
    }

    init(
        action: String = "",
        reason: String = "",
        confidence: String = "",
        taskID: String? = nil
    ) {
        self.action = action
        self.reason = reason
        self.confidence = confidence
        self.taskID = taskID
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        action = try container.decodeIfPresent(String.self, forKey: .action) ?? ""
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        confidence = try container.decodeIfPresent(String.self, forKey: .confidence) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
    }

    var actionLabel: String {
        GovernorDecoding.humanizedToken(action, limit: 120) ?? "No action reported"
    }

    var reasonLabel: String {
        GovernorDecoding.bounded(reason, limit: 240) ?? "No reason reported"
    }

    var confidenceLabel: String {
        GovernorDecoding.confidenceLabel(confidence)
    }

    var taskIDLabel: String? {
        GovernorDecoding.bounded(taskID, limit: 32)
    }
}

/// Bounded diagnosis and metareasoning summary. Task references are decoded
/// from either plain strings or objects carrying an id/title.
struct AllocationMetareasoningView: Codable, Sendable, Equatable {
    static let listPreviewLimit = 6
    static let recommendationPreviewLimit = 4

    let diagnosisStatus: String
    let diagnosisReason: String?
    let diagnosisTarget: String?
    let noProgressTurns: UInt32?
    let stalledTasks: [String]
    let failedTasks: [String]
    let retryingTasks: [String]
    let recommendations: [AllocationRecommendationView]

    enum CodingKeys: String, CodingKey {
        case recommendations
        case diagnosisStatus = "diagnosis_status"
        case diagnosisReason = "diagnosis_reason"
        case diagnosisTarget = "diagnosis_target"
        case noProgressTurns = "no_progress_turns"
        case stalledTasks = "stalled_tasks"
        case failedTasks = "failed_tasks"
        case retryingTasks = "retrying_tasks"
    }

    init(
        diagnosisStatus: String = "",
        diagnosisReason: String? = nil,
        diagnosisTarget: String? = nil,
        noProgressTurns: UInt32? = nil,
        stalledTasks: [String] = [],
        failedTasks: [String] = [],
        retryingTasks: [String] = [],
        recommendations: [AllocationRecommendationView] = []
    ) {
        self.diagnosisStatus = diagnosisStatus
        self.diagnosisReason = diagnosisReason
        self.diagnosisTarget = diagnosisTarget
        self.noProgressTurns = noProgressTurns
        self.stalledTasks = stalledTasks
        self.failedTasks = failedTasks
        self.retryingTasks = retryingTasks
        self.recommendations = recommendations
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        diagnosisStatus =
            try container.decodeIfPresent(String.self, forKey: .diagnosisStatus) ?? ""
        diagnosisReason = try container.decodeIfPresent(String.self, forKey: .diagnosisReason)
        diagnosisTarget = try container.decodeIfPresent(String.self, forKey: .diagnosisTarget)
        noProgressTurns = try container.decodeIfPresent(UInt32.self, forKey: .noProgressTurns)
        stalledTasks = GovernorDecoding.stringList(from: container, forKey: .stalledTasks)
        failedTasks = GovernorDecoding.stringList(from: container, forKey: .failedTasks)
        retryingTasks = GovernorDecoding.stringList(from: container, forKey: .retryingTasks)
        recommendations = GovernorDecoding.recommendations(
            from: container,
            forKey: .recommendations
        )
    }

    var diagnosisStatusLabel: String {
        GovernorDecoding.humanizedToken(diagnosisStatus, limit: 40)?.uppercased()
            ?? "DIAGNOSIS NOT REPORTED"
    }

    var diagnosisReasonLabel: String? {
        GovernorDecoding.bounded(diagnosisReason, limit: 240)
    }

    var diagnosisTargetLabel: String? {
        GovernorDecoding.bounded(diagnosisTarget, limit: 32)
    }

    var noProgressLabel: String? {
        guard let noProgressTurns else { return nil }
        return noProgressTurns == 1
            ? "1 no-progress turn"
            : "\(noProgressTurns) no-progress turns"
    }

    var stalledPreviews: [String] {
        Self.previews(stalledTasks)
    }

    var failedPreviews: [String] {
        Self.previews(failedTasks)
    }

    var retryingPreviews: [String] {
        Self.previews(retryingTasks)
    }

    var stalledOverflow: Int {
        max(0, stalledTasks.count - Self.listPreviewLimit)
    }

    var failedOverflow: Int {
        max(0, failedTasks.count - Self.listPreviewLimit)
    }

    var retryingOverflow: Int {
        max(0, retryingTasks.count - Self.listPreviewLimit)
    }

    var recommendationPreviews: [AllocationRecommendationView] {
        Array(recommendations.prefix(Self.recommendationPreviewLimit))
    }

    var recommendationOverflow: Int {
        max(0, recommendations.count - Self.recommendationPreviewLimit)
    }

    var isEmpty: Bool {
        !GovernorDecoding.hasText(diagnosisStatus)
            && !GovernorDecoding.hasText(diagnosisReason)
            && !GovernorDecoding.hasText(diagnosisTarget)
            && noProgressTurns == nil
            && stalledTasks.isEmpty && failedTasks.isEmpty && retryingTasks.isEmpty
            && recommendations.isEmpty
    }

    private static func previews(_ values: [String]) -> [String] {
        values
            .compactMap { GovernorDecoding.bounded($0, limit: 48) }
            .prefix(listPreviewLimit)
            .map(\.self)
    }
}

/// Attention/trust calibration. All recommendations and counts are
/// daemon-provided; the card presents them as advisory.
struct AllocationCalibrationView: Codable, Sendable, Equatable {
    let attentionRecommendation: String
    let trustRecommendation: String
    let reason: String
    let confidence: String
    let pendingApprovals: UInt32?
    let attentionInterrupts: UInt32?
    let attentionCap: UInt32?
    let verifierRejections: UInt32?
    let failedTasks: UInt32?

    enum CodingKeys: String, CodingKey {
        case reason, confidence
        case attentionRecommendation = "attention_recommendation"
        case trustRecommendation = "trust_recommendation"
        case pendingApprovals = "pending_approvals"
        case attentionInterrupts = "attention_interrupts"
        case attentionCap = "attention_cap"
        case verifierRejections = "verifier_rejections"
        case failedTasks = "failed_tasks"
    }

    init(
        attentionRecommendation: String = "",
        trustRecommendation: String = "",
        reason: String = "",
        confidence: String = "",
        pendingApprovals: UInt32? = nil,
        attentionInterrupts: UInt32? = nil,
        attentionCap: UInt32? = nil,
        verifierRejections: UInt32? = nil,
        failedTasks: UInt32? = nil
    ) {
        self.attentionRecommendation = attentionRecommendation
        self.trustRecommendation = trustRecommendation
        self.reason = reason
        self.confidence = confidence
        self.pendingApprovals = pendingApprovals
        self.attentionInterrupts = attentionInterrupts
        self.attentionCap = attentionCap
        self.verifierRejections = verifierRejections
        self.failedTasks = failedTasks
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        attentionRecommendation =
            try container.decodeIfPresent(String.self, forKey: .attentionRecommendation) ?? ""
        trustRecommendation =
            try container.decodeIfPresent(String.self, forKey: .trustRecommendation) ?? ""
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        confidence = try container.decodeIfPresent(String.self, forKey: .confidence) ?? ""
        pendingApprovals =
            try container.decodeIfPresent(UInt32.self, forKey: .pendingApprovals)
        attentionInterrupts =
            try container.decodeIfPresent(UInt32.self, forKey: .attentionInterrupts)
        attentionCap = try container.decodeIfPresent(UInt32.self, forKey: .attentionCap)
        verifierRejections =
            try container.decodeIfPresent(UInt32.self, forKey: .verifierRejections)
        failedTasks = try container.decodeIfPresent(UInt32.self, forKey: .failedTasks)
    }

    var attentionLabel: String {
        GovernorDecoding.humanizedToken(attentionRecommendation, limit: 200)
            ?? "Attention recommendation not reported"
    }

    var trustLabel: String {
        GovernorDecoding.humanizedToken(trustRecommendation, limit: 200)
            ?? "Trust recommendation not reported"
    }

    var reasonLabel: String? {
        GovernorDecoding.bounded(reason, limit: 240)
    }

    var confidenceLabel: String {
        GovernorDecoding.confidenceLabel(confidence)
    }

    var countLabel: String? {
        var parts: [String] = []
        if let attentionInterrupts {
            if let attentionCap {
                parts.append("\(attentionInterrupts)/\(attentionCap) interrupts")
            } else {
                parts.append("\(attentionInterrupts) interrupts")
            }
        } else if let attentionCap {
            parts.append("cap \(attentionCap) interrupts")
        }
        if let pendingApprovals {
            parts.append(
                "\(pendingApprovals) pending "
                    + (pendingApprovals == 1 ? "approval" : "approvals")
            )
        }
        if let verifierRejections {
            parts.append(
                "\(verifierRejections) verifier "
                    + (verifierRejections == 1 ? "rejection" : "rejections")
            )
        }
        if let failedTasks {
            parts.append(
                "\(failedTasks) failed " + (failedTasks == 1 ? "task" : "tasks")
            )
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    var isEmpty: Bool {
        !GovernorDecoding.hasText(attentionRecommendation)
            && !GovernorDecoding.hasText(trustRecommendation)
            && !GovernorDecoding.hasText(reason)
            && !GovernorDecoding.hasText(confidence)
            && pendingApprovals == nil && attentionInterrupts == nil
            && attentionCap == nil && verifierRejections == nil && failedTasks == nil
    }
}

// MARK: - R4 trade-offs and quality

/// Bounded, read-only view of `session.tradeoffs`. It carries the daemon's
/// profile frontier, quality review, and calibrated confidence. Nothing here
/// selects a profile, changes routing, or claims completion.
struct TradeoffView: Codable, Sendable, Equatable {
    static let profilePreviewLimit = 8
    static let recommendationPreviewLimit = 6
    static let warningPreviewLimit = 6

    let sessionID: String
    let generatedAt: String
    let currentProfile: String
    /// Active contract reserve. Profile rows are documented posture only.
    let effectiveCompletionReserveRatio: Double?
    let reserveSource: String
    let profiles: [ProfileTradeoffView]
    let quality: QualityReviewView?
    let confidence: TradeoffConfidenceView?
    let recommendations: [String]
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case profiles, quality, confidence, recommendations, warnings
        case sessionID = "session_id"
        case generatedAt = "generated_at"
        case currentProfile = "current_profile"
        case effectiveCompletionReserveRatio = "effective_completion_reserve_ratio"
        case reserveSource = "reserve_source"
    }

    init(
        sessionID: String = "",
        generatedAt: String = "",
        currentProfile: String = "",
        effectiveCompletionReserveRatio: Double? = nil,
        reserveSource: String = "",
        profiles: [ProfileTradeoffView] = [],
        quality: QualityReviewView? = nil,
        confidence: TradeoffConfidenceView? = nil,
        recommendations: [String] = [],
        warnings: [String] = []
    ) {
        self.sessionID = sessionID
        self.generatedAt = generatedAt
        self.currentProfile = currentProfile
        self.effectiveCompletionReserveRatio = effectiveCompletionReserveRatio
        self.reserveSource = reserveSource
        self.profiles = profiles
        self.quality = quality
        self.confidence = confidence
        self.recommendations = recommendations
        self.warnings = warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        currentProfile = try container.decodeIfPresent(String.self, forKey: .currentProfile) ?? ""
        effectiveCompletionReserveRatio =
            try container.decodeIfPresent(Double.self, forKey: .effectiveCompletionReserveRatio)
        reserveSource = try container.decodeIfPresent(String.self, forKey: .reserveSource) ?? ""
        profiles = GovernorDecoding.elements(
            ProfileTradeoffView.self,
            from: container,
            forKey: .profiles
        )
        quality = try container.decodeIfPresent(QualityReviewView.self, forKey: .quality)
        confidence =
            try container.decodeIfPresent(TradeoffConfidenceView.self, forKey: .confidence)
        recommendations = GovernorDecoding.displayStringList(
            from: container,
            forKey: .recommendations
        )
        warnings = GovernorDecoding.displayStringList(from: container, forKey: .warnings)
    }

    var currentProfileTitle: String {
        GovernorDecoding.profileTitle(currentProfile)
    }

    /// Active contract reserve; profile rows are documented posture only.
    var effectiveReserveLabel: String {
        guard let ratio = effectiveCompletionReserveRatio, ratio.isFinite else {
            return "not reported"
        }
        let percent = ratio * 100
        if percent.rounded() == percent {
            return "\(Int(percent))%"
        }
        return String(format: "%.1f%%", percent)
    }

    var reserveSourceLabel: String {
        GovernorDecoding.humanizedToken(reserveSource, limit: 40) ?? "not reported"
    }

    var activeReserveSummary: String {
        "active reserve \(effectiveReserveLabel) · \(reserveSourceLabel) · profile rows are documented posture"
    }

    /// Presentation order only: known profiles follow `RunProfile.allCases`,
    /// unknown values stay after them. No profile is added or removed.
    var orderedProfiles: [ProfileTradeoffView] {
        profiles.enumerated()
            .sorted { lhs, rhs in
                let left = lhs.element.profileRank
                let right = rhs.element.profileRank
                if left != right {
                    return left < right
                }
                return lhs.offset < rhs.offset
            }
            .map(\.element)
    }

    var profilePreviews: [ProfileTradeoffView] {
        Array(orderedProfiles.prefix(Self.profilePreviewLimit))
    }

    var profileOverflow: Int {
        max(0, profiles.count - Self.profilePreviewLimit)
    }

    var recommendationPreviews: [String] {
        Self.boundedStrings(
            recommendations,
            limit: Self.recommendationPreviewLimit,
            characters: 240
        )
    }

    var recommendationOverflow: Int {
        max(0, recommendations.count - Self.recommendationPreviewLimit)
    }

    var warningPreviews: [String] {
        Self.boundedStrings(
            warnings,
            limit: Self.warningPreviewLimit,
            characters: 240
        )
    }

    var warningOverflow: Int {
        max(0, warnings.count - Self.warningPreviewLimit)
    }

    /// Strict selection guard: a view for another session is never applied.
    func isForSession(_ targetSessionID: String) -> Bool {
        let target = targetSessionID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !target.isEmpty, !sessionID.isEmpty else { return false }
        return sessionID == target
    }

    private static func boundedStrings(
        _ values: [String],
        limit: Int,
        characters: Int
    ) -> [String] {
        values
            .compactMap { GovernorDecoding.humanizedToken($0, limit: characters) }
            .prefix(limit)
            .map(\.self)
    }
}

/// One profile on the trade-off frontier.
struct ProfileTradeoffView: Codable, Sendable, Equatable {
    let profile: String
    let primaryGoal: String
    let completionReserveRatio: Double?
    let attentionPosture: String
    let qualityPosture: String
    let projectedTurns: UInt32?
    let projectedCostUSD: Double?
    let recommended: Bool?

    enum CodingKeys: String, CodingKey {
        case profile, recommended
        case primaryGoal = "primary_goal"
        case completionReserveRatio = "completion_reserve_ratio"
        case attentionPosture = "attention_posture"
        case qualityPosture = "quality_posture"
        case projectedTurns = "projected_turns"
        case projectedCostUSD = "projected_cost_usd"
    }

    init(
        profile: String = "",
        primaryGoal: String = "",
        completionReserveRatio: Double? = nil,
        attentionPosture: String = "",
        qualityPosture: String = "",
        projectedTurns: UInt32? = nil,
        projectedCostUSD: Double? = nil,
        recommended: Bool? = nil
    ) {
        self.profile = profile
        self.primaryGoal = primaryGoal
        self.completionReserveRatio = completionReserveRatio
        self.attentionPosture = attentionPosture
        self.qualityPosture = qualityPosture
        self.projectedTurns = projectedTurns
        self.projectedCostUSD = projectedCostUSD
        self.recommended = recommended
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        profile = try container.decodeIfPresent(String.self, forKey: .profile) ?? ""
        primaryGoal = try container.decodeIfPresent(String.self, forKey: .primaryGoal) ?? ""
        completionReserveRatio =
            try container.decodeIfPresent(Double.self, forKey: .completionReserveRatio)
        attentionPosture =
            try container.decodeIfPresent(String.self, forKey: .attentionPosture) ?? ""
        qualityPosture = try container.decodeIfPresent(String.self, forKey: .qualityPosture) ?? ""
        projectedTurns = try container.decodeIfPresent(UInt32.self, forKey: .projectedTurns)
        projectedCostUSD =
            try container.decodeIfPresent(Double.self, forKey: .projectedCostUSD)
        recommended = try container.decodeIfPresent(Bool.self, forKey: .recommended)
    }

    var profileTitle: String {
        GovernorDecoding.profileTitle(profile)
    }

    var isRecommended: Bool {
        recommended == true
    }

    /// Defensive presentation guard: even a faulty/older daemon payload must
    /// not show CURRENT and RECOMMENDED on the same row.
    func showsRecommendedBadge(currentProfile: String) -> Bool {
        isRecommended && !isCurrent(currentProfile)
    }

    /// Exact case-insensitive comparison against the daemon's current profile
    /// string. No profile is treated as current by default.
    func isCurrent(_ currentProfile: String) -> Bool {
        let current = currentProfile.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let candidate = profile.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return !current.isEmpty && current == candidate
    }

    var primaryGoalLabel: String {
        GovernorDecoding.bounded(primaryGoal, limit: 160) ?? "Goal not reported"
    }

    var reservePercentLabel: String {
        guard let completionReserveRatio, completionReserveRatio.isFinite else {
            return "posture not reported"
        }
        let percent = completionReserveRatio * 100
        if percent.rounded() == percent {
            return "posture \(Int(percent))%"
        }
        return String(format: "posture %.1f%%", percent)
    }

    var attentionLabel: String {
        GovernorDecoding.bounded(attentionPosture, limit: 80) ?? "attention not reported"
    }

    var qualityLabel: String {
        GovernorDecoding.bounded(qualityPosture, limit: 80) ?? "quality not reported"
    }

    var forecastLabel: String {
        BudgetFormatting.runway(
            turns: projectedTurns,
            seconds: nil,
            cost: projectedCostUSD
        )
    }

    var profileRank: Int {
        RunProfile.allCases.firstIndex {
            $0.rawValue == profile.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        } ?? RunProfile.allCases.count
    }
}

/// Daemon-reported delivery/quality review. Missing evidence stays nil; the
/// card never upgrades readiness or invents a passing test/verifier/merge.
struct QualityReviewView: Codable, Sendable, Equatable {
    static let changedPathPreviewLimit = 8
    static let blockerPreviewLimit = 6
    static let reasonPreviewLimit = 5

    let readinessLevel: String
    let latestCommit: String?
    let latestTest: String?
    let verifier: String?
    let merge: String?
    let completedTasks: UInt32?
    let totalTasks: UInt32?
    let mergedEntries: UInt32?
    let changedPaths: [String]
    let blockers: [String]
    let reviewability: String
    let reasons: [String]

    enum CodingKeys: String, CodingKey {
        case verifier, merge, blockers, reasons, reviewability
        case readinessLevel = "readiness_level"
        case latestCommit = "latest_commit"
        case latestTest = "latest_test"
        case completedTasks = "completed_tasks"
        case totalTasks = "total_tasks"
        case mergedEntries = "merged_entries"
        case changedPaths = "changed_paths"
    }

    init(
        readinessLevel: String = "",
        latestCommit: String? = nil,
        latestTest: String? = nil,
        verifier: String? = nil,
        merge: String? = nil,
        completedTasks: UInt32? = nil,
        totalTasks: UInt32? = nil,
        mergedEntries: UInt32? = nil,
        changedPaths: [String] = [],
        blockers: [String] = [],
        reviewability: String = "",
        reasons: [String] = []
    ) {
        self.readinessLevel = readinessLevel
        self.latestCommit = latestCommit
        self.latestTest = latestTest
        self.verifier = verifier
        self.merge = merge
        self.completedTasks = completedTasks
        self.totalTasks = totalTasks
        self.mergedEntries = mergedEntries
        self.changedPaths = changedPaths
        self.blockers = blockers
        self.reviewability = reviewability
        self.reasons = reasons
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        readinessLevel =
            try container.decodeIfPresent(String.self, forKey: .readinessLevel) ?? ""
        latestCommit = try container.decodeIfPresent(String.self, forKey: .latestCommit)
        latestTest = try container.decodeIfPresent(String.self, forKey: .latestTest)
        verifier = try container.decodeIfPresent(String.self, forKey: .verifier)
        merge = try container.decodeIfPresent(String.self, forKey: .merge)
        completedTasks = try container.decodeIfPresent(UInt32.self, forKey: .completedTasks)
        totalTasks = try container.decodeIfPresent(UInt32.self, forKey: .totalTasks)
        mergedEntries = try container.decodeIfPresent(UInt32.self, forKey: .mergedEntries)
        changedPaths = GovernorDecoding.stringList(from: container, forKey: .changedPaths)
        blockers = GovernorDecoding.stringList(from: container, forKey: .blockers)
        reviewability = try container.decodeIfPresent(String.self, forKey: .reviewability) ?? ""
        reasons = GovernorDecoding.stringList(from: container, forKey: .reasons)
    }

    var readinessLabel: String {
        let value = readinessLevel.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        switch value {
        case "none": return "NONE"
        case "dirty": return "DIRTY"
        case "committed": return "COMMITTED"
        case "tests_passed": return "TESTS PASSED"
        case "verified": return "VERIFIED"
        case "merged": return "MERGED"
        case "partial": return "PARTIAL"
        case "blocked": return "BLOCKED"
        case "": return "NOT REPORTED"
        default: return String(value.uppercased().prefix(24))
        }
    }

    var reviewabilityLabel: String {
        let value = reviewability.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        switch value {
        case "ok": return "OK"
        case "watch": return "WATCH"
        case "risk": return "RISK"
        case "": return "NOT REPORTED"
        default: return String(value.uppercased().prefix(24))
        }
    }

    var taskProgressLabel: String {
        switch (completedTasks, totalTasks) {
        case let (completed?, total?):
            "\(completed) / \(total) tasks"
        case let (completed?, nil):
            "\(completed) tasks completed"
        case let (nil, total?):
            "\(total) total tasks"
        case (nil, nil):
            "tasks not reported"
        }
    }

    var mergedEntriesLabel: String? {
        guard let mergedEntries else { return nil }
        return mergedEntries == 1 ? "1 merged entry" : "\(mergedEntries) merged entries"
    }

    var commitLabel: String? {
        GovernorDecoding.bounded(latestCommit, limit: 16)
    }

    var testLabel: String? {
        GovernorDecoding.bounded(latestTest, limit: 120)
    }

    var verifierLabel: String? {
        GovernorDecoding.bounded(verifier, limit: 120)
    }

    var mergeLabel: String? {
        GovernorDecoding.bounded(merge, limit: 120)
    }

    var changedPathPreviews: [String] {
        Self.previews(changedPaths, limit: Self.changedPathPreviewLimit, characters: 160)
    }

    var changedPathOverflow: Int {
        max(0, changedPaths.count - Self.changedPathPreviewLimit)
    }

    var blockerPreviews: [String] {
        Self.previews(blockers, limit: Self.blockerPreviewLimit, characters: 180)
    }

    var blockerOverflow: Int {
        max(0, blockers.count - Self.blockerPreviewLimit)
    }

    var reasonPreviews: [String] {
        Self.previews(reasons, limit: Self.reasonPreviewLimit, characters: 200)
    }

    var reasonOverflow: Int {
        max(0, reasons.count - Self.reasonPreviewLimit)
    }

    var isEmpty: Bool {
        readinessLevel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && latestCommit == nil && latestTest == nil && verifier == nil && merge == nil
            && completedTasks == nil && totalTasks == nil && mergedEntries == nil
            && changedPaths.isEmpty && blockers.isEmpty
            && reviewability.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && reasons.isEmpty
    }

    private static func previews(
        _ values: [String],
        limit: Int,
        characters: Int
    ) -> [String] {
        values
            .compactMap { GovernorDecoding.humanizedToken($0, limit: characters) }
            .prefix(limit)
            .map(\.self)
    }
}

/// Calibrated confidence from the daemon. The score is shown as a numeric
/// value; the client does not assume a 0–1 or 0–100 scale or derive a level.
struct TradeoffConfidenceView: Codable, Sendable, Equatable {
    static let reasonPreviewLimit = 5

    let level: String
    let score: Double?
    let sampleCalls: UInt32?
    let reasons: [String]

    enum CodingKeys: String, CodingKey {
        case level, score, reasons
        case sampleCalls = "sample_calls"
    }

    init(
        level: String = "",
        score: Double? = nil,
        sampleCalls: UInt32? = nil,
        reasons: [String] = []
    ) {
        self.level = level
        self.score = score
        self.sampleCalls = sampleCalls
        self.reasons = reasons
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        level = try container.decodeIfPresent(String.self, forKey: .level) ?? ""
        score = try container.decodeIfPresent(Double.self, forKey: .score)
        sampleCalls = try container.decodeIfPresent(UInt32.self, forKey: .sampleCalls)
        reasons = GovernorDecoding.stringList(from: container, forKey: .reasons)
    }

    var levelLabel: String {
        GovernorDecoding.confidenceLabel(level)
    }

    var scoreLabel: String {
        guard let score, score.isFinite else { return "score not reported" }
        return String(format: "score %.2f", score)
    }

    var sampleLabel: String? {
        guard let sampleCalls else { return nil }
        return sampleCalls == 1 ? "1 sample call" : "\(sampleCalls) sample calls"
    }

    var reasonPreviews: [String] {
        reasons
            .compactMap { GovernorDecoding.bounded($0, limit: 200) }
            .prefix(Self.reasonPreviewLimit)
            .map(\.self)
    }

    var reasonOverflow: Int {
        max(0, reasons.count - Self.reasonPreviewLimit)
    }
}

// MARK: - Shared bounded decoding

private enum GovernorDecoding {
    static func bounded(_ raw: String?, limit: Int) -> String? {
        guard let raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(max(0, limit)))
    }

    /// Presentation only: daemon recommendation/reason tokens use snake_case.
    /// Human sentences (which contain spaces) are left untouched.
    static func humanizedToken(_ raw: String, limit: Int) -> String? {
        guard let text = bounded(raw, limit: limit) else { return nil }
        guard !text.contains(where: \.isWhitespace), text.contains("_") else {
            return text
        }
        return text.replacingOccurrences(of: "_", with: " ")
    }

    static func hasText(_ raw: String?) -> Bool {
        bounded(raw, limit: 1) != nil
    }

    /// Known profile titles use the canonical label; an unknown but non-empty
    /// daemon value is shown bounded rather than silently dropped or relabeled.
    static func profileTitle(_ raw: String) -> String {
        if let title = RunProfile.exactTitle(fromWire: raw) {
            return title
        }
        return bounded(raw, limit: 24)?.uppercased() ?? "Profile not reported"
    }

    static func confidenceLabel(_ raw: String) -> String {
        let value = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        switch value {
        case "high": return "HIGH"
        case "medium": return "MEDIUM"
        case "low": return "LOW"
        case "": return "CONFIDENCE NOT REPORTED"
        default: return String(value.uppercased().prefix(16))
        }
    }

    static func stringList<K: CodingKey>(
        from container: KeyedDecodingContainer<K>,
        forKey key: K
    ) -> [String] {
        guard container.contains(key) else { return [] }
        if (try? container.decodeNil(forKey: key)) == true {
            return []
        }
        if let values = try? container.decode([String].self, forKey: key) {
            return values
        }
        if let values = try? container.decode([JSONValue].self, forKey: key) {
            return values.compactMap { value in
                if let text = value.stringValue {
                    return text
                }
                guard let object = value.objectValue else { return nil }
                return object["task_id"]?.stringValue
                    ?? object["id"]?.stringValue
                    ?? object["label"]?.stringValue
                    ?? object["title"]?.stringValue
                    ?? object["message"]?.stringValue
                    ?? object["reason"]?.stringValue
            }
        }
        if let value = try? container.decode(String.self, forKey: key) {
            return [value]
        }
        return []
    }

    /// Recommendations or warnings may be plain strings or objects.
    static func displayStringList<K: CodingKey>(
        from container: KeyedDecodingContainer<K>,
        forKey key: K
    ) -> [String] {
        guard container.contains(key) else { return [] }
        if (try? container.decodeNil(forKey: key)) == true {
            return []
        }
        if let values = try? container.decode([String].self, forKey: key) {
            return values
        }
        if let values = try? container.decode([JSONValue].self, forKey: key) {
            return values.compactMap { value in
                if let text = value.stringValue {
                    return text
                }
                guard let object = value.objectValue else { return nil }
                let primary = object["action"]?.stringValue
                    ?? object["recommendation"]?.stringValue
                    ?? object["text"]?.stringValue
                    ?? object["message"]?.stringValue
                    ?? object["summary"]?.stringValue
                    ?? object["reason"]?.stringValue
                    ?? object["title"]?.stringValue
                guard let primary else { return nil }
                var parts = [primary]
                if let reason = object["reason"]?.stringValue, reason != primary {
                    parts.append(reason)
                }
                if let confidence = object["confidence"]?.stringValue,
                   !confidence.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                {
                    parts.append(confidence.uppercased())
                }
                return parts.joined(separator: " · ")
            }
        }
        if let value = try? container.decode(String.self, forKey: key) {
            return [value]
        }
        return []
    }

    static func recommendations<K: CodingKey>(
        from container: KeyedDecodingContainer<K>,
        forKey key: K
    ) -> [AllocationRecommendationView] {
        guard container.contains(key) else { return [] }
        if (try? container.decodeNil(forKey: key)) == true {
            return []
        }
        if let values = try? container.decode([AllocationRecommendationView].self, forKey: key) {
            return values
        }
        guard let values = try? container.decode([JSONValue].self, forKey: key) else {
            if let value = try? container.decode(String.self, forKey: key) {
                return [AllocationRecommendationView(action: value)]
            }
            return []
        }
        return values.compactMap { value in
            if let text = value.stringValue {
                return AllocationRecommendationView(action: text)
            }
            guard let data = try? JSONEncoder().encode(value) else { return nil }
            return try? JSONDecoder().decode(AllocationRecommendationView.self, from: data)
        }
    }

    /// Decodes elements leniently: a malformed entry is skipped instead of
    /// discarding the whole preview. The daemon remains the source of truth.
    static func elements<Element: Decodable, K: CodingKey>(
        _ type: Element.Type,
        from container: KeyedDecodingContainer<K>,
        forKey key: K
    ) -> [Element] {
        guard container.contains(key) else { return [] }
        if (try? container.decodeNil(forKey: key)) == true {
            return []
        }
        if let values = try? container.decode([Element].self, forKey: key) {
            return values
        }
        guard let values = try? container.decode([JSONValue].self, forKey: key) else {
            return []
        }
        return values.compactMap { value in
            guard let data = try? JSONEncoder().encode(value) else { return nil }
            return try? JSONDecoder().decode(type, from: data)
        }
    }
}

/// Exact (case-insensitive) profile title lookup. Unlike `fromWire`, this does
/// not silently substitute Balanced for an unknown or missing value.
extension RunProfile {
    static func exactTitle(fromWire value: String) -> String? {
        let normalized = value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !normalized.isEmpty else { return nil }
        return RunProfile(rawValue: normalized)?.title
    }
}

private func matches(_ value: String, _ options: String...) -> Bool {
    options.contains(value)
}
