//! Durable external gates and one-shot scoped tokens.
//!
//! A gate is a local wait handle for CI, PR review, deploy, a human decision,
//! or an opaque custom event. It is deliberately provider-neutral: the runtime
//! never interprets the external outcome and never maps it to an approval,
//! merge, budget, or verifier decision. `gate.resolve` authenticates a scoped
//! one-time token and wakes the watching continuation through the normal prompt
//! path. Every other gate is unchanged.
//!
//! Security invariants:
//!
//! - the raw token exists only in the `gate.open` response;
//! - SQLite stores a SHA-256 verifier, never the token;
//! - events carry the public gate plus the verifier, never the token;
//! - tool output, logs, RPC results after `gate.open`, and Swift models never
//!   receive the token or the verifier;
//! - resolution is one-shot, expiry-bound, and fails closed on a wrong token,
//!   a terminal state, or a different outcome replay.
//!
//! There is no public webhook listener. The daemon is loopback-bound; a
//! project may explicitly enable the optional loopback resolve route, which
//! still requires the same scoped token.

use anyhow::{Result, anyhow};
use base64::Engine as Base64Engine;
use rand::TryRngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;
use vp_protocol::types::{
    ExternalGate, GATE_CANCELLED_EVENT, GATE_CONTINUATION_READY_EVENT, GATE_EXPIRED_EVENT,
    GATE_OPENED_EVENT, GATE_RESOLVED_EVENT, GateCancelResult, GateCounts, GateKind, GateListResult,
    GateOpenParams, GateOpenResult, GateOutcome, GatePolicy, GateRecord, GateResolveResult,
    GateState, GateStatusResult, SessionContinuation, SessionDeferResult,
};
use vp_store::store::GateTransition;

use crate::Engine;
use crate::continuation::{MAX_CONTEXT_NOTE_CHARS, validate_body};

const MAX_GATE_LABEL_CHARS: usize = 200;
const MAX_GATE_NOTE_CHARS: usize = 2_000;
const MAX_GATE_PAYLOAD_BYTES: usize = 16 * 1024;
const MAX_GATE_TOKEN_CHARS: usize = 200;
const MAX_GATE_ACTOR_CHARS: usize = 200;
const MAX_DEADLINE_SECS: i64 = 30 * 24 * 60 * 60;

/// Stable gate error codes. These are wire-visible in RPC errors and are
/// mapped to `ToolErrorCode` variants for the agent surface.
pub mod codes {
    pub const GATE_NOT_FOUND: &str = "GATE_NOT_FOUND";
    pub const GATE_TOKEN_INVALID: &str = "GATE_TOKEN_INVALID";
    pub const GATE_INVALID: &str = "GATE_INVALID";
    pub const GATE_EXPIRED: &str = "GATE_EXPIRED";
    pub const GATE_CANCELLED: &str = "GATE_CANCELLED";
    pub const GATE_CONFLICT: &str = "GATE_CONFLICT";
    pub const GATE_ALREADY_WATCHED: &str = "GATE_ALREADY_WATCHED";
    pub const GATE_ARCHIVED: &str = "GATE_ARCHIVED";
    pub const GATE_LOOPBACK_DISABLED: &str = "GATE_LOOPBACK_DISABLED";
}

/// Typed gate failure. `anyhow` preserves the chain; callers can downcast to
/// recover the stable code without parsing the human message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for GateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for GateError {}

pub fn gate_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    anyhow!(GateError {
        code,
        message: message.into(),
    })
}

pub fn gate_error_code(error: &anyhow::Error) -> Option<&'static str> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GateError>().map(|error| error.code))
}

/// A one-time gate token. Debug is redacted; the only way to obtain the raw
/// value is an explicit `expose` call in the operator response path.
#[derive(Clone, PartialEq, Eq)]
pub struct GateToken(String);

impl GateToken {
    /// 32 random bytes, URL-safe base64 without padding, plus a recognizable
    /// `vpg_` prefix. The prefix is a redaction aid, not an authority marker.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| gate_error(codes::GATE_INVALID, "OS randomness is unavailable"))?;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        Ok(Self(format!("vpg_{encoded}")))
    }

    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > MAX_GATE_TOKEN_CHARS {
            return Err(gate_error(
                codes::GATE_TOKEN_INVALID,
                "the gate token is empty or too long",
            ));
        }
        if value.chars().any(char::is_control) {
            return Err(gate_error(
                codes::GATE_TOKEN_INVALID,
                "the gate token contains control characters",
            ));
        }
        Ok(Self(value.to_string()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn hash(&self) -> String {
        hash_gate_token(&self.0)
    }
}

impl std::fmt::Debug for GateToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GateToken(redacted)")
    }
}

pub fn hash_gate_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn verify_gate_token(token: &str, expected_hash: &str) -> bool {
    if expected_hash.len() != 64 || token.is_empty() {
        return false;
    }
    let actual = hash_gate_token(token);
    constant_time_eq(&actual, expected_hash)
}

/// Operator-facing result of opening a gate. The protocol layer serializes
/// `token` exactly once; no event or store row contains it.
#[derive(Debug)]
pub struct GateOpenOutcome {
    pub gate: ExternalGate,
    pub token: GateToken,
    pub resolve_hint: String,
    pub loopback_resolve: bool,
}

