import Foundation

/// Presentation-only summaries for tool calls in the transcript.
///
/// The runtime is the authority on permissions and execution. This helper only
/// turns a structured input object into a short, readable line so the
/// transcript can stay dense without hiding what the agent is doing.
enum ToolTrace {
    static func summary(name: String, input: JSONValue) -> String {
        switch name {
        case "read_file", "write_file", "edit_file", "list_dir", "workspace_read":
            return string(input, "path") ?? ""
        case "grep":
            let pattern = string(input, "pattern") ?? ""
            let path = string(input, "path") ?? "."
            return pattern.isEmpty ? path : "\(pattern) · \(path)"
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

    static func icon(name: String, isError: Bool) -> String {
        if isError {
            return "exclamationmark.triangle"
        }
        switch name {
        case "read_file", "list_dir", "grep", "workspace_read":
            return "magnifyingglass"
        case "write_file", "edit_file":
            return "pencil"
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
