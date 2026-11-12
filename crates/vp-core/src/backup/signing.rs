//! Ed25519 backup signing keys, keyring storage, and rotation.
//!
//! # What this module owns
//!
//! - A `keyring.json` in a user-controlled key directory (0600) that stores
//!   key ids, public keys, secret *references*, creation times, and which key
//!   is active. Private key bytes are never stored in the keyring.
//! - 0600 signing-key files with a single Ed25519 seed, or an environment
//!   variable reference that is resolved at signing time.
//! - Ed25519 signing and verification helpers with explicit domain separation.
//! - Rotation: promote an existing key or generate a new one; retired public
//!   keys stay in the keyring so old snapshots remain verifiable.
//!
//! # Secret handling
//!
//! Private material lives only in [`SigningKeyMaterial`], which is zeroized on
//! drop and has a redacted `Debug`. No report, serialized keyring entry, error,
//! or log line contains a seed. [`PublicKey`] is public by definition.
//!
//! The keyring is intentionally outside the backed-up data directory by
//! default: restoring a data directory must never delete or replace signing
//! keys. `VERLINPARK_BACKUP_KEY_DIR` overrides the location for tests and
//! operators who keep keys on removable media.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::RngCore;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use vp_protocol::now_rfc3339;
use zeroize::Zeroizing;

use super::error::BackupSecurityError;

pub const KEYRING_FILENAME: &str = "keyring.json";
pub const KEYRING_EVENTS_FILENAME: &str = "keyring-events.jsonl";
pub const KEYRING_KIND: &str = "verlinpark-backup-keyring";
pub const KEYRING_FORMAT_VERSION: u32 = 1;
pub const KEY_FILE_KIND: &str = "verlinpark-backup-signing-key";
pub const KEY_FILE_FORMAT_VERSION: u32 = 1;
pub const SIGNING_ALGORITHM: &str = "ed25519";
pub const PUBLIC_KEY_BYTES: usize = 32;
pub const SEED_BYTES: usize = 32;
pub const SIGNATURE_BYTES: usize = 64;
pub const MAX_KEY_ID_BYTES: usize = 64;
pub const MAX_KEYRING_KEYS: usize = 64;
pub const MAX_KEYRING_BYTES: u64 = 1024 * 1024;
pub const MAX_KEY_FILE_BYTES: u64 = 8 * 1024;
pub const MAX_SECRET_REFERENCE_BYTES: usize = 160;
pub const MAX_KEYRING_AUDIT_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_KEYRING_AUDIT_EVENTS: usize = 5000;

/// Domain separator for repository metadata signatures.
pub const METADATA_SIGNATURE_DOMAIN: &[u8] = b"verlinpark-backup-metadata-signature-v1\0";
/// Domain separator for snapshot manifest signatures.
pub const SNAPSHOT_SIGNATURE_DOMAIN: &[u8] = b"verlinpark-backup-snapshot-signature-v1\0";
/// Domain separator for portable anti-rollback anchor documents.
pub const PORTABLE_ANCHOR_SIGNATURE_DOMAIN: &[u8] =
    b"verlinpark-backup-portable-anchor-signature-v1\0";

/// Default key directory, deliberately independent of the daemon data dir.
pub fn default_key_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("VERLINPARK_BACKUP_KEY_DIR") {
        return PathBuf::from(path);
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".verlinpark").join("backup-keys");
    }
    vp_store::paths::data_dir().join("backup-keys")
}

/// Validate a key id used in keyring entries, manifests, and CLI arguments.
///
/// Key ids are lowercase identifiers, not paths. They must be stable across
/// rotation because old snapshots keep referring to the key that signed them.
pub fn validate_key_id(key_id: &str) -> Result<()> {
    if key_id.is_empty() {
        bail!("backup signing key id must not be empty");
    }
    if key_id.len() > MAX_KEY_ID_BYTES {
        bail!(
            "backup signing key id is {} bytes; the limit is {MAX_KEY_ID_BYTES}",
            key_id.len()
        );
    }
    if key_id.starts_with('.') || key_id.starts_with('-') || key_id.ends_with('.') {
        bail!("backup signing key id {key_id:?} must not start with '.' or '-' or end with '.'");
    }
    if !key_id
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
    {
        bail!(
            "backup signing key id {key_id:?} must use lowercase ASCII letters, digits, '.', '_', or '-'"
        );
    }
    if key_id.contains("..") {
        bail!("backup signing key id {key_id:?} must not contain '..'");
    }
    Ok(())
}

fn validate_environment_name(variable: &str) -> Result<()> {
    if variable.is_empty() || variable.len() > 128 {
        bail!("backup signing-key environment variable name is malformed");
    }
    if !variable
        .bytes()
        .enumerate()
        .all(|(index, byte)| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => true,
            b'0'..=b'9' => index > 0,
            _ => false,
        })
    {
        bail!("backup signing-key environment variable name is malformed");
    }
    Ok(())
}

/// A public Ed25519 key bound to a stable key id.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicKey {
    pub key_id: String,
    /// Standard base64 of the 32-byte Ed25519 public key.
    pub public_key_b64: String,
    pub algorithm: String,
}

impl PublicKey {
    pub fn new(key_id: impl Into<String>, bytes: &[u8; PUBLIC_KEY_BYTES]) -> Result<Self> {
        let key_id = key_id.into();
        validate_key_id(&key_id)?;
        Ok(Self {
            key_id,
            public_key_b64: BASE64.encode(bytes),
            algorithm: SIGNING_ALGORITHM.to_string(),
        })
    }

    pub fn from_base64(key_id: impl Into<String>, encoded: &str) -> Result<Self> {
        let key_id = key_id.into();
        validate_key_id(&key_id)?;
        let bytes = decode_public_key(encoded)?;
        Ok(Self {
            key_id,
            public_key_b64: BASE64.encode(bytes),
            algorithm: SIGNING_ALGORITHM.to_string(),
        })
    }

    pub fn bytes(&self) -> Result<[u8; PUBLIC_KEY_BYTES]> {
        decode_public_key(&self.public_key_b64)
    }

    pub fn fingerprint(&self) -> Result<String> {
        Ok(super::crypto::sha256_hex(&self.bytes()?))
    }

    fn validate(&self) -> Result<()> {
        validate_key_id(&self.key_id)?;
        if self.algorithm != SIGNING_ALGORITHM {
            bail!(
                "backup signing key {:?} uses unsupported algorithm {:?}",
                self.key_id,
                self.algorithm
            );
        }
        let bytes = self.bytes()?;
        if BASE64.encode(bytes) != self.public_key_b64 {
            bail!(
                "backup signing key {:?} public key is not canonical base64",
                self.key_id
            );
        }
        Ok(())
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicKey")
            .field("key_id", &self.key_id)
            .field("algorithm", &self.algorithm)
            .field("public_key_b64", &self.public_key_b64)
            .finish()
    }
}

fn decode_public_key(encoded: &str) -> Result<[u8; PUBLIC_KEY_BYTES]> {
    if encoded.len() > 128 {
        bail!("backup signing public key is too large");
    }
    let bytes = BASE64
        .decode(encoded.as_bytes())
        .context("backup signing public key is not valid base64")?;
    let bytes: [u8; PUBLIC_KEY_BYTES] = bytes
        .try_into()
        .map_err(|_| anyhow!("backup signing public key must be {PUBLIC_KEY_BYTES} bytes"))?;
    if BASE64.encode(bytes) != encoded {
        bail!("backup signing public key is not canonical base64");
    }
    Ok(bytes)
}

fn decode_seed(encoded: &str) -> Result<Zeroizing<[u8; SEED_BYTES]>> {
    if encoded.len() > 128 {
        bail!("backup signing seed is too large");
    }
    let bytes = BASE64
        .decode(encoded.as_bytes())
        .context("backup signing seed is not valid base64")?;
    let bytes: [u8; SEED_BYTES] = bytes
        .try_into()
        .map_err(|_| anyhow!("backup signing seed must be {SEED_BYTES} bytes"))?;
    if BASE64.encode(bytes) != encoded {
        bail!("backup signing seed is not canonical base64");
    }
    Ok(Zeroizing::new(bytes))
}

/// Where private signing material is resolved from.
///
/// Only references are serialized. `Environment` is resolved from the daemon
/// or CLI process at signing time; `File` is relative to the key directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretSource {
    File { name: String },
    Environment { variable: String },
}

impl SecretSource {
    fn validate(&self) -> Result<()> {
        match self {
            Self::File { name } => {
                if name.is_empty()
                    || name.len() > MAX_SECRET_REFERENCE_BYTES
                    || name.contains('/')
                    || name.contains('\\')
                    || name == "."
                    || name == ".."
                    || name.starts_with('.')
                    || name.contains('\0')
                {
                    bail!("backup signing key file reference is malformed");
                }
            }
            Self::Environment { variable } => validate_environment_name(variable)?,
        }
        Ok(())
    }

    fn resolved_path(&self, directory: &Path) -> Option<PathBuf> {
        match self {
            Self::File { name } => Some(directory.join(name)),
            Self::Environment { .. } => None,
        }
    }
}

/// One keyring entry. The private key is never part of this structure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyringEntry {
    pub key_id: String,
    pub algorithm: String,
    pub public_key_b64: String,
    pub source: SecretSource,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<String>,
    /// When set, signatures made by this key are rejected for new verification
    /// and restore operations unless the operator passes `--allow-revoked`.
    /// The public key is retained so historical snapshots remain inspectable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

impl KeyringEntry {
    fn public_key(&self) -> Result<PublicKey> {
        let key = PublicKey {
            key_id: self.key_id.clone(),
            public_key_b64: self.public_key_b64.clone(),
            algorithm: self.algorithm.clone(),
        };
        key.validate()?;
        Ok(key)
    }

    fn validate(&self) -> Result<()> {
        self.public_key()?;
        self.source.validate()?;
        validate_timestamp(&self.created_at, "key creation")?;
        if let Some(retired_at) = &self.retired_at {
            validate_timestamp(retired_at, "key retirement")?;
        }
        if let Some(revoked_at) = &self.revoked_at {
            validate_timestamp(revoked_at, "key revocation")?;
        }
        Ok(())
    }
}

