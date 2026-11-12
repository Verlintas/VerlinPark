import Foundation

/// Project-scoped OpenTelemetry GenAI export policy. Export is explicit only;
/// `enabled` gates `telemetry.otel.export`, not a background producer.
struct OtelPolicy: Codable, Sendable, Equatable {
    let enabled: Bool
    let endpoint: String
    let serviceName: String
    let batchLimit: Int
    let timeoutSecs: Int

    enum CodingKeys: String, CodingKey {
        case enabled
        case endpoint
        case serviceName = "service_name"
        case batchLimit = "batch_limit"
        case timeoutSecs = "timeout_secs"
    }

    init(
        enabled: Bool = false,
        endpoint: String = "http://127.0.0.1:4318",
        serviceName: String = "verlinpark",
        batchLimit: Int = 200,
        timeoutSecs: Int = 5
    ) {
        self.enabled = enabled
        self.endpoint = endpoint
        self.serviceName = serviceName
        self.batchLimit = batchLimit
        self.timeoutSecs = timeoutSecs
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        endpoint = try container.decodeIfPresent(String.self, forKey: .endpoint)
            ?? "http://127.0.0.1:4318"
        serviceName = try container.decodeIfPresent(String.self, forKey: .serviceName)
            ?? "verlinpark"
        batchLimit = try container.decodeIfPresent(Int.self, forKey: .batchLimit) ?? 200
        timeoutSecs = try container.decodeIfPresent(Int.self, forKey: .timeoutSecs) ?? 5
    }

    /// Loopback is the only endpoint class accepted by the daemon. The GUI only
    /// displays this derived hint; it never treats it as an authorization check.
    var endpointLooksLoopback: Bool {
        guard let url = URL(string: endpoint), let host = url.host else { return false }
        let bare = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        if bare.caseInsensitiveCompare("localhost") == .orderedSame {
            return true
        }
        return bare == "::1" || bare.hasPrefix("127.")
    }

    var statusLabel: String {
        enabled ? "Explicit export enabled" : "Export disabled"
    }
}

struct OtelPolicyEnvelope: Codable, Sendable, Equatable {
    let projectID: String
    let policy: OtelPolicy
    let endpointValid: Bool
    let endpointLoopback: Bool
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case policy
        case endpointValid = "endpoint_valid"
        case endpointLoopback = "endpoint_loopback"
        case warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        policy = try container.decodeIfPresent(OtelPolicy.self, forKey: .policy) ?? OtelPolicy()
        endpointValid = try container.decodeIfPresent(Bool.self, forKey: .endpointValid) ?? false
        endpointLoopback =
            try container.decodeIfPresent(Bool.self, forKey: .endpointLoopback) ?? false
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}

struct OtelProjectionCounts: Codable, Sendable, Equatable {
    let sessionSpans: Int
    let agentSpans: Int
    let toolSpans: Int
    let usageSpans: Int
    let totalSpans: Int
    let eventsExamined: Int
    let eventsMapped: Int
    let eventsSkipped: Int
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case sessionSpans = "session_spans"
        case agentSpans = "agent_spans"
        case toolSpans = "tool_spans"
        case usageSpans = "usage_spans"
        case totalSpans = "total_spans"
        case eventsExamined = "events_examined"
        case eventsMapped = "events_mapped"
        case eventsSkipped = "events_skipped"
        case truncated
    }

    init() {
        sessionSpans = 0
        agentSpans = 0
        toolSpans = 0
        usageSpans = 0
        totalSpans = 0
        eventsExamined = 0
        eventsMapped = 0
        eventsSkipped = 0
        truncated = false
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionSpans = try container.decodeIfPresent(Int.self, forKey: .sessionSpans) ?? 0
        agentSpans = try container.decodeIfPresent(Int.self, forKey: .agentSpans) ?? 0
        toolSpans = try container.decodeIfPresent(Int.self, forKey: .toolSpans) ?? 0
        usageSpans = try container.decodeIfPresent(Int.self, forKey: .usageSpans) ?? 0
        totalSpans = try container.decodeIfPresent(Int.self, forKey: .totalSpans) ?? 0
        eventsExamined = try container.decodeIfPresent(Int.self, forKey: .eventsExamined) ?? 0
        eventsMapped = try container.decodeIfPresent(Int.self, forKey: .eventsMapped) ?? 0
        eventsSkipped = try container.decodeIfPresent(Int.self, forKey: .eventsSkipped) ?? 0
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }
}

