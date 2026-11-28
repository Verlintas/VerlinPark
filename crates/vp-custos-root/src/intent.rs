//! Ed25519 role intents, 2-of-3 quorum verification, and replay protection.
//!
//! Every intent is a canonical-JSON payload signed by role keys with an
//! explicit domain separator. `core`, `warden`, and `anchor` vote; `sentinel`
//! is liveness-only and can never satisfy a quorum. Verification is pure and
//! takes the current Unix time as an argument, so expiry tests are
//! deterministic.
//!
//! Scope limits: the target hash is compared as an opaque 32-byte commitment.
//! Resolving `pid + start time + code signature`, a commit, or a lease
//! generation is a later executor phase. The replay cache is a root-owned file
//! marker, not a distributed or cryptographic nonce service.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{canonical_json, is_lower_hex, sha256_hex};

/// Domain separator for intent signatures.
pub const INTENT_DOMAIN: &[u8] = b"VP-CUSTOS-ROOT-INTENT-V1\0";
/// Current payload schema version.
pub const INTENT_VERSION: u8 = 1;
/// Accepted clock skew for a not-yet-valid intent.
pub const MAX_CLOCK_SKEW_SECS: i64 = 60;
/// Maximum intent lifetime.
pub const MAX_INTENT_TTL_SECS: i64 = 300;
/// Number of voting roles required.
pub const QUORUM_REQUIRED: usize = 2;
/// Hard bound on signatures accepted in one bundle.
pub const MAX_SIGNATURES: usize = 8;

/// A CUSTOS Advanced process role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Core,
    Warden,
    Anchor,
    Sentinel,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Core, Role::Warden, Role::Anchor, Role::Sentinel];
    pub const VOTING: [Role; 3] = [Role::Core, Role::Warden, Role::Anchor];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Warden => "warden",
            Self::Anchor => "anchor",
            Self::Sentinel => "sentinel",
        }
    }

    /// Sentinel has liveness authority only and never votes.
    pub fn votes(self) -> bool {
        matches!(self, Self::Core | Self::Warden | Self::Anchor)
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The signed part of an intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentPayload {
    pub version: u8,
    pub intent_id: String,
    pub action_id: String,
    pub epoch: u64,
    pub issued_at_unix: i64,
    pub expires_at_unix: i64,
    pub target_hash: String,
    pub nonce: String,
}

impl IntentPayload {
    /// Canonical bytes covered by the domain-separated signature.
    pub fn signing_message(&self) -> Result<Vec<u8>, IntentError> {
        signing_message(self)
    }
}

/// One role's signature over the shared payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSignature {
    pub role: Role,
    pub key_id: String,
    pub signature: String,
}

/// A payload plus one or more role signatures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedIntent {
    pub schema: String,
    pub payload: IntentPayload,
    pub signatures: Vec<RoleSignature>,
}

impl SignedIntent {
    pub fn new(payload: IntentPayload, signatures: Vec<RoleSignature>) -> Self {
        Self {
            schema: "vp-custos-root-intent-v1".to_string(),
            payload,
            signatures,
        }
    }
}

/// A trusted root-owned public key. Key ids are derived from the key bytes, not
/// caller input, so a signature cannot select an arbitrary registry slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedKey {
    pub role: Role,
    pub key_id: String,
    pub public_key: [u8; 32],
}

impl TrustedKey {
    pub fn from_public_key(role: Role, public_key: [u8; 32]) -> Self {
        let key_id = key_id_for_public_key(&public_key);
        Self {
            role,
            key_id,
            public_key,
        }
    }
}

/// Root-owned trusted-key registry. Multiple generations per role are allowed
/// for rotation, but duplicate key ids are rejected and a signature is always
/// bound to the key's registered role.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    keys: Vec<TrustedKey>,
}

