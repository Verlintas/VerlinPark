use super::*;

// Token counts are only exact after a provider reports usage. To keep parallel
// calls from all passing the same pre-flight check, reserve a conservative
// estimate up front: roughly two UTF-8 bytes per token, plus a small per-request
// overhead. The provider's reported usage replaces the estimate on settlement.
pub(super) fn estimate_text_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(2).max(1)
}

pub(super) fn estimate_output_tokens(text: &str, tool_calls: &[ToolCall]) -> u64 {
    estimate_text_tokens(text).saturating_add(
        serde_json::to_string(tool_calls)
            .map(|calls| estimate_text_tokens(&calls))
            .unwrap_or(0),
    )
}

/// Context-window estimates should be closer to the tokenizer than the
/// conservative budget reservation above. English/code averages ~4 ASCII
/// characters per token; CJK and other non-ASCII text is roughly 1.5 tokens
/// per character. This avoids compacting a 1M-token model at half its usable
/// window while still leaving headroom for tokenizer differences.
pub(super) fn estimate_context_tokens(text: &str) -> u64 {
    let mut ascii = 0u64;
    let mut non_ascii = 0u64;
    for character in text.chars() {
        if character.is_ascii() {
            ascii = ascii.saturating_add(1);
        } else {
            non_ascii = non_ascii.saturating_add(1);
        }
    }
    ascii
        .div_ceil(4)
        .saturating_add(non_ascii.saturating_mul(3).div_ceil(2))
        .max(1)
}

pub(super) const MAX_CONTEXT_CALIBRATION: f64 = 2.0;

/// Provider usage is the only exact token count we get. The heuristic estimate
/// is usually close, but CJK text, tool schemas, and provider-side formatting
/// can make it low. Calibration therefore only scales estimates upward: a
/// provider that reports fewer input tokens than the heuristic never makes
/// compaction later than it already was.
#[derive(Clone, Copy, Debug)]
pub(super) struct ContextCalibration {
    pub(super) factor: f64,
    pub(super) ratio: f64,
    pub(super) samples: u64,
}

impl Default for ContextCalibration {
    fn default() -> Self {
        Self {
            factor: 1.0,
            ratio: 1.0,
            samples: 0,
        }
    }
}

impl ContextCalibration {
    pub(super) fn observe(&mut self, estimated: u64, actual: u64) {
        if estimated == 0 || actual == 0 {
            return;
        }
        let sample = (actual as f64 / estimated as f64).clamp(1.0, 4.0);
        self.ratio = if self.samples == 0 {
            sample
        } else {
            (self.ratio * 0.7 + sample * 0.3).min(MAX_CONTEXT_CALIBRATION * 2.0)
        };
        self.factor = self.ratio.clamp(1.0, MAX_CONTEXT_CALIBRATION);
        self.samples = self.samples.saturating_add(1).min(1000);
    }
}

pub(super) fn calibrate_context_tokens(tokens: u64, factor: f64) -> u64 {
    if !factor.is_finite() || factor <= 1.0 {
        return tokens;
    }
    let scaled = (tokens as f64 * factor.clamp(1.0, MAX_CONTEXT_CALIBRATION)).round();
    if scaled.is_finite() && scaled > tokens as f64 {
        scaled.min(u64::MAX as f64) as u64
    } else {
        tokens
    }
}

pub(super) async fn load_context_calibration(
    engine: &Engine,
    session_id: &str,
    model: &str,
) -> ContextCalibration {
    let Ok(Some(event)) = engine
        .latest_event_of_kind(session_id, "context.measured")
        .await
    else {
        return ContextCalibration::default();
    };
    let Ok(measurement) = serde_json::from_value::<ContextMeasurement>(event.payload) else {
        return ContextCalibration::default();
    };
    if measurement.model != model || !measurement.calibration.is_finite() {
        return ContextCalibration::default();
    }
    let factor = measurement.calibration.clamp(1.0, MAX_CONTEXT_CALIBRATION);
    ContextCalibration {
        factor,
        ratio: factor,
        samples: measurement.samples,
    }
}

pub(super) fn estimate_request_tokens(request: &ChatRequest) -> u64 {
    let mut bytes = request.system.as_deref().map_or(0, str::len);
    for message in &request.messages {
        bytes = bytes.saturating_add(message.content.len());
        if let Some(tool_calls) = &message.tool_calls {
            for call in tool_calls {
                bytes = bytes
                    .saturating_add(call.name.len())
                    .saturating_add(call.arguments.len());
            }
        }
    }
    for tool in &request.tools {
        bytes = bytes
            .saturating_add(tool.name.len())
            .saturating_add(tool.description.len());
        if let Ok(parameters) = serde_json::to_string(&tool.parameters) {
            bytes = bytes.saturating_add(parameters.len());
        }
    }
    (bytes as u64).div_ceil(2).saturating_add(64)
}

pub(super) fn string_list(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(8)
                .map(|item| item.chars().take(200).collect())
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn truncate_to(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect::<String>() + "…"
    }
}
