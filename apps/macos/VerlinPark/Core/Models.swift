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

enum GuiSensitiveMode: String, Codable, Hashable, Sendable {
    case deny
    case requireApproval = "require_approval"

    var label: String {
        switch self {
        case .deny: "Deny"
        case .requireApproval: "Require approval"
        }
    }
}

struct GuiPolicy: Codable, Hashable, Sendable {
    var graceSecs: Int
    var waitMs: Int
    var statusTtlSecs: Int
    var sensitiveMode: GuiSensitiveMode
    var sensitiveBundles: [String]
    var sensitiveTitles: [String]
    var timelineRetentionDays: Int

    static let standard = GuiPolicy(
        graceSecs: 45,
        waitMs: 15000,
        statusTtlSecs: 30,
        sensitiveMode: .deny,
        sensitiveBundles: [],
        sensitiveTitles: [],
        timelineRetentionDays: 30
    )

    enum CodingKeys: String, CodingKey {
        case graceSecs = "grace_secs"
        case waitMs = "wait_ms"
        case statusTtlSecs = "status_ttl_secs"
        case sensitiveMode = "sensitive_mode"
        case sensitiveBundles = "sensitive_bundles"
        case sensitiveTitles = "sensitive_titles"
        case timelineRetentionDays = "timeline_retention_days"
    }

    init(
        graceSecs: Int,
        waitMs: Int,
        statusTtlSecs: Int,
        sensitiveMode: GuiSensitiveMode,
        sensitiveBundles: [String],
        sensitiveTitles: [String],
        timelineRetentionDays: Int = 30
    ) {
        self.graceSecs = graceSecs
        self.waitMs = waitMs
        self.statusTtlSecs = statusTtlSecs
        self.sensitiveMode = sensitiveMode
        self.sensitiveBundles = sensitiveBundles
        self.sensitiveTitles = sensitiveTitles
        self.timelineRetentionDays = timelineRetentionDays
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        graceSecs = try container.decodeIfPresent(Int.self, forKey: .graceSecs) ?? 45
        waitMs = try container.decodeIfPresent(Int.self, forKey: .waitMs) ?? 15000
        statusTtlSecs = try container.decodeIfPresent(Int.self, forKey: .statusTtlSecs) ?? 30
        if let raw = try? container.decodeIfPresent(String.self, forKey: .sensitiveMode),
           let mode = GuiSensitiveMode(rawValue: raw)
        {
            sensitiveMode = mode
        } else {
            sensitiveMode = .deny
        }
        sensitiveBundles = try container.decodeIfPresent([String].self, forKey: .sensitiveBundles) ?? []
        sensitiveTitles = try container.decodeIfPresent([String].self, forKey: .sensitiveTitles) ?? []
        timelineRetentionDays = try container.decodeIfPresent(
            Int.self,
            forKey: .timelineRetentionDays
        ) ?? 30
    }
}

struct GuiTimelinePrunedInfo: Codable, Hashable, Sendable {
    var sessionId: String
    var seq: Int
    var retentionDays: Int
    var cutoff: String
    var pruned: Int
    var createdAt: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case seq
        case retentionDays = "retention_days"
        case cutoff
        case pruned
        case createdAt = "created_at"
    }
}

struct GuiTimelineAuditResult: Codable, Hashable, Sendable {
    var projectId: String
    var entries: [GuiTimelinePrunedInfo]
    var total: Int
    var limit: Int

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case entries
        case total
        case limit
    }
}

struct GuiTimelineRetentionResult: Codable, Hashable, Sendable {
    var projectId: String
    var retentionDays: Int
    var keepForever: Bool
    var cutoff: String?
    var total: Int
    var eligible: Int
    var oldestAt: String?
    var newestAt: String?

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case retentionDays = "retention_days"
        case keepForever = "keep_forever"
        case cutoff
        case total
        case eligible
        case oldestAt = "oldest_at"
        case newestAt = "newest_at"
    }
}

struct GuiTimelinePruneResult: Codable, Hashable, Sendable {
    var projectId: String
    var retentionDays: Int
    var cutoff: String
    var dryRun: Bool
    var pruned: Int
    var remaining: Int
    var sessions: Int
    var skippedSessions: Int
    var skippedEvents: Int
    var otherKindsTouched: Int
    var limit: Int
    var truncated: Bool
    var reason: String
    var affectedSessions: [String]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case retentionDays = "retention_days"
        case cutoff
        case dryRun = "dry_run"
        case pruned
        case remaining
        case sessions
        case skippedSessions = "skipped_sessions"
        case skippedEvents = "skipped_events"
        case otherKindsTouched = "other_kinds_touched"
        case limit
        case truncated
        case reason
        case affectedSessions = "affected_sessions"
    }
}

struct DaemonAgentStatus: Codable, Hashable, Sendable {
    var label: String
    var plistPath: String
    var installed: Bool
    var loaded: Bool
    var disabled: Bool
    var running: Bool
    var supervised: Bool
    var pid: Int?
    var state: String
    var detail: String
    var launchctlAvailable: Bool

    enum CodingKeys: String, CodingKey {
        case label
        case plistPath = "plist_path"
        case installed
        case loaded
        case disabled
        case running
        case supervised
        case pid
        case state
        case detail
        case launchctlAvailable = "launchctl_available"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        label = try container.decodeIfPresent(String.self, forKey: .label) ?? "com.verlinpark.daemon"
        plistPath = try container.decodeIfPresent(String.self, forKey: .plistPath) ?? ""
        installed = try container.decodeIfPresent(Bool.self, forKey: .installed) ?? false
        loaded = try container.decodeIfPresent(Bool.self, forKey: .loaded) ?? false
        disabled = try container.decodeIfPresent(Bool.self, forKey: .disabled) ?? false
        running = try container.decodeIfPresent(Bool.self, forKey: .running) ?? false
        supervised = try container.decodeIfPresent(Bool.self, forKey: .supervised) ?? false
        pid = try container.decodeIfPresent(Int.self, forKey: .pid)
        state = try container.decodeIfPresent(String.self, forKey: .state) ?? "unknown"
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
        launchctlAvailable = try container.decodeIfPresent(
            Bool.self,
            forKey: .launchctlAvailable
        ) ?? false
    }
}

struct GuiPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: GuiPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct GuiLeaseInfo: Codable, Hashable, Sendable {
    var leaseId: String
    var ownerId: String
    var sessionId: String
    var agentId: String
    var agentName: String
    var surface: String
    var leaseGeneration: Int
    var acquiredAt: String
    var lastActionAt: String
    var expiresAt: String
    var queuePosition: Int?

    enum CodingKeys: String, CodingKey {
        case leaseId = "lease_id"
        case ownerId = "owner_id"
        case sessionId = "session_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case surface
        case leaseGeneration = "lease_generation"
        case acquiredAt = "acquired_at"
        case lastActionAt = "last_action_at"
        case expiresAt = "expires_at"
        case queuePosition = "queue_position"
    }
}

struct GuiQueueInfo: Codable, Hashable, Sendable {
    var sessionId: String
    var agentId: String
    var agentName: String
    var surface: String
    var position: Int
    var waitingMs: Int

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case surface
        case position
        case waitingMs = "waiting_ms"
    }
}

struct GuiComputerStatus: Codable, Hashable, Sendable {
    var platformSupported: Bool?
    var helperAvailable: Bool?
    var helperPath: String?
    var permissionsChecked: Bool?
    var accessibility: Bool?
    var screenRecording: Bool?
    var error: String?
    var cached: Bool?
    var cacheAgeMs: Int?
    var graceSecs: Int?
    var waitMs: Int?
    var snapshotGenerations: Bool?

    enum CodingKeys: String, CodingKey {
        case platformSupported = "platform_supported"
        case helperAvailable = "helper_available"
        case helperPath = "helper_path"
        case permissionsChecked = "permissions_checked"
        case permissions
        case error
        case cached
        case cacheAgeMs = "cache_age_ms"
        case graceSecs = "grace_secs"
        case waitMs = "wait_ms"
        case snapshotGenerations = "snapshot_generations"
    }

    enum PermissionKeys: String, CodingKey {
        case accessibility
        case screenRecording = "screen_recording"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        platformSupported = try container.decodeIfPresent(Bool.self, forKey: .platformSupported)
        helperAvailable = try container.decodeIfPresent(Bool.self, forKey: .helperAvailable)
        helperPath = try container.decodeIfPresent(String.self, forKey: .helperPath)
        permissionsChecked = try container.decodeIfPresent(Bool.self, forKey: .permissionsChecked)
        error = try container.decodeIfPresent(String.self, forKey: .error)
        cached = try container.decodeIfPresent(Bool.self, forKey: .cached)
        cacheAgeMs = try container.decodeIfPresent(Int.self, forKey: .cacheAgeMs)
        graceSecs = try container.decodeIfPresent(Int.self, forKey: .graceSecs)
        waitMs = try container.decodeIfPresent(Int.self, forKey: .waitMs)
        snapshotGenerations = try container.decodeIfPresent(Bool.self, forKey: .snapshotGenerations)
        if let permissions = try? container.nestedContainer(
            keyedBy: PermissionKeys.self,
            forKey: .permissions
        ) {
            accessibility = try permissions.decodeIfPresent(Bool.self, forKey: .accessibility)
            screenRecording = try permissions.decodeIfPresent(Bool.self, forKey: .screenRecording)
        } else {
            accessibility = nil
            screenRecording = nil
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(platformSupported, forKey: .platformSupported)
        try container.encodeIfPresent(helperAvailable, forKey: .helperAvailable)
        try container.encodeIfPresent(helperPath, forKey: .helperPath)
        try container.encodeIfPresent(permissionsChecked, forKey: .permissionsChecked)
        try container.encodeIfPresent(error, forKey: .error)
        try container.encodeIfPresent(cached, forKey: .cached)
        try container.encodeIfPresent(cacheAgeMs, forKey: .cacheAgeMs)
        try container.encodeIfPresent(graceSecs, forKey: .graceSecs)
        try container.encodeIfPresent(waitMs, forKey: .waitMs)
        try container.encodeIfPresent(snapshotGenerations, forKey: .snapshotGenerations)
        if accessibility != nil || screenRecording != nil {
            var permissions = container.nestedContainer(
                keyedBy: PermissionKeys.self,
                forKey: .permissions
            )
            try permissions.encodeIfPresent(accessibility, forKey: .accessibility)
            try permissions.encodeIfPresent(screenRecording, forKey: .screenRecording)
        }
    }
}

struct GuiStatusResult: Codable, Sendable {
    var runtimeId: String
    var leaseSurface: String
    var holder: GuiLeaseInfo?
    var queue: [GuiQueueInfo]
    var policy: GuiPolicy
    var computer: GuiComputerStatus?
    var browserHelperAvailable: Bool
    var browserHelperPath: String?
    var browserSandboxAvailable: Bool
    var warmComputerHelpers: Int
    var warmBrowserHelpers: Int
    var staleRefSupported: Bool

    enum CodingKeys: String, CodingKey {
        case runtimeId = "runtime_id"
        case leaseSurface = "lease_surface"
        case holder
        case queue
        case policy
        case computer
        case browserHelperAvailable = "browser_helper_available"
        case browserHelperPath = "browser_helper_path"
        case browserSandboxAvailable = "browser_sandbox_available"
        case warmComputerHelpers = "warm_computer_helpers"
        case warmBrowserHelpers = "warm_browser_helpers"
        case staleRefSupported = "stale_ref_supported"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        runtimeId = try container.decodeIfPresent(String.self, forKey: .runtimeId) ?? ""
        leaseSurface = try container.decodeIfPresent(String.self, forKey: .leaseSurface) ?? "gui"
        holder = try container.decodeIfPresent(GuiLeaseInfo.self, forKey: .holder)
        queue = try container.decodeIfPresent([GuiQueueInfo].self, forKey: .queue) ?? []
        policy = try container.decodeIfPresent(GuiPolicy.self, forKey: .policy) ?? .standard
        computer = try container.decodeIfPresent(GuiComputerStatus.self, forKey: .computer)
        browserHelperAvailable = try container.decodeIfPresent(
            Bool.self,
            forKey: .browserHelperAvailable
        ) ?? false
        browserHelperPath = try container.decodeIfPresent(String.self, forKey: .browserHelperPath)
        browserSandboxAvailable = try container.decodeIfPresent(
            Bool.self,
            forKey: .browserSandboxAvailable
        ) ?? false
        warmComputerHelpers = try container.decodeIfPresent(Int.self, forKey: .warmComputerHelpers) ?? 0
        warmBrowserHelpers = try container.decodeIfPresent(Int.self, forKey: .warmBrowserHelpers) ?? 0
        staleRefSupported = try container.decodeIfPresent(Bool.self, forKey: .staleRefSupported) ?? true
    }
}

struct GuiActionInfo: Codable, Hashable, Sendable {
    var id: String
    var sessionId: String
    var agentId: String
    var agentName: String
    var surface: String
    var action: String
    var targetApp: String?
    var targetBundleId: String?
    var targetWindow: String?
    var sensitiveTarget: Bool
    var snapshotGeneration: Int
    var leaseGeneration: Int
    var queuedMs: Int
    var latencyMs: Int
    var result: String
    var errorCode: String?
    var occurredAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case surface
        case action
        case targetApp = "target_app"
        case targetBundleId = "target_bundle_id"
        case targetWindow = "target_window"
        case sensitiveTarget = "sensitive_target"
        case snapshotGeneration = "snapshot_generation"
        case leaseGeneration = "lease_generation"
        case queuedMs = "queued_ms"
        case latencyMs = "latency_ms"
        case result
        case errorCode = "error_code"
        case occurredAt = "occurred_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        agentId = try container.decodeIfPresent(String.self, forKey: .agentId) ?? ""
        agentName = try container.decodeIfPresent(String.self, forKey: .agentName) ?? ""
        surface = try container.decodeIfPresent(String.self, forKey: .surface) ?? "computer"
        action = try container.decodeIfPresent(String.self, forKey: .action) ?? ""
        targetApp = try container.decodeIfPresent(String.self, forKey: .targetApp)
        targetBundleId = try container.decodeIfPresent(String.self, forKey: .targetBundleId)
        targetWindow = try container.decodeIfPresent(String.self, forKey: .targetWindow)
        sensitiveTarget = try container.decodeIfPresent(Bool.self, forKey: .sensitiveTarget) ?? false
        snapshotGeneration = try container.decodeIfPresent(Int.self, forKey: .snapshotGeneration) ?? 0
        leaseGeneration = try container.decodeIfPresent(Int.self, forKey: .leaseGeneration) ?? 0
        queuedMs = try container.decodeIfPresent(Int.self, forKey: .queuedMs) ?? 0
        latencyMs = try container.decodeIfPresent(Int.self, forKey: .latencyMs) ?? 0
        result = try container.decodeIfPresent(String.self, forKey: .result) ?? "ok"
        errorCode = try container.decodeIfPresent(String.self, forKey: .errorCode)
        occurredAt = try container.decodeIfPresent(String.self, forKey: .occurredAt) ?? ""
    }
}

struct GuiTimelineResult: Codable, Sendable {
    var actions: [GuiActionInfo]
    var limit: Int
}

struct ComputerLeaseReleaseResult: Codable, Sendable {
    var released: Bool
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

struct ToolOutputPolicy: Codable, Hashable, Sendable {
    let enabled: Bool
    let spillThresholdBytes: Int
    let previewBytes: Int
    let maxTotalBytesPerTurn: Int
    let retentionSecs: Int
    let maxEntries: Int
    let toolThresholds: [String: Int]?

    static let `default` = ToolOutputPolicy(
        enabled: true,
        spillThresholdBytes: 16384,
        previewBytes: 4096,
        maxTotalBytesPerTurn: 65536,
        retentionSecs: 604_800,
        maxEntries: 2000,
        toolThresholds: nil
    )

    enum CodingKeys: String, CodingKey {
        case enabled
        case spillThresholdBytes = "spill_threshold_bytes"
        case previewBytes = "preview_bytes"
        case maxTotalBytesPerTurn = "max_total_bytes_per_turn"
        case retentionSecs = "retention_secs"
        case maxEntries = "max_entries"
        case toolThresholds = "tool_thresholds"
    }
}

struct ToolOutputAuditResult: Codable, Sendable {
    let projectId: String?
    let rows: Int
    let blobFiles: Int
    let missingBlobs: Int
    let hashMismatches: Int
    let unsafeBlobFiles: Int
    let orphanBlobs: Int
    let orphanBytes: Int
    let cleanedOrphans: Int
    let cleanedBytes: Int
    let checkedAt: String

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case rows
        case blobFiles = "blob_files"
        case missingBlobs = "missing_blobs"
        case hashMismatches = "hash_mismatches"
        case unsafeBlobFiles = "unsafe_blob_files"
        case orphanBlobs = "orphan_blobs"
        case orphanBytes = "orphan_bytes"
        case cleanedOrphans = "cleaned_orphans"
        case cleanedBytes = "cleaned_bytes"
        case checkedAt = "checked_at"
    }
}

struct ToolOutputPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: ToolOutputPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct ToolOutputListResult: Codable, Sendable {
    let entries: [ToolOutputEntry]
    let total: Int
    let truncated: Bool
    let nextBefore: String?

    enum CodingKeys: String, CodingKey {
        case entries
        case total
        case truncated
        case nextBefore = "next_before"
    }
}

struct ToolOutputEntry: Codable, Identifiable, Sendable {
    let id: String
    let sessionId: String
    let toolCallId: String?
    let toolName: String
    let sha256: String
    let bytes: Int
    let lines: Int
    let previewBytes: Int
    let createdAt: String
    let expiresAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case toolCallId = "tool_call_id"
        case toolName = "tool_name"
        case sha256
        case bytes
        case lines
        case previewBytes = "preview_bytes"
        case createdAt = "created_at"
        case expiresAt = "expires_at"
    }
}

struct ToolOutputToolStat: Codable, Sendable {
    let toolName: String
    let entries: Int
    let bytes: Int
    let previewBytes: Int

    enum CodingKeys: String, CodingKey {
        case toolName = "tool_name"
        case entries
        case bytes
        case previewBytes = "preview_bytes"
    }
}

struct ToolOutputStatus: Codable, Sendable {
    let projectId: String?
    let sessionId: String?
    let policy: ToolOutputPolicy
    let entries: Int
    let totalBytes: Int
    let previewBytes: Int
    let activeEntries: Int
    let expiredEntries: Int
    let oldestCreatedAt: String?
    let newestCreatedAt: String?
    let byTool: [ToolOutputToolStat]?

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case sessionId = "session_id"
        case policy
        case entries
        case totalBytes = "total_bytes"
        case previewBytes = "preview_bytes"
        case activeEntries = "active_entries"
        case expiredEntries = "expired_entries"
        case oldestCreatedAt = "oldest_created_at"
        case newestCreatedAt = "newest_created_at"
        case byTool = "by_tool"
    }
}

/// Bounded per-tool activity from `session.tool_activity`. Rows are derived
/// from `tool.finished` events and never contain provider content.
struct ToolActivityResult: Codable, Sendable, Equatable {
    let sessionID: String
    let entries: [ToolActivityEntry]
    let total: Int
    let limit: Int
    let truncated: Bool
    let summary: ToolActivitySummary

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case entries
        case total
        case limit
        case truncated
        case summary
    }
}

struct ToolActivityEntry: Codable, Sendable, Equatable, Identifiable {
    let toolCallID: String
    let name: String
    let seq: Int
    let isError: Bool
    let errorCode: String?
    let retryable: Bool
    let hint: String?
    let durationMs: Int
    let outputBytes: Int
    let truncated: Bool
    let approvalRisk: String?
    let approvalMode: String?
    let approvalWaitMs: Int?
    let createdAt: String

    var id: String {
        seq > 0 ? "\(seq)-\(toolCallID)" : toolCallID
    }

    enum CodingKeys: String, CodingKey {
        case toolCallID = "tool_call_id"
        case name
        case seq
        case isError = "is_error"
        case errorCode = "error_code"
        case retryable
        case hint
        case durationMs = "duration_ms"
        case outputBytes = "output_bytes"
        case truncated
        case approvalRisk = "approval_risk"
        case approvalMode = "approval_mode"
        case approvalWaitMs = "approval_wait_ms"
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        toolCallID = try container.decodeIfPresent(String.self, forKey: .toolCallID) ?? ""
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? "unknown"
        seq = try container.decodeIfPresent(Int.self, forKey: .seq) ?? 0
        isError = try container.decodeIfPresent(Bool.self, forKey: .isError) ?? false
        errorCode = try container.decodeIfPresent(String.self, forKey: .errorCode)
        retryable = try container.decodeIfPresent(Bool.self, forKey: .retryable) ?? false
        hint = try container.decodeIfPresent(String.self, forKey: .hint)
        durationMs = try container.decodeIfPresent(Int.self, forKey: .durationMs) ?? 0
        outputBytes = try container.decodeIfPresent(Int.self, forKey: .outputBytes) ?? 0
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        approvalRisk = try container.decodeIfPresent(String.self, forKey: .approvalRisk)
        approvalMode = try container.decodeIfPresent(String.self, forKey: .approvalMode)
        approvalWaitMs = try container.decodeIfPresent(Int.self, forKey: .approvalWaitMs)
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

struct ToolActivitySummary: Codable, Sendable, Equatable {
    let calls: Int
    let errors: Int
    let retryableErrors: Int
    let truncatedResults: Int
    let totalDurationMs: Int
    let maxDurationMs: Int
    let averageDurationMs: Int
    let outputBytes: Int
    let approvalWaits: Int
    let totalApprovalWaitMs: Int
    let maxApprovalWaitMs: Int
    let byTool: [ToolActivityToolStat]
    let errorCodes: [ToolActivityCodeStat]

    enum CodingKeys: String, CodingKey {
        case calls
        case errors
        case retryableErrors = "retryable_errors"
        case truncatedResults = "truncated_results"
        case totalDurationMs = "total_duration_ms"
        case maxDurationMs = "max_duration_ms"
        case averageDurationMs = "average_duration_ms"
        case outputBytes = "output_bytes"
        case approvalWaits = "approval_waits"
        case totalApprovalWaitMs = "total_approval_wait_ms"
        case maxApprovalWaitMs = "max_approval_wait_ms"
        case byTool = "by_tool"
        case errorCodes = "error_codes"
    }

    init(
        calls: Int,
        errors: Int,
        retryableErrors: Int,
        truncatedResults: Int,
        totalDurationMs: Int,
        maxDurationMs: Int,
        averageDurationMs: Int,
        outputBytes: Int,
        approvalWaits: Int = 0,
        totalApprovalWaitMs: Int = 0,
        maxApprovalWaitMs: Int = 0,
        byTool: [ToolActivityToolStat],
        errorCodes: [ToolActivityCodeStat] = []
    ) {
        self.calls = calls
        self.errors = errors
        self.retryableErrors = retryableErrors
        self.truncatedResults = truncatedResults
        self.totalDurationMs = totalDurationMs
        self.maxDurationMs = maxDurationMs
        self.averageDurationMs = averageDurationMs
        self.outputBytes = outputBytes
        self.approvalWaits = approvalWaits
        self.totalApprovalWaitMs = totalApprovalWaitMs
        self.maxApprovalWaitMs = maxApprovalWaitMs
        self.byTool = byTool
        self.errorCodes = errorCodes
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        calls = try container.decodeIfPresent(Int.self, forKey: .calls) ?? 0
        errors = try container.decodeIfPresent(Int.self, forKey: .errors) ?? 0
        retryableErrors = try container.decodeIfPresent(Int.self, forKey: .retryableErrors) ?? 0
        truncatedResults = try container.decodeIfPresent(Int.self, forKey: .truncatedResults) ?? 0
        totalDurationMs = try container.decodeIfPresent(Int.self, forKey: .totalDurationMs) ?? 0
        maxDurationMs = try container.decodeIfPresent(Int.self, forKey: .maxDurationMs) ?? 0
        averageDurationMs = try container.decodeIfPresent(Int.self, forKey: .averageDurationMs) ?? 0
        outputBytes = try container.decodeIfPresent(Int.self, forKey: .outputBytes) ?? 0
        approvalWaits = try container.decodeIfPresent(Int.self, forKey: .approvalWaits) ?? 0
        totalApprovalWaitMs = try container.decodeIfPresent(Int.self, forKey: .totalApprovalWaitMs) ?? 0
        maxApprovalWaitMs = try container.decodeIfPresent(Int.self, forKey: .maxApprovalWaitMs) ?? 0
        byTool = try container.decodeIfPresent([ToolActivityToolStat].self, forKey: .byTool) ?? []
        errorCodes = try container.decodeIfPresent([ToolActivityCodeStat].self, forKey: .errorCodes) ?? []
    }
}

struct ToolActivityCodeStat: Codable, Sendable, Equatable, Identifiable {
    let code: String
    let calls: Int

    var id: String {
        code
    }

    enum CodingKeys: String, CodingKey {
        case code
        case calls
    }
}

struct ToolActivityToolStat: Codable, Sendable, Equatable, Identifiable {
    let name: String
    let calls: Int
    let errors: Int
    let totalDurationMs: Int
    let outputBytes: Int

    var id: String {
        name
    }

    enum CodingKeys: String, CodingKey {
        case name
        case calls
        case errors
        case totalDurationMs = "total_duration_ms"
        case outputBytes = "output_bytes"
    }
}

struct ProjectUsageSession: Codable, Identifiable, Sendable {
    let sessionId: String
    let goal: String
    let status: String
    let calls: Int
    let inputTokens: Int
    let outputTokens: Int
    let cacheReadTokens: Int?
    let cacheWriteTokens: Int?
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
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
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

enum AcpTransport: String, Codable, CaseIterable, Hashable, Sendable {
    case stdio
    case streamableHTTP = "streamable_http"
    case websocket

    var displayName: String {
        switch self {
        case .stdio: "stdio"
        case .streamableHTTP: "streamable HTTP"
        case .websocket: "WebSocket"
        }
    }

    var isRemote: Bool {
        self != .stdio
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
    let transport: AcpTransport
    let endpoint: String?
    let authHeaderEnv: String?
    let headers: [String: String]
    let connectTimeoutSecs: Int
    let allowRemote: Bool

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
        case transport
        case endpoint
        case authHeaderEnv = "auth_header_env"
        case headers
        case connectTimeoutSecs = "connect_timeout_secs"
        case allowRemote = "allow_remote"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        name = try container.decode(String.self, forKey: .name)
        command = try container.decodeIfPresent(String.self, forKey: .command) ?? ""
        args = try container.decodeIfPresent([String].self, forKey: .args) ?? []
        env = try container.decodeIfPresent([String: String].self, forKey: .env) ?? [:]
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        autoApprove = try container.decodeIfPresent(Bool.self, forKey: .autoApprove) ?? false
        timeoutSecs = try container.decodeIfPresent(Int.self, forKey: .timeoutSecs) ?? 1800
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        transport = try container.decodeIfPresent(AcpTransport.self, forKey: .transport) ?? .stdio
        endpoint = try container.decodeIfPresent(String.self, forKey: .endpoint)
        authHeaderEnv = try container.decodeIfPresent(String.self, forKey: .authHeaderEnv)
        headers = try container.decodeIfPresent([String: String].self, forKey: .headers) ?? [:]
        connectTimeoutSecs = try container.decodeIfPresent(Int.self, forKey: .connectTimeoutSecs) ?? 30
        allowRemote = try container.decodeIfPresent(Bool.self, forKey: .allowRemote) ?? false
    }

    var summary: String {
        if transport.isRemote {
            return endpoint ?? "remote endpoint"
        }
        return command
    }
}

struct AcpAgentListResult: Codable, Sendable {
    let agents: [AcpAgent]
}

struct AcpAgentResult: Codable, Sendable {
    let agent: AcpAgent
}

struct AcpProbeCapabilities: Codable, Sendable {
    let loadSession: Bool
    let mcpHTTP: Bool
    let mcpSSE: Bool
    let promptImage: Bool
    let promptAudio: Bool
    let promptEmbeddedContext: Bool

    enum CodingKeys: String, CodingKey {
        case loadSession = "load_session"
        case mcpHTTP = "mcp_http"
        case mcpSSE = "mcp_sse"
        case promptImage = "prompt_image"
        case promptAudio = "prompt_audio"
        case promptEmbeddedContext = "prompt_embedded_context"
    }

    init() {
        loadSession = false
        mcpHTTP = false
        mcpSSE = false
        promptImage = false
        promptAudio = false
        promptEmbeddedContext = false
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        loadSession = try container.decodeIfPresent(Bool.self, forKey: .loadSession) ?? false
        mcpHTTP = try container.decodeIfPresent(Bool.self, forKey: .mcpHTTP) ?? false
        mcpSSE = try container.decodeIfPresent(Bool.self, forKey: .mcpSSE) ?? false
        promptImage = try container.decodeIfPresent(Bool.self, forKey: .promptImage) ?? false
        promptAudio = try container.decodeIfPresent(Bool.self, forKey: .promptAudio) ?? false
        promptEmbeddedContext =
            try container.decodeIfPresent(Bool.self, forKey: .promptEmbeddedContext) ?? false
    }
}

struct AcpProbeResult: Codable, Sendable {
    let agentId: String
    let agentName: String
    let version: String
    let protocolVersion: Int
    let sessionCreated: Bool
    let transport: AcpTransport
    let endpoint: String?
    let remote: Bool
    let loadSession: Bool
    let capabilities: AcpProbeCapabilities
    let httpVersion: String?
    let messagesSent: Int
    let messagesReceived: Int
    let streamsOpened: Int

    enum CodingKeys: String, CodingKey {
        case agentId = "agent_id"
        case agentName = "agent_name"
        case version
        case protocolVersion = "protocol_version"
        case sessionCreated = "session_created"
        case transport
        case endpoint
        case remote
        case loadSession = "load_session"
        case capabilities
        case httpVersion = "http_version"
        case messagesSent = "messages_sent"
        case messagesReceived = "messages_received"
        case streamsOpened = "streams_opened"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        agentId = try container.decode(String.self, forKey: .agentId)
        agentName = try container.decode(String.self, forKey: .agentName)
        version = try container.decodeIfPresent(String.self, forKey: .version) ?? ""
        protocolVersion = try container.decodeIfPresent(Int.self, forKey: .protocolVersion) ?? 1
        sessionCreated = try container.decodeIfPresent(Bool.self, forKey: .sessionCreated) ?? false
        transport = try container.decodeIfPresent(AcpTransport.self, forKey: .transport) ?? .stdio
        endpoint = try container.decodeIfPresent(String.self, forKey: .endpoint)
        remote = try container.decodeIfPresent(Bool.self, forKey: .remote) ?? false
        loadSession = try container.decodeIfPresent(Bool.self, forKey: .loadSession) ?? false
        capabilities =
            try container.decodeIfPresent(AcpProbeCapabilities.self, forKey: .capabilities)
                ?? AcpProbeCapabilities()
        httpVersion = try container.decodeIfPresent(String.self, forKey: .httpVersion)
        messagesSent = try container.decodeIfPresent(Int.self, forKey: .messagesSent) ?? 0
        messagesReceived = try container.decodeIfPresent(Int.self, forKey: .messagesReceived) ?? 0
        streamsOpened = try container.decodeIfPresent(Int.self, forKey: .streamsOpened) ?? 0
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

/// Retrieval index mode for the optional semantic lane. `scan` is the exact
/// bounded default; `ivf` is an explicit opt-in derived index with automatic
/// fallback. The enum is decoded leniently so a newer daemon value cannot
/// break an older client.
enum MemoryIndexMode: String, Codable, Sendable, CaseIterable, Hashable {
    case scan
    case ivf

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = MemoryIndexMode(rawValue: raw) ?? .scan
    }

