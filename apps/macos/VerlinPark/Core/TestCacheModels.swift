import Foundation

// MARK: - Test-result cache wire models (ADR-0178)

struct TestCachePolicy: Codable, Hashable, Sendable {
    var enabled: Bool
    var maxEntries: Int
    var retentionDays: Int
    var maxOutputBytes: Int
    var reuseLint: Bool
    var includeIntegration: Bool
    var failedRetentionSecs: Int
    var extraEnv: [String]
    var extraPaths: [String]

    static let `default` = TestCachePolicy(
        enabled: true,
        maxEntries: 512,
        retentionDays: 14,
        maxOutputBytes: 256 * 1024,
        reuseLint: true,
        includeIntegration: false,
        failedRetentionSecs: 900,
        extraEnv: [],
        extraPaths: []
    )

    enum CodingKeys: String, CodingKey {
        case enabled
        case maxEntries = "max_entries"
        case retentionDays = "retention_days"
        case maxOutputBytes = "max_output_bytes"
        case reuseLint = "reuse_lint"
        case includeIntegration = "include_integration"
        case failedRetentionSecs = "failed_retention_secs"
        case extraEnv = "extra_env"
        case extraPaths = "extra_paths"
    }

    init(
        enabled: Bool,
        maxEntries: Int,
        retentionDays: Int,
        maxOutputBytes: Int,
        reuseLint: Bool,
        includeIntegration: Bool,
        failedRetentionSecs: Int,
        extraEnv: [String] = [],
        extraPaths: [String] = []
    ) {
        self.enabled = enabled
        self.maxEntries = maxEntries
        self.retentionDays = retentionDays
        self.maxOutputBytes = maxOutputBytes
        self.reuseLint = reuseLint
        self.includeIntegration = includeIntegration
        self.failedRetentionSecs = failedRetentionSecs
        self.extraEnv = extraEnv
        self.extraPaths = extraPaths
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        maxEntries = try container.decodeIfPresent(Int.self, forKey: .maxEntries) ?? 512
        retentionDays = try container.decodeIfPresent(Int.self, forKey: .retentionDays) ?? 14
        maxOutputBytes = try container.decodeIfPresent(Int.self, forKey: .maxOutputBytes)
            ?? 256 * 1024
        reuseLint = try container.decodeIfPresent(Bool.self, forKey: .reuseLint) ?? true
        includeIntegration = try container.decodeIfPresent(Bool.self, forKey: .includeIntegration)
            ?? false
        failedRetentionSecs = try container.decodeIfPresent(Int.self, forKey: .failedRetentionSecs)
            ?? 900
        extraEnv = try container.decodeIfPresent([String].self, forKey: .extraEnv) ?? []
        extraPaths = try container.decodeIfPresent([String].self, forKey: .extraPaths) ?? []
    }
}

struct TestCachePolicyResult: Codable, Sendable {
    let projectId: String
    let policy: TestCachePolicy

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case policy
    }
}

