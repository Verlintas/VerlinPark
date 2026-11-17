use super::*;

impl AgentService {
    pub(in crate::agent) fn scaling_base_caps(&self) -> (usize, usize) {
        self.config
            .read()
            .ok()
            .and_then(|config| {
                config
                    .as_ref()
                    .map(|config| (config.max_workers, config.max_labour))
            })
            .unwrap_or((crate::scaling::HARD_WORKERS, crate::scaling::HARD_LABOUR))
    }

    pub(in crate::agent) fn capacity_snapshot(&self) -> CapacityInfo {
        self.capacity.snapshot()
    }

    /// Evaluate one project/session without persisting or applying anything.
    pub async fn scaling_preview(
        &self,
        session_id: &str,
    ) -> Result<vp_protocol::scaling::ScalingPreviewResult> {
        let (workers, labour) = self.scaling_base_caps();
        let capacity = self.capacity_snapshot();
        crate::scaling::preview(&self.engine, session_id, &capacity, workers, labour).await
    }

    /// Read-only scaling status. It reports durable targets and live agent
    /// counts; it never evaluates or applies a decision.
    pub async fn scaling_status(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<vp_protocol::scaling::ScalingStatusResult> {
        let (workers, labour) = self.scaling_base_caps();
        let engine = self.engine.clone();
        let capacity = self.capacity_snapshot();
        let project_id = project_id.map(ToOwned::to_owned);
        let session_id = session_id.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            crate::scaling::status(
                &engine,
                project_id.as_deref(),
                session_id.as_deref(),
                limit,
                &capacity,
                workers,
                labour,
            )
        })
        .await?
    }

    /// One deterministic controller pass over every enabled project. It only
    /// moves Worker/Labour slot ceilings; all admission, approval, write-scope,
    /// Verifier, and merge gates remain in the existing run paths.
    pub async fn scaling_tick(&self) -> Result<vp_protocol::scaling::ScalingTickResult> {
        let (workers, labour) = self.scaling_base_caps();
        let capacity = self.capacity_snapshot();
        crate::scaling::run_tick(&self.engine, &capacity, workers, labour, None, None, true).await
    }

    pub fn start_scaling_controller(&self) {
        if self.scaling_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let seconds = env::var("VERLINPARK_SCALING_TICK_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30);
        if seconds == 0 {
            return;
        }
        let seconds = seconds.clamp(5, 3_600);
        let service = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(seconds));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match service.scaling_tick().await {
                    Ok(result) if result.evaluated > 0 => {
                        tracing::debug!(
                            projects = result.projects,
                            sessions = result.sessions,
                            evaluated = result.evaluated,
                            applied = result.applied,
                            expanded = result.expanded,
                            scaled_down = result.scaled_down,
                            "deterministic scaling tick"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "deterministic scaling tick failed");
                    }
                }
            }
        });
    }
}