impl Engine {
    /// Open a gate and return its one-time token. The token cannot be recovered
    /// later; an operator who loses it must cancel the gate and open a new one.
    pub async fn open_gate(&self, params: GateOpenParams) -> Result<GateOpenOutcome> {
        let session_id = params.session_id.trim().to_string();
        if session_id.is_empty() {
            return Err(gate_error(codes::GATE_INVALID, "session_id is required"));
        }
        let store = self.store().clone();
        let lookup = session_id.clone();
        let session = tokio::task::spawn_blocking(move || store.get_session(&lookup))
            .await??
            .ok_or_else(|| {
                gate_error(
                    codes::GATE_NOT_FOUND,
                    format!("session {session_id} not found"),
                )
            })?;
        if session.status == "imported" {
            return Err(gate_error(
                codes::GATE_ARCHIVED,
                "imported sessions are read-only archives",
            ));
        }
        if session.status == "landed" {
            return Err(gate_error(
                codes::GATE_ARCHIVED,
                "landed sessions are final; open a new session to continue",
            ));
        }
        let kind = match params.kind.as_deref() {
            Some(kind) => GateKind::parse(kind).ok_or_else(|| {
                gate_error(
                    codes::GATE_INVALID,
                    "gate kind must be ci, pr_review, deploy, human, or custom",
                )
            })?,
            None => GateKind::Custom,
        };
        let label = match params.label.as_deref().map(str::trim) {
            Some(label) if !label.is_empty() => {
                if label.chars().count() > MAX_GATE_LABEL_CHARS {
                    return Err(gate_error(
                        codes::GATE_INVALID,
                        format!("gate label must be at most {MAX_GATE_LABEL_CHARS} characters"),
                    ));
                }
                label.to_string()
            }
            _ => kind.as_str().to_string(),
        };
        let task_id = params
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|task_id| !task_id.is_empty())
            .map(str::to_string);
        if let Some(task_id) = &task_id {
            crate::worktree::validate_task_id(task_id)?;
            let plan = self.get_team_plan(&session_id).await?;
            let Some(plan) = plan else {
                return Err(gate_error(
                    codes::GATE_INVALID,
                    "a task-scoped gate needs a stored TeamPlan",
                ));
            };
            if !plan.tasks.iter().any(|task| &task.id == task_id) {
                return Err(gate_error(
                    codes::GATE_INVALID,
                    format!("task {task_id} is not in the stored TeamPlan"),
                ));
            }
        }
        let payload = params.payload.unwrap_or_else(|| json!({}));
        validate_payload(&payload)?;
        let expires_at = resolve_gate_deadline(
            params.after_secs,
            params.expires_at.as_deref(),
            OffsetDateTime::now_utc(),
        )?;
        let policy = self.get_gate_policy(&session.project_id).await?;
        let token = GateToken::generate()?;
        let record = GateRecord {
            gate: ExternalGate {
                id: Uuid::now_v7().to_string(),
                session_id: session_id.clone(),
                task_id,
                kind,
                label,
                state: GateState::Open,
                outcome: None,
                payload,
                resolution_note: None,
                created_at: vp_protocol::now_rfc3339(),
                expires_at,
                resolved_at: None,
                resolved_by: None,
            },
            token_hash: token.hash(),
        };
        let store = self.store().clone();
        let create = record.clone();
        let (stored, event) =
            tokio::task::spawn_blocking(move || store.create_gate_with_event(&create)).await??;
        let _ = self.events.send(event);
        let resolve_hint = format!(
            "printf '%s' <one-time-token> | vp gate resolve {} --token-stdin [--outcome resolved|rejected|failed] [--note ...]",
            stored.gate.id
        );
        Ok(GateOpenOutcome {
            gate: stored.gate,
            token,
            resolve_hint,
            loopback_resolve: policy.loopback_resolve,
        })
    }

    /// Resolve a gate with its scoped token. The daemon Bearer token is not
    /// consulted here and cannot substitute for the gate token.
    pub async fn resolve_gate(
        &self,
        gate_id: &str,
        token: &str,
        outcome: Option<&str>,
        note: Option<&str>,
        resolved_by: Option<&str>,
    ) -> Result<GateResolveResult> {
        let gate_id = gate_id.trim();
        if gate_id.is_empty() {
            return Err(gate_error(codes::GATE_INVALID, "gate_id is required"));
        }
        let outcome = match outcome {
            Some(outcome) => GateOutcome::parse(outcome).ok_or_else(|| {
                gate_error(
                    codes::GATE_INVALID,
                    "outcome must be resolved, rejected, or failed",
                )
            })?,
            None => GateOutcome::Resolved,
        };
        let token = GateToken::parse(token)?;
        let store = self.store().clone();
        let existing = tokio::task::spawn_blocking({
            let gate_id = gate_id.to_string();
            let store = store.clone();
            move || store.get_gate(&gate_id)
        })
        .await??
        .ok_or_else(|| {
            gate_error(
                codes::GATE_NOT_FOUND,
                format!("gate {gate_id} does not exist"),
            )
        })?;
        self.ensure_gate_session_live(&existing.gate.session_id, false)
            .await?;
        if !verify_gate_token(token.expose(), &existing.token_hash) {
            return Err(gate_error(
                codes::GATE_TOKEN_INVALID,
                "the gate token does not authorize resolution",
            ));
        }
        let note = normalize_note(note)?;
        let actor = normalize_actor(resolved_by)?;
        let expected_hash = token.hash();
        let gate_id_owned = gate_id.to_string();
        let transition = run_gate_store(move || {
            store.resolve_gate(
                &gate_id_owned,
                &expected_hash,
                outcome,
                note.as_deref(),
                actor.as_deref(),
            )
        })
        .await?;
        for event in &transition.events {
            let _ = self.events.send(event.clone());
        }
        Ok(GateResolveResult {
            continuation_id: transition.continuation.as_ref().map(|row| row.id.clone()),
            gate: transition.gate.gate,
            changed: transition.changed,
        })
    }

    /// Operator cancellation. Cancellation is terminal and wakes the waiting
    /// continuation with a `cancelled` state; it does not run any other gate.
    pub async fn cancel_gate(
        &self,
        gate_id: &str,
        reason: Option<&str>,
        cancelled_by: Option<&str>,
    ) -> Result<GateCancelResult> {
        let gate_id = gate_id.trim();
        if gate_id.is_empty() {
            return Err(gate_error(codes::GATE_INVALID, "gate_id is required"));
        }
        let store = self.store().clone();
        let gate_id_owned = gate_id.to_string();
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            move || store.get_gate(&gate_id_owned)
        })
        .await??
        .ok_or_else(|| {
            gate_error(
                codes::GATE_NOT_FOUND,
                format!("gate {gate_id} does not exist"),
            )
        })?;
        self.ensure_gate_session_live(&existing.gate.session_id, true)
            .await?;
        let note = normalize_note(reason)?;
        let actor = normalize_actor(cancelled_by)?;
        let gate_id_owned = gate_id.to_string();
        let transition = run_gate_store(move || {
            store.cancel_gate(&gate_id_owned, note.as_deref(), actor.as_deref())
        })
        .await?;
        for event in &transition.events {
            let _ = self.events.send(event.clone());
        }
        Ok(GateCancelResult {
            continuation_id: transition.continuation.as_ref().map(|row| row.id.clone()),
            gate: transition.gate.gate,
            changed: transition.changed,
        })
    }

    pub async fn gate_status(&self, gate_id: &str) -> Result<GateStatusResult> {
        let gate_id = gate_id.trim().to_string();
        if gate_id.is_empty() {
            return Err(gate_error(codes::GATE_INVALID, "gate_id is required"));
        }
        let store = self.store().clone();
        let lookup = gate_id.clone();
        let record = tokio::task::spawn_blocking(move || store.get_gate(&lookup))
            .await??
            .ok_or_else(|| {
                gate_error(
                    codes::GATE_NOT_FOUND,
                    format!("gate {gate_id} does not exist"),
                )
            })?;
        let store = self.store().clone();
        let continuation =
            tokio::task::spawn_blocking(move || store.gate_continuation(&gate_id)).await??;
        Ok(GateStatusResult {
            gate: record.gate,
            continuation_id: continuation.as_ref().map(|row| row.id.clone()),
            continuation_status: continuation.map(|row| row.status),
        })
    }

    pub async fn list_gates(
        &self,
        session_id: &str,
        state: Option<&str>,
        limit: Option<u32>,
    ) -> Result<GateListResult> {
        let session_id = session_id.trim().to_string();
        if session_id.is_empty() {
            return Err(gate_error(codes::GATE_INVALID, "session_id is required"));
        }
        let state = match state {
            Some(state) => Some(GateState::parse(state).ok_or_else(|| {
                gate_error(
                    codes::GATE_INVALID,
                    "gate state must be open, resolved, expired, or cancelled",
                )
            })?),
            None => None,
        };
        let limit = limit.unwrap_or(100).clamp(1, 500);
        let store = self.store().clone();
        let lookup = session_id.clone();
        let rows =
            tokio::task::spawn_blocking(move || store.list_gates(&lookup, state, limit)).await??;
        let store = self.store().clone();
        let counts = tokio::task::spawn_blocking({
            let session_id = session_id.clone();
            move || store.gate_counts(&session_id)
        })
        .await??;
        let expected = match state {
            Some(GateState::Open) => counts.open,
            Some(GateState::Resolved) => counts.resolved,
            Some(GateState::Expired) => counts.expired,
            Some(GateState::Cancelled) => counts.cancelled,
            None => counts.total,
        };
        Ok(GateListResult {
            session_id,
            truncated: expected > rows.len() as u64,
            gates: rows.into_iter().map(|record| record.gate).collect(),
            counts,
        })
    }

    pub async fn get_gate_policy(&self, project_id: &str) -> Result<GatePolicy> {
        let store = self.store().clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_gate_policy(&project_id)).await?
    }

    pub async fn set_gate_policy(
        &self,
        project_id: &str,
        policy: GatePolicy,
    ) -> Result<GatePolicy> {
        let store = self.store().clone();
        let project_id = project_id.to_string();
        let saved = policy.clone();
        tokio::task::spawn_blocking(move || store.set_gate_policy(&project_id, &saved)).await??;
        let _ = self.record_project_manifest().await;
        Ok(policy)
    }

    /// Loopback resolve checks the project policy before even verifying the
    /// token. The endpoint is loopback-only and additionally policy-gated; it
    /// never becomes a public webhook.
    pub async fn resolve_gate_loopback(
        &self,
        gate_id: &str,
        token: &str,
        outcome: Option<&str>,
        note: Option<&str>,
    ) -> Result<GateResolveResult> {
        let gate_id = gate_id.trim().to_string();
        let store = self.store().clone();
        let lookup = gate_id.clone();
        let record = tokio::task::spawn_blocking(move || store.get_gate(&lookup))
            .await??
            .ok_or_else(|| {
                gate_error(
                    codes::GATE_NOT_FOUND,
                    format!("gate {gate_id} does not exist"),
                )
            })?;
        let store = self.store().clone();
        let session_id = record.gate.session_id.clone();
        let session = tokio::task::spawn_blocking(move || store.get_session(&session_id))
            .await??
            .ok_or_else(|| gate_error(codes::GATE_NOT_FOUND, "gate session not found"))?;
        let policy = self.get_gate_policy(&session.project_id).await?;
        if !policy.loopback_resolve {
            return Err(gate_error(
                codes::GATE_LOOPBACK_DISABLED,
                "the project has not enabled the loopback gate resolve endpoint",
            ));
        }
        self.resolve_gate(&gate_id, token, outcome, note, Some("loopback"))
            .await
    }

    /// Expire gates whose deadline passed and wake their continuations. The
    /// sweep is bounded and idempotent; it never resolves a gate or retries an
    /// external action.
    pub async fn sweep_expired_gates(&self, limit: u32) -> Result<usize> {
        let now = vp_protocol::now_rfc3339();
        let store = self.store().clone();
        let expired =
            tokio::task::spawn_blocking(move || store.list_expired_open_gates(&now, limit))
                .await??;
        let mut changed = 0usize;
        for record in expired {
            let store = self.store().clone();
            let session_id = record.gate.session_id.clone();
            let session =
                tokio::task::spawn_blocking(move || store.get_session(&session_id)).await??;
            let Some(session) = session else {
                continue;
            };
            if matches!(session.status.as_str(), "imported" | "landed") {
                continue;
            }
            let store = self.store().clone();
            let gate_id = record.gate.id.clone();
            let transition = match tokio::task::spawn_blocking(move || store.expire_gate(&gate_id))
                .await
            {
                Ok(Ok(transition)) => transition,
                Ok(Err(error)) => {
                    tracing::warn!(%error, gate_id = %record.gate.id, "gate expiry transition failed");
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, gate_id = %record.gate.id, "gate expiry task failed");
                    continue;
                }
            };
            if transition.changed {
                changed = changed.saturating_add(1);
            }
            for event in &transition.events {
                let _ = self.events.send(event.clone());
            }
        }
        Ok(changed)
    }

    /// Create a duplication-free continuation that watches an existing open
    /// gate. This is the operator `session.defer --gate` helper and the agent
    /// `wait_for_gate` path.
    pub async fn create_gate_wait(
        &self,
        session_id: &str,
        gate_id: &str,
        note: Option<&str>,
    ) -> Result<(ExternalGate, SessionContinuation)> {
        let gate_id = gate_id.trim();
        if gate_id.is_empty() {
            return Err(gate_error(codes::GATE_INVALID, "gate_id is required"));
        }
        let store = self.store().clone();
        let lookup = gate_id.to_string();
        let record = tokio::task::spawn_blocking(move || store.get_gate(&lookup))
            .await??
            .ok_or_else(|| {
                gate_error(
                    codes::GATE_NOT_FOUND,
                    format!("gate {gate_id} does not exist"),
                )
            })?;
        if record.gate.session_id != session_id {
            return Err(gate_error(
                codes::GATE_NOT_FOUND,
                "the gate does not belong to this session",
            ));
        }
        if !record.gate.is_open() {
            return Err(gate_error(
                codes::GATE_INVALID,
                format!(
                    "gate {gate_id} is already {}; read its outcome instead of waiting",
                    record.gate.state.as_str()
                ),
            ));
        }
        let store = self.store().clone();
        let lookup = gate_id.to_string();
        if tokio::task::spawn_blocking(move || store.has_open_gate_watch(&lookup)).await?? {
            return Err(gate_error(
                codes::GATE_ALREADY_WATCHED,
                format!("gate {gate_id} already has an open continuation watch"),
            ));
        }
        let body = match note.map(str::trim).filter(|note| !note.is_empty()) {
            Some(note) => note.to_string(),
            None => format!(
                "Wait for external gate {} ({}) and continue the session from its outcome.",
                record.gate.id, record.gate.label
            ),
        };
        let body = validate_body(&body)?;
        let continuation = self
            .defer_session_watching_gate(
                session_id,
                &body,
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(gate_id),
                None,
            )
            .await?;
        Ok((record.gate, continuation.continuation))
    }

    async fn ensure_gate_session_live(&self, session_id: &str, allow_landed: bool) -> Result<()> {
        let store = self.store().clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| gate_error(codes::GATE_NOT_FOUND, "gate session not found"))?;
        if session.status == "imported" {
            return Err(gate_error(
                codes::GATE_ARCHIVED,
                "imported sessions are read-only archives",
            ));
        }
        if session.status == "landed" && !allow_landed {
            return Err(gate_error(
                codes::GATE_ARCHIVED,
                "landed sessions are final; cancel an open gate instead of resolving it",
            ));
        }
        Ok(())
    }
}

