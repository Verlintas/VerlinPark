//! Project/session auto-budget policy persistence and session budget writes.
//!
//! Effective token/cost/time budgets stay on `sessions` for compatibility.
//! `session_auto_budgets` carries the mode, resolved hard caps, policy copy,
//! and bounded operational state. Every budget mutation appends a session
//! event in the same transaction as the projection update.

use super::*;
use vp_protocol::types::{
    AutoBudgetExtension, AutoBudgetExtensionOutcome, ProjectAutoBudgetEvent,
    SESSION_AUTO_PAUSED_EVENT, SESSION_AUTO_RESUMED_EVENT, SESSION_AUTO_STATE_EVENT,
    SESSION_BUDGET_UPDATED_EVENT, SessionAutoBudgetRecord, SessionAutoBudgetState,
    auto_budget_codes,
};

/// Explicit session budget mutation. `None` means "keep the current value";
/// the `clear_*` flags remove an optional value. This mirrors the wire
/// `SetSessionBudgetParams` without making the store depend on RPC parsing.
#[derive(Debug, Clone, Default)]
pub struct SessionBudgetUpdate {
    pub mode: Option<BudgetMode>,
    pub unlimited: Option<bool>,
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
    pub time_budget_secs: Option<u64>,
    pub clear_token_budget: bool,
    pub clear_cost_budget: bool,
    pub clear_time_budget: bool,
    pub hard_token_cap: Option<u64>,
    pub hard_cost_cap_usd: Option<f64>,
    pub hard_wall_clock_secs: Option<u64>,
    pub clear_hard_token_cap: bool,
    pub clear_hard_cost_cap: bool,
    pub clear_hard_wall_clock_cap: bool,
    pub policy: Option<AutoBudgetPolicy>,
}

impl Store {
    /// Project-scoped auto policy. A missing row is the disabled default, not
    /// "allow anything"; auto creation still needs an explicit hard cap.
    pub fn get_project_auto_budget_policy(&self, project_id: &str) -> Result<AutoBudgetPolicy> {
        let conn = self.lock()?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT policy_json FROM project_auto_budget_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        match stored {
            Some(raw) => {
                let policy: AutoBudgetPolicy = serde_json::from_str(&raw)
                    .context("parsing stored project auto-budget policy")?;
                policy.validate().map_err(|error| {
                    anyhow!("stored project auto-budget policy is invalid: {error}")
                })?;
                Ok(policy)
            }
            None => Ok(AutoBudgetPolicy::default()),
        }
    }