    var label: String {
        switch self {
        case .scan: "exact scan"
        case .ivf: "IVF"
        }
    }
}

struct MemoryEmbeddingPolicy: Codable, Equatable, Sendable {
    var enabled: Bool
    var endpoint: String?
    var model: String?
    var dimensions: Int
    var batchSize: Int
    var timeoutSecs: Int
    var maxCandidates: Int
    var minSimilarity: Double
    var apiKeyEnv: String?
    var index: MemoryIndexMode
    var ivfNlist: Int
    var ivfNprobe: Int
    var ivfMinVectors: Int
    var ivfRebuildVectors: Int

    static let defaults = MemoryEmbeddingPolicy(
        enabled: false,
        endpoint: nil,
        model: nil,
        dimensions: 1536,
        batchSize: 16,
        timeoutSecs: 30,
        maxCandidates: 64,
        minSimilarity: 0.20,
        apiKeyEnv: nil,
        index: .scan,
        ivfNlist: 64,
        ivfNprobe: 8,
        ivfMinVectors: 256,
        ivfRebuildVectors: 4096
    )

    enum CodingKeys: String, CodingKey {
        case enabled
        case endpoint
        case model
        case dimensions
        case batchSize = "batch_size"
        case timeoutSecs = "timeout_secs"
        case maxCandidates = "max_candidates"
        case minSimilarity = "min_similarity"
        case apiKeyEnv = "api_key_env"
        case index
        case ivfNlist = "ivf_nlist"
        case ivfNprobe = "ivf_nprobe"
        case ivfMinVectors = "ivf_min_vectors"
        case ivfRebuildVectors = "ivf_rebuild_vectors"
    }

    init(
        enabled: Bool,
        endpoint: String?,
        model: String?,
        dimensions: Int,
        batchSize: Int,
        timeoutSecs: Int,
        maxCandidates: Int,
        minSimilarity: Double,
        apiKeyEnv: String?,
        index: MemoryIndexMode = .scan,
        ivfNlist: Int = 64,
        ivfNprobe: Int = 8,
        ivfMinVectors: Int = 256,
        ivfRebuildVectors: Int = 4096
    ) {
        self.enabled = enabled
        self.endpoint = endpoint
        self.model = model
        self.dimensions = dimensions
        self.batchSize = batchSize
        self.timeoutSecs = timeoutSecs
        self.maxCandidates = maxCandidates
        self.minSimilarity = minSimilarity
        self.apiKeyEnv = apiKeyEnv
        self.index = index
        self.ivfNlist = ivfNlist
        self.ivfNprobe = ivfNprobe
        self.ivfMinVectors = ivfMinVectors
        self.ivfRebuildVectors = ivfRebuildVectors
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        endpoint = try container.decodeIfPresent(String.self, forKey: .endpoint)
        model = try container.decodeIfPresent(String.self, forKey: .model)
        dimensions = try container.decodeIfPresent(Int.self, forKey: .dimensions) ?? 1536
        batchSize = try container.decodeIfPresent(Int.self, forKey: .batchSize) ?? 16
        timeoutSecs = try container.decodeIfPresent(Int.self, forKey: .timeoutSecs) ?? 30
        maxCandidates = try container.decodeIfPresent(Int.self, forKey: .maxCandidates) ?? 64
        minSimilarity = try container.decodeIfPresent(Double.self, forKey: .minSimilarity) ?? 0.20
        apiKeyEnv = try container.decodeIfPresent(String.self, forKey: .apiKeyEnv)
        index = try container.decodeIfPresent(MemoryIndexMode.self, forKey: .index) ?? .scan
        ivfNlist = try container.decodeIfPresent(Int.self, forKey: .ivfNlist) ?? 64
        ivfNprobe = try container.decodeIfPresent(Int.self, forKey: .ivfNprobe) ?? 8
        ivfMinVectors = try container.decodeIfPresent(Int.self, forKey: .ivfMinVectors) ?? 256
        ivfRebuildVectors = try container.decodeIfPresent(Int.self, forKey: .ivfRebuildVectors)
            ?? 4096
    }

    var isConfigured: Bool {
        !(endpoint?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
            && !(model?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
    }

    var usesIVF: Bool {
        index == .ivf && enabled && isConfigured
    }

    /// JSON-RPC form. The key environment *name* is sent; no secret is ever
    /// part of the policy object.
    func jsonObject() -> [String: JSONValue] {
        [
            "enabled": .bool(enabled),
            "endpoint": endpoint.map(JSONValue.string) ?? .null,
            "model": model.map(JSONValue.string) ?? .null,
            "dimensions": .number(Double(dimensions)),
            "batch_size": .number(Double(batchSize)),
            "timeout_secs": .number(Double(timeoutSecs)),
            "max_candidates": .number(Double(maxCandidates)),
            "min_similarity": .number(minSimilarity),
            "api_key_env": apiKeyEnv.flatMap { value in
                value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    ? nil
                    : JSONValue.string(value)
            } ?? .null,
            "index": .string(index.rawValue),
            "ivf_nlist": .number(Double(ivfNlist)),
            "ivf_nprobe": .number(Double(ivfNprobe)),
            "ivf_min_vectors": .number(Double(ivfMinVectors)),
            "ivf_rebuild_vectors": .number(Double(ivfRebuildVectors)),
        ]
    }
}

struct MemoryPolicy: Codable, Equatable, Sendable {
    let enabled: Bool
    let maxItems: Int?
    let maxAgeDays: Int?
    /// TTL applied to automatic task-result memories.
    let taskResultVerifyDays: Int?
    /// TTL applied to other memories when they are explicitly verified.
    let defaultVerifyDays: Int?
    /// Replace a same-key memory instead of keeping an older active claim.
    let autoSupersede: Bool?
    /// Character budget for the memory brief injected into a run.
    let briefMaxChars: Int?
    /// Opt-in daemon sweep that marks expired memories stale.
    let sweepStale: Bool?
    /// Optional semantic lane. Absent means lexical-only with zero embedding
    /// network calls.
    let embedding: MemoryEmbeddingPolicy?

    enum CodingKeys: String, CodingKey {
        case enabled
        case maxItems = "max_items"
        case maxAgeDays = "max_age_days"
        case taskResultVerifyDays = "task_result_verify_days"
        case defaultVerifyDays = "default_verify_days"
        case autoSupersede = "auto_supersede"
        case briefMaxChars = "brief_max_chars"
        case sweepStale = "sweep_stale"
        case embedding
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

struct MemoryEmbeddingUsage: Codable, Sendable {
    let calls: Int
    let inputTokens: Int
    let estimatedTokens: Int
    let bytes: Int
    let providerReportedCalls: Int

    enum CodingKeys: String, CodingKey {
        case calls
        case inputTokens = "input_tokens"
        case estimatedTokens = "estimated_tokens"
        case bytes
        case providerReportedCalls = "provider_reported_calls"
    }

    init(
        calls: Int = 0,
        inputTokens: Int = 0,
        estimatedTokens: Int = 0,
        bytes: Int = 0,
        providerReportedCalls: Int = 0
    ) {
        self.calls = calls
        self.inputTokens = inputTokens
        self.estimatedTokens = estimatedTokens
        self.bytes = bytes
        self.providerReportedCalls = providerReportedCalls
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        calls = try container.decodeIfPresent(Int.self, forKey: .calls) ?? 0
        inputTokens = try container.decodeIfPresent(Int.self, forKey: .inputTokens) ?? 0
        estimatedTokens = try container.decodeIfPresent(Int.self, forKey: .estimatedTokens) ?? 0
        bytes = try container.decodeIfPresent(Int.self, forKey: .bytes) ?? 0
        providerReportedCalls = try container.decodeIfPresent(
            Int.self,
            forKey: .providerReportedCalls
        ) ?? 0
    }
}

struct MemoryEmbeddingJobStatus: Codable, Identifiable, Hashable, Sendable {
    let memoryId: String
    let title: String
    let status: String
    let attempts: Int
    let lastError: String?
    let nextAttemptAt: String?
    let updatedAt: String

    var id: String {
        memoryId
    }

    enum CodingKeys: String, CodingKey {
        case memoryId = "memory_id"
        case title
        case status
        case attempts
        case lastError = "last_error"
        case nextAttemptAt = "next_attempt_at"
        case updatedAt = "updated_at"
    }
}

/// Derived IVF index health. Aggregate counts only: the daemon never sends
/// centroids, vectors, or embedded text.
struct MemoryEmbeddingIndexStatus: Codable, Sendable, Equatable {
    let mode: String
    let ready: Bool
    let generation: UInt64?
    let model: String?
    let dimensions: Int
    let vectorCount: Int
    let assignedVectors: Int
    let invalidAssignments: Int
    let clusterCount: Int
    let nlist: Int
    let nprobe: Int
    let coverage: Double
    let minVectors: Int
    let rebuildVectors: Int
    let bytes: Int
    let lastRebuiltAt: String?
    let lastError: String?
    let fallbackCount: Int
    let lastFallbackAt: String?
    let lastFallbackReason: String?
    let fallbackReason: String?

    enum CodingKeys: String, CodingKey {
        case mode
        case ready
        case generation
        case model
        case dimensions
        case vectorCount = "vector_count"
        case assignedVectors = "assigned_vectors"
        case invalidAssignments = "invalid_assignments"
        case clusterCount = "cluster_count"
        case nlist
        case nprobe
        case coverage
        case minVectors = "min_vectors"
        case rebuildVectors = "rebuild_vectors"
        case bytes
        case lastRebuiltAt = "last_rebuilt_at"
        case lastError = "last_error"
        case fallbackCount = "fallback_count"
        case lastFallbackAt = "last_fallback_at"
        case lastFallbackReason = "last_fallback_reason"
        case fallbackReason = "fallback_reason"
    }

    init(
        mode: String,
        ready: Bool,
        generation: UInt64?,
        model: String?,
        dimensions: Int,
        vectorCount: Int,
        assignedVectors: Int,
        invalidAssignments: Int,
        clusterCount: Int,
        nlist: Int,
        nprobe: Int,
        coverage: Double,
        minVectors: Int,
        rebuildVectors: Int,
        bytes: Int,
        lastRebuiltAt: String?,
        lastError: String?,
        fallbackCount: Int,
        lastFallbackAt: String?,
        lastFallbackReason: String?,
        fallbackReason: String?
    ) {
        self.mode = mode
        self.ready = ready
        self.generation = generation
        self.model = model
        self.dimensions = dimensions
        self.vectorCount = vectorCount
        self.assignedVectors = assignedVectors
        self.invalidAssignments = invalidAssignments
        self.clusterCount = clusterCount
        self.nlist = nlist
        self.nprobe = nprobe
        self.coverage = coverage
        self.minVectors = minVectors
        self.rebuildVectors = rebuildVectors
        self.bytes = bytes
        self.lastRebuiltAt = lastRebuiltAt
        self.lastError = lastError
        self.fallbackCount = fallbackCount
        self.lastFallbackAt = lastFallbackAt
        self.lastFallbackReason = lastFallbackReason
        self.fallbackReason = fallbackReason
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        mode = try container.decodeIfPresent(String.self, forKey: .mode) ?? "scan"
        ready = try container.decodeIfPresent(Bool.self, forKey: .ready) ?? false
        generation = try container.decodeIfPresent(UInt64.self, forKey: .generation)
        model = try container.decodeIfPresent(String.self, forKey: .model)
        dimensions = try container.decodeIfPresent(Int.self, forKey: .dimensions) ?? 1536
        vectorCount = try container.decodeIfPresent(Int.self, forKey: .vectorCount) ?? 0
        assignedVectors = try container.decodeIfPresent(Int.self, forKey: .assignedVectors) ?? 0
        invalidAssignments = try container.decodeIfPresent(Int.self, forKey: .invalidAssignments) ?? 0
        clusterCount = try container.decodeIfPresent(Int.self, forKey: .clusterCount) ?? 0
        nlist = try container.decodeIfPresent(Int.self, forKey: .nlist) ?? 64
        nprobe = try container.decodeIfPresent(Int.self, forKey: .nprobe) ?? 8
        coverage = try container.decodeIfPresent(Double.self, forKey: .coverage) ?? 0
        minVectors = try container.decodeIfPresent(Int.self, forKey: .minVectors) ?? 256
        rebuildVectors = try container.decodeIfPresent(Int.self, forKey: .rebuildVectors) ?? 4096
        bytes = try container.decodeIfPresent(Int.self, forKey: .bytes) ?? 0
        lastRebuiltAt = try container.decodeIfPresent(String.self, forKey: .lastRebuiltAt)
        lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
        fallbackCount = try container.decodeIfPresent(Int.self, forKey: .fallbackCount) ?? 0
        lastFallbackAt = try container.decodeIfPresent(String.self, forKey: .lastFallbackAt)
        lastFallbackReason = try container.decodeIfPresent(String.self, forKey: .lastFallbackReason)
        fallbackReason = try container.decodeIfPresent(String.self, forKey: .fallbackReason)
    }

    var modeLabel: String {
        mode == "ivf" ? "IVF" : "exact scan"
    }

    var coveragePercent: Double {
        min(max(coverage, 0), 1) * 100
    }

    var coverageLabel: String {
        vectorCount > 0 ? String(format: "%.1f%%", coveragePercent) : "no vectors"
    }

    var generationLabel: String {
        generation.map { "generation \($0)" } ?? "not built"
    }

    var statusLabel: String {
        if ready {
            return "ready"
        }
        if let fallbackReason {
            return fallbackReason
        }
        return "not ready"
    }

    /// Rebuild is meaningful only for an enabled IVF policy. Scan mode and an
    /// absent/disabled lane intentionally expose no action.
    var canRebuild: Bool {
        mode == "ivf"
    }

    var byteLabel: String {
        ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }
}

struct MemoryEmbeddingsStatusResult: Codable, Sendable {
    let projectId: String
    let enabled: Bool
    let configured: Bool
    let model: String?
    let dimensions: Int
    let indexable: Int
    let indexed: Int
    let invalid: Int
    let pending: Int
    let failed: Int
    let skipped: Int
    let coverage: Double
    let lastError: String?
    let usage: MemoryEmbeddingUsage
    let recentJobs: [MemoryEmbeddingJobStatus]
    let index: MemoryEmbeddingIndexStatus?

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case enabled
        case configured
        case model
        case dimensions
        case indexable
        case indexed
        case invalid
        case pending
        case failed
        case skipped
        case coverage
        case lastError = "last_error"
        case usage
        case recentJobs = "recent_jobs"
        case index
    }

    init(
        projectId: String,
        enabled: Bool,
        configured: Bool,
        model: String?,
        dimensions: Int,
        indexable: Int,
        indexed: Int,
        invalid: Int = 0,
        pending: Int,
        failed: Int,
        skipped: Int,
        coverage: Double,
        lastError: String?,
        usage: MemoryEmbeddingUsage,
        recentJobs: [MemoryEmbeddingJobStatus],
        index: MemoryEmbeddingIndexStatus? = nil
    ) {
        self.projectId = projectId
        self.enabled = enabled
        self.configured = configured
        self.model = model
        self.dimensions = dimensions
        self.indexable = indexable
        self.indexed = indexed
        self.invalid = invalid
        self.pending = pending
        self.failed = failed
        self.skipped = skipped
        self.coverage = coverage
        self.lastError = lastError
        self.usage = usage
        self.recentJobs = recentJobs
        self.index = index
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decode(String.self, forKey: .projectId)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        configured = try container.decodeIfPresent(Bool.self, forKey: .configured) ?? false
        model = try container.decodeIfPresent(String.self, forKey: .model)
        dimensions = try container.decodeIfPresent(Int.self, forKey: .dimensions) ?? 1536
        indexable = try container.decodeIfPresent(Int.self, forKey: .indexable) ?? 0
        indexed = try container.decodeIfPresent(Int.self, forKey: .indexed) ?? 0
        invalid = try container.decodeIfPresent(Int.self, forKey: .invalid) ?? 0
        pending = try container.decodeIfPresent(Int.self, forKey: .pending) ?? 0
        failed = try container.decodeIfPresent(Int.self, forKey: .failed) ?? 0
        skipped = try container.decodeIfPresent(Int.self, forKey: .skipped) ?? 0
        coverage = try container.decodeIfPresent(Double.self, forKey: .coverage) ?? 0
        lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
        usage = try container.decodeIfPresent(MemoryEmbeddingUsage.self, forKey: .usage)
            ?? MemoryEmbeddingUsage()
        recentJobs = try container.decodeIfPresent(
            [MemoryEmbeddingJobStatus].self,
            forKey: .recentJobs
        ) ?? []
        index = try container.decodeIfPresent(
            MemoryEmbeddingIndexStatus.self,
            forKey: .index
        )
    }

    var coveragePercent: Double {
        min(max(coverage, 0), 1) * 100
    }

    var coverageLabel: String {
        guard indexable > 0 else { return "no memories" }
        return String(format: "%.1f%%", coveragePercent)
    }

    var healthLabel: String {
        if !configured {
            return "not configured"
        }
        if !enabled {
            return "disabled"
        }
        if failed > 0 || invalid > 0 {
            return "degraded"
        }
        if pending > 0 {
            return "indexing"
        }
        return "healthy"
    }
}

struct MemoryEmbeddingsRebuildResult: Codable, Sendable {
    let projectId: String
    let enabled: Bool
    let model: String?
    let dimensions: Int
    let cleared: Int
    let enqueued: Int

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case enabled
        case model
        case dimensions
        case cleared
        case enqueued
    }
}

struct MemoryEmbeddingsIndexStatusResult: Codable, Sendable {
    let projectId: String
    let index: MemoryEmbeddingIndexStatus

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case index
    }
}

struct MemoryEmbeddingsIndexRebuildResult: Codable, Sendable {
    let projectId: String
    let rebuilt: Bool
    let reason: String?
    let generation: UInt64?
    let vectorCount: Int
    let assignedVectors: Int
    let invalidAssignments: Int
    let clusterCount: Int
    let nlist: Int

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case rebuilt
        case reason
        case generation
        case vectorCount = "vector_count"
        case assignedVectors = "assigned_vectors"
        case invalidAssignments = "invalid_assignments"
        case clusterCount = "cluster_count"
        case nlist
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decode(String.self, forKey: .projectId)
        rebuilt = try container.decodeIfPresent(Bool.self, forKey: .rebuilt) ?? false
        reason = try container.decodeIfPresent(String.self, forKey: .reason)
        generation = try container.decodeIfPresent(UInt64.self, forKey: .generation)
        vectorCount = try container.decodeIfPresent(Int.self, forKey: .vectorCount) ?? 0
        assignedVectors = try container.decodeIfPresent(Int.self, forKey: .assignedVectors) ?? 0
        invalidAssignments = try container.decodeIfPresent(Int.self, forKey: .invalidAssignments) ?? 0
        clusterCount = try container.decodeIfPresent(Int.self, forKey: .clusterCount) ?? 0
        nlist = try container.decodeIfPresent(Int.self, forKey: .nlist) ?? 0
    }

    var summaryLabel: String {
        if rebuilt {
            let generationLabel = generation.map(String.init) ?? "?"
            return "rebuilt generation \(generationLabel) · \(assignedVectors)/\(vectorCount) assigned"
        }
        return reason ?? "not rebuilt"
    }
}

struct MemoryEmbeddingsTestResult: Codable, Sendable {
    let projectId: String
    let ok: Bool
    let model: String?
    let dimensions: Int
    let latencyMs: Int
    let inputTokens: Int
    let estimatedTokens: Int
    let providerReported: Bool
    let error: String?

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case ok
        case model
        case dimensions
        case latencyMs = "latency_ms"
        case inputTokens = "input_tokens"
        case estimatedTokens = "estimated_tokens"
        case providerReported = "provider_reported"
        case error
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

struct MemoryEvidence: Codable, Hashable, Sendable {
    let kind: String
    let reference: String
    let note: String?

    var label: String {
        switch kind {
        case "commit": "commit \(reference.prefix(8))"
        case "path": "path \(reference)"
        case "task": "task \(reference)"
        case "session": "session \(reference.prefix(8))"
        case "event": "event \(reference)"
        case "url": reference
        default: "\(kind) \(reference)"
        }
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
    let status: String?
    let source: String?
    let evidence: [MemoryEvidence]?
    let supersedes: [String]?
    let supersededBy: String?
    let pinned: Bool?
    let useCount: Int?
    let lastUsedAt: String?
    let lastVerifiedAt: String?
    let verifyAfter: String?
    let staleReason: String?

    var displayStatus: String {
        status ?? "active"
    }

    var displaySource: String {
        source ?? "agent"
    }

    var isPinned: Bool {
        pinned ?? false
    }

    var evidenceCount: Int {
        evidence?.count ?? 0
    }

    /// Compact provenance for the memory row. The full evidence list remains
    /// available in the model but is too noisy for a dense dashboard.
    var evidenceLabelParts: [String] {
        Array((evidence ?? []).prefix(3)).map(\.label)
    }

    var isActive: Bool {
        displayStatus == "active"
    }

    var statusDetail: String? {
        if let staleReason, !staleReason.isEmpty {
            return staleReason
        }
        if let verifyAfter {
            return "verify after \(verifyAfter)"
        }
        return nil
    }

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
        case status
        case source
        case evidence
        case supersedes
        case supersededBy = "superseded_by"
        case pinned
        case useCount = "use_count"
        case lastUsedAt = "last_used_at"
        case lastVerifiedAt = "last_verified_at"
        case verifyAfter = "verify_after"
        case staleReason = "stale_reason"
    }
}

struct MemoryUpdateResult: Codable, Sendable {
    let memory: MemoryItem
}

struct MemoryVerificationCheck: Codable, Hashable, Sendable {
    let evidence: MemoryEvidence
    let status: String
    let detail: String
}

struct MemoryVerifyResult: Codable, Sendable {
    let memory: MemoryItem
    let checks: [MemoryVerificationCheck]
}

struct MemoryStatCount: Codable, Hashable, Sendable {
    let key: String
    let count: Int
}

struct MemoryStatsResult: Codable, Sendable {
    let projectId: String
    let total: Int
    let byStatus: [MemoryStatCount]
    let bySource: [MemoryStatCount]
    let byKind: [MemoryStatCount]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case total
        case byStatus = "by_status"
        case bySource = "by_source"
        case byKind = "by_kind"
    }
}

struct MemoryConflict: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let sessionId: String
    let memoryAId: String
    let memoryBId: String
    let memoryATitle: String
    let memoryBTitle: String
    let memoryAKind: String
    let memoryBKind: String
    let memoryAExcerpt: String
    let memoryBExcerpt: String
    let scopeKind: String
    let scopeValue: String
    let reason: String
    let status: String
    let detectedAt: String
    let resolvedAt: String?
    let resolution: String?
    let note: String?

    var isOpen: Bool {
        status == "open"
    }

    var scopeLabel: String {
        "\(scopeKind) \(scopeValue)"
    }

    var initial: String {
        String(id.prefix(8))
    }

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case sessionId = "session_id"
        case memoryAId = "memory_a_id"
        case memoryBId = "memory_b_id"
        case memoryATitle = "memory_a_title"
        case memoryBTitle = "memory_b_title"
        case memoryAKind = "memory_a_kind"
        case memoryBKind = "memory_b_kind"
        case memoryAExcerpt = "memory_a_excerpt"
        case memoryBExcerpt = "memory_b_excerpt"
        case scopeKind = "scope_kind"
        case scopeValue = "scope_value"
        case reason
        case status
        case detectedAt = "detected_at"
        case resolvedAt = "resolved_at"
        case resolution
        case note
    }
}

struct MemoryConflictsResult: Codable, Sendable {
    let projectId: String
    let conflicts: [MemoryConflict]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case conflicts
    }
}

struct MemoryConflictScanResult: Codable, Sendable {
    let projectId: String
    let detected: Int
    let open: Int
    let conflicts: [MemoryConflict]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case detected
        case open
        case conflicts
    }
}

struct MemoryConflictResolveResult: Codable, Sendable {
    let conflict: MemoryConflict
    let memories: [MemoryItem]
}

struct MemoryHealthResult: Codable, Sendable {
    let projectId: String
    let total: Int
    let active: Int
    let stale: Int
    let superseded: Int
    let archived: Int
    let pinned: Int
    let evidenceCovered: Int
    let verified: Int
    let verificationDue: Int
    let openConflicts: Int
    let byKind: [MemoryStatCount]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case total
        case active
        case stale
        case superseded
        case archived
        case pinned
        case evidenceCovered = "evidence_covered"
        case verified
        case verificationDue = "verification_due"
        case openConflicts = "open_conflicts"
        case byKind = "by_kind"
    }
}

/// Explicit operator resolution actions. Detection never applies one of these;
/// the user must choose. Keeping the RPC spelling here prevents accidental
/// drift between the UI and the daemon's validated action strings.
enum MemoryConflictResolution: String, CaseIterable, Identifiable, Sendable {
    case keepA = "keep_a"
    case keepB = "keep_b"
    case keepBoth = "keep_both"
    case archiveBoth = "archive_both"

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .keepA: "Keep A"
        case .keepB: "Keep B"
        case .keepBoth: "Keep Both"
        case .archiveBoth: "Archive Both"
        }
    }

    var detail: String {
        switch self {
        case .keepA: "Supersede B and keep A active."
        case .keepB: "Supersede A and keep B active."
        case .keepBoth: "Resolve the review row without changing either memory."
        case .archiveBoth: "Archive both memories. They stop appearing as active claims."
        }
    }

    var isDestructive: Bool {
        self != .keepBoth
    }
}

struct MemorySweepResult: Codable, Sendable {
    let projectId: String
    let candidates: Int
    let markedStale: Int
    let memoryIds: [String]
    let dryRun: Bool

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case candidates
        case markedStale = "marked_stale"
        case memoryIds = "memory_ids"
        case dryRun = "dry_run"
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
    let cacheReadTokens: Int?
    let cacheWriteTokens: Int?
    let estimated: Bool
    let costUsd: Double
    let createdAt: String

    var totalTokens: Int {
        inputTokens + outputTokens
    }

    /// Cached input as a fraction of total provider input. `inputTokens`
    /// already includes cache reads and writes.
    var cacheHitRate: Double? {
        guard let cacheReadTokens, inputTokens > 0 else {
            return nil
        }
        return Double(cacheReadTokens) / Double(inputTokens)
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
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
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
    let cacheReadTokens: Int?
    let cacheWriteTokens: Int?
    let costUsd: Double

    var id: String {
        key
    }

    enum CodingKeys: String, CodingKey {
        case key
        case calls
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
        case costUsd = "cost_usd"
    }
}

struct UsageTotals: Codable, Sendable {
    let calls: Int
    let inputTokens: Int
    let outputTokens: Int
    let cacheReadTokens: Int?
    let cacheWriteTokens: Int?
    /// Daemon-computed cache read share. Optional so an older daemon that only
    /// reports token counters still decodes.
    let cacheHitRatio: Double?
    let costUsd: Double
    let byRole: [UsageBucket]
    let byModel: [UsageBucket]
    let byAgent: [UsageBucket]
    let byTask: [UsageBucket]
    let byDepartment: [UsageBucket]?

    /// Legacy read share used by older surfaces. `inputTokens` already includes
    /// cache reads and writes under ADR-0138, so this remains a read/total ratio.
    var cacheHitRate: Double? {
        guard let cacheReadTokens, inputTokens > 0 else {
            return nil
        }
        return Double(cacheReadTokens) / Double(inputTokens)
    }

    enum CodingKeys: String, CodingKey {
        case calls
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
        case cacheHitRatio = "cache_hit_ratio"
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
    let unlimited: Bool
    let costSpentUsd: Double
    let baseCommit: String?
    let tokensIn: Int
    let tokensOut: Int
    /// Persisted prompt layout: 2 is the legacy combined prompt, 3 is the
    /// content-addressed instruction store. Optional for older daemons.
    let promptLayout: Int?
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
        case unlimited
        case costSpentUsd = "cost_spent_usd"
        case baseCommit = "base_commit"
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case promptLayout = "prompt_layout"
        case createdAt = "created_at"
    }

