import Foundation
import SwiftUI

// MARK: - Copy and display bounds

/// Display-side bounds for the advisory progress ledger. The daemon already
/// normalizes every field; these limits protect the transcript from legacy or
/// hand-written payloads and keep rows predictable without parsing dates.
enum ProgressText {
    static let maxItemChars = 200
    static let maxReferenceChars = 200
    static let maxInstructionChars = 1000
    static let maxReasonChars = 400
    static let maxRoundIDChars = 80
    static let maxAgentNameChars = 120
    static let maxTaskIDChars = 120
    static let maxItemCount = 8
    static let maxRoundCount = 256

    static func bounded(_ value: String, limit: Int = maxItemChars) -> String {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard limit > 0, trimmed.count > limit else { return trimmed }
        let end = trimmed.index(trimmed.startIndex, offsetBy: limit - 1)
        return String(trimmed[..<end]) + "…"
    }

    static func optionalText(_ value: String?, limit: Int = maxInstructionChars) -> String? {
        guard let value else { return nil }
        let bounded = bounded(value, limit: limit)
        return bounded.isEmpty ? nil : bounded
    }

    static func normalizedList(_ values: [String], limit: Int = maxItemCount) -> [String] {
        var output: [String] = []
        output.reserveCapacity(min(values.count, limit))
        for value in values {
            let bounded = bounded(value)
            guard !bounded.isEmpty else { continue }
            output.append(bounded)
            if output.count >= limit {
                break
            }
        }
        return output
    }

    /// Display-only timestamp normalization. The ledger does not need a parsed
    /// `Date`; ISO-8601 text is shown as a compact local-looking label.
    static func timestamp(_ value: String?) -> String? {
        guard let value else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let normalized = trimmed.replacingOccurrences(of: "T", with: " ")
        return String(normalized.prefix(19))
    }

    static func pluralized(_ count: Int, singular: String, plural: String? = nil) -> String {
        "\(count) \(count == 1 ? singular : plural ?? singular + "s")"
    }
}

enum ProgressCopy {
    /// Exact badge copy requested by the daemon contract.
    static let advisoryBadgeText = "advisory only"
    static let advisoryTitle = "Advisory only"
    static let advisoryDetail =
        "This ledger is evidence, not authorization. It never selects, retries, cancels, merges, or schedules work."
    static let emptyRoundsTitle = "No rounds recorded"
    static let emptyRoundsDetail =
        "No Manager progress rounds have been retained for this session yet."
    static let disabledTitle = "Progress ledger disabled"
    static let disabledDetail =
        "Progress reporting is not enabled for this session. The card stays read-only."
    static let unavailableTitle = "Progress unavailable"
    static let unavailableDetail = "Refresh to load the latest advisory assessment."
    static let loadingTitle = "Loading progress"
    static let truncatedBadge = "truncated"
    static let evidenceBadge = "evidence"
}

// MARK: - Tone and status

/// Semantic color role. Colors live here so the model helper stays usable from
/// any surface without duplicating status switches in each view.
enum ProgressTone: String, CaseIterable, Hashable, Sendable {
    case active
    case positive
    case caution
    case danger
    case neutral

    var tint: Color {
        switch self {
        case .active: VPTheme.accent
        case .positive: VPTheme.success
        case .caution: VPTheme.warning
        case .danger: VPTheme.danger
        case .neutral: VPTheme.textTertiary
        }
    }
}

/// Deterministic advisory status from the daemon. It is evidence only and is
/// never interpreted as a scheduler command.
enum ProgressStatus: String, Codable, CaseIterable, Identifiable, Hashable, Sendable {
    case progressing
    case inLoop = "in_loop"
    case stalled
    case blocked
    case completed
    case insufficientEvidence = "insufficient_evidence"
    case unknown

    var id: String {
        rawValue
    }

