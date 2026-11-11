//! Content-addressed, chunked blob envelope for the v2 backup repository.
//!
//! A blob stores the plaintext bytes of one allowlisted data-directory file.
//! The plaintext SHA-256 is the blob's address. The envelope is independently
//! readable, so a repair or audit tool can inspect its header without loading
//! a snapshot manifest.
//!
//! # Envelope layout
//!
//! ```text
//! offset  size  field
//!      0     8  magic "VPBLOB2\0"
//!      8     1  envelope version (1)
//!      9     1  flags: bit0 encrypted, bit1 zstd, other bits reserved
//!     10     2  reserved, must be zero
//!     12    32  SHA-256 of the original plaintext
//!     44     4  chunk size in bytes (little endian)
//!     48    12  random per-blob AEAD base nonce (zero when unencrypted)
//!     60     -  repeated chunks:
//!                 1 byte  flags: bit0 final, other bits reserved
//!                 4 bytes ciphertext length (little endian)
//!                 N bytes ciphertext (plaintext length + 16 when encrypted)
//! ```
//!
//! Every blob ends with exactly one empty final chunk. This makes truncation at
//! a full chunk boundary detectable for encrypted blobs, because the final-chunk
//! flag and the chunk index are authenticated with the chunk plaintext. For
//! unencrypted blobs the SHA-256 and original size checks detect truncation.
//!
//! # Encryption
//!
//! Each chunk is encrypted with ChaCha20-Poly1305 under a key derived by
//! [`super::crypto`]. The nonce is the random per-blob base nonce with the
//! chunk index XORed into its trailing eight bytes, so nonce reuse within a
//! blob is impossible for indices below 2^64. The AAD binds the plaintext hash,
//! compression and encryption flags, chunk size, chunk index, and final-chunk
//! marker. Changing the header, reordering chunks, concatenating chunks from a
//! different blob, or deleting the final chunk fails authentication.
//!
//! Compression is applied before encryption. The plaintext hash always refers
//! to the original file, so dedup remains stable when compression settings
//! change.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::create_private_new;
use super::super::crypto::{RepositoryCrypto, chunk_nonce, decode_hash, random_nonce};

pub(crate) const BLOB_MAGIC: &[u8; 8] = b"VPBLOB2\0";
pub(crate) const BLOB_ENVELOPE_VERSION: u8 = 1;
pub(crate) const BLOB_HEADER_BYTES: usize = 60;
pub(crate) const BLOB_CHUNK_SIZE: usize = 256 * 1024;
pub(crate) const BLOB_MAX_CHUNK_SIZE: usize = 1024 * 1024;
pub(crate) const BLOB_MIN_CHUNK_SIZE: usize = 32;
pub(crate) const BLOB_CHUNK_RECORD_BYTES: usize = 5;
pub(crate) const BLOB_TAG_BYTES: usize = 16;
pub(crate) const BLOB_MAX_PLAINTEXT_BYTES: u64 = 1 << 40;

const BLOB_FLAG_ENCRYPTED: u8 = 0x01;
const BLOB_FLAG_ZSTD: u8 = 0x02;
const BLOB_FLAG_KNOWN: u8 = BLOB_FLAG_ENCRYPTED | BLOB_FLAG_ZSTD;
const BLOB_CHUNK_FLAG_FINAL: u8 = 0x01;
const BLOB_CHUNK_AAD_LABEL: &[u8] = b"vp-backup-v2/blob-chunk-aad\0";
const ZSTD_LEVEL: i32 = 3;
const ZSTD_WINDOW_LOG_MAX: u32 = 27;

/// Compression stored inside one blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    Zstd,
    None,
}

impl Compression {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "zstd" => Ok(Self::Zstd),
            "none" => Ok(Self::None),
            other => bail!("unsupported backup blob compression {other:?}"),
        }
    }
}