impl TrustStore {
    pub fn new(keys: Vec<TrustedKey>) -> Result<Self, TrustStoreError> {
        let mut seen = HashSet::new();
        for key in &keys {
            if !seen.insert(key.key_id.clone()) {
                return Err(TrustStoreError::DuplicateKey(key.key_id.clone()));
            }
        }
        Ok(Self { keys })
    }

    pub fn from_signers<'a>(
        signers: impl IntoIterator<Item = &'a Ed25519Signer>,
    ) -> Result<Self, TrustStoreError> {
        let keys = signers
            .into_iter()
            .map(|signer| TrustedKey::from_public_key(signer.role, signer.public_key_bytes()))
            .collect();
        Self::new(keys)
    }

    pub fn get(&self, key_id: &str) -> Option<&TrustedKey> {
        self.keys.iter().find(|key| key.key_id == key_id)
    }

    pub fn keys(&self) -> &[TrustedKey] {
        &self.keys
    }
}

#[derive(Debug, Error)]
pub enum TrustStoreError {
    #[error("duplicate trusted key id {0}")]
    DuplicateKey(String),
}

/// Verified intent plus the distinct voting roles that signed it.
#[derive(Clone, Debug)]
pub struct VerifiedIntent {
    pub payload: IntentPayload,
    pub signer_roles: Vec<Role>,
}

/// Pure intent verifier. `now_unix` is injected for deterministic tests.
#[derive(Clone, Debug)]
pub struct IntentVerifier {
    trust: TrustStore,
    expected_epoch: u64,
    expected_target_hash: String,
}

impl IntentVerifier {
    pub fn new(
        trust: TrustStore,
        expected_epoch: u64,
        expected_target_hash: impl Into<String>,
    ) -> Result<Self, IntentError> {
        let expected_target_hash = expected_target_hash.into();
        if !is_lower_hex(&expected_target_hash, 64, 64) {
            return Err(IntentError::InvalidTargetHash);
        }
        Ok(Self {
            trust,
            expected_epoch,
            expected_target_hash,
        })
    }

    pub fn expected_epoch(&self) -> u64 {
        self.expected_epoch
    }

    pub fn expected_target_hash(&self) -> &str {
        &self.expected_target_hash
    }

