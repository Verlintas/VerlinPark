//! Human renderers for LSP, scaling, and remote workspace surfaces.

use super::*;

pub(crate) fn lsp_status_lines(value: &Value) -> Option<Vec<String>> {
    let languages = value.get("languages")?.as_array()?;
    value.get("supported_languages")?;
    let mut lines = vec![format!(
        "code intelligence · {} · {} running · {} installed",
        value["root"].as_str().unwrap_or("?"),
        value["running"].as_u64().unwrap_or(0),
        value["installed"].as_u64().unwrap_or(0)
    )];
    for language in languages {
        let state = if language["running"].as_bool().unwrap_or(false) {
            "running"
        } else if language["installed"].as_bool().unwrap_or(false) {
            "installed"
        } else {
            "missing"
        };
        let server = language["server"].as_str().unwrap_or("—");
        let capabilities = language["capabilities"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "—".to_string());
        lines.push(format!(
            "  {:<12} {:<10} {:<20} {}",
            language["display"].as_str().unwrap_or("?"),
            state,
            server,
            capabilities
        ));
        if state == "missing"
            && let Some(hint) = language["install_hint"].as_str()
            && !hint.is_empty()
        {
            lines.push(format!("    {hint}"));
        }
    }
    Some(lines)
}

pub(crate) fn scaling_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(decision) = value
        .get("decision")
        .filter(|decision| decision.is_object())
    {
        let mut lines = Vec::new();
        let session = value
            .get("session_id")
            .and_then(Value::as_str)
            .or_else(|| decision.get("session_id").and_then(Value::as_str))
            .unwrap_or("?");
        lines.push(format!(
            "scaling preview  {session}  action={}  Workers {}→{}  Labour {}→{}",
            decision
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("hold"),
            decision
                .get("current_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("target_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("current_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("target_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ));
        lines.push(format!(
            "  ready {} · running {} · active Workers {} · active Labour {} · revision {} · applied {}",
            decision
                .get("ready_tasks")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("running_tasks")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("active_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("active_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("plan_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("applied")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ));
        if let Some(reasons) = decision.get("reasons").and_then(Value::as_array) {
            lines.push(format!(
                "  reasons: {}",
                reasons
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        if let Some(hash) = decision.get("inputs_hash").and_then(Value::as_str) {
            let prefix = hash.get(..12).unwrap_or(hash);
            lines.push(format!("  inputs {prefix}…"));
        }
        if let Some(evidence) = decision.get("manager_evidence").and_then(Value::as_str) {
            lines.push(format!("  manager evidence: {evidence}"));
        }
        return Some(lines);
    }
    if value.get("global_worker_cap").is_some()
        && let Some(projects) = value.get("projects").and_then(Value::as_array)
    {
        let mut lines = vec![format!(
            "scaling  global Workers {}/{} available · Labour {}/{} available",
            value
                .get("global_worker_available")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_worker_cap")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_labour_available")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_labour_cap")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )];
        if projects.is_empty() {
            lines.push("  no projects".to_string());
            return Some(lines);
        }
        for project in projects {
            let name = project
                .get("project_name")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let policy = project.get("policy").unwrap_or(&Value::Null);
            lines.push(format!(
                "{name}  scaling={}  Workers {}-{}  Labour {}-{}  threshold {}  cooldown {}s  step {}  budget {}",
                if policy
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "on"
                } else {
                    "off"
                },
                policy
                    .get("min_workers")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_workers")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("min_labour")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_labour")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("expand_ready_threshold")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("cooldown_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_step")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("budget_headroom_ratio")
                    .and_then(Value::as_f64)
                    .map(|ratio| format!("≥{:.0}%", ratio * 100.0))
                    .unwrap_or_else(|| "off".to_string())
            ));
            for session in project
                .get("sessions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let latest = session.get("latest").unwrap_or(&Value::Null);
                let action = latest
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("none");
                lines.push(format!(
                    "  {}  {:<10}  rev {}  tasks {}/{} ready  slots {}W/{}L  active {}W/{}L  latest {}  {}",
                    session
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    session
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    session
                        .get("plan_revision")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("ready_tasks")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("plan_tasks")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("current_workers")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("current_labour")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("active_workers")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("active_labour")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    action,
                    session
                        .get("goal")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                ));
                if latest.is_object()
                    && let Some(reasons) = latest.get("reasons").and_then(Value::as_array)
                    && !reasons.is_empty()
                {
                    lines.push(format!(
                        "    {}",
                        reasons
                            .iter()
                            .filter_map(Value::as_str)
                            .take(6)
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ));
                }
            }
        }
        return Some(lines);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_workers").is_some() && policy.get("max_labour").is_some())
    {
        let mut lines = vec![format!(
            "scaling policy  {}  enabled={}",
            value
                .get("project_id")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            policy
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )];
        lines.push(format!(
            "  Workers {}-{}  Labour {}-{}  threshold {}  critical delay {}s  idle {}s  cooldown {}s  step {}",
            policy
                .get("min_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("min_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("expand_ready_threshold")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("expand_critical_delay_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("scale_down_idle_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("cooldown_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_step")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
        lines.push(format!(
            "  budget headroom: {}",
            policy
                .get("budget_headroom_ratio")
                .and_then(Value::as_f64)
                .map(|ratio| format!("≥{:.0}%", ratio * 100.0))
                .unwrap_or_else(|| "check disabled".to_string())
        ));
        return Some(lines);
    }
    None
}

pub(crate) fn remote_bootstrap_lines(value: &Value) -> Option<Vec<String>> {
    if value.get("used_sudo").is_some()
        && value.get("sha256").is_some()
        && value.get("host").is_some()
    {
        let mut lines = vec![format!(
            "remote bootstrap {}  {}:{}  {}",
            if value["replaced"].as_bool().unwrap_or(false) {
                "replaced"
            } else {
                "installed"
            },
            value["host"].as_str().unwrap_or("?"),
            value["install_path"].as_str().unwrap_or("?"),
            value["helper_version"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "  sha256 {}  bytes {}  protocol {}  target {}/{}  sudo {}",
            value["sha256"].as_str().unwrap_or("?"),
            value["bytes"].as_u64().unwrap_or(0),
            value["protocol_version"].as_u64().unwrap_or(0),
            value["target_os"].as_str().unwrap_or("?"),
            value["target_arch"].as_str().unwrap_or("?"),
            value["used_sudo"].as_bool().unwrap_or(true)
        ));
        lines.push(format!(
            "  risk {}  confirmation required  chunks {}  duration {}ms",
            value["risk"].as_str().unwrap_or("host_state"),
            value["chunks"].as_u64().unwrap_or(0),
            value["duration_ms"].as_u64().unwrap_or(0)
        ));
        if value["host_path_mismatch"].as_bool().unwrap_or(false) {
            lines.push(
                "  note: ExecutionHost.helper_path does not name the installed file; update the host config explicitly"
                    .to_string(),
            );
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat: {caveat}"));
        }
        return Some(lines);
    }
    if value.get("configured").is_some() && value.get("configured_helper_path").is_some() {
        let mut lines = vec![format!(
            "remote bootstrap status for {}: configured={} path={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["configured"].as_bool().unwrap_or(false),
            value["install_path"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "  helper config {}  path matches install {}  risk {}",
            value["configured_helper_path"].as_str().unwrap_or("?"),
            value["path_matches_config"].as_bool().unwrap_or(false),
            value["risk"].as_str().unwrap_or("host_state")
        ));
        if let Some(record) = value.get("record").filter(|record| !record.is_null()) {
            lines.push(format!(
                "  installed {}  version {}  protocol {}  sha256 {}  target {}/{}",
                record["installed_at"].as_str().unwrap_or("?"),
                record["helper_version"].as_str().unwrap_or("?"),
                record["protocol_version"].as_u64().unwrap_or(0),
                record["sha256"].as_str().unwrap_or("?"),
                record["target_os"].as_str().unwrap_or("?"),
                record["target_arch"].as_str().unwrap_or("?")
            ));
        } else {
            lines.push("  no bootstrap record for this host".to_string());
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat: {caveat}"));
        }
        return Some(lines);
    }
    None
}

pub(crate) fn remote_test_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(evidence) = value
        .get("evidence")
        .filter(|evidence| evidence.get("digest").is_some())
    {
        let mut lines = vec![format!(
            "remote test {}  {}  {}:{}  {}",
            evidence["status"].as_str().unwrap_or("?"),
            evidence["profile"].as_str().unwrap_or("?"),
            evidence["host"].as_str().unwrap_or("?"),
            evidence["commit"].as_str().unwrap_or("?"),
            evidence["command"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "  exit {}  duration {}ms  digest {}",
            evidence["exit_code"]
                .as_i64()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_string()),
            evidence["duration_ms"].as_u64().unwrap_or(0),
            evidence["digest"].as_str().unwrap_or("?")
        ));
        if let Some(output) = evidence["output_ref"].as_str() {
            lines.push(format!(
                "  output {}  sha256 {}  bytes {}  truncated {}",
                output,
                evidence["output_sha256"].as_str().unwrap_or("?"),
                evidence["output_bytes"].as_u64().unwrap_or(0),
                evidence["output_truncated"].as_bool().unwrap_or(false)
            ));
        }
        if value
            .get("supplemental")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            lines.push(
                "  supplemental: local Verifier and deterministic Integrator remain mandatory; the merge gate is unchanged"
                    .to_string(),
            );
        }
        if let Some(entry) = value.get("entry").filter(|entry| !entry.is_null()) {
            lines.push(format!(
                "  attached to queue entry {}  verifier {}  status {}",
                entry["id"].as_str().unwrap_or("?"),
                entry["verifier_approved"]
                    .as_bool()
                    .map(|approved| if approved { "approved" } else { "rejected" })
                    .unwrap_or("not yet run"),
                entry["status"].as_str().unwrap_or("?")
            ));
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat: {caveat}"));
        }
        return Some(lines);
    }
    if value.get("passed").is_some()
        && value.get("timed_out").is_some()
        && value.get("evidence").is_some()
    {
        let mut lines = vec![format!(
            "remote test evidence for {}: {} total · {} passed · {} failed · {} timeout",
            value["project_id"].as_str().unwrap_or("?"),
            value["total"].as_u64().unwrap_or(0),
            value["passed"].as_u64().unwrap_or(0),
            value["failed"].as_u64().unwrap_or(0),
            value["timed_out"].as_u64().unwrap_or(0)
        )];
        if let Some(policy) = value.get("policy") {
            lines.push(format!(
                "  policy enabled={} default={} profiles={}",
                policy["enabled"].as_bool().unwrap_or(false),
                policy["default_profile"].as_str().unwrap_or("-"),
                policy["profiles"].as_array().map(Vec::len).unwrap_or(0)
            ));
        }
        for row in value["evidence"].as_array().cloned().unwrap_or_default() {
            lines.push(format!(
                "  {}  {}  {}  {}  digest {}",
                row["status"].as_str().unwrap_or("?"),
                row["profile"].as_str().unwrap_or("?"),
                row["commit"].as_str().unwrap_or("?"),
                row["command"].as_str().unwrap_or("?"),
                row["digest"]
                    .as_str()
                    .map(|digest| &digest[..digest.len().min(12)])
                    .unwrap_or("?")
            ));
        }
        lines.push(
            "  supplemental only: remote evidence never bypasses the local Verifier or deterministic Integrator"
                .to_string(),
        );
        return Some(lines);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("profiles").is_some())
    {
        let mut lines = vec![format!(
            "remote test policy for {}: enabled={} default={} timeout={}s output_cap={}B",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(false),
            policy["default_profile"].as_str().unwrap_or("-"),
            policy["timeout_secs"].as_u64().unwrap_or(0),
            policy["output_cap_bytes"].as_u64().unwrap_or(0)
        )];
        for profile in policy["profiles"].as_array().cloned().unwrap_or_default() {
            lines.push(format!(
                "  {}  {}  {}",
                profile["name"].as_str().unwrap_or("?"),
                profile["kind"].as_str().unwrap_or("?"),
                profile["command"].as_str().unwrap_or("?")
            ));
        }
        return Some(lines);
    }
    None
}

pub(crate) fn remote_integration_lines(value: &Value) -> Option<Vec<String>> {
    if value.get("removed_refs").is_some() && value.get("skipped").is_some() {
        let refs = value["removed_refs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let worktrees = value["removed_worktrees"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let skipped = value["skipped"].as_array().cloned().unwrap_or_default();
        let mut lines = vec![format!(
            "remote integration cleanup for {}: examined {} · removed {} ref(s) · {} worktree(s) · {} skipped",
            value["project_id"].as_str().unwrap_or("?"),
            value["entries_examined"].as_u64().unwrap_or(0),
            refs.len(),
            worktrees.len(),
            skipped.len()
        )];
        for reference in refs {
            if let Some(reference) = reference.as_str() {
                lines.push(format!("  removed {reference}"));
            }
        }
        for worktree in worktrees {
            if let Some(worktree) = worktree.as_str() {
                lines.push(format!("  removed worktree {worktree}"));
            }
        }
        for reason in skipped {
            if let Some(reason) = reason.as_str() {
                lines.push(format!("  skipped {reason}"));
            }
        }
        return Some(lines);
    }
    if let (Some(entry), Some(bundle), Some(verification)) = (
        value.get("entry"),
        value.get("bundle"),
        value.get("verification"),
    ) {
        let mut lines = Vec::new();
        lines.push(format!(
            "remote integration {}  {}  {}:{}",
            entry["status"].as_str().unwrap_or("?"),
            entry["id"].as_str().unwrap_or("?"),
            bundle["host"].as_str().unwrap_or("?"),
            bundle["branch"].as_str().unwrap_or("?")
        ));
        lines.push(format!(
            "  commit {}  bundle sha256 {}  bytes {}",
            entry["commit"].as_str().unwrap_or("?"),
            bundle["sha256"].as_str().unwrap_or("?"),
            bundle["bytes"].as_u64().unwrap_or(0)
        ));
        lines.push(format!(
            "  verifier required={} performed={} approved={}",
            verification["required"].as_bool().unwrap_or(false),
            verification["performed"].as_bool().unwrap_or(false),
            verification["approved"]
                .as_bool()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "—".into())
        ));
        if let Some(summary) = verification["summary"].as_str() {
            lines.push(format!("  verdict {summary}"));
        }
        lines.push(format!(
            "  imported_ref {}  refs_pruned {}",
            value["imported_ref"].as_str().unwrap_or("—"),
            value["refs_pruned"].as_bool().unwrap_or(false)
        ));
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat {caveat}"));
        }
        return Some(lines);
    }
    if let Some(policy) = value.get("policy")
        && let Some(entries) = value.get("entries").and_then(Value::as_array)
    {
        let mut lines = vec![format!(
            "remote integration policy enabled={} verifier_required={} max_bundle_bytes={} keep_refs={}",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["require_verifier"].as_bool().unwrap_or(true),
            policy["max_bundle_bytes"].as_u64().unwrap_or(0),
            policy["keep_import_refs"].as_bool().unwrap_or(false)
        )];
        let hosts = policy["allowed_hosts"]
            .as_array()
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        lines.push(format!(
            "  allowed hosts: {}",
            if hosts.is_empty() {
                "(none)".to_string()
            } else {
                hosts
            }
        ));
        lines.push(format!(
            "  queue total {} · pending {} · merged {} · conflicts {} · failed {} · refs retained {}",
            value["total"].as_u64().unwrap_or(0),
            value["pending"].as_u64().unwrap_or(0),
            value["merged"].as_u64().unwrap_or(0),
            value["conflicts"].as_u64().unwrap_or(0),
            value["failed"].as_u64().unwrap_or(0),
            value["retained_refs"].as_u64().unwrap_or(0)
        ));
        if entries.is_empty() {
            lines.push("  no remote queue entries".to_string());
        }
        for entry in entries {
            lines.push(format!(
                "  {}  {:<10}  {}  {}:{}  attempt {}  {}",
                entry["entry_id"].as_str().unwrap_or("?"),
                entry["status"].as_str().unwrap_or("?"),
                entry["commit"]
                    .as_str()
                    .unwrap_or("?")
                    .chars()
                    .take(12)
                    .collect::<String>(),
                entry["remote_host"].as_str().unwrap_or("?"),
                entry["remote_branch"].as_str().unwrap_or("?"),
                entry["attempt"].as_u64().unwrap_or(0),
                entry["imported_ref"].as_str().unwrap_or("—")
            ));
            if let Some(approved) = entry["verifier_approved"].as_bool() {
                lines.push(format!(
                    "    verifier {}: {}",
                    if approved { "approved" } else { "rejected" },
                    entry["verifier_summary"].as_str().unwrap_or("no summary")
                ));
            }
            if let Some(detail) = entry["detail"].as_str()
                && !detail.is_empty()
            {
                lines.push(format!("    {detail}"));
            }
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat {caveat}"));
        }
        return Some(lines);
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && value.get("entries").is_none()
    {
        let hosts = policy["allowed_hosts"]
            .as_array()
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        return Some(vec![
            format!(
                "remote integration policy for {} enabled={} verifier_required={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["enabled"].as_bool().unwrap_or(false),
                policy["require_verifier"].as_bool().unwrap_or(true)
            ),
            format!(
                "  max_bundle_bytes {} · keep_import_refs {} · allowed hosts {}",
                policy["max_bundle_bytes"].as_u64().unwrap_or(0),
                policy["keep_import_refs"].as_bool().unwrap_or(false),
                if hosts.is_empty() {
                    "(none)".to_string()
                } else {
                    hosts
                }
            ),
        ]);
    }
    None
}