    /// Read the stored policy plus its latest update timestamp, if any.
    pub fn get_project_auto_budget_policy_record(
        &self,
        project_id: &str,
    ) -> Result<(AutoBudgetPolicy, Option<String>)> {
        let conn = self.lock()?;
        let stored: Option<(String, String)> = conn
            .query_row(
                "SELECT policy_json, updated_at FROM project_auto_budget_policies
                 WHERE project_id = ?1",
                params![project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match stored {
            Some((raw, updated_at)) => {
                let policy: AutoBudgetPolicy = serde_json::from_str(&raw)
                    .context("parsing stored project auto-budget policy")?;
                policy.validate().map_err(|error| {
                    anyhow!("stored project auto-budget policy is invalid: {error}")
                })?;
                Ok((policy, Some(updated_at)))
            }
            None => Ok((AutoBudgetPolicy::default(), None)),
        }
    }

    /// Persist a project policy and append the bounded
    /// `project.auto_budget.updated` audit event in one transaction.
    pub fn set_project_auto_budget_policy_with_event(
        &self,
        project_id: &str,
        policy: &AutoBudgetPolicy,
    ) -> Result<(AutoBudgetPolicy, ProjectAutoBudgetEvent)> {
        policy
            .validate()
            .map_err(|error| anyhow!("invalid project auto-budget policy: {error}"))?;
        let encoded = serde_json::to_string(policy)?;
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        require_project_tx(&tx, project_id)?;
        tx.execute(
            "INSERT INTO project_auto_budget_policies (project_id, policy_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy_json = excluded.policy_json,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now],
        )?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM project_auto_budget_events WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        let event = ProjectAutoBudgetEvent {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            seq: seq as u64,
            kind: vp_protocol::types::PROJECT_AUTO_BUDGET_UPDATED_EVENT.to_string(),
            payload: serde_json::json!({
                "project_id": project_id,
                "policy": policy,
                "updated_at": now,
            }),
            created_at: now,
        };
        insert_project_auto_budget_event_tx(&tx, &event)?;
        tx.commit()?;
        Ok((policy.clone(), event))
    }

    /// Rebuild/import path: persist the policy without inventing a new event.
    pub fn set_project_auto_budget_policy_snapshot(
        &self,
        project_id: &str,
        policy: &AutoBudgetPolicy,
    ) -> Result<()> {
        policy
            .validate()
            .map_err(|error| anyhow!("invalid project auto-budget policy: {error}"))?;
        let encoded = serde_json::to_string(policy)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_auto_budget_policies (project_id, policy_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy_json = excluded.policy_json,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn insert_project_auto_budget_event_snapshot(
        &self,
        event: &ProjectAutoBudgetEvent,
    ) -> Result<()> {
        if event.id.trim().is_empty() || event.seq == 0 {
            return Err(anyhow!(
                "project auto-budget event has an empty id or zero seq"
            ));
        }
        let encoded = serde_json::to_string(&event.payload)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO project_auto_budget_events
             (id, project_id, seq, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.project_id,
                event.seq as i64,
                event.kind,
                encoded,
                event.created_at
            ],
        )?;
        Ok(())
    }

    pub fn list_project_auto_budget_events(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ProjectAutoBudgetEvent>> {
        let limit = limit.unwrap_or(10_000).clamp(1, 100_000) as i64;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, seq, kind, payload, created_at
             FROM project_auto_budget_events
             WHERE project_id = ?1
             ORDER BY seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_id, limit], |row| {
            let payload: String = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as u64,
                row.get::<_, String>(3)?,
                payload,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (id, project_id, seq, kind, payload, created_at) = row?;
            events.push(ProjectAutoBudgetEvent {
                id,
                project_id,
                seq,
                kind,
                payload: serde_json::from_str(&payload)
                    .context("parsing stored project auto-budget event payload")?,
                created_at,
            });
        }
        events.reverse();
        Ok(events)
    }

    pub fn get_session_auto_budget(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionAutoBudgetRecord>> {
        let conn = self.lock()?;
        query_session_auto_budget(&conn, session_id)
    }

    /// Apply an explicit session budget transition. The session row, auto row,
    /// and `session.budget.updated` event commit together. Effective budgets
    /// can never be lowered below already spent tokens/USD, and `auto` requires
    /// at least one hard cap.
    pub fn set_session_budget_with_event(
        &self,
        session_id: &str,
        update: &SessionBudgetUpdate,
    ) -> Result<(Session, SessionAutoBudgetRecord, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: Session = tx
            .query_row(
                "SELECT id, project_id, goal, status, worktree, branch, token_budget,
                        cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd,
                        base_commit, tokens_in, tokens_out, created_at, priority, prompt_layout
                 FROM sessions WHERE id = ?1",
                params![session_id],
                map_session,
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if current.status == "imported" {
            return Err(anyhow!("imported sessions are read-only archives"));
        }

        let existing = query_session_auto_budget(&tx, session_id)?;
        let project_policy =
            query_project_auto_budget_policy(&tx, &current.project_id)?.unwrap_or_default();

        let stored_mode = resolve_stored_mode(update, &current, existing.as_ref())?;
        let unlimited = resolve_unlimited(update, &current, stored_mode)?;
        let effective_mode = if unlimited {
            BudgetMode::Unlimited
        } else {
            stored_mode
        };

        if effective_mode != BudgetMode::Auto && update.policy.is_some() {
            return Err(BudgetValidationError::new(
                auto_budget_codes::BUDGET_POLICY_NOT_APPLICABLE,
                "a session auto-budget policy requires budget mode auto",
            )
            .into());
        }
        let mut policy = update
            .policy
            .clone()
            .or_else(|| existing.as_ref().map(|row| row.policy.clone()))
            .unwrap_or_else(|| {
                if effective_mode == BudgetMode::Auto {
                    project_policy
                } else {
                    AutoBudgetPolicy::default()
                }
            });
        policy
            .validate()
            .map_err(|error| anyhow!("invalid auto-budget policy: {error}"))?;

        let mut caps = if update.policy.is_some() {
            AutoBudgetCaps::from_policy(&policy)
        } else if let Some(row) = existing.as_ref() {
            row.caps.clone()
        } else if effective_mode == BudgetMode::Auto {
            AutoBudgetCaps::from_policy(&policy)
        } else {
            AutoBudgetCaps::default()
        };
        if let Some(value) = update.hard_token_cap {
            caps.token = Some(value);
        }
        if update.clear_hard_token_cap {
            caps.token = None;
        }
        if let Some(value) = update.hard_cost_cap_usd {
            caps.cost_usd = Some(value);
        }
        if update.clear_hard_cost_cap {
            caps.cost_usd = None;
        }
        if let Some(value) = update.hard_wall_clock_secs {
            caps.wall_clock_secs = Some(value);
        }
        if update.clear_hard_wall_clock_cap {
            caps.wall_clock_secs = None;
        }
        caps.validate()
            .map_err(|error| anyhow!("invalid hard budget cap: {error}"))?;
        if effective_mode == BudgetMode::Auto && caps.is_empty() {
            return Err(BudgetValidationError::new(
                auto_budget_codes::AUTO_BUDGET_CAP_REQUIRED,
                "auto budget requires at least one hard token, cost, or wall-clock cap",
            )
            .into());
        }
        ensure_caps_cover_spend(&caps, &current)?;

        let mut token_budget = if update.clear_token_budget {
            None
        } else {
            update.token_budget.or(current.token_budget)
        };
        let mut cost_budget_usd = if update.clear_cost_budget {
            None
        } else {
            update.cost_budget_usd.or(current.cost_budget_usd)
        };
        let mut time_budget_secs = if update.clear_time_budget {
            None
        } else {
            update.time_budget_secs.or(current.time_budget_secs)
        };
        if effective_mode == BudgetMode::Auto {
            // Auto starts at the policy's initial value, falling back to the
            // hard cap for dimensions where no separate initial was declared.
            token_budget = token_budget
                .or(policy.resolved_initial_token_budget())
                .or(caps.token);
            cost_budget_usd = cost_budget_usd
                .or(policy.resolved_initial_cost_budget_usd())
                .or(caps.cost_usd);
            time_budget_secs = time_budget_secs
                .or(policy.resolved_initial_wall_clock_secs())
                .or(caps.wall_clock_secs);
            if let (Some(value), Some(cap)) = (token_budget, caps.token) {
                token_budget = Some(value.min(cap));
            }
            if let (Some(value), Some(cap)) = (cost_budget_usd, caps.cost_usd) {
                cost_budget_usd = Some(value.min(cap));
            }
            if let (Some(value), Some(cap)) = (time_budget_secs, caps.wall_clock_secs) {
                time_budget_secs = Some(value.min(cap));
            }
        }
        validate_effective_budgets(token_budget, cost_budget_usd, time_budget_secs, &current)?;

        // Keep the policy copy internally consistent with explicit overrides.
        policy.hard_token_cap = caps.token;
        policy.hard_cost_cap_usd = caps.cost_usd;
        policy.hard_wall_clock_secs = caps.wall_clock_secs;
        if effective_mode == BudgetMode::Auto && token_budget.is_some() {
            policy.initial_token_budget = token_budget;
        }
        if effective_mode == BudgetMode::Auto && cost_budget_usd.is_some() {
            policy.initial_cost_budget_usd = cost_budget_usd;
        }
        if effective_mode == BudgetMode::Auto && time_budget_secs.is_some() {
            policy.initial_wall_clock_secs = time_budget_secs;
        }

        tx.execute(
            "UPDATE sessions
             SET token_budget = ?1, cost_budget_usd = ?2, time_budget_secs = ?3,
                 unlimited = ?4
             WHERE id = ?5",
            params![
                token_budget,
                cost_budget_usd,
                time_budget_secs,
                if unlimited { 1 } else { 0 },
                session_id
            ],
        )?;
        let now = now_rfc3339();
        let state = existing
            .as_ref()
            .map(|row| row.state.clone())
            .unwrap_or_default();
        let created_at = existing
            .as_ref()
            .map(|row| row.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let record = SessionAutoBudgetRecord {
            session_id: session_id.to_string(),
            mode: stored_mode,
            policy,
            state,
            caps,
            created_at,
            updated_at: now.clone(),
        };
        upsert_session_auto_budget(&tx, &record)?;
        let event = insert_event_at_tx(
            &tx,
            session_id,
            SESSION_BUDGET_UPDATED_EVENT,
            serde_json::json!({
                "mode": record.mode,
                "unlimited": unlimited,
                "token_budget": token_budget,
                "cost_budget_usd": cost_budget_usd,
                "time_budget_secs": time_budget_secs,
                "hard_token_cap": record.caps.token,
                "hard_cost_cap_usd": record.caps.cost_usd,
                "hard_wall_clock_secs": record.caps.wall_clock_secs,
                "policy": record.policy,
                "state": record.state,
                "updated_at": now,
            }),
            &now,
        )?;
        tx.commit()?;
        let mut updated = current;
        updated.token_budget = token_budget;
        updated.cost_budget_usd = cost_budget_usd;
        updated.time_budget_secs = time_budget_secs;
        updated.unlimited = unlimited;
        Ok((updated, record, event))
    }

    /// Read/write operational auto state. Forecast snapshots and turn counters
    /// are not lifecycle decisions, so this does not append an event.
    pub fn update_session_auto_budget_state(
        &self,
        session_id: &str,
        state: &SessionAutoBudgetState,
    ) -> Result<SessionAutoBudgetRecord> {
        self.update_session_auto_budget_state_with_progress_event(session_id, state)
            .map(|(record, _)| record)
    }

    /// Operational state write that appends one `session.auto_state` event
    /// only when the opaque `progress` payload changed. Forecast-only writes
    /// stay event-free, while no-progress state survives rebuild/replay.
    pub fn update_session_auto_budget_state_with_progress_event(
        &self,
        session_id: &str,
        state: &SessionAutoBudgetState,
    ) -> Result<(SessionAutoBudgetRecord, Option<Event>)> {
        state
            .validate()
            .map_err(|error| anyhow!("invalid session auto-budget state: {error}"))?;
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = query_session_auto_budget(&tx, session_id)?
            .ok_or_else(|| anyhow!("session has no auto-budget row: {session_id}"))?;
        let progress_changed = existing.state.progress != state.progress;
        let changed = tx.execute(
            "UPDATE session_auto_budgets SET state_json = ?1, updated_at = ?2
             WHERE session_id = ?3",
            params![serde_json::to_string(state)?, now, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session has no auto-budget row: {session_id}"));
        }
        let event = if progress_changed {
            Some(insert_event_at_tx(
                &tx,
                session_id,
                SESSION_AUTO_STATE_EVENT,
                serde_json::json!({"state": state, "updated_at": now}),
                &now,
            )?)
        } else {
            None
        };
        tx.commit()?;
        if let Some(event) = &event
            && let Err(error) = self.append_event_mirror(event)
        {
            tracing::warn!(%error, "failed to append event mirror");
        }
        let record = query_session_auto_budget(&conn, session_id)?
            .ok_or_else(|| anyhow!("session auto-budget row disappeared: {session_id}"))?;
        Ok((record, event))
    }

    /// Write lifecycle state and append a bounded pause/resume event in one
    /// transaction. Auto-extension and no-progress policy decisions remain in
    /// the runtime layer.
    pub fn update_session_auto_budget_state_with_event(
        &self,
        session_id: &str,
        state: &SessionAutoBudgetState,
        event_kind: &str,
        payload: serde_json::Value,
    ) -> Result<(SessionAutoBudgetRecord, Event)> {
        if !matches!(
            event_kind,
            SESSION_AUTO_PAUSED_EVENT | SESSION_AUTO_RESUMED_EVENT
        ) {
            return Err(anyhow!(
                "unsupported auto-budget lifecycle event: {event_kind}"
            ));
        }
        state
            .validate()
            .map_err(|error| anyhow!("invalid session auto-budget state: {error}"))?;
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_auto_budgets SET state_json = ?1, updated_at = ?2
             WHERE session_id = ?3",
            params![serde_json::to_string(state)?, now, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session has no auto-budget row: {session_id}"));
        }
        let event = insert_event_at_tx(&tx, session_id, event_kind, payload, &now)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        let record = query_session_auto_budget(&conn, session_id)?
            .ok_or_else(|| anyhow!("session auto-budget row disappeared: {session_id}"))?;
        Ok((record, event))
    }

    /// Explicit snapshot restore used by bundle import and low-level rebuild.
    pub fn insert_session_auto_budget_snapshot(
        &self,
        record: &SessionAutoBudgetRecord,
    ) -> Result<()> {
        validate_auto_budget_record(record)?;
        let conn = self.lock()?;
        upsert_session_auto_budget(&conn, record)
    }

    /// Atomically extend an auto session's effective budgets under one
    /// `BEGIN IMMEDIATE` transaction. Parallel Workers therefore observe a
    /// serialized old/new pair and can never sum past the hard cap.
    pub fn extend_session_auto_budget_with_event(
        &self,
        session_id: &str,
        extension: &AutoBudgetExtension,
    ) -> Result<SessionAutoBudgetExtension> {
        validate_extension_request(extension)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Session = tx
            .query_row(
                "SELECT id, project_id, goal, status, worktree, branch, token_budget,
                        cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd,
                        base_commit, tokens_in, tokens_out, created_at, priority, prompt_layout
                 FROM sessions WHERE id = ?1",
                params![session_id],
                map_session,
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if current.status == "imported" {
            return Err(anyhow!("imported sessions are read-only archives"));
        }
        if current.unlimited {
            return Err(BudgetValidationError::new(
                auto_budget_codes::INVALID_BUDGET_MODE,
                "unlimited sessions never auto-extend",
            )
            .into());
        }
        let existing = query_session_auto_budget(&tx, session_id)?.ok_or_else(|| {
            BudgetValidationError::new(
                auto_budget_codes::INVALID_BUDGET_MODE,
                "session has no auto-budget row and is not in auto mode",
            )
        })?;
        if existing.mode != BudgetMode::Auto {
            return Err(BudgetValidationError::new(
                auto_budget_codes::INVALID_BUDGET_MODE,
                format!(
                    "session budget mode is {}, not auto",
                    existing.mode.as_str()
                ),
            )
            .into());
        }
        let caps = existing.caps.clone();
        if caps.is_empty() {
            return Err(BudgetValidationError::new(
                auto_budget_codes::AUTO_BUDGET_CAP_REQUIRED,
                "auto session has no hard cap",
            )
            .into());
        }
        let policy = &existing.policy;
        let spent_tokens = current.tokens_in.saturating_add(current.tokens_out);
        let elapsed_secs = existing.state.elapsed_secs;
        let initial_token = policy
            .initial_token_budget
            .or(current.token_budget)
            .or(caps.token);
        let initial_cost = policy
            .initial_cost_budget_usd
            .or(current.cost_budget_usd)
            .or(caps.cost_usd);
        let initial_time = policy
            .initial_wall_clock_secs
            .or(current.time_budget_secs)
            .or(caps.wall_clock_secs);

        // A targeted request (any dimension specified) extends only those
        // dimensions. A fully default request keeps the operator/legacy
        // "extend every capped dimension by one policy step" behavior.
        let targeted = extension.requested_token_extension.is_some()
            || extension.requested_cost_extension_usd.is_some()
            || extension.requested_wall_clock_extension_secs.is_some();
        let token_request = if targeted {
            extension.requested_token_extension
        } else {
            caps.token.map(|_| 0)
        };
        let cost_request = if targeted {
            extension.requested_cost_extension_usd
        } else {
            caps.cost_usd.map(|_| 0.0)
        };
        let time_request = if targeted {
            extension.requested_wall_clock_extension_secs
        } else {
            caps.wall_clock_secs.map(|_| 0)
        };

        let (new_token, token_changed) = extend_u64(
            current.token_budget,
            caps.token,
            initial_token,
            policy.extend_ratio,
            policy.max_multiplier,
            token_request,
            spent_tokens,
        );
        let (new_cost, cost_changed) = extend_f64(
            current.cost_budget_usd,
            caps.cost_usd,
            initial_cost,
            policy.extend_ratio,
            policy.max_multiplier,
            cost_request,
            current.cost_spent_usd,
        );
        let (new_time, time_changed) = extend_u64(
            current.time_budget_secs,
            caps.wall_clock_secs,
            initial_time,
            policy.extend_ratio,
            policy.max_multiplier,
            time_request,
            elapsed_secs,
        );
        let changed = token_changed || cost_changed || time_changed;
        let reason = extension
            .reason
            .clone()
            .unwrap_or_else(|| "auto_budget_extension".to_string());
        if !changed {
            drop(tx);
            return Ok(SessionAutoBudgetExtension {
                outcome: AutoBudgetExtensionOutcome::CapReached,
                old_token_budget: current.token_budget,
                old_cost_budget_usd: current.cost_budget_usd,
                old_time_budget_secs: current.time_budget_secs,
                new_token_budget: current.token_budget,
                new_cost_budget_usd: current.cost_budget_usd,
                new_time_budget_secs: current.time_budget_secs,
                reason,
                event: None,
                session: current,
                record: existing,
            });
        }

        let old_token_budget = current.token_budget;
        let old_cost_budget_usd = current.cost_budget_usd;
        let old_time_budget_secs = current.time_budget_secs;
        let now = now_rfc3339();
        tx.execute(
            "UPDATE sessions
             SET token_budget = ?1, cost_budget_usd = ?2, time_budget_secs = ?3
             WHERE id = ?4",
            params![new_token, new_cost, new_time, session_id],
        )?;
        let mut record = existing;
        record.state.extensions = record.state.extensions.saturating_add(1);
        record.state.last_extension_at = Some(now.clone());
        if let Some(forecast) = &extension.forecast {
            record.state.last_forecast = Some(forecast.clone());
        }
        record.updated_at = now.clone();
        validate_auto_budget_record(&record)?;
        upsert_session_auto_budget(&tx, &record)?;
        let event = insert_event_at_tx(
            &tx,
            session_id,
            vp_protocol::types::SESSION_BUDGET_AUTO_EXTENDED_EVENT,
            serde_json::json!({
                "extensions": record.state.extensions,
                "reason": reason,
                "forecast": extension.forecast,
                "old": {
                    "token_budget": current.token_budget,
                    "cost_budget_usd": current.cost_budget_usd,
                    "time_budget_secs": current.time_budget_secs,
                },
                "new": {
                    "token_budget": new_token,
                    "cost_budget_usd": new_cost,
                    "time_budget_secs": new_time,
                },
                "hard": {
                    "token": record.caps.token,
                    "cost_usd": record.caps.cost_usd,
                    "wall_clock_secs": record.caps.wall_clock_secs,
                },
                "state": record.state,
                "updated_at": now,
            }),
            &now,
        )?;
        tx.commit()?;
        let mut session = current;
        session.token_budget = new_token;
        session.cost_budget_usd = new_cost;
        session.time_budget_secs = new_time;
        Ok(SessionAutoBudgetExtension {
            outcome: AutoBudgetExtensionOutcome::Extended,
            old_token_budget,
            old_cost_budget_usd,
            old_time_budget_secs,
            new_token_budget: new_token,
            new_cost_budget_usd: new_cost,
            new_time_budget_secs: new_time,
            reason,
            event: Some(event),
            session,
            record,
        })
    }
}

/// One serialized auto-extension outcome. The engine attaches the computed
/// status; the store deliberately knows nothing about usage forecasting.
#[derive(Debug, Clone)]
pub struct SessionAutoBudgetExtension {
    pub outcome: AutoBudgetExtensionOutcome,
    pub old_token_budget: Option<u64>,
    pub old_cost_budget_usd: Option<f64>,
    pub old_time_budget_secs: Option<u64>,
    pub new_token_budget: Option<u64>,
    pub new_cost_budget_usd: Option<f64>,
    pub new_time_budget_secs: Option<u64>,
    pub reason: String,
    pub event: Option<Event>,
    pub session: Session,
    pub record: SessionAutoBudgetRecord,
}

fn validate_extension_request(extension: &AutoBudgetExtension) -> Result<()> {
    if extension.requested_token_extension == Some(0) {
        return Err(BudgetValidationError::new(
            auto_budget_codes::INVALID_BUDGET,
            "requested token extension must be greater than zero",
        )
        .into());
    }
    if let Some(value) = extension.requested_cost_extension_usd
        && (!value.is_finite() || value <= 0.0)
    {
        return Err(BudgetValidationError::new(
            auto_budget_codes::INVALID_BUDGET,
            "requested cost extension must be a positive finite USD value",
        )
        .into());
    }
    if extension.requested_wall_clock_extension_secs == Some(0) {
        return Err(BudgetValidationError::new(
            auto_budget_codes::INVALID_BUDGET,
            "requested wall-clock extension must be greater than zero",
        )
        .into());
    }
    Ok(())
}

fn extension_ceiling(cap: Option<u64>, initial: Option<u64>, max_multiplier: f64) -> Option<u64> {
    let cap = cap?;
    let initial = initial.unwrap_or(cap);
    let multiplied = (initial as f64 * max_multiplier).floor();
    if multiplied.is_finite() && multiplied >= 0.0 {
        Some(cap.min(multiplied.min(u64::MAX as f64) as u64))
    } else {
        Some(cap)
    }
}

fn extend_u64(
    current: Option<u64>,
    hard_cap: Option<u64>,
    initial: Option<u64>,
    extend_ratio: f64,
    max_multiplier: f64,
    requested: Option<u64>,
    floor: u64,
) -> (Option<u64>, bool) {
    let Some(current) = current else {
        return (current, false);
    };
    // Only extend a dimension the caller actually asked for. A time-only
    // extension must not silently raise an unrelated token/cost budget just
    // because that dimension also has a hard cap.
    let Some(requested) = requested else {
        return (Some(current), false);
    };
    let Some(ceiling) = extension_ceiling(hard_cap, initial, max_multiplier) else {
        return (Some(current), false);
    };
    if current >= ceiling || floor >= ceiling {
        return (Some(current), false);
    }
    let baseline = initial.unwrap_or(current);
    let step = ((baseline as f64 * extend_ratio).ceil() as u64).max(1);
    let delta = step.max(requested);
    let target = current
        .saturating_add(delta)
        .min(ceiling)
        .max(floor.min(ceiling));
    if target > current {
        (Some(target), true)
    } else {
        (Some(current), false)
    }
}

fn extend_f64(
    current: Option<f64>,
    hard_cap: Option<f64>,
    initial: Option<f64>,
    extend_ratio: f64,
    max_multiplier: f64,
    requested: Option<f64>,
    floor: f64,
) -> (Option<f64>, bool) {
    let Some(current) = current else {
        return (current, false);
    };
    let Some(requested) = requested else {
        return (Some(current), false);
    };
    let Some(cap) = hard_cap else {
        return (Some(current), false);
    };
    let initial = initial.unwrap_or(cap);
    let multiplied = initial * max_multiplier;
    let ceiling = if multiplied.is_finite() && multiplied >= 0.0 {
        cap.min(multiplied)
    } else {
        cap
    };
    if current >= ceiling || floor >= ceiling {
        return (Some(current), false);
    }
    let step = (initial * extend_ratio).max(f64::EPSILON);
    let delta = step.max(requested);
    let target = (current + delta).min(ceiling).max(floor.min(ceiling));
    if target > current {
        (Some(target), true)
    } else {
        (Some(current), false)
    }
}

fn require_project_tx(tx: &Transaction<'_>, project_id: &str) -> Result<()> {
    let found: Option<String> = tx
        .query_row(
            "SELECT id FROM projects WHERE id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()?;
    if found.is_none() {
        return Err(anyhow!("project not found: {project_id}"));
    }
    Ok(())
}

fn query_project_auto_budget_policy(
    conn: &Connection,
    project_id: &str,
) -> Result<Option<AutoBudgetPolicy>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT policy_json FROM project_auto_budget_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()?;
    match stored {
        Some(raw) => {
            let policy: AutoBudgetPolicy =
                serde_json::from_str(&raw).context("parsing stored project auto-budget policy")?;
            policy.validate().map_err(|error| {
                anyhow!("stored project auto-budget policy is invalid: {error}")
            })?;
            Ok(Some(policy))
        }
        None => Ok(None),
    }
}

fn resolve_stored_mode(
    update: &SessionBudgetUpdate,
    current: &Session,
    existing: Option<&SessionAutoBudgetRecord>,
) -> Result<BudgetMode> {
    if let (Some(mode), Some(unlimited)) = (update.mode, update.unlimited)
        && (mode == BudgetMode::Unlimited) != unlimited
    {
        return Err(BudgetValidationError::new(
            auto_budget_codes::BUDGET_MODE_CONFLICT,
            "mode and unlimited flags disagree",
        )
        .into());
    }
    if let Some(mode) = update.mode {
        return Ok(mode);
    }
    if let Some(unlimited) = update.unlimited {
        if unlimited {
            return Ok(existing
                .map(|row| row.mode)
                .filter(|mode| *mode != BudgetMode::Unlimited)
                .unwrap_or(if current.unlimited {
                    BudgetMode::Unlimited
                } else {
                    BudgetMode::Fixed
                }));
        }
        return Ok(existing
            .map(|row| row.mode)
            .filter(|mode| *mode != BudgetMode::Unlimited)
            .unwrap_or(BudgetMode::Fixed));
    }
    if let Some(row) = existing {
        return Ok(row.mode);
    }
    Ok(if current.unlimited {
        BudgetMode::Unlimited
    } else {
        BudgetMode::Fixed
    })
}

fn resolve_unlimited(
    update: &SessionBudgetUpdate,
    current: &Session,
    stored_mode: BudgetMode,
) -> Result<bool> {
    if stored_mode == BudgetMode::Unlimited {
        return Ok(true);
    }
    match update.unlimited {
        Some(value) => Ok(value),
        None => match update.mode {
            Some(BudgetMode::Auto | BudgetMode::Fixed) => Ok(false),
            Some(BudgetMode::Unlimited) => Ok(true),
            None => Ok(current.unlimited),
        },
    }
}

fn ensure_caps_cover_spend(caps: &AutoBudgetCaps, current: &Session) -> Result<()> {
    let spent_tokens = current.tokens_in.saturating_add(current.tokens_out);
    if let Some(cap) = caps.token
        && cap < spent_tokens
    {
        return Err(BudgetValidationError::new(
            auto_budget_codes::BUDGET_BELOW_SPENT,
            format!("hard token cap {cap} is below tokens already spent ({spent_tokens})"),
        )
        .into());
    }
    if let Some(cap) = caps.cost_usd
        && cap + 1e-9 < current.cost_spent_usd
    {
        return Err(BudgetValidationError::new(
            auto_budget_codes::BUDGET_BELOW_SPENT,
            format!(
                "hard cost cap {cap:.6} is below cost already spent ({:.6})",
                current.cost_spent_usd
            ),
        )
        .into());
    }
    Ok(())
}

fn validate_effective_budgets(
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
    time_budget_secs: Option<u64>,
    current: &Session,
) -> Result<()> {
    if token_budget == Some(0) {
        return Err(BudgetValidationError::new(
            auto_budget_codes::INVALID_BUDGET,
            "token budget must be greater than zero",
        )
        .into());
    }
    if let Some(value) = cost_budget_usd
        && (!value.is_finite() || value <= 0.0)
    {
        return Err(BudgetValidationError::new(
            auto_budget_codes::INVALID_BUDGET,
            "cost budget must be a positive finite USD value",
        )
        .into());
    }
    if time_budget_secs == Some(0) {
        return Err(BudgetValidationError::new(
            auto_budget_codes::INVALID_BUDGET,
            "time budget must be greater than zero",
        )
        .into());
    }
    let spent_tokens = current.tokens_in.saturating_add(current.tokens_out);
    if let Some(value) = token_budget
        && value < spent_tokens
    {
        return Err(BudgetValidationError::new(
            auto_budget_codes::BUDGET_BELOW_SPENT,
            format!("token budget {value} is below tokens already spent ({spent_tokens})"),
        )
        .into());
    }
    if let Some(value) = cost_budget_usd
        && value + 1e-9 < current.cost_spent_usd
    {
        return Err(BudgetValidationError::new(
            auto_budget_codes::BUDGET_BELOW_SPENT,
            format!(
                "cost budget {value:.6} is below cost already spent ({:.6})",
                current.cost_spent_usd
            ),
        )
        .into());
    }
    Ok(())
}

fn query_session_auto_budget(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<SessionAutoBudgetRecord>> {
    let stored: Option<(String, String, String, String, String, String)> = conn
        .query_row(
            "SELECT mode, policy_json, state_json, caps_json, created_at, updated_at
             FROM session_auto_budgets WHERE session_id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    match stored {
        Some((mode, policy, state, caps, created_at, updated_at)) => {
            let state: SessionAutoBudgetState =
                serde_json::from_str(&state).context("parsing stored session auto-budget state")?;
            state
                .validate()
                .map_err(|error| anyhow!("stored session auto-budget state is invalid: {error}"))?;
            let record = SessionAutoBudgetRecord {
                session_id: session_id.to_string(),
                mode: BudgetMode::parse(&mode)?,
                policy: serde_json::from_str(&policy)
                    .context("parsing stored session auto-budget policy")?,
                state,
                caps: serde_json::from_str(&caps)
                    .context("parsing stored session auto-budget caps")?,
                created_at,
                updated_at,
            };
            record.policy.validate().map_err(|error| {
                anyhow!("stored session auto-budget policy is invalid: {error}")
            })?;
            record
                .caps
                .validate()
                .map_err(|error| anyhow!("stored session auto-budget caps are invalid: {error}"))?;
            Ok(Some(record))
        }
        None => Ok(None),
    }
}

fn upsert_session_auto_budget(conn: &Connection, record: &SessionAutoBudgetRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO session_auto_budgets
         (session_id, mode, policy_json, state_json, caps_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(session_id) DO UPDATE SET
            mode = excluded.mode,
            policy_json = excluded.policy_json,
            state_json = excluded.state_json,
            caps_json = excluded.caps_json,
            updated_at = excluded.updated_at",
        params![
            record.session_id,
            record.mode.as_str(),
            serde_json::to_string(&record.policy)?,
            serde_json::to_string(&record.state)?,
            serde_json::to_string(&record.caps)?,
            record.created_at,
            record.updated_at
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_session_auto_budget_tx(
    tx: &Transaction<'_>,
    record: &SessionAutoBudgetRecord,
) -> Result<()> {
    validate_auto_budget_record(record)?;
    upsert_session_auto_budget(tx, record)
}

fn validate_auto_budget_record(
    record: &SessionAutoBudgetRecord,
) -> std::result::Result<(), BudgetValidationError> {
    record.state.validate().map_err(|error| {
        BudgetValidationError::new(
            error.code,
            format!(
                "session auto-budget state for {} is invalid: {}",
                record.session_id, error.message
            ),
        )
    })?;
    record.policy.validate().map_err(|error| {
        BudgetValidationError::new(
            error.code,
            format!(
                "session auto-budget policy for {} is invalid: {}",
                record.session_id, error.message
            ),
        )
    })?;
    record.caps.validate().map_err(|error| {
        BudgetValidationError::new(
            error.code,
            format!(
                "session auto-budget caps for {} are invalid: {}",
                record.session_id, error.message
            ),
        )
    })?;
    if record.mode == BudgetMode::Auto && record.caps.is_empty() {
        return Err(BudgetValidationError::new(
            auto_budget_codes::AUTO_BUDGET_CAP_REQUIRED,
            format!("session {} is auto but has no hard cap", record.session_id),
        ));
    }
    Ok(())
}

fn insert_project_auto_budget_event_tx(
    tx: &Transaction<'_>,
    event: &ProjectAutoBudgetEvent,
) -> Result<()> {
    tx.execute(
        "INSERT INTO project_auto_budget_events
         (id, project_id, seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.kind,
            serde_json::to_string(&event.payload)?,
            event.created_at
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Store, Project) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("auto-budget"))
            .unwrap();
        (data, store, project)
    }

    fn policy_with_caps() -> AutoBudgetPolicy {
        AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000),
            initial_cost_budget_usd: Some(2.5),
            hard_token_cap: Some(5_000),
            hard_cost_cap_usd: Some(2.5),
            ..AutoBudgetPolicy::default()
        }
    }

    #[test]
    fn project_policy_round_trips_with_audit_event() {
        let (_data, store, project) = fixture();
        let policy = policy_with_caps();
        let (stored, event) = store
            .set_project_auto_budget_policy_with_event(&project.id, &policy)
            .unwrap();
        assert_eq!(stored, policy);
        assert_eq!(event.kind, "project.auto_budget.updated");
        assert_eq!(
            store.get_project_auto_budget_policy(&project.id).unwrap(),
            policy
        );
        let events = store
            .list_project_auto_budget_events(&project.id, None)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 1);
    }

    #[test]
    fn auto_session_creation_inserts_policy_and_event_atomically() {
        let (_data, store, project) = fixture();
        let session_id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let record = SessionAutoBudgetRecord {
            session_id: session_id.clone(),
            mode: BudgetMode::Auto,
            policy: policy_with_caps(),
            state: SessionAutoBudgetState::default(),
            caps: policy_with_caps().hard_caps(),
            created_at: now.clone(),
            updated_at: now,
        };
        let (session, event) = store
            .create_session_with_worktree_and_event_budget(
                &session_id,
                &project.id,
                "auto session",
                "/tmp/auto-session",
                "verlinpark/auto",
                Some(1_000),
                Some(2.5),
                None,
                false,
                "deadbeef",
                0,
                BudgetMode::Auto,
                Some(&record),
                None,
            )
            .unwrap();
        assert_eq!(session.id, session_id);
        assert_eq!(event.payload["budget_mode"], "auto");
        assert_eq!(
            event.payload["auto_budget"]["mode"],
            serde_json::json!("auto")
        );
        let stored = store.get_session_auto_budget(&session_id).unwrap().unwrap();
        assert_eq!(stored.mode, BudgetMode::Auto);
        assert_eq!(stored.caps.token, Some(5_000));
    }

    #[test]
    fn budget_set_rejects_below_spend_and_enforces_auto_caps() {
        let (_data, store, project) = fixture();
        let session = store.create_session(&project.id, "budget set").unwrap();
        store
            .add_session_usage_with_event(&session.id, 100, 50, false)
            .unwrap();
        let error = store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    token_budget: Some(100),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap_err();
        let typed = error.downcast_ref::<BudgetValidationError>().unwrap();
        assert_eq!(typed.code, "BUDGET_BELOW_SPENT");

        let error = store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Auto),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap_err();
        let typed = error.downcast_ref::<BudgetValidationError>().unwrap();
        assert_eq!(typed.code, "AUTO_BUDGET_CAP_REQUIRED");

        let (updated, record, event) = store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Auto),
                    policy: Some(policy_with_caps()),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap();
        assert!(!updated.unlimited);
        assert_eq!(record.mode, BudgetMode::Auto);
        assert_eq!(updated.token_budget, Some(1_000));
        assert_eq!(updated.cost_budget_usd, Some(2.5));
        assert_eq!(event.kind, "session.budget.updated");
    }

    #[test]
    fn auto_budget_tables_survive_reopen() {
        let (data, store, project) = fixture();
        let policy = policy_with_caps();
        store
            .set_project_auto_budget_policy_snapshot(&project.id, &policy)
            .unwrap();
        let session = store.create_session(&project.id, "reopen").unwrap();
        store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Auto),
                    policy: Some(policy.clone()),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap();
        drop(store);
        let reopened = Store::open(data.path()).unwrap();
        assert_eq!(
            reopened
                .get_project_auto_budget_policy(&project.id)
                .unwrap(),
            policy
        );
        let record = reopened
            .get_session_auto_budget(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(record.mode, BudgetMode::Auto);
        assert_eq!(record.caps.token, Some(5_000));
        assert_eq!(
            reopened
                .get_session(&session.id)
                .unwrap()
                .unwrap()
                .token_budget,
            Some(1_000)
        );
    }

    #[test]
    fn recent_usage_query_reads_only_the_requested_window_from_a_large_ledger() {
        let (_data, store, project) = fixture();
        let session = store.create_session(&project.id, "bounded usage").unwrap();
        let entries = (0..10_000u64)
            .map(|index| UsageEntry {
                id: format!("usage-{index:05}"),
                session_id: session.id.clone(),
                agent_id: "agent".into(),
                agent_name: "Agent".into(),
                role: "leader".into(),
                task_id: None,
                department: None,
                model: "test".into(),
                input_tokens: index,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                estimated: false,
                cost_usd: 0.001,
                created_at: "2026-11-21T00:00:00Z".into(),
            })
            .collect::<Vec<_>>();
        store.replace_usage_ledger(&session.id, &entries).unwrap();

        let recent = store.list_recent_usage_entries(&session.id, 7).unwrap();
        assert_eq!(recent.len(), 7);
        assert_eq!(recent.first().unwrap().id, "usage-09993");
        assert_eq!(recent.last().unwrap().id, "usage-09999");
        assert!(recent.windows(2).all(|window| window[0].id < window[1].id));

        let clamped = store
            .list_recent_usage_entries(&session.id, 10_000)
            .unwrap();
        assert_eq!(clamped.len(), 1_000);
        assert_eq!(clamped.first().unwrap().id, "usage-09000");
        assert_eq!(clamped.last().unwrap().id, "usage-09999");
        // The full-ledger reader is unchanged for export/usage totals.
        assert_eq!(store.list_usage_entries(&session.id).unwrap().len(), 10_000);
    }

    #[test]
    fn auto_extension_clamps_to_hard_cap_and_preserves_progress_state() {
        let (_data, store, project) = fixture();
        let session = store.create_session(&project.id, "extend").unwrap();
        let policy = AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000),
            hard_token_cap: Some(1_500),
            ..AutoBudgetPolicy::default()
        };
        store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Auto),
                    policy: Some(policy),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap();
        let state = SessionAutoBudgetState {
            progress: Some(serde_json::json!({"turn": 3, "progress": true})),
            ..SessionAutoBudgetState::default()
        };
        store
            .update_session_auto_budget_state(&session.id, &state)
            .unwrap();

        let first = store
            .extend_session_auto_budget_with_event(&session.id, &AutoBudgetExtension::default())
            .unwrap();
        assert_eq!(first.outcome, AutoBudgetExtensionOutcome::Extended);
        assert_eq!(first.old_token_budget, Some(1_000));
        assert_eq!(first.new_token_budget, Some(1_250));
        assert!(first.event.is_some());

        let second = store
            .extend_session_auto_budget_with_event(
                &session.id,
                &AutoBudgetExtension {
                    requested_token_extension: Some(10_000),
                    reason: Some("parallel worker runway".into()),
                    ..AutoBudgetExtension::default()
                },
            )
            .unwrap();
        assert_eq!(second.outcome, AutoBudgetExtensionOutcome::Extended);
        assert_eq!(second.old_token_budget, Some(1_250));
        assert_eq!(second.new_token_budget, Some(1_500));

        let third = store
            .extend_session_auto_budget_with_event(&session.id, &AutoBudgetExtension::default())
            .unwrap();
        assert_eq!(third.outcome, AutoBudgetExtensionOutcome::CapReached);
        assert!(third.event.is_none());
        assert_eq!(third.new_token_budget, Some(1_500));

        let record = store.get_session_auto_budget(&session.id).unwrap().unwrap();
        assert_eq!(record.state.extensions, 2);
        assert!(record.state.last_extension_at.is_some());
        assert_eq!(
            record.state.progress,
            Some(serde_json::json!({"turn": 3, "progress": true}))
        );
        assert_eq!(
            store
                .get_session(&session.id)
                .unwrap()
                .unwrap()
                .token_budget,
            Some(1_500)
        );
        let events = store
            .list_all_events(&session.id)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "session.budget.auto_extended")
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].payload["old"]["token_budget"], 1_000);
        assert_eq!(events[1].payload["new"]["token_budget"], 1_500);
    }

    #[test]
    fn targeted_time_extension_does_not_raise_token_or_cost_budgets() {
        let (_data, store, project) = fixture();
        let session = store.create_session(&project.id, "time only").unwrap();
        let policy = AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000_000),
            hard_token_cap: Some(2_000_000),
            initial_cost_budget_usd: Some(2.0),
            hard_cost_cap_usd: Some(8.0),
            initial_wall_clock_secs: Some(60),
            hard_wall_clock_secs: Some(600),
            ..AutoBudgetPolicy::default()
        };
        store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Auto),
                    policy: Some(policy),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap();

        let result = store
            .extend_session_auto_budget_with_event(
                &session.id,
                &AutoBudgetExtension {
                    requested_wall_clock_extension_secs: Some(30),
                    reason: Some("time runway".into()),
                    ..AutoBudgetExtension::default()
                },
            )
            .unwrap();
        assert_eq!(result.outcome, AutoBudgetExtensionOutcome::Extended);
        assert_eq!(result.old_token_budget, Some(1_000_000));
        assert_eq!(result.new_token_budget, Some(1_000_000));
        assert_eq!(result.old_cost_budget_usd, Some(2.0));
        assert_eq!(result.new_cost_budget_usd, Some(2.0));
        assert_eq!(result.old_time_budget_secs, Some(60));
        assert_eq!(result.new_time_budget_secs, Some(90));
    }

    #[test]
    fn concurrent_auto_extensions_cannot_exceed_the_hard_cap() {
        let (data, store, project) = fixture();
        let session = store
            .create_session(&project.id, "concurrent extend")
            .unwrap();
        let policy = AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000),
            hard_token_cap: Some(1_500),
            ..AutoBudgetPolicy::default()
        };
        store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Auto),
                    policy: Some(policy),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap();
        let path = data.path().to_path_buf();
        let session_id = session.id.clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let session_id = session_id.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let store = Store::open(path).unwrap();
                barrier.wait();
                store
                    .extend_session_auto_budget_with_event(
                        &session_id,
                        &AutoBudgetExtension::default(),
                    )
                    .unwrap()
            }));
        }
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.outcome == AutoBudgetExtensionOutcome::Extended)
                .count(),
            2
        );
        let final_session = store.get_session(&session.id).unwrap().unwrap();
        assert_eq!(final_session.token_budget, Some(1_500));
        let events = store
            .list_all_events(&session.id)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "session.budget.auto_extended")
            .count();
        assert_eq!(events, 2);
    }

    #[test]
    fn oversized_auto_state_is_rejected_before_it_is_persisted() {
        let (_data, store, project) = fixture();
        let session = store.create_session(&project.id, "bounded state").unwrap();
        let now = now_rfc3339();
        let record = SessionAutoBudgetRecord {
            session_id: session.id.clone(),
            mode: BudgetMode::Auto,
            policy: policy_with_caps(),
            state: SessionAutoBudgetState {
                progress: Some(serde_json::json!({"blob": "x".repeat(70_000)})),
                ..SessionAutoBudgetState::default()
            },
            caps: policy_with_caps().hard_caps(),
            created_at: now.clone(),
            updated_at: now,
        };
        let error = store
            .insert_session_auto_budget_snapshot(&record)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<BudgetValidationError>().unwrap().code,
            "INVALID_AUTO_BUDGET_STATE"
        );
        assert!(
            store
                .get_session_auto_budget(&session.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn extension_rejects_fixed_and_unlimited_modes() {
        let (_data, store, project) = fixture();
        let fixed = store.create_session(&project.id, "fixed").unwrap();
        let error = store
            .extend_session_auto_budget_with_event(&fixed.id, &AutoBudgetExtension::default())
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<BudgetValidationError>().unwrap().code,
            "INVALID_BUDGET_MODE"
        );

        let unlimited = store.create_session(&project.id, "unlimited").unwrap();
        store
            .set_session_unlimited_with_event(&unlimited.id, true)
            .unwrap();
        let error = store
            .extend_session_auto_budget_with_event(&unlimited.id, &AutoBudgetExtension::default())
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<BudgetValidationError>().unwrap().code,
            "INVALID_BUDGET_MODE"
        );
    }

    #[test]
    fn fixed_auto_unlimited_transitions_are_event_backed() {
        let (_data, store, project) = fixture();
        let session = store.create_session(&project.id, "transitions").unwrap();
        let (_, auto, _) = store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Auto),
                    policy: Some(policy_with_caps()),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap();
        assert_eq!(auto.mode, BudgetMode::Auto);

        let (unlimited, record, _) = store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Unlimited),
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap();
        assert!(unlimited.unlimited);
        assert_eq!(record.mode, BudgetMode::Unlimited);

        let (fixed, record, _) = store
            .set_session_budget_with_event(
                &session.id,
                &SessionBudgetUpdate {
                    mode: Some(BudgetMode::Fixed),
                    clear_hard_token_cap: true,
                    clear_hard_cost_cap: true,
                    ..SessionBudgetUpdate::default()
                },
            )
            .unwrap();
        assert!(!fixed.unlimited);
        assert_eq!(record.mode, BudgetMode::Fixed);
        assert!(record.caps.is_empty());

        let events = store.list_all_events(&session.id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "session.budget.updated")
                .count(),
            3
        );
    }
}
