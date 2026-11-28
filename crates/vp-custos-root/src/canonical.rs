//! Deterministic canonical JSON used by intent signatures and audit hashes.
//!
//! The encoder sorts object keys, preserves array order, rejects non-finite
//! numbers (which `serde_json` already cannot represent), and never emits
//! insignificant whitespace. All security-relevant structs are serialized
//! through this function, so a signer and verifier cannot disagree about
//! formatting.

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CanonicalError {
    #[error("serialize canonical JSON: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("canonical JSON does not support this number")]
    UnsupportedNumber,
}

/// Serialize a value as canonical JSON bytes.
pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    let value = serde_json::to_value(value)?;
    let mut out = Vec::new();
    write_value(&value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => {
            if number.is_f64() && number.as_f64().is_some_and(|value| !value.is_finite()) {
                return Err(CanonicalError::UnsupportedNumber);
            }
            out.extend_from_slice(number.to_string().as_bytes());
        }
        Value::String(text) => {
            let encoded = serde_json::to_string(text)?;
            out.extend_from_slice(encoded.as_bytes());
        }
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_value(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                let encoded = serde_json::to_string(key)?;
                out.extend_from_slice(encoded.as_bytes());
                out.push(b':');
                if let Some(item) = map.get(key) {
                    write_value(item, out)?;
                }
            }
            out.push(b'}');
        }
    }
    Ok(())
}

/// SHA-256 of a byte slice as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    hex_encode(&digest)
}

/// Lowercase hex encoding without an external dependency.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// True when `text` is lowercase hex with an even length in `min..=max`.
pub fn is_lower_hex(text: &str, min: usize, max: usize) -> bool {
    text.len() >= min
        && text.len() <= max
        && text.len().is_multiple_of(2)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_key_order_is_canonical() {
        let first: Value = serde_json::from_str(r#"{"b":2,"a":1,"c":{"z":true,"y":[1,2]}}"#)
            .expect("valid first JSON");
        let second: Value = serde_json::from_str(r#"{"c":{"y":[1,2],"z":true},"a":1,"b":2}"#)
            .expect("valid second JSON");
        assert_eq!(
            canonical_json(&first).expect("canonical first"),
            canonical_json(&second).expect("canonical second")
        );
        assert_eq!(
            String::from_utf8(canonical_json(&first).expect("canonical first"))
                .expect("utf8 canonical JSON"),
            r#"{"a":1,"b":2,"c":{"y":[1,2],"z":true}}"#
        );
    }

    #[test]
    fn hex_validation_is_strict() {
        assert!(is_lower_hex("00ff", 4, 4));
        assert!(!is_lower_hex("00FF", 4, 4));
        assert!(!is_lower_hex("abc", 2, 8));
        assert!(!is_lower_hex("zz", 2, 2));
    }
}
