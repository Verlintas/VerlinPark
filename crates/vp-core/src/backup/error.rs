//! Typed backup-security failures with stable machine-readable codes.
//!
//! The backup repository is a CLI-first, offline surface, so these errors do
//! not flow through the agent tool-error contract. They follow the same shape:
//! a stable code, a human implication, and an actionable next step. Every
//! `Display` implementation includes the code in brackets so a CLI caller that
//! only records `{error:#}` still preserves the machine-readable value.
//!
//! No variant carries private key material, passphrases, or derived keys. Key
//! references are public identifiers; secret-source names are deliberately not
//! included in error messages.

use thiserror::Error;

/// Stable machine-readable code for a [`BackupSecurityError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackupSecurityCode {
    KeyringNotFound,
    KeyringInvalid,
    KeyringInsecure,
    KeyNotFound,
    KeyInvalid,
    KeyMaterialUnavailable,
    KeyRevoked,
    KeyRevokeActive,
    KeyringRequired,
    SigningRequired,
    SignatureMissing,
    SignatureInvalid,
    UnsignedRepository,
    RekeyRefused,
    RekeyFailed,
    RollbackDetected,
    VerificationFailed,
    AnchorInvalid,
    AnchorIo,
    AnchorSignatureInvalid,
    AnchorStale,
    PortableAnchorInvalid,
    RepositoryFormat,
}

impl BackupSecurityCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KeyringNotFound => "BACKUP_KEYRING_NOT_FOUND",
            Self::KeyringInvalid => "BACKUP_KEYRING_INVALID",
            Self::KeyringInsecure => "BACKUP_KEYRING_INSECURE",
            Self::KeyNotFound => "BACKUP_KEY_NOT_FOUND",
            Self::KeyInvalid => "BACKUP_KEY_INVALID",
            Self::KeyMaterialUnavailable => "BACKUP_KEY_MATERIAL_UNAVAILABLE",
            Self::KeyRevoked => "BACKUP_KEY_REVOKED",
            Self::KeyRevokeActive => "BACKUP_KEY_REVOKE_ACTIVE",
            Self::KeyringRequired => "BACKUP_KEYRING_REQUIRED",
            Self::SigningRequired => "BACKUP_SIGNING_REQUIRED",
            Self::SignatureMissing => "BACKUP_SIGNATURE_MISSING",
            Self::SignatureInvalid => "BACKUP_SIGNATURE_INVALID",
            Self::UnsignedRepository => "BACKUP_UNSIGNED_REPOSITORY",
            Self::RekeyRefused => "BACKUP_REKEY_REFUSED",
            Self::RekeyFailed => "BACKUP_REKEY_FAILED",
            Self::RollbackDetected => "BACKUP_ROLLBACK_DETECTED",
            Self::VerificationFailed => "BACKUP_VERIFY_FAILED",
            Self::AnchorInvalid => "BACKUP_ANCHOR_INVALID",
            Self::AnchorIo => "BACKUP_ANCHOR_IO",
            Self::AnchorSignatureInvalid => "BACKUP_ANCHOR_SIGNATURE_INVALID",
            Self::AnchorStale => "BACKUP_ANCHOR_STALE",
            Self::PortableAnchorInvalid => "BACKUP_PORTABLE_ANCHOR_INVALID",
            Self::RepositoryFormat => "BACKUP_REPOSITORY_FORMAT",
        }
    }

    /// Short operator action associated with the code.
    pub fn hint(self) -> &'static str {
        match self {
            Self::KeyringNotFound => {
                "Create a keyring with `vp backup-key-generate`, or pass `--keyring-dir`."
            }
            Self::KeyringInvalid => {
                "Inspect `vp backup-key-list`; restore keyring.json from a trusted copy."
            }
            Self::KeyringInsecure => {
                "Run `chmod 600` on the keyring or key file; group/other access is refused."
            }
            Self::KeyNotFound => {
                "Run `vp backup-key-list` and pass an existing key id, or generate a new key."
            }
            Self::KeyInvalid => {
                "Regenerate or re-import the key; the stored public/private material disagrees."
            }
            Self::KeyMaterialUnavailable => {
                "Export the referenced environment variable or restore the 0600 key file."
            }
            Self::KeyRevoked => {
                "Rotate to a different signing key, then re-run `vp backup-rekey`; pass `--allow-revoked` only for historical verification."
            }
            Self::KeyRevokeActive => {
                "Rotate to a new active key first with `vp backup-key-rotate`; `--force` revokes the active key and disables signing until a new key is active."
            }
            Self::KeyringRequired => {
                "Pass `--keyring-dir` pointing at the keyring that holds the repository's public keys."
            }
            Self::SigningRequired => {
                "Pass `--signing-key <id>` with a keyring that contains the repository key."
            }
            Self::SignatureMissing => {
                "The object is unsigned; restore it from a signed repository or re-create the backup."
            }
            Self::SignatureInvalid => {
                "Treat the repository as tampered with; do not restore it without investigation."
            }
            Self::UnsignedRepository => {
                "v2 repositories are readable but unsigned; create a v3 signed repository."
            }
            Self::RekeyRefused => {
                "Only signed v3 repositories with a complete keyring can be rekeyed; inspect `vp backup-key-list` and `vp backup-verify`."
            }
            Self::RekeyFailed => {
                "Do not use the repository until rekey finishes successfully; re-run `vp backup-rekey` to resume."
            }
            Self::RollbackDetected => {
                "Re-run with `--allow-rollback` only after confirming the older generation is intended."
            }
            Self::VerificationFailed => {
                "Do not restore this repository; inspect the reported signature, chain, entry, or blob issues."
            }
            Self::AnchorInvalid => {
                "Inspect or reset the local anchor with `vp backup-anchor`; do not ignore silently."
            }
            Self::AnchorIo => {
                "Check data-directory permissions and free space for the anchor file."
            }
            Self::AnchorSignatureInvalid => {
                "The portable anchor was tampered with or signed by a different key; use the keyring that signs your backups."
            }
            Self::AnchorStale => {
                "The portable anchor export is older than the local anchor; export a fresh document from the source host."
            }
            Self::PortableAnchorInvalid => {
                "Inspect the portable anchor JSON; only documents written by `vp backup-anchor-export` are accepted."
            }
            Self::RepositoryFormat => {
                "Use a build that supports this repository format; do not edit repository.json by hand."
            }
        }
    }
}

