//! Fenced cross-process runtime leases.
//!
//! The database row is the authority, not an in-process flag. Every
//! acquisition creates an unpredictable token and increments a monotonic
//! generation for the lease key. Renew, release, and diagnostic assertions are
//! conditional on `(owner, token, unexpired)`; zero rows means the caller lost
//! its critical section and must stop before performing an external effect.
//!
//! Two daemons can share one data directory. SQLite WAL still serializes
//! writers, so `BEGIN IMMEDIATE` makes acquisition a compare-and-set: one
//! process commits the row and every competitor observes a live lease. This is
//! local same-host coordination, not distributed consensus. Clock skew between
//! hosts is out of scope (`docs/adr/0161-cross-process-lease-fencing.md`).

use std::collections::HashSet;

use anyhow::{Context, Result, anyhow};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::Store;
use super::{RUNTIME_LEASE_COLUMNS, RuntimeLease, now_rfc3339};

fn parse_lease_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

/// Maximum accepted TTL for a runtime lease. Session and integration callers
/// currently use 180 and 900 seconds; the hard ceiling only prevents a
/// malformed caller from creating an effectively permanent row.
pub const MAX_RUNTIME_LEASE_TTL_SECS: u64 = 86_400;

const META_DAEMON_IDENTITY: &str = "daemon_instance";
const MAX_LEASE_KEY_CHARS: usize = 200;
const MAX_OWNER_ID_CHARS: usize = 320;
const MAX_OWNER_INSTANCE_CHARS: usize = 160;
const MAX_LEASE_KIND_CHARS: usize = 64;

/// Stable identity for the data directory's daemon installation.
///
/// `instance_id` is persisted in `store_meta` and is shared by every process
/// that opens the same directory. It is *not* a process id: two live daemons
/// distinguish themselves with `owner_instance = instance_id:pid` and with a
/// random nonce in each owner id. Keeping the id stable across restarts lets
/// operators correlate lease history with one installation while still making
/// every process incarnation distinguishable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonIdentity {
    /// Random UUID created with the data directory's first store.
    pub instance_id: String,
    /// Process id that first wrote the identity. Diagnostic only; it is not
    /// treated as proof that the process is alive.
    pub pid: u32,
    /// RFC3339 timestamp when the identity row was created.
    pub started_at: String,
}

impl DaemonIdentity {
    /// Process-scoped instance label stored on lease rows.
    pub fn process_instance(&self) -> String {
        format!("{}:{}", self.instance_id, std::process::id())
    }