impl std::fmt::Display for Compression {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Parsed, unauthenticated blob header.
///
/// Header fields are authenticated by each encrypted chunk's AAD (except for
/// the base nonce, which directly determines decryption). Callers still verify
/// the header's hash against the snapshot manifest before treating a blob as
/// valid.
#[derive(Clone, Debug)]
pub(crate) struct BlobHeader {
    pub encrypted: bool,
    pub compression: Compression,
    pub sha256: [u8; 32],
    pub chunk_size: u32,
    pub base_nonce: [u8; 12],
}

impl BlobHeader {
    pub(crate) fn flags(&self) -> u8 {
        let mut flags = 0u8;
        if self.encrypted {
            flags |= BLOB_FLAG_ENCRYPTED;
        }
        if self.compression == Compression::Zstd {
            flags |= BLOB_FLAG_ZSTD;
        }
        flags
    }
}

/// Result of sealing one file into a blob.
#[derive(Clone, Debug)]
pub(crate) struct SealedBlob {
    pub stored_bytes: u64,
    pub plaintext_bytes: u64,
    pub sha256: String,
    pub compression: Compression,
    pub encrypted: bool,
    pub nonce: [u8; 12],
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedBlob {
    pub plaintext_bytes: u64,
    pub sha256: String,
    pub stored_bytes: u64,
    pub compression: Compression,
    pub encrypted: bool,
}

/// Read only the fixed header and basic file metadata.
pub(crate) fn inspect_blob(path: &Path) -> Result<BlobHeader> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading backup blob {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing symlinked backup blob {}", path.display());
    }
    if !metadata.is_file() {
        bail!("backup blob {} is not a regular file", path.display());
    }
    if metadata.len() < (BLOB_HEADER_BYTES + BLOB_CHUNK_RECORD_BYTES) as u64 {
        bail!("backup blob {} is truncated", path.display());
    }
    let mut file =
        File::open(path).with_context(|| format!("opening backup blob {}", path.display()))?;
    let mut header = [0u8; BLOB_HEADER_BYTES];
    file.read_exact(&mut header)
        .with_context(|| format!("reading backup blob header {}", path.display()))?;
    parse_blob_header(&header)
}

/// Seal `source` into `destination`.
///
/// `destination` must not exist. The caller is expected to publish the finished
/// file into the content-addressed blob path atomically and to remove the
/// temporary file on error.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn seal_blob_file(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
    compression: Compression,
    crypto: Option<&RepositoryCrypto>,
) -> Result<SealedBlob> {
    seal_blob_file_with_chunk_size(
        source,
        destination,
        expected_sha256,
        compression,
        crypto,
        BLOB_CHUNK_SIZE,
    )
}

pub(crate) fn seal_blob_file_with_chunk_size(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
    compression: Compression,
    crypto: Option<&RepositoryCrypto>,
    chunk_size: usize,
) -> Result<SealedBlob> {
    if !(BLOB_MIN_CHUNK_SIZE..=BLOB_MAX_CHUNK_SIZE).contains(&chunk_size) {
        bail!(
            "backup blob chunk size {chunk_size} is outside {}..={}",
            BLOB_MIN_CHUNK_SIZE,
            BLOB_MAX_CHUNK_SIZE
        );
    }
    let expected_hash = decode_hash(expected_sha256)?;
    let source_metadata =
        fs::symlink_metadata(source).with_context(|| format!("reading {}", source.display()))?;
    if source_metadata.file_type().is_symlink() {
        bail!("refusing to seal symlinked file {}", source.display());
    }
    if !source_metadata.is_file() {
        bail!("backup source {} is not a regular file", source.display());
    }
    if source_metadata.len() > BLOB_MAX_PLAINTEXT_BYTES {
        bail!(
            "backup source {} is {} bytes; the per-file limit is {}",
            source.display(),
            source_metadata.len(),
            BLOB_MAX_PLAINTEXT_BYTES
        );
    }
    let encrypted = crypto.is_some();
    let header = BlobHeader {
        encrypted,
        compression,
        sha256: expected_hash,
        chunk_size: chunk_size as u32,
        base_nonce: if encrypted { random_nonce() } else { [0u8; 12] },
    };
    let file = create_private_new(destination)?;
    let writer = BlobWriter::new(file, header.clone(), crypto)?;
    let mut source_file =
        File::open(source).with_context(|| format!("opening {}", source.display()))?;
    let mut reader = HashingReader::new(&mut source_file);
    let plaintext_bytes;
    let file = if compression == Compression::Zstd {
        let mut encoder = zstd::stream::write::Encoder::new(writer, ZSTD_LEVEL)
            .context("starting backup blob compression")?;
        io::copy(&mut reader, &mut encoder)
            .with_context(|| format!("compressing {}", source.display()))?;
        let writer = encoder.finish().context("finishing blob compression")?;
        plaintext_bytes = reader.count;
        writer.finish()?
    } else {
        let mut writer = writer;
        io::copy(&mut reader, &mut writer)
            .with_context(|| format!("reading {}", source.display()))?;
        plaintext_bytes = reader.count;
        writer.finish()?
    };
    let actual = hex_lower(&reader.hasher.finalize());
    if actual != expected_sha256 {
        bail!(
            "backup source {} changed while being read (expected SHA-256 {}, got {})",
            source.display(),
            expected_sha256,
            actual
        );
    }
    if plaintext_bytes > BLOB_MAX_PLAINTEXT_BYTES {
        bail!("backup source grew beyond the per-file limit");
    }
    file.sync_all()
        .with_context(|| format!("syncing backup blob {}", destination.display()))?;
    let stored_bytes = fs::metadata(destination)
        .with_context(|| format!("reading {}", destination.display()))?
        .len();
    Ok(SealedBlob {
        stored_bytes,
        plaintext_bytes,
        sha256: expected_sha256.to_string(),
        compression,
        encrypted,
        nonce: header.base_nonce,
    })
}