fn validate_timestamp(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 64 {
        bail!("backup keyring {label} timestamp is malformed");
    }
    Ok(())
}

/// Parsed `keyring.json`.
#[derive(Clone, Debug)]
pub struct Keyring {
    directory: PathBuf,
    active_key_id: Option<String>,
    updated_at: String,
    entries: Vec<KeyringEntry>,
    /// Runtime-only trust policy. Not serialized: a keyring file never grants
    /// itself permission to accept revoked keys.
    allow_revoked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyringFile {
    kind: String,
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_key_id: Option<String>,
    updated_at: String,
    #[serde(default)]
    keys: Vec<KeyringEntry>,
}

/// Public, secret-free summary used by reports and the daemon status RPC.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyringSummary {
    pub path: String,
    pub active_key_id: Option<String>,
    pub key_count: usize,
    /// Additive: number of keys whose signatures are refused by default.
    #[serde(default)]
    pub revoked_key_count: usize,
    pub keys: Vec<KeyringKeySummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyringKeySummary {
    pub key_id: String,
    pub algorithm: String,
    pub public_key_b64: String,
    pub created_at: String,
    pub retired_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    pub active: bool,
}

impl Keyring {
    /// Load an existing keyring. Missing keyring is an actionable error.
    pub fn load(directory: &Path) -> Result<Self> {
        Self::load_optional(directory)?.ok_or_else(|| {
            BackupSecurityError::KeyringNotFound {
                path: directory.join(KEYRING_FILENAME).display().to_string(),
            }
            .into()
        })
    }

    /// Load a keyring if present.
    pub fn load_optional(directory: &Path) -> Result<Option<Self>> {
        let path = directory.join(KEYRING_FILENAME);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BackupSecurityError::KeyringInvalid {
                    reason: format!("keyring {} is a symlink", path.display()),
                }
                .into());
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(BackupSecurityError::KeyringInvalid {
                    reason: format!("keyring {} is not a regular file", path.display()),
                }
                .into());
            }
            Ok(metadata) => check_private_mode(&path, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        }
        if fs::metadata(&path)?.len() > MAX_KEYRING_BYTES {
            return Err(BackupSecurityError::KeyringInvalid {
                reason: "keyring exceeds its size limit".to_string(),
            }
            .into());
        }
        let mut bytes = Vec::new();
        File::open(&path)
            .with_context(|| format!("opening {}", path.display()))?
            .take(MAX_KEYRING_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading {}", path.display()))?;
        if bytes.len() as u64 > MAX_KEYRING_BYTES {
            return Err(BackupSecurityError::KeyringInvalid {
                reason: "keyring grew beyond its size limit".to_string(),
            }
            .into());
        }
        let file: KeyringFile = serde_json::from_slice(&bytes).map_err(|error| {
            BackupSecurityError::KeyringInvalid {
                reason: format!("keyring JSON is invalid: {error}"),
            }
        })?;
        let keyring = Self {
            directory: directory.to_path_buf(),
            active_key_id: file.active_key_id,
            updated_at: file.updated_at,
            entries: file.keys,
            allow_revoked: false,
        };
        keyring.validate_with_file_kind(&file.kind, file.version)?;
        Ok(Some(keyring))
    }

    /// Create a new empty keyring. Refuses to overwrite an existing one.
    pub fn create(directory: &Path) -> Result<Self> {
        vp_store::paths::ensure_private_dir(directory)
            .with_context(|| format!("creating backup key directory {}", directory.display()))?;
        if directory.join(KEYRING_FILENAME).exists() {
            return Err(BackupSecurityError::KeyringInvalid {
                reason: format!(
                    "keyring already exists at {}",
                    directory.join(KEYRING_FILENAME).display()
                ),
            }
            .into());
        }
        let keyring = Self {
            directory: directory.to_path_buf(),
            active_key_id: None,
            updated_at: now_rfc3339(),
            entries: Vec::new(),
            allow_revoked: false,
        };
        keyring.save()?;
        Ok(keyring)
    }

    /// Load or create an empty keyring.
    pub fn load_or_create(directory: &Path) -> Result<Self> {
        match Self::load_optional(directory)? {
            Some(keyring) => Ok(keyring),
            None => Self::create(directory),
        }
    }

