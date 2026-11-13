import Foundation

// S8 portable team templates. These models mirror the bounded wire surface:
// list metadata, preview/conflict rows, import result, diff rows, export result,
// and replay report. They never carry context, memory, inbox, worktree,
// performance, MCP schema, path, URL, or secret content.

struct TeamTemplateEntry: Codable, Identifiable, Hashable, Sendable {
    let templateID: String
    let name: String
    let version: String
    let schemaVersion: Int
    let contentHash: String
    let createdAt: String
    let sourceProject: String?
    let applied: Bool
    let appliedAt: String?
    let appliedBy: String?
    let readOnly: Bool
    let origin: String

    var id: String {
        templateID
    }

    enum CodingKeys: String, CodingKey {
        case templateID = "template_id"
        case name
        case version
        case schemaVersion = "schema_version"
        case contentHash = "content_hash"
        case createdAt = "created_at"
        case sourceProject = "source_project"
        case applied
        case appliedAt = "applied_at"
        case appliedBy = "applied_by"
        case readOnly = "read_only"
        case origin
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        templateID = try container.decodeIfPresent(String.self, forKey: .templateID) ?? ""
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        version = try container.decodeIfPresent(String.self, forKey: .version) ?? ""
        schemaVersion = try container.decodeIfPresent(Int.self, forKey: .schemaVersion) ?? 1
        contentHash = try container.decodeIfPresent(String.self, forKey: .contentHash) ?? ""
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        sourceProject = try container.decodeIfPresent(String.self, forKey: .sourceProject)
        applied = try container.decodeIfPresent(Bool.self, forKey: .applied) ?? false
        appliedAt = try container.decodeIfPresent(String.self, forKey: .appliedAt)
        appliedBy = try container.decodeIfPresent(String.self, forKey: .appliedBy)
        readOnly = try container.decodeIfPresent(Bool.self, forKey: .readOnly) ?? true
        origin = try container.decodeIfPresent(String.self, forKey: .origin) ?? "local"
    }

    init(
        templateID: String,
        name: String,
        version: String,
        schemaVersion: Int,
        contentHash: String,
        createdAt: String,
        sourceProject: String?,
        applied: Bool,
        appliedAt: String?,
        appliedBy: String?,
        readOnly: Bool,
        origin: String = "local"
    ) {
        self.templateID = templateID
        self.name = name
        self.version = version
        self.schemaVersion = schemaVersion
        self.contentHash = contentHash
        self.createdAt = createdAt
        self.sourceProject = sourceProject
        self.applied = applied
        self.appliedAt = appliedAt
        self.appliedBy = appliedBy
        self.readOnly = readOnly
        self.origin = origin
    }
}

struct TeamTemplateListResult: Codable, Sendable {
    let templates: [TeamTemplateEntry]
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case templates
        case truncated
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        templates = try container.decodeIfPresent([TeamTemplateEntry].self, forKey: .templates) ?? []
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }
}

struct TeamTemplateConflict: Codable, Identifiable, Hashable, Sendable {
    let slot: String
    let code: String
    let existingIdentityID: String?
    let proposedTitle: String
    let message: String

    var id: String {
        "\(slot)|\(code)|\(existingIdentityID ?? "")"
    }

    enum CodingKeys: String, CodingKey {
        case slot
        case code
        case existingIdentityID = "existing_identity_id"
        case proposedTitle = "proposed_title"
        case message
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        slot = try container.decodeIfPresent(String.self, forKey: .slot) ?? ""
        code = try container.decodeIfPresent(String.self, forKey: .code) ?? ""
        existingIdentityID =
            try container.decodeIfPresent(String.self, forKey: .existingIdentityID)
        proposedTitle =
            try container.decodeIfPresent(String.self, forKey: .proposedTitle) ?? ""
        message = try container.decodeIfPresent(String.self, forKey: .message) ?? ""
    }
}

struct TeamTemplateSlotPreview: Codable, Identifiable, Hashable, Sendable {
    let slot: String
    let role: String
    let department: String?
    let component: String?
    let title: String
    let profile: String
    let model: String?
    let action: String
    let existingIdentityID: String?
    let pendingHireRequestID: String?
    let conflictReason: String?

    var id: String {
        slot
    }

    enum CodingKeys: String, CodingKey {
        case slot
        case role
        case department
        case component
        case title
        case profile
        case model
        case action
        case existingIdentityID = "existing_identity_id"
        case pendingHireRequestID = "pending_hire_request_id"
        case conflictReason = "conflict_reason"
    }