/// Open a blob and yield its *stored* plaintext (still zstd-compressed when the
/// header says so). Callers normally use [`verify_blob_file`] or
/// [`materialize_blob`], which apply decompression and original-hash checks.
pub(crate) fn open_blob_reader<'a>(
    path: &Path,
    expected_sha256: &str,
    expected_compression: Compression,
    crypto: Option<&'a RepositoryCrypto>,
) -> Result<BlobReader<'a, BufReader<File>>> {
    let expected_hash = decode_hash(expected_sha256)?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading backup blob {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing symlinked backup blob {}", path.display());
    }
    if !metadata.is_file() {
        bail!("backup blob {} is not a regular file", path.display());
    }
    if metadata.len() < (BLOB_HEADER_BYTES + BLOB_CHUNK_RECORD_BYTES) as u64 {
        bail!("backup blob {} is truncated", path.display());
    }
    let file =
        File::open(path).with_context(|| format!("opening backup blob {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut header_bytes = [0u8; BLOB_HEADER_BYTES];
    reader
        .read_exact(&mut header_bytes)
        .with_context(|| format!("reading backup blob header {}", path.display()))?;
    let header = parse_blob_header(&header_bytes)?;
    if header.sha256 != expected_hash {
        bail!(
            "backup blob {} header hash does not match the snapshot manifest",
            path.display()
        );
    }
    if header.compression != expected_compression {
        bail!(
            "backup blob {} uses compression {} but the manifest says {}",
            path.display(),
            header.compression,
            expected_compression
        );
    }
    if header.encrypted != crypto.is_some() {
        bail!(
            "backup blob {} encryption flag does not match the repository",
            path.display()
        );
    }
    Ok(BlobReader {
        reader,
        header,
        crypto,
        index: 0,
        current: Vec::new(),
        position: 0,
        done: false,
        source: path.to_path_buf(),
    })
}

/// Verify a blob end to end: decrypt, decompress, count bytes, and compare the
/// original SHA-256 and size from the snapshot manifest.
pub(crate) fn verify_blob_file(
    path: &Path,
    expected_sha256: &str,
    expected_size: u64,
    expected_compression: Compression,
    crypto: Option<&RepositoryCrypto>,
) -> Result<VerifiedBlob> {
    if expected_size > BLOB_MAX_PLAINTEXT_BYTES {
        bail!(
            "backup entry declares {expected_size} bytes; the per-file limit is {BLOB_MAX_PLAINTEXT_BYTES}"
        );
    }
    let reader = open_blob_reader(path, expected_sha256, expected_compression, crypto)?;
    let (bytes, sha256) = consume_blob(reader, io::sink(), expected_size, path)?;
    if bytes != expected_size {
        bail!(
            "backup blob {} restored {} bytes; the manifest says {}",
            path.display(),
            bytes,
            expected_size
        );
    }
    if sha256 != expected_sha256 {
        bail!(
            "backup blob {} plaintext hash mismatch: expected {}, got {}",
            path.display(),
            expected_sha256,
            sha256
        );
    }
    let stored_bytes = fs::metadata(path)
        .with_context(|| format!("reading backup blob {}", path.display()))?
        .len();
    let header = inspect_blob(path)?;
    Ok(VerifiedBlob {
        plaintext_bytes: bytes,
        sha256,
        stored_bytes,
        compression: header.compression,
        encrypted: header.encrypted,
    })
}

/// Verify one blob while writing its original plaintext to `destination`.
///
/// The destination is created with `0600` and must not already exist. The
/// caller is responsible for removing partial output on error; restore always
/// writes into a private scratch directory first.
pub(crate) fn materialize_blob(
    path: &Path,
    destination: &Path,
    expected_sha256: &str,
    expected_size: u64,
    expected_compression: Compression,
    crypto: Option<&RepositoryCrypto>,
) -> Result<VerifiedBlob> {
    if expected_size > BLOB_MAX_PLAINTEXT_BYTES {
        bail!(
            "backup entry declares {expected_size} bytes; the per-file limit is {BLOB_MAX_PLAINTEXT_BYTES}"
        );
    }
    let reader = open_blob_reader(path, expected_sha256, expected_compression, crypto)?;
    let file = create_private_new(destination)?;
    let (bytes, sha256) = consume_blob(reader, file, expected_size, path)?;
    if bytes != expected_size {
        bail!(
            "backup blob {} restored {} bytes; the manifest says {}",
            path.display(),
            bytes,
            expected_size
        );
    }
    if sha256 != expected_sha256 {
        bail!(
            "backup blob {} plaintext hash mismatch: expected {}, got {}",
            path.display(),
            expected_sha256,
            sha256
        );
    }
    let stored_bytes = fs::metadata(path)
        .with_context(|| format!("reading backup blob {}", path.display()))?
        .len();
    let header = inspect_blob(path)?;
    Ok(VerifiedBlob {
        plaintext_bytes: bytes,
        sha256,
        stored_bytes,
        compression: header.compression,
        encrypted: header.encrypted,
    })
}

fn consume_blob<W: Write>(
    reader: BlobReader<'_, BufReader<File>>,
    sink: W,
    expected_size: u64,
    path: &Path,
) -> Result<(u64, String)> {
    let mut sink = HashWriter::new(sink, expected_size);
    if reader.header.compression == Compression::Zstd {
        let mut decoder = zstd::stream::read::Decoder::new(reader)
            .with_context(|| format!("starting decompression for {}", path.display()))?;
        decoder
            .window_log_max(ZSTD_WINDOW_LOG_MAX)
            .with_context(|| format!("limiting decompression window for {}", path.display()))?;
        io::copy(&mut decoder, &mut sink)
            .with_context(|| format!("decompressing {}", path.display()))?;
    } else {
        let mut reader = reader;
        io::copy(&mut reader, &mut sink).with_context(|| format!("reading {}", path.display()))?;
    }
    sink.inner
        .flush()
        .with_context(|| format!("flushing restored {}", path.display()))?;
    let sha256 = hex_lower(&sink.hasher.finalize());
    Ok((sink.count, sha256))
}

struct HashWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    count: u64,
    limit: u64,
}

