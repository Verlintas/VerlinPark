import Foundation

/// Mirrors the daemon's `ApprovalMode` wire values.
///
/// The daemon owns the actual decision. This enum exists so the UI can explain
/// and persist the selected policy without duplicating command classification.
enum AgentApprovalMode: String, CaseIterable, Identifiable, Codable, Sendable {
    case smart
    case strict
    case unattended

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .smart: L10n.t("Smart")
        case .strict: L10n.t("Ask more")
        case .unattended: L10n.t("Unattended")
        }
    }

    var detail: String {
        switch self {
        case .smart:
            L10n.t("Run workspace edits and ordinary commands. Ask only before host-level changes.")
        case .strict:
            L10n.t("Ask before every file write, shell command, MCP call, or external agent tool.")
        case .unattended:
            L10n.t("Never block on a person. Host-level changes are denied automatically.")
        }
    }

    var icon: String {
        switch self {
        case .smart: "shield.lefthalf.filled"
        case .strict: "hand.raised"
        case .unattended: "moon.zzz"
        }
    }

    static var stored: AgentApprovalMode {
        guard
            let raw = UserDefaults.standard.string(forKey: "verlinpark.approval.mode"),
            let mode = AgentApprovalMode(rawValue: raw)
        else {
            return .smart
        }
        return mode
    }

    func store() {
        UserDefaults.standard.set(rawValue, forKey: "verlinpark.approval.mode")
    }
}
