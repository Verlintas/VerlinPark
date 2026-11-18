use super::*;

impl Engine {
    /// Exact semantic scan used by the default lane and by every IVF fallback.
    /// The candidate window is bounded by the policy and decoded/scored by the
    /// same function as the ANN probe, so a fallback cannot change cosine
    /// semantics.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn scan_semantic_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        include_stale: bool,
        max_candidates: u32,
        query_vector: &[f32],
        min_similarity: f64,
    ) -> Result<Vec<(MemoryItem, f64)>> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let model = model.to_string();
        let semantic_candidates = tokio::task::spawn_blocking(move || {
            store.memory_semantic_candidates(
                &project,
                &model,
                dimensions,
                include_stale,
                max_candidates,
            )
        })
        .await??;
        Ok(crate::memory::semantic_scores(
            semantic_candidates,
            query_vector,
            dimensions,
            min_similarity,
        ))
    }
}
