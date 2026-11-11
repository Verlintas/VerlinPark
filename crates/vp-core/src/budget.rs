use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Token and USD spend observed from the persisted usage ledger.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spend {
    pub tokens: u64,
    pub cost_usd: f64,
}

/// A task or department budget that a provider call must fit inside.
#[derive(Debug, Clone)]
pub struct ScopeRequest {
    /// Stable registry key, for example `session:<id>:task:<task>` or
    /// `project:<id>`.
    pub pool_key: String,
    pub scope: &'static str,
    pub id: String,
    pub limit_tokens: Option<u64>,
    pub limit_cost: Option<f64>,
    pub observed: Spend,
    pub input_price_per_m: f64,
    pub output_price_per_m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetReason {
    Tokens,
    Cost,
    /// A non-critical request would consume the critical-path reserve pool.
    Reserved,
    /// The session is inside the governor's throttle zone.
    Throttled,
}

impl BudgetReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::Cost => "cost",
            Self::Reserved => "reserved",
            Self::Throttled => "throttled",
        }
    }

    /// Stable Q3 error-contract code carried into the agent budget stop.
    pub fn code(self) -> &'static str {
        match self {
            Self::Tokens | Self::Cost => "BUDGET_EXHAUSTED",
            Self::Reserved => "BUDGET_RESERVED",
            Self::Throttled => "BUDGET_THROTTLED",
        }
    }
}

/// Read-only reservation gate derived from the budget governor. The gate
/// accompanies the normal scope requests for one provider call; it can only
/// lower the granted output or reject a non-critical request, never widen a
/// hard budget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GovernorGate {
    pub enabled: bool,
    pub task_id: Option<String>,
    pub critical: bool,
    pub remaining_tokens: Option<u64>,
    pub protected_tokens: u64,
    pub throttled: bool,
    pub min_task_budget: u64,
    pub policy_fingerprint: String,
}

impl GovernorGate {
    /// Maximum output tokens this non-critical request may use without
    /// breaching the protected pool. `Ok(None)` means the gate does not apply
    /// (disabled, critical, or unbounded); `Err` is a typed deferral reason.
    pub fn available_output(
        &self,
        input_estimate: u64,
        min_output: u64,
    ) -> Result<Option<u64>, BudgetReason> {
        if !self.enabled || self.critical {
            return Ok(None);
        }
        let Some(remaining) = self.remaining_tokens else {
            return Ok(None);
        };
        if self.throttled {
            return Err(BudgetReason::Throttled);
        }
        if remaining < self.min_task_budget {
            return Err(BudgetReason::Reserved);
        }
        let available = remaining
            .saturating_sub(self.protected_tokens)
            .saturating_sub(input_estimate);
        if available < min_output {
            return Err(BudgetReason::Reserved);
        }
        Ok(Some(available))
    }
}

#[derive(Debug, Clone)]
pub struct Rejection {
    pub scope: &'static str,
    pub id: String,
    pub reason: BudgetReason,
    pub limit_tokens: Option<u64>,
    pub spent_tokens: u64,
    pub reserved_tokens: u64,
    pub limit_cost: Option<f64>,
    pub spent_cost: f64,
    pub reserved_cost: f64,
    /// Critical-path tokens held back from non-critical work at rejection time.
    pub protected_tokens: u64,
}

#[derive(Debug, Default)]
struct Pool {
    limit_tokens: Option<u64>,
    limit_cost: Option<f64>,
    observed_tokens: u64,
    observed_cost: f64,
    reserved_tokens: u64,
    reserved_cost: f64,
}

/// Daemon-wide reservations for plan task/department budgets.
///
/// The usage ledger is the authoritative spent total, but it is updated only
/// after a provider call. Concurrent tasks in one department would otherwise
/// each pass the ledger check and overshoot together. This registry holds
/// short-lived reservations for the duration of one provider call, so all
/// concurrent callers see the same remaining capacity.
#[derive(Debug, Default)]
pub struct BudgetRegistry {
    inner: Mutex<HashMap<String, Pool>>,
}

/// Releases the reservations for one provider call when dropped, including
/// cancellation and provider errors.
#[derive(Debug)]
pub struct BudgetLease {
    registry: Arc<BudgetRegistry>,
    keys: Vec<(String, u64, f64)>,
    granted_output_tokens: u64,
}

impl BudgetLease {
    pub fn granted_output_tokens(&self) -> u64 {
        self.granted_output_tokens
    }
}

