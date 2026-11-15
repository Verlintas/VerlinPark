//! Bounded server-sent-events decoding for the loopback client.
//!
//! The parser accepts the facade's raw event frames and also unwraps a
//! JSON-RPC `result` envelope for interoperability with A2A peers that wrap
//! each SSE payload. It never reconnects automatically and never allocates an
//! unbounded buffer.

use serde_json::Value;

use crate::error::{A2aError, A2aResult};
use crate::limits::A2aLimits;
use crate::wire::{A2aEventFrame, A2aStreamEvent};

pub(crate) struct SseDecoder {
    limits: A2aLimits,
    expected_task_id: Option<String>,
    last_event_id: Option<u64>,
    line_buffer: Vec<u8>,
    pending_id: Option<u64>,
    pending_event: Option<String>,
    pending_data: String,
}

impl SseDecoder {
    pub(crate) fn new(limits: &A2aLimits, expected_task_id: Option<String>) -> Self {
        Self {
            limits: limits.clone(),
            expected_task_id,
            last_event_id: None,
            line_buffer: Vec::new(),
            pending_id: None,
            pending_event: None,
            pending_data: String::new(),
        }
    }

    /// Feed one transport chunk. A chunk may contain any number of complete
    /// frames plus a partial line.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> A2aResult<Vec<A2aEventFrame>> {
        if chunk.len() > self.limits.max_body_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "sse.chunk",
                max: self.limits.max_body_bytes,
                actual: chunk.len(),
            });
        }
        let line_limit = self.limits.max_sse_event_bytes.saturating_add(64);
        let buffer_limit = self.limits.max_body_bytes.saturating_add(line_limit);
        if self.line_buffer.len().saturating_add(chunk.len()) > buffer_limit {
            return Err(A2aError::BoundsExceeded {
                field: "sse.buffer",
                max: buffer_limit,
                actual: self.line_buffer.len().saturating_add(chunk.len()),
            });
        }
        self.line_buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(index) = self.line_buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.line_buffer.drain(0..=index).collect();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() > line_limit {
                return Err(A2aError::BoundsExceeded {
                    field: "sse.line",
                    max: line_limit,
                    actual: line.len(),
                });
            }
            if let Some(frame) = self.process_line(&line)? {
                frames.push(frame);
            }
        }
        if self.line_buffer.len() > line_limit {
            return Err(A2aError::BoundsExceeded {
                field: "sse.line",
                max: line_limit,
                actual: self.line_buffer.len(),
            });
        }
        Ok(frames)
    }

    /// Flush a final event if the peer closed without a trailing blank line.
    pub(crate) fn finish(&mut self) -> A2aResult<Option<A2aEventFrame>> {
        if !self.line_buffer.is_empty() {
            let line = std::mem::take(&mut self.line_buffer);
            if let Some(frame) = self.process_line(&line)? {
                return Ok(Some(frame));
            }
        }
        if self.pending_data.is_empty() {
            return Ok(None);
        }
        self.dispatch()
    }

    fn process_line(&mut self, line: &[u8]) -> A2aResult<Option<A2aEventFrame>> {
        if line.is_empty() {
            return self.dispatch();
        }
        let line = std::str::from_utf8(line).map_err(|_| A2aError::Protocol {
            reason: "SSE stream is not valid UTF-8".into(),
        })?;
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "id" => {
                if value.is_empty() {
                    return Err(A2aError::Protocol {
                        reason: "SSE event id is empty".into(),
                    });
                }
                let id = value.parse::<u64>().map_err(|_| A2aError::Protocol {
                    reason: "SSE event id must be an unsigned integer".into(),
                })?;
                if let Some(last) = self.last_event_id
                    && id <= last
                {
                    return Err(A2aError::Protocol {
                        reason: "SSE event ids must be strictly increasing".into(),
                    });
                }
                self.pending_id = Some(id);
            }
            "event" => {
                if value.len() > 64 || value.bytes().any(|byte| byte.is_ascii_control()) {
                    return Err(A2aError::Protocol {
                        reason: "SSE event name is invalid".into(),
                    });
                }
                self.pending_event = Some(value.to_string());
            }
            "data" => {
                let next = self
                    .pending_data
                    .len()
                    .saturating_add(value.len())
                    .saturating_add(1);
                if next > self.limits.max_sse_event_bytes {
                    return Err(A2aError::BoundsExceeded {
                        field: "sse.event",
                        max: self.limits.max_sse_event_bytes,
                        actual: next,
                    });
                }
                self.pending_data.push_str(value);
                self.pending_data.push('\n');
            }
            "retry" | "" => {
                // No automatic reconnects. Retry hints are intentionally
                // ignored so a peer cannot turn this into a retry loop.
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> A2aResult<Option<A2aEventFrame>> {
        let data = self.pending_data.trim_end_matches('\n').to_string();
        let id = self.pending_id.take();
        let event_name = self.pending_event.take();
        self.pending_data.clear();
        if data.is_empty() {
            return Ok(None);
        }
        let mut value: Value =
            serde_json::from_str(&data).map_err(|_| A2aError::InvalidAgentResponse {
                reason: "SSE event data is not valid JSON".into(),
            })?;
        if value.get("jsonrpc").is_some()
            && let Some(result) = value.get_mut("result")
        {
            value = result.take();
        }
        let event: A2aStreamEvent =
            serde_json::from_value(value).map_err(|_| A2aError::InvalidAgentResponse {
                reason: "SSE event has an unknown shape".into(),
            })?;
        if let Some(expected) = &self.expected_task_id {
            if event.task_id() != expected {
                return Err(A2aError::InvalidAgentResponse {
                    reason: "SSE event belongs to a different task".into(),
                });
            }
        } else {
            self.expected_task_id = Some(event.task_id().to_string());
        }
        if let Some(name) = event_name
            && name != event.kind()
        {
            return Err(A2aError::InvalidAgentResponse {
                reason: "SSE event name does not match its payload".into(),
            });
        }
        event.validate(&self.limits)?;
        if let Some(id) = id {
            self.last_event_id = Some(id);
        }
        Ok(Some(A2aEventFrame { id, event }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{A2aStreamEvent, TaskState};

    #[test]
    fn decodes_status_and_artifact_frames_with_monotonic_ids() {
        let limits = A2aLimits::default();
        let mut decoder = SseDecoder::new(&limits, Some("task-1".into()));
        let input = concat!(
            "id: 1\n",
            "event: status-update\n",
            "data: {\"taskId\":\"task-1\",\"contextId\":\"ctx-1\",\"status\":{\"state\":\"working\"},\"final\":false,\"kind\":\"status-update\"}\n",
            "\n",
            "id: 2\n",
            "event: artifact-update\n",
            "data: {\"taskId\":\"task-1\",\"contextId\":\"ctx-1\",\"artifact\":{\"artifactId\":\"a-1\",\"parts\":[{\"kind\":\"text\",\"text\":\"hi\"}]},\"append\":false,\"lastChunk\":true,\"kind\":\"artifact-update\"}\n",
            "\n"
        );
        let frames = decoder.push(input.as_bytes()).expect("decode");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id, Some(1));
        assert_eq!(frames[1].id, Some(2));
        match &frames[0].event {
            A2aStreamEvent::Status(event) => assert_eq!(event.status.state, TaskState::Working),
            A2aStreamEvent::Artifact(_) => panic!("wrong event shape"),
        }
    }

    #[test]
    fn rejects_non_monotonic_ids_and_wrong_tasks() {
        let limits = A2aLimits::default();
        let mut decoder = SseDecoder::new(&limits, None);
        let first = "id: 2\ndata: {\"taskId\":\"task-1\",\"contextId\":\"ctx-1\",\"status\":{\"state\":\"working\"},\"final\":false}\n\n";
        decoder.push(first.as_bytes()).expect("first frame");
        let second = "id: 1\ndata: {\"taskId\":\"task-1\",\"contextId\":\"ctx-1\",\"status\":{\"state\":\"completed\"},\"final\":true}\n\n";
        assert!(decoder.push(second.as_bytes()).is_err());

        let mut decoder = SseDecoder::new(&limits, Some("task-1".into()));
        let wrong = "id: 1\ndata: {\"taskId\":\"task-2\",\"contextId\":\"ctx-2\",\"status\":{\"state\":\"working\"},\"final\":false}\n\n";
        assert!(decoder.push(wrong.as_bytes()).is_err());
    }

    #[test]
    fn unwraps_jsonrpc_result_frames() {
        let limits = A2aLimits::default();
        let mut decoder = SseDecoder::new(&limits, Some("task-1".into()));
        let input = concat!(
            "id: 1\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"taskId\":\"task-1\",\"contextId\":\"ctx-1\",\"status\":{\"state\":\"completed\"},\"final\":true,\"kind\":\"status-update\"}}\n",
            "\n"
        );
        let frames = decoder.push(input.as_bytes()).expect("wrapped frame");
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn bounds_are_enforced() {
        let limits = A2aLimits {
            max_sse_event_bytes: 32,
            ..A2aLimits::default()
        };
        let mut decoder = SseDecoder::new(&limits, None);
        let input = format!("data: {}\n\n", "x".repeat(64));
        assert!(matches!(
            decoder.push(input.as_bytes()),
            Err(A2aError::BoundsExceeded { .. })
        ));
    }
}