    /// Verify schema, time, epoch, target, signatures, and quorum. Any malformed
    /// or invalid signature fails the whole bundle closed.
    pub fn verify(
        &self,
        signed: &SignedIntent,
        now_unix: i64,
    ) -> Result<VerifiedIntent, IntentError> {
        if signed.schema != "vp-custos-root-intent-v1" {
            return Err(IntentError::InvalidSchema(signed.schema.clone()));
        }
        let payload = &signed.payload;
        if payload.version != INTENT_VERSION {
            return Err(IntentError::UnsupportedVersion(payload.version));
        }
        if !is_lower_hex(&payload.intent_id, 32, 64) {
            return Err(IntentError::InvalidIntentId);
        }
        if !is_lower_hex(&payload.nonce, 32, 64) {
            return Err(IntentError::InvalidNonce);
        }
        if !valid_action_id(&payload.action_id) {
            return Err(IntentError::InvalidActionId);
        }
        if !is_lower_hex(&payload.target_hash, 64, 64) {
            return Err(IntentError::InvalidTargetHash);
        }
        if payload.target_hash != self.expected_target_hash {
            return Err(IntentError::TargetMismatch);
        }
        if payload.epoch < self.expected_epoch {
            return Err(IntentError::StaleEpoch {
                expected: self.expected_epoch,
                actual: payload.epoch,
            });
        }
        if payload.epoch > self.expected_epoch {
            return Err(IntentError::FutureEpoch {
                expected: self.expected_epoch,
                actual: payload.epoch,
            });
        }
        if payload.expires_at_unix <= payload.issued_at_unix {
            return Err(IntentError::InvalidValidity);
        }
        if payload.expires_at_unix <= now_unix {
            return Err(IntentError::Expired);
        }
        if payload.issued_at_unix > now_unix.saturating_add(MAX_CLOCK_SKEW_SECS) {
            return Err(IntentError::NotYetValid);
        }
        if payload
            .expires_at_unix
            .saturating_sub(payload.issued_at_unix)
            > MAX_INTENT_TTL_SECS
        {
            return Err(IntentError::LifetimeTooLong {
                max_secs: MAX_INTENT_TTL_SECS,
            });
        }
        if signed.signatures.is_empty() {
            return Err(IntentError::InsufficientQuorum {
                got: 0,
                need: QUORUM_REQUIRED,
            });
        }
        if signed.signatures.len() > MAX_SIGNATURES {
            return Err(IntentError::TooManySignatures {
                max: MAX_SIGNATURES,
            });
        }

        let message = signing_message(payload)?;
        let mut roles = Vec::with_capacity(signed.signatures.len());
        let mut seen = HashSet::new();
        for signature in &signed.signatures {
            if !signature.role.votes() {
                return Err(IntentError::IneligibleRole(signature.role));
            }
            if !seen.insert(signature.role) {
                return Err(IntentError::DuplicateRole(signature.role));
            }
            let trusted =
                self.trust
                    .get(&signature.key_id)
                    .ok_or_else(|| IntentError::UnknownKey {
                        key_id: signature.key_id.clone(),
                    })?;
            if trusted.role != signature.role {
                return Err(IntentError::RoleKeyMismatch {
                    key_role: trusted.role,
                    claimed_role: signature.role,
                });
            }
            let signature_bytes =
                decode_fixed::<64>(&signature.signature).ok_or(IntentError::InvalidSignature {
                    role: signature.role,
                })?;
            let verifier = UnparsedPublicKey::new(&ED25519, trusted.public_key);
            verifier.verify(&message, &signature_bytes).map_err(|_| {
                IntentError::InvalidSignature {
                    role: signature.role,
                }
            })?;
            roles.push(signature.role);
        }
        if roles.len() < QUORUM_REQUIRED {
            return Err(IntentError::InsufficientQuorum {
                got: roles.len(),
                need: QUORUM_REQUIRED,
            });
        }
        Ok(VerifiedIntent {
            payload: payload.clone(),
            signer_roles: roles,
        })
    }
}

/// Role key holder. Role daemons use this to sign; tests use fixed seeds.
pub struct Ed25519Signer {
    role: Role,
    key_id: String,
    pair: Ed25519KeyPair,
}

impl Ed25519Signer {
    pub fn from_seed(role: Role, seed: [u8; 32]) -> Result<Self, IntentError> {
        let pair =
            Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|_| IntentError::InvalidSeed)?;
        let public_key = public_key_bytes(&pair);
        let key_id = key_id_for_public_key(&public_key);
        Ok(Self { role, key_id, pair })
    }

    pub fn generate(role: Role) -> Result<Self, IntentError> {
        let mut seed = [0u8; 32];
        SystemRandom::new()
            .fill(&mut seed)
            .map_err(|_| IntentError::RandomUnavailable)?;
        Self::from_seed(role, seed)
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        public_key_bytes(&self.pair)
    }

    pub fn sign_payload(&self, payload: &IntentPayload) -> Result<RoleSignature, IntentError> {
        let message = signing_message(payload)?;
        Ok(RoleSignature {
            role: self.role,
            key_id: self.key_id.clone(),
            signature: self.sign_message(&message),
        })
    }

    /// Sign raw bytes. Prefer [`Self::sign_payload`]; this exists so tests and
    /// future role-daemon transports can construct a signature and still have
    /// the verifier enforce role/key binding.
    pub fn sign_message(&self, message: &[u8]) -> String {
        BASE64.encode(self.pair.sign(message).as_ref())
    }
}

fn public_key_bytes(pair: &Ed25519KeyPair) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(pair.public_key().as_ref());
    bytes
}

fn signing_message(payload: &IntentPayload) -> Result<Vec<u8>, IntentError> {
    let body = canonical_json(payload)?;
    let mut message = Vec::with_capacity(INTENT_DOMAIN.len() + body.len());
    message.extend_from_slice(INTENT_DOMAIN);
    message.extend_from_slice(&body);
    Ok(message)
}

