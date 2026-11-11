//! Offline data-directory backup and restore.
//!
//! The archive is a single ZIP with a `backup.json` manifest, a SQLite
//! snapshot produced by `VACUUM INTO`, the project manifest, per-session event
//! mirrors, and optionally content-addressed artifact blobs. The daemon is
//! never contacted: backup can run while it is live, and restore requires it to
//! be stopped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION, now_rfc3339};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub const BACKUP_FORMAT_VERSION: u32 = 1;
pub const DATABASE_FILENAME: &str = "verlinpark.db";
const MANIFEST_FILENAME: &str = "backup.json";
const PROJECTS_FILENAME: &str = "projects.jsonl";
const SESSIONS_DIR: &str = "sessions";
const ARTIFACTS_DIR: &str = "artifacts";
const ARTIFACT_BLOBS_DIR: &str = "blobs";
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupEntry {
    /// POSIX-style path relative to the data directory.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub created_at: String,
    pub app_version: String,
    pub protocol_version: u32,
    pub includes_artifacts: bool,
    pub sqlite_filename: String,
    pub entries: Vec<BackupEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupReport {
    /// Path of the written ZIP archive.
    pub output: String,
    pub format_version: u32,
    pub created_at: String,
    pub includes_artifacts: bool,
    pub sqlite_filename: String,
    pub entries: usize,
    pub sessions: usize,
    pub artifacts: usize,
    pub bytes: u64,
    pub archive_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    /// Restored data directory.
    pub output: String,
    pub data_dir: String,
    /// Restored copy of `backup.json`.
    pub manifest: String,
    pub format_version: u32,
    pub created_at: String,
    pub includes_artifacts: bool,
    pub entries: usize,
    pub sessions: usize,
    pub artifacts: usize,
    pub bytes: u64,
    pub replaced_existing: bool,
}

/// Create an offline-safe, checksummed backup of `data_dir`.
///
/// The SQLite database is snapshotted with `VACUUM INTO`; WAL and SHM files are
/// never copied. Only the database, `projects.jsonl`, per-session
/// `events.jsonl` mirrors, and (when requested) direct children of
/// `artifacts/blobs` are archived. Discovery, locks, logs, worktrees, scratch
/// data, and secrets are outside the allowlist and are never read.
pub fn create_backup(
    data_dir: &Path,
    output: &Path,
    include_artifacts: bool,
) -> Result<BackupReport> {
    if !data_dir.is_dir() {
        bail!("data directory not found at {}", data_dir.display());
    }
    let output_name = output
        .file_name()
        .ok_or_else(|| anyhow!("backup output must be a file path"))?;
    if output.exists() {
        bail!("backup output already exists at {}", output.display());
    }
    let output_parent = non_empty_parent(output);
    fs::create_dir_all(output_parent)
        .with_context(|| format!("creating {}", output_parent.display()))?;
    let output_parent = fs::canonicalize(output_parent)
        .with_context(|| format!("resolving {}", output_parent.display()))?;
    let data_canonical =
        fs::canonicalize(data_dir).with_context(|| format!("resolving {}", data_dir.display()))?;
    let output_canonical = output_parent.join(output_name);
    if output_canonical == data_canonical.join(DATABASE_FILENAME)
        || output_canonical == data_canonical.join(MANIFEST_FILENAME)
        || output_canonical == data_canonical.join(PROJECTS_FILENAME)
        || output_canonical.starts_with(data_canonical.join(SESSIONS_DIR))
        || output_canonical.starts_with(data_canonical.join(ARTIFACTS_DIR))
    {
        bail!(
            "backup output must not replace data-directory contents: {}",
            output.display()
        );
    }

    let database = data_dir.join(DATABASE_FILENAME);
    let database_meta = fs::symlink_metadata(&database)
        .with_context(|| format!("reading {}", database.display()))?;
    if database_meta.file_type().is_symlink() {
        bail!(
            "refusing to archive symlinked database {}",
            database.display()
        );
    }
    if !database_meta.is_file() {
        bail!(
            "no database at {}; start the daemon once before backing up",
            database.display()
        );
    }

    let scratch = ScratchPath::directory(&output_parent, "backup")?;
    vp_store::snapshot_database(&database, &scratch.path.join(DATABASE_FILENAME))?;

    let mut relatives = BTreeSet::new();
    relatives.insert(DATABASE_FILENAME.to_string());
    relatives.extend(collect_backup_sources(data_dir, include_artifacts)?);
    for relative in &relatives {
        if relative == DATABASE_FILENAME {
            continue;
        }
        copy_private(&data_dir.join(relative), &scratch.path.join(relative))?;
    }

    let mut entries = Vec::with_capacity(relatives.len());
    for relative in relatives {
        classify_archive_path(&relative)?;
        let (size, sha256) = hash_file(&scratch.path.join(&relative))?;
        entries.push(BackupEntry {
            path: relative,
            size,
            sha256,
        });
    }
    let created_at = now_rfc3339();
    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        created_at: created_at.clone(),
        app_version: APP_VERSION.to_string(),
        protocol_version: PROTOCOL_VERSION,
        includes_artifacts: include_artifacts,
        sqlite_filename: DATABASE_FILENAME.to_string(),
        entries,
    };
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).context("serializing backup manifest")?;
    write_private(&scratch.path.join(MANIFEST_FILENAME), &manifest_bytes)?;

    let archive_sha256 = write_archive(&scratch.path, &manifest_bytes, &manifest.entries, output)?;

    Ok(BackupReport {
        output: output.to_string_lossy().to_string(),
        format_version: manifest.format_version,
        created_at,
        includes_artifacts: include_artifacts,
        sqlite_filename: DATABASE_FILENAME.to_string(),
        entries: manifest.entries.len(),
        sessions: manifest
            .entries
            .iter()
            .filter(|entry| matches!(classify_archive_path(&entry.path), Ok(ArchiveKind::Session)))
            .count(),
        artifacts: manifest
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    classify_archive_path(&entry.path),
                    Ok(ArchiveKind::Artifact)
                )
            })
            .count(),
        bytes: manifest.entries.iter().map(|entry| entry.size).sum(),
        archive_sha256,
    })
}

