import Foundation

/// Read-only failure attribution metadata (ADR-0198). The daemon owns the
/// graph, earliest-claim selection, cascade path, and evidence pointers. The
/// client never recomputes a hash, infers a missing edge, or turns a finding
/// into an action.
struct DiagnosisEvidenceView: Codable, Sendable, Identifiable, Equatable {
    var kind: String
    var reference: String
    var hash: String?
    var eventSeq: UInt64?
    var eventKind: String?
    var note: String?

    var id: String {
        "\(kind):\(reference):\(eventSeq ?? 0):\(hash ?? "")"
    }

    enum CodingKeys: String, CodingKey {
        case kind, reference, hash, note
        case eventSeq = "event_seq"
        case eventKind = "event_kind"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        reference = try container.decodeIfPresent(String.self, forKey: .reference) ?? ""
        hash = try container.decodeIfPresent(String.self, forKey: .hash)
        eventSeq = try container.decodeIfPresent(UInt64.self, forKey: .eventSeq)
        eventKind = try container.decodeIfPresent(String.self, forKey: .eventKind)
        note = try container.decodeIfPresent(String.self, forKey: .note)
    }
}

struct ProvenanceNodeView: Codable, Sendable, Identifiable, Equatable {
    var id: String
    var kind: String
    var label: String
    var taskID: String?
    var agentID: String?
    var agentName: String?
    var role: String?
    var identityID: String?
    var status: String?
    var atSeq: UInt64
    var at: String
    var evidence: [DiagnosisEvidenceView]

    enum CodingKeys: String, CodingKey {
        case id, kind, label, role, status, at, evidence
        case taskID = "task_id"
        case agentID = "agent_id"
        case agentName = "agent_name"
        case identityID = "identity_id"
        case atSeq = "at_seq"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        label = try container.decodeIfPresent(String.self, forKey: .label) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        agentID = try container.decodeIfPresent(String.self, forKey: .agentID)
        agentName = try container.decodeIfPresent(String.self, forKey: .agentName)
        role = try container.decodeIfPresent(String.self, forKey: .role)
        identityID = try container.decodeIfPresent(String.self, forKey: .identityID)
        status = try container.decodeIfPresent(String.self, forKey: .status)
        atSeq = try container.decodeIfPresent(UInt64.self, forKey: .atSeq) ?? 0
        at = try container.decodeIfPresent(String.self, forKey: .at) ?? ""
        evidence = try container.decodeIfPresent([DiagnosisEvidenceView].self, forKey: .evidence) ?? []
    }
}

struct ProvenanceEdgeView: Codable, Sendable, Identifiable, Equatable {
    var from: String
    var to: String
    var kind: String
    var atSeq: UInt64

    var id: String {
        "\(from):\(to):\(kind)"
    }

    enum CodingKeys: String, CodingKey {
        case from, to, kind
        case atSeq = "at_seq"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        from = try container.decodeIfPresent(String.self, forKey: .from) ?? ""
        to = try container.decodeIfPresent(String.self, forKey: .to) ?? ""
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        atSeq = try container.decodeIfPresent(UInt64.self, forKey: .atSeq) ?? 0
    }
}

struct ProvenanceGraphView: Codable, Sendable, Equatable {
    var sessionID: String
    var roots: [String]
    var nodes: [ProvenanceNodeView]
    var edges: [ProvenanceEdgeView]
    var truncated: Bool

    enum CodingKeys: String, CodingKey {
        case roots, nodes, edges, truncated
        case sessionID = "session_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        roots = try container.decodeIfPresent([String].self, forKey: .roots) ?? []
        nodes = try container.decodeIfPresent([ProvenanceNodeView].self, forKey: .nodes) ?? []
        edges = try container.decodeIfPresent([ProvenanceEdgeView].self, forKey: .edges) ?? []
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }
}

struct ProvenanceSummaryView: Codable, Sendable, Equatable {
    var nodes: UInt32
    var edges: UInt32
    var tasks: UInt32
    var agents: UInt32
    var artifacts: UInt32
    var commits: UInt32
    var toolCalls: UInt32
    var testEvidence: UInt32
    var verifications: UInt32
    var merges: UInt32
    var maxDepth: UInt32
    var depthLimited: Bool
    var truncated: Bool
    var scannedEvents: UInt32
    var totalEvents: UInt64
    var windowStartSeq: UInt64
    var windowEndSeq: UInt64
    var digest: String

