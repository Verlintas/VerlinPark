use super::support::{
    build_source_chunks, normalize_source_collection, validate_source_labels, validate_source_title,
};
use super::*;

// ---- Project knowledge sources -------------------------------------

impl Engine {
    pub async fn add_source_note(&self, params: SourceCreateNoteParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        validate_source_title(&params.title)?;
        let labels = validate_source_labels(params.labels)?;
        self.ingest_source(
            &project,
            "note",
            normalize_source_collection(params.collection.as_deref())?,
            params.title,
            None,
            Some("text/markdown".to_string()),
            params.content,
            labels,
        )
        .await
    }

    pub async fn add_source_file(&self, params: SourceCreateFileParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        validate_source_title(params.title.as_deref().unwrap_or("source"))?;
        let labels = validate_source_labels(params.labels)?;
        let root = PathBuf::from(&project.root);
        let relative = params.path.clone();
        let file =
            tokio::task::spawn_blocking(move || read_source_file(&root, &relative)).await??;
        let title = params.title.unwrap_or_else(|| file.relative.clone());
        validate_source_title(&title)?;
        self.ingest_source(
            &project,
            "file",
            normalize_source_collection(params.collection.as_deref())?,
            title,
            Some(file.relative),
            Some(file.mime_type),
            file.content,
            labels,
        )
        .await
    }