/// Stable key id derived from the public key.
pub fn key_id_for_public_key(public_key: &[u8; 32]) -> String {
    format!("ed25519:{}", &sha256_hex(public_key)[..32])
}

fn decode_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = BASE64.decode(text).ok()?;
    if bytes.len() != N {
        return None;
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Some(out)
}

fn valid_action_id(action_id: &str) -> bool {
    let mut chars = action_id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    action_id.len() <= 64
        && chars.all(|character| {
            character.is_ascii_uppercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
}

/// Atomic replay protection backed by exclusive file creation in the
/// root-owned replay directory. Entries are never garbage-collected in this
/// slice; their lifetime is bounded by disk and operator maintenance.
#[derive(Clone, Debug)]
pub struct ReplayCache {
    dir: std::path::PathBuf,
}

impl ReplayCache {
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn contains(&self, intent_id: &str) -> bool {
        is_lower_hex(intent_id, 32, 64) && self.path(intent_id).exists()
    }

    /// Record an intent id. Returns `false` if it was already present. The
    /// caller must validate `intent_id` first; this also rejects separators.
    pub fn record(&self, intent_id: &str, expires_at_unix: i64) -> io::Result<bool> {
        if !is_lower_hex(intent_id, 32, 64) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid intent id",
            ));
        }
        let path = self.path(intent_id);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(error),
        };
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        writeln!(file, "{expires_at_unix}")?;
        file.sync_all()?;
        Ok(true)
    }

    fn path(&self, intent_id: &str) -> std::path::PathBuf {
        self.dir.join(intent_id)
    }
}

/// Typed intent failures.
#[derive(Debug, Error)]
pub enum IntentError {
    #[error("canonical JSON error: {0}")]
    Canonical(#[from] crate::canonical::CanonicalError),
    #[error("intent schema {0:?} is not supported")]
    InvalidSchema(String),
    #[error("intent version {0} is not supported")]
    UnsupportedVersion(u8),
    #[error("intent id must be lowercase hex (32..=64 chars)")]
    InvalidIntentId,
    #[error("intent nonce must be lowercase hex (32..=64 chars)")]
    InvalidNonce,
    #[error("intent action id is invalid")]
    InvalidActionId,
    #[error("target hash must be 64 lowercase hex characters")]
    InvalidTargetHash,
    #[error("target hash does not match the expected target")]
    TargetMismatch,
    #[error("intent has expired")]
    Expired,
    #[error("intent is not yet valid")]
    NotYetValid,
    #[error("intent validity window is invalid")]
    InvalidValidity,
    #[error("intent lifetime exceeds {max_secs} seconds")]
    LifetimeTooLong { max_secs: i64 },
    #[error("stale intent epoch: expected {expected}, got {actual}")]
    StaleEpoch { expected: u64, actual: u64 },
    #[error("future intent epoch: expected {expected}, got {actual}")]
    FutureEpoch { expected: u64, actual: u64 },
    #[error("role {0} is not eligible to vote")]
    IneligibleRole(Role),
    #[error("duplicate signer role {0}")]
    DuplicateRole(Role),
    #[error("unknown signing key {key_id}")]
    UnknownKey { key_id: String },
    #[error("key belongs to {key_role}, but the signature claims {claimed_role}")]
    RoleKeyMismatch { key_role: Role, claimed_role: Role },
    #[error("invalid signature for role {role}")]
    InvalidSignature { role: Role },
    #[error("insufficient quorum: {got} valid signatures, need {need}")]
    InsufficientQuorum { got: usize, need: usize },
    #[error("intent carries more than {max} signatures")]
    TooManySignatures { max: usize },
    #[error("signing seed is not a valid Ed25519 seed")]
    InvalidSeed,
    #[error("system random source is unavailable")]
    RandomUnavailable,
}
