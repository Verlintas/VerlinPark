//! Read-only Advanced status model.
//!
//! This is the shape a future `custos`/GUI read surface can consume. It only
//! reads the root-owned marker, safe-mode marker, epoch, and audit chain; it
//! never creates a directory, writes a file, starts a service, or repairs state.
//!
//! `Observer` is reported when no Advanced marker exists. A present-but-invalid
//! marker reports `Advanced` with `Degraded` rather than silently claiming
//! Observer, because a tampered installation still needs operator attention.

use std::fs;

use serde::{Deserialize, Serialize};

use crate::audit::verify_audit_chain;
use crate::layout::{FILE_MARKER_MODE, LayoutError, RootLayout, validate_existing_file};
pub use vp_protocol::custos::CustosAdvancedMarker as AdvancedMarker;
pub use vp_protocol::custos::CustosTier;

/// Read-only Advanced state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvancedState {
    NotInstalled,
    Installed,
    Degraded,
    SafeMode,
    Unknown,
}

/// Read-only status snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootStatus {
    pub tier: CustosTier,
    pub advanced_marker: bool,
    pub advanced_marker_valid: bool,
    pub state: AdvancedState,
    pub epoch: Option<u64>,
    pub audit_entries: u64,
    pub audit_head: Option<String>,
    pub issues: Vec<String>,
}

/// Read status without following symlinks or mutating anything.
pub fn read_status(layout: &RootLayout, expected_uid: u32) -> RootStatus {
    let marker_path = layout.advanced_marker();
    let marker_present = fs::symlink_metadata(&marker_path).is_ok();
    if !marker_present {
        return RootStatus {
            tier: CustosTier::Observer,
            advanced_marker: false,
            advanced_marker_valid: false,
            state: AdvancedState::NotInstalled,
            epoch: None,
            audit_entries: 0,
            audit_head: None,
            issues: Vec::new(),
        };
    }

    let mut issues = Vec::new();
    if let Err(error) = layout.validate(expected_uid, true) {
        issues.push(error.to_string());
    }
    let mut marker = None;
    match validate_existing_file(&marker_path, expected_uid, FILE_MARKER_MODE) {
        Ok(metadata) if metadata.len() <= 16 * 1024 => match fs::read(&marker_path) {
            Ok(bytes) => match serde_json::from_slice::<AdvancedMarker>(&bytes) {
                Ok(parsed) => {
                    if parsed.is_compatible() {
                        marker = Some(parsed);
                    } else {
                        issues.push("advanced marker schema/tier is invalid".to_string());
                    }
                }
                Err(error) => issues.push(format!("advanced marker JSON is invalid: {error}")),
            },
            Err(error) => issues.push(format!("advanced marker is unreadable: {error}")),
        },
        Ok(_) => issues.push("advanced marker exceeds 16 KiB".to_string()),
        Err(error) => issues.push(error.to_string()),
    }

    let marker_valid = marker.is_some();
    let epoch = match layout.read_epoch(expected_uid) {
        Ok(epoch) => {
            if let Some(parsed_epoch) = marker.as_ref().and_then(|parsed| parsed.epoch)
                && parsed_epoch != epoch
            {
                issues.push(format!(
                    "marker epoch {parsed_epoch} does not match root epoch {epoch}"
                ));
            }
            Some(epoch)
        }
        Err(error) => {
            issues.push(error.to_string());
            None
        }
    };

    let mut audit_entries = 0;
    let mut audit_head = None;
    match verify_audit_chain(layout, expected_uid) {
        Ok(chain) => {
            audit_entries = chain.entries;
            if chain.entries > 0 {
                audit_head = Some(chain.head_hash);
            }
        }
        Err(error) => issues.push(format!("audit chain is invalid: {error}")),
    }

    let safe_mode_path = layout.safe_mode_marker();
    let safe_mode_present = fs::symlink_metadata(&safe_mode_path).is_ok();
    let mut safe_mode_valid = false;
    if safe_mode_present {
        match validate_existing_file(&safe_mode_path, expected_uid, FILE_MARKER_MODE) {
            Ok(_) => safe_mode_valid = true,
            Err(error) => issues.push(format!("safe-mode marker is invalid: {error}")),
        }
    }

    let state = if safe_mode_valid {
        AdvancedState::SafeMode
    } else if marker_valid && issues.is_empty() {
        AdvancedState::Installed
    } else {
        AdvancedState::Degraded
    };

    RootStatus {
        tier: CustosTier::Advanced,
        advanced_marker: true,
        advanced_marker_valid: marker_valid,
        state,
        epoch,
        audit_entries,
        audit_head,
        issues,
    }
}

/// Convenience validation for callers that need the marker's fields.
pub fn read_marker(layout: &RootLayout, expected_uid: u32) -> Result<AdvancedMarker, LayoutError> {
    let metadata =
        validate_existing_file(&layout.advanced_marker(), expected_uid, FILE_MARKER_MODE)?;
    if metadata.len() > 16 * 1024 {
        return Err(LayoutError::InvalidMarker(layout.advanced_marker()));
    }
    let bytes = fs::read(layout.advanced_marker()).map_err(|source| LayoutError::Io {
        path: layout.advanced_marker(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|_| LayoutError::InvalidMarker(layout.advanced_marker()))
}