    /// Load from a keyring file path, using its parent as the key directory.
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let directory = path
            .parent()
            .ok_or_else(|| anyhow!("backup keyring path must have a parent directory"))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("backup keyring path is not valid UTF-8"))?;
        if name != KEYRING_FILENAME {
            bail!("backup keyring file must be named {KEYRING_FILENAME}");
        }
        Self::load(directory)
    }

    fn validate_with_file_kind(&self, kind: &str, version: u32) -> Result<()> {
        if kind != KEYRING_KIND {
            return Err(BackupSecurityError::KeyringInvalid {
                reason: format!("unexpected keyring kind {kind:?}"),
            }
            .into());
        }
        if version != KEYRING_FORMAT_VERSION {
            return Err(BackupSecurityError::KeyringInvalid {
                reason: format!("unsupported keyring format version {version}"),
            }
            .into());
        }
        validate_timestamp(&self.updated_at, "updated")?;
        if self.entries.len() > MAX_KEYRING_KEYS {
            return Err(BackupSecurityError::KeyringInvalid {
                reason: format!("keyring has more than {MAX_KEYRING_KEYS} keys"),
            }
            .into());
        }
        let mut ids = BTreeSet::new();
        for entry in &self.entries {
            entry
                .validate()
                .map_err(|error| BackupSecurityError::KeyringInvalid {
                    reason: format!("{error:#}"),
                })?;
            if !ids.insert(entry.key_id.clone()) {
                return Err(BackupSecurityError::KeyringInvalid {
                    reason: format!("duplicate key id {:?}", entry.key_id),
                }
                .into());
            }
        }
        match &self.active_key_id {
            Some(active) => {
                validate_key_id(active)?;
                let entry = self.entries.iter().find(|entry| &entry.key_id == active);
                let Some(entry) = entry else {
                    return Err(BackupSecurityError::KeyringInvalid {
                        reason: format!("active key {active:?} is not in the keyring"),
                    }
                    .into());
                };
                if entry.retired_at.is_some() {
                    return Err(BackupSecurityError::KeyringInvalid {
                        reason: format!("active key {active:?} is marked retired"),
                    }
                    .into());
                }
                if entry.revoked_at.is_some() {
                    return Err(BackupSecurityError::KeyringInvalid {
                        reason: format!("active key {active:?} is marked revoked"),
                    }
                    .into());
                }
            }
            None => {
                // No active key means signing is disabled. This is the normal
                // state after `backup-key-revoke --force` on the active key,
                // and it can also result from an operator-edited keyring. It is
                // not a trust bypass: signing fails closed until another key is
                // explicitly rotated in, while historical verification still
                // uses the retained public keys.
            }
        }
        Ok(())
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn active_key_id(&self) -> Option<&str> {
        self.active_key_id.as_deref()
    }

    pub fn entries(&self) -> &[KeyringEntry] {
        &self.entries
    }

    /// Return a runtime clone that accepts signatures from revoked keys.
    ///
    /// This is deliberately not serialized and never changes what `signing`
    /// will use: a revoked key can never sign, only verify historical objects
    /// when the operator explicitly asks for it.
    pub fn with_revoked_keys_allowed(mut self, allow_revoked: bool) -> Self {
        self.allow_revoked = allow_revoked;
        self
    }

    pub fn allows_revoked_keys(&self) -> bool {
        self.allow_revoked
    }

    pub fn is_key_revoked(&self, key_id: &str) -> Result<bool> {
        Ok(self.entry(key_id)?.revoked_at.is_some())
    }

    /// Refuse a revoked key for signing or trust decisions.
    pub fn ensure_not_revoked(&self, key_id: &str) -> Result<()> {
        let entry = self.entry(key_id)?;
        match &entry.revoked_at {
            Some(revoked_at) => Err(BackupSecurityError::KeyRevoked {
                key_id: key_id.to_string(),
                revoked_at: revoked_at.clone(),
            }
            .into()),
            None => Ok(()),
        }
    }

    /// Public key for signature verification.
    ///
    /// Historical public keys are retained after retirement and revocation so
    /// reports can still name the signing key. A revoked key is rejected unless
    /// this keyring instance was explicitly created with
    /// [`Keyring::with_revoked_keys_allowed`].
    pub fn public_key_for_verification(&self, key_id: &str) -> Result<PublicKey> {
        let entry = self.entry(key_id)?;
        if let Some(revoked_at) = &entry.revoked_at
            && !self.allow_revoked
        {
            return Err(BackupSecurityError::KeyRevoked {
                key_id: key_id.to_string(),
                revoked_at: revoked_at.clone(),
            }
            .into());
        }
        entry.public_key()
    }

    /// Human-readable warning when a verified object uses a revoked key.
    pub fn revocation_warning(&self, key_id: &str) -> Option<String> {
        let entry = self.entries.iter().find(|entry| entry.key_id == key_id)?;
        entry.revoked_at.as_ref().map(|revoked_at| {
            format!("signature key {key_id:?} was revoked at {revoked_at}; accepted only because --allow-revoked was set")
        })
    }

    /// Revoke a key, refusing the active key unless `force` is explicit.
    ///
    /// Revoked public keys remain in the keyring for historical verification
    /// and reporting. Forcing revocation of the active key clears
    /// `active_key_id`, which disables signing until another key is rotated in.
    /// An append-only `keyring-events.jsonl` audit line records the action;
    /// it contains only public identifiers.
    pub fn revoke_key(&mut self, key_id: &str, force: bool) -> Result<KeyRevocationReport> {
        validate_key_id(key_id)?;
        let position = self
            .entries
            .iter()
            .position(|entry| entry.key_id == key_id)
            .ok_or_else(|| BackupSecurityError::KeyNotFound {
                key_id: key_id.to_string(),
            })?;
        if let Some(existing) = &self.entries[position].revoked_at {
            return Ok(KeyRevocationReport {
                key_id: key_id.to_string(),
                revoked_at: existing.clone(),
                was_active: self.active_key_id.as_deref() == Some(key_id),
                active_key_id: self.active_key_id.clone(),
                force,
                changed: false,
                warning: None,
                keyring: self.directory.join(KEYRING_FILENAME).display().to_string(),
            });
        }
        let was_active = self.active_key_id.as_deref() == Some(key_id);
        if was_active && !force {
            return Err(BackupSecurityError::KeyRevokeActive {
                key_id: key_id.to_string(),
            }
            .into());
        }
        let revoked_at = now_rfc3339();
        self.entries[position].revoked_at = Some(revoked_at.clone());
        let mut warning = None;
        if was_active {
            self.active_key_id = None;
            warning = Some(format!(
                "active signing key {key_id:?} was force-revoked; no key can sign until you rotate to a new key"
            ));
        }
        self.updated_at = revoked_at.clone();
        self.validate_with_file_kind(KEYRING_KIND, KEYRING_FORMAT_VERSION)?;
        self.save()?;
        let report = KeyRevocationReport {
            key_id: key_id.to_string(),
            revoked_at,
            was_active,
            active_key_id: self.active_key_id.clone(),
            force,
            changed: true,
            warning,
            keyring: self.directory.join(KEYRING_FILENAME).display().to_string(),
        };
        if let Err(error) = self.append_event(
            "key.revoked",
            serde_json::json!({
                "key_id": report.key_id,
                "revoked_at": report.revoked_at,
                "was_active": report.was_active,
                "force": report.force,
                "active_key_id": report.active_key_id,
            }),
        ) {
            tracing::warn!(%error, "backup key revocation could not be appended to the keyring audit log");
        }
        Ok(report)
    }

    fn append_event(&self, event: &str, details: serde_json::Value) -> Result<()> {
        vp_store::paths::ensure_private_dir(&self.directory)?;
        let path = self.directory.join(KEYRING_EVENTS_FILENAME);
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                bail!(
                    "refusing symlinked backup keyring audit log {}",
                    path.display()
                );
            }
            if !metadata.is_file() {
                bail!(
                    "backup keyring audit log {} is not a regular file",
                    path.display()
                );
            }
        }
        // Repair a trailing partial line from an interrupted append before
        // writing. The audit reader fails closed on malformed complete lines,
        // so leaving a torn record would make the whole log unreadable.
        if let Ok(metadata) = fs::metadata(&path)
            && metadata.len() > 0
            && metadata.len() <= MAX_KEYRING_AUDIT_BYTES
        {
            let bytes = fs::read(&path)
                .with_context(|| format!("reading keyring audit log {}", path.display()))?;
            if bytes.last() != Some(&b'\n') {
                let keep = bytes
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map(|index| index + 1)
                    .unwrap_or(0);
                let file = OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .with_context(|| format!("repairing keyring audit log {}", path.display()))?;
                file.set_len(keep as u64)
                    .with_context(|| format!("truncating keyring audit log {}", path.display()))?;
                file.sync_all()
                    .with_context(|| format!("syncing keyring audit log {}", path.display()))?;
            }
        }
        let record = serde_json::json!({
            "event": event,
            "created_at": now_rfc3339(),
            "details": details,
        });
        let mut line = serde_json::to_vec(&record).context("serializing backup keyring audit")?;
        line.push(b'\n');
        let mut options = OpenOptions::new();
        options.create(true).append(true).read(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&path)
            .with_context(|| format!("opening backup keyring audit log {}", path.display()))?;
        vp_store::paths::ensure_private_file(&path)?;
        file.write_all(&line)
            .with_context(|| format!("appending to {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing {}", path.display()))?;
        Ok(())
    }

    pub fn updated_at(&self) -> &str {
        &self.updated_at
    }

    pub fn public_key(&self, key_id: &str) -> Result<PublicKey> {
        validate_key_id(key_id)?;
        let entry = self.entry(key_id)?;
        entry.public_key()
    }

    pub(crate) fn entry(&self, key_id: &str) -> Result<&KeyringEntry> {
        self.entries
            .iter()
            .find(|entry| entry.key_id == key_id)
            .ok_or_else(|| {
                BackupSecurityError::KeyNotFound {
                    key_id: key_id.to_string(),
                }
                .into()
            })
    }

    /// Resolve the active key id, or an explicitly requested one.
    pub fn resolve_key_id(&self, requested: Option<&str>) -> Result<String> {
        if let Some(requested) = requested {
            validate_key_id(requested)?;
            self.entry(requested)?;
            return Ok(requested.to_string());
        }
        match &self.active_key_id {
            Some(active) => Ok(active.clone()),
            None => Err(BackupSecurityError::SigningRequired.into()),
        }
    }

    /// Load the private credential for `key_id` (or the active key).
    ///
    /// Revoked keys are refused even when requested explicitly: revocation is
    /// a hard stop for new signatures. Historical verification uses
    /// [`Keyring::public_key_for_verification`] instead.
    pub fn signing_credential(&self, requested: Option<&str>) -> Result<SigningCredential> {
        let key_id = self.resolve_key_id(requested)?;
        let entry = self.entry(&key_id)?;
        if let Some(revoked_at) = &entry.revoked_at {
            return Err(BackupSecurityError::KeyRevoked {
                key_id: key_id.clone(),
                revoked_at: revoked_at.clone(),
            }
            .into());
        }
        let material = SigningKeyMaterial::load(&self.directory, &key_id, &entry.source)?;
        let derived = material.public_key_bytes()?;
        let expected = entry.public_key()?.bytes()?;
        if derived != expected {
            return Err(BackupSecurityError::KeyInvalid {
                key_id,
                reason: "private seed does not match the stored public key".to_string(),
            }
            .into());
        }
        Ok(SigningCredential { key_id, material })
    }

    /// Generate a new signing key and optionally make it active.
    pub fn generate_key(
        directory: &Path,
        key_id: &str,
        activate: bool,
        source: SecretSource,
    ) -> Result<KeyGenerationReport> {
        validate_key_id(key_id)?;
        source.validate()?;
        if matches!(source, SecretSource::Environment { .. }) {
            bail!(
                "environment-backed signing keys must be registered with `register_environment_key`; \
                 a generated seed would not be recoverable"
            );
        }
        let mut keyring = Self::load_or_create(directory)?;
        if keyring.entries.iter().any(|entry| entry.key_id == key_id) {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "a key with this id already exists".to_string(),
            }
            .into());
        }
        if keyring.entries.len() >= MAX_KEYRING_KEYS {
            return Err(BackupSecurityError::KeyringInvalid {
                reason: format!("keyring is full ({MAX_KEYRING_KEYS} keys)"),
            }
            .into());
        }
        let mut seed = Zeroizing::new([0u8; SEED_BYTES]);
        rand::rng().fill_bytes(&mut seed[..]);
        let material = SigningKeyMaterial { seed };
        let public = material.public_key_bytes()?;
        let created_at = now_rfc3339();
        if let SecretSource::File { name } = &source {
            let path = directory.join(name);
            if path.exists() {
                return Err(BackupSecurityError::KeyInvalid {
                    key_id: key_id.to_string(),
                    reason: format!("key file {name} already exists in the key directory"),
                }
                .into());
            }
            let file = SigningKeyFile {
                kind: KEY_FILE_KIND.to_string(),
                version: KEY_FILE_FORMAT_VERSION,
                key_id: key_id.to_string(),
                algorithm: SIGNING_ALGORITHM.to_string(),
                seed_b64: BASE64.encode(&material.seed[..]),
                public_key_b64: BASE64.encode(public),
                created_at: created_at.clone(),
            };
            let bytes =
                serde_json::to_vec_pretty(&file).context("serializing backup signing key file")?;
            write_private_atomic(directory, &path, &bytes)?;
        }
        let entry = KeyringEntry {
            key_id: key_id.to_string(),
            algorithm: SIGNING_ALGORITHM.to_string(),
            public_key_b64: BASE64.encode(public),
            source: source.clone(),
            created_at: created_at.clone(),
            retired_at: None,
            revoked_at: None,
        };
        keyring.entries.push(entry);
        keyring
            .entries
            .sort_by(|left, right| left.key_id.cmp(&right.key_id));
        if activate || keyring.active_key_id.is_none() {
            if let Some(previous) = keyring.active_key_id.take()
                && previous != key_id
                && let Some(entry) = keyring
                    .entries
                    .iter_mut()
                    .find(|entry| entry.key_id == previous)
                && entry.retired_at.is_none()
            {
                entry.retired_at = Some(created_at.clone());
            }
            keyring.active_key_id = Some(key_id.to_string());
        }
        keyring.updated_at = created_at.clone();
        keyring.save()?;
        if let Err(error) = keyring.append_event(
            "key.generated",
            serde_json::json!({
                "key_id": key_id,
                "active": keyring.active_key_id.as_deref() == Some(key_id),
                "source": source.describe(),
            }),
        ) {
            tracing::warn!(%error, "backup key generation could not be appended to the keyring audit log");
        }
        Ok(KeyGenerationReport {
            key_id: key_id.to_string(),
            algorithm: SIGNING_ALGORITHM.to_string(),
            public_key_b64: BASE64.encode(public),
            fingerprint: super::crypto::sha256_hex(&public),
            active: keyring.active_key_id.as_deref() == Some(key_id),
            keyring: keyring
                .directory
                .join(KEYRING_FILENAME)
                .display()
                .to_string(),
            key_directory: directory.display().to_string(),
            created_at,
            source: source.describe(),
        })
    }

    /// Import an existing 32-byte seed into the key directory.
    pub fn import_key_file(
        directory: &Path,
        key_id: &str,
        seed_path: &Path,
        activate: bool,
    ) -> Result<KeyGenerationReport> {
        validate_key_id(key_id)?;
        let metadata = fs::symlink_metadata(seed_path)
            .with_context(|| format!("reading backup signing key file {}", seed_path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing symlinked backup signing key file {}",
                seed_path.display()
            );
        }
        if !metadata.is_file() {
            bail!(
                "backup signing key file {} is not a regular file",
                seed_path.display()
            );
        }
        check_private_mode(seed_path, &metadata)?;
        if metadata.len() == 0 || metadata.len() > MAX_KEY_FILE_BYTES {
            bail!("backup signing key file must contain 1..={MAX_KEY_FILE_BYTES} bytes");
        }
        let bytes = fs::read(seed_path)
            .with_context(|| format!("reading backup signing key file {}", seed_path.display()))?;
        let text = std::str::from_utf8(&bytes)
            .context("backup signing key file is not UTF-8")?
            .trim();
        // Accept either a JSON key file or a bare base64 seed (one line).
        let (seed_b64, declared_public) = match serde_json::from_str::<SigningKeyFile>(text) {
            Ok(file) => {
                if file.kind != KEY_FILE_KIND || file.version != KEY_FILE_FORMAT_VERSION {
                    return Err(BackupSecurityError::KeyInvalid {
                        key_id: key_id.to_string(),
                        reason: "imported key file has an unsupported kind or version".to_string(),
                    }
                    .into());
                }
                if file.algorithm != SIGNING_ALGORITHM {
                    return Err(BackupSecurityError::KeyInvalid {
                        key_id: key_id.to_string(),
                        reason: "imported key file uses an unsupported algorithm".to_string(),
                    }
                    .into());
                }
                (file.seed_b64, Some(file.public_key_b64))
            }
            Err(_) => (text.to_string(), None),
        };
        let seed = decode_seed(&seed_b64)?;
        let material = SigningKeyMaterial { seed };
        let public = material.public_key_bytes()?;
        if let Some(declared) = declared_public
            && decode_public_key(&declared)? != public
        {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "imported key file public key does not match its seed".to_string(),
            }
            .into());
        }
        let mut keyring = Self::load_or_create(directory)?;
        if keyring.entries.iter().any(|entry| entry.key_id == key_id) {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "a key with this id already exists".to_string(),
            }
            .into());
        }
        let name = format!("{key_id}.key");
        if directory.join(&name).exists() {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: format!("key file {name} already exists in the key directory"),
            }
            .into());
        }
        let created_at = now_rfc3339();
        let file = SigningKeyFile {
            kind: KEY_FILE_KIND.to_string(),
            version: KEY_FILE_FORMAT_VERSION,
            key_id: key_id.to_string(),
            algorithm: SIGNING_ALGORITHM.to_string(),
            seed_b64: BASE64.encode(&material.seed[..]),
            public_key_b64: BASE64.encode(public),
            created_at: created_at.clone(),
        };
        let bytes =
            serde_json::to_vec_pretty(&file).context("serializing backup signing key file")?;
        write_private_atomic(directory, &directory.join(&name), &bytes)?;
        let source = SecretSource::File { name };
        keyring.entries.push(KeyringEntry {
            key_id: key_id.to_string(),
            algorithm: SIGNING_ALGORITHM.to_string(),
            public_key_b64: BASE64.encode(public),
            source: source.clone(),
            created_at: created_at.clone(),
            retired_at: None,
            revoked_at: None,
        });
        keyring
            .entries
            .sort_by(|left, right| left.key_id.cmp(&right.key_id));
        if activate || keyring.active_key_id.is_none() {
            if let Some(previous) = keyring.active_key_id.take()
                && previous != key_id
                && let Some(entry) = keyring
                    .entries
                    .iter_mut()
                    .find(|entry| entry.key_id == previous)
            {
                entry.retired_at = Some(created_at.clone());
            }
            keyring.active_key_id = Some(key_id.to_string());
        }
        keyring.updated_at = created_at.clone();
        keyring.save()?;
        if let Err(error) = keyring.append_event(
            "key.imported",
            serde_json::json!({
                "key_id": key_id,
                "active": keyring.active_key_id.as_deref() == Some(key_id),
                "source": source.describe(),
            }),
        ) {
            tracing::warn!(%error, "backup key import could not be appended to the keyring audit log");
        }
        Ok(KeyGenerationReport {
            key_id: key_id.to_string(),
            algorithm: SIGNING_ALGORITHM.to_string(),
            public_key_b64: BASE64.encode(public),
            fingerprint: super::crypto::sha256_hex(&public),
            active: keyring.active_key_id.as_deref() == Some(key_id),
            keyring: keyring
                .directory
                .join(KEYRING_FILENAME)
                .display()
                .to_string(),
            key_directory: directory.display().to_string(),
            created_at,
            source: source.describe(),
        })
    }

    /// Register a key whose seed is supplied through an environment variable.
    ///
    /// The environment variable is read once to derive and store the public
    /// key. The seed itself is never written to the keyring or key directory.
    pub fn register_environment_key(
        directory: &Path,
        key_id: &str,
        variable: &str,
        activate: bool,
    ) -> Result<KeyGenerationReport> {
        validate_key_id(key_id)?;
        validate_environment_name(variable)?;
        let value =
            std::env::var(variable).map_err(|_| BackupSecurityError::KeyMaterialUnavailable {
                key_id: key_id.to_string(),
            })?;
        let seed = decode_seed(value.trim())?;
        let material = SigningKeyMaterial { seed };
        let public = material.public_key_bytes()?;
        let mut keyring = Self::load_or_create(directory)?;
        if keyring.entries.iter().any(|entry| entry.key_id == key_id) {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "a key with this id already exists".to_string(),
            }
            .into());
        }
        let created_at = now_rfc3339();
        let source = SecretSource::Environment {
            variable: variable.to_string(),
        };
        keyring.entries.push(KeyringEntry {
            key_id: key_id.to_string(),
            algorithm: SIGNING_ALGORITHM.to_string(),
            public_key_b64: BASE64.encode(public),
            source: source.clone(),
            created_at: created_at.clone(),
            retired_at: None,
            revoked_at: None,
        });
        keyring
            .entries
            .sort_by(|left, right| left.key_id.cmp(&right.key_id));
        if activate || keyring.active_key_id.is_none() {
            if let Some(previous) = keyring.active_key_id.take()
                && previous != key_id
                && let Some(entry) = keyring
                    .entries
                    .iter_mut()
                    .find(|entry| entry.key_id == previous)
            {
                entry.retired_at = Some(created_at.clone());
            }
            keyring.active_key_id = Some(key_id.to_string());
        }
        keyring.updated_at = created_at.clone();
        keyring.save()?;
        if let Err(error) = keyring.append_event(
            "key.registered",
            serde_json::json!({
                "key_id": key_id,
                "active": keyring.active_key_id.as_deref() == Some(key_id),
                "source": source.describe(),
            }),
        ) {
            tracing::warn!(%error, "backup key registration could not be appended to the keyring audit log");
        }
        Ok(KeyGenerationReport {
            key_id: key_id.to_string(),
            algorithm: SIGNING_ALGORITHM.to_string(),
            public_key_b64: BASE64.encode(public),
            fingerprint: super::crypto::sha256_hex(&public),
            active: keyring.active_key_id.as_deref() == Some(key_id),
            keyring: keyring
                .directory
                .join(KEYRING_FILENAME)
                .display()
                .to_string(),
            key_directory: directory.display().to_string(),
            created_at,
            source: source.describe(),
        })
    }

    /// Promote an existing key to active, retiring the previous active key.
    pub fn rotate_to(&mut self, key_id: &str) -> Result<KeyRotationReport> {
        validate_key_id(key_id)?;
        let entry = self.entry(key_id)?;
        if let Some(revoked_at) = &entry.revoked_at {
            return Err(BackupSecurityError::KeyRevoked {
                key_id: key_id.to_string(),
                revoked_at: revoked_at.clone(),
            }
            .into());
        }
        let previous = self.active_key_id.clone();
        let rotated_at = now_rfc3339();
        if let Some(previous) = &previous
            && previous == key_id
        {
            return Ok(KeyRotationReport {
                key_id: key_id.to_string(),
                previous_key_id: previous.clone(),
                changed: false,
                retired_public_keys: self.retired_key_ids(),
                keyring: self.directory.join(KEYRING_FILENAME).display().to_string(),
                rotated_at,
            });
        }
        if let Some(previous) = &previous
            && let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| &entry.key_id == previous)
        {
            entry.retired_at = Some(rotated_at.clone());
        }
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key_id == key_id) {
            entry.retired_at = None;
        }
        self.active_key_id = Some(key_id.to_string());
        self.updated_at = rotated_at.clone();
        self.save()?;
        if let Err(error) = self.append_event(
            "key.rotated",
            serde_json::json!({
                "key_id": key_id,
                "previous_key_id": previous,
                "active_key_id": self.active_key_id,
            }),
        ) {
            tracing::warn!(%error, "backup key rotation could not be appended to the keyring audit log");
        }
        Ok(KeyRotationReport {
            key_id: key_id.to_string(),
            previous_key_id: previous.unwrap_or_default(),
            changed: true,
            retired_public_keys: self.retired_key_ids(),
            keyring: self.directory.join(KEYRING_FILENAME).display().to_string(),
            rotated_at,
        })
    }

    fn retired_key_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.retired_at.is_some())
            .map(|entry| entry.key_id.clone())
            .collect()
    }

    /// Read the bounded keyring lifecycle audit log, newest first.
    ///
    /// The log contains only public key ids, timestamps, and source
    /// *references*. A malformed line, a symlink, or a line that appears to
    /// contain secret material fails closed instead of being skipped.
    pub fn audit_events(&self, limit: usize) -> Result<KeyringAuditReport> {
        let path = self.directory.join(KEYRING_EVENTS_FILENAME);
        let limit = limit.clamp(1, MAX_KEYRING_AUDIT_EVENTS);
        let mut report = KeyringAuditReport {
            path: path.display().to_string(),
            present: false,
            event_count: 0,
            truncated: false,
            events: Vec::new(),
        };
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "refusing symlinked backup keyring audit log {}",
                    path.display()
                );
            }
            Ok(metadata) if !metadata.is_file() => {
                bail!(
                    "backup keyring audit log {} is not a regular file",
                    path.display()
                );
            }
            Ok(metadata) => {
                if metadata.len() > MAX_KEYRING_AUDIT_BYTES {
                    return Err(BackupSecurityError::KeyringInvalid {
                        reason: "keyring audit log exceeds its size limit".to_string(),
                    }
                    .into());
                }
                report.present = true;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(report);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading keyring audit log {}", path.display()));
            }
        }
        let mut bytes = Vec::new();
        File::open(&path)
            .with_context(|| format!("opening keyring audit log {}", path.display()))?
            .take(MAX_KEYRING_AUDIT_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading keyring audit log {}", path.display()))?;
        if bytes.len() as u64 > MAX_KEYRING_AUDIT_BYTES {
            return Err(BackupSecurityError::KeyringInvalid {
                reason: "keyring audit log grew beyond its size limit".to_string(),
            }
            .into());
        }
        // Ignore only an incomplete final line from an interrupted append.
        let complete = if bytes.last() == Some(&b'\n') {
            bytes.as_slice()
        } else if let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') {
            &bytes[..=last_newline]
        } else {
            &[]
        };
        let mut events = Vec::new();
        for (index, line) in complete.split(|byte| *byte == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let text = std::str::from_utf8(line)
                .context("backup keyring audit log is not valid UTF-8")?
                .to_ascii_lowercase();
            if text.contains("seed_b64")
                || text.contains("private_key")
                || text.contains("passphrase")
            {
                return Err(BackupSecurityError::KeyringInvalid {
                    reason: format!(
                        "backup keyring audit log line {} appears to contain secret material",
                        index + 1
                    ),
                }
                .into());
            }
            let value: serde_json::Value = serde_json::from_slice(line)
                .with_context(|| format!("parsing backup keyring audit line {}", index + 1))?;
            let event = value
                .get("event")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("backup keyring audit line has no event name"))?;
            let created_at = value
                .get("created_at")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("backup keyring audit line has no timestamp"))?;
            if event.is_empty()
                || event.len() > 64
                || created_at.is_empty()
                || created_at.len() > 64
            {
                bail!("backup keyring audit line has a malformed event or timestamp");
            }
            events.push(KeyringAuditEvent {
                event: event.to_string(),
                created_at: created_at.to_string(),
                details: value
                    .get("details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            });
        }
        report.event_count = events.len();
        report.truncated = events.len() > limit;
        events.reverse();
        events.truncate(limit);
        report.events = events;
        Ok(report)
    }

    /// Export a public key without any secret reference.
    pub fn export_public(&self, requested: Option<&str>) -> Result<PublicKeyExport> {
        let key_id = self.resolve_key_id(requested)?;
        let public = self.public_key(&key_id)?;
        let entry = self.entry(&public.key_id)?;
        Ok(PublicKeyExport {
            key_id,
            algorithm: SIGNING_ALGORITHM.to_string(),
            public_key_b64: public.public_key_b64.clone(),
            fingerprint: public.fingerprint()?,
            created_at: entry.created_at.clone(),
            revoked_at: entry.revoked_at.clone(),
        })
    }

    pub fn summary(&self) -> KeyringSummary {
        let mut keys: Vec<KeyringKeySummary> = self
            .entries
            .iter()
            .map(|entry| KeyringKeySummary {
                key_id: entry.key_id.clone(),
                algorithm: entry.algorithm.clone(),
                public_key_b64: entry.public_key_b64.clone(),
                created_at: entry.created_at.clone(),
                retired_at: entry.retired_at.clone(),
                revoked_at: entry.revoked_at.clone(),
                active: self.active_key_id.as_deref() == Some(entry.key_id.as_str()),
            })
            .collect();
        keys.sort_by(|left, right| left.key_id.cmp(&right.key_id));
        let revoked_key_count = keys.iter().filter(|key| key.revoked_at.is_some()).count();
        KeyringSummary {
            path: self.directory.join(KEYRING_FILENAME).display().to_string(),
            active_key_id: self.active_key_id.clone(),
            key_count: keys.len(),
            revoked_key_count,
            keys,
        }
    }

    /// Persist the keyring atomically with 0600 permissions.
    pub fn save(&self) -> Result<()> {
        vp_store::paths::ensure_private_dir(&self.directory).with_context(|| {
            format!("creating backup key directory {}", self.directory.display())
        })?;
        let file = KeyringFile {
            kind: KEYRING_KIND.to_string(),
            version: KEYRING_FORMAT_VERSION,
            active_key_id: self.active_key_id.clone(),
            updated_at: self.updated_at.clone(),
            keys: self.entries.clone(),
        };
        file.validate_entries()?;
        let mut bytes = serde_json::to_vec_pretty(&file).context("serializing backup keyring")?;
        if bytes.len() as u64 > MAX_KEYRING_BYTES {
            bail!("backup keyring exceeds its size limit");
        }
        bytes.push(b'\n');
        let path = self.directory.join(KEYRING_FILENAME);
        write_private_atomic(&self.directory, &path, &bytes)
    }
}

