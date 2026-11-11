//! Authenticated encryption for the v2 backup repository.
//!
//! The v1 ZIP format is unencrypted and remains supported by [`super`]. This
//! module implements the cryptographic primitives used by the repository
//! format:
//!
//! - Argon2id password-based key derivation with a random 16-byte salt and
//!   bounded memory/time/parallelism parameters;
//! - domain-separated subkeys for blobs, snapshot manifests, and repository
//!   metadata;
//! - ChaCha20-Poly1305 authenticated encryption for manifest bodies and blob
//!   chunks;
//! - HMAC-SHA-256 authentication for the mutable repository metadata (the
//!   prune tombstones and encryption parameters);
//! - SHA-256 of plaintext for content addressing and post-decryption
//!   verification.
//!
//! # Secret handling
//!
//! A passphrase is accepted only as owned bytes wrapped in [`Passphrase`]. Its
//! `Debug` implementation is redacted, it is capped at [`MAX_PASSPHRASE_BYTES`],
//! and its buffer is zeroized when dropped. Derived root and subkeys use
//! [`Zeroizing`] buffers. No function in this module logs, formats, or returns
//! key material or passphrase bytes. The caller is responsible for not putting a
//! passphrase in process arguments; the CLI accepts only an environment-variable
//! name or a permissions-checked file path.
//!
//! # Bounds
//!
//! All KDF parameters read from an on-disk repository are validated before
//! Argon2 runs. This prevents a hostile or corrupt repository from requesting
//! unbounded memory. The exact bounds are public constants and are also stated
//! in ADR-0167.
//!
//! This is a same-machine, offline backup format. It provides confidentiality
//! and integrity against a party that obtains repository files, but it does not
//! provide key escrow, key rotation, multi-user access control, or protection
//! against a compromised host while a backup or restore is running.

use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Minimum Argon2id memory cost accepted from an existing repository.
pub const KDF_MIN_M_COST_KIB: u32 = 8 * 1024;
/// Maximum Argon2id memory cost accepted from an existing repository.
pub const KDF_MAX_M_COST_KIB: u32 = 256 * 1024;
/// Minimum Argon2id iteration count.
pub const KDF_MIN_T_COST: u32 = 1;
/// Maximum Argon2id iteration count.
pub const KDF_MAX_T_COST: u32 = 10;
/// Minimum Argon2id parallelism.
pub const KDF_MIN_P_COST: u32 = 1;
/// Maximum Argon2id parallelism.
pub const KDF_MAX_P_COST: u32 = 8;
/// Fixed output length for the Argon2id root key.
pub const KDF_OUTPUT_LEN: u32 = 32;
/// Random salt length in bytes.
pub const KDF_SALT_LEN: usize = 16;
/// Default memory cost for newly created repositories (64 MiB).
pub const DEFAULT_KDF_M_COST_KIB: u32 = 64 * 1024;
/// Default iteration count for newly created repositories.
pub const DEFAULT_KDF_T_COST: u32 = 3;
/// Default parallelism for newly created repositories.
pub const DEFAULT_KDF_P_COST: u32 = 1;
/// Argon2 algorithm identifier persisted in repository metadata.
pub const KDF_ALGORITHM: &str = "argon2id";
/// Argon2 version 1.3 (`0x13`), the current Argon2id version.
pub const KDF_VERSION: u32 = 0x13;
/// Maximum passphrase length accepted from a caller.
pub const MAX_PASSPHRASE_BYTES: usize = 4096;
/// Maximum plaintext size of a snapshot manifest.
pub const MAX_MANIFEST_PLAINTEXT_BYTES: usize = 32 * 1024 * 1024;

const MANIFEST_MAGIC: &[u8; 8] = b"VPMANI2\0";
const MANIFEST_ENVELOPE_VERSION: u8 = 1;
const MANIFEST_FLAG_ENCRYPTED: u8 = 0x01;
const MANIFEST_HEADER_BYTES: usize = 8 + 1 + 1 + 2 + 32 + 8 + 12;

const BLOB_KEY_LABEL: &[u8] = b"vp-backup-v2/blob-key\0";
const MANIFEST_KEY_LABEL: &[u8] = b"vp-backup-v2/manifest-key\0";
const METADATA_KEY_LABEL: &[u8] = b"vp-backup-v2/metadata-key\0";
const MANIFEST_AAD_LABEL: &[u8] = b"vp-backup-v2/manifest-aad\0";

/// KDF parameters persisted in `repository.json`.
///
/// The salt is not a secret. The passphrase and derived key are never stored.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KdfParameters {
    pub algorithm: String,
    pub version: u32,
    /// Standard base64 of the random salt.
    pub salt_b64: String,
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub output_len: u32,
}

