use super::*;
use crate::gui;

impl Engine {
    pub async fn get_gui_policy(&self, project_id: &str) -> Result<vp_protocol::types::GuiPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_gui_policy(&project_id)).await?
    }

    pub async fn set_gui_policy(
        &self,
        project_id: &str,
        policy: vp_protocol::types::GuiPolicy,
    ) -> Result<vp_protocol::types::GuiPolicy> {
        policy.validate().map_err(|error| anyhow::anyhow!(error))?;
        let policy = policy.normalized();
        let store = self.store.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            move || store.get_project(&project)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = policy.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking(move || store.set_gui_policy(&project, &for_store)).await??;
        if let Err(error) = self.record_project_manifest().await {
            tracing::warn!(%error, "failed to record GUI policy in the project manifest");
        }
        Ok(policy)
    }

    pub async fn gui_policy_for_session(
        &self,
        session_id: &str,
    ) -> Result<vp_protocol::types::GuiPolicy> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.gui_policy_for_session(&session_id)).await?
    }

    /// Acquire or wait for the single physical GUI surface. The wait is bounded
    /// by the project policy; a timeout returns a typed `GUI_BUSY` with the
    /// holder and retry hint. There is no hidden infinite wait.
    pub async fn gui_begin_action(
        &self,
        surface: gui::GuiSurface,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
    ) -> Result<gui::GuiAcquire> {
        let policy = self.gui_policy_for_session(session_id).await?;
        self.gui_begin_action_with_policy(&policy, surface, session_id, agent_id, agent_name)
            .await
    }

    /// Same acquisition with a policy the caller already loaded. Managers use
    /// this so one action performs one project-policy lookup.
    pub async fn gui_begin_action_with_policy(
        &self,
        policy: &vp_protocol::types::GuiPolicy,
        surface: gui::GuiSurface,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
    ) -> Result<gui::GuiAcquire> {
        self.gui
            .begin(self, policy, surface, session_id, agent_id, agent_name)
            .await
    }

    /// Record one bounded GUI action event. Screen content, typed text, and
    /// element values are never included. A failed event append does not fail
    /// the completed GUI action.
    pub async fn gui_finish_action(
        &self,
        ticket: &gui::GuiActionTicket,
        outcome: gui::GuiActionOutcome,
    ) -> Option<vp_protocol::types::GuiActionInfo> {
        let action = self.gui.finish(ticket, outcome).await;
        let payload = match serde_json::to_value(&action) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "failed to serialize GUI action event");
                return None;
            }
        };
        if let Err(error) = self
            .append_event(&ticket.session_id, "gui.action", payload)
            .await
        {
            tracing::warn!(%error, "failed to append GUI action event");
        }
        Some(action)
    }

    /// Release the GUI lease immediately on close or session cleanup.
    pub async fn gui_end_session(&self, session_id: &str, reason: &str) -> bool {
        self.gui.release_session(session_id, reason).await
    }

    /// Release leases whose grace elapsed. Called by the short GUI sweeper.
    pub async fn sweep_gui_grace(&self) -> usize {
        self.gui.sweep().await
    }

    /// Lightweight holder/FIFO snapshot used by dashboard alerts. Unlike
    /// `gui_status` this does not touch helper/TCC status.
    pub async fn gui_lease_snapshot(
        &self,
    ) -> (
        Option<vp_protocol::types::GuiLeaseInfo>,
        Vec<vp_protocol::types::GuiQueueInfo>,
    ) {
        let (mut holder, queue) = self.gui.status().await;
        if let Some(holder) = holder.as_mut() {
            holder.owner_id = self.runtime_id.clone();
        } else if let Ok(Some(lease)) = self.gui_lease_holder().await {
            holder = Some(vp_protocol::types::GuiLeaseInfo {
                lease_id: lease.lease_id,
                owner_id: lease.owner_id,
                session_id: lease.session_id,
                agent_id: lease.agent_id,
                agent_name: lease.agent_name,
                surface: lease.surface,
                lease_generation: lease.generation,
                acquired_at: lease.acquired_at,
                last_action_at: lease.heartbeat_at,
                expires_at: lease.expires_at,
                queue_position: None,
            });
        }
        (holder, queue)
    }

    pub async fn gui_warm_helper_counts(&self) -> (usize, usize) {
        (self.computers.warm_helpers(), self.browsers.warm_helpers())
    }

    pub async fn gui_timeline(
        &self,
        session_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<vp_protocol::types::GuiActionInfo>> {
        let store = self.store.clone();
        let session_id = session_id.map(str::to_string);
        tokio::task::spawn_blocking(move || store.recent_gui_actions(session_id.as_deref(), limit))
            .await?
    }

    /// Bounded audit history for GUI timeline prunes. Entries contain counts
    /// and cutoffs only; pruned action payloads are gone and are never
    /// reconstructed.
    pub async fn gui_timeline_audit(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::GuiTimelineAuditResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let limit = limit.unwrap_or(50).clamp(1, 1_000);
        let (entries, total) =
            tokio::task::spawn_blocking(move || store.gui_timeline_prune_audit(&project, limit))
                .await??;
        Ok(vp_protocol::types::GuiTimelineAuditResult {
            project_id: project_id.to_string(),
            entries,
            total,
            limit,
        })
    }

    /// Read-only retention stats for the GUI action timeline. It performs one
    /// COUNT/MIN/MAX query and never reads payloads or mirrors.
    pub async fn gui_timeline_retention(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
    ) -> Result<vp_protocol::types::GuiTimelineRetentionResult> {
        let policy = self.get_gui_policy(project_id).await?;
        let days = older_than_days.unwrap_or(policy.timeline_retention_days);
        if days > vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS {
            anyhow::bail!(
                "older_than_days must be 0..={}",
                vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS
            );
        }
        let cutoff = if days == 0 {
            None
        } else {
            Some(gui_retention_cutoff(days)?)
        };
        let store = self.store.clone();
        let project = project_id.to_string();
        let cutoff_for_query = cutoff.clone();
        let stats = tokio::task::spawn_blocking(move || {
            store.gui_action_retention_stats(&project, cutoff_for_query.as_deref())
        })
        .await??;
        Ok(vp_protocol::types::GuiTimelineRetentionResult {
            project_id: project_id.to_string(),
            retention_days: days,
            keep_forever: days == 0,
            cutoff,
            total: stats.total,
            eligible: stats.eligible,
            oldest_at: stats.oldest_at,
            newest_at: stats.newest_at,
        })
    }

    /// Explicit, bounded prune of the GUI action timeline for one project.
    /// `older_than_days = 0` means "use the project policy"; an explicit
    /// `Some(0)` from an RPC is rejected before reaching this method because
    /// pruning everything is not a retention operation.
    pub async fn prune_gui_timeline(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        dry_run: bool,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::GuiTimelinePruneResult> {
        let policy = self.get_gui_policy(project_id).await?;
        let days = older_than_days.unwrap_or(policy.timeline_retention_days);
        if days == 0 {
            anyhow::bail!(
                "GUI timeline retention is 0 (keep forever); set a positive retention or pass older_than_days"
            );
        }
        if days > vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS {
            anyhow::bail!(
                "older_than_days must be 1..={}",
                vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS
            );
        }
        let cutoff = gui_retention_cutoff(days)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let outcome = tokio::task::spawn_blocking(move || {
            store.prune_gui_action_events(&project, &cutoff, days, limit.unwrap_or(1_000), dry_run)
        })
        .await??;
        Ok(gui_prune_result(outcome))
    }

    /// Daemon sweep: apply each project's `timeline_retention_days` with a
    /// bounded batch. `0` keeps events forever and is skipped. Errors are
    /// returned per project so the daemon can log without stopping other
    /// projects; a partial prune is never reported as complete.
    pub async fn sweep_gui_timeline_retention(
        &self,
        limit_per_project: u32,
    ) -> Result<Vec<vp_protocol::types::GuiTimelinePruneResult>> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        let mut results = Vec::new();
        for project in projects {
            let policy = self.get_gui_policy(&project.id).await?;
            if policy.timeline_retention_days == 0 {
                continue;
            }
            match self
                .prune_gui_timeline(
                    &project.id,
                    Some(policy.timeline_retention_days),
                    false,
                    Some(limit_per_project),
                )
                .await
            {
                Ok(result) if result.pruned > 0 || result.skipped_events > 0 => {
                    results.push(result)
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        %error,
                        project_id = %project.id,
                        "GUI timeline retention sweep failed for project"
                    );
                }
            }
        }
        Ok(results)
    }

    /// Unified GUI status for Manage Dash, the CLI, and `gui.status`. The
    /// computer status path is cached for the project `status_ttl_secs`.
    pub async fn gui_status(
        &self,
        project_id: Option<&str>,
    ) -> Result<vp_protocol::types::GuiStatusResult> {
        let policy = match project_id {
            Some(project_id) => self.get_gui_policy(project_id).await?,
            None => vp_protocol::types::GuiPolicy::default(),
        };
        let computer = self.computers.status(&policy, false).await;
        let browser = self.browsers.status();
        let (mut holder, queue) = self.gui.status().await;
        if let Some(holder) = holder.as_mut() {
            holder.owner_id = self.runtime_id.clone();
        } else if let Ok(Some(lease)) = self.gui_lease_holder().await {
            holder = Some(vp_protocol::types::GuiLeaseInfo {
                lease_id: lease.lease_id,
                owner_id: lease.owner_id,
                session_id: lease.session_id,
                agent_id: lease.agent_id,
                agent_name: lease.agent_name,
                surface: lease.surface,
                lease_generation: lease.generation,
                acquired_at: lease.acquired_at,
                last_action_at: lease.heartbeat_at,
                expires_at: lease.expires_at,
                queue_position: None,
            });
        }
        let (warm_computer_helpers, warm_browser_helpers) = self.gui_warm_helper_counts().await;
        Ok(vp_protocol::types::GuiStatusResult {
            runtime_id: self.runtime_id.clone(),
            lease_surface: gui::GUI_SURFACE.to_string(),
            holder,
            queue,
            policy,
            computer,
            browser_helper_available: browser.helper_available,
            browser_helper_path: browser.helper_path,
            browser_sandbox_available: browser.sandbox_available,
            warm_computer_helpers,
            warm_browser_helpers,
            stale_ref_supported: true,
        })
    }
}