    var label: String {
        switch self {
        case .progressing: L10n.t("Progressing")
        case .inLoop: L10n.t("In loop")
        case .stalled: L10n.t("Stalled")
        case .blocked: L10n.t("Blocked")
        case .completed: L10n.t("Completed")
        case .insufficientEvidence: L10n.t("Insufficient evidence")
        case .unknown: L10n.t("Unknown")
        }
    }

    var tone: ProgressTone {
        switch self {
        case .progressing: .active
        case .inLoop, .stalled: .caution
        case .blocked: .danger
        case .completed: .positive
        case .insufficientEvidence, .unknown: .neutral
        }
    }

    var tint: Color {
        tone.tint
    }

    var icon: String {
        switch self {
        case .progressing: "chart.line.uptrend.xyaxis"
        case .inLoop: "arrow.triangle.2.circlepath"
        case .stalled: "hourglass"
        case .blocked: "hand.raised.fill"
        case .completed: "checkmark.circle.fill"
        case .insufficientEvidence: "questionmark.circle"
        case .unknown: "questionmark.circle"
        }
    }

    var helpText: String {
        switch self {
        case .progressing: "Retained rounds show forward movement."
        case .inLoop: "Repeated rounds describe the same loop."
        case .stalled: "Retained rounds show no material progress."
        case .blocked: "A reported blocker prevents progress."
        case .completed: "The retained evidence reports completion."
        case .insufficientEvidence: "Not enough retained rounds to assess progress."
        case .unknown: "The daemon returned a status this client does not recognize."
        }
    }

    var needsAttention: Bool {
        self == .inLoop || self == .stalled || self == .blocked
    }

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .unknown
    }
}

// MARK: - Evidence

enum ProgressEvidenceKind: String, Codable, CaseIterable, Identifiable, Hashable, Sendable {
    case task
    case test
    case commit
    case artifact
    case event
    case note
    case unknown

    var id: String {
        rawValue
    }

    var label: String {
        switch self {
        case .task: L10n.t("Task")
        case .test: L10n.t("Test")
        case .commit: L10n.t("Commit")
        case .artifact: L10n.t("Artifact")
        case .event: L10n.t("Event")
        case .note: L10n.t("Note")
        case .unknown: L10n.t("Other")
        }
    }

    var icon: String {
        switch self {
        case .task: "checklist"
        case .test: "testtube.2"
        case .commit: "arrow.triangle.branch"
        case .artifact: "shippingbox"
        case .event: "bolt"
        case .note: "note.text"
        case .unknown: "ellipsis.circle"
        }
    }

    var tint: Color {
        switch self {
        case .task: VPTheme.accent
        case .test: VPTheme.success
        case .commit: VPTheme.plan
        case .artifact: VPTheme.info
        case .event: VPTheme.warning
        case .note: VPTheme.textSecondary
        case .unknown: VPTheme.textTertiary
        }
    }

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .unknown
    }
}

/// One bounded evidence pointer. It is a reference, never file content.
struct ProgressEvidence: Codable, Identifiable, Hashable, Sendable {
    let kind: ProgressEvidenceKind
    let reference: String
    let note: String?

    var id: String {
        "\(kind.rawValue)|\(reference)|\(note ?? "")"
    }

    init(kind: ProgressEvidenceKind, reference: String = "", note: String? = nil) {
        self.kind = kind
        self.reference = ProgressText.bounded(
            reference,
            limit: ProgressText.maxReferenceChars
        )
        self.note = ProgressText.optionalText(note, limit: ProgressText.maxItemChars)
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decodeIfPresent(ProgressEvidenceKind.self, forKey: .kind)
            ?? .unknown
        let reference = try container.decodeIfPresent(String.self, forKey: .reference) ?? ""
        let note = try container.decodeIfPresent(String.self, forKey: .note)
        self.init(kind: kind, reference: reference, note: note)
    }

    var hasReference: Bool {
        !reference.isEmpty
    }

    var displayReference: String {
        reference.isEmpty ? "no reference" : reference
    }

    /// Shorter label used by dense chips and flow layouts.
    var shortReference: String {
        ProgressText.bounded(reference, limit: 64)
    }

