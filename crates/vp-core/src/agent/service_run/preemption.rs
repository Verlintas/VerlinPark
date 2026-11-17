use super::*;

impl AgentService {
    pub fn cancel(&self, session_id: &str) -> bool {
        if let Ok(runs) = self.runs.lock()
            && let Some(control) = runs.get(session_id)
        {
            if !control.cancellable {
                return false;
            }
            control.cancel();
            return true;
        }
        false
    }

    /// Hard-preempt one active run. This aborts in-flight provider and tool
    /// work instead of waiting for the next cooperative boundary. It is
    /// operator-only: no scheduler or agent can call it, and unknown runs fail
    /// closed without creating a preemption record.
    pub async fn preempt(
        &self,
        session_id: &str,
        reason: Option<&str>,
    ) -> Result<SessionPreemptResult> {
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(|reason| reason.chars().take(500).collect::<String>())
            .unwrap_or_else(|| "operator requested preemption".to_string());
        let control = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?
            .get(session_id)
            .cloned();
        let Some(control) = control else {
            return Ok(SessionPreemptResult {
                session_id: session_id.to_string(),
                preempted: false,
                reason: String::new(),
                detail: "no active run holds a preempt token".to_string(),
            });
        };
        if !control.cancellable {
            return Ok(SessionPreemptResult {
                session_id: session_id.to_string(),
                preempted: false,
                reason: String::new(),
                detail: "this operation is not preemptible".to_string(),
            });
        }
        if control.is_cancelled() {
            return Ok(SessionPreemptResult {
                session_id: session_id.to_string(),
                preempted: false,
                reason: String::new(),
                detail: "the run is already cancelling".to_string(),
            });
        }
        if control.is_preempted() {
            return Ok(SessionPreemptResult {
                session_id: session_id.to_string(),
                preempted: false,
                reason: control
                    .preempt_report()
                    .map(|snapshot| snapshot.reason.as_str().to_string())
                    .unwrap_or_default(),
                detail: "the run is already preempted".to_string(),
            });
        }
        self.engine
            .append_event(
                session_id,
                "session.preempt_requested",
                serde_json::json!({
                    "reason": reason,
                    "phase": control.preempt.phase().as_str(),
                }),
            )
            .await?;
        let snapshot = control.request_preempt();
        Ok(SessionPreemptResult {
            session_id: session_id.to_string(),
            preempted: true,
            reason: snapshot.reason.as_str().to_string(),
            detail: format!(
                "preemption signalled during {}; the run will stop after bounded cleanup",
                snapshot.phase.as_str()
            ),
        })
    }

    /// Session-scoped preemption history plus the live in-memory state. The
    /// history is derived from events; `active` is the only volatile part.
    pub async fn preemption(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<SessionPreemptionResult> {
        let mut result = self.engine.session_preemption(session_id, limit).await?;
        result.active = self
            .runs
            .lock()
            .ok()
            .and_then(|runs| runs.get(session_id).cloned())
            .is_some_and(|control| control.is_preempted());
        Ok(result)
    }

    pub async fn pause(&self, session_id: &str) -> Result<bool> {
        let control = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?
            .get(session_id)
            .cloned();
        let Some(control) = control else {
            return Ok(false);
        };
        if control.manual {
            // Manual context compaction is a short foreground operation and
            // has no resumable run loop; pausing it would leave the session
            // stuck in `paused` after the compaction finishes.
            return Ok(false);
        }
        if control.is_paused() {
            return Ok(true);
        }
        let finalized = control.finalized.lock().await;
        if finalized.is_some() {
            return Ok(false);
        }
        control.pause();
        self.engine
            .update_session_status(session_id, "paused")
            .await?;
        self.engine
            .append_event(session_id, "agent.paused", serde_json::json!({}))
            .await?;
        drop(finalized);
        Ok(true)
    }

    pub async fn resume(&self, session_id: &str) -> Result<bool> {
        let control = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?
            .get(session_id)
            .cloned();
        let Some(control) = control else {
            return Ok(false);
        };
        if !control.is_paused() {
            return Ok(true);
        }
        let finalized = control.finalized.lock().await;
        if finalized.is_some() {
            return Ok(false);
        }
        control.resume();
        self.engine
            .update_session_status(session_id, "running")
            .await?;
        self.engine
            .append_event(session_id, "agent.resumed", serde_json::json!({}))
            .await?;
        drop(finalized);
        Ok(true)
    }
}