/// RFC3339 UTC cutoff used by GUI timeline retention. `days` is validated by
/// the caller; saturating arithmetic avoids a panic on an absurd policy.
fn gui_retention_cutoff(days: u32) -> Result<String> {
    let cutoff = time::OffsetDateTime::now_utc()
        .checked_sub(time::Duration::days(i64::from(days)))
        .ok_or_else(|| anyhow!("GUI retention cutoff is out of range"))?;
    cutoff
        .format(&time::format_description::well_known::Rfc3339)
        .context("formatting GUI retention cutoff")
}

fn gui_prune_result(
    outcome: vp_store::store::GuiTimelinePruneOutcome,
) -> vp_protocol::types::GuiTimelinePruneResult {
    vp_protocol::types::GuiTimelinePruneResult {
        project_id: outcome.project_id,
        retention_days: outcome.retention_days,
        cutoff: outcome.cutoff,
        dry_run: outcome.dry_run,
        pruned: outcome.pruned,
        remaining: outcome.remaining,
        sessions: outcome.sessions,
        skipped_sessions: outcome.skipped_sessions,
        skipped_events: outcome.skipped_events,
        other_kinds_touched: outcome.other_kinds_touched,
        limit: outcome.limit,
        truncated: outcome.truncated,
        reason: outcome.reason,
        affected_sessions: outcome.affected_sessions,
    }
}