    enum CodingKeys: String, CodingKey {
        case kind
        case reference
        case note
    }
}

// MARK: - Round

/// One bounded Manager progress round.
struct ProgressRound: Codable, Identifiable, Hashable, Sendable {
    let roundID: String
    let agentID: String?
    let agentName: String?
    let taskID: String?
    let satisfied: [String]
    let inLoop: [String]
    let progressing: [String]
    let blockers: [String]
    let nextTask: String?
    let instruction: String?
    let evidence: [ProgressEvidence]
    let createdAt: String?

    var id: String {
        if !roundID.isEmpty {
            return roundID
        }
        let agent = agentID ?? agentName ?? "round"
        let task = taskID ?? ""
        let time = createdAt ?? ""
        return "\(agent)|\(task)|\(time)|\(satisfied.first ?? "")"
    }

    init(
        roundID: String = "",
        agentID: String? = nil,
        agentName: String? = nil,
        taskID: String? = nil,
        satisfied: [String] = [],
        inLoop: [String] = [],
        progressing: [String] = [],
        blockers: [String] = [],
        nextTask: String? = nil,
        instruction: String? = nil,
        evidence: [ProgressEvidence] = [],
        createdAt: String? = nil
    ) {
        self.roundID = ProgressText.bounded(roundID, limit: ProgressText.maxRoundIDChars)
        self.agentID = ProgressText.optionalText(agentID, limit: ProgressText.maxAgentNameChars)
        self.agentName = ProgressText.optionalText(
            agentName,
            limit: ProgressText.maxAgentNameChars
        )
        self.taskID = ProgressText.optionalText(taskID, limit: ProgressText.maxTaskIDChars)
        self.satisfied = ProgressText.normalizedList(satisfied)
        self.inLoop = ProgressText.normalizedList(inLoop)
        self.progressing = ProgressText.normalizedList(progressing)
        self.blockers = ProgressText.normalizedList(blockers)
        self.nextTask = ProgressText.optionalText(
            nextTask,
            limit: ProgressText.maxInstructionChars
        )
        self.instruction = ProgressText.optionalText(
            instruction,
            limit: ProgressText.maxInstructionChars
        )
        self.evidence = Array(evidence.prefix(ProgressText.maxItemCount))
        self.createdAt = ProgressText.timestamp(createdAt)
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let roundID = try container.decodeIfPresent(String.self, forKey: .roundID) ?? ""
        let agentID = try container.decodeIfPresent(String.self, forKey: .agentID)
        let agentName = try container.decodeIfPresent(String.self, forKey: .agentName)
        let taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        let satisfied = try container.decodeIfPresent([String].self, forKey: .satisfied) ?? []
        let inLoop = try container.decodeIfPresent([String].self, forKey: .inLoop) ?? []
        let progressing = try container.decodeIfPresent([String].self, forKey: .progressing) ?? []
        let blockers = try container.decodeIfPresent([String].self, forKey: .blockers) ?? []
        let nextTask = try container.decodeIfPresent(String.self, forKey: .nextTask)
        let instruction = try container.decodeIfPresent(String.self, forKey: .instruction)
        let evidence = try container.decodeIfPresent([ProgressEvidence].self, forKey: .evidence)
            ?? []
        let createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt)
        self.init(
            roundID: roundID,
            agentID: agentID,
            agentName: agentName,
            taskID: taskID,
            satisfied: satisfied,
            inLoop: inLoop,
            progressing: progressing,
            blockers: blockers,
            nextTask: nextTask,
            instruction: instruction,
            evidence: evidence,
            createdAt: createdAt
        )
    }

    var agentDisplayName: String {
        guard let agentName, !agentName.isEmpty else {
            return L10n.t("Manager")
        }
        return agentName
    }

    var agentSeed: String {
        agentID ?? agentName ?? roundID
    }

    var timestampLabel: String? {
        createdAt
    }

    var listItemCount: Int {
        satisfied.count + inLoop.count + progressing.count + blockers.count
    }

    var countsSummary: String {
        guard listItemCount > 0 || !evidence.isEmpty else {
            return "no items"
        }
        var parts: [String] = []
        if !satisfied.isEmpty {
            parts.append("\(satisfied.count) satisfied")
        }
        if !inLoop.isEmpty {
            parts.append("\(inLoop.count) in loop")
        }
        if !progressing.isEmpty {
            parts.append("\(progressing.count) progressing")
        }
        if !blockers.isEmpty {
            parts.append("\(blockers.count) blocked")
        }
        if !evidence.isEmpty {
            parts.append("\(evidence.count) evidence")
        }
        return parts.joined(separator: " · ")
    }

    var isEmpty: Bool {
        listItemCount == 0
            && evidence.isEmpty
            && (nextTask?.isEmpty ?? true)
            && (instruction?.isEmpty ?? true)
    }

    enum CodingKeys: String, CodingKey {
        case roundID = "round_id"
        case agentID = "agent_id"
        case agentName = "agent_name"
        case taskID = "task_id"
        case satisfied
        case inLoop = "in_loop"
        case progressing
        case blockers
        case nextTask = "next_task"
        case instruction
        case evidence
        case createdAt = "created_at"
    }
}

