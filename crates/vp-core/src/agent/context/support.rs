use super::*;

pub(in crate::agent) fn estimate_messages_tokens(messages: &[ChatMessage]) -> u64 {
    messages.iter().fold(0u64, |total, message| {
        let tool_tokens = message
            .tool_calls
            .as_ref()
            .and_then(|calls| serde_json::to_string(calls).ok())
            .map(|calls| estimate_context_tokens(&calls))
            .unwrap_or(0);
        total
            .saturating_add(estimate_context_tokens(&message.content))
            .saturating_add(tool_tokens)
    })
}

pub(in crate::agent) fn estimate_message_tokens(message: &ChatMessage) -> u64 {
    let tool_tokens = message
        .tool_calls
        .as_ref()
        .and_then(|calls| serde_json::to_string(calls).ok())
        .map(|calls| estimate_context_tokens(&calls))
        .unwrap_or(0);
    estimate_context_tokens(&message.content).saturating_add(tool_tokens)
}

/// Deterministic token-aware cut. The retained tail must satisfy both the
/// message-count window and the token window. `keep_tokens == 0` disables the
/// token constraint and preserves the legacy message-count behavior.
pub(in crate::agent) fn safe_context_cut_with_tokens(
    conversation: &[ChatMessage],
    keep_messages: usize,
    keep_tokens: u64,
    floor: usize,
) -> Option<usize> {
    let len = conversation.len();
    if len <= floor.saturating_add(2) {
        return None;
    }
    let keep_messages = keep_messages.max(2);
    let count_cut = len
        .saturating_sub(keep_messages)
        .max(floor.saturating_add(1));
    let token_cut = if keep_tokens == 0 {
        floor.saturating_add(1)
    } else {
        let mut tokens = 0u64;
        let mut index = len;
        while index > floor + 1 {
            let token = estimate_message_tokens(&conversation[index - 1]);
            if tokens.saturating_add(token) > keep_tokens {
                break;
            }
            tokens = tokens.saturating_add(token);
            index -= 1;
        }
        index.max(floor.saturating_add(1))
    };
    let mut proposed = count_cut.max(token_cut);
    if proposed >= len {
        proposed = len.saturating_sub(1);
    }
    if proposed <= floor {
        proposed = floor.saturating_add(1);
    }
    (proposed..len.saturating_sub(1))
        .find(|index| matches!(conversation[*index].role.as_str(), "user" | "assistant"))
}

#[cfg(test)]
pub(in crate::agent) fn safe_context_cut(
    conversation: &[ChatMessage],
    keep: usize,
    floor: usize,
) -> Option<usize> {
    safe_context_cut_with_tokens(conversation, keep, 0, floor)
}

pub(in crate::agent) fn message_cursor_hash(messages: &[ChatMessage], cut: usize) -> String {
    let mut material = String::new();
    for message in messages.iter().take(cut) {
        material.push_str(&message.role);
        material.push('\u{1f}');
        material.push_str(&message.content.len().to_string());
        material.push('\u{1f}');
        material.push_str(&message.content);
        material.push('\u{1f}');
        if let Some(calls) = &message.tool_calls {
            material.push_str(&serde_json::to_string(calls).unwrap_or_default());
        }
        material.push('\u{1f}');
        if let Some(call_id) = &message.tool_call_id {
            material.push_str(call_id);
        }
        material.push('\u{1e}');
    }
    crate::sha256_hex(material.as_bytes())
}

pub(in crate::agent) fn compaction_transcript_budget(
    config: &AgentConfig,
    spec: &AgentSpec,
    calibration: f64,
) -> u64 {
    let summary_window = context_window_for(config, spec.role);
    if summary_window > 0 {
        let budget = summary_window
            .saturating_sub(1024)
            .saturating_sub(estimate_context_tokens(COMPACTION_SYSTEM_PROMPT))
            .saturating_sub(512)
            .max(512);
        if calibration > 1.0 {
            (budget as f64 / calibration).round().max(512.0) as u64
        } else {
            budget
        }
    } else {
        60_000
    }
}
