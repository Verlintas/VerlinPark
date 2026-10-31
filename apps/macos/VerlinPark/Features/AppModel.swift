import AppKit
import Observation

struct ArtifactPreviewData {
    let path: String
    let image: NSImage?
    let text: String?
    let hex: String?
    let truncated: Bool
}

@MainActor
@Observable
final class AppModel {
    enum ConnectionState: Equatable {
        case idle
        case connecting
        case connected
        case failed(String)
    }

    enum WorkspaceMode: Equatable {
        case resident
        case project
    }

    var connection: ConnectionState = .idle
    var mode: WorkspaceMode = .resident
    var projects: [Project] = []
    var sessions: [Session] = []
    var events: [Event] = []
    var selectedProjectID: String?
    var selectedSessionID: String?
    var isBusy = false
    var lastError: String?
    var agentInfo: AgentInfo?
    var streamingText = ""
    var searchQuery = ""
    var searchHits: [SearchHit] = []
    var searchTruncated = false
    var searchBusy = false
    var todos: [TodoItem] = []
    var agents: [AgentRecord] = []
    var mergeQueue: [MergeQueueEntry] = []
    var mailbox: [AgentMessage] = []
    var artifacts: [Artifact] = []
    var usageLedger: UsageLedgerResult?
    var usageStatus: String?
    var shellPolicy = ShellPolicy.allow
    var shellPolicyNetwork = "allow"
    var shellPolicyReadDenyText = ""
    var shellPolicyBusy = false
    var shellPolicyStatus: String?
    var integrationPolicy = IntegrationPolicy.disabled
    var integrationAutoResolve = false
    var integrationMaxAutoAttempts = 1
    var integrationMaxTokensText = ""
    var integrationPolicyBusy = false
    var integrationPolicyStatus: String?
    var artifactPolicy = ArtifactPolicy.disabled
    var artifactAutoGc = false
    var artifactMaxBytesText = ""
    var artifactMaxAgeText = ""
    var artifactMaxCountText = ""
    var artifactPolicyBusy = false
    var artifactPolicyStatus: String?
    var projectBudget = ProjectBudget.empty
    var projectSpentTokens = 0
    var projectSpentCostUsd = 0.0
    var projectTokenBudgetText = ""
    var projectCostBudgetText = ""
    var projectBudgetBusy = false
    var projectBudgetStatus: String?
    var mailboxPolicy = MailboxPolicy.disabled
    var mailboxMaxAgeText = ""
    var mailboxMaxCountText = ""
    var mailboxDeleteUnread = false
    var mailboxAutoGc = false
    var mailboxPolicyBusy = false
    var mailboxPolicyStatus: String?
    var teamPlan: TeamPlan?
    var planTaskStatuses: [String: String] = [:]
    var sessionDiff: SessionDiff?
    var diffLoading = false
    var diffError: String?
    var lastCommit: SessionCommit?
    var commitBusy = false
    var landingBusy = false
    var lastLanding: SessionLandResult?
    var pullRequestBusy = false
    var lastPullRequest: PullRequestResult?
    var providerKind =
        UserDefaults.standard.string(forKey: "provider.kind") ?? "openai-compatible"
    var providerBaseURL =
        UserDefaults.standard.string(forKey: "provider.baseURL")
            ?? "https://api.openai.com/v1"
    var providerModel = UserDefaults.standard.string(forKey: "provider.model") ?? ""
    var roleModels = AppModel.loadRoleModels()
    var providerAPIKey = Keychain.read(account: "provider.apiKey") ?? ""
    var providerStatus: String?
    var providerBusy = false
    var providerProbeBusy = false
    var providerProbeStatus: String?
    var cleanupOlderDaysText = "7"
    var cleanupDryRun = true
    var cleanupDeleteBranches = false
    var cleanupBusy = false
    var cleanupStatus: String?
    var cleanupResult: WorktreeCleanupResult?
    var doctorReport: DoctorReport?
    var doctorBusy = false
    var doctorStatus: String?

    private static func loadRoleModels() -> RoleModels {
        guard let data = UserDefaults.standard.data(forKey: "provider.roleModels"),
              let models = try? JSONDecoder().decode(RoleModels.self, from: data)
        else {
            return RoleModels()
        }
        return models
    }

    private let core = CoreClient()
    private let daemon = DaemonController()
    private var eventTask: Task<Void, Never>?
    private var started = false

    var selectedProject: Project? {
        projects.first { $0.id == selectedProjectID }
    }

    var selectedSession: Session? {
        sessions.first { $0.id == selectedSessionID }
    }

    func start() async {
        guard !started else { return }
        started = true
        connection = .connecting
        do {
            let discovery = try await daemon.ensureRunning()
            try await core.connect(to: discovery)
            connection = .connected
            await configureStoredProvider()
            agentInfo = try? await core.agentInfo()
            if let info = agentInfo {
                roleModels = info.roleModels
            }
            consumeEvents()
            await refresh()
        } catch {
            connection = .failed(error.localizedDescription)
            lastError = error.localizedDescription
        }
    }