// MARK: - Assessment and ledger

/// Deterministic assessment over retained rounds. `advisoryOnly` is always
/// treated as true by the UI even if an older daemon omits or contradicts it.
struct ProgressAssessment: Codable, Identifiable, Hashable, Sendable {
    let status: ProgressStatus
    let reasonCode: String
    let reason: String
    let inLoopStreak: Int
    let stalledRounds: Int
    let satisfiedCount: Int
    let evidenceCount: Int
    let nextTask: String?
    let instruction: String?
    let advisoryOnly: Bool

    static let empty = ProgressAssessment(
        status: .insufficientEvidence,
        reasonCode: "no_rounds",
        reason: "No progress rounds recorded.",
        inLoopStreak: 0,
        stalledRounds: 0,
        satisfiedCount: 0,
        evidenceCount: 0,
        nextTask: nil,
        instruction: nil,
        advisoryOnly: true
    )

    var id: String {
        "\(status.rawValue)|\(reasonCode)|\(inLoopStreak)|\(stalledRounds)"
    }

    init(
        status: ProgressStatus,
        reasonCode: String,
        reason: String,
        inLoopStreak: Int,
        stalledRounds: Int,
        satisfiedCount: Int,
        evidenceCount: Int,
        nextTask: String?,
        instruction: String?,
        advisoryOnly: Bool = true
    ) {
        self.status = status
        self.reasonCode = ProgressText.bounded(reasonCode, limit: 80)
        self.reason = ProgressText.bounded(reason, limit: ProgressText.maxReasonChars)
        self.inLoopStreak = max(0, inLoopStreak)
        self.stalledRounds = max(0, stalledRounds)
        self.satisfiedCount = max(0, satisfiedCount)
        self.evidenceCount = max(0, evidenceCount)
        self.nextTask = ProgressText.optionalText(
            nextTask,
            limit: ProgressText.maxInstructionChars
        )
        self.instruction = ProgressText.optionalText(
            instruction,
            limit: ProgressText.maxInstructionChars
        )
        self.advisoryOnly = advisoryOnly
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let rawStatus = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        let status = ProgressStatus(rawValue: rawStatus) ?? .insufficientEvidence
        let reasonCode = try container.decodeIfPresent(String.self, forKey: .reasonCode) ?? ""
        let reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        let inLoopStreak = try container.decodeIfPresent(Int.self, forKey: .inLoopStreak) ?? 0
        let stalledRounds = try container.decodeIfPresent(Int.self, forKey: .stalledRounds) ?? 0
        let satisfiedCount = try container.decodeIfPresent(Int.self, forKey: .satisfiedCount) ?? 0
        let evidenceCount = try container.decodeIfPresent(Int.self, forKey: .evidenceCount) ?? 0
        let nextTask = try container.decodeIfPresent(String.self, forKey: .nextTask)
        let instruction = try container.decodeIfPresent(String.self, forKey: .instruction)
        let advisoryOnly = try container.decodeIfPresent(Bool.self, forKey: .advisoryOnly) ?? true
        self.init(
            status: status,
            reasonCode: reasonCode,
            reason: reason,
            inLoopStreak: inLoopStreak,
            stalledRounds: stalledRounds,
            satisfiedCount: satisfiedCount,
            evidenceCount: evidenceCount,
            nextTask: nextTask,
            instruction: instruction,
            advisoryOnly: advisoryOnly
        )
    }

