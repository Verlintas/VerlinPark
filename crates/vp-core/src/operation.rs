//! Idempotent session operation receipts.
//!
//! A receipt binds a caller-supplied `operation_id` to a canonical
//! fingerprint of the method and payload. It is operational state, like a
//! runtime lease: it prevents duplicate side effects but is not a session
//! event or a replayed projection.

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use vp_store::Store;
use vp_store::store::Claim;

const MAX_OPERATION_ID_CHARS: usize = 200;
const MAX_ERROR_CHARS: usize = 4_000;

pub(crate) enum OperationStart {
    /// Proceed with the operation. `Some` means a receipt must be completed
    /// or failed before returning; `None` means no operation id was supplied.
    Proceed(Option<OperationReceipt>),
    /// The operation already completed with the same method and payload.
    Replay(String),
}

pub(crate) struct OperationReceipt {
    store: Store,
    session_id: String,
    operation_id: String,
}

pub(crate) fn normalize_operation_id(operation_id: Option<&str>) -> Result<Option<String>> {
    let Some(operation_id) = operation_id else {
        return Ok(None);
    };
    let operation_id = operation_id.trim();
    if operation_id.is_empty() || operation_id.chars().count() > MAX_OPERATION_ID_CHARS {
        anyhow::bail!("operation_id must be 1-{MAX_OPERATION_ID_CHARS} non-blank characters");
    }
    if operation_id.chars().any(char::is_control) {
        anyhow::bail!("operation_id must not contain control characters");
    }
    Ok(Some(operation_id.to_string()))
}

/// SHA-256 over the method and a canonical JSON payload. Object keys are
/// sorted recursively so wire-map order and field order cannot change the
/// fingerprint. The operation id itself is never part of the payload.
pub(crate) fn fingerprint(method: &str, payload: &Value) -> Result<String> {
    let canonical = serde_json::to_string(&canonical_value(payload))
        .context("serializing canonical operation payload")?;
    let mut hasher = Sha256::new();
    hasher.update(method.as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key.clone(), canonical_value(value));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

pub(crate) async fn begin(
    store: &Store,
    session_id: &str,
    operation_id: Option<&str>,
    method: &str,
    payload: &Value,
) -> Result<OperationStart> {
    let Some(operation_id) = normalize_operation_id(operation_id)? else {
        return Ok(OperationStart::Proceed(None));
    };
    let fingerprint = fingerprint(method, payload)?;
    let session_id = session_id.to_string();
    let method = method.to_string();
    let claim_store = store.clone();
    let claim_session = session_id.clone();
    let claim_operation = operation_id.clone();
    let claim = tokio::task::spawn_blocking(move || {
        claim_store.claim_session_operation(&claim_session, &claim_operation, &method, &fingerprint)
    })
    .await??;
    match claim {
        Claim::New => Ok(OperationStart::Proceed(Some(OperationReceipt {
            store: store.clone(),
            session_id,
            operation_id,
        }))),
        Claim::Replay(result) => Ok(OperationStart::Replay(result)),
        Claim::InProgress => Err(anyhow!(
            "operation {operation_id} is already in progress for session {session_id}; not starting a duplicate"
        )),
        Claim::Conflict => Err(anyhow!(
            "operation id {operation_id} was already used for session {session_id} with a different method or payload"
        )),
        Claim::Unknown => Err(anyhow!(
            "operation {operation_id} has an unknown outcome after a daemon restart; inspect session {session_id} before retrying"
        )),
    }
}

pub(crate) fn replay<T: DeserializeOwned>(result_json: &str) -> Result<T> {
    serde_json::from_str(result_json).context("stored session operation result is invalid")
}

pub(crate) async fn complete<T: Serialize>(
    receipt: &Option<OperationReceipt>,
    result: &T,
) -> Result<()> {
    if let Some(receipt) = receipt {
        receipt.complete(result).await?;
    }
    Ok(())
}

pub(crate) async fn fail(receipt: &Option<OperationReceipt>, error: &str) {
    let Some(receipt) = receipt else {
        return;
    };
    if let Err(receipt_error) = receipt.fail(error).await {
        tracing::warn!(%receipt_error, "failed to record a failed session operation");
    }
}

impl OperationReceipt {
    pub(crate) async fn complete<T: Serialize>(&self, result: &T) -> Result<()> {
        let result_json =
            serde_json::to_string(result).context("serializing session operation result")?;
        let store = self.store.clone();
        let session_id = self.session_id.clone();
        let operation_id = self.operation_id.clone();
        tokio::task::spawn_blocking(move || {
            store.complete_session_operation(&session_id, &operation_id, &result_json)
        })
        .await?
    }

    pub(crate) async fn fail(&self, error: &str) -> Result<()> {
        let error = error.chars().take(MAX_ERROR_CHARS).collect::<String>();
        let store = self.store.clone();
        let session_id = self.session_id.clone();
        let operation_id = self.operation_id.clone();
        tokio::task::spawn_blocking(move || {
            store.fail_session_operation(&session_id, &operation_id, &error)
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprints_sort_object_keys_and_include_the_method() {
        let first = json!({
            "session_id": "ses-1",
            "content": "hello",
            "nested": {"beta": 2, "alpha": 1}
        });
        let second = json!({
            "nested": {"alpha": 1, "beta": 2},
            "content": "hello",
            "session_id": "ses-1"
        });
        assert_eq!(
            fingerprint("session.prompt", &first).unwrap(),
            fingerprint("session.prompt", &second).unwrap()
        );
        assert_ne!(
            fingerprint("session.prompt", &first).unwrap(),
            fingerprint("session.steer", &first).unwrap()
        );
    }

    #[test]
    fn operation_ids_are_trimmed_and_bounded() {
        assert_eq!(
            normalize_operation_id(Some("  op-1  ")).unwrap().as_deref(),
            Some("op-1")
        );
        assert!(normalize_operation_id(Some("   ")).is_err());
        assert!(normalize_operation_id(Some(&"x".repeat(201))).is_err());
    }
}
