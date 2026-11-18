use super::*;

impl Engine {
    pub async fn memory_conflicts(
        &self,
        project_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<MemoryConflictsResult> {
        if let Some(status) = status
            && !matches!(status, "open" | "resolved")
        {
            anyhow::bail!("memory conflict status must be open or resolved");
        }
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let status = status.map(str::to_string);
        let conflicts = tokio::task::spawn_blocking(move || {
            store.list_memory_conflicts(&project, status.as_deref(), limit)
        })
        .await??;
        Ok(MemoryConflictsResult {
            project_id: project_id.to_string(),
            conflicts,
        })
    }

    /// Deterministic detection pass. Never auto-resolves and never changes a
    /// memory lifecycle status.
    pub async fn memory_conflict_scan(&self, project_id: &str) -> Result<MemoryConflictScanResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let scan =
            tokio::task::spawn_blocking(move || store.scan_memory_conflicts(&project)).await??;
        for event in &scan.events {
            let _ = self.events.send(event.clone());
        }
        Ok(MemoryConflictScanResult {
            project_id: project_id.to_string(),
            detected: u32::try_from(scan.new.len()).unwrap_or(u32::MAX),
            open: u32::try_from(scan.open.len()).unwrap_or(u32::MAX),
            conflicts: scan.open,
        })
    }

    pub async fn memory_conflict_resolve(
        &self,
        id: &str,
        action: &str,
        note: Option<&str>,
    ) -> Result<MemoryConflictResolveResult> {
        if !matches!(action, "keep_a" | "keep_b" | "keep_both" | "archive_both") {
            anyhow::bail!(
                "memory conflict action must be keep_a, keep_b, keep_both, or archive_both"
            );
        }
        let store = self.store.clone();
        let conflict_id = id.to_string();
        let conflict = tokio::task::spawn_blocking(move || store.get_memory_conflict(&conflict_id))
            .await??
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "memory conflict {id} not found: memory no longer exists or the conflict was removed with a deleted memory"
                )
            })?;
        let project_id = conflict.project_id.clone();
        let store = self.store.clone();
        let conflict_id = id.to_string();
        let action = action.to_string();
        let note = note
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(str::to_string);
        let resolution = tokio::task::spawn_blocking(move || {
            store.resolve_memory_conflict(&project_id, &conflict_id, &action, note.as_deref())
        })
        .await??;
        for event in &resolution.events {
            let _ = self.events.send(event.clone());
        }
        Ok(MemoryConflictResolveResult {
            conflict: resolution.conflict,
            memories: resolution.memories,
        })
    }

    pub async fn memory_health(&self, project_id: &str) -> Result<MemoryHealthResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking(move || store.memory_health(&project)).await?
    }

    /// Daemon tick: detect conflicts for every project. Detection only; it
    /// never resolves a conflict or changes a memory status.
    pub async fn scan_all_memory_conflicts(&self) -> Result<(usize, usize, usize)> {
        let projects = self.list_projects().await?;
        let mut projects_scanned = 0usize;
        let mut detected = 0usize;
        let mut open = 0usize;
        for project in projects {
            match self.memory_conflict_scan(&project.id).await {
                Ok(result) => {
                    projects_scanned += 1;
                    detected += result.detected as usize;
                    open += result.open as usize;
                    if result.detected > 0 {
                        tracing::info!(
                            project_id = %project.id,
                            detected = result.detected,
                            open = result.open,
                            "memory conflict scan detected new same-scope conflicts"
                        );
                    } else {
                        tracing::debug!(
                            project_id = %project.id,
                            detected = result.detected,
                            open = result.open,
                            "memory conflict scan complete"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, project_id = %project.id, "memory conflict scan failed");
                }
            }
        }
        Ok((projects_scanned, detected, open))
    }

    /// Daemon tick: sweep only projects whose policy opted into `sweep_stale`.
    pub async fn sweep_memory_staleness(&self) -> Result<(usize, usize)> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_memory_sweep_policies()).await??;
        let mut projects = 0;
        let mut marked = 0;
        for (project_id, policy) in policies {
            if !policy.sweep_stale {
                continue;
            }
            match self.memory_sweep(&project_id, None, false).await {
                Ok(result) => {
                    projects += 1;
                    marked += result.marked_stale;
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "memory staleness sweep failed");
                }
            }
        }
        Ok((projects, marked))
    }
}
