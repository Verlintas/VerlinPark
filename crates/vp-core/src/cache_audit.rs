//! Deterministic cache cost/latency audit over persisted session events.
//!
//! The audit never calls a provider and never infers a provider TTL fact. It
//! only reports patterns that are visible in `context.cache`,
//! `context.prompt_prefix`, and `context.compaction` events: a breakpoint count
//! above Anthropic's documented limit, cache writes without reads, repeated
//! tool/project churn, missing component hashes on old events, a warm call with
//! no follow-up, and compaction resets with their estimated invalidation cost.

use std::collections::{BTreeMap, BTreeSet};

use vp_protocol::types::{CachePolicy, Event, SessionCacheAuditFinding, SessionCacheAuditResult};

use crate::prompt_cache;

/// Audit a bounded event window. The caller supplies the latest provider and
/// policy so the report can explain which values were expected.
pub fn audit_cache_events(
    session_id: &str,
    provider: Option<&str>,
    policy: Option<&CachePolicy>,
    cache_events: &[Event],
    prefix_events: &[Event],
    compaction_events: &[Event],
) -> SessionCacheAuditResult {
    let mut findings = Vec::new();
    let aggregate = prompt_cache::aggregate_cache_events(cache_events, compaction_events);
    let calls_checked = aggregate.calls;
    let warm_calls_checked = aggregate.warm_calls;
    let calls_with_read = cache_events
        .iter()
        .filter(|event| payload_u64(event, "cache_read_tokens") > 0)
        .count() as u64;
    let cache_read_tokens = sum_u64(cache_events, "cache_read_tokens");
    let cache_write_tokens = sum_u64(cache_events, "cache_write_tokens");
    let input_tokens = sum_u64(cache_events, "input_tokens");
    let hit_ratio =
        vp_protocol::types::cache_hit_ratio(input_tokens, cache_read_tokens, cache_write_tokens);

    // 1. Anthropic's documented hard limit is four breakpoints per request.
    if let Some(event) = cache_events
        .iter()
        .find(|event| payload_u64(event, "cache_breakpoints") > 4)
    {
        let breakpoints = payload_u64(event, "cache_breakpoints");
        findings.push(finding(
            "breakpoint_limit_exceeded",
            "critical",
            format!(
                "a provider call requested {breakpoints} cache breakpoints; Anthropic accepts at most four"
            ),
            BTreeMap::from([
                ("breakpoints".to_string(), breakpoints.into()),
                ("at".to_string(), event.created_at.clone().into()),
            ]),
        ));
    }

    // 2. A generation that only ever wrote cache cannot have paid back.
    if calls_checked >= 2 && cache_write_tokens > 0 && cache_read_tokens == 0 {
        findings.push(finding(
            "cache_writes_without_reads",
            "warning",
            "multiple calls wrote prompt cache but none reported a cache read; the prefix may change too often or the TTL may be too short"
                .to_string(),
            BTreeMap::from([
                ("calls".to_string(), calls_checked.into()),
                ("cache_write_tokens".to_string(), cache_write_tokens.into()),
            ]),
        ));
    }

    // 3. Low hit ratio is only actionable once there are enough calls to
    //    distinguish a cold start from a systemic miss.
    if calls_checked >= 5 && hit_ratio < 0.2 {
        findings.push(finding(
            "low_cache_hit_ratio",
            "warning",
            format!(
                "cache-read share is {:.1}% over {calls_checked} calls; inspect miss reasons and prefix churn",
                hit_ratio * 100.0
            ),
            BTreeMap::from([
                ("hit_ratio".to_string(), hit_ratio.into()),
                ("calls".to_string(), calls_checked.into()),
            ]),
        ));
    }

    // 4. Project and tool hash churn are distinct causes and should not be
    //    collapsed into one generic "prefix changed" warning.
    let projects = distinct_hash(cache_events, "project_hash");
    if projects.len() > 1 {
        findings.push(finding(
            "project_prefix_churn",
            "info",
            format!(
                "the project component changed {} times in the observation window; source/skill edits should be checked before blaming the provider",
                projects.len()
            ),
            BTreeMap::from([("generations".to_string(), projects.len().into())]),
        ));
    }
    let tools = distinct_hash(cache_events, "tools_hash");
    if tools.len() > 1 {
        findings.push(finding(
            "tool_bundle_churn",
            "warning",
            format!(
                "the sorted tool bundle changed {} times; dynamic MCP activation or registry order may be moving the prefix",
                tools.len()
            ),
            BTreeMap::from([("generations".to_string(), tools.len().into())]),
        ));
    }

    // 5. Legacy events may predate component hashes. Report the gap instead of
    //    pretending attribution is possible.
    let missing_hashes = cache_events
        .iter()
        .filter(|event| {
            event.payload.get("prompt_prefix_hash").is_some()
                && event.payload.get("core_hash").is_none()
        })
        .count() as u64;
    if missing_hashes > 0 {
        findings.push(finding(
            "missing_component_hashes",
            "info",
            format!(
                "{missing_hashes} cache event(s) predate component hashes; miss attribution is limited to prefix changes"
            ),
            BTreeMap::from([("calls".to_string(), missing_hashes.into())]),
        ));
    }

    // 6. A one-hour TTL with no observed reads likely paid the higher write
    //    cost without a reuse. This is an observation, not a TTL fact.
    let one_hour_calls = cache_events
        .iter()
        .filter(|event| payload_str(event, "cache_ttl") == Some("1h"))
        .count() as u64;
    if one_hour_calls > 0 && calls_with_read == 0 {
        findings.push(finding(
            "one_hour_ttl_without_reads",
            "warning",
            "the 1h cache pool was requested but no call reported cache reads; the extended write cost may not be recovering"
                .to_string(),
            BTreeMap::from([("calls".to_string(), one_hour_calls.into())]),
        ));
    }

    // 7. Explicit warm calls should usually be followed by a normal call. A
    //    trailing warm is not an error, but it did spend tokens.
    if let Some(last) = cache_events.last()
        && last
            .payload
            .get("warm")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        let warm_input = payload_u64(last, "input_tokens");
        findings.push(finding(
            "warm_without_followup",
            "info",
            "the latest cache observation is an explicit warm with no later agent call; the warm cost is recorded but its prefix has not been reused yet"
                .to_string(),
            BTreeMap::from([("input_tokens".to_string(), warm_input.into())]),
        ));
    }

    // 8. Compaction resets are expected; report their estimated cost so they
    //    are not mistaken for a provider miss.
    if aggregate.compaction_resets > 0 {
        findings.push(finding(
            "compaction_cache_resets",
            "info",
            format!(
                "{} compaction reset(s) invalidated an estimated {} token prefix (${:.6} estimated lost cache-read savings)",
                aggregate.compaction_resets,
                aggregate.compaction_invalidation_tokens,
                aggregate.compaction_invalidation_estimated_usd
            ),
            BTreeMap::from([
                ("resets".to_string(), aggregate.compaction_resets.into()),
                (
                    "invalidation_tokens".to_string(),
                    aggregate.compaction_invalidation_tokens.into(),
                ),
                (
                    "invalidation_estimated_usd".to_string(),
                    aggregate.compaction_invalidation_estimated_usd.into(),
                ),
            ]),
        ));
    }

    // 9. A disabled policy with cache markers would mean the policy was not
    //    applied. This is a real correctness error, not a performance hint.
    if policy.is_some_and(|policy| !policy.enabled)
        && cache_events
            .iter()
            .any(|event| payload_u64(event, "cache_breakpoints") > 0)
    {
        findings.push(finding(
            "disabled_policy_emitted_breakpoints",
            "critical",
            "the project cache policy is disabled but cache markers were emitted; policy propagation is broken"
                .to_string(),
            BTreeMap::new(),
        ));
    }

    // 10. The prefix plan is the authoritative expectation. If a later call
    //     reports a different provider plan, surface the mismatch instead of
    //     silently trusting telemetry.
    let latest_plan = prefix_events
        .iter()
        .rev()
        .find_map(|event| event.payload.get("cache_plan"));
    if let (Some(plan), Some(event)) = (latest_plan, cache_events.last()) {
        let planned = plan
            .get("breakpoints")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let observed = payload_u64(event, "cache_breakpoints");
        if planned != observed {
            findings.push(finding(
                "breakpoint_plan_mismatch",
                "warning",
                format!(
                    "the latest prompt-prefix plan requested {planned} breakpoint(s) but the cache event reports {observed}"
                ),
                BTreeMap::from([
                    ("planned".to_string(), planned.into()),
                    ("observed".to_string(), observed.into()),
                ]),
            ));
        }
    }

    findings.sort_by(|left, right| {
        severity_rank(&left.severity)
            .cmp(&severity_rank(&right.severity))
            .then_with(|| left.code.cmp(&right.code))
    });
    let clean = findings.iter().all(|finding| finding.severity == "info");
    SessionCacheAuditResult {
        session_id: session_id.to_string(),
        provider: provider.map(str::to_string),
        calls_checked,
        warm_calls_checked,
        compactions_checked: aggregate.compaction_resets,
        hit_ratio,
        cache_read_tokens,
        cache_write_tokens,
        findings,
        clean,
    }
}

