//! Deterministic, bounded redaction for persisted tool/event/approval payloads.
//!
//! This is a persistence-boundary filter, not a runtime argument filter. The
//! real tool input still reaches the tool, effect probe, contract check, and
//! run-journal fingerprint; only the copy stored in an event or approval row is
//! sanitized. The helper is deliberately conservative and idempotent so replay,
//! repair, and bundle round-trips do not keep rewriting payloads.
//!
//! Two passes are applied to every string:
//!
//! 1. Key-name redaction for JSON objects (`authorization`, `api_key`, `token`,
//!    `password`, `secret`, `cookie`, ...). The key is kept; only the value is
//!    replaced.
//! 2. Text redaction through the existing [`super::errors`] filters, extended
//!    with URL query parameters, cookie headers, JWT/`vpg_`/`sk_live_` shapes,
//!    and common assignments such as `API_KEY=...`.
//!
//! Depth and per-string size are bounded. Strings above the size cap are
//! truncated before matching, so an accidental multi-megabyte file body in a
//! tool argument cannot turn event persistence into unbounded regex work.

use serde_json::Value;

use super::errors::redact_secrets_for_display;

/// Maximum object/array nesting depth that is inspected. Deeper subtrees are
/// replaced with a marker instead of being persisted uninspected.
pub const MAX_REDACTION_DEPTH: usize = 16;
/// Maximum characters inspected in one string value. Longer values are
/// truncated before matching and marked as bounded.
pub const MAX_REDACTION_TEXT_CHARS: usize = 262_144;

const REDACTED: &str = "[redacted]";
const DEPTH_LIMIT: &str = "[redacted:depth-limit]";
const TEXT_LIMIT: &str = "\n… [redaction bounded]";

/// Redact one persisted JSON payload in place. Key names and non-sensitive
/// scalar/structural fields are preserved.
pub fn redact_persisted_payload(value: &mut Value) {
    redact_value(value, 0);
}

/// Clone and redact a JSON payload. Use this when the caller still needs the
/// original value for execution, approval evaluation, or an effect probe.
pub fn redact_persisted_value(value: &Value) -> Value {
    let mut redacted = value.clone();
    redact_persisted_payload(&mut redacted);
    redacted
}

/// Redact one string for persistence. This reuses the tool-failure redaction
/// grammar and is safe to apply more than once.
pub fn redact_persisted_text(text: &str) -> String {
    let bounded = bound_text(text);
    redact_secrets_for_display(bounded.as_ref())
}

fn redact_value(value: &mut Value, depth: usize) {
    if depth > MAX_REDACTION_DEPTH {
        *value = Value::String(DEPTH_LIMIT.to_string());
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if is_sensitive_key(key)
                    && matches!(child, Value::String(_) | Value::Array(_) | Value::Object(_))
                {
                    *child = Value::String(REDACTED.to_string());
                } else {
                    redact_value(child, depth + 1);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                redact_value(item, depth + 1);
            }
        }
        Value::String(text) => {
            *text = redact_persisted_text(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Key names that identify credential material. Normalization folds case and
/// `-`/`_` so `Authorization`, `authorization`, and `X-Api-Key` all match.
/// Deliberately excludes a bare `key`; URL query parameters handle that case
/// without turning ordinary object keys into false positives.
fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxy_authorization"
            | "cookie"
            | "set_cookie"
            | "password"
            | "passwd"
            | "secret"
            | "client_secret"
            | "token"
            | "access_token"
            | "refresh_token"
            | "session_token"
            | "id_token"
            | "api_key"
            | "apikey"
            | "private_key"
            | "access_key_id"
            | "secret_access_key"
            | "aws_access_key_id"
            | "aws_secret_access_key"
            | "credential"
            | "credentials"
            | "signature"
    ) || normalized.ends_with("_secret")
        || normalized.ends_with("_token")
        || normalized.ends_with("_password")
        || normalized.ends_with("_credential")
        || normalized.ends_with("_api_key")
}

fn bound_text(text: &str) -> std::borrow::Cow<'_, str> {
    match text.char_indices().nth(MAX_REDACTION_TEXT_CHARS) {
        Some((index, _)) => {
            let mut bounded = String::with_capacity(index + TEXT_LIMIT.len());
            bounded.push_str(&text[..index]);
            bounded.push_str(TEXT_LIMIT);
            std::borrow::Cow::Owned(bounded)
        }
        None => std::borrow::Cow::Borrowed(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_values_are_redacted_without_renaming_keys() {
        let mut payload = json!({
            "input": {
                "headers": {
                    "Authorization": "Bearer abcdefghijklmnop",
                    "X-Api-Key": "key-value",
                    "Accept": "application/json"
                },
                "password": "hunter2",
                "nested": [{"refresh-token": "refresh-value"}, {"label": "keep me"}],
            }
        });
        redact_persisted_payload(&mut payload);
        assert_eq!(
            payload["input"]["headers"]["Authorization"],
            json!("[redacted]")
        );
        assert_eq!(
            payload["input"]["headers"]["X-Api-Key"],
            json!("[redacted]")
        );
        assert_eq!(
            payload["input"]["headers"]["Accept"],
            json!("application/json")
        );
        assert_eq!(payload["input"]["password"], json!("[redacted]"));
        assert_eq!(
            payload["input"]["nested"][0]["refresh-token"],
            json!("[redacted]")
        );
        assert_eq!(payload["input"]["nested"][1]["label"], json!("keep me"));
        assert!(payload["input"].get("headers").is_some());
    }

    #[test]
    fn text_redaction_is_idempotent_and_preserves_ordinary_text() {
        let original = concat!(
            "API_KEY=sk_live_abcdefgh1234\n",
            "token: abc123\n",
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature123\n",
            "Cookie: session=secret-cookie; theme=dark\n",
            "headers={\"Cookie\":\"session=embedded-cookie\"}\n",
            "url=https://user:pass@example.test/path?token=query-secret&keep=1\n",
            "let token_count = 3; // ordinary code\n"
        );
        let once = redact_persisted_text(original);
        let twice = redact_persisted_text(&once);
        assert_eq!(once, twice);
        assert!(!once.contains("sk_live_abcdefgh1234"));
        assert!(!once.contains("abc123"));
        assert!(!once.contains("eyJhbGciOiJIUzI1NiJ9"));
        assert!(!once.contains("secret-cookie"));
        assert!(!once.contains("embedded-cookie"));
        assert!(!once.contains("user:pass"));
        assert!(!once.contains("query-secret"));
        assert!(once.contains("keep=1"));
        assert!(once.contains("let token_count = 3;"));
        assert!(once.contains("[redacted]"));
    }
}