    init(
        id: String,
        projectId: String,
        goal: String,
        status: String,
        priority: Int?,
        worktree: String?,
        branch: String?,
        tokenBudget: Int?,
        costBudgetUsd: Double?,
        timeBudgetSecs: Int?,
        unlimited: Bool = false,
        costSpentUsd: Double,
        baseCommit: String?,
        tokensIn: Int,
        tokensOut: Int,
        promptLayout: Int? = nil,
        createdAt: String
    ) {
        self.id = id
        self.projectId = projectId
        self.goal = goal
        self.status = status
        self.priority = priority
        self.worktree = worktree
        self.branch = branch
        self.tokenBudget = tokenBudget
        self.costBudgetUsd = costBudgetUsd
        self.timeBudgetSecs = timeBudgetSecs
        self.unlimited = unlimited
        self.costSpentUsd = costSpentUsd
        self.baseCommit = baseCommit
        self.tokensIn = tokensIn
        self.tokensOut = tokensOut
        self.promptLayout = promptLayout
        self.createdAt = createdAt
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectId = try container.decode(String.self, forKey: .projectId)
        goal = try container.decode(String.self, forKey: .goal)
        status = try container.decode(String.self, forKey: .status)
        priority = try container.decodeIfPresent(Int.self, forKey: .priority)
        worktree = try container.decodeIfPresent(String.self, forKey: .worktree)
        branch = try container.decodeIfPresent(String.self, forKey: .branch)
        tokenBudget = try container.decodeIfPresent(Int.self, forKey: .tokenBudget)
        costBudgetUsd = try container.decodeIfPresent(Double.self, forKey: .costBudgetUsd)
        timeBudgetSecs = try container.decodeIfPresent(Int.self, forKey: .timeBudgetSecs)
        unlimited = try container.decodeIfPresent(Bool.self, forKey: .unlimited) ?? false
        costSpentUsd = try container.decodeIfPresent(Double.self, forKey: .costSpentUsd) ?? 0
        baseCommit = try container.decodeIfPresent(String.self, forKey: .baseCommit)
        tokensIn = try container.decodeIfPresent(Int.self, forKey: .tokensIn) ?? 0
        tokensOut = try container.decodeIfPresent(Int.self, forKey: .tokensOut) ?? 0
        promptLayout = try container.decodeIfPresent(Int.self, forKey: .promptLayout)
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
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
    let data: JSONValue?
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
    let dataBase64: String?
    let version: Int
    let provenance: ArtifactProvenance?
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
        case dataBase64 = "data_base64"
        case version
        case provenance
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        agentId = try container.decode(String.self, forKey: .agentId)
        agentName = try container.decode(String.self, forKey: .agentName)
        taskId = try container.decodeIfPresent(String.self, forKey: .taskId)
        name = try container.decode(String.self, forKey: .name)
        kind = try container.decode(String.self, forKey: .kind)
        mimeType = try container.decode(String.self, forKey: .mimeType)
        content = try container.decodeIfPresent(String.self, forKey: .content) ?? ""
        storage = try container.decodeIfPresent(String.self, forKey: .storage) ?? "inline"
        compression = try container.decodeIfPresent(String.self, forKey: .compression)
        sizeBytes = try container.decodeIfPresent(Int.self, forKey: .sizeBytes) ?? 0
        sha256 = try container.decodeIfPresent(String.self, forKey: .sha256) ?? ""
        path = try container.decodeIfPresent(String.self, forKey: .path)
        dataBase64 = try container.decodeIfPresent(String.self, forKey: .dataBase64)
        version = try container.decodeIfPresent(Int.self, forKey: .version) ?? 1
        provenance = try container.decodeIfPresent(ArtifactProvenance.self, forKey: .provenance)
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

struct ArtifactVersion: Codable, Identifiable, Hashable, Sendable {
    let artifactId: String
    let version: Int
    let name: String
    let kind: String
    let mimeType: String
    let content: String
    let storage: String
    let compression: String
    let sizeBytes: Int
    let sha256: String
    let path: String?
    let provenance: ArtifactProvenance?
    let createdAt: String

    var id: Int {
        version
    }

    enum CodingKeys: String, CodingKey {
        case artifactId = "artifact_id"
        case version
        case name
        case kind
        case mimeType = "mime_type"
        case content
        case storage
        case compression
        case sizeBytes = "size_bytes"
        case sha256
        case path
        case provenance
        case createdAt = "created_at"
    }
}

struct ArtifactVersionsResult: Codable, Sendable {
    let versions: [ArtifactVersion]
}

struct MailboxListResult: Codable, Sendable {
    let messages: [AgentMessage]
}

struct ArtifactListResult: Codable, Sendable {
    let artifacts: [Artifact]
}

struct ArtifactResult: Codable, Sendable {
    let artifact: Artifact
    let provenance: ArtifactProvenance?
}

struct ArtifactProvenance: Codable, Hashable, Sendable {
    let operation: String
    let sourceArtifactId: String?
    let sourceArtifactName: String?
    let sourceSha256: String?
    let sourceMimeType: String?
    let entryPath: String?
    let archiveFormat: String?
    let extractionId: String?
    let entrySha256: String?
    let sourceFormat: String?
    let outputFormat: String?
    let tool: String?
    let maxDurationSecs: Int?
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case operation
        case sourceArtifactId = "source_artifact_id"
        case sourceArtifactName = "source_artifact_name"
        case sourceSha256 = "source_sha256"
        case sourceMimeType = "source_mime_type"
        case entryPath = "entry_path"
        case archiveFormat = "archive_format"
        case extractionId = "extraction_id"
        case entrySha256 = "entry_sha256"
        case sourceFormat = "source_format"
        case outputFormat = "output_format"
        case tool
        case maxDurationSecs = "max_duration_secs"
        case truncated
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        operation = try container.decodeIfPresent(String.self, forKey: .operation) ?? ""
        sourceArtifactId = try container.decodeIfPresent(String.self, forKey: .sourceArtifactId)
        sourceArtifactName = try container.decodeIfPresent(String.self, forKey: .sourceArtifactName)
        sourceSha256 = try container.decodeIfPresent(String.self, forKey: .sourceSha256)
        sourceMimeType = try container.decodeIfPresent(String.self, forKey: .sourceMimeType)
        entryPath = try container.decodeIfPresent(String.self, forKey: .entryPath)
        archiveFormat = try container.decodeIfPresent(String.self, forKey: .archiveFormat)
        extractionId = try container.decodeIfPresent(String.self, forKey: .extractionId)
        entrySha256 = try container.decodeIfPresent(String.self, forKey: .entrySha256)
        sourceFormat = try container.decodeIfPresent(String.self, forKey: .sourceFormat)
        outputFormat = try container.decodeIfPresent(String.self, forKey: .outputFormat)
        tool = try container.decodeIfPresent(String.self, forKey: .tool)
        maxDurationSecs = try container.decodeIfPresent(Int.self, forKey: .maxDurationSecs)
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }
}

struct ArchiveLimits: Codable, Hashable, Sendable {
    let maxEntries: Int
    let maxEntryBytes: Int
    let maxTotalBytes: Int
    let maxArchiveBytes: Int
    let maxCompressionRatio: Int

    enum CodingKeys: String, CodingKey {
        case maxEntries = "max_entries"
        case maxEntryBytes = "max_entry_bytes"
        case maxTotalBytes = "max_total_bytes"
        case maxArchiveBytes = "max_archive_bytes"
        case maxCompressionRatio = "max_compression_ratio"
    }
}

struct ArchiveEntry: Codable, Identifiable, Hashable, Sendable {
    let index: Int
    let path: String
    let kind: String
    let sizeBytes: Int
    let compressedSize: Int?
    let mode: Int?
    let safe: Bool
    let unsafeReason: String?
    let duplicate: Bool
    let modifiedAt: String?

    var id: String {
        "\(index):\(path)"
    }

    enum CodingKeys: String, CodingKey {
        case index
        case path
        case kind
        case sizeBytes = "size_bytes"
        case compressedSize = "compressed_size"
        case mode
        case safe
        case unsafeReason = "unsafe_reason"
        case duplicate
        case modifiedAt = "modified_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        index = try container.decodeIfPresent(Int.self, forKey: .index) ?? 0
        path = try container.decode(String.self, forKey: .path)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? "other"
        sizeBytes = try container.decodeIfPresent(Int.self, forKey: .sizeBytes) ?? 0
        compressedSize = try container.decodeIfPresent(Int.self, forKey: .compressedSize)
        mode = try container.decodeIfPresent(Int.self, forKey: .mode)
        safe = try container.decodeIfPresent(Bool.self, forKey: .safe) ?? false
        unsafeReason = try container.decodeIfPresent(String.self, forKey: .unsafeReason)
        duplicate = try container.decodeIfPresent(Bool.self, forKey: .duplicate) ?? false
        modifiedAt = try container.decodeIfPresent(String.self, forKey: .modifiedAt)
    }
}

struct ArtifactArchiveListResult: Codable, Sendable {
    let artifactId: String
    let name: String
    let mimeType: String
    let sizeBytes: Int
    let sha256: String
    let format: String
    let entries: [ArchiveEntry]
    let entryCount: Int
    let truncated: Bool
    let totalUncompressedBytes: Int
    let totalCompressedBytes: Int
    let limits: ArchiveLimits
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case artifactId = "artifact_id"
        case name
        case mimeType = "mime_type"
        case sizeBytes = "size_bytes"
        case sha256
        case format
        case entries
        case entryCount = "entry_count"
        case truncated
        case totalUncompressedBytes = "total_uncompressed_bytes"
        case totalCompressedBytes = "total_compressed_bytes"
        case limits
        case warnings
    }

    var safeFileCount: Int {
        entries.filter { $0.safe && $0.kind == "file" }.count
    }
}

struct ExtractedArchiveEntry: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let kind: String
    let sizeBytes: Int
    let sha256: String?
    let publishedArtifactId: String?
    let preview: String?
    let previewEncoding: String
    let truncated: Bool
    let skippedReason: String?

    var id: String {
        path
    }

    enum CodingKeys: String, CodingKey {
        case path
        case kind
        case sizeBytes = "size_bytes"
        case sha256
        case publishedArtifactId = "published_artifact_id"
        case preview
        case previewEncoding = "preview_encoding"
        case truncated
        case skippedReason = "skipped_reason"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        path = try container.decode(String.self, forKey: .path)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? "file"
        sizeBytes = try container.decodeIfPresent(Int.self, forKey: .sizeBytes) ?? 0
        sha256 = try container.decodeIfPresent(String.self, forKey: .sha256)
        publishedArtifactId = try container.decodeIfPresent(String.self, forKey: .publishedArtifactId)
        preview = try container.decodeIfPresent(String.self, forKey: .preview)
        previewEncoding = try container.decodeIfPresent(String.self, forKey: .previewEncoding) ?? "none"
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        skippedReason = try container.decodeIfPresent(String.self, forKey: .skippedReason)
    }
}

struct ArtifactArchiveExtractResult: Codable, Sendable {
    let artifactId: String
    let extractionId: String
    let format: String
    let entries: [ExtractedArchiveEntry]
    let publishedArtifacts: [Artifact]
    let totalBytes: Int
    let scratchRetained: Bool
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case artifactId = "artifact_id"
        case extractionId = "extraction_id"
        case format
        case entries
        case publishedArtifacts = "published_artifacts"
        case totalBytes = "total_bytes"
        case scratchRetained = "scratch_retained"
        case warnings
    }
}

struct MediaStreamInfo: Codable, Hashable, Sendable {
    let index: Int
    let codecType: String
    let codecName: String
    let codecLongName: String?
    let width: Int?
    let height: Int?
    let durationSecs: Double?
    let bitRate: Int?
    let sampleRate: Int?
    let channels: Int?
    let language: String?
    let frames: Int?

    enum CodingKeys: String, CodingKey {
        case index
        case codecType = "codec_type"
        case codecName = "codec_name"
        case codecLongName = "codec_long_name"
        case width
        case height
        case durationSecs = "duration_secs"
        case bitRate = "bit_rate"
        case sampleRate = "sample_rate"
        case channels
        case language
        case frames
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        index = try container.decodeIfPresent(Int.self, forKey: .index) ?? 0
        codecType = try container.decodeIfPresent(String.self, forKey: .codecType) ?? "unknown"
        codecName = try container.decodeIfPresent(String.self, forKey: .codecName) ?? ""
        codecLongName = try container.decodeIfPresent(String.self, forKey: .codecLongName)
        width = try container.decodeIfPresent(Int.self, forKey: .width)
        height = try container.decodeIfPresent(Int.self, forKey: .height)
        durationSecs = try container.decodeIfPresent(Double.self, forKey: .durationSecs)
        bitRate = try container.decodeIfPresent(Int.self, forKey: .bitRate)
        sampleRate = try container.decodeIfPresent(Int.self, forKey: .sampleRate)
        channels = try container.decodeIfPresent(Int.self, forKey: .channels)
        language = try container.decodeIfPresent(String.self, forKey: .language)
        frames = try container.decodeIfPresent(Int.self, forKey: .frames)
    }
}

struct MediaInfo: Codable, Hashable, Sendable {
    let container: String?
    let formatLongName: String?
    let durationSecs: Double?
    let bitRate: Int?
    let width: Int?
    let height: Int?
    let streams: [MediaStreamInfo]
    let tags: [String: String]
    let source: String
    let tool: String?
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case container
        case formatLongName = "format_long_name"
        case durationSecs = "duration_secs"
        case bitRate = "bit_rate"
        case width
        case height
        case streams
        case tags
        case source
        case tool
        case warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.container = try container.decodeIfPresent(String.self, forKey: .container)
        formatLongName = try container.decodeIfPresent(String.self, forKey: .formatLongName)
        durationSecs = try container.decodeIfPresent(Double.self, forKey: .durationSecs)
        bitRate = try container.decodeIfPresent(Int.self, forKey: .bitRate)
        width = try container.decodeIfPresent(Int.self, forKey: .width)
        height = try container.decodeIfPresent(Int.self, forKey: .height)
        streams = try container.decodeIfPresent([MediaStreamInfo].self, forKey: .streams) ?? []
        tags = try container.decodeIfPresent([String: String].self, forKey: .tags) ?? [:]
        source = try container.decodeIfPresent(String.self, forKey: .source) ?? "unknown"
        tool = try container.decodeIfPresent(String.self, forKey: .tool)
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}

struct MediaInfoResult: Codable, Sendable {
    let artifactId: String
    let name: String
    let mimeType: String
    let sizeBytes: Int
    let sha256: String
    let info: MediaInfo

    enum CodingKeys: String, CodingKey {
        case artifactId = "artifact_id"
        case name
        case mimeType = "mime_type"
        case sizeBytes = "size_bytes"
        case sha256
        case info
    }
}

struct MediaTranscodeResult: Codable, Sendable {
    let sourceArtifactId: String
    let artifact: Artifact
    let format: String
    let durationSecs: Double?
    let sizeBytes: Int
    let elapsedMs: Int
    let commandSummary: String
    let maxDurationSecs: Int
    let maxBytes: Int
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case sourceArtifactId = "source_artifact_id"
        case artifact
        case format
        case durationSecs = "duration_secs"
        case sizeBytes = "size_bytes"
        case elapsedMs = "elapsed_ms"
        case commandSummary = "command_summary"
        case maxDurationSecs = "max_duration_secs"
        case maxBytes = "max_bytes"
        case warnings
    }
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
    let agentId: String?
    let taskId: String?
    let approvalRisk: String?
    let approvalReason: String?
    let waiterCount: Int
    let templateId: String?
    let priority: Int
    let expiresAt: String?
    let notify: Bool
    let notifyKey: String?
    let dedupKey: String?

    var isDenied: Bool {
        status == "denied"
    }

    var isPending: Bool {
        status == "pending"
    }

    var attentionLabel: String {
        var parts: [String] = []
        if waiterCount > 1 {
            parts.append("\(waiterCount) waiters")
        }
        if priority != 0 {
            parts.append("criticality \(priority)")
        }
        if let templateId, !templateId.isEmpty {
            parts.append("template \(templateId.prefix(8))")
        }
        return parts.joined(separator: " · ")
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
        case agentId = "agent_id"
        case taskId = "task_id"
        case approvalRisk = "approval_risk"
        case approvalReason = "approval_reason"
        case waiterCount = "waiter_count"
        case templateId = "template_id"
        case priority
        case expiresAt = "expires_at"
        case notify
        case notifyKey = "notify_key"
        case dedupKey = "dedup_key"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        toolCallId = try container.decode(String.self, forKey: .toolCallId)
        tool = try container.decode(String.self, forKey: .tool)
        input = try container.decodeIfPresent(JSONValue.self, forKey: .input) ?? .null
        status = try container.decode(String.self, forKey: .status)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        resolvedAt = try container.decodeIfPresent(String.self, forKey: .resolvedAt)
        agentName = try container.decodeIfPresent(String.self, forKey: .agentName)
        agentRole = try container.decodeIfPresent(String.self, forKey: .agentRole)
        agentTitle = try container.decodeIfPresent(String.self, forKey: .agentTitle)
        agentId = try container.decodeIfPresent(String.self, forKey: .agentId)
        taskId = try container.decodeIfPresent(String.self, forKey: .taskId)
        approvalRisk = try container.decodeIfPresent(String.self, forKey: .approvalRisk)
        approvalReason = try container.decodeIfPresent(String.self, forKey: .approvalReason)
        waiterCount = try container.decodeIfPresent(Int.self, forKey: .waiterCount) ?? 0
        templateId = try container.decodeIfPresent(String.self, forKey: .templateId)
        priority = try container.decodeIfPresent(Int.self, forKey: .priority) ?? 0
        expiresAt = try container.decodeIfPresent(String.self, forKey: .expiresAt)
        notify = try container.decodeIfPresent(Bool.self, forKey: .notify) ?? false
        notifyKey = try container.decodeIfPresent(String.self, forKey: .notifyKey)
        dedupKey = try container.decodeIfPresent(String.self, forKey: .dedupKey)
    }
}

struct ApprovalTemplate: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let name: String
    let enabled: Bool
    let toolPattern: String
    let riskLevels: [String]
    let pathGlobs: [String]
    let action: String
    let expiresAt: String?
    let maxUses: Int?
    let usedCount: Int
    let createdBy: String
    let createdAt: String
    let updatedAt: String?

    var remainingUses: Int? {
        maxUses.map { max(0, $0 - usedCount) }
    }

    var isExhausted: Bool {
        remainingUses == 0
    }

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case name
        case enabled
        case toolPattern = "tool_pattern"
        case riskLevels = "risk_levels"
        case pathGlobs = "path_globs"
        case action
        case expiresAt = "expires_at"
        case maxUses = "max_uses"
        case usedCount = "used_count"
        case createdBy = "created_by"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectId = try container.decode(String.self, forKey: .projectId)
        name = try container.decode(String.self, forKey: .name)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        toolPattern = try container.decode(String.self, forKey: .toolPattern)
        riskLevels = try container.decodeIfPresent([String].self, forKey: .riskLevels) ?? []
        pathGlobs = try container.decodeIfPresent([String].self, forKey: .pathGlobs) ?? []
        action = try container.decodeIfPresent(String.self, forKey: .action) ?? "allow"
        expiresAt = try container.decodeIfPresent(String.self, forKey: .expiresAt)
        maxUses = try container.decodeIfPresent(Int.self, forKey: .maxUses)
        usedCount = try container.decodeIfPresent(Int.self, forKey: .usedCount) ?? 0
        createdBy = try container.decode(String.self, forKey: .createdBy)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt)
    }
}

struct ApprovalTemplateListResult: Codable, Sendable {
    let templates: [ApprovalTemplate]
}

struct ApprovalTemplateResult: Codable, Sendable {
    let template: ApprovalTemplate
}

struct ApprovalTemplateDeleteResult: Codable, Sendable {
    let deleted: Bool
}

struct AttentionPolicy: Codable, Hashable, Sendable {
    var enabled: Bool = true
    var maxPendingPerSession: Int = 64
    var coalesceWindowSecs: Int = 120
    var staleAfterSecs: Int = 86400
    var priorityBoostRisk: String = "host_state"
    var notifyCoalesceSecs: Int = 60

    enum CodingKeys: String, CodingKey {
        case enabled
        case maxPendingPerSession = "max_pending_per_session"
        case coalesceWindowSecs = "coalesce_window_secs"
        case staleAfterSecs = "stale_after_secs"
        case priorityBoostRisk = "priority_boost_risk"
        case notifyCoalesceSecs = "notify_coalesce_secs"
    }

    init(
        enabled: Bool = true,
        maxPendingPerSession: Int = 64,
        coalesceWindowSecs: Int = 120,
        staleAfterSecs: Int = 86400,
        priorityBoostRisk: String = "host_state",
        notifyCoalesceSecs: Int = 60
    ) {
        self.enabled = enabled
        self.maxPendingPerSession = maxPendingPerSession
        self.coalesceWindowSecs = coalesceWindowSecs
        self.staleAfterSecs = staleAfterSecs
        self.priorityBoostRisk = priorityBoostRisk
        self.notifyCoalesceSecs = notifyCoalesceSecs
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        maxPendingPerSession = try container.decodeIfPresent(Int.self, forKey: .maxPendingPerSession) ?? 64
        coalesceWindowSecs = try container.decodeIfPresent(Int.self, forKey: .coalesceWindowSecs) ?? 120
        staleAfterSecs = try container.decodeIfPresent(Int.self, forKey: .staleAfterSecs) ?? 86400
        priorityBoostRisk = try container.decodeIfPresent(String.self, forKey: .priorityBoostRisk) ?? "host_state"
        notifyCoalesceSecs = try container.decodeIfPresent(Int.self, forKey: .notifyCoalesceSecs) ?? 60
    }
}

struct ApprovalPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: AttentionPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

extension AttentionPolicy {
    var jsonObject: [String: JSONValue] {
        [
            "enabled": .bool(enabled),
            "max_pending_per_session": .number(Double(maxPendingPerSession)),
            "coalesce_window_secs": .number(Double(coalesceWindowSecs)),
            "stale_after_secs": .number(Double(staleAfterSecs)),
            "priority_boost_risk": .string(priorityBoostRisk),
            "notify_coalesce_secs": .number(Double(notifyCoalesceSecs)),
        ]
    }
}

struct ApprovalQueueResult: Codable, Sendable {
    let approvals: [Approval]
    let policy: AttentionPolicy
    let pendingCount: Int
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case approvals
        case policy
        case pendingCount = "pending_count"
        case generatedAt = "generated_at"
    }
}

struct ApprovalBatchItemResult: Codable, Identifiable, Sendable {
    let approvalId: String
    let resolved: Bool
    let status: String?
    let code: String?
    let message: String?
    let waiterCount: Int

    var id: String {
        approvalId
    }

    enum CodingKeys: String, CodingKey {
        case approvalId = "approval_id"
        case resolved
        case status
        case code
        case message
        case waiterCount = "waiter_count"
    }
}

struct ApprovalBatchResolveResult: Codable, Sendable {
    let results: [ApprovalBatchItemResult]
    let resolvedCount: Int
    let failedCount: Int

    enum CodingKeys: String, CodingKey {
        case results
        case resolvedCount = "resolved_count"
        case failedCount = "failed_count"
    }
}

struct ApprovalListResult: Codable, Sendable {
    let approvals: [Approval]
}

struct RoleModelConfig: Codable, Hashable, Sendable {
    var model: String? = nil
    var inputUsdPerMillion: Double? = nil
    var outputUsdPerMillion: Double? = nil
    var cacheReadUsdPerMillion: Double? = nil
    var cacheWriteUsdPerMillion: Double? = nil
    var contextWindow: Int? = nil

    enum CodingKeys: String, CodingKey {
        case model
        case inputUsdPerMillion = "input_usd_per_million"
        case outputUsdPerMillion = "output_usd_per_million"
        case cacheReadUsdPerMillion = "cache_read_usd_per_million"
        case cacheWriteUsdPerMillion = "cache_write_usd_per_million"
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

    /// Model actually configured by the daemon for the session Leader.
    ///
    /// The client's `providerModel` UserDefaults can be stale after a daemon
    /// restart, a role override, or a remote settings edit. The Stage and
    /// header must show the daemon's configured Leader model instead.
    var leaderModelLabel: String? {
        let leader = roleModels.leader.model?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !leader.isEmpty {
            return leader
        }
        let base = model?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return base.isEmpty ? nil : base
    }

    /// Nil when no provider is configured. This intentionally never falls back
    /// to a local UserDefaults value.
    var configuredModelLabel: String? {
        guard configured else { return nil }
        return leaderModelLabel
    }
}

struct SessionCacheStatus: Codable, Sendable {
    let sessionID: String?
    let inputTokens: Int?
    let uncachedInputTokens: Int?
    let cacheReadTokens: Int?
    let cacheWriteTokens: Int?
    let cacheHitRatio: Double?
    let cacheReadRatio: Double?
    let cacheWriteRatio: Double?
    let estimatedSavingsUSD: Double?
    let cacheCalls: Int?
    let lastCallAt: String?
    let promptPrefixHash: String?
    let cacheGeneration: String?
    let coreHash: String?
    let projectHash: String?
    let toolsHash: String?
    let volatileHash: String?
    let preambleTokensEstimate: Int?
    let lastBreakpoints: Int?
    let lastCacheTTL: String?
    let lastMissReason: String?
    let lastFirstTokenMS: Int?
    let recentCalls: [SessionCacheCall]
    let policy: SessionCachePolicy?
    // L5 additive telemetry. Every field is optional so a pre-L5 daemon and a
    // legacy payload still decode.
    let cacheMissTokens: Int?
    let theoreticalMaxHitRatio: Double?
    let cacheEpoch: String?
    let cacheEpochSource: String?
    let appendOnlyCalls: Int?
    let appendOnlyBreaks: Int?
    let lastAppendOnly: Bool?
    let hitRatioP50: Double?
    let hitRatioP95: Double?
    let theoreticalMaxP50: Double?
    let theoreticalMaxP95: Double?
    let costUSD: Double?
    let cacheEpochTimeline: [SessionCacheEpochBucket]
    let perRole: [SessionCacheRoleBucket]
    let topMissReasons: [SessionCacheCountBucket]
    let crossSessionReuseSessions: Int?
    let crossSessionHitTokens: Int?
    let crossSessionCallsObserved: Int?
    let compactionLostCacheUSD: Double?

    /// `session.cache` may report either total provider input (which already
    /// includes cache reads/writes) or an explicit uncached count. Prefer the
    /// explicit count, otherwise normalize the total before deriving a ratio.
    var resolvedUncachedInputTokens: Int? {
        if let uncachedInputTokens {
            return max(0, uncachedInputTokens)
        }
        guard let inputTokens else { return nil }
        let cacheTotal = max(0, cacheReadTokens ?? 0) + max(0, cacheWriteTokens ?? 0)
        guard cacheTotal <= inputTokens else { return max(0, inputTokens) }
        return max(0, inputTokens - cacheTotal)
    }

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case inputTokens = "input_tokens"
        case uncachedInputTokens = "uncached_input_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
        case cacheHitRatio = "cache_hit_ratio"
        case cacheReadRatio = "cache_read_ratio"
        case cacheWriteRatio = "cache_write_ratio"
        case estimatedSavingsUSD = "estimated_savings_usd"
        case cacheCalls = "cache_calls"
        case lastCallAt = "last_call_at"
        case promptPrefixHash = "prompt_prefix_hash"
        case cacheGeneration = "cache_generation"
        case coreHash = "core_hash"
        case projectHash = "project_hash"
        case toolsHash = "tools_hash"
        case volatileHash = "volatile_hash"
        case preambleTokensEstimate = "preamble_tokens_estimate"
        case lastBreakpoints = "last_breakpoints"
        case lastCacheTTL = "last_cache_ttl"
        case lastMissReason = "last_miss_reason"
        case lastFirstTokenMS = "last_first_token_ms"
        case recentCalls = "recent_calls"
        case policy
        case cacheMissTokens = "cache_miss_tokens"
        case theoreticalMaxHitRatio = "theoretical_max_hit_ratio"
        case cacheEpoch = "cache_epoch"
        case cacheEpochSource = "cache_epoch_source"
        case appendOnlyCalls = "append_only_calls"
        case appendOnlyBreaks = "append_only_breaks"
        case lastAppendOnly = "last_append_only"
        case hitRatioP50 = "hit_ratio_p50"
        case hitRatioP95 = "hit_ratio_p95"
        case theoreticalMaxP50 = "theoretical_max_p50"
        case theoreticalMaxP95 = "theoretical_max_p95"
        case costUSD = "cost_usd"
        case cacheEpochTimeline = "cache_epoch_timeline"
        case perRole = "per_role"
        case topMissReasons = "top_miss_reasons"
        case crossSessionReuseSessions = "cross_session_reuse_sessions"
        case crossSessionHitTokens = "cross_session_hit_tokens"
        case crossSessionCallsObserved = "cross_session_calls_observed"
        case compactionLostCacheUSD = "compaction_lost_cache_usd"
    }

    private struct CacheKey: CodingKey {
        let stringValue: String
        var intValue: Int? {
            nil
        }

        init(_ string: String) {
            stringValue = string
        }

        init?(stringValue: String) {
            self.init(stringValue)
        }

        init?(intValue _: Int) {
            nil
        }
    }

    /// The cache RPC is newer than the client. Decode every field independently
    /// and accept the event field names (`prefix_hash`, `cache_generation`) so a
    /// daemon-side naming change does not break the whole dashboard.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CacheKey.self)

        func string(_ names: String...) -> String? {
            for name in names {
                if let value = try? container.decode(String.self, forKey: CacheKey(name)) {
                    return value
                }
            }
            return nil
        }

        func int(_ names: String...) -> Int? {
            for name in names {
                if let value = try? container.decode(Int.self, forKey: CacheKey(name)) {
                    return value
                }
                if let value = try? container.decode(Double.self, forKey: CacheKey(name)),
                   value.isFinite
                {
                    return Int(exactly: value)
                }
            }
            return nil
        }

        func double(_ names: String...) -> Double? {
            for name in names {
                if let value = try? container.decode(Double.self, forKey: CacheKey(name)) {
                    return value
                }
            }
            return nil
        }

        func bool(_ names: String...) -> Bool? {
            for name in names {
                if let value = try? container.decode(Bool.self, forKey: CacheKey(name)) {
                    return value
                }
            }
            return nil
        }

        func buckets<T: Decodable>(_: T.Type, _ names: String...) -> [T] {
            for name in names {
                if let value = try? container.decode([T].self, forKey: CacheKey(name)) {
                    return value
                }
            }
            return []
        }

        sessionID = string("session_id")
        inputTokens = int("input_tokens")
        uncachedInputTokens = int("uncached_input_tokens")
        cacheReadTokens = int("cache_read_tokens")
        cacheWriteTokens = int("cache_write_tokens")
        cacheHitRatio = double("cache_hit_ratio")
        cacheReadRatio = double("cache_read_ratio")
        cacheWriteRatio = double("cache_write_ratio")
        estimatedSavingsUSD = double("estimated_savings_usd")
        cacheCalls = int("cache_calls")
        lastCallAt = string("last_call_at", "last_call")
        promptPrefixHash = string("prompt_prefix_hash", "prefix_hash", "cache_generation")
        cacheGeneration = string("cache_generation", "prompt_prefix_hash")
        coreHash = string("core_hash")
        projectHash = string("project_hash")
        toolsHash = string("tools_hash")
        volatileHash = string("volatile_hash")
        preambleTokensEstimate = int("preamble_tokens_estimate", "estimated_preamble_tokens")
        lastBreakpoints = int("last_breakpoints", "cache_breakpoints")
        lastCacheTTL = string("last_cache_ttl", "cache_ttl")
        lastMissReason = string("last_miss_reason", "miss_reason")
        lastFirstTokenMS = int("last_first_token_ms", "first_token_ms")
        recentCalls = (try? container.decode([SessionCacheCall].self, forKey: CacheKey("recent_calls"))) ?? []
        policy = try? container.decode(SessionCachePolicy.self, forKey: CacheKey("policy"))
        cacheMissTokens = int("cache_miss_tokens")
        theoreticalMaxHitRatio = double("theoretical_max_hit_ratio")
        cacheEpoch = string("cache_epoch")
        cacheEpochSource = string("cache_epoch_source")
        appendOnlyCalls = int("append_only_calls")
        appendOnlyBreaks = int("append_only_breaks")
        lastAppendOnly = bool("last_append_only")
        hitRatioP50 = double("hit_ratio_p50")
        hitRatioP95 = double("hit_ratio_p95")
        theoreticalMaxP50 = double("theoretical_max_p50")
        theoreticalMaxP95 = double("theoretical_max_p95")
        costUSD = double("cost_usd")
        cacheEpochTimeline = buckets(SessionCacheEpochBucket.self, "cache_epoch_timeline")
        perRole = buckets(SessionCacheRoleBucket.self, "per_role")
        topMissReasons = buckets(SessionCacheCountBucket.self, "top_miss_reasons")
        crossSessionReuseSessions = int("cross_session_reuse_sessions")
        crossSessionHitTokens = int("cross_session_hit_tokens")
        crossSessionCallsObserved = int("cross_session_calls_observed")
        compactionLostCacheUSD = double("compaction_lost_cache_usd")
    }
}

/// One ordered instruction section reported by `session.instructions`.
/// Contains no prompt text; only the content hash and metadata.
struct SessionInstructionSection: Codable, Sendable, Identifiable, Equatable {
    var id: String {
        "\(ordinal)-\(key)"
    }

    let key: String
    let hash: String
    let ordinal: Int
    let tail: Bool
    let chars: Int
    let available: Bool

    enum CodingKeys: String, CodingKey {
        case key
        case hash
        case ordinal
        case tail
        case chars
        case available
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        key = try container.decodeIfPresent(String.self, forKey: .key) ?? ""
        hash = try container.decodeIfPresent(String.self, forKey: .hash) ?? ""
        ordinal = try container.decodeIfPresent(Int.self, forKey: .ordinal) ?? 0
        tail = try container.decodeIfPresent(Bool.self, forKey: .tail) ?? false
        chars = try container.decodeIfPresent(Int.self, forKey: .chars) ?? 0
        available = try container.decodeIfPresent(Bool.self, forKey: .available) ?? false
    }