async fn run_gate_store<F>(f: F) -> Result<GateTransition>
where
    F: FnOnce() -> Result<GateTransition> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await?
        .map_err(map_store_gate_error)
}

fn map_store_gate_error(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<GateError>().is_some() {
        return error;
    }
    let message = format!("{error:#}");
    for code in [
        codes::GATE_NOT_FOUND,
        codes::GATE_TOKEN_INVALID,
        codes::GATE_EXPIRED,
        codes::GATE_CANCELLED,
        codes::GATE_CONFLICT,
    ] {
        if message.starts_with(code) {
            return gate_error(code, message);
        }
    }
    error
}

fn resolve_gate_deadline(
    after_secs: Option<u64>,
    at: Option<&str>,
    now: OffsetDateTime,
) -> Result<Option<String>> {
    if after_secs.is_some() && at.is_some() {
        return Err(gate_error(
            codes::GATE_INVALID,
            "pass either after_secs or an RFC3339 expires_at, not both",
        ));
    }
    let deadline = if let Some(seconds) = after_secs {
        if seconds == 0 {
            return Err(gate_error(
                codes::GATE_INVALID,
                "gate expiry must be in the future",
            ));
        }
        let seconds = i64::try_from(seconds)
            .map_err(|_| gate_error(codes::GATE_INVALID, "gate expiry is too far away"))?;
        if seconds > MAX_DEADLINE_SECS {
            return Err(gate_error(
                codes::GATE_INVALID,
                "gate expiry must be within 30 days",
            ));
        }
        Some(now + time::Duration::seconds(seconds))
    } else if let Some(at) = at {
        let parsed =
            OffsetDateTime::parse(at.trim(), &time::format_description::well_known::Rfc3339)
                .map_err(|_| gate_error(codes::GATE_INVALID, "gate expiry must be RFC3339"))?;
        if parsed <= now {
            return Err(gate_error(
                codes::GATE_INVALID,
                "gate expiry must be in the future",
            ));
        }
        if (parsed - now).whole_seconds() > MAX_DEADLINE_SECS {
            return Err(gate_error(
                codes::GATE_INVALID,
                "gate expiry must be within 30 days",
            ));
        }
        Some(parsed)
    } else {
        None
    };
    deadline
        .map(|deadline| {
            deadline
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|error| {
                    gate_error(
                        codes::GATE_INVALID,
                        format!("formatting gate expiry: {error}"),
                    )
                })
        })
        .transpose()
}

