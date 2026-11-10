import Foundation

/// A single visible row in the session transcript.
///
/// The model is deliberately pure: it turns the append-only event stream into
/// stable, renderable rows. Keeping this outside `SessionChatView` makes the
/// transcript replayable, testable, and cheap to reason about when tools are
/// running or approvals are resolved.
enum TranscriptEntry: Identifiable, Equatable {
    case user(id: String, text: String)
    case assistant(id: String, text: String, agent: String?, citations: [SourceCitation])
    case tool(TranscriptToolEntry)
    case approval(TranscriptApprovalEntry)
    case question(id: String)
    case notice(id: String, text: String)
    case error(id: String, text: String)

    var id: String {
        switch self {
        case let .user(id, _), let .assistant(id, _, _, _), let .notice(id, _),
             let .error(id, _):
            id
        case let .tool(entry):
            entry.id
        case let .approval(entry):
            entry.id
        case let .question(id):
            "question-\(id)"
        }
    }
}

struct TranscriptToolEntry: Identifiable, Equatable {
    let toolCallID: String
    let name: String
    let input: JSONValue
    let output: String
    let isError: Bool
    let agent: String?
    let startedAt: String
    let finishedAt: String?

    var id: String {
        "tool-\(toolCallID)"
    }

    var isRunning: Bool {
        finishedAt == nil
    }

    var duration: TimeInterval? {
        guard let finishedAt,
              let start = TranscriptProjection.date(startedAt),
              let end = TranscriptProjection.date(finishedAt)
        else {
            return nil
        }
        let interval = end.timeIntervalSince(start)
        return interval >= 0 ? interval : nil
    }

    var summary: String {
        ToolTrace.summary(name: name, input: input)
    }
}

struct TranscriptApprovalEntry: Identifiable, Equatable {
    let approval: Approval
    let resolved: Bool
    let decision: String?

    var id: String {
        "approval-\(approval.id)"
    }

    var isPending: Bool {
        !resolved && approval.status == "pending"
    }

    var status: String {
        decision ?? approval.status
    }

    var isDenied: Bool {
        status == "denied"
    }

    var summary: String {
        ToolTrace.summary(name: approval.tool, input: approval.input)
    }
}

enum TranscriptProjection {
    static func build(events: [Event]) -> [TranscriptEntry] {
        var builder = Builder()
        for event in events {
            builder.consume(event)
        }
        return builder.entries
    }

    /// Low-signal operations stay in the audit event stream but do not
    /// clutter the conversation. Errors still surface.
    static func isRoutineTool(_ name: String) -> Bool {
        [
            "read_file", "list_dir", "grep", "write_file", "edit_file",
            "find_files", "file_info", "read_files", "project_profile", "git",
            "repo_map", "find_symbol", "find_references", "dependency_graph",
            "build_context", "affected_tests", "code_diagnostics",
            "lsp_query", "read_pdf", "read_image", "read_document", "data_query",
            "budget_status", "task_detail", "tool_search",
            "todo_write", "fetch_url", "remember", "recall", "coordinate",
            "team_status", "send_message", "read_mailbox", "list_artifacts",
            "read_artifact", "publish_artifact", "search_sources", "read_source",
            "list_sources",
        ].contains(name)
    }

