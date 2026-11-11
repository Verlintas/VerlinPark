import Foundation

/// Presentation-only summaries for tool calls in the transcript.
///
/// The runtime is the authority on permissions and execution. This helper only
/// turns a structured input object into a short, readable line so the
/// transcript can stay dense without hiding what the agent is doing.
enum ToolTrace {
    static func summary(name: String, input: JSONValue) -> String {
        switch name {
        case "read_file", "write_file", "edit_file", "list_dir", "workspace_read", "file_info":
            return string(input, "path") ?? ""
        case "read_files":
            let count = arrayCount(input, "files")
            return "\(count) file\(count == 1 ? "" : "s")"
        case "grep":
            let pattern = string(input, "pattern") ?? ""
            let path = string(input, "path") ?? "."
            return pattern.isEmpty ? path : "\(pattern) · \(path)"
        case "find_files":
            let pattern = string(input, "pattern") ?? "**/*"
            let path = string(input, "path") ?? "."
            return "\(pattern) · \(path)"
        case "apply_patch":
            return "unified patch"
        case "file_manage":
            let action = string(input, "action") ?? "manage"
            let source = string(input, "source") ?? ""
            let destination = string(input, "destination") ?? ""
            return [action, source, destination]
                .filter { !$0.isEmpty }
                .joined(separator: " · ")
        case "project_profile":
            return "detected commands"
        case "run_project_task":
            let kind = string(input, "kind") ?? "task"
            let package = string(input, "package") ?? ""
            let filter = string(input, "filter") ?? ""
            return [kind, package, filter].filter { !$0.isEmpty }.joined(separator: " · ")
        case "git":
            let action = string(input, "action") ?? "status"
            let path = string(input, "path") ?? ""
            return path.isEmpty ? action : "\(action) · \(path)"
        case "lsp_query":
            let action = string(input, "action") ?? "symbols"
            let path = string(input, "path") ?? string(input, "language") ?? ""
            return path.isEmpty ? action : "\(action) · \(path)"
        case "find_symbol":
            let query = string(input, "query") ?? "symbol"
            let source = string(input, "source") ?? ""
            return source.isEmpty ? query : "\(query) · \(source)"
        case "find_references":
            let target = string(input, "name") ?? string(input, "symbol_id") ?? "references"
            let source = string(input, "source") ?? ""
            return source.isEmpty ? target : "\(target) · \(source)"
        case "repo_map":
            let path = string(input, "path") ?? "."
            return "\(path) · \(string(input, "source") ?? "lexical")"
        case "build_context":
            let goal = string(input, "goal") ?? "context"
            return "\(goal) · \(string(input, "source") ?? "lexical")"
        case "code_diagnostics":
            let severity = string(input, "severity") ?? "error"
            let source = string(input, "source") ?? "auto"
            let paths = arrayCount(input, "paths")
            let detail = paths == 0 ? "" : " · \(paths) paths"
            return "\(severity) · \(source)\(detail)"
        case "web_search":
            return string(input, "query") ?? "web search"
        case "read_pdf", "read_image":
            return string(input, "path") ?? ""
        case "read_document":
            let path = string(input, "path") ?? ""
            let filter = string(input, "sheets") ?? string(input, "pages") ?? ""
            return filter.isEmpty ? path : "\(path) · \(filter)"
        case "data_query":
            let path = string(input, "path") ?? ""
            let query = string(input, "query") ?? ""
            return query.isEmpty ? path : "\(path) · \(query)"
        case "browser":
            let action = string(input, "action") ?? "snapshot"
            let detail = string(input, "url")
                ?? string(input, "ref")
                ?? string(input, "selector")
                ?? string(input, "condition")
                ?? ""
            return detail.isEmpty ? action : "\(action) · \(detail)"
        case "budget_status":
            return "budgets"
        case "task_detail", "task_control":
            let action = string(input, "action") ?? ""
            let task = string(input, "task_id") ?? ""
            return [action, task].filter { !$0.isEmpty }.joined(separator: " · ")
        case "tool_search":
            return string(input, "query") ?? "tool catalog"
        case "publish_artifact_file":
            return string(input, "path") ?? "artifact"
        case "shell":
            return collapsed(string(input, "command") ?? "")
        case "fetch_url":
            return string(input, "url") ?? ""
        case "mcp":
            let action = string(input, "action") ?? "call"
            let server = string(input, "server") ?? ""
            let tool = string(input, "tool") ?? ""
            return [action, server, tool].filter { !$0.isEmpty }.joined(separator: " · ")
        case "delegate":
            let role = string(input, "role") ?? "task"
            let objective = string(input, "objective") ?? ""
            return objective.isEmpty ? role : "\(role) · \(objective)"
        case "delegate_many":
            let count = arrayCount(input, "tasks")
            return "\(count) parallel task\(count == 1 ? "" : "s")"
        case "delegate_acp":
            let agent = string(input, "agent") ?? "external agent"
            let objective = string(input, "objective") ?? ""
            return objective.isEmpty ? agent : "\(agent) · \(objective)"
        case "submit_team_plan":
            return string(input, "goal") ?? "team plan"
        case "todo_write":
            let count = arrayCount(input, "items")
            return "\(count) plan item\(count == 1 ? "" : "s")"
        case "ask_user":
            return string(input, "question") ?? ""
        case "send_message":
            let to = string(input, "to") ?? "agent"
            let kind = string(input, "kind") ?? "message"
            return "\(kind) → \(to)"
        case "publish_artifact":
            return string(input, "path") ?? string(input, "name") ?? "artifact"
        case "remember":
            return string(input, "title") ?? "memory"
        case "recall":
            return string(input, "query") ?? "memory search"
        default:
            return firstString(in: input)
        }
    }

