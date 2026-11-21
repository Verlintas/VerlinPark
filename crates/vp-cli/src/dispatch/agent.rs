//! Project identity, hiring, assignment, workspace, and inbox commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Agents {
            project,
            session,
            role,
            status,
        } => {
            if let Some(session) = session {
                rpc(&discovery, "agent.list", json!({"session_id": session})).await?
            } else {
                let projects = rpc(&discovery, "project.list", json!({})).await?;
                let is_project = projects["projects"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| row["id"].as_str() == Some(&project))
                });
                if is_project {
                    rpc(
                        &discovery,
                        "agent.identities.list",
                        json!({"project_id": project, "role": role, "status": status}),
                    )
                    .await?
                } else {
                    // Backward-compatible `vp agents <session>` behavior.
                    rpc(&discovery, "agent.list", json!({"session_id": project})).await?
                }
            }
        }
        Command::AgentOrg {
            project,
            include_retired,
            limit,
        } => {
            rpc(
                &discovery,
                "agent.org.snapshot",
                json!({
                    "project_id": project,
                    "include_retired": include_retired,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::AgentDetail {
            identity,
            project,
            include_inbox,
            inbox_limit,
            memory_limit,
            assignment_limit,
        } => {
            rpc(
                &discovery,
                "agent.identity.detail",
                json!({
                    "identity_id": identity,
                    "project_id": project,
                    "include_inbox_content": include_inbox,
                    "inbox_limit": inbox_limit,
                    "memory_limit": memory_limit,
                    "assignment_limit": assignment_limit,
                }),
            )
            .await?
        }
        Command::AgentInspect {
            session,
            agent,
            timeline_limit,
        } => {
            rpc(
                &discovery,
                "agent.inspect",
                json!({
                    "session_id": session,
                    "agent_id": agent,
                    "timeline_limit": timeline_limit,
                }),
            )
            .await?
        }
        Command::AgentMessage {
            session,
            agent,
            text,
            task,
            wake,
            operation_id,
        } => {
            rpc(
                &discovery,
                "agent.message.send",
                json!({
                    "session_id": session,
                    "agent_id": agent,
                    "body": text,
                    "task_id": task,
                    "operation_id": operation_id,
                    "wake_session": wake,
                }),
            )
            .await?
        }
        Command::TeamExport {
            project,
            name,
            version,
            description,
            out,
        } => {
            let value = rpc(
                &discovery,
                "team.template.export",
                json!({
                    "project_id": project,
                    "name": name,
                    "version": version,
                    "description": description,
                }),
            )
            .await?;
            let json_body = value
                .get("json")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("team export returned no JSON body"))?;
            let markdown = value
                .get("markdown")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("team export returned no Markdown body"))?;
            std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
            let json_path = out.join("team.json");
            let markdown_path = out.join("TEAM.md");
            std::fs::write(&json_path, json_body)
                .with_context(|| format!("writing {}", json_path.display()))?;
            std::fs::write(&markdown_path, markdown)
                .with_context(|| format!("writing {}", markdown_path.display()))?;
            json!({
                "out": out.display().to_string(),
                "json_path": json_path.display().to_string(),
                "markdown_path": markdown_path.display().to_string(),
                "template_id": value["template"]["template_id"],
                "name": value["template"]["name"],
                "version": value["template"]["version"],
                "content_hash": value["template"]["content_hash"],
                "warnings": value["warnings"],
            })
        }
        Command::TeamImport {
            project,
            file,
            dry_run,
            apply,
            approved,
            operator,
            resolution_json,
        } => {
            let json_body = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let resolutions: Vec<vp_protocol::types::TeamTemplateResolution> = match resolution_json
            {
                Some(raw) => serde_json::from_str(&raw)
                    .context("--resolution-json must be a JSON array of resolutions")?,
                None => Vec::new(),
            };
            let method = if dry_run {
                "team.template.preview"
            } else {
                "team.template.import"
            };
            rpc(
                &discovery,
                method,
                json!({
                    "project_id": project,
                    "json": json_body,
                    "dry_run": dry_run,
                    "apply": apply && !dry_run,
                    "approved": approved,
                    "operator": operator,
                    "resolutions": resolutions,
                }),
            )
            .await?
        }
        Command::TeamTemplates { project, limit } => {
            rpc(
                &discovery,
                "team.template.list",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::TeamDiff { project, template } => {
            rpc(
                &discovery,
                "team.template.diff",
                json!({"project_id": project, "template_id": template}),
            )
            .await?
        }
        Command::TeamTemplateShow { template, project } => {
            rpc(
                &discovery,
                "team.template.get",
                json!({"template_id": template, "project_id": project}),
            )
            .await?
        }
        Command::TeamReplay { project, repair } => {
            rpc(
                &discovery,
                "team.template.replay",
                json!({"project_id": project, "repair": repair}),
            )
            .await?
        }
        Command::AgentCreate {
            project,
            role,
            department,
            component,
            title,
            profile,
            model,
            parent,
            policy,
            approved,
        } => {
            let policy = parse_agent_identity_policy_json(policy.as_deref())?;
            if role.trim().eq_ignore_ascii_case("leader") {
                rpc(
                    &discovery,
                    "agent.identity.create",
                    json!({
                        "project_id": project,
                        "role": role,
                        "department": department,
                        "component": component,
                        "title": title,
                        "profile": profile,
                        "model": model,
                        "parent_identity_id": parent,
                        "policy": policy,
                    }),
                )
                .await?
            } else {
                rpc(
                    &discovery,
                    "agent.hire",
                    json!({
                        "project_id": project,
                        "role": role,
                        "department": department,
                        "component": component,
                        "title": title,
                        "profile": profile,
                        "model": model,
                        "parent_identity_id": parent,
                        "policy": policy,
                        "approved": approved,
                    }),
                )
                .await?
            }
        }
        Command::AgentUpdate {
            identity,
            role,
            department,
            clear_department,
            component,
            clear_component,
            title,
            clear_title,
            profile,
            clear_profile,
            model,
            clear_model,
            parent,
            clear_parent,
            successor,
            clear_successor,
            policy,
        } => {
            if role.is_some()
                || department.is_some()
                || clear_department
                || component.is_some()
                || clear_component
                || parent.is_some()
                || clear_parent
            {
                bail!(
                    "role/department/component/parent changes require `vp agent-promote` or `vp agent-transfer` approval"
                );
            }
            if clear_department && department.is_some() {
                bail!("pass only one of --department or --clear-department");
            }
            if clear_component && component.is_some() {
                bail!("pass only one of --component or --clear-component");
            }
            if clear_title && title.is_some() {
                bail!("pass only one of --title or --clear-title");
            }
            if clear_profile && profile.is_some() {
                bail!("pass only one of --profile or --clear-profile");
            }
            if clear_model && model.is_some() {
                bail!("pass only one of --model or --clear-model");
            }
            if clear_parent && parent.is_some() {
                bail!("pass only one of --parent or --clear-parent");
            }
            if clear_successor && successor.is_some() {
                bail!("pass only one of --successor or --clear-successor");
            }
            let mut params = serde_json::Map::new();
            params.insert("identity_id".into(), json!(identity));
            if let Some(role) = role {
                params.insert("role".into(), json!(role));
            }
            insert_agent_identity_optional(&mut params, "department", department, clear_department);
            insert_agent_identity_optional(&mut params, "component", component, clear_component);
            insert_agent_identity_optional(&mut params, "title", title, clear_title);
            insert_agent_identity_optional(&mut params, "profile", profile, clear_profile);
            insert_agent_identity_optional(&mut params, "model", model, clear_model);
            insert_agent_identity_optional(&mut params, "parent_identity_id", parent, clear_parent);
            insert_agent_identity_optional(&mut params, "successor_id", successor, clear_successor);
            if let Some(policy) = policy {
                params.insert(
                    "policy".into(),
                    parse_agent_identity_policy_json(Some(&policy))?.unwrap_or_else(|| json!({})),
                );
            }
            rpc(&discovery, "agent.identity.update", Value::Object(params)).await?
        }
        Command::AgentPause { identity } => {
            rpc(
                &discovery,
                "agent.identity.pause",
                json!({"identity_id": identity}),
            )
            .await?
        }
        Command::AgentResume { identity } => {
            rpc(
                &discovery,
                "agent.identity.resume",
                json!({"identity_id": identity}),
            )
            .await?
        }
        Command::AgentRetire {
            identity,
            successor,
            proposal,
            approved,
            force,
            reason,
            operator,
        } => {
            rpc(
                &discovery,
                "agent.retire",
                json!({
                    "identity_id": identity,
                    "successor_id": successor,
                    "proposal_id": proposal,
                    "approved": approved,
                    "force": force,
                    "reason": reason,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::AgentHire {
            project,
            role,
            department,
            component,
            title,
            profile,
            model,
            parent,
            policy,
            approved,
            operator,
        } => {
            let policy = parse_agent_identity_policy_json(policy.as_deref())?;
            rpc(
                &discovery,
                "agent.hire",
                json!({
                    "project_id": project,
                    "role": role,
                    "department": department,
                    "component": component,
                    "title": title,
                    "profile": profile,
                    "model": model,
                    "parent_identity_id": parent,
                    "policy": policy,
                    "approved": approved,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::AgentHires {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "agent.hire.list",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::AgentHireApprove { request, operator } => {
            rpc(
                &discovery,
                "agent.hire.approve",
                json!({"request_id": request, "operator": operator, "approval_risk": "host_state"}),
            )
            .await?
        }
        Command::AgentHireReject {
            request,
            reason,
            operator,
        } => {
            rpc(
                &discovery,
                "agent.hire.reject",
                json!({"request_id": request, "reason": reason, "operator": operator}),
            )
            .await?
        }
        Command::AgentPromote {
            identity,
            role,
            parent,
            title,
            force,
            operator,
        } => {
            rpc(
                &discovery,
                "agent.promote",
                json!({
                    "identity_id": identity,
                    "target_role": role,
                    "parent_identity_id": parent,
                    "title": title,
                    "approved": true,
                    "force": force,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::AgentTransfer {
            identity,
            department,
            clear_department,
            component,
            clear_component,
            title,
            clear_title,
            parent,
            clear_parent,
            force,
            operator,
        } => {
            if clear_department && department.is_some() {
                bail!("pass only one of --department or --clear-department");
            }
            if clear_component && component.is_some() {
                bail!("pass only one of --component or --clear-component");
            }
            if clear_title && title.is_some() {
                bail!("pass only one of --title or --clear-title");
            }
            if clear_parent && parent.is_some() {
                bail!("pass only one of --parent or --clear-parent");
            }
            if department.is_none()
                && !clear_department
                && component.is_none()
                && !clear_component
                && title.is_none()
                && !clear_title
                && parent.is_none()
                && !clear_parent
            {
                bail!("pass at least one transfer field");
            }
            let mut params = serde_json::Map::new();
            params.insert("identity_id".into(), json!(identity));
            params.insert("approved".into(), json!(true));
            params.insert("force".into(), json!(force));
            if let Some(operator) = operator {
                params.insert("operator".into(), json!(operator));
            }
            insert_agent_identity_optional(&mut params, "department", department, clear_department);
            insert_agent_identity_optional(&mut params, "component", component, clear_component);
            insert_agent_identity_optional(&mut params, "title", title, clear_title);
            insert_agent_identity_optional(&mut params, "parent_identity_id", parent, clear_parent);
            rpc(&discovery, "agent.transfer", Value::Object(params)).await?
        }
        Command::AgentPerformance {
            project,
            identity,
            lookback_days,
            limit,
        } => {
            if let Some(identity) = identity {
                rpc(
                    &discovery,
                    "agent.performance",
                    json!({
                        "project_id": project,
                        "identity_id": identity,
                        "lookback_days": lookback_days,
                    }),
                )
                .await?
            } else {
                rpc(
                    &discovery,
                    "agent.performance.summary",
                    json!({
                        "project_id": project,
                        "lookback_days": lookback_days,
                        "limit": limit,
                    }),
                )
                .await?
            }
        }
        Command::AgentRetirement {
            project,
            candidates,
            proposals,
            propose,
            approve,
            reject,
            reason,
            identity,
            status,
            operator,
            limit,
        } => {
            let selected = [
                candidates,
                proposals,
                propose.is_some(),
                approve.is_some(),
                reject.is_some(),
            ]
            .into_iter()
            .filter(|value| *value)
            .count();
            if selected > 1 {
                bail!("pass only one retirement action");
            }
            if let Some(identity) = propose {
                rpc(
                    &discovery,
                    "agent.retirement.propose",
                    json!({"identity_id": identity, "operator": operator}),
                )
                .await?
            } else if let Some(proposal) = approve {
                rpc(
                    &discovery,
                    "agent.retirement.approve",
                    json!({"proposal_id": proposal, "operator": operator}),
                )
                .await?
            } else if let Some(proposal) = reject {
                rpc(
                    &discovery,
                    "agent.retirement.reject",
                    json!({"proposal_id": proposal, "reason": reason, "operator": operator}),
                )
                .await?
            } else if proposals {
                rpc(
                    &discovery,
                    "agent.retirement.proposals",
                    json!({
                        "project_id": project,
                        "identity_id": identity,
                        "status": status,
                        "limit": limit,
                    }),
                )
                .await?
            } else {
                rpc(
                    &discovery,
                    "agent.retirement.candidates",
                    json!({"project_id": project, "limit": limit}),
                )
                .await?
            }
        }
        Command::RetirementPolicy { project } => {
            rpc(
                &discovery,
                "project.retirement_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RetirementPolicySet {
            project,
            enabled,
            lookback_days,
            min_completed_tasks,
            max_consecutive_failures,
            max_verifier_rejections,
            max_unsubstantiated_handoffs,
            max_false_consensus,
            cooldown_secs,
            require_approval,
        } => {
            let current = rpc(
                &discovery,
                "project.retirement_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(value) = lookback_days {
                policy["lookback_days"] = json!(value);
            }
            if let Some(value) = min_completed_tasks {
                policy["min_completed_tasks"] = json!(value);
            }
            if let Some(value) = max_consecutive_failures {
                policy["max_consecutive_failures"] = json!(value);
            }
            if let Some(value) = max_verifier_rejections {
                policy["max_verifier_rejections"] = json!(value);
            }
            if let Some(value) = max_unsubstantiated_handoffs {
                policy["max_unsubstantiated_handoffs"] = json!(value);
            }
            if let Some(value) = max_false_consensus {
                policy["max_false_consensus"] = json!(value);
            }
            if let Some(value) = cooldown_secs {
                policy["cooldown_secs"] = json!(value);
            }
            if let Some(value) = require_approval {
                policy["require_approval"] = json!(value);
            }
            rpc(
                &discovery,
                "project.retirement_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::AgentPolicy { project } => {
            rpc(
                &discovery,
                "project.agent_identity_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::AgentPolicySet {
            project,
            enabled,
            max_managers,
            max_workers,
            hire_requires_approval,
            retirement_requires_approval,
        } => {
            let current = rpc(
                &discovery,
                "project.agent_identity_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(max_managers) = max_managers {
                policy["max_managers"] = json!(max_managers);
            }
            if let Some(max_workers) = max_workers {
                policy["max_workers"] = json!(max_workers);
            }
            if let Some(hire_requires_approval) = hire_requires_approval {
                policy["hire_requires_approval"] = json!(hire_requires_approval);
            }
            if let Some(retirement_requires_approval) = retirement_requires_approval {
                policy["retirement_requires_approval"] = json!(retirement_requires_approval);
            }
            rpc(
                &discovery,
                "project.agent_identity_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Assignments {
            session,
            identity,
            project,
            status,
            limit,
        } => {
            if session.is_none() && identity.is_none() && project.is_none() {
                bail!("pass a session, --identity, or --project");
            }
            rpc(
                &discovery,
                "session.assignments",
                json!({
                    "session_id": session,
                    "identity_id": identity,
                    "project_id": project,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::Assign {
            session,
            identity,
            role,
            lease_secs,
            wake,
            operator,
        } => {
            rpc(
                &discovery,
                "session.assign",
                json!({
                    "session_id": session,
                    "identity_id": identity,
                    "role": role,
                    "lease_secs": lease_secs,
                    "wake_session": wake,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::AssignmentRenew {
            session,
            identity,
            lease_secs,
            operator,
        } => {
            rpc(
                &discovery,
                "session.assignment.renew",
                json!({
                    "session_id": session,
                    "identity_id": identity,
                    "lease_secs": lease_secs,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::Release {
            session,
            identity,
            operator,
            reason,
        } => {
            rpc(
                &discovery,
                "session.release",
                json!({
                    "session_id": session,
                    "identity_id": identity,
                    "operator": operator,
                    "reason": reason,
                }),
            )
            .await?
        }
        Command::Workspaces {
            project,
            session,
            identity,
            status,
            limit,
        } => {
            if project.is_none() && session.is_none() && identity.is_none() {
                bail!("pass --project, --session, or --identity");
            }
            rpc(
                &discovery,
                "agent.workspace.list",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "identity_id": identity,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::Workspace { workspace, session } => match (workspace, session) {
            (Some(workspace_id), None) => {
                rpc(
                    &discovery,
                    "agent.workspace.get",
                    json!({"workspace_id": workspace_id}),
                )
                .await?
            }
            (None, Some(session_id)) => {
                rpc(
                    &discovery,
                    "session.workspace",
                    json!({"session_id": session_id}),
                )
                .await?
            }
            (Some(_), Some(_)) => bail!("pass either a workspace id or --session, not both"),
            (None, None) => bail!("pass a workspace id or --session"),
        },
        Command::WorkspaceLand {
            workspace,
            session,
            operator,
            yes,
        } => {
            if !yes {
                bail!("workspace-land requires --yes to confirm the component-branch merge");
            }
            rpc(
                &discovery,
                "agent.workspace.land",
                json!({
                    "workspace_id": workspace,
                    "session_id": session,
                    "operator": operator,
                    "confirm": true,
                }),
            )
            .await?
        }
        Command::WorkspaceArchive {
            workspace,
            retire,
            operator,
            reason,
        } => {
            let method = if retire {
                "agent.workspace.retire"
            } else {
                "agent.workspace.archive"
            };
            rpc(
                &discovery,
                method,
                json!({
                    "workspace_id": workspace,
                    "operator": operator,
                    "reason": reason,
                }),
            )
            .await?
        }
        Command::Inbox {
            identity,
            project,
            session,
            status,
            unread,
            limit,
        } => {
            if identity.is_none() && project.is_none() && session.is_none() {
                bail!("pass an identity, --project, or --session");
            }
            rpc(
                &discovery,
                "agent.inbox.list",
                json!({
                    "identity_id": identity,
                    "project_id": project,
                    "session_id": session,
                    "status": status,
                    "unread_only": unread,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::InboxSend {
            identity,
            project,
            session,
            from_identity,
            kind,
            body,
            payload,
            expires_in_secs,
            wake,
        } => {
            if body.is_some() && payload.is_some() {
                bail!("pass either --body or --payload-json, not both");
            }
            let payload = match payload {
                Some(raw) => {
                    let value: Value =
                        serde_json::from_str(&raw).context("--payload-json must be valid JSON")?;
                    if !value.is_object() {
                        bail!("--payload-json must be a JSON object");
                    }
                    Some(value)
                }
                None => None,
            };
            let project = match project {
                Some(project) => Some(project),
                None => {
                    let identity = rpc(
                        &discovery,
                        "agent.identities.get",
                        json!({"identity_id": identity}),
                    )
                    .await?;
                    identity
                        .get("identity")
                        .and_then(|identity| identity.get("project_id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }
            };
            rpc(
                &discovery,
                "agent.inbox.send",
                json!({
                    "project_id": project,
                    "identity_id": identity,
                    "session_id": session,
                    "from_identity_id": from_identity,
                    "kind": kind,
                    "body": body,
                    "payload": payload,
                    "expires_in_secs": expires_in_secs,
                    "wake_session": wake,
                }),
            )
            .await?
        }
        Command::InboxRead {
            message,
            project,
            identity,
        } => {
            rpc(
                &discovery,
                "agent.inbox.read",
                json!({
                    "message_id": message,
                    "project_id": project,
                    "identity_id": identity,
                }),
            )
            .await?
        }
        Command::InboxAck {
            message,
            project,
            identity,
        } => {
            rpc(
                &discovery,
                "agent.inbox.ack",
                json!({
                    "message_id": message,
                    "project_id": project,
                    "identity_id": identity,
                }),
            )
            .await?
        }
        Command::CoordinationReplay { project, repair } => {
            rpc(
                &discovery,
                "agent.coordination.replay",
                json!({"project_id": project, "repair": repair}),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
