import Foundation

enum CoreError: LocalizedError, Sendable {
    case invalidURL
    case notConnected
    case disconnected
    case rpc(code: Int, message: String)
    case daemonNotFound(String)
    case timeout

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            "Invalid daemon WebSocket URL."
        case .notConnected:
            "Not connected to the VerlinPark daemon."
        case .disconnected:
            "The daemon connection was closed."
        case let .rpc(_, message):
            message
        case let .daemonNotFound(details):
            details
        case .timeout:
            "The daemon did not respond in time."
        }
    }
}

actor CoreClient {
    private var urlSession: URLSession?
    private var socket: URLSessionWebSocketTask?
    private var pending: [String: CheckedContinuation<JSONValue, Error>] = [:]
    private var eventContinuation: AsyncStream<Event>.Continuation?
    private var receiveTask: Task<Void, Never>?
    private var isConnected = false

    func connect(to discovery: Discovery) async throws {
        disconnect()

        guard let url = URL(string: "ws://127.0.0.1:\(discovery.port)/ws") else {
            throw CoreError.invalidURL
        }

        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        let session = URLSession(configuration: configuration)
        let socket = session.webSocketTask(with: url)
        urlSession = session
        self.socket = socket
        socket.resume()
        isConnected = true
        receiveTask = Task { [weak self] in
            await self?.receiveLoop()
        }

        let authResult = try await request(
            method: "auth",
            params: ["token": .string(discovery.token)]
        )
        guard try authResult.decode(AuthResult.self).ok else {
            throw CoreError.rpc(code: -32001, message: "authentication failed")
        }
        _ = try await request(method: "event.subscribe", params: [:])
    }

    func disconnect() {
        isConnected = false
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        urlSession?.invalidateAndCancel()
        urlSession = nil
        receiveTask?.cancel()
        receiveTask = nil
        failAll(CoreError.disconnected)
        eventContinuation?.finish()
        eventContinuation = nil
    }

    func events() -> AsyncStream<Event> {
        AsyncStream { continuation in
            self.eventContinuation = continuation
            continuation.onTermination = { @Sendable _ in
                Task { await self.clearEventContinuation() }
            }
        }
    }

    func ping() async throws -> JSONValue {
        try await request(method: "ping", params: [:])
    }

    func serverInfo() async throws -> ServerInfo {
        try await request(method: "server.info", params: [:]).decode(ServerInfo.self)
    }

    func listProjects() async throws -> [Project] {
        try await request(method: "project.list", params: [:])
            .decode(ProjectListResult.self)
            .projects
    }

    func shellPolicy(projectID: String) async throws -> ShellPolicy {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "project.shell_policy.get", params: params)
            .decode(ShellPolicyResult.self)
            .policy
    }

    func setShellPolicy(projectID: String, policy: ShellPolicy) async throws -> ShellPolicy {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object([
                "network": .string(policy.network),
                "read_deny": .array(policy.readDeny.map(JSONValue.string)),
                "restrict_home": .bool(policy.restrictHome),
            ]),
        ]
        return try await request(method: "project.shell_policy.set", params: params)
            .decode(ShellPolicyResult.self)
            .policy
    }

    func integrationPolicy(projectID: String) async throws -> IntegrationPolicy {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "project.integration_policy.get", params: params)
            .decode(IntegrationPolicyResult.self)
            .policy
    }

    func setIntegrationPolicy(
        projectID: String,
        policy: IntegrationPolicy
    ) async throws -> IntegrationPolicy {
        var policyValue: [String: JSONValue] = [
            "auto_resolve": .bool(policy.autoResolve),
            "max_auto_attempts": .number(Double(policy.maxAutoAttempts)),
        ]
        if let tokens = policy.maxResolveTokens {
            policyValue["max_resolve_tokens"] = .number(Double(tokens))
        } else {
            policyValue["max_resolve_tokens"] = .null
        }
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object(policyValue),
        ]
        return try await request(method: "project.integration_policy.set", params: params)
            .decode(IntegrationPolicyResult.self)
            .policy
    }

    func addProject(root: String, name: String?) async throws -> Project {
        let params: [String: JSONValue] = [
            "root": .string(root),
            "name": name.map(JSONValue.string) ?? .null,
        ]
        return try await request(method: "project.add", params: params)
            .decode(Project.self)
    }

    func listSessions(projectID: String?) async throws -> [Session] {
        let params: [String: JSONValue] = [
            "project_id": projectID.map(JSONValue.string) ?? .null,
        ]
        return try await request(method: "session.list", params: params)
            .decode(SessionListResult.self)
            .sessions
    }

    func createSession(
        projectID: String,
        goal: String,
        tokenBudget: Int?,
        costBudgetUSD: Double? = nil,
        timeBudgetSecs: Int? = nil
    ) async throws -> Session {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "goal": .string(goal),
        ]
        if let tokenBudget, tokenBudget > 0 {
            params["token_budget"] = .number(Double(tokenBudget))
        }
        if let costBudgetUSD, costBudgetUSD > 0 {
            params["cost_budget_usd"] = .number(costBudgetUSD)
        }
        if let timeBudgetSecs, timeBudgetSecs > 0 {
            params["time_budget_secs"] = .number(Double(timeBudgetSecs))
        }
        return try await request(method: "session.create", params: params)
            .decode(Session.self)
    }

    func resumePlan(sessionID: String) async throws -> Bool {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.plan_resume", params: params)
            .decode(SessionPlanResumeResult.self)
            .started
    }

    func transcript(sessionID: String) async throws -> SessionTranscriptResult {
        try await request(
            method: "session.transcript",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionTranscriptResult.self)
    }

    func search(query: String, projectID: String?, limit: Int = 50) async throws -> SearchResult {
        let params: [String: JSONValue] = [
            "query": .string(query),
            "project_id": projectID.map(JSONValue.string) ?? .null,
            "limit": .number(Double(limit)),
        ]
        return try await request(method: "session.search", params: params)
            .decode(SearchResult.self)
    }

    func listEvents(
        sessionID: String,
        since: UInt64 = 0,
        limit: UInt32 = 500
    ) async throws -> [Event] {
        let params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "since_seq": .number(Double(since)),
            "limit": .number(Double(limit)),
        ]
        return try await request(method: "event.list", params: params)
            .decode(EventListResult.self)
            .events
    }

    func discardSession(sessionID: String) async throws {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        _ = try await request(method: "session.discard", params: params)
    }

    func commitSession(sessionID: String, message: String) async throws -> SessionCommit {
        let params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "message": .string(message),
        ]
        return try await request(method: "session.commit", params: params)
            .decode(SessionCommit.self)
    }

    func getTeamPlan(sessionID: String) async throws -> TeamPlan? {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "team_plan.get", params: params)
            .decode(TeamPlanResult.self)
            .plan
    }

    func listAgents(sessionID: String) async throws -> [AgentRecord] {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "agent.list", params: params)
            .decode(AgentListResult.self)
            .agents
    }

    func listMailbox(
        sessionID: String,
        agentID: String? = nil,
        unreadOnly: Bool = false
    ) async throws -> [AgentMessage] {
        var params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "unread_only": .bool(unreadOnly),
        ]
        if let agentID {
            params["agent_id"] = .string(agentID)
        }
        return try await request(method: "mailbox.list", params: params)
            .decode(MailboxListResult.self)
            .messages
    }

    func markMessageRead(messageID: String) async throws -> AgentMessage {
        let params: [String: JSONValue] = ["message_id": .string(messageID)]
        return try await request(method: "mailbox.read", params: params)
            .decode(AgentMessageResult.self)
            .message
    }

    func listArtifacts(sessionID: String) async throws -> [Artifact] {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "artifact.list", params: params)
            .decode(ArtifactListResult.self)
            .artifacts
    }

    func getArtifact(artifactID: String) async throws -> Artifact {
        let params: [String: JSONValue] = ["artifact_id": .string(artifactID)]
        return try await request(method: "artifact.get", params: params)
            .decode(ArtifactResult.self)
            .artifact
    }

    func artifactPolicy(projectID: String) async throws -> ArtifactPolicy {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "artifact.policy.get", params: params)
            .decode(ArtifactPolicyResult.self)
            .policy
    }

    func setArtifactPolicy(
        projectID: String,
        policy: ArtifactPolicy
    ) async throws -> ArtifactPolicy {
        var value: [String: JSONValue] = ["auto_gc": .bool(policy.autoGc)]
        value["max_bytes"] = policy.maxBytes.map { .number(Double($0)) } ?? .null
        value["max_age_secs"] = policy.maxAgeSecs.map { .number(Double($0)) } ?? .null
        value["max_count"] = policy.maxCount.map { .number(Double($0)) } ?? .null
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object(value),
        ]
        return try await request(method: "artifact.policy.set", params: params)
            .decode(ArtifactPolicyResult.self)
            .policy
    }

    func cleanupWorktrees(
        projectID: String,
        olderThanDays: Int,
        dryRun: Bool,
        force: Bool,
        deleteBranches: Bool,
        keepFailed: Bool
    ) async throws -> WorktreeCleanupResult {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "older_than_secs": .number(Double(olderThanDays) * 86400),
            "dry_run": .bool(dryRun),
            "force": .bool(force),
            "delete_branches": .bool(deleteBranches),
            "keep_failed": .bool(keepFailed),
        ]
        return try await request(method: "project.cleanup_worktrees", params: params)
            .decode(WorktreeCleanupResult.self)
    }

    func worktreePolicy(projectID: String) async throws -> WorktreePolicyResult {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "project.worktree_policy.get", params: params)
            .decode(WorktreePolicyResult.self)
    }

    func setWorktreePolicy(
        projectID: String,
        policy: WorktreeRetentionPolicy
    ) async throws -> WorktreePolicyResult {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object([
                "enabled": .bool(policy.enabled),
                "older_than_secs": .number(Double(policy.olderThanSecs)),
                "delete_branches": .bool(policy.deleteBranches),
                "keep_failed": .bool(policy.keepFailed),
            ]),
        ]
        return try await request(method: "project.worktree_policy.set", params: params)
            .decode(WorktreePolicyResult.self)
    }

    func doctor() async throws -> DoctorReport {
        try await request(method: "server.doctor", params: [:])
            .decode(DoctorReport.self)
    }

    func probeProvider() async throws -> AgentProbeResult {
        try await request(method: "agent.probe", params: [:])
            .decode(AgentProbeResult.self)
    }

    func projectBudget(projectID: String) async throws -> ProjectBudgetResult {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "project.budget.get", params: params)
            .decode(ProjectBudgetResult.self)
    }

    func setProjectBudget(
        projectID: String,
        budget: ProjectBudget
    ) async throws -> ProjectBudgetResult {
        var value: [String: JSONValue] = [:]
        value["token_budget"] = budget.tokenBudget.map { .number(Double($0)) } ?? .null
        value["cost_budget_usd"] = budget.costBudgetUsd.map { .number($0) } ?? .null
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "budget": .object(value),
        ]
        return try await request(method: "project.budget.set", params: params)
            .decode(ProjectBudgetResult.self)
    }

    func mailboxPolicy(projectID: String) async throws -> MailboxPolicy {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "mailbox.policy.get", params: params)
            .decode(MailboxPolicyResult.self)
            .policy
    }

    func setMailboxPolicy(
        projectID: String,
        policy: MailboxPolicy
    ) async throws -> MailboxPolicy {
        var value: [String: JSONValue] = [
            "delete_unread": .bool(policy.deleteUnread),
            "auto_gc": .bool(policy.autoGc),
        ]
        value["max_age_secs"] = policy.maxAgeSecs.map { .number(Double($0)) } ?? .null
        value["max_count"] = policy.maxCount.map { .number(Double($0)) } ?? .null
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object(value),
        ]
        return try await request(method: "mailbox.policy.set", params: params)
            .decode(MailboxPolicyResult.self)
            .policy
    }

    func gcMailbox(projectID: String) async throws -> MailboxGcResult {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "mailbox.gc", params: params)
            .decode(MailboxGcResult.self)
    }

    func materializeArtifact(artifactID: String) async throws -> ArtifactMaterialization {
        let params: [String: JSONValue] = ["artifact_id": .string(artifactID)]
        return try await request(method: "artifact.materialize", params: params)
            .decode(ArtifactMaterialization.self)
    }

    func gcArtifacts(projectID: String) async throws -> ArtifactGcResult {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "artifact.gc", params: params)
            .decode(ArtifactGcResult.self)
    }

    func listMergeQueue(sessionID: String) async throws -> [MergeQueueEntry] {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "merge_queue.list", params: params)
            .decode(MergeQueueListResult.self)
            .entries
    }

    func retryMerge(entryID: String) async throws -> MergeQueueEntry {
        let params: [String: JSONValue] = ["entry_id": .string(entryID)]
        return try await request(method: "merge_queue.retry", params: params)
            .decode(MergeQueueRetryResult.self)
            .entry
    }

    func resolveMerge(entryID: String) async throws -> MergeQueueEntry {
        let params: [String: JSONValue] = ["entry_id": .string(entryID)]
        return try await request(method: "merge_queue.resolve", params: params)
            .decode(MergeQueueRetryResult.self)
            .entry
    }

    func landSession(sessionID: String) async throws -> SessionLandResult {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.land", params: params)
            .decode(SessionLandResult.self)
    }

    func pauseSession(sessionID: String) async throws -> Bool {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.pause", params: params)
            .decode(SessionPauseResult.self)
            .paused
    }

    func resumeSession(sessionID: String) async throws -> Bool {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.resume", params: params)
            .decode(SessionResumeResult.self)
            .resumed
    }

    func openPullRequest(sessionID: String) async throws -> PullRequestResult {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.pr", params: params)
            .decode(PullRequestResult.self)
    }

    func sessionTodos(sessionID: String) async throws -> [TodoItem] {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.todos", params: params)
            .decode(SessionTodosResult.self)
            .items
    }

    func usageLedger(sessionID: String) async throws -> UsageLedgerResult {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.usage", params: params)
            .decode(UsageLedgerResult.self)
    }

    func listQuestions(sessionID: String) async throws -> [Question] {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "question.list", params: params)
            .decode(QuestionListResult.self)
            .questions
    }

    func listAllQuestions() async throws -> [Question] {
        try await request(method: "question.list", params: [:])
            .decode(QuestionListResult.self)
            .questions
    }

    func answerQuestion(id: String, answer: String) async throws -> Bool {
        let params: [String: JSONValue] = [
            "question_id": .string(id),
            "answer": .string(answer),
        ]
        return try await request(method: "question.answer", params: params)
            .decode(AnswerQuestionResult.self)
            .answered
    }

    func listMemories(
        projectID: String,
        query: String?,
        kind: String?,
        limit: Int = 50
    ) async throws -> [MemoryItem] {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "query": query.map(JSONValue.string) ?? .null,
            "kind": kind.map(JSONValue.string) ?? .null,
            "limit": .number(Double(limit)),
        ]
        return try await request(method: "memory.list", params: params)
            .decode(MemoryListResult.self)
            .memories
    }

    func createMemory(
        sessionID: String,
        kind: String,
        title: String,
        body: String,
        tags: [String]
    ) async throws -> MemoryItem {
        let params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "kind": .string(kind),
            "title": .string(title),
            "body": .string(body),
            "tags": .array(tags.map(JSONValue.string)),
        ]
        return try await request(method: "memory.create", params: params)
            .decode(MemoryCreateResult.self)
            .memory
    }

    func projectUsage(projectID: String) async throws -> ProjectUsageResult {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "project.usage", params: params)
            .decode(ProjectUsageResult.self)
    }

    func usageCsv(sessionID: String) async throws -> UsageCsvResult {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.usage_csv", params: params)
            .decode(UsageCsvResult.self)
    }

    func sessionDiff(sessionID: String) async throws -> SessionDiff {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.diff", params: params)
            .decode(SessionDiff.self)
    }

    func prompt(sessionID: String, content: String) async throws {
        let params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "content": .string(content),
        ]
        _ = try await request(method: "session.prompt", params: params)
    }

    func listApprovals(sessionID: String?) async throws -> [Approval] {
        let params: [String: JSONValue] = [
            "session_id": sessionID.map(JSONValue.string) ?? .null,
        ]
        return try await request(method: "approval.list", params: params)
            .decode(ApprovalListResult.self)
            .approvals
    }

    func cancelSession(sessionID: String) async throws -> Bool {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.cancel", params: params)
            .decode(CancelResult.self)
            .cancelled
    }

    func resolveApproval(id: String, approved: Bool) async throws {
        let params: [String: JSONValue] = [
            "approval_id": .string(id),
            "approved": .bool(approved),
        ]
        _ = try await request(method: "approval.resolve", params: params)
    }

    func agentInfo() async throws -> AgentInfo {
        try await request(method: "agent.info", params: [:])
            .decode(AgentInfo.self)
    }

    func configureAgent(
        kind: String,
        baseURL: String,
        model: String,
        roleModels: RoleModels? = nil,
        apiKey: String,
        maxManagers: Int,
        maxWorkers: Int,
        maxLabour: Int
    ) async throws -> AgentInfo {
        var params: [String: JSONValue] = [
            "provider": .string(kind),
            "base_url": .string(baseURL),
            "model": .string(model),
            "max_managers": .number(Double(maxManagers)),
            "max_workers": .number(Double(maxWorkers)),
            "max_labour": .number(Double(maxLabour)),
        ]
        if let roleModels {
            params["role_models"] = Self.roleModelsValue(roleModels)
        }
        if !apiKey.isEmpty {
            params["api_key"] = .string(apiKey)
        }
        return try await request(method: "agent.configure", params: params)
            .decode(AgentInfo.self)
    }

    private static func roleModelsValue(_ models: RoleModels) -> JSONValue {
        func value(_ role: RoleModelConfig) -> JSONValue {
            var object: [String: JSONValue] = [:]
            if let model = role.model, !model.isEmpty {
                object["model"] = .string(model)
            }
            if let price = role.inputUsdPerMillion, price > 0 {
                object["input_usd_per_million"] = .number(price)
            }
            if let price = role.outputUsdPerMillion, price > 0 {
                object["output_usd_per_million"] = .number(price)
            }
            return .object(object)
        }
        return .object([
            "leader": value(models.leader),
            "manager": value(models.manager),
            "worker": value(models.worker),
            "verifier": value(models.verifier),
            "labour": value(models.labour),
        ])
    }

    private func request(
        method: String,
        params: [String: JSONValue]
    ) async throws -> JSONValue {
        guard isConnected, let socket else {
            throw CoreError.notConnected
        }
        let id = UUID().uuidString
        let payload = JSONValue.object([
            "jsonrpc": .string("2.0"),
            "id": .string(id),
            "method": .string(method),
            "params": .object(params),
        ])
        let data = try JSONEncoder().encode(payload)
        guard let text = String(data: data, encoding: .utf8) else {
            throw CoreError.invalidURL
        }

        return try await withCheckedThrowingContinuation { continuation in
            pending[id] = continuation
            Task {
                do {
                    try await socket.send(.string(text))
                } catch {
                    self.failRequest(id, error)
                }
            }
        }
    }

    private func receiveLoop() async {
        guard let socket else { return }
        while isConnected {
            do {
                let message = try await socket.receive()
                switch message {
                case let .string(text):
                    handle(text)
                case let .data(data):
                    if let text = String(data: data, encoding: .utf8) {
                        handle(text)
                    }
                @unknown default:
                    break
                }
            } catch {
                failAll(error)
                break
            }
        }
    }

    private func handle(_ text: String) {
        guard
            let data = text.data(using: .utf8),
            let envelope = try? JSONDecoder().decode(RPCEnvelope.self, from: data)
        else {
            return
        }

        if let idValue = envelope.id,
           let id = idValue.stringValue,
           let continuation = pending.removeValue(forKey: id)
        {
            if let error = envelope.error {
                continuation.resume(
                    throwing: CoreError.rpc(code: error.code, message: error.message)
                )
            } else {
                continuation.resume(returning: envelope.result ?? .null)
            }
            return
        }

        if envelope.method == "event.appended",
           let params = envelope.params,
           let event = try? params.decode(Event.self)
        {
            eventContinuation?.yield(event)
        }
    }

    private func failRequest(_ id: String, _ error: Error) {
        pending.removeValue(forKey: id)?.resume(throwing: error)
    }

    private func failAll(_ error: Error) {
        let continuations = pending.values
        pending.removeAll()
        for continuation in continuations {
            continuation.resume(throwing: error)
        }
    }

    private func clearEventContinuation() {
        eventContinuation = nil
    }
}