    var shortHash: String {
        String(hash.prefix(12))
    }
}

/// One per-agent instruction scope. The Leader uses the session id; task
/// agents use `<session>#<agent>`.
struct SessionInstructionStateReport: Codable, Sendable, Identifiable, Equatable {
    var id: String {
        stateID
    }

    let stateID: String
    let role: String?
    let agentID: String?
    let promptLayout: Int
    let headHash: String
    let tailHash: String
    let identityHash: String?
    let sections: [SessionInstructionSection]

    enum CodingKeys: String, CodingKey {
        case stateID = "state_id"
        case role
        case agentID = "agent_id"
        case promptLayout = "prompt_layout"
        case headHash = "head_hash"
        case tailHash = "tail_hash"
        case identityHash = "identity_hash"
        case sections
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        stateID = try container.decodeIfPresent(String.self, forKey: .stateID) ?? ""
        role = try container.decodeIfPresent(String.self, forKey: .role)
        agentID = try container.decodeIfPresent(String.self, forKey: .agentID)
        promptLayout = try container.decodeIfPresent(Int.self, forKey: .promptLayout) ?? 3
        headHash = try container.decodeIfPresent(String.self, forKey: .headHash) ?? ""
        tailHash = try container.decodeIfPresent(String.self, forKey: .tailHash) ?? ""
        identityHash = try container.decodeIfPresent(String.self, forKey: .identityHash)
        sections = try container.decodeIfPresent([SessionInstructionSection].self, forKey: .sections) ?? []
    }

    var headSections: [SessionInstructionSection] {
        sections.filter { !$0.tail }
    }

    var tailSections: [SessionInstructionSection] {
        sections.filter(\.tail)
    }
}

/// Bounded, text-free view of the content-addressed instruction store.
struct SessionInstructionsReport: Codable, Sendable, Equatable {
    let sessionID: String
    let promptLayout: Int
    let latestPrefixHash: String?
    let latestCacheEpoch: String?
    let latestStablePrefixHash: String?
    let latestIdentityID: String?
    let latestIdentityGeneration: Int?
    let latestIdentityHash: String?
    let states: [SessionInstructionStateReport]
    let lastRebuilt: Int

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case promptLayout = "prompt_layout"
        case latestPrefixHash = "latest_prefix_hash"
        case latestCacheEpoch = "latest_cache_epoch"
        case latestStablePrefixHash = "latest_stable_prefix_hash"
        case latestIdentityID = "latest_identity_id"
        case latestIdentityGeneration = "latest_identity_generation"
        case latestIdentityHash = "latest_identity_hash"
        case states
        case lastRebuilt = "last_rebuilt"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        promptLayout = try container.decodeIfPresent(Int.self, forKey: .promptLayout) ?? 2
        latestPrefixHash = try container.decodeIfPresent(String.self, forKey: .latestPrefixHash)
        latestCacheEpoch = try container.decodeIfPresent(String.self, forKey: .latestCacheEpoch)
        latestStablePrefixHash = try container.decodeIfPresent(
            String.self,
            forKey: .latestStablePrefixHash
        )
        latestIdentityID = try container.decodeIfPresent(String.self, forKey: .latestIdentityID)
        latestIdentityGeneration = try container.decodeIfPresent(
            Int.self,
            forKey: .latestIdentityGeneration
        )
        latestIdentityHash = try container.decodeIfPresent(String.self, forKey: .latestIdentityHash)
        states = try container.decodeIfPresent([SessionInstructionStateReport].self, forKey: .states) ?? []
        lastRebuilt = try container.decodeIfPresent(Int.self, forKey: .lastRebuilt) ?? 0
    }

    var leaderState: SessionInstructionStateReport? {
        states.first { state in
            state.agentID == nil
        } ?? states.first
    }
}

/// Bounded, text-free task-contract metadata from `session.contract`.
struct SessionContractMetadata: Codable, Sendable, Equatable {
    let contractID: String
    let hash: String
    let role: String
    let taskID: String?
    let planRevision: Int
    let allowedPathsCount: Int
    let forbiddenPathsCount: Int
    let allowedPathsPreview: [String]
    let egress: String
    let unrestrictedScope: Bool
    let acceptanceCount: Int
    let allowedToolsCount: Int
    let maxWrites: Int?
    let maxBytes: Int?
    let maxCommits: Int?
    let writesUsed: Int
    let bytesUsed: Int
    let commitsUsed: Int
    let enforcementMode: String

    enum CodingKeys: String, CodingKey {
        case contractID = "contract_id"
        case hash
        case role
        case taskID = "task_id"
        case planRevision = "plan_revision"
        case allowedPathsCount = "allowed_paths_count"
        case forbiddenPathsCount = "forbidden_paths_count"
        case allowedPathsPreview = "allowed_paths_preview"
        case egress
        case unrestrictedScope = "unrestricted_scope"
        case acceptanceCount = "acceptance_count"
        case allowedToolsCount = "allowed_tools_count"
        case maxWrites = "max_writes"
        case maxBytes = "max_bytes"
        case maxCommits = "max_commits"
        case writesUsed = "writes_used"
        case bytesUsed = "bytes_used"
        case commitsUsed = "commits_used"
        case enforcementMode = "enforcement_mode"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        contractID = try container.decodeIfPresent(String.self, forKey: .contractID) ?? ""
        hash = try container.decodeIfPresent(String.self, forKey: .hash) ?? ""
        role = try container.decodeIfPresent(String.self, forKey: .role) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        allowedPathsCount = try container.decodeIfPresent(Int.self, forKey: .allowedPathsCount) ?? 0
        forbiddenPathsCount = try container.decodeIfPresent(Int.self, forKey: .forbiddenPathsCount) ?? 0
        allowedPathsPreview = try container.decodeIfPresent([String].self, forKey: .allowedPathsPreview) ?? []
        egress = try container.decodeIfPresent(String.self, forKey: .egress) ?? "allow"
        unrestrictedScope = try container.decodeIfPresent(Bool.self, forKey: .unrestrictedScope) ?? false
        acceptanceCount = try container.decodeIfPresent(Int.self, forKey: .acceptanceCount) ?? 0
        allowedToolsCount = try container.decodeIfPresent(Int.self, forKey: .allowedToolsCount) ?? 0
        maxWrites = try container.decodeIfPresent(Int.self, forKey: .maxWrites)
        maxBytes = try container.decodeIfPresent(Int.self, forKey: .maxBytes)
        maxCommits = try container.decodeIfPresent(Int.self, forKey: .maxCommits)
        writesUsed = try container.decodeIfPresent(Int.self, forKey: .writesUsed) ?? 0
        bytesUsed = try container.decodeIfPresent(Int.self, forKey: .bytesUsed) ?? 0
        commitsUsed = try container.decodeIfPresent(Int.self, forKey: .commitsUsed) ?? 0
        enforcementMode = try container.decodeIfPresent(String.self, forKey: .enforcementMode) ?? "enforce"
    }

    var shortHash: String {
        String(hash.prefix(12))
    }

    var scopeLabel: String {
        if unrestrictedScope {
            return "unrestricted"
        }
        return allowedPathsPreview.prefix(2).joined(separator: ", ")
    }
}

struct SessionContractPolicySummary: Codable, Sendable, Equatable {
    let enabled: Bool
    let mode: String
    let enforceShell: Bool

    enum CodingKeys: String, CodingKey {
        case enabled
        case mode
        case enforceShell = "enforce_shell"
    }

    init() {
        enabled = true
        mode = "enforce"
        enforceShell = false
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        mode = try container.decodeIfPresent(String.self, forKey: .mode) ?? "enforce"
        enforceShell = try container.decodeIfPresent(Bool.self, forKey: .enforceShell) ?? false
    }
}

/// Bounded task-contract view. No contract text, prompt, or argument values.
struct SessionContractReport: Codable, Sendable, Equatable {
    let sessionID: String
    let projectID: String
    let contract: SessionContractMetadata?
    let policy: SessionContractPolicySummary
    let decisionCount: Int

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case projectID = "project_id"
        case contract
        case policy
        case decisionCount = "recent_decisions"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        contract = try container.decodeIfPresent(SessionContractMetadata.self, forKey: .contract)
        policy = try container.decodeIfPresent(
            SessionContractPolicySummary.self,
            forKey: .policy
        ) ?? SessionContractPolicySummary()
        decisionCount = try container.decodeIfPresent([JSONValue].self, forKey: .decisionCount)?.count ?? 0
    }
}

/// Bounded local effect-receipt metadata from `session.effects`. No file
/// content, command text, prompt text, or provider secret is rendered.
struct EffectTargetView: Codable, Sendable, Equatable {
    let kind: String
    let key: String

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        key = try container.decodeIfPresent(String.self, forKey: .key) ?? ""
    }

    var label: String {
        key.isEmpty ? kind : "\(kind):\(key)"
    }
}

struct EffectEvidenceView: Codable, Sendable, Equatable {
    let probe: String
    let outcome: String
    let reason: String?
    let sha256: String?
    let commit: String?
    let externalID: String?
    let change: String?
    let repository: String?
    let note: String?
    let caveat: String?
    let replayed: Bool

    enum CodingKeys: String, CodingKey {
        case probe
        case outcome
        case reason
        case sha256
        case commit
        case externalID = "external_id"
        case change
        case repository
        case note
        case caveat
        case replayed
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        probe = try container.decodeIfPresent(String.self, forKey: .probe) ?? ""
        outcome = try container.decodeIfPresent(String.self, forKey: .outcome) ?? ""
        reason = try container.decodeIfPresent(String.self, forKey: .reason)
        sha256 = try container.decodeIfPresent(String.self, forKey: .sha256)
        commit = try container.decodeIfPresent(String.self, forKey: .commit)
        externalID = try container.decodeIfPresent(String.self, forKey: .externalID)
        change = try container.decodeIfPresent(String.self, forKey: .change)
        repository = try container.decodeIfPresent(String.self, forKey: .repository)
        note = try container.decodeIfPresent(String.self, forKey: .note)
        caveat = try container.decodeIfPresent(String.self, forKey: .caveat)
        replayed = try container.decodeIfPresent(Bool.self, forKey: .replayed) ?? false
    }

    var anchor: String? {
        if let sha256, !sha256.isEmpty {
            return String(sha256.prefix(16))
        }
        if let commit, !commit.isEmpty {
            return String(commit.prefix(16))
        }
        if let externalID, !externalID.isEmpty {
            return String(externalID.prefix(48))
        }
        return nil
    }
}

struct EffectReceiptView: Codable, Sendable, Identifiable, Equatable {
    let id: String
    let sessionID: String
    let taskID: String?
    let attempt: Int
    let tool: String
    let target: EffectTargetView
    let state: String
    let externalID: String?
    let evidence: EffectEvidenceView

    enum CodingKeys: String, CodingKey {
        case id
        case sessionID = "session_id"
        case taskID = "task_id"
        case attempt
        case tool
        case target
        case state
        case externalID = "external_id"
        case evidence
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        attempt = try container.decodeIfPresent(Int.self, forKey: .attempt) ?? 0
        tool = try container.decodeIfPresent(String.self, forKey: .tool) ?? ""
        target = try container.decodeIfPresent(EffectTargetView.self, forKey: .target)
            ?? EffectTargetView.empty
        state = try container.decodeIfPresent(String.self, forKey: .state) ?? "unknown"
        externalID = try container.decodeIfPresent(String.self, forKey: .externalID)
        evidence = try container.decodeIfPresent(EffectEvidenceView.self, forKey: .evidence)
            ?? EffectEvidenceView.empty
    }

    var blocksMerge: Bool {
        state == "intent" || state == "unknown"
    }

    var stateLabel: String {
        state.uppercased()
    }
}

extension EffectTargetView {
    static let empty = EffectTargetView(kind: "", key: "")

    init(kind: String, key: String) {
        self.kind = kind
        self.key = key
    }
}

extension EffectEvidenceView {
    static let empty = EffectEvidenceView(
        probe: "",
        outcome: "",
        reason: nil,
        sha256: nil,
        commit: nil,
        externalID: nil,
        change: nil,
        repository: nil,
        note: nil,
        caveat: nil,
        replayed: false
    )

    init(
        probe: String,
        outcome: String,
        reason: String?,
        sha256: String?,
        commit: String?,
        externalID: String?,
        change: String?,
        repository: String?,
        note: String?,
        caveat: String?,
        replayed: Bool
    ) {
        self.probe = probe
        self.outcome = outcome
        self.reason = reason
        self.sha256 = sha256
        self.commit = commit
        self.externalID = externalID
        self.change = change
        self.repository = repository
        self.note = note
        self.caveat = caveat
        self.replayed = replayed
    }
}

struct SessionEffectCounts: Codable, Sendable, Equatable {
    let total: Int
    let intent: Int
    let done: Int
    let failed: Int
    let unknown: Int
    let blocking: Int

    init() {
        total = 0
        intent = 0
        done = 0
        failed = 0
        unknown = 0
        blocking = 0
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        total = try container.decodeIfPresent(Int.self, forKey: .total) ?? 0
        intent = try container.decodeIfPresent(Int.self, forKey: .intent) ?? 0
        done = try container.decodeIfPresent(Int.self, forKey: .done) ?? 0
        failed = try container.decodeIfPresent(Int.self, forKey: .failed) ?? 0
        unknown = try container.decodeIfPresent(Int.self, forKey: .unknown) ?? 0
        blocking = try container.decodeIfPresent(Int.self, forKey: .blocking) ?? 0
    }
}

struct SessionEffectPolicySummary: Codable, Sendable, Equatable {
    let enabled: Bool
    let unknownBlocksMerge: Bool
    let staleIntentGraceSecs: Int
    let shellMode: String

    enum CodingKeys: String, CodingKey {
        case enabled
        case unknownBlocksMerge = "unknown_blocks_merge"
        case staleIntentGraceSecs = "stale_intent_grace_secs"
        case shellMode = "shell_mode"
    }

    init() {
        enabled = true
        unknownBlocksMerge = true
        staleIntentGraceSecs = 120
        shellMode = "external_only"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        unknownBlocksMerge =
            try container.decodeIfPresent(Bool.self, forKey: .unknownBlocksMerge) ?? true
        staleIntentGraceSecs =
            try container.decodeIfPresent(Int.self, forKey: .staleIntentGraceSecs) ?? 120
        shellMode = try container.decodeIfPresent(String.self, forKey: .shellMode)
            ?? "external_only"
    }
}

/// Read-only public gate projection. The daemon never sends a token or token
/// hash to the GUI; the one-time token belongs to the explicit CLI/RPC open
/// response only.
struct ExternalGateView: Codable, Sendable, Identifiable, Equatable {
    let id: String
    let sessionID: String
    let taskID: String?
    let kind: String
    let label: String
    let state: String
    let outcome: String?
    let resolutionNote: String?
    let createdAt: String
    let expiresAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case sessionID = "session_id"
        case taskID = "task_id"
        case kind
        case label
        case state
        case outcome
        case resolutionNote = "resolution_note"
        case createdAt = "created_at"
        case expiresAt = "expires_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? "custom"
        label = try container.decodeIfPresent(String.self, forKey: .label) ?? ""
        state = try container.decodeIfPresent(String.self, forKey: .state) ?? "open"
        outcome = try container.decodeIfPresent(String.self, forKey: .outcome)
        resolutionNote = try container.decodeIfPresent(String.self, forKey: .resolutionNote)
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        expiresAt = try container.decodeIfPresent(String.self, forKey: .expiresAt)
    }

    var isOpen: Bool {
        state == "open"
    }

    var stateLabel: String {
        state.uppercased()
    }
}

struct SessionGateCounts: Codable, Sendable, Equatable {
    let total: Int
    let open: Int
    let resolved: Int
    let expired: Int
    let cancelled: Int

    init() {
        total = 0
        open = 0
        resolved = 0
        expired = 0
        cancelled = 0
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        total = try container.decodeIfPresent(Int.self, forKey: .total) ?? 0
        open = try container.decodeIfPresent(Int.self, forKey: .open) ?? 0
        resolved = try container.decodeIfPresent(Int.self, forKey: .resolved) ?? 0
        expired = try container.decodeIfPresent(Int.self, forKey: .expired) ?? 0
        cancelled = try container.decodeIfPresent(Int.self, forKey: .cancelled) ?? 0
    }
}

struct SessionGatesReport: Codable, Sendable, Equatable {
    let sessionID: String
    let gates: [ExternalGateView]
    let counts: SessionGateCounts
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case gates
        case counts
        case truncated
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        gates = try container.decodeIfPresent([ExternalGateView].self, forKey: .gates) ?? []
        counts = try container.decodeIfPresent(SessionGateCounts.self, forKey: .counts)
            ?? SessionGateCounts()
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }

    var openGates: [ExternalGateView] {
        gates.filter(\.isOpen)
    }
}

struct SessionEffectsReport: Codable, Sendable, Equatable { let sessionID: String
    let receipts: [EffectReceiptView]
    let counts: SessionEffectCounts
    let blocked: Bool
    let blockReason: String?
    let truncated: Bool
    let policy: SessionEffectPolicySummary

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case receipts
        case counts
        case blocked
        case blockReason = "block_reason"
        case truncated
        case policy
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        receipts = try container.decodeIfPresent([EffectReceiptView].self, forKey: .receipts) ?? []
        counts = try container.decodeIfPresent(SessionEffectCounts.self, forKey: .counts)
            ?? SessionEffectCounts()
        blocked = try container.decodeIfPresent(Bool.self, forKey: .blocked) ?? false
        blockReason = try container.decodeIfPresent(String.self, forKey: .blockReason)
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        policy = try container.decodeIfPresent(SessionEffectPolicySummary.self, forKey: .policy)
            ?? SessionEffectPolicySummary()
    }

    var unresolved: [EffectReceiptView] {
        receipts.filter(\.blocksMerge)
    }

    var blockingReceipts: [EffectReceiptView] {
        blocked ? receipts.filter(\.blocksMerge) : []
    }
}

struct EffectPolicyEnvelope: Codable, Sendable, Equatable {
    let policy: SessionEffectPolicySummary

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        policy = try container.decodeIfPresent(SessionEffectPolicySummary.self, forKey: .policy)
            ?? SessionEffectPolicySummary()
    }
}

struct EffectReconcileResponse: Codable, Sendable, Equatable {
    let receipt: EffectReceiptView?
    let changed: Bool

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        receipt = try container.decodeIfPresent(EffectReceiptView.self, forKey: .receipt)
        changed = try container.decodeIfPresent(Bool.self, forKey: .changed) ?? false
    }
}

/// One bounded per-call cache observation from `session.cache`.
struct SessionCacheCall: Codable, Sendable, Identifiable, Equatable {
    var id: String {
        "\(at)-\(agentName ?? "")-\(inputTokens)"
    }

    let at: String
    let agentName: String?
    let role: String?
    let model: String?
    let inputTokens: Int
    let outputTokens: Int
    let cacheReadTokens: Int
    let cacheWriteTokens: Int
    let cacheHitRatio: Double
    let cacheBreakpoints: Int
    let cacheTTL: String?
    let firstTokenMS: Int?
    let coreHash: String?
    let projectHash: String?
    let toolsHash: String?
    let toolBundle: String?
    let toolBundleHash: String?
    let volatileHash: String?
    let promptPrefixHash: String?
    let estimatedSavingsUSD: Double
    let missReason: String?
    let warm: Bool
    let cacheMissTokens: Int
    let theoreticalMaxHitRatio: Double
    let messagePrefixHash: String?
    let previousMessagePrefixHash: String?
    let messagePrefixCount: Int
    let previousMessagePrefixCount: Int
    let appendOnly: Bool?
    let cacheEpoch: String?
    let cacheEpochSource: String?
    let previousInputTokens: Int

    enum CodingKeys: String, CodingKey {
        case at
        case agentName = "agent_name"
        case role
        case model
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
        case cacheHitRatio = "cache_hit_ratio"
        case cacheBreakpoints = "cache_breakpoints"
        case cacheTTL = "cache_ttl"
        case firstTokenMS = "first_token_ms"
        case coreHash = "core_hash"
        case projectHash = "project_hash"
        case toolsHash = "tools_hash"
        case toolBundle = "tool_bundle"
        case toolBundleHash = "tool_bundle_hash"
        case volatileHash = "volatile_hash"
        case promptPrefixHash = "prompt_prefix_hash"
        case estimatedSavingsUSD = "estimated_savings_usd"
        case missReason = "miss_reason"
        case warm
        case cacheMissTokens = "cache_miss_tokens"
        case theoreticalMaxHitRatio = "theoretical_max_hit_ratio"
        case messagePrefixHash = "message_prefix_hash"
        case previousMessagePrefixHash = "previous_message_prefix_hash"
        case messagePrefixCount = "message_prefix_count"
        case previousMessagePrefixCount = "previous_message_prefix_count"
        case appendOnly = "append_only"
        case cacheEpoch = "cache_epoch"
        case cacheEpochSource = "cache_epoch_source"
        case previousInputTokens = "previous_input_tokens"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        at = (try? container.decode(String.self, forKey: .at)) ?? ""
        agentName = try? container.decode(String.self, forKey: .agentName)
        role = try? container.decode(String.self, forKey: .role)
        model = try? container.decode(String.self, forKey: .model)
        inputTokens = (try? container.decode(Int.self, forKey: .inputTokens)) ?? 0
        outputTokens = (try? container.decode(Int.self, forKey: .outputTokens)) ?? 0
        cacheReadTokens = (try? container.decode(Int.self, forKey: .cacheReadTokens)) ?? 0
        cacheWriteTokens = (try? container.decode(Int.self, forKey: .cacheWriteTokens)) ?? 0
        cacheHitRatio = (try? container.decode(Double.self, forKey: .cacheHitRatio)) ?? 0
        cacheBreakpoints = (try? container.decode(Int.self, forKey: .cacheBreakpoints)) ?? 0
        cacheTTL = try? container.decode(String.self, forKey: .cacheTTL)
        firstTokenMS = try? container.decode(Int.self, forKey: .firstTokenMS)
        coreHash = try? container.decode(String.self, forKey: .coreHash)
        projectHash = try? container.decode(String.self, forKey: .projectHash)
        toolsHash = try? container.decode(String.self, forKey: .toolsHash)
        toolBundle = try? container.decode(String.self, forKey: .toolBundle)
        toolBundleHash = try? container.decode(String.self, forKey: .toolBundleHash)
        volatileHash = try? container.decode(String.self, forKey: .volatileHash)
        promptPrefixHash = try? container.decode(String.self, forKey: .promptPrefixHash)
        estimatedSavingsUSD = (try? container.decode(Double.self, forKey: .estimatedSavingsUSD)) ?? 0
        missReason = try? container.decode(String.self, forKey: .missReason)
        warm = (try? container.decode(Bool.self, forKey: .warm)) ?? false
        cacheMissTokens = (try? container.decode(Int.self, forKey: .cacheMissTokens)) ?? 0
        theoreticalMaxHitRatio = (try? container.decode(Double.self, forKey: .theoreticalMaxHitRatio)) ?? 0
        messagePrefixHash = try? container.decode(String.self, forKey: .messagePrefixHash)
        previousMessagePrefixHash = try? container.decode(String.self, forKey: .previousMessagePrefixHash)
        messagePrefixCount = (try? container.decode(Int.self, forKey: .messagePrefixCount)) ?? 0
        previousMessagePrefixCount = (try? container.decode(Int.self, forKey: .previousMessagePrefixCount)) ??
            0
        appendOnly = try? container.decode(Bool.self, forKey: .appendOnly)
        cacheEpoch = try? container.decode(String.self, forKey: .cacheEpoch)
        cacheEpochSource = try? container.decode(String.self, forKey: .cacheEpochSource)
        previousInputTokens = (try? container.decode(Int.self, forKey: .previousInputTokens)) ?? 0
    }
}

/// One cache epoch in the bounded session timeline.
struct SessionCacheEpochBucket: Codable, Sendable, Identifiable, Equatable {
    var id: String {
        epoch
    }

    let epoch: String
    let source: String?
    let calls: Int
    let firstAt: String
    let lastAt: String
    let inputTokens: Int
    let cacheReadTokens: Int
    let cacheMissTokens: Int
    let hitRatio: Double
    let appendOnlyBreaks: Int

    enum CodingKeys: String, CodingKey {
        case epoch
        case source
        case calls
        case firstAt = "first_at"
        case lastAt = "last_at"
        case inputTokens = "input_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheMissTokens = "cache_miss_tokens"
        case hitRatio = "hit_ratio"
        case appendOnlyBreaks = "append_only_breaks"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        epoch = (try? container.decode(String.self, forKey: .epoch)) ?? ""
        source = try? container.decode(String.self, forKey: .source)
        calls = (try? container.decode(Int.self, forKey: .calls)) ?? 0
        firstAt = (try? container.decode(String.self, forKey: .firstAt)) ?? ""
        lastAt = (try? container.decode(String.self, forKey: .lastAt)) ?? ""
        inputTokens = (try? container.decode(Int.self, forKey: .inputTokens)) ?? 0
        cacheReadTokens = (try? container.decode(Int.self, forKey: .cacheReadTokens)) ?? 0
        cacheMissTokens = (try? container.decode(Int.self, forKey: .cacheMissTokens)) ?? 0
        hitRatio = (try? container.decode(Double.self, forKey: .hitRatio)) ?? 0
        appendOnlyBreaks = (try? container.decode(Int.self, forKey: .appendOnlyBreaks)) ?? 0
    }
}

/// Per-role cache accounting from `session.cache`.
struct SessionCacheRoleBucket: Codable, Sendable, Identifiable, Equatable {
    var id: String {
        role
    }

    let role: String
    let calls: Int
    let inputTokens: Int
    let cacheReadTokens: Int
    let cacheMissTokens: Int
    let hitRatio: Double
    let theoreticalMaxHitRatio: Double
    let costUSD: Double

    enum CodingKeys: String, CodingKey {
        case role
        case calls
        case inputTokens = "input_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheMissTokens = "cache_miss_tokens"
        case hitRatio = "hit_ratio"
        case theoreticalMaxHitRatio = "theoretical_max_hit_ratio"
        case costUSD = "cost_usd"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        role = (try? container.decode(String.self, forKey: .role)) ?? ""
        calls = (try? container.decode(Int.self, forKey: .calls)) ?? 0
        inputTokens = (try? container.decode(Int.self, forKey: .inputTokens)) ?? 0
        cacheReadTokens = (try? container.decode(Int.self, forKey: .cacheReadTokens)) ?? 0
        cacheMissTokens = (try? container.decode(Int.self, forKey: .cacheMissTokens)) ?? 0
        hitRatio = (try? container.decode(Double.self, forKey: .hitRatio)) ?? 0
        theoreticalMaxHitRatio = (try? container.decode(Double.self, forKey: .theoreticalMaxHitRatio)) ?? 0
        costUSD = (try? container.decode(Double.self, forKey: .costUSD)) ?? 0
    }
}

/// Miss-reason count bucket. Unknown keys stay visible.
struct SessionCacheCountBucket: Codable, Sendable, Identifiable, Equatable {
    var id: String {
        key
    }

    let key: String
    let calls: Int

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        key = (try? container.decode(String.self, forKey: .key)) ?? ""
        calls = (try? container.decode(Int.self, forKey: .calls)) ?? 0
    }

    enum CodingKeys: String, CodingKey {
        case key
        case calls
    }
}

/// Bounded newest-first per-call cache table from `session.cache.calls`.
struct SessionCacheCallsResult: Codable, Sendable, Equatable {
    let sessionID: String
    let calls: [SessionCacheCall]
    let observed: Int
    let returned: Int
    let truncated: Bool
    let limit: Int
    let role: String?
    let missReason: String?
    let includeWarm: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case calls
        case observed
        case returned
        case truncated
        case limit
        case role
        case missReason = "miss_reason"
        case includeWarm = "include_warm"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = (try? container.decode(String.self, forKey: .sessionID)) ?? ""
        calls = (try? container.decode([SessionCacheCall].self, forKey: .calls)) ?? []
        observed = (try? container.decode(Int.self, forKey: .observed)) ?? 0
        returned = (try? container.decode(Int.self, forKey: .returned)) ?? calls.count
        truncated = (try? container.decode(Bool.self, forKey: .truncated)) ?? false
        limit = (try? container.decode(Int.self, forKey: .limit)) ?? 0
        role = try? container.decode(String.self, forKey: .role)
        missReason = try? container.decode(String.self, forKey: .missReason)
        includeWarm = (try? container.decode(Bool.self, forKey: .includeWarm)) ?? false
    }
}

/// Result of an explicit `session.cache.warm` call. Every field is reported by
/// the daemon; the UI never invents a warm success.
struct SessionCacheWarmResult: Codable, Sendable {
    let sessionID: String
    let warmed: Bool
    let provider: String
    let model: String
    let role: String?
    let source: String?
    let cacheBreakpoints: Int?
    let cacheTTL: String?
    let automatic: Bool?
    let identityHash: String?
    let identityID: String?
    let identityGeneration: Int?
    let inputTokens: Int?
    let outputTokens: Int?
    let cacheReadTokens: Int?
    let cacheWriteTokens: Int?
    let estimated: Bool?
    let costUSD: Double?
    let latencyMS: Int?
    let firstTokenMS: Int?
    let message: String?

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case warmed
        case provider
        case model
        case role
        case source
        case cacheBreakpoints = "cache_breakpoints"
        case cacheTTL = "cache_ttl"
        case automatic
        case identityHash = "identity_hash"
        case identityID = "identity_id"
        case identityGeneration = "identity_generation"
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
        case estimated
        case costUSD = "cost_usd"
        case latencyMS = "latency_ms"
        case firstTokenMS = "first_token_ms"
        case message
    }
}

struct ProjectCachePolicyResult: Codable, Sendable {
    let projectID: String
    let policy: SessionCachePolicy

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case policy
    }
}

struct SessionCacheAuditFinding: Codable, Sendable, Identifiable {
    var id: String {
        "\(severity)-\(code)"
    }

    let code: String
    let severity: String
    let message: String
    let evidence: [String: JSONValue]
}

struct SessionCacheAuditResult: Codable, Sendable {
    let sessionID: String
    let provider: String?
    let callsChecked: Int
    let warmCallsChecked: Int
    let compactionsChecked: Int
    let hitRatio: Double
    let cacheReadTokens: Int
    let cacheWriteTokens: Int
    let findings: [SessionCacheAuditFinding]
    let clean: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case provider
        case callsChecked = "calls_checked"
        case warmCallsChecked = "warm_calls_checked"
        case compactionsChecked = "compactions_checked"
        case hitRatio = "hit_ratio"
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
        case findings
        case clean
    }
}

struct SessionCachePlanRole: Codable, Sendable, Identifiable {
    var id: String {
        role
    }

    let role: String
    let provider: String
    let model: String
    let cacheBreakpoints: Int
    let cacheTTL: String
    let ttlExplicit: Bool
    let automatic: Bool
    let tailMessage: Bool
    let toolBreakpoint: Bool
    let coreBreakpoint: Bool
    let projectBreakpoint: Bool
    let coreHash: String
    let projectHash: String
    let toolsHash: String
    let volatileHash: String
    let prefixHash: String
    let identityHash: String?
    let identityID: String?
    let identityGeneration: Int?
    let coreChars: Int
    let projectChars: Int
    let stablePrefixChars: Int
    let toolCount: Int
    let systemTokensEstimate: Int
    let toolTokensEstimate: Int

