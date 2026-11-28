//! Root state layout and no-follow ownership/mode validation.
//!
//! The real installation root is
//! `/Library/Application Support/VerlinParkCustos`; every constructor takes the
//! root as a parameter so tests can use a temporary directory. Validation never
//! follows a symlink at a checked path, never uses `unsafe`, and takes the
//! expected owning uid as an argument. Production passes `0`; tests pass the
//! uid that owns their temporary directory.
//!
//! These checks are meaningful only against a non-root attacker. Root/admin can
//! change ownership, modes, and contents, which ADR-0247 records as an explicit
//! non-goal.

use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Production root owner. Tests inject their temporary directory's owner
/// instead; this constant documents the non-negotiable production value.
pub const ROOT_UID: u32 = 0;
/// Default Advanced state root.
pub const ADVANCED_ROOT: &str = "/Library/Application Support/VerlinParkCustos";

/// Root directory: traversable by role users, never writable by non-root.
pub const DIR_ROOT_MODE: u32 = 0o755;
/// Directory holding executables; traversable by role users.
pub const DIR_BIN_MODE: u32 = 0o755;
/// Root-only directory for keys, intents, receipts, audit, state, replay.
pub const DIR_PRIVATE_MODE: u32 = 0o700;
/// Private files such as the audit log, epoch, and receipts.
pub const FILE_PRIVATE_MODE: u32 = 0o600;
/// Public markers and install records.
pub const FILE_MARKER_MODE: u32 = 0o644;
/// Trusted role public keys (readable, not writable).
pub const FILE_PUBLIC_KEY_MODE: u32 = 0o444;

/// The parameterized root state layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootLayout {
    root: PathBuf,
}