fn normalize_note(note: Option<&str>) -> Result<Option<String>> {
    let Some(note) = note else {
        return Ok(None);
    };
    let note = note.trim();
    if note.is_empty() {
        return Ok(None);
    }
    if note.chars().count() > MAX_GATE_NOTE_CHARS {
        return Err(gate_error(
            codes::GATE_INVALID,
            format!("gate note must be at most {MAX_GATE_NOTE_CHARS} characters"),
        ));
    }
    Ok(Some(note.to_string()))
}

fn normalize_actor(actor: Option<&str>) -> Result<Option<String>> {
    let Some(actor) = actor else {
        return Ok(None);
    };
    let actor = actor.trim();
    if actor.is_empty() {
        return Ok(None);
    }
    if actor.chars().count() > MAX_GATE_ACTOR_CHARS {
        return Err(gate_error(
            codes::GATE_INVALID,
            format!("gate actor must be at most {MAX_GATE_ACTOR_CHARS} characters"),
        ));
    }
    Ok(Some(actor.to_string()))
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

/// Prompt-safe gate summary. It contains no token, token hash, or caller
/// payload; only the typed identity and outcome the model needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatePromptSummary {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub state: String,
    pub outcome: Option<String>,
    pub note: Option<String>,
}

impl GatePromptSummary {
    pub fn from_gate(gate: &ExternalGate) -> Self {
        Self {
            id: gate.id.clone(),
            kind: gate.kind.as_str().to_string(),
            label: truncate_chars(&gate.label, 120),
            state: gate.state.as_str().to_string(),
            outcome: gate.outcome.map(|outcome| outcome.as_str().to_string()),
            note: gate
                .resolution_note
                .as_deref()
                .map(|note| truncate_chars(note, MAX_CONTEXT_NOTE_CHARS.min(2_000))),
        }
    }
}