    init(
        slot: String,
        role: String,
        department: String?,
        component: String?,
        title: String,
        profile: String,
        model: String?,
        action: String,
        existingIdentityID: String?,
        pendingHireRequestID: String?,
        conflictReason: String?
    ) {
        self.slot = slot
        self.role = role
        self.department = department
        self.component = component
        self.title = title
        self.profile = profile
        self.model = model
        self.action = action
        self.existingIdentityID = existingIdentityID
        self.pendingHireRequestID = pendingHireRequestID
        self.conflictReason = conflictReason
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        slot = try container.decodeIfPresent(String.self, forKey: .slot) ?? ""
        role = try container.decodeIfPresent(String.self, forKey: .role) ?? ""
        department = try container.decodeIfPresent(String.self, forKey: .department)
        component = try container.decodeIfPresent(String.self, forKey: .component)
        title = try container.decodeIfPresent(String.self, forKey: .title) ?? ""
        profile = try container.decodeIfPresent(String.self, forKey: .profile) ?? ""
        model = try container.decodeIfPresent(String.self, forKey: .model)
        action = try container.decodeIfPresent(String.self, forKey: .action) ?? ""
        existingIdentityID =
            try container.decodeIfPresent(String.self, forKey: .existingIdentityID)
        pendingHireRequestID =
            try container.decodeIfPresent(String.self, forKey: .pendingHireRequestID)
        conflictReason =
            try container.decodeIfPresent(String.self, forKey: .conflictReason)
    }

    var actionLabel: String {
        switch action {
        case "existing": "Existing"
        case "create_pending_hire": "New pending hire"
        case "conflict": "Conflict"
        case "blocked": "Blocked"
        case "skipped": "Skipped"
        case "renamed": "Renamed"
        default: action.isEmpty ? "Unknown" : action
        }
    }
}

struct TeamTemplateDepartmentPreview: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let objective: String
    let manager: TeamTemplateSlotPreview
    let workers: [TeamTemplateSlotPreview]

    var id: String {
        name
    }

    enum CodingKeys: String, CodingKey {
        case name
        case objective
        case manager
        case workers
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        objective = try container.decodeIfPresent(String.self, forKey: .objective) ?? ""
        manager =
            try container.decodeIfPresent(TeamTemplateSlotPreview.self, forKey: .manager)
                ?? TeamTemplateSlotPreview.empty
        workers =
            try container.decodeIfPresent([TeamTemplateSlotPreview].self, forKey: .workers) ?? []
    }
}

