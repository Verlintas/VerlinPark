//! Human renderers for effects, diagnosis, runtime pins, replay, and gates.

use super::support::*;
use super::*;

/// Bounded human rendering for effect receipts and policy. It prints only
/// target labels, states, attempts, and probe evidence — never file content,
/// command text, or provider secrets.
pub(crate) fn effect_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(receipts) = value.get("receipts").and_then(Value::as_array) {
        let policy = value.get("policy").cloned().unwrap_or_else(|| json!({}));
        let counts = value.get("counts").cloned().unwrap_or_else(|| json!({}));
        let blocked = value
            .get("blocked")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut lines = vec![format!(
            "effects: {} total · {} intent · {} done · {} failed · {} unknown · {} blocking{}",
            counts.get("total").and_then(Value::as_u64).unwrap_or(0),
            counts.get("intent").and_then(Value::as_u64).unwrap_or(0),
            counts.get("done").and_then(Value::as_u64).unwrap_or(0),
            counts.get("failed").and_then(Value::as_u64).unwrap_or(0),
            counts.get("unknown").and_then(Value::as_u64).unwrap_or(0),
            counts.get("blocking").and_then(Value::as_u64).unwrap_or(0),
            if blocked { " (merge blocked)" } else { "" },
        )];
        lines.push(format!(
            "policy: enabled={} unknown_blocks_merge={} stale_grace={}s shell_mode={}",
            policy
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("unknown_blocks_merge")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("stale_intent_grace_secs")
                .and_then(Value::as_u64)
                .unwrap_or(120),
            policy
                .get("shell_mode")
                .and_then(Value::as_str)
                .unwrap_or("external_only"),
        ));
        if let Some(reason) = value.get("block_reason").and_then(Value::as_str) {
            lines.push(reason.to_string());
        }
        for receipt in receipts {
            let target = receipt
                .get("target")
                .map(|target| {
                    format!(
                        "{}:{}",
                        target.get("kind").and_then(Value::as_str).unwrap_or("?"),
                        target.get("key").and_then(Value::as_str).unwrap_or("")
                    )
                })
                .unwrap_or_else(|| "?".to_string());
            let evidence = receipt
                .get("evidence")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let anchor = evidence
                .get("sha256")
                .and_then(Value::as_str)
                .or_else(|| evidence.get("commit").and_then(Value::as_str))
                .or_else(|| receipt.get("external_id").and_then(Value::as_str))
                .map(|value| format!(" @ {}", value.chars().take(16).collect::<String>()))
                .unwrap_or_default();
            lines.push(format!(
                "{}  {}  attempt {}{}  {} → {}",
                receipt.get("id").and_then(Value::as_str).unwrap_or("?"),
                receipt.get("state").and_then(Value::as_str).unwrap_or("?"),
                receipt.get("attempt").and_then(Value::as_u64).unwrap_or(0),
                anchor,
                receipt.get("tool").and_then(Value::as_str).unwrap_or("?"),
                target,
            ));
            if let Some(reason) = evidence.get("reason").and_then(Value::as_str) {
                lines.push(format!("    reason: {reason}"));
            }
        }
        return Some(lines);
    }
    if let Some(receipt) = value.get("receipt")
        && let Some(state) = receipt.get("state").and_then(Value::as_str)
    {
        return Some(vec![format!(
            "effect {}: {} attempt {} · {}",
            receipt.get("id").and_then(Value::as_str).unwrap_or("?"),
            state,
            receipt.get("attempt").and_then(Value::as_u64).unwrap_or(0),
            receipt
                .get("target")
                .map(|target| format!(
                    "{}:{}",
                    target.get("kind").and_then(Value::as_str).unwrap_or("?"),
                    target.get("key").and_then(Value::as_str).unwrap_or("")
                ))
                .unwrap_or_else(|| "?".to_string()),
        )]);
    }
    if let Some(policy) = value.get("policy")
        && let Some(project) = value.get("project_id").and_then(Value::as_str)
    {
        return Some(vec![format!(
            "project {project}  effects enabled={} unknown_blocks_merge={} stale_grace={}s shell_mode={} http_mutations={} http_reads={} mcp_mutations={} workspace_writes={}",
            policy
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("unknown_blocks_merge")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("stale_intent_grace_secs")
                .and_then(Value::as_u64)
                .unwrap_or(120),
            policy
                .get("shell_mode")
                .and_then(Value::as_str)
                .unwrap_or("external_only"),
            policy
                .get("track_mutating_http")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("track_read_http")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            policy
                .get("track_mcp_mutations")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("track_workspace_writes")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        )]);
    }
    if let Some(swept) = value.get("swept").and_then(Value::as_u64) {
        return Some(vec![format!(
            "effect sweep: {swept} stale intent(s) moved to unknown"
        )]);
    }
    None
}

