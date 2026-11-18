//! WebSocket framing, event cursors, and gap/resync notifications.
//!
//! The WebSocket transport is the only live event transport. Two properties
//! are enforced here:
//!
//! - a frame is size-checked **before** it is parsed, and the pre-auth frame
//!   has a much smaller bound than an authenticated request. `/rpc` already
//!   authenticates before reading a body; this closes the equivalent gap on
//!   the upgrade path.
//! - a lagged broadcast receiver never silently loses state. The daemon sends
//!   a bounded `event.gap` notification with the last delivered seq so the
//!   client can re-fetch or reconnect with `since_seq`.
//!
//! `event.gap` is an additive notification. Clients that do not know it ignore
//! unknown methods (the bundled Swift client does), so the wire stays
//! backward compatible within protocol major version 1.

use serde_json::json;
use vp_protocol::rpc::{RpcError, RpcNotification, RpcRequest, codes};
use vp_protocol::types::Event;

/// Documented maximum message size for an authenticated WebSocket frame
/// (ARCHITECTURE §WebSocket: "max 8 MB").
pub(crate) const MAX_WS_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// axum's per-frame ceiling. A message may be split across frames, so this is
/// deliberately not smaller than the documented message bound: the 8 MiB
/// message limit is the contract, and a smaller frame limit would reject a
/// legitimate single-frame request (for example a long prompt) below it.
pub(crate) const MAX_WS_FRAME_BYTES: usize = MAX_WS_MESSAGE_BYTES;

/// The first frame only carries `auth` (token + optional protocol version).
/// Keep it far below the general message bound so an unauthenticated peer
/// cannot force a large allocation or a large JSON parse.
pub(crate) const MAX_PREAUTH_FRAME_BYTES: usize = 64 * 1024;

/// Bound one `since_seq` backfill burst. `store.list_events` clamps to 1 000
/// rows, and the client is told (`truncated`/`event.gap`) when more exist.
pub(crate) const MAX_REPLAY_EVENTS: u32 = 1_000;

/// Size-check then parse one JSON-RPC frame. The size check runs first so an
/// oversized frame is rejected without allocating a JSON DOM for it.
pub(crate) fn parse_rpc_frame(
    text: &str,
    limit: usize,
) -> std::result::Result<RpcRequest, RpcError> {
    if text.len() > limit {
        return Err(RpcError::new(
            codes::INVALID_REQUEST,
            format!(
                "websocket frame is {} bytes; the limit is {limit} bytes",
                text.len()
            ),
        ));
    }
    serde_json::from_str(text).map_err(|error| RpcError::new(codes::PARSE_ERROR, error.to_string()))
}

/// First-frame parse. Uses the small pre-auth bound; the token is checked by
/// the caller immediately afterwards.
pub(crate) fn parse_auth_frame(text: &str) -> std::result::Result<RpcRequest, RpcError> {
    parse_rpc_frame(text, MAX_PREAUTH_FRAME_BYTES)
}

/// Tracks the last event sequence delivered to one WebSocket client and
/// renders bounded gap notifications.
#[derive(Debug, Default)]
pub(crate) struct EventCursor {
    filter: Option<String>,
    last_seq: Option<u64>,
    last_session_id: Option<String>,
}

impl EventCursor {
    /// `baseline` is the client-supplied `since_seq`: it is the last seq the
    /// client already has, so replay starts at `baseline + 1`.
    pub(crate) fn new(filter: Option<String>, baseline: Option<u64>) -> Self {
        Self {
            last_session_id: filter.clone(),
            filter,
            last_seq: baseline,
        }
    }

    /// Returns true when the event must be forwarded. Events outside the
    /// session filter are dropped, and replayed events that overlap the live
    /// stream are deduplicated by seq (only within one session: seq counters
    /// are per session, so they are not comparable across sessions).
    pub(crate) fn observe(&mut self, event: &Event) -> bool {
        if let Some(filter) = self.filter.as_deref()
            && filter != event.session_id
        {
            return false;
        }
        if self.filter.as_deref() == Some(event.session_id.as_str())
            && self.last_seq.is_some_and(|seq| event.seq <= seq)
        {
            return false;
        }
        self.last_seq = Some(event.seq);
        self.last_session_id = Some(event.session_id.clone());
        true
    }