    enum CodingKeys: String, CodingKey {
        case nodes, edges, tasks, agents, artifacts, commits, verifications, merges
        case toolCalls = "tool_calls"
        case testEvidence = "test_evidence"
        case maxDepth = "max_depth"
        case depthLimited = "depth_limited"
        case truncated
        case scannedEvents = "scanned_events"
        case totalEvents = "total_events"
        case windowStartSeq = "window_start_seq"
        case windowEndSeq = "window_end_seq"
        case digest
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        nodes = try container.decodeIfPresent(UInt32.self, forKey: .nodes) ?? 0
        edges = try container.decodeIfPresent(UInt32.self, forKey: .edges) ?? 0
        tasks = try container.decodeIfPresent(UInt32.self, forKey: .tasks) ?? 0
        agents = try container.decodeIfPresent(UInt32.self, forKey: .agents) ?? 0
        artifacts = try container.decodeIfPresent(UInt32.self, forKey: .artifacts) ?? 0
        commits = try container.decodeIfPresent(UInt32.self, forKey: .commits) ?? 0
        toolCalls = try container.decodeIfPresent(UInt32.self, forKey: .toolCalls) ?? 0
        testEvidence = try container.decodeIfPresent(UInt32.self, forKey: .testEvidence) ?? 0
        verifications = try container.decodeIfPresent(UInt32.self, forKey: .verifications) ?? 0
        merges = try container.decodeIfPresent(UInt32.self, forKey: .merges) ?? 0
        maxDepth = try container.decodeIfPresent(UInt32.self, forKey: .maxDepth) ?? 0
        depthLimited = try container.decodeIfPresent(Bool.self, forKey: .depthLimited) ?? false
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        scannedEvents = try container.decodeIfPresent(UInt32.self, forKey: .scannedEvents) ?? 0
        totalEvents = try container.decodeIfPresent(UInt64.self, forKey: .totalEvents) ?? 0
        windowStartSeq = try container.decodeIfPresent(UInt64.self, forKey: .windowStartSeq) ?? 0
        windowEndSeq = try container.decodeIfPresent(UInt64.self, forKey: .windowEndSeq) ?? 0
        digest = try container.decodeIfPresent(String.self, forKey: .digest) ?? ""
    }
}

struct AttributionAgentView: Codable, Sendable, Identifiable, Equatable {
    var agentID: String?
    var agentName: String?
    var role: String?
    var taskID: String?
    var identityID: String?
    var responsibility: String

    var id: String {
        "\(agentID ?? ""):\(agentName ?? ""):\(role ?? ""):\(responsibility)"
    }

    var displayName: String {
        agentName ?? agentID ?? "unknown agent"
    }

    enum CodingKeys: String, CodingKey {
        case role, responsibility
        case agentID = "agent_id"
        case agentName = "agent_name"
        case taskID = "task_id"
        case identityID = "identity_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        agentID = try container.decodeIfPresent(String.self, forKey: .agentID)
        agentName = try container.decodeIfPresent(String.self, forKey: .agentName)
        role = try container.decodeIfPresent(String.self, forKey: .role)
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        identityID = try container.decodeIfPresent(String.self, forKey: .identityID)
        responsibility = try container.decodeIfPresent(String.self, forKey: .responsibility) ?? ""
    }
}

struct AttributionFindingView: Codable, Sendable, Identifiable, Equatable {
    var findingID: String
    var reasonCode: String
    var severity: String
    var nodeID: String
    var taskID: String?
    var atSeq: UInt64
    var at: String
    var summary: String
    var agents: [AttributionAgentView]
    var evidence: [DiagnosisEvidenceView]
    var flags: [String]

    var id: String {
        findingID
    }