/// Errors raised by backup signing, keyring, and anti-rollback checks.
#[derive(Debug, Error)]
pub enum BackupSecurityError {
    #[error(
        "no backup signing keyring at {path}; create one with `vp backup-key-generate` or pass `--keyring-dir` [BACKUP_KEYRING_NOT_FOUND]"
    )]
    KeyringNotFound { path: String },

    #[error("backup keyring is invalid: {reason} [BACKUP_KEYRING_INVALID]")]
    KeyringInvalid { reason: String },

    #[error(
        "backup key material at {path} is accessible by group or others (mode {mode:04o}); require 0600 [BACKUP_KEYRING_INSECURE]"
    )]
    KeyringInsecure { path: String, mode: u32 },

    #[error(
        "backup signing key {key_id:?} is not present in the keyring; run `vp backup-key-list` [BACKUP_KEY_NOT_FOUND]"
    )]
    KeyNotFound { key_id: String },

    #[error("backup signing key {key_id:?} is invalid: {reason} [BACKUP_KEY_INVALID]")]
    KeyInvalid { key_id: String, reason: String },

    #[error(
        "private material for backup signing key {key_id:?} is unavailable from its configured source; export the environment variable or restore the 0600 key file [BACKUP_KEY_MATERIAL_UNAVAILABLE]"
    )]
    KeyMaterialUnavailable { key_id: String },

    #[error(
        "backup signing key {key_id:?} was revoked at {revoked_at}; rotate to a trusted key and rekey the repository, or pass `--allow-revoked` for historical verification [BACKUP_KEY_REVOKED]"
    )]
    KeyRevoked { key_id: String, revoked_at: String },

    #[error(
        "backup signing key {key_id:?} is the active key; rotate to a new key first, or pass `--force` to revoke it and disable signing until another key is active [BACKUP_KEY_REVOKE_ACTIVE]"
    )]
    KeyRevokeActive { key_id: String },

    #[error(
        "backup repository is signed but no keyring was provided; pass `--keyring-dir` with its public keys [BACKUP_KEYRING_REQUIRED]"
    )]
    KeyringRequired,

    #[error(
        "backup repository requires a signing key but none was provided; pass `--signing-key <id>` with a keyring [BACKUP_SIGNING_REQUIRED]"
    )]
    SigningRequired,

    #[error("backup {object} is missing its required Ed25519 signature [BACKUP_SIGNATURE_MISSING]")]
    SignatureMissing { object: String },

    #[error(
        "backup signature verification failed for {object} with key {key_id:?}; the repository was tampered with or the wrong key is configured [BACKUP_SIGNATURE_INVALID]"
    )]
    SignatureInvalid { object: String, key_id: String },

    #[error(
        "backup repository format {format_version} is unsigned; v2 repositories remain readable but cannot prove authenticity [BACKUP_UNSIGNED_REPOSITORY]"
    )]
    UnsignedRepository { format_version: u32 },

    #[error("backup rekey refused: {reason} [BACKUP_REKEY_REFUSED]")]
    RekeyRefused { reason: String },

    #[error("backup rekey failed: {reason} [BACKUP_REKEY_FAILED]")]
    RekeyFailed { reason: String },

    #[error(
        "backup rollback rejected: repository {repository_id} generation {generation} is older than the local anchor generation {anchor_generation}; pass `--allow-rollback` only after confirming intent [BACKUP_ROLLBACK_DETECTED]"
    )]
    RollbackDetected {
        repository_id: String,
        generation: u64,
        anchor_generation: u64,
    },

    #[error(
        "backup verification failed; inspect the reported issues before restoring [BACKUP_VERIFY_FAILED]"
    )]
    VerificationFailed,

    #[error("backup anti-rollback anchor is invalid: {reason} [BACKUP_ANCHOR_INVALID]")]
    AnchorInvalid { reason: String },

    #[error(
        "backup anti-rollback anchor could not be read or written: {reason} [BACKUP_ANCHOR_IO]"
    )]
    AnchorIo { reason: String },

    #[error(
        "backup portable anchor signature is invalid or signed by an untrusted key {key_id:?}: {reason} [BACKUP_ANCHOR_SIGNATURE_INVALID]"
    )]
    AnchorSignatureInvalid { key_id: String, reason: String },

    #[error(
        "backup portable anchor exported at {exported_at} is older than the local anchor updated at {local_updated_at}; export a fresh document or omit strict freshness [BACKUP_ANCHOR_STALE]"
    )]
    AnchorStale {
        exported_at: String,
        local_updated_at: String,
    },

    #[error("backup portable anchor is invalid: {reason} [BACKUP_PORTABLE_ANCHOR_INVALID]")]
    PortableAnchorInvalid { reason: String },

    #[error(
        "unsupported backup repository format version {format_version}; this build supports {minimum}..={maximum} [BACKUP_REPOSITORY_FORMAT]"
    )]
    RepositoryFormat {
        format_version: u32,
        minimum: u32,
        maximum: u32,
    },
}