struct TeamTemplatePreview: Codable, Hashable, Sendable {
    let projectID: String
    let templateID: String
    let name: String
    let version: String
    let contentHash: String
    let valid: Bool
    let applyAllowed: Bool
    let requiresApproval: Bool
    let leaderOverlay: Bool
    let leaderIdentityID: String?
    let departments: [TeamTemplateDepartmentPreview]
    let proposedOrg: [String]
    let newHireSlots: [String]
    let existingSlots: [String]
    let skippedSlots: [String]
    let blockedSlots: [String]
    let conflicts: [TeamTemplateConflict]
    let budgetChanges: [String]
    let modelGaps: [String]
    let toolGaps: [String]
    let policyConflicts: [String]
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case templateID = "template_id"
        case name
        case version
        case contentHash = "content_hash"
        case valid
        case applyAllowed = "apply_allowed"
        case requiresApproval = "requires_approval"
        case leaderOverlay = "leader_overlay"
        case leaderIdentityID = "leader_identity_id"
        case departments
        case proposedOrg = "proposed_org"
        case newHireSlots = "new_hire_slots"
        case existingSlots = "existing_slots"
        case skippedSlots = "skipped_slots"
        case blockedSlots = "blocked_slots"
        case conflicts
        case budgetChanges = "budget_changes"
        case modelGaps = "model_gaps"
        case toolGaps = "tool_gaps"
        case policyConflicts = "policy_conflicts"
        case warnings
    }

    init(
        projectID: String,
        templateID: String,
        name: String,
        version: String,
        contentHash: String,
        valid: Bool,
        applyAllowed: Bool,
        requiresApproval: Bool,
        leaderOverlay: Bool,
        leaderIdentityID: String?,
        departments: [TeamTemplateDepartmentPreview],
        proposedOrg: [String],
        newHireSlots: [String],
        existingSlots: [String],
        skippedSlots: [String],
        blockedSlots: [String],
        conflicts: [TeamTemplateConflict],
        budgetChanges: [String],
        modelGaps: [String],
        toolGaps: [String],
        policyConflicts: [String],
        warnings: [String]
    ) {
        self.projectID = projectID
        self.templateID = templateID
        self.name = name
        self.version = version
        self.contentHash = contentHash
        self.valid = valid
        self.applyAllowed = applyAllowed
        self.requiresApproval = requiresApproval
        self.leaderOverlay = leaderOverlay
        self.leaderIdentityID = leaderIdentityID
        self.departments = departments
        self.proposedOrg = proposedOrg
        self.newHireSlots = newHireSlots
        self.existingSlots = existingSlots
        self.skippedSlots = skippedSlots
        self.blockedSlots = blockedSlots
        self.conflicts = conflicts
        self.budgetChanges = budgetChanges
        self.modelGaps = modelGaps
        self.toolGaps = toolGaps
        self.policyConflicts = policyConflicts
        self.warnings = warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        templateID = try container.decodeIfPresent(String.self, forKey: .templateID) ?? ""
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        version = try container.decodeIfPresent(String.self, forKey: .version) ?? ""
        contentHash = try container.decodeIfPresent(String.self, forKey: .contentHash) ?? ""
        valid = try container.decodeIfPresent(Bool.self, forKey: .valid) ?? false
        applyAllowed = try container.decodeIfPresent(Bool.self, forKey: .applyAllowed) ?? false
        requiresApproval =
            try container.decodeIfPresent(Bool.self, forKey: .requiresApproval) ?? true
        leaderOverlay = try container.decodeIfPresent(Bool.self, forKey: .leaderOverlay) ?? false
        leaderIdentityID =
            try container.decodeIfPresent(String.self, forKey: .leaderIdentityID)
        departments =
            try container.decodeIfPresent([TeamTemplateDepartmentPreview].self, forKey: .departments)
                ?? []
        proposedOrg =
            try container.decodeIfPresent([String].self, forKey: .proposedOrg) ?? []
        newHireSlots =
            try container.decodeIfPresent([String].self, forKey: .newHireSlots) ?? []
        existingSlots =
            try container.decodeIfPresent([String].self, forKey: .existingSlots) ?? []
        skippedSlots =
            try container.decodeIfPresent([String].self, forKey: .skippedSlots) ?? []
        blockedSlots =
            try container.decodeIfPresent([String].self, forKey: .blockedSlots) ?? []
        conflicts =
            try container.decodeIfPresent([TeamTemplateConflict].self, forKey: .conflicts) ?? []
        budgetChanges =
            try container.decodeIfPresent([String].self, forKey: .budgetChanges) ?? []
        modelGaps = try container.decodeIfPresent([String].self, forKey: .modelGaps) ?? []
        toolGaps = try container.decodeIfPresent([String].self, forKey: .toolGaps) ?? []
        policyConflicts =
            try container.decodeIfPresent([String].self, forKey: .policyConflicts) ?? []
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}

struct TeamTemplateImportResult: Codable, Sendable {
    let projectID: String
    let preview: TeamTemplatePreview
    let applied: Bool
    let changed: Bool
    let entry: TeamTemplateEntry?
    let pendingHireRequestIDs: [String]
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case preview
        case applied
        case changed
        case entry
        case pendingHireRequestIDs = "pending_hire_request_ids"
        case warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        preview =
            try container.decodeIfPresent(TeamTemplatePreview.self, forKey: .preview)
                ?? TeamTemplatePreview.empty
        applied = try container.decodeIfPresent(Bool.self, forKey: .applied) ?? false
        changed = try container.decodeIfPresent(Bool.self, forKey: .changed) ?? false
        entry = try container.decodeIfPresent(TeamTemplateEntry.self, forKey: .entry)
        pendingHireRequestIDs =
            try container.decodeIfPresent([String].self, forKey: .pendingHireRequestIDs) ?? []
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}

struct TeamTemplateDiffRow: Codable, Identifiable, Hashable, Sendable {
    let kind: String
    let slot: String
    let detail: String

    var id: String {
        "\(kind)|\(slot)|\(detail)"
    }