/// Restore a backup created by [`create_backup`] into `data_dir`.
///
/// The archive is validated (manifest, paths, types, sizes, and SHA-256
/// checksums) before the target is touched, then extracted into a sibling
/// temporary directory and moved into place. A live daemon is never stopped or
/// signalled; restore refuses when discovery or the instance lock indicates one
/// is running.
pub fn restore_backup(input: &Path, data_dir: &Path, force: bool) -> Result<RestoreReport> {
    let file =
        File::open(input).with_context(|| format!("opening backup archive {}", input.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("reading backup archive {}", input.display()))?;
    let plan = read_restore_plan(&mut archive)?;
    verify_checksums(&mut archive, &plan)?;

    let data_dir = if data_dir.is_absolute() {
        data_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolving the current directory")?
            .join(data_dir)
    };
    let data_dir = data_dir.as_path();
    if data_dir.file_name().is_none() {
        bail!("restore target must be a directory path");
    }
    let existing = match fs::symlink_metadata(data_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                bail!(
                    "refusing to restore over symlinked target {}",
                    data_dir.display()
                );
            }
            if !meta.is_dir() {
                bail!("restore target {} is not a directory", data_dir.display());
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", data_dir.display()));
        }
    };

    if let Some(reason) = daemon_appears_running(data_dir)? {
        bail!(
            "refusing to restore while a daemon appears to be running: {reason}. \
             Stop verlinpadd for {} and retry; restore never stops a daemon.",
            data_dir.display()
        );
    }
    let had_contents = existing
        && fs::read_dir(data_dir)
            .with_context(|| format!("reading {}", data_dir.display()))?
            .next()
            .is_some();
    if existing && !force {
        if data_dir.join(DATABASE_FILENAME).exists() {
            bail!(
                "target {} contains an existing database; pass --force to replace it",
                data_dir.display()
            );
        }
        if data_dir.join("run").join("daemon.lock").exists() {
            bail!(
                "target {} contains an instance lock; pass --force to replace it",
                data_dir.display()
            );
        }
        if had_contents {
            bail!(
                "target directory {} is not empty; pass --force to replace it",
                data_dir.display()
            );
        }
    }

    let parent = non_empty_parent(data_dir);
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let parent =
        fs::canonicalize(parent).with_context(|| format!("resolving {}", parent.display()))?;
    let mut scratch = ScratchPath::directory(&parent, "restore")?;
    write_private(&scratch.path.join(MANIFEST_FILENAME), &plan.manifest_bytes)?;
    for entry in &plan.manifest.entries {
        extract_entry(&mut archive, &plan.index_by_name, entry, &scratch.path)?;
    }
    verify_sqlite_header(&scratch.path.join(DATABASE_FILENAME))?;

    // Close the race with a daemon that started after the first check.
    if let Some(reason) = daemon_appears_running(data_dir)? {
        bail!(
            "refusing to replace {} because a daemon appears to be running: {reason}",
            data_dir.display()
        );
    }

    let replaced_existing = existing;
    let mut previous = None;
    if existing {
        let aside = unique_sibling(&parent, "restore-old");
        fs::rename(data_dir, &aside).with_context(|| {
            format!(
                "moving existing target {} aside to {}",
                data_dir.display(),
                aside.display()
            )
        })?;
        previous = Some(aside);
    }
    if let Err(error) = fs::rename(&scratch.path, data_dir) {
        if let Some(previous) = &previous
            && let Err(restore_error) = fs::rename(previous, data_dir)
        {
            return Err(error).with_context(|| {
                format!(
                    "moving restored data to {}, and the previous target could not be \
                     restored from {}: {restore_error}",
                    data_dir.display(),
                    previous.display()
                )
            });
        }
        return Err(error)
            .with_context(|| format!("moving restored data into {}", data_dir.display()));
    }
    scratch.disarm();
    if let Some(previous) = previous
        && let Err(error) = fs::remove_dir_all(&previous)
    {
        tracing::warn!(
            %error,
            path = %previous.display(),
            "restored data but could not remove the previous target"
        );
    }

    Ok(RestoreReport {
        output: data_dir.to_string_lossy().to_string(),
        data_dir: data_dir.to_string_lossy().to_string(),
        manifest: data_dir
            .join(MANIFEST_FILENAME)
            .to_string_lossy()
            .to_string(),
        format_version: plan.manifest.format_version,
        created_at: plan.manifest.created_at,
        includes_artifacts: plan.manifest.includes_artifacts,
        entries: plan.manifest.entries.len(),
        sessions: plan.session_entries,
        artifacts: plan.artifact_entries,
        bytes: plan.manifest.entries.iter().map(|entry| entry.size).sum(),
        replaced_existing,
    })
}