impl BackupSecurityError {
    pub fn code(&self) -> BackupSecurityCode {
        match self {
            Self::KeyringNotFound { .. } => BackupSecurityCode::KeyringNotFound,
            Self::KeyringInvalid { .. } => BackupSecurityCode::KeyringInvalid,
            Self::KeyringInsecure { .. } => BackupSecurityCode::KeyringInsecure,
            Self::KeyNotFound { .. } => BackupSecurityCode::KeyNotFound,
            Self::KeyInvalid { .. } => BackupSecurityCode::KeyInvalid,
            Self::KeyMaterialUnavailable { .. } => BackupSecurityCode::KeyMaterialUnavailable,
            Self::KeyRevoked { .. } => BackupSecurityCode::KeyRevoked,
            Self::KeyRevokeActive { .. } => BackupSecurityCode::KeyRevokeActive,
            Self::KeyringRequired => BackupSecurityCode::KeyringRequired,
            Self::SigningRequired => BackupSecurityCode::SigningRequired,
            Self::SignatureMissing { .. } => BackupSecurityCode::SignatureMissing,
            Self::SignatureInvalid { .. } => BackupSecurityCode::SignatureInvalid,
            Self::UnsignedRepository { .. } => BackupSecurityCode::UnsignedRepository,
            Self::RekeyRefused { .. } => BackupSecurityCode::RekeyRefused,
            Self::RekeyFailed { .. } => BackupSecurityCode::RekeyFailed,
            Self::RollbackDetected { .. } => BackupSecurityCode::RollbackDetected,
            Self::VerificationFailed => BackupSecurityCode::VerificationFailed,
            Self::AnchorInvalid { .. } => BackupSecurityCode::AnchorInvalid,
            Self::AnchorIo { .. } => BackupSecurityCode::AnchorIo,
            Self::AnchorSignatureInvalid { .. } => BackupSecurityCode::AnchorSignatureInvalid,
            Self::AnchorStale { .. } => BackupSecurityCode::AnchorStale,
            Self::PortableAnchorInvalid { .. } => BackupSecurityCode::PortableAnchorInvalid,
            Self::RepositoryFormat { .. } => BackupSecurityCode::RepositoryFormat,
        }
    }