impl<W: Write> HashWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            count: 0,
            limit,
        }
    }
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let next = self.count.saturating_add(buffer.len() as u64);
        if next > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "backup blob expands beyond the manifest size ({} > {})",
                    next, self.limit
                ),
            ));
        }
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.count += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct HashingReader<R: Read> {
    inner: R,
    hasher: Sha256,
    count: u64,
}

impl<R: Read> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            count: 0,
        }
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        self.count += read as u64;
        Ok(read)
    }
}

pub(crate) struct BlobReader<'a, R: Read> {
    reader: R,
    header: BlobHeader,
    crypto: Option<&'a RepositoryCrypto>,
    index: u64,
    current: Vec<u8>,
    position: usize,
    done: bool,
    source: std::path::PathBuf,
}

impl<R: Read> BlobReader<'_, R> {
    fn next_chunk(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        let final_chunk;
        let ciphertext_len;
        {
            let mut flags = [0u8; 1];
            let read = self.reader.read(&mut flags)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "backup blob {} is truncated before its final chunk",
                        self.source.display()
                    ),
                ));
            }
            if flags[0] & !BLOB_CHUNK_FLAG_FINAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "backup blob {} has unknown chunk flags",
                        self.source.display()
                    ),
                ));
            }
            final_chunk = flags[0] & BLOB_CHUNK_FLAG_FINAL != 0;
            let mut length = [0u8; 4];
            self.reader.read_exact(&mut length)?;
            ciphertext_len = u32::from_le_bytes(length) as usize;
        }
        let chunk_size = self.header.chunk_size as usize;
        let maximum = chunk_size
            + if self.header.encrypted {
                BLOB_TAG_BYTES
            } else {
                0
            };
        if ciphertext_len > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "backup blob {} has a chunk larger than its declared chunk size",
                    self.source.display()
                ),
            ));
        }
        if self.header.encrypted && ciphertext_len < BLOB_TAG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "backup blob {} has a truncated encrypted chunk",
                    self.source.display()
                ),
            ));
        }
        let plaintext_len = if self.header.encrypted {
            ciphertext_len - BLOB_TAG_BYTES
        } else {
            ciphertext_len
        };
        if final_chunk {
            if plaintext_len >= chunk_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "backup blob {} has an oversized final chunk",
                        self.source.display()
                    ),
                ));
            }
        } else if plaintext_len != chunk_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "backup blob {} has a short non-final chunk",
                    self.source.display()
                ),
            ));
        }
        let mut ciphertext = vec![0u8; ciphertext_len];
        self.reader.read_exact(&mut ciphertext)?;
        let aad = chunk_aad(&self.header, self.index, final_chunk);
        let nonce = chunk_nonce(&self.header.base_nonce, self.index);
        let plaintext = if self.header.encrypted {
            let Some(crypto) = self.crypto else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "encrypted backup blob opened without a key",
                ));
            };
            crypto
                .open_blob_chunk(&nonce, &aad, &ciphertext)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:#}")))?
        } else {
            ciphertext
        };
        if plaintext.len() != plaintext_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "backup blob {} chunk length changed during decryption",
                    self.source.display()
                ),
            ));
        }
        self.index = self.index.checked_add(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "backup blob chunk index overflow",
            )
        })?;
        self.current = plaintext;
        self.position = 0;
        if final_chunk {
            let mut extra = [0u8; 1];
            if self.reader.read(&mut extra)? != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "backup blob {} has trailing data after its final chunk",
                        self.source.display()
                    ),
                ));
            }
            self.done = true;
        }
        Ok(())
    }
}

