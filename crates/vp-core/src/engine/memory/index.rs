use super::*;

impl Engine {
    /// Derived IVF index health for one project. Contains aggregate counts and
    /// generation metadata only; centroids and vectors never leave the store.
    pub async fn memory_embedding_index_status(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsIndexStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let store = self.store.clone();
        let project = project_id.to_string();
        let index = tokio::task::spawn_blocking(move || {
            crate::memory_index::index_status(&store, &project, policy.embedding.as_ref())
        })
        .await??;
        Ok(MemoryEmbeddingsIndexStatusResult {
            project_id: project_id.to_string(),
            index,
        })
    }

    /// Explicit derived-index rebuild. Never calls the embedding endpoint,
    /// never changes memory truth, and never deletes vectors. Scan mode or a
    /// below-minimum vector count removes stale derived index state and
    /// returns `rebuilt = false` with a reason.
    pub async fn memory_embedding_index_rebuild(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsIndexRebuildResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let Some(embedding) = policy.embedding else {
            return Ok(MemoryEmbeddingsIndexRebuildResult {
                project_id: project_id.to_string(),
                rebuilt: false,
                reason: Some("not_configured".to_string()),
                generation: None,
                vector_count: 0,
                assigned_vectors: 0,
                cluster_count: 0,
                nlist: 0,
            });
        };
        crate::memory_embedding::validate_embedding_policy(&embedding)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let embedding_for_build = embedding.clone();
        let report = tokio::task::spawn_blocking(move || {
            crate::memory_index::rebuild_index(&store, &project, &embedding_for_build)
        })
        .await??;
        Ok(MemoryEmbeddingsIndexRebuildResult {
            project_id: project_id.to_string(),
            rebuilt: report.rebuilt,
            reason: report.reason,
            generation: report.generation,
            vector_count: report.vector_count,
            assigned_vectors: report.assigned_vectors,
            cluster_count: report.cluster_count,
            nlist: report.nlist,
        })
    }

    /// Look up the project embedding policy and run bounded index
    /// maintenance. Used after delete/prune so assignments and centroid
    /// metadata cannot drift from the live vector set.
    pub(super) async fn maintain_memory_index_if_configured(&self, project_id: &str) {
        match self.get_memory_policy(project_id).await {
            Ok(policy) => {
                if let Some(embedding) = policy.policy.embedding {
                    self.maintain_project_memory_index(project_id, &embedding)
                        .await;
                }
            }
            Err(error) => {
                tracing::warn!(%error, %project_id, "could not load memory policy for index maintenance");
            }
        }
    }

    /// Bounded derived-index maintenance after an embedding batch. This is
    /// best-effort: a failed index build never fails memory writes or the
    /// embedding reconciler, and recall falls back to the exact scan.
    pub(super) async fn maintain_project_memory_index(
        &self,
        project_id: &str,
        policy: &MemoryEmbeddingPolicy,
    ) {
        if !policy.uses_ivf_index() {
            return;
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy = policy.clone();
        match tokio::task::spawn_blocking(move || {
            crate::memory_index::maintain_index(&store, &project, &policy)
        })
        .await
        {
            Ok(Ok(report)) => {
                if let Some(generation) = report.rebuilt_generation {
                    tracing::info!(
                        %project_id,
                        generation,
                        assigned = report.assigned_vectors,
                        vectors = report.vector_count,
                        "memory embedding IVF index rebuilt"
                    );
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, %project_id, "memory embedding IVF index maintenance failed");
            }
            Err(error) => {
                tracing::warn!(%error, %project_id, "memory embedding IVF index task failed");
            }
        }
    }
}
