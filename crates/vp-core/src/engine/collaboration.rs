use super::*;

impl Engine {
    /// Read-only loopback A2A facade status. Never returns a bearer token.
    pub fn a2a_status(&self) -> vp_protocol::a2a::A2aStatus {
        self.a2a.status()
    }

    /// Update the stored A2A policy while the facade is stopped.
    pub fn a2a_set_policy(
        &self,
        policy: vp_protocol::a2a::A2aPolicy,
    ) -> Result<vp_protocol::a2a::A2aStatus> {
        self.a2a.set_policy(policy)
    }

    /// Read the running Agent Card as JSON. Returns `None` when stopped.
    pub fn a2a_card(&self) -> Option<serde_json::Value> {
        self.a2a.card()
    }

    /// Read the bounded in-memory inbound-task list. This is operational state
    /// and is intentionally not persisted across daemon restarts.
    pub fn a2a_tasks(
        &self,
        limit: u32,
        state: Option<&str>,
    ) -> vp_protocol::a2a::A2aTaskListResult {
        self.a2a.tasks(limit, state)
    }

    /// Start the loopback A2A facade. `approved` is the explicit host-state
    /// gate; the facade is disabled by default and binds loopback only.
    pub async fn a2a_start(
        &self,
        approved: bool,
        policy: Option<vp_protocol::a2a::A2aPolicy>,
    ) -> Result<vp_protocol::a2a::A2aStartResult> {
        self.a2a.start(approved, policy).await
    }

    /// Stop the loopback A2A facade. The one-time token is not retained.
    pub async fn a2a_stop(&self, reason: Option<&str>) -> Result<vp_protocol::a2a::A2aStatus> {
        self.a2a.stop(reason).await
    }