pub(crate) fn diagnosis_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(rows) = value.get("attributions").and_then(Value::as_array) {
        let mut lines = vec![format!(
            "attributions: {} recorded  total={}  truncated={}",
            rows.len(),
            value.get("total").and_then(Value::as_u64).unwrap_or(0),
            value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )];
        for row in rows.iter().take(32) {
            let attribution = row.get("attribution").unwrap_or(row);
            let target = attribution.get("target").unwrap_or(&Value::Null);
            let earliest = attribution
                .get("earliest_node_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            lines.push(format!(
                "  seq {} · {} · target={}:{} · earliest={} @{}",
                row.get("event_seq").and_then(Value::as_u64).unwrap_or(0),
                attribution
                    .get("reason_code")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                target.get("kind").and_then(Value::as_str).unwrap_or("?"),
                target.get("id").and_then(Value::as_str).unwrap_or("-"),
                earliest,
                attribution
                    .get("earliest_seq")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ));
        }
        lines.push(
            "advisory only: recorded evidence never retries, cancels, merges, reassigns, or changes budgets"
                .to_string(),
        );
        return Some(lines);
    }
    value.get("graph_summary")?;
    let target = value.get("target").unwrap_or(&Value::Null);
    let mut lines = vec![format!(
        "diagnosis: {}  target={}:{}  status={}  truncated={}",
        value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_uppercase(),
        target.get("kind").and_then(Value::as_str).unwrap_or("-"),
        target.get("id").and_then(Value::as_str).unwrap_or("-"),
        target.get("status").and_then(Value::as_str).unwrap_or("-"),
        value
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    )];
    if let Some(recorded) = value.get("recorded_event_seq").and_then(Value::as_u64) {
        lines.push(format!("recorded: failure.attribution seq {recorded}"));
    }
    if let Some(earliest) = value.get("earliest") {
        lines.push(format!(
            "earliest unsupported claim: {} · {} @{} · task={} · node={}",
            earliest
                .get("reason_code")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            earliest
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            earliest.get("at_seq").and_then(Value::as_u64).unwrap_or(0),
            earliest
                .get("task_id")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            earliest
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or("-")
        ));
        if let Some(summary) = earliest.get("summary").and_then(Value::as_str) {
            lines.push(format!("  {}", bounded_line(summary, 220)));
        }
        if let Some(agents) = earliest.get("agents").and_then(Value::as_array) {
            for agent in agents.iter().take(4) {
                lines.push(format!(
                    "  responsible: {} [{}] {}",
                    agent
                        .get("agent_name")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    agent.get("role").and_then(Value::as_str).unwrap_or("-"),
                    agent.get("agent_id").and_then(Value::as_str).unwrap_or("-")
                ));
            }
        }
        if let Some(evidence) = earliest.get("evidence").and_then(Value::as_array) {
            for item in evidence.iter().take(6) {
                let hash = item
                    .get("hash")
                    .and_then(Value::as_str)
                    .map(short_hash_str)
                    .unwrap_or_else(|| "-".to_string());
                lines.push(format!(
                    "  evidence: {}:{} @{} hash={}",
                    item.get("kind").and_then(Value::as_str).unwrap_or("?"),
                    item.get("reference").and_then(Value::as_str).unwrap_or("-"),
                    item.get("event_seq").and_then(Value::as_u64).unwrap_or(0),
                    hash
                ));
            }
        }
    }
    if let Some(cascade) = value.get("cascade").and_then(Value::as_array)
        && !cascade.is_empty()
    {
        let path = cascade
            .iter()
            .take(24)
            .map(|step| {
                format!(
                    "{}[{}]",
                    step.get("task_id").and_then(Value::as_str).unwrap_or("?"),
                    step.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                )
            })
            .collect::<Vec<_>>()
            .join(" -> ");
        lines.push(format!("cascade: {path}"));
    }
    if let Some(findings) = value.get("findings").and_then(Value::as_array) {
        for finding in findings.iter().take(12) {
            lines.push(format!(
                "  finding: {} [{}] task={} node={}",
                finding
                    .get("reason_code")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                finding
                    .get("severity")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                finding
                    .get("task_id")
                    .and_then(Value::as_str)
                    .unwrap_or("-"),
                finding
                    .get("node_id")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
            ));
        }
    }
    if let Some(summary) = value.get("graph_summary") {
        lines.push(format!(
            "provenance: nodes={} edges={} tasks={} agents={} artifacts={} commits={} checks={} verdicts={} merges={} depth={} depth_limited={} digest={}",
            summary.get("nodes").and_then(Value::as_u64).unwrap_or(0),
            summary.get("edges").and_then(Value::as_u64).unwrap_or(0),
            summary.get("tasks").and_then(Value::as_u64).unwrap_or(0),
            summary.get("agents").and_then(Value::as_u64).unwrap_or(0),
            summary
                .get("artifacts")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary.get("commits").and_then(Value::as_u64).unwrap_or(0),
            summary
                .get("test_evidence")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("verifications")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary.get("merges").and_then(Value::as_u64).unwrap_or(0),
            summary
                .get("max_depth")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("depth_limited")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            summary
                .get("digest")
                .and_then(Value::as_str)
                .map(short_hash_str)
                .unwrap_or_else(|| "-".to_string())
        ));
        lines.push(format!(
            "window: {} of {} events (seq {}..{})",
            summary
                .get("scanned_events")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("total_events")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("window_start_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("window_end_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
    }
    if value
        .get("other_failures")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        lines.push(format!(
            "other failures in window: {} (not attributed by this report)",
            value
                .get("other_failures")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
    }
    if let Some(assignments) = value.get("assignments").and_then(Value::as_array)
        && !assignments.is_empty()
    {
        let identity = assignments
            .iter()
            .take(4)
            .map(|assignment| {
                format!(
                    "{} [{}] {}",
                    assignment
                        .get("identity_id")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    assignment
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    assignment
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "assignment context (not proof of a historical run identity): {identity}"
        ));
    }
    if let Some(warnings) = value.get("warnings").and_then(Value::as_array) {
        for warning in warnings.iter().take(8).filter_map(Value::as_str) {
            lines.push(format!("  warning: {}", bounded_line(warning, 220)));
        }
    }
    lines.push(
        "advisory only: this report never retries, cancels, merges, reassigns, or changes budgets, capacity, approvals, or plans"
            .to_string(),
    );
    Some(lines)
}

pub(crate) fn runtime_pin_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(status) = value.get("status").and_then(Value::as_str)
        && value.get("snapshot_age_secs").is_some()
        && value.get("checked_at").is_some()
    {
        let mut lines = vec![format!(
            "compatibility: {}  target={}  plan_revision={}  snapshot_age={}s",
            status.to_uppercase(),
            value.get("target").and_then(Value::as_str).unwrap_or("?"),
            value
                .get("plan_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("snapshot_age_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        )];
        if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
            lines.push(format!("run: {run_id}"));
        }
        if let Some(changed) = value.get("changed").and_then(Value::as_array) {
            for change in changed.iter().take(16) {
                lines.push(format!(
                    "  {}: {} -> {} ({})",
                    change.get("field").and_then(Value::as_str).unwrap_or("?"),
                    change.get("stored").and_then(Value::as_str).unwrap_or(""),
                    change.get("current").and_then(Value::as_str).unwrap_or(""),
                    change
                        .get("severity")
                        .and_then(Value::as_str)
                        .unwrap_or("drift")
                ));
            }
        }
        if let Some(warnings) = value.get("warnings").and_then(Value::as_array) {
            for warning in warnings.iter().take(16) {
                if let Some(warning) = warning.as_str() {
                    lines.push(format!("  warning: {warning}"));
                }
            }
        }
        if value
            .get("migration_required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            lines.push(
                "migration required: review the diff, then run `vp plan-migrate <session> --yes`"
                    .to_string(),
            );
        }
        return Some(lines);
    }
    if value.get("migrated").is_some() {
        let mut lines = vec![format!(
            "plan migration: migrated={}  revision={}  event_seq={}",
            value
                .get("migrated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            value
                .get("plan_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value.get("event_seq").and_then(Value::as_u64).unwrap_or(0)
        )];
        if let Some(snapshot) = value.get("snapshot") {
            lines.push(format!(
                "snapshot: runtime={} build={} policy={} bundle={} layout={} contract={}",
                snapshot
                    .get("runtime_version")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                snapshot
                    .get("runtime_build")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                short_optional_hash(snapshot.get("policy_hash").and_then(Value::as_str)),
                short_optional_hash(snapshot.get("tool_bundle_hash").and_then(Value::as_str)),
                snapshot
                    .get("prompt_layout_version")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                short_optional_hash(snapshot.get("contract_hash").and_then(Value::as_str)),
            ));
        }
        if let Some(warnings) = value.get("warnings").and_then(Value::as_array) {
            for warning in warnings.iter().take(16) {
                if let Some(warning) = warning.as_str() {
                    lines.push(format!("  note: {warning}"));
                }
            }
        }
        return Some(lines);
    }
    None
}

pub(crate) fn controller_replay_lines(value: &Value) -> Option<Vec<String>> {
    if !value
        .get("controller_equivalent_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let mut lines = vec![format!(
        "controller replay: matched={} mismatched={} skipped={} examined={} total={} truncated={}",
        value.get("matched").and_then(Value::as_u64).unwrap_or(0),
        value.get("mismatched").and_then(Value::as_u64).unwrap_or(0),
        value.get("skipped").and_then(Value::as_u64).unwrap_or(0),
        value.get("examined").and_then(Value::as_u64).unwrap_or(0),
        value
            .get("total_recorded")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        value
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    )];
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        for entry in entries.iter().take(200) {
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if status == "matched" {
                continue;
            }
            lines.push(format!(
                "  #{} {} {} recorded={} recomputed={}",
                entry.get("seq").and_then(Value::as_u64).unwrap_or(0),
                entry.get("kind").and_then(Value::as_str).unwrap_or("?"),
                status.to_uppercase(),
                entry
                    .get("recorded_action")
                    .and_then(Value::as_str)
                    .unwrap_or("-"),
                entry
                    .get("recomputed_action")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
            ));
            if let Some(changes) = entry.get("changes").and_then(Value::as_array) {
                for change in changes.iter().take(6) {
                    lines.push(format!(
                        "      {}: {} -> {}",
                        change.get("field").and_then(Value::as_str).unwrap_or("?"),
                        change.get("recorded").and_then(Value::as_str).unwrap_or(""),
                        change
                            .get("recomputed")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    ));
                }
            }
            if let Some(reason) = entry.get("reason").and_then(Value::as_str) {
                lines.push(format!("      reason: {reason}"));
            }
        }
    }
    lines.push(
        "controller-equivalent only; replay never proves model behavior and is not verification or merge evidence"
            .to_string(),
    );
    Some(lines)
}

pub(crate) fn gate_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(gate) = value.get("gate")
        && gate.is_object()
    {
        let mut lines = vec![format!(
            "gate {}  {}  {}  {}",
            gate.get("id").and_then(Value::as_str).unwrap_or("?"),
            gate.get("kind").and_then(Value::as_str).unwrap_or("?"),
            gate.get("state").and_then(Value::as_str).unwrap_or("?"),
            gate.get("label").and_then(Value::as_str).unwrap_or("")
        )];
        if let Some(outcome) = gate.get("outcome").and_then(Value::as_str) {
            lines.push(format!("outcome: {outcome}"));
        }
        if let Some(task) = gate.get("task_id").and_then(Value::as_str) {
            lines.push(format!("task: {task}"));
        }
        if let Some(expires) = gate.get("expires_at").and_then(Value::as_str) {
            lines.push(format!("expires: {expires}"));
        }
        if let Some(note) = gate.get("resolution_note").and_then(Value::as_str) {
            lines.push(format!("note: {note}"));
        }
        if let Some(changed) = value.get("changed").and_then(Value::as_bool) {
            lines.push(format!("changed: {changed}"));
        }
        if let Some(continuation) = value.get("continuation_id").and_then(Value::as_str) {
            lines.push(format!("continuation: {continuation}"));
        }
        if let Some(status) = value.get("continuation_status").and_then(Value::as_str) {
            lines.push(format!("continuation status: {status}"));
        }
        if let Some(token) = value.get("token").and_then(Value::as_str) {
            lines.push(String::new());
            lines.push("ONE-TIME GATE TOKEN (shown once; never stored in plaintext):".to_string());
            lines.push(token.to_string());
            if let Some(hint) = value.get("resolve_hint").and_then(Value::as_str) {
                lines.push(format!("resolve: {hint}"));
            }
            if value
                .get("loopback_resolve")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                lines.push(
                    "loopback resolve is enabled for this project's same-host CI (scoped token still required)"
                        .to_string(),
                );
            }
        }
        return Some(lines);
    }
    if let Some(gates) = value.get("gates").and_then(Value::as_array) {
        let counts = value.get("counts").cloned().unwrap_or_else(|| json!({}));
        let mut lines = vec![format!(
            "gates: {} total · {} open · {} resolved · {} expired · {} cancelled{}",
            counts.get("total").and_then(Value::as_u64).unwrap_or(0),
            counts.get("open").and_then(Value::as_u64).unwrap_or(0),
            counts.get("resolved").and_then(Value::as_u64).unwrap_or(0),
            counts.get("expired").and_then(Value::as_u64).unwrap_or(0),
            counts.get("cancelled").and_then(Value::as_u64).unwrap_or(0),
            if value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                " (truncated)"
            } else {
                ""
            },
        )];
        for gate in gates {
            lines.push(format!(
                "{}  {}  {}  {}{}",
                gate.get("id").and_then(Value::as_str).unwrap_or("?"),
                gate.get("state").and_then(Value::as_str).unwrap_or("?"),
                gate.get("kind").and_then(Value::as_str).unwrap_or("?"),
                gate.get("label").and_then(Value::as_str).unwrap_or(""),
                gate.get("outcome")
                    .and_then(Value::as_str)
                    .map(|outcome| format!(" → {outcome}"))
                    .unwrap_or_default()
            ));
        }
        return Some(lines);
    }
    if let Some(expired) = value.get("expired_gates").and_then(Value::as_u64) {
        return Some(vec![format!(
            "gate sweep: {expired} open gate(s) expired and their waits woken"
        )]);
    }
    if let Some(policy) = value.get("policy")
        && policy.get("loopback_resolve").is_some()
        && let Some(project) = value.get("project_id").and_then(Value::as_str)
    {
        return Some(vec![format!(
            "project {project}  gates loopback_resolve={}",
            policy
                .get("loopback_resolve")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )]);
    }
    None
}
