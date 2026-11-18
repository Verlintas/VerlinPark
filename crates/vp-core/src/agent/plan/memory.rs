use super::*;

pub(in crate::agent) async fn record_task_memory(
    engine: &Engine,
    session_id: &str,
    task_id: &str,
    summary: &str,
) {
    let Ok((_session, project)) = load_session_project(engine.store().clone(), session_id).await
    else {
        return;
    };
    let title = format!("Task {task_id} result");
    let mut evidence = vec![
        vp_protocol::types::MemoryEvidence {
            kind: "task".to_string(),
            reference: task_id.to_string(),
            note: None,
        },
        vp_protocol::types::MemoryEvidence {
            kind: "session".to_string(),
            reference: session_id.to_string(),
            note: None,
        },
    ];
    let store = engine.store().clone();
    let lookup_session = session_id.to_string();
    let lookup_task = task_id.to_string();
    let merged = tokio::task::spawn_blocking(move || {
        store.latest_merged_entry_for_task(&lookup_session, &lookup_task)
    })
    .await
    .ok()
    .and_then(std::result::Result::ok)
    .flatten();
    if let Some(entry) = merged {
        evidence.push(vp_protocol::types::MemoryEvidence {
            kind: "commit".to_string(),
            reference: entry.commit.clone(),
            note: None,
        });
        if !entry.base_commit.trim().is_empty()
            && let Ok(paths) = crate::git_output(
                std::path::Path::new(&project.root),
                &[
                    "diff",
                    "--name-only",
                    &format!("{}..{}", entry.base_commit, entry.commit),
                ],
            )
        {
            for path in paths
                .lines()
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .take(20)
            {
                evidence.push(vp_protocol::types::MemoryEvidence {
                    kind: "path".to_string(),
                    reference: path.to_string(),
                    note: None,
                });
            }
        }
    }
    let Ok(policy) = engine.get_memory_policy(&project.id).await else {
        return;
    };
    let verify_after = crate::memory_verify_after(policy.policy.task_result_verify_days);
    if let Err(error) = engine
        .create_memory_with_metadata(
            session_id,
            "orchestrator",
            "Orchestrator",
            "task_result",
            &title,
            &truncate_to(summary, 8_000),
            vec![task_id.to_string(), "task_result".to_string()],
            "task_result",
            evidence,
            false,
            verify_after,
        )
        .await
    {
        tracing::warn!(%error, task_id, "failed to persist task result memory");
    }
}

/// Persist a capture memory with the project's default verification TTL.
/// Capture is best-effort: callers keep their normal gate result even when the
/// memory write fails.
#[allow(clippy::too_many_arguments)]
pub(super) async fn record_capture_memory(
    engine: &Engine,
    session_id: &str,
    agent_id: &str,
    agent_name: &str,
    kind: &str,
    title: &str,
    body: &str,
    tags: Vec<String>,
    source: &str,
    evidence: Vec<vp_protocol::types::MemoryEvidence>,
) {
    let Ok((_session, project)) = load_session_project(engine.store().clone(), session_id).await
    else {
        tracing::warn!(%session_id, kind, "failed to load project for memory capture");
        return;
    };
    let verify_after = match engine.get_memory_policy(&project.id).await {
        Ok(policy) => crate::memory_verify_after(policy.policy.default_verify_days.or(Some(365))),
        Err(error) => {
            tracing::warn!(%error, %session_id, kind, "memory policy lookup failed; using fallback TTL");
            crate::memory_verify_after(Some(365))
        }
    };
    if let Err(error) = engine
        .create_memory_with_metadata(
            session_id,
            agent_id,
            agent_name,
            kind,
            title,
            body,
            tags,
            source,
            evidence,
            false,
            verify_after,
        )
        .await
    {
        tracing::warn!(%error, %session_id, kind, "failed to persist capture memory");
    }
}

/// Capture a Verifier verdict as durable project knowledge. This is
/// best-effort and never changes the Worker/Verifier/merge result.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_verification_memory(
    engine: &Engine,
    session_id: &str,
    task_id: Option<&str>,
    worker_name: &str,
    commit: &str,
    verdict: &VerifierVerdict,
    integration: Option<&str>,
) {
    let task_id = task_id.map(str::trim).filter(|value| !value.is_empty());
    let title = match task_id {
        Some(task_id) => format!("Task {task_id} verification"),
        None => format!(
            "Worker {} verification {}",
            worker_name.trim(),
            commit.trim().chars().take(8).collect::<String>()
        ),
    };
    let mut body = format!(
        "approved: {}\nverdict: {}\n",
        verdict.approved,
        truncate_to(verdict.summary.trim(), 2_000)
    );
    if let Some(integration) = integration {
        body.push_str(&format!(
            "integration: {}\n",
            truncate_to(integration.trim(), 4_000)
        ));
    }
    body.push_str(&format!("commit: {}", commit.trim()));
    let mut tags = Vec::new();
    if let Some(task_id) = task_id {
        tags.push(task_id.to_string());
    }
    tags.push("verification".to_string());
    let mut evidence = Vec::new();
    if let Some(task_id) = task_id {
        evidence.push(vp_protocol::types::MemoryEvidence {
            kind: "task".to_string(),
            reference: task_id.to_string(),
            note: None,
        });
    }
    evidence.push(vp_protocol::types::MemoryEvidence {
        kind: "session".to_string(),
        reference: session_id.to_string(),
        note: None,
    });
    evidence.push(vp_protocol::types::MemoryEvidence {
        kind: "commit".to_string(),
        reference: commit.trim().to_string(),
        note: None,
    });
    record_capture_memory(
        engine,
        session_id,
        "verifier",
        "Verifier",
        "verification",
        &truncate_to(&title, 199),
        &body,
        tags,
        "verifier",
        evidence,
    )
    .await;
}

/// Capture a successfully re-verified and requeued conflict resolution.
/// Callers only invoke this after the conflict was resolved and requeued; the
/// outcome detail stays honest when the later integration attempt fails.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_integration_resolution_memory(
    engine: &Engine,
    session_id: &str,
    entry_id: &str,
    task_id: Option<&str>,
    commit: &str,
    verifier_summary: &str,
    conflict_detail: &str,
    outcome_detail: &str,
) {
    let task_id = task_id.map(str::trim).filter(|value| !value.is_empty());
    let title = format!("Integration {entry_id} resolution");
    let body = format!(
        "resolved: true\nverifier: {}\ncommit: {}\nconflict: {}\noutcome: {}",
        truncate_to(verifier_summary.trim(), 2_000),
        commit.trim(),
        truncate_to(conflict_detail.trim(), 2_000),
        truncate_to(outcome_detail.trim(), 2_000)
    );
    let mut tags = Vec::new();
    if let Some(task_id) = task_id {
        tags.push(task_id.to_string());
    }
    tags.push("integration".to_string());
    let mut evidence = Vec::new();
    if let Some(task_id) = task_id {
        evidence.push(vp_protocol::types::MemoryEvidence {
            kind: "task".to_string(),
            reference: task_id.to_string(),
            note: None,
        });
    }
    evidence.push(vp_protocol::types::MemoryEvidence {
        kind: "session".to_string(),
        reference: session_id.to_string(),
        note: None,
    });
    evidence.push(vp_protocol::types::MemoryEvidence {
        kind: "commit".to_string(),
        reference: commit.trim().to_string(),
        note: None,
    });
    record_capture_memory(
        engine,
        session_id,
        "orchestrator",
        "Orchestrator",
        "integration",
        &truncate_to(&title, 199),
        &body,
        tags,
        "orchestrator",
        evidence,
    )
    .await;
}