impl RootLayout {
    /// Create a layout for an absolute root.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, LayoutError> {
        let root = root.into();
        if root == Path::new("/") {
            return Err(LayoutError::UnsafeRoot(root));
        }
        if !root.is_absolute() {
            return Err(LayoutError::RelativeRoot(root));
        }
        if root
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(LayoutError::ParentComponent(root));
        }
        Ok(Self { root })
    }

    /// The default production root. This does not touch the filesystem.
    pub fn advanced_default() -> Self {
        Self {
            root: PathBuf::from(ADVANCED_ROOT),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// Public read-only markers and install records.
    pub fn public_dir(&self) -> PathBuf {
        self.root.join("public")
    }

    pub fn keys_dir(&self) -> PathBuf {
        self.root.join("keys")
    }

    pub fn intents_dir(&self) -> PathBuf {
        self.root.join("intents")
    }

    pub fn receipts_dir(&self) -> PathBuf {
        self.root.join("receipts")
    }

    pub fn audit_dir(&self) -> PathBuf {
        self.root.join("audit")
    }

    pub fn audit_log(&self) -> PathBuf {
        self.audit_dir().join("audit.jsonl")
    }

    pub fn audit_lock(&self) -> PathBuf {
        self.audit_dir().join("audit.lock")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    pub fn epoch_file(&self) -> PathBuf {
        self.state_dir().join("epoch")
    }

    pub fn replay_dir(&self) -> PathBuf {
        self.root.join("replay")
    }

    pub fn advanced_marker(&self) -> PathBuf {
        self.public_dir().join("advanced.json")
    }

    pub fn safe_mode_marker(&self) -> PathBuf {
        self.public_dir().join("safe-mode.json")
    }

    pub fn install_record(&self) -> PathBuf {
        self.root.join("install.json")
    }

    pub fn public_key_file(&self, role: crate::intent::Role) -> PathBuf {
        self.keys_dir().join(format!("{}.pub", role.as_str()))
    }

    pub fn receipt_file(&self, intent_id: &str) -> PathBuf {
        self.receipts_dir().join(format!("{intent_id}.json"))
    }

    pub fn replay_file(&self, intent_id: &str) -> PathBuf {
        self.replay_dir().join(intent_id)
    }

    /// Create the directory skeleton with the documented modes.
    ///
    /// This is an installer/self-test helper, not an authority check. It never
    /// chowns anything, so a caller running as root must chown the result
    /// before treating it as an installed Advanced root.
    pub fn ensure_directories(&self) -> Result<(), LayoutError> {
        create_dir_mode(&self.root, DIR_ROOT_MODE)?;
        for path in [
            self.bin_dir(),
            self.public_dir(),
            self.keys_dir(),
            self.intents_dir(),
            self.receipts_dir(),
            self.audit_dir(),
            self.state_dir(),
            self.replay_dir(),
        ] {
            let mode = if path == self.bin_dir() {
                DIR_BIN_MODE
            } else if path == self.public_dir() {
                DIR_ROOT_MODE
            } else {
                DIR_PRIVATE_MODE
            };
            create_dir_mode(&path, mode)?;
        }
        Ok(())
    }

    /// Validate every required directory, plus optional files that exist.
    ///
    /// With `require_exists = true`, the root and all private directories must
    /// exist. Files are optional; a missing audit log or marker is normal before
    /// the first append/install.
    pub fn validate(&self, expected_uid: u32, require_exists: bool) -> Result<(), LayoutError> {
        if require_exists || self.root.exists() {
            validate_existing_dir(&self.root, expected_uid, DIR_ROOT_MODE)?;
        } else {
            return Ok(());
        }
        let required_dirs = [
            (self.bin_dir(), DIR_BIN_MODE),
            (self.public_dir(), DIR_ROOT_MODE),
            (self.keys_dir(), DIR_PRIVATE_MODE),
            (self.intents_dir(), DIR_PRIVATE_MODE),
            (self.receipts_dir(), DIR_PRIVATE_MODE),
            (self.audit_dir(), DIR_PRIVATE_MODE),
            (self.state_dir(), DIR_PRIVATE_MODE),
            (self.replay_dir(), DIR_PRIVATE_MODE),
        ];
        for (path, mode) in required_dirs {
            if require_exists || path.exists() {
                validate_existing_dir(&path, expected_uid, mode)?;
            }
        }
        for (path, mode) in [
            (self.audit_log(), FILE_PRIVATE_MODE),
            (self.audit_lock(), FILE_PRIVATE_MODE),
            (self.epoch_file(), FILE_PRIVATE_MODE),
            (self.advanced_marker(), FILE_MARKER_MODE),
            (self.safe_mode_marker(), FILE_MARKER_MODE),
            (self.install_record(), FILE_MARKER_MODE),
        ] {
            if path.exists() || fs::symlink_metadata(&path).is_ok() {
                validate_existing_file(&path, expected_uid, mode)?;
            }
        }
        Ok(())
    }

    /// Read the root-owned monotonic epoch without following symlinks.
    pub fn read_epoch(&self, expected_uid: u32) -> Result<u64, LayoutError> {
        validate_existing_dir(&self.state_dir(), expected_uid, DIR_PRIVATE_MODE)?;
        let metadata = validate_existing_file(&self.epoch_file(), expected_uid, FILE_PRIVATE_MODE)?;
        if metadata.len() > 64 {
            return Err(LayoutError::InvalidEpoch(self.epoch_file()));
        }
        let text = fs::read_to_string(self.epoch_file()).map_err(|source| LayoutError::Io {
            path: self.epoch_file(),
            source,
        })?;
        text.trim()
            .parse::<u64>()
            .map_err(|_| LayoutError::InvalidEpoch(self.epoch_file()))
    }
}

impl Default for RootLayout {
    fn default() -> Self {
        Self::advanced_default()
    }
}

/// Validate a directory: not a symlink, a directory, owned by `expected_uid`,
/// and no mode bits outside `allowed_mode`.
pub fn validate_existing_dir(
    path: &Path,
    expected_uid: u32,
    allowed_mode: u32,
) -> Result<Metadata, LayoutError> {
    let metadata = metadata_no_follow(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(LayoutError::Symlink(path.to_path_buf()));
    }
    if !file_type.is_dir() {
        return Err(LayoutError::NotDirectory(path.to_path_buf()));
    }
    validate_owner_mode(path, &metadata, expected_uid, allowed_mode)?;
    Ok(metadata)
}

/// Validate a regular file: not a symlink, a regular file, owned by
/// `expected_uid`, and no mode bits outside `allowed_mode`.
pub fn validate_existing_file(
    path: &Path,
    expected_uid: u32,
    allowed_mode: u32,
) -> Result<Metadata, LayoutError> {
    let metadata = metadata_no_follow(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(LayoutError::Symlink(path.to_path_buf()));
    }
    if !file_type.is_file() {
        return Err(LayoutError::NotRegularFile(path.to_path_buf()));
    }
    validate_owner_mode(path, &metadata, expected_uid, allowed_mode)?;
    Ok(metadata)
}

fn metadata_no_follow(path: &Path) -> Result<Metadata, LayoutError> {
    fs::symlink_metadata(path).map_err(|source| LayoutError::Missing {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_owner_mode(
    path: &Path,
    metadata: &Metadata,
    expected_uid: u32,
    allowed_mode: u32,
) -> Result<(), LayoutError> {
    let actual_uid = metadata.uid();
    if actual_uid != expected_uid {
        return Err(LayoutError::WrongOwner {
            path: path.to_path_buf(),
            expected: expected_uid,
            actual: actual_uid,
        });
    }
    let mode = metadata.mode() & 0o7777;
    if mode & !allowed_mode != 0 {
        return Err(LayoutError::ModeTooBroad {
            path: path.to_path_buf(),
            mode,
            allowed: allowed_mode,
        });
    }
    Ok(())
}

fn create_dir_mode(path: &Path, mode: u32) -> Result<(), LayoutError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(LayoutError::Symlink(path.to_path_buf()));
        }
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => return Err(LayoutError::NotDirectory(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(LayoutError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(mode);
    builder.create(path).map_err(|source| LayoutError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| LayoutError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Typed layout failures. Error text contains paths and numeric modes; it never
/// contains file contents or secrets.
#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("custos root must not be the filesystem root")]
    UnsafeRoot(PathBuf),
    #[error("custos root must be absolute: {0}")]
    RelativeRoot(PathBuf),
    #[error("custos root must not contain '..': {0}")]
    ParentComponent(PathBuf),
    #[error("custos path is a symlink: {0}")]
    Symlink(PathBuf),
    #[error("custos path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("custos path is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("custos path {path} is owned by uid {actual}, expected uid {expected}")]
    WrongOwner {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    #[error("custos path {path} has mode {mode:04o}, allowed at most {allowed:04o}")]
    ModeTooBroad {
        path: PathBuf,
        mode: u32,
        allowed: u32,
    },
    #[error("custos path is missing or unreadable: {path}: {source}")]
    Missing {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("custos path I/O failed: {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("custos epoch file is invalid: {0}")]
    InvalidEpoch(PathBuf),
    #[error("custos Advanced marker is invalid: {0}")]
    InvalidMarker(PathBuf),
    #[error("custos installer plan is invalid: {0}")]
    PlanInvalid(String),
}