    pub async fn add_source_url(&self, params: SourceCreateUrlParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        let url = params.url.trim();
        if url.is_empty() || url.len() > 2_000 {
            anyhow::bail!("source URL must be 1-2000 characters");
        }
        let fetched = crate::tools::fetch_public_url(
            url,
            1_000_000,
            crate::sources::MAX_SOURCE_BYTES as usize,
        )
        .await?;
        if fetched.truncated {
            tracing::warn!(url, "source URL response was truncated by the fetch limit");
        }
        let is_html = fetched.content_type.to_ascii_lowercase().contains("html")
            || fetched.text.trim_start().starts_with("<!doctype")
            || fetched.text.trim_start().starts_with("<html");
        let content = if is_html {
            crate::sources::html_to_text(&fetched.text)
        } else {
            fetched.text
        };
        if content.trim().is_empty() {
            anyhow::bail!("the URL returned no readable text");
        }
        let title = params
            .title
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| source_title_from_url(url));
        validate_source_title(&title)?;
        let labels = validate_source_labels(params.labels)?;
        self.ingest_source(
            &project,
            "url",
            normalize_source_collection(params.collection.as_deref())?,
            title,
            Some(url.to_string()),
            Some(fetched.content_type),
            content,
            labels,
        )
        .await
    }

    /// Index text-like files under the project root. Existing source URIs are
    /// skipped; binary, oversized, malformed, and ignored files are counted as
    /// skipped rather than silently becoming partial sources.
    pub async fn import_sources(&self, params: SourceImportParams) -> Result<SourceImportResult> {
        let project = self.project_for_sources(&params.project_id).await?;
        let labels = validate_source_labels(params.labels)?;
        let root = PathBuf::from(&project.root);
        let relative = params.path.clone();
        let max_files = params.max_files.unwrap_or(100).clamp(1, 500) as usize;
        let scan = tokio::task::spawn_blocking(move || {
            collect_import_files(&root, relative.as_deref(), max_files)
        })
        .await??;
        // The candidate file list is bounded, so check exactly those URIs.
        // `list_sources` returns only the newest 1,000 rows and cannot prove
        // that an older source with the same path does not already exist.
        let candidate_uris = scan
            .files
            .iter()
            .map(|file| file.relative.clone())
            .collect::<Vec<_>>();
        let project_id = project.id.clone();
        let existing_uris = {
            let store = self.store.clone();
            tokio::task::spawn_blocking(move || {
                store.existing_source_uris(&project_id, &candidate_uris)
            })
            .await??
        };
        let mut imported = Vec::new();
        let mut skipped = scan.skipped;
        let mut errors = Vec::new();
        for file in scan.files {
            if existing_uris.contains(&file.relative) {
                skipped += 1;
                continue;
            }
            match self
                .ingest_source(
                    &project,
                    "file",
                    normalize_source_collection(params.collection.as_deref())?,
                    file.title.clone(),
                    Some(file.relative.clone()),
                    Some(file.mime_type.clone()),
                    file.content,
                    labels.clone(),
                )
                .await
            {
                Ok(source) => imported.push(source),
                Err(error) => errors.push(SourceImportError {
                    path: file.relative,
                    error: format!("{error:#}"),
                }),
            }
        }
        Ok(SourceImportResult {
            imported,
            skipped,
            errors,
        })
    }

    pub async fn delete_source(&self, source_id: String) -> Result<SourceDeleteResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id)).await??;
        let Some(source) = source else {
            return Ok(SourceDeleteResult { deleted: false });
        };
        // Remove the durable snapshot first. If the row delete then fails the
        // source remains visible but cannot be rebuilt from a stale snapshot;
        // the reverse order could resurrect a deleted source in `db-rebuild`.
        let store = self.store.clone();
        let project_id = source.project_id.clone();
        let id = source_id.clone();
        let deleted = tokio::task::spawn_blocking(move || {
            store.delete_source_snapshot(&project_id, &id)?;
            store.delete_source(&id)
        })
        .await??;
        Ok(SourceDeleteResult { deleted })
    }

    pub async fn reindex_source(&self, source_id: String) -> Result<SourceReindexResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let store = self.store.clone();
        let project_id = source.project_id.clone();
        let id = source_id.clone();
        let snapshot =
            tokio::task::spawn_blocking(move || store.read_source_snapshot(&project_id, &id))
                .await??
                .ok_or_else(|| {
                    anyhow!("source snapshot is missing; delete and re-add the source")
                })?;
        self.replace_source_content(source, snapshot.content, None)
            .await
    }

    /// Re-read a file or public URL and replace the indexed content. Notes are
    /// reindexed from their durable snapshot.
    pub async fn refresh_source(&self, source_id: String) -> Result<SourceReindexResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let project = self.project_for_sources(&source.project_id).await?;
        let (content, mime_type) = match source.kind.as_str() {
            "file" => {
                let relative = source
                    .uri
                    .clone()
                    .ok_or_else(|| anyhow!("file source has no path"))?;
                let root = PathBuf::from(&project.root);
                let file = tokio::task::spawn_blocking(move || read_source_file(&root, &relative))
                    .await??;
                (file.content, Some(file.mime_type))
            }
            "url" => {
                let url = source
                    .uri
                    .clone()
                    .ok_or_else(|| anyhow!("URL source has no URL"))?;
                let fetched = crate::tools::fetch_public_url(
                    &url,
                    1_000_000,
                    crate::sources::MAX_SOURCE_BYTES as usize,
                )
                .await?;
                let is_html = fetched.content_type.to_ascii_lowercase().contains("html")
                    || fetched.text.trim_start().starts_with("<!doctype")
                    || fetched.text.trim_start().starts_with("<html");
                let content = if is_html {
                    crate::sources::html_to_text(&fetched.text)
                } else {
                    fetched.text
                };
                (content, Some(fetched.content_type))
            }
            _ => {
                let store = self.store.clone();
                let project_id = source.project_id.clone();
                let id = source.id.clone();
                let snapshot = tokio::task::spawn_blocking(move || {
                    store.read_source_snapshot(&project_id, &id)
                })
                .await??
                .ok_or_else(|| anyhow!("source snapshot is missing"))?;
                (snapshot.content, None)
            }
        };
        if content.trim().is_empty() {
            anyhow::bail!("the refreshed source returned no readable text");
        }
        self.replace_source_content(source, content, mime_type)
            .await
    }

    async fn replace_source_content(
        &self,
        source: Source,
        content: String,
        mime_type: Option<String>,
    ) -> Result<SourceReindexResult> {
        let content = crate::sources::normalize_source_text(&content);
        if content.chars().count() > crate::sources::MAX_SOURCE_CHARS
            || content.len() > crate::sources::MAX_SOURCE_BYTES as usize
        {
            anyhow::bail!(
                "source is too large (limit {} bytes)",
                crate::sources::MAX_SOURCE_BYTES
            );
        }
        let chunks = build_source_chunks(&source.id, &content);
        if chunks.is_empty() {
            anyhow::bail!("source produced no readable chunks");
        }
        let now = now_rfc3339();
        let mut updated = source;
        updated.revision = updated.revision.saturating_add(1);
        updated.chunk_count = chunks.len();
        updated.char_count = content.chars().count();
        updated.token_count = chunks
            .iter()
            .map(|chunk| u64::from(chunk.token_count))
            .sum();
        updated.status = "indexed".to_string();
        updated.error = None;
        updated.size_bytes = content.len() as u64;
        updated.sha256 = sha256_hex(content.as_bytes());
        updated.updated_at = now.clone();
        updated.indexed_at = Some(now);
        if let Some(mime_type) = mime_type {
            updated.mime_type = Some(mime_type);
        }
        let snapshot = SourceSnapshot {
            version: 1,
            source: updated.clone(),
            content,
        };
        let store = self.store.clone();
        let task = updated.clone();
        let chunks_for_db = chunks.clone();
        tokio::task::spawn_blocking(move || {
            store.write_source_snapshot(&snapshot)?;
            store.insert_source(&task, &chunks_for_db)
        })
        .await??;
        Ok(SourceReindexResult {
            source: updated,
            chunks: chunks.len(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_source(
        &self,
        project: &Project,
        kind: &str,
        collection: Option<String>,
        title: String,
        uri: Option<String>,
        mime_type: Option<String>,
        content: String,
        labels: Vec<String>,
    ) -> Result<Source> {
        validate_source_title(&title)?;
        let content = crate::sources::normalize_source_text(&content);
        if content.trim().is_empty() {
            anyhow::bail!("source content must not be empty");
        }
        if content.chars().count() > crate::sources::MAX_SOURCE_CHARS
            || content.len() > crate::sources::MAX_SOURCE_BYTES as usize
        {
            anyhow::bail!(
                "source is too large (limit {} bytes)",
                crate::sources::MAX_SOURCE_BYTES
            );
        }
        let id = Uuid::now_v7().to_string();
        let chunks = build_source_chunks(&id, &content);
        if chunks.is_empty() {
            anyhow::bail!("source produced no readable chunks");
        }
        let now = now_rfc3339();
        let source = Source {
            id: id.clone(),
            project_id: project.id.clone(),
            kind: kind.to_string(),
            collection,
            title,
            uri,
            mime_type,
            status: "indexed".to_string(),
            size_bytes: content.len() as u64,
            sha256: sha256_hex(content.as_bytes()),
            revision: 1,
            chunk_count: chunks.len(),
            char_count: content.chars().count(),
            token_count: chunks
                .iter()
                .map(|chunk| u64::from(chunk.token_count))
                .sum(),
            error: None,
            labels,
            created_at: now.clone(),
            updated_at: now.clone(),
            indexed_at: Some(now),
        };
        let snapshot = SourceSnapshot {
            version: 1,
            source: source.clone(),
            content,
        };
        let store = self.store.clone();
        let source_for_db = source.clone();
        let chunks_for_db = chunks.clone();
        tokio::task::spawn_blocking(move || {
            store.write_source_snapshot(&snapshot)?;
            store.insert_source(&source_for_db, &chunks_for_db)
        })
        .await??;
        Ok(source)
    }
}

struct SourceFileInput {
    relative: String,
    title: String,
    mime_type: String,
    content: String,
}

struct ImportScan {
    files: Vec<SourceFileInput>,
    skipped: usize,
}

fn read_source_file(root: &Path, relative: &str) -> Result<SourceFileInput> {
    let root = root.canonicalize().context("resolving project root")?;
    let relative_path = Path::new(relative.trim());
    if relative.trim().is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative_path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
    {
        anyhow::bail!("source path must be relative, inside the project, and outside .git");
    }
    let path = root
        .join(relative_path)
        .canonicalize()
        .with_context(|| format!("resolving source file {relative}"))?;
    if !path.starts_with(&root) {
        anyhow::bail!("source path escapes the project root");
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        anyhow::bail!("source path is not a file: {relative}");
    }
    if metadata.len() > crate::sources::MAX_SOURCE_BYTES {
        anyhow::bail!(
            "source file is {} bytes; the limit is {} bytes",
            metadata.len(),
            crate::sources::MAX_SOURCE_BYTES
        );
    }
    let bytes = std::fs::read(&path)?;
    if bytes.contains(&0) {
        anyhow::bail!("source file looks binary");
    }
    let content = String::from_utf8(bytes).context("source file is not UTF-8")?;
    let relative = path
        .strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    Ok(SourceFileInput {
        title: relative.clone(),
        mime_type: source_mime_type(&relative),
        relative,
        content,
    })
}

fn collect_import_files(
    root: &Path,
    relative: Option<&str>,
    max_files: usize,
) -> Result<ImportScan> {
    let root = root.canonicalize().context("resolving project root")?;
    let search_root = match relative.map(str::trim).filter(|value| !value.is_empty()) {
        Some(relative) => {
            let path = Path::new(relative);
            if path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                anyhow::bail!("source import path must be relative and inside the project");
            }
            let resolved = root
                .join(path)
                .canonicalize()
                .with_context(|| format!("resolving source import path {relative}"))?;
            if !resolved.starts_with(&root) {
                anyhow::bail!("source import path escapes the project root");
            }
            if !resolved.is_dir() {
                anyhow::bail!("source import path must be a directory");
            }
            resolved
        }
        None => root.clone(),
    };
    let walker = WalkBuilder::new(&search_root)
        .hidden(false)
        .max_depth(Some(20))
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
        .build();
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for (scanned, entry) in walker.enumerate() {
        if files.len() >= max_files || scanned >= MAX_SOURCE_IMPORT_SCAN {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative_path = match path.strip_prefix(&root) {
            Ok(relative) => relative.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        if !is_source_extension(path) {
            skipped += 1;
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if metadata.len() > crate::sources::MAX_SOURCE_BYTES {
            skipped += 1;
            continue;
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if bytes.contains(&0) {
            skipped += 1;
            continue;
        }
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if content.trim().is_empty() {
            skipped += 1;
            continue;
        }
        files.push(SourceFileInput {
            title: relative_path.clone(),
            mime_type: source_mime_type(&relative_path),
            relative: relative_path,
            content,
        });
    }
    Ok(ImportScan { files, skipped })
}

fn is_source_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "md" | "markdown"
            | "mdx"
            | "txt"
            | "rst"
            | "org"
            | "adoc"
            | "json"
            | "jsonl"
            | "yaml"
            | "yml"
            | "toml"
            | "csv"
            | "tsv"
            | "rs"
            | "swift"
            | "py"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "hpp"
            | "m"
            | "mm"
            | "cs"
            | "rb"
            | "php"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "less"
            | "sql"
            | "graphql"
            | "proto"
            | "xml"
            | "svg"
    )
}

fn source_mime_type(path: &str) -> String {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" | "mdx" => "text/markdown",
        "json" | "jsonl" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "html" | "htm" => "text/html",
        "css" | "scss" | "less" => "text/css",
        "xml" | "svg" => "application/xml",
        "csv" | "tsv" => "text/csv",
        _ => "text/plain",
    }
    .to_string()
}

fn source_title_from_url(url: &str) -> String {
    let parsed = reqwest::Url::parse(url).ok();
    let host = parsed
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or("web");
    let tail = parsed
        .as_ref()
        .and_then(|url| url.path_segments())
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .unwrap_or("");
    let title = if tail.is_empty() {
        host.to_string()
    } else {
        format!("{host} / {tail}")
    };
    title.chars().take(200).collect()
}

const MAX_SOURCE_IMPORT_SCAN: usize = 20_000;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn source(project_id: &str, index: usize, uri: Option<String>) -> Source {
        let now = now_rfc3339();
        Source {
            id: format!("existing-{index:04}"),
            project_id: project_id.to_string(),
            kind: "file".to_string(),
            collection: None,
            title: uri.clone().unwrap_or_else(|| format!("doc-{index:04}.md")),
            uri,
            mime_type: Some("text/markdown".to_string()),
            status: "indexed".to_string(),
            size_bytes: 1,
            sha256: "a".repeat(64),
            revision: 1,
            chunk_count: 0,
            char_count: 1,
            token_count: 1,
            error: None,
            labels: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            indexed_at: Some(now),
        }
    }

    #[tokio::test]
    async fn import_skips_existing_files_beyond_the_first_thousand_sources() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join("docs")).unwrap();
        std::fs::write(repo.path().join("docs/existing.md"), "# Existing\n").unwrap();
        let store = vp_store::Store::open(data.path()).unwrap();
        let engine = Engine::new(store.clone());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("import".into()),
            )
            .await
            .unwrap();
        for index in 0..1_001 {
            let uri = if index == 0 {
                "docs/existing.md".to_string()
            } else {
                format!("docs/older-{index:04}.md")
            };
            let row = source(&project.id, index, Some(uri));
            store.insert_source(&row, &[]).unwrap();
        }
        let result = engine
            .import_sources(SourceImportParams {
                project_id: project.id.clone(),
                collection: None,
                path: Some(".".into()),
                max_files: Some(10),
                labels: Vec::new(),
            })
            .await
            .unwrap();
        assert!(result.imported.is_empty(), "{:?}", result.imported);
        assert!(result.skipped >= 1);
        let stats = engine.source_stats(project.id).await.unwrap();
        assert_eq!(stats.sources, 1_001);
    }
}
