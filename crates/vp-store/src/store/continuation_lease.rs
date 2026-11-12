//! Token-fenced delivery ownership for deferred session continuations.
//!
//! A continuation is a persisted operator intent, not an agent tool. The
//! scheduler may run on multiple daemon processes that share a data directory,
//! so transitioning `ready -> delivering` must be an atomic compare-and-set.
//! The winning process receives an unpredictable token; every acknowledgement
//! (`delivered`, `retry`, `failed`) is conditional on that token and on the
//! claim not having expired. A stale process can therefore never write the
//! terminal state of a row that another process has reclaimed.
//!
//! Delivery itself is still an external effect: the process calls the normal
//! session start path after claiming. The reconciler uses a stable session
//! operation receipt (`continuation-delivery:<id>`) so a crash between claim
//! and acknowledgement cannot silently start a second run. The database claim
//! prevents duplicate *delivery ownership*; the receipt closes the remaining
//! start window. Neither is a distributed transaction with the provider.

use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::Store;
use super::{
    ContinuationDelivery, SESSION_CONTINUATION_COLUMNS, SessionContinuation, insert_event_tx,
    next_recurring_continuation, query_session_continuation_tx,
};

const MAX_CONTINUATION_CLAIM_TTL_SECS: u64 = 3_600;

/// Unpredictable delivery-claim secret. It is returned only to the claiming
/// scheduler and is never serialized into a session event, RPC result, bundle,
/// or log. Debug redacts it.
#[derive(Clone, PartialEq, Eq)]
pub struct ContinuationClaimToken(String);

impl ContinuationClaimToken {
    pub(crate) fn generate() -> Self {
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

impl std::fmt::Debug for ContinuationClaimToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ContinuationClaimToken(redacted)")
    }
}

/// A claimed delivery attempt. `continuation` is the public projection with
/// `lease_owner`, `lease_expires_at`, and `delivery_attempt` populated; the
/// token stays in this internal wrapper.
#[derive(Clone)]
pub struct SessionContinuationClaim {
    pub continuation: SessionContinuation,
    pub owner_id: String,
    pub expires_at: String,
    pub delivery_attempt: u32,
    pub reclaimed: bool,
    pub event: vp_protocol::types::Event,
    token: ContinuationClaimToken,
}

impl SessionContinuationClaim {
    pub fn token(&self) -> &ContinuationClaimToken {
        &self.token
    }
}

impl std::fmt::Debug for SessionContinuationClaim {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionContinuationClaim")
            .field("continuation_id", &self.continuation.id)
            .field("owner_id", &self.owner_id)
            .field("expires_at", &self.expires_at)
            .field("delivery_attempt", &self.delivery_attempt)
            .field("reclaimed", &self.reclaimed)
            .field("token", &"redacted")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationAckLostReason {
    Missing,
    NotDelivering,
    OwnerMismatch,
    TokenMismatch,
    Expired,
    AlreadyTerminal,
}

impl ContinuationAckLostReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::NotDelivering => "not_delivering",
            Self::OwnerMismatch => "owner_mismatch",
            Self::TokenMismatch => "token_mismatch",
            Self::Expired => "expired",
            Self::AlreadyTerminal => "already_terminal",
        }
    }
}

/// Typed stale-acknowledgement error. The reconciler treats every `Lost`
/// variant as a signal to abandon its local result without retrying a terminal
/// write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionContinuationAckError {
    Lost {
        continuation_id: String,
        owner_id: String,
        reason: ContinuationAckLostReason,
    },
    Internal(String),
}

impl std::fmt::Display for SessionContinuationAckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lost {
                continuation_id,
                owner_id,
                reason,
            } => write!(
                formatter,
                "continuation {continuation_id} delivery claim was lost for {owner_id}: {}",
                reason.as_str()
            ),
            Self::Internal(message) => {
                write!(
                    formatter,
                    "continuation delivery operation failed: {message}"
                )
            }
        }
    }
}

impl std::error::Error for SessionContinuationAckError {}

impl From<anyhow::Error> for SessionContinuationAckError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(format!("{error:#}"))
    }
}

impl From<rusqlite::Error> for SessionContinuationAckError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