    var hasReason: Bool {
        !reason.isEmpty
    }

    var reasonCodeLabel: String {
        reasonCode.isEmpty ? "NO REASON CODE" : reasonCode.uppercased()
    }

    var hasNextStep: Bool {
        nextTask != nil || instruction != nil
    }

    enum CodingKeys: String, CodingKey {
        case status
        case reasonCode = "reason_code"
        case reason
        case inLoopStreak = "in_loop_streak"
        case stalledRounds = "stalled_rounds"
        case satisfiedCount = "satisfied_count"
        case evidenceCount = "evidence_count"
        case nextTask = "next_task"
        case instruction
        case advisoryOnly = "advisory_only"
    }
}

/// Session-scoped advisory progress ledger returned by `session.progress`.
/// The newest round is expected to be last; the daemon stores rounds in
/// ascending plan/sequence order.
struct ProgressLedger: Codable, Identifiable, Hashable, Sendable {
    let sessionID: String
    let planRevision: Int
    let rounds: [ProgressRound]
    let assessment: ProgressAssessment
    let truncated: Bool
    let advisoryOnly: Bool

    var id: String {
        sessionID
    }

    init(
        sessionID: String,
        planRevision: Int = 0,
        rounds: [ProgressRound] = [],
        assessment: ProgressAssessment = .empty,
        truncated: Bool = false,
        advisoryOnly: Bool = true
    ) {
        self.sessionID = ProgressText.bounded(sessionID, limit: 120)
        self.planRevision = max(0, planRevision)
        self.rounds = Array(rounds.prefix(ProgressText.maxRoundCount))
        self.assessment = assessment
        self.truncated = truncated
        self.advisoryOnly = advisoryOnly
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        let planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        let rounds = try container.decodeIfPresent([ProgressRound].self, forKey: .rounds) ?? []
        let assessment = try container.decodeIfPresent(ProgressAssessment.self, forKey: .assessment)
            ?? .empty
        let truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        let advisoryOnly = try container.decodeIfPresent(Bool.self, forKey: .advisoryOnly) ?? true
        self.init(
            sessionID: sessionID,
            planRevision: planRevision,
            rounds: rounds,
            assessment: assessment,
            truncated: truncated,
            advisoryOnly: advisoryOnly
        )
    }

    static func empty(sessionID: String, planRevision: Int = 0) -> ProgressLedger {
        ProgressLedger(sessionID: sessionID, planRevision: planRevision)
    }

    var latestRound: ProgressRound? {
        rounds.last
    }

    var earlierRounds: [ProgressRound] {
        Array(rounds.dropLast().suffix(3))
    }

    var hasRounds: Bool {
        !rounds.isEmpty
    }

    var isAdvisoryOnly: Bool {
        advisoryOnly || assessment.advisoryOnly
    }

    var roundCountLabel: String {
        ProgressText.pluralized(rounds.count, singular: "round")
    }

    var planRevisionLabel: String {
        "r\(planRevision)"
    }

    var sessionLabel: String {
        sessionID.isEmpty ? "unknown session" : String(sessionID.prefix(18))
    }

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case planRevision = "plan_revision"
        case rounds
        case assessment
        case truncated
        case advisoryOnly = "advisory_only"
    }
}
