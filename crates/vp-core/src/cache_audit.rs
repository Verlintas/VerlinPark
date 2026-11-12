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
    break_events: &[Event],
) -> SessionCacheAuditResult {
    let mut findings = Vec::new();
    let aggregate = prompt_cache::aggregate_cache_events(cache_events, compaction_events);
    let continuity = crate::conversation::aggregate_continuity(cache_events, break_events);
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

    // 8b. A v3 prompt prefix or compaction boundary must carry the contract
    // capsule identity when task contracts are enabled. Its absence means the
    // capsule was dropped or a caller bypassed the typed contract; it is never
    // silently inferred from the summary.
    let latest_prefix = prefix_events.last();
    let latest_compaction = compaction_events.last();
    let prefix_missing = latest_prefix.is_some_and(|event| {
        event
            .payload
            .get("prompt_layout")
            .and_then(serde_json::Value::as_u64)
            == Some(3)
            && event
                .payload
                .get("contract_policy_enabled")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            && event
                .payload
                .get("contract_hash")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|hash| hash.is_empty())
    });
    let compaction_missing = latest_compaction.is_some_and(|event| {
        event
            .payload
            .get("contract_policy_enabled")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && event
                .payload
                .get("contract_hash")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|hash| hash.is_empty())
    });
    if prefix_missing || compaction_missing {
        findings.push(finding(
            "contract_capsule_missing",
            "warning",
            "the latest v3 prompt prefix or compaction boundary has no contract capsule hash while task contracts are enabled"
                .to_string(),
            BTreeMap::from([
                ("prefix_missing".to_string(), prefix_missing.into()),
                ("compaction_missing".to_string(), compaction_missing.into()),
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

    // 10. L5 continuity. A break after the first observation means the
    //     canonical message list was edited/reordered or a prior message was
    //     re-serialized differently. The audit cannot repair it and must not
    //     pretend the provider caused it.
    if aggregate.append_only_breaks > 0 {
        let first_break = cache_events
            .iter()
            .find(|event| {
                event
                    .payload
                    .get("append_only")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
                    && (payload_u64(event, "previous_message_prefix_count") > 0
                        || payload_u64(event, "previous_input_tokens") > 0)
            })
            .or_else(|| cache_events.first());
        let mut evidence =
            BTreeMap::from([("breaks".to_string(), aggregate.append_only_breaks.into())]);
        if let Some(event) = first_break {
            evidence.insert(
                "first_break_at".to_string(),
                event.created_at.clone().into(),
            );
            if let Some(hash) = payload_str(event, "previous_message_prefix_hash") {
                evidence.insert("previous_message_prefix_hash".to_string(), hash.into());
            }
            if let Some(hash) = payload_str(event, "message_prefix_hash") {
                evidence.insert("message_prefix_hash".to_string(), hash.into());
            }
            if let Some(reason) = payload_str(event, "miss_reason") {
                evidence.insert("miss_reason".to_string(), reason.into());
            }
        }
        findings.push(finding(
            "append_only_prefix_break",
            "warning",
            format!(
                "{} call(s) did not extend the previous canonical message list; prior history may have been edited, reordered, or re-serialized",
                aggregate.append_only_breaks
            ),
            evidence,
        ));
    }

    // 11. Epoch churn separates our stable-identity changes from a provider
    //     eviction. More than one epoch is expected after an intentional
    //     model/tool/project change; it is informational evidence, not an
    //     error by itself.
    if aggregate.cache_epoch_timeline.len() > 1 {
        findings.push(finding(
            "cache_epoch_churn",
            "info",
            format!(
                "{} cache epochs were observed in the bounded window; check whether a model, tool bundle, project section, or compaction boundary changed",
                aggregate.cache_epoch_timeline.len()
            ),
            BTreeMap::from([(
                "epochs".to_string(),
                aggregate.cache_epoch_timeline.len().into(),
            )]),
        ));
    }

    // 12. Theoretical max is the append-only headroom implied by the previous
    //     prompt. A sustained gap means a provider eviction or a larger tail
    //     than the message history explains; it is not a TTL fact.
    let observed_hit_ratio = aggregate.hit_ratio;
    let theoretical = aggregate.theoretical_max_p50.unwrap_or(0.0);
    if aggregate.calls >= 5 && theoretical >= 0.5 && observed_hit_ratio + 0.05 < theoretical {
        findings.push(finding(
            "theoretical_cache_headroom_gap",
            "warning",
            format!(
                "observed cache-read share is {:.1}% but the append-only prompt headroom is {:.1}%; inspect provider eviction and uncached tail size",
                observed_hit_ratio * 100.0,
                theoretical * 100.0
            ),
            BTreeMap::from([
                ("hit_ratio".to_string(), observed_hit_ratio.into()),
                ("theoretical_max_hit_ratio".to_string(), theoretical.into()),
                ("calls".to_string(), aggregate.calls.into()),
            ]),
        ));
    }

    // 13. Long-context sessions have an explicit acceptance target. Report a
    //     miss honestly; short sessions are not compared against the 99.x
    //     target because their initial prompt dominates the denominator.
    if aggregate.calls >= 5 && aggregate.input_tokens >= 200_000 {
        if observed_hit_ratio < 0.99 {
            findings.push(finding(
                "long_context_cache_below_99",
                "warning",
                format!(
                    "long-context session hit ratio is {:.2}%; the working target is 99.0% and the stretch target is 99.84%",
                    observed_hit_ratio * 100.0
                ),
                BTreeMap::from([
                    ("hit_ratio".to_string(), observed_hit_ratio.into()),
                    ("input_tokens".to_string(), aggregate.input_tokens.into()),
                    ("cache_miss_tokens".to_string(), aggregate.cache_miss_tokens.into()),
                ]),
            ));
        } else if observed_hit_ratio < 0.995 {
            findings.push(finding(
                "long_context_cache_below_995",
                "info",
                format!(
                    "long-context session hit ratio is {:.2}%; the long-prefix benchmark target is 99.5% and the stretch target is 99.84%",
                    observed_hit_ratio * 100.0
                ),
                BTreeMap::from([
                    ("hit_ratio".to_string(), observed_hit_ratio.into()),
                    ("cache_miss_tokens".to_string(), aggregate.cache_miss_tokens.into()),
                ]),
            ));
        }
    }

    // 14. Legacy events cannot prove continuity. Say so instead of treating
    //     missing append-only fields as a break.
    let continuity_observed = cache_events.iter().any(|event| {
        event
            .payload
            .get("append_only")
            .and_then(serde_json::Value::as_bool)
            .is_some()
    });
    if calls_checked >= 2 && !continuity_observed {
        findings.push(finding(
            "missing_append_only_telemetry",
            "info",
            "cache events predate append-only continuity telemetry; the audit cannot distinguish a provider miss from a message-list break"
                .to_string(),
            BTreeMap::from([("calls".to_string(), calls_checked.into())]),
        ));
    }

    // 15. Identify the first stable section key that changed between the two
    //     latest prompt-prefix events. It is intentionally a key/hash diff, not
    //     a prompt-text diff: operators get a section to inspect without the
    //     audit ever reading prompt content.
    if let Some(change) = first_stable_section_change(prefix_events) {
        let (key, previous_hash, current_hash) = change;
        findings.push(finding(
            "stable_section_changed",
            "warning",
            format!(
                "stable section '{key}' changed between the two latest prompt-prefix observations; check source/skill/model/tool edits before blaming the provider"
            ),
            BTreeMap::from([
                ("section_key".to_string(), key.into()),
                ("previous_hash".to_string(), previous_hash.into()),
                ("current_hash".to_string(), current_hash.into()),
            ]),
        ));
    }

    // 16. A placeholder epoch is expected before L4 finalization. Surface it so
    //     an operator does not treat the prompt-prefix hash as a finalized
    //     cache-epoch identity.
    if cache_events.iter().any(|event| {
        payload_bool(event, "cache_epoch_placeholder") == Some(true)
            || payload_str(event, "cache_epoch_source") == Some("l1_placeholder")
    }) {
        findings.push(finding(
            "cache_epoch_placeholder",
            "info",
            "one or more calls use the prompt-prefix placeholder as the cache epoch; a later epoch lifecycle can finalize the identity without changing this report"
                .to_string(),
            BTreeMap::new(),
        ));
    }

    // 17. The prefix plan is the authoritative expectation. If a later call
    //     reports a different provider plan, surface the mismatch instead of
    //     silently trusting telemetry.
    // 11. Epoch churn is distinct from tool/project churn: it says the stable
    //     instruction identity moved enough times that cross-session reuse is
    //     unlikely to pay back. It is informational unless the window is large
    //     enough for the count to be meaningful.
    if aggregate.cache_epoch_counts.len() > 2 {
        findings.push(finding(
            "cache_epoch_churn",
            if calls_checked >= 5 {
                "warning"
            } else {
                "info"
            },
            format!(
                "{} distinct cache epochs were observed in the window; inspect model/tool/project changes before expecting cross-session reuse",
                aggregate.cache_epoch_counts.len()
            ),
            BTreeMap::from([
                ("epochs".to_string(), aggregate.cache_epoch_counts.len().into()),
                ("calls".to_string(), calls_checked.into()),
            ]),
        ));
    }

    // 12. A same-epoch low hit is provider eviction or TTL pressure, not a
    //     local prefix change. Report it separately so operators do not chase
    //     a generator change that did not happen.
    if aggregate.provider_cache_misses > 0 {
        let severe =
            calls_checked >= 3 && aggregate.provider_cache_misses.saturating_mul(4) > calls_checked;
        findings.push(finding(
            "provider_cache_miss_pressure",
            if severe { "warning" } else { "info" },
            format!(
                "{} call(s) had an unchanged cache epoch but a low or zero provider cache hit; this is provider eviction/TTL pressure, not local prefix churn",
                aggregate.provider_cache_misses
            ),
            BTreeMap::from([
                (
                    "provider_cache_misses".to_string(),
                    aggregate.provider_cache_misses.into(),
                ),
                ("calls".to_string(), calls_checked.into()),
            ]),
        ));
    }

    // 13. Cross-session reuse is only claimed from provider-reported reads on
    //     the first call of a session that shared a known epoch.
    if aggregate.cross_session_first_calls > 0 {
        findings.push(finding(
            "cross_session_prefix_reuse",
            "info",
            format!(
                "{} first session call(s) reused a known epoch and reported {} cache-read token(s)",
                aggregate.cross_session_first_calls, aggregate.cross_session_hit_tokens
            ),
            BTreeMap::from([
                (
                    "first_calls".to_string(),
                    aggregate.cross_session_first_calls.into(),
                ),
                (
                    "cache_read_tokens".to_string(),
                    aggregate.cross_session_hit_tokens.into(),
                ),
            ]),
        ));
    }

    // 14. A compaction boundary that preserves the stable epoch is expected:
    //     the conversation cache reset is real, but the shareable instruction
    //     block did not change. Surface it so the reset is not misread as a
    //     new epoch.
    let preserved_epoch_resets = compaction_events
        .iter()
        .filter(|event| {
            let before = event
                .payload
                .get("cache_epoch_before")
                .and_then(serde_json::Value::as_str);
            let after = event
                .payload
                .get("cache_epoch_after")
                .and_then(serde_json::Value::as_str);
            matches!((before, after), (Some(before), Some(after)) if before == after)
                && event
                    .payload
                    .get("cache_epoch_changed")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
        })
        .count() as u64;
    if preserved_epoch_resets > 0 {
        findings.push(finding(
            "compaction_preserved_cache_epoch",
            "info",
            format!(
                "{preserved_epoch_resets} compaction reset(s) kept the stable cache epoch; only the conversation cache boundary moved"
            ),
            BTreeMap::from([(
                "resets".to_string(),
                preserved_epoch_resets.into(),
            )]),
        ));
    }

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

    // 11. A non-append-only history is a local byte change, not a provider TTL
    //     guess. Report the bounded reason counts and the latest index. No
    //     message content is available or reported.
    let structural_breaks = continuity
        .break_reason_counts
        .iter()
        .filter(|bucket| {
            bucket.calls > 0
                && !matches!(
                    bucket.key.as_str(),
                    "first_call" | "compaction_reset" | "provider_cache_miss"
                )
        })
        .map(|bucket| format!("{}={}", bucket.key, bucket.calls))
        .collect::<Vec<_>>();
    if continuity.non_append_only_calls > 0 && !structural_breaks.is_empty() {
        let latest = break_events.iter().rev().find(|event| {
            event
                .payload
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|reason| {
                    !matches!(
                        reason,
                        "first_call" | "compaction_reset" | "provider_cache_miss"
                    )
                })
        });
        let mut evidence = BTreeMap::from([
            (
                "non_append_only_calls".to_string(),
                continuity.non_append_only_calls.into(),
            ),
            (
                "append_only_calls".to_string(),
                continuity.append_only_calls.into(),
            ),
            ("reasons".to_string(), structural_breaks.join(",").into()),
        ]);
        if let Some(event) = latest {
            if let Some(index) = event
                .payload
                .get("first_differing_index")
                .and_then(serde_json::Value::as_u64)
            {
                evidence.insert("first_differing_index".to_string(), index.into());
            }
            if let Some(role) = event
                .payload
                .get("first_differing_role")
                .and_then(serde_json::Value::as_str)
            {
                evidence.insert("first_differing_role".to_string(), role.into());
            }
            if let Some(tool_call_id) = event
                .payload
                .get("first_differing_tool_call_id")
                .and_then(serde_json::Value::as_str)
            {
                evidence.insert(
                    "first_differing_tool_call_id".to_string(),
                    tool_call_id.into(),
                );
            }
            evidence.insert("at".to_string(), event.created_at.clone().into());
        }
        findings.push(finding(
            "non_append_only_history",
            "warning",
            format!(
                "{} provider call(s) did not extend the previous canonical message list; local breaks were {}",
                continuity.non_append_only_calls,
                structural_breaks.join(", ")
            ),
            evidence,
        ));
    }

    // 12. A miss on an otherwise append-only list is provider-side. Keep it
    //     separate from local churn so an operator does not edit stable bytes
    //     to chase an eviction that had nothing to do with them.
    let provider_misses = continuity
        .break_reason_counts
        .iter()
        .find(|bucket| bucket.key == "provider_cache_miss")
        .map(|bucket| bucket.calls)
        .unwrap_or(0);
    if provider_misses > 0 && structural_breaks.is_empty() {
        findings.push(finding(
            "provider_evicted_append_only_prefix",
            "info",
            format!(
                "{provider_misses} append-only call(s) reported no cache reads; the message chain was unchanged, so this is a provider-side miss rather than local prefix churn"
            ),
            BTreeMap::from([
                ("provider_misses".to_string(), provider_misses.into()),
                (
                    "append_only_calls".to_string(),
                    continuity.append_only_calls.into(),
                ),
            ]),
        ));
    }

    // 13. Legacy events have no canonical identity. Say so instead of claiming
    //     continuity or a break that cannot be proven.
    let calls_without_continuity = cache_events
        .iter()
        .filter(|event| {
            event
                .payload
                .get("warm")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
                && event.payload.get("message_prefix_hash").is_none()
        })
        .count() as u64;
    if calls_without_continuity > 0 {
        findings.push(finding(
            "legacy_events_without_continuity",
            "info",
            format!(
                "{calls_without_continuity} cache event(s) predate canonical message hashes; append-only continuity cannot be proven for them"
            ),
            BTreeMap::from([("calls".to_string(), calls_without_continuity.into())]),
        ));
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
        input_tokens,
        cache_miss_tokens: aggregate.cache_miss_tokens,
        theoretical_max_hit_ratio: aggregate.latest_theoretical_max_hit_ratio,
        hit_ratio_p50: aggregate.hit_ratio_p50,
        hit_ratio_p95: aggregate.hit_ratio_p95,
        append_only_calls: aggregate.append_only_calls,
        append_only_breaks: aggregate.append_only_breaks,
        cache_epochs: aggregate.cache_epoch_timeline.len() as u64,
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

fn payload_bool(event: &Event, key: &str) -> Option<bool> {
    event.payload.get(key).and_then(serde_json::Value::as_bool)
}

/// Compare the ordered head-section key/hash list between the two latest
/// prompt-prefix events. Returns the first changed key and both hashes. A new
/// or removed section is also a change; the missing side is reported as an
/// empty hash rather than a fabricated value.
fn first_stable_section_change(prefix_events: &[Event]) -> Option<(String, String, String)> {
    let mut latest = prefix_events
        .iter()
        .rev()
        .filter_map(|event| {
            let sections = event.payload.get("head_sections")?.as_array()?;
            let pairs = sections
                .iter()
                .filter_map(|section| {
                    Some((
                        section.get("key")?.as_str()?.to_string(),
                        section.get("hash")?.as_str()?.to_string(),
                    ))
                })
                .collect::<Vec<_>>();
            (!pairs.is_empty()).then_some(pairs)
        })
        .take(2);
    let current = latest.next()?;
    let previous = latest.next()?;
    for (index, (key, hash)) in current.iter().enumerate() {
        match previous.get(index) {
            Some((previous_key, previous_hash)) if previous_key == key && previous_hash == hash => {
            }
            Some((previous_key, previous_hash)) if previous_key == key => {
                return Some((key.clone(), previous_hash.clone(), hash.clone()));
            }
            Some((previous_key, previous_hash)) => {
                return Some((
                    key.clone(),
                    format!("{previous_key}:{previous_hash}"),
                    hash.clone(),
                ));
            }
            None => return Some((key.clone(), String::new(), hash.clone())),
        }
    }
    if let Some((key, hash)) = previous.get(current.len()) {
        return Some((key.clone(), hash.clone(), String::new()));
    }
    None
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
                "prompt_prefix_hash": "x",
                "append_only": true,
                "previous_message_prefix_count": 0,
                "previous_input_tokens": 0
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
                "prompt_prefix_hash": "x",
                "append_only": true,
                "previous_message_prefix_count": 2,
                "previous_input_tokens": 1000
            })),
        ];
        let result = audit_cache_events(
            "ses-1",
            Some("anthropic"),
            Some(&CachePolicy::default()),
            &events,
            &[],
            &[],
            &[],
        );
        assert!(result.clean);
        assert_eq!(codes(&result), vec!["legacy_events_without_continuity"]);
        assert_eq!(result.calls_checked, 2);
        assert!(result.hit_ratio > 0.5);
    }

    #[test]
    fn canonical_events_do_not_report_a_legacy_gap() {
        let events = vec![cache(serde_json::json!({
            "input_tokens": 1000,
            "cache_read_tokens": 600,
            "cache_breakpoints": 4,
            "cache_ttl": "5m",
            "message_prefix_hash": "chain",
            "append_only": true
        }))];
        let result = audit_cache_events(
            "ses-1",
            Some("openai-compatible"),
            Some(&CachePolicy::default()),
            &events,
            &[],
            &[],
            &[],
        );
        assert!(result.findings.is_empty());
        assert!(result.clean);
    }

    #[test]
    fn contract_capsule_missing_is_reported_only_when_contracts_are_enabled() {
        let prefix = event(
            "context.prompt_prefix",
            serde_json::json!({
                "prompt_layout": 3,
                "contract_policy_enabled": true,
                "contract_hash": null,
                "prefix_hash": "p",
            }),
        );
        let result = audit_cache_events(
            "ses-1",
            Some("openai-compatible"),
            None,
            &[],
            std::slice::from_ref(&prefix),
            &[],
            &[],
        );
        assert!(codes(&result).contains(&"contract_capsule_missing"));

        let with_contract = event(
            "context.prompt_prefix",
            serde_json::json!({
                "prompt_layout": 3,
                "contract_policy_enabled": true,
                "contract_hash": "c".repeat(64),
                "prefix_hash": "p",
            }),
        );
        let result = audit_cache_events(
            "ses-1",
            Some("openai-compatible"),
            None,
            &[],
            &[with_contract],
            &[],
            &[],
        );
        assert!(!codes(&result).contains(&"contract_capsule_missing"));

        let disabled = event(
            "context.prompt_prefix",
            serde_json::json!({
                "prompt_layout": 3,
                "contract_policy_enabled": false,
                "prefix_hash": "p",
            }),
        );
        let result = audit_cache_events(
            "ses-1",
            Some("openai-compatible"),
            None,
            &[],
            &[disabled],
            &[],
            &[],
        );
        assert!(!codes(&result).contains(&"contract_capsule_missing"));
    }

    #[test]
    fn non_append_only_history_is_a_warning_without_content() {
        let cache_events = vec![cache(serde_json::json!({
            "input_tokens": 1000,
            "cache_read_tokens": 0,
            "message_prefix_hash": "chain-2",
            "append_only": false,
            "cache_break_reason": "message_edited"
        }))];
        let breaks = vec![event(
            "context.cache_break",
            serde_json::json!({
                "reason": "message_edited",
                "first_differing_index": 2,
                "first_differing_role": "assistant",
                "first_differing_tool_call_id": "call_1"
            }),
        )];
        let result = audit_cache_events(
            "ses-1",
            Some("openai-compatible"),
            None,
            &cache_events,
            &[],
            &[],
            &breaks,
        );
        let finding = result
            .findings
            .iter()
            .find(|finding| finding.code == "non_append_only_history")
            .unwrap();
        assert_eq!(finding.severity, "warning");
        assert_eq!(finding.evidence["first_differing_index"], 2);
        assert_eq!(finding.evidence["first_differing_role"], "assistant");
        assert!(!finding.message.contains("call_1"));
        assert!(!result.clean);
    }

    #[test]
    fn provider_miss_on_an_append_only_prefix_is_informational() {
        let cache_events = vec![cache(serde_json::json!({
            "input_tokens": 1000,
            "cache_read_tokens": 0,
            "message_prefix_hash": "chain-2",
            "append_only": true
        }))];
        let breaks = vec![event(
            "context.cache_break",
            serde_json::json!({"reason": "provider_cache_miss"}),
        )];
        let result = audit_cache_events(
            "ses-1",
            Some("deepseek"),
            None,
            &cache_events,
            &[],
            &[],
            &breaks,
        );
        let codes = codes(&result);
        assert!(codes.contains(&"provider_evicted_append_only_prefix"));
        assert!(!codes.contains(&"non_append_only_history"));
        assert!(result.clean);
    }

    #[test]
    fn breakpoint_over_limit_is_critical() {
        let events = vec![cache(
            serde_json::json!({"cache_breakpoints": 5, "at": "x"}),
        )];
        let result = audit_cache_events("ses-1", Some("anthropic"), None, &events, &[], &[], &[]);
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
        let result = audit_cache_events("ses-1", Some("anthropic"), None, &events, &[], &[], &[]);
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
        let result = audit_cache_events("ses-1", None, None, &events, &[], &[], &[]);
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
        let result = audit_cache_events("ses-1", Some("anthropic"), None, &events, &[], &[], &[]);
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
            &[],
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
        let result = audit_cache_events("ses-1", None, Some(&policy), &events, &[], &[], &[]);
        let codes = codes(&result);
        assert!(codes.contains(&"disabled_policy_emitted_breakpoints"));
        assert!(!result.clean);
    }

    #[test]
    fn epoch_churn_provider_misses_and_cross_session_reuse_are_distinct_findings() {
        let epoch = "a".repeat(64);
        let other_epoch = "b".repeat(64);
        let third_epoch = "c".repeat(64);
        let cache_events = vec![
            cache(serde_json::json!({
                "cache_epoch": epoch,
                "input_tokens": 1000,
                "cache_read_tokens": 900,
                "cross_session_first_call": true,
                "cross_session_cache_read_tokens": 900,
            })),
            cache(serde_json::json!({
                "cache_epoch": epoch,
                "input_tokens": 1000,
                "cache_read_tokens": 100,
                "miss_reason": "provider_cache_miss",
            })),
            cache(serde_json::json!({
                "cache_epoch": other_epoch,
                "input_tokens": 500,
                "cache_read_tokens": 0,
                "miss_reason": "provider_cache_miss",
            })),
            cache(serde_json::json!({
                "cache_epoch": third_epoch,
                "input_tokens": 500,
                "cache_read_tokens": 0,
                "miss_reason": "provider_cache_miss",
            })),
        ];
        let resets = vec![event(
            "context.compaction.cache_reset",
            serde_json::json!({
                "reset_reason": "compaction",
                "cache_epoch_before": other_epoch,
                "cache_epoch_after": other_epoch,
                "cache_epoch_changed": false,
                "invalidated_tokens": 400,
                "estimated_lost_cache_read_usd": 0.001,
            }),
        )];
        let result = audit_cache_events("ses-1", None, None, &cache_events, &[], &resets, &[]);
        let codes = codes(&result);
        assert!(codes.contains(&"cache_epoch_churn"));
        assert!(codes.contains(&"provider_cache_miss_pressure"));
        assert!(codes.contains(&"cross_session_prefix_reuse"));
        assert!(codes.contains(&"compaction_preserved_cache_epoch"));
        let pressure = result
            .findings
            .iter()
            .find(|finding| finding.code == "provider_cache_miss_pressure")
            .unwrap();
        assert_eq!(pressure.evidence["provider_cache_misses"].as_u64(), Some(3));
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

    #[test]
    fn append_only_break_and_epoch_churn_are_reported() {
        let events = vec![
            cache(serde_json::json!({
                "input_tokens": 1_000,
                "cache_read_tokens": 900,
                "cache_epoch": "e1",
                "cache_epoch_source": "prompt_prefix_v3",
                "message_prefix_hash": "current",
                "previous_message_prefix_hash": "previous",
                "previous_message_prefix_count": 2,
                "previous_input_tokens": 900,
                "append_only": true,
                "cache_breakpoints": 4,
                "cache_ttl": "5m",
                "core_hash": "c",
                "project_hash": "p",
                "tools_hash": "t",
                "prompt_prefix_hash": "x"
            })),
            cache(serde_json::json!({
                "input_tokens": 1_100,
                "cache_read_tokens": 0,
                "cache_epoch": "e2",
                "cache_epoch_source": "prompt_prefix_v3",
                "message_prefix_hash": "current-2",
                "previous_message_prefix_hash": "current",
                "previous_message_prefix_count": 2,
                "previous_input_tokens": 1_000,
                "append_only": false,
                "miss_reason": "prefix_changed",
                "cache_breakpoints": 4,
                "cache_ttl": "5m",
                "core_hash": "c",
                "project_hash": "p",
                "tools_hash": "t",
                "prompt_prefix_hash": "x"
            })),
        ];
        let result = audit_cache_events("ses-1", Some("deepseek"), None, &events, &[], &[], &[]);
        let codes = codes(&result);
        assert!(codes.contains(&"append_only_prefix_break"));
        assert!(codes.contains(&"cache_epoch_churn"));
        assert_eq!(result.append_only_breaks, 1);
        assert_eq!(result.append_only_calls, 1);
        assert_eq!(result.cache_epochs, 2);
        assert_eq!(result.input_tokens, 2_100);
        assert_eq!(result.cache_miss_tokens, 1_200);
        let break_finding = result
            .findings
            .iter()
            .find(|finding| finding.code == "append_only_prefix_break")
            .unwrap();
        assert_eq!(break_finding.evidence["breaks"].as_u64(), Some(1));
        assert_eq!(
            break_finding.evidence["previous_message_prefix_hash"].as_str(),
            Some("current")
        );
    }

    #[test]
    fn theoretical_headroom_gap_and_long_context_targets_are_reported() {
        let events = (0..5)
            .map(|_| {
                cache(serde_json::json!({
                    "input_tokens": 50_000,
                    "cache_read_tokens": 45_000,
                    "cache_miss_tokens": 5_000,
                    "theoretical_max_hit_ratio": 0.99,
                    "append_only": true,
                    "previous_message_prefix_count": 1,
                    "previous_input_tokens": 49_000,
                    "cache_breakpoints": 0,
                    "cache_ttl": "5m"
                }))
            })
            .collect::<Vec<_>>();
        let result = audit_cache_events(
            "ses-1",
            Some("openai-compatible"),
            None,
            &events,
            &[],
            &[],
            &[],
        );
        let codes = codes(&result);
        assert!(codes.contains(&"theoretical_cache_headroom_gap"));
        assert!(codes.contains(&"long_context_cache_below_99"));
        assert_eq!(result.input_tokens, 250_000);
        assert_eq!(result.cache_miss_tokens, 25_000);
        assert_eq!(result.theoretical_max_hit_ratio, 0.99);
    }

    #[test]
    fn first_changed_stable_section_and_placeholder_epoch_are_reported() {
        let prefix_events = vec![
            event(
                "context.prompt_prefix",
                serde_json::json!({
                    "head_sections": [
                        {"key": "core/charter", "hash": "c1", "ordinal": 10, "tail": false},
                        {"key": "project/sources", "hash": "p1", "ordinal": 30, "tail": false}
                    ]
                }),
            ),
            event(
                "context.prompt_prefix",
                serde_json::json!({
                    "head_sections": [
                        {"key": "core/charter", "hash": "c1", "ordinal": 10, "tail": false},
                        {"key": "project/sources", "hash": "p2", "ordinal": 30, "tail": false}
                    ]
                }),
            ),
        ];
        let cache_events = vec![cache(serde_json::json!({
            "input_tokens": 100,
            "cache_read_tokens": 10,
            "cache_epoch": "epoch",
            "cache_epoch_source": "l1_placeholder",
            "cache_epoch_placeholder": true,
            "cache_breakpoints": 0,
            "cache_ttl": "5m"
        }))];
        let result = audit_cache_events(
            "ses-1",
            Some("deepseek"),
            None,
            &cache_events,
            &prefix_events,
            &[],
            &[],
        );
        let codes = codes(&result);
        assert!(codes.contains(&"stable_section_changed"));
        assert!(codes.contains(&"cache_epoch_placeholder"));
        let change = result
            .findings
            .iter()
            .find(|finding| finding.code == "stable_section_changed")
            .unwrap();
        assert_eq!(
            change.evidence["section_key"].as_str(),
            Some("project/sources")
        );
        assert_eq!(change.evidence["previous_hash"].as_str(), Some("p1"));
        assert_eq!(change.evidence["current_hash"].as_str(), Some("p2"));
    }

    #[test]
    fn first_changed_section_detects_added_removed_and_reordered_sections() {
        let event_with = |sections: serde_json::Value| {
            event(
                "context.prompt_prefix",
                serde_json::json!({"head_sections": sections}),
            )
        };
        let base = event_with(serde_json::json!([
            {"key": "core/charter", "hash": "c1", "ordinal": 10, "tail": false},
            {"key": "project/sources", "hash": "p1", "ordinal": 30, "tail": false}
        ]));
        let added = event_with(serde_json::json!([
            {"key": "core/charter", "hash": "c1", "ordinal": 10, "tail": false},
            {"key": "project/sources", "hash": "p1", "ordinal": 30, "tail": false},
            {"key": "project/skills", "hash": "s1", "ordinal": 40, "tail": false}
        ]));
        let removed = event_with(serde_json::json!([
            {"key": "core/charter", "hash": "c1", "ordinal": 10, "tail": false}
        ]));
        let reordered = event_with(serde_json::json!([
            {"key": "project/sources", "hash": "p1", "ordinal": 30, "tail": false},
            {"key": "core/charter", "hash": "c1", "ordinal": 10, "tail": false}
        ]));
        let added_change =
            first_stable_section_change(&[base.clone(), added]).expect("added section");
        assert_eq!(added_change.0, "project/skills");
        assert_eq!(added_change.1, "");
        let removed_change =
            first_stable_section_change(&[base.clone(), removed]).expect("removed section");
        assert_eq!(removed_change.0, "project/sources");
        assert_eq!(removed_change.2, "");
        let reordered_change =
            first_stable_section_change(&[base, reordered]).expect("reordered sections");
        assert_eq!(reordered_change.0, "project/sources");
        assert!(reordered_change.1.contains("core/charter"));
    }

    #[test]
    fn legacy_events_are_reported_as_missing_continuity_not_a_break() {
        let events = (0..2)
            .map(|_| {
                cache(serde_json::json!({
                    "input_tokens": 100,
                    "cache_read_tokens": 50,
                    "cache_breakpoints": 0,
                    "cache_ttl": "5m"
                }))
            })
            .collect::<Vec<_>>();
        let result = audit_cache_events("ses-1", None, None, &events, &[], &[], &[]);
        let codes = codes(&result);
        assert!(codes.contains(&"missing_append_only_telemetry"));
        assert!(!codes.contains(&"append_only_prefix_break"));
        assert_eq!(result.append_only_breaks, 0);
        assert_eq!(result.cache_epochs, 0);
    }
}