    /// Fully qualified owner id for an Engine nonce.
    pub fn owner_id(&self, nonce: &str) -> String {
        format!("{}:{}:{nonce}", self.instance_id, std::process::id())
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct StoredDaemonIdentity {
    instance_id: String,
    pid: u32,
    started_at: String,
}

/// Load the persisted daemon identity or create it exactly once. A
/// concurrent first-open race is resolved with `INSERT OR IGNORE` followed by a
/// read, so both processes observe the same id.
pub(crate) fn load_or_create_daemon_identity(
    conn: &rusqlite::Connection,
) -> Result<DaemonIdentity> {
    if let Some(raw) = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            params![META_DAEMON_IDENTITY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        let stored: StoredDaemonIdentity = serde_json::from_str(&raw)
            .with_context(|| "stored daemon identity is not valid JSON")?;
        validate_identity(&stored.instance_id)?;
        if stored.started_at.trim().is_empty() {
            return Err(anyhow!("stored daemon identity has an empty started_at"));
        }
        return Ok(DaemonIdentity {
            instance_id: stored.instance_id,
            pid: stored.pid,
            started_at: stored.started_at,
        });
    }

    let identity = DaemonIdentity {
        instance_id: Uuid::new_v4().to_string(),
        pid: std::process::id(),
        started_at: now_rfc3339(),
    };
    let stored = StoredDaemonIdentity {
        instance_id: identity.instance_id.clone(),
        pid: identity.pid,
        started_at: identity.started_at.clone(),
    };
    let raw = serde_json::to_string(&stored)?;
    conn.execute(
        "INSERT OR IGNORE INTO store_meta (key, value) VALUES (?1, ?2)",
        params![META_DAEMON_IDENTITY, raw],
    )?;
    // If another process won the insert race, use its row rather than the
    // identity we generated. This keeps every opener on one data dir aligned.
    let raw = conn.query_row(
        "SELECT value FROM store_meta WHERE key = ?1",
        params![META_DAEMON_IDENTITY],
        |row| row.get::<_, String>(0),
    )?;
    let stored: StoredDaemonIdentity =
        serde_json::from_str(&raw).with_context(|| "stored daemon identity is not valid JSON")?;
    validate_identity(&stored.instance_id)?;
    Ok(DaemonIdentity {
        instance_id: stored.instance_id,
        pid: stored.pid,
        started_at: stored.started_at,
    })
}

fn validate_identity(instance_id: &str) -> Result<()> {
    if instance_id.is_empty()
        || instance_id.len() > 128
        || instance_id.trim() != instance_id
        || instance_id.contains(':')
        || instance_id.chars().any(char::is_control)
    {
        return Err(anyhow!("daemon instance id is malformed"));
    }
    Ok(())
}

/// Unpredictable per-acquisition secret. It is never serialized, logged, or
/// returned through RPC. `Debug` redacts it so an accidental `{:?}` of an
/// acquisition or guard cannot leak the token into an audit transcript.
#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeLeaseToken(String);

impl RuntimeLeaseToken {
    pub(crate) fn generate() -> Self {
        // Two independent v4 UUIDs give 244 random bits. No cryptographic RNG
        // dependency beyond `uuid` is needed for an unpredictable fence.
        Self(format!(
            "{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        ))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RuntimeLeaseToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RuntimeLeaseToken(redacted)")
    }
}

/// Result of a successful acquisition. The public lease projection holds no
/// token; only the caller's guard receives that.
#[derive(Debug, Clone)]
pub struct RuntimeLeaseAcquisition {
    pub lease: RuntimeLease,
    token: RuntimeLeaseToken,
}

impl RuntimeLeaseAcquisition {
    pub fn token(&self) -> &RuntimeLeaseToken {
        &self.token
    }

    pub fn fencing_token(&self) -> u64 {
        self.lease.fencing_token
    }

    pub fn into_parts(self) -> (RuntimeLease, RuntimeLeaseToken) {
        (self.lease, self.token)
    }
}

/// Why an operation on a previously held lease was rejected. `Busy` is a
/// normal acquisition race; `Lost` means a guard must stop its critical
/// section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeLeaseError {
    Busy {
        holder: Option<Box<RuntimeLease>>,
    },
    Lost {
        lease_key: String,
        owner_id: String,
        reason: LeaseLostReason,
    },
    Internal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseLostReason {
    Missing,
    Expired,
    Released,
    OwnerMismatch,
    TokenMismatch,
    InvalidLease,
}

impl LeaseLostReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Expired => "expired",
            Self::Released => "released",
            Self::OwnerMismatch => "owner_mismatch",
            Self::TokenMismatch => "token_mismatch",
            Self::InvalidLease => "invalid_lease",
        }
    }
}

impl std::fmt::Display for RuntimeLeaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy {
                holder: Some(holder),
            } => write!(
                formatter,
                "runtime lease {} is busy (held by {} on {} until {})",
                holder.lease_key, holder.owner_id, holder.owner_instance, holder.expires_at
            ),
            Self::Busy { holder: None } => formatter.write_str("runtime lease is busy"),
            Self::Lost {
                lease_key,
                owner_id,
                reason,
            } => write!(
                formatter,
                "runtime lease {lease_key} was lost for {owner_id}: {}",
                reason.as_str()
            ),
            Self::Internal(message) => {
                write!(formatter, "runtime lease operation failed: {message}")
            }
        }
    }
}

impl std::error::Error for RuntimeLeaseError {}

impl From<anyhow::Error> for RuntimeLeaseError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(format!("{error:#}"))
    }
}

impl From<rusqlite::Error> for RuntimeLeaseError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

/// Liveness is derived from the persisted expiry, not from a heartbeat flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLeaseState {
    Live,
    Stale,
    Released,
}

impl RuntimeLeaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Stale => "stale",
            Self::Released => "released",
        }
    }
}

