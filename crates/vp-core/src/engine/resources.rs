//! Read-only Run Contract, resource, and readiness views.
//!
//! R1 is deliberately observe-only: it derives bounded snapshots from the
//! existing budget, usage, approval, task, merge, effect, and worktree state.
//! It does not pause, spend, approve, merge, or route anything.

use super::*;

use std::collections::BTreeMap;
use std::path::PathBuf;
use vp_protocol::types::{
    AutoBudgetCaps, ReadinessSnapshot, ResourceAttention, ResourceCount, ResourceDimension,
    ResourceSnapshot, ResourceSoftTargets, RunContractView, RunStopPolicy,
};

impl Engine {
    async fn resource_session(&self, session_id: &str) -> Result<Session> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))
    }

    /// Bounded, newest-first readiness projection. It is intentionally
    /// conservative: `verified`/`merged` come from the merge queue and effect
    /// receipts, not from an agent's prose claim.
    pub async fn session_readiness(&self, session_id: &str) -> Result<ReadinessSnapshot> {
        let session = self.resource_session(session_id).await?;
        let effects = self.session_effects(session_id, None, false, 200).await?;
        let queue = self.list_merge_queue(session_id).await?;
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let approvals = self.list_approvals(Some(session_id.to_string())).await?;

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
        for entry in &queue {
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

        let any_merged = queue.iter().any(|entry| entry.status == "merged");
        let any_verified = queue
            .iter()
            .any(|entry| matches!(entry.status.as_str(), "queued" | "merging" | "merged"));
        let level = if effects.blocked
            || queue
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
        let latest_commit = queue
            .iter()
            .rev()
            .find(|entry| entry.status == "merged")
            .map(|entry| entry.commit.clone())
            .or(head);
        let verifier = queue
            .iter()
            .rev()
            .find(|entry| matches!(entry.status.as_str(), "queued" | "merging" | "merged"))
            .map(|entry| {
                if entry.status == "merged" {
                    "approved · merged".to_string()
                } else {
                    "approved · queue entry".to_string()
                }
            });
        let merge = queue.last().map(|entry| entry.status.clone());

        Ok(ReadinessSnapshot {
            session_id: session_id.to_string(),
            level: level.to_string(),
            shippable: matches!(level, "verified" | "merged") && blockers.is_empty(),
            mergeable: matches!(level, "verified" | "merged") && blockers.is_empty(),
            latest_commit,
            latest_test: None,
            verifier,
            merge,
            changed_paths,
            remaining_work,
            blockers,
            generated_at: now_rfc3339(),
        })
    }

    /// Bounded multi-resource snapshot. All values are derived; a missing
    /// price or test signal is reported as unavailable, never fabricated.
    pub async fn session_resources(&self, session_id: &str) -> Result<ResourceSnapshot> {
        let readiness = self.session_readiness(session_id).await?;
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

        let mut dimensions = Vec::new();
        dimensions.push(resource_money(&budget, cost, pricing_source));
        dimensions.push(resource_tokens(
            "tokens",
            "Raw tokens",
            raw_tokens as f64,
            budget.remaining_tokens,
            budget.hard_token_cap.or(budget.effective_token_budget),
            "usage_ledger",
            "Cumulative input + output tokens; includes cache-read/write subsets.",
        ));
        dimensions.push(resource_tokens(
            "weighted_tokens",
            "Weighted tokens",
            weighted_tokens,
            None,
            None,
            "usage_ledger",
            "Cache-aware equivalent (cache read 0.1x, cache write 1.25x, output 1x). Not a hard cap.",
        ));
        dimensions.push(resource_time(&budget));
        dimensions.push(resource_attention_dim(&attention, 20));
        let risk_state = if effects.blocked {
            "hard_limit"
        } else if risk_used > 0 {
            "watch"
        } else {
            "ok"
        };
        dimensions.push(ResourceDimension {
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
        });
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
                "{completed_tasks}/{total_tasks} tasks completed; {merged} merge(s); test evidence is not inferred in R1"
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
            "R1 resource governance is observe-only: no automatic pause, reallocation, or top-up yet."
                .to_string(),
        );

        Ok(ResourceSnapshot {
            session_id: session_id.to_string(),
            generated_at: now_rfc3339(),
            profile: "balanced".to_string(),
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
        let hard_token = budget.hard_token_cap.or(budget.effective_token_budget);
        let hard_cost = budget
            .hard_cost_cap_usd
            .or(budget.effective_cost_budget_usd);
        let hard_time = budget
            .hard_wall_clock_secs
            .or(budget.effective_time_budget_secs);
        let mut warnings = Vec::new();
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
            profile: "balanced".to_string(),
            source: "derived".to_string(),
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
                attention: Some(16),
            },
            completion_reserve_ratio: 0.20,
            attention_cap: 20,
            stop_policy: RunStopPolicy::default(),
            warnings,
            generated_at: now_rfc3339(),
        })
    }
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
    use vp_protocol::types::{PlanDepartment, PlanTask, TeamPlan};

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
                .any(|warning| warning.contains("R1"))
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
}
