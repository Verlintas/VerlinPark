import Foundation

/// Read-only Custos status projection. Only the fields the instrument renders
/// are decoded; the wire response may carry additional status data.
struct CustosStatus: Decodable, Sendable, Equatable {
    let state: CustosState
    let observeOnly: Bool
    let role: String
    let charterHash: String
    let dimensions: [CustosDimension]
    let counts: CustosCounts
    let ring: CustosRing
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case state
        case observeOnly = "observe_only"
        case role
        case charterHash = "charter_hash"
        case dimensions
        case counts
        case ring
        case generatedAt = "generated_at"
    }
}

enum CustosState: String, Decodable, Sendable, Equatable {
    case healthy
    case watch
    case degraded
    case critical
    case quarantined
    case signalLost = "signal_lost"
    case unknown

    init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = CustosState(rawValue: raw) ?? .unknown
    }

    var word: String {
        switch self {
        case .healthy:
            "HEALTHY"
        case .watch:
            "WATCH"
        case .degraded:
            "DEGRADED"
        case .critical:
            "CRITICAL"
        case .quarantined:
            "QUARANTINED"
        case .signalLost:
            "SIGNAL LOST"
        case .unknown:
            "UNKNOWN"
        }
    }
}

enum CustosDimensionState: String, Decodable, Sendable, Equatable {
    case ok
    case watch
    case degraded
    case critical
    case unknown

    init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = CustosDimensionState(rawValue: raw) ?? .unknown
    }

    var word: String {
        rawValue.uppercased()
    }
}

struct CustosDimension: Decodable, Sendable, Equatable {
    let key: String
    let state: CustosDimensionState
    let detail: String
}

struct CustosCounts: Decodable, Sendable, Equatable {
    let sessionsTotal: Int
    let sessionsRunning: Int
    let sessionsPaused: Int
    let sessionsFailed: Int
    let tasksPending: Int
    let mergesQueued: Int
    let mergesConflict: Int
    let effectsUnknown: Int
    let approvalsPending: Int
    let leasesExpired: Int
    let eventAgeSecs: Int?

    enum CodingKeys: String, CodingKey {
        case sessionsTotal = "sessions_total"
        case sessionsRunning = "sessions_running"
        case sessionsPaused = "sessions_paused"
        case sessionsFailed = "sessions_failed"
        case tasksPending = "tasks_pending"
        case mergesQueued = "merges_queued"
        case mergesConflict = "merges_conflict"
        case effectsUnknown = "effects_unknown"
        case approvalsPending = "approvals_pending"
        case leasesExpired = "leases_expired"
        case eventAgeSecs = "event_age_secs"
    }
}

struct CustosRing: Decodable, Sendable, Equatable {
    let mode: String
    let fresh: Int
    let stale: Int
    let missing: Int
    let quorum: Bool
}

// MARK: - Presentation

struct CustosDimensionRow: Identifiable, Sendable, Equatable {
    let key: String
    let label: String
    let state: CustosDimensionState
    let detail: String

    var id: String {
        key
    }

    static let canonicalKeys = [
        "runtime",
        "data",
        "leases",
        "queues",
        "effects",
        "signal",
        "ring",
        "processes",
    ]

    static func label(for key: String) -> String {
        switch key {
        case "runtime": "RUNTIME"
        case "data": "DATA"
        case "leases": "LEASES"
        case "queues": "QUEUES"
        case "effects": "EFFECTS"
        case "signal": "SIGNAL"
        case "ring": "RING"
        case "processes": "PROCESSES"
        default: key.uppercased()
        }
    }

    static let placeholders: [CustosDimensionRow] = canonicalKeys.map {
        CustosDimensionRow(
            key: $0,
            label: label(for: $0),
            state: .unknown,
            detail: "no observation"
        )
    }
}

struct CustosCountItem: Identifiable, Sendable, Equatable {
    let id: String
    let label: String
    let value: String
}