impl KdfParameters {
    /// Generate parameters with a fresh cryptographic salt.
    pub fn generate(config: KdfConfig) -> Result<Self> {
        config.validate()?;
        let mut salt = [0u8; KDF_SALT_LEN];
        rand::rng().fill_bytes(&mut salt);
        Ok(Self {
            algorithm: KDF_ALGORITHM.to_string(),
            version: KDF_VERSION,
            salt_b64: BASE64.encode(salt),
            m_cost_kib: config.m_cost_kib,
            t_cost: config.t_cost,
            p_cost: config.p_cost,
            output_len: KDF_OUTPUT_LEN,
        })
    }

    /// Validate every field before it is used to derive a key.
    ///
    /// This is deliberately strict: unknown algorithms, a non-canonical salt,
    /// weak or excessive KDF costs, and an output length other than 32 bytes
    /// are all rejected. A repository written by a future format version must
    /// use a future repository format version rather than silently changing
    /// these semantics.
    pub fn validate(&self) -> Result<()> {
        if self.algorithm != KDF_ALGORITHM {
            bail!(
                "unsupported backup KDF algorithm {:?}; expected {KDF_ALGORITHM}",
                self.algorithm
            );
        }
        if self.version != KDF_VERSION {
            bail!(
                "unsupported Argon2 version {}; expected {KDF_VERSION}",
                self.version
            );
        }
        if self.output_len != KDF_OUTPUT_LEN {
            bail!(
                "unsupported backup KDF output length {}; expected {KDF_OUTPUT_LEN}",
                self.output_len
            );
        }
        if !(KDF_MIN_M_COST_KIB..=KDF_MAX_M_COST_KIB).contains(&self.m_cost_kib) {
            bail!(
                "backup KDF memory cost {} KiB is outside the supported range {}..={} KiB",
                self.m_cost_kib,
                KDF_MIN_M_COST_KIB,
                KDF_MAX_M_COST_KIB
            );
        }
        if !(KDF_MIN_T_COST..=KDF_MAX_T_COST).contains(&self.t_cost) {
            bail!(
                "backup KDF time cost {} is outside the supported range {}..={}",
                self.t_cost,
                KDF_MIN_T_COST,
                KDF_MAX_T_COST
            );
        }
        if !(KDF_MIN_P_COST..=KDF_MAX_P_COST).contains(&self.p_cost) {
            bail!(
                "backup KDF parallelism {} is outside the supported range {}..={}",
                self.p_cost,
                KDF_MIN_P_COST,
                KDF_MAX_P_COST
            );
        }
        if self.salt_b64.len() > 128 {
            bail!("backup KDF salt is too large");
        }
        let salt = BASE64
            .decode(self.salt_b64.as_bytes())
            .context("backup KDF salt is not valid base64")?;
        if salt.len() != KDF_SALT_LEN {
            bail!(
                "backup KDF salt is {} bytes; expected {KDF_SALT_LEN}",
                salt.len()
            );
        }
        if BASE64.encode(&salt) != self.salt_b64 {
            bail!("backup KDF salt is not canonical base64");
        }
        Ok(())
    }

    /// Stable public fingerprint of the KDF parameters, including the salt.
    ///
    /// This is used to detect a snapshot manifest paired with different
    /// repository metadata. It contains no secret.
    pub fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        hex_lower(&Sha256::digest(bytes))
    }

    /// Public, redacted summary for reports.
    pub fn summary(&self) -> KdfSummary {
        KdfSummary {
            algorithm: self.algorithm.clone(),
            version: self.version,
            m_cost_kib: self.m_cost_kib,
            t_cost: self.t_cost,
            p_cost: self.p_cost,
            output_len: self.output_len,
            fingerprint: self.fingerprint(),
        }
    }

    fn salt(&self) -> Result<[u8; KDF_SALT_LEN]> {
        let decoded = BASE64
            .decode(self.salt_b64.as_bytes())
            .context("backup KDF salt is not valid base64")?;
        let salt: [u8; KDF_SALT_LEN] = decoded
            .try_into()
            .map_err(|_| anyhow!("backup KDF salt has the wrong length"))?;
        Ok(salt)
    }
}

/// KDF cost configuration for a newly created repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfConfig {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for KdfConfig {
    fn default() -> Self {
        Self {
            m_cost_kib: DEFAULT_KDF_M_COST_KIB,
            t_cost: DEFAULT_KDF_T_COST,
            p_cost: DEFAULT_KDF_P_COST,
        }
    }
}

