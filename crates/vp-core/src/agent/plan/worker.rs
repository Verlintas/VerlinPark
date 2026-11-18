use super::*;

#[allow(clippy::too_many_arguments)]
pub(in crate::agent) async fn finish_worker(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    manager_spec: &AgentSpec,
    worker_spec: &AgentSpec,
    objective: &str,
    summary: &str,
    acceptance: Option<&str>,
) -> Result<WorkerFinish> {
    let commit = match commit_worker(worker_spec, &worker_spec.name).await {
        Ok(commit) => commit,
        Err(error) => {
            let summary = format!("integration failed: {error}");
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&summary),
                    "merged": false,
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            return Ok(WorkerFinish::Done {
                report: summary,
                success: false,
            });
        }
    };
    let Some(commit) = commit else {
        engine.update_agent_status(&worker_spec.id, "idle").await?;
        emit(
            engine,
            &session.id,
            worker_spec,
            "agent.finished",
            serde_json::json!({
                "status": "idle",
                "summary": truncate(summary),
                "merged": false,
                "verified": false,
                "worktree": worker_spec.worktree,
                "branch": worker_spec.branch,
            }),
        )
        .await?;
        return Ok(WorkerFinish::Done {
            report: format!("no changes to merge\n{}", truncate(summary)),
            success: !worker_report_reports_failure(summary),
        });
    };

    record_worker_commit_effect(engine, session, worker_spec, &commit).await;
    let changed_paths = bounded_worker_changed_paths(worker_spec, &commit);
    let verification = verify_worker(
        engine,
        config,
        session,
        project,
        control,
        budget,
        manager_spec,
        worker_spec,
        &commit,
        objective,
        summary,
        acceptance,
        false,
        &VerifierProvenance::default(),
    )
    .await?;
    match verification {
        VerificationOutcome::Approved(verdict) => {
            match integrate_worker(
                engine,
                session,
                worker_spec,
                &commit,
                &verdict.summary,
                config.integration_command.as_deref(),
            )
            .await
            {
                Ok(detail) => {
                    record_verification_memory(
                        engine,
                        &session.id,
                        worker_spec.task_id.as_deref(),
                        &worker_spec.name,
                        &commit,
                        &verdict,
                        Some(&detail),
                    )
                    .await;
                    engine.update_agent_status(&worker_spec.id, "idle").await?;
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "agent.finished",
                        serde_json::json!({
                            "status": "idle",
                            "summary": truncate(summary),
                            "commit": commit,
                            "changed_paths": changed_paths,
                            "merged": true,
                            "verified": true,
                            "approved": true,
                            "integrator": true,
                            "integration": truncate(&detail),
                            "verdict": truncate(&verdict.summary),
                            "worktree": worker_spec.worktree,
                            "branch": worker_spec.branch,
                        }),
                    )
                    .await?;
                    Ok(WorkerFinish::Done {
                        report: format!(
                            "merged: true\nverified: true\nintegrator: {}\n{}\nverdict: {}",
                            truncate(&detail),
                            truncate(summary),
                            truncate(&verdict.summary)
                        ),
                        success: true,
                    })
                }
                Err(error) => {
                    engine
                        .update_agent_status(&worker_spec.id, "failed")
                        .await?;
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "agent.finished",
                        serde_json::json!({
                            "status": "failed",
                            "summary": truncate(error.to_string().as_str()),
                            "commit": commit,
                            "changed_paths": changed_paths,
                            "merged": false,
                            "verified": true,
                            "approved": true,
                            "integrator": true,
                            "verdict": truncate(&verdict.summary),
                            "worktree": worker_spec.worktree,
                            "branch": worker_spec.branch,
                        }),
                    )
                    .await?;
                    let memory_integration = format!("integration failed: {error:#}");
                    record_verification_memory(
                        engine,
                        &session.id,
                        worker_spec.task_id.as_deref(),
                        &worker_spec.name,
                        &commit,
                        &verdict,
                        Some(&memory_integration),
                    )
                    .await;
                    Ok(WorkerFinish::Done {
                        report: format!(
                            "merge/integration failed: {error}\nverdict: {}",
                            truncate(&verdict.summary)
                        ),
                        success: false,
                    })
                }
            }
        }
        VerificationOutcome::Rejected(verdict) => {
            engine
                .update_agent_status(&worker_spec.id, "changes_requested")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "changes_requested",
                    "summary": truncate(summary),
                    "commit": commit,
                    "changed_paths": changed_paths,
                    "merged": false,
                    "verified": true,
                    "approved": false,
                    "verdict": truncate(&verdict.summary),
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            record_verification_memory(
                engine,
                &session.id,
                worker_spec.task_id.as_deref(),
                &worker_spec.name,
                &commit,
                &verdict,
                None,
            )
            .await;
            Ok(WorkerFinish::Done {
                report: format!(
                    "merged: false\nverification rejected:\n{}",
                    truncate(&verdict.summary)
                ),
                success: false,
            })
        }
        VerificationOutcome::Error(error) => {
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&error),
                    "commit": commit,
                    "changed_paths": changed_paths,
                    "merged": false,
                    "verified": false,
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!("verification error: {error}"),
                success: false,
            })
        }
        VerificationOutcome::Cancelled => {
            engine
                .update_agent_status(&worker_spec.id, "cancelled")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "cancelled",
                    "commit": commit,
                    "merged": false,
                }),
            )
            .await?;
            Ok(WorkerFinish::Cancelled)
        }
        VerificationOutcome::BudgetExceeded(stop) => {
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": format!("{} during verification", stop.summary()),
                    "commit": commit,
                    "merged": false,
                }),
            )
            .await?;
            if stop.is_abortive() {
                Ok(WorkerFinish::BudgetExceeded(stop))
            } else {
                Ok(WorkerFinish::Done {
                    report: format!("verification stopped: {}", stop.summary()),
                    success: false,
                })
            }
        }
    }
}

