import Foundation

/// Read-only runtime version pinning metadata (ADR-0196). The daemon owns the
/// snapshot, compatibility classification, and migration. The client never
/// recomputes a hash or infers compatibility from a version string.
struct RuntimeSnapshotView: Codable, Sendable, Equatable {
    var schemaVersion: Int
    var runtimeVersion: String
    var runtimeBuild: String
    var policyHash: String
    var toolBundleHash: String
    var promptLayoutVersion: Int
    var planRevision: Int
    var contractHash: String
    var createdAt: String

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case runtimeVersion = "runtime_version"
        case runtimeBuild = "runtime_build"
        case policyHash = "policy_hash"
        case toolBundleHash = "tool_bundle_hash"
        case promptLayoutVersion = "prompt_layout_version"
        case planRevision = "plan_revision"
        case contractHash = "contract_hash"
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try container.decodeIfPresent(Int.self, forKey: .schemaVersion) ?? 0
        runtimeVersion = try container.decodeIfPresent(String.self, forKey: .runtimeVersion) ?? ""
        runtimeBuild = try container.decodeIfPresent(String.self, forKey: .runtimeBuild) ?? ""
        policyHash = try container.decodeIfPresent(String.self, forKey: .policyHash) ?? ""
        toolBundleHash = try container.decodeIfPresent(String.self, forKey: .toolBundleHash) ?? ""
        promptLayoutVersion = try container.decodeIfPresent(Int.self, forKey: .promptLayoutVersion) ?? 0
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        contractHash = try container.decodeIfPresent(String.self, forKey: .contractHash) ?? ""
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

struct CompatibilityChangeView: Codable, Sendable, Identifiable, Equatable {
    var field: String
    var stored: String
    var current: String
    var severity: String

    var id: String {
        "\(field):\(severity)"
    }

    enum CodingKeys: String, CodingKey {
        case field, stored, current, severity
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        field = try container.decodeIfPresent(String.self, forKey: .field) ?? ""
        stored = try container.decodeIfPresent(String.self, forKey: .stored) ?? ""
        current = try container.decodeIfPresent(String.self, forKey: .current) ?? ""
        severity = try container.decodeIfPresent(String.self, forKey: .severity) ?? "drift"
    }
}

/// `compatible`, `drift`, or `incompatible`. Kept as a raw string so an older
/// client can still display a future classification without failing decode.
struct CompatibilityReportView: Codable, Sendable, Equatable {
    var status: String
    var target: String
    var sessionID: String
    var runID: String?
    var planRevision: Int
    var snapshotAgeSecs: UInt64
    var snapshotAge: String
    var snapshotCreatedAt: String?
    var stored: RuntimeSnapshotView?
    var current: RuntimeSnapshotView?
    var changed: [CompatibilityChangeView]
    var warnings: [String]
    var migrationRequired: Bool
    var checkedAt: String

    enum CodingKeys: String, CodingKey {
        case status, target
        case sessionID = "session_id"
        case runID = "run_id"
        case planRevision = "plan_revision"
        case snapshotAgeSecs = "snapshot_age_secs"
        case snapshotAge = "snapshot_age"
        case snapshotCreatedAt = "snapshot_created_at"
        case stored, current, changed, warnings
        case migrationRequired = "migration_required"
        case checkedAt = "checked_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? "compatible"
        target = try container.decodeIfPresent(String.self, forKey: .target) ?? "plan"
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        runID = try container.decodeIfPresent(String.self, forKey: .runID)
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        snapshotAgeSecs = try container.decodeIfPresent(UInt64.self, forKey: .snapshotAgeSecs) ?? 0
        snapshotAge = try container.decodeIfPresent(String.self, forKey: .snapshotAge) ?? ""
        snapshotCreatedAt = try container.decodeIfPresent(String.self, forKey: .snapshotCreatedAt)
        stored = try container.decodeIfPresent(RuntimeSnapshotView.self, forKey: .stored)
        current = try container.decodeIfPresent(RuntimeSnapshotView.self, forKey: .current)
        changed = try container.decodeIfPresent([CompatibilityChangeView].self, forKey: .changed) ?? []
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
        migrationRequired = try container.decodeIfPresent(Bool.self, forKey: .migrationRequired) ?? false
        checkedAt = try container.decodeIfPresent(String.self, forKey: .checkedAt) ?? ""
    }