    enum CodingKeys: String, CodingKey {
        case role
        case provider
        case model
        case cacheBreakpoints = "cache_breakpoints"
        case cacheTTL = "cache_ttl"
        case ttlExplicit = "ttl_explicit"
        case automatic
        case tailMessage = "tail_message"
        case toolBreakpoint = "tool_breakpoint"
        case coreBreakpoint = "core_breakpoint"
        case projectBreakpoint = "project_breakpoint"
        case coreHash = "core_hash"
        case projectHash = "project_hash"
        case toolsHash = "tools_hash"
        case volatileHash = "volatile_hash"
        case prefixHash = "prefix_hash"
        case identityHash = "identity_hash"
        case identityID = "identity_id"
        case identityGeneration = "identity_generation"
        case coreChars = "core_chars"
        case projectChars = "project_chars"
        case stablePrefixChars = "stable_prefix_chars"
        case toolCount = "tool_count"
        case systemTokensEstimate = "system_tokens_estimate"
        case toolTokensEstimate = "tool_tokens_estimate"
    }
}

struct SessionCachePlanResult: Codable, Sendable {
    let sessionID: String
    let provider: String
    let policy: SessionCachePolicy?
    let roles: [SessionCachePlanRole]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case provider
        case policy
        case roles
    }
}

/// Project cache policy as reported by `session.cache`.
struct SessionCachePolicy: Codable, Sendable {
    let enabled: Bool
    let ttl: String
    let automaticCaching: Bool
    let warmOnResume: Bool
    let compactAware: Bool

    enum CodingKeys: String, CodingKey {
        case enabled
        case ttl
        case automaticCaching = "automatic_caching"
        case warmOnResume = "warm_on_resume"
        case compactAware = "compact_aware"
    }
}

struct ContextUsage: Codable, Sendable {
    let estimatedTokens: Int
    let contextWindow: Int
    let thresholdTokens: Int
    let hardLimitTokens: Int
    let messages: Int
    let calibration: Double?
    /// Estimated tokens in the stable system/tool preamble. Optional because
    /// preamble telemetry arrived after the original context snapshot.
    let preambleTokensEstimate: Int?
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case estimatedTokens = "estimated_tokens"
        case contextWindow = "context_window"
        case thresholdTokens = "threshold_tokens"
        case hardLimitTokens = "hard_limit_tokens"
        case messages
        case calibration
        case preambleTokensEstimate = "preamble_tokens_estimate"
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
    let latencyMs: Int?
    let backgroundReuse: Bool?
    let fallbackToSync: Bool?
    let compactionCount: Int?
    let structuredState: JSONValue?

    enum CodingKeys: String, CodingKey {
        case mode
        case summarizedMessages = "summarized_messages"
        case droppedMessages = "dropped_messages"
        case keptMessages = "kept_messages"
        case estimatedTokensBefore = "estimated_tokens_before"
        case estimatedTokensAfter = "estimated_tokens_after"
        case createdAt = "created_at"
        case latencyMs = "latency_ms"
        case backgroundReuse = "background_reuse"
        case fallbackToSync = "fallback_to_sync"
        case compactionCount = "compaction_count"
        case structuredState = "structured_state"
    }
}

struct SessionContextStatus: Codable, Sendable {
    let sessionID: String
    let usage: ContextUsage?
    let lastCompaction: ContextCompactionSummary?
    let lastMeasurement: ContextMeasurement?
    let compactionCount: Int?
    let lastCompactionLatencyMs: Int?
    let lastCompactionTokensBefore: Int?
    let lastCompactionTokensAfter: Int?
    let compactionBackgroundReuse: Bool?
    let compactionFallbackToSync: Bool?
    let checkpointCount: Int?
    let latestCheckpointID: String?
    let latestCheckpointAt: String?

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case usage
        case lastCompaction = "last_compaction"
        case lastMeasurement = "last_measurement"
        case compactionCount = "compaction_count"
        case lastCompactionLatencyMs = "last_compaction_latency_ms"
        case lastCompactionTokensBefore = "last_compaction_tokens_before"
        case lastCompactionTokensAfter = "last_compaction_tokens_after"
        case compactionBackgroundReuse = "compaction_background_reuse"
        case compactionFallbackToSync = "compaction_fallback_to_sync"
        case checkpointCount = "checkpoint_count"
        case latestCheckpointID = "latest_checkpoint_id"
        case latestCheckpointAt = "latest_checkpoint_at"
    }
}

struct ContextCheckpointPolicy: Codable, Sendable, Equatable {
    var enabled: Bool
    var maxCheckpoints: Int
    var maxAgeDays: Int

    enum CodingKeys: String, CodingKey {
        case enabled
        case maxCheckpoints = "max_checkpoints"
        case maxAgeDays = "max_age_days"
    }
}

struct ContextCheckpoint: Codable, Sendable, Identifiable, Equatable {
    let id: String
    let sessionID: String
    let projectID: String
    let label: String
    let mode: String
    let summary: String?
    let throughMessageID: String?
    let messageCursor: Int
    let planRevision: Int
    let provider: String
    let model: String
    let promptPrefixHash: String
    let worktreeHead: String
    let tokensBefore: Int
    let tokensAfter: Int
    let structuredState: JSONValue?
    let sourceCompactionSeq: Int?
    let automatic: Bool
    let createdAt: String

    var tokenDelta: Int {
        max(0, tokensBefore - tokensAfter)
    }

    enum CodingKeys: String, CodingKey {
        case id
        case sessionID = "session_id"
        case projectID = "project_id"
        case label
        case mode
        case summary
        case throughMessageID = "through_message_id"
        case messageCursor = "message_cursor"
        case planRevision = "plan_revision"
        case provider
        case model
        case promptPrefixHash = "prompt_prefix_hash"
        case worktreeHead = "worktree_head"
        case tokensBefore = "tokens_before"
        case tokensAfter = "tokens_after"
        case structuredState = "structured_state"
        case sourceCompactionSeq = "source_compaction_seq"
        case automatic
        case createdAt = "created_at"
    }
}

struct ContextCheckpointListResult: Codable, Sendable {
    let sessionID: String
    let checkpoints: [ContextCheckpoint]
    let latestID: String?
    let policy: ContextCheckpointPolicy
    let limit: Int

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case checkpoints
        case latestID = "latest_id"
        case policy
        case limit
    }
}

struct ContextRestorePreflight: Codable, Sendable, Equatable {
    let restorable: Bool
    let errors: [String]
    let warnings: [String]
    let estimatedTokens: Int
    let hardLimitTokens: Int
    let thresholdTokens: Int
    let activeMessages: Int
    let planRevision: Int
    let checkpointPlanRevision: Int
    let provider: String
    let model: String
    let worktreeHead: String
    let currentWorktreeHead: String?
    let worktreeDescendant: Bool
    let budgetExhausted: Bool

    enum CodingKeys: String, CodingKey {
        case restorable
        case errors
        case warnings
        case estimatedTokens = "estimated_tokens"
        case hardLimitTokens = "hard_limit_tokens"
        case thresholdTokens = "threshold_tokens"
        case activeMessages = "active_messages"
        case planRevision = "plan_revision"
        case checkpointPlanRevision = "checkpoint_plan_revision"
        case provider
        case model
        case worktreeHead = "worktree_head"
        case currentWorktreeHead = "current_worktree_head"
        case worktreeDescendant = "worktree_descendant"
        case budgetExhausted = "budget_exhausted"
    }
}

struct ContextCheckpointDetailResult: Codable, Sendable, Equatable {
    let checkpoint: ContextCheckpoint
    let preflight: ContextRestorePreflight
    let isActive: Bool
    let summaryPreview: String

    enum CodingKeys: String, CodingKey {
        case checkpoint
        case preflight
        case isActive = "is_active"
        case summaryPreview = "summary_preview"
    }
}

struct ContextRestoreResult: Codable, Sendable {
    let sessionID: String
    let checkpointID: String
    let mode: String
    let throughMessageID: String?
    let activeMessages: Int
    let estimatedTokens: Int
    let hardLimitTokens: Int
    let warning: String?
    let compactionEventSeq: Int
    let restoredEventSeq: Int

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case checkpointID = "checkpoint_id"
        case mode
        case throughMessageID = "through_message_id"
        case activeMessages = "active_messages"
        case estimatedTokens = "estimated_tokens"
        case hardLimitTokens = "hard_limit_tokens"
        case warning
        case compactionEventSeq = "compaction_event_seq"
        case restoredEventSeq = "restored_event_seq"
    }
}

struct ContextCheckpointDeleteResult: Codable, Sendable {
    let sessionID: String
    let checkpointID: String
    let removed: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case checkpointID = "checkpoint_id"
        case removed
    }
}

struct ContextCheckpointPruneResult: Codable, Sendable {
    let sessionID: String
    let candidates: Int
    let removed: Int
    let dryRun: Bool
    let checkpoints: [ContextCheckpoint]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case candidates
        case removed
        case dryRun = "dry_run"
        case checkpoints
    }
}

struct ContextCheckpointPolicyResult: Codable, Sendable {
    let projectID: String
    let policy: ContextCheckpointPolicy

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case policy
    }
}

extension ContextCheckpointPolicy {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        maxCheckpoints = try container.decodeIfPresent(Int.self, forKey: .maxCheckpoints) ?? 20
        maxAgeDays = try container.decodeIfPresent(Int.self, forKey: .maxAgeDays) ?? 30
    }
}

extension ContextCheckpoint {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        sessionID = try container.decode(String.self, forKey: .sessionID)
        projectID = try container.decode(String.self, forKey: .projectID)
        label = try container.decodeIfPresent(String.self, forKey: .label) ?? ""
        mode = try container.decodeIfPresent(String.self, forKey: .mode) ?? "manual"
        summary = try container.decodeIfPresent(String.self, forKey: .summary)
        throughMessageID = try container.decodeIfPresent(String.self, forKey: .throughMessageID)
        messageCursor = try container.decodeIfPresent(Int.self, forKey: .messageCursor) ?? 0
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        provider = try container.decodeIfPresent(String.self, forKey: .provider) ?? ""
        model = try container.decodeIfPresent(String.self, forKey: .model) ?? ""
        promptPrefixHash = try container.decodeIfPresent(String.self, forKey: .promptPrefixHash) ?? ""
        worktreeHead = try container.decodeIfPresent(String.self, forKey: .worktreeHead) ?? ""
        tokensBefore = try container.decodeIfPresent(Int.self, forKey: .tokensBefore) ?? 0
        tokensAfter = try container.decodeIfPresent(Int.self, forKey: .tokensAfter) ?? 0
        structuredState = try container.decodeIfPresent(JSONValue.self, forKey: .structuredState)
        sourceCompactionSeq = try container.decodeIfPresent(Int.self, forKey: .sourceCompactionSeq)
        automatic = try container.decodeIfPresent(Bool.self, forKey: .automatic) ?? false
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

extension ContextCheckpointListResult {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decode(String.self, forKey: .sessionID)
        checkpoints = try container.decodeIfPresent([ContextCheckpoint].self, forKey: .checkpoints) ?? []
        latestID = try container.decodeIfPresent(String.self, forKey: .latestID)
        policy = try container.decodeIfPresent(ContextCheckpointPolicy.self, forKey: .policy)
            ?? ContextCheckpointPolicy(enabled: false, maxCheckpoints: 20, maxAgeDays: 30)
        limit = try container.decodeIfPresent(Int.self, forKey: .limit) ?? 50
    }
}

extension ContextRestorePreflight {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        restorable = try container.decodeIfPresent(Bool.self, forKey: .restorable) ?? false
        errors = try container.decodeIfPresent([String].self, forKey: .errors) ?? []
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
        estimatedTokens = try container.decodeIfPresent(Int.self, forKey: .estimatedTokens) ?? 0
        hardLimitTokens = try container.decodeIfPresent(Int.self, forKey: .hardLimitTokens) ?? 0
        thresholdTokens = try container.decodeIfPresent(Int.self, forKey: .thresholdTokens) ?? 0
        activeMessages = try container.decodeIfPresent(Int.self, forKey: .activeMessages) ?? 0
        planRevision = try container.decodeIfPresent(Int.self, forKey: .planRevision) ?? 0
        checkpointPlanRevision = try container.decodeIfPresent(Int.self, forKey: .checkpointPlanRevision) ?? 0
        provider = try container.decodeIfPresent(String.self, forKey: .provider) ?? ""
        model = try container.decodeIfPresent(String.self, forKey: .model) ?? ""
        worktreeHead = try container.decodeIfPresent(String.self, forKey: .worktreeHead) ?? ""
        currentWorktreeHead = try container.decodeIfPresent(String.self, forKey: .currentWorktreeHead)
        worktreeDescendant = try container.decodeIfPresent(Bool.self, forKey: .worktreeDescendant) ?? false
        budgetExhausted = try container.decodeIfPresent(Bool.self, forKey: .budgetExhausted) ?? false
    }
}

extension ContextCheckpointDetailResult {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        checkpoint = try container.decode(ContextCheckpoint.self, forKey: .checkpoint)
        preflight = try container.decode(ContextRestorePreflight.self, forKey: .preflight)
        isActive = try container.decodeIfPresent(Bool.self, forKey: .isActive) ?? false
        summaryPreview = try container.decodeIfPresent(String.self, forKey: .summaryPreview) ?? ""
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
    let nextAttemptAt: String?

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
        case nextAttemptAt = "next_attempt_at"
    }
}

struct TaskSchedulerPolicy: Codable, Hashable, Sendable {
    var enabled: Bool
    var cooldownSecs: UInt64
    var dispatchTasks: Bool
    var includeIntake: Bool
    var maxIntakeParallel: Int

    static let disabled = TaskSchedulerPolicy(
        enabled: false,
        cooldownSecs: 60,
        dispatchTasks: false,
        includeIntake: false,
        maxIntakeParallel: 1
    )

    init(
        enabled: Bool,
        cooldownSecs: UInt64,
        dispatchTasks: Bool,
        includeIntake: Bool = true,
        maxIntakeParallel: Int = 2
    ) {
        self.enabled = enabled
        self.cooldownSecs = cooldownSecs
        self.dispatchTasks = dispatchTasks
        self.includeIntake = includeIntake
        self.maxIntakeParallel = maxIntakeParallel
    }

    enum CodingKeys: String, CodingKey {
        case enabled
        case cooldownSecs = "cooldown_secs"
        case dispatchTasks = "dispatch_tasks"
        case includeIntake = "include_intake"
        case maxIntakeParallel = "max_intake_parallel"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        cooldownSecs = try container.decodeIfPresent(UInt64.self, forKey: .cooldownSecs) ?? 60
        dispatchTasks = try container.decodeIfPresent(Bool.self, forKey: .dispatchTasks) ?? false
        includeIntake = try container.decodeIfPresent(Bool.self, forKey: .includeIntake) ?? true
        maxIntakeParallel = try container.decodeIfPresent(Int.self, forKey: .maxIntakeParallel) ?? 2
    }
}

struct ScalingPolicy: Codable, Hashable, Sendable {
    var enabled: Bool
    var minWorkers: Int
    var maxWorkers: Int
    var minLabour: Int
    var maxLabour: Int
    var expandReadyThreshold: Int
    var expandCriticalDelaySecs: UInt64
    var scaleDownIdleSecs: UInt64
    var cooldownSecs: UInt64
    var maxStep: Int
    var budgetHeadroomRatio: Double?

    static let disabled = ScalingPolicy(
        enabled: false,
        minWorkers: 1,
        maxWorkers: 4,
        minLabour: 1,
        maxLabour: 8,
        expandReadyThreshold: 2,
        expandCriticalDelaySecs: 120,
        scaleDownIdleSecs: 600,
        cooldownSecs: 120,
        maxStep: 2,
        budgetHeadroomRatio: nil
    )

    init(
        enabled: Bool,
        minWorkers: Int,
        maxWorkers: Int,
        minLabour: Int,
        maxLabour: Int,
        expandReadyThreshold: Int,
        expandCriticalDelaySecs: UInt64,
        scaleDownIdleSecs: UInt64,
        cooldownSecs: UInt64,
        maxStep: Int,
        budgetHeadroomRatio: Double?
    ) {
        self.enabled = enabled
        self.minWorkers = minWorkers
        self.maxWorkers = maxWorkers
        self.minLabour = minLabour
        self.maxLabour = maxLabour
        self.expandReadyThreshold = expandReadyThreshold
        self.expandCriticalDelaySecs = expandCriticalDelaySecs
        self.scaleDownIdleSecs = scaleDownIdleSecs
        self.cooldownSecs = cooldownSecs
        self.maxStep = maxStep
        self.budgetHeadroomRatio = budgetHeadroomRatio
    }

    enum CodingKeys: String, CodingKey {
        case enabled
        case minWorkers = "min_workers"
        case maxWorkers = "max_workers"
        case minLabour = "min_labour"
        case maxLabour = "max_labour"
        case expandReadyThreshold = "expand_ready_threshold"
        case expandCriticalDelaySecs = "expand_critical_delay_secs"
        case scaleDownIdleSecs = "scale_down_idle_secs"
        case cooldownSecs = "cooldown_secs"
        case maxStep = "max_step"
        case budgetHeadroomRatio = "budget_headroom_ratio"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        minWorkers = try container.decodeIfPresent(Int.self, forKey: .minWorkers) ?? 1
        maxWorkers = try container.decodeIfPresent(Int.self, forKey: .maxWorkers) ?? 4
        minLabour = try container.decodeIfPresent(Int.self, forKey: .minLabour) ?? 1
        maxLabour = try container.decodeIfPresent(Int.self, forKey: .maxLabour) ?? 8
        expandReadyThreshold = try container.decodeIfPresent(
            Int.self,
            forKey: .expandReadyThreshold
        ) ?? 2
        expandCriticalDelaySecs = try container.decodeIfPresent(
            UInt64.self,
            forKey: .expandCriticalDelaySecs
        ) ?? 120
        scaleDownIdleSecs = try container.decodeIfPresent(
            UInt64.self,
            forKey: .scaleDownIdleSecs
        ) ?? 600
        cooldownSecs = try container.decodeIfPresent(UInt64.self, forKey: .cooldownSecs) ?? 120
        maxStep = try container.decodeIfPresent(Int.self, forKey: .maxStep) ?? 2
        budgetHeadroomRatio = try container.decodeIfPresent(
            Double.self,
            forKey: .budgetHeadroomRatio
        )
    }

    var summary: String {
        let state = enabled ? "enabled" : "disabled"
        return "\(state) · Workers \(minWorkers)-\(maxWorkers) · Labour \(minLabour)-\(maxLabour) · threshold \(expandReadyThreshold) · step \(maxStep)"
    }

    var jsonValue: JSONValue {
        .object([
            "enabled": .bool(enabled),
            "min_workers": .number(Double(minWorkers)),
            "max_workers": .number(Double(maxWorkers)),
            "min_labour": .number(Double(minLabour)),
            "max_labour": .number(Double(maxLabour)),
            "expand_ready_threshold": .number(Double(expandReadyThreshold)),
            "expand_critical_delay_secs": .number(Double(expandCriticalDelaySecs)),
            "scale_down_idle_secs": .number(Double(scaleDownIdleSecs)),
            "cooldown_secs": .number(Double(cooldownSecs)),
            "max_step": .number(Double(maxStep)),
            "budget_headroom_ratio": budgetHeadroomRatio.map { .number($0) } ?? .null,
        ])
    }
}

struct ScalingDecision: Codable, Identifiable, Hashable, Sendable {
    var id: String {
        decisionID.isEmpty ? decisionKey : decisionID
    }

    let decisionID: String
    let decisionKey: String
    let sessionID: String
    let projectID: String
    let planRevision: UInt64
    let action: String
    let currentWorkers: Int
    let currentLabour: Int
    let targetWorkers: Int
    let targetLabour: Int
    let activeWorkers: Int
    let activeLabour: Int
    let readyTasks: Int
    let runningTasks: Int
    let inputsHash: String
    let reasons: [String]
    let managerRecommended: Bool
    let managerEvidence: String?
    let applied: Bool
    let idleSinceWorkers: String?
    let idleSinceLabour: String?
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case decisionID = "decision_id"
        case decisionKey = "decision_key"
        case sessionID = "session_id"
        case projectID = "project_id"
        case planRevision = "plan_revision"
        case action
        case currentWorkers = "current_workers"
        case currentLabour = "current_labour"
        case targetWorkers = "target_workers"
        case targetLabour = "target_labour"
        case activeWorkers = "active_workers"
        case activeLabour = "active_labour"
        case readyTasks = "ready_tasks"
        case runningTasks = "running_tasks"
        case inputsHash = "inputs_hash"
        case reasons
        case managerRecommended = "manager_recommended"
        case managerEvidence = "manager_evidence"
        case applied
        case idleSinceWorkers = "idle_since_workers"
        case idleSinceLabour = "idle_since_labour"
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        decisionID = try container.decodeIfPresent(String.self, forKey: .decisionID) ?? ""
        decisionKey = try container.decodeIfPresent(String.self, forKey: .decisionKey) ?? ""
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        planRevision = try container.decodeIfPresent(UInt64.self, forKey: .planRevision) ?? 0
        action = try container.decodeIfPresent(String.self, forKey: .action) ?? "hold"
        currentWorkers = try container.decodeIfPresent(Int.self, forKey: .currentWorkers) ?? 0
        currentLabour = try container.decodeIfPresent(Int.self, forKey: .currentLabour) ?? 0
        targetWorkers = try container.decodeIfPresent(Int.self, forKey: .targetWorkers) ?? 0
        targetLabour = try container.decodeIfPresent(Int.self, forKey: .targetLabour) ?? 0
        activeWorkers = try container.decodeIfPresent(Int.self, forKey: .activeWorkers) ?? 0
        activeLabour = try container.decodeIfPresent(Int.self, forKey: .activeLabour) ?? 0
        readyTasks = try container.decodeIfPresent(Int.self, forKey: .readyTasks) ?? 0
        runningTasks = try container.decodeIfPresent(Int.self, forKey: .runningTasks) ?? 0
        inputsHash = try container.decodeIfPresent(String.self, forKey: .inputsHash) ?? ""
        reasons = try container.decodeIfPresent([String].self, forKey: .reasons) ?? []
        managerRecommended = try container.decodeIfPresent(
            Bool.self,
            forKey: .managerRecommended
        ) ?? false
        managerEvidence = try container.decodeIfPresent(String.self, forKey: .managerEvidence)
        applied = try container.decodeIfPresent(Bool.self, forKey: .applied) ?? false
        idleSinceWorkers = try container.decodeIfPresent(
            String.self,
            forKey: .idleSinceWorkers
        )
        idleSinceLabour = try container.decodeIfPresent(String.self, forKey: .idleSinceLabour)
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }

    var isExpansion: Bool {
        action == "expand_workers" || action == "expand_labour"
            || action == "expand_workers_and_labour"
    }

    var isScaleDown: Bool {
        action == "scale_down_labour" || action == "scale_down_workers"
    }

    var summary: String {
        "\(action) · Workers \(currentWorkers)→\(targetWorkers) · Labour \(currentLabour)→\(targetLabour)"
    }
}

struct ScalingPolicyResult: Codable, Sendable {
    let projectID: String
    let policy: ScalingPolicy

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case policy
    }
}

struct ScalingPreviewResult: Codable, Sendable {
    let sessionID: String
    let projectID: String
    let policy: ScalingPolicy
    let decision: ScalingDecision

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case projectID = "project_id"
        case policy
        case decision
    }
}

struct ScalingSessionStatus: Codable, Identifiable, Hashable, Sendable {
    var id: String {
        sessionID
    }

    let sessionID: String
    let projectID: String
    let goal: String
    let status: String
    let planRevision: UInt64
    let planTasks: Int
    let readyTasks: Int
    let runningTasks: Int
    let completedTasks: Int
    let currentWorkers: Int
    let currentLabour: Int
    let activeWorkers: Int
    let activeLabour: Int
    let latest: ScalingDecision?
    let idleSinceWorkers: String?
    let idleSinceLabour: String?

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case projectID = "project_id"
        case goal
        case status
        case planRevision = "plan_revision"
        case planTasks = "plan_tasks"
        case readyTasks = "ready_tasks"
        case runningTasks = "running_tasks"
        case completedTasks = "completed_tasks"
        case currentWorkers = "current_workers"
        case currentLabour = "current_labour"
        case activeWorkers = "active_workers"
        case activeLabour = "active_labour"
        case latest
        case idleSinceWorkers = "idle_since_workers"
        case idleSinceLabour = "idle_since_labour"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decode(String.self, forKey: .sessionID)
        projectID = try container.decode(String.self, forKey: .projectID)
        goal = try container.decodeIfPresent(String.self, forKey: .goal) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? "idle"
        planRevision = try container.decodeIfPresent(UInt64.self, forKey: .planRevision) ?? 0
        planTasks = try container.decodeIfPresent(Int.self, forKey: .planTasks) ?? 0
        readyTasks = try container.decodeIfPresent(Int.self, forKey: .readyTasks) ?? 0
        runningTasks = try container.decodeIfPresent(Int.self, forKey: .runningTasks) ?? 0
        completedTasks = try container.decodeIfPresent(Int.self, forKey: .completedTasks) ?? 0
        currentWorkers = try container.decodeIfPresent(Int.self, forKey: .currentWorkers) ?? 0
        currentLabour = try container.decodeIfPresent(Int.self, forKey: .currentLabour) ?? 0
        activeWorkers = try container.decodeIfPresent(Int.self, forKey: .activeWorkers) ?? 0
        activeLabour = try container.decodeIfPresent(Int.self, forKey: .activeLabour) ?? 0
        latest = try container.decodeIfPresent(ScalingDecision.self, forKey: .latest)
        idleSinceWorkers = try container.decodeIfPresent(
            String.self,
            forKey: .idleSinceWorkers
        )
        idleSinceLabour = try container.decodeIfPresent(String.self, forKey: .idleSinceLabour)
    }
}

struct ScalingProjectStatus: Codable, Identifiable, Hashable, Sendable {
    var id: String {
        projectID
    }

    let projectID: String
    let projectName: String
    let policy: ScalingPolicy
    let sessions: [ScalingSessionStatus]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case projectName = "project_name"
        case policy
        case sessions
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decode(String.self, forKey: .projectID)
        projectName = try container.decodeIfPresent(String.self, forKey: .projectName) ?? ""
        policy = try container.decodeIfPresent(ScalingPolicy.self, forKey: .policy) ?? .disabled
        sessions = try container.decodeIfPresent([ScalingSessionStatus].self, forKey: .sessions)
            ?? []
    }
}

struct ScalingStatusResult: Codable, Sendable {
    let globalWorkerCap: Int
    let globalLabourCap: Int
    let globalWorkerAvailable: Int
    let globalLabourAvailable: Int
    let projects: [ScalingProjectStatus]

    enum CodingKeys: String, CodingKey {
        case globalWorkerCap = "global_worker_cap"
        case globalLabourCap = "global_labour_cap"
        case globalWorkerAvailable = "global_worker_available"
        case globalLabourAvailable = "global_labour_available"
        case projects
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        globalWorkerCap = try container.decodeIfPresent(Int.self, forKey: .globalWorkerCap) ?? 0
        globalLabourCap = try container.decodeIfPresent(Int.self, forKey: .globalLabourCap) ?? 0
        globalWorkerAvailable = try container.decodeIfPresent(
            Int.self,
            forKey: .globalWorkerAvailable
        ) ?? 0
        globalLabourAvailable = try container.decodeIfPresent(
            Int.self,
            forKey: .globalLabourAvailable
        ) ?? 0
        projects = try container.decodeIfPresent([ScalingProjectStatus].self, forKey: .projects)
            ?? []
    }
}

struct IntakeTask: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectID: String
    let title: String
    let instructions: String
    let profile: String
    let kind: String
    let priority: Int
    let writeScope: [String]
    let acceptance: [String]
    let deps: [String]
    let tokenBudget: UInt64?
    let costBudgetUsd: Double?
    let timeBudgetSecs: UInt64?
    let status: String
    let attempts: UInt32
    let maxAttempts: UInt32
    let nextAttemptAt: String?
    let sessionID: String?
    let taskID: String?
    let lastError: String?
    let resultSummary: String?
    let origin: String
    let labels: [String]
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case projectID = "project_id"
        case title
        case instructions
        case profile
        case kind
        case priority
        case writeScope = "write_scope"
        case acceptance
        case deps
        case tokenBudget = "token_budget"
        case costBudgetUsd = "cost_budget_usd"
        case timeBudgetSecs = "time_budget_secs"
        case status
        case attempts
        case maxAttempts = "max_attempts"
        case nextAttemptAt = "next_attempt_at"
        case sessionID = "session_id"
        case taskID = "task_id"
        case lastError = "last_error"
        case resultSummary = "result_summary"
        case origin
        case labels
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectID = try container.decode(String.self, forKey: .projectID)
        title = try container.decode(String.self, forKey: .title)
        instructions = try container.decode(String.self, forKey: .instructions)
        profile = try container.decode(String.self, forKey: .profile)
        kind = try container.decode(String.self, forKey: .kind)
        priority = try container.decode(Int.self, forKey: .priority)
        writeScope = try container.decodeIfPresent([String].self, forKey: .writeScope) ?? []
        acceptance = try container.decodeIfPresent([String].self, forKey: .acceptance) ?? []
        deps = try container.decodeIfPresent([String].self, forKey: .deps) ?? []
        tokenBudget = try container.decodeIfPresent(UInt64.self, forKey: .tokenBudget)
        costBudgetUsd = try container.decodeIfPresent(Double.self, forKey: .costBudgetUsd)
        timeBudgetSecs = try container.decodeIfPresent(UInt64.self, forKey: .timeBudgetSecs)
        status = try container.decode(String.self, forKey: .status)
        attempts = try container.decode(UInt32.self, forKey: .attempts)
        maxAttempts = try container.decode(UInt32.self, forKey: .maxAttempts)
        nextAttemptAt = try container.decodeIfPresent(String.self, forKey: .nextAttemptAt)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID)
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
        resultSummary = try container.decodeIfPresent(String.self, forKey: .resultSummary)
        origin = try container.decodeIfPresent(String.self, forKey: .origin) ?? "unknown"
        labels = try container.decodeIfPresent([String].self, forKey: .labels) ?? []
        createdAt = try container.decode(String.self, forKey: .createdAt)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
    }

    var isActive: Bool {
        status == "running" || status == "dispatching"
    }

    var isTerminal: Bool {
        ["completed", "failed", "cancelled"].contains(status)
    }
}

struct IntakeListResult: Codable, Sendable {
    let tasks: [IntakeTask]
}

struct IntakeDispatchResult: Codable, Sendable {
    let started: Bool
    let task: IntakeTask
    let sessionID: String?

    enum CodingKeys: String, CodingKey {
        case started
        case task
        case sessionID = "session_id"
    }
}

struct IntakeDeleteResult: Codable, Sendable {
    let deleted: Bool
}

struct ProgramPolicy: Codable, Hashable, Sendable {
    var autoDispatch: Bool
    var maxAttempts: UInt32
    var retryBackoffSecs: UInt64
    var dependencyFailure: String
    var tokenBudget: UInt64?
    var costBudgetUsd: Double?
    var maxParallelMilestones: Int