impl Drop for BudgetLease {
    fn drop(&mut self) {
        if let Ok(mut pools) = self.registry.inner.lock() {
            for (key, tokens, cost) in &self.keys {
                if let Some(pool) = pools.get_mut(key) {
                    pool.reserved_tokens = pool.reserved_tokens.saturating_sub(*tokens);
                    pool.reserved_cost = (pool.reserved_cost - cost).max(0.0);
                }
            }
        }
    }
}

impl BudgetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant an output-token budget that fits every request. The returned lease
    /// must be held through the provider call and usage recording.
    pub fn grant(
        self: &Arc<Self>,
        requests: &[ScopeRequest],
        input_estimate: u64,
        desired_output: u64,
        min_output: u64,
    ) -> Result<BudgetLease, Rejection> {
        self.grant_governed(requests, None, input_estimate, desired_output, min_output)
    }

    /// Grant a budget subject to an optional critical-path gate. The governor
    /// can only reject or reduce a non-critical request below the normal hard
    /// budget; a critical request is never blocked by the reserve pool.
    pub fn grant_governed(
        self: &Arc<Self>,
        requests: &[ScopeRequest],
        governor: Option<&GovernorGate>,
        input_estimate: u64,
        desired_output: u64,
        min_output: u64,
    ) -> Result<BudgetLease, Rejection> {
        if requests.is_empty() {
            return Ok(BudgetLease {
                registry: self.clone(),
                keys: Vec::new(),
                granted_output_tokens: desired_output,
            });
        }
        let mut pools = match self.inner.lock() {
            Ok(pools) => pools,
            Err(_) => {
                return Err(Rejection {
                    scope: "registry",
                    id: "budget-registry".to_string(),
                    reason: BudgetReason::Tokens,
                    limit_tokens: None,
                    spent_tokens: 0,
                    reserved_tokens: 0,
                    limit_cost: None,
                    spent_cost: 0.0,
                    reserved_cost: 0.0,
                    protected_tokens: 0,
                });
            }
        };
        let mut keys = Vec::with_capacity(requests.len());
        for request in requests {
            let key = request.pool_key.clone();
            let pool = pools.entry(key.clone()).or_default();
            // Limits are part of the current configuration, not sticky
            // history: removing a project/task/department budget must clear the
            // old pool limit instead of keeping the previous value alive.
            pool.limit_tokens = request.limit_tokens;
            pool.limit_cost = request.limit_cost;
            pool.observed_tokens = pool.observed_tokens.max(request.observed.tokens);
            pool.observed_cost = pool.observed_cost.max(request.observed.cost_usd);
            keys.push((key, request));
        }

        let mut governor_cap: Option<u64> = None;
        if let Some(gate) = governor {
            match gate.available_output(input_estimate, min_output) {
                Ok(cap) => governor_cap = cap,
                Err(reason) => {
                    return Err(first_governor_rejection(
                        &pools,
                        &keys,
                        reason,
                        gate.protected_tokens,
                    ));
                }
            }
        }

        let mut granted = desired_output;
        let mut limiting: Option<(&'static str, &str, BudgetReason)> = None;
        for (key, request) in &keys {
            let Some(pool) = pools.get(key) else {
                continue;
            };
            if let Some(limit) = pool.limit_tokens {
                let used = pool.observed_tokens.saturating_add(pool.reserved_tokens);
                if limit
                    <= used
                        .saturating_add(input_estimate)
                        .saturating_add(min_output)
                {
                    return Err(rejection(pool, request, BudgetReason::Tokens));
                }
                let available_output = limit - used - input_estimate;
                if available_output < granted {
                    granted = available_output;
                    limiting = Some((request.scope, request.id.as_str(), BudgetReason::Tokens));
                }
            }
            if let Some(limit) = pool.limit_cost {
                let used = pool.observed_cost + pool.reserved_cost;
                let remaining = limit - used;
                if remaining <= 0.0 {
                    return Err(rejection(pool, request, BudgetReason::Cost));
                }
                let input_cost = token_cost(input_estimate, request.input_price_per_m);
                if input_cost >= remaining && request.output_price_per_m <= 0.0 {
                    return Err(rejection(pool, request, BudgetReason::Cost));
                }
                if request.output_price_per_m > 0.0 {
                    if input_cost > remaining {
                        return Err(rejection(pool, request, BudgetReason::Cost));
                    }
                    let available_output = ((remaining - input_cost) * 1_000_000.0
                        / request.output_price_per_m)
                        .floor()
                        .max(0.0) as u64;
                    if available_output < granted {
                        granted = available_output;
                        limiting = Some((request.scope, request.id.as_str(), BudgetReason::Cost));
                    }
                }
            }
        }
        if let Some(cap) = governor_cap {
            granted = granted.min(cap);
        }

        if granted < min_output {
            if let Some((scope, id, reason)) = limiting
                && let Some((key, request)) = keys
                    .iter()
                    .find(|(_, request)| request.scope == scope && request.id == id)
                && let Some(pool) = pools.get(key)
            {
                return Err(rejection(pool, request, reason));
            }
            if let Some((key, request)) = keys.first()
                && let Some(pool) = pools.get(key)
            {
                return Err(rejection(pool, request, BudgetReason::Tokens));
            }
        }

        let mut planned = Vec::with_capacity(keys.len());
        for (key, request) in &keys {
            let Some(pool) = pools.get(key) else {
                continue;
            };
            let want_tokens = input_estimate.saturating_add(granted);
            let want_cost = token_cost(input_estimate, request.input_price_per_m)
                + token_cost(granted, request.output_price_per_m);
            if let Some(limit) = pool.limit_tokens {
                let used = pool.observed_tokens.saturating_add(pool.reserved_tokens);
                if used.saturating_add(want_tokens) > limit {
                    return Err(rejection(pool, request, BudgetReason::Tokens));
                }
            }
            if let Some(limit) = pool.limit_cost
                && pool.observed_cost + pool.reserved_cost + want_cost > limit + f64::EPSILON
            {
                return Err(rejection(pool, request, BudgetReason::Cost));
            }
            planned.push((key.clone(), want_tokens, want_cost));
        }

        for (key, tokens, cost) in &planned {
            if let Some(pool) = pools.get_mut(key) {
                pool.reserved_tokens = pool.reserved_tokens.saturating_add(*tokens);
                pool.reserved_cost += cost;
            }
        }

        Ok(BudgetLease {
            registry: self.clone(),
            keys: planned,
            granted_output_tokens: granted,
        })
    }

    /// Remove all reservations and limits for a finished session. Live leases
    /// may still release into an absent entry; that release is ignored.
    pub fn clear_session(&self, session_id: &str) {
        let prefix = format!("session:{session_id}:");
        if let Ok(mut pools) = self.inner.lock() {
            pools.retain(|key, _| !key.starts_with(&prefix));
        }
    }

    pub fn clear_project(&self, project_id: &str) {
        let key = format!("project:{project_id}");
        if let Ok(mut pools) = self.inner.lock() {
            pools.remove(&key);
        }
    }

    /// Tokens currently reserved by in-flight provider calls for one session.
    /// Project pools are shared across sessions and are intentionally excluded.
    pub fn reserved_for_session(&self, session_id: &str) -> u64 {
        let prefix = format!("session:{session_id}:");
        self.inner
            .lock()
            .map(|pools| {
                pools
                    .iter()
                    .filter(|(key, _)| key.starts_with(&prefix))
                    .map(|(_, pool)| pool.reserved_tokens)
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Tokens currently reserved by in-flight provider calls against a project.
    pub fn reserved_for_project(&self, project_id: &str) -> u64 {
        let key = format!("project:{project_id}");
        self.inner
            .lock()
            .map(|pools| {
                pools
                    .get(&key)
                    .map(|pool| pool.reserved_tokens)
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }
}

fn first_governor_rejection(
    pools: &HashMap<String, Pool>,
    keys: &[(String, &ScopeRequest)],
    reason: BudgetReason,
    protected_tokens: u64,
) -> Rejection {
    if let Some((key, request)) = keys.first()
        && let Some(pool) = pools.get(key)
    {
        let mut rejection = rejection(pool, request, reason);
        rejection.protected_tokens = protected_tokens;
        return rejection;
    }
    Rejection {
        scope: "governor",
        id: "critical-path".to_string(),
        reason,
        limit_tokens: None,
        spent_tokens: 0,
        reserved_tokens: 0,
        limit_cost: None,
        spent_cost: 0.0,
        reserved_cost: 0.0,
        protected_tokens,
    }
}

fn rejection(pool: &Pool, request: &ScopeRequest, reason: BudgetReason) -> Rejection {
    Rejection {
        scope: request.scope,
        id: request.id.clone(),
        reason,
        limit_tokens: pool.limit_tokens,
        spent_tokens: pool.observed_tokens,
        reserved_tokens: pool.reserved_tokens,
        limit_cost: pool.limit_cost,
        spent_cost: pool.observed_cost,
        reserved_cost: pool.reserved_cost,
        protected_tokens: 0,
    }
}

fn token_cost(tokens: u64, price_per_million: f64) -> f64 {
    if !price_per_million.is_finite() || price_per_million <= 0.0 {
        return 0.0;
    }
    tokens as f64 * price_per_million / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(scope: &'static str, id: &str, tokens: u64) -> ScopeRequest {
        ScopeRequest {
            pool_key: format!("{scope}:{id}"),
            scope,
            id: id.to_string(),
            limit_tokens: Some(tokens),
            limit_cost: None,
            observed: Spend::default(),
            input_price_per_m: 0.0,
            output_price_per_m: 0.0,
        }
    }

    #[test]
    fn concurrent_grants_share_one_department_budget() {
        let registry = Arc::new(BudgetRegistry::new());
        let department = request("department", "impl", 100);
        let first = registry
            .grant(std::slice::from_ref(&department), 10, 50, 10)
            .unwrap();
        // 50 output leaves 40 after the first reservation (100 - 10 - 50).
        let second = registry
            .grant(std::slice::from_ref(&department), 10, 50, 10)
            .unwrap();
        assert_eq!(first.granted_output_tokens(), 50);
        assert_eq!(second.granted_output_tokens(), 30);
        assert!(registry.grant(&[department], 10, 50, 10).is_err());
        drop(first);
        drop(second);
        assert!(
            registry
                .grant(&[request("department", "impl", 100)], 10, 50, 10)
                .is_ok()
        );
    }

    #[test]
    fn task_and_department_reservations_are_atomic() {
        let registry = Arc::new(BudgetRegistry::new());
        let task = request("task", "T1", 100);
        let department = request("department", "impl", 100);
        let first = registry
            .grant(&[task.clone(), department.clone()], 10, 40, 10)
            .unwrap();
        assert_eq!(first.granted_output_tokens(), 40);
        let second = registry.grant(&[task, department], 10, 40, 10).unwrap();
        assert_eq!(second.granted_output_tokens(), 40);
    }

    #[test]
    fn cost_budget_lowers_output_budget() {
        let registry = Arc::new(BudgetRegistry::new());
        let mut request = request("task", "T1", 10_000);
        request.limit_cost = Some(0.001);
        request.observed = Spend {
            tokens: 0,
            cost_usd: 0.0,
        };
        request.input_price_per_m = 0.0;
        request.output_price_per_m = 10.0;
        // $0.001 buys 100 output tokens at $10 / 1M.
        let lease = registry.grant(&[request], 0, 500, 1).unwrap();
        assert_eq!(lease.granted_output_tokens(), 100);
    }

    #[test]
    fn concurrent_threads_cannot_overshoot_the_same_department() {
        let registry = Arc::new(BudgetRegistry::new());
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let results = Arc::new(Mutex::new(Vec::new()));
        let handles = (0..4)
            .map(|_| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                let results = results.clone();
                std::thread::spawn(move || {
                    let request = request("department", "impl", 100);
                    let lease = registry.grant(&[request], 10, 50, 10).ok();
                    let granted = lease.as_ref().map(BudgetLease::granted_output_tokens);
                    if let Ok(mut results) = results.lock() {
                        results.push(granted);
                    }
                    barrier.wait();
                    drop(lease);
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        let results = results.lock().unwrap();
        let granted = results.iter().flatten().copied().collect::<Vec<_>>();
        let used = granted.iter().sum::<u64>() + 10 * granted.len() as u64;
        assert!(used <= 100, "reserved {used} of 100: {results:?}");
    }

    #[test]
    fn observed_ledger_spend_is_never_lowered_by_a_stale_request() {
        let registry = Arc::new(BudgetRegistry::new());
        let mut stale = request("task", "T1", 100);
        stale.observed = Spend {
            tokens: 20,
            cost_usd: 0.0,
        };
        let lease = registry.grant(&[stale.clone()], 10, 50, 10).unwrap();
        assert_eq!(lease.granted_output_tokens(), 50);
        let mut newer = stale;
        newer.observed = Spend {
            tokens: 70,
            cost_usd: 0.0,
        };
        assert!(registry.grant(&[newer], 10, 50, 10).is_err());
    }

    fn gate(remaining: u64, protected: u64) -> GovernorGate {
        GovernorGate {
            enabled: true,
            task_id: Some("T2".to_string()),
            critical: false,
            remaining_tokens: Some(remaining),
            protected_tokens: protected,
            throttled: false,
            min_task_budget: 50,
            policy_fingerprint: "hash".to_string(),
        }
    }

    #[test]
    fn governor_gate_clamps_non_critical_output_to_free_headroom() {
        let registry = Arc::new(BudgetRegistry::new());
        let lease = registry
            .grant_governed(
                &[request("task", "T2", 10_000)],
                Some(&gate(1_000, 800)),
                100,
                500,
                10,
            )
            .unwrap();
        // 1,000 - 800 protected - 100 input leaves 100 output.
        assert_eq!(lease.granted_output_tokens(), 100);
    }

    #[test]
    fn governor_gate_rejects_when_reserve_leaves_no_minimum_output() {
        let registry = Arc::new(BudgetRegistry::new());
        let rejection = registry
            .grant_governed(
                &[request("task", "T2", 10_000)],
                Some(&gate(1_000, 980)),
                100,
                500,
                64,
            )
            .unwrap_err();
        assert_eq!(rejection.reason, BudgetReason::Reserved);
        assert_eq!(rejection.reason.code(), "BUDGET_RESERVED");
        assert_eq!(rejection.protected_tokens, 980);
    }

    #[test]
    fn governor_gate_rejects_inside_the_throttle_zone() {
        let registry = Arc::new(BudgetRegistry::new());
        let mut throttle = gate(1_000, 500);
        throttle.throttled = true;
        let rejection = registry
            .grant_governed(
                &[request("task", "T2", 10_000)],
                Some(&throttle),
                10,
                100,
                10,
            )
            .unwrap_err();
        assert_eq!(rejection.reason, BudgetReason::Throttled);
        assert_eq!(rejection.reason.code(), "BUDGET_THROTTLED");
    }

    #[test]
    fn governor_gate_never_blocks_a_critical_request() {
        let registry = Arc::new(BudgetRegistry::new());
        let mut critical = gate(1_000, 999);
        critical.critical = true;
        let lease = registry
            .grant_governed(
                &[request("task", "T2", 1_000)],
                Some(&critical),
                100,
                500,
                10,
            )
            .unwrap();
        // 1,000 - 100 input leaves 900 output; the reserve does not apply.
        assert_eq!(lease.granted_output_tokens(), 500);
    }

    #[test]
    fn governor_gate_rejects_below_min_task_budget() {
        let registry = Arc::new(BudgetRegistry::new());
        let rejection = registry
            .grant_governed(
                &[request("task", "T2", 10_000)],
                Some(&gate(40, 0)),
                10,
                100,
                10,
            )
            .unwrap_err();
        assert_eq!(rejection.reason, BudgetReason::Reserved);
    }

    #[test]
    fn disabled_governor_gate_preserves_normal_grant_behavior() {
        let registry = Arc::new(BudgetRegistry::new());
        let mut disabled = gate(100, 100);
        disabled.enabled = false;
        let lease = registry
            .grant_governed(
                &[request("task", "T2", 10_000)],
                Some(&disabled),
                10,
                500,
                10,
            )
            .unwrap();
        assert_eq!(lease.granted_output_tokens(), 500);
    }

    #[test]
    fn reserved_for_session_counts_only_session_pools() {
        let registry = Arc::new(BudgetRegistry::new());
        let mut task = request("task", "T1", 1_000);
        task.pool_key = "session:session-1:task:T1".to_string();
        let mut department = request("department", "impl", 1_000);
        department.pool_key = "session:session-1:department:impl".to_string();
        let project = ScopeRequest {
            pool_key: "project:p1".to_string(),
            scope: "project",
            id: "p1".to_string(),
            limit_tokens: Some(1_000),
            limit_cost: None,
            observed: Spend::default(),
            input_price_per_m: 0.0,
            output_price_per_m: 0.0,
        };
        let lease = registry
            .grant(&[task, department, project], 10, 100, 10)
            .unwrap();
        assert_eq!(registry.reserved_for_session("session-1"), 220);
        assert_eq!(registry.reserved_for_project("p1"), 110);
        drop(lease);
        assert_eq!(registry.reserved_for_session("session-1"), 0);
        assert_eq!(registry.reserved_for_project("p1"), 0);
    }
}
