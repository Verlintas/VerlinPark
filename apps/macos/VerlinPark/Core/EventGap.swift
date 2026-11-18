import Foundation

/// Additive daemon notification describing a gap in the live event stream.
///
/// The daemon emits `event.gap` when its broadcast receiver lagged or a
/// bounded replay was truncated. It is a resync instruction, not an event:
/// clients must never append it to the event transcript.
struct EventGap: Equatable, Sendable {
    let skipped: UInt64?
    let truncated: Bool
    let lastSeq: UInt64?
    let lastSessionID: String?
    let resumeFromSeq: UInt64?
    let sessionID: String?
    let resyncRequired: Bool
    let reason: String?

    /// The session the gap can be backfilled from. A global subscriber gets
    /// `session_id = null`; the last delivered event's session is then the
    /// only session for which `resume_from_seq` is meaningful.
    var targetSessionID: String? {
        sessionID ?? lastSessionID
    }

    /// First seq to fetch. `resume_from_seq` is authoritative when present;
    /// fall back to `last_seq + 1` and never overflow.
    var effectiveResumeFromSeq: UInt64? {
        if let resumeFromSeq, resumeFromSeq > 0 {
            return resumeFromSeq
        }
        guard let lastSeq else { return nil }
        return lastSeq == UInt64.max ? nil : lastSeq + 1
    }

    func targets(sessionID candidate: String?) -> Bool {
        guard let candidate, let target = targetSessionID else { return false }
        return target == candidate
    }

    /// Routes an RPC notification only when its method is exactly
    /// `event.gap`. Unknown methods and missing params are ignored.
    static func notification(method: String?, params: JSONValue?) -> EventGap? {
        guard method == "event.gap", let params else { return nil }
        return decode(from: params)
    }

    /// Lenient decode: unknown fields are ignored, malformed values degrade to
    /// nil, and a payload with no recognizable gap fields is discarded. A
    /// partially malformed frame may still trigger a resync, which is safe.
    static func decode(from payload: JSONValue) -> EventGap? {
        guard let object = payload.objectValue else { return nil }
        let recognized = [
            "skipped", "truncated", "last_seq", "last_session_id",
            "resume_from_seq", "session_id", "resync_required", "reason",
        ]
        guard recognized.contains(where: { object[$0] != nil }) else {
            return nil
        }
        return EventGap(
            skipped: object["skipped"]?.uint64Value,
            truncated: object["truncated"]?.boolValue ?? false,
            lastSeq: object["last_seq"]?.uint64Value,
            lastSessionID: object["last_session_id"]?.stringValue,
            resumeFromSeq: object["resume_from_seq"]?.uint64Value,
            sessionID: object["session_id"]?.stringValue,
            resyncRequired: object["resync_required"]?.boolValue ?? true,
            reason: object["reason"]?.stringValue
        )
    }
}

private extension JSONValue {
    /// `JSONValue` stores numbers as `Double`; event sequences are exact
    /// non-negative integers well below 2^53.
    var uint64Value: UInt64? {
        guard case let .number(value) = self,
              value.isFinite,
              value >= 0
        else {
            return nil
        }
        return UInt64(exactly: value)
    }
}
