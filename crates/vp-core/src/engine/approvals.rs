use super::*;

impl Engine {
    pub async fn list_approvals(&self, session_id: Option<String>) -> Result<Vec<Approval>> {
        let store = self.store.clone();
        let mut approvals =
            tokio::task::spawn_blocking(move || store.list_approvals(session_id.as_deref()))
                .await??;
        // New rows are redacted before insert. This read-side pass also hides
        // credential-like input persisted by an older build or imported bundle.
        for approval in &mut approvals {
            crate::tooling::redaction::redact_persisted_payload(&mut approval.input);
        }
        Ok(approvals)
    }

    pub async fn list_approval_grants(&self, project_id: &str) -> Result<Vec<ApprovalGrant>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.list_approval_grants(&project_id)).await?
    }

    pub async fn create_approval_grant(
        &self,
        project_id: &str,
        tool: &str,
        path_prefix: &str,
        include_children: bool,
        created_by: &str,
    ) -> Result<ApprovalGrant> {
        let tool = tool.trim();
        let path_prefix = path_prefix.trim().trim_start_matches("./");
        if tool.is_empty() || tool.chars().count() > 120 {
            anyhow::bail!("approval tool must be 1-120 characters");
        }
        if path_prefix.is_empty() || path_prefix.chars().count() > 1_000 {
            anyhow::bail!("approval path must be 1-1000 characters");
        }
        if path_prefix.contains('$') || path_prefix.contains('~') {
            anyhow::bail!("approval path cannot contain shell or home expansion");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let tool = tool.to_string();
        let path_prefix = path_prefix.to_string();
        let created_by = created_by.to_string();
        let grant = tokio::task::spawn_blocking(move || {
            store.create_approval_grant(
                &project_id,
                &tool,
                &path_prefix,
                include_children,
                &created_by,
            )
        })
        .await??;
        self.record_project_manifest().await?;
        Ok(grant)
    }

    pub async fn delete_approval_grant(&self, id: &str) -> Result<bool> {
        let store = self.store.clone();
        let id = id.to_string();
        let deleted =
            tokio::task::spawn_blocking(move || store.delete_approval_grant(&id)).await??;
        if deleted {
            self.record_project_manifest().await?;
        }
        Ok(deleted)
    }

    /// Deterministic precedence evaluation for one tool call. The store's
    /// single writer lock makes template use consumption atomic.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_tool_permission(
        &self,
        session_id: &str,
        project_id: &str,
        assessment: &crate::permissions::PermissionAssessment,
        tool: &str,
        input: &serde_json::Value,
        mode: vp_protocol::types::ApprovalMode,
        auto_approve: bool,
    ) -> Result<crate::approvals::ApprovalEvaluation> {
        let templates = self.store.list_approval_templates(project_id)?;
        let grants = self.store.list_approval_grants(project_id)?;
        let now = time::OffsetDateTime::now_utc();
        let now_string = now_rfc3339();
        let mut consume = |template: &ApprovalTemplate| {
            self.store
                .consume_approval_template_use(&template.id, &now_string)
                .ok()
                .flatten()
        };
        let evaluation = crate::approvals::evaluate_approval_rules(
            &templates,
            &grants,
            assessment,
            tool,
            input,
            mode,
            auto_approve,
            now,
            &mut consume,
        );
        if let (Some(template_id), Some(template_name)) = (
            evaluation.template_id.as_deref(),
            evaluation.template_name.as_deref(),
        ) {
            let event = ApprovalAuditEvent {
                id: Uuid::now_v7().to_string(),
                project_id: project_id.to_string(),
                kind: "approval.template.matched".to_string(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "template_id": template_id,
                    "template_name": template_name,
                    "source": evaluation.source.as_str(),
                    "action": match evaluation.source {
                        crate::approvals::ApprovalDecisionSource::TemplateDeny => "deny",
                        crate::approvals::ApprovalDecisionSource::TemplateEscalate => "escalate",
                        _ => "allow",
                    },
                    "tool": tool,
                    "risk": evaluation.risk().as_str(),
                    "scope": evaluation.scope.clone(),
                    "remaining_uses": evaluation.remaining_uses,
                    "auto_approve": auto_approve,
                }),
                created_at: now_rfc3339(),
            };
            if let Err(error) = self.store.insert_approval_audit(&event) {
                tracing::warn!(%error, "failed to record approval template match audit");
            }
        }
        Ok(evaluation)
    }

    pub async fn approval_grant_matches(
        &self,
        project_id: &str,
        tool: &str,
        input: &serde_json::Value,
    ) -> Result<bool> {
        if tool.starts_with("remote_") {
            return Ok(false);
        }
        let paths = crate::approvals::extract_input_paths(input);
        if paths.is_empty() {
            return Ok(false);
        }
        let grants = self.list_approval_grants(project_id).await?;
        Ok(crate::approvals::match_grant(&grants, tool, &paths).is_some())
    }

    pub async fn request_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: serde_json::Value,
        agent: Option<serde_json::Value>,
    ) -> Result<(Approval, oneshot::Receiver<bool>)> {
        self.request_approval_with_risk(session_id, tool_call_id, tool, input, agent, None, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn request_approval_with_risk(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: serde_json::Value,
        agent: Option<serde_json::Value>,
        approval_risk: Option<&str>,
        approval_reason: Option<&str>,
    ) -> Result<(Approval, oneshot::Receiver<bool>)> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let tool_call_id = tool_call_id.to_string();
        let tool = tool.to_string();
        let approval_risk = approval_risk.map(str::to_string);
        let approval_reason = approval_reason.map(str::to_string);
        let policy = self.store.attention_policy_for_session(session_id)?;
        let priority = agent
            .as_ref()
            .and_then(|agent| agent.get("task_id"))
            .and_then(serde_json::Value::as_str)
            .and_then(|task_id| self.store.approval_task_priority(session_id, task_id).ok())
            .unwrap_or(0);
        // Hash and dedup from the real input so identical calls still coalesce.
        // The store row/event gets a redacted copy; evaluation already ran on
        // the real value before this request was created.
        let scope_hash = crate::approvals::approval_scope_hash(&input);
        let dedup_key = crate::approvals::approval_dedup_key(
            session_id,
            agent.as_ref(),
            &tool,
            &scope_hash,
            approval_risk.as_deref().unwrap_or("unknown"),
        );
        let input_for_store = crate::tooling::redaction::redact_persisted_value(&input);
        let now = time::OffsetDateTime::now_utc();
        let expires_at = policy
            .enabled
            .then(|| crate::approvals::approval_expires_at(now, approval_risk.as_deref(), &policy));
        let coalesce_window = if policy.enabled {
            policy.coalesce_window_secs
        } else {
            0
        };
        let notify_window = if policy.enabled {
            policy.notify_coalesce_secs
        } else {
            0
        };
        let max_pending = if policy.enabled {
            policy.max_pending_per_session
        } else {
            0
        };
        let agent_for_store = agent.clone();
        let risk_for_store = approval_risk.clone();
        let reason_for_store = approval_reason.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            store.create_approval_deduplicated(
                &session_id_owned,
                &tool_call_id,
                &tool,
                &input_for_store,
                agent_for_store.as_ref(),
                risk_for_store.as_deref(),
                reason_for_store.as_deref(),
                Some(&dedup_key),
                Some(&scope_hash),
                priority,
                expires_at.as_deref(),
                coalesce_window,
                notify_window,
                max_pending,
                &policy.priority_boost_risk,
            )
        })
        .await??;

        // Expired rows from the per-session cap deny their current waiters.
        for expired in &outcome.expired {
            self.deny_approval_waiters(&expired.id);
        }

        let (sender, receiver) = oneshot::channel();
        self.approval_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("approval mutex poisoned"))?
            .entry(outcome.approval.id.clone())
            .or_default()
            .push(sender);
        // Close the insert/resolve race: an operator may resolve before the
        // waiter is registered. Check the row once and deliver immediately if
        // it already reached a terminal status.
        if let Some(current) = self.store.get_approval(&outcome.approval.id)?
            && current.status != "pending"
        {
            let approved = current.status == "approved";
            if let Some(senders) = self
                .approval_waiters
                .lock()
                .ok()
                .and_then(|mut waiters| waiters.remove(&current.id))
            {
                for sender in senders {
                    let _ = sender.send(approved);
                }
            }
        }
        for event in &outcome.events {
            let _ = self.events.send(event.clone());
        }
        Ok((outcome.approval, receiver))
    }

    fn deny_approval_waiters(&self, approval_id: &str) {
        if let Some(senders) = self
            .approval_waiters
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(approval_id))
        {
            for sender in senders {
                let _ = sender.send(false);
            }
        }
    }

    pub async fn resolve_approval(&self, approval_id: &str, approved: bool) -> Result<bool> {
        let store = self.store.clone();
        let id = approval_id.to_string();
        let resolved =
            tokio::task::spawn_blocking(move || store.resolve_approval_with_events(&id, approved))
                .await??;
        if let Some((approval, events)) = resolved {
            self.release_approval_waiters(&approval.id, approved);
            for event in events {
                let _ = self.events.send(event);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn release_approval_waiters(&self, approval_id: &str, approved: bool) {
        if let Some(senders) = self
            .approval_waiters
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(approval_id))
        {
            for sender in senders {
                let _ = sender.send(approved);
            }
        }
    }

    /// Resolve an explicit per-id decision list. There is intentionally no
    /// "approve every pending" shorthand; the caller must name each id.
    pub async fn resolve_approvals(
        &self,
        decisions: Vec<(String, bool)>,
    ) -> Result<ApprovalBatchResolveResult> {
        if decisions.is_empty() {
            anyhow::bail!("approval batch must contain at least one decision");
        }
        if decisions.len() > crate::approvals::MAX_BATCH_APPROVALS {
            anyhow::bail!(
                "approval batch accepts at most {} decisions",
                crate::approvals::MAX_BATCH_APPROVALS
            );
        }
        let mut seen = HashSet::new();
        for (id, _) in &decisions {
            if id.trim().is_empty() {
                anyhow::bail!("approval id cannot be empty");
            }
            if !seen.insert(id.clone()) {
                anyhow::bail!("approval id '{id}' appears more than once");
            }
        }
        let store = self.store.clone();
        let decisions_for_store = decisions.clone();
        let outcomes = tokio::task::spawn_blocking(move || {
            store.resolve_approvals_with_events(&decisions_for_store)
        })
        .await??;
        let mut results = Vec::with_capacity(outcomes.len());
        let mut resolved_count = 0u32;
        let mut failed_count = 0u32;
        for outcome in outcomes {
            let approved = decisions
                .iter()
                .find(|(id, _)| id == &outcome.approval_id)
                .map(|(_, approved)| *approved)
                .unwrap_or(false);
            if let Some(approval) = &outcome.approval {
                if outcome.code.is_none() {
                    self.release_approval_waiters(&approval.id, approved);
                    resolved_count = resolved_count.saturating_add(1);
                } else {
                    failed_count = failed_count.saturating_add(1);
                }
                for event in &outcome.events {
                    let _ = self.events.send(event.clone());
                }
                let code = outcome.code.clone();
                let message = code.as_deref().map(|code| {
                    let (hint, _) = crate::approvals::error_hint(code);
                    format!("approval is {}; {hint}", approval.status)
                });
                results.push(ApprovalBatchItemResult {
                    approval_id: outcome.approval_id,
                    resolved: outcome.code.is_none(),
                    status: Some(approval.status.clone()),
                    code,
                    message,
                    waiter_count: approval.waiter_count,
                });
            } else {
                failed_count = failed_count.saturating_add(1);
                results.push(ApprovalBatchItemResult {
                    approval_id: outcome.approval_id,
                    resolved: false,
                    status: outcome.status,
                    code: outcome.code.clone(),
                    message: Some(
                        crate::approvals::error_hint(crate::approvals::APPROVAL_NOT_FOUND)
                            .0
                            .to_string(),
                    ),
                    waiter_count: 0,
                });
            }
        }
        Ok(ApprovalBatchResolveResult {
            results,
            resolved_count,
            failed_count,
        })
    }

    /// Expire stale approvals for every project with attention enabled. The
    /// sweep denies waiters and never approves a tool call.
    pub async fn sweep_expired_approvals(&self) -> Result<usize> {
        let store = self.store.clone();
        let now = time::OffsetDateTime::now_utc();
        let expired =
            tokio::task::spawn_blocking(move || store.expire_stale_approvals(now)).await??;
        let mut count = 0;
        for (approval, events) in expired {
            self.deny_approval_waiters(&approval.id);
            for event in events {
                let _ = self.events.send(event);
            }
            count += 1;
        }
        Ok(count)
    }

    pub async fn approval_queue(
        &self,
        session_id: Option<String>,
        project_id: Option<String>,
    ) -> Result<ApprovalQueueResult> {
        let store = self.store.clone();
        let session_filter = session_id.clone();
        let project_filter = project_id.clone();
        let mut approvals = tokio::task::spawn_blocking(move || {
            store.list_pending_approvals(session_filter.as_deref(), project_filter.as_deref())
        })
        .await??;
        for approval in &mut approvals {
            crate::tooling::redaction::redact_persisted_payload(&mut approval.input);
        }
        let policy = match (&session_id, &project_id) {
            (Some(session_id), _) => self.store.attention_policy_for_session(session_id)?,
            (None, Some(project_id)) => self.store.get_attention_policy(project_id)?,
            (None, None) => AttentionPolicy::default(),
        };
        Ok(ApprovalQueueResult {
            pending_count: approvals.len() as u32,
            approvals,
            policy,
            generated_at: now_rfc3339(),
        })
    }

    pub async fn list_approval_templates(&self, project_id: &str) -> Result<Vec<ApprovalTemplate>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.list_approval_templates(&project_id)).await?
    }

    pub async fn create_approval_template(
        &self,
        params: ApprovalTemplateCreateParams,
    ) -> Result<ApprovalTemplateResult> {
        let now = time::OffsetDateTime::now_utc();
        let action = crate::approvals::ApprovalTemplateAction::parse(&params.action)?;
        let tool_pattern = crate::approvals::validate_tool_pattern(&params.tool_pattern)?;
        let risk_levels = crate::approvals::validate_risk_levels(&params.risk_levels, action)?;
        let path_globs = crate::approvals::validate_path_globs(&params.path_globs)?;
        let expires_at =
            crate::approvals::validate_template_expiry(params.expires_at.as_deref(), now)?;
        let max_uses = crate::approvals::validate_max_uses(params.max_uses)?;
        let name = validate_approval_template_name(&params.name)?;
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let existing = store.list_approval_templates(&project_id)?;
        if existing.len() >= crate::approvals::MAX_APPROVAL_TEMPLATES {
            return Err(crate::approvals::ApprovalRuleError::invalid(format!(
                "a project supports at most {} approval templates",
                crate::approvals::MAX_APPROVAL_TEMPLATES
            ))
            .into());
        }
        if existing
            .iter()
            .any(|template| template.name.eq_ignore_ascii_case(&name))
        {
            return Err(crate::approvals::ApprovalRuleError::invalid(
                "approval template name already exists in this project",
            )
            .into());
        }
        let template = ApprovalTemplate {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.clone(),
            name,
            enabled: true,
            tool_pattern,
            risk_levels,
            path_globs,
            action: action.as_str().to_string(),
            expires_at,
            max_uses,
            used_count: 0,
            created_by: params
                .created_by
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "operator".to_string()),
            created_at: now
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|error| anyhow::anyhow!("formatting template time: {error}"))?,
            updated_at: None,
        };
        let store = self.store.clone();
        let template_for_store = template.clone();
        tokio::task::spawn_blocking(move || store.create_approval_template(&template_for_store))
            .await??;
        self.record_approval_audit(
            &project_id,
            "approval.template.created",
            serde_json::json!({
                "template_id": template.id,
                "name": template.name,
                "action": template.action,
                "tool_pattern": template.tool_pattern,
                "risk_levels": template.risk_levels,
                "path_globs": template.path_globs,
                "expires_at": template.expires_at,
                "max_uses": template.max_uses,
                "created_by": template.created_by,
            }),
        )
        .await?;
        self.record_project_manifest().await?;
        Ok(ApprovalTemplateResult { template })
    }

    pub async fn update_approval_template(
        &self,
        params: ApprovalTemplateUpdateParams,
    ) -> Result<ApprovalTemplateResult> {
        let now = time::OffsetDateTime::now_utc();
        let action = crate::approvals::ApprovalTemplateAction::parse(&params.action)?;
        let tool_pattern = crate::approvals::validate_tool_pattern(&params.tool_pattern)?;
        let risk_levels = crate::approvals::validate_risk_levels(&params.risk_levels, action)?;
        let path_globs = crate::approvals::validate_path_globs(&params.path_globs)?;
        let expires_at =
            crate::approvals::validate_template_expiry(params.expires_at.as_deref(), now)?;
        let max_uses = crate::approvals::validate_max_uses(params.max_uses)?;
        let name = validate_approval_template_name(&params.name)?;
        let store = self.store.clone();
        let existing = store.get_approval_template(&params.id)?.ok_or_else(|| {
            crate::approvals::ApprovalRuleError::new(
                crate::approvals::APPROVAL_TEMPLATE_INVALID,
                format!("approval template not found: {}", params.id),
            )
        })?;
        if !existing.name.eq_ignore_ascii_case(&name) {
            let siblings = store.list_approval_templates(&existing.project_id)?;
            if siblings
                .iter()
                .any(|template| template.name.eq_ignore_ascii_case(&name))
            {
                return Err(crate::approvals::ApprovalRuleError::invalid(
                    "approval template name already exists in this project",
                )
                .into());
            }
        }
        let template = ApprovalTemplate {
            id: existing.id,
            project_id: existing.project_id.clone(),
            name,
            enabled: params.enabled,
            tool_pattern,
            risk_levels,
            path_globs,
            action: action.as_str().to_string(),
            expires_at,
            max_uses,
            used_count: existing.used_count,
            created_by: existing.created_by,
            created_at: existing.created_at,
            updated_at: Some(
                now.format(&time::format_description::well_known::Rfc3339)
                    .map_err(|error| anyhow::anyhow!("formatting template time: {error}"))?,
            ),
        };
        let store = self.store.clone();
        let template_for_store = template.clone();
        let updated = tokio::task::spawn_blocking(move || {
            store.update_approval_template(&template_for_store)
        })
        .await??;
        if !updated {
            anyhow::bail!("approval template disappeared during update");
        }
        self.record_approval_audit(
            &template.project_id,
            "approval.template.updated",
            serde_json::json!({
                "template_id": template.id,
                "name": template.name,
                "enabled": template.enabled,
                "action": template.action,
                "tool_pattern": template.tool_pattern,
                "risk_levels": template.risk_levels,
                "path_globs": template.path_globs,
                "expires_at": template.expires_at,
                "max_uses": template.max_uses,
                "used_count": template.used_count,
            }),
        )
        .await?;
        self.record_project_manifest().await?;
        Ok(ApprovalTemplateResult { template })
    }

    pub async fn delete_approval_template(
        &self,
        params: ApprovalTemplateDeleteParams,
    ) -> Result<ApprovalTemplateDeleteResult> {
        let store = self.store.clone();
        let existing = store.get_approval_template(&params.id)?;
        let Some(existing) = existing else {
            return Ok(ApprovalTemplateDeleteResult { deleted: false });
        };
        let store = self.store.clone();
        let id = params.id.clone();
        let deleted =
            tokio::task::spawn_blocking(move || store.delete_approval_template(&id)).await??;
        if deleted {
            self.record_approval_audit(
                &existing.project_id,
                "approval.template.deleted",
                serde_json::json!({
                    "template_id": existing.id,
                    "name": existing.name,
                    "action": existing.action,
                    "used_count": existing.used_count,
                }),
            )
            .await?;
            self.record_project_manifest().await?;
        }
        Ok(ApprovalTemplateDeleteResult { deleted })
    }

    pub async fn approval_template_from_approval(
        &self,
        params: ApprovalTemplateFromApprovalParams,
    ) -> Result<ApprovalTemplateResult> {
        let store = self.store.clone();
        let approval = store.get_approval(&params.approval_id)?.ok_or_else(|| {
            crate::approvals::ApprovalRuleError::new(
                crate::approvals::APPROVAL_NOT_FOUND,
                format!("approval not found: {}", params.approval_id),
            )
        })?;
        let session = store
            .get_session(&approval.session_id)?
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", approval.session_id))?;
        let now = time::OffsetDateTime::now_utc();
        let mut template = crate::approvals::template_from_approval(
            &approval,
            params.name.as_deref(),
            params.max_uses,
            params.expires_at.as_deref(),
            "operator",
            now,
        )?;
        template.project_id = session.project_id.clone();
        let existing = store.list_approval_templates(&session.project_id)?;
        if existing.len() >= crate::approvals::MAX_APPROVAL_TEMPLATES {
            return Err(crate::approvals::ApprovalRuleError::invalid(format!(
                "a project supports at most {} approval templates",
                crate::approvals::MAX_APPROVAL_TEMPLATES
            ))
            .into());
        }
        if existing
            .iter()
            .any(|candidate| candidate.name.eq_ignore_ascii_case(&template.name))
        {
            template.name = format!(
                "{} {}",
                template.name,
                &template.id[..8.min(template.id.len())]
            );
        }
        let store = self.store.clone();
        let template_for_store = template.clone();
        tokio::task::spawn_blocking(move || store.create_approval_template(&template_for_store))
            .await??;
        self.record_approval_audit(
            &template.project_id,
            "approval.template.created",
            serde_json::json!({
                "template_id": template.id,
                "name": template.name,
                "action": template.action,
                "tool_pattern": template.tool_pattern,
                "risk_levels": template.risk_levels,
                "path_globs": template.path_globs,
                "expires_at": template.expires_at,
                "max_uses": template.max_uses,
                "created_by": template.created_by,
                "from_approval": approval.id,
            }),
        )
        .await?;
        self.record_project_manifest().await?;
        Ok(ApprovalTemplateResult { template })
    }

    pub async fn get_attention_policy(&self, project_id: &str) -> Result<AttentionPolicyResult> {
        let project_id = project_id.to_string();
        let store = self.store.clone();
        let id = project_id.clone();
        let policy = tokio::task::spawn_blocking(move || store.get_attention_policy(&id)).await??;
        Ok(AttentionPolicyResult { project_id, policy })
    }

    pub async fn set_attention_policy(
        &self,
        params: AttentionPolicySetParams,
    ) -> Result<AttentionPolicyResult> {
        let policy = crate::approvals::validate_attention_policy(params.policy);
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || {
            store.set_attention_policy(&project_id, &policy_for_store)
        })
        .await??;
        self.record_approval_audit(
            &params.project_id,
            "approval.policy.updated",
            serde_json::json!({
                "enabled": policy.enabled,
                "max_pending_per_session": policy.max_pending_per_session,
                "coalesce_window_secs": policy.coalesce_window_secs,
                "stale_after_secs": policy.stale_after_secs,
                "priority_boost_risk": policy.priority_boost_risk,
                "notify_coalesce_secs": policy.notify_coalesce_secs,
            }),
        )
        .await?;
        self.record_project_manifest().await?;
        Ok(AttentionPolicyResult {
            project_id: params.project_id,
            policy,
        })
    }

    pub async fn approval_audit(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<ApprovalAuditResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let events =
            tokio::task::spawn_blocking(move || store.list_approval_audit(&project_id, limit))
                .await??;
        Ok(ApprovalAuditResult { events })
    }

    async fn record_approval_audit(
        &self,
        project_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let event = ApprovalAuditEvent {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            kind: kind.to_string(),
            payload,
            created_at: now_rfc3339(),
        };
        let store = self.store.clone();
        match tokio::task::spawn_blocking(move || store.insert_approval_audit(&event)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                tracing::warn!(%error, kind, "failed to record approval audit event");
                Err(error)
            }
            Err(error) => {
                tracing::warn!(%error, kind, "approval audit task failed");
                Err(anyhow::anyhow!("approval audit task failed: {error}"))
            }
        }
    }
}

fn validate_approval_template_name(
    value: &str,
) -> std::result::Result<String, crate::approvals::ApprovalRuleError> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(crate::approvals::ApprovalRuleError::invalid(
            "approval template name must be 1-120 characters",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(crate::approvals::ApprovalRuleError::invalid(
            "approval template name cannot contain control characters",
        ));
    }
    Ok(name.to_string())
}