struct TestCacheEntry: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectId: String
    let sessionId: String
    let kind: String
    let command: String
    let commandSource: String
    let keyVersion: Int
    let cacheKey: String
    let inputsHash: String
    let impactGeneration: String
    let status: String
    let exitCode: Int
    let durationMs: Int
    let outputRef: String?
    let outputBytes: Int
    let coveredPaths: [String]
    let warnings: [String]
    let warningCount: Int
    let failureCount: Int
    let createdAt: String
    let expiresAt: String
    let hitCount: Int
    let lastHitAt: String?

    var isPass: Bool {
        status == "passed"
    }

    var shortInputsHash: String {
        String(inputsHash.prefix(12))
    }

    var shortCacheKey: String {
        String(cacheKey.suffix(12))
    }

    enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case sessionId = "session_id"
        case kind
        case command
        case commandSource = "command_source"
        case keyVersion = "key_version"
        case cacheKey = "cache_key"
        case inputsHash = "inputs_hash"
        case impactGeneration = "impact_generation"
        case status
        case exitCode = "exit_code"
        case durationMs = "duration_ms"
        case outputRef = "output_ref"
        case outputBytes = "output_bytes"
        case coveredPaths = "covered_paths"
        case warnings
        case warningCount = "warning_count"
        case failureCount = "failure_count"
        case createdAt = "created_at"
        case expiresAt = "expires_at"
        case hitCount = "hit_count"
        case lastHitAt = "last_hit_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectId = try container.decode(String.self, forKey: .projectId)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        kind = try container.decode(String.self, forKey: .kind)
        command = try container.decode(String.self, forKey: .command)
        commandSource = try container.decodeIfPresent(String.self, forKey: .commandSource) ?? ""
        keyVersion = try container.decodeIfPresent(Int.self, forKey: .keyVersion) ?? 1
        cacheKey = try container.decode(String.self, forKey: .cacheKey)
        inputsHash = try container.decode(String.self, forKey: .inputsHash)
        impactGeneration = try container.decode(String.self, forKey: .impactGeneration)
        status = try container.decode(String.self, forKey: .status)
        exitCode = try container.decodeIfPresent(Int.self, forKey: .exitCode) ?? 0
        durationMs = try container.decodeIfPresent(Int.self, forKey: .durationMs) ?? 0
        outputRef = try container.decodeIfPresent(String.self, forKey: .outputRef)
        outputBytes = try container.decodeIfPresent(Int.self, forKey: .outputBytes) ?? 0
        coveredPaths = try container.decodeIfPresent([String].self, forKey: .coveredPaths) ?? []
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
        warningCount = try container.decodeIfPresent(Int.self, forKey: .warningCount) ?? 0
        failureCount = try container.decodeIfPresent(Int.self, forKey: .failureCount) ?? 0
        createdAt = try container.decode(String.self, forKey: .createdAt)
        expiresAt = try container.decode(String.self, forKey: .expiresAt)
        hitCount = try container.decodeIfPresent(Int.self, forKey: .hitCount) ?? 0
        lastHitAt = try container.decodeIfPresent(String.self, forKey: .lastHitAt)
    }
}

struct TestCacheStats: Codable, Sendable {
    let projectId: String
    let entries: Int
    let passed: Int
    let failed: Int
    let error: Int
    let expired: Int
    let totalHits: Int
    let outputBytes: Int
    let byKind: [String: Int]
    let oldestCreatedAt: String?
    let newestCreatedAt: String?
    let policy: TestCachePolicy
    let generatedAt: String

    var passRatio: Double {
        let finished = passed + failed + error
        return finished == 0 ? 0 : Double(passed) / Double(finished)
    }

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case entries
        case passed
        case failed
        case error
        case expired
        case totalHits = "total_hits"
        case outputBytes = "output_bytes"
        case byKind = "by_kind"
        case oldestCreatedAt = "oldest_created_at"
        case newestCreatedAt = "newest_created_at"
        case policy
        case generatedAt = "generated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try container.decode(String.self, forKey: .projectId)
        entries = try container.decodeIfPresent(Int.self, forKey: .entries) ?? 0
        passed = try container.decodeIfPresent(Int.self, forKey: .passed) ?? 0
        failed = try container.decodeIfPresent(Int.self, forKey: .failed) ?? 0
        error = try container.decodeIfPresent(Int.self, forKey: .error) ?? 0
        expired = try container.decodeIfPresent(Int.self, forKey: .expired) ?? 0
        totalHits = try container.decodeIfPresent(Int.self, forKey: .totalHits) ?? 0
        outputBytes = try container.decodeIfPresent(Int.self, forKey: .outputBytes) ?? 0
        byKind = try container.decodeIfPresent([String: Int].self, forKey: .byKind) ?? [:]
        oldestCreatedAt = try container.decodeIfPresent(String.self, forKey: .oldestCreatedAt)
        newestCreatedAt = try container.decodeIfPresent(String.self, forKey: .newestCreatedAt)
        policy = try container.decodeIfPresent(TestCachePolicy.self, forKey: .policy)
            ?? .default
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
    }
}

struct TestCacheEntriesResult: Codable, Sendable {
    let projectId: String
    let entries: [TestCacheEntry]
    let total: Int
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case entries
        case total
        case truncated
    }
}

