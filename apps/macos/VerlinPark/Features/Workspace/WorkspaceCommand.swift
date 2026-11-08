import Foundation
import Observation

enum WorkspaceCommand: String, CaseIterable, Identifiable, Sendable {
    case commandPalette
    case newSession
    case search
    case dashboard
    case notifications
    case shortcuts
    case focusMode
    case inspector
    case resetLayout
    case addProject
    case refresh
    case toggleLanguage

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .commandPalette: L10n.t("Command Palette")
        case .newSession: L10n.t("New Session")
        case .search: L10n.t("Search Messages & Events")
        case .dashboard: L10n.t("Manage Dash")
        case .notifications: L10n.t("Notifications")
        case .shortcuts: L10n.t("Keyboard Shortcuts")
        case .focusMode: L10n.t("Toggle Focus Mode")
        case .inspector: L10n.t("Toggle Inspector")
        case .resetLayout: L10n.t("Reset Window Layout")
        case .addProject: L10n.t("Add Project")
        case .refresh: L10n.t("Refresh")
        case .toggleLanguage: L10n.t("Switch Language")
        }
    }
}

/// A small intents bus for the app menu. SwiftUI commands live in `App` while
/// sheet and focus state live in `RootView`; routing through one observable
/// keeps the menu, keyboard shortcuts, and command palette on the same path.
@MainActor
@Observable
final class WorkspaceCommandRouter {
    private(set) var sequence = 0
    private(set) var lastCommand: WorkspaceCommand?

    func send(_ command: WorkspaceCommand) {
        lastCommand = command
        sequence += 1
    }
}
