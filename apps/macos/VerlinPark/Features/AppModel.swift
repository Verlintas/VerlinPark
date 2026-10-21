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

    var connection: ConnectionState = .idle
    var projects: [Project] = []
    var sessions: [Session] = []
    var events: [Event] = []
    var selectedProjectID: String?
    var selectedSessionID: String?
    var isBusy = false
    var lastError: String?

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
        } catch {
            handle(error)
        }
    }

    func selectProject(_ id: String?) {
        guard selectedProjectID != id else { return }
        selectedProjectID = id
        selectedSessionID = nil
        events = []
        Task { await loadSessions() }
    }

    func selectSession(_ id: String?) {
        guard selectedSessionID != id else { return }
        selectedSessionID = id
        Task { await loadEvents() }
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
            await loadSessions()
        } catch {
            handle(error)
        }
    }

    func createSession(goal: String) async {
        guard let projectID = selectedProjectID else {
            lastError = "Choose a project first."
            return
        }
        let trimmed = goal.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }

        isBusy = true
        defer { isBusy = false }
        do {
            let session = try await core.createSession(projectID: projectID, goal: trimmed)
            sessions = try await core.listSessions(projectID: projectID)
            selectedSessionID = session.id
            events = try await core.listEvents(sessionID: session.id)
        } catch {
            handle(error)
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
        } catch {
            handle(error)
        }
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
        if event.type == "session.created" {
            Task { await self.loadSessions() }
        }
    }

    private func handle(_ error: Error) {
        lastError = error.localizedDescription
        if case .connecting = connection {
            connection = .failed(error.localizedDescription)
        }
    }
}