extension CustosStatus {
    var dimensionRows: [CustosDimensionRow] {
        CustosDimensionRow.canonicalKeys.map { key in
            if let dimension = dimensions.first(where: { $0.key == key }) {
                return CustosDimensionRow(
                    key: key,
                    label: CustosDimensionRow.label(for: key),
                    state: dimension.state,
                    detail: dimension.detail
                )
            }
            return CustosDimensionRow(
                key: key,
                label: CustosDimensionRow.label(for: key),
                state: .unknown,
                detail: "no observation"
            )
        }
    }

    var countItems: [CustosCountItem] {
        [
            CustosCountItem(id: "sessions", label: "SESSIONS", value: "\(counts.sessionsTotal)"),
            CustosCountItem(id: "running", label: "RUNNING", value: "\(counts.sessionsRunning)"),
            CustosCountItem(id: "paused", label: "PAUSED", value: "\(counts.sessionsPaused)"),
            CustosCountItem(id: "failed", label: "FAILED", value: "\(counts.sessionsFailed)"),
            CustosCountItem(id: "tasks", label: "TASKS PENDING", value: "\(counts.tasksPending)"),
            CustosCountItem(id: "queued", label: "MERGES QUEUED", value: "\(counts.mergesQueued)"),
            CustosCountItem(id: "conflicts", label: "MERGE CONFLICTS", value: "\(counts.mergesConflict)"),
            CustosCountItem(id: "effects", label: "EFFECTS UNKNOWN", value: "\(counts.effectsUnknown)"),
            CustosCountItem(id: "approvals", label: "APPROVALS", value: "\(counts.approvalsPending)"),
            CustosCountItem(id: "leases", label: "LEASES EXPIRED", value: "\(counts.leasesExpired)"),
            CustosCountItem(
                id: "event-age",
                label: "EVENT AGE",
                value: counts.eventAgeSecs.map { "\($0)s" } ?? "--"
            ),
        ]
    }

    var ringLine: String {
        let mode = ring.mode.isEmpty ? "UNKNOWN" : ring.mode.uppercased()
        let quorum = ring.quorum ? "YES" : "NO"
        return "\(mode)  /  FRESH \(ring.fresh)  /  STALE \(ring.stale)  /  MISSING \(ring.missing)  /  QUORUM \(quorum)"
    }

    var sentence: String {
        let watchCount = dimensions.filter { $0.state == .watch }.count
        let degradedCount = dimensions.filter { $0.state == .degraded }.count
        let criticalCount = dimensions.filter { $0.state == .critical }.count

        switch state {
        case .healthy:
            return "All eight observations are within tolerance. Protection remains active."
        case .watch:
            return plural(
                watchCount,
                singular: "One observation is at watch; no containment is active.",
                plural: "\(watchCount) observations are at watch; no containment is active."
            )
        case .degraded:
            return plural(
                degradedCount,
                singular: "One dimension is degraded; protection remains active and sealed.",
                plural: "\(degradedCount) dimensions are degraded; protection remains active and sealed."
            )
        case .critical:
            return plural(
                criticalCount,
                singular: "One dimension is critical; containment is sealed and audited.",
                plural: "\(criticalCount) dimensions are critical; containment is sealed and audited."
            )
        case .quarantined:
            return "A protected surface is quarantined. Evidence is preserved; Custos remains sealed."
        case .signalLost:
            return "The read-only Custos socket did not answer. No live state is asserted."
        case .unknown:
            return "The Custos socket reported an unrecognized state. No action is asserted."
        }
    }

    var shortCharter: String {
        let value = charterHash.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return "--" }
        if value.count <= 26 {
            return value
        }
        return String(value.prefix(23)) + "..."
    }

    var generatedLabel: String {
        let value = generatedAt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return "--" }
        return String(value.replacingOccurrences(of: "T", with: " ").prefix(19))
    }

    private func plural(_ count: Int, singular: String, plural: String) -> String {
        count == 1 ? singular : plural
    }
}