impl KdfConfig {
    pub fn validate(self) -> Result<()> {
        if !(KDF_MIN_M_COST_KIB..=KDF_MAX_M_COST_KIB).contains(&self.m_cost_kib) {
            bail!(
                "KDF memory cost {} KiB is outside the supported range {}..={} KiB",
                self.m_cost_kib,
                KDF_MIN_M_COST_KIB,
                KDF_MAX_M_COST_KIB
            );
        }
        if !(KDF_MIN_T_COST..=KDF_MAX_T_COST).contains(&self.t_cost) {
            bail!(
                "KDF time cost {} is outside the supported range {}..={}",
                self.t_cost,
                KDF_MIN_T_COST,
                KDF_MAX_T_COST
            );
        }
        if !(KDF_MIN_P_COST..=KDF_MAX_P_COST).contains(&self.p_cost) {
            bail!(
                "KDF parallelism {} is outside the supported range {}..={}",
                self.p_cost,
                KDF_MIN_P_COST,
                KDF_MAX_P_COST
            );
        }
        Ok(())
    }

    /// Fast parameters for tests only. Production callers should use the
    /// default 64 MiB / 3 iteration configuration.
    #[cfg(test)]
    pub(crate) fn fast_for_tests() -> Self {
        Self {
            m_cost_kib: KDF_MIN_M_COST_KIB,
            t_cost: 1,
            p_cost: 1,
        }
    }
}

/// Public KDF description safe to show in reports.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KdfSummary {
    pub algorithm: String,
    pub version: u32,
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub output_len: u32,
    pub fingerprint: String,
}

/// An owned passphrase whose memory is zeroized on drop.
///
/// There is intentionally no `Clone`, no `Display`, and no serialization. Use
/// [`Passphrase::new`] for library callers. The CLI resolves an environment
/// variable or a permissions-checked file and constructs this type; the
/// passphrase is never accepted in argv.
pub struct Passphrase {
    bytes: Zeroizing<Vec<u8>>,
}

impl Passphrase {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            bail!("backup passphrase must not be empty");
        }
        if bytes.len() > MAX_PASSPHRASE_BYTES {
            bail!(
                "backup passphrase is {} bytes; the limit is {MAX_PASSPHRASE_BYTES}",
                bytes.len()
            );
        }
        if bytes.contains(&0) {
            bail!("backup passphrase must not contain NUL bytes");
        }
        Ok(Self {
            bytes: Zeroizing::new(bytes),
        })
    }

    /// Borrow the passphrase for key derivation. Not public outside the crate
    /// so downstream code cannot accidentally log or copy it.
    pub(crate) fn expose(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for Passphrase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Passphrase(<redacted>)")
    }
}

/// Aead key wrapper with zeroization on drop.
pub(crate) struct AeadKey {
    bytes: Zeroizing<[u8; 32]>,
}

impl AeadKey {
    fn derive(root: &[u8; 32], label: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(label);
        digest.update([0u8]);
        digest.update(root);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest.finalize());
        Self {
            bytes: Zeroizing::new(bytes),
        }
    }

    fn cipher(&self) -> Result<ChaCha20Poly1305> {
        ChaCha20Poly1305::new_from_slice(&self.bytes[..])
            .map_err(|_| anyhow!("internal backup key has the wrong length"))
    }

    pub(crate) fn seal(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = self.cipher()?;
        cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| anyhow!("backup encryption failed"))
    }

    pub(crate) fn open(&self, nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
        let cipher = self.cipher()?;
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| {
                anyhow!("backup authentication failed (wrong passphrase or tampered data)")
            })
    }
}

/// Derived keys for one open repository.
///
/// A single Argon2id derivation produces the root key; domain-separated
/// SHA-256 subkeys are used for blobs, manifests, and metadata. Every buffer is
/// zeroized on drop.
pub(crate) struct RepositoryCrypto {
    blob_key: AeadKey,
    manifest_key: AeadKey,
    metadata_key: AeadKey,
}

impl RepositoryCrypto {
    /// Derive repository keys from a passphrase and validated KDF parameters.
    pub(crate) fn derive(passphrase: &Passphrase, kdf: &KdfParameters) -> Result<Self> {
        kdf.validate()?;
        let salt = kdf.salt()?;
        let params = Params::new(
            kdf.m_cost_kib,
            kdf.t_cost,
            kdf.p_cost,
            Some(KDF_OUTPUT_LEN as usize),
        )
        .map_err(|error| anyhow!("invalid backup KDF parameters: {error}"))?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut root = Zeroizing::new([0u8; 32]);
        argon2
            .hash_password_into(passphrase.expose(), &salt, &mut root[..])
            .map_err(|error| anyhow!("backup key derivation failed: {error}"))?;
        let crypto = Self {
            blob_key: AeadKey::derive(&root, BLOB_KEY_LABEL),
            manifest_key: AeadKey::derive(&root, MANIFEST_KEY_LABEL),
            metadata_key: AeadKey::derive(&root, METADATA_KEY_LABEL),
        };
        Ok(crypto)
    }

