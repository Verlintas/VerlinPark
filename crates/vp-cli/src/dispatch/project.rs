//! Project configuration, budget, scaling, and scheduler commands.

use super::*;

const AUTO_BUDGET_POLICY_FIELDS: &[&str] = &[
    "enabled",
    "initial_token_budget",
    "initial_cost_budget_usd",
    "initial_wall_clock_secs",
    "hard_token_cap",
    "hard_cost_cap_usd",
    "hard_wall_clock_secs",
    "extend_ratio",
    "max_multiplier",
    "forecast_calls",
    "min_runway_turns",
    "warn_at_ratio",
    "pause_at_ratio",
    "max_turns_per_run",
    "max_total_turns",
    "no_progress_turns",
    "no_progress_action",
    "auto_tool_bundle",
];

fn typed_local_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    crate::rpc::CliRpcError {
        kind: "local",
        code: None,
        typed_code: Some(code.to_string()),
        message: message.into(),
        hint: None,
        data: None,
    }
    .into()
}

/// Parse a full policy JSON object. Unknown keys are rejected locally so a
/// misspelled cap cannot be silently ignored (which would preserve the old cap
/// and look like a successful widening or narrowing).
pub(super) fn parse_auto_budget_policy_json(text: &str) -> Result<Value> {
    let parsed: Value =
        serde_json::from_str(text).context("--policy-file must contain one JSON policy object")?;
    let object = parsed
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("--policy-file must contain a JSON object"))?;
    for key in object.keys() {
        if !AUTO_BUDGET_POLICY_FIELDS.contains(&key.as_str()) {
            return Err(typed_local_error(
                vp_protocol::types::auto_budget_codes::INVALID_AUTO_BUDGET_POLICY,
                format!("unknown auto-budget policy field `{key}`"),
            ));
        }
    }
    Ok(parsed)
}