impl From<serde_json::Error> for SessionContinuationAckError {
    fn from(error: serde_json::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

fn lost(
    continuation_id: &str,
    owner_id: &str,
    reason: ContinuationAckLostReason,
) -> SessionContinuationAckError {
    SessionContinuationAckError::Lost {
        continuation_id: continuation_id.to_string(),
        owner_id: owner_id.to_string(),
        reason,
    }
}

fn format_time(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .map_err(|error| anyhow!("formatting continuation claim time: {error}"))
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

struct OwnedContinuation {
    continuation: SessionContinuation,
    token: Option<String>,
}

fn query_owned_continuation_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> Result<Option<OwnedContinuation>> {
    let row = tx
        .query_row(
            &format!(
                "SELECT {SESSION_CONTINUATION_COLUMNS}, lease_token
                 FROM session_continuations WHERE id = ?1"
            ),
            params![id],
            |row| {
                Ok((
                    super::map_session_continuation(row)?,
                    row.get::<_, Option<String>>(27)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(|(continuation, token)| OwnedContinuation {
        continuation,
        token,
    }))
}

fn validate_claim(
    row: &OwnedContinuation,
    owner_id: &str,
    token: &ContinuationClaimToken,
    now: OffsetDateTime,
) -> Result<(), ContinuationAckLostReason> {
    let continuation = &row.continuation;
    if continuation.status != "delivering" {
        return Err(
            if matches!(
                continuation.status.as_str(),
                "delivered" | "failed" | "cancelled"
            ) {
                ContinuationAckLostReason::AlreadyTerminal
            } else {
                ContinuationAckLostReason::NotDelivering
            },
        );
    }
    if continuation.lease_owner.as_deref() != Some(owner_id) {
        return Err(ContinuationAckLostReason::OwnerMismatch);
    }
    if row.token.as_deref() != Some(token.as_str()) {
        return Err(ContinuationAckLostReason::TokenMismatch);
    }
    let unexpired = continuation
        .lease_expires_at
        .as_deref()
        .and_then(parse_time)
        .is_some_and(|expires| expires > now);
    if !unexpired {
        return Err(ContinuationAckLostReason::Expired);
    }
    Ok(())
}

fn claimable(continuation: &SessionContinuation, now: OffsetDateTime) -> bool {
    match continuation.status.as_str() {
        "ready" => continuation
            .lease_expires_at
            .as_deref()
            .and_then(parse_time)
            .is_none_or(|expires| expires <= now),
        "delivering" => continuation
            .lease_expires_at
            .as_deref()
            .and_then(parse_time)
            .is_some_and(|expires| expires <= now),
        _ => false,
    }
}

impl Store {
    /// Claim a ready row, or reclaim a `delivering` row whose claim expired.
    /// The transaction is `BEGIN IMMEDIATE`, so two Store instances racing on
    /// one data directory produce exactly one winner; the loser sees a live
    /// claim and returns `None`.
    pub fn claim_session_continuation_for_delivery_with_event(
        &self,
        id: &str,
        owner_id: &str,
        ttl_secs: u64,
    ) -> Result<Option<SessionContinuationClaim>> {
        self.claim_session_continuation_for_delivery_at(
            id,
            owner_id,
            ttl_secs,
            OffsetDateTime::now_utc(),
        )
    }

    /// Deterministic-clock variant. A zero TTL immediately expires, which the
    /// tests use to exercise reclaim without sleeping.
    pub fn claim_session_continuation_for_delivery_at(
        &self,
        id: &str,
        owner_id: &str,
        ttl_secs: u64,
        now: OffsetDateTime,
    ) -> Result<Option<SessionContinuationClaim>> {
        if owner_id.trim().is_empty() || owner_id.len() > 320 {
            anyhow::bail!("continuation delivery owner must be 1-320 non-blank characters");
        }
        let now_text = format_time(now)?;
        let expires_text = format_time(
            now + time::Duration::seconds(ttl_secs.min(MAX_CONTINUATION_CLAIM_TTL_SECS) as i64),
        )?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(row) = query_owned_continuation_tx(&tx, id)? else {
            return Ok(None);
        };
        if !claimable(&row.continuation, now) {
            return Ok(None);
        }
        let reclaimed = row.continuation.status == "delivering";
        let token = ContinuationClaimToken::generate();
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'delivering',
                 lease_owner = ?1,
                 lease_token = ?2,
                 lease_expires_at = ?3,
                 delivery_attempt = delivery_attempt + 1,
                 next_attempt_at = NULL,
                 updated_at = ?4
             WHERE id = ?5
               AND (
                    (status = 'ready'
                     AND (lease_expires_at IS NULL
                          OR julianday(lease_expires_at) <= julianday(?4)))
                 OR (status = 'delivering'
                     AND lease_expires_at IS NOT NULL
                     AND julianday(lease_expires_at) <= julianday(?4))
               )",
            params![owner_id, token.as_str(), expires_text, now_text, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during delivery claim: {id}"))?;
        let mut payload = serde_json::to_value(&continuation)?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("reclaimed".into(), serde_json::json!(reclaimed));
        }
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.delivering",
            payload,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(SessionContinuationClaim {
            continuation,
            owner_id: owner_id.to_string(),
            expires_at: expires_text,
            delivery_attempt: row.continuation.delivery_attempt.saturating_add(1),
            reclaimed,
            event,
            token,
        }))
    }

    /// Mark a claimed row delivered and, when the recurrence still has budget,
    /// insert the next child in the same transaction. The update is fenced on
    /// owner+token+unexpired; a stale claimant gets a typed error and no row is
    /// changed.
    pub fn mark_session_continuation_delivered_with_event(
        &self,
        id: &str,
        owner_id: &str,
        token: &ContinuationClaimToken,
    ) -> Result<Option<ContinuationDelivery>, SessionContinuationAckError> {
        self.mark_session_continuation_delivered_at(id, owner_id, token, OffsetDateTime::now_utc())
    }

    pub fn mark_session_continuation_delivered_at(
        &self,
        id: &str,
        owner_id: &str,
        token: &ContinuationClaimToken,
        now: OffsetDateTime,
    ) -> Result<Option<ContinuationDelivery>, SessionContinuationAckError> {
        let now_text = format_time(now)?;
        let mut conn = self
            .lock()
            .map_err(|_| SessionContinuationAckError::Internal("store mutex poisoned".into()))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(row) = query_owned_continuation_tx(&tx, id)? else {
            return Err(lost(id, owner_id, ContinuationAckLostReason::Missing));
        };
        if let Err(reason) = validate_claim(&row, owner_id, token, now) {
            return Err(lost(id, owner_id, reason));
        }
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'delivered', run_count = run_count + 1, delivered_at = ?1,
                 next_attempt_at = NULL, updated_at = ?1,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?2 AND status = 'delivering'
               AND lease_owner = ?3 AND lease_token = ?4
               AND lease_expires_at IS NOT NULL
               AND julianday(lease_expires_at) > julianday(?1)",
            params![now_text, id, owner_id, token.as_str()],
        )?;
        if changed == 0 {
            return Err(lost(id, owner_id, ContinuationAckLostReason::Expired));
        }
        let delivered = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during delivery: {id}"))?;
        let mut events = vec![insert_event_tx(
            &tx,
            &delivered.session_id,
            "session.continuation.delivered",
            serde_json::to_value(&delivered)?,
        )?];
        let scheduled = next_recurring_continuation(&delivered, &now_text)?;
        if let Some(child) = &scheduled {
            super::insert_session_continuation_tx(&tx, child)?;
            events.push(insert_event_tx(
                &tx,
                &child.session_id,
                "session.continuation.scheduled",
                serde_json::to_value(child)?,
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(Some(ContinuationDelivery {
            delivered,
            scheduled,
            events,
        }))
    }

    /// Return a claimed row to `ready` after a recoverable start failure. The
    /// claim must still be live and owned by the caller. Claim fields are
    /// cleared so the next scheduler tick can acquire a fresh token.
    pub fn retry_session_continuation_with_event(
        &self,
        id: &str,
        owner_id: &str,
        token: &ContinuationClaimToken,
        error: &str,
        next_attempt_at: &str,
    ) -> Result<Option<(SessionContinuation, vp_protocol::types::Event)>, SessionContinuationAckError>
    {
        self.retry_session_continuation_at(
            id,
            owner_id,
            token,
            error,
            next_attempt_at,
            OffsetDateTime::now_utc(),
        )
    }

    pub fn retry_session_continuation_at(
        &self,
        id: &str,
        owner_id: &str,
        token: &ContinuationClaimToken,
        error: &str,
        next_attempt_at: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(SessionContinuation, vp_protocol::types::Event)>, SessionContinuationAckError>
    {
        let now_text = format_time(now)?;
        let mut conn = self
            .lock()
            .map_err(|_| SessionContinuationAckError::Internal("store mutex poisoned".into()))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(row) = query_owned_continuation_tx(&tx, id)? else {
            return Err(lost(id, owner_id, ContinuationAckLostReason::Missing));
        };
        if let Err(reason) = validate_claim(&row, owner_id, token, now) {
            return Err(lost(id, owner_id, reason));
        }
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'ready', attempts = attempts + 1, last_error = ?1,
                 next_attempt_at = ?2, updated_at = ?3,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?4 AND status = 'delivering'
               AND lease_owner = ?5 AND lease_token = ?6
               AND lease_expires_at IS NOT NULL
               AND julianday(lease_expires_at) > julianday(?3)",
            params![
                error,
                next_attempt_at,
                now_text,
                id,
                owner_id,
                token.as_str()
            ],
        )?;
        if changed == 0 {
            return Err(lost(id, owner_id, ContinuationAckLostReason::Expired));
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during retry: {id}"))?;
        let mut payload = serde_json::to_value(&continuation)?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("error".into(), serde_json::json!(error));
        }
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.retrying",
            payload,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    /// Fail a claimed delivery after the start attempt exhausted its attempts
    /// or hit a non-retryable error. Fenced exactly like delivery.
    pub fn fail_session_continuation_with_event(
        &self,
        id: &str,
        owner_id: &str,
        token: &ContinuationClaimToken,
        error: &str,
    ) -> Result<Option<(SessionContinuation, vp_protocol::types::Event)>, SessionContinuationAckError>
    {
        self.fail_session_continuation_at(id, owner_id, token, error, OffsetDateTime::now_utc())
    }

    pub fn fail_session_continuation_at(
        &self,
        id: &str,
        owner_id: &str,
        token: &ContinuationClaimToken,
        error: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(SessionContinuation, vp_protocol::types::Event)>, SessionContinuationAckError>
    {
        let now_text = format_time(now)?;
        let mut conn = self
            .lock()
            .map_err(|_| SessionContinuationAckError::Internal("store mutex poisoned".into()))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(row) = query_owned_continuation_tx(&tx, id)? else {
            return Err(lost(id, owner_id, ContinuationAckLostReason::Missing));
        };
        if let Err(reason) = validate_claim(&row, owner_id, token, now) {
            return Err(lost(id, owner_id, reason));
        }
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'failed', attempts = attempts + 1, last_error = ?1,
                 next_attempt_at = NULL, updated_at = ?2,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?3 AND status = 'delivering'
               AND lease_owner = ?4 AND lease_token = ?5
               AND lease_expires_at IS NOT NULL
               AND julianday(lease_expires_at) > julianday(?2)",
            params![error, now_text, id, owner_id, token.as_str()],
        )?;
        if changed == 0 {
            return Err(lost(id, owner_id, ContinuationAckLostReason::Expired));
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during failure: {id}"))?;
        let mut payload = serde_json::to_value(&continuation)?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("error".into(), serde_json::json!(error));
        }
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.failed",
            payload,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    /// Fail a row that has no live delivery claim. Used by the reconciler for
    /// missing sessions and by legacy recovery. Rows claimed by another live
    /// process are left untouched.
    pub fn fail_unclaimed_session_continuation_with_event(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<(SessionContinuation, vp_protocol::types::Event)>> {
        let now = super::now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'failed', attempts = attempts + 1, last_error = ?1,
                 next_attempt_at = NULL, updated_at = ?2,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?3
               AND (status IN ('pending', 'ready')
                    OR (status = 'delivering'
                        AND (lease_expires_at IS NULL
                             OR julianday(lease_expires_at) <= julianday(?2))))",
            params![error, now, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during failure: {id}"))?;
        let mut payload = serde_json::to_value(&continuation)?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("error".into(), serde_json::json!(error));
        }
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.failed",
            payload,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    /// Reclaim a single expired claim into `ready` and emit an audit event.
    /// Startup recovery and the scheduler use this instead of failing the row
    /// closed. The delivery operation receipt is what prevents a duplicate run
    /// when the expired claimant may already have started one.
    pub fn reclaim_expired_session_continuation_with_event(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(SessionContinuation, vp_protocol::types::Event)>> {
        self.reclaim_expired_session_continuation_at(id, reason, OffsetDateTime::now_utc())
    }

    pub fn reclaim_expired_session_continuation_at(
        &self,
        id: &str,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(SessionContinuation, vp_protocol::types::Event)>> {
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'ready', last_error = ?1, next_attempt_at = NULL,
                 updated_at = ?2,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?3 AND status = 'delivering'
               AND lease_expires_at IS NOT NULL
               AND julianday(lease_expires_at) <= julianday(?2)",
            params![reason, now_text, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during reclaim: {id}"))?;
        let mut payload = serde_json::to_value(&continuation)?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("reason".into(), serde_json::json!(reason));
        }
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.reclaimed",
            payload,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    /// Public, token-free claim visibility for diagnostics. A live foreign
    /// claim returns `live = true`; an expired claim is showable but claimable.
    pub fn session_continuation_claim_status(
        &self,
        id: &str,
    ) -> Result<Option<SessionContinuationClaimStatus>> {
        let now = OffsetDateTime::now_utc();
        let conn = self.lock()?;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {SESSION_CONTINUATION_COLUMNS}, lease_token
                     FROM session_continuations WHERE id = ?1"
                ),
                params![id],
                |row| {
                    Ok(OwnedContinuation {
                        continuation: super::map_session_continuation(row)?,
                        token: row.get::<_, Option<String>>(27)?,
                    })
                },
            )
            .optional()?;
        Ok(row.and_then(|row| {
            // Claim metadata is meaningful only while a delivery is in
            // flight. Pending/ready/terminal rows expose their attempt count
            // through the continuation projection instead.
            if row.continuation.status != "delivering" {
                return None;
            }
            let live = row
                .continuation
                .lease_expires_at
                .as_deref()
                .and_then(parse_time)
                .is_some_and(|expires| expires > now);
            Some(SessionContinuationClaimStatus {
                continuation_id: row.continuation.id,
                status: row.continuation.status,
                owner_id: row.continuation.lease_owner,
                expires_at: row.continuation.lease_expires_at,
                delivery_attempt: row.continuation.delivery_attempt,
                live,
            })
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionContinuationClaimStatus {
    pub continuation_id: String,
    pub status: String,
    pub owner_id: Option<String>,
    pub expires_at: Option<String>,
    pub delivery_attempt: u32,
    pub live: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_token_debug_is_redacted() {
        let token = ContinuationClaimToken::generate();
        let debug = format!("{token:?}");
        assert_eq!(debug, "ContinuationClaimToken(redacted)");
        assert!(!debug.contains(token.as_str()));
    }

    #[test]
    fn claimability_requires_ready_or_expired_delivery() {
        let now = OffsetDateTime::parse("2026-11-11T00:00:00Z", &Rfc3339).unwrap();
        let mut continuation = SessionContinuation {
            id: "c1".into(),
            session_id: "s1".into(),
            project_id: "p1".into(),
            body: "body".into(),
            watch_task_ids: Vec::new(),
            watch_mode: "all".into(),
            watch_gate_id: None,
            deadline_at: None,
            status: "ready".into(),
            attempts: 0,
            max_attempts: 3,
            wake_reason: Some("deadline".into()),
            outcomes: Vec::new(),
            last_error: None,
            next_attempt_at: None,
            created_at: "2026-11-11T00:00:00Z".into(),
            ready_at: Some("2026-11-11T00:00:00Z".into()),
            delivered_at: None,
            updated_at: "2026-11-11T00:00:00Z".into(),
            series_id: String::new(),
            parent_continuation_id: None,
            recurrence_every_secs: None,
            recurrence_max_runs: 1,
            run_count: 0,
            lease_owner: None,
            lease_expires_at: None,
            delivery_attempt: 0,
        };
        assert!(claimable(&continuation, now));
        continuation.lease_owner = Some("daemon".into());
        continuation.lease_expires_at = Some("2026-11-11T00:01:00Z".into());
        assert!(!claimable(&continuation, now));
        continuation.status = "delivering".into();
        assert!(!claimable(&continuation, now));
        let later = now + time::Duration::seconds(61);
        assert!(claimable(&continuation, later));
        continuation.status = "delivered".into();
        assert!(!claimable(&continuation, later));
    }
}