impl KeyringFile {
    fn validate_entries(&self) -> Result<()> {
        if self.kind != KEYRING_KIND || self.version != KEYRING_FORMAT_VERSION {
            bail!("backup keyring kind or version is invalid");
        }
        let keyring = Keyring {
            directory: PathBuf::new(),
            active_key_id: self.active_key_id.clone(),
            updated_at: self.updated_at.clone(),
            entries: self.keys.clone(),
            allow_revoked: false,
        };
        keyring.validate_with_file_kind(&self.kind, self.version)
    }
}

/// A generated or imported key, never containing the seed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyGenerationReport {
    pub key_id: String,
    pub algorithm: String,
    pub public_key_b64: String,
    pub fingerprint: String,
    pub active: bool,
    pub keyring: String,
    pub key_directory: String,
    pub created_at: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyRotationReport {
    pub key_id: String,
    pub previous_key_id: String,
    pub changed: bool,
    pub retired_public_keys: Vec<String>,
    pub keyring: String,
    pub rotated_at: String,
}

/// Result of revoking one signing key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyRevocationReport {
    pub key_id: String,
    pub revoked_at: String,
    pub was_active: bool,
    /// `None` after a forced active-key revocation disables signing.
    pub active_key_id: Option<String>,
    pub force: bool,
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    pub keyring: String,
}