struct RestorePlan {
    manifest: BackupManifest,
    manifest_bytes: Vec<u8>,
    index_by_name: BTreeMap<String, usize>,
    session_entries: usize,
    artifact_entries: usize,
}

fn read_restore_plan(archive: &mut ZipArchive<File>) -> Result<RestorePlan> {
    if archive.is_empty() {
        bail!("backup archive is empty");
    }
    let mut index_by_name = BTreeMap::new();
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .with_context(|| format!("reading backup entry #{index}"))?;
        let name = file.name().to_string();
        if file.is_dir() || name.ends_with('/') {
            bail!("backup contains directory entry {name:?}; only files are allowed");
        }
        if is_symlink_mode(file.unix_mode()) {
            bail!("backup contains symlink entry {name:?}; symlinks are not allowed");
        }
        classify_archive_path(&name)?;
        if index_by_name.insert(name.clone(), index).is_some() {
            bail!("backup contains duplicate entry {name:?}");
        }
    }

    let manifest_index = *index_by_name
        .get(MANIFEST_FILENAME)
        .ok_or_else(|| anyhow!("backup is missing {MANIFEST_FILENAME}"))?;
    let manifest_bytes = {
        let file = archive.by_index(manifest_index)?;
        if file.size() > MAX_MANIFEST_BYTES {
            bail!(
                "{MANIFEST_FILENAME} is larger than the {} byte limit",
                MAX_MANIFEST_BYTES
            );
        }
        let mut bytes = Vec::new();
        file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            bail!(
                "{MANIFEST_FILENAME} is larger than the {} byte limit",
                MAX_MANIFEST_BYTES
            );
        }
        bytes
    };
    let manifest: BackupManifest =
        serde_json::from_slice(&manifest_bytes).context("parsing backup manifest")?;
    if manifest.format_version != BACKUP_FORMAT_VERSION {
        bail!(
            "unsupported backup format version {}; this build supports {}",
            manifest.format_version,
            BACKUP_FORMAT_VERSION
        );
    }
    if manifest.sqlite_filename != DATABASE_FILENAME {
        bail!(
            "backup manifest names SQLite file {:?}; expected {DATABASE_FILENAME}",
            manifest.sqlite_filename
        );
    }

    let mut listed = BTreeSet::new();
    let mut database_entries = 0usize;
    let mut session_entries = 0usize;
    let mut artifact_entries = 0usize;
    for entry in &manifest.entries {
        let kind = classify_archive_path(&entry.path)?;
        if kind == ArchiveKind::Manifest {
            bail!("backup manifest must not list {MANIFEST_FILENAME} as a data entry");
        }
        if !listed.insert(entry.path.clone()) {
            bail!("backup manifest contains duplicate entry {:?}", entry.path);
        }
        if !is_sha256_hex(&entry.sha256) {
            bail!(
                "backup manifest has an invalid SHA-256 for {:?}",
                entry.path
            );
        }
        if !index_by_name.contains_key(&entry.path) {
            bail!(
                "backup is missing entry {:?} listed in the manifest",
                entry.path
            );
        }
        match kind {
            ArchiveKind::Database => database_entries += 1,
            ArchiveKind::Session => session_entries += 1,
            ArchiveKind::Artifact if !manifest.includes_artifacts => {
                bail!(
                    "backup contains artifact {:?} but the manifest says artifacts are excluded",
                    entry.path
                );
            }
            ArchiveKind::Artifact => artifact_entries += 1,
            ArchiveKind::Manifest | ArchiveKind::Projects => {}
        }
    }
    if database_entries != 1 {
        bail!(
            "backup manifest must list exactly one {DATABASE_FILENAME}; found {database_entries}"
        );
    }
    for name in index_by_name.keys() {
        if name == MANIFEST_FILENAME {
            continue;
        }
        if !listed.contains(name) {
            bail!("backup contains entry {name:?} that is not listed in the manifest");
        }
    }

    Ok(RestorePlan {
        manifest,
        manifest_bytes,
        index_by_name,
        session_entries,
        artifact_entries,
    })
}