/// Overlay a policy file and explicit flags onto the stored policy. Fields not
/// named by either source keep their current value, so a one-field edit cannot
/// silently clear a hard cap.
pub(super) fn merge_auto_budget_policy(
    mut policy: Value,
    file: Option<&Value>,
    updates: &[(&'static str, Value)],
) -> Value {
    if !policy.is_object() {
        policy = json!({});
    }
    if let Some(file) = file.and_then(Value::as_object) {
        for (key, value) in file {
            policy[key] = value.clone();
        }
    }
    for (key, value) in updates {
        policy[*key] = value.clone();
    }
    policy
}

fn read_policy_file(path: &str) -> Result<String> {
    if path == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .context("reading the auto-budget policy from stdin")?;
        return Ok(text);
    }
    std::fs::read_to_string(path)
        .with_context(|| format!("reading the auto-budget policy file {path}"))
}

fn policy_update(
    updates: &mut Vec<(&'static str, Value)>,
    key: &'static str,
    value: Option<Value>,
) {
    if let Some(value) = value {
        updates.push((key, value));
    }
}

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
        Command::ProjectAutoBudget { project, .. } => {
            rpc(
                &discovery,
                "project.auto_budget.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ProjectAutoBudgetSet {
            project,
            enabled,
            initial_token_budget,
            initial_cost_budget_usd,
            initial_wall_clock_secs,
            hard_token_cap,
            hard_cost_cap_usd,
            hard_wall_clock_secs,
            extend_ratio,
            max_multiplier,
            forecast_calls,
            min_runway_turns,
            warn_at_ratio,
            pause_at_ratio,
            max_turns_per_run,
            max_total_turns,
            no_progress_turns,
            no_progress_action,
            auto_tool_bundle,
            clear_initial_token_budget,
            clear_initial_cost_budget_usd,
            clear_initial_wall_clock_secs,
            clear_hard_token_cap,
            clear_hard_cost_cap_usd,
            clear_hard_wall_clock_secs,
            clear_max_total_turns,
            policy_file,
            ..
        } => {
            let mut updates: Vec<(&'static str, Value)> = Vec::new();
            policy_update(&mut updates, "enabled", enabled.map(Value::from));
            policy_update(
                &mut updates,
                "initial_token_budget",
                initial_token_budget.map(Value::from),
            );
            policy_update(
                &mut updates,
                "initial_cost_budget_usd",
                initial_cost_budget_usd.map(Value::from),
            );
            policy_update(
                &mut updates,
                "initial_wall_clock_secs",
                initial_wall_clock_secs.map(Value::from),
            );
            policy_update(
                &mut updates,
                "hard_token_cap",
                hard_token_cap.map(Value::from),
            );
            policy_update(
                &mut updates,
                "hard_cost_cap_usd",
                hard_cost_cap_usd.map(Value::from),
            );
            policy_update(
                &mut updates,
                "hard_wall_clock_secs",
                hard_wall_clock_secs.map(Value::from),
            );
            policy_update(&mut updates, "extend_ratio", extend_ratio.map(Value::from));
            policy_update(
                &mut updates,
                "max_multiplier",
                max_multiplier.map(Value::from),
            );
            policy_update(
                &mut updates,
                "forecast_calls",
                forecast_calls.map(Value::from),
            );
            policy_update(
                &mut updates,
                "min_runway_turns",
                min_runway_turns.map(Value::from),
            );
            policy_update(
                &mut updates,
                "warn_at_ratio",
                warn_at_ratio.map(Value::from),
            );
            policy_update(
                &mut updates,
                "pause_at_ratio",
                pause_at_ratio.map(Value::from),
            );
            policy_update(
                &mut updates,
                "max_turns_per_run",
                max_turns_per_run.map(Value::from),
            );
            policy_update(
                &mut updates,
                "max_total_turns",
                max_total_turns.map(Value::from),
            );
            policy_update(
                &mut updates,
                "no_progress_turns",
                no_progress_turns.map(Value::from),
            );
            policy_update(
                &mut updates,
                "no_progress_action",
                no_progress_action.map(|action| json!(action.as_str())),
            );
            policy_update(
                &mut updates,
                "auto_tool_bundle",
                auto_tool_bundle.map(Value::from),
            );
            for (clear, key) in [
                (clear_initial_token_budget, "initial_token_budget"),
                (clear_initial_cost_budget_usd, "initial_cost_budget_usd"),
                (clear_initial_wall_clock_secs, "initial_wall_clock_secs"),
                (clear_hard_token_cap, "hard_token_cap"),
                (clear_hard_cost_cap_usd, "hard_cost_cap_usd"),
                (clear_hard_wall_clock_secs, "hard_wall_clock_secs"),
                (clear_max_total_turns, "max_total_turns"),
            ] {
                if clear {
                    updates.push((key, Value::Null));
                }
            }

            let policy_file_value = match policy_file.as_deref() {
                Some(path) => Some(parse_auto_budget_policy_json(&read_policy_file(path)?)?),
                None => None,
            };
            if updates.is_empty() && policy_file_value.is_none() {
                bail!("pass at least one auto-budget policy field or --policy-file -");
            }
            // Read-modify-write: unspecified fields (especially hard caps)
            // keep the stored value instead of falling back to defaults.
            let current = rpc(
                &discovery,
                "project.auto_budget.get",
                json!({"project_id": project}),
            )
            .await?;
            let current_policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            let policy =
                merge_auto_budget_policy(current_policy, policy_file_value.as_ref(), &updates);
            rpc(
                &discovery,
                "project.auto_budget.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::ProjectRunPolicy { project } => {
            rpc(
                &discovery,
                "project.run_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ProjectRunPolicySet {
            project,
            profile,
            reserve,
            attention_cap,
            no_progress_turns,
        } => {
            if profile.is_none()
                && reserve.is_none()
                && attention_cap.is_none()
                && no_progress_turns.is_none()
            {
                bail!(
                    "pass at least one of --profile, --reserve, --attention-cap, or --no-progress-turns"
                );
            }
            // Read-modify-write: unspecified fields keep the stored policy.
            let current = rpc(
                &discovery,
                "project.run_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy: vp_protocol::types::ProjectRunPolicy = serde_json::from_value(
                current.get("policy").cloned().unwrap_or_else(|| json!({})),
            )?;
            if let Some(profile) = profile {
                policy.default_profile = vp_protocol::types::RunProfile::parse(&profile)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "profile must be quality, balanced, fast, cheap, or unattended"
                        )
                    })?;
            }
            if let Some(reserve) = reserve {
                policy.completion_reserve_ratio = reserve;
            }
            if let Some(attention_cap) = attention_cap {
                policy.attention_cap = attention_cap;
            }
            if let Some(no_progress_turns) = no_progress_turns {
                policy.stop_policy.no_progress_turns = no_progress_turns;
            }
            rpc(
                &discovery,
                "project.run_policy.set",
                json!({"project_id": project, "policy": policy}),
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
            clear_context_window,
            input_price,
            clear_input_price,
            output_price,
            clear_output_price,
            notes,
        } => {
            let current = rpc(&discovery, "model.preset.list", json!({})).await?;
            let existing = current
                .get("presets")
                .and_then(Value::as_array)
                .and_then(|presets| {
                    presets.iter().find(|preset| {
                        preset.get("id").and_then(Value::as_str) == Some(id.as_str())
                    })
                })
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("model preset not found: {id}"))?;
            let params = merge_model_preset(
                existing,
                name,
                provider,
                model,
                context_window,
                clear_context_window,
                input_price,
                clear_input_price,
                output_price,
                clear_output_price,
                notes,
            )?;
            rpc(&discovery, "model.preset.update", params).await?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_merge_preserves_unspecified_caps() {
        let current = json!({
            "enabled": true,
            "initial_token_budget": 10_000,
            "hard_token_cap": 50_000,
            "hard_cost_cap_usd": 5.0,
        });
        let merged = merge_auto_budget_policy(
            current,
            None,
            &[("hard_cost_cap_usd", json!(7.5)), ("enabled", json!(false))],
        );
        assert_eq!(merged["enabled"], false);
        assert_eq!(merged["hard_token_cap"], 50_000);
        assert_eq!(merged["hard_cost_cap_usd"], 7.5);
        assert_eq!(merged["initial_token_budget"], 10_000);
    }

    #[test]
    fn policy_merge_applies_file_then_flags() {
        let current = json!({"hard_token_cap": 50_000, "enabled": false});
        let file = json!({"enabled": true, "min_runway_turns": 3});
        let merged = merge_auto_budget_policy(
            current,
            Some(&file),
            &[("enabled", json!(false)), ("forecast_calls", json!(12))],
        );
        assert_eq!(merged["enabled"], false);
        assert_eq!(merged["min_runway_turns"], 3);
        assert_eq!(merged["forecast_calls"], 12);
        assert_eq!(merged["hard_token_cap"], 50_000);
    }

    #[test]
    fn policy_file_rejects_unknown_fields() {
        let error =
            parse_auto_budget_policy_json(r#"{"hard_token_cap": 100, "typo": 1}"#).unwrap_err();
        let typed = error.downcast_ref::<crate::rpc::CliRpcError>().unwrap();
        assert_eq!(
            typed.typed_code.as_deref(),
            Some(vp_protocol::types::auto_budget_codes::INVALID_AUTO_BUDGET_POLICY)
        );
        assert!(typed.message.contains("typo"));
    }

    #[test]
    fn policy_file_requires_an_object() {
        assert!(parse_auto_budget_policy_json("[1, 2]").is_err());
        assert!(parse_auto_budget_policy_json("not json").is_err());
    }

    #[test]
    fn project_run_policy_commands_parse_and_keep_partial_fields() {
        let cli = Cli::try_parse_from(["vp", "project-run-policy", "proj-1"]).unwrap();
        match cli.command {
            Command::ProjectRunPolicy { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected project-run-policy, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "project-run-policy-set",
            "proj-1",
            "--profile",
            "fast",
            "--reserve",
            "0.3",
            "--attention-cap",
            "9",
            "--no-progress-turns",
            "4",
        ])
        .unwrap();
        match cli.command {
            Command::ProjectRunPolicySet {
                project,
                profile,
                reserve,
                attention_cap,
                no_progress_turns,
            } => {
                assert_eq!(project, "proj-1");
                assert_eq!(profile.as_deref(), Some("fast"));
                assert_eq!(reserve, Some(0.3));
                assert_eq!(attention_cap, Some(9));
                assert_eq!(no_progress_turns, Some(4));
            }
            other => panic!("expected project-run-policy-set, got {other:?}"),
        }
    }
}