impl<R: Read> Read for BlobReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if self.position < self.current.len() {
                let available = self.current.len() - self.position;
                let take = available.min(output.len());
                output[..take].copy_from_slice(&self.current[self.position..self.position + take]);
                self.position += take;
                return Ok(take);
            }
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
    }
}

pub(crate) struct BlobWriter<'a, W: Write> {
    inner: W,
    header: BlobHeader,
    crypto: Option<&'a RepositoryCrypto>,
    buffer: Vec<u8>,
    index: u64,
    finalized: bool,
}

impl<'a, W: Write> BlobWriter<'a, W> {
    fn new(file: W, header: BlobHeader, crypto: Option<&'a RepositoryCrypto>) -> Result<Self> {
        let mut writer = Self {
            inner: file,
            buffer: Vec::with_capacity(header.chunk_size as usize),
            header,
            crypto,
            index: 0,
            finalized: false,
        };
        let header_bytes = encode_blob_header(&writer.header);
        writer
            .inner
            .write_all(&header_bytes)
            .context("writing backup blob header")?;
        Ok(writer)
    }

    fn flush_chunk(&mut self, final_chunk: bool) -> Result<()> {
        let plaintext = std::mem::take(&mut self.buffer);
        let aad = chunk_aad(&self.header, self.index, final_chunk);
        let nonce = chunk_nonce(&self.header.base_nonce, self.index);
        let ciphertext = if self.header.encrypted {
            let Some(crypto) = self.crypto else {
                bail!("encrypted backup blob writer has no key");
            };
            crypto.seal_blob_chunk(&nonce, &aad, &plaintext)?
        } else {
            plaintext.clone()
        };
        let length = u32::try_from(ciphertext.len())
            .map_err(|_| anyhow!("backup blob chunk length exceeds u32"))?;
        self.inner.write_all(&[u8::from(final_chunk)])?;
        self.inner.write_all(&length.to_le_bytes())?;
        self.inner.write_all(&ciphertext)?;
        self.index = self
            .index
            .checked_add(1)
            .ok_or_else(|| anyhow!("backup blob chunk index overflow"))?;
        self.buffer = Vec::with_capacity(self.header.chunk_size as usize);
        Ok(())
    }

    fn finish(mut self) -> Result<W> {
        if self.finalized {
            bail!("backup blob writer was already finalized");
        }
        // Any buffered data is a short final chunk; an exact multiple of the
        // chunk size is followed by an empty final chunk.
        self.flush_chunk(true)?;
        self.finalized = true;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for BlobWriter<'_, W> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        if self.finalized {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "backup blob writer is finalized",
            ));
        }
        let chunk_size = self.header.chunk_size as usize;
        let total = input.len();
        while !input.is_empty() {
            let space = chunk_size - self.buffer.len();
            let take = space.min(input.len());
            self.buffer.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.buffer.len() == chunk_size {
                self.flush_chunk(false)
                    .map_err(|error| io::Error::other(format!("{error:#}")))?;
            }
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn encode_blob_header(header: &BlobHeader) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(BLOB_HEADER_BYTES);
    bytes.extend_from_slice(BLOB_MAGIC);
    bytes.push(BLOB_ENVELOPE_VERSION);
    bytes.push(header.flags());
    bytes.extend_from_slice(&[0u8, 0u8]);
    bytes.extend_from_slice(&header.sha256);
    bytes.extend_from_slice(&header.chunk_size.to_le_bytes());
    bytes.extend_from_slice(&header.base_nonce);
    bytes
}