/// One append-only keyring lifecycle event, as read by `backup-key-events`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct KeyringAuditEvent {
    pub event: String,
    pub created_at: String,
    pub details: serde_json::Value,
}

/// Bounded, secret-free view of the keyring audit log.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct KeyringAuditReport {
    pub path: String,
    pub present: bool,
    pub event_count: usize,
    pub truncated: bool,
    pub events: Vec<KeyringAuditEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicKeyExport {
    pub key_id: String,
    pub algorithm: String,
    pub public_key_b64: String,
    pub fingerprint: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

impl SecretSource {
    fn describe(&self) -> String {
        match self {
            Self::File { name } => format!("file:{name}"),
            Self::Environment { variable } => format!("env:{variable}"),
        }
    }
}

/// On-disk 0600 signing key file.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SigningKeyFile {
    kind: String,
    version: u32,
    key_id: String,
    algorithm: String,
    seed_b64: String,
    public_key_b64: String,
    created_at: String,
}

impl fmt::Debug for SigningKeyFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SigningKeyFile")
            .field("kind", &self.kind)
            .field("version", &self.version)
            .field("key_id", &self.key_id)
            .field("algorithm", &self.algorithm)
            .field("seed_b64", &"<redacted>")
            .field("public_key_b64", &self.public_key_b64)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Ed25519 private material. Zeroized on drop, redacted in `Debug`.
pub struct SigningKeyMaterial {
    seed: Zeroizing<[u8; SEED_BYTES]>,
}

impl fmt::Debug for SigningKeyMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SigningKeyMaterial(<redacted>)")
    }
}

impl SigningKeyMaterial {
    fn load(directory: &Path, key_id: &str, source: &SecretSource) -> Result<Self> {
        source.validate()?;
        match source {
            SecretSource::File { .. } => {
                let path = source.resolved_path(directory).ok_or_else(|| {
                    BackupSecurityError::KeyMaterialUnavailable {
                        key_id: key_id.to_string(),
                    }
                })?;
                Self::load_file(&path, key_id)
            }
            SecretSource::Environment { variable } => {
                let value = std::env::var(variable).map_err(|_| {
                    BackupSecurityError::KeyMaterialUnavailable {
                        key_id: key_id.to_string(),
                    }
                })?;
                let seed =
                    decode_seed(value.trim()).map_err(|_| BackupSecurityError::KeyInvalid {
                        key_id: key_id.to_string(),
                        reason: "environment seed is not a canonical 32-byte base64 seed"
                            .to_string(),
                    })?;
                Ok(Self { seed })
            }
        }
    }