    enum CodingKeys: String, CodingKey {
        case kind
        case slot
        case detail
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? ""
        slot = try container.decodeIfPresent(String.self, forKey: .slot) ?? ""
        detail = try container.decodeIfPresent(String.self, forKey: .detail) ?? ""
    }
}

struct TeamTemplateDiffResult: Codable, Sendable {
    let projectID: String
    let templateID: String
    let contentHash: String
    let rows: [TeamTemplateDiffRow]
    let truncated: Bool
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case templateID = "template_id"
        case contentHash = "content_hash"
        case rows
        case truncated
        case warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        templateID = try container.decodeIfPresent(String.self, forKey: .templateID) ?? ""
        contentHash = try container.decodeIfPresent(String.self, forKey: .contentHash) ?? ""
        rows = try container.decodeIfPresent([TeamTemplateDiffRow].self, forKey: .rows) ?? []
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}

struct TeamTemplateReplayReport: Codable, Sendable {
    let projectID: String
    let events: UInt64
    let templates: UInt64
    let links: UInt64
    let repairedLinks: UInt64
    let mismatches: [String]
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case events
        case templates
        case links
        case repairedLinks = "repaired_links"
        case mismatches
        case warnings
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        events = try container.decodeIfPresent(UInt64.self, forKey: .events) ?? 0
        templates = try container.decodeIfPresent(UInt64.self, forKey: .templates) ?? 0
        links = try container.decodeIfPresent(UInt64.self, forKey: .links) ?? 0
        repairedLinks = try container.decodeIfPresent(UInt64.self, forKey: .repairedLinks) ?? 0
        mismatches = try container.decodeIfPresent([String].self, forKey: .mismatches) ?? []
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}

struct TeamTemplateExportResult: Decodable, Sendable {
    let templateID: String
    let name: String
    let version: String
    let contentHash: String
    let json: String
    let markdown: String
    let warnings: [String]

    enum CodingKeys: String, CodingKey {
        case templateID = "template_id"
        case name
        case version
        case contentHash = "content_hash"
        case json
        case markdown
        case warnings
        case template
    }

    private struct TemplateIdentity: Codable {
        let templateID: String
        let name: String
        let version: String
        let contentHash: String

        enum CodingKeys: String, CodingKey {
            case templateID = "template_id"
            case name
            case version
            case contentHash = "content_hash"
        }
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let identity =
            try container.decodeIfPresent(TemplateIdentity.self, forKey: .template)
                ?? TemplateIdentity(
                    templateID: "",
                    name: "",
                    version: "",
                    contentHash: ""
                )
        templateID = identity.templateID
        name = identity.name
        version = identity.version
        contentHash = identity.contentHash
        json = try container.decodeIfPresent(String.self, forKey: .json) ?? ""
        markdown = try container.decodeIfPresent(String.self, forKey: .markdown) ?? ""
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }
}

extension TeamTemplateSlotPreview {
    static let empty = TeamTemplateSlotPreview(
        slot: "",
        role: "",
        department: nil,
        component: nil,
        title: "",
        profile: "",
        model: nil,
        action: "",
        existingIdentityID: nil,
        pendingHireRequestID: nil,
        conflictReason: nil
    )
}

extension TeamTemplatePreview {
    static let empty = TeamTemplatePreview(
        projectID: "",
        templateID: "",
        name: "",
        version: "",
        contentHash: "",
        valid: false,
        applyAllowed: false,
        requiresApproval: true,
        leaderOverlay: false,
        leaderIdentityID: nil,
        departments: [],
        proposedOrg: [],
        newHireSlots: [],
        existingSlots: [],
        skippedSlots: [],
        blockedSlots: [],
        conflicts: [],
        budgetChanges: [],
        modelGaps: [],
        toolGaps: [],
        policyConflicts: [],
        warnings: []
    )
}

/// One explicit operator conflict resolution sent to the daemon. The client
/// never synthesizes an apply; `approved` is a separate operator decision.
struct TeamTemplateResolutionRequest: Codable, Hashable, Sendable {
    let slot: String
    let action: String
    let identityID: String?
    let title: String?

    enum CodingKeys: String, CodingKey {
        case slot
        case action
        case identityID = "identity_id"
        case title
    }

    init(slot: String, action: String, identityID: String? = nil, title: String? = nil) {
        self.slot = slot
        self.action = action
        self.identityID = identityID
        self.title = title
    }
}

enum TeamTemplateSheet: String, Identifiable {
    case importTemplate
    case diff

    var id: String {
        rawValue
    }
}