    enum CodingKeys: String, CodingKey {
        case severity, at, summary, agents, evidence, flags
        case findingID = "finding_id"
        case reasonCode = "reason_code"
        case nodeID = "node_id"
        case taskID = "task_id"
        case atSeq = "at_seq"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        findingID = try container.decodeIfPresent(String.self, forKey: .findingID) ?? ""
        reasonCode = try container.decodeIfPresent(String.self, forKey: .reasonCode) ?? "unknown"
        severity = try container.decodeIfPresent(String.self, forKey: .severity) ?? "contributing"
        nodeID = try container.decodeIfPresent(String.self, forKey: .nodeID) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        atSeq = try container.decodeIfPresent(UInt64.self, forKey: .atSeq) ?? 0
        at = try container.decodeIfPresent(String.self, forKey: .at) ?? ""
        summary = try container.decodeIfPresent(String.self, forKey: .summary) ?? ""
        agents = try container.decodeIfPresent([AttributionAgentView].self, forKey: .agents) ?? []
        evidence = try container.decodeIfPresent([DiagnosisEvidenceView].self, forKey: .evidence) ?? []
        flags = try container.decodeIfPresent([String].self, forKey: .flags) ?? []
    }
}

struct CascadeStepView: Codable, Sendable, Identifiable, Equatable {
    var nodeID: String
    var kind: String
    var taskID: String?
    var status: String?
    var atSeq: UInt64
    var relation: String

    var id: String {
        "\(nodeID):\(relation):\(atSeq)"
    }

    enum CodingKeys: String, CodingKey {
        case kind, status, relation
        case nodeID = "node_id"
        case taskID = "task_id"
        case atSeq = "at_seq"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        nodeID = try container.decodeIfPresent(String.self, forKey: .nodeID) ?? ""
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        status = try container.decodeIfPresent(String.self, forKey: .status)
        atSeq = try container.decodeIfPresent(UInt64.self, forKey: .atSeq) ?? 0
        relation = try container.decodeIfPresent(String.self, forKey: .relation) ?? ""
    }
}

struct DiagnosisTargetView: Codable, Sendable, Equatable {
    var kind: String
    var id: String
    var taskID: String?
    var status: String
    var atSeq: UInt64

    enum CodingKeys: String, CodingKey {
        case kind, id, status
        case taskID = "task_id"
        case atSeq = "at_seq"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        taskID = try container.decodeIfPresent(String.self, forKey: .taskID)
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        atSeq = try container.decodeIfPresent(UInt64.self, forKey: .atSeq) ?? 0
    }
}

struct DiagnosisAssignmentView: Codable, Sendable, Identifiable, Equatable {
    var identityID: String
    var role: String?
    var status: String
    var assignedAt: String
    var leaseExpiresAt: String?

    var id: String {
        identityID
    }

    enum CodingKeys: String, CodingKey {
        case role, status
        case identityID = "identity_id"
        case assignedAt = "assigned_at"
        case leaseExpiresAt = "lease_expires_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        identityID = try container.decodeIfPresent(String.self, forKey: .identityID) ?? ""
        role = try container.decodeIfPresent(String.self, forKey: .role)
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        assignedAt = try container.decodeIfPresent(String.self, forKey: .assignedAt) ?? ""
        leaseExpiresAt = try container.decodeIfPresent(String.self, forKey: .leaseExpiresAt)
    }
}

struct SessionDiagnosisReport: Codable, Sendable, Equatable {
    var schemaVersion: UInt32
    var sessionID: String
    var generatedAt: String
    var status: String
    var target: DiagnosisTargetView?
    var earliest: AttributionFindingView?
    var findings: [AttributionFindingView]
    var responsibleAgents: [AttributionAgentView]
    var cascade: [CascadeStepView]
    var graph: ProvenanceGraphView?
    var graphSummary: ProvenanceSummaryView
    var otherFailures: UInt32
    var assignments: [DiagnosisAssignmentView]
    var advisory: Bool
    var truncated: Bool
    var warnings: [String]
    var recordedEventSeq: UInt64?

