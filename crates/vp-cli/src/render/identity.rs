//! Human renderers for project identities, workspaces, and coordination.

use super::support::*;
use super::*;

pub(crate) fn agent_memory_lines(value: &Value) -> Option<Vec<String>> {
    let line = |memory: &Value| {
        let id = memory["id"].as_str().unwrap_or("?");
        let scope = memory["scope"].as_str().unwrap_or("?");
        let status = if memory["expired"].as_bool().unwrap_or(false) {
            "expired"
        } else {
            memory["status"].as_str().unwrap_or("?")
        };
        let source = memory["source"].as_str().unwrap_or("?");
        let content = memory["content"].as_str().unwrap_or("");
        let evidence = memory["evidence"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let kind = row["kind"].as_str()?;
                        let reference = row["reference"].as_str()?;
                        Some(format!("{kind} {reference}"))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let provenance = if evidence.is_empty() {
            String::new()
        } else {
            format!(" · {}", evidence.join(", "))
        };
        format!("{id}  {scope:<10}  {status:<10}  {source:<12}  {content}{provenance}")
    };
    if let Some(memories) = value.get("memories").and_then(Value::as_array)
        && memories.iter().all(|memory| memory.get("scope").is_some())
    {
        if memories.is_empty() {
            return Some(vec!["No identity memories.".to_string()]);
        }
        let mut lines = Vec::with_capacity(memories.len() + 1);
        lines.push(format!(
            "{} identity memories (total {})",
            memories.len(),
            value["total"].as_u64().unwrap_or(memories.len() as u64)
        ));
        lines.extend(memories.iter().map(line));
        return Some(lines);
    }
    if let Some(memory) = value
        .get("memory")
        .filter(|memory| memory.get("scope").is_some())
    {
        let suffix = if value["changed"].as_bool().unwrap_or(false) {
            " (changed)"
        } else {
            " (unchanged)"
        };
        return Some(vec![format!("{}{}", line(memory), suffix)]);
    }
    if let (Some(previous), Some(replacement)) = (
        value
            .get("previous")
            .filter(|memory| memory.get("scope").is_some()),
        value
            .get("replacement")
            .filter(|memory| memory.get("scope").is_some()),
    ) {
        return Some(vec![
            format!("superseded {}", line(previous)),
            format!("replacement {}", line(replacement)),
        ]);
    }
    if value.get("memory_id").is_some()
        && value.get("pruned").is_some()
        && value.get("changed").is_some()
    {
        return Some(vec![format!(
            "{}  {}",
            value["memory_id"].as_str().unwrap_or("?"),
            if value["pruned"].as_bool().unwrap_or(false) {
                "pruned"
            } else if value["changed"].as_bool().unwrap_or(false) {
                "forgotten (stale)"
            } else {
                "unchanged"
            }
        )]);
    }
    if value.get("expired").is_some()
        && value.get("pruned").is_some()
        && value.get("remaining").is_some()
    {
        return Some(vec![format!(
            "expired {} · pruned {} · remaining {}",
            value["expired"].as_u64().unwrap_or(0),
            value["pruned"].as_u64().unwrap_or(0),
            value["remaining"].as_u64().unwrap_or(0)
        )]);
    }
    None
}

pub(crate) fn team_template_lines(value: &Value) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    if let Some(markdown) = value.get("markdown").and_then(Value::as_str)
        && value.get("entry").is_some()
    {
        lines.extend(markdown.lines().map(str::to_string));
        return Some(lines);
    }
    if let Some(templates) = value.get("templates").and_then(Value::as_array) {
        if templates.is_empty() {
            lines.push("No team templates.".to_string());
            return Some(lines);
        }
        for entry in templates {
            lines.push(format!(
                "{:<28} {:<10} {:<6} {:<10} {}",
                entry["template_id"].as_str().unwrap_or("?"),
                entry["version"].as_str().unwrap_or("?"),
                if entry["applied"].as_bool().unwrap_or(false) {
                    "applied"
                } else {
                    "library"
                },
                short_hash(&entry["content_hash"]),
                entry["name"].as_str().unwrap_or("")
            ));
            if entry["read_only"].as_bool().unwrap_or(false) {
                lines.push("  read-only until explicit apply".to_string());
            }
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            lines.push("… truncated".to_string());
        }
        return Some(lines);
    }
    if let Some(preview) = value.get("preview") {
        let name = preview["name"].as_str().unwrap_or("?");
        let version = preview["version"].as_str().unwrap_or("?");
        lines.push(format!(
            "{name} v{version}  hash {}  apply {}",
            short_hash(&preview["content_hash"]),
            if preview["apply_allowed"].as_bool().unwrap_or(false) {
                "allowed"
            } else {
                "blocked"
            }
        ));
        if preview["leader_overlay"].as_bool().unwrap_or(false) {
            lines.push("  Leader: existing overlay (never overwritten)".to_string());
        }
        for line in preview["proposed_org"].as_array().into_iter().flatten() {
            if let Some(line) = line.as_str() {
                lines.push(format!("  {line}"));
            }
        }
        for conflict in preview["conflicts"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  conflict {}: {}",
                conflict["slot"].as_str().unwrap_or("?"),
                conflict["message"].as_str().unwrap_or("")
            ));
        }
        for gap in preview["model_gaps"].as_array().into_iter().flatten() {
            lines.push(format!("  model gap: {}", gap.as_str().unwrap_or("")));
        }
        for gap in preview["tool_gaps"].as_array().into_iter().flatten() {
            lines.push(format!("  tool gap: {}", gap.as_str().unwrap_or("")));
        }
        for conflict in preview["policy_conflicts"].as_array().into_iter().flatten() {
            lines.push(format!("  policy: {}", conflict.as_str().unwrap_or("")));
        }
        for warning in preview["warnings"].as_array().into_iter().flatten() {
            lines.push(format!("  warning: {}", warning.as_str().unwrap_or("")));
        }
        if value["applied"].as_bool().unwrap_or(false) {
            lines.push(format!(
                "  applied {} pending hire request(s)",
                value["pending_hire_request_ids"]
                    .as_array()
                    .map_or(0, Vec::len)
            ));
        }
        return Some(lines);
    }
    if let Some(rows) = value.get("rows").and_then(Value::as_array) {
        lines.push(format!(
            "diff {}  template {}  hash {}",
            value["project_id"].as_str().unwrap_or("?"),
            value["template_id"].as_str().unwrap_or("?"),
            short_hash(&value["content_hash"])
        ));
        if rows.is_empty() {
            lines.push("  no differences".to_string());
        }
        for row in rows {
            lines.push(format!(
                "  {:<14} {:<24} {}",
                row["kind"].as_str().unwrap_or("?"),
                row["slot"].as_str().unwrap_or("?"),
                row["detail"].as_str().unwrap_or("")
            ));
        }
        return Some(lines);
    }
    if let Some(template) = value.get("template")
        && value.get("json").is_some()
    {
        lines.push(format!(
            "exported {} v{}  hash {}",
            template["template_id"].as_str().unwrap_or("?"),
            template["version"].as_str().unwrap_or("?"),
            short_hash(&template["content_hash"])
        ));
        if let Some(path) = value.get("json_path").and_then(Value::as_str) {
            lines.push(format!("  json     {path}"));
        }
        if let Some(path) = value.get("markdown_path").and_then(Value::as_str) {
            lines.push(format!("  markdown {path}"));
        }
        for warning in value["warnings"].as_array().into_iter().flatten() {
            lines.push(format!("  warning: {}", warning.as_str().unwrap_or("")));
        }
        return Some(lines);
    }
    if value.get("template_id").is_some()
        && value.get("json").is_some()
        && value.get("out").is_none()
    {
        lines.push(format!(
            "exported {} v{}  hash {}",
            value["template_id"].as_str().unwrap_or("?"),
            value["version"].as_str().unwrap_or("?"),
            short_hash(&value["content_hash"])
        ));
        if let Some(path) = value.get("json_path").and_then(Value::as_str) {
            lines.push(format!("  json     {path}"));
        }
        return Some(lines);
    }
    if value.get("repaired_links").is_some() && value.get("mismatches").is_some() {
        lines.push(format!(
            "template replay {}  events {} · templates {} · links {} · repaired {}",
            value["project_id"].as_str().unwrap_or("?"),
            value["events"].as_u64().unwrap_or(0),
            value["templates"].as_u64().unwrap_or(0),
            value["links"].as_u64().unwrap_or(0),
            value["repaired_links"].as_u64().unwrap_or(0)
        ));
        for mismatch in value["mismatches"].as_array().into_iter().flatten() {
            lines.push(format!("  mismatch: {}", mismatch.as_str().unwrap_or("")));
        }
        return Some(lines);
    }
    None
}