    static let draft = ProgramPolicy(
        autoDispatch: false,
        maxAttempts: 2,
        retryBackoffSecs: 60,
        dependencyFailure: "block",
        tokenBudget: nil,
        costBudgetUsd: nil,
        maxParallelMilestones: 2
    )

    init(
        autoDispatch: Bool,
        maxAttempts: UInt32,
        retryBackoffSecs: UInt64,
        dependencyFailure: String,
        tokenBudget: UInt64?,
        costBudgetUsd: Double?,
        maxParallelMilestones: Int
    ) {
        self.autoDispatch = autoDispatch
        self.maxAttempts = maxAttempts
        self.retryBackoffSecs = retryBackoffSecs
        self.dependencyFailure = dependencyFailure
        self.tokenBudget = tokenBudget
        self.costBudgetUsd = costBudgetUsd
        self.maxParallelMilestones = maxParallelMilestones
    }

    enum CodingKeys: String, CodingKey {
        case autoDispatch = "auto_dispatch"
        case maxAttempts = "max_attempts"
        case retryBackoffSecs = "retry_backoff_secs"
        case dependencyFailure = "dependency_failure"
        case tokenBudget = "token_budget"
        case costBudgetUsd = "cost_budget_usd"
        case maxParallelMilestones = "max_parallel_milestones"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        autoDispatch = try container.decodeIfPresent(Bool.self, forKey: .autoDispatch) ?? false
        maxAttempts = try container.decodeIfPresent(UInt32.self, forKey: .maxAttempts) ?? 2
        retryBackoffSecs = try container.decodeIfPresent(UInt64.self, forKey: .retryBackoffSecs) ?? 60
        dependencyFailure = try container.decodeIfPresent(String.self, forKey: .dependencyFailure)
            ?? "block"
        tokenBudget = try container.decodeIfPresent(UInt64.self, forKey: .tokenBudget)
        costBudgetUsd = try container.decodeIfPresent(Double.self, forKey: .costBudgetUsd)
        maxParallelMilestones = try container.decodeIfPresent(
            Int.self,
            forKey: .maxParallelMilestones
        ) ?? 2
    }

    var jsonValue: JSONValue {
        .object([
            "auto_dispatch": .bool(autoDispatch),
            "max_attempts": .number(Double(maxAttempts)),
            "retry_backoff_secs": .number(Double(retryBackoffSecs)),
            "dependency_failure": .string(dependencyFailure),
            "token_budget": tokenBudget.map { .number(Double($0)) } ?? .null,
            "cost_budget_usd": costBudgetUsd.map { .number($0) } ?? .null,
            "max_parallel_milestones": .number(Double(maxParallelMilestones)),
        ])
    }
}

struct ProjectProgram: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectID: String
    let goal: String
    let status: String
    let policy: ProgramPolicy
    let tokenBudget: UInt64?
    let costBudgetUsd: Double?
    let reason: String?
    let archived: Bool
    let createdAt: String
    let updatedAt: String
    let completedAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case projectID = "project_id"
        case goal
        case status
        case policy
        case tokenBudget = "token_budget"
        case costBudgetUsd = "cost_budget_usd"
        case reason
        case archived
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case completedAt = "completed_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectID = try container.decode(String.self, forKey: .projectID)
        goal = try container.decode(String.self, forKey: .goal)
        status = try container.decode(String.self, forKey: .status)
        policy = try container.decodeIfPresent(ProgramPolicy.self, forKey: .policy) ?? .draft
        tokenBudget = try container.decodeIfPresent(UInt64.self, forKey: .tokenBudget)
        costBudgetUsd = try container.decodeIfPresent(Double.self, forKey: .costBudgetUsd)
        reason = try container.decodeIfPresent(String.self, forKey: .reason)
        archived = try container.decodeIfPresent(Bool.self, forKey: .archived) ?? false
        createdAt = try container.decode(String.self, forKey: .createdAt)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
        completedAt = try container.decodeIfPresent(String.self, forKey: .completedAt)
    }

    var isActive: Bool {
        status == "active"
    }

    var isTerminal: Bool {
        status == "completed" || status == "failed"
    }

    var canPause: Bool {
        isActive && !archived
    }

    var canResume: Bool {
        (status == "paused" || status == "draft") && !archived
    }

    var canDelete: Bool {
        !archived && (status == "draft" || isTerminal)
    }
}

struct ProgramMilestone: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let programID: String
    let key: String
    let title: String
    let objective: String
    let acceptance: [String]
    let deps: [String]
    let priority: Int
    let status: String
    let sessionID: String?
    let planRevision: UInt32?
    let attempts: UInt32
    let maxAttempts: UInt32
    let nextAttemptAt: String?
    let result: String?
    let lastError: String?
    let profile: String
    let writeScope: [String]
    let tokenBudget: UInt64?
    let costBudgetUsd: Double?
    let createdAt: String
    let updatedAt: String
    let completedAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case programID = "program_id"
        case key
        case title
        case objective
        case acceptance
        case deps
        case priority
        case status
        case sessionID = "session_id"
        case planRevision = "plan_revision"
        case attempts
        case maxAttempts = "max_attempts"
        case nextAttemptAt = "next_attempt_at"
        case result
        case lastError = "last_error"
        case profile
        case writeScope = "write_scope"
        case tokenBudget = "token_budget"
        case costBudgetUsd = "cost_budget_usd"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case completedAt = "completed_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        programID = try container.decode(String.self, forKey: .programID)
        key = try container.decode(String.self, forKey: .key)
        title = try container.decode(String.self, forKey: .title)
        objective = try container.decode(String.self, forKey: .objective)
        acceptance = try container.decodeIfPresent([String].self, forKey: .acceptance) ?? []
        deps = try container.decodeIfPresent([String].self, forKey: .deps) ?? []
        priority = try container.decodeIfPresent(Int.self, forKey: .priority) ?? 0
        status = try container.decode(String.self, forKey: .status)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID)
        planRevision = try container.decodeIfPresent(UInt32.self, forKey: .planRevision)
        attempts = try container.decodeIfPresent(UInt32.self, forKey: .attempts) ?? 0
        maxAttempts = try container.decodeIfPresent(UInt32.self, forKey: .maxAttempts) ?? 2
        nextAttemptAt = try container.decodeIfPresent(String.self, forKey: .nextAttemptAt)
        result = try container.decodeIfPresent(String.self, forKey: .result)
        lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
        profile = try container.decodeIfPresent(String.self, forKey: .profile) ?? "coding"
        writeScope = try container.decodeIfPresent([String].self, forKey: .writeScope) ?? []
        tokenBudget = try container.decodeIfPresent(UInt64.self, forKey: .tokenBudget)
        costBudgetUsd = try container.decodeIfPresent(Double.self, forKey: .costBudgetUsd)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
        completedAt = try container.decodeIfPresent(String.self, forKey: .completedAt)
    }

    var isActive: Bool {
        status == "running" || status == "verifying"
    }

    var isTerminal: Bool {
        ["completed", "blocked", "cancelled"].contains(status)
    }

    var canRetry: Bool {
        ["failed", "blocked", "cancelled"].contains(status)
    }

    var canStart: Bool {
        ["pending", "ready", "failed"].contains(status) && attempts < maxAttempts
    }

    var canSkip: Bool {
        !isTerminal && !isActive
    }
}

struct ProgramMilestoneCounts: Codable, Hashable, Sendable {
    let pending: Int
    let ready: Int
    let running: Int
    let verifying: Int
    let completed: Int
    let failed: Int
    let blocked: Int
    let cancelled: Int

    enum CodingKeys: String, CodingKey {
        case pending
        case ready
        case running
        case verifying
        case completed
        case failed
        case blocked
        case cancelled
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        pending = try container.decodeIfPresent(Int.self, forKey: .pending) ?? 0
        ready = try container.decodeIfPresent(Int.self, forKey: .ready) ?? 0
        running = try container.decodeIfPresent(Int.self, forKey: .running) ?? 0
        verifying = try container.decodeIfPresent(Int.self, forKey: .verifying) ?? 0
        completed = try container.decodeIfPresent(Int.self, forKey: .completed) ?? 0
        failed = try container.decodeIfPresent(Int.self, forKey: .failed) ?? 0
        blocked = try container.decodeIfPresent(Int.self, forKey: .blocked) ?? 0
        cancelled = try container.decodeIfPresent(Int.self, forKey: .cancelled) ?? 0
    }

    var total: Int {
        pending + ready + running + verifying + completed + failed + blocked + cancelled
    }

    var active: Int {
        running + verifying
    }
}

struct ProgramListEntry: Codable, Identifiable, Hashable, Sendable {
    let program: ProjectProgram
    let counts: ProgramMilestoneCounts

    var id: String {
        program.id
    }

    enum CodingKeys: String, CodingKey {
        case program
        case counts
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        program = try container.decode(ProjectProgram.self, forKey: .program)
        counts = try container.decodeIfPresent(
            ProgramMilestoneCounts.self,
            forKey: .counts
        ) ?? ProgramMilestoneCounts.empty
    }
}

extension ProgramMilestoneCounts {
    static let empty = try! JSONDecoder().decode(
        ProgramMilestoneCounts.self,
        from: Data("{}".utf8)
    )
}

struct ProgramListResult: Codable, Sendable {
    let programs: [ProgramListEntry]
}

struct ProgramResult: Codable, Sendable {
    let program: ProjectProgram
}

struct ProgramMilestoneResult: Codable, Sendable {
    let milestone: ProgramMilestone
}

struct ProgramDeleteResult: Codable, Sendable {
    let deleted: Bool
}

struct ProgramUsageSummary: Codable, Hashable, Sendable {
    let tokenSpent: UInt64
    let costSpentUsd: Double

    enum CodingKeys: String, CodingKey {
        case tokenSpent = "token_spent"
        case costSpentUsd = "cost_spent_usd"
    }

    init(tokenSpent: UInt64, costSpentUsd: Double) {
        self.tokenSpent = tokenSpent
        self.costSpentUsd = costSpentUsd
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        tokenSpent = try container.decodeIfPresent(UInt64.self, forKey: .tokenSpent) ?? 0
        costSpentUsd = try container.decodeIfPresent(Double.self, forKey: .costSpentUsd) ?? 0
    }
}

struct ProgramEventRecord: Codable, Hashable, Identifiable, Sendable {
    let programID: String
    let projectID: String
    let seq: UInt64
    let type: String
    let createdAt: String

    var id: String {
        "\(programID)-\(seq)"
    }

    enum CodingKeys: String, CodingKey {
        case programID = "program_id"
        case projectID = "project_id"
        case seq
        case type
        case createdAt = "created_at"
    }
}

struct ProgramStatusResult: Codable, Sendable {
    let program: ProjectProgram
    let milestones: [ProgramMilestone]
    let counts: ProgramMilestoneCounts
    let usage: ProgramUsageSummary
    let events: [ProgramEventRecord]

    enum CodingKeys: String, CodingKey {
        case program
        case milestones
        case counts
        case usage
        case events
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        program = try container.decode(ProjectProgram.self, forKey: .program)
        milestones = try container.decodeIfPresent([ProgramMilestone].self, forKey: .milestones) ?? []
        counts = try container.decodeIfPresent(
            ProgramMilestoneCounts.self,
            forKey: .counts
        ) ?? ProgramMilestoneCounts.empty
        usage = try container.decodeIfPresent(
            ProgramUsageSummary.self,
            forKey: .usage
        ) ?? ProgramUsageSummary(tokenSpent: 0, costSpentUsd: 0)
        events = try container.decodeIfPresent([ProgramEventRecord].self, forKey: .events) ?? []
    }
}

struct ProgramReplayResult: Codable, Sendable {
    let programID: String
    let eventCount: Int
    let programMatches: Bool
    let milestonesMatch: Bool
    let consistent: Bool
    let repaired: Bool
    let mismatches: [String]

    enum CodingKeys: String, CodingKey {
        case programID = "program_id"
        case eventCount = "event_count"
        case programMatches = "program_matches"
        case milestonesMatch = "milestones_match"
        case consistent
        case repaired
        case mismatches
    }
}

struct ProgramTickResult: Codable, Sendable {
    let programs: Int
    let milestonesAdvanced: Int
    let sessionsStarted: Int
    let completed: Int
    let failed: Int
    let ready: [ProgramMilestone]

    enum CodingKeys: String, CodingKey {
        case programs
        case milestonesAdvanced = "milestones_advanced"
        case sessionsStarted = "sessions_started"
        case completed
        case failed
        case ready
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

struct SchedulerProjectStatus: Codable, Hashable, Sendable, Identifiable {
    let projectID: String
    let projectName: String
    let enabled: Bool
    let cooldownSecs: UInt64
    let dispatchTasks: Bool
    let includeIntake: Bool
    let maxIntakeParallel: Int
    let pendingSessions: Int
    let pendingPlanTasks: Int
    let runningPlanTasks: Int
    let queuedIntake: Int
    let activeIntake: Int
    let failedIntake: Int

    var id: String {
        projectID
    }

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case projectName = "project_name"
        case enabled
        case cooldownSecs = "cooldown_secs"
        case dispatchTasks = "dispatch_tasks"
        case includeIntake = "include_intake"
        case maxIntakeParallel = "max_intake_parallel"
        case pendingSessions = "pending_sessions"
        case pendingPlanTasks = "pending_plan_tasks"
        case runningPlanTasks = "running_plan_tasks"
        case queuedIntake = "queued_intake"
        case activeIntake = "active_intake"
        case failedIntake = "failed_intake"
    }
}

struct SchedulerStatusResult: Codable, Sendable {
    let activeSessions: Int
    let queuedSessions: Int
    let pausedSessions: Int
    let pendingPlanTasks: Int
    let runningPlanTasks: Int
    let queuedIntake: Int
    let activeIntake: Int
    let failedIntake: Int
    let globalSessionCap: Int
    let globalWorkerCap: Int
    let globalLabourCap: Int
    let projects: [SchedulerProjectStatus]

    enum CodingKeys: String, CodingKey {
        case activeSessions = "active_sessions"
        case queuedSessions = "queued_sessions"
        case pausedSessions = "paused_sessions"
        case pendingPlanTasks = "pending_plan_tasks"
        case runningPlanTasks = "running_plan_tasks"
        case queuedIntake = "queued_intake"
        case activeIntake = "active_intake"
        case failedIntake = "failed_intake"
        case globalSessionCap = "global_session_cap"
        case globalWorkerCap = "global_worker_cap"
        case globalLabourCap = "global_labour_cap"
        case projects
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

struct SupervisedProcess: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sessionId: String
    let name: String
    let command: String
    let argv: [String]
    let cwd: String
    let worktree: String
    let envHash: String
    let commandHash: String
    let state: String
    let pid: Int?
    let pgid: Int?
    let nonce: String
    let startedAt: String
    let processStartedAt: String?
    let heartbeatAt: String
    let finishedAt: String?
    let exitCode: Int?
    let signal: Int?
    let outputPath: String
    let outputBytes: UInt64
    let outputLines: UInt64
    let totalOutputBytes: UInt64
    let outputRotations: UInt64
    let maxOutputBytes: UInt64
    let maxOutputLines: UInt64
    let ownerInstance: String
    let retentionDeadline: String?
    let idleTtlSecs: UInt64
    let lastAccessAt: String
    let detail: String?

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case name
        case command
        case argv
        case cwd
        case worktree
        case envHash = "env_hash"
        case commandHash = "command_hash"
        case state
        case pid
        case pgid
        case nonce
        case startedAt = "started_at"
        case processStartedAt = "process_started_at"
        case heartbeatAt = "heartbeat_at"
        case finishedAt = "finished_at"
        case exitCode = "exit_code"
        case signal
        case outputPath = "output_path"
        case outputBytes = "output_bytes"
        case outputLines = "output_lines"
        case totalOutputBytes = "total_output_bytes"
        case outputRotations = "output_rotations"
        case maxOutputBytes = "max_output_bytes"
        case maxOutputLines = "max_output_lines"
        case ownerInstance = "owner_instance"
        case retentionDeadline = "retention_deadline"
        case idleTtlSecs = "idle_ttl_secs"
        case lastAccessAt = "last_access_at"
        case detail
    }
}

struct ProcessListResult: Codable, Sendable {
    let sessionId: String
    let processes: [SupervisedProcess]
    let active: UInt32
    let limit: UInt32

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case processes
        case active
        case limit
    }
}

struct ProcessLogsResult: Codable, Sendable {
    let sessionId: String
    let processId: String
    let state: String
    let content: String
    let offset: UInt64
    let nextOffset: UInt64
    let totalBytes: UInt64
    let eof: Bool
    let truncated: Bool
    let rotated: Bool

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case processId = "process_id"
        case state
        case content
        case offset
        case nextOffset = "next_offset"
        case totalBytes = "total_bytes"
        case eof
        case truncated
        case rotated
    }
}

struct ProcessStopResult: Codable, Sendable {
    let process: SupervisedProcess
    let forced: Bool
    let waitedMs: UInt64

    enum CodingKeys: String, CodingKey {
        case process
        case forced
        case waitedMs = "waited_ms"
    }
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
    let identityId: String?
    let workspaceId: String?
    let parentWorkspaceId: String?
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
        case identityId = "identity_id"
        case workspaceId = "workspace_id"
        case parentWorkspaceId = "parent_workspace_id"
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

/// Read-only project identity row. S7 owns the full org chart, hire/retire
/// sheets, and identity detail; S1 decodes the durable roster for display only.
struct AgentIdentity: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let role: String
    let department: String?
    let component: String?
    let title: String?
    let profile: String?
    let model: String?
    let parentIdentityId: String?
    let status: String
    let generation: Int
    let createdAt: String
    let updatedAt: String
    let retiredAt: String?
    let successorId: String?
    let origin: String

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case role
        case department
        case component
        case title
        case profile
        case model
        case parentIdentityId = "parent_identity_id"
        case status
        case generation
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case retiredAt = "retired_at"
        case successorId = "successor_id"
        case origin
    }

    var displayName: String {
        if let title, !title.isEmpty {
            return title
        }
        if let component, !component.isEmpty {
            return component
        }
        return role.capitalized
    }

    var isImported: Bool {
        origin == "imported"
    }
}

struct AgentIdentitiesResult: Codable, Sendable {
    let identities: [AgentIdentity]
}

/// Read-only session→identity assignment metadata. The lease token is never on
/// the wire; `leaseActive` is a server-derived, time-relative hint.
struct AgentAssignment: Codable, Identifiable, Hashable, Sendable {
    let sessionId: String
    let projectId: String
    let identityId: String
    let role: String
    let status: String
    let assignedAt: String
    let releasedAt: String?
    let heartbeatAt: String
    let leaseExpiresAt: String?
    let leaseActive: Bool

    var id: String {
        "\(sessionId)#\(identityId)"
    }

    var isLive: Bool {
        status == "active" && leaseActive
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case projectId = "project_id"
        case identityId = "identity_id"
        case role
        case status
        case assignedAt = "assigned_at"
        case releasedAt = "released_at"
        case heartbeatAt = "heartbeat_at"
        case leaseExpiresAt = "lease_expires_at"
        case leaseActive = "lease_active"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        projectId = try container.decode(String.self, forKey: .projectId)
        identityId = try container.decode(String.self, forKey: .identityId)
        role = try container.decode(String.self, forKey: .role)
        status = try container.decode(String.self, forKey: .status)
        assignedAt = try container.decode(String.self, forKey: .assignedAt)
        releasedAt = try container.decodeIfPresent(String.self, forKey: .releasedAt)
        heartbeatAt = try container.decode(String.self, forKey: .heartbeatAt)
        leaseExpiresAt = try container.decodeIfPresent(String.self, forKey: .leaseExpiresAt)
        leaseActive = try container.decodeIfPresent(Bool.self, forKey: .leaseActive) ?? false
    }
}

struct SessionAssignmentsResult: Codable, Sendable {
    let assignments: [AgentAssignment]
}

/// Read-only Worker component workspace metadata. The lease token is never on
/// the wire; `leaseActive` is a server-derived hint. The client never attaches,
/// lands, archives, or retires a workspace from a view.
struct AgentWorkspace: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let identityId: String
    let component: String
    let branch: String
    let baseCommit: String
    let worktreePath: String
    let status: String
    let createdAt: String
    let updatedAt: String
    let lastSessionId: String?
    let leaseActive: Bool
    let leaseExpiresAt: String?
    let origin: String

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case identityId = "identity_id"
        case component
        case branch
        case baseCommit = "base_commit"
        case worktreePath = "worktree_path"
        case status
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case lastSessionId = "last_session_id"
        case leaseActive = "lease_active"
        case leaseExpiresAt = "lease_expires_at"
        case origin
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectId = try container.decode(String.self, forKey: .projectId)
        identityId = try container.decode(String.self, forKey: .identityId)
        component = try container.decode(String.self, forKey: .component)
        branch = try container.decode(String.self, forKey: .branch)
        baseCommit = try container.decodeIfPresent(String.self, forKey: .baseCommit) ?? ""
        worktreePath = try container.decodeIfPresent(String.self, forKey: .worktreePath) ?? ""
        status = try container.decode(String.self, forKey: .status)
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
        lastSessionId = try container.decodeIfPresent(String.self, forKey: .lastSessionId)
        leaseActive = try container.decodeIfPresent(Bool.self, forKey: .leaseActive) ?? false
        leaseExpiresAt = try container.decodeIfPresent(String.self, forKey: .leaseExpiresAt)
        origin = try container.decodeIfPresent(String.self, forKey: .origin) ?? "local"
    }

    var isImported: Bool {
        origin == "imported"
    }
}

/// Read-only session attachment of a project workspace.
struct SessionWorkerWorkspace: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let workspaceId: String
    let sessionId: String
    let identityId: String
    let worktreePath: String
    let branch: String
    let baseCommit: String
    let status: String
    let createdAt: String
    let updatedAt: String
    let leaseActive: Bool
    let leaseExpiresAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case workspaceId = "workspace_id"
        case sessionId = "session_id"
        case identityId = "identity_id"
        case worktreePath = "worktree_path"
        case branch
        case baseCommit = "base_commit"
        case status
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case leaseActive = "lease_active"
        case leaseExpiresAt = "lease_expires_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        workspaceId = try container.decode(String.self, forKey: .workspaceId)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        identityId = try container.decode(String.self, forKey: .identityId)
        worktreePath = try container.decodeIfPresent(String.self, forKey: .worktreePath) ?? ""
        branch = try container.decodeIfPresent(String.self, forKey: .branch) ?? ""
        baseCommit = try container.decodeIfPresent(String.self, forKey: .baseCommit) ?? ""
        status = try container.decode(String.self, forKey: .status)
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
        leaseActive = try container.decodeIfPresent(Bool.self, forKey: .leaseActive) ?? false
        leaseExpiresAt = try container.decodeIfPresent(String.self, forKey: .leaseExpiresAt)
    }
}

struct AgentWorkspaceListResult: Codable, Sendable {
    let workspaces: [AgentWorkspace]
    let sessions: [SessionWorkerWorkspace]

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        workspaces = try container.decodeIfPresent([AgentWorkspace].self, forKey: .workspaces) ?? []
        sessions = try container.decodeIfPresent([SessionWorkerWorkspace].self, forKey: .sessions) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case workspaces
        case sessions
    }
}

struct SessionWorkspaceResult: Codable, Sendable {
    let sessionId: String
    let bindings: [SessionWorkerWorkspace]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case bindings
    }
}

/// Known inbox payload fields. Unknown fields are ignored by the view and are
/// never interpreted as instructions or credentials.
struct AgentInboxPayload: Codable, Hashable, Sendable {
    let values: [String: JSONValue]

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        values = try container.decode([String: JSONValue].self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(values)
    }

    var displaySummary: String {
        for key in ["summary", "title", "body", "message", "question", "decision"] {
            if case let .string(value) = values[key], !value.isEmpty {
                return value
            }
        }
        return "…"
    }
}

/// Read-only project inbox row. Delivery is pull-based; no view logic sends,
/// acks, or wakes anything.
struct AgentInboxMessage: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let identityId: String
    let sessionId: String?
    let fromIdentityId: String?
    let fromLabel: String
    let kind: String
    let payload: AgentInboxPayload
    let status: String
    let createdAt: String
    let readAt: String?
    let expiresAt: String?

    var isUnread: Bool {
        status == "unread"
    }

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case identityId = "identity_id"
        case sessionId = "session_id"
        case fromIdentityId = "from_identity_id"
        case fromLabel = "from_label"
        case kind
        case payload
        case status
        case createdAt = "created_at"
        case readAt = "read_at"
        case expiresAt = "expires_at"
    }
}

struct AgentInboxCounts: Codable, Hashable, Sendable {
    let unread: Int
    let read: Int
    let done: Int
    let expired: Int
}

struct AgentInboxListResult: Codable, Sendable {
    let messages: [AgentInboxMessage]
    let counts: AgentInboxCounts
}

struct AgentCoordinationWake: Codable, Hashable, Sendable {
    let requested: Bool
    let created: Bool
    let continuationId: String?
    let reason: String

    enum CodingKeys: String, CodingKey {
        case requested
        case created
        case continuationId = "continuation_id"
        case reason
    }
}

struct AgentInboxSendResult: Codable, Sendable {
    let message: AgentInboxMessage
    let wake: AgentCoordinationWake
}

struct AgentInboxReadResult: Codable, Sendable {
    let message: AgentInboxMessage
    let changed: Bool
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
    let preMergeCommit: String?
    let status: String
    let attempt: Int
    let detail: String?
    let source: String?
    let remoteHost: String?
    let remoteBranch: String?
    let bundleSha256: String?
    let importedRef: String?
    let remoteTestEvidenceId: String?
    let remoteTestDigest: String?
    let remoteTestStatus: String?
    let createdAt: String
    let updatedAt: String

    var isRemote: Bool {
        source == "remote"
    }

    enum CodingKeys: String, CodingKey {
        case id
        case sessionId = "session_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case taskId = "task_id"
        case branch
        case commit
        case baseCommit = "base_commit"
        case preMergeCommit = "pre_merge_commit"
        case status
        case attempt
        case detail
        case source
        case remoteHost = "remote_host"
        case remoteBranch = "remote_branch"
        case bundleSha256 = "bundle_sha256"
        case importedRef = "imported_ref"
        case remoteTestEvidenceId = "remote_test_evidence_id"
        case remoteTestDigest = "remote_test_digest"
        case remoteTestStatus = "remote_test_status"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct RemoteIntegrationPolicy: Codable, Sendable, Equatable {
    let enabled: Bool
    let maxBundleBytes: Int
    let requireVerifier: Bool
    let allowedHosts: [String]
    let keepImportRefs: Bool

    static let disabled = RemoteIntegrationPolicy(
        enabled: false,
        maxBundleBytes: 256 * 1024 * 1024,
        requireVerifier: true,
        allowedHosts: [],
        keepImportRefs: false
    )

    enum CodingKeys: String, CodingKey {
        case enabled
        case maxBundleBytes = "max_bundle_bytes"
        case requireVerifier = "require_verifier"
        case allowedHosts = "allowed_hosts"
        case keepImportRefs = "keep_import_refs"
    }

    init(
        enabled: Bool,
        maxBundleBytes: Int,
        requireVerifier: Bool,
        allowedHosts: [String],
        keepImportRefs: Bool
    ) {
        self.enabled = enabled
        self.maxBundleBytes = maxBundleBytes
        self.requireVerifier = requireVerifier
        self.allowedHosts = allowedHosts
        self.keepImportRefs = keepImportRefs
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        maxBundleBytes =
            try container.decodeIfPresent(Int.self, forKey: .maxBundleBytes)
                ?? 256 * 1024 * 1024
        requireVerifier =
            try container.decodeIfPresent(Bool.self, forKey: .requireVerifier) ?? true
        allowedHosts =
            try container.decodeIfPresent([String].self, forKey: .allowedHosts) ?? []
        keepImportRefs =
            try container.decodeIfPresent(Bool.self, forKey: .keepImportRefs) ?? false
    }
}

struct RemoteIntegrationPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: RemoteIntegrationPolicy
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
        case updatedAt = "updated_at"
    }
}

struct RemoteIntegrationQueueEntry: Codable, Identifiable, Hashable, Sendable {
    var id: String {
        entryId
    }

    let entryId: String
    let sessionId: String
    let status: String
    let commit: String
    let branch: String
    let remoteHost: String?
    let remoteBranch: String?
    let bundleSha256: String?
    let importedRef: String?
    let verifierApproved: Bool?
    let verifierSummary: String?
    let environmentCaveat: String?
    let remoteTestEvidenceId: String?
    let remoteTestDigest: String?
    let remoteTestStatus: String?
    let attempt: Int
    let detail: String?
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case entryId = "entry_id"
        case sessionId = "session_id"
        case status
        case commit
        case branch
        case remoteHost = "remote_host"
        case remoteBranch = "remote_branch"
        case bundleSha256 = "bundle_sha256"
        case importedRef = "imported_ref"
        case verifierApproved = "verifier_approved"
        case verifierSummary = "verifier_summary"
        case environmentCaveat = "environment_caveat"
        case remoteTestEvidenceId = "remote_test_evidence_id"
        case remoteTestDigest = "remote_test_digest"
        case remoteTestStatus = "remote_test_status"
        case attempt
        case detail
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct RemoteIntegrationCleanupResult: Codable, Sendable {
    let projectId: String
    let sessionId: String?
    let removedRefs: [String]
    let removedWorktrees: [String]
    let skipped: [String]
    let entriesExamined: Int

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case sessionId = "session_id"
        case removedRefs = "removed_refs"
        case removedWorktrees = "removed_worktrees"
        case skipped
        case entriesExamined = "entries_examined"
    }
}

struct RemoteIntegrationStatusResult: Codable, Sendable {
    let projectId: String
    let sessionId: String?
    let policy: RemoteIntegrationPolicy
    let policyUpdatedAt: String
    let entries: [RemoteIntegrationQueueEntry]
    let total: Int
    let pending: Int
    let merged: Int
    let conflicts: Int
    let failed: Int
    let retainedRefs: Int
    let verifierRequired: Bool
    let environmentCaveat: String

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case sessionId = "session_id"
        case policy
        case policyUpdatedAt = "policy_updated_at"
        case entries
        case total
        case pending
        case merged
        case conflicts
        case failed
        case retainedRefs = "retained_refs"
        case verifierRequired = "verifier_required"
        case environmentCaveat = "environment_caveat"
    }
}

struct RemoteBootstrapRecord: Codable, Sendable, Equatable {
    let projectId: String
    let host: String
    let installPath: String
    let sha256: String
    let bytes: Int
    let helperVersion: String
    let protocolVersion: Int
    let targetOs: String
    let targetArch: String
    let installedAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case host
        case installPath = "install_path"
        case sha256
        case bytes
        case helperVersion = "helper_version"
        case protocolVersion = "protocol_version"
        case targetOs = "target_os"
        case targetArch = "target_arch"
        case installedAt = "installed_at"
        case updatedAt = "updated_at"
    }
}

struct RemoteBootstrapStatusResult: Codable, Sendable {
    let projectId: String
    let configured: Bool
    let host: String?
    let installPath: String
    let configuredHelperPath: String
    let pathMatchesConfig: Bool
    let record: RemoteBootstrapRecord?
    let risk: String
    let approvalRequired: Bool
    let environmentCaveat: String

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case configured
        case host
        case installPath = "install_path"
        case configuredHelperPath = "configured_helper_path"
        case pathMatchesConfig = "path_matches_config"
        case record
        case risk
        case approvalRequired = "approval_required"
        case environmentCaveat = "environment_caveat"
    }
}

