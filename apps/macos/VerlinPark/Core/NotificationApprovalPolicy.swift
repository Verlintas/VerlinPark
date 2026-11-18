import Foundation

/// Notification approval is deliberately narrower than the in-app approval
/// card. Only routine workspace writes can be resolved without showing risk,
/// tool, and target. Destructive, host-state, external/remote, MCP, and
/// sensitive GUI requests must route to the app.
enum NotificationApprovalPolicy {
    static let inlineCategory = "verlinpark.approval"
    static let reviewCategory = "verlinpark.approval.review"

    static func allowsInlineResolution(risk: String?) -> Bool {
        let normalized = risk?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased() ?? ""
        return normalized == "workspace_write"
    }

    static func categoryIdentifier(risk: String?) -> String {
        allowsInlineResolution(risk: risk) ? inlineCategory : reviewCategory
    }

    /// Human-readable risk label used in the notification body. Unknown or
    /// missing risk is treated as requiring review.
    static func riskLabel(risk: String?) -> String {
        let normalized = risk?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased() ?? ""
        switch normalized {
        case "workspace_write": return "workspace write"
        case "destructive": return "destructive"
        case "host_state": return "host state"
        case "external": return "external"
        case "": return "unclassified risk"
        default: return normalized.replacingOccurrences(of: "_", with: " ")
        }
    }
}
