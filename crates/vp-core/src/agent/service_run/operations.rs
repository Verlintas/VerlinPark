use super::*;

impl AgentService {
    pub(in crate::agent) fn begin_manual_operation(
        &self,
        session_id: &str,
    ) -> Result<Arc<RunControl>> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        if runs.contains_key(session_id) {
            anyhow::bail!("session is already running");
        }
        let control = Arc::new(RunControl::new_manual());
        runs.insert(session_id.to_string(), control.clone());
        Ok(control)
    }

    /// A resumed or normal long run is cancellable and pausable. Foreground
    /// operations use the non-cancellable variants above.
    pub(in crate::agent) fn begin_resumable_run(
        &self,
        session_id: &str,
        time_budget_secs: Option<u64>,
        policy: vp_protocol::types::PreemptionPolicy,
    ) -> Result<Arc<RunControl>> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        if runs.contains_key(session_id) {
            anyhow::bail!("session is already running");
        }
        let control = Arc::new(RunControl::new());
        control.arm_preemption(time_budget_secs, policy);
        runs.insert(session_id.to_string(), control.clone());
        Ok(control)
    }

    pub(in crate::agent) fn begin_operation(&self, session_id: &str) -> Result<()> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        if runs.contains_key(session_id) {
            anyhow::bail!("session is already running");
        }
        runs.insert(
            session_id.to_string(),
            Arc::new(RunControl::new_operation()),
        );
        Ok(())
    }

    pub(in crate::agent) fn end_operation(&self, session_id: &str) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(session_id);
        }
    }

    pub(in crate::agent) fn begin_operations(&self, session_ids: &[&str]) -> Result<()> {
        let mut ids = session_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        for id in &ids {
            if runs.contains_key(*id) {
                anyhow::bail!("session is already running: {id}");
            }
        }
        for id in ids {
            runs.insert(id.to_string(), Arc::new(RunControl::new_operation()));
        }
        Ok(())
    }

    pub(in crate::agent) fn end_operations(&self, session_ids: &[&str]) {
        if let Ok(mut runs) = self.runs.lock() {
            for id in session_ids {
                runs.remove(*id);
            }
        }
    }
}
