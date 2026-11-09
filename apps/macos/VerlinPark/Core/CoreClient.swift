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
    private var requestTimeouts: [String: Task<Void, Never>] = [:]
    private var eventContinuation: AsyncStream<Event>.Continuation?
    private var receiveTask: Task<Void, Never>?
    private var isConnected = false
    private var disconnectHandler: (@Sendable (Error) -> Void)?

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
            params: [
                "token": .string(discovery.token),
                "protocol": .number(1),
            ]
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

    func setDisconnectHandler(_ handler: @escaping @Sendable (Error) -> Void) {
        disconnectHandler = handler
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

    func taskSchedulerPolicy(projectID: String) async throws -> TaskSchedulerPolicy {
        try await request(
            method: "project.task_scheduler.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(TaskSchedulerPolicyResult.self)
        .policy
    }

    func setTaskSchedulerPolicy(
        projectID: String,
        policy: TaskSchedulerPolicy
    ) async throws -> TaskSchedulerPolicy {
        try await request(
            method: "project.task_scheduler.set",
            params: [
                "project_id": .string(projectID),
                "policy": .object([
                    "enabled": .bool(policy.enabled),
                    "cooldown_secs": .number(Double(policy.cooldownSecs)),
                    "dispatch_tasks": .bool(policy.dispatchTasks),
                    "include_intake": .bool(policy.includeIntake),
                    "max_intake_parallel": .number(Double(policy.maxIntakeParallel)),
                ]),
            ]
        )
        .decode(TaskSchedulerPolicyResult.self)
        .policy
    }

    func intakeTasks(
        projectID: String?,
        status: String? = nil,
        limit: Int = 100
    ) async throws -> IntakeListResult {
        try await request(
            method: "intake.list",
            params: [
                "project_id": projectID.map(JSONValue.string) ?? .null,
                "status": status.map(JSONValue.string) ?? .null,
                "limit": .number(Double(limit)),
            ]
        )
        .decode(IntakeListResult.self)
    }

    func createIntakeTask(
        projectID: String,
        title: String,
        instructions: String,
        profile: String,
        kind: String?,
        priority: Int,
        writeScope: [String],
        acceptance: [String],
        deps: [String],
        tokenBudget: UInt64?,
        costBudgetUsd: Double?,
        timeBudgetSecs: UInt64?,
        maxAttempts: UInt32,
        labels: [String]
    ) async throws -> IntakeTask {
        try await request(
            method: "intake.create",
            params: [
                "project_id": .string(projectID),
                "title": .string(title),
                "instructions": .string(instructions),
                "profile": .string(profile),
                "kind": kind.map(JSONValue.string) ?? .null,
                "priority": .number(Double(priority)),
                "write_scope": .array(writeScope.map(JSONValue.string)),
                "acceptance": .array(acceptance.map(JSONValue.string)),
                "deps": .array(deps.map(JSONValue.string)),
                "token_budget": tokenBudget.map { .number(Double($0)) } ?? .null,
                "cost_budget_usd": costBudgetUsd.map { .number($0) } ?? .null,
                "time_budget_secs": timeBudgetSecs.map { .number(Double($0)) } ?? .null,
                "max_attempts": .number(Double(maxAttempts)),
                "labels": .array(labels.map(JSONValue.string)),
                "origin": .string("app"),
            ]
        )
        .decode(IntakeTask.self)
    }

    func updateIntakeTask(
        id: String,
        title: String,
        instructions: String,
        profile: String,
        kind: String?,
        priority: Int,
        writeScope: [String],
        acceptance: [String],
        deps: [String],
        tokenBudget: UInt64?,
        costBudgetUsd: Double?,
        timeBudgetSecs: UInt64?,
        maxAttempts: UInt32,
        labels: [String]
    ) async throws -> IntakeTask {
        try await request(
            method: "intake.update",
            params: [
                "id": .string(id),
                "title": .string(title),
                "instructions": .string(instructions),
                "profile": .string(profile),
                "kind": kind.map(JSONValue.string) ?? .null,
                "priority": .number(Double(priority)),
                "write_scope": .array(writeScope.map(JSONValue.string)),
                "acceptance": .array(acceptance.map(JSONValue.string)),
                "deps": .array(deps.map(JSONValue.string)),
                "token_budget": tokenBudget.map { .number(Double($0)) } ?? .null,
                "cost_budget_usd": costBudgetUsd.map { .number($0) } ?? .null,
                "time_budget_secs": timeBudgetSecs.map { .number(Double($0)) } ?? .null,
                "max_attempts": .number(Double(maxAttempts)),
                "labels": .array(labels.map(JSONValue.string)),
            ]
        )
        .decode(IntakeTask.self)
    }

    func dispatchIntakeTask(id: String) async throws -> IntakeDispatchResult {
        try await request(method: "intake.dispatch", params: ["id": .string(id)])
            .decode(IntakeDispatchResult.self)
    }

    func cancelIntakeTask(id: String) async throws -> IntakeTask {
        try await request(method: "intake.cancel", params: ["id": .string(id)])
            .decode(IntakeTask.self)
    }

    func retryIntakeTask(id: String) async throws -> IntakeTask {
        try await request(method: "intake.retry", params: ["id": .string(id)])
            .decode(IntakeTask.self)
    }

    func deleteIntakeTask(id: String) async throws -> IntakeDeleteResult {
        try await request(method: "intake.delete", params: ["id": .string(id)])
            .decode(IntakeDeleteResult.self)
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
                "read_jail": .bool(policy.readJail),
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
        timeBudgetSecs: Int? = nil,
        priority: Int = 0
    ) async throws -> Session {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "goal": .string(goal),
            "priority": .number(Double(priority)),
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

    func checkpointSession(sessionID: String, label: String?) async throws -> SessionCheckpoint {
        try await request(
            method: "session.checkpoint",
            params: [
                "session_id": .string(sessionID),
                "label": label.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(SessionCheckpoint.self)
    }

    func listCheckpoints(sessionID: String) async throws -> [SessionCheckpoint] {
        try await request(
            method: "session.checkpoints",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionCheckpointListResult.self)
        .checkpoints
    }

    func restoreCheckpoint(
        sessionID: String,
        checkpointID: String
    ) async throws -> SessionCheckpointRestoreResult {
        try await request(
            method: "session.checkpoint.restore",
            params: [
                "session_id": .string(sessionID),
                "checkpoint_id": .string(checkpointID),
            ]
        )
        .decode(SessionCheckpointRestoreResult.self)
    }

    func checkpointDiff(
        sessionID: String,
        checkpointID: String
    ) async throws -> SessionCheckpointDiffResult {
        try await request(
            method: "session.checkpoint.diff",
            params: [
                "session_id": .string(sessionID),
                "checkpoint_id": .string(checkpointID),
            ]
        )
        .decode(SessionCheckpointDiffResult.self)
    }

    func checkpointStats(sessionID: String) async throws -> SessionCheckpointStatsResult {
        try await request(
            method: "session.checkpoint.stats",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionCheckpointStatsResult.self)
    }

    func deleteCheckpoint(
        sessionID: String,
        checkpointID: String
    ) async throws -> SessionCheckpointDeleteResult {
        try await request(
            method: "session.checkpoint.delete",
            params: [
                "session_id": .string(sessionID),
                "checkpoint_id": .string(checkpointID),
            ]
        )
        .decode(SessionCheckpointDeleteResult.self)
    }

    func pruneCheckpoints(
        sessionID: String,
        olderThanDays: Int?,
        keepLatest: Int?,
        dryRun: Bool
    ) async throws -> SessionCheckpointPruneResult {
        try await request(
            method: "session.checkpoint.prune",
            params: [
                "session_id": .string(sessionID),
                "older_than_days": olderThanDays.map { .number(Double($0)) } ?? .null,
                "keep_latest": keepLatest.map { .number(Double($0)) } ?? .null,
                "dry_run": .bool(dryRun),
            ]
        )
        .decode(SessionCheckpointPruneResult.self)
    }

    func mergeFork(
        sourceSessionID: String,
        forkSessionID: String
    ) async throws -> SessionMergeForkResult {
        try await request(
            method: "session.fork.merge",
            params: [
                "source_session_id": .string(sourceSessionID),
                "fork_session_id": .string(forkSessionID),
            ]
        )
        .decode(SessionMergeForkResult.self)
    }

    func downloadUpdate(repo: String? = nil) async throws -> UpdateDownloadResult {
        try await request(
            method: "update.download",
            params: ["repo": repo.map(JSONValue.string) ?? .null]
        )
        .decode(UpdateDownloadResult.self)
    }

    func checkForUpdates(repo: String? = nil) async throws -> UpdateCheckResult {
        try await request(
            method: "update.check",
            params: ["repo": repo.map(JSONValue.string) ?? .null]
        )
        .decode(UpdateCheckResult.self)
    }

    func listModelPresets() async throws -> [ModelPreset] {
        try await request(method: "model.preset.list", params: [:])
            .decode(ModelPresetListResult.self)
            .presets
    }

    func createModelPreset(
        name: String,
        provider: String,
        model: String,
        contextWindow: Int?,
        inputPrice: Double?,
        outputPrice: Double?,
        notes: String
    ) async throws -> ModelPreset {
        try await request(
            method: "model.preset.create",
            params: [
                "name": .string(name),
                "provider": .string(provider),
                "model": .string(model),
                "context_window": contextWindow.map { .number(Double($0)) } ?? .null,
                "input_usd_per_million": inputPrice.map(JSONValue.number) ?? .null,
                "output_usd_per_million": outputPrice.map(JSONValue.number) ?? .null,
                "notes": .string(notes),
            ]
        )
        .decode(ModelPreset.self)
    }

    func updateModelPreset(
        id: String,
        name: String,
        provider: String,
        model: String,
        contextWindow: Int?,
        inputPrice: Double?,
        outputPrice: Double?,
        notes: String
    ) async throws -> ModelPreset {
        try await request(
            method: "model.preset.update",
            params: [
                "id": .string(id),
                "name": .string(name),
                "provider": .string(provider),
                "model": .string(model),
                "context_window": contextWindow.map { .number(Double($0)) } ?? .null,
                "input_usd_per_million": inputPrice.map(JSONValue.number) ?? .null,
                "output_usd_per_million": outputPrice.map(JSONValue.number) ?? .null,
                "notes": .string(notes),
            ]
        )
        .decode(ModelPreset.self)
    }

    func deleteModelPreset(id: String) async throws -> DeleteModelPresetResult {
        try await request(method: "model.preset.delete", params: ["id": .string(id)])
            .decode(DeleteModelPresetResult.self)
    }

    func dashboardSnapshot(projectID: String?) async throws -> DashboardResult {
        try await request(
            method: "dashboard.snapshot",
            params: ["project_id": projectID.map(JSONValue.string) ?? .null]
        )
        .decode(DashboardResult.self)
    }

    func projectForkForest(projectID: String) async throws -> ProjectForkForestResult {
        try await request(
            method: "project.fork.forest",
            params: ["project_id": .string(projectID)]
        )
        .decode(ProjectForkForestResult.self)
    }

    func auditProjectForks(projectID: String) async throws -> ForkAuditResult {
        try await request(
            method: "project.fork.audit",
            params: ["project_id": .string(projectID)]
        )
        .decode(ForkAuditResult.self)
    }

    func repairProjectForks(projectID: String) async throws -> ForkRepairResult {
        try await request(
            method: "project.fork.repair",
            params: ["project_id": .string(projectID)]
        )
        .decode(ForkRepairResult.self)
    }

    func sessionForkTree(sessionID: String) async throws -> SessionForkTreeResult {
        try await request(
            method: "session.fork.tree",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionForkTreeResult.self)
    }

    func sessionLineage(sessionID: String) async throws -> SessionLineageResult {
        try await request(
            method: "session.lineage",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionLineageResult.self)
    }

    func compareSessions(
        sessionID: String,
        otherSessionID: String
    ) async throws -> SessionCompareResult {
        try await request(
            method: "session.compare",
            params: [
                "session_id": .string(sessionID),
                "other_session_id": .string(otherSessionID),
            ]
        )
        .decode(SessionCompareResult.self)
    }

    func forkSession(
        sessionID: String,
        checkpointID: String?,
        goal: String?
    ) async throws -> SessionForkResult {
        try await request(
            method: "session.fork",
            params: [
                "session_id": .string(sessionID),
                "checkpoint_id": checkpointID.map(JSONValue.string) ?? .null,
                "goal": goal.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(SessionForkResult.self)
    }

    func compactSession(sessionID: String) async throws -> SessionCompactResult {
        try await request(
            method: "session.compact",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionCompactResult.self)
    }

    func contextStatus(sessionID: String) async throws -> SessionContextStatus {
        try await request(
            method: "session.context",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionContextStatus.self)
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

    func checkpointPolicy(projectID: String) async throws -> CheckpointPolicyResult {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "project.checkpoint_policy.get", params: params)
            .decode(CheckpointPolicyResult.self)
    }

    func setCheckpointPolicy(
        projectID: String,
        policy: CheckpointRetentionPolicy
    ) async throws -> CheckpointPolicyResult {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object([
                "enabled": .bool(policy.enabled),
                "older_than_secs": .number(Double(policy.olderThanSecs)),
                "keep_latest": .number(Double(policy.keepLatest)),
            ]),
        ]
        return try await request(method: "project.checkpoint_policy.set", params: params)
            .decode(CheckpointPolicyResult.self)
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

    func listAcpAgents() async throws -> [AcpAgent] {
        try await request(method: "acp.agent.list", params: [:])
            .decode(AcpAgentListResult.self)
            .agents
    }

    func addAcpAgent(
        name: String,
        command: String,
        args: [String],
        env: [String: String],
        autoApprove: Bool,
        timeoutSecs: Int
    ) async throws -> AcpAgent {
        let params: [String: JSONValue] = [
            "name": .string(name),
            "command": .string(command),
            "args": .array(args.map(JSONValue.string)),
            "env": .object(env.mapValues(JSONValue.string)),
            "enabled": .bool(true),
            "auto_approve": .bool(autoApprove),
            "timeout_secs": .number(Double(timeoutSecs)),
        ]
        return try await request(method: "acp.agent.add", params: params)
            .decode(AcpAgentResult.self)
            .agent
    }

    func updateAcpAgent(
        _ agent: AcpAgent,
        enabled: Bool,
        autoApprove: Bool
    ) async throws -> AcpAgent {
        let params: [String: JSONValue] = [
            "id": .string(agent.id),
            "name": .string(agent.name),
            "command": .string(agent.command),
            "args": .array(agent.args.map(JSONValue.string)),
            "env": .object(agent.env.mapValues(JSONValue.string)),
            "enabled": .bool(enabled),
            "auto_approve": .bool(autoApprove),
            "timeout_secs": .number(Double(agent.timeoutSecs)),
        ]
        return try await request(method: "acp.agent.update", params: params)
            .decode(AcpAgentResult.self)
            .agent
    }

    func removeAcpAgent(id: String) async throws -> Bool {
        try await request(
            method: "acp.agent.remove",
            params: ["agent_id": .string(id)]
        )
        .decode(AcpRemoveResult.self)
        .removed
    }

    func probeAcpAgent(id: String, projectID: String?) async throws -> AcpProbeResult {
        var params: [String: JSONValue] = ["agent_id": .string(id)]
        if let projectID {
            params["project_id"] = .string(projectID)
        }
        return try await request(method: "acp.agent.probe", params: params)
            .decode(AcpProbeResult.self)
    }

    func listMcpServers() async throws -> [McpServer] {
        try await request(method: "mcp.server.list", params: [:])
            .decode(McpServerListResult.self)
            .servers
    }

    func probeMcpServer(serverID: String) async throws -> McpProbeResult {
        try await request(
            method: "mcp.server.probe",
            params: ["server_id": .string(serverID)]
        )
        .decode(McpProbeResult.self)
    }

    func addMcpServer(
        name: String,
        transport: String,
        command: String,
        args: [String],
        env: [String: String],
        url: String?,
        headers: [String: String]
    ) async throws -> McpServer {
        var params: [String: JSONValue] = [
            "name": .string(name),
            "transport": .string(transport),
            "command": .string(command),
            "args": .array(args.map(JSONValue.string)),
            "env": .object(env.mapValues(JSONValue.string)),
            "headers": .object(headers.mapValues(JSONValue.string)),
            "enabled": .bool(true),
        ]
        if let url {
            params["url"] = .string(url)
        }
        return try await request(method: "mcp.server.add", params: params)
            .decode(McpServerResult.self)
            .server
    }

    func updateMcpServer(
        id: String,
        name: String,
        transport: String,
        command: String,
        args: [String],
        env: [String: String],
        url: String?,
        headers: [String: String],
        enabled: Bool
    ) async throws -> McpServer {
        var params: [String: JSONValue] = [
            "id": .string(id),
            "name": .string(name),
            "transport": .string(transport),
            "command": .string(command),
            "args": .array(args.map(JSONValue.string)),
            "env": .object(env.mapValues(JSONValue.string)),
            "headers": .object(headers.mapValues(JSONValue.string)),
            "enabled": .bool(enabled),
        ]
        if let url {
            params["url"] = .string(url)
        }
        return try await request(method: "mcp.server.update", params: params)
            .decode(McpServerResult.self)
            .server
    }

    func removeMcpServer(id: String) async throws -> Bool {
        try await request(
            method: "mcp.server.remove",
            params: ["server_id": .string(id)]
        )
        .decode(McpRemoveResult.self)
        .removed
    }

    func listMcpTools(serverID: String) async throws -> [McpTool] {
        try await request(
            method: "mcp.tools",
            params: ["server_id": .string(serverID)]
        )
        .decode(McpToolListResult.self)
        .tools
    }

    func listMcpResources(serverID: String) async throws -> McpResourceListResult {
        try await request(
            method: "mcp.resources",
            params: ["server_id": .string(serverID)]
        )
        .decode(McpResourceListResult.self)
    }

    func listMcpPrompts(serverID: String) async throws -> McpPromptListResult {
        try await request(
            method: "mcp.prompts",
            params: ["server_id": .string(serverID)]
        )
        .decode(McpPromptListResult.self)
    }

    func callMcpTool(serverID: String, tool: String, arguments: JSONValue) async throws -> McpCallResult {
        let params: [String: JSONValue] = [
            "server_id": .string(serverID),
            "tool": .string(tool),
            "arguments": arguments,
        ]
        return try await request(method: "mcp.call", params: params)
            .decode(McpCallResult.self)
    }

    func gitLog(sessionID: String, limit: Int = 50) async throws -> GitLogResult {
        try await request(
            method: "git.log",
            params: [
                "session_id": .string(sessionID),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(GitLogResult.self)
    }

    func gitShow(sessionID: String, commit: String) async throws -> GitShowResult {
        try await request(
            method: "git.show",
            params: ["session_id": .string(sessionID), "commit": .string(commit)]
        )
        .decode(GitShowResult.self)
    }

    func workspaceTree(sessionID: String) async throws -> WorkspaceTreeResult {
        try await request(
            method: "workspace.tree",
            params: ["session_id": .string(sessionID)]
        )
        .decode(WorkspaceTreeResult.self)
    }

    func workspaceList(
        sessionID: String,
        path: String,
        offset: Int = 0,
        limit: Int = 500
    ) async throws -> WorkspaceListResult {
        try await request(
            method: "workspace.list",
            params: [
                "session_id": .string(sessionID),
                "path": .string(path),
                "offset": .number(Double(offset)),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(WorkspaceListResult.self)
    }

    func workspaceSearch(
        sessionID: String,
        query: String,
        limit: Int = 300
    ) async throws -> WorkspaceSearchResult {
        try await request(
            method: "workspace.search",
            params: [
                "session_id": .string(sessionID),
                "query": .string(query),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(WorkspaceSearchResult.self)
    }

    func workspaceGrep(
        sessionID: String,
        query: String,
        path: String? = nil,
        caseSensitive: Bool = false,
        maxResults: Int = 200
    ) async throws -> WorkspaceGrepResult {
        try await request(
            method: "workspace.grep",
            params: [
                "session_id": .string(sessionID),
                "query": .string(query),
                "path": path.map(JSONValue.string) ?? .null,
                "case_sensitive": .bool(caseSensitive),
                "max_results": .number(Double(maxResults)),
            ]
        )
        .decode(WorkspaceGrepResult.self)
    }

    func workspaceFile(sessionID: String, path: String) async throws -> WorkspaceFileResult {
        try await request(
            method: "workspace.file",
            params: ["session_id": .string(sessionID), "path": .string(path)]
        )
        .decode(WorkspaceFileResult.self)
    }

    func workspaceWrite(
        sessionID: String,
        path: String,
        content: String,
        expectedSha256: String?
    ) async throws -> WorkspaceWriteResult {
        let params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "path": .string(path),
            "content": .string(content),
            "expected_sha256": expectedSha256.map(JSONValue.string) ?? .null,
        ]
        return try await request(method: "workspace.write", params: params)
            .decode(WorkspaceWriteResult.self)
    }

    func schedulerStatus(projectID: String?) async throws -> SchedulerStatusResult {
        try await request(
            method: "scheduler.status",
            params: ["project_id": projectID.map(JSONValue.string) ?? .null]
        )
        .decode(SchedulerStatusResult.self)
    }

    func taskQueue(projectID: String?, sessionID: String?) async throws -> TaskQueueResult {
        try await request(
            method: "task.queue",
            params: [
                "project_id": projectID.map(JSONValue.string) ?? .null,
                "session_id": sessionID.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(TaskQueueResult.self)
    }

    func orchestrationPolicy(projectID: String) async throws -> OrchestrationPolicy {
        try await request(
            method: "orchestration.policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(OrchestrationPolicyResult.self)
        .policy
    }

    func setOrchestrationPolicy(
        projectID: String,
        policy: OrchestrationPolicy
    ) async throws -> OrchestrationPolicy {
        let params: [String: JSONValue] = try [
            "project_id": .string(projectID),
            "policy": jsonValue(policy),
        ]
        return try await request(method: "orchestration.policy.set", params: params)
            .decode(OrchestrationPolicyResult.self)
            .policy
    }

    func orchestrationOverview(
        projectID: String?,
        limit: Int = 20
    ) async throws -> OrchestrationOverviewResult {
        var params: [String: JSONValue] = ["limit": .number(Double(limit))]
        params["project_id"] = projectID.map(JSONValue.string) ?? .null
        return try await request(method: "orchestration.overview", params: params)
            .decode(OrchestrationOverviewResult.self)
    }

    func runtimeLeases() async throws -> RuntimeLeaseListResult {
        try await request(method: "runtime.leases", params: [:])
            .decode(RuntimeLeaseListResult.self)
    }

    func sweepRuntimeLeases() async throws -> Int {
        try await request(method: "runtime.leases.sweep", params: [:])
            .decode(RuntimeLeaseSweepResult.self)
            .removed
    }

    func previewOrchestration(sessionID: String) async throws -> OrchestrationPreviewResult {
        try await request(
            method: "orchestration.preview",
            params: ["session_id": .string(sessionID)]
        )
        .decode(OrchestrationPreviewResult.self)
    }

    func advanceOrchestration(sessionID: String) async throws -> [OrchestrationDecision] {
        try await request(
            method: "orchestration.advance",
            params: ["session_id": .string(sessionID)]
        )
        .decode(OrchestrationAdvanceResult.self)
        .actions
    }

    func orchestrationSnapshot(sessionID: String) async throws -> OrchestrationSnapshot {
        try await request(
            method: "orchestration.snapshot",
            params: ["session_id": .string(sessionID)]
        )
        .decode(OrchestrationSnapshotResult.self)
        .snapshot
    }

    func planRevisions(sessionID: String) async throws -> [PlanRevisionInfo] {
        try await request(
            method: "team_plan.revisions",
            params: ["session_id": .string(sessionID)]
        )
        .decode(PlanRevisionListResult.self)
        .revisions
    }

    func planRevisionDetail(
        sessionID: String,
        revision: Int
    ) async throws -> PlanRevisionDetailResult {
        try await request(
            method: "team_plan.revision",
            params: [
                "session_id": .string(sessionID),
                "revision": .number(Double(revision)),
            ]
        )
        .decode(PlanRevisionDetailResult.self)
    }

    func restoreTeamPlanRevision(
        sessionID: String,
        revision: Int,
        reason: String,
        expectedRevision: Int?
    ) async throws -> ReviseTeamPlanResult {
        var params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "revision": .number(Double(revision)),
            "reason": .string(reason),
        ]
        params["expected_revision"] = expectedRevision.map { .number(Double($0)) } ?? .null
        return try await request(method: "team_plan.restore", params: params)
            .decode(ReviseTeamPlanResult.self)
    }

    func editPlanTask(
        sessionID: String,
        task: PlanTask,
        reason: String,
        expectedRevision: Int?
    ) async throws -> ReviseTeamPlanResult {
        var params: [String: JSONValue] = try [
            "session_id": .string(sessionID),
            "task": jsonValue(task),
            "reason": .string(reason),
        ]
        params["expected_revision"] = expectedRevision.map { .number(Double($0)) } ?? .null
        return try await request(method: "team_plan.task_edit", params: params)
            .decode(ReviseTeamPlanResult.self)
    }

    func reviseTeamPlan(
        sessionID: String,
        plan: TeamPlan,
        reason: String,
        expectedRevision: Int?
    ) async throws -> ReviseTeamPlanResult {
        var params: [String: JSONValue] = try [
            "session_id": .string(sessionID),
            "plan": jsonValue(plan),
            "reason": .string(reason),
        ]
        params["expected_revision"] = expectedRevision.map { .number(Double($0)) } ?? .null
        return try await request(method: "team_plan.revise", params: params)
            .decode(ReviseTeamPlanResult.self)
    }

    func coordinationNote(
        sessionID: String,
        action: String,
        taskID: String?,
        targetAgentID: String?,
        targetNoteID: String?,
        message: String,
        severity: String
    ) async throws -> String {
        var params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "action": .string(action),
            "message": .string(message),
            "severity": .string(severity),
        ]
        params["task_id"] = taskID.map(JSONValue.string) ?? .null
        params["target_agent_id"] = targetAgentID.map(JSONValue.string) ?? .null
        params["target_note_id"] = targetNoteID.map(JSONValue.string) ?? .null
        return try await request(method: "coordination.note", params: params)
            .decode(CoordinationNoteResult.self)
            .noteId
    }

    func dispatchTask(sessionID: String) async throws -> Bool {
        try await request(
            method: "task.dispatch",
            params: ["session_id": .string(sessionID)]
        )
        .decode(TaskDispatchResult.self)
        .started
    }

    func retryTask(sessionID: String, taskID: String) async throws -> SchedulerTask {
        try await request(
            method: "task.retry",
            params: ["session_id": .string(sessionID), "task_id": .string(taskID)]
        )
        .decode(TaskActionResult.self)
        .task
    }

    func pauseTask(sessionID: String, taskID: String) async throws -> SchedulerTask {
        try await request(
            method: "task.pause",
            params: ["session_id": .string(sessionID), "task_id": .string(taskID)]
        )
        .decode(TaskActionResult.self)
        .task
    }

    func resumeTask(sessionID: String, taskID: String) async throws -> SchedulerTask {
        try await request(
            method: "task.resume",
            params: ["session_id": .string(sessionID), "task_id": .string(taskID)]
        )
        .decode(TaskActionResult.self)
        .task
    }

    func cancelTask(sessionID: String, taskID: String) async throws -> SchedulerTask {
        try await request(
            method: "task.cancel",
            params: ["session_id": .string(sessionID), "task_id": .string(taskID)]
        )
        .decode(TaskActionResult.self)
        .task
    }

    func skipTask(sessionID: String, taskID: String) async throws -> SchedulerTask {
        try await request(
            method: "task.skip",
            params: ["session_id": .string(sessionID), "task_id": .string(taskID)]
        )
        .decode(TaskActionResult.self)
        .task
    }

    func openTerminal(sessionID: String, cols: Int, rows: Int) async throws -> TerminalOpenResult {
        try await request(
            method: "terminal.open",
            params: [
                "session_id": .string(sessionID),
                "cols": .number(Double(cols)),
                "rows": .number(Double(rows)),
            ]
        )
        .decode(TerminalOpenResult.self)
    }

    func writeTerminal(terminalID: String, data: Data) async throws -> Bool {
        try await request(
            method: "terminal.write",
            params: [
                "terminal_id": .string(terminalID),
                "data": .string(data.base64EncodedString()),
            ]
        )
        .decode(TerminalWriteResult.self)
        .written
    }

    func readTerminal(terminalID: String, afterSeq: UInt64) async throws -> TerminalReadResult {
        try await request(
            method: "terminal.read",
            params: [
                "terminal_id": .string(terminalID),
                "after_seq": .number(Double(afterSeq)),
            ]
        )
        .decode(TerminalReadResult.self)
    }

    func resizeTerminal(terminalID: String, cols: Int, rows: Int) async throws -> Bool {
        try await request(
            method: "terminal.resize",
            params: [
                "terminal_id": .string(terminalID),
                "cols": .number(Double(cols)),
                "rows": .number(Double(rows)),
            ]
        )
        .decode(TerminalResizeResult.self)
        .resized
    }

    func closeTerminal(terminalID: String) async throws -> TerminalCloseResult {
        try await request(
            method: "terminal.close",
            params: ["terminal_id": .string(terminalID)]
        )
        .decode(TerminalCloseResult.self)
    }

    func openConsole(sessionID: String) async throws -> ConsoleOpenResult {
        try await request(
            method: "console.open",
            params: ["session_id": .string(sessionID)]
        )
        .decode(ConsoleOpenResult.self)
    }

    func writeConsole(consoleID: String, data: String) async throws -> Bool {
        try await request(
            method: "console.write",
            params: ["console_id": .string(consoleID), "data": .string(data)]
        )
        .decode(ConsoleWriteResult.self)
        .written
    }

    func readConsole(consoleID: String, afterSeq: UInt64) async throws -> ConsoleReadResult {
        try await request(
            method: "console.read",
            params: [
                "console_id": .string(consoleID),
                "after_seq": .number(Double(afterSeq)),
            ]
        )
        .decode(ConsoleReadResult.self)
    }

    func signalConsole(consoleID: String, signal: String) async throws -> Bool {
        try await request(
            method: "console.signal",
            params: ["console_id": .string(consoleID), "signal": .string(signal)]
        )
        .decode(ConsoleSignalResult.self)
        .signaled
    }

    func closeConsole(consoleID: String) async throws -> ConsoleCloseResult {
        try await request(
            method: "console.close",
            params: ["console_id": .string(consoleID)]
        )
        .decode(ConsoleCloseResult.self)
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

    func memoryPolicy(projectID: String) async throws -> MemoryPolicy {
        try await request(
            method: "memory.policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(MemoryPolicyResult.self)
        .policy
    }

    func setMemoryPolicy(
        projectID: String,
        enabled: Bool,
        maxItems: Int?,
        maxAgeDays: Int?
    ) async throws -> MemoryPolicy {
        let policy: [String: JSONValue] = [
            "enabled": .bool(enabled),
            "max_items": maxItems.map { .number(Double($0)) } ?? .null,
            "max_age_days": maxAgeDays.map { .number(Double($0)) } ?? .null,
        ]
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object(policy),
        ]
        return try await request(method: "memory.policy.set", params: params)
            .decode(MemoryPolicyResult.self)
            .policy
    }

    func deleteMemory(id: String) async throws -> MemoryDeleteResult {
        try await request(method: "memory.delete", params: ["id": .string(id)])
            .decode(MemoryDeleteResult.self)
    }

    func pruneMemories(
        projectID: String,
        olderThanDays: Int?,
        keepLatest: Int?,
        dryRun: Bool
    ) async throws -> MemoryPruneResult {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "older_than_days": olderThanDays.map { .number(Double($0)) } ?? .null,
            "keep_latest": keepLatest.map { .number(Double($0)) } ?? .null,
            "dry_run": .bool(dryRun),
        ]
        return try await request(method: "memory.prune", params: params)
            .decode(MemoryPruneResult.self)
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

    func listApprovalGrants(projectID: String) async throws -> [ApprovalGrant] {
        try await request(
            method: "approval.grants",
            params: ["project_id": .string(projectID)]
        )
        .decode(ApprovalGrantListResult.self)
        .grants
    }

    func createApprovalGrant(
        projectID: String,
        tool: String,
        pathPrefix: String,
        includeChildren: Bool
    ) async throws -> ApprovalGrant {
        try await request(
            method: "approval.grant.create",
            params: [
                "project_id": .string(projectID),
                "tool": .string(tool),
                "path_prefix": .string(pathPrefix),
                "include_children": .bool(includeChildren),
            ]
        )
        .decode(ApprovalGrantCreateResult.self)
        .grant
    }

    func deleteApprovalGrant(id: String) async throws -> Bool {
        try await request(
            method: "approval.grant.delete",
            params: ["id": .string(id)]
        )
        .decode(ApprovalGrantDeleteResult.self)
        .deleted
    }

    func listApprovals(sessionID: String?) async throws -> [Approval] {
        let params: [String: JSONValue] = [
            "session_id": sessionID.map(JSONValue.string) ?? .null,
        ]
        return try await request(method: "approval.list", params: params)
            .decode(ApprovalListResult.self)
            .approvals
    }

    func setSessionPriority(sessionID: String, priority: Int) async throws -> SessionPriorityResult {
        try await request(
            method: "session.priority.set",
            params: [
                "session_id": .string(sessionID),
                "priority": .number(Double(priority)),
            ]
        )
        .decode(SessionPriorityResult.self)
    }

    func cancelSession(sessionID: String) async throws -> Bool {
        let params: [String: JSONValue] = ["session_id": .string(sessionID)]
        return try await request(method: "session.cancel", params: params)
            .decode(CancelResult.self)
            .cancelled
    }

    func resolveApproval(id: String, approved: Bool) async throws -> Bool {
        let params: [String: JSONValue] = [
            "approval_id": .string(id),
            "approved": .bool(approved),
        ]
        return try await request(method: "approval.resolve", params: params)
            .decode(ResolveApprovalResult.self)
            .resolved
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
        maxLabour: Int,
        autoExpandWorkers: Bool = false,
        contextWindow: Int? = nil,
        approvalMode: String = AgentApprovalMode.smart.rawValue
    ) async throws -> AgentInfo {
        var params: [String: JSONValue] = [
            "provider": .string(kind),
            "base_url": .string(baseURL),
            "model": .string(model),
            "max_managers": .number(Double(maxManagers)),
            "max_workers": .number(Double(maxWorkers)),
            "max_labour": .number(Double(maxLabour)),
            "auto_expand_workers": .bool(autoExpandWorkers),
            "approval_mode": .string(approvalMode),
        ]
        if let roleModels {
            params["role_models"] = Self.roleModelsValue(roleModels)
        }
        if !apiKey.isEmpty {
            params["api_key"] = .string(apiKey)
        }
        if let contextWindow {
            params["context_window"] = .number(Double(contextWindow))
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
            if let contextWindow = role.contextWindow, contextWindow > 0 {
                object["context_window"] = .number(Double(contextWindow))
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

    private func jsonValue(_ value: some Encodable) throws -> JSONValue {
        let data = try JSONEncoder().encode(value)
        return try JSONDecoder().decode(JSONValue.self, from: data)
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
            requestTimeouts[id] = Task { [weak self] in
                try? await Task.sleep(for: .seconds(30))
                await self?.timeoutRequest(id)
            }
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
                isConnected = false
                eventContinuation?.finish()
                failAll(error)
                disconnectHandler?(error)
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
            requestTimeouts.removeValue(forKey: id)?.cancel()
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
        requestTimeouts.removeValue(forKey: id)?.cancel()
        pending.removeValue(forKey: id)?.resume(throwing: error)
    }

    private func timeoutRequest(_ id: String) {
        requestTimeouts.removeValue(forKey: id)
        pending.removeValue(forKey: id)?.resume(throwing: CoreError.timeout)
    }

    private func failAll(_ error: Error) {
        let continuations = pending.values
        pending.removeAll()
        for task in requestTimeouts.values {
            task.cancel()
        }
        requestTimeouts.removeAll()
        for continuation in continuations {
            continuation.resume(throwing: error)
        }
    }

    private func clearEventContinuation() {
        eventContinuation = nil
    }
}
