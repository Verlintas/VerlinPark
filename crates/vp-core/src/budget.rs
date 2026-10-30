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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    session_id: String,
    scope: &'static str,
    id: String,
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
    inner: Mutex<HashMap<PoolKey, Pool>>,
}

/// Releases the reservations for one provider call when dropped, including
/// cancellation and provider errors.
pub struct BudgetLease {
    registry: Arc<BudgetRegistry>,
    keys: Vec<(PoolKey, u64, f64)>,
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
        session_id: &str,
        requests: &[ScopeRequest],
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
                    scope: "session",
                    id: session_id.to_string(),
                    reason: BudgetReason::Tokens,
                    limit_tokens: None,
                    spent_tokens: 0,
                    reserved_tokens: 0,
                    limit_cost: None,
                    spent_cost: 0.0,
                    reserved_cost: 0.0,
                });
            }
        };
        let mut keys = Vec::with_capacity(requests.len());
        for request in requests {
            let key = PoolKey {
                session_id: session_id.to_string(),
                scope: request.scope,
                id: request.id.clone(),
            };
            let pool = pools.entry(key.clone()).or_default();
            if request.limit_tokens.is_some() {
                pool.limit_tokens = request.limit_tokens;
            }
            if request.limit_cost.is_some() {
                pool.limit_cost = request.limit_cost;
            }
            pool.observed_tokens = pool.observed_tokens.max(request.observed.tokens);
            pool.observed_cost = pool.observed_cost.max(request.observed.cost_usd);
            keys.push((key, request));
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

        if granted < min_output {
            if let Some((scope, id, reason)) = limiting
                && let Some((key, request)) = keys
                    .iter()
                    .find(|(key, request)| key.scope == scope && request.id == id)
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
        if let Ok(mut pools) = self.inner.lock() {
            pools.retain(|key, _| key.session_id != session_id);
        }
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
            .grant("session", std::slice::from_ref(&department), 10, 50, 10)
            .unwrap();
        // 50 output leaves 40 after the first reservation (100 - 10 - 50).
        let second = registry
            .grant("session", std::slice::from_ref(&department), 10, 50, 10)
            .unwrap();
        assert_eq!(first.granted_output_tokens(), 50);
        assert_eq!(second.granted_output_tokens(), 30);
        assert!(
            registry
                .grant("session", &[department], 10, 50, 10)
                .is_err()
        );
        drop(first);
        drop(second);
        assert!(
            registry
                .grant("session", &[request("department", "impl", 100)], 10, 50, 10)
                .is_ok()
        );
    }

    #[test]
    fn task_and_department_reservations_are_atomic() {
        let registry = Arc::new(BudgetRegistry::new());
        let task = request("task", "T1", 100);
        let department = request("department", "impl", 100);
        let first = registry
            .grant("session", &[task.clone(), department.clone()], 10, 40, 10)
            .unwrap();
        assert_eq!(first.granted_output_tokens(), 40);
        let second = registry
            .grant("session", &[task, department], 10, 40, 10)
            .unwrap();
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
        let lease = registry.grant("session", &[request], 0, 500, 1).unwrap();
        assert_eq!(lease.granted_output_tokens(), 100);
    }

    #[test]
    fn observed_ledger_spend_is_never_lowered_by_a_stale_request() {
        let registry = Arc::new(BudgetRegistry::new());
        let mut stale = request("task", "T1", 100);
        stale.observed = Spend {
            tokens: 20,
            cost_usd: 0.0,
        };
        let lease = registry
            .grant("session", &[stale.clone()], 10, 50, 10)
            .unwrap();
        assert_eq!(lease.granted_output_tokens(), 50);
        let mut newer = stale;
        newer.observed = Spend {
            tokens: 70,
            cost_usd: 0.0,
        };
        assert!(registry.grant("session", &[newer], 10, 50, 10).is_err());
    }
}