struct RemoteTestProfile: Codable, Sendable, Equatable {
    let name: String
    let kind: String
    let command: String
    let cwd: String?
    let timeoutSecs: Int?

    enum CodingKeys: String, CodingKey {
        case name
        case kind
        case command
        case cwd
        case timeoutSecs = "timeout_secs"
    }
}

struct RemoteTestPolicy: Codable, Sendable, Equatable {
    let enabled: Bool
    let defaultProfile: String?
    let profiles: [RemoteTestProfile]
    let timeoutSecs: Int
    let outputCapBytes: Int

    static let disabled = RemoteTestPolicy(
        enabled: false,
        defaultProfile: nil,
        profiles: [],
        timeoutSecs: 900,
        outputCapBytes: 65536
    )

    enum CodingKeys: String, CodingKey {
        case enabled
        case defaultProfile = "default_profile"
        case profiles
        case timeoutSecs = "timeout_secs"
        case outputCapBytes = "output_cap_bytes"
    }
}

struct RemoteTestEvidence: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let sessionId: String
    let entryId: String?
    let profile: String
    let kind: String
    let host: String
    let commit: String
    let command: String
    let cwd: String?
    let status: String
    let exitCode: Int?
    let durationMs: Int
    let outputRef: String?
    let outputSha256: String?
    let outputBytes: Int
    let outputTruncated: Bool
    let timedOut: Bool
    let detail: String?
    let digest: String
    let environmentCaveat: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case sessionId = "session_id"
        case entryId = "entry_id"
        case profile
        case kind
        case host
        case commit
        case command
        case cwd
        case status
        case exitCode = "exit_code"
        case durationMs = "duration_ms"
        case outputRef = "output_ref"
        case outputSha256 = "output_sha256"
        case outputBytes = "output_bytes"
        case outputTruncated = "output_truncated"
        case timedOut = "timed_out"
        case detail
        case digest
        case environmentCaveat = "environment_caveat"
        case createdAt = "created_at"
    }
}

struct RemoteTestStatusResult: Codable, Sendable {
    let projectId: String
    let sessionId: String?
    let entryId: String?
    let policy: RemoteTestPolicy
    let evidence: [RemoteTestEvidence]
    let total: Int
    let passed: Int
    let failed: Int
    let timedOut: Int
    let latestDigest: String?
    let environmentCaveat: String
    let localVerifierRequired: Bool
    let mergeGateUnchanged: Bool

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case sessionId = "session_id"
        case entryId = "entry_id"
        case policy
        case evidence
        case total
        case passed
        case failed
        case timedOut = "timed_out"
        case latestDigest = "latest_digest"
        case environmentCaveat = "environment_caveat"
        case localVerifierRequired = "local_verifier_required"
        case mergeGateUnchanged = "merge_gate_unchanged"
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
    /// Missing on older daemons; `nil` means the provider was not reported.
    let provider: String?
    let repository: String?
    let change: String?

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case branch
        case baseBranch = "base_branch"
        case pushed
        case url
        case detail
        case provider
        case repository
        case change
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        branch = try container.decode(String.self, forKey: .branch)
        baseBranch = try container.decode(String.self, forKey: .baseBranch)
        pushed = try container.decode(Bool.self, forKey: .pushed)
        url = try container.decodeIfPresent(String.self, forKey: .url)
        detail = try container.decode(String.self, forKey: .detail)
        provider = try container.decodeIfPresent(String.self, forKey: .provider)
        repository = try container.decodeIfPresent(String.self, forKey: .repository)
        change = try container.decodeIfPresent(String.self, forKey: .change)
    }
}

struct ForgeProviderStatus: Codable, Sendable, Identifiable {
    let provider: String
    let changeName: String
    let transport: String
    let command: String?
    let commandAvailable: Bool
    let authConfigured: Bool?
    let credentialSource: String?
    let supported: Bool
    let detail: String

    var id: String {
        provider
    }

    enum CodingKeys: String, CodingKey {
        case provider
        case changeName = "change_name"
        case transport
        case command
        case commandAvailable = "command_available"
        case authConfigured = "auth_configured"
        case credentialSource = "credential_source"
        case supported
        case detail
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        provider = try container.decode(String.self, forKey: .provider)
        changeName = try container.decodeIfPresent(String.self, forKey: .changeName) ?? "PR"
        transport = try container.decodeIfPresent(String.self, forKey: .transport) ?? "cli"
        command = try container.decodeIfPresent(String.self, forKey: .command)
        commandAvailable = try container.decodeIfPresent(Bool.self, forKey: .commandAvailable)
            ?? false
        authConfigured = try container.decodeIfPresent(Bool.self, forKey: .authConfigured)
        credentialSource = try container.decodeIfPresent(
            String.self,
            forKey: .credentialSource
        )
        supported = try container.decodeIfPresent(Bool.self, forKey: .supported) ?? false
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
    }
}

struct ForgeOriginStatus: Codable, Sendable {
    let host: String?
    let provider: String?
    let repository: String?
    let supported: Bool
    let serverDetected: Bool
    let detail: String

    enum CodingKeys: String, CodingKey {
        case host
        case provider
        case repository
        case supported
        case serverDetected = "server_detected"
        case detail
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        host = try container.decodeIfPresent(String.self, forKey: .host)
        provider = try container.decodeIfPresent(String.self, forKey: .provider)
        repository = try container.decodeIfPresent(String.self, forKey: .repository)
        supported = try container.decodeIfPresent(Bool.self, forKey: .supported) ?? false
        serverDetected = try container.decodeIfPresent(Bool.self, forKey: .serverDetected)
            ?? false
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
    }
}

struct ForgeStatus: Codable, Sendable {
    let providerOverride: String?
    let defaultProvider: String
    let origin: ForgeOriginStatus?
    let providers: [ForgeProviderStatus]
    let detail: String

    enum CodingKeys: String, CodingKey {
        case providerOverride = "provider_override"
        case defaultProvider = "default_provider"
        case origin
        case providers
        case detail
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        providerOverride = try container.decodeIfPresent(
            String.self,
            forKey: .providerOverride
        )
        defaultProvider = try container.decodeIfPresent(String.self, forKey: .defaultProvider)
            ?? "github"
        origin = try container.decodeIfPresent(ForgeOriginStatus.self, forKey: .origin)
        providers = try container.decodeIfPresent(
            [ForgeProviderStatus].self,
            forKey: .providers
        ) ?? []
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
    }

    var bitbucket: ForgeProviderStatus? {
        providers.first { $0.provider == "bitbucket" }
    }
}

struct ForgePreflightResult: Codable, Sendable {
    let provider: String
    let changeName: String
    let repository: String?
    let auth: String
    let detail: String

    enum CodingKeys: String, CodingKey {
        case provider
        case changeName = "change_name"
        case repository
        case auth
        case detail
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        provider = try container.decode(String.self, forKey: .provider)
        changeName = try container.decodeIfPresent(String.self, forKey: .changeName) ?? "PR"
        repository = try container.decodeIfPresent(String.self, forKey: .repository)
        auth = try container.decodeIfPresent(String.self, forKey: .auth) ?? "unknown"
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
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

// MARK: - Hard preemption and deadlines

struct PreemptionPolicy: Codable, Sendable, Equatable {
    var enabled: Bool = true
    var providerGraceMs: UInt64 = 2000
    var toolGraceMs: UInt64 = 5000
    var killEscalationMs: UInt64 = 2000
    var toolDeadlineSecs: UInt64?
    var checkpointOnPreempt: Bool = true

    enum CodingKeys: String, CodingKey {
        case enabled
        case providerGraceMs = "provider_grace_ms"
        case toolGraceMs = "tool_grace_ms"
        case killEscalationMs = "kill_escalation_ms"
        case toolDeadlineSecs = "tool_deadline_secs"
        case checkpointOnPreempt = "checkpoint_on_preempt"
    }

    init(
        enabled: Bool = true,
        providerGraceMs: UInt64 = 2000,
        toolGraceMs: UInt64 = 5000,
        killEscalationMs: UInt64 = 2000,
        toolDeadlineSecs: UInt64? = nil,
        checkpointOnPreempt: Bool = true
    ) {
        self.enabled = enabled
        self.providerGraceMs = providerGraceMs
        self.toolGraceMs = toolGraceMs
        self.killEscalationMs = killEscalationMs
        self.toolDeadlineSecs = toolDeadlineSecs
        self.checkpointOnPreempt = checkpointOnPreempt
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        providerGraceMs = try container.decodeIfPresent(UInt64.self, forKey: .providerGraceMs) ?? 2000
        toolGraceMs = try container.decodeIfPresent(UInt64.self, forKey: .toolGraceMs) ?? 5000
        killEscalationMs = try container.decodeIfPresent(UInt64.self, forKey: .killEscalationMs) ?? 2000
        toolDeadlineSecs = try container.decodeIfPresent(UInt64.self, forKey: .toolDeadlineSecs)
        checkpointOnPreempt = try container.decodeIfPresent(Bool.self, forKey: .checkpointOnPreempt) ?? true
    }
}

struct PreemptionPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: PreemptionPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct PreemptionRecord: Codable, Identifiable, Hashable, Sendable {
    let sessionId: String
    let projectId: String
    let agentId: String
    let agentName: String
    let agentRole: String
    let taskId: String?
    let department: String?
    let reason: String
    let phase: String
    let tool: String?
    let elapsedMs: UInt64
    let sideEffectsUnknown: Bool
    let partialInputTokens: UInt64
    let partialOutputTokens: UInt64
    let partialOutputBytes: UInt64
    let usageEstimated: Bool
    let checkpointId: String?
    let taskCheckpointCommit: String?
    let detail: String?
    let terminal: Bool
    let createdAt: String

    var id: String {
        "\(sessionId)/\(createdAt)/\(phase)/\(terminal)"
    }

    var summary: String {
        var text = "\(reason) preempted during \(phase) after \(elapsedMs)ms"
        if let tool, !tool.isEmpty {
            text += " (\(tool))"
        }
        if sideEffectsUnknown {
            text += " · side effects unknown"
        }
        return text
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case projectId = "project_id"
        case agentId = "agent_id"
        case agentName = "agent_name"
        case agentRole = "agent_role"
        case taskId = "task_id"
        case department
        case reason
        case phase
        case tool
        case elapsedMs = "elapsed_ms"
        case sideEffectsUnknown = "side_effects_unknown"
        case partialInputTokens = "partial_input_tokens"
        case partialOutputTokens = "partial_output_tokens"
        case partialOutputBytes = "partial_output_bytes"
        case usageEstimated = "usage_estimated"
        case checkpointId = "checkpoint_id"
        case taskCheckpointCommit = "task_checkpoint_commit"
        case detail
        case terminal
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decodeIfPresent(String.self, forKey: .sessionId) ?? ""
        projectId = try container.decodeIfPresent(String.self, forKey: .projectId) ?? ""
        agentId = try container.decodeIfPresent(String.self, forKey: .agentId) ?? ""
        agentName = try container.decodeIfPresent(String.self, forKey: .agentName) ?? ""
        agentRole = try container.decodeIfPresent(String.self, forKey: .agentRole) ?? ""
        taskId = try container.decodeIfPresent(String.self, forKey: .taskId)
        department = try container.decodeIfPresent(String.self, forKey: .department)
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? "explicit"
        phase = try container.decodeIfPresent(String.self, forKey: .phase) ?? "provider"
        tool = try container.decodeIfPresent(String.self, forKey: .tool)
        elapsedMs = try container.decodeIfPresent(UInt64.self, forKey: .elapsedMs) ?? 0
        sideEffectsUnknown = try container.decodeIfPresent(Bool.self, forKey: .sideEffectsUnknown) ?? false
        partialInputTokens = try container.decodeIfPresent(UInt64.self, forKey: .partialInputTokens) ?? 0
        partialOutputTokens = try container.decodeIfPresent(UInt64.self, forKey: .partialOutputTokens) ?? 0
        partialOutputBytes = try container.decodeIfPresent(UInt64.self, forKey: .partialOutputBytes) ?? 0
        usageEstimated = try container.decodeIfPresent(Bool.self, forKey: .usageEstimated) ?? false
        checkpointId = try container.decodeIfPresent(String.self, forKey: .checkpointId)
        taskCheckpointCommit = try container.decodeIfPresent(String.self, forKey: .taskCheckpointCommit)
        detail = try container.decodeIfPresent(String.self, forKey: .detail)
        terminal = try container.decodeIfPresent(Bool.self, forKey: .terminal) ?? false
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

struct SessionPreemptResult: Codable, Sendable {
    let sessionId: String
    let preempted: Bool
    let reason: String
    let detail: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case preempted
        case reason
        case detail
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decodeIfPresent(String.self, forKey: .sessionId) ?? ""
        preempted = try container.decodeIfPresent(Bool.self, forKey: .preempted) ?? false
        reason = try container.decodeIfPresent(String.self, forKey: .reason) ?? ""
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
    }
}

struct PreemptionCount: Codable, Hashable, Sendable {
    let key: String
    let count: UInt64
}

struct PreemptionSessionSummary: Codable, Identifiable, Hashable, Sendable {
    let sessionId: String
    let goal: String
    let status: String
    let terminalCount: UInt64
    let lastReason: String
    let lastPhase: String
    let lastTool: String?
    let lastAt: String

    var id: String {
        sessionId
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case goal
        case status
        case terminalCount = "terminal_count"
        case lastReason = "last_reason"
        case lastPhase = "last_phase"
        case lastTool = "last_tool"
        case lastAt = "last_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        goal = try container.decodeIfPresent(String.self, forKey: .goal) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        terminalCount = try container.decodeIfPresent(UInt64.self, forKey: .terminalCount) ?? 0
        lastReason = try container.decodeIfPresent(String.self, forKey: .lastReason) ?? ""
        lastPhase = try container.decodeIfPresent(String.self, forKey: .lastPhase) ?? ""
        lastTool = try container.decodeIfPresent(String.self, forKey: .lastTool)
        lastAt = try container.decodeIfPresent(String.self, forKey: .lastAt) ?? ""
    }
}

struct PreemptionOverviewResult: Codable, Sendable {
    let projectId: String
    let policy: PreemptionPolicy
    let total: UInt64
    let sessions: [PreemptionSessionSummary]
    let byPhase: [PreemptionCount]
    let byReason: [PreemptionCount]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
        case total
        case sessions
        case byPhase = "by_phase"
        case byReason = "by_reason"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decode(String.self, forKey: .projectId)
        policy = try container.decodeIfPresent(PreemptionPolicy.self, forKey: .policy) ?? PreemptionPolicy()
        total = try container.decodeIfPresent(UInt64.self, forKey: .total) ?? 0
        sessions = try container.decodeIfPresent([PreemptionSessionSummary].self, forKey: .sessions) ?? []
        byPhase = try container.decodeIfPresent([PreemptionCount].self, forKey: .byPhase) ?? []
        byReason = try container.decodeIfPresent([PreemptionCount].self, forKey: .byReason) ?? []
    }
}

struct SessionPreemptionResult: Codable, Sendable { let sessionId: String
    let policy: PreemptionPolicy
    let active: Bool
    let preempted: Bool
    let records: [PreemptionRecord]
    let byPhase: [PreemptionCount]
    let byReason: [PreemptionCount]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case policy
        case active
        case preempted
        case records
        case byPhase = "by_phase"
        case byReason = "by_reason"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        policy = try container.decodeIfPresent(PreemptionPolicy.self, forKey: .policy) ?? PreemptionPolicy()
        active = try container.decodeIfPresent(Bool.self, forKey: .active) ?? false
        preempted = try container.decodeIfPresent(Bool.self, forKey: .preempted) ?? false
        records = try container.decodeIfPresent([PreemptionRecord].self, forKey: .records) ?? []
        byPhase = try container.decodeIfPresent([PreemptionCount].self, forKey: .byPhase) ?? []
        byReason = try container.decodeIfPresent([PreemptionCount].self, forKey: .byReason) ?? []
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

struct ApprovalGrant: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let tool: String
    let pathPrefix: String
    let includeChildren: Bool
    let createdBy: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case tool
        case pathPrefix = "path_prefix"
        case includeChildren = "include_children"
        case createdBy = "created_by"
        case createdAt = "created_at"
    }
}

struct ApprovalGrantListResult: Codable, Sendable {
    let grants: [ApprovalGrant]
}

struct ApprovalGrantCreateResult: Codable, Sendable {
    let grant: ApprovalGrant
}

struct ApprovalGrantDeleteResult: Codable, Sendable {
    let deleted: Bool
}

struct WorkspaceListResult: Codable, Sendable {
    let root: String
    let path: String
    let entries: [WorkspaceEntry]
    let total: Int
    let hasMore: Bool
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case root
        case path
        case entries
        case total
        case hasMore = "has_more"
        case truncated
    }
}

struct WorkspaceSearchResult: Codable, Sendable {
    let entries: [WorkspaceEntry]
    let truncated: Bool
}

enum WorkspaceSearchMode: String, CaseIterable, Identifiable, Sendable {
    case names
    case contents

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .names: L10n.t("Names")
        case .contents: L10n.t("Contents")
        }
    }
}

struct WorkspaceGrepMatch: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let line: UInt32
    let column: UInt32
    let preview: String

    var id: String {
        "\(path):\(line):\(column)"
    }
}

struct WorkspaceGrepResult: Codable, Sendable {
    let query: String
    let matches: [WorkspaceGrepMatch]
    let filesScanned: Int
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case query
        case matches
        case filesScanned = "files_scanned"
        case truncated
    }
}

struct ProjectSource: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectID: String
    let kind: String
    let collection: String?
    let title: String
    let uri: String?
    let mimeType: String?
    let status: String
    let sizeBytes: UInt64
    let sha256: String
    let revision: UInt32
    let chunkCount: Int
    let charCount: Int
    let tokenCount: UInt64
    let error: String?
    let labels: [String]
    let createdAt: String
    let updatedAt: String
    let indexedAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case projectID = "project_id"
        case kind
        case collection
        case title
        case uri
        case mimeType = "mime_type"
        case status
        case sizeBytes = "size_bytes"
        case sha256
        case revision
        case chunkCount = "chunk_count"
        case charCount = "char_count"
        case tokenCount = "token_count"
        case error
        case labels
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case indexedAt = "indexed_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectID = try container.decode(String.self, forKey: .projectID)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? "file"
        collection = try container.decodeIfPresent(String.self, forKey: .collection)
        title = try container.decode(String.self, forKey: .title)
        uri = try container.decodeIfPresent(String.self, forKey: .uri)
        mimeType = try container.decodeIfPresent(String.self, forKey: .mimeType)
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? "indexed"
        sizeBytes = try container.decodeIfPresent(UInt64.self, forKey: .sizeBytes) ?? 0
        sha256 = try container.decodeIfPresent(String.self, forKey: .sha256) ?? ""
        revision = try container.decodeIfPresent(UInt32.self, forKey: .revision) ?? 1
        chunkCount = try container.decodeIfPresent(Int.self, forKey: .chunkCount) ?? 0
        charCount = try container.decodeIfPresent(Int.self, forKey: .charCount) ?? 0
        tokenCount = try container.decodeIfPresent(UInt64.self, forKey: .tokenCount) ?? 0
        error = try container.decodeIfPresent(String.self, forKey: .error)
        labels = try container.decodeIfPresent([String].self, forKey: .labels) ?? []
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
        indexedAt = try container.decodeIfPresent(String.self, forKey: .indexedAt)
    }
}

struct SourceChunk: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let sourceID: String
    let chunkNo: UInt32
    let heading: String?
    let content: String
    let startOffset: Int
    let endOffset: Int
    let tokenCount: UInt32

    enum CodingKeys: String, CodingKey {
        case id
        case sourceID = "source_id"
        case chunkNo = "chunk_no"
        case heading
        case content
        case startOffset = "start_offset"
        case endOffset = "end_offset"
        case tokenCount = "token_count"
    }
}

struct SourceStats: Codable, Hashable, Sendable {
    let sources: Int
    let indexed: Int
    let failed: Int
    let chunks: Int
    let bytes: UInt64
    let tokens: UInt64
}

struct SourceListResult: Codable, Sendable {
    let sources: [ProjectSource]
    let stats: SourceStats
}

struct SourceGetResult: Codable, Sendable {
    let source: ProjectSource
    let chunks: [SourceChunk]
    let totalChunks: Int

    enum CodingKeys: String, CodingKey {
        case source
        case chunks
        case totalChunks = "total_chunks"
    }
}

struct SourceImportError: Codable, Hashable, Sendable {
    let path: String
    let error: String
}

struct SourceImportResult: Codable, Sendable {
    let imported: [ProjectSource]
    let skipped: Int
    let errors: [SourceImportError]
}

struct SourceSearchHit: Codable, Identifiable, Hashable, Sendable {
    let sourceID: String
    let sourceTitle: String
    let collection: String?
    let labels: [String]
    let chunkID: String
    let chunkNo: UInt32
    let heading: String?
    let content: String
    let score: Double
    /// Effective lane: lexical, semantic, or hybrid. Legacy payloads decode as
    /// lexical because that is the only lane they could have come from.
    let source: String
    let confidence: Double
    let origins: [String]
    let lexicalScore: Double?
    let semanticScore: Double?
    /// `off` or `deterministic`. Older payloads decode as `off`.
    let rerank: String
    let rerankScore: Double?
    let lexicalOverlap: Double?
    let titleMatch: Double?
    let freshness: Double?
    let expanded: Bool
    let expansionTerms: [String]
    let sourceUpdatedAt: String?

    var id: String {
        chunkID
    }

    enum CodingKeys: String, CodingKey {
        case sourceID = "source_id"
        case sourceTitle = "source_title"
        case collection
        case labels
        case chunkID = "chunk_id"
        case chunkNo = "chunk_no"
        case heading
        case content
        case score
        case source
        case confidence
        case origins
        case lexicalScore = "lexical_score"
        case semanticScore = "semantic_score"
        case rerank
        case rerankScore = "rerank_score"
        case lexicalOverlap = "lexical_overlap"
        case titleMatch = "title_match"
        case freshness
        case expanded
        case expansionTerms = "expansion_terms"
        case sourceUpdatedAt = "source_updated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sourceID = try container.decode(String.self, forKey: .sourceID)
        sourceTitle = try container.decode(String.self, forKey: .sourceTitle)
        collection = try container.decodeIfPresent(String.self, forKey: .collection)
        labels = try container.decodeIfPresent([String].self, forKey: .labels) ?? []
        chunkID = try container.decode(String.self, forKey: .chunkID)
        chunkNo = try container.decode(UInt32.self, forKey: .chunkNo)
        heading = try container.decodeIfPresent(String.self, forKey: .heading)
        content = try container.decode(String.self, forKey: .content)
        score = try container.decode(Double.self, forKey: .score)
        source = try container.decodeIfPresent(String.self, forKey: .source) ?? "lexical"
        confidence = try container.decodeIfPresent(Double.self, forKey: .confidence) ?? score
        origins = try container.decodeIfPresent([String].self, forKey: .origins) ?? ["lexical"]
        lexicalScore = try container.decodeIfPresent(Double.self, forKey: .lexicalScore)
        semanticScore = try container.decodeIfPresent(Double.self, forKey: .semanticScore)
        rerank = try container.decodeIfPresent(String.self, forKey: .rerank) ?? "off"
        rerankScore = try container.decodeIfPresent(Double.self, forKey: .rerankScore)
        lexicalOverlap = try container.decodeIfPresent(Double.self, forKey: .lexicalOverlap)
        titleMatch = try container.decodeIfPresent(Double.self, forKey: .titleMatch)
        freshness = try container.decodeIfPresent(Double.self, forKey: .freshness)
        expanded = try container.decodeIfPresent(Bool.self, forKey: .expanded) ?? false
        expansionTerms = try container.decodeIfPresent(
            [String].self,
            forKey: .expansionTerms
        ) ?? []
        sourceUpdatedAt = try container.decodeIfPresent(String.self, forKey: .sourceUpdatedAt)
    }

    init(
        sourceID: String,
        sourceTitle: String,
        collection: String? = nil,
        labels: [String] = [],
        chunkID: String,
        chunkNo: UInt32,
        heading: String? = nil,
        content: String,
        score: Double,
        source: String = "lexical",
        confidence: Double? = nil,
        origins: [String] = ["lexical"],
        lexicalScore: Double? = nil,
        semanticScore: Double? = nil,
        rerank: String = "off",
        rerankScore: Double? = nil,
        lexicalOverlap: Double? = nil,
        titleMatch: Double? = nil,
        freshness: Double? = nil,
        expanded: Bool = false,
        expansionTerms: [String] = [],
        sourceUpdatedAt: String? = nil
    ) {
        self.sourceID = sourceID
        self.sourceTitle = sourceTitle
        self.collection = collection
        self.labels = labels
        self.chunkID = chunkID
        self.chunkNo = chunkNo
        self.heading = heading
        self.content = content
        self.score = score
        self.source = source
        self.confidence = confidence ?? score
        self.origins = origins
        self.lexicalScore = lexicalScore
        self.semanticScore = semanticScore
        self.rerank = rerank
        self.rerankScore = rerankScore
        self.lexicalOverlap = lexicalOverlap
        self.titleMatch = titleMatch
        self.freshness = freshness
        self.expanded = expanded
        self.expansionTerms = expansionTerms
        self.sourceUpdatedAt = sourceUpdatedAt
    }
}

/// Retrieval lane chosen by the Sources UI. `automatic` sends no mode and lets
/// the daemon pick hybrid when source embeddings are enabled, otherwise
/// lexical. The daemon still owns fallback: `semantic`/`hybrid` returns lexical
/// hits with a reason when unavailable.
enum SourceSearchMode: String, CaseIterable, Identifiable, Codable, Sendable {
    case automatic
    case lexical
    case semantic
    case hybrid

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .automatic: L10n.t("Auto")
        case .lexical: L10n.t("Lexical")
        case .semantic: L10n.t("Semantic")
        case .hybrid: L10n.t("Hybrid")
        }
    }

    /// `automatic` is UI-only; the protocol keeps the three explicit lanes.
    var protocolValue: SourceSearchMode? {
        self == .automatic ? nil : self
    }
}

struct SourceSearchResult: Codable, Sendable {
    let hits: [SourceSearchHit]
    let degraded: Bool
    let mode: String
    let requestedMode: String
    let semanticUnavailable: String?
    let candidatesScanned: Int
    let indexPath: String
    let indexGeneration: UInt64?
    let indexFallbackReason: String?
    let probedClusters: UInt32
    let annCandidates: Int
    let rerank: String
    let expansion: String
    let expansionTerms: [String]

    enum CodingKeys: String, CodingKey {
        case hits
        case degraded
        case mode
        case requestedMode = "requested_mode"
        case semanticUnavailable = "semantic_unavailable"
        case candidatesScanned = "candidates_scanned"
        case indexPath = "index_path"
        case indexGeneration = "index_generation"
        case indexFallbackReason = "index_fallback_reason"
        case probedClusters = "probed_clusters"
        case annCandidates = "ann_candidates"
        case rerank
        case expansion
        case expansionTerms = "expansion_terms"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        hits = try container.decode([SourceSearchHit].self, forKey: .hits)
        degraded = try container.decodeIfPresent(Bool.self, forKey: .degraded) ?? false
        mode = try container.decodeIfPresent(String.self, forKey: .mode) ?? "lexical"
        requestedMode = try container.decodeIfPresent(String.self, forKey: .requestedMode) ?? mode
        semanticUnavailable = try container.decodeIfPresent(
            String.self,
            forKey: .semanticUnavailable
        )
        candidatesScanned = try container.decodeIfPresent(Int.self, forKey: .candidatesScanned) ?? 0
        indexPath = try container.decodeIfPresent(String.self, forKey: .indexPath) ?? "exact"
        indexGeneration = try container.decodeIfPresent(UInt64.self, forKey: .indexGeneration)
        indexFallbackReason = try container.decodeIfPresent(
            String.self,
            forKey: .indexFallbackReason
        )
        probedClusters = try container.decodeIfPresent(UInt32.self, forKey: .probedClusters) ?? 0
        annCandidates = try container.decodeIfPresent(Int.self, forKey: .annCandidates) ?? 0
        rerank = try container.decodeIfPresent(String.self, forKey: .rerank) ?? "off"
        expansion = try container.decodeIfPresent(String.self, forKey: .expansion) ?? "off"
        expansionTerms = try container.decodeIfPresent(
            [String].self,
            forKey: .expansionTerms
        ) ?? []
    }

    init(
        hits: [SourceSearchHit],
        degraded: Bool = false,
        mode: String = "lexical",
        requestedMode: String = "lexical",
        semanticUnavailable: String? = nil,
        candidatesScanned: Int = 0,
        indexPath: String = "exact",
        indexGeneration: UInt64? = nil,
        indexFallbackReason: String? = nil,
        probedClusters: UInt32 = 0,
        annCandidates: Int = 0,
        rerank: String = "off",
        expansion: String = "off",
        expansionTerms: [String] = []
    ) {
        self.hits = hits
        self.degraded = degraded
        self.mode = mode
        self.requestedMode = requestedMode
        self.semanticUnavailable = semanticUnavailable
        self.candidatesScanned = candidatesScanned
        self.indexPath = indexPath
        self.indexGeneration = indexGeneration
        self.indexFallbackReason = indexFallbackReason
        self.probedClusters = probedClusters
        self.annCandidates = annCandidates
        self.rerank = rerank
        self.expansion = expansion
        self.expansionTerms = expansionTerms
    }
}

struct SourceEmbeddingPolicy: Codable, Equatable, Sendable {
    var enabled: Bool
    var endpointEnv: String?
    var model: String?
    var dimensions: Int
    var batchSize: Int
    var maxChunksPerSource: Int
    var retentionDays: Int
    var timeoutSecs: Int
    var apiKeyEnv: String?

    static let defaults = SourceEmbeddingPolicy(
        enabled: false,
        endpointEnv: nil,
        model: nil,
        dimensions: 1536,
        batchSize: 8,
        maxChunksPerSource: 200,
        retentionDays: 30,
        timeoutSecs: 30,
        apiKeyEnv: nil
    )

    enum CodingKeys: String, CodingKey {
        case enabled
        case endpointEnv = "endpoint_env"
        case model
        case dimensions
        case batchSize = "batch_size"
        case maxChunksPerSource = "max_chunks_per_source"
        case retentionDays = "retention_days"
        case timeoutSecs = "timeout_secs"
        case apiKeyEnv = "api_key_env"
    }

    init(
        enabled: Bool,
        endpointEnv: String?,
        model: String?,
        dimensions: Int,
        batchSize: Int,
        maxChunksPerSource: Int,
        retentionDays: Int,
        timeoutSecs: Int,
        apiKeyEnv: String?
    ) {
        self.enabled = enabled
        self.endpointEnv = endpointEnv
        self.model = model
        self.dimensions = dimensions
        self.batchSize = batchSize
        self.maxChunksPerSource = maxChunksPerSource
        self.retentionDays = retentionDays
        self.timeoutSecs = timeoutSecs
        self.apiKeyEnv = apiKeyEnv
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        endpointEnv = try container.decodeIfPresent(String.self, forKey: .endpointEnv)
        model = try container.decodeIfPresent(String.self, forKey: .model)
        dimensions = try container.decodeIfPresent(Int.self, forKey: .dimensions) ?? 1536
        batchSize = try container.decodeIfPresent(Int.self, forKey: .batchSize) ?? 8
        maxChunksPerSource = try container.decodeIfPresent(Int.self, forKey: .maxChunksPerSource)
            ?? 200
        retentionDays = try container.decodeIfPresent(Int.self, forKey: .retentionDays) ?? 30
        timeoutSecs = try container.decodeIfPresent(Int.self, forKey: .timeoutSecs) ?? 30
        apiKeyEnv = try container.decodeIfPresent(String.self, forKey: .apiKeyEnv)
    }

