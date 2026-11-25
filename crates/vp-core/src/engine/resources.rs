//! Read-only Run Contract, resource, and readiness views.
//!
//! R1 is deliberately observe-only: it derives bounded snapshots from the
//! existing budget, usage, approval, task, merge, effect, and worktree state.
//! It does not pause, spend, approve, merge, or route anything.

use super::*;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use vp_protocol::types::{
    AutoBudgetCaps, AutoBudgetExtension, BudgetMode, MergeQueueEntry, ProjectRunPolicy,
    ReadinessSnapshot, ResourceAttention, ResourceCount, ResourceDimension, ResourceSnapshot,
    ResourceSoftTargets, RunContractOverride, RunContractView, SESSION_RESOURCE_DECISION_EVENT,
    SESSION_RUN_CONTRACT_UPDATED_EVENT, SessionBudgetStatus, SetRunContractParams,
    SetSessionBudgetParams, TopUpSessionResourcesParams,
};

const READINESS_CACHE_TTL: Duration = Duration::from_secs(2);
const READINESS_CACHE_MAX_ENTRIES: usize = 64;

fn readiness_cache_hit(cached: &CachedReadiness, seq: u64, now: Instant) -> bool {
    cached.seq == seq && now.saturating_duration_since(cached.cached_at) <= READINESS_CACHE_TTL
}

/// Keep only the latest merge-queue attempt for each task. A failed first
/// attempt followed by a successful retry must not keep readiness or program
/// evidence blocked forever. Untasked rows are unique and retained.
pub(crate) fn latest_merge_entries(queue: &[MergeQueueEntry]) -> Vec<&MergeQueueEntry> {
    let mut latest = HashMap::<String, &MergeQueueEntry>::new();
    for entry in queue {
        let key = entry
            .task_id
            .clone()
            .unwrap_or_else(|| format!("entry:{}", entry.id));
        let replace = latest.get(&key).is_none_or(|current| {
            (entry.attempt, &entry.created_at) >= (current.attempt, &current.created_at)
        });
        if replace {
            latest.insert(key, entry);
        }
    }
    latest.into_values().collect()
}