fn finding(
    code: &str,
    severity: &str,
    message: String,
    evidence: BTreeMap<String, serde_json::Value>,
) -> SessionCacheAuditFinding {
    SessionCacheAuditFinding {
        code: code.to_string(),
        severity: severity.to_string(),
        message,
        evidence,
    }
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "warning" => 1,
        _ => 2,
    }
}

fn payload_u64(event: &Event, key: &str) -> u64 {
    event
        .payload
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

fn payload_str<'a>(event: &'a Event, key: &str) -> Option<&'a str> {
    event.payload.get(key).and_then(serde_json::Value::as_str)
}

fn sum_u64(events: &[Event], key: &str) -> u64 {
    events
        .iter()
        .map(|event| payload_u64(event, key))
        .fold(0u64, u64::saturating_add)
}

fn distinct_hash(events: &[Event], key: &str) -> BTreeSet<String> {
    events
        .iter()
        .filter_map(|event| payload_str(event, key).map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, payload: serde_json::Value) -> Event {
        Event {
            session_id: "ses-1".into(),
            seq: 1,
            kind: kind.into(),
            payload,
            created_at: "2026-11-11T00:00:00Z".into(),
        }
    }

    fn cache(payload: serde_json::Value) -> Event {
        event("context.cache", payload)
    }

    fn codes(result: &SessionCacheAuditResult) -> Vec<&str> {
        result
            .findings
            .iter()
            .map(|finding| finding.code.as_str())
            .collect()
    }

    #[test]
    fn clean_window_has_only_info_findings() {
        let events = vec![
            cache(serde_json::json!({
                "input_tokens": 1000,
                "cache_read_tokens": 600,
                "cache_write_tokens": 100,
                "cache_breakpoints": 4,
                "cache_ttl": "5m",
                "project_hash": "p1",
                "core_hash": "c1",
                "tools_hash": "t1",
                "prompt_prefix_hash": "x"
            })),
            cache(serde_json::json!({
                "input_tokens": 1200,
                "cache_read_tokens": 900,
                "cache_write_tokens": 0,
                "cache_breakpoints": 4,
                "cache_ttl": "5m",
                "project_hash": "p1",
                "core_hash": "c1",
                "tools_hash": "t1",
                "prompt_prefix_hash": "x"
            })),
        ];
        let result = audit_cache_events(
            "ses-1",
            Some("anthropic"),
            Some(&CachePolicy::default()),
            &events,
            &[],
            &[],
        );
        assert!(result.clean);
        assert!(result.findings.is_empty());
        assert_eq!(result.calls_checked, 2);
        assert!(result.hit_ratio > 0.5);
    }

    #[test]
    fn breakpoint_over_limit_is_critical() {
        let events = vec![cache(
            serde_json::json!({"cache_breakpoints": 5, "at": "x"}),
        )];
        let result = audit_cache_events("ses-1", Some("anthropic"), None, &events, &[], &[]);
        let finding = result
            .findings
            .iter()
            .find(|finding| finding.code == "breakpoint_limit_exceeded")
            .unwrap();
        assert_eq!(finding.severity, "critical");
        assert!(finding.message.contains("at most four"));
        assert!(!result.clean);
    }

    #[test]
    fn writes_without_reads_and_low_hit_ratio_are_warnings() {
        let events = (0..5)
            .map(|_| {
                cache(serde_json::json!({
                    "input_tokens": 1000,
                    "cache_read_tokens": 0,
                    "cache_write_tokens": 500,
                    "cache_breakpoints": 4,
                    "cache_ttl": "5m"
                }))
            })
            .collect::<Vec<_>>();
        let result = audit_cache_events("ses-1", Some("anthropic"), None, &events, &[], &[]);
        let codes = codes(&result);
        assert!(codes.contains(&"cache_writes_without_reads"));
        assert!(codes.contains(&"low_cache_hit_ratio"));
        assert_eq!(result.calls_checked, 5);
    }

    #[test]
    fn project_and_tool_churn_are_reported_separately() {
        let events = vec![
            cache(
                serde_json::json!({"project_hash": "p1", "tools_hash": "t1", "cache_read_tokens": 1}),
            ),
            cache(
                serde_json::json!({"project_hash": "p2", "tools_hash": "t2", "cache_read_tokens": 1}),
            ),
        ];
        let result = audit_cache_events("ses-1", None, None, &events, &[], &[]);
        let codes = codes(&result);
        assert!(codes.contains(&"project_prefix_churn"));
        assert!(codes.contains(&"tool_bundle_churn"));
    }

    #[test]
    fn missing_hashes_and_one_hour_without_reads_are_reported() {
        let events = vec![cache(serde_json::json!({
            "prompt_prefix_hash": "legacy",
            "cache_ttl": "1h",
            "cache_read_tokens": 0,
            "cache_write_tokens": 100
        }))];
        let result = audit_cache_events("ses-1", Some("anthropic"), None, &events, &[], &[]);
        let codes = codes(&result);
        assert!(codes.contains(&"missing_component_hashes"));
        assert!(codes.contains(&"one_hour_ttl_without_reads"));
    }

    #[test]
    fn trailing_warm_and_compaction_resets_are_informational() {
        let cache_events = vec![cache(serde_json::json!({
            "warm": true,
            "input_tokens": 2000,
            "cache_read_tokens": 0,
            "cache_write_tokens": 1800,
            "cache_breakpoints": 3,
            "cache_ttl": "5m",
            "core_hash": "c",
            "project_hash": "p",
            "tools_hash": "t",
            "prompt_prefix_hash": "x"
        }))];
        let compactions = vec![event(
            "context.compaction",
            serde_json::json!({
                "cache_reset_reason": "compaction",
                "cache_invalidation_tokens": 700,
                "cache_invalidation_estimated_usd": 0.0063
            }),
        )];
        let result = audit_cache_events(
            "ses-1",
            Some("anthropic"),
            None,
            &cache_events,
            &[],
            &compactions,
        );
        let codes = codes(&result);
        assert!(codes.contains(&"warm_without_followup"));
        assert!(codes.contains(&"compaction_cache_resets"));
        assert!(result.clean, "info-only findings are not a failure");
    }

    #[test]
    fn disabled_policy_with_breakpoints_is_critical() {
        let events = vec![cache(serde_json::json!({"cache_breakpoints": 2}))];
        let policy = CachePolicy {
            enabled: false,
            ..Default::default()
        };
        let result = audit_cache_events("ses-1", None, Some(&policy), &events, &[], &[]);
        let codes = codes(&result);
        assert!(codes.contains(&"disabled_policy_emitted_breakpoints"));
        assert!(!result.clean);
    }

    #[test]
    fn breakpoint_plan_mismatch_is_reported() {
        let cache_events = vec![cache(serde_json::json!({"cache_breakpoints": 3}))];
        let prefix_events = vec![event(
            "context.prompt_prefix",
            serde_json::json!({"cache_plan": {"breakpoints": 4}}),
        )];
        let result = audit_cache_events(
            "ses-1",
            Some("anthropic"),
            None,
            &cache_events,
            &prefix_events,
            &[],
        );
        let finding = result
            .findings
            .iter()
            .find(|finding| finding.code == "breakpoint_plan_mismatch")
            .unwrap();
        assert_eq!(finding.severity, "warning");
        assert_eq!(finding.evidence["planned"].as_u64(), Some(4));
        assert_eq!(finding.evidence["observed"].as_u64(), Some(3));
    }
}
