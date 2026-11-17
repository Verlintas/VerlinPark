use super::*;

impl Engine {
    pub async fn plan_revisions(&self, session_id: &str) -> Result<Vec<PlanRevisionInfo>> {
        let events = self.recent_events(session_id, 5_000).await?;
        Ok(crate::orchestration::revisions_from_events(&events))
    }

    pub async fn revise_team_plan(
        &self,
        session_id: &str,
        plan: TeamPlan,
        reason: &str,
        author: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 1_000 {
            anyhow::bail!("revision reason must be 1-1000 chars");
        }
        let author = author.trim();
        if author.is_empty() || author.chars().count() > 80 {
            anyhow::bail!("revision author must be 1-80 chars");
        }
        let session_id = session_id.to_string();
        let store = self.store.clone();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.get_session(&session_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        crate::ensure_session_quiescent(&session, "revise the team plan")?;
        let current_raw = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.get_team_plan(&session_id)
        })
        .await??;
        let current: TeamPlan = current_raw
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .ok_or_else(|| anyhow::anyhow!("session has no TeamPlan to revise"))?;
        let tasks = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.list_plan_tasks(None, Some(&session_id))
        })
        .await??;
        let events = self.recent_events(&session_id, 5_000).await?;
        let current_revision = crate::orchestration::plan_revision(&events);
        if let Some(expected) = expected_revision
            && expected != current_revision
        {
            anyhow::bail!(
                "plan revision conflict: expected {expected}, current {current_revision}"
            );
        }
        crate::agent::validate_team_plan(&plan, max_managers)?;
        let statuses = crate::orchestration::status_map(&current, &tasks, &events);
        crate::orchestration::ensure_revision_safe(&current, &plan, &statuses)?;
        let diff = crate::orchestration::diff_plan(&current, &plan);
        let revision = current_revision.saturating_add(1);
        let snapshot = self.pin_plan_snapshot(&session, &plan, revision).await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let payload = serde_json::json!({
            "plan": plan,
            "revision": revision,
            "reason": reason,
            "author": author,
            "task_count": plan.tasks.len(),
            "added": diff.added,
            "removed": diff.removed,
            "changed": diff.changed,
            "runtime_snapshot": snapshot,
        });
        let plan_json = serde_json::to_string(&plan)?;
        let event = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            let plan_json = plan_json.clone();
            let payload = payload.clone();
            let snapshot_json = snapshot_json.clone();
            move || {
                store.set_team_plan_revision_with_runtime_snapshot_and_event(
                    &session_id,
                    &plan_json,
                    "team_plan.revised",
                    payload,
                    Some(&snapshot_json),
                )
            }
        })
        .await??;
        let _ = self.events.send(event.clone());
        self.scheduler.sync_plan(&session, &plan).await?;
        Ok(ReviseTeamPlanResult {
            revision: PlanRevisionInfo {
                revision,
                seq: event.seq,
                kind: event.kind,
                reason: Some(reason.to_string()),
                author: author.to_string(),
                task_count: plan.tasks.len(),
                added: diff.added,
                removed: diff.removed,
                changed: diff.changed,
                created_at: event.created_at,
            },
            plan,
        })
    }

    pub async fn plan_revision_detail(
        &self,
        session_id: &str,
        revision: u32,
    ) -> Result<PlanRevisionDetailResult> {
        let events = self.all_events(session_id).await?;
        let revisions = crate::orchestration::revisions_from_events(&events);
        let info = revisions
            .iter()
            .find(|candidate| candidate.revision == revision)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("plan revision {revision} not found"))?;
        let plan = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "team_plan.submitted" | "team_plan.revised"
                )
            })
            .filter(|event| {
                event
                    .payload
                    .get("revision")
                    .and_then(serde_json::Value::as_u64)
                    .map(|value| value as u32)
                    .unwrap_or_else(|| {
                        revisions
                            .iter()
                            .find(|candidate| candidate.seq == event.seq)
                            .map(|candidate| candidate.revision)
                            .unwrap_or(0)
                    })
                    == revision
            })
            .find_map(crate::orchestration::plan::plan_from_event)
            .ok_or_else(|| anyhow::anyhow!("plan revision {revision} has no stored plan"))?;
        Ok(PlanRevisionDetailResult {
            revision: info,
            plan,
        })
    }

    pub async fn restore_team_plan_revision(
        &self,
        session_id: &str,
        revision: u32,
        reason: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let detail = self.plan_revision_detail(session_id, revision).await?;
        self.revise_team_plan(
            session_id,
            detail.plan,
            reason,
            "user",
            expected_revision,
            max_managers,
        )
        .await
    }

    pub async fn edit_plan_task(
        &self,
        session_id: &str,
        task: vp_protocol::types::PlanTask,
        reason: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let current = self
            .get_team_plan(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session has no TeamPlan to edit"))?;
        let mut plan = current.clone();
        let index = plan
            .tasks
            .iter()
            .position(|candidate| candidate.id == task.id)
            .ok_or_else(|| anyhow::anyhow!("task not found in plan: {}", task.id))?;
        plan.tasks[index] = task;
        self.revise_team_plan(
            session_id,
            plan,
            reason,
            "user",
            expected_revision,
            max_managers,
        )
        .await
    }

    pub async fn set_team_plan(&self, session_id: &str, plan: &TeamPlan) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let json = serde_json::to_string(plan)?;
        let loaded = {
            let store = store.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || store.get_session(&id)).await??
        }
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let revision = self
            .current_plan_revision(session_id)
            .await?
            .saturating_add(1);
        let snapshot = self.pin_plan_snapshot(&loaded, plan, revision).await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let event = tokio::task::spawn_blocking(move || {
            store.set_team_plan_with_runtime_snapshot_and_event(&id, &json, Some(&snapshot_json))
        })
        .await??;
        let _ = self.events.send(event);
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        self.scheduler.sync_plan(&session, plan).await?;
        Ok(())
    }

    pub async fn get_team_plan(&self, session_id: &str) -> Result<Option<TeamPlan>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let raw = tokio::task::spawn_blocking(move || store.get_team_plan(&id)).await??;
        Ok(raw.and_then(|json| serde_json::from_str(&json).ok()))
    }
}
