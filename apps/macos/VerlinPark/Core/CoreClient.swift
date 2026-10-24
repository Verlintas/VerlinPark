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

    func createSession(projectID: String, goal: String) async throws -> Session {
        let params: [String: JSONValue] = [
            "project_id": .string(projectID),
            "goal": .string(goal),
        ]
        return try await request(method: "session.create", params: params)
            .decode(Session.self)
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
        apiKey: String
    ) async throws -> AgentInfo {
        var params: [String: JSONValue] = [
            "provider": .string(kind),
            "base_url": .string(baseURL),
            "model": .string(model),
        ]
        if !apiKey.isEmpty {
            params["api_key"] = .string(apiKey)
        }
        return try await request(method: "agent.configure", params: params)
            .decode(AgentInfo.self)
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