/// Finish one workspace-bound Labour run. The Labour commit is merged into the
/// parent Worker session workspace with scope/diff checks and an exact
/// rollback. This is internal Worker integration, not a Verifier/Integrator
/// bypass: the resulting Worker commit still enters the normal Verifier ->
/// merge queue -> deterministic Integrator path.
pub(in crate::agent) async fn finish_labour(
    engine: &Engine,
    session: &Session,
    parent_spec: &AgentSpec,
    labour_spec: &AgentSpec,
    summary: &str,
) -> Result<WorkerFinish> {
    let commit = match commit_worker(labour_spec, &labour_spec.name).await {
        Ok(commit) => commit,
        Err(error) => {
            let detail = format!("Labour commit failed: {error}");
            engine
                .update_agent_status(&labour_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                labour_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&detail),
                    "merged": false,
                    "worktree": labour_spec.worktree,
                    "branch": labour_spec.branch,
                    "parent_workspace_id": labour_spec.parent_workspace_id,
                }),
            )
            .await?;
            return Ok(WorkerFinish::Done {
                report: detail,
                success: false,
            });
        }
    };
    let Some(commit) = commit else {
        engine.update_agent_status(&labour_spec.id, "idle").await?;
        emit(
            engine,
            &session.id,
            labour_spec,
            "agent.finished",
            serde_json::json!({
                "status": "idle",
                "summary": truncate(summary),
                "merged": false,
                "worktree": labour_spec.worktree,
                "branch": labour_spec.branch,
                "parent_workspace_id": labour_spec.parent_workspace_id,
            }),
        )
        .await?;
        return Ok(WorkerFinish::Done {
            report: format!("no changes to merge\n{}", truncate(summary)),
            success: !worker_report_reports_failure(summary),
        });
    };
    record_worker_commit_effect(engine, session, labour_spec, &commit).await;
    let workspace_id = parent_spec
        .workspace_id
        .as_deref()
        .ok_or_else(|| anyhow!("Labour workspace binding has no parent project workspace"))?;
    let outcome = engine
        .merge_labour_commit(
            workspace_id,
            &session.id,
            &labour_spec.id,
            labour_spec.task_id.as_deref(),
            &commit,
            &labour_spec.write_scope,
        )
        .await;
    match outcome {
        Ok(outcome) if outcome.merged => {
            engine.update_agent_status(&labour_spec.id, "idle").await?;
            emit(
                engine,
                &session.id,
                labour_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "idle",
                    "summary": truncate(summary),
                    "commit": commit,
                    "merged": true,
                    "merged_into": "worker_workspace",
                    "pre_merge": outcome.pre_merge,
                    "post_merge": outcome.post_merge,
                    "paths": outcome.paths,
                    "worktree": labour_spec.worktree,
                    "branch": labour_spec.branch,
                    "parent_workspace_id": labour_spec.parent_workspace_id,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!(
                    "merged into Worker workspace: true\ncommit: {}\nworker commit: {}\npaths: {}\n{}",
                    commit,
                    outcome.post_merge,
                    truncate(&outcome.paths.join(", ")),
                    truncate(summary)
                ),
                success: true,
            })
        }
        Ok(outcome) => {
            let detail = truncate(&outcome.detail);
            engine
                .update_agent_status(&labour_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                labour_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&detail),
                    "commit": commit,
                    "merged": false,
                    "merged_into": "worker_workspace",
                    "pre_merge": outcome.pre_merge,
                    "post_merge": outcome.post_merge,
                    "conflicts": outcome.conflicts,
                    "paths": outcome.paths,
                    "worktree": labour_spec.worktree,
                    "branch": labour_spec.branch,
                    "parent_workspace_id": labour_spec.parent_workspace_id,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!(
                    "internal Worker integration failed; Worker HEAD is unchanged at {}\n{}\n{}",
                    outcome.pre_merge,
                    detail,
                    truncate(summary)
                ),
                success: false,
            })
        }
        Err(error) => {
            let detail = truncate(&format!("{error:#}"));
            engine
                .update_agent_status(&labour_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                labour_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": detail,
                    "commit": commit,
                    "merged": false,
                    "merged_into": "worker_workspace",
                    "worktree": labour_spec.worktree,
                    "branch": labour_spec.branch,
                    "parent_workspace_id": labour_spec.parent_workspace_id,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!("internal Worker integration failed: {detail}"),
                success: false,
            })
        }
    }
}

pub(in crate::agent) fn parse_verdict(input: &serde_json::Value) -> Result<VerifierVerdict> {
    let approved = input
        .get("approved")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| anyhow!("approved is required"))?;
    let summary = input
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty() && summary.len() <= 2000)
        .ok_or_else(|| anyhow!("summary is required (max 2000 chars)"))?
        .to_string();
    Ok(VerifierVerdict { approved, summary })
}
