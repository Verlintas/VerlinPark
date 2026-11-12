import Foundation

enum CoreError: LocalizedError, Sendable {
    case invalidURL
    case notConnected
    case disconnected
    case rpc(code: Int, message: String, data: JSONValue?)
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
        case let .rpc(_, message, _):
            message
        case let .daemonNotFound(details):
            details
        case .timeout:
            "The daemon did not respond in time."
        }
    }

    /// Stable Q3 error code carried in `error.data.code`, when present.
    var typedCode: String? {
        guard case let .rpc(_, _, data) = self, let data else { return nil }
        return data.objectValue?["code"]?.stringValue
    }

    /// Bounded hint carried in `error.data.hint`, when present.
    var typedHint: String? {
        guard case let .rpc(_, _, data) = self, let data else { return nil }
        return data.objectValue?["hint"]?.stringValue
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
            throw CoreError.rpc(
                code: -32001,
                message: "authentication failed",
                data: nil
            )
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

    func scalingPolicy(projectID: String) async throws -> ScalingPolicy {
        try await request(
            method: "scaling.policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(ScalingPolicyResult.self)
        .policy
    }

    func setScalingPolicy(
        projectID: String,
        policy: ScalingPolicy
    ) async throws -> ScalingPolicy {
        try await request(
            method: "scaling.policy.set",
            params: [
                "project_id": .string(projectID),
                "policy": policy.jsonValue,
            ]
        )
        .decode(ScalingPolicyResult.self)
        .policy
    }

    func scalingStatus(
        projectID: String?,
        sessionID: String? = nil,
        limit: Int = 100
    ) async throws -> ScalingStatusResult {
        try await request(
            method: "scaling.status",
            params: [
                "project_id": projectID.map(JSONValue.string) ?? .null,
                "session_id": sessionID.map(JSONValue.string) ?? .null,
                "limit": .number(Double(limit)),
            ]
        )
        .decode(ScalingStatusResult.self)
    }

    func scalingPreview(sessionID: String) async throws -> ScalingPreviewResult {
        try await request(
            method: "scaling.preview",
            params: ["session_id": .string(sessionID)]
        )
        .decode(ScalingPreviewResult.self)
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

    func programs(
        projectID: String?,
        status: String? = nil,
        limit: Int = 100
    ) async throws -> ProgramListResult {
        try await request(
            method: "program.list",
            params: [
                "project_id": projectID.map(JSONValue.string) ?? .null,
                "status": status.map(JSONValue.string) ?? .null,
                "limit": .number(Double(limit)),
            ]
        )
        .decode(ProgramListResult.self)
    }

    func programStatus(id: String, eventLimit: Int = 50) async throws -> ProgramStatusResult {
        try await request(
            method: "program.status",
            params: [
                "program_id": .string(id),
                "event_limit": .number(Double(eventLimit)),
            ]
        )
        .decode(ProgramStatusResult.self)
    }

    func createProgram(
        projectID: String,
        goal: String,
        policy: ProgramPolicy,
        tokenBudget: UInt64?,
        costBudgetUsd: Double?,
        activate: Bool
    ) async throws -> ProjectProgram {
        try await request(
            method: "program.create",
            params: [
                "project_id": .string(projectID),
                "goal": .string(goal),
                "policy": policy.jsonValue,
                "token_budget": tokenBudget.map { .number(Double($0)) } ?? .null,
                "cost_budget_usd": costBudgetUsd.map { .number($0) } ?? .null,
                "activate": .bool(activate),
            ]
        )
        .decode(ProgramResult.self)
        .program
    }

    func updateProgram(
        id: String,
        goal: String,
        policy: ProgramPolicy,
        tokenBudget: UInt64?,
        costBudgetUsd: Double?
    ) async throws -> ProjectProgram {
        try await request(
            method: "program.update",
            params: [
                "program_id": .string(id),
                "goal": .string(goal),
                "policy": policy.jsonValue,
                "token_budget": tokenBudget.map { .number(Double($0)) } ?? .null,
                "cost_budget_usd": costBudgetUsd.map { .number($0) } ?? .null,
            ]
        )
        .decode(ProgramResult.self)
        .program
    }

    func pauseProgram(id: String, reason: String? = nil) async throws -> ProjectProgram {
        try await request(
            method: "program.pause",
            params: [
                "program_id": .string(id),
                "reason": reason.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(ProgramResult.self)
        .program
    }

    func resumeProgram(id: String) async throws -> ProjectProgram {
        try await request(method: "program.resume", params: ["program_id": .string(id)])
            .decode(ProgramResult.self)
            .program
    }

    func cancelProgram(id: String, reason: String) async throws -> ProjectProgram {
        try await request(
            method: "program.cancel",
            params: [
                "program_id": .string(id),
                "reason": .string(reason),
            ]
        )
        .decode(ProgramResult.self)
        .program
    }

    func deleteProgram(id: String) async throws -> Bool {
        try await request(method: "program.delete", params: ["program_id": .string(id)])
            .decode(ProgramDeleteResult.self)
            .deleted
    }

    func createProgramMilestone(
        programID: String,
        key: String?,
        title: String,
        objective: String,
        acceptance: [String],
        deps: [String],
        priority: Int,
        maxAttempts: UInt32,
        profile: String,
        writeScope: [String],
        tokenBudget: UInt64?,
        costBudgetUsd: Double?
    ) async throws -> ProgramMilestone {
        try await request(
            method: "program.milestone.create",
            params: [
                "program_id": .string(programID),
                "key": key.map(JSONValue.string) ?? .null,
                "title": .string(title),
                "objective": .string(objective),
                "acceptance": .array(acceptance.map(JSONValue.string)),
                "deps": .array(deps.map(JSONValue.string)),
                "priority": .number(Double(priority)),
                "max_attempts": .number(Double(maxAttempts)),
                "profile": .string(profile),
                "write_scope": .array(writeScope.map(JSONValue.string)),
                "token_budget": tokenBudget.map { .number(Double($0)) } ?? .null,
                "cost_budget_usd": costBudgetUsd.map { .number($0) } ?? .null,
            ]
        )
        .decode(ProgramMilestoneResult.self)
        .milestone
    }

    func retryProgramMilestone(id: String) async throws -> ProgramMilestone {
        try await request(
            method: "program.milestone.retry",
            params: ["milestone_id": .string(id)]
        )
        .decode(ProgramMilestoneResult.self)
        .milestone
    }

    func skipProgramMilestone(id: String, reason: String? = nil) async throws -> ProgramMilestone {
        try await request(
            method: "program.milestone.skip",
            params: [
                "milestone_id": .string(id),
                "reason": reason.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(ProgramMilestoneResult.self)
        .milestone
    }

    func startProgramMilestone(id: String) async throws -> ProgramMilestone {
        try await request(
            method: "program.milestone.start",
            params: ["milestone_id": .string(id)]
        )
        .decode(ProgramMilestoneResult.self)
        .milestone
    }

    func programTick(projectID: String?) async throws -> ProgramTickResult {
        try await request(
            method: "program.tick",
            params: ["project_id": projectID.map(JSONValue.string) ?? .null]
        )
        .decode(ProgramTickResult.self)
    }

    func replayProgram(id: String, repair: Bool = false) async throws -> ProgramReplayResult {
        try await request(
            method: "program.replay",
            params: [
                "program_id": .string(id),
                "repair": .bool(repair),
            ]
        )
        .decode(ProgramReplayResult.self)
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

    func daemonAgentStatus() async throws -> DaemonAgentStatus {
        try await request(method: "daemon.agent.status", params: [:])
            .decode(DaemonAgentStatus.self)
    }

    func guiTimelineAudit(projectID: String, limit: Int = 50) async throws -> GuiTimelineAuditResult {
        try await request(
            method: "gui.timeline.audit",
            params: [
                "project_id": .string(projectID),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(GuiTimelineAuditResult.self)
    }

    func guiTimelineRetention(
        projectID: String,
        olderThanDays: Int? = nil
    ) async throws -> GuiTimelineRetentionResult {
        var params: [String: JSONValue] = ["project_id": .string(projectID)]
        if let olderThanDays {
            params["older_than_days"] = .number(Double(olderThanDays))
        }
        return try await request(method: "gui.timeline.retention", params: params)
            .decode(GuiTimelineRetentionResult.self)
    }

    func guiTimelinePrune(
        projectID: String,
        olderThanDays: Int?,
        limit: Int = 1000,
        dryRun: Bool,
        confirm: Bool = false
    ) async throws -> GuiTimelinePruneResult {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "limit": .number(Double(limit)),
            "dry_run": .bool(dryRun),
            "confirm": .bool(confirm),
        ]
        if let olderThanDays {
            params["older_than_days"] = .number(Double(olderThanDays))
        }
        return try await request(method: "gui.timeline.prune", params: params)
            .decode(GuiTimelinePruneResult.self)
    }

    func guiStatus(projectID: String?) async throws -> GuiStatusResult {
        var params: [String: JSONValue] = [:]
        if let projectID {
            params["project_id"] = .string(projectID)
        }
        return try await request(method: "gui.status", params: params)
            .decode(GuiStatusResult.self)
    }

    func guiTimeline(sessionID: String?, limit: Int = 100) async throws -> GuiTimelineResult {
        var params: [String: JSONValue] = ["limit": .number(Double(limit))]
        if let sessionID {
            params["session_id"] = .string(sessionID)
        }
        return try await request(method: "gui.timeline", params: params)
            .decode(GuiTimelineResult.self)
    }

    func releaseComputerLease(leaseID: String) async throws -> Bool {
        try await request(
            method: "computer.leases.release",
            params: ["lease_id": .string(leaseID)]
        )
        .decode(ComputerLeaseReleaseResult.self)
        .released
    }

    func guiPolicy(projectID: String) async throws -> GuiPolicy {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "gui.policy.get", params: params)
            .decode(GuiPolicyResult.self)
            .policy
    }

    func setGuiPolicy(projectID: String, policy: GuiPolicy) async throws -> GuiPolicy {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object([
                "grace_secs": .number(Double(policy.graceSecs)),
                "wait_ms": .number(Double(policy.waitMs)),
                "status_ttl_secs": .number(Double(policy.statusTtlSecs)),
                "sensitive_mode": .string(policy.sensitiveMode.rawValue),
                "sensitive_bundles": .array(policy.sensitiveBundles.map(JSONValue.string)),
                "sensitive_titles": .array(policy.sensitiveTitles.map(JSONValue.string)),
            ]),
        ]
        return try await request(method: "gui.policy.set", params: params)
            .decode(GuiPolicyResult.self)
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
        priority: Int = 0,
        unlimited: Bool = false
    ) async throws -> Session {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "goal": .string(goal),
            "priority": .number(Double(priority)),
            "unlimited": .bool(unlimited),
        ]
        if !unlimited {
            if let tokenBudget, tokenBudget > 0 {
                params["token_budget"] = .number(Double(tokenBudget))
            }
            if let costBudgetUSD, costBudgetUSD > 0 {
                params["cost_budget_usd"] = .number(costBudgetUSD)
            }
            if let timeBudgetSecs, timeBudgetSecs > 0 {
                params["time_budget_secs"] = .number(Double(timeBudgetSecs))
            }
        }
        return try await request(method: "session.create", params: params)
            .decode(Session.self)
    }

    func setSessionUnlimited(
        sessionID: String,
        unlimited: Bool
    ) async throws -> SessionUnlimitedResult {
        try await request(
            method: "session.budget.set",
            params: [
                "session_id": .string(sessionID),
                "unlimited": .bool(unlimited),
            ]
        )
        .decode(SessionUnlimitedResult.self)
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

    func backupStatus() async throws -> BackupStatusResult {
        try await request(method: "backup.status", params: [:])
            .decode(BackupStatusResult.self)
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

    func contextCheckpoints(
        sessionID: String,
        limit: Int? = nil
    ) async throws -> ContextCheckpointListResult {
        try await request(
            method: "session.context.checkpoints",
            params: [
                "session_id": .string(sessionID),
                "limit": limit.map { .number(Double($0)) } ?? .null,
            ]
        )
        .decode(ContextCheckpointListResult.self)
    }

    func contextCheckpointDetail(
        sessionID: String,
        checkpointID: String
    ) async throws -> ContextCheckpointDetailResult {
        try await request(
            method: "session.context.checkpoint.detail",
            params: [
                "session_id": .string(sessionID),
                "checkpoint_id": .string(checkpointID),
            ]
        )
        .decode(ContextCheckpointDetailResult.self)
    }

    func createContextCheckpoint(
        sessionID: String,
        label: String?
    ) async throws -> ContextCheckpoint {
        try await request(
            method: "session.context.checkpoint",
            params: [
                "session_id": .string(sessionID),
                "label": label.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(ContextCheckpoint.self)
    }

    func restoreContextCheckpoint(
        sessionID: String,
        checkpointID: String,
        force: Bool
    ) async throws -> ContextRestoreResult {
        try await request(
            method: "session.context.restore",
            params: [
                "session_id": .string(sessionID),
                "checkpoint_id": .string(checkpointID),
                "force": .bool(force),
            ]
        )
        .decode(ContextRestoreResult.self)
    }

    func deleteContextCheckpoint(
        sessionID: String,
        checkpointID: String
    ) async throws -> ContextCheckpointDeleteResult {
        try await request(
            method: "session.context.checkpoint.delete",
            params: [
                "session_id": .string(sessionID),
                "checkpoint_id": .string(checkpointID),
            ]
        )
        .decode(ContextCheckpointDeleteResult.self)
    }

    func pruneContextCheckpoints(
        sessionID: String,
        olderThanDays: Int?,
        keepLatest: Int?,
        dryRun: Bool
    ) async throws -> ContextCheckpointPruneResult {
        try await request(
            method: "session.context.checkpoint.prune",
            params: [
                "session_id": .string(sessionID),
                "older_than_days": olderThanDays.map { .number(Double($0)) } ?? .null,
                "keep_latest": keepLatest.map { .number(Double($0)) } ?? .null,
                "dry_run": .bool(dryRun),
            ]
        )
        .decode(ContextCheckpointPruneResult.self)
    }

    func contextCheckpointPolicy(projectID: String) async throws -> ContextCheckpointPolicyResult {
        try await request(
            method: "session.context.policy",
            params: ["project_id": .string(projectID)]
        )
        .decode(ContextCheckpointPolicyResult.self)
    }

    func setContextCheckpointPolicy(
        projectID: String,
        policy: ContextCheckpointPolicy
    ) async throws -> ContextCheckpointPolicyResult {
        try await request(
            method: "session.context.policy.set",
            params: [
                "project_id": .string(projectID),
                "policy": .object([
                    "enabled": .bool(policy.enabled),
                    "max_checkpoints": .number(Double(policy.maxCheckpoints)),
                    "max_age_days": .number(Double(policy.maxAgeDays)),
                ]),
            ]
        )
        .decode(ContextCheckpointPolicyResult.self)
    }

    func sessionCache(sessionID: String) async throws -> SessionCacheStatus {
        try await request(
            method: "session.cache",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionCacheStatus.self)
    }

    func sessionCacheCalls(
        sessionID: String,
        limit: Int = 20,
        role: String? = nil,
        missReason: String? = nil,
        includeWarm: Bool = false
    ) async throws -> SessionCacheCallsResult {
        var params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "limit": .number(Double(max(1, min(100, limit)))),
            "include_warm": .bool(includeWarm),
        ]
        if let role {
            params["role"] = .string(role)
        }
        if let missReason {
            params["miss_reason"] = .string(missReason)
        }
        return try await request(method: "session.cache.calls", params: params)
            .decode(SessionCacheCallsResult.self)
    }

    func sessionInstructions(
        sessionID: String,
        agentID: String? = nil
    ) async throws -> SessionInstructionsReport {
        var params: [String: JSONValue] = ["session_id": .string(sessionID)]
        if let agentID {
            params["agent_id"] = .string(agentID)
        }
        return try await request(
            method: "session.instructions",
            params: params
        )
        .decode(SessionInstructionsReport.self)
    }

    func setSessionPromptLayout(
        sessionID: String,
        layout: Int
    ) async throws -> SessionInstructionsReport {
        _ = try await request(
            method: "session.prompt_layout.set",
            params: [
                "session_id": .string(sessionID),
                "prompt_layout": .number(Double(layout)),
            ]
        )
        return try await sessionInstructions(sessionID: sessionID)
    }

    func budgetGovernor(
        sessionID: String,
        limit: Int = 20
    ) async throws -> GovernorSessionStatus {
        try await request(
            method: "session.budget.governor",
            params: [
                "session_id": .string(sessionID),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(GovernorSessionStatus.self)
    }

    func governorPreview(
        sessionID: String,
        remainingTokens: Int? = nil,
        limit: Int = 20
    ) async throws -> GovernorSessionStatus {
        var params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "limit": .number(Double(limit)),
        ]
        if let remainingTokens {
            params["remaining_tokens"] = .number(Double(remainingTokens))
        }
        return try await request(method: "governor.preview", params: params)
            .decode(GovernorSessionStatus.self)
    }

    func criticalPathPolicy(projectID: String) async throws -> CriticalPathPolicy {
        try await request(
            method: "critical_path.policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(CriticalPathPolicyResult.self)
        .policy
    }

    func setCriticalPathPolicy(
        projectID: String,
        policy: CriticalPathPolicy
    ) async throws -> CriticalPathPolicy {
        try await request(
            method: "critical_path.policy.set",
            params: [
                "project_id": .string(projectID),
                "policy": policy.jsonValue(),
            ]
        )
        .decode(CriticalPathPolicyResult.self)
        .policy
    }

    func toolActivity(
        sessionID: String,
        limit: Int = 50
    ) async throws -> ToolActivityResult {
        try await request(
            method: "session.tool_activity",
            params: [
                "session_id": .string(sessionID),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(ToolActivityResult.self)
    }

    /// Explicit, operator-triggered warm of the stable prompt prefix. This
    /// makes one provider call and is never invoked by a refresh path.
    func warmSessionCache(sessionID: String, role: String? = nil) async throws
        -> SessionCacheWarmResult
    {
        var params: [String: JSONValue] = ["session_id": .string(sessionID)]
        if let role {
            params["role"] = .string(role)
        }
        return try await request(method: "session.cache.warm", params: params)
            .decode(SessionCacheWarmResult.self)
    }

    func cachePolicy(projectID: String) async throws -> SessionCachePolicy {
        try await request(
            method: "project.cache_policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(ProjectCachePolicyResult.self)
        .policy
    }

    func cachePlan(sessionID: String, roles: [String] = []) async throws
        -> SessionCachePlanResult
    {
        try await request(
            method: "session.cache.plan",
            params: [
                "session_id": .string(sessionID),
                "roles": .array(roles.map(JSONValue.string)),
            ]
        )
        .decode(SessionCachePlanResult.self)
    }

    func cacheAudit(sessionID: String) async throws -> SessionCacheAuditResult {
        try await request(
            method: "session.cache.audit",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionCacheAuditResult.self)
    }

    func toolOutputPolicy(projectID: String) async throws -> ToolOutputPolicy {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "tool_output.policy.get", params: params)
            .decode(ToolOutputPolicyResult.self)
            .policy
    }

    func toolOutputStatus(
        projectID: String?,
        sessionID: String?
    ) async throws -> ToolOutputStatus {
        var params: [String: JSONValue] = [:]
        if let projectID {
            params["project_id"] = .string(projectID)
        }
        if let sessionID {
            params["session_id"] = .string(sessionID)
        }
        return try await request(method: "tool_output.status", params: params)
            .decode(ToolOutputStatus.self)
    }

    func setToolOutputPolicy(
        projectID: String,
        policy: ToolOutputPolicy
    ) async throws -> ToolOutputPolicy {
        var value: [String: JSONValue] = [
            "enabled": .bool(policy.enabled),
            "spill_threshold_bytes": .number(Double(policy.spillThresholdBytes)),
            "preview_bytes": .number(Double(policy.previewBytes)),
            "max_total_bytes_per_turn": .number(Double(policy.maxTotalBytesPerTurn)),
            "retention_secs": .number(Double(policy.retentionSecs)),
            "max_entries": .number(Double(policy.maxEntries)),
        ]
        if let thresholds = policy.toolThresholds, !thresholds.isEmpty {
            value["tool_thresholds"] = .object(
                thresholds.mapValues { .number(Double($0)) }
            )
        }
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object(value),
        ]
        return try await request(method: "tool_output.policy.set", params: params)
            .decode(ToolOutputPolicyResult.self)
            .policy
    }

    func auditToolOutputs(
        projectID: String?,
        verifyHashes: Bool,
        cleanOrphans: Bool
    ) async throws -> ToolOutputAuditResult {
        var params: [String: JSONValue] = [
            "verify_hashes": .bool(verifyHashes),
            "clean_orphans": .bool(cleanOrphans),
        ]
        if let projectID {
            params["project_id"] = .string(projectID)
        }
        return try await request(method: "tool_output.audit", params: params)
            .decode(ToolOutputAuditResult.self)
    }

    func testCacheScopePreview(projectID: String) async throws -> TestCacheScopePreview {
        try await request(
            method: "test_cache.scope.preview",
            params: ["project_id": .string(projectID)]
        )
        .decode(TestCacheScopePreview.self)
    }

    func testCacheStats(projectID: String) async throws -> TestCacheStats {
        try await request(
            method: "test_cache.stats",
            params: ["project_id": .string(projectID)]
        )
        .decode(TestCacheStats.self)
    }

    func testCachePolicy(projectID: String) async throws -> TestCachePolicy {
        try await request(
            method: "test_cache.policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(TestCachePolicyResult.self)
        .policy
    }

    func setTestCachePolicy(
        projectID: String,
        policy: TestCachePolicy
    ) async throws -> TestCachePolicy {
        let value: [String: JSONValue] = [
            "enabled": .bool(policy.enabled),
            "max_entries": .number(Double(policy.maxEntries)),
            "retention_days": .number(Double(policy.retentionDays)),
            "max_output_bytes": .number(Double(policy.maxOutputBytes)),
            "reuse_lint": .bool(policy.reuseLint),
            "include_integration": .bool(policy.includeIntegration),
            "failed_retention_secs": .number(Double(policy.failedRetentionSecs)),
        ]
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object(value),
        ]
        return try await request(method: "test_cache.policy.set", params: params)
            .decode(TestCachePolicyResult.self)
            .policy
    }

    func testCacheEntries(
        projectID: String,
        sessionID: String? = nil,
        kind: String? = nil,
        status: String? = nil,
        includeExpired: Bool = false,
        limit: Int = 100
    ) async throws -> TestCacheEntriesResult {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "include_expired": .bool(includeExpired),
            "limit": .number(Double(limit)),
        ]
        if let sessionID {
            params["session_id"] = .string(sessionID)
        }
        if let kind {
            params["kind"] = .string(kind)
        }
        if let status {
            params["status"] = .string(status)
        }
        return try await request(method: "test_cache.entries", params: params)
            .decode(TestCacheEntriesResult.self)
    }

    func pruneTestCache(
        projectID: String,
        olderThanSecs: Int? = nil,
        expiredOnly: Bool = false,
        kind: String? = nil,
        dryRun: Bool = false
    ) async throws -> TestCachePruneResult {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "expired_only": .bool(expiredOnly),
            "dry_run": .bool(dryRun),
        ]
        if let olderThanSecs {
            params["older_than_secs"] = .number(Double(olderThanSecs))
        }
        if let kind {
            params["kind"] = .string(kind)
        }
        return try await request(method: "test_cache.prune", params: params)
            .decode(TestCachePruneResult.self)
    }

    func clearTestCache(
        projectID: String,
        sessionID: String? = nil,
        kind: String? = nil,
        confirm: Bool
    ) async throws -> TestCacheClearResult {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "confirm": .bool(confirm),
        ]
        if let sessionID {
            params["session_id"] = .string(sessionID)
        }
        if let kind {
            params["kind"] = .string(kind)
        }
        return try await request(method: "test_cache.clear", params: params)
            .decode(TestCacheClearResult.self)
    }

    func testImpact(
        sessionID: String,
        changedPaths: [String],
        limit: Int = 32,
        includeHistory: Bool = true
    ) async throws -> TestCacheImpactResult {
        let params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "changed_paths": .array(changedPaths.map(JSONValue.string)),
            "limit": .number(Double(limit)),
            "include_history": .bool(includeHistory),
        ]
        return try await request(method: "test_cache.impact", params: params)
            .decode(TestCacheImpactResult.self)
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

    func sessionCitations(
        sessionID: String,
        messageID: String? = nil
    ) async throws -> SessionCitationsResult {
        try await request(
            method: "session.citations",
            params: [
                "session_id": .string(sessionID),
                "message_id": messageID.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(SessionCitationsResult.self)
    }

    func artifactDiff(
        artifactID: String,
        fromVersion: Int,
        toVersion: Int? = nil,
        context: Int = 3
    ) async throws -> ArtifactDiffResult {
        try await request(
            method: "artifact.diff",
            params: [
                "artifact_id": .string(artifactID),
                "from_version": .number(Double(fromVersion)),
                "to_version": toVersion.map { .number(Double($0)) } ?? .null,
                "context": .number(Double(context)),
            ]
        )
        .decode(ArtifactDiffResult.self)
    }

    func artifactVersions(artifactID: String) async throws -> ArtifactVersionsResult {
        try await request(
            method: "artifact.versions",
            params: ["artifact_id": .string(artifactID)]
        )
        .decode(ArtifactVersionsResult.self)
    }

    func artifactArchiveList(
        artifactID: String,
        maxEntries: Int? = nil
    ) async throws -> ArtifactArchiveListResult {
        var params: [String: JSONValue] = ["artifact_id": .string(artifactID)]
        if let maxEntries {
            params["max_entries"] = .number(Double(maxEntries))
        }
        return try await request(method: "artifact.archive.list", params: params)
            .decode(ArtifactArchiveListResult.self)
    }

    func artifactArchiveExtract(
        artifactID: String,
        entries: [String],
        all: Bool = false,
        maxEntries: Int? = nil,
        includePreviews: Bool = true
    ) async throws -> ArtifactArchiveExtractResult {
        var params: [String: JSONValue] = [
            "artifact_id": .string(artifactID),
            "entries": .array(entries.map(JSONValue.string)),
            "all": .bool(all),
            "include_previews": .bool(includePreviews),
        ]
        if let maxEntries {
            params["max_entries"] = .number(Double(maxEntries))
        }
        return try await request(method: "artifact.archive.extract", params: params)
            .decode(ArtifactArchiveExtractResult.self)
    }

    func mediaInfo(artifactID: String) async throws -> MediaInfoResult {
        try await request(
            method: "media.info",
            params: ["artifact_id": .string(artifactID)]
        )
        .decode(MediaInfoResult.self)
    }

    func mediaTranscode(
        artifactID: String,
        format: String,
        maxDurationSecs: Int,
        maxBytes: Int? = nil,
        approved: Bool
    ) async throws -> MediaTranscodeResult {
        var params: [String: JSONValue] = [
            "artifact_id": .string(artifactID),
            "format": .string(format),
            "max_duration_secs": .number(Double(maxDurationSecs)),
            "approved": .bool(approved),
        ]
        if let maxBytes {
            params["max_bytes"] = .number(Double(maxBytes))
        }
        return try await request(method: "media.transcode", params: params)
            .decode(MediaTranscodeResult.self)
    }

    func restoreArtifactVersion(artifactID: String, version: Int) async throws -> Artifact {
        try await request(
            method: "artifact.restore",
            params: [
                "artifact_id": .string(artifactID),
                "version": .number(Double(version)),
            ]
        )
        .decode(ArtifactResult.self)
        .artifact
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

    func remoteBootstrapStatus(projectID: String) async throws -> RemoteBootstrapStatusResult {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "remote.bootstrap.status", params: params)
            .decode(RemoteBootstrapStatusResult.self)
    }

    func remoteTestStatus(
        projectID: String,
        sessionID: String?,
        entryID: String?,
        limit: Int = 20
    ) async throws -> RemoteTestStatusResult {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "limit": .number(Double(limit)),
        ]
        if let sessionID {
            params["session_id"] = .string(sessionID)
        }
        if let entryID {
            params["entry_id"] = .string(entryID)
        }
        return try await request(method: "remote.test.status", params: params)
            .decode(RemoteTestStatusResult.self)
    }

    func remoteIntegrationStatus(
        projectID: String,
        sessionID: String?
    ) async throws -> RemoteIntegrationStatusResult {
        var params: [String: JSONValue] = ["project_id": .string(projectID)]
        if let sessionID {
            params["session_id"] = .string(sessionID)
        }
        return try await request(method: "remote.integration.status", params: params)
            .decode(RemoteIntegrationStatusResult.self)
    }

    func cleanupRemoteIntegration(
        projectID: String,
        sessionID: String?,
        entryID: String?,
        force: Bool
    ) async throws -> RemoteIntegrationCleanupResult {
        var params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "force": .bool(force),
        ]
        if let sessionID {
            params["session_id"] = .string(sessionID)
        }
        if let entryID {
            params["entry_id"] = .string(entryID)
        }
        return try await request(method: "remote.integration.cleanup", params: params)
            .decode(RemoteIntegrationCleanupResult.self)
    }

    func remoteIntegrationPolicy(projectID: String) async throws -> RemoteIntegrationPolicyResult {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        return try await request(method: "remote.integration.policy.get", params: params)
            .decode(RemoteIntegrationPolicyResult.self)
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

    func forgeStatus(projectID: String?) async throws -> ForgeStatus {
        var params: [String: JSONValue] = [:]
        if let projectID {
            params["project_id"] = .string(projectID)
        }
        return try await request(method: "forge.status", params: params)
            .decode(ForgeStatus.self)
    }

    func forgePreflight(projectID: String, provider: String? = nil) async throws
        -> ForgePreflightResult
    {
        var params: [String: JSONValue] = ["project_id": .string(projectID)]
        if let provider {
            params["provider"] = .string(provider)
        }
        return try await request(method: "forge.preflight", params: params)
            .decode(ForgePreflightResult.self)
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
        timeoutSecs: Int,
        transport: AcpTransport = .stdio,
        endpoint: String? = nil,
        authHeaderEnv: String? = nil,
        headers: [String: String] = [:],
        connectTimeoutSecs: Int = 30,
        allowRemote: Bool = false
    ) async throws -> AcpAgent {
        var params: [String: JSONValue] = [
            "name": .string(name),
            "command": .string(command),
            "args": .array(args.map(JSONValue.string)),
            "env": .object(env.mapValues(JSONValue.string)),
            "enabled": .bool(true),
            "auto_approve": .bool(autoApprove),
            "timeout_secs": .number(Double(timeoutSecs)),
            "transport": .string(transport.rawValue),
            "headers": .object(headers.mapValues(JSONValue.string)),
            "connect_timeout_secs": .number(Double(connectTimeoutSecs)),
            "allow_remote": .bool(allowRemote),
        ]
        if let endpoint {
            params["endpoint"] = .string(endpoint)
        }
        if let authHeaderEnv {
            params["auth_header_env"] = .string(authHeaderEnv)
        }
        return try await request(method: "acp.agent.add", params: params)
            .decode(AcpAgentResult.self)
            .agent
    }

    func updateAcpAgent(
        _ agent: AcpAgent,
        enabled: Bool,
        autoApprove: Bool
    ) async throws -> AcpAgent {
        try await updateAcpAgentSettings(
            id: agent.id,
            name: agent.name,
            command: agent.command,
            args: agent.args,
            env: agent.env,
            transport: agent.transport,
            endpoint: agent.endpoint,
            authHeaderEnv: agent.authHeaderEnv,
            headers: agent.headers,
            connectTimeoutSecs: agent.connectTimeoutSecs,
            allowRemote: agent.allowRemote,
            autoApprove: autoApprove,
            timeoutSecs: agent.timeoutSecs,
            enabled: enabled
        )
    }

    func updateAcpAgentSettings(
        id: String,
        name: String,
        command: String,
        args: [String],
        env: [String: String],
        transport: AcpTransport,
        endpoint: String?,
        authHeaderEnv: String?,
        headers: [String: String],
        connectTimeoutSecs: Int,
        allowRemote: Bool,
        autoApprove: Bool,
        timeoutSecs: Int,
        enabled: Bool
    ) async throws -> AcpAgent {
        var params: [String: JSONValue] = [
            "id": .string(id),
            "name": .string(name),
            "command": .string(command),
            "args": .array(args.map(JSONValue.string)),
            "env": .object(env.mapValues(JSONValue.string)),
            "enabled": .bool(enabled),
            "auto_approve": .bool(autoApprove),
            "timeout_secs": .number(Double(timeoutSecs)),
            "transport": .string(transport.rawValue),
            "headers": .object(headers.mapValues(JSONValue.string)),
            "connect_timeout_secs": .number(Double(connectTimeoutSecs)),
            "allow_remote": .bool(allowRemote),
        ]
        if let endpoint {
            params["endpoint"] = .string(endpoint)
        }
        if let authHeaderEnv {
            params["auth_header_env"] = .string(authHeaderEnv)
        }
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

    func citationAudit(sessionID: String) async throws -> SessionCitationAuditResult {
        try await request(
            method: "session.citation_audit",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionCitationAuditResult.self)
    }

    func skillActivations(sessionID: String) async throws -> SkillActivationsResult {
        try await request(
            method: "skill.activations",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SkillActivationsResult.self)
    }

    func skills(
        projectID: String,
        includeDisabled: Bool = true
    ) async throws -> SkillListResult {
        try await request(
            method: "skill.list",
            params: [
                "project_id": .string(projectID),
                "include_disabled": .bool(includeDisabled),
            ]
        )
        .decode(SkillListResult.self)
    }

    func createSkill(
        projectID: String,
        name: String,
        description: String,
        whenToUse: String,
        version: String?,
        instructions: String,
        toolAllow: [String],
        toolDeny: [String],
        profiles: [String],
        roles: [String]
    ) async throws -> ProjectSkill {
        try await request(
            method: "skill.create",
            params: [
                "project_id": .string(projectID),
                "name": .string(name),
                "description": .string(description),
                "when_to_use": .string(whenToUse),
                "version": version.map(JSONValue.string) ?? .null,
                "instructions": .string(instructions),
                "tool_allow": .array(toolAllow.map(JSONValue.string)),
                "tool_deny": .array(toolDeny.map(JSONValue.string)),
                "profiles": .array(profiles.map(JSONValue.string)),
                "roles": .array(roles.map(JSONValue.string)),
            ]
        )
        .decode(ProjectSkill.self)
    }

    func importSkill(projectID: String, path: String) async throws -> ProjectSkill {
        try await request(
            method: "skill.import",
            params: [
                "project_id": .string(projectID),
                "path": .string(path),
            ]
        )
        .decode(ProjectSkill.self)
    }

    func updateSkill(
        id: String,
        name: String,
        description: String,
        whenToUse: String,
        version: String?,
        instructions: String,
        toolAllow: [String],
        toolDeny: [String],
        profiles: [String],
        roles: [String]
    ) async throws -> ProjectSkill {
        try await request(
            method: "skill.update",
            params: [
                "id": .string(id),
                "name": .string(name),
                "description": .string(description),
                "when_to_use": .string(whenToUse),
                "version": version.map(JSONValue.string) ?? .null,
                "instructions": .string(instructions),
                "tool_allow": .array(toolAllow.map(JSONValue.string)),
                "tool_deny": .array(toolDeny.map(JSONValue.string)),
                "profiles": .array(profiles.map(JSONValue.string)),
                "roles": .array(roles.map(JSONValue.string)),
            ]
        )
        .decode(ProjectSkill.self)
    }

    func setSkillEnabled(id: String, enabled: Bool) async throws -> ProjectSkill {
        try await request(
            method: "skill.set_enabled",
            params: ["id": .string(id), "enabled": .bool(enabled)]
        )
        .decode(ProjectSkill.self)
    }

    func deleteSkill(id: String) async throws -> SkillDeleteResult {
        try await request(method: "skill.delete", params: ["id": .string(id)])
            .decode(SkillDeleteResult.self)
    }

    func sources(
        projectID: String,
        query: String? = nil,
        status: String? = nil,
        limit: Int = 100
    ) async throws -> SourceListResult {
        try await request(
            method: "source.list",
            params: [
                "project_id": .string(projectID),
                "query": query.map(JSONValue.string) ?? .null,
                "status": status.map(JSONValue.string) ?? .null,
                "limit": .number(Double(limit)),
            ]
        )
        .decode(SourceListResult.self)
    }

    func sourceGet(
        sourceID: String,
        offset: Int = 0,
        limit: Int = 20
    ) async throws -> SourceGetResult {
        try await request(
            method: "source.get",
            params: [
                "source_id": .string(sourceID),
                "offset": .number(Double(offset)),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(SourceGetResult.self)
    }

    func addSourceNote(
        projectID: String,
        collection: String? = nil,
        title: String,
        content: String
    ) async throws -> ProjectSource {
        try await request(
            method: "source.add_note",
            params: [
                "project_id": .string(projectID),
                "collection": collection.map(JSONValue.string) ?? .null,
                "title": .string(title),
                "content": .string(content),
            ]
        )
        .decode(ProjectSource.self)
    }

    func addSourceFile(
        projectID: String,
        collection: String? = nil,
        path: String,
        title: String? = nil
    ) async throws -> ProjectSource {
        try await request(
            method: "source.add_file",
            params: [
                "project_id": .string(projectID),
                "collection": collection.map(JSONValue.string) ?? .null,
                "path": .string(path),
                "title": title.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(ProjectSource.self)
    }

    func addSourceURL(
        projectID: String,
        collection: String? = nil,
        url: String,
        title: String? = nil
    ) async throws -> ProjectSource {
        try await request(
            method: "source.add_url",
            params: [
                "project_id": .string(projectID),
                "collection": collection.map(JSONValue.string) ?? .null,
                "url": .string(url),
                "title": title.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(ProjectSource.self)
    }

    func importSources(
        projectID: String,
        collection: String? = nil,
        path: String? = nil,
        maxFiles: Int = 100
    ) async throws -> SourceImportResult {
        try await request(
            method: "source.import",
            params: [
                "project_id": .string(projectID),
                "collection": collection.map(JSONValue.string) ?? .null,
                "path": path.map(JSONValue.string) ?? .null,
                "max_files": .number(Double(maxFiles)),
            ]
        )
        .decode(SourceImportResult.self)
    }

    func deleteSource(sourceID: String) async throws -> SourceDeleteResult {
        try await request(
            method: "source.delete",
            params: ["source_id": .string(sourceID)]
        )
        .decode(SourceDeleteResult.self)
    }

    func refreshSource(sourceID: String) async throws -> SourceReindexResult {
        try await request(
            method: "source.refresh",
            params: ["source_id": .string(sourceID)]
        )
        .decode(SourceReindexResult.self)
    }

    func reindexSource(sourceID: String) async throws -> SourceReindexResult {
        try await request(
            method: "source.reindex",
            params: ["source_id": .string(sourceID)]
        )
        .decode(SourceReindexResult.self)
    }

    func searchSources(
        projectID: String,
        query: String,
        sourceIDs: [String] = [],
        collections: [String] = [],
        labels: [String] = [],
        limit: Int = 8,
        mode: SourceSearchMode? = nil
    ) async throws -> SourceSearchResult {
        try await request(
            method: "source.search",
            params: [
                "project_id": .string(projectID),
                "query": .string(query),
                "source_ids": .array(sourceIDs.map(JSONValue.string)),
                "collections": .array(collections.map(JSONValue.string)),
                "labels": .array(labels.map(JSONValue.string)),
                "limit": .number(Double(limit)),
                "mode": mode?.protocolValue.map { .string($0.rawValue) } ?? .null,
            ]
        )
        .decode(SourceSearchResult.self)
    }

    func sourceEmbeddingStatus(projectID: String) async throws -> SourceEmbeddingStatusResult {
        try await request(
            method: "source.embedding.status",
            params: ["project_id": .string(projectID)]
        )
        .decode(SourceEmbeddingStatusResult.self)
    }

    func reindexSourceEmbeddings(projectID: String) async throws -> SourceEmbeddingReindexResult {
        try await request(
            method: "source.embedding.reindex",
            params: ["project_id": .string(projectID)]
        )
        .decode(SourceEmbeddingReindexResult.self)
    }

    func sourceEmbeddingPolicy(projectID: String) async throws -> SourceEmbeddingPolicyResult {
        try await request(
            method: "source.embedding.policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(SourceEmbeddingPolicyResult.self)
    }

    func setSourceEmbeddingPolicy(
        projectID: String,
        policy: SourceEmbeddingPolicy
    ) async throws -> SourceEmbeddingPolicyResult {
        try await request(
            method: "source.embedding.policy.set",
            params: [
                "project_id": .string(projectID),
                "policy": .object(policy.jsonObject()),
            ]
        )
        .decode(SourceEmbeddingPolicyResult.self)
    }

    func sourceRetrievalPolicy(projectID: String) async throws -> SourceRetrievalPolicyResult {
        try await request(
            method: "source.retrieval.policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(SourceRetrievalPolicyResult.self)
    }

    func setSourceRetrievalPolicy(
        projectID: String,
        policy: SourceRetrievalPolicy
    ) async throws -> SourceRetrievalPolicyResult {
        try await request(
            method: "source.retrieval.policy.set",
            params: [
                "project_id": .string(projectID),
                "policy": .object(policy.jsonObject()),
            ]
        )
        .decode(SourceRetrievalPolicyResult.self)
    }

    func sourceIndexStatus(projectID: String) async throws -> SourceIndexStatusResult {
        try await request(
            method: "source.embedding.index_status",
            params: ["project_id": .string(projectID)]
        )
        .decode(SourceIndexStatusResult.self)
    }

    func rebuildSourceIndex(projectID: String) async throws -> SourceIndexRebuildResult {
        try await request(
            method: "source.embedding.index_rebuild",
            params: ["project_id": .string(projectID)]
        )
        .decode(SourceIndexRebuildResult.self)
    }

    func sourceCollections(projectID: String) async throws -> SourceCollectionsResult {
        try await request(
            method: "source.collections",
            params: ["project_id": .string(projectID)]
        )
        .decode(SourceCollectionsResult.self)
    }

    func updateSourceMetadata(
        sourceID: String,
        title: String,
        collection: String?,
        labels: [String]
    ) async throws -> ProjectSource {
        try await request(
            method: "source.update",
            params: [
                "source_id": .string(sourceID),
                "title": .string(title),
                "collection": collection.map(JSONValue.string) ?? .null,
                "labels": .array(labels.map(JSONValue.string)),
            ]
        )
        .decode(ProjectSource.self)
    }

    func setSourceCollection(
        sourceID: String,
        collection: String?
    ) async throws -> ProjectSource {
        try await request(
            method: "source.set_collection",
            params: [
                "source_id": .string(sourceID),
                "collection": collection.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(ProjectSource.self)
    }

    func setSourceLabels(sourceID: String, labels: [String]) async throws -> ProjectSource {
        try await request(
            method: "source.set_labels",
            params: [
                "source_id": .string(sourceID),
                "labels": .array(labels.map(JSONValue.string)),
            ]
        )
        .decode(ProjectSource.self)
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

    func steerSession(
        sessionID: String,
        body: String,
        agentID: String?,
        taskID: String?
    ) async throws -> SessionSteerResult {
        try await request(
            method: "session.steer",
            params: [
                "session_id": .string(sessionID),
                "body": .string(body),
                "agent_id": agentID.map(JSONValue.string) ?? .null,
                "task_id": taskID.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(SessionSteerResult.self)
    }

    func sessionSteering(
        sessionID: String,
        status: String?,
        limit: Int?
    ) async throws -> SessionSteeringResult {
        try await request(
            method: "session.steering",
            params: [
                "session_id": .string(sessionID),
                "status": status.map(JSONValue.string) ?? .null,
                "limit": limit.map { .number(Double($0)) } ?? .null,
            ]
        )
        .decode(SessionSteeringResult.self)
    }

    func cancelSteering(id: String) async throws -> SessionSteeringCancelResult {
        try await request(
            method: "session.steering.cancel",
            params: ["id": .string(id)]
        )
        .decode(SessionSteeringCancelResult.self)
    }

    func deferSession(
        sessionID: String,
        body: String,
        watchTaskIDs: [String],
        watchMode: String,
        afterSecs: UInt64?,
        deadlineAt: String?
    ) async throws -> SessionDeferResult {
        try await request(
            method: "session.defer",
            params: [
                "session_id": .string(sessionID),
                "body": .string(body),
                "watch_task_ids": .array(watchTaskIDs.map(JSONValue.string)),
                "watch_mode": .string(watchMode),
                "after_secs": afterSecs.map { .number(Double($0)) } ?? .null,
                "deadline_at": deadlineAt.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(SessionDeferResult.self)
    }

    func sessionContinuations(
        sessionID: String,
        status: String?,
        limit: Int?
    ) async throws -> SessionContinuationsResult {
        try await request(
            method: "session.continuations",
            params: [
                "session_id": .string(sessionID),
                "status": status.map(JSONValue.string) ?? .null,
                "limit": limit.map { .number(Double($0)) } ?? .null,
            ]
        )
        .decode(SessionContinuationsResult.self)
    }

    func cancelContinuation(id: String) async throws -> SessionContinuationCancelResult {
        try await request(
            method: "session.continuation.cancel",
            params: ["id": .string(id)]
        )
        .decode(SessionContinuationCancelResult.self)
    }

    func orchestrationRecoveries(
        sessionID: String
    ) async throws -> OrchestrationRecoveriesResult {
        try await request(
            method: "orchestration.recoveries",
            params: ["session_id": .string(sessionID)]
        )
        .decode(OrchestrationRecoveriesResult.self)
    }

    func sessionStalls(sessionID: String) async throws -> SessionStallsResult {
        try await request(
            method: "session.stalls",
            params: ["session_id": .string(sessionID)]
        )
        .decode(SessionStallsResult.self)
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

    func listProcesses(
        sessionID: String,
        includeFinished: Bool = true,
        limit: UInt32 = 50
    ) async throws -> ProcessListResult {
        try await request(
            method: "session.processes",
            params: [
                "session_id": .string(sessionID),
                "include_finished": .bool(includeFinished),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(ProcessListResult.self)
    }

    func processLogs(
        sessionID: String,
        processID: String,
        offset: UInt64 = 0,
        maxBytes: UInt32 = 65536,
        maxLines: UInt32? = nil,
        fromEnd: Bool = false
    ) async throws -> ProcessLogsResult {
        var params: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "process_id": .string(processID),
            "offset": .number(Double(offset)),
            "max_bytes": .number(Double(maxBytes)),
            "from_end": .bool(fromEnd),
        ]
        if let maxLines {
            params["max_lines"] = .number(Double(maxLines))
        }
        return try await request(method: "session.process.logs", params: params)
            .decode(ProcessLogsResult.self)
    }

    func stopProcess(
        sessionID: String,
        processID: String,
        signal: String = "TERM",
        graceSecs: UInt32 = 5
    ) async throws -> ProcessStopResult {
        try await request(
            method: "session.process.stop",
            params: [
                "session_id": .string(sessionID),
                "process_id": .string(processID),
                "signal": .string(signal),
                "grace_secs": .number(Double(graceSecs)),
            ]
        )
        .decode(ProcessStopResult.self)
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
        status: String? = nil,
        includeStale: Bool = false,
        limit: Int = 50
    ) async throws -> [MemoryItem] {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "query": query.map(JSONValue.string) ?? .null,
            "kind": kind.map(JSONValue.string) ?? .null,
            "status": status.map(JSONValue.string) ?? .null,
            "include_stale": .bool(includeStale),
            "limit": .number(Double(limit)),
        ]
        return try await request(method: "memory.list", params: params)
            .decode(MemoryListResult.self)
            .memories
    }

    func memoryUpdate(
        id: String,
        status: String?,
        pinned: Bool?,
        tags: [String]?
    ) async throws -> MemoryItem {
        let params: [String: JSONValue] = [
            "id": .string(id),
            "status": status.map(JSONValue.string) ?? .null,
            "pinned": pinned.map(JSONValue.bool) ?? .null,
            "tags": tags.map { .array($0.map(JSONValue.string)) } ?? .null,
        ]
        return try await request(method: "memory.update", params: params)
            .decode(MemoryUpdateResult.self)
            .memory
    }

    func memoryVerify(id: String) async throws -> MemoryVerifyResult {
        try await request(method: "memory.verify", params: ["id": .string(id)])
            .decode(MemoryVerifyResult.self)
    }

    func memorySweep(
        projectID: String,
        olderThanDays: Int?,
        dryRun: Bool
    ) async throws -> MemorySweepResult {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "older_than_days": olderThanDays.map { .number(Double($0)) } ?? .null,
            "dry_run": .bool(dryRun),
        ]
        return try await request(method: "memory.sweep", params: params)
            .decode(MemorySweepResult.self)
    }

    func memoryStats(projectID: String) async throws -> MemoryStatsResult {
        try await request(
            method: "memory.stats",
            params: ["project_id": .string(projectID)]
        )
        .decode(MemoryStatsResult.self)
    }

    func memoryConflicts(
        projectID: String,
        status: String?,
        limit: Int
    ) async throws -> [MemoryConflict] {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "status": status.map(JSONValue.string) ?? .null,
            "limit": .number(Double(limit)),
        ]
        return try await request(method: "memory.conflicts", params: params)
            .decode(MemoryConflictsResult.self)
            .conflicts
    }

    func scanMemoryConflicts(projectID: String) async throws -> MemoryConflictScanResult {
        try await request(
            method: "memory.conflict.scan",
            params: ["project_id": .string(projectID)]
        )
        .decode(MemoryConflictScanResult.self)
    }

    func resolveMemoryConflict(
        id: String,
        action: MemoryConflictResolution,
        note: String?
    ) async throws -> MemoryConflictResolveResult {
        let params: [String: JSONValue] = [
            "id": .string(id),
            "action": .string(action.rawValue),
            "note": note.map(JSONValue.string) ?? .null,
        ]
        return try await request(method: "memory.conflict.resolve", params: params)
            .decode(MemoryConflictResolveResult.self)
    }

    func memoryHealth(projectID: String) async throws -> MemoryHealthResult {
        try await request(
            method: "memory.health",
            params: ["project_id": .string(projectID)]
        )
        .decode(MemoryHealthResult.self)
    }

    func memoryEmbeddingsStatus(projectID: String) async throws -> MemoryEmbeddingsStatusResult {
        try await request(
            method: "memory.embeddings.status",
            params: ["project_id": .string(projectID)]
        )
        .decode(MemoryEmbeddingsStatusResult.self)
    }

    func rebuildMemoryEmbeddings(projectID: String) async throws -> MemoryEmbeddingsRebuildResult {
        try await request(
            method: "memory.embeddings.rebuild",
            params: ["project_id": .string(projectID)]
        )
        .decode(MemoryEmbeddingsRebuildResult.self)
    }

    func testMemoryEmbeddings(
        projectID: String,
        text: String?
    ) async throws -> MemoryEmbeddingsTestResult {
        try await request(
            method: "memory.embeddings.test",
            params: [
                "project_id": .string(projectID),
                "text": text.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(MemoryEmbeddingsTestResult.self)
    }

    func memoryEmbeddingIndexStatus(
        projectID: String
    ) async throws -> MemoryEmbeddingsIndexStatusResult {
        try await request(
            method: "memory.embeddings.index.status",
            params: ["project_id": .string(projectID)]
        )
        .decode(MemoryEmbeddingsIndexStatusResult.self)
    }

    func rebuildMemoryEmbeddingIndex(
        projectID: String
    ) async throws -> MemoryEmbeddingsIndexRebuildResult {
        try await request(
            method: "memory.embeddings.index.rebuild",
            params: ["project_id": .string(projectID)]
        )
        .decode(MemoryEmbeddingsIndexRebuildResult.self)
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
        policy: MemoryPolicy
    ) async throws -> MemoryPolicy {
        // Send every field: the daemon replaces the whole policy object, and
        // omitting v2 fields would silently reset verification TTLs or the
        // staleness sweep whenever retention is saved.
        let body: [String: JSONValue] = [
            "enabled": .bool(policy.enabled),
            "max_items": policy.maxItems.map { .number(Double($0)) } ?? .null,
            "max_age_days": policy.maxAgeDays.map { .number(Double($0)) } ?? .null,
            "task_result_verify_days": policy.taskResultVerifyDays.map { .number(Double($0)) }
                ?? .number(90),
            "default_verify_days": policy.defaultVerifyDays.map { .number(Double($0)) }
                ?? .number(365),
            "auto_supersede": .bool(policy.autoSupersede ?? true),
            "brief_max_chars": policy.briefMaxChars.map { .number(Double($0)) } ?? .number(4000),
            "sweep_stale": .bool(policy.sweepStale ?? false),
            "embedding": policy.embedding.map { .object($0.jsonObject()) } ?? .null,
        ]
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "policy": .object(body),
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

    func preemptSession(sessionID: String, reason: String? = nil) async throws -> SessionPreemptResult {
        var params: [String: JSONValue] = ["session_id": .string(sessionID)]
        if let reason, !reason.isEmpty {
            params["reason"] = .string(reason)
        }
        return try await request(method: "session.preempt", params: params)
            .decode(SessionPreemptResult.self)
    }

    func sessionPreemption(sessionID: String, limit: Int = 100) async throws -> SessionPreemptionResult {
        try await request(
            method: "session.preemption",
            params: [
                "session_id": .string(sessionID),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(SessionPreemptionResult.self)
    }

    func preemptionOverview(projectID: String, limit: Int = 100) async throws -> PreemptionOverviewResult {
        try await request(
            method: "preemption.overview",
            params: [
                "project_id": .string(projectID),
                "limit": .number(Double(limit)),
            ]
        )
        .decode(PreemptionOverviewResult.self)
    }

    func preemptionPolicy(projectID: String) async throws -> PreemptionPolicy {
        try await request(
            method: "project.preemption_policy.get",
            params: ["project_id": .string(projectID)]
        )
        .decode(PreemptionPolicyResult.self)
        .policy
    }

    func setPreemptionPolicy(projectID: String, policy: PreemptionPolicy) async throws -> PreemptionPolicy {
        var object: [String: JSONValue] = [
            "enabled": .bool(policy.enabled),
            "provider_grace_ms": .number(Double(policy.providerGraceMs)),
            "tool_grace_ms": .number(Double(policy.toolGraceMs)),
            "kill_escalation_ms": .number(Double(policy.killEscalationMs)),
            "checkpoint_on_preempt": .bool(policy.checkpointOnPreempt),
        ]
        if let deadline = policy.toolDeadlineSecs {
            object["tool_deadline_secs"] = .number(Double(deadline))
        } else {
            object["tool_deadline_secs"] = .null
        }
        return try await request(
            method: "project.preemption_policy.set",
            params: [
                "project_id": .string(projectID),
                "policy": .object(object),
            ]
        )
        .decode(PreemptionPolicyResult.self)
        .policy
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

    func approvalQueue(sessionID: String?, projectID: String?) async throws -> ApprovalQueueResult {
        try await request(
            method: "approval.queue",
            params: [
                "session_id": sessionID.map(JSONValue.string) ?? .null,
                "project_id": projectID.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(ApprovalQueueResult.self)
    }

    func resolveApprovals(_ decisions: [(String, Bool)]) async throws -> ApprovalBatchResolveResult {
        let payload = decisions.map { id, approved in
            JSONValue.object([
                "approval_id": .string(id),
                "approved": .bool(approved),
            ])
        }
        return try await request(
            method: "approval.batch.resolve",
            params: ["decisions": .array(payload)]
        )
        .decode(ApprovalBatchResolveResult.self)
    }

    func listApprovalTemplates(projectID: String) async throws -> [ApprovalTemplate] {
        try await request(
            method: "approval.templates",
            params: ["project_id": .string(projectID)]
        )
        .decode(ApprovalTemplateListResult.self)
        .templates
    }

    func createApprovalTemplate(
        projectID: String,
        name: String,
        toolPattern: String,
        riskLevels: [String],
        pathGlobs: [String],
        action: String,
        expiresAt: String?,
        maxUses: Int?
    ) async throws -> ApprovalTemplate {
        try await request(
            method: "approval.template.create",
            params: [
                "project_id": .string(projectID),
                "name": .string(name),
                "tool_pattern": .string(toolPattern),
                "risk_levels": .array(riskLevels.map(JSONValue.string)),
                "path_globs": .array(pathGlobs.map(JSONValue.string)),
                "action": .string(action),
                "expires_at": expiresAt.map(JSONValue.string) ?? .null,
                "max_uses": maxUses.map { .number(Double($0)) } ?? .null,
                "created_by": .string("operator"),
            ]
        )
        .decode(ApprovalTemplateResult.self)
        .template
    }

    func updateApprovalTemplate(
        id: String,
        name: String,
        enabled: Bool,
        toolPattern: String,
        riskLevels: [String],
        pathGlobs: [String],
        action: String,
        expiresAt: String?,
        maxUses: Int?
    ) async throws -> ApprovalTemplate {
        try await request(
            method: "approval.template.update",
            params: [
                "id": .string(id),
                "name": .string(name),
                "enabled": .bool(enabled),
                "tool_pattern": .string(toolPattern),
                "risk_levels": .array(riskLevels.map(JSONValue.string)),
                "path_globs": .array(pathGlobs.map(JSONValue.string)),
                "action": .string(action),
                "expires_at": expiresAt.map(JSONValue.string) ?? .null,
                "max_uses": maxUses.map { .number(Double($0)) } ?? .null,
            ]
        )
        .decode(ApprovalTemplateResult.self)
        .template
    }

    func deleteApprovalTemplate(id: String) async throws -> Bool {
        try await request(
            method: "approval.template.delete",
            params: ["id": .string(id)]
        )
        .decode(ApprovalTemplateDeleteResult.self)
        .deleted
    }

    func createApprovalTemplateFromApproval(
        approvalID: String,
        name: String?,
        maxUses: Int,
        expiresAt: String?
    ) async throws -> ApprovalTemplate {
        try await request(
            method: "approval.template.from_approval",
            params: [
                "approval_id": .string(approvalID),
                "name": name.map(JSONValue.string) ?? .null,
                "max_uses": .number(Double(maxUses)),
                "expires_at": expiresAt.map(JSONValue.string) ?? .null,
            ]
        )
        .decode(ApprovalTemplateResult.self)
        .template
    }

    func attentionPolicy(projectID: String) async throws -> ApprovalPolicyResult {
        try await request(
            method: "approval.policy",
            params: ["project_id": .string(projectID)]
        )
        .decode(ApprovalPolicyResult.self)
    }

    func setAttentionPolicy(
        projectID: String,
        policy: AttentionPolicy
    ) async throws -> ApprovalPolicyResult {
        try await request(
            method: "approval.policy.set",
            params: [
                "project_id": .string(projectID),
                "policy": .object(policy.jsonObject),
            ]
        )
        .decode(ApprovalPolicyResult.self)
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
            if let price = role.cacheReadUsdPerMillion, price > 0 {
                object["cache_read_usd_per_million"] = .number(price)
            }
            if let price = role.cacheWriteUsdPerMillion, price > 0 {
                object["cache_write_usd_per_million"] = .number(price)
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
                    throwing: CoreError.rpc(
                        code: error.code,
                        message: error.message,
                        data: error.data
                    )
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
