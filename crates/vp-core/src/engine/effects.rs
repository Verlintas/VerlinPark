use super::*;

impl Engine {
    pub async fn get_contract_policy(&self, project_id: &str) -> Result<ContractPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_contract_policy(&project_id)).await?
    }

    pub async fn set_contract_policy(
        &self,
        project_id: &str,
        policy: ContractPolicy,
    ) -> Result<ContractPolicy> {
        let store = self.store.clone();
        let project_id_for_check = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&project_id_for_check))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = policy
            .normalized()
            .map_err(|error| anyhow::anyhow!("invalid contract policy: {error}"))?;
        let for_store = policy.clone();
        let project_id = project_id.to_string();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.set_contract_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    // ---------------------------------------------------------------------
    // Effect receipts and postconditions
    // ---------------------------------------------------------------------

    pub async fn get_effect_policy(&self, project_id: &str) -> Result<EffectPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_effect_policy(&project_id)).await?
    }

    pub async fn set_effect_policy(
        &self,
        project_id: &str,
        policy: EffectPolicy,
    ) -> Result<EffectPolicy> {
        let store = self.store.clone();
        let project_id_for_check = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&project_id_for_check))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = policy
            .normalized()
            .map_err(|error| anyhow::anyhow!("invalid effect policy: {error}"))?;
        let for_store = policy.clone();
        let project_id = project_id.to_string();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.set_effect_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    /// Look up the logical effect before a call executes. A completed row is
    /// replayed, an unresolved row blocks execution, and a failed row starts a
    /// new attempt. The caller still passes the returned receipt to the journal
    /// intent so the idempotency check happens inside the same transaction.
    pub async fn prepare_tool_effect(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        agent_id: &str,
        worktree_head: &str,
        plan: &crate::effect::ToolEffectPlan,
    ) -> Result<crate::effect::EffectPrepare> {
        let store = self.store.clone();
        let session = session_id.to_string();
        let task = task_id.map(str::to_string);
        let tool = plan.tool.clone();
        let target = plan.target.canonical();
        // One logical effect includes the agent and checkout context. A
        // Verifier must not replay a Worker's task, and a write after a reset
        // or new commit must not be skipped as already done.
        let request_hash = crate::effect::scope_request_hash(
            &plan.request_hash,
            agent_id.trim(),
            worktree_head.trim(),
        );
        let lookup_hash = request_hash.clone();
        let existing = tokio::task::spawn_blocking(move || {
            store.latest_effect(&session, task.as_deref(), &tool, &target, &lookup_hash)
        })
        .await??;
        let mut scoped_plan = plan.clone();
        scoped_plan.request_hash = request_hash;
        let prepared = match existing {
            Some(receipt) if receipt.state == EffectState::Done => {
                crate::effect::EffectPrepare::Replay(receipt)
            }
            Some(receipt)
                if matches!(receipt.state, EffectState::Intent | EffectState::Unknown) =>
            {
                crate::effect::EffectPrepare::Blocked(receipt)
            }
            Some(receipt) => crate::effect::EffectPrepare::Proceed(crate::effect::intent_receipt(
                session_id,
                task_id,
                &scoped_plan,
                receipt.attempt.saturating_add(1),
            )),
            None => crate::effect::EffectPrepare::Proceed(crate::effect::intent_receipt(
                session_id,
                task_id,
                &scoped_plan,
                1,
            )),
        };
        Ok(prepared)
    }

    /// Persist a standalone effect intent (runtime actions such as a Git push,
    /// PR creation, landing, or remote integration). Returns the stored row and
    /// whether a new intent was actually inserted.
    pub async fn begin_effect(&self, receipt: EffectReceipt) -> Result<(EffectReceipt, bool)> {
        let store = self.store.clone();
        let (stored, events, inserted) =
            tokio::task::spawn_blocking(move || store.begin_effect(&receipt)).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok((stored, inserted))
    }

    /// Settle a standalone effect with bounded evidence. The stored state is
    /// terminal for that attempt.
    pub async fn settle_effect(&self, receipt: EffectReceipt) -> Result<EffectReceipt> {
        let store = self.store.clone();
        let (stored, events, _) =
            tokio::task::spawn_blocking(move || store.settle_effect(&receipt)).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(stored)
    }

    /// Record an already-settled runtime effect (Worker commit, integration
    /// merge). The row and event commit together; an existing logical receipt
    /// is returned unchanged.
    pub async fn record_settled_effect(&self, receipt: EffectReceipt) -> Result<EffectReceipt> {
        let store = self.store.clone();
        let (stored, event, _) =
            tokio::task::spawn_blocking(move || store.record_settled_effect(&receipt)).await??;
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        Ok(stored)
    }

    pub async fn lookup_effect(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        tool: &str,
        target: &str,
        request_hash: &str,
    ) -> Result<Option<EffectReceipt>> {
        let store = self.store.clone();
        let session = session_id.to_string();
        let task = task_id.map(str::to_string);
        let tool = tool.to_string();
        let target = target.to_string();
        let hash = request_hash.to_string();
        tokio::task::spawn_blocking(move || {
            store.latest_effect(&session, task.as_deref(), &tool, &target, &hash)
        })
        .await?
    }

    pub async fn get_effect(&self, id: &str) -> Result<Option<EffectReceipt>> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.get_effect(&id)).await?
    }

    /// Unresolved receipts that block a task merge. The check always includes
    /// session-scoped rows (for example a partially failed PR or remote
    /// integration) and, when `task_id` is present, that task's rows too.
    pub async fn effect_blockers(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<Vec<EffectReceipt>> {
        let store = self.store.clone();
        let session = session_id.to_string();
        let rows = tokio::task::spawn_blocking(move || {
            store.list_effect_receipts(&session, None, false, 500)
        })
        .await??;
        let project_id = self.session_project_id(session_id).await?;
        let policy = self.get_effect_policy(&project_id).await?;
        let task = task_id.map(str::to_string);
        Ok(rows
            .into_iter()
            .filter(|receipt| {
                // A session-scope gate (`task_id = None`) must include
                // task-scoped receipts too, otherwise `session.land` can merge
                // a branch whose task effect is still unknown.
                let in_scope = match task.as_deref() {
                    None => true,
                    Some(task) => {
                        receipt.task_id.is_none() || receipt.task_id.as_deref() == Some(task)
                    }
                };
                in_scope
                    && receipt.state.blocks_merge()
                    && policy.enabled
                    && policy.unknown_blocks_merge
            })
            .collect())
    }

    /// Fail-closed integration gate. This is checked before a queue entry is
    /// created and again immediately before the session branch is mutated.
    pub async fn assert_effects_settled(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<()> {
        let blockers = self.effect_blockers(session_id, task_id).await?;
        if blockers.is_empty() {
            return Ok(());
        }
        let refs = blockers.iter().collect::<Vec<_>>();
        Err(anyhow::Error::new(crate::effect::EffectBlocked {
            session_id: session_id.to_string(),
            task_id: task_id.map(str::to_string),
            receipt_ids: blockers.iter().map(|receipt| receipt.id.clone()).collect(),
            message: crate::effect::block_message(&refs),
        }))
    }

    async fn session_project_id(&self, session_id: &str) -> Result<String> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .map(|session| session.project_id)
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))
    }

    pub async fn session_effects(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        include_settled: bool,
        limit: u32,
    ) -> Result<SessionEffectsResult> {
        let project_id = self.session_project_id(session_id).await?;
        let policy = self.get_effect_policy(&project_id).await?;
        let store = self.store.clone();
        let session = session_id.to_string();
        let task = task_id.map(str::to_string);
        let limit = limit.clamp(1, 500);
        let receipts = tokio::task::spawn_blocking(move || {
            store.list_effect_receipts(&session, task.as_deref(), include_settled, limit)
        })
        .await??;
        let other_store = self.store.clone();
        let other_session = session_id.to_string();
        let other_task = task_id.map(str::to_string);
        let rows = tokio::task::spawn_blocking(move || {
            other_store.list_effect_receipts(&other_session, other_task.as_deref(), true, 500)
        })
        .await??;
        let mut counts = crate::effect::counts(&rows, &policy);
        let blockers = crate::effect::blocking_receipts(&rows, &policy);
        let mut blocked = !blockers.is_empty();
        let mut block_reason = blocked.then(|| crate::effect::block_message(&blockers));
        let truncated = rows.len() >= 500;
        if truncated && policy.enabled && policy.unknown_blocks_merge {
            // The newest 500 rows may not include an older unresolved receipt.
            // Count without the display cap and fail closed if any exist.
            let count_store = self.store.clone();
            let count_session = session_id.to_string();
            let count_task = task_id.map(str::to_string);
            let unresolved = tokio::task::spawn_blocking(move || {
                count_store.count_unresolved_effect_receipts(&count_session, count_task.as_deref())
            })
            .await??;
            if unresolved > 0 {
                counts.blocking = counts.blocking.max(unresolved);
            }
            if unresolved > 0 && !blocked {
                blocked = true;
                block_reason = Some(format!(
                    "{unresolved} unresolved effect receipt(s) exist outside the bounded 500-row window; reconcile them before merge or landing"
                ));
            }
        }
        Ok(SessionEffectsResult {
            session_id: session_id.to_string(),
            policy,
            receipts,
            counts,
            blocked,
            block_reason,
            truncated,
        })
    }

    pub async fn reconcile_effect(
        &self,
        params: EffectReconcileParams,
    ) -> Result<EffectReconcileResult> {
        let receipt = self
            .get_effect(&params.effect_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("effect receipt not found: {}", params.effect_id))?;
        if let Some(session_id) = params.session_id.as_deref()
            && session_id != receipt.session_id
        {
            anyhow::bail!("effect receipt belongs to a different session");
        }
        let session_store = self.store.clone();
        let session_id = receipt.session_id.clone();
        let session = tokio::task::spawn_blocking(move || session_store.get_session(&session_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", receipt.session_id))?;
        if matches!(session.status.as_str(), "imported" | "landed") {
            anyhow::bail!(
                "session {} is {} and its effect receipts are read-only evidence",
                session.id,
                session.status
            );
        }
        let evidence = crate::effect::reconcile_evidence(params.decision, &params.evidence)?;
        let store = self.store.clone();
        let effect_id = params.effect_id.clone();
        let decision = params.decision;
        let operator = params.operator.clone();
        let (stored, events, changed) = tokio::task::spawn_blocking(move || {
            store.reconcile_effect(&effect_id, decision, &evidence, operator.as_deref())
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(EffectReconcileResult {
            receipt: stored,
            changed,
        })
    }

    /// Sweep stale intents across every project with an enabled effect policy.
    /// This is the daemon-restart path: an `intent` that outlived the grace
    /// period becomes `unknown`, never a silent retry.
    pub async fn sweep_stale_effect_intents(&self, limit: u32) -> Result<usize> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        let mut total = 0usize;
        let now = time::OffsetDateTime::now_utc();
        for project in projects {
            let policy = self.get_effect_policy(&project.id).await?;
            if !policy.enabled {
                continue;
            }
            let store = self.store.clone();
            let project_id = project.id.clone();
            let grace = policy.stale_intent_grace_secs;
            let events = tokio::task::spawn_blocking(move || {
                store.sweep_stale_effect_intents(&project_id, grace, limit, &now)
            })
            .await??;
            total = total.saturating_add(events.len());
            for event in events {
                let _ = self.events.send(event);
            }
        }
        Ok(total)
    }

    /// Derive and persist a contract from trusted inputs. The same inputs
    /// return the existing row unchanged; a changed plan/policy produces a new
    /// contract and a new `contract.derived` event.
    pub async fn derive_task_contract(
        &self,
        input: crate::contract::ContractDerivation,
    ) -> Result<crate::contract::TaskContract> {
        let mut contract =
            crate::contract::derive_contract(input).map_err(|error| anyhow::anyhow!("{error}"))?;
        let store = self.store.clone();
        let session_id = contract.session_id.clone();
        let task_id = contract.task_id.clone();
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            let task_id = task_id.clone();
            move || store.latest_contract(&session_id, task_id.as_deref())
        })
        .await??;
        if let Some(row) = existing
            && let Ok(existing) =
                serde_json::from_str::<crate::contract::TaskContract>(&row.contract)
            && existing.hash == contract.hash
            && existing.verify_hash()
        {
            return Ok(existing);
        }
        contract.created_at = vp_protocol::now_rfc3339();
        let encoded = serde_json::to_string(&contract)?;
        let row = StoredContractRow {
            contract_id: contract.contract_id.clone(),
            project_id: contract.project_id.clone(),
            session_id: contract.session_id.clone(),
            task_id: contract.task_id.clone(),
            plan_revision: contract.plan_revision,
            role: contract.role.clone(),
            hash: contract.hash.clone(),
            source_hash: contract.source_hash.clone(),
            contract: encoded,
            created_at: contract.created_at.clone(),
        };
        // The audit event is written first. Replay/repair recreates the
        // projection if the process dies between the two writes.
        self.append_event(
            &contract.session_id,
            "contract.derived",
            serde_json::json!({
                "contract": contract,
                "created_at": contract.created_at,
            }),
        )
        .await?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.insert_contract(&row)).await??;
        Ok(contract)
    }

    pub async fn latest_task_contract(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<Option<crate::contract::TaskContract>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.map(str::to_string);
        let row = tokio::task::spawn_blocking(move || {
            store.latest_contract(&session_id, task_id.as_deref())
        })
        .await??;
        let Some(row) = row else {
            return Ok(None);
        };
        let contract = serde_json::from_str::<crate::contract::TaskContract>(&row.contract)
            .context("stored task contract is malformed")?;
        if !contract.verify_hash() {
            anyhow::bail!("stored task contract hash does not match its body");
        }
        Ok(Some(contract))
    }

    pub async fn contract_usage(
        &self,
        contract_id: &str,
    ) -> Result<crate::contract::MutationUsage> {
        let store = self.store.clone();
        let contract_id = contract_id.to_string();
        let row =
            tokio::task::spawn_blocking(move || store.get_contract_usage(&contract_id)).await??;
        Ok(crate::contract::MutationUsage {
            writes: row.writes,
            bytes: row.bytes,
            commits: row.commits,
        })
    }

    /// Append the bounded `intent.decision` audit row. No argument values,
    /// prompt text, URLs, paths, or secrets are copied into the payload.
    pub async fn record_intent_decision(
        &self,
        contract: &crate::contract::TaskContract,
        decision: &crate::contract::ContractDecision,
        spec: &crate::contract::ContractCallSpec<'_>,
    ) -> Result<()> {
        let payload = decision.payload(contract, spec);
        self.append_event(&contract.session_id, "intent.decision", payload)
            .await?;
        Ok(())
    }

    /// Record the post-approval mutation delta and update the usage
    /// projection. The event is appended before the projection update.
    pub async fn record_intent_mutation(
        &self,
        contract: &crate::contract::TaskContract,
        delta: crate::contract::MutationDelta,
    ) -> Result<crate::contract::MutationUsage> {
        if delta.is_zero() {
            return self.contract_usage(&contract.contract_id).await;
        }
        self.append_event(
            &contract.session_id,
            "intent.mutation",
            serde_json::json!({
                "contract_id": contract.contract_id,
                "contract_hash": contract.hash,
                "task_id": contract.task_id,
                "writes": delta.writes,
                "bytes": delta.bytes,
                "commits": delta.commits,
            }),
        )
        .await?;
        let store = self.store.clone();
        let contract_id = contract.contract_id.clone();
        let project_id = contract.project_id.clone();
        let session_id = contract.session_id.clone();
        let task_id = contract.task_id.clone();
        let row = tokio::task::spawn_blocking(move || {
            store.add_contract_usage(
                &contract_id,
                &project_id,
                &session_id,
                task_id.as_deref(),
                delta.writes,
                delta.bytes,
                delta.commits,
            )
        })
        .await??;
        Ok(crate::contract::MutationUsage {
            writes: row.writes,
            bytes: row.bytes,
            commits: row.commits,
        })
    }

    pub async fn session_contract_status(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<SessionContractResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let policy = self.get_contract_policy(&session.project_id).await?;
        let contract = self.latest_task_contract(session_id, task_id).await?;
        let usage = match &contract {
            Some(contract) => self.contract_usage(&contract.contract_id).await?,
            None => crate::contract::MutationUsage::default(),
        };
        let contract = contract.map(|contract| contract.metadata(policy.mode, usage));
        let decisions = self
            .recent_events_of_kind(session_id, "intent.decision", 20)
            .await?
            .into_iter()
            .filter(|event| {
                task_id.is_none_or(|task| {
                    event
                        .payload
                        .get("task_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(task)
                })
            })
            .map(|event| IntentDecisionRecord {
                seq: event
                    .payload
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(event.seq),
                at: event
                    .payload
                    .get("at")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&event.created_at)
                    .to_string(),
                contract_hash: event
                    .payload
                    .get("contract_hash")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool: event
                    .payload
                    .get("tool")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                task_id: event
                    .payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                role: event
                    .payload
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                rule: event
                    .payload
                    .get("rule")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                decision: event
                    .payload
                    .get("decision")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                risk: event
                    .payload
                    .get("risk")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                reason: event
                    .payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();
        Ok(SessionContractResult {
            session_id: session.id,
            project_id: session.project_id,
            contract,
            policy,
            recent_decisions: decisions,
        })
    }

    // ---------------------------------------------------------------------
    // MCP tool schema pins
    // ---------------------------------------------------------------------
}
