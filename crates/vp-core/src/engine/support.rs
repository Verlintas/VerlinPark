use super::*;

pub(crate) fn memory_verify_after(days: Option<u32>) -> Option<String> {
    let days = days?;
    let when = time::OffsetDateTime::now_utc() + time::Duration::days(i64::from(days));
    Some(
        when.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| now_rfc3339()),
    )
}

pub(crate) fn verify_memory_evidence(
    store: &Store,
    project_root: &Path,
    project_id: &str,
    memory_session_id: &str,
    evidence: &[MemoryEvidence],
) -> Vec<MemoryVerificationCheck> {
    evidence
        .iter()
        .map(|item| {
            let (status, detail) = match item.kind.as_str() {
                "commit" => verify_commit_evidence(project_root, &item.reference),
                "path" => verify_path_evidence(project_root, &item.reference),
                "task" => match store.plan_task_exists(project_id, item.reference.trim()) {
                    Ok(true) => (
                        "verified".to_string(),
                        "task exists in this project".to_string(),
                    ),
                    Ok(false) => (
                        "missing".to_string(),
                        "task not found in this project".to_string(),
                    ),
                    Err(error) => (
                        "unchecked".to_string(),
                        format!("task lookup failed: {error}"),
                    ),
                },
                "session" => match store.get_session(item.reference.trim()) {
                    Ok(Some(_)) => ("verified".to_string(), "session exists".to_string()),
                    Ok(None) => ("missing".to_string(), "session not found".to_string()),
                    Err(error) => (
                        "unchecked".to_string(),
                        format!("session lookup failed: {error}"),
                    ),
                },
                "event" => match parse_event_reference(&item.reference, memory_session_id) {
                    Some((session_id, seq)) => match store.event_exists(&session_id, seq) {
                        Ok(true) => (
                            "verified".to_string(),
                            format!("event {session_id}:{seq} exists"),
                        ),
                        Ok(false) => (
                            "missing".to_string(),
                            format!("event {session_id}:{seq} not found"),
                        ),
                        Err(error) => (
                            "unchecked".to_string(),
                            format!("event lookup failed: {error}"),
                        ),
                    },
                    None => (
                        "unchecked".to_string(),
                        "event reference is not '<session>:<seq>' or '<seq>'".to_string(),
                    ),
                },
                "url" => (
                    "unchecked".to_string(),
                    "URL evidence is not fetched by local verification".to_string(),
                ),
                other => (
                    "unchecked".to_string(),
                    format!("unsupported evidence kind: {other}"),
                ),
            };
            MemoryVerificationCheck {
                evidence: item.clone(),
                status,
                detail,
            }
        })
        .collect()
}

pub(crate) fn verify_commit_evidence(project_root: &Path, reference: &str) -> (String, String) {
    let reference = reference.trim();
    if reference.is_empty() {
        return ("missing".to_string(), "empty commit reference".to_string());
    }
    let repo = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--git-dir"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match repo {
        Ok(output) if output.status.success() => {}
        Ok(_) | Err(_) => {
            return (
                "unchecked".to_string(),
                "project root is not a git repository".to_string(),
            );
        }
    }
    let object = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["cat-file", "-e", &format!("{reference}^{{commit}}")])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match object {
        Ok(output) if output.status.success() => {}
        Ok(_) => {
            return (
                "missing".to_string(),
                format!("commit object {reference} not found"),
            );
        }
        Err(error) => {
            return (
                "missing".to_string(),
                format!("git cat-file failed: {error}"),
            );
        }
    }
    let ancestor = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["merge-base", "--is-ancestor", reference, "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match ancestor {
        Ok(output) if output.status.success() => (
            "verified".to_string(),
            format!("{reference} is an ancestor of HEAD"),
        ),
        Ok(output) if output.status.code() == Some(1) => (
            "missing".to_string(),
            format!("{reference} is not an ancestor of HEAD"),
        ),
        Ok(output) => (
            "missing".to_string(),
            format!(
                "git merge-base failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ),
        Err(error) => (
            "missing".to_string(),
            format!("git merge-base failed: {error}"),
        ),
    }
}

pub(crate) fn verify_path_evidence(project_root: &Path, reference: &str) -> (String, String) {
    let reference = reference.trim();
    if reference.is_empty() {
        return ("missing".to_string(), "empty path reference".to_string());
    }
    let path = Path::new(reference);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return (
            "missing".to_string(),
            "path escapes the project root".to_string(),
        );
    }
    let candidate = project_root.join(path);
    if !candidate.exists() {
        return (
            "missing".to_string(),
            "path not found under the project root".to_string(),
        );
    }
    if let (Ok(root), Ok(resolved)) = (project_root.canonicalize(), candidate.canonicalize())
        && !resolved.starts_with(&root)
    {
        return (
            "missing".to_string(),
            "path resolves outside the project root".to_string(),
        );
    }
    (
        "verified".to_string(),
        format!("exists at {}", candidate.display()),
    )
}