    pub fn hint(&self) -> &'static str {
        self.code().hint()
    }
}

/// Extract a stable backup-security code from an `anyhow` chain, if present.
pub fn backup_security_code(error: &anyhow::Error) -> Option<&'static str> {
    error
        .downcast_ref::<BackupSecurityError>()
        .map(|error| error.code().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_a_stable_unique_code_and_hint() {
        let errors = [
            BackupSecurityError::KeyringNotFound {
                path: "/tmp/keyring.json".into(),
            },
            BackupSecurityError::KeyringInvalid {
                reason: "bad".into(),
            },
            BackupSecurityError::KeyringInsecure {
                path: "/tmp/key".into(),
                mode: 0o644,
            },
            BackupSecurityError::KeyNotFound {
                key_id: "primary".into(),
            },
            BackupSecurityError::KeyInvalid {
                key_id: "primary".into(),
                reason: "bad".into(),
            },
            BackupSecurityError::KeyMaterialUnavailable {
                key_id: "primary".into(),
            },
            BackupSecurityError::KeyRevoked {
                key_id: "primary".into(),
                revoked_at: "2026-11-12T00:00:00Z".into(),
            },
            BackupSecurityError::KeyRevokeActive {
                key_id: "primary".into(),
            },
            BackupSecurityError::KeyringRequired,
            BackupSecurityError::SigningRequired,
            BackupSecurityError::SignatureMissing {
                object: "snapshot".into(),
            },
            BackupSecurityError::SignatureInvalid {
                object: "snapshot".into(),
                key_id: "primary".into(),
            },
            BackupSecurityError::UnsignedRepository { format_version: 2 },
            BackupSecurityError::RekeyRefused {
                reason: "unsigned".into(),
            },
            BackupSecurityError::RekeyFailed {
                reason: "partial".into(),
            },
            BackupSecurityError::RollbackDetected {
                repository_id: "repo".into(),
                generation: 1,
                anchor_generation: 2,
            },
            BackupSecurityError::VerificationFailed,
            BackupSecurityError::AnchorInvalid {
                reason: "bad".into(),
            },
            BackupSecurityError::AnchorIo {
                reason: "bad".into(),
            },
            BackupSecurityError::AnchorSignatureInvalid {
                key_id: "primary".into(),
                reason: "tampered".into(),
            },
            BackupSecurityError::AnchorStale {
                exported_at: "2026-11-01T00:00:00Z".into(),
                local_updated_at: "2026-11-12T00:00:00Z".into(),
            },
            BackupSecurityError::PortableAnchorInvalid {
                reason: "bad".into(),
            },
            BackupSecurityError::RepositoryFormat {
                format_version: 9,
                minimum: 2,
                maximum: 3,
            },
        ];
        let mut codes = std::collections::BTreeSet::new();
        for error in &errors {
            let code = error.code().as_str();
            assert!(codes.insert(code), "duplicate code {code}");
            assert!(!error.hint().is_empty());
            let rendered = error.to_string();
            assert!(rendered.contains(code), "missing code in {rendered}");
            assert!(!rendered.contains("seed"));
            assert!(!rendered.contains("passphrase"));
        }
        assert_eq!(codes.len(), errors.len());
    }

    #[test]
    fn anyhow_downcast_recovers_the_code() {
        let error = anyhow::Error::new(BackupSecurityError::SigningRequired);
        assert_eq!(
            backup_security_code(&error),
            Some("BACKUP_SIGNING_REQUIRED")
        );
        let plain = anyhow::anyhow!("something else");
        assert!(backup_security_code(&plain).is_none());
    }
}
