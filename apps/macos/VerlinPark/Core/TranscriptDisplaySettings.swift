import Foundation

/// Display-only transcript preference. It never changes the event payload or
/// the model's stored text.
enum TranscriptDisplayFormat: String, CaseIterable, Identifiable, Sendable {
    case plainText = "plain_text"
    case markdown

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .plainText: L10n.t("Plain text")
        case .markdown: L10n.t("Markdown")
        }
    }

    var detail: String {
        switch self {
        case .plainText: L10n.t("Exact characters and line breaks")
        case .markdown: L10n.t("Headings, lists, code, links, and tables")
        }
    }

    var icon: String {
        switch self {
        case .plainText: "text.alignleft"
        case .markdown: "textformat"
        }
    }
}

enum TranscriptDisplayPreference {
    static let userFormatKey = "verlinpark.transcript.userFormat"
    static let agentFormatKey = "verlinpark.transcript.agentFormat"
    static let defaultUserFormat: TranscriptDisplayFormat = .plainText
    static let defaultAgentFormat: TranscriptDisplayFormat = .markdown

    /// Unknown or legacy values fall back to the documented default instead of
    /// crashing or silently changing the other transcript side.
    static func format(
        rawValue: String,
        fallback: TranscriptDisplayFormat
    ) -> TranscriptDisplayFormat {
        TranscriptDisplayFormat(rawValue: rawValue) ?? fallback
    }
}