    static func displayName(_ name: String) -> String {
        name.isEmpty ? "tool" : name
    }

    /// Optional source badge for code-intelligence tool results. Unknown or
    /// absent sources return nil so ordinary tools do not get a badge.
    static func intelligenceSource(_ structured: JSONValue?) -> String? {
        guard let value = structured?.objectValue?["source"]?.stringValue else {
            return nil
        }
        switch value.lowercased() {
        case "lsp":
            return "LSP"
        case "lexical":
            return "Lexical"
        case "build":
            return "Build"
        default:
            return nil
        }
    }

    /// Cache provenance comes only from the runtime's structured result. A
    /// missing or non-boolean `cache.cache_hit` never renders a badge.
    static func cacheProvenance(_ structured: JSONValue?) -> String? {
        guard let cache = structured?.objectValue?["cache"]?.objectValue,
              cache["cache_hit"]?.boolValue == true
        else {
            return nil
        }
        return "CACHED"
    }

    static func intelligenceStatus(_ structured: JSONValue?) -> String? {
        guard let status = structured?.objectValue?["status"]?.stringValue,
              !status.isEmpty
        else {
            return nil
        }
        return status
    }

    static func icon(name: String, isError: Bool) -> String {
        if isError {
            return "exclamationmark.triangle"
        }
        switch name {
        case "read_file", "list_dir", "grep", "find_files", "read_files", "file_info", "workspace_read":
            return "magnifyingglass"
        case "write_file", "edit_file", "apply_patch", "file_manage":
            return "pencil"
        case "project_profile", "run_project_task":
            return "hammer"
        case "git":
            return "arrow.triangle.branch"
        case "lsp_query":
            return "magnifyingglass"
        case "web_search":
            return "globe"
        case "read_pdf", "read_image", "read_document", "data_query":
            return "doc.text.magnifyingglass"
        case "budget_status", "task_detail", "task_control", "tool_search":
            return "gauge"
        case "browser":
            return "safari"
        case "shell":
            return "terminal"
        case "fetch_url":
            return "globe"
        case "mcp":
            return "puzzlepiece.extension"
        case "delegate", "delegate_many", "delegate_acp":
            return "person.2"
        case "publish_artifact", "list_artifacts", "read_artifact":
            return "cube"
        default:
            return "wrench.and.screwdriver"
        }
    }

    static func isHostSensitive(name: String, input: JSONValue) -> Bool {
        guard name == "shell" else { return false }
        let command = (string(input, "command") ?? "").lowercased()
        let markers = [
            "sudo ", "su ", "shutdown", "reboot", "halt", "poweroff", "diskutil",
            "launchctl", "mkfs", "newfs", "dd if=", "csrutil", "nvram",
            "defaults write", "osascript", "security ", "brew ", "docker ",
            "kubectl ", "terraform ", "aws ", "gcloud ", "az ",
        ]
        return markers.contains { command.contains($0) }
    }

    private static func string(_ input: JSONValue, _ key: String) -> String? {
        guard let value = input.objectValue?[key]?.stringValue else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private static func arrayCount(_ input: JSONValue, _ key: String) -> Int {
        guard case let .array(values)? = input.objectValue?[key] else { return 0 }
        return values.count
    }

    private static func firstString(in input: JSONValue) -> String {
        guard let object = input.objectValue else { return "" }
        for key in object.keys.sorted() {
            if let value = object[key]?.stringValue {
                let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty {
                    return trimmed
                }
            }
        }
        return ""
    }

    private static func collapsed(_ value: String) -> String {
        value
            .split(whereSeparator: \.isWhitespace)
            .joined(separator: " ")
    }
}