    /// Bounded `event.gap` frame. `skipped` is `None` when the daemon cannot
    /// count the gap (a truncated backfill); `truncated` makes that explicit.
    pub(crate) fn gap_notification(&self, skipped: Option<u64>, reason: &str) -> RpcNotification {
        let last_seq = self.last_seq;
        let resume_from_seq = last_seq.map(|seq| seq.saturating_add(1));
        RpcNotification::new(
            "event.gap",
            json!({
                "skipped": skipped,
                "truncated": skipped.is_none(),
                "last_seq": last_seq,
                "last_session_id": self.last_session_id,
                "resume_from_seq": resume_from_seq,
                "session_id": self.filter,
                "resync_required": true,
                "reason": reason,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;
    use tokio::sync::broadcast::error::TryRecvError;

    fn event(session: &str, seq: u64) -> Event {
        Event {
            session_id: session.to_string(),
            seq,
            kind: "session.status".to_string(),
            payload: json!({"status": "running"}),
            created_at: "2026-11-19T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn oversized_preauth_frames_are_rejected_before_json_parsing() {
        // Invalid JSON on purpose: if the size check did not run first this
        // would surface PARSE_ERROR instead of INVALID_REQUEST.
        let oversized = "x".repeat(MAX_PREAUTH_FRAME_BYTES + 1);
        let error = parse_auth_frame(&oversized).unwrap_err();
        assert_eq!(error.code, codes::INVALID_REQUEST);
        assert!(error.message.contains("limit"), "{}", error.message);

        let auth = r#"{"jsonrpc":"2.0","id":1,"method":"auth","params":{"token":"t"}}"#;
        let request = parse_auth_frame(auth).unwrap();
        assert_eq!(request.method, "auth");
    }

    #[test]
    fn authenticated_frames_use_the_documented_eight_megabyte_bound() {
        let oversized = "x".repeat(MAX_WS_MESSAGE_BYTES + 1);
        let error = parse_rpc_frame(&oversized, MAX_WS_MESSAGE_BYTES).unwrap_err();
        assert_eq!(error.code, codes::INVALID_REQUEST);

        // A payload above the pre-auth bound but below the general bound is a
        // normal authenticated request.
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"pad":"{}"}}}}"#,
            "y".repeat(MAX_PREAUTH_FRAME_BYTES * 2)
        );
        assert_eq!(
            parse_rpc_frame(&body, MAX_WS_MESSAGE_BYTES).unwrap().method,
            "ping"
        );
    }

    #[test]
    fn since_seq_replay_deduplicates_the_live_overlap() {
        let mut cursor = EventCursor::new(Some("ses-1".to_string()), Some(10));
        assert!(
            !cursor.observe(&event("ses-1", 10)),
            "already held by client"
        );
        assert!(!cursor.observe(&event("ses-1", 9)), "older than the cursor");
        assert!(cursor.observe(&event("ses-1", 11)));
        assert!(!cursor.observe(&event("ses-2", 12)), "outside the filter");
        assert!(!cursor.observe(&event("ses-1", 11)), "already delivered");
    }

    #[test]
    fn lagged_cursor_reports_the_last_observed_seq_and_requires_resync() {
        let mut cursor = EventCursor::new(Some("ses-1".to_string()), None);
        for seq in 1..=3 {
            assert!(cursor.observe(&event("ses-1", seq)));
        }
        let gap = cursor.gap_notification(Some(2_048), "lagged");
        assert_eq!(gap.method, "event.gap");
        assert_eq!(gap.params["skipped"], 2_048);
        assert_eq!(gap.params["truncated"], false);
        assert_eq!(gap.params["last_seq"], 3);
        assert_eq!(gap.params["last_session_id"], "ses-1");
        assert_eq!(gap.params["resume_from_seq"], 4);
        assert_eq!(gap.params["session_id"], "ses-1");
        assert_eq!(gap.params["resync_required"], true);
        assert_eq!(gap.params["reason"], "lagged");
    }

    #[test]
    fn truncated_backfill_gap_has_no_invented_skipped_count() {
        let cursor = EventCursor::new(Some("ses-1".to_string()), Some(500));
        let gap = cursor.gap_notification(None, "backfill_truncated");
        assert_eq!(gap.params["skipped"], serde_json::Value::Null);
        assert_eq!(gap.params["truncated"], true);
        assert_eq!(gap.params["resume_from_seq"], 501);
        assert_eq!(gap.params["resync_required"], true);
    }

    #[tokio::test]
    async fn lagged_subscriber_frames_start_with_a_gap_then_resume_in_order() {
        // Mirrors the receive loop: a stalled reader overflows the channel,
        // the first outcome is `Lagged`, and the gap frame is sent before the
        // surviving events. No event after the gap is forwarded out of order.
        let (sender, mut receiver) = broadcast::channel(4);
        let mut cursor = EventCursor::new(Some("ses-1".to_string()), None);
        for seq in 1..=8 {
            sender.send(event("ses-1", seq)).unwrap();
        }

        let mut frames: Vec<String> = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(event) => {
                    if cursor.observe(&event) {
                        frames.push(format!("event:{}", event.seq));
                    }
                }
                Err(TryRecvError::Lagged(skipped)) => {
                    frames.push(format!(
                        "gap:{}",
                        cursor.gap_notification(Some(skipped), "lagged").params["last_seq"]
                    ));
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
            }
        }

        assert_eq!(frames.first().map(String::as_str), Some("gap:null"));
        let delivered: Vec<u64> = frames
            .iter()
            .filter_map(|frame| {
                frame
                    .strip_prefix("event:")
                    .and_then(|seq| seq.parse().ok())
            })
            .collect();
        assert!(!delivered.is_empty());
        assert!(
            delivered.windows(2).all(|pair| pair[0] < pair[1]),
            "events after the gap must be ordered: {delivered:?}"
        );
    }
}