    /// Seal the canonical JSON body of a snapshot manifest.
    pub(crate) fn seal_manifest(
        &self,
        repository_id: &str,
        snapshot_id: &str,
        plaintext: &[u8],
    ) -> Result<SealedObject> {
        if plaintext.is_empty() {
            bail!("refusing to seal an empty backup manifest");
        }
        if plaintext.len() > MAX_MANIFEST_PLAINTEXT_BYTES {
            bail!(
                "backup manifest is {} bytes; the limit is {}",
                plaintext.len(),
                MAX_MANIFEST_PLAINTEXT_BYTES
            );
        }
        let nonce = random_nonce();
        let aad = manifest_aad(repository_id, snapshot_id, true);
        let ciphertext = self.manifest_key.seal(&nonce, &aad, plaintext)?;
        let plaintext_sha256 = sha256_hex(plaintext);
        let bytes = encode_manifest_envelope(
            MANIFEST_FLAG_ENCRYPTED,
            &nonce,
            &plaintext_sha256,
            plaintext.len() as u64,
            &ciphertext,
        )?;
        Ok(SealedObject {
            bytes,
            nonce,
            plaintext_sha256,
        })
    }

    /// Authenticate and decrypt a snapshot manifest envelope.
    pub(crate) fn open_manifest(
        &self,
        repository_id: &str,
        snapshot_id: &str,
        bytes: &[u8],
    ) -> Result<OpenedObject> {
        let header = parse_manifest_envelope(bytes, true)?;
        if header.plaintext_len as usize > MAX_MANIFEST_PLAINTEXT_BYTES {
            bail!(
                "backup manifest declares {} bytes; the limit is {}",
                header.plaintext_len,
                MAX_MANIFEST_PLAINTEXT_BYTES
            );
        }
        let aad = manifest_aad(repository_id, snapshot_id, true);
        let plaintext = self.manifest_key.open(&header.nonce, &aad, header.body)?;
        if plaintext.len() as u64 != header.plaintext_len {
            bail!("backup manifest plaintext length changed during decryption");
        }
        let actual = sha256_hex(&plaintext);
        if actual != header.plaintext_sha256 {
            bail!("backup manifest hash mismatch after decryption");
        }
        Ok(OpenedObject {
            plaintext,
            encrypted: true,
        })
    }

    /// Compute the metadata MAC over canonical metadata bytes (with the MAC
    /// field absent).
    pub(crate) fn metadata_mac(&self, canonical: &[u8]) -> String {
        hex_lower(&hmac_sha256(&self.metadata_key.bytes[..], canonical))
    }

    /// Verify a metadata MAC in constant time for equal-length values.
    pub(crate) fn verify_metadata_mac(&self, canonical: &[u8], mac: &str) -> Result<()> {
        let expected = self.metadata_mac(canonical);
        if !constant_time_eq(expected.as_bytes(), mac.as_bytes()) {
            bail!(
                "backup repository metadata authentication failed; \
                 the passphrase is wrong or the repository was tampered with"
            );
        }
        Ok(())
    }

    pub(crate) fn seal_blob_chunk(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        self.blob_key.seal(nonce, aad, plaintext)
    }

    pub(crate) fn open_blob_chunk(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        self.blob_key.open(nonce, aad, ciphertext)
    }
}

/// Seal a manifest in the explicitly unencrypted repository mode.
///
/// This still records a plaintext SHA-256 and length so accidental truncation
/// or corruption is detected. It is not authentication: anyone who can rewrite
/// the manifest and all referenced blobs can construct a self-consistent
/// repository. Callers must opt into plaintext mode explicitly.
pub(crate) fn seal_manifest_plain(
    repository_id: &str,
    snapshot_id: &str,
    plaintext: &[u8],
) -> Result<SealedObject> {
    if plaintext.is_empty() {
        bail!("refusing to seal an empty backup manifest");
    }
    if plaintext.len() > MAX_MANIFEST_PLAINTEXT_BYTES {
        bail!(
            "backup manifest is {} bytes; the limit is {}",
            plaintext.len(),
            MAX_MANIFEST_PLAINTEXT_BYTES
        );
    }
    let nonce = [0u8; 12];
    let plaintext_sha256 = sha256_hex(plaintext);
    let bytes = encode_manifest_envelope(
        0,
        &nonce,
        &plaintext_sha256,
        plaintext.len() as u64,
        plaintext,
    )?;
    let _ = (repository_id, snapshot_id);
    Ok(SealedObject {
        bytes,
        nonce,
        plaintext_sha256,
    })
}