    enum CodingKeys: String, CodingKey {
        case status, target, earliest, findings, cascade, graph, assignments, advisory, truncated, warnings
        case schemaVersion = "schema_version"
        case sessionID = "session_id"
        case generatedAt = "generated_at"
        case responsibleAgents = "responsible_agents"
        case graphSummary = "graph_summary"
        case otherFailures = "other_failures"
        case recordedEventSeq = "recorded_event_seq"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try container.decodeIfPresent(UInt32.self, forKey: .schemaVersion) ?? 0
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? "no_failure"
        target = try container.decodeIfPresent(DiagnosisTargetView.self, forKey: .target)
        earliest = try container.decodeIfPresent(AttributionFindingView.self, forKey: .earliest)
        findings = try container.decodeIfPresent([AttributionFindingView].self, forKey: .findings) ?? []
        responsibleAgents =
            try container.decodeIfPresent([AttributionAgentView].self, forKey: .responsibleAgents) ?? []
        cascade = try container.decodeIfPresent([CascadeStepView].self, forKey: .cascade) ?? []
        graph = try container.decodeIfPresent(ProvenanceGraphView.self, forKey: .graph)
        graphSummary =
            try container.decodeIfPresent(ProvenanceSummaryView.self, forKey: .graphSummary)
                ?? ProvenanceSummaryView.empty
        otherFailures = try container.decodeIfPresent(UInt32.self, forKey: .otherFailures) ?? 0
        assignments =
            try container.decodeIfPresent([DiagnosisAssignmentView].self, forKey: .assignments) ?? []
        advisory = try container.decodeIfPresent(Bool.self, forKey: .advisory) ?? true
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
        recordedEventSeq = try container.decodeIfPresent(UInt64.self, forKey: .recordedEventSeq)
    }

    var hasRootFinding: Bool {
        earliest != nil
    }

    var isAttributed: Bool {
        status == "attributed"
    }
}

struct FailureAttributionRecordView: Codable, Sendable, Equatable {
    var schemaVersion: UInt32
    var attributionID: String
    var sessionID: String
    var target: DiagnosisTargetView?
    var status: String
    var reasonCode: String
    var earliestNodeID: String
    var earliestSeq: UInt64
    var responsibleAgents: [AttributionAgentView]
    var findings: [AttributionFindingView]
    var cascade: [CascadeStepView]
    var graphDigest: String
    var graphNodes: UInt32
    var graphEdges: UInt32
    var truncated: Bool
    var advisory: Bool
    var createdAt: String

    var id: String {
        attributionID
    }

    enum CodingKeys: String, CodingKey {
        case status, findings, cascade, truncated, advisory
        case schemaVersion = "schema_version"
        case attributionID = "attribution_id"
        case sessionID = "session_id"
        case target
        case reasonCode = "reason_code"
        case earliestNodeID = "earliest_node_id"
        case earliestSeq = "earliest_seq"
        case responsibleAgents = "responsible_agents"
        case graphDigest = "graph_digest"
        case graphNodes = "graph_nodes"
        case graphEdges = "graph_edges"
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try container.decodeIfPresent(UInt32.self, forKey: .schemaVersion) ?? 0
        attributionID = try container.decodeIfPresent(String.self, forKey: .attributionID) ?? ""
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        target = try container.decodeIfPresent(DiagnosisTargetView.self, forKey: .target)
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""
        reasonCode = try container.decodeIfPresent(String.self, forKey: .reasonCode) ?? ""
        earliestNodeID = try container.decodeIfPresent(String.self, forKey: .earliestNodeID) ?? ""
        earliestSeq = try container.decodeIfPresent(UInt64.self, forKey: .earliestSeq) ?? 0
        responsibleAgents =
            try container.decodeIfPresent([AttributionAgentView].self, forKey: .responsibleAgents) ?? []
        findings = try container.decodeIfPresent([AttributionFindingView].self, forKey: .findings) ?? []
        cascade = try container.decodeIfPresent([CascadeStepView].self, forKey: .cascade) ?? []
        graphDigest = try container.decodeIfPresent(String.self, forKey: .graphDigest) ?? ""
        graphNodes = try container.decodeIfPresent(UInt32.self, forKey: .graphNodes) ?? 0
        graphEdges = try container.decodeIfPresent(UInt32.self, forKey: .graphEdges) ?? 0
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        advisory = try container.decodeIfPresent(Bool.self, forKey: .advisory) ?? true
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

struct RecordedAttributionView: Codable, Sendable, Identifiable, Equatable {
    var eventSeq: UInt64
    var recordedAt: String
    var attribution: FailureAttributionRecordView

    var id: UInt64 {
        eventSeq
    }