fn parse_blob_header(bytes: &[u8]) -> Result<BlobHeader> {
    if bytes.len() != BLOB_HEADER_BYTES {
        bail!("backup blob header has the wrong length");
    }
    if &bytes[0..8] != BLOB_MAGIC {
        bail!("backup blob has an unknown magic header");
    }
    let version = bytes[8];
    if version != BLOB_ENVELOPE_VERSION {
        bail!("unsupported backup blob envelope version {version}");
    }
    let flags = bytes[9];
    if flags & !BLOB_FLAG_KNOWN != 0 {
        bail!("backup blob has unknown flags");
    }
    if bytes[10] != 0 || bytes[11] != 0 {
        bail!("backup blob header reserved bytes are not zero");
    }
    let encrypted = flags & BLOB_FLAG_ENCRYPTED != 0;
    let compression = if flags & BLOB_FLAG_ZSTD != 0 {
        Compression::Zstd
    } else {
        Compression::None
    };
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&bytes[12..44]);
    let chunk_size = u32::from_le_bytes(
        bytes[44..48]
            .try_into()
            .map_err(|_| anyhow!("backup blob chunk size is malformed"))?,
    );
    if !(BLOB_MIN_CHUNK_SIZE as u32..=BLOB_MAX_CHUNK_SIZE as u32).contains(&chunk_size) {
        bail!(
            "backup blob chunk size {chunk_size} is outside {}..={}",
            BLOB_MIN_CHUNK_SIZE,
            BLOB_MAX_CHUNK_SIZE
        );
    }
    let mut base_nonce = [0u8; 12];
    base_nonce.copy_from_slice(&bytes[48..60]);
    if !encrypted && base_nonce != [0u8; 12] {
        bail!("unencrypted backup blob has a non-zero nonce");
    }
    Ok(BlobHeader {
        encrypted,
        compression,
        sha256,
        chunk_size,
        base_nonce,
    })
}