impl RuntimeLease {
    pub fn state_at(&self, now: OffsetDateTime) -> RuntimeLeaseState {
        if self.released_at.is_some() {
            return RuntimeLeaseState::Released;
        }
        match parse_lease_time(&self.expires_at) {
            Some(expires) if expires > now => RuntimeLeaseState::Live,
            _ => RuntimeLeaseState::Stale,
        }
    }

    pub fn state(&self) -> RuntimeLeaseState {
        self.state_at(OffsetDateTime::now_utc())
    }

    pub fn is_live_at(&self, now: OffsetDateTime) -> bool {
        self.state_at(now) == RuntimeLeaseState::Live
    }

    pub fn is_live(&self) -> bool {
        self.is_live_at(OffsetDateTime::now_utc())
    }
}

/// Token-free audit row for lease ownership transitions. The audit is
/// operational state: it is not a session event and is never used to reconstruct
/// a live lease. It exists so an operator can answer "who held this key, which
/// generation, and when did it move?" after tombstones are swept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLeaseAuditEntry {
    pub id: i64,
    pub lease_key: String,
    pub kind: String,
    pub action: String,
    pub owner_id: String,
    pub owner_instance: String,
    pub fencing_token: u64,
    pub detail: Option<String>,
    pub occurred_at: String,
}

fn map_audit_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuntimeLeaseAuditEntry> {
    Ok(RuntimeLeaseAuditEntry {
        id: row.get(0)?,
        lease_key: row.get(1)?,
        kind: row.get(2)?,
        action: row.get(3)?,
        owner_id: row.get(4)?,
        owner_instance: row.get(5)?,
        fencing_token: row.get::<_, i64>(6)?.max(0) as u64,
        detail: row.get(7)?,
        occurred_at: row.get(8)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn insert_audit_tx(
    tx: &rusqlite::Transaction<'_>,
    lease_key: &str,
    kind: &str,
    action: &str,
    owner_id: &str,
    owner_instance: &str,
    fencing_token: u64,
    detail: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO runtime_lease_audit
         (lease_key, kind, action, owner_id, owner_instance, fencing_token, detail, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            lease_key,
            kind,
            action,
            owner_id,
            owner_instance,
            fencing_token as i64,
            detail,
            occurred_at
        ],
    )?;
    Ok(())
}

struct LeaseRow {
    lease: RuntimeLease,
    token: String,
}

fn map_lease_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LeaseRow> {
    // Column order matches `RUNTIME_LEASE_COLUMNS` plus the token appended at
    // the end by `query_lease_row`.
    let lease = RuntimeLease {
        lease_key: row.get(0)?,
        owner_id: row.get(1)?,
        owner_instance: row.get(2)?,
        kind: row.get(3)?,
        session_id: row.get(4)?,
        fencing_token: row.get::<_, i64>(5)?.max(0) as u64,
        acquired_at: row.get(6)?,
        heartbeat_at: row.get(7)?,
        renewed_at: row.get(8)?,
        expires_at: row.get(9)?,
        released_at: row.get(10)?,
    };
    Ok(LeaseRow {
        lease,
        token: row.get(11)?,
    })
}

fn query_lease_row(conn: &rusqlite::Connection, lease_key: &str) -> Result<Option<LeaseRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {RUNTIME_LEASE_COLUMNS}, lease_token
                 FROM runtime_leases WHERE lease_key = ?1"
            ),
            params![lease_key],
            map_lease_row,
        )
        .optional()?)
}

fn query_lease_row_tx(tx: &rusqlite::Transaction<'_>, lease_key: &str) -> Result<Option<LeaseRow>> {
    Ok(tx
        .query_row(
            &format!(
                "SELECT {RUNTIME_LEASE_COLUMNS}, lease_token
                 FROM runtime_leases WHERE lease_key = ?1"
            ),
            params![lease_key],
            map_lease_row,
        )
        .optional()?)
}

fn lost(
    lease_key: &str,
    owner_id: &str,
    reason: LeaseLostReason,
) -> Result<RuntimeLease, RuntimeLeaseError> {
    Err(RuntimeLeaseError::Lost {
        lease_key: lease_key.to_string(),
        owner_id: owner_id.to_string(),
        reason,
    })
}