impl Engine {
    async fn resource_session(&self, session_id: &str) -> Result<Session> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))
    }

    async fn latest_session_seq(&self, session_id: &str) -> Result<u64> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.latest_session_seq(&id)).await?
    }

    fn cache_readiness(&self, session_id: &str, seq: u64, snapshot: &ReadinessSnapshot) {
        let Ok(mut cache) = self.readiness_cache.lock() else {
            return;
        };
        let now = Instant::now();
        cache.retain(|_, cached| {
            now.saturating_duration_since(cached.cached_at) <= READINESS_CACHE_TTL
        });
        if cache.len() >= READINESS_CACHE_MAX_ENTRIES
            && !cache.contains_key(session_id)
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, cached)| cached.cached_at)
                .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(
            session_id.to_string(),
            CachedReadiness {
                seq,
                cached_at: now,
                snapshot: snapshot.clone(),
            },
        );
    }

    async fn project_run_policy_record(
        &self,
        project_id: &str,
    ) -> Result<(ProjectRunPolicy, Option<String>)> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project_run_policy_record(&project_id))
            .await?
    }

    /// Bounded, newest-first readiness projection. It is intentionally
    /// conservative: `verified`/`merged` come from the merge queue and effect
    /// receipts, not from an agent's prose claim.
    /// Fresh readiness. Explicit reads and state-changing operations use this
    /// path; a direct worktree edit is reflected immediately.
    pub async fn session_readiness(&self, session_id: &str) -> Result<ReadinessSnapshot> {
        self.compute_session_readiness(session_id).await
    }

    /// Short-lived readiness cache for the read-only resource card. It is keyed
    /// by the newest event sequence and a 2-second TTL, so any appended event
    /// invalidates it. An explicit external worktree edit can lag by the TTL;
    /// callers that need exact freshness use `session_readiness`.
    pub(crate) async fn session_readiness_cached(
        &self,
        session_id: &str,
    ) -> Result<ReadinessSnapshot> {
        let seq = self.latest_session_seq(session_id).await?;
        if let Ok(cache) = self.readiness_cache.lock()
            && let Some(cached) = cache.get(session_id).cloned()
            && readiness_cache_hit(&cached, seq, Instant::now())
        {
            return Ok(cached.snapshot);
        }
        let snapshot = self.compute_session_readiness(session_id).await?;
        self.cache_readiness(session_id, seq, &snapshot);
        Ok(snapshot)
    }

    async fn compute_session_readiness(&self, session_id: &str) -> Result<ReadinessSnapshot> {
        let session = self.resource_session(session_id).await?;
        let effects = self.session_effects(session_id, None, false, 200).await?;
        let queue = self.list_merge_queue(session_id).await?;
        let active_queue = latest_merge_entries(&queue);
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let approvals = self.list_approvals(Some(session_id.to_string())).await?;
        // Test evidence is supplementary: a test-cache read failure must not
        // fail readiness. Only `kind=test` counts; lint/check entries are not
        // tests. Expired cache rows still prove that the run happened.
        let latest_test_result = {
            let store = self.store.clone();
            let project_id = session.project_id.clone();
            let session_id = session_id.to_string();
            let now = vp_protocol::now_rfc3339();
            tokio::task::spawn_blocking(move || {
                store.list_test_results(
                    &project_id,
                    Some(&session_id),
                    Some("test"),
                    None,
                    true,
                    1,
                    &now,
                )
            })
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|rows| rows.into_iter().next())
        };
        let (latest_test, latest_test_passed) = match latest_test_result {
            Some(entry) => {
                let passed = entry.status == "passed";
                let label = format!("{} {} · {}", entry.kind, entry.status, entry.created_at);
                (Some(label.chars().take(200).collect()), Some(passed))
            }
            None => (None, None),
        };

        let base_commit = session.base_commit.clone().unwrap_or_default();
        let (head, status) = match session.worktree.as_deref() {
            Some(path) if !path.trim().is_empty() => {
                let path = PathBuf::from(path);
                let task = tokio::task::spawn_blocking(move || {
                    let head = crate::worktree::head_commit(&path).ok();
                    let status = crate::worktree::status_porcelain_all(&path).unwrap_or_default();
                    (head, status)
                });
                task.await.unwrap_or((None, String::new()))
            }
            _ => (None, String::new()),
        };
        let dirty = !status.trim().is_empty();
        let changed_paths = status
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let _code = parts.next()?;
                let path = parts.collect::<Vec<_>>().join(" ");
                (!path.is_empty()).then_some(path)
            })
            .take(50)
            .collect::<Vec<_>>();

        let remaining_work = tasks
            .iter()
            .filter(|task| !matches!(task.status.as_str(), "completed" | "skipped"))
            .map(|task| format!("{}: {}", task.task_id, task.status))
            .take(100)
            .collect::<Vec<_>>();

        let mut blockers = Vec::new();
        if let Some(reason) = effects.block_reason.clone() {
            blockers.push(reason);
        }
        for entry in &active_queue {
            if matches!(entry.status.as_str(), "conflict" | "failed" | "interrupted") {
                blockers.push(format!(
                    "merge queue {} {}: {}",
                    entry.task_id.as_deref().unwrap_or("untasked"),
                    entry.status,
                    entry.detail.as_deref().unwrap_or("no detail")
                ));
            }
        }
        let pending_approvals = approvals.iter().filter(|a| a.status == "pending").count();
        if pending_approvals > 0 {
            blockers.push(format!(
                "{pending_approvals} pending approval(s) require operator attention"
            ));
        }
        blockers.truncate(20);

        let any_merged = active_queue.iter().any(|entry| entry.status == "merged");
        let any_verified = active_queue
            .iter()
            .any(|entry| matches!(entry.status.as_str(), "queued" | "merging" | "merged"));
        let level = if effects.blocked
            || active_queue
                .iter()
                .any(|entry| matches!(entry.status.as_str(), "conflict" | "failed" | "interrupted"))
        {
            "blocked"
        } else if any_merged && remaining_work.is_empty() {
            "merged"
        } else if any_merged {
            "partial"
        } else if any_verified {
            "verified"
        } else if dirty {
            "dirty"
        } else if head.as_deref().is_some_and(|head| head != base_commit) {
            "committed"
        } else {
            "none"
        };
        let latest_commit = active_queue
            .iter()
            .filter(|entry| entry.status == "merged")
            .max_by_key(|entry| entry.created_at.as_str())
            .map(|entry| entry.commit.clone())
            .or(head);
        let verifier = active_queue
            .iter()
            .filter(|entry| matches!(entry.status.as_str(), "queued" | "merging" | "merged"))
            .max_by_key(|entry| entry.created_at.as_str())
            .map(|entry| {
                if entry.status == "merged" {
                    "approved · merged".to_string()
                } else {
                    "approved · queue entry".to_string()
                }
            });
        let merge = active_queue
            .iter()
            .max_by_key(|entry| entry.created_at.as_str())
            .map(|entry| entry.status.clone());
        let partial_delivery = self.latest_partial_delivery(session_id).await?;

        let snapshot = ReadinessSnapshot {
            session_id: session_id.to_string(),
            level: level.to_string(),
            shippable: matches!(level, "verified" | "merged") && blockers.is_empty(),
            mergeable: matches!(level, "verified" | "merged") && blockers.is_empty(),
            latest_commit,
            latest_test,
            latest_test_passed,
            verifier,
            merge,
            changed_paths,
            remaining_work,
            blockers,
            partial_delivery,
            generated_at: now_rfc3339(),
        };
        Ok(snapshot)
    }

    /// Bounded multi-resource snapshot. All values are derived; a missing
    /// price or test signal is reported as unavailable, never fabricated.
    pub async fn session_resources(&self, session_id: &str) -> Result<ResourceSnapshot> {
        let session = self.resource_session(session_id).await?;
        let readiness = self.session_readiness_cached(session_id).await?;
        let (project_run_policy, _) = self.project_run_policy_record(&session.project_id).await?;
        let contract_override = self.run_contract_override(session_id).await?;
        let profile = contract_override
            .as_ref()
            .map(|value| value.profile)
            .unwrap_or(project_run_policy.default_profile);
        let attention_cap = contract_override
            .as_ref()
            .and_then(|value| value.attention_cap)
            .unwrap_or(project_run_policy.attention_cap);
        let budget = self.session_budget(session_id).await?;
        let usage = self.usage_totals(session_id).await?;
        let context = self.session_context_status(session_id).await?;
        let approvals = self.list_approvals(Some(session_id.to_string())).await?;
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let queue = self.list_merge_queue(session_id).await?;
        let effects = self.session_effects(session_id, None, false, 200).await?;
        let agents = self.list_agents(session_id).await?;

        let totals = usage;
        let cache_read = totals.cache_read_tokens.min(totals.input_tokens);
        let cache_write = totals
            .cache_write_tokens
            .min(totals.input_tokens.saturating_sub(cache_read));
        let uncached = totals
            .input_tokens
            .saturating_sub(cache_read)
            .saturating_sub(cache_write);
        let weighted_tokens = uncached as f64
            + (cache_read as f64 * 0.10)
            + (cache_write as f64 * 1.25)
            + totals.output_tokens as f64;
        let raw_tokens = totals.input_tokens.saturating_add(totals.output_tokens);
        let cost = budget.spent_cost_usd.max(totals.cost_usd);
        let pricing_source = if cost > 0.0 {
            "configured"
        } else {
            "unconfigured"
        };

        let (attention, risk_used) = resource_attention(&approvals, effects.counts.unknown);
        let no_progress = budget
            .state
            .as_ref()
            .map(|state| state.turns_without_progress)
            .unwrap_or(0);
        let completed_tasks = tasks
            .iter()
            .filter(|task| task.status == "completed")
            .count();
        let total_tasks = tasks.len();
        let merged = queue
            .iter()
            .filter(|entry| entry.status == "merged")
            .count();
        let quality_score = if total_tasks > 0 {
            (completed_tasks as f64 / total_tasks as f64) * 100.0
        } else {
            0.0
        };

        let mut dimensions = vec![
            resource_money(&budget, cost, pricing_source),
            resource_tokens(
                "tokens",
                "Raw tokens",
                raw_tokens as f64,
                budget.remaining_tokens,
                budget.hard_token_cap.or(budget.effective_token_budget),
                "usage_ledger",
                "Cumulative input + output tokens; includes cache-read/write subsets.",
            ),
            resource_tokens(
                "weighted_tokens",
                "Weighted tokens",
                weighted_tokens,
                None,
                None,
                "usage_ledger",
                "Cache-aware equivalent (cache read 0.1x, cache write 1.25x, output 1x). Not a hard cap.",
            ),
            resource_time(&budget),
            resource_attention_dim(&attention, attention_cap),
        ];
        dimensions.push(resource_risk(risk_used, effects.blocked));
        dimensions.push(resource_context(
            context.usage.as_ref(),
            context.compaction_count,
        ));
        let active_agents = agents
            .iter()
            .filter(|agent| matches!(agent.status.as_str(), "running" | "waiting_input"))
            .count();
        dimensions.push(ResourceDimension {
            key: "capacity".to_string(),
            label: "Active agents".to_string(),
            unit: "agents".to_string(),
            used: active_agents as f64,
            remaining: None,
            soft_target: None,
            hard_cap: None,
            forecast_p50: None,
            forecast_p90: None,
            state: "ok".to_string(),
            source: "agents".to_string(),
            note: Some(format!(
                "{active_agents} active / {} total session agents",
                agents.len()
            )),
        });
        dimensions.push(ResourceDimension {
            key: "quality".to_string(),
            label: "Task completion".to_string(),
            unit: "%".to_string(),
            used: quality_score,
            remaining: None,
            soft_target: Some(100.0),
            hard_cap: Some(100.0),
            forecast_p50: None,
            forecast_p90: None,
            state: if effects.blocked {
                "hard_limit"
            } else if total_tasks > 0 && completed_tasks == total_tasks {
                "ok"
            } else {
                "watch"
            }
            .to_string(),
            source: "task queue/Verifier/Integrator".to_string(),
            note: Some(format!(
                "{completed_tasks}/{total_tasks} tasks completed; {merged} merge(s); test evidence comes from the test cache when available, never from prose"
            )),
        });
        dimensions.push(ResourceDimension {
            key: "progress".to_string(),
            label: "Progress".to_string(),
            unit: "evidence".to_string(),
            used: (completed_tasks + merged) as f64,
            remaining: None,
            soft_target: None,
            hard_cap: None,
            forecast_p50: None,
            forecast_p90: None,
            state: if no_progress > 0 { "watch" } else { "ok" }.to_string(),
            source: "run journal/auto budget state".to_string(),
            note: Some(format!(
                "{no_progress} turn(s) without progress; {completed_tasks} completed task(s), {merged} merged entry/entries"
            )),
        });

        let bottleneck = resource_bottleneck(
            &dimensions,
            attention.pending,
            no_progress,
            effects.blocked,
            !readiness.blockers.is_empty(),
        );
        let mut warnings = Vec::new();
        if contract_override.is_some() {
            warnings.push("Session Run Contract override is active.".to_string());
        }
        if pricing_source == "unconfigured" {
            warnings.push(
                "Provider prices are not configured; USD is unavailable and weighted tokens are the fallback economic view."
                    .to_string(),
            );
        }
        if readiness.latest_test.is_none() && total_tasks > 0 {
            warnings.push(
                "R1 does not infer test pass/fail from raw shell output; readiness stops at committed/verified/merged evidence."
                    .to_string(),
            );
        }
        if attention.pending > 0 {
            warnings.push(format!(
                "{} pending approval(s) are consuming operator attention.",
                attention.pending
            ));
        }
        if effects.blocked {
            warnings.push(
                "Unresolved effect receipts block merge; reconcile them explicitly before retrying."
                    .to_string(),
            );
        }
        warnings.push(
            "Run Contract governance: pre-dispatch deferral, profile/contract overrides, bounded top-up, and turn-boundary pause. It cannot approve, merge, rebalance, or route models."
                .to_string(),
        );

        Ok(ResourceSnapshot {
            session_id: session_id.to_string(),
            generated_at: now_rfc3339(),
            profile: profile.as_str().to_string(),
            dimensions,
            bottleneck,
            attention,
            readiness: Some(readiness),
            forecast: Some(budget.forecast.clone()),
            warnings,
            sample_calls: totals.calls.min(u32::MAX as u64) as u32,
            pricing_source: pricing_source.to_string(),
        })
    }

    /// Derived, read-only contract view. R2 adds persisted operator overrides
    /// and governor actions; R1 only exposes the effective envelope.
    pub async fn run_contract_view(&self, session_id: &str) -> Result<RunContractView> {
        let session = self.resource_session(session_id).await?;
        let budget = self.session_budget(session_id).await?;
        let plan = self.get_team_plan(session_id).await?;
        let effects = self.session_effects(session_id, None, false, 200).await?;
        let (project_run_policy, project_run_policy_updated_at) =
            self.project_run_policy_record(&session.project_id).await?;
        let contract_override = self.run_contract_override(session_id).await?;
        let profile = contract_override
            .as_ref()
            .map(|value| value.profile)
            .unwrap_or(project_run_policy.default_profile);
        let completion_reserve_ratio = contract_override
            .as_ref()
            .and_then(|value| value.completion_reserve_ratio)
            .unwrap_or(project_run_policy.completion_reserve_ratio);
        let attention_cap = contract_override
            .as_ref()
            .and_then(|value| value.attention_cap)
            .unwrap_or(project_run_policy.attention_cap);
        let stop_policy = contract_override
            .as_ref()
            .and_then(|value| value.stop_policy.clone())
            .unwrap_or_else(|| project_run_policy.stop_policy.clone());
        let hard_token = budget.hard_token_cap.or(budget.effective_token_budget);
        let hard_cost = budget
            .hard_cost_cap_usd
            .or(budget.effective_cost_budget_usd);
        let hard_time = budget
            .hard_wall_clock_secs
            .or(budget.effective_time_budget_secs);
        let mut warnings = Vec::new();
        if contract_override.is_some() {
            warnings.push("Session Run Contract override is active.".to_string());
        }
        if hard_cost.is_none() {
            warnings.push(
                "No USD cap is configured; the contract cannot make a cost-based stop decision."
                    .to_string(),
            );
        }
        if plan.is_none() {
            warnings.push("No TeamPlan is stored; acceptance evidence is empty.".to_string());
        }
        if effects.blocked {
            warnings.push("Unresolved effect receipts block merge.".to_string());
        }
        let acceptance = plan
            .as_ref()
            .map(|plan| {
                plan.tasks
                    .iter()
                    .flat_map(|task| task.acceptance.iter().cloned())
                    .take(100)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(RunContractView {
            session_id: session_id.to_string(),
            project_id: session.project_id.clone(),
            profile: profile.as_str().to_string(),
            source: if contract_override.is_some() {
                "session_override".to_string()
            } else if project_run_policy_updated_at.is_some() {
                "project_policy".to_string()
            } else {
                "derived".to_string()
            },
            goal: session.goal.clone(),
            acceptance,
            risk_ceiling: "approval-managed".to_string(),
            autonomy: "operator-configured".to_string(),
            hard_caps: AutoBudgetCaps {
                token: hard_token,
                cost_usd: hard_cost,
                wall_clock_secs: hard_time,
            },
            soft_targets: ResourceSoftTargets {
                cost_usd: hard_cost.map(|cap| cap * 0.8),
                time_secs: hard_time.map(|cap| (cap as f64 * 0.8) as u64),
                attention: Some((attention_cap as f64 * 0.8).max(1.0) as u32),
            },
            completion_reserve_ratio,
            attention_cap,
            stop_policy,
            warnings,
            generated_at: now_rfc3339(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResourceGovernorDecision {
    pub session_id: String,
    pub phase: String,
    /// `continue`, `warn`, `defer`, `pause`, or `stop`.
    pub action: String,
    pub reason: String,
    pub bottleneck: Option<String>,
    pub details: serde_json::Value,
    pub generated_at: String,
}

impl ResourceGovernorDecision {
    pub fn should_block(&self) -> bool {
        matches!(self.action.as_str(), "defer" | "pause" | "stop")
    }
}

impl Engine {
    /// Latest operator Run Contract override, if any. The override is an
    /// event-only state: the newest `session.run_contract.updated` payload is
    /// the durable source, so replay/import preserve it without a projection.
    pub async fn run_contract_override(
        &self,
        session_id: &str,
    ) -> Result<Option<RunContractOverride>> {
        let Some(event) = self
            .latest_event_of_kind(session_id, SESSION_RUN_CONTRACT_UPDATED_EVENT)
            .await?
        else {
            return Ok(None);
        };
        let parsed: RunContractOverride = serde_json::from_value(event.payload)
            .context("session.run_contract.updated payload is invalid")?;
        parsed.validate()?;
        Ok(Some(parsed))
    }

    pub async fn set_run_contract(&self, params: SetRunContractParams) -> Result<RunContractView> {
        let session = self.resource_session(&params.session_id).await?;
        if session.status == "imported" {
            bail!("imported sessions are read-only archives");
        }
        let contract_override = RunContractOverride {
            profile: params.profile,
            completion_reserve_ratio: params.completion_reserve_ratio,
            attention_cap: params.attention_cap,
            stop_policy: params.stop_policy,
            updated_at: now_rfc3339(),
            updated_by: params
                .updated_by
                .map(|value| value.chars().take(120).collect::<String>()),
        };
        contract_override.validate()?;
        self.append_event(
            &session.id,
            SESSION_RUN_CONTRACT_UPDATED_EVENT,
            serde_json::to_value(&contract_override)?,
        )
        .await?;
        self.run_contract_view(&session.id).await
    }

    /// Add to the effective budget without removing caps or switching to
    /// unlimited. Auto sessions use the existing atomic extension path; fixed
    /// sessions set a new effective budget clamped to the existing hard caps.
    /// The caller decides whether to resume a paused session.
    pub async fn top_up_session_budget(
        &self,
        params: TopUpSessionResourcesParams,
    ) -> Result<(SessionBudgetStatus, bool)> {
        let has_add = params.add_tokens.is_some()
            || params.add_cost_usd.is_some()
            || params.add_time_secs.is_some();
        if !has_add {
            bail!("top-up requires at least one positive token, cost, or time increment");
        }
        if params.add_tokens == Some(0) || params.add_time_secs == Some(0) {
            bail!("top-up increments must be greater than zero");
        }
        if let Some(cost) = params.add_cost_usd
            && (!cost.is_finite() || cost <= 0.0)
        {
            bail!("top-up cost must be a positive finite USD amount");
        }
        let status = self.session_budget(&params.session_id).await?;
        if status.unlimited {
            return Ok((status, false));
        }
        match status.mode {
            BudgetMode::Auto => {
                let request = AutoBudgetExtension {
                    requested_token_extension: params.add_tokens,
                    requested_cost_extension_usd: params.add_cost_usd,
                    requested_wall_clock_extension_secs: params.add_time_secs,
                    reason: Some(
                        params
                            .reason
                            .unwrap_or_else(|| "operator top-up".to_string())
                            .chars()
                            .take(200)
                            .collect(),
                    ),
                    forecast: Some(status.forecast.clone()),
                };
                let result = self
                    .extend_session_auto_budget(&params.session_id, request)
                    .await?;
                Ok((result.budget, !result.extended))
            }
            BudgetMode::Fixed => {
                let (new_token, token_capped) = top_up_u64(
                    status.effective_token_budget,
                    params.add_tokens,
                    status.hard_token_cap,
                );
                let (new_cost, cost_capped) = top_up_f64(
                    status.effective_cost_budget_usd,
                    params.add_cost_usd,
                    status.hard_cost_cap_usd,
                );
                let (new_time, time_capped) = top_up_u64(
                    status.effective_time_budget_secs,
                    params.add_time_secs,
                    status.hard_wall_clock_secs,
                );
                let result = self
                    .set_session_budget(SetSessionBudgetParams {
                        session_id: params.session_id.clone(),
                        mode: Some(BudgetMode::Fixed),
                        token_budget: new_token,
                        cost_budget_usd: new_cost,
                        time_budget_secs: new_time,
                        ..SetSessionBudgetParams::default()
                    })
                    .await?;
                Ok((result.budget, token_capped || cost_capped || time_capped))
            }
            BudgetMode::Unlimited => Ok((status, false)),
        }
    }

    /// Deterministic R2a resource guard. It never spends, approves, merges, or
    /// routes; it only returns a bounded decision for dispatch/turn boundaries.
    ///
    /// Dispatch keeps the full readiness gate. Turn boundaries use the lean
    /// method below: an active run is already subject to approval, effect,
    /// merge, and Verifier gates, and must not pay for a `git status`,
    /// task-queue, merge-queue, and agent-roster scan before every provider
    /// call.
    pub async fn resource_governor_decision(
        &self,
        session_id: &str,
        phase: &str,
    ) -> Result<ResourceGovernorDecision> {
        if phase == "turn" {
            return self.resource_governor_turn_decision(session_id).await;
        }
        let snapshot = self.session_resources(session_id).await?;
        let contract = self.run_contract_view(session_id).await?;
        let unlimited = self.session_budget(session_id).await?.unlimited;
        let inputs = ResourceGovernorInputs {
            profile: &contract.profile,
            attention_cap: contract.attention_cap,
            attention_interrupts: snapshot.attention.interrupts,
            completion_reserve_ratio: contract.completion_reserve_ratio,
            hard_caps: &contract.hard_caps,
            dimensions: &snapshot.dimensions,
            readiness_blocked: snapshot
                .readiness
                .as_ref()
                .is_some_and(|readiness| readiness.level == "blocked"),
            unlimited,
        };
        let (action, reason) = resource_governor_action(&inputs);
        let details = resource_governor_details(
            phase,
            &contract.profile,
            snapshot.bottleneck.as_deref(),
            contract.attention_cap,
            contract.completion_reserve_ratio,
            &snapshot.dimensions,
        );
        Ok(ResourceGovernorDecision {
            session_id: session_id.to_string(),
            phase: phase.to_string(),
            action,
            reason,
            bottleneck: snapshot.bottleneck,
            details,
            generated_at: now_rfc3339(),
        })
    }

    /// Lean R2b turn-boundary decision. It computes only the dimensions the
    /// governor can act on at a safe provider boundary: attention/risk,
    /// money/token/time hard limits, active context, and the completion
    /// reserve. It deliberately does not call `session_readiness` (git status),
    /// the task queue, the merge queue, or the agent roster; readiness remains
    /// a dispatch-only new-work gate, while an active run must be able to
    /// finish approval/effect/MCP flows.
    pub(crate) async fn resource_governor_turn_decision(
        &self,
        session_id: &str,
    ) -> Result<ResourceGovernorDecision> {
        let session = self.resource_session(session_id).await?;
        let budget = self.session_budget(session_id).await?;
        let usage = self.usage_totals(session_id).await?;
        let (project_run_policy, _) = self.project_run_policy_record(&session.project_id).await?;
        let contract_override = self.run_contract_override(session_id).await?;
        let profile = contract_override
            .as_ref()
            .map(|value| value.profile)
            .unwrap_or(project_run_policy.default_profile);
        let attention_cap = contract_override
            .as_ref()
            .and_then(|value| value.attention_cap)
            .unwrap_or(project_run_policy.attention_cap);
        let completion_reserve_ratio = contract_override
            .as_ref()
            .and_then(|value| value.completion_reserve_ratio)
            .unwrap_or(project_run_policy.completion_reserve_ratio);
        // A rescue grant releases only the completion reserve for this safe
        // turn boundary. It never removes a hard cap, attention limit, or
        // context limit: those still pause the run.
        let active_rescue = self.active_rescue_grant(session_id).await?;
        let effective_completion_reserve_ratio = if active_rescue.is_some() {
            0.0
        } else {
            completion_reserve_ratio
        };
        let context = self.session_context_status(session_id).await?;
        let approvals = self.list_approvals(Some(session_id.to_string())).await?;
        let effects = self.session_effects(session_id, None, false, 200).await?;
        let (attention, risk_used) = resource_attention(&approvals, effects.counts.unknown);

        let hard_token = budget.hard_token_cap.or(budget.effective_token_budget);
        let hard_cost = budget
            .hard_cost_cap_usd
            .or(budget.effective_cost_budget_usd);
        let hard_time = budget
            .hard_wall_clock_secs
            .or(budget.effective_time_budget_secs);
        let raw_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
        let cost = budget.spent_cost_usd.max(usage.cost_usd);
        let pricing_source = if cost > 0.0 {
            "configured"
        } else {
            "unconfigured"
        };
        let dimensions = vec![
            resource_money(&budget, cost, pricing_source),
            resource_tokens(
                "tokens",
                "Raw tokens",
                raw_tokens as f64,
                budget.remaining_tokens,
                hard_token,
                "usage_ledger",
                "Cumulative input + output tokens; includes cache-read/write subsets.",
            ),
            resource_time(&budget),
            resource_attention_dim(&attention, attention_cap),
            resource_risk(risk_used, effects.blocked),
            resource_context(context.usage.as_ref(), context.compaction_count),
        ];
        let no_progress = budget
            .state
            .as_ref()
            .map(|state| state.turns_without_progress)
            .unwrap_or(0);
        let bottleneck = resource_bottleneck(
            &dimensions,
            attention.pending,
            no_progress,
            effects.blocked,
            false,
        );
        let hard_caps = AutoBudgetCaps {
            token: hard_token,
            cost_usd: hard_cost,
            wall_clock_secs: hard_time,
        };
        let profile = profile.as_str();
        let inputs = ResourceGovernorInputs {
            profile,
            attention_cap,
            attention_interrupts: attention.interrupts,
            completion_reserve_ratio: effective_completion_reserve_ratio,
            hard_caps: &hard_caps,
            dimensions: &dimensions,
            readiness_blocked: false,
            unlimited: budget.unlimited,
        };
        let (action, reason) = resource_governor_action(&inputs);
        let mut details = resource_governor_details(
            "turn",
            profile,
            bottleneck.as_deref(),
            attention_cap,
            effective_completion_reserve_ratio,
            &dimensions,
        );
        details["attention_batch"] = attention_batch_details(&approvals, &attention, attention_cap);
        if let Some(grant) = &active_rescue {
            details["rescue"] = serde_json::json!({
                "grant_id": grant.id.clone(),
                "expires_at": grant.expires_at.clone(),
                "original_completion_reserve_ratio": completion_reserve_ratio,
            });
        }
        Ok(ResourceGovernorDecision {
            session_id: session_id.to_string(),
            phase: "turn".to_string(),
            action,
            reason,
            bottleneck,
            details,
            generated_at: now_rfc3339(),
        })
    }

    pub async fn record_resource_decision(
        &self,
        decision: &ResourceGovernorDecision,
    ) -> Result<()> {
        if decision.action == "continue" {
            return Ok(());
        }
        // A scheduler tick can re-evaluate the same terminal resource block
        // repeatedly. Keep one event until something else happens in the
        // session; otherwise an unresolved approval/readiness block would grow
        // the event log on every cooldown tick.
        if let Some(previous) = self
            .latest_event_of_kind(&decision.session_id, SESSION_RESOURCE_DECISION_EVENT)
            .await?
        {
            let previous_seq = previous.seq;
            if let Ok(previous) =
                serde_json::from_value::<ResourceGovernorDecision>(previous.payload)
                && previous.phase == decision.phase
                && previous.action == decision.action
                && previous.reason == decision.reason
                && previous.bottleneck == decision.bottleneck
                && previous.details == decision.details
            {
                let store = self.store.clone();
                let session_id = decision.session_id.clone();
                let latest =
                    tokio::task::spawn_blocking(move || store.latest_session_seq(&session_id))
                        .await??;
                if latest == previous_seq {
                    return Ok(());
                }
            }
        }
        self.append_event(
            &decision.session_id,
            SESSION_RESOURCE_DECISION_EVENT,
            serde_json::to_value(decision)?,
        )
        .await?;
        Ok(())
    }
}

fn top_up_u64(current: Option<u64>, add: Option<u64>, hard: Option<u64>) -> (Option<u64>, bool) {
    let Some(add) = add else {
        return (current, false);
    };
    let proposed = current.unwrap_or(0).saturating_add(add);
    let capped = hard.is_some_and(|cap| proposed > cap);
    (Some(hard.map_or(proposed, |cap| cap.min(proposed))), capped)
}

fn top_up_f64(current: Option<f64>, add: Option<f64>, hard: Option<f64>) -> (Option<f64>, bool) {
    let Some(add) = add else {
        return (current, false);
    };
    let proposed = current.unwrap_or(0.0) + add;
    let capped = hard.is_some_and(|cap| proposed > cap);
    (Some(hard.map_or(proposed, |cap| cap.min(proposed))), capped)
}

fn resource_money(
    budget: &vp_protocol::types::SessionBudgetStatus,
    cost: f64,
    pricing_source: &str,
) -> ResourceDimension {
    let hard = budget
        .hard_cost_cap_usd
        .or(budget.effective_cost_budget_usd);
    let soft = hard.map(|cap| cap * 0.8);
    ResourceDimension {
        key: "money".to_string(),
        label: "Provider cost".to_string(),
        unit: "usd".to_string(),
        used: cost,
        remaining: budget.remaining_cost_usd,
        soft_target: soft,
        hard_cap: hard,
        forecast_p50: budget.forecast.runway_cost_usd,
        forecast_p90: None,
        state: resource_state(Some(cost), soft, hard).to_string(),
        source: if pricing_source == "configured" {
            "usage_ledger".to_string()
        } else {
            "unconfigured".to_string()
        },
        note: (pricing_source == "unconfigured").then(|| {
            "No configured prices; this value is 0.00 and must not be read as a cost measurement."
                .to_string()
        }),
    }
}

fn resource_tokens(
    key: &str,
    label: &str,
    used: f64,
    remaining: Option<u64>,
    hard: Option<u64>,
    source: &str,
    note: &str,
) -> ResourceDimension {
    let hard = hard.map(|value| value as f64);
    let soft = hard.map(|value| value * 0.8);
    ResourceDimension {
        key: key.to_string(),
        label: label.to_string(),
        unit: "tokens".to_string(),
        used,
        remaining: remaining.map(|value| value as f64),
        soft_target: soft,
        hard_cap: hard,
        forecast_p50: None,
        forecast_p90: None,
        state: resource_state(Some(used), soft, hard).to_string(),
        source: source.to_string(),
        note: Some(note.to_string()),
    }
}

fn resource_time(budget: &vp_protocol::types::SessionBudgetStatus) -> ResourceDimension {
    let hard = budget
        .hard_wall_clock_secs
        .or(budget.effective_time_budget_secs)
        .map(|value| value as f64);
    let used = budget.elapsed_secs as f64;
    let soft = hard.map(|value| value * 0.8);
    ResourceDimension {
        key: "time".to_string(),
        label: "Wall clock".to_string(),
        unit: "seconds".to_string(),
        used,
        remaining: budget.remaining_secs.map(|value| value as f64),
        soft_target: soft,
        hard_cap: hard,
        forecast_p50: budget.forecast.runway_secs.map(|value| value as f64),
        forecast_p90: None,
        state: resource_state(Some(used), soft, hard).to_string(),
        source: "run journal".to_string(),
        note: None,
    }
}

fn resource_attention_dim(attention: &ResourceAttention, cap: u32) -> ResourceDimension {
    let hard = cap as f64;
    let soft = hard * 0.8;
    ResourceDimension {
        key: "attention".to_string(),
        label: "Operator attention".to_string(),
        unit: "interrupts".to_string(),
        used: attention.interrupts as f64,
        remaining: Some((cap.saturating_sub(attention.interrupts)) as f64),
        soft_target: Some(soft),
        hard_cap: Some(hard),
        forecast_p50: None,
        forecast_p90: None,
        state: resource_state(Some(attention.interrupts as f64), Some(soft), Some(hard))
            .to_string(),
        source: "approvals".to_string(),
        note: Some(format!(
            "{} pending, {} approved, {} denied, {} expired; read-only/scratch operations are excluded",
            attention.pending, attention.approved, attention.denied, attention.expired
        )),
    }
}

fn resource_context(
    usage: Option<&vp_protocol::types::ContextUsage>,
    compaction_count: u64,
) -> ResourceDimension {
    let Some(usage) = usage else {
        return ResourceDimension {
            key: "context".to_string(),
            label: "Active context".to_string(),
            unit: "tokens".to_string(),
            used: 0.0,
            remaining: None,
            soft_target: None,
            hard_cap: None,
            forecast_p50: None,
            forecast_p90: None,
            state: "unavailable".to_string(),
            source: "context.usage".to_string(),
            note: Some("No context.usage event is recorded for this session.".to_string()),
        };
    };
    let used = usage.estimated_tokens as f64;
    let hard = usage.hard_limit_tokens as f64;
    let soft = usage.threshold_tokens as f64;
    ResourceDimension {
        key: "context".to_string(),
        label: "Active context".to_string(),
        unit: "tokens".to_string(),
        used,
        remaining: Some(
            (usage
                .hard_limit_tokens
                .saturating_sub(usage.estimated_tokens)) as f64,
        ),
        soft_target: Some(soft),
        hard_cap: Some(hard),
        forecast_p50: None,
        forecast_p90: None,
        state: resource_state(Some(used), Some(soft), Some(hard)).to_string(),
        source: "context.usage".to_string(),
        note: Some(format!(
            "window {}; {} compaction(s); threshold {}",
            usage.context_window, compaction_count, usage.threshold_tokens
        )),
    }
}

fn resource_risk(risk_used: u64, effects_blocked: bool) -> ResourceDimension {
    let risk_state = if effects_blocked {
        "hard_limit"
    } else if risk_used > 0 {
        "watch"
    } else {
        "ok"
    };
    ResourceDimension {
        key: "risk".to_string(),
        label: "Risk".to_string(),
        unit: "units".to_string(),
        used: risk_used as f64,
        remaining: None,
        soft_target: Some(20.0),
        hard_cap: None,
        forecast_p50: None,
        forecast_p90: None,
        state: risk_state.to_string(),
        source: "approvals/effects".to_string(),
        note: Some(
            "Risk is a ceiling, not a budget: host-state/destructive/external actions and unresolved effects cannot be purchased away."
                .to_string(),
        ),
    }
}

fn resource_dimension<'a>(
    dimensions: &'a [ResourceDimension],
    key: &str,
) -> Option<&'a ResourceDimension> {
    dimensions.iter().find(|dimension| dimension.key == key)
}

/// Shared decision inputs for the dispatch and lean turn phases. The action
/// ladder is shared so the two paths cannot drift; only `readiness_blocked` and
/// the dimension list differ between them.
struct ResourceGovernorInputs<'a> {
    profile: &'a str,
    attention_cap: u32,
    attention_interrupts: u32,
    completion_reserve_ratio: f64,
    hard_caps: &'a AutoBudgetCaps,
    dimensions: &'a [ResourceDimension],
    readiness_blocked: bool,
    /// `Session.unlimited` bypasses session/plan/project economic gates. It
    /// does not bypass attention, readiness, risk, context, approvals, or any
    /// verification/merge gate.
    unlimited: bool,
}

fn resource_governor_action(inputs: &ResourceGovernorInputs<'_>) -> (String, String) {
    if inputs.readiness_blocked {
        return ("defer".to_string(), "readiness_blocked".to_string());
    }
    if inputs.attention_interrupts >= inputs.attention_cap {
        return ("defer".to_string(), "attention_cap".to_string());
    }
    if inputs.profile == "unattended"
        && resource_dimension(inputs.dimensions, "risk")
            .is_some_and(|dimension| dimension.used > 0.0)
    {
        return ("defer".to_string(), "unattended_risk".to_string());
    }
    for key in ["money", "time", "context", "tokens"] {
        if inputs.unlimited && key != "context" {
            continue;
        }
        if resource_dimension(inputs.dimensions, key)
            .is_some_and(|dimension| dimension.state == "hard_limit")
        {
            return ("defer".to_string(), format!("resource_cap_{key}"));
        }
    }
    if !inputs.unlimited {
        for (key, hard) in [
            ("tokens", inputs.hard_caps.token.map(|value| value as f64)),
            (
                "time",
                inputs.hard_caps.wall_clock_secs.map(|value| value as f64),
            ),
        ] {
            let Some(hard) = hard else { continue };
            let Some(remaining) = resource_dimension(inputs.dimensions, key)
                .and_then(|dimension| dimension.remaining)
            else {
                continue;
            };
            let reserve = hard * inputs.completion_reserve_ratio;
            if remaining < reserve {
                return ("defer".to_string(), format!("completion_reserve_{key}"));
            }
        }
        if let (Some(hard), Some(remaining)) = (
            inputs.hard_caps.cost_usd,
            resource_dimension(inputs.dimensions, "money")
                .and_then(|dimension| dimension.remaining),
        ) && remaining < hard * inputs.completion_reserve_ratio
        {
            return ("defer".to_string(), "completion_reserve_money".to_string());
        }
    }
    if inputs.dimensions.iter().any(|dimension| {
        matches!(
            dimension.key.as_str(),
            "money" | "time" | "attention" | "context" | "tokens"
        ) && !(inputs.unlimited && matches!(dimension.key.as_str(), "money" | "time" | "tokens"))
            && matches!(dimension.state.as_str(), "soft_limit" | "watch")
    }) {
        return ("warn".to_string(), "soft_resource_limit".to_string());
    }
    ("continue".to_string(), String::new())
}

fn resource_governor_details(
    phase: &str,
    profile: &str,
    bottleneck: Option<&str>,
    attention_cap: u32,
    completion_reserve_ratio: f64,
    dimensions: &[ResourceDimension],
) -> serde_json::Value {
    serde_json::json!({
        "phase": phase,
        "profile": profile,
        "bottleneck": bottleneck,
        "attention_cap": attention_cap,
        "completion_reserve_ratio": completion_reserve_ratio,
        "states": dimensions.iter().map(|dimension| {
            serde_json::json!({"key": dimension.key, "state": dimension.state, "used": dimension.used})
        }).collect::<Vec<_>>(),
    })
}

fn resource_state(used: Option<f64>, soft: Option<f64>, hard: Option<f64>) -> &'static str {
    let used = used.unwrap_or(0.0);
    if hard.is_some_and(|cap| cap > 0.0 && used >= cap) {
        "hard_limit"
    } else if soft.is_some_and(|cap| cap > 0.0 && used >= cap) {
        "soft_limit"
    } else if hard.is_some_and(|cap| cap > 0.0 && used >= cap * 0.9) {
        "watch"
    } else {
        "ok"
    }
}

fn resource_bottleneck(
    dimensions: &[ResourceDimension],
    pending: u32,
    no_progress: u32,
    effects_blocked: bool,
    readiness_blocked: bool,
) -> Option<String> {
    if effects_blocked || readiness_blocked {
        return Some("risk".to_string());
    }
    if pending > 0 {
        return Some("attention".to_string());
    }
    if no_progress > 0 {
        return Some("progress".to_string());
    }
    for key in ["context", "time", "money"] {
        if dimensions
            .iter()
            .find(|dimension| dimension.key == key)
            .is_some_and(|dimension| {
                matches!(dimension.state.as_str(), "soft_limit" | "hard_limit")
            })
        {
            return Some(key.to_string());
        }
    }
    None
}

/// Bounded operator prompt carried by a turn-boundary pause. It includes only
/// approval metadata; raw tool input, command text, and file contents are
/// deliberately excluded. The existing approval queue and batch-resolve path
/// remain the only decision surfaces.
fn attention_batch_details(
    approvals: &[Approval],
    attention: &ResourceAttention,
    attention_cap: u32,
) -> serde_json::Value {
    let pending = approvals
        .iter()
        .filter(|approval| approval.status == "pending")
        .collect::<Vec<_>>();
    let mut by_risk: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for approval in &pending {
        let risk = approval
            .approval_risk
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let weight = approval_risk_weight(approval.approval_risk.as_deref());
        let entry = by_risk.entry(risk).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(weight);
    }
    let items = pending
        .iter()
        .take(8)
        .map(|approval| {
            serde_json::json!({
                "approval_id": approval.id,
                "tool": bounded_text(&approval.tool, 80),
                "risk": approval.approval_risk.as_deref().unwrap_or("unknown"),
                "task_id": approval.task_id.as_deref(),
                "priority": approval.priority,
                "waiters": approval.waiter_count,
                "reason": approval.approval_reason.as_deref().map(|reason| bounded_text(reason, 160)),
            })
        })
        .collect::<Vec<_>>();
    let by_risk = by_risk
        .into_iter()
        .map(|(risk, (count, weight))| {
            serde_json::json!({"risk": risk, "count": count, "weight": weight})
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema": "verlinpark.attention_batch/v1",
        "pending": pending.len(),
        "interrupts": attention.interrupts,
        "cap": attention_cap,
        "by_risk": by_risk,
        "items": items,
        "truncated": pending.len() > 8,
    })
}

fn bounded_text(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn resource_attention(approvals: &[Approval], unknown_effects: u64) -> (ResourceAttention, u64) {
    let mut attention = ResourceAttention::default();
    let mut by_risk: BTreeMap<String, u32> = BTreeMap::new();
    let mut risk = unknown_effects.saturating_mul(5);
    for approval in approvals {
        let weight = approval_risk_weight(approval.approval_risk.as_deref());
        match approval.status.as_str() {
            "pending" => attention.pending += 1,
            "approved" => attention.approved += 1,
            "denied" => attention.denied += 1,
            "expired" => attention.expired += 1,
            _ => {}
        }
        attention.interrupts = attention.interrupts.saturating_add(weight);
        *by_risk
            .entry(
                approval
                    .approval_risk
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            )
            .or_insert(0) += 1;
        risk = risk.saturating_add(weight as u64);
    }
    attention.by_risk = by_risk
        .into_iter()
        .map(|(key, count)| ResourceCount {
            key,
            count,
            weight: 0,
        })
        .collect();
    (attention, risk)
}

fn approval_risk_weight(risk: Option<&str>) -> u32 {
    match risk {
        Some("workspace_write") => 1,
        Some("external") => 2,
        Some("host_state") => 3,
        Some("destructive") => 5,
        Some("mcp") | Some("sensitive_gui") => 3,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::{
        PlanDepartment, PlanTask, RunProfile, RunStopPolicy, SetProjectRunPolicyParams, TeamPlan,
    };

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    /// Record usage in both places a real provider call updates: the session
    /// totals (budget status) and the per-agent usage ledger (resource
    /// snapshot). `Engine::record_usage` intentionally updates only the session
    /// totals, so it cannot be used to compare the two governor paths.
    async fn record_usage(engine: &Engine, session_id: &str, input: u64, output: u64) {
        engine
            .record_agent_usage(
                session_id,
                "test-agent",
                "Test Agent",
                "leader",
                None,
                None,
                "test-model",
                input,
                output,
                false,
                0.0,
            )
            .await
            .unwrap();
    }

    async fn fixture() -> (TempDir, TempDir, Engine, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("resources"))
            .unwrap();
        let session = store
            .create_session(&project.id, "resource snapshot")
            .unwrap();
        let engine = Engine::new(store);
        engine
            .create_leader_agent(&session.id, "test-model")
            .await
            .unwrap();
        let plan = TeamPlan {
            goal: "resource snapshot".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build it".into(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks: vec![PlanTask {
                id: "T1".into(),
                department: "impl".into(),
                title: "Build it".into(),
                kind: "coding".into(),
                priority: 5,
                profile: Some("coding".into()),
                deps: Vec::new(),
                write_scope: vec!["src/lib.rs".into()],
                acceptance: vec!["tests pass".into()],
                token_budget: None,
                cost_budget_usd: None,
                parent_task_id: None,
                fanout_key: None,
                fanout_join_for: None,
            }],
        };
        engine.submit_team_plan(&session.id, &plan).await.unwrap();
        (data, repo, engine, session)
    }

    #[tokio::test]
    async fn resource_and_readiness_views_are_bounded_and_read_only() {
        let (_data, _repo, engine, session) = fixture().await;
        let before = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap()
            .len();
        let readiness = engine.session_readiness(&session.id).await.unwrap();
        assert_eq!(readiness.session_id, session.id);
        assert!(matches!(
            readiness.level.as_str(),
            "none" | "dirty" | "committed" | "tests_passed" | "verified" | "merged" | "partial"
        ));
        assert!(!readiness.shippable);
        assert!(
            readiness
                .remaining_work
                .iter()
                .any(|task| task.starts_with("T1"))
        );

        let resources = engine.session_resources(&session.id).await.unwrap();
        assert_eq!(resources.profile, "balanced");
        assert_eq!(resources.pricing_source, "unconfigured");
        for key in [
            "money",
            "tokens",
            "weighted_tokens",
            "time",
            "attention",
            "risk",
            "context",
            "capacity",
            "quality",
            "progress",
        ] {
            assert!(
                resources
                    .dimensions
                    .iter()
                    .any(|dimension| dimension.key == key),
                "missing {key}: {:?}",
                resources.dimensions
            );
        }
        assert!(resources.readiness.is_some());
        assert!(
            resources
                .warnings
                .iter()
                .any(|warning| warning.contains("Run Contract governance"))
        );
        assert_eq!(resources.attention.pending, 0);

        let contract = engine.run_contract_view(&session.id).await.unwrap();
        assert_eq!(contract.profile, "balanced");
        assert_eq!(contract.source, "derived");
        assert!(contract.acceptance.contains(&"tests pass".to_string()));
        assert_eq!(contract.stop_policy.no_progress_turns, 6);
        assert!(
            contract
                .warnings
                .iter()
                .any(|warning| warning.contains("USD cap"))
        );

        let after = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap()
            .len();
        assert_eq!(before, after, "snapshots must not append events");
    }

    #[tokio::test]
    async fn readiness_tracks_dirty_and_committed_worktree_state() {
        let (_data, _repo, engine, session) = fixture().await;
        let session = engine.ensure_session_worktree(&session.id).await.unwrap();
        let worktree = session.worktree.as_deref().expect("session worktree");
        std::fs::create_dir_all(Path::new(worktree).join("src")).unwrap();
        std::fs::write(Path::new(worktree).join("src/lib.rs"), "pub fn a() {}\n").unwrap();

        let dirty = engine.session_readiness(&session.id).await.unwrap();
        assert_eq!(dirty.level, "dirty", "{dirty:?}");
        assert!(
            dirty
                .changed_paths
                .iter()
                .any(|path| path.contains("src/lib.rs"))
        );
        assert!(!dirty.shippable);

        let status = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["add", "src/lib.rs"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["commit", "-q", "-m", "add lib"])
            .status()
            .unwrap();
        assert!(status.success());

        let committed = engine.session_readiness(&session.id).await.unwrap();
        assert_eq!(committed.level, "committed", "{committed:?}");
        assert!(committed.changed_paths.is_empty());
        assert!(committed.latest_commit.is_some());
        assert!(!committed.shippable);
    }

    #[tokio::test]
    async fn readiness_reports_latest_test_evidence_and_tradeoffs_use_it() {
        let (_data, _repo, engine, session) = fixture().await;
        let session = engine.ensure_session_worktree(&session.id).await.unwrap();
        let now = vp_protocol::now_rfc3339();
        let entry = vp_protocol::test_cache::TestCacheEntry {
            id: "tr-1".into(),
            project_id: session.project_id.clone(),
            session_id: session.id.clone(),
            kind: "test".into(),
            command: "cargo test".into(),
            command_source: "project".into(),
            key_version: 1,
            cache_key: "key-1".into(),
            inputs_hash: "inputs".into(),
            impact_generation: "gen-1".into(),
            status: "passed".into(),
            exit_code: 0,
            duration_ms: 10,
            output_ref: None,
            output_bytes: 0,
            covered_paths: Vec::new(),
            warnings: Vec::new(),
            warning_count: 0,
            failure_count: 0,
            created_at: now,
            expires_at: "2099-01-01T00:00:00Z".into(),
            hit_count: 0,
            last_hit_at: None,
        };
        engine
            .store()
            .replace_test_results(&session.id, &[entry])
            .unwrap();

        let readiness = engine.session_readiness(&session.id).await.unwrap();
        assert_eq!(readiness.latest_test_passed, Some(true), "{readiness:?}");
        assert!(
            readiness
                .latest_test
                .as_deref()
                .is_some_and(|test| test.contains("test passed")),
            "{readiness:?}"
        );

        let tradeoffs = engine
            .session_tradeoffs(vp_protocol::types::SessionTradeoffsParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert!(
            tradeoffs
                .quality
                .reasons
                .contains(&"tests_passed".to_string()),
            "{:?}",
            tradeoffs.quality.reasons
        );
        assert!(
            tradeoffs
                .confidence
                .reasons
                .contains(&"passing test evidence".to_string()),
            "{:?}",
            tradeoffs.confidence.reasons
        );
    }

    #[tokio::test]
    async fn run_contract_override_is_event_backed_and_bounded() {
        let (_data, _repo, engine, session) = fixture().await;
        let before = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap()
            .len();
        let view = engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Quality,
                completion_reserve_ratio: Some(0.30),
                attention_cap: Some(5),
                stop_policy: Some(RunStopPolicy {
                    no_progress_turns: 3,
                    ..RunStopPolicy::default()
                }),
                updated_by: Some("operator".into()),
            })
            .await
            .unwrap();
        assert_eq!(view.profile, "quality");
        assert_eq!(view.source, "session_override");
        assert_eq!(view.completion_reserve_ratio, 0.30);
        assert_eq!(view.attention_cap, 5);
        assert_eq!(view.stop_policy.no_progress_turns, 3);
        assert!(
            view.warnings
                .iter()
                .any(|warning| warning.contains("override"))
        );

        let stored_override = engine
            .run_contract_override(&session.id)
            .await
            .unwrap()
            .expect("override");
        assert_eq!(stored_override.profile, RunProfile::Quality);
        assert_eq!(stored_override.updated_by.as_deref(), Some("operator"));
        let resources = engine.session_resources(&session.id).await.unwrap();
        assert_eq!(resources.profile, "quality");
        assert_eq!(
            resources
                .dimension("attention")
                .and_then(|dimension| dimension.hard_cap),
            Some(5.0)
        );
        let after = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap()
            .len();
        assert_eq!(after, before + 1);
    }

    #[tokio::test]
    async fn project_run_policy_supplies_defaults_and_session_override_wins() {
        let (_data, _repo, engine, session) = fixture().await;
        let initial = engine.run_contract_view(&session.id).await.unwrap();
        assert_eq!(initial.source, "derived");
        assert_eq!(initial.profile, "balanced");
        assert_eq!(initial.completion_reserve_ratio, 0.20);
        assert_eq!(initial.attention_cap, 20);

        let policy = ProjectRunPolicy {
            default_profile: RunProfile::Fast,
            completion_reserve_ratio: 0.30,
            attention_cap: 7,
            stop_policy: RunStopPolicy {
                no_progress_turns: 4,
                ..RunStopPolicy::default()
            },
            auto_rescue: false,
            auto_reallocate_envelopes: false,
        };
        let saved = engine
            .set_project_run_policy(SetProjectRunPolicyParams {
                project_id: session.project_id.clone(),
                policy: policy.clone(),
            })
            .await
            .unwrap();
        assert_eq!(saved.policy, policy);
        assert!(saved.updated_at.is_some());

        let project_contract = engine.run_contract_view(&session.id).await.unwrap();
        assert_eq!(project_contract.source, "project_policy");
        assert_eq!(project_contract.profile, "fast");
        assert_eq!(project_contract.completion_reserve_ratio, 0.30);
        assert_eq!(project_contract.attention_cap, 7);
        assert_eq!(project_contract.stop_policy.no_progress_turns, 4);

        let project_resources = engine.session_resources(&session.id).await.unwrap();
        assert_eq!(project_resources.profile, "fast");
        assert_eq!(
            project_resources
                .dimension("attention")
                .and_then(|dimension| dimension.hard_cap),
            Some(7.0)
        );

        // A session override still wins field-by-field; unspecified optional
        // fields inherit the project policy instead of hardcoded defaults.
        let overridden = engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Quality,
                completion_reserve_ratio: None,
                attention_cap: None,
                stop_policy: None,
                updated_by: Some("operator".into()),
            })
            .await
            .unwrap();
        assert_eq!(overridden.source, "session_override");
        assert_eq!(overridden.profile, "quality");
        assert_eq!(overridden.completion_reserve_ratio, 0.30);
        assert_eq!(overridden.attention_cap, 7);
        assert_eq!(overridden.stop_policy.no_progress_turns, 4);

        let overridden_resources = engine.session_resources(&session.id).await.unwrap();
        assert_eq!(overridden_resources.profile, "quality");
        assert_eq!(
            overridden_resources
                .dimension("attention")
                .and_then(|dimension| dimension.hard_cap),
            Some(7.0)
        );
    }

    #[tokio::test]
    async fn invalid_run_contract_override_fails_closed() {
        let (_data, _repo, engine, session) = fixture().await;
        let error = engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Balanced,
                completion_reserve_ratio: Some(0.9),
                attention_cap: Some(0),
                stop_policy: None,
                updated_by: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("reserve ratio"));
        assert!(
            engine
                .run_contract_override(&session.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn governor_pauses_on_attention_cap_and_completion_reserve() {
        let (_data, _repo, engine, session) = fixture().await;
        let quiet = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(quiet.action, "continue", "{quiet:?}");

        engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Balanced,
                completion_reserve_ratio: None,
                attention_cap: Some(1),
                stop_policy: None,
                updated_by: None,
            })
            .await
            .unwrap();
        let (_approval, _receiver) = engine
            .request_approval_with_risk(
                &session.id,
                "call-resource-1",
                "shell",
                json!({"command": "git status"}),
                None,
                Some("host_state"),
                Some("test attention"),
            )
            .await
            .unwrap();
        let attention = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(attention.action, "defer", "{attention:?}");
        assert_eq!(attention.reason, "attention_cap");
        engine.record_resource_decision(&attention).await.unwrap();
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == SESSION_RESOURCE_DECISION_EVENT)
        );

        let _ = engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(1_000),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        engine.record_usage(&session.id, 900, 0).await.unwrap();
        engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Balanced,
                completion_reserve_ratio: Some(0.2),
                attention_cap: Some(100),
                stop_policy: None,
                updated_by: None,
            })
            .await
            .unwrap();
        let reserve = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(reserve.action, "defer", "{reserve:?}");
        assert_eq!(reserve.reason, "completion_reserve_tokens");
    }

    #[tokio::test]
    async fn unlimited_bypasses_economic_governor_but_keeps_attention() {
        let (_data, _repo, engine, session) = fixture().await;
        engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(1_000),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        engine.record_usage(&session.id, 950, 0).await.unwrap();
        let capped = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(capped.reason, "completion_reserve_tokens", "{capped:?}");

        engine
            .set_session_unlimited(&session.id, true)
            .await
            .unwrap();
        let unlimited_dispatch = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert!(
            !unlimited_dispatch.should_block(),
            "unlimited must bypass retained economic caps: {unlimited_dispatch:?}"
        );
        let unlimited_turn = engine
            .resource_governor_decision(&session.id, "turn")
            .await
            .unwrap();
        assert!(
            !unlimited_turn.should_block(),
            "unlimited must not pause on retained economic caps: {unlimited_turn:?}"
        );

        engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Balanced,
                completion_reserve_ratio: None,
                attention_cap: Some(1),
                stop_policy: None,
                updated_by: None,
            })
            .await
            .unwrap();
        let (_approval, _receiver) = engine
            .request_approval_with_risk(
                &session.id,
                "call-unlimited-attention",
                "shell",
                json!({"command": "git status"}),
                None,
                Some("host_state"),
                Some("unlimited attention"),
            )
            .await
            .unwrap();
        let attention = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(
            attention.reason, "attention_cap",
            "unlimited does not bypass attention: {attention:?}"
        );
    }

    #[tokio::test]
    async fn fixed_top_up_clamps_to_the_existing_hard_cap() {
        let (_data, _repo, engine, session) = fixture().await;
        let _ = engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(1_000),
                hard_token_cap: Some(1_200),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        let (budget, capped) = engine
            .top_up_session_budget(TopUpSessionResourcesParams {
                session_id: session.id.clone(),
                add_tokens: Some(500),
                ..TopUpSessionResourcesParams::default()
            })
            .await
            .unwrap();
        assert!(capped);
        assert_eq!(budget.effective_token_budget, Some(1_200));
        assert_eq!(budget.hard_token_cap, Some(1_200));
    }

    #[tokio::test]
    async fn turn_governor_attention_and_hard_token_limits_are_lean() {
        let (_data, _repo, engine, session) = fixture().await;
        let quiet = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(quiet.action, "continue", "{quiet:?}");
        assert_eq!(quiet.phase, "turn");
        let quiet_states = quiet.details["states"].as_array().unwrap();
        assert!(quiet_states.iter().any(|state| state["key"] == "attention"));
        assert!(
            quiet_states.iter().all(|state| state["key"] != "capacity"
                && state["key"] != "quality"
                && state["key"] != "progress"),
            "the turn payload must not carry dispatch-only dimensions: {quiet_states:?}"
        );

        engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Balanced,
                completion_reserve_ratio: None,
                attention_cap: Some(1),
                stop_policy: None,
                updated_by: None,
            })
            .await
            .unwrap();
        let (_approval, _receiver) = engine
            .request_approval_with_risk(
                &session.id,
                "call-turn-attention",
                "shell",
                json!({"command": "git status"}),
                None,
                Some("host_state"),
                Some("turn attention"),
            )
            .await
            .unwrap();
        let attention = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(attention.action, "defer", "{attention:?}");
        assert_eq!(attention.reason, "attention_cap");
        assert_eq!(attention.details["phase"], "turn");
        let dispatch = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(dispatch.action, "defer", "{dispatch:?}");
        assert_eq!(dispatch.reason, "attention_cap");

        engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Balanced,
                completion_reserve_ratio: None,
                attention_cap: Some(100),
                stop_policy: None,
                updated_by: None,
            })
            .await
            .unwrap();
        engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(1_000),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        record_usage(&engine, &session.id, 1_000, 0).await;
        let tokens = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(tokens.action, "defer", "{tokens:?}");
        assert_eq!(tokens.reason, "resource_cap_tokens");
        let dispatch = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(dispatch.action, "defer", "{dispatch:?}");
        assert_eq!(dispatch.reason, "resource_cap_tokens");
    }

    #[tokio::test]
    async fn turn_attention_batch_is_bounded_and_excludes_raw_input() {
        let (_data, _repo, engine, session) = fixture().await;
        engine
            .set_run_contract(SetRunContractParams {
                session_id: session.id.clone(),
                profile: RunProfile::Balanced,
                completion_reserve_ratio: None,
                attention_cap: Some(1),
                stop_policy: None,
                updated_by: None,
            })
            .await
            .unwrap();
        let mut receivers = Vec::new();
        for index in 0..10 {
            let (approval, receiver) = engine
                .request_approval_with_risk(
                    &session.id,
                    &format!("call-batch-{index}"),
                    "shell",
                    json!({
                        "command": format!("echo secret-token-{index}"),
                        "path": format!("src/f{index}.rs"),
                    }),
                    None,
                    Some("host_state"),
                    Some("batch test approval"),
                )
                .await
                .unwrap();
            assert_eq!(approval.status, "pending");
            receivers.push(receiver);
        }

        let decision = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(decision.reason, "attention_cap");
        let batch = &decision.details["attention_batch"];
        assert_eq!(batch["schema"], "verlinpark.attention_batch/v1");
        assert_eq!(batch["pending"], 10);
        assert_eq!(batch["cap"], 1);
        assert_eq!(batch["items"].as_array().unwrap().len(), 8);
        assert_eq!(batch["truncated"], true);
        assert_eq!(batch["by_risk"][0]["risk"], "host_state");
        assert_eq!(batch["by_risk"][0]["count"], 10);
        assert_eq!(batch["items"][0]["tool"], "shell");
        assert!(batch["items"][0].get("input").is_none());

        let serialized = decision.details.to_string();
        assert!(!serialized.contains("secret-token"));
        assert!(!serialized.contains("src/f0.rs"));
    }

    #[tokio::test]
    async fn turn_governor_completion_reserve_and_warn_match_dispatch() {
        let (_data, _repo, engine, session) = fixture().await;
        engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(1_000),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        record_usage(&engine, &session.id, 850, 0).await;
        let reserve = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(reserve.action, "defer", "{reserve:?}");
        assert_eq!(reserve.reason, "completion_reserve_tokens");
        assert_eq!(reserve.details["completion_reserve_ratio"], 0.2);
        let dispatch = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(dispatch.action, "defer", "{dispatch:?}");
        assert_eq!(dispatch.reason, "completion_reserve_tokens");

        let (_warn_data, _warn_repo, warn_engine, warn_session) = fixture().await;
        warn_engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: warn_session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(1_000),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        record_usage(&warn_engine, &warn_session.id, 800, 0).await;
        let warning = warn_engine
            .resource_governor_turn_decision(&warn_session.id)
            .await
            .unwrap();
        assert_eq!(warning.action, "warn", "{warning:?}");
        assert_eq!(warning.reason, "soft_resource_limit");
        let dispatch = warn_engine
            .resource_governor_decision(&warn_session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(dispatch.action, "warn", "{dispatch:?}");
        assert_eq!(dispatch.reason, "soft_resource_limit");
    }

    #[tokio::test]
    async fn turn_governor_context_hard_limit_matches_dispatch() {
        let (_data, _repo, engine, session) = fixture().await;
        engine
            .append_event(
                &session.id,
                "context.usage",
                json!({
                    "estimated_tokens": 9_000,
                    "context_window": 10_000,
                    "threshold_tokens": 7_500,
                    "hard_limit_tokens": 9_000,
                    "threshold_source": "window_ratio",
                    "threshold_ratio_basis_points": 9000,
                    "messages": 10,
                    "calibration": 1.0,
                }),
            )
            .await
            .unwrap();
        let context = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(context.action, "defer", "{context:?}");
        assert_eq!(context.reason, "resource_cap_context");
        let dispatch = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(dispatch.action, "defer", "{dispatch:?}");
        assert_eq!(dispatch.reason, "resource_cap_context");
    }

    #[tokio::test]
    async fn readiness_blocked_stays_a_dispatch_only_gate() {
        let (_data, _repo, engine, session) = fixture().await;
        let plan = crate::effect::classify_tool_effect(
            "remote_write",
            &json!({"path": "out/app.txt", "content": "deploy"}),
            crate::permissions::RiskLevel::External,
            false,
            &vp_protocol::types::EffectPolicy::default(),
        )
        .unwrap()
        .expect("remote write plan");
        let prepare = engine
            .prepare_tool_effect(&session.id, Some("T1"), "agent-1", "", &plan)
            .await
            .unwrap();
        let intent = match prepare {
            crate::effect::EffectPrepare::Proceed(receipt) => receipt,
            other => panic!("expected proceed, got {other:?}"),
        };
        engine.begin_effect(intent).await.unwrap();

        let readiness = engine.session_readiness(&session.id).await.unwrap();
        assert_eq!(readiness.level, "blocked", "{readiness:?}");
        let turn = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(turn.action, "continue", "{turn:?}");
        assert_ne!(turn.reason, "readiness_blocked");
        assert_eq!(turn.bottleneck.as_deref(), Some("risk"));

        let dispatch = engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
            .unwrap();
        assert_eq!(dispatch.action, "defer", "{dispatch:?}");
        assert_eq!(dispatch.reason, "readiness_blocked");
    }

    #[test]
    fn attention_weights_risk_and_unknown_effects() {
        let approval: Approval = serde_json::from_value(json!({
            "id": "ap-1",
            "session_id": "s-1",
            "tool_call_id": "call-1",
            "tool": "shell",
            "input": {},
            "status": "pending",
            "created_at": "2026-11-24T00:00:00Z",
            "approval_risk": "host_state"
        }))
        .unwrap();
        let denied: Approval = serde_json::from_value(json!({
            "id": "ap-2",
            "session_id": "s-1",
            "tool_call_id": "call-2",
            "tool": "shell",
            "input": {},
            "status": "denied",
            "created_at": "2026-11-24T00:00:00Z",
            "approval_risk": "destructive"
        }))
        .unwrap();
        let (attention, risk) = resource_attention(&[approval, denied], 1);
        assert_eq!(attention.pending, 1);
        assert_eq!(attention.denied, 1);
        assert_eq!(attention.interrupts, 8);
        assert_eq!(risk, 13);
        assert_eq!(attention.by_risk.len(), 2);
    }

    #[tokio::test]
    async fn latest_merge_attempt_supersedes_an_earlier_failed_entry() {
        let (_data, _repo, engine, session) = fixture().await;
        let store = engine.store().clone();
        let first = store
            .enqueue_merge(
                &session.id,
                "agent-1",
                "Worker",
                Some("T1"),
                "branch-1",
                "commit-1",
                "base",
                Some("first"),
            )
            .unwrap();
        store
            .update_merge_queue_entry(&first.id, "failed", Some("first failed"))
            .unwrap()
            .expect("first entry");
        let second = store
            .enqueue_merge(
                &session.id,
                "agent-1",
                "Worker",
                Some("T1"),
                "branch-2",
                "commit-2",
                "base",
                Some("second"),
            )
            .unwrap();
        store
            .update_merge_queue_entry(&second.id, "merged", Some("second merged"))
            .unwrap()
            .expect("second entry");
        let queue = engine.list_merge_queue(&session.id).await.unwrap();
        let active = latest_merge_entries(&queue);
        assert_eq!(active.len(), 1, "{active:?}");
        assert_eq!(active[0].status, "merged", "{active:?}");
        let readiness = engine.compute_session_readiness(&session.id).await.unwrap();
        assert_ne!(
            readiness.level, "blocked",
            "a superseded failed attempt must not keep readiness blocked: {readiness:?}"
        );
    }

    #[test]
    fn readiness_cache_hit_requires_the_same_event_seq_and_short_ttl() {
        let now = Instant::now();
        let snapshot = ReadinessSnapshot {
            session_id: "ses-1".into(),
            level: "committed".into(),
            shippable: false,
            mergeable: false,
            latest_commit: Some("abc123".into()),
            latest_test: None,
            latest_test_passed: None,
            verifier: None,
            merge: None,
            changed_paths: Vec::new(),
            remaining_work: Vec::new(),
            blockers: Vec::new(),
            partial_delivery: None,
            generated_at: "2026-11-25T00:00:00Z".into(),
        };
        let cached = CachedReadiness {
            seq: 9,
            cached_at: now - Duration::from_millis(500),
            snapshot,
        };
        assert!(readiness_cache_hit(&cached, 9, now));
        assert!(!readiness_cache_hit(&cached, 10, now));
        assert!(!readiness_cache_hit(
            &cached,
            9,
            now + READINESS_CACHE_TTL + Duration::from_secs(1)
        ));
    }
}