/// Read-only projection status. No prompt, tool argument, tool result, file
/// content, or secret is part of this wire shape.
struct OtelStatusReport: Codable, Sendable, Equatable {
    let projectID: String
    let sessionID: String?
    let policy: OtelPolicy
    let enabled: Bool
    let endpointValid: Bool
    let endpointLoopback: Bool
    let tracesPath: String
    let counts: OtelProjectionCounts
    let lastEventSeq: Int
    let generatedAt: String
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case sessionID = "session_id"
        case policy
        case enabled
        case endpointValid = "endpoint_valid"
        case endpointLoopback = "endpoint_loopback"
        case tracesPath = "traces_path"
        case counts
        case lastEventSeq = "last_event_seq"
        case generatedAt = "generated_at"
        case warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID)
        policy = try container.decodeIfPresent(OtelPolicy.self, forKey: .policy) ?? OtelPolicy()
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        endpointValid = try container.decodeIfPresent(Bool.self, forKey: .endpointValid) ?? false
        endpointLoopback =
            try container.decodeIfPresent(Bool.self, forKey: .endpointLoopback) ?? false
        tracesPath = try container.decodeIfPresent(String.self, forKey: .tracesPath) ?? ""
        counts = try container.decodeIfPresent(OtelProjectionCounts.self, forKey: .counts)
            ?? OtelProjectionCounts()
        lastEventSeq = try container.decodeIfPresent(Int.self, forKey: .lastEventSeq) ?? 0
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }

    var countsLabel: String {
        "\(counts.totalSpans) span(s) · \(counts.eventsExamined) event(s)"
    }

    var endpointLabel: String {
        "\(policy.endpoint) → \(tracesPath.isEmpty ? "/v1/traces" : tracesPath)"
    }
}

struct OtelExportResult: Codable, Sendable, Equatable {
    let sessionID: String
    let exported: Bool
    let status: String
    let spanCount: Int
    let availableSpans: Int
    let truncated: Bool
    let httpStatus: Int?
    let durationMs: Int
    let endpoint: String
    let requestBytes: Int
    let errorCode: String?
    let message: String?
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case exported
        case status
        case spanCount = "span_count"
        case availableSpans = "available_spans"
        case truncated
        case httpStatus = "http_status"
        case durationMs = "duration_ms"
        case endpoint
        case requestBytes = "request_bytes"
        case errorCode = "error_code"
        case message
        case generatedAt = "generated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        exported = try container.decodeIfPresent(Bool.self, forKey: .exported) ?? false
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        spanCount = try container.decodeIfPresent(Int.self, forKey: .spanCount) ?? 0
        availableSpans = try container.decodeIfPresent(Int.self, forKey: .availableSpans) ?? 0
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        httpStatus = try container.decodeIfPresent(Int.self, forKey: .httpStatus)
        durationMs = try container.decodeIfPresent(Int.self, forKey: .durationMs) ?? 0
        endpoint = try container.decodeIfPresent(String.self, forKey: .endpoint) ?? ""
        requestBytes = try container.decodeIfPresent(Int.self, forKey: .requestBytes) ?? 0
        errorCode = try container.decodeIfPresent(String.self, forKey: .errorCode)
        message = try container.decodeIfPresent(String.self, forKey: .message)
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
    }

    var statusLabel: String {
        if exported {
            return "EXPORTED"
        }
        if status == "empty" {
            return "EMPTY"
        }
        return "FAILED"
    }
}

struct OtelTestResult: Codable, Sendable, Equatable {
    let projectID: String
    let endpoint: String
    let ok: Bool
    let httpStatus: Int?
    let durationMs: Int
    let requestBytes: Int
    let spanCount: Int
    let errorCode: String?
    let message: String?
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case endpoint
        case ok
        case httpStatus = "http_status"
        case durationMs = "duration_ms"
        case requestBytes = "request_bytes"
        case spanCount = "span_count"
        case errorCode = "error_code"
        case message
        case generatedAt = "generated_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        endpoint = try container.decodeIfPresent(String.self, forKey: .endpoint) ?? ""
        ok = try container.decodeIfPresent(Bool.self, forKey: .ok) ?? false
        httpStatus = try container.decodeIfPresent(Int.self, forKey: .httpStatus)
        durationMs = try container.decodeIfPresent(Int.self, forKey: .durationMs) ?? 0
        requestBytes = try container.decodeIfPresent(Int.self, forKey: .requestBytes) ?? 0
        spanCount = try container.decodeIfPresent(Int.self, forKey: .spanCount) ?? 0
        errorCode = try container.decodeIfPresent(String.self, forKey: .errorCode)
        message = try container.decodeIfPresent(String.self, forKey: .message)
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
    }
}