struct TestCachePruneResult: Codable, Sendable {
    let projectId: String
    let pruned: Int
    let remaining: Int
    let dryRun: Bool
    let affectedSessions: Int
    let removedIds: [String]
    let reason: String

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case pruned
        case remaining
        case dryRun = "dry_run"
        case affectedSessions = "affected_sessions"
        case removedIds = "removed_ids"
        case reason
    }
}

struct TestCacheClearResult: Codable, Sendable {
    let projectId: String
    let cleared: Int
    let remaining: Int
    let affectedSessions: Int

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case cleared
        case remaining
        case affectedSessions = "affected_sessions"
    }
}

struct TestCacheProvenance: Codable, Hashable, Sendable {
    let decision: String
    let cacheHit: Bool
    let resultId: String?
    let cacheKey: String?
    let inputsHash: String?
    let impactGeneration: String?
    let cachedAt: String?
    let originSessionId: String?
    let status: String?
    let note: String

    enum CodingKeys: String, CodingKey {
        case decision
        case cacheHit = "cache_hit"
        case resultId = "result_id"
        case cacheKey = "cache_key"
        case inputsHash = "inputs_hash"
        case impactGeneration = "impact_generation"
        case cachedAt = "cached_at"
        case originSessionId = "origin_session_id"
        case status
        case note
    }
}

struct TestImpactTarget: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let reason: String
    let confidence: Double
    let source: String

    var id: String {
        "\(source)-\(path)"
    }

    var confidencePercent: Int {
        Int((confidence * 100).rounded())
    }

    var sourceLabel: String {
        switch source {
        case "profile": "Profile"
        case "lexical": "Lexical"
        case "lsp": "LSP"
        case "history": "History"
        default: source.capitalized
        }
    }
}

struct TestImpactMap: Codable, Sendable {
    let changedPaths: [String]
    let targets: [TestImpactTarget]
    let generation: String
    let profileHash: String
    let sources: [String: Int]
    let dependencyPaths: [String]
    let truncated: Bool
    let lspAvailable: Bool
    let lexicalOnly: Bool
    let notes: [String]

    enum CodingKeys: String, CodingKey {
        case changedPaths = "changed_paths"
        case targets
        case generation
        case profileHash = "profile_hash"
        case sources
        case dependencyPaths = "dependency_paths"
        case truncated
        case lspAvailable = "lsp_available"
        case lexicalOnly = "lexical_only"
        case notes
    }
}

struct TestCacheImpactResult: Codable, Sendable {
    let sessionId: String
    let projectId: String
    let map: TestImpactMap
    let testCommand: String?

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case projectId = "project_id"
        case map
        case testCommand = "test_command"
    }
}

enum TestCacheFormat {
    static func bytes(_ value: Int) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(value), countStyle: .file)
    }

    static func duration(_ milliseconds: Int) -> String {
        if milliseconds < 1000 {
            return "\(milliseconds)ms"
        }
        return String(format: "%.1fs", Double(milliseconds) / 1000)
    }

    static func timestamp(_ value: String?) -> String {
        guard let value else { return "never" }
        return value
            .replacingOccurrences(of: "T", with: " ")
            .replacingOccurrences(of: "Z", with: "Z")
    }
}

struct TestCacheScopeEntry: Codable, Hashable, Sendable {
    var kind: String
    var label: String
    var present: Bool
    var bytes: Int
    var complete: Bool
    var note: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case label
        case present
        case bytes
        case complete
        case note
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? "path"
        label = try container.decodeIfPresent(String.self, forKey: .label) ?? ""
        present = try container.decodeIfPresent(Bool.self, forKey: .present) ?? false
        bytes = try container.decodeIfPresent(Int.self, forKey: .bytes) ?? 0
        complete = try container.decodeIfPresent(Bool.self, forKey: .complete) ?? false
        note = try container.decodeIfPresent(String.self, forKey: .note)
    }
}

struct TestCacheScopePreview: Codable, Hashable, Sendable {
    var projectId: String
    var enabled: Bool
    var extraEnv: Int
    var extraPaths: Int
    var complete: Bool
    var notes: [String]
    var entries: [TestCacheScopeEntry]

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case enabled
        case extraEnv = "extra_env"
        case extraPaths = "extra_paths"
        case complete
        case notes
        case entries
    }
}