pub(crate) fn parse_event_reference(
    reference: &str,
    fallback_session_id: &str,
) -> Option<(String, u64)> {
    let reference = reference.trim();
    if let Some((session_id, seq)) = reference.split_once(':') {
        let seq = seq.trim().parse::<u64>().ok()?;
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return None;
        }
        return Some((session_id.to_string(), seq));
    }
    reference
        .parse::<u64>()
        .ok()
        .map(|seq| (fallback_session_id.to_string(), seq))
}

/// A failed push is `unknown` only when the transport may have reached the
/// remote. A local rejection (non-fast-forward, missing remote) is `failed`.
pub(crate) fn push_error_is_ambiguous(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("timed out")
        || message.contains("timeout")
        || message.contains("connection reset")
        || message.contains("connection refused")
        || message.contains("could not resolve")
        || message.contains("unable to access")
        || message.contains("ssl")
        || message.contains("tls")
        || message.contains("remote end hung up")
}

/// A forge create error is `unknown` when the provider may have created the
/// change despite the local failure. This is exactly the double-PR case the
/// receipt prevents; the operator reconciles rather than retries blindly.
pub(crate) fn forge_error_is_ambiguous(error: &crate::forge::ForgeError) -> bool {
    matches!(
        error.code.as_str(),
        "FORGE_TIMEOUT"
            | "FORGE_SERVICE_UNAVAILABLE"
            | "FORGE_NETWORK_FAILED"
            | "FORGE_CONFLICT"
            | "FORGE_RESPONSE_INVALID"
    )
}

pub(crate) fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("running git")?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

pub(crate) fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub(crate) fn parse_numstat(numstat: &str) -> Vec<SessionCompareFile> {
    let mut files = Vec::new();
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let additions = parts.next().unwrap_or("-");
        let deletions = parts.next().unwrap_or("-");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            continue;
        }
        let binary = additions == "-" || deletions == "-";
        files.push(SessionCompareFile {
            path: path.to_string(),
            additions: additions.parse().unwrap_or(0),
            deletions: deletions.parse().unwrap_or(0),
            binary,
        });
    }
    files
}

pub(crate) fn clip_transcript(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        text.chars().take(limit).collect::<String>() + "\n… truncated"
    }
}

pub(crate) fn validate_acp_agent_config(config: AcpAgentConfig) -> Result<AcpAgentConfig> {
    acp::validate_agent_config(config)
}