    /// Record one bounded advisory progress round. The ledger is evidence
    /// only: it never changes the plan, task queue, scheduler, budgets, or
    /// merge state.
    pub async fn record_progress_round(
        &self,
        session_id: &str,
        mut round: ProgressRound,
    ) -> Result<ProgressLedgerResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            bail!("imported sessions are read-only archives");
        }
        let plan_revision = u64::from(self.current_plan_revision(session_id).await.unwrap_or(0));
        let store = self.store.clone();
        let id = session_id.to_string();
        let existing = tokio::task::spawn_blocking(move || {
            store.list_progress_rounds(&id, Some(plan_revision), MAX_PROGRESS_ROUNDS)
        })
        .await??;
        let policy = crate::progress_ledger::ProgressPolicy::default();
        let existing_rounds = existing.0;
        let mut ledger =
            crate::progress_ledger::ProgressLedger::try_new(session_id, plan_revision)?;
        ledger.rounds = existing_rounds.clone();
        ledger.assessment = ledger.assess(&policy);
        if round.round_id.trim().is_empty() {
            round.round_id = ledger.next_round_id()?;
        }
        round.agent_id = round
            .agent_id
            .or_else(|| Some("operator".to_string()))
            .map(|value| value.chars().take(200).collect());
        round.agent_name = round
            .agent_name
            .or_else(|| Some("Operator".to_string()))
            .map(|value| value.chars().take(200).collect());
        ledger.push_round(&policy, round)?;
        let stored = ledger
            .latest_round()
            .cloned()
            .ok_or_else(|| anyhow!("progress round was not appended"))?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let (stored, event) = tokio::task::spawn_blocking(move || {
            store.insert_progress_round_with_event(&id, plan_revision, &stored)
        })
        .await??;
        let mut ledger =
            crate::progress_ledger::ProgressLedger::try_new(session_id, plan_revision)?;
        ledger.rounds = existing_rounds;
        ledger.rounds.push(stored);
        let result = ledger.to_result(&policy, false);
        self.broadcast_event(event);
        Ok(result)
    }

    /// Read the newest bounded progress ledger for a session and optional plan
    /// revision. Missing rounds produce the conservative insufficient-evidence
    /// assessment; this never synthesizes progress.
    pub async fn session_progress(
        &self,
        session_id: &str,
        plan_revision: Option<u64>,
        limit: Option<u32>,
    ) -> Result<ProgressLedgerResult> {
        let fallback_revision =
            u64::from(self.current_plan_revision(session_id).await.unwrap_or(0));
        let revision = match plan_revision {
            Some(revision) => revision,
            None => {
                let store = self.store.clone();
                let id = session_id.to_string();
                tokio::task::spawn_blocking(move || store.latest_progress_revision(&id))
                    .await??
                    .unwrap_or(fallback_revision)
            }
        };
        let limit = limit
            .unwrap_or(vp_protocol::progress::DEFAULT_PROGRESS_LIMIT)
            .clamp(1, MAX_PROGRESS_ROUNDS);
        let store = self.store.clone();
        let id = session_id.to_string();
        let (rounds, truncated) = tokio::task::spawn_blocking(move || {
            store.list_progress_rounds(&id, Some(revision), limit)
        })
        .await??;
        let policy = crate::progress_ledger::ProgressPolicy::default();
        let mut ledger = crate::progress_ledger::ProgressLedger::try_new(session_id, revision)?;
        ledger.rounds = rounds;
        Ok(ledger.to_result(&policy, truncated))
    }

    pub async fn request_question(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        question: &str,
        options: Vec<String>,
    ) -> Result<(Question, oneshot::Receiver<String>)> {
        let question = question.trim();
        if question.is_empty() || question.chars().count() > 2_000 {
            anyhow::bail!("question must be 1-2000 characters");
        }
        if options.len() > 8 || options.iter().any(|option| option.chars().count() > 200) {
            anyhow::bail!("at most 8 options of 200 characters are allowed");
        }
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let question = question.to_string();
        let (item, event) = tokio::task::spawn_blocking(move || {
            store.create_question_with_event(
                &session_id_owned,
                &agent_id,
                &agent_name,
                &question,
                &options,
            )
        })
        .await??;
        let (sender, receiver) = oneshot::channel();
        self.question_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("question mutex poisoned"))?
            .insert(item.id.clone(), sender);
        let _ = self.events.send(event);
        Ok((item, receiver))
    }

    pub async fn answer_question(&self, question_id: &str, answer: &str) -> Result<bool> {
        let answer = answer.trim();
        if answer.is_empty() || answer.chars().count() > 4_000 {
            anyhow::bail!("answer must be 1-4000 characters");
        }
        let store = self.store.clone();
        let question_id_owned = question_id.to_string();
        let answer_owned = answer.to_string();
        let answered = tokio::task::spawn_blocking(move || {
            store.answer_question_with_event(&question_id_owned, &answer_owned)
        })
        .await??;
        let Some((question, event)) = answered else {
            return Ok(false);
        };
        if let Ok(mut waiters) = self.question_waiters.lock()
            && let Some(sender) = waiters.remove(&question.id)
        {
            let _ = sender.send(question.answer.unwrap_or_default());
        }
        let _ = self.events.send(event);
        Ok(true)
    }

    pub async fn close_question(
        &self,
        question_id: &str,
        status: &str,
    ) -> Result<Option<Question>> {
        let store = self.store.clone();
        let question_id_owned = question_id.to_string();
        let status_owned = status.to_string();
        let closed = tokio::task::spawn_blocking(move || {
            store.close_question_with_event(&question_id_owned, &status_owned)
        })
        .await??;
        let Some((question, event)) = closed else {
            return Ok(None);
        };
        if let Ok(mut waiters) = self.question_waiters.lock() {
            waiters.remove(&question.id);
        }
        let _ = self.events.send(event);
        Ok(Some(question))
    }

    pub async fn list_questions(&self, session_id: Option<String>) -> Result<QuestionListResult> {
        let store = self.store.clone();
        let questions =
            tokio::task::spawn_blocking(move || store.list_questions(session_id.as_deref()))
                .await??;
        Ok(QuestionListResult { questions })
    }

    pub async fn get_mailbox_policy(&self, project_id: &str) -> Result<MailboxPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_mailbox_policy(&project_id)).await?
    }

    pub async fn set_mailbox_policy(
        &self,
        project_id: &str,
        policy: MailboxPolicy,
    ) -> Result<MailboxPolicy> {
        if policy.max_age_secs.is_some_and(|value| value == 0) {
            anyhow::bail!("max_age_secs must be greater than zero");
        }
        if policy.max_count.is_some_and(|value| value == 0) {
            anyhow::bail!("max_count must be greater than zero");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_mailbox_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    pub(super) async fn auto_gc_mailbox_for_session(&self, session_id: &str) {
        let store = self.store.clone();
        let id = session_id.to_string();
        let project_id = match tokio::task::spawn_blocking(move || store.get_session(&id)).await {
            Ok(Ok(Some(session))) => session.project_id,
            Ok(Ok(None)) | Err(_) | Ok(Err(_)) => return,
        };
        let policy = match self.get_mailbox_policy(&project_id).await {
            Ok(policy) if policy.auto_gc => policy,
            _ => return,
        };
        if let Err(error) = self.gc_mailbox(&project_id, &policy).await {
            tracing::warn!(%error, project_id, "automatic mailbox GC failed");
        }
    }

    pub async fn gc_mailbox(
        &self,
        project_id: &str,
        policy: &MailboxPolicy,
    ) -> Result<MailboxGcResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let messages = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.list_agent_messages_for_project(&project_id)
        })
        .await??;
        let now = time::OffsetDateTime::now_utc();
        let mut remove = Vec::new();
        if let Some(max_age) = policy.max_age_secs {
            for message in &messages {
                if let Ok(created) = time::OffsetDateTime::parse(
                    &message.created_at,
                    &time::format_description::well_known::Rfc3339,
                ) && (now - created).whole_seconds().max(0) as u64 > max_age
                {
                    remove.push(message.id.clone());
                }
            }
        }
        if let Some(max_count) = policy.max_count
            && messages.len() as u64 > max_count
        {
            let keep = messages.len() - max_count as usize;
            remove.extend(messages.iter().take(keep).map(|message| message.id.clone()));
        }
        remove.sort();
        remove.dedup();
        let mut removed = 0usize;
        for id in remove {
            let Some(message) = messages.iter().find(|message| message.id == id) else {
                continue;
            };
            if !policy.delete_unread && message.status == "unread" {
                continue;
            }
            let store = self.store.clone();
            let event =
                tokio::task::spawn_blocking(move || store.delete_agent_message_with_event(&id))
                    .await??;
            if let Some(event) = event {
                let _ = self.events.send(event);
                removed += 1;
            }
        }
        Ok(MailboxGcResult { removed })
    }
}
