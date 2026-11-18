//! Project configuration, budget, scaling, and scheduler commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Projects => rpc(&discovery, "project.list", json!({})).await?,
        Command::ProjectAdd { path, name } => {
            let root = path
                .canonicalize()
                .with_context(|| format!("resolving {}", path.display()))?;
            rpc(
                &discovery,
                "project.add",
                json!({"root": root, "name": name}),
            )
            .await?
        }
        Command::ProjectBudget { project } => {
            rpc(
                &discovery,
                "project.budget.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ProjectUsage { project } => {
            rpc(&discovery, "project.usage", json!({"project_id": project})).await?
        }
        Command::ProjectCache { project, limit } => {
            rpc(
                &discovery,
                "project.cache",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::ProjectBudgetSet {
            project,
            token_budget,
            cost_budget_usd,
        } => {
            rpc(
                &discovery,
                "project.budget.set",
                json!({
                    "project_id": project,
                    "budget": {
                        "token_budget": token_budget,
                        "cost_budget_usd": cost_budget_usd,
                    }
                }),
            )
            .await?
        }
        Command::Governor { session, limit } => {
            rpc(
                &discovery,
                "session.budget.governor",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::CriticalPath {
            session,
            remaining_tokens,
            mode,
        } => {
            let mode = parse_governor_mode(mode.as_deref())?;
            rpc(
                &discovery,
                "governor.preview",
                json!({
                    "session_id": session,
                    "remaining_tokens": remaining_tokens,
                    "limit": 1,
                    "mode": mode,
                }),
            )
            .await?
        }
        Command::GovernorPreview {
            session,
            remaining_tokens,
            limit,
            mode,
        } => {
            let mode = parse_governor_mode(mode.as_deref())?;
            rpc(
                &discovery,
                "governor.preview",
                json!({
                    "session_id": session,
                    "remaining_tokens": remaining_tokens,
                    "limit": limit,
                    "mode": mode,
                }),
            )
            .await?
        }
        Command::GovernorPolicy { project } => {
            rpc(
                &discovery,
                "critical_path.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::GovernorPolicySet {
            project,
            enabled,
            reserve_ratio,
            throttle_at_ratio,
            critical_boost,
            min_task_budget,
            mode,
            max_paths,
            department_min_ratio,
            observed_weight_ratio,
            starvation_after_secs,
        } => {
            let current = rpc(
                &discovery,
                "critical_path.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(value) = enabled {
                policy["enabled"] = json!(value);
            }
            if let Some(value) = reserve_ratio {
                policy["reserve_ratio"] = json!(value);
            }
            if let Some(value) = throttle_at_ratio {
                policy["throttle_at_ratio"] = json!(value);
            }
            if let Some(value) = critical_boost {
                policy["critical_boost"] = json!(value);
            }
            if let Some(value) = min_task_budget {
                policy["min_task_budget"] = json!(value);
            }
            if let Some(value) = mode {
                let parsed =
                    vp_protocol::types::CriticalPathMode::parse(&value).ok_or_else(|| {
                        anyhow::anyhow!("invalid --mode {value:?}; use `single` or `multi`")
                    })?;
                policy["mode"] = json!(parsed.as_str());
            }
            if let Some(value) = max_paths {
                policy["max_paths"] = json!(value);
            }
            if let Some(value) = department_min_ratio {
                policy["department_min_ratio"] = json!(value);
            }
            if let Some(value) = observed_weight_ratio {
                policy["observed_weight_ratio"] = json!(value);
            }
            if let Some(value) = starvation_after_secs {
                policy["starvation_after_secs"] = json!(value);
            }
            rpc(
                &discovery,
                "critical_path.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::CleanupWorktrees {
            project,
            older_than_days,
            dry_run,
            force,
            delete_branches,
            drop_failed,
        } => {
            rpc(
                &discovery,
                "project.cleanup_worktrees",
                json!({
                    "project_id": project,
                    "older_than_secs": older_than_days.saturating_mul(86_400),
                    "dry_run": dry_run,
                    "force": force,
                    "delete_branches": delete_branches,
                    "keep_failed": !drop_failed,
                }),
            )
            .await?
        }
        Command::TaskScheduler { project } => {
            rpc(
                &discovery,
                "project.task_scheduler.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::TaskSchedulerSet {
            project,
            enabled,
            cooldown_secs,
            dispatch_tasks,
            include_intake,
            max_intake_parallel,
        } => {
            // Merge with the stored policy so a one-field CLI edit does not
            // silently reset intake or per-task behavior to defaults.
            let current = rpc(
                &discovery,
                "project.task_scheduler.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            rpc(
                &discovery,
                "project.task_scheduler.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled
                            .unwrap_or_else(|| policy["enabled"].as_bool().unwrap_or(true)),
                        "cooldown_secs": cooldown_secs
                            .unwrap_or_else(|| policy["cooldown_secs"].as_u64().unwrap_or(60)),
                        "dispatch_tasks": dispatch_tasks
                            .unwrap_or_else(|| policy["dispatch_tasks"].as_bool().unwrap_or(false)),
                        "include_intake": include_intake
                            .unwrap_or_else(|| policy["include_intake"].as_bool().unwrap_or(true)),
                        "max_intake_parallel": max_intake_parallel
                            .unwrap_or_else(|| {
                                policy["max_intake_parallel"].as_u64().unwrap_or(2) as u32
                            }),
                    }
                }),
            )
            .await?
        }
        Command::Scaling {
            project,
            session,
            limit,
        } => {
            rpc(
                &discovery,
                "scaling.status",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ScalingPreview { session } => {
            rpc(
                &discovery,
                "scaling.preview",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ScalingPolicy { project } => {
            rpc(
                &discovery,
                "scaling.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ScalingPolicySet {
            project,
            enabled,
            min_workers,
            max_workers,
            min_labour,
            max_labour,
            expand_ready_threshold,
            expand_critical_delay_secs,
            scale_down_idle_secs,
            cooldown_secs,
            max_step,
            budget_headroom_ratio,
            no_budget_check,
        } => {
            // Merge with the stored policy so a one-field edit does not reset
            // the rest to defaults.
            let current = rpc(
                &discovery,
                "scaling.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            let budget_headroom_ratio = if no_budget_check {
                Value::Null
            } else if let Some(ratio) = budget_headroom_ratio {
                json!(ratio)
            } else {
                policy
                    .get("budget_headroom_ratio")
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            rpc(
                &discovery,
                "scaling.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled
                            .unwrap_or_else(|| policy["enabled"].as_bool().unwrap_or(false)),
                        "min_workers": min_workers
                            .unwrap_or_else(|| policy["min_workers"].as_u64().unwrap_or(1) as u32),
                        "max_workers": max_workers
                            .unwrap_or_else(|| policy["max_workers"].as_u64().unwrap_or(4) as u32),
                        "min_labour": min_labour
                            .unwrap_or_else(|| policy["min_labour"].as_u64().unwrap_or(1) as u32),
                        "max_labour": max_labour
                            .unwrap_or_else(|| policy["max_labour"].as_u64().unwrap_or(8) as u32),
                        "expand_ready_threshold": expand_ready_threshold.unwrap_or_else(|| {
                            policy["expand_ready_threshold"].as_u64().unwrap_or(2) as u32
                        }),
                        "expand_critical_delay_secs": expand_critical_delay_secs.unwrap_or_else(
                            || policy["expand_critical_delay_secs"].as_u64().unwrap_or(120),
                        ),
                        "scale_down_idle_secs": scale_down_idle_secs.unwrap_or_else(|| {
                            policy["scale_down_idle_secs"].as_u64().unwrap_or(600)
                        }),
                        "cooldown_secs": cooldown_secs.unwrap_or_else(|| {
                            policy["cooldown_secs"].as_u64().unwrap_or(120)
                        }),
                        "max_step": max_step
                            .unwrap_or_else(|| policy["max_step"].as_u64().unwrap_or(2) as u32),
                        "budget_headroom_ratio": budget_headroom_ratio,
                    }
                }),
            )
            .await?
        }
        Command::Dashboard { project } => {
            rpc(
                &discovery,
                "dashboard.snapshot",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ModelPresets => rpc(&discovery, "model.preset.list", json!({})).await?,
        Command::ModelPresetCreate {
            name,
            provider,
            model,
            context_window,
            input_price,
            output_price,
            notes,
        } => {
            rpc(
                &discovery,
                "model.preset.create",
                json!({
                    "name": name,
                    "provider": provider,
                    "model": model,
                    "context_window": context_window,
                    "input_usd_per_million": input_price,
                    "output_usd_per_million": output_price,
                    "notes": notes
                }),
            )
            .await?
        }
        Command::ModelPresetUpdate {
            id,
            name,
            provider,
            model,
            context_window,
            input_price,
            output_price,
            notes,
        } => {
            rpc(
                &discovery,
                "model.preset.update",
                json!({
                    "id": id,
                    "name": name,
                    "provider": provider,
                    "model": model,
                    "context_window": context_window,
                    "input_usd_per_million": input_price,
                    "output_usd_per_million": output_price,
                    "notes": notes
                }),
            )
            .await?
        }
        Command::ModelPresetDelete { id } => {
            rpc(&discovery, "model.preset.delete", json!({"id": id})).await?
        }
        Command::UpdateCheck { repo } => {
            rpc(&discovery, "update.check", json!({"repo": repo})).await?
        }
        Command::WorktreePolicy { project } => {
            rpc(
                &discovery,
                "project.worktree_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::WorktreePolicySet {
            project,
            enabled,
            older_than_days,
            delete_branches,
            drop_failed,
        } => {
            let policy = json!({
                "enabled": enabled,
                "older_than_secs": older_than_days.saturating_mul(86_400),
                "delete_branches": delete_branches,
                "keep_failed": !drop_failed
            });
            rpc(
                &discovery,
                "project.worktree_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
