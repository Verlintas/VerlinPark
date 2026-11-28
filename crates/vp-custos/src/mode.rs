//! CUSTOS product tier detection.
//!
//! Observer is the default. Advanced is recognized only from a root-owned
//! public marker under `/Library/Application Support/VerlinParkCustos/`.
//! This module is read-only and does not verify the marker signature itself;
//! the root installation owns that stronger check.

use std::fs;
use std::path::{Path, PathBuf};

use vp_protocol::custos::{CustosAdvancedMarker, CustosTier};

pub const ADVANCED_STATE_ROOT: &str = "/Library/Application Support/VerlinParkCustos";
pub const ADVANCED_MARKER_RELATIVE: &str = "public/advanced.json";
pub const MAX_MARKER_BYTES: u64 = 16 * 1024;

/// Default Advanced marker path.
pub fn advanced_marker_path() -> PathBuf {
    Path::new(ADVANCED_STATE_ROOT).join(ADVANCED_MARKER_RELATIVE)
}

/// Detect the active tier from the default root-owned marker path.
pub fn detect_tier() -> CustosTier {
    detect_tier_at(&advanced_marker_path(), true)
}

/// Detect the tier from an explicit marker path. `require_root_owner` is true
/// for production; tests pass false to exercise parsing in a temp directory.
pub fn detect_tier_at(path: &Path, require_root_owner: bool) -> CustosTier {
    match read_marker(path, require_root_owner) {
        Some(marker) if marker.is_compatible() => CustosTier::Advanced,
        _ => CustosTier::Observer,
    }
}

/// Read and validate the bounded marker. A missing, oversized, non-root-owned,
/// group/other-writable, or malformed marker is treated as absent.
pub fn read_marker(path: &Path, require_root_owner: bool) -> Option<CustosAdvancedMarker> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_MARKER_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if require_root_owner && metadata.uid() != 0 {
            return None;
        }
        // Root-owned public markers are normally 0644. Refuse group/other
        // writable files even in tests.
        if metadata.mode() & 0o022 != 0 {
            return None;
        }
    }
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice::<CustosAdvancedMarker>(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use vp_protocol::custos::ADVANCED_MARKER_SCHEMA;

    fn marker() -> CustosAdvancedMarker {
        CustosAdvancedMarker {
            schema: ADVANCED_MARKER_SCHEMA.into(),
            install_id: "install-1".into(),
            build_hash: "build-1".into(),
            charter_hash: "charter-1".into(),
            root_key_id: "key-1".into(),
            created_at: "2026-11-29T00:00:00Z".into(),
            tier: Some("advanced".into()),
            epoch: Some(1),
            installed_at_unix: Some(1_764_460_800),
            signature: Some("sig".into()),
        }
    }

    fn write_marker(path: &Path, value: &CustosAdvancedMarker) {
        let mut file = fs::File::create(path).unwrap();
        file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn missing_marker_is_observer() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            detect_tier_at(&dir.path().join("missing.json"), false),
            CustosTier::Observer
        );
    }

    #[test]
    fn compatible_marker_is_advanced_in_an_injected_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("advanced.json");
        write_marker(&path, &marker());
        assert_eq!(detect_tier_at(&path, false), CustosTier::Advanced);
        assert!(read_marker(&path, false).is_some());
    }

    #[test]
    fn incompatible_or_writable_marker_is_observer() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("advanced.json");
        let mut broken = marker();
        broken.charter_hash.clear();
        write_marker(&path, &broken);
        assert_eq!(detect_tier_at(&path, false), CustosTier::Observer);

        write_marker(&path, &marker());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
            assert_eq!(detect_tier_at(&path, false), CustosTier::Observer);
        }
    }
}