pub(crate) fn agent_org_lines(value: &Value) -> Option<Vec<String>> {
    let counts = value.get("counts")?;
    counts.get("identities")?;
    let mut lines = Vec::new();
    let project_id = value["project_id"].as_str().unwrap_or("?");
    let enabled = value["enabled"].as_bool().unwrap_or(false);
    lines.push(format!(
        "Project org {project_id} · {} identities · {} leaders · {} managers · {} workers · policy {}",
        counts["identities"].as_u64().unwrap_or(0),
        counts["leaders"].as_u64().unwrap_or(0),
        counts["managers"].as_u64().unwrap_or(0),
        counts["workers"].as_u64().unwrap_or(0),
        if enabled { "enabled" } else { "disabled" }
    ));
    lines.push(format!(
        "status active {} · idle {} · paused {} · retired {} · imported {} · assignments {} · workspaces {} · unread {} · memory {}",
        counts["active"].as_u64().unwrap_or(0),
        counts["idle"].as_u64().unwrap_or(0),
        counts["paused"].as_u64().unwrap_or(0),
        counts["retired"].as_u64().unwrap_or(0),
        counts["imported"].as_u64().unwrap_or(0),
        counts["active_assignments"].as_u64().unwrap_or(0),
        counts["active_workspaces"].as_u64().unwrap_or(0),
        counts["unread_inbox"].as_u64().unwrap_or(0),
        counts["memory_count"].as_u64().unwrap_or(0)
    ));
    lines.push(format!(
        "pending hires {} · pending retirement proposals {} · advisory candidates {}",
        counts["pending_hire_requests"].as_u64().unwrap_or(0),
        counts["pending_retirement_proposals"].as_u64().unwrap_or(0),
        counts["retirement_candidates"].as_u64().unwrap_or(0)
    ));
    if let Some(leader) = value.get("leader").filter(|row| !row.is_null()) {
        lines.push("Leader".to_string());
        agent_org_node_lines(leader, 1, &mut lines);
    } else {
        lines.push("Leader: none".to_string());
    }
    if let Some(unassigned) = value["unassigned"].as_array()
        && !unassigned.is_empty()
    {
        lines.push(format!(
            "Unassigned {} (no parent in the active hierarchy)",
            unassigned.len()
        ));
        for node in unassigned {
            agent_org_node_lines(node, 1, &mut lines);
        }
    }
    if let Some(departments) = value["departments"].as_array() {
        for department in departments {
            lines.push(format!(
                "Department {} · {} managers · {} workers · {} active · unread {}",
                department["name"].as_str().unwrap_or("?"),
                department["manager_count"].as_u64().unwrap_or(0),
                department["worker_count"].as_u64().unwrap_or(0),
                department["active_count"].as_u64().unwrap_or(0),
                department["unread_inbox"].as_u64().unwrap_or(0)
            ));
        }
    }
    if let Some(requests) = value["pending_hire_requests"].as_array()
        && !requests.is_empty()
    {
        lines.push(format!("Pending hire requests {}", requests.len()));
        for request in requests {
            lines.push(format!(
                "  {} · {} · {} · requested {}",
                request["id"].as_str().unwrap_or("?"),
                request["role"].as_str().unwrap_or("?"),
                request["title"]
                    .as_str()
                    .or_else(|| request["component"].as_str())
                    .unwrap_or(""),
                request["requested_at"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(proposals) = value["pending_retirement_proposals"].as_array()
        && !proposals.is_empty()
    {
        lines.push(format!("Pending retirement proposals {}", proposals.len()));
        for proposal in proposals {
            lines.push(format!(
                "  {} · identity {} · {} · proposed {}",
                proposal["id"].as_str().unwrap_or("?"),
                proposal["identity_id"].as_str().unwrap_or("?"),
                proposal["reason_code"].as_str().unwrap_or("?"),
                proposal["proposed_at"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(assignments) = value["active_assignments"].as_array()
        && !assignments.is_empty()
    {
        lines.push(format!("Active assignments {}", assignments.len()));
        for assignment in assignments {
            lines.push(format!(
                "session {} · identity {} · {} · lease {}",
                assignment["session_id"].as_str().unwrap_or("?"),
                assignment["identity_id"].as_str().unwrap_or("?"),
                assignment["status"].as_str().unwrap_or("?"),
                if assignment["lease_active"].as_bool().unwrap_or(false) {
                    "active"
                } else {
                    "expired"
                }
            ));
        }
    }
    for warning in value["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            lines.push(format!("warning: {warning}"));
        }
    }
    Some(lines)
}

pub(crate) fn agent_org_node_lines(node: &Value, depth: usize, lines: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    let identity = &node["identity"];
    let mut detail = vec![format!(
        "{}{} · {} · {} · gen {}",
        indent,
        identity["title"]
            .as_str()
            .or_else(|| identity["component"].as_str())
            .unwrap_or_else(|| identity["role"].as_str().unwrap_or("?")),
        identity["role"].as_str().unwrap_or("?"),
        identity["status"].as_str().unwrap_or("?"),
        identity["generation"].as_u64().unwrap_or(0)
    )];
    if let Some(department) = identity["department"].as_str() {
        detail.push(format!("department {department}"));
    }
    if let Some(component) = identity["component"].as_str() {
        detail.push(format!("component {component}"));
    }
    if identity["origin"].as_str() == Some("imported") {
        detail.push("imported/read-only".to_string());
    }
    if let Some(assignment) = node.get("active_assignment").filter(|row| !row.is_null()) {
        detail.push(format!(
            "session {} (lease {})",
            assignment["session_id"].as_str().unwrap_or("?"),
            if assignment["lease_active"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "expired"
            }
        ));
    }
    if let Some(workspace) = node.get("workspace").filter(|row| !row.is_null()) {
        detail.push(format!(
            "workspace {} · {}",
            workspace["component"].as_str().unwrap_or("?"),
            workspace["status"].as_str().unwrap_or("?")
        ));
    }
    let unread = node["unread_inbox"].as_u64().unwrap_or(0);
    let memory = node["memory_count"].as_u64().unwrap_or(0);
    if unread > 0 || memory > 0 {
        detail.push(format!("inbox {unread} · memory {memory}"));
    }
    let performance = &node["performance"];
    let completed = performance["completed_tasks"].as_u64().unwrap_or(0);
    let failed = performance["failed_tasks"].as_u64().unwrap_or(0);
    let approvals = performance["verifier_approvals"].as_u64().unwrap_or(0);
    let rejections = performance["verifier_rejections"].as_u64().unwrap_or(0);
    if completed > 0 || failed > 0 || approvals > 0 || rejections > 0 {
        detail.push(format!(
            "tasks {completed}/{failed} · verifier {approvals}/{rejections}"
        ));
    }
    let retirement = &node["retirement"];
    if retirement["candidate"].as_bool().unwrap_or(false) {
        detail.push(format!(
            "retirement candidate severity {}",
            retirement["severity"].as_u64().unwrap_or(0)
        ));
    }
    if retirement["pending_proposals"].as_u64().unwrap_or(0) > 0 {
        detail.push("retirement proposal pending".to_string());
    }
    lines.push(detail.join(" · "));
    for report in node["reports"].as_array().into_iter().flatten() {
        agent_org_node_lines(report, depth + 1, lines);
    }
}

pub(crate) fn agent_detail_lines(value: &Value) -> Option<Vec<String>> {
    let identity = value.get("identity")?;
    if identity.get("project_id").is_none() || value.get("node").is_none() {
        return None;
    }
    let mut lines = Vec::new();
    let mut header = vec![format!(
        "{} · {} · {} · gen {}",
        identity["title"]
            .as_str()
            .or_else(|| identity["component"].as_str())
            .unwrap_or_else(|| identity["role"].as_str().unwrap_or("?")),
        identity["role"].as_str().unwrap_or("?"),
        identity["status"].as_str().unwrap_or("?"),
        identity["generation"].as_u64().unwrap_or(0)
    )];
    header.push(format!("id {}", identity["id"].as_str().unwrap_or("?")));
    if let Some(department) = identity["department"].as_str() {
        header.push(format!("department {department}"));
    }
    if let Some(component) = identity["component"].as_str() {
        header.push(format!("component {component}"));
    }
    if let Some(parent) = identity["parent_identity_id"].as_str() {
        header.push(format!("parent {parent}"));
    }
    if let Some(successor) = identity["successor_id"].as_str() {
        header.push(format!("successor {successor}"));
    }
    if value["read_only"].as_bool().unwrap_or(false) {
        header.push("read-only".to_string());
    }
    lines.push(header.join(" · "));
    if let Some(reason) = value["read_only_reason"].as_str() {
        lines.push(format!("read-only: {reason}"));
    }
    let node = &value["node"];
    if let Some(assignment) = node.get("active_assignment").filter(|row| !row.is_null()) {
        lines.push(format!(
            "active assignment session {} · lease {} · {} assignments / {} sessions",
            assignment["session_id"].as_str().unwrap_or("?"),
            if assignment["lease_active"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "expired"
            },
            assignment["assignment_count"].as_u64().unwrap_or(0),
            assignment["session_count"].as_u64().unwrap_or(0)
        ));
    } else {
        lines.push("active assignment: none".to_string());
    }
    if let Some(workspace) = node.get("workspace").filter(|row| !row.is_null()) {
        lines.push(format!(
            "workspace {} · {} · branch {} · lease {}",
            workspace["component"].as_str().unwrap_or("?"),
            workspace["status"].as_str().unwrap_or("?"),
            workspace["branch"].as_str().unwrap_or("?"),
            if workspace["lease_active"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "idle"
            }
        ));
    }
    lines.push(format!(
        "inbox unread {} / read {} / done {} · memory {}/{}",
        value["inbox_counts"]["unread"].as_u64().unwrap_or(0),
        value["inbox_counts"]["read"].as_u64().unwrap_or(0),
        value["inbox_counts"]["done"].as_u64().unwrap_or(0),
        value["memories"].as_array().map(Vec::len).unwrap_or(0),
        value["memory_total"].as_u64().unwrap_or(0)
    ));
    let performance = &value["performance"];
    lines.push(format!(
        "performance completed {} · failed {} · consecutive {} · verifier {}/{} · tokens {}/{} · cost ${:.3}",
        performance["completed_tasks"].as_u64().unwrap_or(0),
        performance["failed_tasks"].as_u64().unwrap_or(0),
        performance["consecutive_failures"].as_u64().unwrap_or(0),
        performance["verifier_approvals"].as_u64().unwrap_or(0),
        performance["verifier_rejections"].as_u64().unwrap_or(0),
        performance["input_tokens"].as_u64().unwrap_or(0),
        performance["output_tokens"].as_u64().unwrap_or(0),
        performance["cost_usd"].as_f64().unwrap_or(0.0)
    ));
    if let Some(assignments) = value["assignments"].as_array()
        && !assignments.is_empty()
    {
        lines.push(format!("Assignments {}", assignments.len()));
        for assignment in assignments {
            lines.push(format!(
                "  session {} · {} · assigned {} · lease {}",
                assignment["session_id"].as_str().unwrap_or("?"),
                assignment["status"].as_str().unwrap_or("?"),
                assignment["assigned_at"].as_str().unwrap_or("?"),
                if assignment["lease_active"].as_bool().unwrap_or(false) {
                    "active"
                } else {
                    "expired"
                }
            ));
        }
    }
    if let Some(inbox) = value["inbox"].as_array()
        && !inbox.is_empty()
    {
        lines.push(format!("Inbox {}", inbox.len()));
        for message in inbox {
            let mut row = vec![format!(
                "  {} · {} · {} · from {}",
                message["kind"].as_str().unwrap_or("?"),
                message["status"].as_str().unwrap_or("?"),
                message["created_at"].as_str().unwrap_or("?"),
                message["from_label"].as_str().unwrap_or("?")
            )];
            if let Some(summary) = message["summary"].as_str() {
                row.push(summary.to_string());
            }
            lines.push(row.join(" · "));
        }
    }
    if let Some(memories) = value["memories"].as_array()
        && !memories.is_empty()
    {
        lines.push(format!("Memory {}", memories.len()));
        for memory in memories {
            lines.push(format!(
                "  {} · {} · {} · {}",
                memory["scope"].as_str().unwrap_or("?"),
                memory["status"].as_str().unwrap_or("?"),
                memory["created_at"].as_str().unwrap_or("?"),
                memory["content"].as_str().unwrap_or("")
            ));
        }
    }
    if let Some(candidates) = value["retirement_candidates"].as_array()
        && !candidates.is_empty()
    {
        lines.push("Retirement candidates (advisory, explicit approval required)".to_string());
        for candidate in candidates {
            lines.push(format!(
                "  severity {} · {}",
                candidate["severity"].as_u64().unwrap_or(0),
                candidate["reasons"]
                    .as_array()
                    .map(|reasons| reasons
                        .iter()
                        .map(|reason| format!(
                            "{} {}/{}",
                            reason["code"].as_str().unwrap_or("?"),
                            reason["count"].as_u64().unwrap_or(0),
                            reason["threshold"].as_u64().unwrap_or(0)
                        ))
                        .collect::<Vec<_>>()
                        .join(" · "))
                    .unwrap_or_default()
            ));
        }
    }
    if let Some(proposals) = value["retirement_proposals"].as_array()
        && !proposals.is_empty()
    {
        lines.push("Retirement proposals".to_string());
        for proposal in proposals {
            lines.push(format!(
                "  {} · {} · {} · reason {}",
                proposal["id"].as_str().unwrap_or("?"),
                proposal["status"].as_str().unwrap_or("?"),
                proposal["proposed_at"].as_str().unwrap_or("?"),
                proposal["reason_code"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(requests) = value["hire_requests"].as_array()
        && !requests.is_empty()
    {
        lines.push("Hire requests".to_string());
        for request in requests {
            lines.push(format!(
                "  {} · {} · {} · role {}",
                request["id"].as_str().unwrap_or("?"),
                request["status"].as_str().unwrap_or("?"),
                request["requested_at"].as_str().unwrap_or("?"),
                request["role"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(events) = value["employment_events"].as_array()
        && !events.is_empty()
    {
        lines.push("Employment events".to_string());
        for event in events {
            lines.push(format!(
                "  {} · {} · {}",
                event["kind"].as_str().unwrap_or("?"),
                event["created_at"].as_str().unwrap_or("?"),
                event["id"].as_str().unwrap_or("?")
            ));
        }
    }
    for warning in value["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            lines.push(format!("warning: {warning}"));
        }
    }
    Some(lines)
}

pub(crate) fn agent_identity_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(identities) = value.get("identities").and_then(Value::as_array)
        && identities
            .iter()
            .all(|identity| identity.get("project_id").is_some())
    {
        if identities.is_empty() {
            return Some(vec!["No project identities.".to_string()]);
        }
        let mut lines = Vec::with_capacity(identities.len() + 1);
        lines.push(format!("{} project identities", identities.len()));
        for identity in identities {
            let id = identity["id"].as_str().unwrap_or("?");
            let role = identity["role"].as_str().unwrap_or("?");
            let status = identity["status"].as_str().unwrap_or("?");
            let generation = identity["generation"].as_u64().unwrap_or(0);
            let mut detail = vec![format!("{id}  {role:<7}  {status:<7}  gen {generation}")];
            if let Some(component) = identity["component"].as_str() {
                detail.push(format!("component {component}"));
            }
            if let Some(department) = identity["department"].as_str() {
                detail.push(format!("department {department}"));
            }
            if let Some(title) = identity["title"].as_str() {
                detail.push(format!("title {title}"));
            }
            if let Some(parent) = identity["parent_identity_id"].as_str() {
                detail.push(format!("parent {parent}"));
            }
            if let Some(successor) = identity["successor_id"].as_str() {
                detail.push(format!("successor {successor}"));
            }
            if identity["origin"].as_str() == Some("imported") {
                detail.push("imported/read-only".to_string());
            }
            lines.push(detail.join(" · "));
        }
        return Some(lines);
    }
    if let Some(identity) = value
        .get("identity")
        .filter(|identity| identity.get("project_id").is_some())
    {
        let changed = if value["changed"].as_bool().unwrap_or(false) {
            " (changed)"
        } else {
            " (unchanged)"
        };
        let mut lines = vec![format!(
            "{}  {}{}",
            identity["id"].as_str().unwrap_or("?"),
            identity["status"].as_str().unwrap_or("?"),
            changed
        )];
        lines.push(
            agent_identity_lines(&json!({"identities": [identity]}))
                .and_then(|lines| lines.into_iter().nth(1))
                .unwrap_or_default(),
        );
        return Some(lines);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_managers").is_some())
    {
        return Some(vec![format!(
            "identities {} · managers ≤ {} · workers ≤ {} · hire approval {} · retirement approval {}",
            if policy["enabled"].as_bool().unwrap_or(false) {
                "enabled"
            } else {
                "disabled"
            },
            policy["max_managers"].as_u64().unwrap_or(0),
            policy["max_workers"].as_u64().unwrap_or(0),
            policy["hire_requires_approval"].as_bool().unwrap_or(true),
            policy["retirement_requires_approval"]
                .as_bool()
                .unwrap_or(true),
        )]);
    }
    None
}

pub(crate) fn coordination_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(assignments) = value.get("assignments").and_then(Value::as_array) {
        if assignments.is_empty() {
            return Some(vec!["No session assignments.".to_string()]);
        }
        let mut lines = Vec::with_capacity(assignments.len() + 1);
        lines.push(format!("{} session assignments", assignments.len()));
        for assignment in assignments {
            let latest = assignment["status"].as_str().unwrap_or("?");
            let lease = assignment["lease_expires_at"]
                .as_str()
                .map(|expires| format!(" · lease {expires}"))
                .unwrap_or_default();
            let active = if assignment["lease_active"].as_bool().unwrap_or(false) {
                " · live"
            } else {
                ""
            };
            lines.push(format!(
                "{}  {}  {}  {}{}{}",
                assignment["session_id"].as_str().unwrap_or("?"),
                assignment["identity_id"].as_str().unwrap_or("?"),
                assignment["role"].as_str().unwrap_or("?"),
                latest,
                active,
                lease
            ));
        }
        return Some(lines);
    }
    if let Some(assignment) = value.get("assignment")
        && (value.get("created").is_some()
            || value.get("released").is_some()
            || value.get("renewed").is_some())
    {
        let changed = if value["created"].as_bool().unwrap_or(false) {
            "created"
        } else if value["released"].as_bool().unwrap_or(false) {
            "released"
        } else if value["renewed"].as_bool().unwrap_or(false) {
            "renewed"
        } else {
            "unchanged"
        };
        let mut lines = vec![format!(
            "{}  {}  {}  {changed}",
            assignment["session_id"].as_str().unwrap_or("?"),
            assignment["identity_id"].as_str().unwrap_or("?"),
            assignment["status"].as_str().unwrap_or("?")
        )];
        if let Some(wake) = value.get("wake")
            && wake["requested"].as_bool().unwrap_or(false)
        {
            lines.push(format!(
                "wake: {} ({})",
                if wake["created"].as_bool().unwrap_or(false) {
                    "deferred continuation created"
                } else {
                    "not created"
                },
                wake["reason"].as_str().unwrap_or("unknown")
            ));
        }
        return Some(lines);
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array)
        && value.get("counts").is_some()
    {
        let counts = &value["counts"];
        let mut lines = vec![format!(
            "{} inbox rows · unread {} · read {} · done {} · expired {}",
            messages.len(),
            counts["unread"].as_u64().unwrap_or(0),
            counts["read"].as_u64().unwrap_or(0),
            counts["done"].as_u64().unwrap_or(0),
            counts["expired"].as_u64().unwrap_or(0)
        )];
        for message in messages {
            let summary = message["payload"]
                .get("summary")
                .or_else(|| message["payload"].get("body"))
                .or_else(|| message["payload"].get("title"))
                .and_then(Value::as_str)
                .unwrap_or("");
            lines.push(format!(
                "{}  {}  {}  {}  {}{}",
                message["id"].as_str().unwrap_or("?"),
                message["status"].as_str().unwrap_or("?"),
                message["kind"].as_str().unwrap_or("?"),
                message["from_label"].as_str().unwrap_or("operator"),
                summary,
                message["expires_at"]
                    .as_str()
                    .map(|expires| format!(" · expires {expires}"))
                    .unwrap_or_default()
            ));
        }
        return Some(lines);
    }
    if let Some(message) = value.get("message")
        && value.get("changed").is_some()
        && message.get("identity_id").is_some()
    {
        return Some(vec![format!(
            "{}  {}  {}{}",
            message["id"].as_str().unwrap_or("?"),
            message["status"].as_str().unwrap_or("?"),
            message["kind"].as_str().unwrap_or("?"),
            if value["changed"].as_bool().unwrap_or(false) {
                " (changed)"
            } else {
                " (unchanged)"
            }
        )]);
    }
    if value.get("consistent").is_some()
        && value.get("assignments").is_some()
        && value.get("inbox").is_some()
        && value.get("events").is_some()
    {
        return Some(vec![format!(
            "coordination replay: assignments {} · inbox {} · events {} · {}",
            value["assignments"].as_u64().unwrap_or(0),
            value["inbox"].as_u64().unwrap_or(0),
            value["events"].as_u64().unwrap_or(0),
            if value["consistent"].as_bool().unwrap_or(false) {
                "consistent"
            } else {
                "drift detected"
            }
        )]);
    }
    None
}

pub(crate) fn workspace_line(workspace: &Value) -> String {
    let lease = if workspace["lease_active"].as_bool().unwrap_or(false) {
        " · live lease"
    } else {
        ""
    };
    format!(
        "{}  {}  {}  {}  {}{}",
        workspace["id"].as_str().unwrap_or("?"),
        workspace["identity_id"].as_str().unwrap_or("?"),
        workspace["component"].as_str().unwrap_or("?"),
        workspace["status"].as_str().unwrap_or("?"),
        workspace["branch"].as_str().unwrap_or("?"),
        lease
    )
}

pub(crate) fn workspace_session_line(session: &Value) -> String {
    format!(
        "{}  {}  {}  {}  {}",
        session["session_id"].as_str().unwrap_or("?"),
        session["identity_id"].as_str().unwrap_or("?"),
        session["workspace_id"].as_str().unwrap_or("?"),
        session["status"].as_str().unwrap_or("?"),
        session["branch"].as_str().unwrap_or("?")
    )
}

pub(crate) fn workspace_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(bindings) = value.get("bindings").and_then(Value::as_array) {
        if bindings.is_empty() {
            return Some(vec![format!(
                "session {} has no Worker workspaces.",
                value["session_id"].as_str().unwrap_or("?")
            )]);
        }
        let mut lines = vec![format!(
            "session {} · {} Worker workspace(s)",
            value["session_id"].as_str().unwrap_or("?"),
            bindings.len()
        )];
        lines.extend(bindings.iter().map(workspace_session_line));
        return Some(lines);
    }
    if value.get("landed_commit").is_some() {
        let status = if value["already_up_to_date"].as_bool().unwrap_or(false) {
            "already up to date"
        } else {
            "landed"
        };
        let mut lines = vec![format!(
            "workspace {} · {} · session {}",
            value["workspace"]["id"].as_str().unwrap_or("?"),
            status,
            value["session_id"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "component {} -> {} ({} commit(s))",
            value["previous_component_commit"].as_str().unwrap_or("?"),
            value["landed_commit"].as_str().unwrap_or("?"),
            value["commits"].as_array().map(Vec::len).unwrap_or(0)
        ));
        lines.push(format!(
            "branch {} · worktree {}",
            value["workspace"]["branch"].as_str().unwrap_or("?"),
            value["workspace"]["worktree_path"].as_str().unwrap_or("?")
        ));
        return Some(lines);
    }
    if value.get("workspaces").is_some_and(Value::is_array) {
        let workspaces = value["workspaces"].as_array()?;
        let sessions = value["sessions"].as_array();
        if workspaces.is_empty() && sessions.is_none_or(Vec::is_empty) {
            return Some(vec!["No worker workspaces.".to_string()]);
        }
        let mut lines = vec![format!(
            "{} worker workspace(s){}",
            workspaces.len(),
            sessions
                .map(|rows| format!(" · {} session binding(s)", rows.len()))
                .unwrap_or_default()
        )];
        lines.extend(workspaces.iter().map(workspace_line));
        if let Some(sessions) = sessions {
            lines.extend(sessions.iter().map(workspace_session_line));
        }
        return Some(lines);
    }
    if let Some(workspace) = value.get("workspace")
        && workspace.is_object()
    {
        let mut lines = vec![workspace_line(workspace)];
        if let Some(rows) = value.get("sessions").and_then(Value::as_array) {
            lines.extend(rows.iter().map(workspace_session_line));
        }
        if value.get("created").is_some() || value.get("attached").is_some() {
            lines.push(format!(
                "created {} · attached {}",
                value["created"].as_bool().unwrap_or(false),
                value["attached"].as_bool().unwrap_or(false)
            ));
        }
        if value.get("changed").is_some() {
            lines.push(format!(
                "changed {}",
                value["changed"].as_bool().unwrap_or(false)
            ));
        }
        if let Some(binding) = value.get("session")
            && binding.is_object()
        {
            lines.push(workspace_session_line(binding));
        }
        return Some(lines);
    }
    if value.get("project_id").is_some()
        && value.get("workspaces").is_some()
        && value.get("events").is_some()
    {
        return Some(vec![format!(
            "workspace replay: workspaces {} · sessions {} · events {} · {}",
            value["workspaces"].as_u64().unwrap_or(0),
            value["sessions"].as_u64().unwrap_or(0),
            value["events"].as_u64().unwrap_or(0),
            if value["consistent"].as_bool().unwrap_or(false) {
                "consistent"
            } else {
                "drift detected"
            }
        )]);
    }
    None
}

pub(crate) fn agent_employment_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(requests) = value.get("requests").and_then(Value::as_array) {
        if requests.is_empty() {
            return Some(vec!["No hire requests.".to_string()]);
        }
        let mut lines = vec![format!("{} hire requests", requests.len())];
        for request in requests {
            lines.push(format!(
                "{}  {}  {}  {}  parent {}",
                request["id"].as_str().unwrap_or("?"),
                request["role"].as_str().unwrap_or("?"),
                request["status"].as_str().unwrap_or("?"),
                request["title"]
                    .as_str()
                    .or_else(|| request["component"].as_str())
                    .unwrap_or("-"),
                request["parent_identity_id"].as_str().unwrap_or("-"),
            ));
        }
        return Some(lines);
    }
    if let Some(request) = value
        .get("request")
        .filter(|request| request.get("role").is_some())
    {
        return Some(vec![format!(
            "hire {} · {} · {}{}",
            request["id"].as_str().unwrap_or("?"),
            request["role"].as_str().unwrap_or("?"),
            value["state"].as_str().unwrap_or("pending"),
            value["identity"]["id"]
                .as_str()
                .map(|id| format!(" · identity {id}"))
                .unwrap_or_default(),
        )]);
    }
    if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
        if candidates.is_empty() {
            let state = if value["policy_enabled"].as_bool().unwrap_or(false) {
                "No retirement candidates crossed a configured threshold."
            } else {
                "Retirement policy is disabled; no candidates."
            };
            return Some(vec![state.to_string()]);
        }
        let mut lines = vec![format!("{} retirement candidates", candidates.len())];
        for candidate in candidates {
            let reasons = candidate["reasons"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|reason| {
                    format!(
                        "{} {}/{}",
                        reason["code"].as_str().unwrap_or("?"),
                        reason["count"].as_u64().unwrap_or(0),
                        reason["threshold"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "{}  {}  gen {}  severity {}  {}",
                candidate["identity_id"].as_str().unwrap_or("?"),
                candidate["role"].as_str().unwrap_or("?"),
                candidate["generation"].as_u64().unwrap_or(0),
                candidate["severity"].as_u64().unwrap_or(0),
                reasons
            ));
        }
        return Some(lines);
    }
    if let Some(proposals) = value.get("proposals").and_then(Value::as_array) {
        if proposals.is_empty() {
            return Some(vec!["No retirement proposals.".to_string()]);
        }
        let mut lines = vec![format!("{} retirement proposals", proposals.len())];
        for proposal in proposals {
            lines.push(format!(
                "{}  {}  {}  {}",
                proposal["id"].as_str().unwrap_or("?"),
                proposal["identity_id"].as_str().unwrap_or("?"),
                proposal["status"].as_str().unwrap_or("?"),
                proposal["reason_code"].as_str().unwrap_or("-")
            ));
        }
        return Some(lines);
    }
    if value.get("proposal").is_some() && value.get("reason_code").is_some() {
        return Some(vec![format!(
            "{}  {}  {}",
            value["proposal"]["id"].as_str().unwrap_or("?"),
            value["proposal"]["status"].as_str().unwrap_or("?"),
            value["changed"].as_bool().unwrap_or(false)
        )]);
    }
    if value.get("candidate").is_some() && value.get("proposal").is_some() {
        return Some(vec![format!(
            "proposal {} · {} · {}",
            value["proposal"]["id"].as_str().unwrap_or("?"),
            value["proposal"]["status"].as_str().unwrap_or("?"),
            if value["already_pending"].as_bool().unwrap_or(false) {
                "already pending"
            } else {
                "created"
            }
        )]);
    }
    if value.get("totals").is_some() && value.get("identities").is_some() {
        let totals = &value["totals"];
        let mut lines = vec![format!(
            "{} identities · completed {} · failed {} · verifier {}/{} · tokens {}/{} · ${:.4}",
            totals["identities"].as_u64().unwrap_or(0),
            totals["completed_tasks"].as_u64().unwrap_or(0),
            totals["failed_tasks"].as_u64().unwrap_or(0),
            totals["verifier_approvals"].as_u64().unwrap_or(0),
            totals["verifier_rejections"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        )];
        if value["identities"].as_array().is_some_and(Vec::is_empty) {
            lines.push("No performance rows.".to_string());
        }
        for row in value["identities"].as_array().into_iter().flatten() {
            lines.push(performance_line(row));
        }
        return Some(lines);
    }
    if value.get("completed_tasks").is_some() && value.get("identity_id").is_some() {
        return Some(vec![performance_line(value)]);
    }
    if value.get("previous_role").is_some() && value.get("identity").is_some() {
        return Some(vec![format!(
            "promoted {} → {} · gen {} · released {} assignment(s), archived {} workspace(s)",
            value["previous_role"].as_str().unwrap_or("?"),
            value["identity"]["role"].as_str().unwrap_or("?"),
            value["identity"]["generation"].as_u64().unwrap_or(0),
            value["released_assignments"].as_u64().unwrap_or(0),
            value["archived_workspaces"].as_u64().unwrap_or(0)
        )]);
    }
    if value.get("identity").is_some() && value.get("changed").is_some() {
        return Some(vec![format!(
            "{}  {}  changed {}  released {} assignment(s), archived {} workspace(s)",
            value["identity"]["id"].as_str().unwrap_or("?"),
            value["identity"]["status"].as_str().unwrap_or("?"),
            value["changed"].as_bool().unwrap_or(false),
            value["released_assignments"].as_u64().unwrap_or(0),
            value["archived_workspaces"].as_u64().unwrap_or(0)
        )]);
    }
    None
}

pub(crate) fn performance_line(row: &Value) -> String {
    format!(
        "{}  {}  {}  completed {} · failed {} · consecutive {} · retries {} · verifier {}/{} · evidence {}",
        row["identity_id"].as_str().unwrap_or("?"),
        row["role"].as_str().unwrap_or("?"),
        row["status"].as_str().unwrap_or("?"),
        row["completed_tasks"].as_u64().unwrap_or(0),
        row["failed_tasks"].as_u64().unwrap_or(0),
        row["consecutive_failures"].as_u64().unwrap_or(0),
        row["retries"].as_u64().unwrap_or(0),
        row["verifier_approvals"].as_u64().unwrap_or(0),
        row["verifier_rejections"].as_u64().unwrap_or(0),
        row["evidence"].as_array().map(Vec::len).unwrap_or(0)
    )
}