    static func date(_ value: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = formatter.date(from: value) {
            return date
        }
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: value)
    }

    static func agentLabel(_ event: Event) -> String? {
        guard let role = event.payload.objectValue?["agent_role"]?.stringValue,
              role != "leader"
        else {
            return nil
        }
        let name = event.payload.objectValue?["agent_name"]?.stringValue
        let title = event.payload.objectValue?["agent_title"]?.stringValue
        let label = title ?? name
        return label.map { "\(role.capitalized) · \($0)" } ?? role.capitalized
    }

    private struct Builder {
        var entries: [TranscriptEntry] = []
        var toolIndices: [String: Int] = [:]
        var approvalIndices: [String: Int] = [:]

        mutating func consume(_ event: Event) {
            switch event.type {
            case "session.forked":
                consumeFork(event)
            case "message.created":
                consumeMessage(event)
            case "agent.message":
                consumeAgentMessage(event)
            case "tool.started":
                consumeToolStarted(event)
            case "tool.finished":
                consumeToolFinished(event)
            case "question.requested":
                if let question = try? event.payload.decode(Question.self) {
                    entries.append(.question(id: question.id))
                }
            case "approval.requested":
                consumeApprovalRequested(event)
            case "approval.resolved":
                consumeApprovalResolved(event)
            case "agent.failed", "agent.unavailable":
                let message = event.payload.objectValue?["error"]?.stringValue
                    ?? event.payload.objectValue?["message"]?.stringValue
                    ?? "The agent stopped."
                entries.append(.error(id: event.id, text: message))
            case "agent.budget_warning":
                entries.append(.notice(id: event.id, text: budgetWarning(event)))
            case "agent.budget_exceeded":
                entries.append(.notice(id: event.id, text: budgetExceeded(event)))
            case "agent.cancelled":
                entries.append(.notice(id: event.id, text: "Run cancelled."))
            default:
                break
            }
        }

        private mutating func consumeFork(_ event: Event) {
            if let source = event.payload.objectValue?["source_session_id"]?.stringValue,
               let commit = event.payload.objectValue?["commit"]?.stringValue
            {
                entries.append(
                    .notice(
                        id: event.id,
                        text: "Forked from \(String(source.prefix(8))) at \(String(commit.prefix(8)))"
                    )
                )
            } else if let target = event.payload.objectValue?["fork_session_id"]?.stringValue {
                entries.append(
                    .notice(id: event.id, text: "Forked to session \(String(target.prefix(8)))")
                )
            }
        }

        private mutating func consumeMessage(_ event: Event) {
            guard event.payload.objectValue?["role"]?.stringValue == "user",
                  let content = event.payload.objectValue?["content"]?.stringValue
            else {
                return
            }
            entries.append(.user(id: event.id, text: content))
        }

        private mutating func consumeAgentMessage(_ event: Event) {
            guard let text = event.payload.objectValue?["text"]?.stringValue, !text.isEmpty else {
                return
            }
            let citations = event.payload.objectValue?["citations"]
                .flatMap { try? $0.decode([SourceCitation].self) } ?? []
            entries.append(
                .assistant(
                    id: event.id,
                    text: text,
                    agent: TranscriptProjection.agentLabel(event),
                    citations: citations
                )
            )
        }

        private mutating func consumeToolStarted(_ event: Event) {
            guard let toolCallID = event.payload.objectValue?["tool_call_id"]?.stringValue,
                  let name = event.payload.objectValue?["name"]?.stringValue
            else {
                return
            }
            if TranscriptProjection.isRoutineTool(name) {
                return
            }
            let entry = TranscriptToolEntry(
                toolCallID: toolCallID,
                name: name,
                input: event.payload.objectValue?["input"] ?? .null,
                output: "",
                isError: false,
                agent: TranscriptProjection.agentLabel(event),
                startedAt: event.createdAt,
                finishedAt: nil
            )
            if let index = toolIndices[toolCallID] {
                entries[index] = .tool(entry)
            } else {
                toolIndices[toolCallID] = entries.count
                entries.append(.tool(entry))
            }
        }

        private mutating func consumeToolFinished(_ event: Event) {
            guard let toolCallID = event.payload.objectValue?["tool_call_id"]?.stringValue,
                  let name = event.payload.objectValue?["name"]?.stringValue
            else {
                return
            }
            let output = event.payload.objectValue?["output"]?.stringValue ?? ""
            let isError = event.payload.objectValue?["is_error"]?.boolValue ?? false
            if TranscriptProjection.isRoutineTool(name), !isError,
               toolIndices[toolCallID] == nil
            {
                return
            }
            let entry = TranscriptToolEntry(
                toolCallID: toolCallID,
                name: name,
                input: event.payload.objectValue?["input"] ?? .null,
                output: output,
                isError: isError,
                agent: TranscriptProjection.agentLabel(event),
                startedAt: event.createdAt,
                finishedAt: event.createdAt
            )
            if let index = toolIndices[toolCallID],
               case let .tool(existing) = entries[index]
            {
                entries[index] = .tool(
                    TranscriptToolEntry(
                        toolCallID: existing.toolCallID,
                        name: existing.name,
                        input: existing.input,
                        output: output,
                        isError: isError,
                        agent: existing.agent,
                        startedAt: existing.startedAt,
                        finishedAt: event.createdAt
                    )
                )
            } else {
                toolIndices[toolCallID] = entries.count
                entries.append(.tool(entry))
            }
        }

        private mutating func consumeApprovalRequested(_ event: Event) {
            guard let approval = try? event.payload.decode(Approval.self) else { return }
            let entry = TranscriptApprovalEntry(
                approval: approval,
                resolved: false,
                decision: nil
            )
            approvalIndices[approval.id] = entries.count
            entries.append(.approval(entry))
        }

        private mutating func consumeApprovalResolved(_ event: Event) {
            guard let approvalID = event.payload.objectValue?["approval_id"]?.stringValue,
                  let index = approvalIndices[approvalID],
                  case let .approval(existing) = entries[index]
            else {
                return
            }
            let approved = event.payload.objectValue?["approved"]?.boolValue ?? false
            entries[index] = .approval(
                TranscriptApprovalEntry(
                    approval: existing.approval,
                    resolved: true,
                    decision: approved ? "approved" : "denied"
                )
            )
        }

        private func budgetWarning(_ event: Event) -> String {
            let scope = event.payload.objectValue?["scope"]?.stringValue ?? "session"
            let reason = event.payload.objectValue?["reason"]?.stringValue ?? "tokens"
            if reason == "cost" {
                let spent = event.payload.objectValue?["cost_spent_usd"]?.doubleValue ?? 0
                let limit = event.payload.objectValue?["cost_limit_usd"]?.doubleValue ?? 0
                return "\(scope) USD budget is at 80% ($"
                    + String(format: "%.4f", spent) + " / $"
                    + String(format: "%.2f", limit) + ")."
            }
            let spent = event.payload.objectValue?["spent"]?.intValue ?? 0
            let limit = event.payload.objectValue?["limit"]?.intValue ?? 0
            return "\(scope) token budget is at 80% (\(spent) / \(limit))."
        }

        private func budgetExceeded(_ event: Event) -> String {
            let reason = event.payload.objectValue?["reason"]?.stringValue ?? "tokens"
            switch reason {
            case "cost":
                let limit = event.payload.objectValue?["cost_limit_usd"]?.doubleValue ?? 0
                let spent = event.payload.objectValue?["cost_spent_usd"]?.doubleValue ?? 0
                return "USD budget exhausted ($\(String(format: "%.4f", spent)) / $"
                    + String(format: "%.2f", limit) + ")."
            case "time":
                let limit = event.payload.objectValue?["time_budget_secs"]?.intValue ?? 0
                return "Wall-clock budget exhausted (\(limit)s)."
            default:
                let limit = event.payload.objectValue?["limit"]?.intValue ?? 0
                let spent = event.payload.objectValue?["spent"]?.intValue ?? 0
                return "Token budget exhausted (\(spent) / \(limit))."
            }
        }
    }
}