    func refresh() async {
        guard connection == .connected else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            projects = try await core.listProjects()
            if selectedProjectID == nil ||
                !projects.contains(where: { $0.id == selectedProjectID })
            {
                selectedProjectID = projects.first?.id
            }
            await loadSessions()
            await loadShellPolicy()
            await loadIntegrationPolicy()
            await loadArtifactPolicy()
            await loadMailboxPolicy()
            await loadProjectBudget()
        } catch {
            handle(error)
        }
    }

    func selectProject(_ id: String?) {
        guard let id else {
            selectedProjectID = nil
            selectedSessionID = nil
            events = []
            sessionDiff = nil
            todos = []
            agents = []
            mergeQueue = []
            mailbox = []
            artifacts = []
            usageLedger = nil
            usageStatus = nil
            shellPolicy = .allow
            shellPolicyNetwork = "allow"
            shellPolicyReadDenyText = ""
            shellPolicyStatus = nil
            integrationPolicy = .disabled
            integrationAutoResolve = false
            integrationMaxAutoAttempts = 1
            integrationMaxTokensText = ""
            integrationPolicyStatus = nil
            artifactPolicy = .disabled
            artifactAutoGc = false
            artifactMaxBytesText = ""
            artifactMaxAgeText = ""
            artifactMaxCountText = ""
            artifactPolicyStatus = nil
            mailboxPolicy = .disabled
            mailboxMaxAgeText = ""
            mailboxMaxCountText = ""
            mailboxDeleteUnread = false
            mailboxAutoGc = false
            mailboxPolicyStatus = nil
            projectBudget = .empty
            projectSpentTokens = 0
            projectSpentCostUsd = 0
            projectTokenBudgetText = ""
            projectCostBudgetText = ""
            projectBudgetStatus = nil
            teamPlan = nil
            planTaskStatuses = [:]
            lastCommit = nil
            lastLanding = nil
            lastPullRequest = nil
            mode = .resident
            return
        }
        mode = .project
        guard selectedProjectID != id else { return }
        selectedProjectID = id
        selectedSessionID = nil
        events = []
        sessionDiff = nil
        todos = []
        agents = []
        mergeQueue = []
        mailbox = []
        artifacts = []
        usageLedger = nil
        usageStatus = nil
        Task { await loadShellPolicy() }
        Task { await loadIntegrationPolicy() }
        Task { await loadArtifactPolicy() }
        Task { await loadMailboxPolicy() }
        Task { await loadProjectBudget() }
        teamPlan = nil
        planTaskStatuses = [:]
        lastCommit = nil
        lastLanding = nil
        lastPullRequest = nil
        Task { await loadSessions() }
    }

    func selectResident() {
        mode = .resident
    }

    func selectSession(_ id: String?) {
        guard selectedSessionID != id else { return }
        mode = .project
        selectedSessionID = id
        streamingText = ""
        sessionDiff = nil
        todos = []
        agents = []
        mergeQueue = []
        teamPlan = nil
        planTaskStatuses = [:]
        lastCommit = nil
        lastLanding = nil
        lastPullRequest = nil
        Task {
            await loadEvents()
            await loadTodos()
            await loadAgents()
            await loadMergeQueue()
            await loadMailbox()
            await loadTeamPlan()
            await loadDiff()
        }
    }

    func exportTranscript() async {
        guard let session = selectedSession else { return }
        do {
            let result = try await core.transcript(sessionID: session.id)
            let panel = NSSavePanel()
            panel.canCreateDirectories = true
            let sanitized = session.goal
                .replacingOccurrences(of: "/", with: "-")
                .replacingOccurrences(of: ":", with: "-")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let stem = String(sanitized.prefix(60))
            panel.nameFieldStringValue = (stem.isEmpty ? session.id : stem) + ".md"
            guard panel.runModal() == .OK, let url = panel.url else { return }
            try result.markdown.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            handle(error)
        }
    }

    func runSearch(projectID: String?) async {
        let trimmed = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            searchHits = []
            searchTruncated = false
            return
        }
        searchBusy = true
        defer { searchBusy = false }
        do {
            let result = try await core.search(query: trimmed, projectID: projectID)
            guard trimmed == searchQuery.trimmingCharacters(in: .whitespacesAndNewlines) else {
                return
            }
            searchHits = result.hits
            searchTruncated = result.truncated
        } catch {
            searchHits = []
            searchTruncated = false
            handle(error)
        }
    }

    func openSearchHit(_ hit: SearchHit) async {
        if selectedProjectID != hit.projectId {
            selectProject(hit.projectId)
        }
        do {
            let loaded = try await core.listSessions(projectID: hit.projectId)
            if selectedProjectID == hit.projectId {
                sessions = loaded
            }
        } catch {
            handle(error)
            return
        }
        guard sessions.contains(where: { $0.id == hit.sessionId }) else {
            lastError = "That session is no longer available."
            return
        }
        if selectedSessionID != hit.sessionId {
            selectedSessionID = nil
            selectSession(hit.sessionId)
        }
    }

    func addProject() async {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Add Project"
        panel.message = "Choose a Git repository or project folder."
        if panel.runModal() == .OK, let url = panel.url {
            await addProject(at: url)
        }
    }

    func addProject(at url: URL) async {
        guard connection == .connected else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            let project = try await core.addProject(root: url.path, name: nil)
            projects = try await core.listProjects()
            selectedProjectID = project.id
            selectedSessionID = nil
            events = []
            mode = .project
            await loadSessions()
        } catch {
            handle(error)
        }
    }

    var defaultTokenBudget: Int {
        UserDefaults.standard.object(forKey: "verlinpark.session.tokenBudget") as? Int
            ?? 200_000
    }

    func createSession(
        goal: String,
        tokenBudget: Int? = nil,
        costBudgetUSD: Double? = nil,
        timeBudgetSecs: Int? = nil
    ) async {
        guard let projectID = selectedProjectID else {
            lastError = "Choose a project first."
            return
        }
        let trimmed = goal.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let budget = (tokenBudget ?? defaultTokenBudget).clampedTokenBudget
        UserDefaults.standard.set(budget, forKey: "verlinpark.session.tokenBudget")

        isBusy = true
        defer { isBusy = false }
        do {
            let session = try await core.createSession(
                projectID: projectID,
                goal: trimmed,
                tokenBudget: budget,
                costBudgetUSD: costBudgetUSD,
                timeBudgetSecs: timeBudgetSecs
            )
            sessions = try await core.listSessions(projectID: projectID)
            selectedSessionID = session.id
            mode = .project
            events = try await core.listEvents(sessionID: session.id)
            refreshPlanTaskStatuses()
            sessionDiff = try? await core.sessionDiff(sessionID: session.id)
            todos = await (try? core.sessionTodos(sessionID: session.id)) ?? []
            mergeQueue = await (try? core.listMergeQueue(sessionID: session.id)) ?? []
            lastCommit = nil
        } catch {
            handle(error)
        }
    }

    func sendMessage(_ text: String) async {
        guard let sessionID = selectedSessionID else {
            lastError = "Choose or create a session first."
            return
        }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        do {
            try await core.prompt(sessionID: sessionID, content: trimmed)
        } catch {
            handle(error)
        }
    }

    func cancelSession() async {
        guard let sessionID = selectedSessionID else { return }
        do {
            _ = try await core.cancelSession(sessionID: sessionID)
        } catch {
            handle(error)
        }
    }

    func pauseSession() async {
        guard let sessionID = selectedSessionID else { return }
        do {
            _ = try await core.pauseSession(sessionID: sessionID)
        } catch {
            handle(error)
        }
    }

    func resumeSession() async {
        guard let sessionID = selectedSessionID else { return }
        do {
            _ = try await core.resumeSession(sessionID: sessionID)
        } catch {
            handle(error)
        }
    }

    func loadShellPolicy() async {
        guard let projectID = selectedProjectID else {
            shellPolicy = .allow
            shellPolicyNetwork = "allow"
            shellPolicyReadDenyText = ""
            shellPolicyStatus = nil
            return
        }
        shellPolicyBusy = true
        defer { shellPolicyBusy = false }
        do {
            let policy = try await core.shellPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            shellPolicy = policy
            shellPolicyNetwork = policy.network
            shellPolicyReadDenyText = policy.readDeny.joined(separator: "\n")
            shellPolicyStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveShellPolicy() async {
        guard let projectID = selectedProjectID else {
            shellPolicyStatus = "Select a project first."
            return
        }
        let readDeny = shellPolicyReadDenyText
            .split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        shellPolicyBusy = true
        defer { shellPolicyBusy = false }
        do {
            let policy = try await core.setShellPolicy(
                projectID: projectID,
                policy: ShellPolicy(network: shellPolicyNetwork, readDeny: readDeny)
            )
            guard projectID == selectedProjectID else { return }
            shellPolicy = policy
            shellPolicyNetwork = policy.network
            shellPolicyReadDenyText = policy.readDeny.joined(separator: "\n")
            shellPolicyStatus =
                "Saved · network \(policy.network) · \(policy.readDeny.count) read-deny paths"
        } catch {
            shellPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetShellPolicy() async {
        shellPolicyNetwork = "allow"
        shellPolicyReadDenyText = ""
        await saveShellPolicy()
    }

    func loadIntegrationPolicy() async {
        guard let projectID = selectedProjectID else {
            integrationPolicy = .disabled
            integrationAutoResolve = false
            integrationMaxAutoAttempts = 1
            integrationMaxTokensText = ""
            integrationPolicyStatus = nil
            return
        }
        integrationPolicyBusy = true
        defer { integrationPolicyBusy = false }
        do {
            let policy = try await core.integrationPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            integrationPolicy = policy
            integrationAutoResolve = policy.autoResolve
            integrationMaxAutoAttempts = policy.maxAutoAttempts
            integrationMaxTokensText = policy.maxResolveTokens.map(String.init) ?? ""
            integrationPolicyStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveIntegrationPolicy() async {
        guard let projectID = selectedProjectID else {
            integrationPolicyStatus = "Select a project first."
            return
        }
        let rawTokens = integrationMaxTokensText.trimmingCharacters(in: .whitespaces)
        let tokens: Int?
        if rawTokens.isEmpty {
            tokens = nil
        } else if let value = Int(rawTokens), value > 0 {
            tokens = value
        } else {
            integrationPolicyStatus = "Max tokens must be empty or a positive integer."
            return
        }
        integrationPolicyBusy = true
        defer { integrationPolicyBusy = false }
        do {
            let policy = try await core.setIntegrationPolicy(
                projectID: projectID,
                policy: IntegrationPolicy(
                    autoResolve: integrationAutoResolve,
                    maxAutoAttempts: integrationMaxAutoAttempts,
                    maxResolveTokens: tokens
                )
            )
            guard projectID == selectedProjectID else { return }
            integrationPolicy = policy
            integrationAutoResolve = policy.autoResolve
            integrationMaxAutoAttempts = policy.maxAutoAttempts
            integrationMaxTokensText = policy.maxResolveTokens.map(String.init) ?? ""
            integrationPolicyStatus = policy.autoResolve
                ? "Auto-resolve enabled · attempts \(policy.maxAutoAttempts)"
                : "Auto-resolve disabled"
        } catch {
            integrationPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadProjectBudget() async {
        guard let projectID = selectedProjectID else {
            projectBudget = .empty
            projectSpentTokens = 0
            projectSpentCostUsd = 0
            projectTokenBudgetText = ""
            projectCostBudgetText = ""
            projectBudgetStatus = nil
            return
        }
        projectBudgetBusy = true
        defer { projectBudgetBusy = false }
        do {
            let result = try await core.projectBudget(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            projectBudget = result.budget
            projectSpentTokens = result.spentTokens
            projectSpentCostUsd = result.spentCostUsd
            projectTokenBudgetText = result.budget.tokenBudget.map { String($0) } ?? ""
            projectCostBudgetText = result.budget.costBudgetUsd.map { String($0) } ?? ""
            projectBudgetStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveProjectBudget() async {
        guard let projectID = selectedProjectID else {
            projectBudgetStatus = "Select a project first."
            return
        }
        func optionalInt(_ text: String) -> Int? {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? nil : Int(trimmed)
        }
        func optionalDouble(_ text: String) -> Double? {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? nil : Double(trimmed)
        }
        let tokens = optionalInt(projectTokenBudgetText)
        let cost = optionalDouble(projectCostBudgetText)
        if (!projectTokenBudgetText.trimmingCharacters(in: .whitespaces).isEmpty && tokens == nil)
            || (!projectCostBudgetText.trimmingCharacters(in: .whitespaces).isEmpty && cost == nil)
        {
            projectBudgetStatus = "Budgets must be empty or positive numbers."
            return
        }
        projectBudgetBusy = true
        defer { projectBudgetBusy = false }
        do {
            let result = try await core.setProjectBudget(
                projectID: projectID,
                budget: ProjectBudget(tokenBudget: tokens, costBudgetUsd: cost)
            )
            guard projectID == selectedProjectID else { return }
            projectBudget = result.budget
            projectSpentTokens = result.spentTokens
            projectSpentCostUsd = result.spentCostUsd
            projectBudgetStatus = "Project budget saved"
        } catch {
            projectBudgetStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetProjectBudget() async {
        projectTokenBudgetText = ""
        projectCostBudgetText = ""
        await saveProjectBudget()
    }

    func loadMailboxPolicy() async {
        guard let projectID = selectedProjectID else {
            mailboxPolicy = .disabled
            mailboxMaxAgeText = ""
            mailboxMaxCountText = ""
            mailboxDeleteUnread = false
            mailboxAutoGc = false
            mailboxPolicyStatus = nil
            projectBudget = .empty
            projectSpentTokens = 0
            projectSpentCostUsd = 0
            projectTokenBudgetText = ""
            projectCostBudgetText = ""
            projectBudgetStatus = nil
            return
        }
        mailboxPolicyBusy = true
        defer { mailboxPolicyBusy = false }
        do {
            let policy = try await core.mailboxPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            mailboxPolicy = policy
            mailboxMaxAgeText = policy.maxAgeSecs.map(String.init) ?? ""
            mailboxMaxCountText = policy.maxCount.map(String.init) ?? ""
            mailboxDeleteUnread = policy.deleteUnread
            mailboxAutoGc = policy.autoGc
            mailboxPolicyStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveMailboxPolicy() async {
        guard let projectID = selectedProjectID else {
            mailboxPolicyStatus = "Select a project first."
            return
        }
        func optionalInt(_ text: String) -> Int? {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? nil : Int(trimmed)
        }
        let maxAge = optionalInt(mailboxMaxAgeText)
        let maxCount = optionalInt(mailboxMaxCountText)
        if (!mailboxMaxAgeText.trimmingCharacters(in: .whitespaces).isEmpty && maxAge == nil)
            || (!mailboxMaxCountText.trimmingCharacters(in: .whitespaces).isEmpty && maxCount == nil)
        {
            mailboxPolicyStatus = "Limits must be empty or positive integers."
            return
        }
        mailboxPolicyBusy = true
        defer { mailboxPolicyBusy = false }
        do {
            let policy = try await core.setMailboxPolicy(
                projectID: projectID,
                policy: MailboxPolicy(
                    maxAgeSecs: maxAge,
                    maxCount: maxCount,
                    deleteUnread: mailboxDeleteUnread,
                    autoGc: mailboxAutoGc
                )
            )
            guard projectID == selectedProjectID else { return }
            mailboxPolicy = policy
            mailboxPolicyStatus = "Mailbox policy saved"
        } catch {
            mailboxPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetMailboxPolicy() async {
        mailboxMaxAgeText = ""
        mailboxMaxCountText = ""
        mailboxDeleteUnread = false
        mailboxAutoGc = false
        await saveMailboxPolicy()
    }

    func gcMailbox() async {
        guard let projectID = selectedProjectID else { return }
        mailboxPolicyBusy = true
        defer { mailboxPolicyBusy = false }
        do {
            let result = try await core.gcMailbox(projectID: projectID)
            mailboxPolicyStatus = "Removed \(result.removed) message(s)"
            await loadMailbox()
        } catch {
            mailboxPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func loadArtifactPolicy() async {
        guard let projectID = selectedProjectID else {
            artifactPolicy = .disabled
            artifactAutoGc = false
            artifactMaxBytesText = ""
            artifactMaxAgeText = ""
            artifactMaxCountText = ""
            artifactPolicyStatus = nil
            mailboxPolicy = .disabled
            mailboxMaxAgeText = ""
            mailboxMaxCountText = ""
            mailboxDeleteUnread = false
            mailboxAutoGc = false
            mailboxPolicyStatus = nil
            projectBudget = .empty
            projectSpentTokens = 0
            projectSpentCostUsd = 0
            projectTokenBudgetText = ""
            projectCostBudgetText = ""
            projectBudgetStatus = nil
            return
        }
        artifactPolicyBusy = true
        defer { artifactPolicyBusy = false }
        do {
            let policy = try await core.artifactPolicy(projectID: projectID)
            guard projectID == selectedProjectID else { return }
            artifactPolicy = policy
            artifactAutoGc = policy.autoGc
            artifactMaxBytesText = policy.maxBytes.map(String.init) ?? ""
            artifactMaxAgeText = policy.maxAgeSecs.map(String.init) ?? ""
            artifactMaxCountText = policy.maxCount.map(String.init) ?? ""
            artifactPolicyStatus = nil
        } catch {
            handle(error)
        }
    }

    func saveArtifactPolicy() async {
        guard let projectID = selectedProjectID else {
            artifactPolicyStatus = "Select a project first."
            return
        }
        func optionalInt(_ text: String) -> Int? {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? nil : Int(trimmed)
        }
        let maxBytes = optionalInt(artifactMaxBytesText)
        let maxAge = optionalInt(artifactMaxAgeText)
        let maxCount = optionalInt(artifactMaxCountText)
        if (!artifactMaxBytesText.trimmingCharacters(in: .whitespaces).isEmpty && maxBytes == nil)
            || (!artifactMaxAgeText.trimmingCharacters(in: .whitespaces).isEmpty && maxAge == nil)
            || (!artifactMaxCountText.trimmingCharacters(in: .whitespaces).isEmpty && maxCount == nil)
        {
            artifactPolicyStatus = "Limits must be empty or positive integers."
            return
        }
        artifactPolicyBusy = true
        defer { artifactPolicyBusy = false }
        do {
            let policy = try await core.setArtifactPolicy(
                projectID: projectID,
                policy: ArtifactPolicy(
                    autoGc: artifactAutoGc,
                    maxBytes: maxBytes,
                    maxAgeSecs: maxAge,
                    maxCount: maxCount
                )
            )
            guard projectID == selectedProjectID else { return }
            artifactPolicy = policy
            artifactPolicyStatus = policy.autoGc ? "Automatic GC enabled" : "Artifact policy saved"
        } catch {
            artifactPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetArtifactPolicy() async {
        artifactAutoGc = false
        artifactMaxBytesText = ""
        artifactMaxAgeText = ""
        artifactMaxCountText = ""
        await saveArtifactPolicy()
    }

    func gcArtifacts() async {
        guard let projectID = selectedProjectID else { return }
        artifactPolicyBusy = true
        defer { artifactPolicyBusy = false }
        do {
            let result = try await core.gcArtifacts(projectID: projectID)
            artifactPolicyStatus =
                "Removed \(result.removed) artifact(s), freed \(result.bytesFreed) bytes"
            await loadMailbox()
        } catch {
            artifactPolicyStatus = error.localizedDescription
            handle(error)
        }
    }

    func resetIntegrationPolicy() async {
        integrationAutoResolve = false
        integrationMaxAutoAttempts = 1
        integrationMaxTokensText = ""
        await saveIntegrationPolicy()
    }

    func resolveApproval(_ id: String, approved: Bool) async {
        do {
            try await core.resolveApproval(id: id, approved: approved)
        } catch {
            handle(error)
        }
    }

    private var orgCaps: (managers: Int, workers: Int, labour: Int) {
        let defaults = UserDefaults.standard
        return (
            defaults.object(forKey: "verlinpark.org.managers") as? Int ?? 4,
            defaults.object(forKey: "verlinpark.org.workers") as? Int ?? 6,
            defaults.object(forKey: "verlinpark.org.labour") as? Int ?? 12
        )
    }

    func saveProviderSettings() async {
        let baseURL = providerBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        let model = providerModel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !model.isEmpty else {
            providerStatus = "Enter a model name."
            return
        }
        guard !baseURL.isEmpty else {
            providerStatus = "Enter a base URL."
            return
        }
        providerBusy = true
        defer { providerBusy = false }
        do {
            let caps = orgCaps
            let info = try await core.configureAgent(
                kind: providerKind,
                baseURL: baseURL,
                model: model,
                roleModels: roleModels,
                apiKey: providerAPIKey,
                maxManagers: caps.managers,
                maxWorkers: caps.workers,
                maxLabour: caps.labour
            )
            agentInfo = info
            roleModels = info.roleModels
            UserDefaults.standard.set(providerKind, forKey: "provider.kind")
            UserDefaults.standard.set(baseURL, forKey: "provider.baseURL")
            UserDefaults.standard.set(model, forKey: "provider.model")
            if let data = try? JSONEncoder().encode(roleModels) {
                UserDefaults.standard.set(data, forKey: "provider.roleModels")
            }
            if providerAPIKey.isEmpty {
                Keychain.delete(account: "provider.apiKey")
            } else {
                try Keychain.save(providerAPIKey, account: "provider.apiKey")
            }
            providerStatus = "Connected · \(model)"
        } catch {
            providerStatus = error.localizedDescription
        }
    }

    private func configureStoredProvider() async {
        let baseURL = providerBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        let model = providerModel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !baseURL.isEmpty, !model.isEmpty else { return }
        do {
            let caps = orgCaps
            _ = try await core.configureAgent(
                kind: providerKind,
                baseURL: baseURL,
                model: model,
                roleModels: roleModels,
                apiKey: providerAPIKey,
                maxManagers: caps.managers,
                maxWorkers: caps.workers,
                maxLabour: caps.labour
            )
        } catch {
            providerStatus = error.localizedDescription
        }
    }

    func applyOrgSettings() async {
        await configureStoredProvider()
    }

    func commitSession(message: String) async {
        guard let sessionID = selectedSessionID else { return }
        let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            lastError = "Enter a commit message."
            return
        }
        commitBusy = true
        defer { commitBusy = false }
        do {
            lastCommit = try await core.commitSession(sessionID: sessionID, message: trimmed)
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    func discardChanges() async {
        guard let sessionID = selectedSessionID else { return }
        commitBusy = true
        defer { commitBusy = false }
        do {
            try await core.discardSession(sessionID: sessionID)
            lastCommit = nil
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    func loadTodos() async {
        guard let sessionID = selectedSessionID else {
            todos = []
            return
        }
        do {
            let items = try await core.sessionTodos(sessionID: sessionID)
            if sessionID == selectedSessionID {
                todos = items
            }
        } catch {
            // The event stream will retry on the next todo.updated.
        }
    }

    func loadTeamPlan() async {
        guard let sessionID = selectedSessionID else {
            teamPlan = nil
            return
        }
        do {
            let plan = try await core.getTeamPlan(sessionID: sessionID)
            if sessionID == selectedSessionID {
                teamPlan = plan
            }
        } catch {
            // The event stream will retry on team_plan.submitted.
        }
    }

    func artifactPreviewData(_ artifact: Artifact) async -> ArtifactPreviewData? {
        do {
            let materialized = try await core.materializeArtifact(artifactID: artifact.id)
            let data = try await Task.detached(priority: .userInitiated) {
                try Data(
                    contentsOf: URL(fileURLWithPath: materialized.path),
                    options: [.mappedIfSafe]
                )
            }.value
            let mime = artifact.mimeType.lowercased()
            if mime.hasPrefix("image/"), let image = NSImage(data: data) {
                return ArtifactPreviewData(
                    path: materialized.path,
                    image: image,
                    text: nil,
                    hex: nil,
                    truncated: false
                )
            }
            let limit = 64 * 1024
            let truncated = data.count > limit
            let slice = data.prefix(limit)
            if isTextMime(mime) {
                return ArtifactPreviewData(
                    path: materialized.path,
                    image: nil,
                    text: String(decoding: slice, as: UTF8.self),
                    hex: nil,
                    truncated: truncated
                )
            }
            return ArtifactPreviewData(
                path: materialized.path,
                image: nil,
                text: nil,
                hex: hexDump(Data(slice.prefix(1024))),
                truncated: truncated
            )
        } catch {
            return nil
        }
    }

    private func isTextMime(_ mime: String) -> Bool {
        mime.hasPrefix("text/")
            || mime.contains("json")
            || mime.contains("xml")
            || mime.contains("yaml")
            || mime.contains("javascript")
            || mime.contains("toml")
            || mime.contains("csv")
            || mime.contains("svg")
            || mime.contains("sql")
            || mime.contains("shellscript")
    }

    private func hexDump(_ data: Data) -> String {
        let bytes = [UInt8](data)
        return stride(from: 0, to: bytes.count, by: 16).map { offset in
            let end = min(offset + 16, bytes.count)
            let chunk = bytes[offset ..< end]
            let hex = chunk.map { String(format: "%02x", $0) }.joined(separator: " ")
            let ascii = chunk.map { byte in
                (32 ... 126).contains(byte) ? String(UnicodeScalar(byte)) : "."
            }.joined()
            return String(format: "%08x  ", offset)
                + hex.padding(toLength: 47, withPad: " ", startingAt: 0)
                + "  " + ascii
        }.joined(separator: "\n")
    }

    func openArtifact(_ artifact: Artifact) async {
        do {
            let materialized = try await core.materializeArtifact(artifactID: artifact.id)
            NSWorkspace.shared.open(URL(fileURLWithPath: materialized.path))
        } catch {
            handle(error)
        }
    }

    func runWorktreeCleanup() async {
        guard let projectID = selectedProjectID else {
            cleanupStatus = "Select a project first."
            return
        }
        let days = Int(cleanupOlderDaysText.trimmingCharacters(in: .whitespaces)) ?? 7
        cleanupBusy = true
        defer { cleanupBusy = false }
        do {
            let result = try await core.cleanupWorktrees(
                projectID: projectID,
                olderThanDays: max(0, days),
                dryRun: cleanupDryRun,
                force: false,
                deleteBranches: cleanupDeleteBranches
            )
            cleanupResult = result
            cleanupStatus =
                "scanned \(result.scanned), removed \(result.removed), skipped \(result.skipped)"
        } catch {
            cleanupStatus = error.localizedDescription
            handle(error)
        }
    }

    func runDoctor() async {
        doctorBusy = true
        defer { doctorBusy = false }
        do {
            doctorReport = try await core.doctor()
            doctorStatus = doctorReport?.ok == true ? "All checks passed" : "Warnings or errors found"
        } catch {
            doctorStatus = error.localizedDescription
            handle(error)
        }
    }

    func probeProvider() async {
        providerProbeBusy = true
        defer { providerProbeBusy = false }
        do {
            let result = try await core.probeProvider()
            providerProbeStatus =
                "\(result.provider) / \(result.model) · \(result.latencyMs) ms · \(result.text)"
        } catch {
            providerProbeStatus = error.localizedDescription
            handle(error)
        }
    }

    func refreshAgentInfo() async {
        do {
            agentInfo = try await core.agentInfo()
        } catch {
            handle(error)
        }
    }

    func loadAgents() async {
        guard let sessionID = selectedSessionID else {
            agents = []
            return
        }
        do {
            let records = try await core.listAgents(sessionID: sessionID)
            if sessionID == selectedSessionID {
                agents = records
            }
        } catch {
            // The event stream will retry on the next agent event.
        }
    }

    func loadMailbox() async {
        guard let sessionID = selectedSessionID else {
            mailbox = []
            artifacts = []
            usageLedger = nil
            usageStatus = nil
            return
        }
        do {
            let messages = try await core.listMailbox(sessionID: sessionID)
            let published = try await core.listArtifacts(sessionID: sessionID)
            guard sessionID == selectedSessionID else { return }
            mailbox = messages
            artifacts = published
        } catch {
            // The event stream will retry on mailbox/artifact events.
        }
    }

    func loadUsage() async {
        guard let sessionID = selectedSessionID else {
            usageLedger = nil
            usageStatus = nil
            return
        }
        do {
            let ledger = try await core.usageLedger(sessionID: sessionID)
            guard sessionID == selectedSessionID else { return }
            usageLedger = ledger
            usageStatus = nil
        } catch {
            if sessionID == selectedSessionID {
                usageStatus = error.localizedDescription
            }
        }
    }

    func markMessageRead(_ messageID: String) async {
        do {
            _ = try await core.markMessageRead(messageID: messageID)
            await loadMailbox()
        } catch {
            handle(error)
        }
    }

    func loadMergeQueue() async {
        guard let sessionID = selectedSessionID else {
            mergeQueue = []
            return
        }
        do {
            mergeQueue = try await core.listMergeQueue(sessionID: sessionID)
        } catch {
            // The event stream will retry on the next merge queue event.
        }
    }

    func retryMergeEntry(_ entryID: String) async {
        do {
            _ = try await core.retryMerge(entryID: entryID)
            await loadMergeQueue()
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    func resolveMergeEntry(_ entryID: String) async {
        do {
            _ = try await core.resolveMerge(entryID: entryID)
            await loadMergeQueue()
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    func landSession() async {
        guard let sessionID = selectedSessionID else { return }
        landingBusy = true
        defer { landingBusy = false }
        do {
            lastLanding = try await core.landSession(sessionID: sessionID)
            await loadSessions()
            await loadDiff()
            await loadMergeQueue()
        } catch {
            handle(error)
        }
    }

    func openPullRequest() async {
        guard let sessionID = selectedSessionID else { return }
        pullRequestBusy = true
        defer { pullRequestBusy = false }
        do {
            lastPullRequest = try await core.openPullRequest(sessionID: sessionID)
            await loadSessions()
        } catch {
            handle(error)
        }
    }

    func loadDiff() async {
        guard let sessionID = selectedSessionID else {
            sessionDiff = nil
            return
        }
        diffLoading = true
        defer { diffLoading = false }
        do {
            let diff = try await core.sessionDiff(sessionID: sessionID)
            if sessionID == selectedSessionID {
                sessionDiff = diff
                diffError = nil
            }
        } catch {
            if sessionID == selectedSessionID {
                diffError = error.localizedDescription
            }
        }
    }

    func clearError() {
        lastError = nil
    }

    private func loadSessions() async {
        guard let projectID = selectedProjectID else {
            sessions = []
            selectedSessionID = nil
            events = []
            return
        }
        do {
            sessions = try await core.listSessions(projectID: projectID)
            if let selectedSessionID,
               !sessions.contains(where: { $0.id == selectedSessionID })
            {
                self.selectedSessionID = nil
            }
            if selectedSessionID == nil {
                selectedSessionID = sessions.first?.id
            }
            await loadEvents()
            await loadTodos()
            await loadAgents()
            await loadMergeQueue()
            await loadTeamPlan()
            await loadDiff()
        } catch {
            handle(error)
        }
    }

    private func loadEvents() async {
        guard let sessionID = selectedSessionID else {
            events = []
            return
        }
        do {
            events = try await core.listEvents(sessionID: sessionID)
            refreshPlanTaskStatuses()
        } catch {
            handle(error)
        }
    }

    private func refreshPlanTaskStatuses() {
        var statuses: [String: String] = [:]
        for event in events {
            guard event.type == "plan_task.started" || event.type == "plan_task.finished",
                  let taskID = event.payload.objectValue?["task_id"]?.stringValue,
                  let status = event.payload.objectValue?["task_status"]?.stringValue
            else {
                continue
            }
            statuses[taskID] = status
        }
        planTaskStatuses = statuses
    }

    private func consumeEvents() {
        eventTask?.cancel()
        eventTask = Task { [weak self] in
            guard let self else { return }
            let stream = await core.events()
            for await event in stream {
                apply(event)
            }
        }
    }

    private func apply(_ event: Event) {
        if event.sessionId == selectedSessionID,
           !events.contains(where: { $0.id == event.id })
        {
            events.append(event)
            if events.count > 1000 {
                events.removeFirst(events.count - 1000)
            }
        }

        if event.sessionId == selectedSessionID {
            switch event.type {
            case "agent.started":
                streamingText = ""
            case "agent.message_delta":
                if let text = event.payload.objectValue?["text"]?.stringValue {
                    streamingText += text
                }
            case "agent.message", "agent.turn_finished", "agent.failed", "agent.cancelled",
                 "agent.budget_exceeded":
                streamingText = ""
            default:
                break
            }
        }

        if event.type == "todo.updated",
           event.sessionId == selectedSessionID,
           let items = event.payload.objectValue?["items"],
           let data = try? JSONEncoder().encode(items),
           let decoded = try? JSONDecoder().decode([TodoItem].self, from: data)
        {
            todos = decoded
        }

        if event.type == "agent.usage",
           let input = event.payload.objectValue?["input_tokens"]?.intValue,
           let output = event.payload.objectValue?["output_tokens"]?.intValue,
           let index = sessions.firstIndex(where: { $0.id == event.sessionId })
        {
            let old = sessions[index]
            sessions[index] = Session(
                id: old.id,
                projectId: old.projectId,
                goal: old.goal,
                status: old.status,
                worktree: old.worktree,
                branch: old.branch,
                tokenBudget: old.tokenBudget,
                costBudgetUsd: old.costBudgetUsd,
                timeBudgetSecs: old.timeBudgetSecs,
                costSpentUsd: old.costSpentUsd,
                baseCommit: old.baseCommit,
                tokensIn: old.tokensIn + input,
                tokensOut: old.tokensOut + output,
                createdAt: old.createdAt
            )
        }

        if event.type == "agent.usage" || event.type == "agent.cost" {
            Task { await loadUsage() }
        }

        if event.type == "agent.cost",
           let total = event.payload.objectValue?["total_usd"]?.doubleValue,
           let index = sessions.firstIndex(where: { $0.id == event.sessionId })
        {
            let old = sessions[index]
            sessions[index] = Session(
                id: old.id,
                projectId: old.projectId,
                goal: old.goal,
                status: old.status,
                worktree: old.worktree,
                branch: old.branch,
                tokenBudget: old.tokenBudget,
                costBudgetUsd: old.costBudgetUsd,
                timeBudgetSecs: old.timeBudgetSecs,
                costSpentUsd: total,
                baseCommit: old.baseCommit,
                tokensIn: old.tokensIn,
                tokensOut: old.tokensOut,
                createdAt: old.createdAt
            )
        }

        if event.type == "session.updated",
           let status = event.payload.objectValue?["status"]?.stringValue,
           let index = sessions.firstIndex(where: { $0.id == event.sessionId })
        {
            let old = sessions[index]
            sessions[index] = Session(
                id: old.id,
                projectId: old.projectId,
                goal: old.goal,
                status: status,
                worktree: old.worktree,
                branch: old.branch,
                tokenBudget: old.tokenBudget,
                costBudgetUsd: old.costBudgetUsd,
                timeBudgetSecs: old.timeBudgetSecs,
                costSpentUsd: old.costSpentUsd,
                baseCommit: old.baseCommit,
                tokensIn: old.tokensIn,
                tokensOut: old.tokensOut,
                createdAt: old.createdAt
            )
        }

        if event.type == "session.created"
            || event.type == "session.worktree"
            || event.type == "session.imported"
            || event.type == "session.rehydrated"
        {
            Task {
                await self.loadSessions()
                await self.loadDiff()
            }
        }

        if event.sessionId == selectedSessionID,
           ["agent.spawned", "agent.finished", "agent.updated"].contains(event.type)
        {
            Task { await self.loadAgents() }
        }

        if event.sessionId == selectedSessionID {
            if event.type == "team_plan.submitted" {
                planTaskStatuses = [:]
                Task { await self.loadTeamPlan() }
            }
            if event.type == "plan_task.started" || event.type == "plan_task.finished",
               let taskID = event.payload.objectValue?["task_id"]?.stringValue,
               let status = event.payload.objectValue?["task_status"]?.stringValue
            {
                planTaskStatuses[taskID] = status
            }
        }

        if event.sessionId == selectedSessionID,
           event.type.hasPrefix("merge_queue.") || event.type.hasPrefix("integrator.")
        {
            Task {
                await self.loadMergeQueue()
                await self.loadDiff()
            }
        }

        if event.sessionId == selectedSessionID,
           event.type.hasPrefix("mailbox.") || event.type.hasPrefix("artifact.")
        {
            Task { await self.loadMailbox() }
        }

        if event.sessionId == selectedSessionID,
           ["tool.finished", "agent.turn_finished", "session.worktree"].contains(event.type)
        {
            Task { await self.loadDiff() }
        }
    }

    private func handle(_ error: Error) {
        lastError = error.localizedDescription
        if case .connecting = connection {
            connection = .failed(error.localizedDescription)
        }
    }
}