/// Public projection helpers used by the daemon to build its response without
/// touching the private verifier.
pub fn open_result(outcome: GateOpenOutcome) -> GateOpenResult {
    GateOpenResult {
        gate: outcome.gate,
        token: outcome.token.expose().to_string(),
        resolve_hint: outcome.resolve_hint,
        loopback_resolve: outcome.loopback_resolve,
    }
}

/// Event/response constants re-exported for callers that only depend on the
/// core crate.
pub const OPENED_EVENT: &str = GATE_OPENED_EVENT;
pub const RESOLVED_EVENT: &str = GATE_RESOLVED_EVENT;
pub const EXPIRED_EVENT: &str = GATE_EXPIRED_EVENT;
pub const CANCELLED_EVENT: &str = GATE_CANCELLED_EVENT;
pub const CONTINUATION_READY_EVENT: &str = GATE_CONTINUATION_READY_EVENT;

/// Replay one session's gate records. A malformed payload returns `None` so
/// replay reports drift rather than dropping the gate verifier.
pub fn gates_from_events(events: &[vp_protocol::types::Event]) -> Option<Vec<GateRecord>> {
    vp_store::store::gates_from_events(events)
}

/// Count gates without a store round trip (used by tests/UI helpers).
pub fn counts(gates: &[ExternalGate]) -> GateCounts {
    let mut counts = GateCounts::default();
    for gate in gates {
        counts.total = counts.total.saturating_add(1);
        match gate.state {
            GateState::Open => counts.open = counts.open.saturating_add(1),
            GateState::Resolved => counts.resolved = counts.resolved.saturating_add(1),
            GateState::Expired => counts.expired = counts.expired.saturating_add(1),
            GateState::Cancelled => counts.cancelled = counts.cancelled.saturating_add(1),
        }
    }
    counts
}

/// Bounded summary used by the continuation prompt. Never includes the token
/// or verifier.
pub fn prompt_summary(record: &GateRecord) -> GatePromptSummary {
    GatePromptSummary::from_gate(&record.gate)
}

/// Normalize a JSON payload for tests and callers that need the same bound as
/// `gate.open`.
pub fn validate_payload(payload: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(payload)?;
    if bytes.len() > MAX_GATE_PAYLOAD_BYTES {
        return Err(gate_error(
            codes::GATE_INVALID,
            format!("gate payload must be at most {MAX_GATE_PAYLOAD_BYTES} bytes"),
        ));
    }
    if vp_protocol::types::json_has_sensitive_key(payload) {
        return Err(gate_error(
            codes::GATE_INVALID,
            "gate payload uses a credential-like key; store a label or external id, not a secret",
        ));
    }
    Ok(())
}

