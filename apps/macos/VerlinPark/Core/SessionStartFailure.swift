import Foundation

/// `session.create` is deliberately non-transactional on the daemon: the
/// session row/worktree may exist even when the Leader failed to start. D10
/// returns a typed `SESSION_START_FAILED` RPC error that names the created
/// session. Treating that as a plain failure invites a duplicate session.
struct SessionStartFailure: Equatable, Sendable {
    let sessionID: String
    let projectID: String?
    let phase: String?
    let message: String
    let status: String?
    let retryable: Bool

    var warningDetail: String {
        var text = "Session \(sessionID.prefix(8)) exists, but the leader failed to start"
        if let phase, !phase.isEmpty {
            text += " at \(phase)"
        }
        let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty {
            text += ": \(trimmed)"
        }
        return String(text.prefix(400))
    }
}

enum SessionCreateErrorMapper {
    /// Returns a partial-success description only when all three daemon
    /// guarantees are present: the typed code, `session_created == true`, and
    /// a non-empty session id. Everything else keeps the ordinary error path.
    static func partialSuccess(from error: Error) -> SessionStartFailure? {
        guard let coreError = error as? CoreError,
              case let .rpc(_, message, data) = coreError,
              let object = data?.objectValue,
              object["code"]?.stringValue == "SESSION_START_FAILED",
              object["session_created"]?.boolValue == true,
              let rawSessionID = object["session_id"]?.stringValue
        else {
            return nil
        }
        let sessionID = rawSessionID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !sessionID.isEmpty else { return nil }
        return SessionStartFailure(
            sessionID: sessionID,
            projectID: object["project_id"]?.stringValue,
            phase: object["phase"]?.stringValue,
            message: message,
            status: object["status"]?.stringValue,
            retryable: object["retryable"]?.boolValue ?? false
        )
    }
}