/// Open an unencrypted manifest envelope and verify its self-hash.
pub(crate) fn open_manifest_plain(
    repository_id: &str,
    snapshot_id: &str,
    bytes: &[u8],
) -> Result<OpenedObject> {
    let header = parse_manifest_envelope(bytes, false)?;
    if header.plaintext_len as usize > MAX_MANIFEST_PLAINTEXT_BYTES {
        bail!(
            "backup manifest declares {} bytes; the limit is {}",
            header.plaintext_len,
            MAX_MANIFEST_PLAINTEXT_BYTES
        );
    }
    let actual = sha256_hex(header.body);
    if actual != header.plaintext_sha256 {
        bail!("backup manifest hash mismatch");
    }
    let _ = (repository_id, snapshot_id);
    let _ = actual;
    Ok(OpenedObject {
        plaintext: header.body.to_vec(),
        encrypted: false,
    })
}

/// One-shot sealed object (currently only manifests).
pub(crate) struct SealedObject {
    pub bytes: Vec<u8>,
    pub nonce: [u8; 12],
    pub plaintext_sha256: String,
}

#[derive(Debug)]
pub(crate) struct OpenedObject {
    pub plaintext: Vec<u8>,
    pub encrypted: bool,
}

struct ManifestEnvelope<'a> {
    nonce: [u8; 12],
    plaintext_sha256: String,
    plaintext_len: u64,
    body: &'a [u8],
}

fn encode_manifest_envelope(
    flags: u8,
    nonce: &[u8; 12],
    plaintext_sha256: &str,
    plaintext_len: u64,
    body: &[u8],
) -> Result<Vec<u8>> {
    let hash = decode_hash(plaintext_sha256)?;
    let mut bytes = Vec::with_capacity(MANIFEST_HEADER_BYTES + body.len());
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.push(MANIFEST_ENVELOPE_VERSION);
    bytes.push(flags);
    bytes.extend_from_slice(&[0u8, 0u8]);
    bytes.extend_from_slice(&hash);
    bytes.extend_from_slice(&plaintext_len.to_le_bytes());
    bytes.extend_from_slice(nonce);
    bytes.extend_from_slice(body);
    Ok(bytes)
}

fn parse_manifest_envelope(bytes: &[u8], expect_encrypted: bool) -> Result<ManifestEnvelope<'_>> {
    if bytes.len() < MANIFEST_HEADER_BYTES {
        bail!("backup manifest is truncated");
    }
    if &bytes[0..8] != MANIFEST_MAGIC {
        bail!("backup manifest has an unknown magic header");
    }
    let version = bytes[8];
    if version != MANIFEST_ENVELOPE_VERSION {
        bail!("unsupported backup manifest envelope version {version}");
    }
    let flags = bytes[9];
    if flags & !MANIFEST_FLAG_ENCRYPTED != 0 {
        bail!("backup manifest has unknown flags");
    }
    if bytes[10] != 0 || bytes[11] != 0 {
        bail!("backup manifest header reserved bytes are not zero");
    }
    let encrypted = flags & MANIFEST_FLAG_ENCRYPTED != 0;
    if encrypted != expect_encrypted {
        bail!(
            "backup manifest encryption flag does not match the repository (expected {})",
            if expect_encrypted {
                "encrypted"
            } else {
                "unencrypted"
            }
        );
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes[12..44]);
    let plaintext_len = u64::from_le_bytes(
        bytes[44..52]
            .try_into()
            .map_err(|_| anyhow!("backup manifest length field is malformed"))?,
    );
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&bytes[52..64]);
    let body = &bytes[MANIFEST_HEADER_BYTES..];
    if !encrypted {
        if nonce != [0u8; 12] {
            bail!("unencrypted backup manifest has a non-zero nonce");
        }
        if body.len() as u64 != plaintext_len {
            bail!(
                "backup manifest length mismatch: header says {} bytes, envelope has {}",
                plaintext_len,
                body.len()
            );
        }
    } else {
        if body.len() < 16 {
            bail!("encrypted backup manifest is truncated");
        }
        if body.len() as u64 != plaintext_len + 16 {
            bail!(
                "encrypted backup manifest length mismatch: header says {} plaintext bytes, envelope has {} ciphertext bytes",
                plaintext_len,
                body.len()
            );
        }
    }
    Ok(ManifestEnvelope {
        nonce,
        plaintext_sha256: hex_lower(&hash),
        plaintext_len,
        body,
    })
}

