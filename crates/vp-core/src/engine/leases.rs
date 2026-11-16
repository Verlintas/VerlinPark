use super::*;

/// A held cross-process session lease. Dropping without calling `release`
/// aborts the heartbeat and performs a best-effort fenced release; a process
/// abort still leaves the row to expire after the TTL.
pub struct SessionLease {
    store: Store,
    events: broadcast::Sender<Event>,
    pub lease_key: String,
    pub owner_id: String,
    pub owner_instance: String,
    pub session_id: String,
    pub fencing_token: u64,
    token: RuntimeLeaseToken,
    lost: Arc<AtomicBool>,
    released: bool,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl SessionLease {
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    fn abort_heartbeat(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }

    fn release_sync(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.abort_heartbeat();
        let _ = self
            .store
            .release_runtime_lease(&self.lease_key, &self.owner_id, &self.token);
    }

    pub async fn release(mut self) {
        self.abort_heartbeat();
        if self.released {
            return;
        }
        let failed = {
            let store = self.store.clone();
            let key = self.lease_key.clone();
            let owner = self.owner_id.clone();
            let token = self.token.clone();
            tokio::task::spawn_blocking(move || {
                store.release_runtime_lease(&key, &owner, &token).is_err()
            })
            .await
            .unwrap_or(true)
        };
        self.released = true;
        if failed {
            tracing::warn!(
                lease_key = %self.lease_key,
                owner_id = %self.owner_id,
                "session lease was already lost before explicit release"
            );
        }
        if let Ok(event) = self.store.append_event(
            &self.session_id,
            "session.lease.released",
            serde_json::json!({
                "lease_key": self.lease_key,
                "owner_id": self.owner_id,
                "owner_instance": self.owner_instance,
                "fencing_token": self.fencing_token,
                "lease_lost": failed,
            }),
        ) {
            let _ = self.events.send(event);
        }
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.release_sync();
    }
}

/// A generic daemon-wide lease used for integration and other serialized
/// operations. It carries the unpredictable acquisition token and monotonic
/// generation. Renewal is fenced; a failed heartbeat marks the guard lost so
/// the critical section can stop before its next external effect.
pub struct RuntimeLeaseGuard {
    store: Store,
    pub lease_key: String,
    pub kind: String,
    pub owner_id: String,
    pub owner_instance: String,
    pub fencing_token: u64,
    token: RuntimeLeaseToken,
    lost: Arc<AtomicBool>,
    lost_notify: Arc<tokio::sync::Notify>,
    released: bool,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl RuntimeLeaseGuard {
    fn from_acquisition(store: Store, acquisition: RuntimeLeaseAcquisition) -> Self {
        let (lease, token) = acquisition.into_parts();
        Self {
            store,
            lease_key: lease.lease_key,
            kind: lease.kind,
            owner_id: lease.owner_id,
            owner_instance: lease.owner_instance,
            fencing_token: lease.fencing_token,
            token,
            lost: Arc::new(AtomicBool::new(false)),
            lost_notify: Arc::new(tokio::sync::Notify::new()),
            released: false,
            heartbeat: None,
        }
    }

    pub fn generation(&self) -> u64 {
        self.fencing_token
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    pub(crate) fn token(&self) -> &RuntimeLeaseToken {
        &self.token
    }

    /// Cheap local check for code that cannot await. The authoritative check
    /// is [`Self::assert_held`] at the effect boundary.
    pub fn assert_not_lost(&self) -> Result<()> {
        if self.is_lost() {
            anyhow::bail!(
                "runtime lease {} generation {} was lost",
                self.lease_key,
                self.fencing_token
            );
        }
        Ok(())
    }

    /// Query the database and fail if the row is missing, expired, released,
    /// or owned by a newer token. This is the fence the Integrator uses before
    /// mutating the session branch.
    pub async fn assert_held(&self) -> Result<vp_store::store::RuntimeLease> {
        self.assert_not_lost()?;
        let store = self.store.clone();
        let key = self.lease_key.clone();
        let owner = self.owner_id.clone();
        let token = self.token.clone();
        let result =
            tokio::task::spawn_blocking(move || store.assert_runtime_lease(&key, &owner, &token))
                .await?;
        match result {
            Ok(lease) => Ok(lease),
            Err(error) => {
                self.lost.store(true, Ordering::SeqCst);
                self.lost_notify.notify_waiters();
                Err(anyhow!(error))
            }
        }
    }

    /// Await the moment this guard loses ownership. The future returns
    /// immediately when the flag is already set. Timers and cancellation are
    /// the caller's responsibility; this is a notification, not a queue.
    pub async fn lost_signal(&self) {
        if self.is_lost() {
            return;
        }
        self.lost_notify.notified().await;
    }

    fn abort_heartbeat(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }

    fn release_sync(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.abort_heartbeat();
        let _ = self
            .store
            .release_runtime_lease(&self.lease_key, &self.owner_id, &self.token);
    }

    pub async fn release(mut self) {
        self.abort_heartbeat();
        if self.released {
            return;
        }
        let failed = {
            let store = self.store.clone();
            let key = self.lease_key.clone();
            let owner = self.owner_id.clone();
            let token = self.token.clone();
            tokio::task::spawn_blocking(move || {
                store.release_runtime_lease(&key, &owner, &token).is_err()
            })
            .await
            .unwrap_or(true)
        };
        self.released = true;
        if failed {
            tracing::warn!(
                lease_key = %self.lease_key,
                owner_id = %self.owner_id,
                generation = self.fencing_token,
                "runtime lease was already lost before explicit release"
            );
        }
    }
}

impl Drop for RuntimeLeaseGuard {
    fn drop(&mut self) {
        // Best effort: normal callers await `release`, but a dropped guard
        // (including a panic unwind) should not pin the key until TTL.
        self.release_sync();
    }
}

/// A held daemon-wide lease over an interactive surface such as the browser.
/// It has its own SQLite row and audit events; it is not a worktree write
/// scope. Dropping the guard stops the heartbeat and leaves the row to expire
/// after the TTL, which is the crash path. Normal callers release explicitly.
pub struct ComputerLeaseGuard {
    store: Store,
    events: broadcast::Sender<Event>,
    pub lease: vp_store::store::ComputerLease,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl ComputerLeaseGuard {
    pub fn lease_id(&self) -> &str {
        &self.lease.lease_id
    }

    pub fn surface(&self) -> &str {
        &self.lease.surface
    }

    pub async fn release(mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        let store = self.store.clone();
        let lease_id = self.lease.lease_id.clone();
        let owner = self.lease.owner_id.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.release_computer_lease(&lease_id, Some(&owner))
        })
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
    }
}

impl Drop for ComputerLeaseGuard {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }
}

impl Engine {
    /// Acquire a named runtime lease (for example `integration:{session}`).
    /// The returned guard carries the random acquisition token and monotonic
    /// generation; all later renew/release/assert operations are fenced.
    pub async fn acquire_runtime_lease(
        &self,
        kind: &str,
        resource: &str,
        ttl: std::time::Duration,
    ) -> Result<RuntimeLeaseGuard> {
        let ttl_secs = ttl.as_secs().clamp(10, 86_400);
        let lease_key = format!("{kind}:{resource}");
        let owner_id = self.runtime_id.clone();
        let owner_instance = self.owner_instance();
        let store = self.store.clone();
        let key = lease_key.clone();
        let owner = owner_id.clone();
        let instance = owner_instance.clone();
        let kind_owned = kind.to_string();
        let resource_owned = resource.to_string();
        let acquisition = tokio::task::spawn_blocking(move || {
            store.acquire_runtime_lease(
                &key,
                &owner,
                &instance,
                &kind_owned,
                Some(&resource_owned),
                ttl_secs,
            )
        })
        .await??;
        let mut guard = RuntimeLeaseGuard::from_acquisition(self.store.clone(), acquisition);
        let heartbeat_store = self.store.clone();
        let heartbeat_key = guard.lease_key.clone();
        let heartbeat_owner = guard.owner_id.clone();
        let heartbeat_token = guard.token.clone();
        let heartbeat_lost = guard.lost.clone();
        let heartbeat_notify = guard.lost_notify.clone();
        let heartbeat_secs = (ttl_secs / 3).max(5);
        let heartbeat = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let store = heartbeat_store.clone();
                let key = heartbeat_key.clone();
                let owner = heartbeat_owner.clone();
                let token = heartbeat_token.clone();
                let renewed = tokio::task::spawn_blocking(move || {
                    store
                        .renew_runtime_lease(&key, &owner, &token, ttl_secs)
                        .is_ok()
                })
                .await
                .unwrap_or(false);
                if !renewed {
                    heartbeat_lost.store(true, Ordering::SeqCst);
                    heartbeat_notify.notify_waiters();
                    break;
                }
            }
        });
        guard.heartbeat = Some(heartbeat);
        Ok(guard)
    }

    /// Acquire the cross-process lease for a session run. The database row is
    /// the authority; the in-process run map is only a fast path. A lease
    /// heartbeat keeps a long run from expiring, and an expired lease from a
    /// crashed daemon can be taken over after the TTL.
    pub async fn acquire_session_lease(
        &self,
        session_id: &str,
        ttl: std::time::Duration,
    ) -> Result<SessionLease> {
        let ttl_secs = ttl.as_secs().clamp(10, 86_400);
        let lease_key = format!("session:{session_id}");
        let owner_id = self.runtime_id.clone();
        let owner_instance = self.owner_instance();
        let store = self.store.clone();
        let key = lease_key.clone();
        let owner = owner_id.clone();
        let instance = owner_instance.clone();
        let session = session_id.to_string();
        let acquisition = match tokio::task::spawn_blocking(move || {
            store.acquire_runtime_lease(
                &key,
                &owner,
                &instance,
                "session",
                Some(&session),
                ttl_secs,
            )
        })
        .await?
        {
            Ok(acquisition) => acquisition,
            Err(vp_store::store::RuntimeLeaseError::Busy {
                holder: Some(holder),
            }) => {
                anyhow::bail!(
                    "session {session_id} is already running (held by {} on {} until {})",
                    holder.owner_id,
                    holder.owner_instance,
                    holder.expires_at
                );
            }
            Err(vp_store::store::RuntimeLeaseError::Busy { holder: None }) => {
                anyhow::bail!("session {session_id} is already running");
            }
            Err(error) => return Err(anyhow!(error)),
        };
        let fencing_token = acquisition.fencing_token();
        let token = acquisition.token().clone();
        let event = self
            .append_event(
                session_id,
                "session.lease.acquired",
                serde_json::json!({
                    "lease_key": lease_key,
                    "owner_id": owner_id,
                    "owner_instance": owner_instance,
                    "fencing_token": fencing_token,
                    "ttl_secs": ttl_secs,
                }),
            )
            .await;
        if let Err(error) = event {
            let release_store = self.store.clone();
            let release_key = lease_key.clone();
            let release_owner = owner_id.clone();
            let release_token = token.clone();
            let _ = tokio::task::spawn_blocking(move || {
                release_store.release_runtime_lease(&release_key, &release_owner, &release_token)
            })
            .await;
            return Err(error);
        }
        let lost = Arc::new(AtomicBool::new(false));
        let heartbeat_store = self.store.clone();
        let heartbeat_key = lease_key.clone();
        let heartbeat_owner = owner_id.clone();
        let heartbeat_token = token.clone();
        let heartbeat_lost = lost.clone();
        let heartbeat_secs = (ttl_secs / 3).max(5);
        let heartbeat = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let store = heartbeat_store.clone();
                let key = heartbeat_key.clone();
                let owner = heartbeat_owner.clone();
                let token = heartbeat_token.clone();
                let renewed = tokio::task::spawn_blocking(move || {
                    store
                        .renew_runtime_lease(&key, &owner, &token, ttl_secs)
                        .is_ok()
                })
                .await
                .unwrap_or(false);
                if !renewed {
                    heartbeat_lost.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
        Ok(SessionLease {
            store: self.store.clone(),
            events: self.events.clone(),
            lease_key,
            owner_id,
            owner_instance,
            session_id: session_id.to_string(),
            fencing_token,
            token,
            lost,
            released: false,
            heartbeat: Some(heartbeat),
        })
    }

    /// Acquire a runtime lease at an explicit clock for deterministic tests and
    /// recovery drills. Production callers use `acquire_runtime_lease`.
    #[cfg(test)]
    pub(crate) async fn acquire_runtime_lease_at_for_test(
        &self,
        kind: &str,
        resource: &str,
        ttl_secs: u64,
        now: time::OffsetDateTime,
    ) -> Result<RuntimeLeaseGuard> {
        let lease_key = format!("{kind}:{resource}");
        let store = self.store.clone();
        let key = lease_key.clone();
        let owner = self.runtime_id.clone();
        let instance = self.owner_instance();
        let kind_owned = kind.to_string();
        let resource_owned = resource.to_string();
        let acquisition = tokio::task::spawn_blocking(move || {
            store.acquire_runtime_lease_at(
                &key,
                &owner,
                &instance,
                &kind_owned,
                Some(&resource_owned),
                ttl_secs,
                now,
            )
        })
        .await??;
        Ok(RuntimeLeaseGuard::from_acquisition(
            self.store.clone(),
            acquisition,
        ))
    }

    pub async fn orchestration_candidates(&self, limit: u32) -> Result<Vec<String>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.orchestration_candidate_sessions(limit)).await?
    }

    pub async fn list_runtime_leases(&self) -> Result<Vec<vp_store::store::RuntimeLease>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_runtime_leases()).await?
    }

    pub async fn sweep_runtime_leases(&self) -> Result<usize> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.clear_expired_runtime_leases()).await?
    }

    /// Token-free lease history for operators. The audit is operational state,
    /// not a replay projection.
    pub async fn runtime_lease_audit(
        &self,
        lease_key: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<vp_store::store::RuntimeLeaseAuditEntry>> {
        let store = self.store.clone();
        let lease_key = lease_key.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.list_runtime_lease_audit(lease_key.as_deref(), limit)
        })
        .await?
    }

    /// Bound the operational lease audit table. Defaults to 30 days and runs
    /// from the daemon's existing lease sweep loop.
    pub async fn prune_runtime_lease_audit(&self, retention_days: u64) -> Result<usize> {
        let days = retention_days.clamp(1, 365);
        let cutoff = time::OffsetDateTime::now_utc() - time::Duration::days(days as i64);
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.prune_runtime_lease_audit(cutoff)).await?
    }

    /// Acquire the daemon-wide browser/GUI surface lease. The SQLite row is
    /// the authority; an unexpired lease on the same surface fails fast with
    /// the holder instead of queueing. The guard heartbeats at TTL/3.
    pub async fn acquire_computer_lease(
        &self,
        surface: &str,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        ttl: std::time::Duration,
    ) -> Result<ComputerLeaseGuard> {
        let ttl_secs = ttl.as_secs().clamp(10, 3_600);
        let store = self.store.clone();
        let owner_id = self.runtime_id.clone();
        let surface_owned = surface.to_string();
        let session_owned = session_id.to_string();
        let agent_owned = agent_id.to_string();
        let agent_name_owned = agent_name.to_string();
        let attempt = tokio::task::spawn_blocking(move || {
            store.acquire_computer_lease(
                &surface_owned,
                &owner_id,
                &session_owned,
                &agent_owned,
                &agent_name_owned,
                ttl_secs,
            )
        })
        .await??;
        match attempt {
            vp_store::store::ComputerLeaseAttempt::Acquired { lease, events } => {
                for event in events {
                    let _ = self.events.send(event);
                }
                let heartbeat_store = self.store.clone();
                let heartbeat_owner = lease.owner_id.clone();
                let heartbeat_lease = lease.lease_id.clone();
                let heartbeat_secs = (ttl_secs / 3).max(5);
                let heartbeat = tokio::spawn(async move {
                    let mut ticker =
                        tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    ticker.tick().await;
                    loop {
                        ticker.tick().await;
                        let store = heartbeat_store.clone();
                        let owner = heartbeat_owner.clone();
                        let lease = heartbeat_lease.clone();
                        let renewed = tokio::task::spawn_blocking(move || {
                            store.renew_computer_lease(&lease, &owner, ttl_secs)
                        })
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or(false);
                        if !renewed {
                            break;
                        }
                    }
                });
                Ok(ComputerLeaseGuard {
                    store: self.store.clone(),
                    events: self.events.clone(),
                    lease,
                    heartbeat: Some(heartbeat),
                })
            }
            vp_store::store::ComputerLeaseAttempt::Busy { holder, events } => {
                for event in events {
                    let _ = self.events.send(event);
                }
                let detail = holder
                    .map(|holder| {
                        format!(
                            "held by session {} agent {} until {}",
                            holder.session_id, holder.agent_name, holder.expires_at
                        )
                    })
                    .unwrap_or_else(|| "held by another runtime".to_string());
                anyhow::bail!("{surface} computer surface is busy ({detail})")
            }
        }
    }

    /// Operator release of a stuck surface lease. Normal agents release their
    /// guard, which also enforces the owner id.
    pub async fn release_computer_lease(&self, lease_id: &str) -> Result<bool> {
        let store = self.store.clone();
        let lease_id = lease_id.to_string();
        let event =
            tokio::task::spawn_blocking(move || store.release_computer_lease(&lease_id, None))
                .await??;
        if let Some(event) = event {
            let _ = self.events.send(event);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn list_computer_leases(&self) -> Result<Vec<vp_store::store::ComputerLease>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_computer_leases()).await?
    }

    pub async fn computer_lease_active(&self, lease_id: &str) -> Result<bool> {
        let store = self.store.clone();
        let lease_id = lease_id.to_string();
        let lease = tokio::task::spawn_blocking(move || store.computer_lease(&lease_id)).await??;
        Ok(lease.is_some())
    }

    pub async fn sweep_computer_leases(&self) -> Result<usize> {
        let store = self.store.clone();
        let (removed, events) =
            tokio::task::spawn_blocking(move || store.clear_expired_computer_leases()).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(removed)
    }

    /// Retire legacy `browser`/`computer` rows at daemon startup. Helper
    /// processes are daemon children, so no legacy helper survives a restart.
    pub async fn retire_legacy_gui_leases(&self) -> Result<usize> {
        let store = self.store.clone();
        let (removed, events) = tokio::task::spawn_blocking(move || {
            store.clear_legacy_gui_leases("legacy GUI surface retired at daemon startup")
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        if removed > 0 {
            tracing::info!(removed, "retired legacy browser/computer GUI leases");
        }
        Ok(removed)
    }

    pub async fn gui_lease_holder(&self) -> Result<Option<vp_store::store::ComputerLease>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.gui_lease_holder()).await?
    }
}