fn verify_checksums(archive: &mut ZipArchive<File>, plan: &RestorePlan) -> Result<()> {
    for entry in &plan.manifest.entries {
        let index = *plan
            .index_by_name
            .get(&entry.path)
            .ok_or_else(|| anyhow!("backup is missing entry {:?}", entry.path))?;
        let mut file = archive.by_index(index)?;
        let (size, sha256) = hash_reader(&mut file)?;
        if size != entry.size {
            bail!(
                "size mismatch for {:?}: manifest says {} bytes, archive has {}",
                entry.path,
                entry.size,
                size
            );
        }
        if sha256 != entry.sha256 {
            bail!(
                "checksum mismatch for {:?}: manifest says {}, archive has {}",
                entry.path,
                entry.sha256,
                sha256
            );
        }
    }
    Ok(())
}

fn extract_entry(
    archive: &mut ZipArchive<File>,
    index_by_name: &BTreeMap<String, usize>,
    entry: &BackupEntry,
    root: &Path,
) -> Result<()> {
    let index = *index_by_name
        .get(&entry.path)
        .ok_or_else(|| anyhow!("backup is missing entry {:?}", entry.path))?;
    let mut source = archive.by_index(index)?;
    if source.is_dir() || is_symlink_mode(source.unix_mode()) {
        bail!("backup entry {:?} is not a regular file", entry.path);
    }
    let relative = Path::new(&entry.path);
    let destination = root.join(relative);
    if let Some(parent) = destination.parent() {
        vp_store::paths::ensure_private_dir(parent)?;
    }
    let mut target = create_private_new(&destination)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    let mut written = 0u64;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        target.write_all(&buffer[..read])?;
        written += read as u64;
    }
    target.flush()?;
    target.sync_all()?;
    if written != entry.size {
        bail!(
            "short read extracting {:?}: expected {} bytes, wrote {}",
            entry.path,
            entry.size,
            written
        );
    }
    let actual = hex_lower(&hasher.finalize());
    if actual != entry.sha256 {
        bail!(
            "checksum mismatch while extracting {:?}: expected {}, wrote {}",
            entry.path,
            entry.sha256,
            actual
        );
    }
    Ok(())
}

fn verify_sqlite_header(path: &Path) -> Result<()> {
    let mut file = File::open(path)
        .with_context(|| format!("opening restored database {}", path.display()))?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header)
        .with_context(|| format!("reading restored database {}", path.display()))?;
    if &header != b"SQLite format 3\0" {
        bail!(
            "restored database {} does not have a SQLite header",
            path.display()
        );
    }
    Ok(())
}

fn collect_backup_sources(data_dir: &Path, include_artifacts: bool) -> Result<Vec<String>> {
    let mut sources = BTreeSet::new();

    let projects = data_dir.join(PROJECTS_FILENAME);
    match fs::symlink_metadata(&projects) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("refusing to archive symlinked {}", projects.display());
        }
        Ok(meta) if meta.is_file() => {
            sources.insert(PROJECTS_FILENAME.to_string());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", projects.display()));
        }
    }

    let sessions = data_dir.join(SESSIONS_DIR);
    match fs::symlink_metadata(&sessions) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("refusing to archive symlinked {}", sessions.display());
        }
        Ok(meta) if meta.is_dir() => {
            let mut ids = Vec::new();
            for entry in fs::read_dir(&sessions)
                .with_context(|| format!("reading {}", sessions.display()))?
            {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_symlink() || !file_type.is_dir() {
                    continue;
                }
                let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if !safe_component(&id) {
                    continue;
                }
                ids.push(id);
            }
            ids.sort();
            for id in ids {
                let events = sessions.join(&id).join("events.jsonl");
                match fs::symlink_metadata(&events) {
                    Ok(meta) if meta.file_type().is_symlink() => continue,
                    Ok(meta) if meta.is_file() => {
                        sources.insert(format!("{SESSIONS_DIR}/{id}/events.jsonl"));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| format!("reading {}", events.display()));
                    }
                }
            }
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", sessions.display()));
        }
    }

    if include_artifacts {
        let blobs = data_dir.join(ARTIFACTS_DIR).join(ARTIFACT_BLOBS_DIR);
        match fs::symlink_metadata(&blobs) {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("refusing to archive symlinked {}", blobs.display());
            }
            Ok(meta) if meta.is_dir() => {
                let mut names = Vec::new();
                for entry in
                    fs::read_dir(&blobs).with_context(|| format!("reading {}", blobs.display()))?
                {
                    let entry = entry?;
                    let file_type = entry.file_type()?;
                    if file_type.is_symlink() || !file_type.is_file() {
                        continue;
                    }
                    let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                        continue;
                    };
                    if !safe_component(&name) {
                        continue;
                    }
                    names.push(name);
                }
                names.sort();
                for name in names {
                    sources.insert(format!("{ARTIFACTS_DIR}/{ARTIFACT_BLOBS_DIR}/{name}"));
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", blobs.display()));
            }
        }
    }

    Ok(sources.into_iter().collect())
}