fn manifest_aad(repository_id: &str, snapshot_id: &str, encrypted: bool) -> Vec<u8> {
    let mut aad = Vec::with_capacity(64 + repository_id.len() + snapshot_id.len());
    aad.extend_from_slice(MANIFEST_AAD_LABEL);
    aad.push(u8::from(encrypted));
    aad.extend_from_slice(&(repository_id.len() as u16).to_le_bytes());
    aad.extend_from_slice(repository_id.as_bytes());
    aad.extend_from_slice(&(snapshot_id.len() as u16).to_le_bytes());
    aad.extend_from_slice(snapshot_id.as_bytes());
    aad
}

/// Random 96-bit AEAD nonce from the operating system CSPRNG.
pub(crate) fn random_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    nonce
}

/// Derive a per-chunk nonce by XORing the chunk index into the trailing 8
/// bytes of the random per-blob base nonce.
///
/// The index is authenticated in the chunk AAD. Random 96-bit base nonces make
/// cross-blob collisions negligible; within one blob every chunk index maps to
/// a distinct nonce.
pub(crate) fn chunk_nonce(base: &[u8; 12], index: u64) -> [u8; 12] {
    let mut nonce = *base;
    let mask = index.to_le_bytes();
    for (slot, byte) in nonce[4..].iter_mut().zip(mask) {
        *slot ^= byte;
    }
    nonce
}

/// SHA-256 of bytes as lowercase hexadecimal.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

pub(crate) fn decode_hash(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("expected a lowercase SHA-256 hex digest");
    }
    let bytes = value.as_bytes();
    let mut output = [0u8; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2])?;
        let low = hex_nibble(bytes[index * 2 + 1])?;
        *byte = (high << 4) | low;
    }
    Ok(output)
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hash"),
    }
}