    fn load_file(path: &Path, key_id: &str) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            BackupSecurityError::KeyMaterialUnavailable {
                key_id: key_id.to_string(),
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: format!("key file {} is a symlink", path.display()),
            }
            .into());
        }
        if !metadata.is_file() {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: format!("key file {} is not a regular file", path.display()),
            }
            .into());
        }
        check_private_mode(path, &metadata)?;
        if metadata.len() == 0 || metadata.len() > MAX_KEY_FILE_BYTES {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "key file size is outside the supported range".to_string(),
            }
            .into());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .with_context(|| format!("opening backup signing key file {}", path.display()))?
            .take(MAX_KEY_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading backup signing key file {}", path.display()))?;
        if bytes.len() as u64 > MAX_KEY_FILE_BYTES {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "key file grew beyond its size limit".to_string(),
            }
            .into());
        }
        let file: SigningKeyFile =
            serde_json::from_slice(&bytes).map_err(|error| BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: format!("key file JSON is invalid: {error}"),
            })?;
        if file.kind != KEY_FILE_KIND || file.version != KEY_FILE_FORMAT_VERSION {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "key file has an unsupported kind or version".to_string(),
            }
            .into());
        }
        if file.key_id != key_id {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: format!("key file belongs to key {:?}", file.key_id),
            }
            .into());
        }
        if file.algorithm != SIGNING_ALGORITHM {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "key file uses an unsupported algorithm".to_string(),
            }
            .into());
        }
        validate_timestamp(&file.created_at, "key file creation")?;
        let seed = decode_seed(&file.seed_b64).map_err(|_| BackupSecurityError::KeyInvalid {
            key_id: key_id.to_string(),
            reason: "key file seed is not canonical base64".to_string(),
        })?;
        let public = decode_public_key(&file.public_key_b64).map_err(|_| {
            BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "key file public key is not canonical base64".to_string(),
            }
        })?;
        let material = Self { seed };
        if material.public_key_bytes()? != public {
            return Err(BackupSecurityError::KeyInvalid {
                key_id: key_id.to_string(),
                reason: "key file public key does not match its seed".to_string(),
            }
            .into());
        }
        Ok(material)
    }

    fn public_key_bytes(&self) -> Result<[u8; PUBLIC_KEY_BYTES]> {
        let pair = Ed25519KeyPair::from_seed_unchecked(&self.seed[..])
            .map_err(|_| anyhow!("backup signing seed is not a valid Ed25519 seed"))?;
        let public = pair.public_key();
        let bytes: [u8; PUBLIC_KEY_BYTES] = public
            .as_ref()
            .try_into()
            .map_err(|_| anyhow!("backup signing public key has the wrong length"))?;
        Ok(bytes)
    }
}

/// A loaded signing credential. Not `Clone` and redacted in `Debug`.
pub struct SigningCredential {
    key_id: String,
    material: SigningKeyMaterial,
}

impl fmt::Debug for SigningCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SigningCredential")
            .field("key_id", &self.key_id)
            .field("material", &"<redacted>")
            .finish()
    }
}

impl SigningCredential {
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn public_key(&self) -> Result<PublicKey> {
        PublicKey::new(self.key_id.clone(), &self.material.public_key_bytes()?)
    }

    pub(crate) fn sign(&self, domain: &[u8], payload: &[u8]) -> Result<Vec<u8>> {
        let pair = Ed25519KeyPair::from_seed_unchecked(&self.material.seed[..])
            .map_err(|_| anyhow!("backup signing seed is not a valid Ed25519 seed"))?;
        let message = domain_message(domain, payload);
        Ok(pair.sign(&message).as_ref().to_vec())
    }
}

fn domain_message(domain: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(domain.len() + 8 + payload.len());
    message.extend_from_slice(domain);
    message.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    message.extend_from_slice(payload);
    message
}

/// Sign `payload` with a domain separator.
#[cfg(test)]
pub(crate) fn sign_payload(
    credential: &SigningCredential,
    domain: &[u8],
    payload: &[u8],
) -> Result<Vec<u8>> {
    credential.sign(domain, payload)
}

/// Verify a detached signature over `domain || len || payload`.
pub(crate) fn verify_payload(
    public_key: &PublicKey,
    domain: &[u8],
    payload: &[u8],
    signature: &[u8],
) -> Result<()> {
    if signature.len() != SIGNATURE_BYTES {
        return Err(BackupSecurityError::SignatureInvalid {
            object: "signature".to_string(),
            key_id: public_key.key_id.clone(),
        }
        .into());
    }
    let key = public_key.bytes()?;
    let message = domain_message(domain, payload);
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&message, signature)
        .map_err(|_| {
            BackupSecurityError::SignatureInvalid {
                object: "signature".to_string(),
                key_id: public_key.key_id.clone(),
            }
            .into()
        })
}

/// Generate a fresh signing key with the operating-system CSPRNG.
///
/// This is public for tooling and tests; the CLI uses
/// [`Keyring::generate_key`] so the public key is registered atomically.
pub fn generate_ed25519_keypair(key_id: &str) -> Result<(Zeroizing<[u8; SEED_BYTES]>, PublicKey)> {
    validate_key_id(key_id)?;
    let mut seed = Zeroizing::new([0u8; SEED_BYTES]);
    rand::rng().fill_bytes(&mut seed[..]);
    let derived = Ed25519KeyPair::from_seed_unchecked(&seed[..])
        .map_err(|_| anyhow!("deriving the generated Ed25519 public key failed"))?;
    let public = derived.public_key();
    let bytes: [u8; PUBLIC_KEY_BYTES] = public
        .as_ref()
        .try_into()
        .map_err(|_| anyhow!("generated Ed25519 public key has the wrong length"))?;
    Ok((seed, PublicKey::new(key_id, &bytes)?))
}

fn check_private_mode(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(BackupSecurityError::KeyringInsecure {
                path: path.display().to_string(),
                mode,
            }
            .into());
        }
    }
    let _ = (path, metadata);
    Ok(())
}

