import Foundation

enum ToolActivityFilter: String, CaseIterable, Identifiable {
    case all
    case errors
    case retryable

    var id: String {
        rawValue
    }

    var label: String {
        switch self {
        case .all: "All"
        case .errors: "Errors"
        case .retryable: "Retryable"
        }
    }
}

/// Pure presentation helpers for the bounded tool-activity surface. The view
/// reads model state; this type only formats values already returned by the
/// daemon and is unit-tested directly.
enum ToolActivityMetrics {
    /// Most rows displayed by the dashboard card. The RPC can return more; the
    /// full bounded window remains available to a future detail sheet.
    static let dashboardRowLimit = 8

    static func statusLabel(_ entry: ToolActivityEntry) -> String {
        guard entry.isError else { return "ok" }
        let code = entry.errorCode?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return code.isEmpty ? "TOOL_FAILED" : code
    }

    static func retryLabel(_ entry: ToolActivityEntry) -> String? {
        guard entry.isError else { return nil }
        return entry.retryable ? "retryable" : "not retryable"
    }

    static func durationLabel(_ milliseconds: Int) -> String {
        let value = max(0, milliseconds)
        if value < 1000 {
            return "\(value)ms"
        }
        if value < 60000 {
            return String(format: "%.1fs", Double(value) / 1000)
        }
        let minutes = value / 60000
        let seconds = (value % 60000) / 1000
        return "\(minutes)m \(seconds)s"
    }

    static func byteLabel(_ bytes: Int) -> String {
        let value = max(0, bytes)
        if value < 1024 {
            return "\(value) B"
        }
        if value < 1024 * 1024 {
            return String(format: "%.1f KB", Double(value) / 1024)
        }
        return String(format: "%.1f MB", Double(value) / (1024 * 1024))
    }

    static func summaryLine(_ summary: ToolActivitySummary) -> String {
        "\(summary.calls) calls · \(summary.errors) errors · avg "
            + durationLabel(summary.averageDurationMs)
            + " · max " + durationLabel(summary.maxDurationMs)
    }

    static func errorLine(_ entry: ToolActivityEntry) -> String? {
        guard entry.isError else { return nil }
        var parts = [statusLabel(entry)]
        if let retry = retryLabel(entry) {
            parts.append(retry)
        }
        if let hint = entry.hint?.trimmingCharacters(in: .whitespacesAndNewlines),
           !hint.isEmpty
        {
            parts.append(hint)
        }
        return parts.joined(separator: " · ")
    }

    /// Rows are returned newest-first by the daemon. The card adds a stable
    /// ordering on `seq` so a test or a future merged stream cannot reorder
    /// equal timestamps unexpectedly.
    static func dashboardRows(_ result: ToolActivityResult?) -> [ToolActivityEntry] {
        guard let result else { return [] }
        return Array(result.entries.sorted { $0.seq > $1.seq }.prefix(dashboardRowLimit))
    }

    static func isRetryableFailure(_ entry: ToolActivityEntry) -> Bool {
        entry.isError && entry.retryable
    }

    /// Tool names in stable alphabetical order for the inspector filter.
    static func toolNames(_ result: ToolActivityResult?) -> [String] {
        guard let result else { return [] }
        return Array(Set(result.entries.map(\.name))).sorted()
    }

    /// Newest-first filter used by the full inspector. Search is a
    /// case-insensitive literal match over the tool name, stable code, and
    /// hint; it never searches provider output because the RPC does not return
    /// it.
    static func filtered(
        _ result: ToolActivityResult?,
        filter: ToolActivityFilter,
        search: String,
        tool: String?
    ) -> [ToolActivityEntry] {
        guard let result else { return [] }
        let needle = search.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return result.entries
            .filter { entry in
                switch filter {
                case .all:
                    break
                case .errors:
                    guard entry.isError else { return false }
                case .retryable:
                    guard isRetryableFailure(entry) else { return false }
                }
                if let tool, !tool.isEmpty, entry.name != tool {
                    return false
                }
                guard !needle.isEmpty else { return true }
                let haystack = [
                    entry.name,
                    entry.errorCode ?? "",
                    entry.hint ?? "",
                    entry.approvalRisk ?? "",
                    entry.approvalMode ?? "",
                ]
                .joined(separator: " ")
                .lowercased()
                return haystack.contains(needle)
            }
            .sorted { $0.seq > $1.seq }
    }

    /// Recompute a summary for the filtered window. The daemon summary covers
    /// the unfiltered RPC window; the inspector labels this as a local view.
    static func summary(for entries: [ToolActivityEntry]) -> ToolActivitySummary {
        let calls = entries.count
        let errors = entries.count(where: \.isError)
        let retryable = entries.count(where: \.retryable)
        let truncated = entries.count(where: \.truncated)
        let totalDuration = entries.reduce(0) { $0 + max(0, $1.durationMs) }
        let outputBytes = entries.reduce(0) { $0 + max(0, $1.outputBytes) }
        var byTool: [String: ToolActivityToolStat] = [:]
        var byCode: [String: Int] = [:]
        var approvalWaits = 0
        var totalApprovalWait = 0
        var maxApprovalWait = 0
        for entry in entries {
            let existing = byTool[entry.name]
            byTool[entry.name] = ToolActivityToolStat(
                name: entry.name,
                calls: (existing?.calls ?? 0) + 1,
                errors: (existing?.errors ?? 0) + (entry.isError ? 1 : 0),
                totalDurationMs: (existing?.totalDurationMs ?? 0) + max(0, entry.durationMs),
                outputBytes: (existing?.outputBytes ?? 0) + max(0, entry.outputBytes)
            )
            if entry.isError, let code = entry.errorCode, !code.isEmpty {
                byCode[code, default: 0] += 1
            }
            if let wait = entry.approvalWaitMs {
                approvalWaits += 1
                totalApprovalWait += max(0, wait)
                maxApprovalWait = max(maxApprovalWait, max(0, wait))
            }
        }
        return ToolActivitySummary(
            calls: calls,
            errors: errors,
            retryableErrors: retryable,
            truncatedResults: truncated,
            totalDurationMs: totalDuration,
            maxDurationMs: entries.map(\.durationMs).max() ?? 0,
            averageDurationMs: calls == 0 ? 0 : totalDuration / calls,
            outputBytes: outputBytes,
            approvalWaits: approvalWaits,
            totalApprovalWaitMs: totalApprovalWait,
            maxApprovalWaitMs: maxApprovalWait,
            byTool: byTool.values.sorted {
                if $0.calls != $1.calls {
                    return $0.calls > $1.calls
                }
                if $0.totalDurationMs != $1.totalDurationMs {
                    return $0.totalDurationMs > $1.totalDurationMs
                }
                return $0.name < $1.name
            },
            errorCodes: byCode
                .map { ToolActivityCodeStat(code: $0.key, calls: $0.value) }
                .sorted {
                    $0.calls == $1.calls ? $0.code < $1.code : $0.calls > $1.calls
                }
        )
    }
}
