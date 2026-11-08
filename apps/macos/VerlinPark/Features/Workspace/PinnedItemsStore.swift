import Foundation
import Observation

@MainActor
@Observable
final class PinnedItemsStore {
    static let sessionsKey = "verlinpark.pinned.sessions"
    static let projectsKey = "verlinpark.pinned.projects"

    private(set) var sessionIDs: [String]
    private(set) var projectIDs: [String]
    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        sessionIDs = Self.deduplicated(defaults.stringArray(forKey: Self.sessionsKey) ?? [])
        projectIDs = Self.deduplicated(defaults.stringArray(forKey: Self.projectsKey) ?? [])
    }

    func isPinnedSession(_ id: String) -> Bool {
        sessionIDs.contains(id)
    }

    func isPinnedProject(_ id: String) -> Bool {
        projectIDs.contains(id)
    }

    func toggleSession(_ id: String) {
        if let index = sessionIDs.firstIndex(of: id) {
            sessionIDs.remove(at: index)
        } else {
            sessionIDs.insert(id, at: 0)
        }
        save()
    }

    func toggleProject(_ id: String) {
        if let index = projectIDs.firstIndex(of: id) {
            projectIDs.remove(at: index)
        } else {
            projectIDs.insert(id, at: 0)
        }
        save()
    }

    func prune(validSessionIDs: Set<String>, validProjectIDs: Set<String>) {
        let sessions = sessionIDs.filter { validSessionIDs.contains($0) }
        let projects = projectIDs.filter { validProjectIDs.contains($0) }
        guard sessions != sessionIDs || projects != projectIDs else { return }
        sessionIDs = sessions
        projectIDs = projects
        save()
    }

    private func save() {
        defaults.set(sessionIDs, forKey: Self.sessionsKey)
        defaults.set(projectIDs, forKey: Self.projectsKey)
    }

    private static func deduplicated(_ ids: [String]) -> [String] {
        var seen: Set<String> = []
        return ids.filter { seen.insert($0).inserted }
    }
}