fn chunk_aad(header: &BlobHeader, index: u64, final_chunk: bool) -> Vec<u8> {
    let mut aad = Vec::with_capacity(BLOB_CHUNK_AAD_LABEL.len() + 32 + 16);
    aad.extend_from_slice(BLOB_CHUNK_AAD_LABEL);
    aad.extend_from_slice(&header.sha256);
    aad.push(header.flags());
    aad.extend_from_slice(&header.chunk_size.to_le_bytes());
    aad.extend_from_slice(&index.to_le_bytes());
    aad.push(u8::from(final_chunk));
    aad
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[allow(dead_code)]
fn blob_hash_from_path(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let hash = name.strip_suffix(".blob")?;
    if hash.len() == 64
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        Some(hash.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::crypto::sha256_hex;
    use crate::backup::crypto::{KdfConfig, KdfParameters, Passphrase};
    use tempfile::TempDir;

    fn test_crypto() -> RepositoryCrypto {
        let kdf = KdfParameters::generate(KdfConfig::fast_for_tests()).unwrap();
        let passphrase = Passphrase::new(b"blob-test-passphrase".to_vec()).unwrap();
        RepositoryCrypto::derive(&passphrase, &kdf).unwrap()
    }

    fn write_source(dir: &TempDir, name: &str, bytes: &[u8]) -> (std::path::PathBuf, String) {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        (path, sha256_hex(bytes))
    }

    fn round_trip(bytes: &[u8], compression: Compression, encrypted: bool, chunk_size: usize) {
        let dir = TempDir::new().unwrap();
        let crypto = encrypted.then(test_crypto);
        let (source, hash) = write_source(&dir, "source", bytes);
        let destination = dir.path().join("blob");
        let sealed = seal_blob_file_with_chunk_size(
            &source,
            &destination,
            &hash,
            compression,
            crypto.as_ref(),
            chunk_size,
        )
        .unwrap();
        assert_eq!(sealed.plaintext_bytes, bytes.len() as u64);
        assert_eq!(sealed.sha256, hash);
        assert_eq!(sealed.compression, compression);
        assert_eq!(sealed.encrypted, encrypted);
        assert!(sealed.stored_bytes >= (BLOB_HEADER_BYTES + BLOB_CHUNK_RECORD_BYTES) as u64);

        let header = inspect_blob(&destination).unwrap();
        assert_eq!(header.compression, compression);
        assert_eq!(header.encrypted, encrypted);
        assert_eq!(header.chunk_size as usize, chunk_size);

        let verified = verify_blob_file(
            &destination,
            &hash,
            bytes.len() as u64,
            compression,
            crypto.as_ref(),
        )
        .unwrap();
        assert_eq!(verified.plaintext_bytes, bytes.len() as u64);
        assert_eq!(verified.sha256, hash);
        assert_eq!(verified.stored_bytes, sealed.stored_bytes);

        let restored = dir.path().join("restored");
        let materialized = materialize_blob(
            &destination,
            &restored,
            &hash,
            bytes.len() as u64,
            compression,
            crypto.as_ref(),
        )
        .unwrap();
        assert_eq!(materialized.plaintext_bytes, bytes.len() as u64);
        assert_eq!(fs::read(&restored).unwrap(), bytes);
    }

    #[test]
    fn encrypted_and_plain_blob_round_trips() {
        for compression in [Compression::None, Compression::Zstd] {
            for encrypted in [false, true] {
                for size in [0usize, 1, 31, 32, 33, 1024, 4096] {
                    let bytes = (0..size)
                        .map(|index| (index.wrapping_mul(37) & 0xff) as u8)
                        .collect::<Vec<_>>();
                    round_trip(&bytes, compression, encrypted, 32);
                }
            }
        }
    }

    #[test]
    fn multi_chunk_blobs_use_distinct_nonces_and_authenticate_every_chunk() {
        let dir = TempDir::new().unwrap();
        let crypto = test_crypto();
        let bytes = vec![0x5au8; 10 * 1024];
        let (source, hash) = write_source(&dir, "source", &bytes);
        let first = dir.path().join("first.blob");
        let second = dir.path().join("second.blob");
        let first_info = seal_blob_file_with_chunk_size(
            &source,
            &first,
            &hash,
            Compression::None,
            Some(&crypto),
            1024,
        )
        .unwrap();
        let second_info = seal_blob_file_with_chunk_size(
            &source,
            &second,
            &hash,
            Compression::None,
            Some(&crypto),
            1024,
        )
        .unwrap();
        assert_ne!(first_info.nonce, second_info.nonce);
        let first_bytes = fs::read(&first).unwrap();
        let second_bytes = fs::read(&second).unwrap();
        assert_ne!(first_bytes, second_bytes);
        verify_blob_file(
            &first,
            &hash,
            bytes.len() as u64,
            Compression::None,
            Some(&crypto),
        )
        .unwrap();
        verify_blob_file(
            &second,
            &hash,
            bytes.len() as u64,
            Compression::None,
            Some(&crypto),
        )
        .unwrap();
    }

    #[test]
    fn empty_final_chunk_marks_exact_multiple_boundaries() {
        let dir = TempDir::new().unwrap();
        let bytes = vec![7u8; 64];
        let (source, hash) = write_source(&dir, "source", &bytes);
        let blob = dir.path().join("blob");
        let info =
            seal_blob_file_with_chunk_size(&source, &blob, &hash, Compression::None, None, 32)
                .unwrap();
        assert!(info.stored_bytes > BLOB_HEADER_BYTES as u64);
        verify_blob_file(&blob, &hash, 64, Compression::None, None).unwrap();
        // Tampering with a full non-final chunk is detected by the hash for
        // plaintext blobs even though there is no AEAD tag.
        let mut bytes = fs::read(&blob).unwrap();
        bytes[BLOB_HEADER_BYTES + 5] ^= 0xff;
        fs::write(&blob, bytes).unwrap();
        assert!(verify_blob_file(&blob, &hash, 64, Compression::None, None).is_err());
    }

    #[test]
    fn encrypted_blob_tampering_truncation_and_trailing_data_fail() {
        let dir = TempDir::new().unwrap();
        let crypto = test_crypto();
        let bytes = vec![0x19u8; 4096];
        let (source, hash) = write_source(&dir, "source", &bytes);
        let blob = dir.path().join("blob");
        seal_blob_file_with_chunk_size(
            &source,
            &blob,
            &hash,
            Compression::None,
            Some(&crypto),
            1024,
        )
        .unwrap();
        verify_blob_file(
            &blob,
            &hash,
            bytes.len() as u64,
            Compression::None,
            Some(&crypto),
        )
        .unwrap();
        let original = fs::read(&blob).unwrap();

        let mut tampered = original.clone();
        let index = BLOB_HEADER_BYTES + 10;
        tampered[index] ^= 0x01;
        fs::write(&blob, &tampered).unwrap();
        assert!(
            verify_blob_file(
                &blob,
                &hash,
                bytes.len() as u64,
                Compression::None,
                Some(&crypto)
            )
            .is_err()
        );

        let truncated = &original[..original.len() - 1];
        fs::write(&blob, truncated).unwrap();
        assert!(
            verify_blob_file(
                &blob,
                &hash,
                bytes.len() as u64,
                Compression::None,
                Some(&crypto)
            )
            .is_err()
        );

        let mut trailing = original.clone();
        trailing.push(0);
        fs::write(&blob, trailing).unwrap();
        assert!(
            verify_blob_file(
                &blob,
                &hash,
                bytes.len() as u64,
                Compression::None,
                Some(&crypto)
            )
            .is_err()
        );

        let mut changed_header = original.clone();
        changed_header[12] ^= 0xff;
        fs::write(&blob, changed_header).unwrap();
        assert!(
            verify_blob_file(
                &blob,
                &hash,
                bytes.len() as u64,
                Compression::None,
                Some(&crypto)
            )
            .is_err()
        );

        assert!(
            verify_blob_file(
                &blob,
                &"0".repeat(64),
                bytes.len() as u64,
                Compression::None,
                Some(&crypto)
            )
            .is_err()
        );
        assert!(
            verify_blob_file(
                &blob,
                &hash,
                bytes.len() as u64,
                Compression::Zstd,
                Some(&crypto)
            )
            .is_err()
        );
        assert!(
            verify_blob_file(&blob, &hash, bytes.len() as u64, Compression::None, None).is_err()
        );
    }

    #[test]
    fn decompression_size_limit_is_enforced_before_writing_unbounded_data() {
        let dir = TempDir::new().unwrap();
        let crypto = test_crypto();
        let bytes = vec![0xa5u8; 128 * 1024];
        let (source, hash) = write_source(&dir, "source", &bytes);
        let blob = dir.path().join("blob");
        seal_blob_file_with_chunk_size(
            &source,
            &blob,
            &hash,
            Compression::Zstd,
            Some(&crypto),
            4096,
        )
        .unwrap();
        let error =
            verify_blob_file(&blob, &hash, 1024, Compression::Zstd, Some(&crypto)).unwrap_err();
        assert!(
            format!("{error:#}").contains("expands beyond")
                || format!("{error:#}").contains("bytes"),
            "unexpected error: {error:#}"
        );
        assert!(
            materialize_blob(
                &blob,
                &dir.path().join("out"),
                &hash,
                1024,
                Compression::Zstd,
                Some(&crypto)
            )
            .is_err()
        );
    }

    #[test]
    fn blob_header_and_source_validation() {
        let dir = TempDir::new().unwrap();
        let bytes = b"payload";
        let (source, hash) = write_source(&dir, "source", bytes);
        assert!(
            seal_blob_file_with_chunk_size(
                &source,
                &dir.path().join("blob"),
                &hash,
                Compression::None,
                None,
                8
            )
            .is_err()
        );
        assert!(
            seal_blob_file_with_chunk_size(
                &source,
                &dir.path().join("blob"),
                &hash,
                Compression::None,
                None,
                BLOB_MAX_CHUNK_SIZE + 1
            )
            .is_err()
        );
        assert!(
            seal_blob_file_with_chunk_size(
                &source,
                &dir.path().join("blob"),
                &"z".repeat(64),
                Compression::None,
                None,
                BLOB_CHUNK_SIZE
            )
            .is_err()
        );

        let blob = dir.path().join("blob");
        seal_blob_file(&source, &blob, &hash, Compression::None, None).unwrap();
        assert!(seal_blob_file(&source, &blob, &hash, Compression::None, None).is_err());

        let mut header = fs::read(&blob).unwrap();
        header[8] = 9;
        let bad_version = dir.path().join("bad-version.blob");
        fs::write(&bad_version, &header).unwrap();
        assert!(inspect_blob(&bad_version).is_err());

        let mut header = fs::read(&blob).unwrap();
        header[9] |= 0x80;
        let bad_flags = dir.path().join("bad-flags.blob");
        fs::write(&bad_flags, &header).unwrap();
        assert!(inspect_blob(&bad_flags).is_err());

        let mut header = fs::read(&blob).unwrap();
        header[10] = 1;
        let bad_reserved = dir.path().join("bad-reserved.blob");
        fs::write(&bad_reserved, &header).unwrap();
        assert!(inspect_blob(&bad_reserved).is_err());

        let mut header = fs::read(&blob).unwrap();
        header[44..48].copy_from_slice(&1u32.to_le_bytes());
        let bad_chunk = dir.path().join("bad-chunk.blob");
        fs::write(&bad_chunk, &header).unwrap();
        assert!(inspect_blob(&bad_chunk).is_err());

        let truncated = dir.path().join("truncated.blob");
        fs::write(&truncated, &fs::read(&blob).unwrap()[..BLOB_HEADER_BYTES]).unwrap();
        assert!(inspect_blob(&truncated).is_err());
        assert!(
            verify_blob_file(
                &truncated,
                &hash,
                bytes.len() as u64,
                Compression::None,
                None
            )
            .is_err()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = dir.path().join("link.blob");
            symlink(&blob, &link).unwrap();
            assert!(inspect_blob(&link).is_err());
            assert!(
                verify_blob_file(&link, &hash, bytes.len() as u64, Compression::None, None)
                    .is_err()
            );
        }
    }

    #[test]
    fn compression_names_are_stable() {
        assert_eq!(Compression::Zstd.as_str(), "zstd");
        assert_eq!(Compression::None.as_str(), "none");
        assert_eq!(Compression::parse("zstd").unwrap(), Compression::Zstd);
        assert_eq!(Compression::parse("none").unwrap(), Compression::None);
        assert!(Compression::parse("gzip").is_err());
        assert_eq!(
            serde_json::to_string(&Compression::None).unwrap(),
            "\"none\""
        );
    }
}