fn classify_lost(
    row: &LeaseRow,
    owner_id: &str,
    token: &RuntimeLeaseToken,
    now: OffsetDateTime,
) -> LeaseLostReason {
    if row.lease.released_at.is_some() {
        LeaseLostReason::Released
    } else if row.lease.owner_id != owner_id {
        LeaseLostReason::OwnerMismatch
    } else if row.token != token.as_str() {
        LeaseLostReason::TokenMismatch
    } else if !row.lease.is_live_at(now) {
        LeaseLostReason::Expired
    } else {
        // The row looked valid when queried but the conditional update matched
        // zero rows. Treat it as lost rather than guessing.
        LeaseLostReason::InvalidLease
    }
}

fn format_time(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .map_err(|error| anyhow!("formatting lease timestamp: {error}"))
}

fn validate_acquire(
    lease_key: &str,
    owner_id: &str,
    owner_instance: &str,
    kind: &str,
) -> Result<()> {
    if lease_key.is_empty()
        || lease_key.len() > MAX_LEASE_KEY_CHARS
        || lease_key.trim() != lease_key
        || lease_key.chars().any(char::is_control)
    {
        anyhow::bail!("runtime lease key must be 1-{MAX_LEASE_KEY_CHARS} non-padded characters");
    }
    if owner_id.is_empty()
        || owner_id.len() > MAX_OWNER_ID_CHARS
        || owner_id.trim() != owner_id
        || owner_id.chars().any(char::is_control)
    {
        anyhow::bail!("runtime lease owner must be 1-{MAX_OWNER_ID_CHARS} non-padded characters");
    }
    if owner_instance.is_empty()
        || owner_instance.len() > MAX_OWNER_INSTANCE_CHARS
        || owner_instance.trim() != owner_instance
        || owner_instance.chars().any(char::is_control)
    {
        anyhow::bail!(
            "runtime lease owner instance must be 1-{MAX_OWNER_INSTANCE_CHARS} characters"
        );
    }
    if kind.is_empty() || kind.len() > MAX_LEASE_KIND_CHARS || kind.trim() != kind {
        anyhow::bail!("runtime lease kind must be 1-{MAX_LEASE_KIND_CHARS} non-padded characters");
    }
    Ok(())
}

fn bump_fence_tx(
    tx: &rusqlite::Transaction<'_>,
    lease_key: &str,
    existing: u64,
    now_text: &str,
) -> Result<u64> {
    let stored: Option<i64> = tx
        .query_row(
            "SELECT fencing_token FROM runtime_lease_fences WHERE lease_key = ?1",
            params![lease_key],
            |row| row.get(0),
        )
        .optional()?;
    let next = existing
        .max(stored.unwrap_or(0).max(0) as u64)
        .saturating_add(1);
    tx.execute(
        "INSERT INTO runtime_lease_fences (lease_key, fencing_token, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(lease_key) DO UPDATE SET
            fencing_token = excluded.fencing_token,
            updated_at = excluded.updated_at",
        params![lease_key, next as i64, now_text],
    )?;
    Ok(next)
}

impl Store {
    /// Stable daemon identity for this data directory.
    pub fn daemon_identity(&self) -> &DaemonIdentity {
        &self.daemon_identity
    }

    /// Process-scoped instance label used on lease rows.
    pub fn process_owner_instance(&self) -> String {
        self.daemon_identity.process_instance()
    }

    /// Fully qualified owner id for an Engine/process nonce.
    pub fn owner_id_for_nonce(&self, nonce: &str) -> String {
        self.daemon_identity.owner_id(nonce)
    }

    /// Acquire a fenced runtime lease using the current clock.
    pub fn acquire_runtime_lease(
        &self,
        lease_key: &str,
        owner_id: &str,
        owner_instance: &str,
        kind: &str,
        session_id: Option<&str>,
        ttl_secs: u64,
    ) -> Result<RuntimeLeaseAcquisition, RuntimeLeaseError> {
        self.acquire_runtime_lease_at(
            lease_key,
            owner_id,
            owner_instance,
            kind,
            session_id,
            ttl_secs,
            OffsetDateTime::now_utc(),
        )
    }