/// Continuation delivery helper. It is intentionally read-only and returns
/// `None` when the gate row is missing so the caller can fail the continuation
/// honestly instead of inventing an outcome.
pub async fn gate_for_continuation(
    engine: &Engine,
    continuation: &SessionContinuation,
) -> Result<Option<GateRecord>> {
    let Some(gate_id) = continuation.watch_gate_id.as_deref() else {
        return Ok(None);
    };
    let store = engine.store().clone();
    let gate_id = gate_id.to_string();
    tokio::task::spawn_blocking(move || store.get_gate(&gate_id)).await?
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

/// A defer operation that carries a gate watch. Kept separate from
/// `defer_session` so existing callers remain source-compatible.
pub type GateDeferResult = SessionDeferResult;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_prefixed_and_hash_only() {
        let first = GateToken::generate().unwrap();
        let second = GateToken::generate().unwrap();
        assert!(first.expose().starts_with("vpg_"));
        assert_ne!(first.expose(), second.expose());
        assert_eq!(first.hash().len(), 64);
        assert!(!first.hash().contains(first.expose()));
        assert_eq!(format!("{first:?}"), "GateToken(redacted)");
        assert!(!format!("{first:?}").contains(first.expose()));
        assert!(verify_gate_token(first.expose(), &first.hash()));
        assert!(!verify_gate_token(second.expose(), &first.hash()));
    }

    #[test]
    fn token_parse_rejects_empty_control_and_oversized_values() {
        assert!(GateToken::parse("  ").is_err());
        assert!(GateToken::parse("vpg_\nsecret").is_err());
        assert!(GateToken::parse(&"x".repeat(MAX_GATE_TOKEN_CHARS + 1)).is_err());
        assert_eq!(GateToken::parse(" vpg_ok ").unwrap().expose(), "vpg_ok");
    }

    #[test]
    fn deadlines_are_future_bounded_and_exclusive() {
        let now = OffsetDateTime::parse(
            "2026-11-12T00:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(resolve_gate_deadline(None, None, now).unwrap().is_none());
        assert!(
            resolve_gate_deadline(Some(60), None, now)
                .unwrap()
                .is_some()
        );
        assert!(resolve_gate_deadline(Some(0), None, now).is_err());
        assert!(resolve_gate_deadline(Some((MAX_DEADLINE_SECS + 1) as u64), None, now).is_err());
        assert!(resolve_gate_deadline(Some(60), Some("2026-11-12T01:00:00Z"), now).is_err());
        assert!(resolve_gate_deadline(None, Some("2026-11-11T00:00:00Z"), now).is_err());
        assert!(
            resolve_gate_deadline(None, Some("2026-11-13T00:00:00Z"), now)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn prompt_summary_never_contains_verifier_or_token() {
        let record = GateRecord {
            gate: ExternalGate {
                id: "g1".into(),
                session_id: "s1".into(),
                kind: GateKind::Ci,
                label: "unit tests".into(),
                state: GateState::Resolved,
                outcome: Some(GateOutcome::Resolved),
                resolution_note: Some("green".into()),
                ..ExternalGate::default()
            },
            token_hash: "a".repeat(64),
        };
        let summary = prompt_summary(&record);
        let rendered = format!("{summary:?}");
        assert!(rendered.contains("green"));
        assert!(!rendered.contains(&"a".repeat(64)));
        assert!(!rendered.contains("vpg_"));
    }

    #[test]
    fn payload_bound_is_enforced() {
        validate_payload(&json!({"pr": 42})).unwrap();
        let oversized = json!({"blob": "x".repeat(MAX_GATE_PAYLOAD_BYTES + 1)});
        assert!(validate_payload(&oversized).is_err());
        assert!(validate_payload(&json!({"nested": {"api_key": "not-a-secret"}})).is_err());
    }

    #[test]
    fn counts_are_state_totals() {
        let gate = |state| ExternalGate {
            state,
            ..ExternalGate::default()
        };
        let counts = counts(&[
            gate(GateState::Open),
            gate(GateState::Resolved),
            gate(GateState::Expired),
            gate(GateState::Cancelled),
        ]);
        assert_eq!(counts.total, 4);
        assert_eq!(counts.open, 1);
        assert_eq!(counts.resolved, 1);
        assert_eq!(counts.expired, 1);
        assert_eq!(counts.cancelled, 1);
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use crate::Engine;
    use std::time::Duration;
    use tempfile::TempDir;
    use vp_protocol::types::GateOpenParams;
    use vp_store::Store;

    struct Fixture {
        _data: TempDir,
        _repo: TempDir,
        engine: Engine,
        session_id: String,
    }

    fn init_repo(path: &std::path::Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .unwrap()
                .success()
                .then_some(())
                .unwrap();
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(path.join("README.md"), "gates\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);
    }

    async fn fixture() -> Fixture {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let project = engine
            .store()
            .add_project(repo.path().to_str().unwrap(), Some("gates"))
            .unwrap();
        let session = engine
            .create_session(project.id, "gates".into(), None)
            .await
            .unwrap();
        Fixture {
            _data: data,
            _repo: repo,
            engine,
            session_id: session.id,
        }
    }

    async fn open(fixture: &Fixture, kind: &str, after_secs: Option<u64>) -> GateOpenOutcome {
        fixture
            .engine
            .open_gate(GateOpenParams {
                session_id: fixture.session_id.clone(),
                task_id: None,
                kind: Some(kind.into()),
                label: Some("unit tests".into()),
                payload: Some(json!({"workflow": "ci"})),
                expires_at: None,
                after_secs,
            })
            .await
            .unwrap()
    }

    fn code(error: &anyhow::Error) -> &'static str {
        gate_error_code(error).expect("typed gate error")
    }

    #[tokio::test]
    async fn open_returns_token_once_and_never_stores_or_emits_it() {
        let fixture = fixture().await;
        let outcome = open(&fixture, "ci", None).await;
        let token = outcome.token.expose().to_string();
        let gate_id = outcome.gate.id.clone();
        assert!(token.starts_with("vpg_"));
        assert_eq!(outcome.gate.state, GateState::Open);

        let stored = fixture.engine.store().get_gate(&gate_id).unwrap().unwrap();
        assert_eq!(stored.token_hash, hash_gate_token(&token));
        assert_ne!(stored.token_hash, token);
        assert!(!format!("{stored:?}").contains(&token));
        assert!(
            !serde_json::to_string(&stored.gate)
                .unwrap()
                .contains(&token)
        );
        assert!(
            !serde_json::to_string(&stored.gate)
                .unwrap()
                .contains("token_hash")
        );

        let events = fixture
            .engine
            .list_events(fixture.session_id.clone(), 0, 100)
            .await
            .unwrap();
        let opened = events
            .iter()
            .find(|event| event.kind == OPENED_EVENT)
            .unwrap();
        assert!(
            !serde_json::to_string(&opened.payload)
                .unwrap()
                .contains(&token)
        );
        assert_eq!(opened.payload["token_hash"], hash_gate_token(&token));

        let listed = fixture
            .engine
            .list_gates(&fixture.session_id, None, Some(10))
            .await
            .unwrap();
        let encoded = serde_json::to_string(&listed).unwrap();
        assert!(!encoded.contains(&token));
        assert!(!encoded.contains("token_hash"));
        assert_eq!(listed.counts.open, 1);
    }

    #[tokio::test]
    async fn resolve_is_one_shot_idempotent_and_wrong_token_fails_closed() {
        let fixture = fixture().await;
        let outcome = open(&fixture, "ci", None).await;
        let token = outcome.token.expose().to_string();
        let gate_id = outcome.gate.id.clone();

        let error = fixture
            .engine
            .resolve_gate(&gate_id, "vpg_wrong", None, None, None)
            .await
            .unwrap_err();
        assert_eq!(code(&error), codes::GATE_TOKEN_INVALID);
        assert!(!format!("{error:#}").contains("vpg_wrong"));

        let resolved = fixture
            .engine
            .resolve_gate(
                &gate_id,
                &token,
                Some("resolved"),
                Some("green"),
                Some("ci"),
            )
            .await
            .unwrap();
        assert!(resolved.changed);
        assert_eq!(resolved.gate.state, GateState::Resolved);
        assert_eq!(resolved.gate.outcome, Some(GateOutcome::Resolved));

        let replay = fixture
            .engine
            .resolve_gate(&gate_id, &token, Some("resolved"), None, None)
            .await
            .unwrap();
        assert!(!replay.changed);
        let conflict = fixture
            .engine
            .resolve_gate(&gate_id, &token, Some("rejected"), None, None)
            .await
            .unwrap_err();
        assert_eq!(code(&conflict), codes::GATE_CONFLICT);
        assert!(
            fixture
                .engine
                .resolve_gate(&gate_id, "vpg_wrong", Some("resolved"), None, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cancel_and_expiry_are_terminal_and_wake_the_wait() {
        let fixture = fixture().await;
        let cancelled = open(&fixture, "human", None).await;
        let cancel_token = cancelled.token.expose().to_string();
        let (_, continuation) = fixture
            .engine
            .create_gate_wait(
                &fixture.session_id,
                &cancelled.gate.id,
                Some("resume after review"),
            )
            .await
            .unwrap();
        assert_eq!(continuation.status, "pending");
        let cancel = fixture
            .engine
            .cancel_gate(&cancelled.gate.id, Some("not needed"), Some("operator"))
            .await
            .unwrap();
        assert!(cancel.changed);
        assert_eq!(cancel.gate.state, GateState::Cancelled);
        assert_eq!(
            cancel.continuation_id.as_deref(),
            Some(continuation.id.as_str())
        );
        let error = fixture
            .engine
            .resolve_gate(&cancelled.gate.id, &cancel_token, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(code(&error), codes::GATE_CANCELLED);

        let expiring = open(&fixture, "deploy", Some(1)).await;
        let expiry_token = expiring.token.expose().to_string();
        let (_, continuation) = fixture
            .engine
            .create_gate_wait(&fixture.session_id, &expiring.gate.id, None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1_150)).await;
        let swept = fixture.engine.sweep_expired_gates(10).await.unwrap();
        assert!(swept >= 1);
        let status = fixture.engine.gate_status(&expiring.gate.id).await.unwrap();
        assert_eq!(status.gate.state, GateState::Expired);
        assert_eq!(
            status.continuation_id.as_deref(),
            Some(continuation.id.as_str())
        );
        assert_eq!(status.continuation_status.as_deref(), Some("ready"));
        let error = fixture
            .engine
            .resolve_gate(&expiring.gate.id, &expiry_token, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(code(&error), codes::GATE_EXPIRED);
    }

    #[tokio::test]
    async fn gate_wait_survives_restart_and_has_one_open_watch() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let project = engine
            .store()
            .add_project(repo.path().to_str().unwrap(), Some("gates"))
            .unwrap();
        let session = engine
            .create_session(project.id, "gates".into(), None)
            .await
            .unwrap();
        let outcome = engine
            .open_gate(GateOpenParams {
                session_id: session.id.clone(),
                task_id: None,
                kind: Some("ci".into()),
                label: None,
                payload: None,
                expires_at: None,
                after_secs: None,
            })
            .await
            .unwrap();
        let gate_id = outcome.gate.id.clone();
        let token = outcome.token.expose().to_string();
        let (_, continuation) = engine
            .create_gate_wait(&session.id, &gate_id, Some("restart"))
            .await
            .unwrap();
        let duplicate = engine
            .create_gate_wait(&session.id, &gate_id, None)
            .await
            .unwrap_err();
        assert_eq!(code(&duplicate), codes::GATE_ALREADY_WATCHED);
        drop(engine);

        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let recovered = engine
            .session_continuations(&session.id, None, Some(10))
            .await
            .unwrap();
        assert_eq!(recovered.continuations.len(), 1);
        assert_eq!(recovered.continuations[0].id, continuation.id);
        assert_eq!(
            recovered.continuations[0].watch_gate_id.as_deref(),
            Some(gate_id.as_str())
        );
        assert_eq!(recovered.continuations[0].status, "pending");
        let gate = engine.store().get_gate(&gate_id).unwrap().unwrap();
        assert_eq!(gate.gate.state, GateState::Open);

        let resolved = engine
            .resolve_gate(&gate_id, &token, Some("resolved"), None, Some("ci"))
            .await
            .unwrap();
        assert_eq!(
            resolved.continuation_id.as_deref(),
            Some(continuation.id.as_str())
        );
        let ready = engine
            .session_continuations(&session.id, Some("ready"), Some(10))
            .await
            .unwrap();
        assert_eq!(ready.continuations.len(), 1);
    }

    #[tokio::test]
    async fn gate_resolution_never_resolves_an_approval() {
        let fixture = fixture().await;
        let approval = fixture
            .engine
            .store()
            .create_approval(
                &fixture.session_id,
                "call-1",
                "shell",
                &json!({"command": "make deploy"}),
                Some("host_state"),
                Some("host change"),
            )
            .unwrap();
        let outcome = open(&fixture, "deploy", None).await;
        fixture
            .engine
            .resolve_gate(
                &outcome.gate.id,
                outcome.token.expose(),
                Some("resolved"),
                None,
                Some("ci"),
            )
            .await
            .unwrap();
        let approvals = fixture
            .engine
            .list_approvals(Some(fixture.session_id.clone()))
            .await
            .unwrap();
        let current = approvals.iter().find(|row| row.id == approval.id).unwrap();
        assert_eq!(current.status, "pending");
    }

    #[tokio::test]
    async fn operator_defer_watching_a_gate_does_not_need_the_token() {
        let fixture = fixture().await;
        let outcome = open(&fixture, "pr_review", None).await;
        let deferred = fixture
            .engine
            .defer_session_watching_gate(
                &fixture.session_id,
                "review the PR after approval",
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&outcome.gate.id),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            deferred.continuation.watch_gate_id.as_deref(),
            Some(outcome.gate.id.as_str())
        );
        assert_eq!(deferred.continuation.status, "pending");
        let revealed = fixture
            .engine
            .resolve_gate(
                &outcome.gate.id,
                outcome.token.expose(),
                Some("rejected"),
                Some("changes requested"),
                Some("reviewer"),
            )
            .await
            .unwrap();
        assert!(revealed.changed);
        let ready = fixture
            .engine
            .session_continuations(&fixture.session_id, Some("ready"), Some(10))
            .await
            .unwrap();
        assert_eq!(ready.continuations.len(), 1);
        assert_eq!(ready.continuations[0].wake_reason.as_deref(), Some("gate"));
    }

    #[tokio::test]
    async fn replay_repair_and_bundle_export_include_gates() {
        let fixture = fixture().await;
        let outcome = open(&fixture, "ci", None).await;
        let (_, _) = fixture
            .engine
            .create_gate_wait(&fixture.session_id, &outcome.gate.id, None)
            .await
            .unwrap();
        fixture
            .engine
            .resolve_gate(
                &outcome.gate.id,
                outcome.token.expose(),
                Some("resolved"),
                Some("green"),
                Some("ci"),
            )
            .await
            .unwrap();
        let report = fixture
            .engine
            .replay_session(&fixture.session_id)
            .await
            .unwrap();
        assert!(report.gates_match, "{:?}", report.mismatches);
        assert!(report.continuations_match, "{:?}", report.mismatches);

        fixture
            .engine
            .store()
            .replace_gates(&fixture.session_id, &[])
            .unwrap();
        let report = fixture
            .engine
            .replay_session(&fixture.session_id)
            .await
            .unwrap();
        assert!(!report.gates_match);
        let repaired = fixture
            .engine
            .repair_session(&fixture.session_id)
            .await
            .unwrap();
        assert!(repaired.consistent, "{:?}", repaired.mismatches);
        let gates = fixture
            .engine
            .list_gates(&fixture.session_id, None, Some(10))
            .await
            .unwrap();
        assert_eq!(gates.counts.resolved, 1);

        let bundle = fixture
            .engine
            .export_session(&fixture.session_id)
            .await
            .unwrap();
        assert_eq!(bundle.gates.len(), 1);
        assert_eq!(bundle.gates[0].gate.id, outcome.gate.id);
        assert!(
            !serde_json::to_string(&bundle.gates[0].gate)
                .unwrap()
                .contains("token")
        );
    }

    #[tokio::test]
    async fn open_validates_kind_task_expiry_and_payload() {
        let fixture = fixture().await;
        let base = GateOpenParams {
            session_id: fixture.session_id.clone(),
            task_id: None,
            kind: Some("jenkins".into()),
            label: None,
            payload: None,
            expires_at: None,
            after_secs: None,
        };
        let error = fixture.engine.open_gate(base.clone()).await.unwrap_err();
        assert_eq!(code(&error), codes::GATE_INVALID);

        let mut params = base.clone();
        params.kind = Some("ci".into());
        params.task_id = Some("T-missing".into());
        let error = fixture.engine.open_gate(params).await.unwrap_err();
        assert_eq!(code(&error), codes::GATE_INVALID);

        let mut params = base.clone();
        params.kind = Some("ci".into());
        params.after_secs = Some(60);
        params.expires_at = Some("2026-11-13T00:00:00Z".into());
        let error = fixture.engine.open_gate(params).await.unwrap_err();
        assert_eq!(code(&error), codes::GATE_INVALID);

        let mut params = base;
        params.kind = Some("custom".into());
        params.payload = Some(json!({"blob": "x".repeat(17 * 1024)}));
        let error = fixture.engine.open_gate(params).await.unwrap_err();
        assert_eq!(code(&error), codes::GATE_INVALID);
    }

    #[tokio::test]
    async fn list_status_and_state_filters_are_bounded_and_token_free() {
        let fixture = fixture().await;
        let open_gate = open(&fixture, "ci", None).await;
        let resolved_gate = open(&fixture, "human", None).await;
        fixture
            .engine
            .resolve_gate(
                &resolved_gate.gate.id,
                resolved_gate.token.expose(),
                Some("rejected"),
                Some("not yet"),
                Some("reviewer"),
            )
            .await
            .unwrap();
        let open_only = fixture
            .engine
            .list_gates(&fixture.session_id, Some("open"), Some(10))
            .await
            .unwrap();
        assert_eq!(open_only.gates.len(), 1);
        assert_eq!(open_only.gates[0].id, open_gate.gate.id);
        assert_eq!(open_only.counts.total, 2);
        let status = fixture
            .engine
            .gate_status(&resolved_gate.gate.id)
            .await
            .unwrap();
        assert_eq!(status.gate.outcome, Some(GateOutcome::Rejected));
        assert_eq!(status.gate.resolution_note.as_deref(), Some("not yet"));
        assert!(status.continuation_id.is_none());
        let error = fixture
            .engine
            .list_gates(&fixture.session_id, Some("pending"), None)
            .await
            .unwrap_err();
        assert_eq!(code(&error), codes::GATE_INVALID);
        let encoded = serde_json::to_string(&open_only).unwrap();
        assert!(!encoded.contains(&open_gate.token.expose().to_string()));
        assert!(!encoded.contains("token_hash"));
    }

    #[tokio::test]
    async fn gate_defer_operation_id_replays_the_same_continuation() {
        let fixture = fixture().await;
        let outcome = open(&fixture, "ci", None).await;
        let first = fixture
            .engine
            .defer_session_watching_gate(
                &fixture.session_id,
                "resume after ci",
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&outcome.gate.id),
                Some("gate-defer-op"),
            )
            .await
            .unwrap();
        let second = fixture
            .engine
            .defer_session_watching_gate(
                &fixture.session_id,
                "resume after ci",
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&outcome.gate.id),
                Some("gate-defer-op"),
            )
            .await
            .unwrap();
        assert_eq!(first.continuation.id, second.continuation.id);
        let all = fixture
            .engine
            .session_continuations(&fixture.session_id, None, Some(10))
            .await
            .unwrap();
        assert_eq!(all.continuations.len(), 1);
        let conflict = fixture
            .engine
            .defer_session_watching_gate(
                &fixture.session_id,
                "a different body",
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&outcome.gate.id),
                Some("gate-defer-op"),
            )
            .await
            .unwrap_err();
        assert!(format!("{conflict:#}").contains("different method or payload"));
    }

    #[tokio::test]
    async fn imported_sessions_cannot_open_or_resolve_gates() {
        let fixture = fixture().await;
        let outcome = open(&fixture, "ci", None).await;
        fixture
            .engine
            .update_session_status(&fixture.session_id, "imported")
            .await
            .unwrap();
        let error = fixture
            .engine
            .resolve_gate(
                &outcome.gate.id,
                outcome.token.expose(),
                Some("resolved"),
                None,
                Some("ci"),
            )
            .await
            .unwrap_err();
        assert_eq!(code(&error), codes::GATE_ARCHIVED);
        let error = fixture
            .engine
            .open_gate(GateOpenParams {
                session_id: fixture.session_id.clone(),
                task_id: None,
                kind: Some("ci".into()),
                label: None,
                payload: None,
                expires_at: None,
                after_secs: None,
            })
            .await
            .unwrap_err();
        assert_eq!(code(&error), codes::GATE_ARCHIVED);
    }

    #[tokio::test]
    async fn loopback_resolution_requires_explicit_project_policy() {
        let fixture = fixture().await;
        let outcome = open(&fixture, "ci", None).await;
        let token = outcome.token.expose().to_string();
        let error = fixture
            .engine
            .resolve_gate_loopback(&outcome.gate.id, &token, None, None)
            .await
            .unwrap_err();
        assert_eq!(code(&error), codes::GATE_LOOPBACK_DISABLED);

        let project_id = fixture
            .engine
            .store()
            .get_session(&fixture.session_id)
            .unwrap()
            .unwrap()
            .project_id;
        fixture
            .engine
            .set_gate_policy(
                &project_id,
                GatePolicy {
                    loopback_resolve: true,
                },
            )
            .await
            .unwrap();
        let resolved = fixture
            .engine
            .resolve_gate_loopback(&outcome.gate.id, &token, Some("resolved"), Some("ci"))
            .await
            .unwrap();
        assert_eq!(resolved.gate.state, GateState::Resolved);
        assert!(!serde_json::to_string(&resolved).unwrap().contains(&token));
    }
}
