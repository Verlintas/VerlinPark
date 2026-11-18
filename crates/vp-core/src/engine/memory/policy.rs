use super::*;

impl Engine {
    pub async fn get_memory_policy(&self, project_id: &str) -> Result<MemoryPolicyResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_memory_policy(&project)).await??;
        Ok(MemoryPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn set_memory_policy(
        &self,
        project_id: &str,
        policy: MemoryPolicy,
    ) -> Result<MemoryPolicyResult> {
        validate_memory_policy(&policy)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let stored = policy.clone();
        tokio::task::spawn_blocking(move || store.set_memory_policy(&project, &stored)).await??;
        Ok(MemoryPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn sweep_memory_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_memory_policies()).await??;
        let mut removed = 0;
        for (project_id, policy) in policies {
            if policy.max_items.is_none() && policy.max_age_days.is_none() {
                continue;
            }
            match self
                .prune_memories(&project_id, policy.max_age_days, policy.max_items, false)
                .await
            {
                Ok(result) => removed += result.removed,
                Err(error) => {
                    tracing::warn!(%error, %project_id, "memory retention sweep failed");
                }
            }
        }
        Ok(removed)
    }
}

fn validate_memory_policy(policy: &MemoryPolicy) -> Result<()> {
    if let Some(max_items) = policy.max_items
        && !(1..=100_000).contains(&max_items)
    {
        anyhow::bail!("memory max_items must be between 1 and 100000");
    }
    if let Some(max_age_days) = policy.max_age_days
        && !(1..=36_500).contains(&max_age_days)
    {
        anyhow::bail!("memory max_age_days must be between 1 and 36500");
    }
    if policy.enabled && policy.max_items.is_none() && policy.max_age_days.is_none() {
        anyhow::bail!("enabled memory policy requires max_items or max_age_days");
    }
    for (name, days) in [
        ("task_result_verify_days", policy.task_result_verify_days),
        ("default_verify_days", policy.default_verify_days),
    ] {
        if let Some(days) = days
            && !(1..=36_500).contains(&days)
        {
            anyhow::bail!("memory {name} must be between 1 and 36500 (or null to disable)");
        }
    }
    if let Some(max_chars) = policy.brief_max_chars
        && !(200..=100_000).contains(&max_chars)
    {
        anyhow::bail!("memory brief_max_chars must be between 200 and 100000");
    }
    if let Some(embedding) = &policy.embedding {
        crate::memory_embedding::validate_embedding_policy(embedding)?;
    }
    Ok(())
}
