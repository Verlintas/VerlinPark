use super::support::{
    build_source_chunks, normalize_source_collection, validate_source_labels, validate_source_title,
};
use super::*;

impl Engine {
    pub async fn source_collections(&self, project_id: String) -> Result<SourceCollectionsResult> {
        let store = self.store.clone();
        let project_for_count = project_id.clone();
        let collections =
            tokio::task::spawn_blocking(move || store.list_source_collections(&project_id))
                .await??;
        let store = self.store.clone();
        let stats =
            tokio::task::spawn_blocking(move || store.source_stats(&project_for_count)).await??;
        Ok(SourceCollectionsResult {
            total_sources: stats.sources,
            collections,
        })
    }

    /// Replace user-editable source metadata and keep the durable snapshot in
    /// sync in the same blocking section. Content/chunks are untouched.
    pub async fn update_source_metadata(
        &self,
        params: UpdateSourceMetadataParams,
    ) -> Result<Source> {
        let title = params.title.trim();
        validate_source_title(title)?;
        let collection = normalize_source_collection(params.collection.as_deref())?;
        let labels = validate_source_labels(params.labels)?;
        let store = self.store.clone();
        let id = params.source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {}", params.source_id))?;
        let mut updated = current;
        updated.title = title.to_string();
        updated.collection = collection;
        updated.labels = labels;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    pub async fn set_source_collection(
        &self,
        source_id: String,
        collection: Option<String>,
    ) -> Result<Source> {
        let collection = normalize_source_collection(collection.as_deref())?;
        let store = self.store.clone();
        let id = source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let mut updated = current;
        updated.collection = collection;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    pub async fn set_source_labels(
        &self,
        source_id: String,
        labels: Vec<String>,
    ) -> Result<Source> {
        let labels = validate_source_labels(labels)?;
        let store = self.store.clone();
        let id = source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let mut updated = current;
        updated.labels = labels;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    /// Rebuild the SQLite source projection from durable JSON snapshots. Used
    /// by `db-rebuild`; malformed snapshots are skipped and counted by the
    /// caller's rebuild report.
    pub async fn restore_sources_from_disk(&self) -> Result<usize> {
        let store = self.store.clone();
        let snapshots =
            tokio::task::spawn_blocking(move || store.list_source_snapshots()).await??;
        let mut restored = 0usize;
        for snapshot in snapshots {
            let project = {
                let store = self.store.clone();
                let id = snapshot.source.project_id.clone();
                tokio::task::spawn_blocking(move || store.get_project(&id)).await??
            };
            if project.is_none() {
                tracing::warn!(
                    source_id = %snapshot.source.id,
                    "skipping source snapshot for a missing project"
                );
                continue;
            }
            let content = crate::sources::normalize_source_text(&snapshot.content);
            let chunks = build_source_chunks(&snapshot.source.id, &content);
            if chunks.is_empty() {
                continue;
            }
            let now = now_rfc3339();
            let mut source = snapshot.source.clone();
            source.chunk_count = chunks.len();
            source.char_count = content.chars().count();
            source.token_count = chunks
                .iter()
                .map(|chunk| u64::from(chunk.token_count))
                .sum();
            source.status = "indexed".to_string();
            source.error = None;
            source.updated_at = now.clone();
            source.indexed_at = Some(now);
            let store = self.store.clone();
            let source_for_db = source.clone();
            let chunks_for_db = chunks.clone();
            tokio::task::spawn_blocking(move || {
                store.insert_source(&source_for_db, &chunks_for_db)
            })
            .await??;
            restored += 1;
        }
        Ok(restored)
    }
}
