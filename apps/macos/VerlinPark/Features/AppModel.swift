import AppKit
import Observation

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
    var todos: [TodoItem] = []
    var agents: [AgentRecord] = []
    var mergeQueue: [MergeQueueEntry] = []
    var shellPolicy = ShellPolicy.allow
    var shellPolicyNetwork = "allow"
    var shellPolicyReadDenyText = ""
    var shellPolicyBusy = false
    var shellPolicyStatus: String?
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
    var providerAPIKey = Keychain.read(account: "provider.apiKey") ?? ""
    var providerStatus: String?
    var providerBusy = false

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
            shellPolicy = .allow
            shellPolicyNetwork = "allow"
            shellPolicyReadDenyText = ""
            shellPolicyStatus = nil
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
        Task { await loadShellPolicy() }
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
            await loadTeamPlan()
            await loadDiff()
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

    func createSession(goal: String, tokenBudget: Int? = nil) async {
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
                tokenBudget: budget
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
                apiKey: providerAPIKey,
                maxManagers: caps.managers,
                maxWorkers: caps.workers,
                maxLabour: caps.labour
            )
            agentInfo = info
            UserDefaults.standard.set(providerKind, forKey: "provider.kind")
            UserDefaults.standard.set(baseURL, forKey: "provider.baseURL")
            UserDefaults.standard.set(model, forKey: "provider.model")
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
                baseCommit: old.baseCommit,
                tokensIn: old.tokensIn + input,
                tokensOut: old.tokensOut + output,
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