pub(crate) fn validate_model_preset(
    name: &str,
    provider: &str,
    model: &str,
    context_window: Option<u64>,
    input_price: Option<f64>,
    output_price: Option<f64>,
    notes: &str,
) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        anyhow::bail!("model preset name must be 1-100 characters");
    }
    let provider = provider.trim();
    if !matches!(provider, "openai-compatible" | "anthropic") {
        anyhow::bail!("model preset provider must be openai-compatible or anthropic");
    }
    let model = model.trim();
    if model.is_empty() || model.chars().count() > 200 {
        anyhow::bail!("model preset model must be 1-200 characters");
    }
    if context_window.is_some_and(|window| window > 10_000_000) {
        anyhow::bail!("model preset context window must be at most 10,000,000");
    }
    for price in [input_price, output_price].into_iter().flatten() {
        if !price.is_finite() || price < 0.0 || price > 100_000.0 {
            anyhow::bail!("model preset prices must be finite and between 0 and 100,000");
        }
    }
    if notes.chars().count() > 2_000 {
        anyhow::bail!("model preset notes must be at most 2,000 characters");
    }
    Ok(())
}

pub(crate) async fn ensure_session_not_active(
    store: &Store,
    session_id: &str,
    action: &str,
) -> Result<()> {
    let store = store.clone();
    let id = session_id.to_string();
    let session = tokio::task::spawn_blocking(move || store.get_session(&id))
        .await??
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
    if matches!(
        session.status.as_str(),
        "queued" | "running" | "paused" | "waiting_input"
    ) {
        anyhow::bail!(
            "session must be idle to {action}; current status is {}",
            session.status
        );
    }
    Ok(())
}