fn write_archive(
    staging: &Path,
    manifest_bytes: &[u8],
    entries: &[BackupEntry],
    output: &Path,
) -> Result<String> {
    let output_parent = non_empty_parent(output);
    let mut scratch = ScratchPath::file(output_parent, "archive")?;
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&scratch.path)
            .with_context(|| format!("creating {}", scratch.path.display()))?;
        vp_store::paths::ensure_private_file(&scratch.path)?;
        let mut writer = ZipWriter::new(file);
        writer.start_file(MANIFEST_FILENAME, options)?;
        writer.write_all(manifest_bytes)?;
        for entry in entries {
            writer.start_file(entry.path.clone(), options)?;
            let source = staging.join(&entry.path);
            let mut input = File::open(&source)
                .with_context(|| format!("opening staged file {}", source.display()))?;
            std::io::copy(&mut input, &mut writer)
                .with_context(|| format!("archiving {}", entry.path))?;
        }
        let file = writer.finish()?;
        file.sync_all()?;
    }
    let archive_sha256 = hash_file(&scratch.path)?.1;
    fs::hard_link(&scratch.path, output).with_context(|| {
        format!(
            "publishing backup {} from {}",
            output.display(),
            scratch.path.display()
        )
    })?;
    let _ = fs::remove_file(&scratch.path);
    scratch.disarm();
    Ok(archive_sha256)
}

fn copy_private(source: &Path, destination: &Path) -> Result<()> {
    let mut input = File::open(source).with_context(|| format!("opening {}", source.display()))?;
    if let Some(parent) = destination.parent() {
        vp_store::paths::ensure_private_dir(parent)?;
    }
    let mut output = create_private_new(destination)?;
    std::io::copy(&mut input, &mut output)
        .with_context(|| format!("copying {} to {}", source.display(), destination.display()))?;
    output.flush()?;
    output.sync_all()?;
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        vp_store::paths::ensure_private_dir(parent)?;
    }
    let mut file = create_private_new(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .with_context(|| format!("creating {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path).with_context(|| format!("creating {}", path.display()))?;
    }
    vp_store::paths::ensure_private_dir(path)?;
    Ok(())
}

fn create_private_new(path: &Path) -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        vp_store::paths::ensure_private_file(path)?;
        Ok(file)
    }
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    hash_reader(&mut file).with_context(|| format!("hashing {}", path.display()))
}

fn hash_reader(reader: &mut impl Read) -> Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((total, hex_lower(&hasher.finalize())))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_symlink_mode(mode: Option<u32>) -> bool {
    mode.map(|mode| mode & 0o170000 == 0o120000)
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    Manifest,
    Database,
    Projects,
    Session,
    Artifact,
}

fn classify_archive_path(path: &str) -> Result<ArchiveKind> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains("//")
    {
        bail!("unsafe archive path {path:?}");
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        bail!("unsafe archive path {path:?}");
    }
    match parts.as_slice() {
        [MANIFEST_FILENAME] => Ok(ArchiveKind::Manifest),
        [DATABASE_FILENAME] => Ok(ArchiveKind::Database),
        [PROJECTS_FILENAME] => Ok(ArchiveKind::Projects),
        [SESSIONS_DIR, id, "events.jsonl"] if safe_component(id) => Ok(ArchiveKind::Session),
        [ARTIFACTS_DIR, ARTIFACT_BLOBS_DIR, name] if safe_component(name) => {
            Ok(ArchiveKind::Artifact)
        }
        _ => bail!("archive path {path:?} is outside the backup allowlist"),
    }
}

fn safe_component(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 255
        && part != "."
        && part != ".."
        && !part.contains('/')
        && !part.contains('\\')
        && !part.contains('\0')
}

fn daemon_appears_running(data_dir: &Path) -> Result<Option<String>> {
    let discovery = data_dir.join("run").join("daemon.json");
    if discovery.exists() {
        return Ok(Some(format!(
            "discovery file exists at {}",
            discovery.display()
        )));
    }
    let lock = data_dir.join("run").join("daemon.lock");
    if !lock.exists() {
        return Ok(None);
    }
    let file = match OpenOptions::new().read(true).write(true).open(&lock) {
        Ok(file) => file,
        Err(error) => {
            return Ok(Some(format!(
                "instance lock {} cannot be opened for inspection: {error}",
                lock.display()
            )));
        }
    };
    if instance_lock_held(&file) {
        return Ok(Some(format!("instance lock is held at {}", lock.display())));
    }
    Ok(None)
}

#[cfg(unix)]
fn instance_lock_held(file: &File) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: the descriptor belongs to `file` and remains valid for the call.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        // SAFETY: same descriptor; releasing our probe lock does not affect a
        // daemon that holds a separate descriptor.
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        false
    } else {
        true
    }
}

#[cfg(not(unix))]
fn instance_lock_held(_file: &File) -> bool {
    // Discovery is the portable signal; non-Unix builds cannot probe flock.
    false
}