fn write_private_atomic(directory: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    vp_store::paths::ensure_private_dir(directory)
        .with_context(|| format!("creating directory {}", directory.display()))?;
    let scratch = directory.join(format!(".verlinpark-key-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&scratch)
        .with_context(|| format!("creating {}", scratch.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)
            .with_context(|| format!("writing {}", scratch.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing {}", scratch.display()))?;
        vp_store::paths::ensure_private_file(&scratch)?;
        fs::rename(&scratch, path).with_context(|| format!("publishing {}", path.display()))?;
        fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .ok();
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&scratch);
    }
    result
}

#[cfg(test)]
fn backup_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn keyring_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    fn generate(dir: &TempDir, key_id: &str) -> KeyGenerationReport {
        Keyring::generate_key(
            dir.path(),
            key_id,
            true,
            SecretSource::File {
                name: format!("{key_id}.key"),
            },
        )
        .unwrap()
    }

    #[test]
    fn generate_list_export_and_reload_round_trip() {
        let dir = keyring_dir();
        let report = generate(&dir, "primary");
        assert_eq!(report.key_id, "primary");
        assert_eq!(report.algorithm, "ed25519");
        assert!(report.active);
        assert_eq!(report.public_key_b64.len(), 44);
        assert_eq!(report.fingerprint.len(), 64);
        assert!(report.source.starts_with("file:"));
        assert!(dir.path().join("keyring.json").is_file());
        assert!(dir.path().join("primary.key").is_file());

        let keyring = Keyring::load(dir.path()).unwrap();
        assert_eq!(keyring.active_key_id(), Some("primary"));
        assert_eq!(keyring.entries().len(), 1);
        let summary = keyring.summary();
        assert_eq!(summary.key_count, 1);
        assert_eq!(summary.keys[0].key_id, "primary");
        assert!(summary.keys[0].active);
        assert!(!summary.keys[0].public_key_b64.is_empty());

        let export = keyring.export_public(None).unwrap();
        assert_eq!(export.key_id, "primary");
        assert_eq!(export.public_key_b64, report.public_key_b64);
        assert_eq!(export.fingerprint, report.fingerprint);
        assert!(!format!("{export:?}").contains("seed"));
    }

    #[test]
    fn keyring_and_key_files_are_private_and_never_store_the_seed_in_the_keyring() {
        let dir = keyring_dir();
        let _report = generate(&dir, "primary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in ["keyring.json", "primary.key"] {
                let mode = fs::metadata(dir.path().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600, "{name} mode");
            }
        }
        let keyring_bytes = fs::read(dir.path().join("keyring.json")).unwrap();
        let keyring_text = String::from_utf8_lossy(&keyring_bytes);
        assert!(!keyring_text.contains("seed_b64"));
        // The keyring stores a reference and public key, never the seed.
        let key_file = fs::read_to_string(dir.path().join("primary.key")).unwrap();
        assert!(key_file.contains("seed_b64"));
        assert!(!keyring_text.contains("seed_b64"));
        let keyring: serde_json::Value = serde_json::from_slice(&keyring_bytes).unwrap();
        assert_eq!(keyring["keys"][0]["source"]["kind"], "file");
        assert_eq!(keyring["keys"][0]["source"]["name"], "primary.key");
        assert!(keyring["keys"][0].get("seed_b64").is_none());
    }

    #[test]
    fn insecure_key_material_is_refused() {
        let dir = keyring_dir();
        let _ = generate(&dir, "primary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.path().join("keyring.json"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            let error = Keyring::load(dir.path()).unwrap_err();
            assert_eq!(
                error.downcast_ref::<BackupSecurityError>().unwrap().code(),
                super::super::error::BackupSecurityCode::KeyringInsecure
            );
            fs::set_permissions(
                dir.path().join("keyring.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            fs::set_permissions(
                dir.path().join("primary.key"),
                fs::Permissions::from_mode(0o640),
            )
            .unwrap();
            let keyring = Keyring::load(dir.path()).unwrap();
            let error = keyring.signing_credential(None).unwrap_err();
            assert!(
                format!("{error:#}").contains("BACKUP_KEYRING_INSECURE"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn invalid_key_ids_and_duplicate_keys_are_rejected() {
        for invalid in ["", "../escape", "UPPER", ".hidden", "a..b", "a/b", "-dash"] {
            assert!(validate_key_id(invalid).is_err(), "{invalid}");
        }
        for valid in ["primary", "2026-11-12", "key_1", "a.b-c_d"] {
            validate_key_id(valid).unwrap();
        }
        let dir = keyring_dir();
        let _ = generate(&dir, "primary");
        let error = Keyring::generate_key(
            dir.path(),
            "primary",
            true,
            SecretSource::File {
                name: "primary.key".to_string(),
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("already exists"));
    }

    #[test]
    fn rotation_promotes_a_new_key_and_keeps_old_public_keys() {
        let dir = keyring_dir();
        let first = generate(&dir, "one");
        let second = generate(&dir, "two");
        assert_eq!(first.key_id, "one");
        assert!(second.active);

        let mut keyring = Keyring::load(dir.path()).unwrap();
        assert_eq!(keyring.active_key_id(), Some("two"));
        let rotated = keyring.rotate_to("one").unwrap();
        assert!(rotated.changed);
        assert_eq!(rotated.previous_key_id, "two");
        assert_eq!(rotated.key_id, "one");
        assert!(rotated.retired_public_keys.contains(&"two".to_string()));
        assert_eq!(keyring.active_key_id(), Some("one"));

        // Both keys remain resolvable for verification.
        keyring.public_key("one").unwrap();
        keyring.public_key("two").unwrap();
        assert!(keyring.signing_credential(Some("one")).is_ok());
        assert!(keyring.signing_credential(Some("two")).is_ok());
        assert!(keyring.signing_credential(Some("missing")).is_err());
    }

    #[test]
    fn activate_missing_or_retired_key_is_actionable() {
        let dir = keyring_dir();
        let _ = generate(&dir, "one");
        let _ = generate(&dir, "two");
        let mut keyring = Keyring::load(dir.path()).unwrap();
        let error = keyring.rotate_to("missing").unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_NOT_FOUND"));
        // Rotation can re-activate an older key; it becomes active again and
        // the previous active key is retired for selection but stays usable
        // for verification.
        let rotated_back = keyring.rotate_to("one").unwrap();
        assert!(rotated_back.changed);
        assert_eq!(keyring.active_key_id(), Some("one"));
        assert!(
            rotated_back
                .retired_public_keys
                .contains(&"two".to_string())
        );
        // Activating the current active key is idempotent.
        let current = keyring.active_key_id().unwrap().to_string();
        assert!(!keyring.rotate_to(&current).unwrap().changed);
    }

    #[test]
    fn environment_key_sources_never_write_seed_material() {
        let dir = keyring_dir();
        let seed = [42u8; SEED_BYTES];
        let encoded = BASE64.encode(seed);
        // SAFETY: test-only process-local environment variable with a unique name.
        unsafe {
            std::env::set_var("VERLINPARK_TEST_BACKUP_KEY_7F3", &encoded);
        }
        let report = Keyring::register_environment_key(
            dir.path(),
            "env-key",
            "VERLINPARK_TEST_BACKUP_KEY_7F3",
            true,
        )
        .unwrap();
        assert!(report.active);
        let keyring = Keyring::load(dir.path()).unwrap();
        let entry = &keyring.entries()[0];
        assert!(matches!(entry.source, SecretSource::Environment { .. }));
        let bytes = fs::read(dir.path().join("keyring.json")).unwrap();
        assert!(!backup_contains(&bytes, &seed));
        assert!(!dir.path().join("env-key.key").exists());
        let credential = keyring.signing_credential(None).unwrap();
        let public = credential.public_key().unwrap();
        let signature = credential
            .sign(SNAPSHOT_SIGNATURE_DOMAIN, b"payload")
            .unwrap();
        verify_payload(&public, SNAPSHOT_SIGNATURE_DOMAIN, b"payload", &signature).unwrap();
        unsafe {
            std::env::remove_var("VERLINPARK_TEST_BACKUP_KEY_7F3");
        }
        let error = keyring.signing_credential(None).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_MATERIAL_UNAVAILABLE"));
    }

    #[test]
    fn wrong_environment_seed_does_not_match_the_registered_public_key() {
        let dir = keyring_dir();
        let seed = [7u8; SEED_BYTES];
        let encoded = BASE64.encode(seed);
        // SAFETY: test-only process-local environment variable.
        unsafe {
            std::env::set_var("VERLINPARK_TEST_BACKUP_KEY_CHANGED", &encoded);
        }
        Keyring::register_environment_key(
            dir.path(),
            "env-key",
            "VERLINPARK_TEST_BACKUP_KEY_CHANGED",
            true,
        )
        .unwrap();
        let replacement = BASE64.encode([8u8; SEED_BYTES]);
        unsafe {
            std::env::set_var("VERLINPARK_TEST_BACKUP_KEY_CHANGED", &replacement);
        }
        let keyring = Keyring::load(dir.path()).unwrap();
        let error = keyring.signing_credential(None).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_INVALID"));
        unsafe {
            std::env::remove_var("VERLINPARK_TEST_BACKUP_KEY_CHANGED");
        }
    }

    #[test]
    fn tampered_public_key_or_key_file_is_rejected() {
        let dir = keyring_dir();
        let _ = generate(&dir, "primary");
        let keyring = Keyring::load(dir.path()).unwrap();
        keyring.signing_credential(None).unwrap();

        let key_path = dir.path().join("primary.key");
        let text = fs::read_to_string(&key_path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["public_key_b64"] = serde_json::json!(BASE64.encode([9u8; PUBLIC_KEY_BYTES]));
        fs::write(&key_path, serde_json::to_vec(&value).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = keyring.signing_credential(None).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_INVALID"));

        // A keyring entry whose public key no longer matches is also rejected.
        let mut keyring = Keyring::load(dir.path()).unwrap();
        keyring.entries[0].public_key_b64 = BASE64.encode([3u8; PUBLIC_KEY_BYTES]);
        let error = keyring.signing_credential(None).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_INVALID"));
    }

    #[test]
    fn keyring_validation_rejects_structural_corruption() {
        let dir = keyring_dir();
        let _ = generate(&dir, "primary");
        let path = dir.path().join("keyring.json");
        let original = fs::read_to_string(&path).unwrap();

        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        value["version"] = serde_json::json!(99);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let error = Keyring::load(dir.path()).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEYRING_INVALID"));

        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        value["active_key_id"] = serde_json::json!("missing");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(Keyring::load(dir.path()).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        value["keys"] = serde_json::json!([]);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let error = Keyring::load(dir.path()).unwrap_err();
        assert!(format!("{error:#}").contains("active key"));
    }

    #[test]
    fn symlinked_keyring_and_key_file_are_rejected() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let dir = keyring_dir();
            let outside = TempDir::new().unwrap();
            fs::write(outside.path().join("keyring.json"), b"{}").unwrap();
            symlink(
                outside.path().join("keyring.json"),
                dir.path().join("keyring.json"),
            )
            .unwrap();
            let error = Keyring::load(dir.path()).unwrap_err();
            assert!(format!("{error:#}").contains("symlink"));

            let dir = keyring_dir();
            let _ = generate(&dir, "primary");
            let real = dir.path().join("primary.key");
            let moved = dir.path().join("moved.key");
            fs::rename(&real, &moved).unwrap();
            symlink(&moved, &real).unwrap();
            let keyring = Keyring::load(dir.path()).unwrap();
            let error = keyring.signing_credential(None).unwrap_err();
            assert!(format!("{error:#}").contains("symlink"));
        }
    }

    #[test]
    fn signature_round_trip_tamper_and_domain_separation() {
        let dir = keyring_dir();
        let _ = generate(&dir, "primary");
        let keyring = Keyring::load(dir.path()).unwrap();
        let credential = keyring.signing_credential(None).unwrap();
        let public = keyring.public_key("primary").unwrap();
        let signature = sign_payload(&credential, SNAPSHOT_SIGNATURE_DOMAIN, b"manifest").unwrap();
        assert_eq!(signature.len(), SIGNATURE_BYTES);
        verify_payload(&public, SNAPSHOT_SIGNATURE_DOMAIN, b"manifest", &signature).unwrap();
        assert!(
            verify_payload(&public, METADATA_SIGNATURE_DOMAIN, b"manifest", &signature).is_err()
        );
        assert!(
            verify_payload(&public, SNAPSHOT_SIGNATURE_DOMAIN, b"tampered", &signature).is_err()
        );
        assert!(
            verify_payload(
                &public,
                SNAPSHOT_SIGNATURE_DOMAIN,
                b"manifest",
                &signature[..32]
            )
            .is_err()
        );
        assert!(
            verify_payload(
                &public,
                SNAPSHOT_SIGNATURE_DOMAIN,
                b"manifest",
                &[0u8; SIGNATURE_BYTES]
            )
            .is_err()
        );
    }

    #[test]
    fn import_accepts_a_bare_seed_and_rejects_mismatched_public_keys() {
        let source = TempDir::new().unwrap();
        let dir = keyring_dir();
        let seed = [11u8; SEED_BYTES];
        let seed_file = source.path().join("seed.txt");
        fs::write(&seed_file, BASE64.encode(seed)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&seed_file, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let report = Keyring::import_key_file(dir.path(), "imported", &seed_file, true).unwrap();
        assert!(report.active);
        assert!(dir.path().join("imported.key").is_file());

        let bad = source.path().join("bad.json");
        fs::write(
            &bad,
            serde_json::to_vec(&serde_json::json!({
                "kind": KEY_FILE_KIND,
                "version": 1,
                "key_id": "bad",
                "algorithm": "ed25519",
                "seed_b64": BASE64.encode([1u8; SEED_BYTES]),
                "public_key_b64": BASE64.encode([2u8; PUBLIC_KEY_BYTES]),
                "created_at": now_rfc3339(),
            }))
            .unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bad, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = Keyring::import_key_file(dir.path(), "bad", &bad, false).unwrap_err();
        assert!(format!("{error:#}").contains("does not match"));
    }

    #[test]
    fn keyring_missing_is_typed_and_create_refuses_to_overwrite() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keys");
        let error = Keyring::load(&path).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEYRING_NOT_FOUND"));
        let keyring = Keyring::create(&path).unwrap();
        assert!(keyring.active_key_id().is_none());
        assert!(Keyring::create(&path).is_err());
        assert!(keyring.signing_credential(None).is_err());
    }

    #[test]
    fn debug_output_never_contains_seed_or_passphrase_material() {
        let dir = keyring_dir();
        let _ = generate(&dir, "primary");
        let keyring = Keyring::load(dir.path()).unwrap();
        let credential = keyring.signing_credential(None).unwrap();
        let key_text = fs::read_to_string(dir.path().join("primary.key")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&key_text).unwrap();
        let seed_b64 = value["seed_b64"].as_str().unwrap();
        for rendered in [
            format!("{credential:?}"),
            format!("{:?}", keyring.summary()),
            format!("{:?}", keyring.export_public(None).unwrap()),
        ] {
            assert!(!rendered.contains(seed_b64));
            assert!(!rendered.contains("seed_b64"));
        }
    }

    #[test]
    fn generate_ed25519_keypair_produces_usable_material() {
        let (seed, public) = generate_ed25519_keypair("test-key").unwrap();
        assert_eq!(seed.len(), SEED_BYTES);
        assert_eq!(public.bytes().unwrap().len(), PUBLIC_KEY_BYTES);
        let material = SigningKeyMaterial {
            seed: Zeroizing::new(*seed),
        };
        assert_eq!(
            material.public_key_bytes().unwrap(),
            public.bytes().unwrap()
        );
    }

    #[test]
    fn revocation_is_additive_persistent_and_refuses_new_signatures() {
        let dir = keyring_dir();
        let _ = generate(&dir, "one");
        let _ = generate(&dir, "two");
        let mut keyring = Keyring::load(dir.path()).unwrap();
        assert_eq!(keyring.active_key_id(), Some("two"));

        let missing = keyring.revoke_key("missing", false).unwrap_err();
        assert!(
            format!("{missing:#}").contains("BACKUP_KEY_NOT_FOUND"),
            "{missing:#}"
        );
        let malformed = keyring.revoke_key("../escape", false).unwrap_err();
        assert!(
            format!("{malformed:#}").contains("must not contain")
                || format!("{malformed:#}").contains("key id"),
            "{malformed:#}"
        );

        let report = keyring.revoke_key("one", false).unwrap();
        assert!(report.changed);
        assert!(!report.was_active);
        assert_eq!(report.active_key_id.as_deref(), Some("two"));
        assert!(report.warning.is_none());

        // Revocation is persisted additively and survives reload.
        let reloaded = Keyring::load(dir.path()).unwrap();
        let summary = reloaded.summary();
        assert_eq!(summary.revoked_key_count, 1);
        let entry = summary.keys.iter().find(|key| key.key_id == "one").unwrap();
        assert_eq!(
            entry.revoked_at.as_deref(),
            Some(report.revoked_at.as_str())
        );
        assert!(!entry.active);

        // The public key remains available for historical reporting, but both
        // signing and trust decisions fail closed.
        assert!(reloaded.public_key("one").is_ok());
        let error = reloaded.signing_credential(Some("one")).unwrap_err();
        assert_eq!(
            error.downcast_ref::<BackupSecurityError>().unwrap().code(),
            super::super::error::BackupSecurityCode::KeyRevoked
        );
        let error = reloaded.public_key_for_verification("one").unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_REVOKED"));
        let tolerant = reloaded.clone().with_revoked_keys_allowed(true);
        assert!(tolerant.public_key_for_verification("one").is_ok());
        assert!(tolerant.revocation_warning("one").is_some());

        // Revoking twice is idempotent and does not move the timestamp.
        let mut keyring = Keyring::load(dir.path()).unwrap();
        let second = keyring.revoke_key("one", false).unwrap();
        assert!(!second.changed);
        assert_eq!(second.revoked_at, report.revoked_at);

        // A keyring without the optional field still loads.
        let bytes = fs::read(dir.path().join(KEYRING_FILENAME)).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["keys"][0]
            .as_object_mut()
            .unwrap()
            .remove("revoked_at");
        fs::write(
            dir.path().join(KEYRING_FILENAME),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.path().join(KEYRING_FILENAME),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let legacy = Keyring::load(dir.path()).unwrap();
        assert!(!legacy.is_key_revoked("one").unwrap());
    }

    #[test]
    fn active_key_revocation_requires_force_and_disables_signing() {
        let dir = keyring_dir();
        let _ = generate(&dir, "primary");
        let mut keyring = Keyring::load(dir.path()).unwrap();
        let error = keyring.revoke_key("primary", false).unwrap_err();
        assert_eq!(
            error.downcast_ref::<BackupSecurityError>().unwrap().code(),
            super::super::error::BackupSecurityCode::KeyRevokeActive
        );
        assert_eq!(keyring.active_key_id(), Some("primary"));

        let report = keyring.revoke_key("primary", true).unwrap();
        assert!(report.changed);
        assert!(report.was_active);
        assert!(report.active_key_id.is_none());
        assert!(
            report
                .warning
                .as_deref()
                .unwrap()
                .contains("no key can sign")
        );

        // The keyring is still structurally valid with every key revoked.
        let mut reloaded = Keyring::load(dir.path()).unwrap();
        assert!(reloaded.active_key_id().is_none());
        let error = reloaded.signing_credential(None).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_SIGNING_REQUIRED"));
        assert!(reloaded.rotate_to("primary").is_err());

        // Generating the first usable key after a forced revocation activates
        // it so the operator can recover without editing the keyring.
        Keyring::generate_key(
            dir.path(),
            "replacement",
            false,
            SecretSource::File {
                name: "replacement.key".to_string(),
            },
        )
        .unwrap();
        let recovered = Keyring::load(dir.path()).unwrap();
        assert_eq!(recovered.active_key_id(), Some("replacement"));
        assert!(recovered.signing_credential(None).is_ok());
    }

    #[test]
    fn forced_active_revocation_with_retired_keys_stays_loadable_and_recoverable() {
        let dir = keyring_dir();
        let _ = generate(&dir, "one");
        let _ = generate(&dir, "two");
        let mut keyring = Keyring::load(dir.path()).unwrap();
        keyring.revoke_key("two", true).unwrap();
        let mut reloaded = Keyring::load(dir.path()).unwrap();
        assert!(reloaded.active_key_id().is_none());
        assert!(!reloaded.is_key_revoked("one").unwrap());
        assert!(reloaded.is_key_revoked("two").unwrap());
        // A retired key can be promoted again after the active key was
        // force-revoked; the revoked key stays unusable.
        let rotation = reloaded.rotate_to("one").unwrap();
        assert!(rotation.changed);
        assert_eq!(reloaded.active_key_id(), Some("one"));
        assert!(reloaded.signing_credential(None).is_ok());
        assert!(reloaded.signing_credential(Some("two")).is_err());
    }

    #[test]
    fn revocation_reports_and_audit_logs_never_contain_seed_material() {
        let dir = keyring_dir();
        let _ = generate(&dir, "one");
        let _ = generate(&dir, "two");
        let key_text = fs::read_to_string(dir.path().join("one.key")).unwrap();
        let key_json: serde_json::Value = serde_json::from_str(&key_text).unwrap();
        let seed_b64 = key_json["seed_b64"].as_str().unwrap();

        let mut keyring = Keyring::load(dir.path()).unwrap();
        let report = keyring.revoke_key("one", false).unwrap();
        let rendered = serde_json::to_string(&report).unwrap();
        assert!(!rendered.contains(seed_b64));
        assert!(!rendered.contains("seed_b64"));
        assert!(!format!("{:?}", keyring.summary()).contains(seed_b64));

        let audit = fs::read_to_string(dir.path().join(KEYRING_EVENTS_FILENAME)).unwrap();
        assert!(audit.contains("key.revoked"), "{audit}");
        assert!(audit.contains("\"key_id\":\"one\""), "{audit}");
        assert!(!audit.contains(seed_b64));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(KEYRING_EVENTS_FILENAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn keyring_audit_reports_lifecycle_events_newest_first_without_secrets() {
        let dir = keyring_dir();
        let _ = generate(&dir, "one");
        let _ = generate(&dir, "two");
        let mut keyring = Keyring::load(dir.path()).unwrap();
        keyring.rotate_to("one").unwrap();
        keyring.revoke_key("two", false).unwrap();

        let report = keyring.audit_events(100).unwrap();
        assert!(report.present);
        assert_eq!(report.event_count, 4);
        assert!(!report.truncated);
        let names: Vec<&str> = report
            .events
            .iter()
            .map(|event| event.event.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "key.revoked",
                "key.rotated",
                "key.generated",
                "key.generated"
            ]
        );
        assert_eq!(report.events[0].details["key_id"].as_str(), Some("two"));
        let rendered = serde_json::to_string(&report).unwrap();
        assert!(!rendered.contains("seed"));
        assert!(!rendered.contains("private"));

        let limited = keyring.audit_events(2).unwrap();
        assert_eq!(limited.event_count, 4);
        assert!(limited.truncated);
        assert_eq!(limited.events.len(), 2);
        assert_eq!(limited.events[0].event, "key.revoked");
        assert_eq!(limited.events[1].event, "key.rotated");

        // Missing logs are reported, not synthesized.
        let empty_dir = keyring_dir();
        let empty = Keyring::create(empty_dir.path()).unwrap();
        let missing = empty.audit_events(10).unwrap();
        assert!(!missing.present);
        assert_eq!(missing.event_count, 0);
    }

    #[test]
    fn keyring_audit_log_rejects_secret_material_and_incomplete_lines() {
        let dir = keyring_dir();
        let _ = generate(&dir, "one");
        let keyring = Keyring::load(dir.path()).unwrap();
        let path = dir.path().join(KEYRING_EVENTS_FILENAME);

        // An interrupted final append is ignored.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"event\":\"key.generated\"").unwrap();
        drop(file);
        let report = keyring.audit_events(10).unwrap();
        assert_eq!(report.event_count, 1);

        // A line that looks like it contains key material fails closed. The
        // writer repairs the torn trailing line before appending this record.
        keyring
            .append_event("key.generated", serde_json::json!({"seed_b64": "AAAA"}))
            .unwrap();
        let error = keyring.audit_events(10).unwrap_err();
        assert!(
            format!("{error:#}").contains("secret material"),
            "{error:#}"
        );
    }

    #[test]
    fn keyring_audit_log_rejects_symlinks_and_oversized_files() {
        let dir = keyring_dir();
        let _ = generate(&dir, "one");
        let keyring = Keyring::load(dir.path()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let path = dir.path().join(KEYRING_EVENTS_FILENAME);
            let moved = dir.path().join("real-events.jsonl");
            fs::rename(&path, &moved).unwrap();
            symlink(&moved, &path).unwrap();
            let error = keyring.audit_events(10).unwrap_err();
            assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        }
    }
}