/// HMAC-SHA-256 (RFC 2104) used to authenticate mutable repository metadata.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        normalized[..32].copy_from_slice(&digest);
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    let mut output = [0u8; 32];
    output.copy_from_slice(&outer.finalize());
    output
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_kdf() -> KdfParameters {
        KdfParameters::generate(KdfConfig::fast_for_tests()).unwrap()
    }

    fn crypto(passphrase: &str, kdf: &KdfParameters) -> RepositoryCrypto {
        let passphrase = Passphrase::new(passphrase.as_bytes().to_vec()).unwrap();
        RepositoryCrypto::derive(&passphrase, kdf).unwrap()
    }

    #[test]
    fn kdf_parameters_round_trip_and_are_bounded() {
        let kdf = fast_kdf();
        kdf.validate().unwrap();
        assert_eq!(kdf.algorithm, KDF_ALGORITHM);
        assert_eq!(kdf.version, KDF_VERSION);
        assert_eq!(kdf.output_len, KDF_OUTPUT_LEN);
        assert_eq!(kdf.m_cost_kib, KDF_MIN_M_COST_KIB);
        assert_eq!(kdf.t_cost, 1);
        assert_eq!(kdf.p_cost, 1);
        assert_eq!(kdf.summary().fingerprint.len(), 64);
        assert_eq!(kdf.summary().fingerprint, kdf.fingerprint());

        let encoded = serde_json::to_vec(&kdf).unwrap();
        let decoded: KdfParameters = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, kdf);
    }

    #[test]
    fn kdf_parameters_reject_weak_or_excessive_values() {
        let base = fast_kdf();

        let mut value = base.clone();
        value.m_cost_kib = KDF_MIN_M_COST_KIB - 1;
        assert!(value.validate().unwrap_err().to_string().contains("memory"));

        let mut value = base.clone();
        value.m_cost_kib = KDF_MAX_M_COST_KIB + 1;
        assert!(value.validate().unwrap_err().to_string().contains("memory"));

        let mut value = base.clone();
        value.t_cost = 0;
        assert!(value.validate().unwrap_err().to_string().contains("time"));

        let mut value = base.clone();
        value.t_cost = KDF_MAX_T_COST + 1;
        assert!(value.validate().unwrap_err().to_string().contains("time"));

        let mut value = base.clone();
        value.p_cost = 0;
        assert!(
            value
                .validate()
                .unwrap_err()
                .to_string()
                .contains("parallelism")
        );

        let mut value = base.clone();
        value.p_cost = KDF_MAX_P_COST + 1;
        assert!(
            value
                .validate()
                .unwrap_err()
                .to_string()
                .contains("parallelism")
        );

        let mut value = base.clone();
        value.output_len = 64;
        assert!(value.validate().unwrap_err().to_string().contains("output"));

        let mut value = base.clone();
        value.algorithm = "scrypt".to_string();
        assert!(
            value
                .validate()
                .unwrap_err()
                .to_string()
                .contains("algorithm")
        );

        let mut value = base.clone();
        value.version = 0x10;
        assert!(
            value
                .validate()
                .unwrap_err()
                .to_string()
                .contains("version")
        );

        let mut value = base.clone();
        value.salt_b64 = BASE64.encode([0u8; 8]);
        assert!(value.validate().unwrap_err().to_string().contains("salt"));

        let mut value = base;
        value.salt_b64 = "not base64!".to_string();
        assert!(value.validate().is_err());
    }

    #[test]
    fn kdf_config_validation_matches_repository_bounds() {
        assert!(KdfConfig::default().validate().is_ok());
        assert!(KdfConfig::fast_for_tests().validate().is_ok());
        assert!(
            KdfConfig {
                m_cost_kib: 1024,
                ..KdfConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            KdfConfig {
                t_cost: 0,
                ..KdfConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            KdfConfig {
                p_cost: 9,
                ..KdfConfig::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn derivation_is_deterministic_for_the_same_salt_and_passphrase() {
        let kdf = fast_kdf();
        let first = crypto("correct horse battery staple", &kdf);
        let second = crypto("correct horse battery staple", &kdf);
        let aad = b"test";
        let nonce = [7u8; 12];
        let one = first.seal_blob_chunk(&nonce, aad, b"payload").unwrap();
        let two = second.seal_blob_chunk(&nonce, aad, b"payload").unwrap();
        assert_eq!(one, two);
        assert_eq!(
            second.open_blob_chunk(&nonce, aad, &one).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn different_salts_or_passphrases_produce_different_keys() {
        let first_kdf = fast_kdf();
        let second_kdf = fast_kdf();
        assert_ne!(first_kdf.salt_b64, second_kdf.salt_b64);
        let first = crypto("same passphrase", &first_kdf);
        let second = crypto("same passphrase", &second_kdf);
        let third = crypto("different passphrase", &first_kdf);
        let nonce = [3u8; 12];
        let sealed = first.seal_blob_chunk(&nonce, b"aad", b"value").unwrap();
        assert!(second.open_blob_chunk(&nonce, b"aad", &sealed).is_err());
        assert!(third.open_blob_chunk(&nonce, b"aad", &sealed).is_err());
    }

    #[test]
    fn chunk_nonces_are_unique_per_index_and_preserve_the_prefix() {
        let base = random_nonce();
        let mut seen = std::collections::BTreeSet::new();
        for index in 0..1024 {
            let nonce = chunk_nonce(&base, index);
            assert_eq!(&nonce[..4], &base[..4]);
            assert!(seen.insert(nonce), "nonce repeated at index {index}");
        }
        assert_eq!(chunk_nonce(&base, 0), base);
        assert_ne!(chunk_nonce(&base, 1), base);
    }

    #[test]
    fn manifest_envelope_round_trip_and_authentication() {
        let kdf = fast_kdf();
        let crypto = crypto("manifest passphrase", &kdf);
        let plaintext = br#"{"hello":"world"}"#;
        let sealed = crypto
            .seal_manifest("repo-id", "snapshot-id", plaintext)
            .unwrap();
        assert!(sealed.bytes.starts_with(MANIFEST_MAGIC));
        assert_eq!(sealed.plaintext_sha256, sha256_hex(plaintext));
        assert_ne!(sealed.nonce, [0u8; 12]);

        let opened = crypto
            .open_manifest("repo-id", "snapshot-id", &sealed.bytes)
            .unwrap();
        assert_eq!(opened.plaintext, plaintext);
        assert!(opened.encrypted);

        let error = crypto
            .open_manifest("other-repo", "snapshot-id", &sealed.bytes)
            .unwrap_err();
        assert!(format!("{error:#}").contains("authentication"));

        let error = crypto
            .open_manifest("repo-id", "other-snapshot", &sealed.bytes)
            .unwrap_err();
        assert!(format!("{error:#}").contains("authentication"));
    }

    #[test]
    fn manifest_tampering_truncation_and_wrong_password_fail_closed() {
        let kdf = fast_kdf();
        let crypto = crypto("passphrase", &kdf);
        let wrong = self::crypto("not-the-real-passphrase-991", &kdf);
        let plaintext = b"manifest body";
        let sealed = crypto.seal_manifest("repo", "snapshot", plaintext).unwrap();

        let mut tampered = sealed.bytes.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        assert!(wrong.open_manifest("repo", "snapshot", &tampered).is_err());
        assert!(crypto.open_manifest("repo", "snapshot", &tampered).is_err());

        let truncated = &sealed.bytes[..sealed.bytes.len() - 1];
        assert!(crypto.open_manifest("repo", "snapshot", truncated).is_err());

        let empty: &[u8] = &[];
        assert!(crypto.open_manifest("repo", "snapshot", empty).is_err());

        let error = wrong
            .open_manifest("repo", "snapshot", &sealed.bytes)
            .unwrap_err();
        let text = format!("{error:#}");
        assert!(!text.contains("not-the-real-passphrase-991"), "{text}");
        assert!(!text.contains("not-the-real-passphrase-991"), "{text}");
    }

    #[test]
    fn plaintext_manifest_detects_corruption_but_is_not_authenticated() {
        let plaintext = br#"{"plain":true}"#;
        let sealed = seal_manifest_plain("repo", "snapshot", plaintext).unwrap();
        assert_eq!(sealed.nonce, [0u8; 12]);
        let opened = open_manifest_plain("repo", "snapshot", &sealed.bytes).unwrap();
        assert!(!opened.encrypted);
        assert_eq!(opened.plaintext, plaintext);

        let mut corrupted = sealed.bytes.clone();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0x01;
        assert!(open_manifest_plain("repo", "snapshot", &corrupted).is_err());

        // The encrypted opener refuses a plaintext envelope.
        let kdf = fast_kdf();
        let crypto = crypto("p", &kdf);
        assert!(
            crypto
                .open_manifest("repo", "snapshot", &sealed.bytes)
                .is_err()
        );
    }

    #[test]
    fn metadata_mac_authenticates_canonical_bytes() {
        let kdf = fast_kdf();
        let crypto = crypto("metadata passphrase", &kdf);
        let canonical = br#"{"kind":"test","revision":1}"#;
        let mac = crypto.metadata_mac(canonical);
        assert_eq!(mac.len(), 64);
        crypto.verify_metadata_mac(canonical, &mac).unwrap();
        assert!(
            crypto
                .verify_metadata_mac(br#"{"kind":"test","revision":2}"#, &mac)
                .is_err()
        );
        assert!(crypto.verify_metadata_mac(canonical, "00").is_err());
        assert!(
            crypto
                .verify_metadata_mac(canonical, &"0".repeat(64))
                .is_err()
        );

        let other = self::crypto("other passphrase", &kdf);
        assert!(other.verify_metadata_mac(canonical, &mac).is_err());
    }

    #[test]
    fn hmac_matches_rfc_4231_test_case() {
        // RFC 4231 test case 2.
        let key = b"Jefe";
        let data = b"what do ya want for nothing?";
        let expected = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
        assert_eq!(hex_lower(&hmac_sha256(key, data)), expected);
    }

    #[test]
    fn passphrase_is_bounded_and_redacted() {
        assert!(Passphrase::new(Vec::new()).is_err());
        assert!(Passphrase::new(vec![b'a'; MAX_PASSPHRASE_BYTES + 1]).is_err());
        assert!(Passphrase::new(b"has\0nul").is_err());
        let passphrase = Passphrase::new(b"super-secret-value".to_vec()).unwrap();
        let debug = format!("{passphrase:?}");
        assert_eq!(debug, "Passphrase(<redacted>)");
        assert!(!debug.contains("super-secret-value"));
        assert_eq!(passphrase.expose(), b"super-secret-value");
        assert!(
            Passphrase::new(vec![b'a'; MAX_PASSPHRASE_BYTES])
                .unwrap()
                .expose()
                .len()
                == MAX_PASSPHRASE_BYTES
        );
    }

    #[test]
    fn manifest_envelope_header_validation() {
        let sealed = seal_manifest_plain("repo", "snapshot", b"body").unwrap();
        assert_eq!(sealed.bytes.len(), MANIFEST_HEADER_BYTES + 4);

        let mut bad_version = sealed.bytes.clone();
        bad_version[8] = 9;
        assert!(open_manifest_plain("repo", "snapshot", &bad_version).is_err());

        let mut bad_flags = sealed.bytes.clone();
        bad_flags[9] = 0xff;
        assert!(open_manifest_plain("repo", "snapshot", &bad_flags).is_err());

        let mut bad_reserved = sealed.bytes.clone();
        bad_reserved[10] = 1;
        assert!(open_manifest_plain("repo", "snapshot", &bad_reserved).is_err());

        let mut bad_magic = sealed.bytes.clone();
        bad_magic[0] = b'X';
        assert!(open_manifest_plain("repo", "snapshot", &bad_magic).is_err());

        let mut bad_length = sealed.bytes.clone();
        bad_length[44..52].copy_from_slice(&999u64.to_le_bytes());
        assert!(open_manifest_plain("repo", "snapshot", &bad_length).is_err());
    }

    #[test]
    fn oversized_manifest_is_rejected_before_writing() {
        let kdf = fast_kdf();
        let crypto = crypto("passphrase", &kdf);
        let oversized = vec![0u8; MAX_MANIFEST_PLAINTEXT_BYTES + 1];
        assert!(
            crypto
                .seal_manifest("repo", "snapshot", &oversized)
                .is_err()
        );
        assert!(seal_manifest_plain("repo", "snapshot", &oversized).is_err());
    }
}