    enum CodingKeys: String, CodingKey {
        case attribution
        case eventSeq = "event_seq"
        case recordedAt = "recorded_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        eventSeq = try container.decodeIfPresent(UInt64.self, forKey: .eventSeq) ?? 0
        recordedAt = try container.decodeIfPresent(String.self, forKey: .recordedAt) ?? ""
        attribution =
            try container.decodeIfPresent(FailureAttributionRecordView.self, forKey: .attribution)
                ?? FailureAttributionRecordView.empty
    }
}

struct SessionAttributionsReport: Codable, Sendable, Equatable {
    var sessionID: String
    var attributions: [RecordedAttributionView]
    var total: UInt64
    var truncated: Bool

    enum CodingKeys: String, CodingKey {
        case attributions, total, truncated
        case sessionID = "session_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        attributions =
            try container.decodeIfPresent([RecordedAttributionView].self, forKey: .attributions) ?? []
        total = try container.decodeIfPresent(UInt64.self, forKey: .total) ?? 0
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }
}

extension ProvenanceSummaryView {
    init(
        nodes: UInt32,
        edges: UInt32,
        tasks: UInt32,
        agents: UInt32,
        artifacts: UInt32,
        commits: UInt32,
        toolCalls: UInt32,
        testEvidence: UInt32,
        verifications: UInt32,
        merges: UInt32,
        maxDepth: UInt32,
        depthLimited: Bool,
        truncated: Bool,
        scannedEvents: UInt32,
        totalEvents: UInt64,
        windowStartSeq: UInt64,
        windowEndSeq: UInt64,
        digest: String
    ) {
        self.nodes = nodes
        self.edges = edges
        self.tasks = tasks
        self.agents = agents
        self.artifacts = artifacts
        self.commits = commits
        self.toolCalls = toolCalls
        self.testEvidence = testEvidence
        self.verifications = verifications
        self.merges = merges
        self.maxDepth = maxDepth
        self.depthLimited = depthLimited
        self.truncated = truncated
        self.scannedEvents = scannedEvents
        self.totalEvents = totalEvents
        self.windowStartSeq = windowStartSeq
        self.windowEndSeq = windowEndSeq
        self.digest = digest
    }

    static let empty = ProvenanceSummaryView(
        nodes: 0,
        edges: 0,
        tasks: 0,
        agents: 0,
        artifacts: 0,
        commits: 0,
        toolCalls: 0,
        testEvidence: 0,
        verifications: 0,
        merges: 0,
        maxDepth: 0,
        depthLimited: false,
        truncated: false,
        scannedEvents: 0,
        totalEvents: 0,
        windowStartSeq: 0,
        windowEndSeq: 0,
        digest: ""
    )
}

extension FailureAttributionRecordView {
    init(
        schemaVersion: UInt32,
        attributionID: String,
        sessionID: String,
        target: DiagnosisTargetView?,
        status: String,
        reasonCode: String,
        earliestNodeID: String,
        earliestSeq: UInt64,
        responsibleAgents: [AttributionAgentView],
        findings: [AttributionFindingView],
        cascade: [CascadeStepView],
        graphDigest: String,
        graphNodes: UInt32,
        graphEdges: UInt32,
        truncated: Bool,
        advisory: Bool,
        createdAt: String
    ) {
        self.schemaVersion = schemaVersion
        self.attributionID = attributionID
        self.sessionID = sessionID
        self.target = target
        self.status = status
        self.reasonCode = reasonCode
        self.earliestNodeID = earliestNodeID
        self.earliestSeq = earliestSeq
        self.responsibleAgents = responsibleAgents
        self.findings = findings
        self.cascade = cascade
        self.graphDigest = graphDigest
        self.graphNodes = graphNodes
        self.graphEdges = graphEdges
        self.truncated = truncated
        self.advisory = advisory
        self.createdAt = createdAt
    }

    static let empty = FailureAttributionRecordView(
        schemaVersion: 0,
        attributionID: "",
        sessionID: "",
        target: nil,
        status: "",
        reasonCode: "",
        earliestNodeID: "",
        earliestSeq: 0,
        responsibleAgents: [],
        findings: [],
        cascade: [],
        graphDigest: "",
        graphNodes: 0,
        graphEdges: 0,
        truncated: false,
        advisory: true,
        createdAt: ""
    )
}