    var isIncompatible: Bool {
        status == "incompatible"
    }

    var isDrift: Bool {
        status == "drift"
    }

    var isCompatible: Bool {
        status == "compatible"
    }
}

struct PlanMigrateResultView: Codable, Sendable, Equatable {
    var sessionID: String
    var migrated: Bool
    var planRevision: Int
    var snapshot: RuntimeSnapshotView?
    var eventSeq: UInt64
    var warnings: [String]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case migrated
        case planRevision = "plan_revision"
        case snapshot
        case eventSeq = "event_seq"
        case warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        migrated = try container.decodeIfPresent(Bool.self, forKey: .migrated) ?? false
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        snapshot = try container.decodeIfPresent(RuntimeSnapshotView.self, forKey: .snapshot)
        eventSeq = try container.decodeIfPresent(UInt64.self, forKey: .eventSeq) ?? 0
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}

struct ControllerReplayDiffView: Codable, Sendable, Equatable {
    var field: String
    var recorded: String
    var recomputed: String

    enum CodingKeys: String, CodingKey { case field, recorded, recomputed }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        field = try container.decodeIfPresent(String.self, forKey: .field) ?? ""
        recorded = try container.decodeIfPresent(String.self, forKey: .recorded) ?? ""
        recomputed = try container.decodeIfPresent(String.self, forKey: .recomputed) ?? ""
    }
}

struct ControllerReplayEntryView: Codable, Sendable, Identifiable, Equatable {
    var seq: UInt64
    var kind: String
    var recordedAt: String?
    var status: String
    var recordedAction: String?
    var recomputedAction: String?
    var taskID: String?
    var changes: [ControllerReplayDiffView]
    var reason: String?

    var id: UInt64 {
        seq
    }

    enum CodingKeys: String, CodingKey {
        case seq, kind
        case recordedAt = "recorded_at"
        case status
        case recordedAction = "recorded_action"
        case recomputedAction = "recomputed_action"
        case taskID = "task_id"
        case changes, reason
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        seq = try container.decodeIfPresent(UInt64.self, forKey: .seq) ?? 0
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        recordedAt = try container.decodeIfPresent(String.self, forKey: .recordedAt)
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? "skipped"
        recordedAction = try container.decodeIfPresent(String.self, forKey: .recordedAction)
        recomputedAction = try container.decodeIfPresent(String.self, forKey: .recomputedAction)
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        changes = try container.decodeIfPresent([ControllerReplayDiffView].self, forKey: .changes) ?? []
        reason = try container.decodeIfPresent(String.self, forKey: .reason)
    }
}

struct ControllerReplayReportView: Codable, Sendable, Equatable {
    var sessionID: String
    var limit: Int
    var totalRecorded: UInt64
    var examined: UInt64
    var matched: UInt64
    var mismatched: UInt64
    var skipped: UInt64
    var truncated: Bool
    var entries: [ControllerReplayEntryView]
    var controllerEquivalentOnly: Bool
    var generatedAt: String
    var warnings: [String]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case limit
        case totalRecorded = "total_recorded"
        case examined, matched, mismatched, skipped, truncated, entries
        case controllerEquivalentOnly = "controller_equivalent_only"
        case generatedAt = "generated_at"
        case warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        limit = try container.decodeIfPresent(Int.self, forKey: .limit) ?? 0
        totalRecorded = try container.decodeIfPresent(UInt64.self, forKey: .totalRecorded) ?? 0
        examined = try container.decodeIfPresent(UInt64.self, forKey: .examined) ?? 0
        matched = try container.decodeIfPresent(UInt64.self, forKey: .matched) ?? 0
        mismatched = try container.decodeIfPresent(UInt64.self, forKey: .mismatched) ?? 0
        skipped = try container.decodeIfPresent(UInt64.self, forKey: .skipped) ?? 0
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        entries = try container.decodeIfPresent([ControllerReplayEntryView].self, forKey: .entries) ?? []
        controllerEquivalentOnly = try container.decodeIfPresent(
            Bool.self,
            forKey: .controllerEquivalentOnly
        ) ?? true
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}