fn non_empty_parent(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn unique_sibling(parent: &Path, label: &str) -> PathBuf {
    parent.join(format!(".verlinpark-{label}-{}.tmp", Uuid::new_v4()))
}

struct ScratchPath {
    path: PathBuf,
    directory: bool,
    armed: bool,
}

impl ScratchPath {
    fn directory(parent: &Path, label: &str) -> Result<Self> {
        let path = unique_sibling(parent, label);
        // `create_dir` (not `create_dir_all`) fails rather than reusing a
        // colliding path that this guard would later delete.
        create_private_dir(&path)?;
        Ok(Self {
            path,
            directory: true,
            armed: true,
        })
    }

    fn file(parent: &Path, label: &str) -> Result<Self> {
        Ok(Self {
            path: unique_sibling(parent, label),
            directory: false,
            armed: true,
        })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ScratchPath {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let result = if self.directory {
            fs::remove_dir_all(&self.path)
        } else {
            fs::remove_file(&self.path)
        };
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                %error,
                path = %self.path.display(),
                "failed to remove backup scratch path"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;
    use vp_protocol::types::{Project, Session};
    use vp_store::Store;

    fn seed_data_dir(dir: &Path) -> (Store, String) {
        let store = Store::open(dir).unwrap();
        let project = Project {
            id: "project-1".to_string(),
            name: "Backup test".to_string(),
            root: "/tmp/backup-test-repo".to_string(),
            default_branch: "main".to_string(),
            created_at: now_rfc3339(),
        };
        store.insert_project_snapshot(&project).unwrap();
        let session = Session {
            id: "session-1".to_string(),
            project_id: project.id,
            goal: "verify backup round trip".to_string(),
            status: "idle".to_string(),
            priority: 0,
            worktree: None,
            branch: Some("session/backup".to_string()),
            token_budget: Some(1000),
            cost_budget_usd: None,
            time_budget_secs: None,
            unlimited: false,
            cost_spent_usd: 0.0,
            base_commit: None,
            tokens_in: 0,
            tokens_out: 0,
            created_at: now_rfc3339(),
        };
        store.insert_session_snapshot(&session).unwrap();
        store
            .append_event(
                &session.id,
                "session.created",
                json!({"goal": session.goal, "project_id": session.project_id}),
            )
            .unwrap();
        store
            .append_event(
                &session.id,
                "message.user",
                json!({"text": "back up this session"}),
            )
            .unwrap();
        (store, session.id)
    }

    fn assert_no_scratch(path: &Path) {
        let leftovers = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".verlinpark-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "scratch leftovers: {leftovers:?}");
    }

    fn zip_names(path: &Path) -> Vec<String> {
        let file = File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect()
    }

    fn read_manifest(path: &Path) -> BackupManifest {
        let file = File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut manifest = archive.by_name(MANIFEST_FILENAME).unwrap();
        let mut bytes = Vec::new();
        manifest.read_to_end(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn write_test_zip(path: &Path, entries: &[(&str, &[u8], u32)]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        for (name, bytes, mode) in entries {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Deflated)
                        .unix_permissions(*mode),
                )
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    fn rewrite_zip_entry(
        input: &Path,
        output: &Path,
        target: &str,
        mut transform: impl FnMut(Vec<u8>) -> Vec<u8>,
    ) {
        let file = File::open(input).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let out = File::create(output).unwrap();
        let mut writer = ZipWriter::new(out);
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_string();
            let mode = entry.unix_mode().unwrap_or(0o600);
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if name == target {
                bytes = transform(bytes);
            }
            writer
                .start_file(
                    name,
                    SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Deflated)
                        .unix_permissions(mode),
                )
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn round_trip_restores_database_and_event_mirror() {
        let data = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        let archive = output.path().join("backup.zip");
        let (store, session_id) = seed_data_dir(data.path());
        let report = create_backup(data.path(), &archive, false).unwrap();
        drop(store);
        assert_no_scratch(output.path());

        assert_eq!(report.entries, 2);
        assert_eq!(report.sessions, 1);
        assert_eq!(report.artifacts, 0);
        assert!(!report.includes_artifacts);
        assert!(report.bytes > 0);
        assert_eq!(report.archive_sha256.len(), 64);

        let names = zip_names(&archive);
        assert!(names.contains(&MANIFEST_FILENAME.to_string()));
        assert!(names.contains(&DATABASE_FILENAME.to_string()));
        assert!(names.contains(&format!("sessions/{session_id}/events.jsonl")));
        assert_eq!(names.len(), 3);

        let manifest = read_manifest(&archive);
        assert_eq!(manifest.format_version, BACKUP_FORMAT_VERSION);
        assert_eq!(manifest.sqlite_filename, DATABASE_FILENAME);
        assert!(!manifest.includes_artifacts);
        assert_eq!(manifest.entries.len(), 2);
        assert!(
            manifest
                .entries
                .iter()
                .all(|entry| entry.sha256.len() == 64)
        );

        let target = output.path().join("restored");
        let restore = restore_backup(&archive, &target, false).unwrap();
        assert_no_scratch(output.path());
        assert_eq!(restore.entries, 2);
        assert_eq!(restore.sessions, 1);
        assert_eq!(restore.artifacts, 0);
        assert!(!restore.includes_artifacts);
        assert_eq!(restore.output, target.to_string_lossy().to_string());
        assert!(target.join(MANIFEST_FILENAME).is_file());

        let store = Store::open(&target).unwrap();
        let session = store.get_session(&session_id).unwrap().unwrap();
        assert_eq!(session.goal, "verify backup round trip");
        let events = store.list_all_events(&session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "session.created");
        assert_eq!(events[1].kind, "message.user");
        let mirror = store.read_event_mirror(&session_id).unwrap().unwrap();
        assert_eq!(mirror.len(), 2);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(target.join(DATABASE_FILENAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn checksum_mismatch_is_rejected_before_the_target_is_touched() {
        let data = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        let archive = output.path().join("backup.zip");
        let (store, _) = seed_data_dir(data.path());
        create_backup(data.path(), &archive, false).unwrap();
        drop(store);

        let tampered = output.path().join("tampered.zip");
        rewrite_zip_entry(&archive, &tampered, DATABASE_FILENAME, |mut bytes| {
            let last = bytes.len() - 1;
            bytes[last] ^= 0xff;
            bytes
        });

        let target = output.path().join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep.txt"), b"do not lose me").unwrap();
        let error = restore_backup(&tampered, &target, true).unwrap_err();
        assert!(
            format!("{error:#}").contains("checksum mismatch"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            fs::read(target.join("keep.txt")).unwrap(),
            b"do not lose me"
        );
        assert!(!target.join(DATABASE_FILENAME).exists());
    }

    #[test]
    fn path_traversal_absolute_and_symlink_entries_are_rejected() {
        let output = TempDir::new().unwrap();
        let target = output.path().join("target");

        let traversing = output.path().join("traversal.zip");
        write_test_zip(&traversing, &[("../escape.json", b"{}", 0o600)]);
        let error = restore_backup(&traversing, &target, false).unwrap_err();
        assert!(
            format!("{error:#}").contains("unsafe archive path"),
            "unexpected error: {error:#}"
        );

        let absolute = output.path().join("absolute.zip");
        write_test_zip(&absolute, &[("/tmp/escape.json", b"{}", 0o600)]);
        let error = restore_backup(&absolute, &target, false).unwrap_err();
        assert!(
            format!("{error:#}").contains("unsafe archive path"),
            "unexpected error: {error:#}"
        );

        let symlink = output.path().join("symlink.zip");
        {
            let file = File::create(&symlink).unwrap();
            let mut writer = ZipWriter::new(file);
            writer
                .add_symlink(
                    "sessions/session-1/events.jsonl",
                    "../outside-secret",
                    SimpleFileOptions::default().unix_permissions(0o777),
                )
                .unwrap();
            writer.finish().unwrap();
        }
        let error = restore_backup(&symlink, &target, false).unwrap_err();
        assert!(
            format!("{error:#}").contains("symlink"),
            "unexpected error: {error:#}"
        );
        assert!(!target.exists());
    }

    #[test]
    fn non_empty_target_requires_force_and_failure_leaves_it_untouched() {
        let data = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        let archive = output.path().join("backup.zip");
        let (store, _) = seed_data_dir(data.path());
        create_backup(data.path(), &archive, false).unwrap();
        drop(store);

        let target = output.path().join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();
        let error = restore_backup(&archive, &target, false).unwrap_err();
        assert!(
            format!("{error:#}").contains("not empty"),
            "unexpected error: {error:#}"
        );
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");

        // Existing database and instance lock are called out explicitly.
        let database_target = output.path().join("database-target");
        fs::create_dir_all(&database_target).unwrap();
        fs::write(database_target.join(DATABASE_FILENAME), b"").unwrap();
        let error = restore_backup(&archive, &database_target, false).unwrap_err();
        assert!(
            format!("{error:#}").contains("existing database"),
            "unexpected error: {error:#}"
        );

        let lock_target = output.path().join("lock-target");
        fs::create_dir_all(lock_target.join("run")).unwrap();
        fs::write(lock_target.join("run/daemon.lock"), b"").unwrap();
        let error = restore_backup(&archive, &lock_target, false).unwrap_err();
        assert!(
            format!("{error:#}").contains("instance lock"),
            "unexpected error: {error:#}"
        );

        // Force replaces the existing contents only after full validation.
        let replaced = restore_backup(&archive, &target, true).unwrap();
        assert!(replaced.replaced_existing);
        assert!(!target.join("keep.txt").exists());
        assert!(target.join(DATABASE_FILENAME).is_file());
    }

    #[test]
    fn discovery_lock_and_secret_files_are_never_archived() {
        let data = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        let archive = output.path().join("backup.zip");
        let (store, session_id) = seed_data_dir(data.path());
        drop(store);

        fs::create_dir_all(data.path().join("run")).unwrap();
        fs::create_dir_all(data.path().join("logs")).unwrap();
        fs::write(
            data.path().join("run/daemon.json"),
            br#"{"rpc_url":"http://127.0.0.1:1","token":"secret"}"#,
        )
        .unwrap();
        fs::write(data.path().join("run/daemon.lock"), b"").unwrap();
        fs::write(data.path().join("logs/daemon.log"), b"log").unwrap();
        fs::write(data.path().join(".env"), b"OPENAI_API_KEY=secret").unwrap();
        fs::write(data.path().join("provider-keys.json"), b"secret").unwrap();
        fs::create_dir_all(data.path().join("sessions").join(&session_id)).unwrap();
        fs::write(
            data.path()
                .join("sessions")
                .join(&session_id)
                .join("scratch.txt"),
            b"scratch",
        )
        .unwrap();
        fs::write(data.path().join("projects.jsonl"), b"{}\n").unwrap();
        fs::create_dir_all(data.path().join("artifacts/blobs")).unwrap();
        fs::write(
            data.path()
                .join("artifacts/blobs")
                .join("a".repeat(64) + ".zst"),
            b"blob",
        )
        .unwrap();

        let report = create_backup(data.path(), &archive, false).unwrap();
        assert_eq!(report.artifacts, 0);
        let names = zip_names(&archive);
        let mut sorted = names.clone();
        sorted.sort();
        let mut expected = vec![
            MANIFEST_FILENAME.to_string(),
            PROJECTS_FILENAME.to_string(),
            DATABASE_FILENAME.to_string(),
            format!("sessions/{session_id}/events.jsonl"),
        ];
        expected.sort();
        assert_eq!(sorted, expected);
        for forbidden in [
            "run/daemon.json",
            "run/daemon.lock",
            "logs/daemon.log",
            ".env",
            "provider-keys.json",
        ] {
            assert!(!names.iter().any(|name| name == forbidden), "{forbidden}");
        }
        assert!(!names.iter().any(|name| name.starts_with("artifacts/")));
        assert!(
            !names
                .iter()
                .any(|name| name == &format!("sessions/{session_id}/scratch.txt"))
        );
    }

    #[test]
    fn artifact_blobs_are_optional_and_restored_when_included() {
        let data = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        let (store, session_id) = seed_data_dir(data.path());
        drop(store);
        let blob_name = format!("{}.zst", "b".repeat(64));
        fs::create_dir_all(data.path().join("artifacts/blobs")).unwrap();
        fs::write(
            data.path().join("artifacts/blobs").join(&blob_name),
            b"artifact bytes",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::write(output.path().join("outside-secret"), b"secret").unwrap();
            let _ = symlink(
                output.path().join("outside-secret"),
                data.path().join("artifacts/blobs").join("linked.zst"),
            );
        }

        let without = output.path().join("without.zip");
        let report = create_backup(data.path(), &without, false).unwrap();
        assert_eq!(report.artifacts, 0);
        assert!(!read_manifest(&without).includes_artifacts);
        assert!(
            !zip_names(&without)
                .iter()
                .any(|name| name.starts_with("artifacts/"))
        );
        let default_target = output.path().join("default-restored");
        let default_restore = restore_backup(&without, &default_target, false).unwrap();
        assert_eq!(default_restore.artifacts, 0);
        assert!(!default_target.join("artifacts").exists());

        let with = output.path().join("with.zip");
        let report = create_backup(data.path(), &with, true).unwrap();
        assert_eq!(report.artifacts, 1);
        assert!(report.includes_artifacts);
        let manifest = read_manifest(&with);
        assert!(manifest.includes_artifacts);
        let names = zip_names(&with);
        assert!(names.contains(&format!("artifacts/blobs/{blob_name}")));
        assert!(!names.contains(&"artifacts/blobs/linked.zst".to_string()));

        let target = output.path().join("restored");
        let restore = restore_backup(&with, &target, false).unwrap();
        assert_eq!(restore.artifacts, 1);
        assert!(restore.includes_artifacts);
        assert_eq!(
            fs::read(target.join("artifacts/blobs").join(&blob_name)).unwrap(),
            b"artifact bytes"
        );
        assert!(!target.join("artifacts/blobs/linked.zst").exists());
        // The restored mirror is part of the archive even with artifacts on.
        assert!(target.join("sessions").join(session_id).exists());
    }

    #[test]
    fn daemon_discovery_refuses_restore_even_with_force() {
        let data = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        let archive = output.path().join("backup.zip");
        let (store, _) = seed_data_dir(data.path());
        create_backup(data.path(), &archive, false).unwrap();
        drop(store);

        let target = output.path().join("target");
        fs::create_dir_all(target.join("run")).unwrap();
        fs::write(target.join("run/daemon.json"), b"{}").unwrap();
        let error = restore_backup(&archive, &target, true).unwrap_err();
        assert!(
            format!("{error:#}").contains("daemon"),
            "unexpected error: {error:#}"
        );
        assert!(target.join("run/daemon.json").exists());
    }
}