/// Cost is recorded twice when a usage entry has a price: `agent.usage`
/// carries the per-call cost and `agent.cost` carries the running session total.
/// Prefer the latest total so both `record_agent_usage` and the legacy
/// `record_cost` path replay identically; fall back to summing older usage
/// events that predate `agent.cost`.
/// Fold conflict lifecycle events last-write-wins by conflict id. A deleted
/// memory also tombstones conflicts that reference it, matching the projection
/// delete in `Store::delete_memories_with_events`.
pub(crate) fn replay_memory_conflicts(events: &[Event]) -> Vec<MemoryConflict> {
    let mut conflicts: HashMap<String, MemoryConflict> = HashMap::new();
    for event in events {
        match event.kind.as_str() {
            "memory.conflict.detected" | "memory.conflict.resolved" => {
                if let Ok(conflict) =
                    serde_json::from_value::<MemoryConflict>(event.payload.clone())
                {
                    conflicts.insert(conflict.id.clone(), conflict);
                }
            }
            "memory.deleted" => {
                if let Ok(memory) = serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                    conflicts.retain(|_, conflict| {
                        conflict.memory_a_id != memory.id && conflict.memory_b_id != memory.id
                    });
                }
            }
            _ => {}
        }
    }
    let mut conflicts = conflicts.into_values().collect::<Vec<_>>();
    conflicts.sort_by(|left, right| {
        left.scope_kind
            .cmp(&right.scope_kind)
            .then_with(|| left.scope_value.cmp(&right.scope_value))
            .then_with(|| left.memory_a_id.cmp(&right.memory_a_id))
            .then_with(|| left.memory_b_id.cmp(&right.memory_b_id))
            .then_with(|| right.detected_at.cmp(&left.detected_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    conflicts
}

pub(crate) fn replay_session_prompt_layout(events: &[Event]) -> Option<u32> {
    events
        .iter()
        .rev()
        .find_map(|event| match event.kind.as_str() {
            "session.prompt_layout" => event
                .payload
                .get("prompt_layout")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as u32)
                .filter(|value| matches!(value, 2 | 3)),
            "session.created" => event
                .payload
                .get("prompt_layout")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as u32)
                .filter(|value| matches!(value, 2 | 3)),
            _ => None,
        })
}

pub(crate) fn replay_session_unlimited(events: &[Event]) -> bool {
    events
        .iter()
        .rev()
        .find_map(|event| match event.kind.as_str() {
            "session.budget_mode_changed" => event
                .payload
                .get("unlimited")
                .and_then(serde_json::Value::as_bool),
            "session.created" => event
                .payload
                .get("unlimited")
                .and_then(serde_json::Value::as_bool),
            _ => None,
        })
        .unwrap_or(false)
}

/// Stall observability threshold. `VERLINPARK_TASK_STALL_SECS=0` disables the
/// sweep; malformed or absent values fall back to 30 minutes.
pub fn task_stall_threshold_secs() -> u64 {
    parse_task_stall_threshold(std::env::var("VERLINPARK_TASK_STALL_SECS").ok().as_deref())
}

pub fn parse_task_stall_threshold(value: Option<&str>) -> u64 {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(1_800)
}

pub(crate) fn replay_steering(events: &[Event]) -> Vec<SteeringMessage> {
    let mut latest = HashMap::new();
    for event in events {
        if !matches!(
            event.kind.as_str(),
            "steering.created" | "steering.delivered" | "steering.cancelled"
        ) {
            continue;
        }
        if let Ok(message) = serde_json::from_value::<SteeringMessage>(event.payload.clone()) {
            latest.insert(message.id.clone(), message);
        }
    }
    let mut messages = latest.into_values().collect::<Vec<_>>();
    messages.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    messages
}

pub(crate) fn replay_continuations(events: &[Event]) -> Vec<SessionContinuation> {
    let mut latest = HashMap::new();
    for event in events {
        if !matches!(
            event.kind.as_str(),
            "session.continuation.created"
                | "session.continuation.ready"
                | "session.continuation.delivering"
                | "session.continuation.reclaimed"
                | "session.continuation.retrying"
                | "session.continuation.delivered"
                | "session.continuation.scheduled"
                | "session.continuation.cancelled"
                | "session.continuation.failed"
        ) {
            continue;
        }
        if let Ok(continuation) =
            serde_json::from_value::<SessionContinuation>(event.payload.clone())
        {
            latest.insert(continuation.id.clone(), continuation);
        }
    }
    let mut continuations = latest.into_values().collect::<Vec<_>>();
    continuations.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    continuations
}

pub(crate) fn recovery_key(recovery: &OrchestrationRecovery) -> (String, u64, String) {
    (
        recovery.session_id.clone(),
        recovery.plan_seq,
        recovery.task_id.clone(),
    )
}

pub(crate) fn replay_session_cost(events: &[Event]) -> f64 {
    if let Some(total) = events.iter().rev().find_map(|event| {
        if event.kind != "agent.cost" {
            return None;
        }
        event
            .payload
            .get("total_usd")
            .and_then(serde_json::Value::as_f64)
    }) {
        return total;
    }
    events
        .iter()
        .filter(|event| event.kind == "agent.usage")
        .map(|event| {
            event
                .payload
                .get("cost_usd")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
        })
        .sum()
}

pub(crate) fn first_line(value: &str, limit: usize) -> String {
    let line = value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= limit {
        line.to_string()
    } else {
        let mut truncated = line
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        truncated
    }
}

pub(crate) fn tool_activity_entry(event: Event) -> ToolActivityEntry {
    let payload = &event.payload;
    let is_error = payload
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let structured = payload.get("structured");
    let error_code = payload
        .get("error_code")
        .and_then(serde_json::Value::as_str)
        .map(|code| {
            crate::tooling::errors::ToolErrorCode::from_code_or_failed(code)
                .as_str()
                .to_string()
        })
        .or_else(|| {
            if !is_error {
                return None;
            }
            structured
                .and_then(|value| value.get("code"))
                .and_then(serde_json::Value::as_str)
                .map(|code| {
                    crate::tooling::errors::ToolErrorCode::from_code_or_failed(code)
                        .as_str()
                        .to_string()
                })
                .or_else(|| Some("TOOL_FAILED".to_string()))
        });
    let hint = payload
        .get("error_hint")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            structured
                .and_then(|value| value.get("hint"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            error_code.as_deref().map(|code| {
                crate::tooling::errors::ToolErrorCode::from_code_or_failed(code)
                    .default_hint()
                    .to_string()
            })
        })
        .map(|hint| hint.chars().take(600).collect());
    let retryable = payload
        .get("retryable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let duration_ms = payload
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(86_400_000);
    let output_bytes = payload
        .get("output_bytes")
        .or_else(|| payload.get("total_bytes"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    ToolActivityEntry {
        tool_call_id: payload
            .get("tool_call_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
        name: payload
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .take(120)
            .collect(),
        seq: event.seq,
        is_error,
        error_code,
        retryable,
        hint,
        duration_ms,
        output_bytes,
        truncated: payload
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        approval_risk: payload
            .get("approval_risk")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        approval_mode: payload
            .get("approval_mode")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        approval_wait_ms: payload
            .get("approval_wait_ms")
            .and_then(serde_json::Value::as_u64)
            .map(|wait| wait.min(86_400_000)),
        created_at: event.created_at,
    }
}

pub(crate) fn tool_activity_summary(entries: &[ToolActivityEntry]) -> ToolActivitySummary {
    let calls = entries.len() as u64;
    let errors = entries.iter().filter(|entry| entry.is_error).count() as u64;
    let retryable_errors = entries
        .iter()
        .filter(|entry| entry.is_error && entry.retryable)
        .count() as u64;
    let total_duration_ms = entries
        .iter()
        .fold(0u64, |total, entry| total.saturating_add(entry.duration_ms));
    let max_duration_ms = entries
        .iter()
        .map(|entry| entry.duration_ms)
        .max()
        .unwrap_or(0);
    let output_bytes = entries.iter().fold(0u64, |total, entry| {
        total.saturating_add(entry.output_bytes)
    });
    let truncated_results = entries.iter().filter(|entry| entry.truncated).count() as u64;
    let average_duration_ms = total_duration_ms.checked_div(calls).unwrap_or(0);
    let mut by_tool: std::collections::BTreeMap<String, ToolActivityToolStat> =
        std::collections::BTreeMap::new();
    let mut by_code: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut approval_waits = 0u64;
    let mut total_approval_wait_ms = 0u64;
    let mut max_approval_wait_ms = 0u64;
    for entry in entries {
        let stat = by_tool
            .entry(entry.name.clone())
            .or_insert_with(|| ToolActivityToolStat {
                name: entry.name.clone(),
                ..ToolActivityToolStat::default()
            });
        stat.calls = stat.calls.saturating_add(1);
        stat.errors = stat.errors.saturating_add(u64::from(entry.is_error));
        stat.total_duration_ms = stat.total_duration_ms.saturating_add(entry.duration_ms);
        stat.output_bytes = stat.output_bytes.saturating_add(entry.output_bytes);
        if let Some(code) = &entry.error_code {
            *by_code.entry(code.clone()).or_default() = by_code
                .get(code)
                .copied()
                .unwrap_or_default()
                .saturating_add(1);
        }
        if let Some(wait) = entry.approval_wait_ms {
            approval_waits = approval_waits.saturating_add(1);
            total_approval_wait_ms = total_approval_wait_ms.saturating_add(wait);
            max_approval_wait_ms = max_approval_wait_ms.max(wait);
        }
    }
    let mut by_tool = by_tool.into_values().collect::<Vec<_>>();
    by_tool.sort_by(|left, right| {
        right
            .calls
            .cmp(&left.calls)
            .then_with(|| left.name.cmp(&right.name))
    });
    by_tool.truncate(12);
    let mut error_codes = by_code
        .into_iter()
        .map(|(code, calls)| ToolActivityCodeStat { code, calls })
        .collect::<Vec<_>>();
    error_codes.sort_by(|left, right| {
        right
            .calls
            .cmp(&left.calls)
            .then_with(|| left.code.cmp(&right.code))
    });
    error_codes.truncate(8);
    ToolActivitySummary {
        calls,
        errors,
        retryable_errors,
        truncated_results,
        total_duration_ms,
        max_duration_ms,
        average_duration_ms,
        output_bytes,
        approval_waits,
        total_approval_wait_ms,
        max_approval_wait_ms,
        by_tool,
        error_codes,
    }
}