    var isConfigured: Bool {
        !(endpointEnv?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
            && !(model?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
    }

    func jsonObject() -> [String: JSONValue] {
        [
            "enabled": .bool(enabled),
            "endpoint_env": endpointEnv.flatMap { value in
                value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    ? nil
                    : JSONValue.string(value)
            } ?? .null,
            "model": model.flatMap { value in
                value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    ? nil
                    : JSONValue.string(value)
            } ?? .null,
            "dimensions": .number(Double(dimensions)),
            "batch_size": .number(Double(batchSize)),
            "max_chunks_per_source": .number(Double(maxChunksPerSource)),
            "retention_days": .number(Double(retentionDays)),
            "timeout_secs": .number(Double(timeoutSecs)),
            "api_key_env": apiKeyEnv.flatMap { value in
                value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    ? nil
                    : JSONValue.string(value)
            } ?? .null,
        ]
    }
}

struct SourceEmbeddingPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: SourceEmbeddingPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct SourceEmbeddingUsage: Codable, Sendable {
    let calls: UInt64
    let inputTokens: UInt64
    let estimatedTokens: UInt64
    let bytes: UInt64
    let providerReportedCalls: UInt64

    enum CodingKeys: String, CodingKey {
        case calls
        case inputTokens = "input_tokens"
        case estimatedTokens = "estimated_tokens"
        case bytes
        case providerReportedCalls = "provider_reported_calls"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        calls = try container.decodeIfPresent(UInt64.self, forKey: .calls) ?? 0
        inputTokens = try container.decodeIfPresent(UInt64.self, forKey: .inputTokens) ?? 0
        estimatedTokens = try container.decodeIfPresent(UInt64.self, forKey: .estimatedTokens) ?? 0
        bytes = try container.decodeIfPresent(UInt64.self, forKey: .bytes) ?? 0
        providerReportedCalls = try container.decodeIfPresent(
            UInt64.self,
            forKey: .providerReportedCalls
        ) ?? 0
    }
}

struct SourceEmbeddingJobStatus: Codable, Identifiable, Hashable, Sendable {
    let sourceID: String
    let sourceTitle: String
    let status: String
    let attempts: UInt32
    let generation: UInt64
    let chunksTotal: UInt32
    let chunksDone: UInt32
    let lastError: String?
    let nextAttemptAt: String?
    let updatedAt: String

    var id: String {
        sourceID
    }

    enum CodingKeys: String, CodingKey {
        case sourceID = "source_id"
        case sourceTitle = "source_title"
        case status
        case attempts
        case generation
        case chunksTotal = "chunks_total"
        case chunksDone = "chunks_done"
        case lastError = "last_error"
        case nextAttemptAt = "next_attempt_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sourceID = try container.decode(String.self, forKey: .sourceID)
        sourceTitle = try container.decodeIfPresent(String.self, forKey: .sourceTitle) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? "pending"
        attempts = try container.decodeIfPresent(UInt32.self, forKey: .attempts) ?? 0
        generation = try container.decodeIfPresent(UInt64.self, forKey: .generation) ?? 0
        chunksTotal = try container.decodeIfPresent(UInt32.self, forKey: .chunksTotal) ?? 0
        chunksDone = try container.decodeIfPresent(UInt32.self, forKey: .chunksDone) ?? 0
        lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
        nextAttemptAt = try container.decodeIfPresent(String.self, forKey: .nextAttemptAt)
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
    }
}

struct SourceEmbeddingIntegrity: Codable, Hashable, Sendable {
    let validVectors: UInt32
    let invalidVectors: UInt32
    let staleVectors: UInt32
    let orphanVectors: UInt32
    let generationMismatched: UInt32

    enum CodingKeys: String, CodingKey {
        case validVectors = "valid_vectors"
        case invalidVectors = "invalid_vectors"
        case staleVectors = "stale_vectors"
        case orphanVectors = "orphan_vectors"
        case generationMismatched = "generation_mismatched"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        validVectors = try container.decodeIfPresent(UInt32.self, forKey: .validVectors) ?? 0
        invalidVectors = try container.decodeIfPresent(UInt32.self, forKey: .invalidVectors) ?? 0
        staleVectors = try container.decodeIfPresent(UInt32.self, forKey: .staleVectors) ?? 0
        orphanVectors = try container.decodeIfPresent(UInt32.self, forKey: .orphanVectors) ?? 0
        generationMismatched = try container.decodeIfPresent(
            UInt32.self,
            forKey: .generationMismatched
        ) ?? 0
    }
}

struct SourceEmbeddingStatusResult: Codable, Sendable {
    let projectId: String
    let enabled: Bool
    let configured: Bool
    let endpointEnv: String?
    let endpointEnvPresent: Bool
    let apiKeyEnv: String?
    let apiKeyEnvPresent: Bool
    let model: String?
    let dimensions: Int
    let batchSize: Int
    let maxChunksPerSource: Int
    let retentionDays: Int
    let indexableChunks: UInt32
    let indexedChunks: UInt32
    let staleChunks: UInt32
    let invalidChunks: UInt32
    let pendingJobs: UInt32
    let runningJobs: UInt32
    let failedJobs: UInt32
    let deadJobs: UInt32
    let doneJobs: UInt32
    let coverage: Double
    let semanticUnavailable: String?
    let lastError: String?
    let usage: SourceEmbeddingUsage
    let recentJobs: [SourceEmbeddingJobStatus]
    let integrity: SourceEmbeddingIntegrity

    var activeJobs: UInt32 {
        pendingJobs + runningJobs
    }

    var attentionJobs: UInt32 {
        failedJobs + deadJobs
    }

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case enabled
        case configured
        case endpointEnv = "endpoint_env"
        case endpointEnvPresent = "endpoint_env_present"
        case apiKeyEnv = "api_key_env"
        case apiKeyEnvPresent = "api_key_env_present"
        case model
        case dimensions
        case batchSize = "batch_size"
        case maxChunksPerSource = "max_chunks_per_source"
        case retentionDays = "retention_days"
        case indexableChunks = "indexable_chunks"
        case indexedChunks = "indexed_chunks"
        case staleChunks = "stale_chunks"
        case invalidChunks = "invalid_chunks"
        case pendingJobs = "pending_jobs"
        case runningJobs = "running_jobs"
        case failedJobs = "failed_jobs"
        case deadJobs = "dead_jobs"
        case doneJobs = "done_jobs"
        case coverage
        case semanticUnavailable = "semantic_unavailable"
        case lastError = "last_error"
        case usage
        case recentJobs = "recent_jobs"
        case integrity
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decode(String.self, forKey: .projectId)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        configured = try container.decodeIfPresent(Bool.self, forKey: .configured) ?? false
        endpointEnv = try container.decodeIfPresent(String.self, forKey: .endpointEnv)
        endpointEnvPresent = try container.decodeIfPresent(
            Bool.self,
            forKey: .endpointEnvPresent
        ) ?? false
        apiKeyEnv = try container.decodeIfPresent(String.self, forKey: .apiKeyEnv)
        apiKeyEnvPresent = try container.decodeIfPresent(
            Bool.self,
            forKey: .apiKeyEnvPresent
        ) ?? true
        model = try container.decodeIfPresent(String.self, forKey: .model)
        dimensions = try container.decodeIfPresent(Int.self, forKey: .dimensions) ?? 0
        batchSize = try container.decodeIfPresent(Int.self, forKey: .batchSize) ?? 0
        maxChunksPerSource = try container.decodeIfPresent(Int.self, forKey: .maxChunksPerSource)
            ?? 0
        retentionDays = try container.decodeIfPresent(Int.self, forKey: .retentionDays) ?? 0
        indexableChunks = try container.decodeIfPresent(UInt32.self, forKey: .indexableChunks) ?? 0
        indexedChunks = try container.decodeIfPresent(UInt32.self, forKey: .indexedChunks) ?? 0
        staleChunks = try container.decodeIfPresent(UInt32.self, forKey: .staleChunks) ?? 0
        invalidChunks = try container.decodeIfPresent(UInt32.self, forKey: .invalidChunks) ?? 0
        pendingJobs = try container.decodeIfPresent(UInt32.self, forKey: .pendingJobs) ?? 0
        runningJobs = try container.decodeIfPresent(UInt32.self, forKey: .runningJobs) ?? 0
        failedJobs = try container.decodeIfPresent(UInt32.self, forKey: .failedJobs) ?? 0
        deadJobs = try container.decodeIfPresent(UInt32.self, forKey: .deadJobs) ?? 0
        doneJobs = try container.decodeIfPresent(UInt32.self, forKey: .doneJobs) ?? 0
        coverage = try container.decodeIfPresent(Double.self, forKey: .coverage) ?? 0
        semanticUnavailable = try container.decodeIfPresent(
            String.self,
            forKey: .semanticUnavailable
        )
        lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
        usage = try container.decodeIfPresent(SourceEmbeddingUsage.self, forKey: .usage)
            ?? SourceEmbeddingUsage.empty
        recentJobs = try container.decodeIfPresent(
            [SourceEmbeddingJobStatus].self,
            forKey: .recentJobs
        ) ?? []
        integrity = try container.decodeIfPresent(
            SourceEmbeddingIntegrity.self,
            forKey: .integrity
        ) ?? SourceEmbeddingIntegrity.empty
    }
}

struct SourceEmbeddingReindexResult: Codable, Sendable {
    let projectId: String
    let enabled: Bool
    let model: String?
    let clearedVectors: UInt32
    let clearedJobs: UInt32
    let enqueuedSources: UInt32
    let maxChunksPerSource: UInt32

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case enabled
        case model
        case clearedVectors = "cleared_vectors"
        case clearedJobs = "cleared_jobs"
        case enqueuedSources = "enqueued_sources"
        case maxChunksPerSource = "max_chunks_per_source"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decode(String.self, forKey: .projectId)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        model = try container.decodeIfPresent(String.self, forKey: .model)
        clearedVectors = try container.decodeIfPresent(UInt32.self, forKey: .clearedVectors) ?? 0
        clearedJobs = try container.decodeIfPresent(UInt32.self, forKey: .clearedJobs) ?? 0
        enqueuedSources = try container.decodeIfPresent(UInt32.self, forKey: .enqueuedSources) ?? 0
        maxChunksPerSource = try container.decodeIfPresent(
            UInt32.self,
            forKey: .maxChunksPerSource
        ) ?? 0
    }
}

enum SourceRerankMode: String, CaseIterable, Identifiable, Codable, Equatable, Sendable {
    case off
    case deterministic

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .off: L10n.t("Off")
        case .deterministic: L10n.t("Deterministic")
        }
    }
}

enum SourceQueryExpansionMode: String, CaseIterable, Identifiable, Codable, Equatable, Sendable {
    case off
    case lexical

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .off: L10n.t("Off")
        case .lexical: L10n.t("Lexical")
        }
    }
}

/// Retrieval v2 controls layered on top of the opt-in source embedding policy.
/// ANN and expansion are off by default; rerank defaults to the deterministic,
/// model-free blend. The daemon clamps numeric values on write.
struct SourceRetrievalPolicy: Codable, Equatable, Sendable {
    var annEnabled: Bool
    var annMinVectors: Int
    var annNProbe: Int
    var rerank: SourceRerankMode
    var queryExpansion: SourceQueryExpansionMode
    var maxExpandedTerms: Int

    static let defaults = SourceRetrievalPolicy(
        annEnabled: false,
        annMinVectors: 256,
        annNProbe: 8,
        rerank: .deterministic,
        queryExpansion: .off,
        maxExpandedTerms: 8
    )

    enum CodingKeys: String, CodingKey {
        case annEnabled = "ann_enabled"
        case annMinVectors = "ann_min_vectors"
        case annNProbe = "ann_nprobe"
        case rerank
        case queryExpansion = "query_expansion"
        case maxExpandedTerms = "max_expanded_terms"
    }

    init(
        annEnabled: Bool,
        annMinVectors: Int,
        annNProbe: Int,
        rerank: SourceRerankMode,
        queryExpansion: SourceQueryExpansionMode,
        maxExpandedTerms: Int
    ) {
        self.annEnabled = annEnabled
        self.annMinVectors = annMinVectors
        self.annNProbe = annNProbe
        self.rerank = rerank
        self.queryExpansion = queryExpansion
        self.maxExpandedTerms = maxExpandedTerms
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        annEnabled = try container.decodeIfPresent(Bool.self, forKey: .annEnabled) ?? false
        annMinVectors = try container.decodeIfPresent(Int.self, forKey: .annMinVectors) ?? 256
        annNProbe = try container.decodeIfPresent(Int.self, forKey: .annNProbe) ?? 8
        rerank = try container.decodeIfPresent(SourceRerankMode.self, forKey: .rerank)
            ?? .deterministic
        queryExpansion = try container.decodeIfPresent(
            SourceQueryExpansionMode.self,
            forKey: .queryExpansion
        ) ?? .off
        maxExpandedTerms = try container.decodeIfPresent(Int.self, forKey: .maxExpandedTerms)
            ?? 8
    }

    func jsonObject() -> [String: JSONValue] {
        [
            "ann_enabled": .bool(annEnabled),
            "ann_min_vectors": .number(Double(annMinVectors)),
            "ann_nprobe": .number(Double(annNProbe)),
            "rerank": .string(rerank.rawValue),
            "query_expansion": .string(queryExpansion.rawValue),
            "max_expanded_terms": .number(Double(maxExpandedTerms)),
        ]
    }
}

struct SourceRetrievalPolicyResult: Codable, Sendable {
    let projectId: String
    let policy: SourceRetrievalPolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct SourceIndexStatusResult: Codable, Sendable {
    let projectId: String
    let policy: SourceRetrievalPolicy
    let embeddingEnabled: Bool
    let embeddingConfigured: Bool
    let model: String?
    let dimensions: Int
    let annEnabled: Bool
    let ready: Bool
    let path: String
    let generation: UInt64?
    let vectorCount: UInt32
    let assignedVectors: UInt32
    let invalidAssignments: UInt32
    let centroidCount: UInt32
    let nlist: UInt32
    let nprobe: UInt32
    let minVectors: UInt32
    let coverage: Double
    let bytes: UInt64
    let fallbackReason: String?
    let lastRebuiltAt: String?
    let lastError: String?

    var activePathLabel: String {
        path == "ann" ? L10n.t("ANN") : L10n.t("Exact scan")
    }

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
        case embeddingEnabled = "embedding_enabled"
        case embeddingConfigured = "embedding_configured"
        case model
        case dimensions
        case annEnabled = "ann_enabled"
        case ready
        case path
        case generation
        case vectorCount = "vector_count"
        case assignedVectors = "assigned_vectors"
        case invalidAssignments = "invalid_assignments"
        case centroidCount = "centroid_count"
        case nlist
        case nprobe
        case minVectors = "min_vectors"
        case coverage
        case bytes
        case fallbackReason = "fallback_reason"
        case lastRebuiltAt = "last_rebuilt_at"
        case lastError = "last_error"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decode(String.self, forKey: .projectId)
        policy = try container.decodeIfPresent(SourceRetrievalPolicy.self, forKey: .policy)
            ?? .defaults
        embeddingEnabled = try container.decodeIfPresent(
            Bool.self,
            forKey: .embeddingEnabled
        ) ?? false
        embeddingConfigured = try container.decodeIfPresent(
            Bool.self,
            forKey: .embeddingConfigured
        ) ?? false
        model = try container.decodeIfPresent(String.self, forKey: .model)
        dimensions = try container.decodeIfPresent(Int.self, forKey: .dimensions) ?? 0
        annEnabled = try container.decodeIfPresent(Bool.self, forKey: .annEnabled) ?? false
        ready = try container.decodeIfPresent(Bool.self, forKey: .ready) ?? false
        path = try container.decodeIfPresent(String.self, forKey: .path) ?? "exact"
        generation = try container.decodeIfPresent(UInt64.self, forKey: .generation)
        vectorCount = try container.decodeIfPresent(UInt32.self, forKey: .vectorCount) ?? 0
        assignedVectors = try container.decodeIfPresent(UInt32.self, forKey: .assignedVectors)
            ?? 0
        invalidAssignments = try container.decodeIfPresent(
            UInt32.self,
            forKey: .invalidAssignments
        ) ?? 0
        centroidCount = try container.decodeIfPresent(UInt32.self, forKey: .centroidCount) ?? 0
        nlist = try container.decodeIfPresent(UInt32.self, forKey: .nlist) ?? 0
        nprobe = try container.decodeIfPresent(UInt32.self, forKey: .nprobe) ?? 0
        minVectors = try container.decodeIfPresent(UInt32.self, forKey: .minVectors) ?? 0
        coverage = try container.decodeIfPresent(Double.self, forKey: .coverage) ?? 0
        bytes = try container.decodeIfPresent(UInt64.self, forKey: .bytes) ?? 0
        fallbackReason = try container.decodeIfPresent(String.self, forKey: .fallbackReason)
        lastRebuiltAt = try container.decodeIfPresent(String.self, forKey: .lastRebuiltAt)
        lastError = try container.decodeIfPresent(String.self, forKey: .lastError)
    }
}

struct SourceIndexRebuildResult: Codable, Sendable {
    let projectId: String
    let rebuilt: Bool
    let reason: String?
    let generation: UInt64?
    let vectorCount: UInt32
    let assignedVectors: UInt32
    let centroidCount: UInt32
    let nlist: UInt32
    let nprobe: UInt32
    let path: String

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case rebuilt
        case reason
        case generation
        case vectorCount = "vector_count"
        case assignedVectors = "assigned_vectors"
        case centroidCount = "centroid_count"
        case nlist
        case nprobe
        case path
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decode(String.self, forKey: .projectId)
        rebuilt = try container.decodeIfPresent(Bool.self, forKey: .rebuilt) ?? false
        reason = try container.decodeIfPresent(String.self, forKey: .reason)
        generation = try container.decodeIfPresent(UInt64.self, forKey: .generation)
        vectorCount = try container.decodeIfPresent(UInt32.self, forKey: .vectorCount) ?? 0
        assignedVectors = try container.decodeIfPresent(UInt32.self, forKey: .assignedVectors)
            ?? 0
        centroidCount = try container.decodeIfPresent(UInt32.self, forKey: .centroidCount) ?? 0
        nlist = try container.decodeIfPresent(UInt32.self, forKey: .nlist) ?? 0
        nprobe = try container.decodeIfPresent(UInt32.self, forKey: .nprobe) ?? 0
        path = try container.decodeIfPresent(String.self, forKey: .path) ?? "exact"
    }
}

extension SourceEmbeddingUsage {
    static let empty = SourceEmbeddingUsage(
        calls: 0,
        inputTokens: 0,
        estimatedTokens: 0,
        bytes: 0,
        providerReportedCalls: 0
    )

    init(
        calls: UInt64,
        inputTokens: UInt64,
        estimatedTokens: UInt64,
        bytes: UInt64,
        providerReportedCalls: UInt64
    ) {
        self.calls = calls
        self.inputTokens = inputTokens
        self.estimatedTokens = estimatedTokens
        self.bytes = bytes
        self.providerReportedCalls = providerReportedCalls
    }
}

extension SourceEmbeddingIntegrity {
    static let empty = SourceEmbeddingIntegrity(
        validVectors: 0,
        invalidVectors: 0,
        staleVectors: 0,
        orphanVectors: 0,
        generationMismatched: 0
    )

    init(
        validVectors: UInt32,
        invalidVectors: UInt32,
        staleVectors: UInt32,
        orphanVectors: UInt32,
        generationMismatched: UInt32
    ) {
        self.validVectors = validVectors
        self.invalidVectors = invalidVectors
        self.staleVectors = staleVectors
        self.orphanVectors = orphanVectors
        self.generationMismatched = generationMismatched
    }
}

struct SourceCitation: Codable, Identifiable, Hashable, Sendable {
    let key: String
    let sourceID: String
    let sourceTitle: String
    let chunkID: String
    let chunkNo: UInt32
    let heading: String?
    let excerpt: String
    let score: Double
    let origin: String

    var id: String {
        key
    }

    enum CodingKeys: String, CodingKey {
        case key
        case sourceID = "source_id"
        case sourceTitle = "source_title"
        case chunkID = "chunk_id"
        case chunkNo = "chunk_no"
        case heading
        case excerpt
        case score
        case origin
    }
}

struct SourceDeleteResult: Codable, Sendable {
    let deleted: Bool
}

struct SourceReindexResult: Codable, Sendable {
    let source: ProjectSource
    let chunks: Int
}

struct ProjectSkill: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectID: String
    let name: String
    let slug: String
    let description: String
    let whenToUse: String
    let version: String
    let instructions: String
    let toolAllow: [String]
    let toolDeny: [String]
    let profiles: [String]
    let roles: [String]
    let enabled: Bool
    let source: String
    let sha256: String
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case projectID = "project_id"
        case name
        case slug
        case description
        case whenToUse = "when_to_use"
        case version
        case instructions
        case toolAllow = "tool_allow"
        case toolDeny = "tool_deny"
        case profiles
        case roles
        case enabled
        case source
        case sha256
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectID = try container.decode(String.self, forKey: .projectID)
        name = try container.decode(String.self, forKey: .name)
        slug = try container.decodeIfPresent(String.self, forKey: .slug) ?? ""
        description = try container.decodeIfPresent(String.self, forKey: .description) ?? ""
        whenToUse = try container.decodeIfPresent(String.self, forKey: .whenToUse) ?? ""
        version = try container.decodeIfPresent(String.self, forKey: .version) ?? "1.0.0"
        instructions = try container.decodeIfPresent(String.self, forKey: .instructions) ?? ""
        toolAllow = try container.decodeIfPresent([String].self, forKey: .toolAllow) ?? []
        toolDeny = try container.decodeIfPresent([String].self, forKey: .toolDeny) ?? []
        profiles = try container.decodeIfPresent([String].self, forKey: .profiles) ?? []
        roles = try container.decodeIfPresent([String].self, forKey: .roles) ?? []
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        source = try container.decodeIfPresent(String.self, forKey: .source) ?? "user"
        sha256 = try container.decodeIfPresent(String.self, forKey: .sha256) ?? ""
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
    }
}

struct SkillListResult: Codable, Sendable {
    let skills: [ProjectSkill]
}

struct SkillDeleteResult: Codable, Sendable {
    let deleted: Bool
}

struct MessageCitation: Codable, Identifiable, Hashable, Sendable {
    let messageID: String
    let citation: SourceCitation

    var id: String {
        "\(messageID)-\(citation.key)"
    }

    enum CodingKeys: String, CodingKey {
        case messageID = "message_id"
        case citation
    }
}

struct SessionCitationsResult: Codable, Sendable {
    let citations: [MessageCitation]
}

struct SourceCollection: Codable, Identifiable, Hashable, Sendable {
    let name: String?
    let sources: Int
    let chunks: Int
    let bytes: UInt64
    let tokens: UInt64
    let labels: [String]

    var id: String {
        name ?? ""
    }

    var displayName: String {
        name ?? L10n.t("Uncollected")
    }
}

struct SourceCollectionsResult: Codable, Sendable {
    let collections: [SourceCollection]
    let totalSources: Int

    enum CodingKeys: String, CodingKey {
        case collections
        case totalSources = "total_sources"
    }
}

struct TextDiffLine: Codable, Identifiable, Hashable, Sendable {
    let kind: String
    let oldLine: Int?
    let newLine: Int?
    let text: String

    var id: String {
        "\(oldLine ?? 0)-\(newLine ?? 0)-\(kind)-\(text)"
    }

    enum CodingKeys: String, CodingKey {
        case kind
        case oldLine = "old_line"
        case newLine = "new_line"
        case text
    }
}

struct TextDiffHunk: Codable, Identifiable, Hashable, Sendable {
    let oldStart: Int
    let oldLines: Int
    let newStart: Int
    let newLines: Int
    let lines: [TextDiffLine]

    var id: String {
        "\(oldStart)-\(oldLines)-\(newStart)-\(newLines)"
    }

    enum CodingKeys: String, CodingKey {
        case oldStart = "old_start"
        case oldLines = "old_lines"
        case newStart = "new_start"
        case newLines = "new_lines"
        case lines
    }
}

struct TextDiff: Codable, Hashable, Sendable {
    let hunks: [TextDiffHunk]
    let additions: Int
    let deletions: Int
    let unchanged: Int
    let identical: Bool
    let truncated: Bool
}

struct ArtifactDiffResult: Codable, Sendable {
    let artifactID: String
    let name: String
    let fromVersion: Int
    let toVersion: Int
    let diff: TextDiff

    enum CodingKeys: String, CodingKey {
        case artifactID = "artifact_id"
        case name
        case fromVersion = "from_version"
        case toVersion = "to_version"
        case diff
    }
}

struct SkillActivation: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let slug: String
    let version: String
    let role: String
    let profile: String
    let toolAllow: [String]
    let toolDeny: [String]
    let agentID: String?
    let agentName: String?
    let taskID: String?
    let activatedAt: String

    var id: String {
        "\(name)-\(activatedAt)-\(agentID ?? "")"
    }

    enum CodingKeys: String, CodingKey {
        case name
        case slug
        case version
        case role
        case profile
        case toolAllow = "tool_allow"
        case toolDeny = "tool_deny"
        case agentID = "agent_id"
        case agentName = "agent_name"
        case taskID = "task_id"
        case activatedAt = "activated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        slug = try container.decodeIfPresent(String.self, forKey: .slug) ?? ""
        version = try container.decodeIfPresent(String.self, forKey: .version) ?? "1.0.0"
        role = try container.decodeIfPresent(String.self, forKey: .role) ?? ""
        profile = try container.decodeIfPresent(String.self, forKey: .profile) ?? ""
        toolAllow = try container.decodeIfPresent([String].self, forKey: .toolAllow) ?? []
        toolDeny = try container.decodeIfPresent([String].self, forKey: .toolDeny) ?? []
        agentID = try container.decodeIfPresent(String.self, forKey: .agentID)
        agentName = try container.decodeIfPresent(String.self, forKey: .agentName)
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        activatedAt = try container.decodeIfPresent(String.self, forKey: .activatedAt) ?? ""
    }
}

struct SkillActivationsResult: Codable, Sendable {
    let activations: [SkillActivation]
}

struct CitationAuditMessage: Codable, Identifiable, Hashable, Sendable {
    let messageID: String
    let agentName: String?
    let excerpt: String
    let citationCount: Int
    let referencedKeys: [String]
    let danglingKeys: [String]
    let unusedKeys: [String]
    let createdAt: String

    var id: String {
        messageID
    }

    enum CodingKeys: String, CodingKey {
        case messageID = "message_id"
        case agentName = "agent_name"
        case excerpt
        case citationCount = "citation_count"
        case referencedKeys = "referenced_keys"
        case danglingKeys = "dangling_keys"
        case unusedKeys = "unused_keys"
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        messageID = try container.decode(String.self, forKey: .messageID)
        agentName = try container.decodeIfPresent(String.self, forKey: .agentName)
        excerpt = try container.decodeIfPresent(String.self, forKey: .excerpt) ?? ""
        citationCount = try container.decodeIfPresent(Int.self, forKey: .citationCount) ?? 0
        referencedKeys = try container.decodeIfPresent([String].self, forKey: .referencedKeys) ?? []
        danglingKeys = try container.decodeIfPresent([String].self, forKey: .danglingKeys) ?? []
        unusedKeys = try container.decodeIfPresent([String].self, forKey: .unusedKeys) ?? []
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

struct CitationAudit: Codable, Hashable, Sendable {
    let messages: [CitationAuditMessage]
    let messagesWithCitations: Int
    let messagesWithoutCitations: Int
    let totalCitations: Int
    let uniqueSources: Int
    let danglingKeyCount: Int
    let unusedCitationCount: Int

    enum CodingKeys: String, CodingKey {
        case messages
        case messagesWithCitations = "messages_with_citations"
        case messagesWithoutCitations = "messages_without_citations"
        case totalCitations = "total_citations"
        case uniqueSources = "unique_sources"
        case danglingKeyCount = "dangling_key_count"
        case unusedCitationCount = "unused_citation_count"
    }
}

struct SessionCitationAuditResult: Codable, Sendable {
    let audit: CitationAudit
}

struct SessionUnlimitedResult: Codable, Sendable {
    let sessionID: String
    let unlimited: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case unlimited
    }
}

struct BackupAnchorStatus: Codable, Sendable, Identifiable {
    let repositoryID: String
    let repositoryPath: String
    let highestGeneration: UInt64
    let lastVerifiedGeneration: UInt64
    let lastSnapshotID: String?
    let lastOperation: String
    let firstSeenAt: String
    let lastVerifiedAt: String
    let origin: String?

    var id: String {
        repositoryID
    }

    enum CodingKeys: String, CodingKey {
        case repositoryID = "repository_id"
        case repositoryPath = "repository_path"
        case highestGeneration = "highest_generation"
        case lastVerifiedGeneration = "last_verified_generation"
        case lastSnapshotID = "last_snapshot_id"
        case lastOperation = "last_operation"
        case firstSeenAt = "first_seen_at"
        case lastVerifiedAt = "last_verified_at"
        case origin
    }
}

struct BackupSigningKeyStatus: Codable, Sendable, Identifiable {
    let keyID: String
    let algorithm: String
    let publicKeyB64: String
    let createdAt: String
    let retiredAt: String?
    let revokedAt: String?
    let active: Bool

    var id: String {
        keyID
    }

    enum CodingKeys: String, CodingKey {
        case keyID = "key_id"
        case algorithm
        case publicKeyB64 = "public_key_b64"
        case createdAt = "created_at"
        case retiredAt = "retired_at"
        case revokedAt = "revoked_at"
        case active
    }
}

struct BackupStatusResult: Codable, Sendable {
    let anchorPath: String
    let anchorPresent: Bool
    let anchorUpdatedAt: String?
    let repositories: [BackupAnchorStatus]
    let keyringPath: String
    let keyringPresent: Bool
    let activeKeyID: String?
    let keyCount: Int
    let keys: [BackupSigningKeyStatus]
    let signedRepositoryFormats: [Int]
    let revokedKeyCount: Int?
    let portableAnchorFormatVersion: Int?
    let importedRepositories: Int?
    let note: String

    enum CodingKeys: String, CodingKey {
        case anchorPath = "anchor_path"
        case anchorPresent = "anchor_present"
        case anchorUpdatedAt = "anchor_updated_at"
        case repositories
        case keyringPath = "keyring_path"
        case keyringPresent = "keyring_present"
        case activeKeyID = "active_key_id"
        case keyCount = "key_count"
        case keys
        case signedRepositoryFormats = "signed_repository_formats"
        case revokedKeyCount = "revoked_key_count"
        case portableAnchorFormatVersion = "portable_anchor_format_version"
        case importedRepositories = "imported_repositories"
        case note
    }
}