    /// Deterministic-clock acquisition. A zero TTL is deliberately allowed:
    /// callers use it to force an immediately stale row in tests and recovery
    /// diagnostics. Normal Engine callers clamp TTL to at least ten seconds.
    #[allow(clippy::too_many_arguments)]
    pub fn acquire_runtime_lease_at(
        &self,
        lease_key: &str,
        owner_id: &str,
        owner_instance: &str,
        kind: &str,
        session_id: Option<&str>,
        ttl_secs: u64,
        now: OffsetDateTime,
    ) -> Result<RuntimeLeaseAcquisition, RuntimeLeaseError> {
        validate_acquire(lease_key, owner_id, owner_instance, kind)
            .map_err(|error| RuntimeLeaseError::Internal(format!("{error:#}")))?;
        let now_text = format_time(now).map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let expires_text = format_time(
            now + time::Duration::seconds(ttl_secs.min(MAX_RUNTIME_LEASE_TTL_SECS) as i64),
        )
        .map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let poison = || RuntimeLeaseError::Busy { holder: None };
        let mut conn = self.lock().map_err(|_| poison())?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| poison())?;
        let existing = query_lease_row_tx(&tx, lease_key).map_err(|_| poison())?;
        if let Some(row) = &existing
            && row.lease.released_at.is_none()
            && row.lease.is_live_at(now)
            && row.lease.owner_id != owner_id
        {
            return Err(RuntimeLeaseError::Busy {
                holder: Some(Box::new(row.lease.clone())),
            });
        }
        let existing_fence = existing
            .as_ref()
            .map(|row| row.lease.fencing_token)
            .unwrap_or(0);
        let fencing_token =
            bump_fence_tx(&tx, lease_key, existing_fence, &now_text).map_err(|_| poison())?;
        let (audit_action, audit_detail) = match &existing {
            Some(row) if row.lease.released_at.is_some() => (
                "reacquired",
                Some(format!(
                    "released tombstone generation {}",
                    row.lease.fencing_token
                )),
            ),
            Some(row) if row.lease.is_live_at(now) => (
                "reacquired",
                Some(format!(
                    "live same-owner generation {}",
                    row.lease.fencing_token
                )),
            ),
            Some(row) => (
                "acquired",
                Some(format!(
                    "expired predecessor generation {}",
                    row.lease.fencing_token
                )),
            ),
            None => ("acquired", None),
        };
        let token = RuntimeLeaseToken::generate();
        tx.execute(
            "INSERT INTO runtime_leases
             (lease_key, owner_id, owner_instance, kind, session_id, lease_token,
              fencing_token, acquired_at, heartbeat_at, renewed_at, expires_at, released_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?8, ?9, NULL)
             ON CONFLICT(lease_key) DO UPDATE SET
                owner_id = excluded.owner_id,
                owner_instance = excluded.owner_instance,
                kind = excluded.kind,
                session_id = excluded.session_id,
                lease_token = excluded.lease_token,
                fencing_token = excluded.fencing_token,
                acquired_at = excluded.acquired_at,
                heartbeat_at = excluded.heartbeat_at,
                renewed_at = excluded.renewed_at,
                expires_at = excluded.expires_at,
                released_at = NULL",
            params![
                lease_key,
                owner_id,
                owner_instance,
                kind,
                session_id,
                token.as_str(),
                fencing_token as i64,
                now_text,
                expires_text
            ],
        )
        .map_err(|_| poison())?;
        insert_audit_tx(
            &tx,
            lease_key,
            kind,
            audit_action,
            owner_id,
            owner_instance,
            fencing_token,
            audit_detail.as_deref(),
            &now_text,
        )
        .map_err(|_| poison())?;
        let row = query_lease_row_tx(&tx, lease_key)
            .map_err(|_| poison())?
            .ok_or_else(poison)?;
        tx.commit().map_err(|_| poison())?;
        Ok(RuntimeLeaseAcquisition {
            lease: row.lease,
            token,
        })
    }

    /// Renew a held lease. The update is conditional on owner, token, not
    /// released, and an unexpired row. A zero-row result means another process
    /// owns the next generation and the caller must stop.
    pub fn renew_runtime_lease(
        &self,
        lease_key: &str,
        owner_id: &str,
        token: &RuntimeLeaseToken,
        ttl_secs: u64,
    ) -> Result<RuntimeLease, RuntimeLeaseError> {
        self.renew_runtime_lease_at(
            lease_key,
            owner_id,
            token,
            ttl_secs,
            OffsetDateTime::now_utc(),
        )
    }

    pub fn renew_runtime_lease_at(
        &self,
        lease_key: &str,
        owner_id: &str,
        token: &RuntimeLeaseToken,
        ttl_secs: u64,
        now: OffsetDateTime,
    ) -> Result<RuntimeLease, RuntimeLeaseError> {
        let now_text = format_time(now).map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let expires_text = format_time(
            now + time::Duration::seconds(ttl_secs.min(MAX_RUNTIME_LEASE_TTL_SECS) as i64),
        )
        .map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let mut conn = self
            .lock()
            .map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let changed = tx.execute(
            "UPDATE runtime_leases
             SET heartbeat_at = ?1, renewed_at = ?1, expires_at = ?2
             WHERE lease_key = ?3 AND owner_id = ?4 AND lease_token = ?5
               AND released_at IS NULL
               AND julianday(expires_at) > julianday(?1)",
            params![now_text, expires_text, lease_key, owner_id, token.as_str()],
        )?;
        if changed == 0 {
            let row = query_lease_row_tx(&tx, lease_key)?;
            let reason = row
                .as_ref()
                .map(|row| classify_lost(row, owner_id, token, now))
                .unwrap_or(LeaseLostReason::Missing);
            return lost(lease_key, owner_id, reason);
        }
        let row = query_lease_row_tx(&tx, lease_key)?
            .ok_or_else(|| anyhow!("runtime lease disappeared during renewal"))?;
        tx.commit()?;
        Ok(row.lease)
    }

    /// Release a held lease. Release is token-fenced but intentionally allowed
    /// after expiry: if nobody has reacquired the key, cleaning up an expired
    /// row is harmless. A stale token can never delete a newer generation.
    pub fn release_runtime_lease(
        &self,
        lease_key: &str,
        owner_id: &str,
        token: &RuntimeLeaseToken,
    ) -> Result<bool, RuntimeLeaseError> {
        self.release_runtime_lease_at(lease_key, owner_id, token, OffsetDateTime::now_utc())
    }

    pub fn release_runtime_lease_at(
        &self,
        lease_key: &str,
        owner_id: &str,
        token: &RuntimeLeaseToken,
        now: OffsetDateTime,
    ) -> Result<bool, RuntimeLeaseError> {
        let now_text = format_time(now).map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let mut conn = self
            .lock()
            .map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let changed = tx.execute(
            "UPDATE runtime_leases
             SET released_at = ?1, heartbeat_at = ?1, expires_at = ?1
             WHERE lease_key = ?2 AND owner_id = ?3 AND lease_token = ?4
               AND released_at IS NULL",
            params![now_text, lease_key, owner_id, token.as_str()],
        )?;
        if changed == 0 {
            let row = query_lease_row_tx(&tx, lease_key)?;
            let reason = row
                .as_ref()
                .map(|row| classify_lost(row, owner_id, token, now))
                .unwrap_or(LeaseLostReason::Missing);
            return Err(RuntimeLeaseError::Lost {
                lease_key: lease_key.to_string(),
                owner_id: owner_id.to_string(),
                reason,
            });
        }
        let row = query_lease_row_tx(&tx, lease_key)?
            .ok_or_else(|| anyhow!("runtime lease disappeared during release"))?;
        insert_audit_tx(
            &tx,
            lease_key,
            &row.lease.kind,
            "released",
            owner_id,
            &row.lease.owner_instance,
            row.lease.fencing_token,
            None,
            &now_text,
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Diagnostic/effect-boundary assertion. It returns the public lease
    /// projection when the caller still owns an unexpired generation. The
    /// integration path calls this immediately before mutating the session
    /// branch.
    pub fn assert_runtime_lease(
        &self,
        lease_key: &str,
        owner_id: &str,
        token: &RuntimeLeaseToken,
    ) -> Result<RuntimeLease, RuntimeLeaseError> {
        self.assert_runtime_lease_at(lease_key, owner_id, token, OffsetDateTime::now_utc())
    }

    pub fn assert_runtime_lease_at(
        &self,
        lease_key: &str,
        owner_id: &str,
        token: &RuntimeLeaseToken,
        now: OffsetDateTime,
    ) -> Result<RuntimeLease, RuntimeLeaseError> {
        let conn = self
            .lock()
            .map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let row = query_lease_row(&conn, lease_key)
            .map_err(|_| RuntimeLeaseError::Busy { holder: None })?;
        let Some(row) = row else {
            return lost(lease_key, owner_id, LeaseLostReason::Missing);
        };
        if row.lease.released_at.is_some() {
            return lost(lease_key, owner_id, LeaseLostReason::Released);
        }
        if row.lease.owner_id != owner_id {
            return lost(lease_key, owner_id, LeaseLostReason::OwnerMismatch);
        }
        if row.token != token.as_str() {
            return lost(lease_key, owner_id, LeaseLostReason::TokenMismatch);
        }
        if !row.lease.is_live_at(now) {
            return lost(lease_key, owner_id, LeaseLostReason::Expired);
        }
        Ok(row.lease)
    }

    /// Public projection for the current row. Released tombstones are hidden.
    /// Expired rows remain visible until swept or reacquired so operators see
    /// stale ownership instead of a silent gap.
    pub fn runtime_lease_owner(&self, lease_key: &str) -> Result<Option<RuntimeLease>> {
        let conn = self.lock()?;
        let row = query_lease_row(&conn, lease_key)?;
        Ok(row.and_then(|row| {
            if row.lease.released_at.is_some() {
                None
            } else {
                Some(row.lease)
            }
        }))
    }

    pub fn list_runtime_leases(&self) -> Result<Vec<RuntimeLease>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUNTIME_LEASE_COLUMNS}
             FROM runtime_leases
             WHERE released_at IS NULL
             ORDER BY acquired_at ASC, lease_key ASC"
        ))?;
        let rows = stmt.query_map([], super::map_runtime_lease)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Audit view that includes released tombstones. No token is mapped.
    pub fn list_runtime_leases_including_released(&self) -> Result<Vec<RuntimeLease>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUNTIME_LEASE_COLUMNS}
             FROM runtime_leases
             ORDER BY acquired_at ASC, lease_key ASC"
        ))?;
        let rows = stmt.query_map([], super::map_runtime_lease)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Monotonic generation currently reserved for a key, even if the live row
    /// has been swept. Useful for diagnostics and tests.
    pub fn runtime_lease_fencing_token(&self, lease_key: &str) -> Result<Option<u64>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT fencing_token FROM runtime_lease_fences WHERE lease_key = ?1",
                params![lease_key],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|value| value.max(0) as u64))
    }

    pub fn clear_expired_runtime_leases(&self) -> Result<usize> {
        self.clear_expired_runtime_leases_at(OffsetDateTime::now_utc())
    }

    pub fn clear_expired_runtime_leases_at(&self, now: OffsetDateTime) -> Result<usize> {
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {RUNTIME_LEASE_COLUMNS}
                 FROM runtime_leases
                 WHERE released_at IS NULL
                   AND julianday(expires_at) <= julianday(?1)
                 ORDER BY lease_key ASC"
            ))?;
            let rows = stmt.query_map(params![now_text], super::map_runtime_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for lease in &expired {
            insert_audit_tx(
                &tx,
                &lease.lease_key,
                &lease.kind,
                "expired",
                &lease.owner_id,
                &lease.owner_instance,
                lease.fencing_token,
                Some("swept after expiry"),
                &now_text,
            )?;
        }
        let changed = tx.execute(
            "DELETE FROM runtime_leases
             WHERE julianday(expires_at) <= julianday(?1) OR released_at IS NOT NULL",
            params![now_text],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    /// Delete audit rows older than `before`. Returns the number removed.
    /// The live lease and fence tables are unaffected.
    pub fn prune_runtime_lease_audit(&self, before: OffsetDateTime) -> Result<usize> {
        let before_text = format_time(before)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "DELETE FROM runtime_lease_audit WHERE julianday(occurred_at) < julianday(?1)",
            params![before_text],
        )?;
        Ok(changed)
    }

    /// Token-free lease audit history, newest first. `lease_key` filters one
    /// key; `None` returns the most recent transitions across all keys.
    pub fn list_runtime_lease_audit(
        &self,
        lease_key: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<RuntimeLeaseAuditEntry>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 5_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT id, lease_key, kind, action, owner_id, owner_instance,
                    fencing_token, detail, occurred_at
             FROM runtime_lease_audit
             WHERE (?1 IS NULL OR lease_key = ?1)
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![lease_key, limit], map_audit_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Startup cleanup is process-scoped. It never deletes a live lease owned
    /// by another process sharing the data directory. Empty `owner_instance`
    /// rows predate fencing and are safe to remove: no fenced guard can renew
    /// them.
    pub fn clear_own_process_runtime_leases(&self) -> Result<usize> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "DELETE FROM runtime_leases
             WHERE owner_instance = ?1 OR owner_instance = ''",
            params![self.process_owner_instance()],
        )?;
        Ok(changed)
    }

    /// Lease keys with a live foreign lease. Used by startup recovery so one
    /// process does not fail another process's running session or partially
    /// merged worktree.
    pub fn live_foreign_lease_keys(&self) -> Result<HashSet<String>> {
        let now_text = now_rfc3339();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT lease_key FROM runtime_leases
             WHERE released_at IS NULL
               AND owner_instance <> ''
               AND owner_instance <> ?1
               AND julianday(expires_at) > julianday(?2)",
        )?;
        let rows = stmt.query_map(params![self.process_owner_instance(), now_text], |row| {
            row.get::<_, String>(0)
        })?;
        Ok(rows.collect::<Result<HashSet<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_identity_and_owner_ids_distinguish_processes() {
        let identity = DaemonIdentity {
            instance_id: "instance-a".into(),
            pid: 42,
            started_at: "2026-11-11T00:00:00Z".into(),
        };
        let owner = identity.owner_id("nonce");
        assert!(owner.starts_with("instance-a:"));
        assert!(owner.ends_with(":nonce"));
        assert_eq!(
            identity.process_instance(),
            format!("instance-a:{}", std::process::id())
        );
    }

    #[test]
    fn token_debug_is_redacted() {
        let token = RuntimeLeaseToken::generate();
        let debug = format!("{token:?}");
        assert_eq!(debug, "RuntimeLeaseToken(redacted)");
        assert!(!debug.contains(token.as_str()));
    }

    #[test]
    fn lease_state_is_derived_from_expiry_and_release() {
        let mut lease = RuntimeLease {
            lease_key: "session:s1".into(),
            owner_id: "daemon-a".into(),
            owner_instance: "instance:1".into(),
            kind: "session".into(),
            session_id: Some("s1".into()),
            fencing_token: 3,
            acquired_at: "2026-11-11T00:00:00Z".into(),
            heartbeat_at: "2026-11-11T00:00:00Z".into(),
            renewed_at: "2026-11-11T00:00:00Z".into(),
            expires_at: "2026-11-11T00:01:00Z".into(),
            released_at: None,
        };
        let now = OffsetDateTime::parse("2026-11-11T00:00:30Z", &Rfc3339).unwrap();
        assert_eq!(lease.state_at(now), RuntimeLeaseState::Live);
        let later = now + time::Duration::seconds(60);
        assert_eq!(lease.state_at(later), RuntimeLeaseState::Stale);
        lease.released_at = Some("2026-11-11T00:00:40Z".into());
        assert_eq!(lease.state_at(now), RuntimeLeaseState::Released);
    }

    #[test]
    fn malformed_expiry_fails_closed_as_stale() {
        let lease = RuntimeLease {
            lease_key: "session:s1".into(),
            owner_id: "daemon-a".into(),
            owner_instance: "instance:1".into(),
            kind: "session".into(),
            session_id: None,
            fencing_token: 1,
            acquired_at: "not-a-time".into(),
            heartbeat_at: "not-a-time".into(),
            renewed_at: String::new(),
            expires_at: "not-a-time".into(),
            released_at: None,
        };
        assert_eq!(
            lease.state_at(OffsetDateTime::now_utc()),
            RuntimeLeaseState::Stale
        );
    }
}
