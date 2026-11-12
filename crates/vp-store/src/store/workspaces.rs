//! Project-scoped Worker workspaces and session attachments (S4).
//!
//! A workspace is the durable accountability record for one Worker identity's
//! component branch. A session attaches the workspace, gets its own session
//! Worker branch/worktree, and Labour children branch from that worktree. The
//! store owns projection rows, the component lease, and one append-only
//! `agent_workspace_events` stream per project. Git itself is owned by
//! `vp_core::agent_workspace`; this module never runs a command.
//!
//! The lease is same-host SQLite coordination. It is not a distributed lock,
//! cross-host consensus, a security credential, or a claim that a workspace
//! process is alive. An expired lease is reclaimed lazily and by the daemon
//! sweep, and a live lease can never be silently reassigned.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vp_protocol::types::{
    AGENT_WORKSPACE_EVENT_KINDS, AGENT_WORKSPACE_MAX_LIST, AGENT_WORKSPACE_STATUSES,
    AgentWorkspace, AgentWorkspaceError, AgentWorkspaceEvent, SessionWorkerWorkspace,
    agent_identity_role, agent_workspace_codes, component_branch_name,
    normalize_workspace_component, normalize_workspace_lease_secs, session_workspace_branch_name,
};

use super::{Store, agent_identity_policy_tx, now_rfc3339};

pub const AGENT_WORKSPACE_EVENT_LIMIT: u32 = 100_000;

/// Bounded mutation result. Events are already persisted; the caller broadcasts
/// them after the transaction commits.
#[derive(Debug, Clone)]
pub struct WorkspaceMutation {
    pub workspace: AgentWorkspace,
    pub session: Option<SessionWorkerWorkspace>,
    pub changed: bool,
    pub events: Vec<AgentWorkspaceEvent>,
}

/// Input for creating or reusing one active workspace. The engine computes the
/// deterministic component/branch/worktree values; the store validates every
/// one again so a malformed RPC cannot create an arbitrary ref.
#[derive(Debug, Clone)]
pub struct PrepareWorkspaceRequest<'a> {
    pub project_id: &'a str,
    pub identity_id: &'a str,
    pub component: Option<&'a str>,
    pub branch: &'a str,
    pub base_commit: &'a str,
    pub worktree_path: &'a str,
    pub session_id: Option<&'a str>,
    pub lease_secs: Option<u64>,
    pub operator: Option<&'a str>,
}

/// Input for attaching a workspace to a session.
#[derive(Debug, Clone)]
pub struct AttachWorkspaceRequest<'a> {
    pub workspace_id: &'a str,
    pub session_id: &'a str,
    pub branch: &'a str,
    pub worktree_path: &'a str,
    pub base_commit: &'a str,
    pub lease_secs: Option<u64>,
    pub operator: Option<&'a str>,
}

fn workspace_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentWorkspaceError::new(code, message).into()
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn format_time(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .map_err(|error| anyhow!("formatting workspace time: {error}"))
}

fn lease_expiry(now: OffsetDateTime, lease_secs: u64) -> Result<String> {
    format_time(now + time::Duration::seconds(lease_secs as i64))
}

fn new_lease_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn validate_operator(operator: Option<&str>) -> Result<Option<String>> {
    let Some(operator) = operator.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if operator.chars().count() > 200 || operator.chars().any(char::is_control) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            "operator label must be 1-200 printable characters",
        ));
    }
    Ok(Some(operator.to_string()))
}

fn validate_reason(reason: Option<&str>) -> Result<Option<String>> {
    let Some(reason) = reason.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if reason.chars().count() > vp_protocol::types::AGENT_WORKSPACE_MAX_REASON_CHARS
        || reason.chars().any(char::is_control)
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!(
                "reason must be 1-{} printable characters",
                vp_protocol::types::AGENT_WORKSPACE_MAX_REASON_CHARS
            ),
        ));
    }
    Ok(Some(reason.to_string()))
}

fn ensure_local_active(workspace: &AgentWorkspace) -> Result<()> {
    if workspace.origin != "local" {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IMPORTED_READONLY,
            "an imported workspace is read-only evidence",
        ));
    }
    if workspace.status != "active" {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_NOT_ACTIVE,
            format!("workspace {} is {}", workspace.id, workspace.status),
        ));
    }
    Ok(())
}

fn project_for_session_tx(tx: &Transaction<'_>, session_id: &str) -> Result<String> {
    tx.query_row(
        "SELECT project_id FROM sessions WHERE id = ?1",
        params![session_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| {
        workspace_error(
            agent_workspace_codes::WORKSPACE_NOT_FOUND,
            format!("session not found: {session_id}"),
        )
    })
}

struct IdentityRow {
    project_id: String,
    role: String,
    status: String,
    origin: String,
}

fn identity_row_tx(tx: &Transaction<'_>, identity_id: &str) -> Result<IdentityRow> {
    tx.query_row(
        "SELECT project_id, role, status, origin FROM agent_identities WHERE id = ?1",
        params![identity_id],
        |row| {
            Ok(IdentityRow {
                project_id: row.get(0)?,
                role: row.get(1)?,
                status: row.get(2)?,
                origin: row.get(3)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| {
        workspace_error(
            agent_workspace_codes::WORKSPACE_NOT_FOUND,
            format!("agent identity not found: {identity_id}"),
        )
    })
}

fn ensure_identity_workspacable(identity: &IdentityRow, project_id: &str) -> Result<()> {
    if identity.project_id != project_id {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
            "identity and workspace belong to different projects",
        ));
    }
    if identity.origin != "local" {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IMPORTED_READONLY,
            "an imported identity is read-only and cannot own a local workspace",
        ));
    }
    if agent_identity_role(&identity.role) != Some("worker") {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IDENTITY_INVALID,
            "only a Worker identity can own a component workspace",
        ));
    }
    match identity.status.as_str() {
        "retired" => Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IDENTITY_RETIRED,
            "a retired Worker identity cannot own or attach a workspace",
        )),
        "paused" => Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IDENTITY_INVALID,
            "a paused Worker identity cannot own or attach a workspace",
        )),
        "active" | "idle" => Ok(()),
        other => Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IDENTITY_INVALID,
            format!("Worker identity has an unsupported status: {other}"),
        )),
    }
}

fn ensure_session_open_tx(tx: &Transaction<'_>, session_id: &str) -> Result<()> {
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?;
    match status.as_deref() {
        Some("imported") => Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IMPORTED_READONLY,
            "an imported session cannot attach a local workspace",
        )),
        Some("landed") => Err(workspace_error(
            agent_workspace_codes::WORKSPACE_NOT_ACTIVE,
            "a landed session is final and cannot attach a workspace",
        )),
        _ => Ok(()),
    }
}

fn ensure_active_assignment_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    identity_id: &str,
) -> Result<()> {
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM session_agent_assignments
             WHERE session_id = ?1 AND identity_id = ?2",
            params![session_id, identity_id],
            |row| row.get(0),
        )
        .optional()?;
    match status.as_deref() {
        Some("active") => Ok(()),
        _ => Err(workspace_error(
            agent_workspace_codes::WORKSPACE_ASSIGNMENT_REQUIRED,
            format!("session {session_id} has no active assignment for identity {identity_id}"),
        )),
    }
}

fn map_workspace(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentWorkspace> {
    let status: String = row.get(7)?;
    let lease_expires_at: Option<String> = row.get(12)?;
    let now = OffsetDateTime::now_utc();
    let lease_active = status == "active"
        && lease_expires_at
            .as_deref()
            .and_then(parse_time)
            .is_some_and(|expires| expires > now);
    Ok(AgentWorkspace {
        id: row.get(0)?,
        project_id: row.get(1)?,
        identity_id: row.get(2)?,
        component: row.get(3)?,
        branch: row.get(4)?,
        base_commit: row.get(5)?,
        worktree_path: row.get(6)?,
        status,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        last_session_id: row.get(10)?,
        lease_active,
        lease_expires_at,
        origin: row.get(13)?,
    })
}

const WORKSPACE_SELECT: &str = "SELECT id, project_id, identity_id, component, branch,
        base_commit, worktree_path, status, created_at, updated_at, last_session_id,
        lease_token, lease_expires_at, origin
     FROM agent_workspaces";

fn map_session_workspace(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionWorkerWorkspace> {
    let status: String = row.get(7)?;
    let lease_expires_at: Option<String> = row.get(11)?;
    let now = OffsetDateTime::now_utc();
    let lease_active = status == "active"
        && lease_expires_at
            .as_deref()
            .and_then(parse_time)
            .is_some_and(|expires| expires > now);
    Ok(SessionWorkerWorkspace {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        session_id: row.get(2)?,
        identity_id: row.get(3)?,
        worktree_path: row.get(4)?,
        branch: row.get(5)?,
        base_commit: row.get(6)?,
        status,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        lease_active,
        lease_expires_at,
    })
}

const SESSION_WORKSPACE_SELECT: &str = "SELECT id, workspace_id, session_id, identity_id,
        worktree_path, branch, base_commit, status, created_at, updated_at, lease_token,
        lease_expires_at
     FROM session_worker_workspaces";

fn get_workspace_tx(tx: &Transaction<'_>, id: &str) -> Result<Option<AgentWorkspace>> {
    tx.query_row(
        &format!("{WORKSPACE_SELECT} WHERE id = ?1"),
        params![id],
        map_workspace,
    )
    .optional()
    .map_err(Into::into)
}

fn get_session_workspace_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    identity_id: &str,
) -> Result<Option<SessionWorkerWorkspace>> {
    tx.query_row(
        &format!("{SESSION_WORKSPACE_SELECT} WHERE session_id = ?1 AND identity_id = ?2"),
        params![session_id, identity_id],
        map_session_workspace,
    )
    .optional()
    .map_err(Into::into)
}

fn active_workspace_for_identity_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    identity_id: &str,
    component: &str,
) -> Result<Option<AgentWorkspace>> {
    tx.query_row(
        &format!(
            "{WORKSPACE_SELECT}
             WHERE project_id = ?1 AND identity_id = ?2 AND component = ?3 AND status = 'active'"
        ),
        params![project_id, identity_id, component],
        map_workspace,
    )
    .optional()
    .map_err(Into::into)
}

fn insert_workspace_event_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    kind: &str,
    workspace: &AgentWorkspace,
    session: Option<&SessionWorkerWorkspace>,
    extra: Option<(&str, serde_json::Value)>,
) -> Result<AgentWorkspaceEvent> {
    if !AGENT_WORKSPACE_EVENT_KINDS.contains(&kind) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID_EVENT,
            format!("unknown workspace event kind: {kind}"),
        ));
    }
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_workspace_events WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    let mut payload = serde_json::json!({"workspace": workspace});
    if let Some(session) = session {
        payload["session"] = serde_json::to_value(session)?;
    }
    if let Some((key, value)) = extra
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(key.to_string(), value);
    }
    let event = AgentWorkspaceEvent {
        id: Uuid::now_v7().to_string(),
        project_id: project_id.to_string(),
        seq: seq.max(1) as u64,
        kind: kind.to_string(),
        payload,
        created_at: now_rfc3339(),
    };
    tx.execute(
        "INSERT INTO agent_workspace_events (id, project_id, seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.kind,
            serde_json::to_string(&event.payload)?,
            event.created_at
        ],
    )?;
    Ok(event)
}

/// Clear one due workspace lease and release its active session binding. The
/// caller must hold an immediate transaction.
fn expire_workspace_if_due_tx(
    tx: &Transaction<'_>,
    workspace_id: &str,
    now: OffsetDateTime,
) -> Result<Option<AgentWorkspaceEvent>> {
    let row: Option<(String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT project_id, lease_token, lease_expires_at FROM agent_workspaces
             WHERE id = ?1 AND status = 'active'",
            params![workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((project_id, lease_token, expires_at)) = row else {
        return Ok(None);
    };
    if lease_token.is_none() {
        return Ok(None);
    }
    let due = expires_at
        .as_deref()
        .and_then(parse_time)
        .is_none_or(|expires| expires <= now);
    if !due {
        return Ok(None);
    }
    let now_text = format_time(now)?;
    let binding: Option<SessionWorkerWorkspace> = tx
        .query_row(
            &format!("{SESSION_WORKSPACE_SELECT} WHERE workspace_id = ?1 AND status = 'active'"),
            params![workspace_id],
            map_session_workspace,
        )
        .optional()?;
    tx.execute(
        "UPDATE agent_workspaces
         SET lease_token = NULL, lease_expires_at = NULL, updated_at = ?1
         WHERE id = ?2",
        params![now_text, workspace_id],
    )?;
    if let Some(binding) = &binding {
        tx.execute(
            "UPDATE session_worker_workspaces
             SET status = 'released', lease_token = NULL, lease_expires_at = NULL, updated_at = ?1
             WHERE id = ?2 AND status = 'active'",
            params![now_text, binding.id],
        )?;
    }
    let workspace = get_workspace_tx(tx, workspace_id)?
        .ok_or_else(|| anyhow!("workspace disappeared during lease expiry"))?;
    let released_session = match &binding {
        Some(binding) => get_session_workspace_tx(tx, &binding.session_id, &binding.identity_id)?,
        None => None,
    };
    insert_workspace_event_tx(
        tx,
        &project_id,
        vp_protocol::types::AGENT_WORKSPACE_LEASE_EXPIRED_EVENT,
        &workspace,
        released_session.as_ref(),
        Some(("reason", serde_json::json!("lease_expired"))),
    )
    .map(Some)
}

fn expire_workspaces_for_component_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    component: &str,
    now: OffsetDateTime,
) -> Result<Vec<AgentWorkspaceEvent>> {
    let ids = {
        let mut statement = tx.prepare(
            "SELECT id FROM agent_workspaces
             WHERE project_id = ?1 AND component = ?2 AND status = 'active'
               AND lease_token IS NOT NULL",
        )?;
        statement
            .query_map(params![project_id, component], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut events = Vec::new();
    for id in ids {
        if let Some(event) = expire_workspace_if_due_tx(tx, &id, now)? {
            events.push(event);
        }
    }
    Ok(events)
}

fn live_lease_owner_tx(
    tx: &Transaction<'_>,
    workspace_id: &str,
    now: OffsetDateTime,
) -> Result<Option<Option<String>>> {
    let row: Option<(Option<String>, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT lease_token, lease_expires_at, last_session_id
             FROM agent_workspaces WHERE id = ?1",
            params![workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((token, expires_at, last_session_id)) = row else {
        return Ok(None);
    };
    if token.is_none() {
        return Ok(None);
    }
    if expires_at
        .as_deref()
        .and_then(parse_time)
        .is_some_and(|expires| expires > now)
    {
        let session: Option<String> = tx
            .query_row(
                "SELECT session_id FROM session_worker_workspaces
                 WHERE workspace_id = ?1 AND status = 'active'",
                params![workspace_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(Some(session.or(last_session_id)))
    } else {
        Ok(None)
    }
}

fn component_lease_holder_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    component: &str,
    except_workspace_id: &str,
    now: OffsetDateTime,
) -> Result<Option<(String, Option<String>)>> {
    let rows = {
        let mut statement = tx.prepare(
            "SELECT id, lease_expires_at FROM agent_workspaces
             WHERE project_id = ?1 AND component = ?2 AND status = 'active'
               AND lease_token IS NOT NULL AND id <> ?3",
        )?;
        statement
            .query_map(params![project_id, component, except_workspace_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (id, expires_at) in rows {
        if expires_at
            .as_deref()
            .and_then(parse_time)
            .is_some_and(|expires| expires > now)
        {
            let session: Option<String> = tx
                .query_row(
                    "SELECT session_id FROM session_worker_workspaces
                     WHERE workspace_id = ?1 AND status = 'active'",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?;
            return Ok(Some((id, session)));
        }
    }
    Ok(None)
}

/// Reject a row whose branch would not be the deterministic branch for its
/// identity/component. This is the store-level guard against an arbitrary ref
/// entering through an import or a future caller.
fn validate_workspace_row(workspace: &AgentWorkspace) -> Result<()> {
    if workspace.id.is_empty()
        || workspace.project_id.is_empty()
        || workspace.identity_id.is_empty()
        || workspace.component.is_empty()
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            "workspace is missing an id, project, identity, or component",
        ));
    }
    if !AGENT_WORKSPACE_STATUSES.contains(&workspace.status.as_str()) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!("workspace {} has an invalid status", workspace.id),
        ));
    }
    if !matches!(workspace.origin.as_str(), "local" | "imported") {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!("workspace {} has an invalid origin", workspace.id),
        ));
    }
    let component = normalize_workspace_component(&workspace.component)?;
    let expected = component_branch_name(&workspace.identity_id, &component);
    if workspace.branch != expected {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
            format!(
                "workspace {} branch is not the deterministic component branch",
                workspace.id
            ),
        ));
    }
    if !std::path::Path::new(&workspace.worktree_path).is_absolute() {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
            "workspace worktree path must be absolute",
        ));
    }
    if workspace.base_commit.trim().is_empty() {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            "workspace base commit is required",
        ));
    }
    Ok(())
}

fn validate_session_row(session: &SessionWorkerWorkspace) -> Result<()> {
    if session.id.is_empty()
        || session.workspace_id.is_empty()
        || session.session_id.is_empty()
        || session.identity_id.is_empty()
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            "session workspace is missing an id, workspace, session, or identity",
        ));
    }
    if !vp_protocol::types::AGENT_SESSION_WORKSPACE_STATUSES.contains(&session.status.as_str()) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!("session workspace {} has an invalid status", session.id),
        ));
    }
    let expected = session_workspace_branch_name(&session.session_id, &session.identity_id);
    if session.branch != expected {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
            format!(
                "session workspace {} branch is not the deterministic session branch",
                session.id
            ),
        ));
    }
    if !std::path::Path::new(&session.worktree_path).is_absolute() {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
            "session workspace worktree path must be absolute",
        ));
    }
    Ok(())
}

pub(crate) fn insert_workspace_row_tx(
    tx: &Transaction<'_>,
    workspace: &AgentWorkspace,
) -> Result<()> {
    validate_workspace_row(workspace)?;
    tx.execute(
        "INSERT INTO agent_workspaces
         (id, project_id, identity_id, component, branch, base_commit, worktree_path,
          status, created_at, updated_at, last_session_id, lease_token, lease_expires_at, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, NULL, ?12)
         ON CONFLICT(id) DO UPDATE SET
            component = excluded.component,
            branch = excluded.branch,
            base_commit = excluded.base_commit,
            worktree_path = excluded.worktree_path,
            status = excluded.status,
            updated_at = excluded.updated_at,
            last_session_id = excluded.last_session_id,
            lease_token = NULL,
            lease_expires_at = NULL,
            origin = excluded.origin",
        params![
            workspace.id,
            workspace.project_id,
            workspace.identity_id,
            workspace.component,
            workspace.branch,
            workspace.base_commit,
            workspace.worktree_path,
            workspace.status,
            workspace.created_at,
            workspace.updated_at,
            workspace.last_session_id,
            workspace.origin
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_session_row_tx(
    tx: &Transaction<'_>,
    session: &SessionWorkerWorkspace,
) -> Result<()> {
    validate_session_row(session)?;
    tx.execute(
        "INSERT INTO session_worker_workspaces
         (id, workspace_id, session_id, identity_id, worktree_path, branch, base_commit,
          status, created_at, updated_at, lease_token, lease_expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL)
         ON CONFLICT(session_id, identity_id) DO UPDATE SET
            workspace_id = excluded.workspace_id,
            worktree_path = excluded.worktree_path,
            branch = excluded.branch,
            base_commit = excluded.base_commit,
            status = excluded.status,
            updated_at = excluded.updated_at,
            lease_token = NULL,
            lease_expires_at = NULL",
        params![
            session.id,
            session.workspace_id,
            session.session_id,
            session.identity_id,
            session.worktree_path,
            session.branch,
            session.base_commit,
            session.status,
            session.created_at,
            session.updated_at
        ],
    )?;
    Ok(())
}

impl Store {
    // ---------------------------------------------------------------------
    // Workspace rows and leases
    // ---------------------------------------------------------------------

    pub fn get_agent_workspace(&self, workspace_id: &str) -> Result<Option<AgentWorkspace>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("{WORKSPACE_SELECT} WHERE id = ?1"),
            params![workspace_id],
            map_workspace,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_session_worker_workspace(
        &self,
        session_id: &str,
        identity_id: &str,
    ) -> Result<Option<SessionWorkerWorkspace>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("{SESSION_WORKSPACE_SELECT} WHERE session_id = ?1 AND identity_id = ?2"),
            params![session_id, identity_id],
            map_session_workspace,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_agent_workspaces(
        &self,
        project_id: Option<&str>,
        identity_id: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspace>> {
        if project_id.is_none() && identity_id.is_none() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID,
                "list workspaces requires a project or identity filter",
            ));
        }
        if let Some(status) = status
            && !AGENT_WORKSPACE_STATUSES.contains(&status)
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID,
                format!("workspace status must be active, archived, or retired: {status}"),
            ));
        }
        let limit = limit
            .unwrap_or(AGENT_WORKSPACE_MAX_LIST)
            .clamp(1, AGENT_WORKSPACE_MAX_LIST) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{WORKSPACE_SELECT}
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR identity_id = ?2)
               AND (?3 IS NULL OR status = ?3)
             ORDER BY updated_at DESC, id ASC
             LIMIT ?4"
        ))?;
        let rows = statement.query_map(
            params![project_id, identity_id, status, limit],
            map_workspace,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_worker_workspaces(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionWorkerWorkspace>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{SESSION_WORKSPACE_SELECT} WHERE session_id = ?1 ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = statement.query_map(params![session_id], map_session_workspace)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_workspaces_for_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<SessionWorkerWorkspace>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{SESSION_WORKSPACE_SELECT} WHERE workspace_id = ?1 ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = statement.query_map(params![workspace_id], map_session_workspace)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Create or reuse one active workspace and acquire its lease.
    ///
    /// Idempotent for the same identity/component. A live lease held by another
    /// session, or a live component lease held by another workspace, is a typed
    /// `WORKSPACE_BUSY` conflict. An expired lease is recorded as
    /// `agent.workspace.lease_expired` before the new lease is acquired.
    pub fn prepare_agent_workspace(
        &self,
        request: PrepareWorkspaceRequest<'_>,
    ) -> Result<WorkspaceMutation> {
        let lease_secs = normalize_workspace_lease_secs(request.lease_secs)?;
        let operator = validate_operator(request.operator)?;
        let component = match request.component {
            Some(component) => normalize_workspace_component(component)?,
            None => {
                let conn = self.lock()?;
                conn.query_row(
                    "SELECT component FROM agent_identities WHERE id = ?1",
                    params![request.identity_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .unwrap_or_else(|| "worker".to_string())
            }
        };
        let component = normalize_workspace_component(&component)?;
        let expected_branch = component_branch_name(request.identity_id, &component);
        if request.branch != expected_branch {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
                "branch must be the deterministic component branch",
            ));
        }
        if !std::path::Path::new(request.worktree_path).is_absolute() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
                "workspace worktree path must be absolute",
            ));
        }
        if request.base_commit.trim().is_empty() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID,
                "workspace base commit is required",
            ));
        }
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = request
            .session_id
            .map(|_| lease_expiry(now, lease_secs))
            .transpose()?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = identity_row_tx(&tx, request.identity_id)?;
        ensure_identity_workspacable(&identity, request.project_id)?;
        let policy = agent_identity_policy_tx(&tx, request.project_id)?;
        if !policy.enabled {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_POLICY_DISABLED,
                "project agent identities are disabled; enable the policy before creating workspaces",
            ));
        }
        if let Some(session_id) = request.session_id {
            if project_for_session_tx(&tx, session_id)? != request.project_id {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                    "session and workspace belong to different projects",
                ));
            }
            ensure_session_open_tx(&tx, session_id)?;
            ensure_active_assignment_tx(&tx, session_id, request.identity_id)?;
        }
        let existing = active_workspace_for_identity_tx(
            &tx,
            request.project_id,
            request.identity_id,
            &component,
        )?;
        let mut events =
            expire_workspaces_for_component_tx(&tx, request.project_id, &component, now)?;
        if let Some(holder) = component_lease_holder_tx(
            &tx,
            request.project_id,
            &component,
            existing
                .as_ref()
                .map(|workspace| workspace.id.as_str())
                .unwrap_or(""),
            now,
        )? {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BUSY,
                format!(
                    "component {} is leased by workspace {}{}",
                    component,
                    holder.0,
                    holder
                        .1
                        .as_deref()
                        .map(|session| format!(" (session {session})"))
                        .unwrap_or_default()
                ),
            ));
        }
        let mut created = false;
        let workspace = match existing {
            Some(workspace) => {
                if workspace.origin != "local" {
                    return Err(workspace_error(
                        agent_workspace_codes::WORKSPACE_IMPORTED_READONLY,
                        "an imported workspace is read-only",
                    ));
                }
                if let Some(owner) = live_lease_owner_tx(&tx, &workspace.id, now)? {
                    match (owner.as_deref(), request.session_id) {
                        (Some(owner), Some(session)) if owner == session => {}
                        (Some(owner), _) => {
                            return Err(workspace_error(
                                agent_workspace_codes::WORKSPACE_BUSY,
                                format!(
                                    "workspace {} is leased by session {}",
                                    workspace.id, owner
                                ),
                            ));
                        }
                        (None, Some(_)) => {
                            return Err(workspace_error(
                                agent_workspace_codes::WORKSPACE_BUSY,
                                format!("workspace {} is leased by an operator", workspace.id),
                            ));
                        }
                        (None, None) => {}
                    }
                }
                tx.execute(
                    "UPDATE agent_workspaces
                     SET branch = ?1, base_commit = ?2, worktree_path = ?3, updated_at = ?4,
                         last_session_id = COALESCE(?5, last_session_id),
                         lease_token = ?6, lease_expires_at = ?7
                     WHERE id = ?8",
                    params![
                        request.branch,
                        request.base_commit,
                        request.worktree_path,
                        now_text,
                        request.session_id,
                        request.session_id.map(|_| new_lease_token()),
                        expires,
                        workspace.id
                    ],
                )?;
                get_workspace_tx(&tx, &workspace.id)?
                    .ok_or_else(|| anyhow!("workspace disappeared during prepare"))?
            }
            None => {
                let workspace = AgentWorkspace {
                    id: Uuid::now_v7().to_string(),
                    project_id: request.project_id.to_string(),
                    identity_id: request.identity_id.to_string(),
                    component: component.clone(),
                    branch: request.branch.to_string(),
                    base_commit: request.base_commit.to_string(),
                    worktree_path: request.worktree_path.to_string(),
                    status: "active".to_string(),
                    created_at: now_text.clone(),
                    updated_at: now_text.clone(),
                    last_session_id: request.session_id.map(str::to_string),
                    lease_active: request.session_id.is_some(),
                    lease_expires_at: expires.clone(),
                    origin: "local".to_string(),
                };
                validate_workspace_row(&workspace)?;
                tx.execute(
                    "INSERT INTO agent_workspaces
                     (id, project_id, identity_id, component, branch, base_commit, worktree_path,
                      status, created_at, updated_at, last_session_id, lease_token,
                      lease_expires_at, origin)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'local')",
                    params![
                        workspace.id,
                        workspace.project_id,
                        workspace.identity_id,
                        workspace.component,
                        workspace.branch,
                        workspace.base_commit,
                        workspace.worktree_path,
                        workspace.status,
                        workspace.created_at,
                        workspace.updated_at,
                        workspace.last_session_id,
                        request.session_id.map(|_| new_lease_token()),
                        expires
                    ],
                )?;
                created = true;
                let stored = get_workspace_tx(&tx, &workspace.id)?
                    .ok_or_else(|| anyhow!("workspace disappeared after creation"))?;
                events.push(insert_workspace_event_tx(
                    &tx,
                    request.project_id,
                    vp_protocol::types::AGENT_WORKSPACE_CREATED_EVENT,
                    &stored,
                    None,
                    Some(("operator", serde_json::json!(operator))),
                )?);
                stored
            }
        };
        tx.commit()?;
        Ok(WorkspaceMutation {
            workspace,
            session: None,
            changed: created,
            events,
        })
    }

    /// Attach a prepared workspace to a session. The active assignment is
    /// required; the binding is one row per `(session, identity)` and is
    /// idempotently reactivated on a later run.
    pub fn attach_session_workspace(
        &self,
        request: AttachWorkspaceRequest<'_>,
    ) -> Result<WorkspaceMutation> {
        let lease_secs = normalize_workspace_lease_secs(request.lease_secs)?;
        let operator = validate_operator(request.operator)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = lease_expiry(now, lease_secs)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_session_open_tx(&tx, request.session_id)?;
        let project_id = project_for_session_tx(&tx, request.session_id)?;
        let workspace = get_workspace_tx(&tx, request.workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {}", request.workspace_id),
            )
        })?;
        if workspace.project_id != project_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                "workspace and session belong to different projects",
            ));
        }
        ensure_local_active(&workspace)?;
        ensure_active_assignment_tx(&tx, request.session_id, &workspace.identity_id)?;
        let identity = identity_row_tx(&tx, &workspace.identity_id)?;
        ensure_identity_workspacable(&identity, &project_id)?;
        let expected_branch =
            session_workspace_branch_name(request.session_id, &workspace.identity_id);
        if request.branch != expected_branch {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
                "branch must be the deterministic session workspace branch",
            ));
        }
        if !std::path::Path::new(request.worktree_path).is_absolute() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
                "session worktree path must be absolute",
            ));
        }
        let mut events =
            expire_workspaces_for_component_tx(&tx, &project_id, &workspace.component, now)?;
        if let Some(event) = expire_workspace_if_due_tx(&tx, &workspace.id, now)? {
            events.push(event);
        }
        if let Some(owner) = live_lease_owner_tx(&tx, &workspace.id, now)? {
            match owner.as_deref() {
                Some(owner) if owner == request.session_id => {}
                Some(owner) => {
                    return Err(workspace_error(
                        agent_workspace_codes::WORKSPACE_BUSY,
                        format!("workspace {} is leased by session {}", workspace.id, owner),
                    ));
                }
                None => {
                    return Err(workspace_error(
                        agent_workspace_codes::WORKSPACE_BUSY,
                        format!("workspace {} is leased by an operator", workspace.id),
                    ));
                }
            }
        }
        if let Some(holder) =
            component_lease_holder_tx(&tx, &project_id, &workspace.component, &workspace.id, now)?
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BUSY,
                format!(
                    "component {} is leased by workspace {}{}",
                    workspace.component,
                    holder.0,
                    holder
                        .1
                        .as_deref()
                        .map(|session| format!(" (session {session})"))
                        .unwrap_or_default()
                ),
            ));
        }
        let existing = get_session_workspace_tx(&tx, request.session_id, &workspace.identity_id)?;
        if let Some(existing) = &existing
            && existing.workspace_id != workspace.id
            && existing.status == "active"
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_CONFLICT,
                format!(
                    "session already has an active workspace {} for this identity",
                    existing.workspace_id
                ),
            ));
        }
        match existing {
            Some(existing) => {
                tx.execute(
                    "UPDATE session_worker_workspaces
                     SET workspace_id = ?1, worktree_path = ?2, branch = ?3, base_commit = ?4,
                         status = 'active', updated_at = ?5, lease_token = ?6, lease_expires_at = ?7
                     WHERE id = ?8",
                    params![
                        workspace.id,
                        request.worktree_path,
                        request.branch,
                        request.base_commit,
                        now_text,
                        new_lease_token(),
                        expires,
                        existing.id
                    ],
                )?;
            }
            None => {
                let binding = SessionWorkerWorkspace {
                    id: Uuid::now_v7().to_string(),
                    workspace_id: workspace.id.clone(),
                    session_id: request.session_id.to_string(),
                    identity_id: workspace.identity_id.clone(),
                    worktree_path: request.worktree_path.to_string(),
                    branch: request.branch.to_string(),
                    base_commit: request.base_commit.to_string(),
                    status: "active".to_string(),
                    created_at: now_text.clone(),
                    updated_at: now_text.clone(),
                    lease_active: true,
                    lease_expires_at: Some(expires.clone()),
                };
                validate_session_row(&binding)?;
                tx.execute(
                    "INSERT INTO session_worker_workspaces
                     (id, workspace_id, session_id, identity_id, worktree_path, branch,
                      base_commit, status, created_at, updated_at, lease_token, lease_expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11)",
                    params![
                        binding.id,
                        binding.workspace_id,
                        binding.session_id,
                        binding.identity_id,
                        binding.worktree_path,
                        binding.branch,
                        binding.base_commit,
                        binding.created_at,
                        binding.updated_at,
                        new_lease_token(),
                        expires
                    ],
                )?;
            }
        }
        tx.execute(
            "UPDATE agent_workspaces
             SET lease_token = ?1, lease_expires_at = ?2, last_session_id = ?3, updated_at = ?4
             WHERE id = ?5",
            params![
                new_lease_token(),
                expires,
                request.session_id,
                now_text,
                workspace.id
            ],
        )?;
        let workspace = get_workspace_tx(&tx, &workspace.id)?
            .ok_or_else(|| anyhow!("workspace disappeared during attach"))?;
        let binding = get_session_workspace_tx(&tx, request.session_id, &workspace.identity_id)?
            .ok_or_else(|| anyhow!("session workspace disappeared during attach"))?;
        events.push(insert_workspace_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_WORKSPACE_SESSION_ATTACHED_EVENT,
            &workspace,
            Some(&binding),
            Some(("operator", serde_json::json!(operator))),
        )?);
        tx.commit()?;
        Ok(WorkspaceMutation {
            workspace,
            session: Some(binding),
            changed: true,
            events,
        })
    }

    /// Explicitly release one session binding and its workspace lease. This is
    /// idempotent and never archives or deletes evidence.
    pub fn release_session_worker_workspace(
        &self,
        session_id: &str,
        identity_id: Option<&str>,
        reason: &str,
        operator: Option<&str>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        let operator = validate_operator(operator)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let bindings = {
            let mut statement = tx.prepare(&format!(
                "{SESSION_WORKSPACE_SELECT}
                 WHERE session_id = ?1 AND status = 'active'
                   AND (?2 IS NULL OR identity_id = ?2)"
            ))?;
            statement
                .query_map(params![session_id, identity_id], map_session_workspace)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let now_text = now_rfc3339();
        let mut events = Vec::new();
        for binding in bindings {
            tx.execute(
                "UPDATE session_worker_workspaces
                 SET status = 'released', lease_token = NULL, lease_expires_at = NULL,
                     updated_at = ?1
                 WHERE id = ?2 AND status = 'active'",
                params![now_text, binding.id],
            )?;
            tx.execute(
                "UPDATE agent_workspaces
                 SET lease_token = NULL, lease_expires_at = NULL, updated_at = ?1
                 WHERE id = ?2 AND project_id = ?3",
                params![now_text, binding.workspace_id, project_id],
            )?;
            let released = get_session_workspace_tx(&tx, session_id, &binding.identity_id)?
                .ok_or_else(|| anyhow!("session workspace disappeared during release"))?;
            let workspace = get_workspace_tx(&tx, &binding.workspace_id)?.ok_or_else(|| {
                anyhow!(
                    "workspace disappeared during release: {}",
                    binding.workspace_id
                )
            })?;
            events.push(insert_workspace_event_tx(
                &tx,
                &project_id,
                vp_protocol::types::AGENT_WORKSPACE_RELEASED_EVENT,
                &workspace,
                Some(&released),
                Some((
                    "operator",
                    serde_json::json!({
                        "operator": operator,
                        "reason": reason,
                    }),
                )),
            )?);
        }
        tx.commit()?;
        Ok(events)
    }

    /// Record one internal Labour→Worker merge. The Git outcome is passed in by
    /// the engine; this method only persists bounded evidence and never treats
    /// a failed merge as a success.
    #[allow(clippy::too_many_arguments)]
    pub fn record_workspace_labour_merge(
        &self,
        workspace_id: &str,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        commit: &str,
        pre_merge: &str,
        post_merge: &str,
        merged: bool,
        paths: &[String],
        detail: &str,
    ) -> Result<AgentWorkspaceEvent> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let workspace = get_workspace_tx(&tx, workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {workspace_id}"),
            )
        })?;
        if workspace.project_id != project_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                "workspace and session belong to different projects",
            ));
        }
        let binding = get_session_workspace_tx(&tx, session_id, &workspace.identity_id)?;
        let bounded_paths = paths
            .iter()
            .take(vp_protocol::types::AGENT_WORKSPACE_MAX_PATHS)
            .cloned()
            .collect::<Vec<_>>();
        let event = insert_workspace_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_WORKSPACE_LABOUR_MERGED_EVENT,
            &workspace,
            binding.as_ref(),
            Some((
                "merge",
                serde_json::json!({
                    "agent_id": agent_id,
                    "task_id": task_id,
                    "commit": commit,
                    "pre_merge": pre_merge,
                    "post_merge": post_merge,
                    "merged": merged,
                    "paths": bounded_paths,
                    "detail": detail.chars().take(2_000).collect::<String>(),
                }),
            )),
        )?;
        tx.commit()?;
        Ok(event)
    }

    /// Persist the accepted component-branch commit after an explicit land.
    /// Git has already succeeded; this transition is the durable evidence.
    pub fn record_workspace_landed(
        &self,
        workspace_id: &str,
        session_id: &str,
        previous_component_commit: &str,
        landed_commit: &str,
        commits: &[String],
        operator: Option<&str>,
    ) -> Result<AgentWorkspaceEvent> {
        let operator = validate_operator(operator)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let workspace = get_workspace_tx(&tx, workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {workspace_id}"),
            )
        })?;
        if workspace.project_id != project_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                "workspace and session belong to different projects",
            ));
        }
        ensure_local_active(&workspace)?;
        let now_text = now_rfc3339();
        tx.execute(
            "UPDATE agent_workspaces
             SET base_commit = ?1, last_session_id = ?2, updated_at = ?3,
                 lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?4",
            params![landed_commit, session_id, now_text, workspace_id],
        )?;
        tx.execute(
            "UPDATE session_worker_workspaces
             SET status = 'released', lease_token = NULL, lease_expires_at = NULL,
                 updated_at = ?1
             WHERE session_id = ?2 AND identity_id = ?3 AND status = 'active'",
            params![now_text, session_id, workspace.identity_id],
        )?;
        let workspace = get_workspace_tx(&tx, workspace_id)?
            .ok_or_else(|| anyhow!("workspace disappeared during land"))?;
        let binding = get_session_workspace_tx(&tx, session_id, &workspace.identity_id)?;
        let event = insert_workspace_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_WORKSPACE_LANDED_EVENT,
            &workspace,
            binding.as_ref(),
            Some((
                "land",
                serde_json::json!({
                    "previous_component_commit": previous_component_commit,
                    "landed_commit": landed_commit,
                    "commits": commits
                        .iter()
                        .take(vp_protocol::types::AGENT_WORKSPACE_MAX_PATHS)
                        .cloned()
                        .collect::<Vec<_>>(),
                    "operator": operator,
                }),
            )),
        )?;
        tx.commit()?;
        Ok(event)
    }

    /// Archive a workspace after release. A live lease or an active binding
    /// fails closed; evidence (branch, commits, paths, events) is preserved.
    pub fn archive_agent_workspace(
        &self,
        workspace_id: &str,
        retire: bool,
        operator: Option<&str>,
        reason: Option<&str>,
    ) -> Result<WorkspaceMutation> {
        let operator = validate_operator(operator)?;
        let reason = validate_reason(reason)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = get_workspace_tx(&tx, workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {workspace_id}"),
            )
        })?;
        ensure_local_active(&workspace)?;
        if let Some(owner) = live_lease_owner_tx(&tx, workspace_id, now)? {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BUSY,
                format!(
                    "workspace {} still has a live lease{}; release it before archiving",
                    workspace_id,
                    owner
                        .as_deref()
                        .map(|session| format!(" (session {session})"))
                        .unwrap_or_default()
                ),
            ));
        }
        let active_binding: Option<String> = tx
            .query_row(
                "SELECT session_id FROM session_worker_workspaces
                 WHERE workspace_id = ?1 AND status = 'active'",
                params![workspace_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(session_id) = active_binding {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BUSY,
                format!(
                    "workspace {} still has an active session binding ({session_id}); release it first",
                    workspace_id
                ),
            ));
        }
        let status = if retire { "retired" } else { "archived" };
        tx.execute(
            "UPDATE agent_workspaces
             SET status = ?1, lease_token = NULL, lease_expires_at = NULL, updated_at = ?2
             WHERE id = ?3",
            params![status, now_text, workspace_id],
        )?;
        tx.execute(
            "UPDATE session_worker_workspaces
             SET status = 'archived', lease_token = NULL, lease_expires_at = NULL, updated_at = ?1
             WHERE workspace_id = ?2 AND status = 'released'",
            params![now_text, workspace_id],
        )?;
        let workspace = get_workspace_tx(&tx, workspace_id)?
            .ok_or_else(|| anyhow!("workspace disappeared during archive"))?;
        let kind = if retire {
            vp_protocol::types::AGENT_WORKSPACE_RETIRED_EVENT
        } else {
            vp_protocol::types::AGENT_WORKSPACE_ARCHIVED_EVENT
        };
        let event = insert_workspace_event_tx(
            &tx,
            &workspace.project_id,
            kind,
            &workspace,
            None,
            Some((
                "operator",
                serde_json::json!({
                    "operator": operator,
                    "reason": reason,
                }),
            )),
        )?;
        tx.commit()?;
        Ok(WorkspaceMutation {
            workspace,
            session: None,
            changed: true,
            events: vec![event],
        })
    }

    /// Expire every due workspace lease. The periodic daemon sweep and startup
    /// reconciliation use the same operation.
    pub fn expire_agent_workspace_leases(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        self.expire_agent_workspace_leases_at(OffsetDateTime::now_utc(), limit)
    }

    pub fn expire_agent_workspace_leases_at(
        &self,
        now: OffsetDateTime,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        let limit = limit.unwrap_or(100).clamp(1, 1_000) as i64;
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = tx.prepare(
                "SELECT id FROM agent_workspaces
                 WHERE status = 'active' AND lease_token IS NOT NULL
                   AND (lease_expires_at IS NULL
                        OR julianday(lease_expires_at) <= julianday(?1))
                 ORDER BY lease_expires_at ASC, id ASC
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![now_text, limit], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::with_capacity(rows.len());
        for id in rows {
            if let Some(event) = expire_workspace_if_due_tx(&tx, &id, now)? {
                events.push(event);
            }
        }
        tx.commit()?;
        Ok(events)
    }

    /// Release workspace leases for sessions that are no longer live. Called on
    /// startup and by the periodic coordination sweep so a daemon crash cannot
    /// pin a component forever.
    pub fn reconcile_agent_workspace_leases(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        let limit = limit.unwrap_or(100).clamp(1, 1_000) as i64;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = tx.prepare(
                "SELECT w.id, b.session_id, s.status
                 FROM agent_workspaces w
                 JOIN session_worker_workspaces b ON b.workspace_id = w.id
                      AND b.status = 'active'
                 JOIN sessions s ON s.id = b.session_id
                 WHERE w.status = 'active'
                   AND s.status NOT IN ('running', 'queued', 'paused', 'waiting_input')
                 ORDER BY w.updated_at ASC
                 LIMIT ?1",
            )?;
            statement
                .query_map(params![limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::new();
        for (workspace_id, session_id, status) in rows {
            tx.execute(
                "UPDATE session_worker_workspaces
                 SET status = 'released', lease_token = NULL, lease_expires_at = NULL,
                     updated_at = ?1
                 WHERE workspace_id = ?2 AND session_id = ?3 AND status = 'active'",
                params![now_text, workspace_id, session_id],
            )?;
            tx.execute(
                "UPDATE agent_workspaces
                 SET lease_token = NULL, lease_expires_at = NULL, updated_at = ?1
                 WHERE id = ?2",
                params![now_text, workspace_id],
            )?;
            let workspace = get_workspace_tx(&tx, &workspace_id)?
                .ok_or_else(|| anyhow!("workspace disappeared during reconciliation"))?;
            let binding: Option<SessionWorkerWorkspace> = tx
                .query_row(
                    &format!(
                        "{SESSION_WORKSPACE_SELECT} WHERE workspace_id = ?1 AND session_id = ?2"
                    ),
                    params![workspace_id, session_id],
                    map_session_workspace,
                )
                .optional()?;
            events.push(insert_workspace_event_tx(
                &tx,
                &workspace.project_id,
                vp_protocol::types::AGENT_WORKSPACE_RELEASED_EVENT,
                &workspace,
                binding.as_ref(),
                Some((
                    "reason",
                    serde_json::json!({
                        "reason": "session_inactive",
                        "session_status": status,
                    }),
                )),
            )?);
        }
        drop(tx);
        drop(conn);
        let mut expired = self.expire_agent_workspace_leases(Some(limit.max(1) as u32))?;
        events.append(&mut expired);
        Ok(events)
    }

    // ---------------------------------------------------------------------
    // Replay, repair, manifest, and bundle snapshots
    // ---------------------------------------------------------------------

    pub fn list_agent_workspace_events_for_project(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        let limit = limit
            .unwrap_or(AGENT_WORKSPACE_EVENT_LIMIT)
            .clamp(1, AGENT_WORKSPACE_EVENT_LIMIT) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, project_id, seq, kind, payload, created_at
             FROM agent_workspace_events
             WHERE project_id = ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id, limit], map_workspace_event)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_agent_workspaces_for_replay(
        &self,
        project_id: &str,
    ) -> Result<Vec<AgentWorkspace>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{WORKSPACE_SELECT} WHERE project_id = ?1 ORDER BY id ASC LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            params![project_id, AGENT_WORKSPACE_EVENT_LIMIT as i64],
            map_workspace,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_workspaces_for_replay(
        &self,
        project_id: &str,
    ) -> Result<Vec<SessionWorkerWorkspace>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT s.id, s.workspace_id, s.session_id, s.identity_id, s.worktree_path,
                    s.branch, s.base_commit, s.status, s.created_at, s.updated_at,
                    s.lease_token, s.lease_expires_at
             FROM session_worker_workspaces s
             JOIN agent_workspaces w ON w.id = s.workspace_id
             WHERE w.project_id = ?1
             ORDER BY s.session_id ASC, s.identity_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![project_id, AGENT_WORKSPACE_EVENT_LIMIT as i64],
            map_session_workspace,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn insert_agent_workspace_event_snapshot(&self, event: &AgentWorkspaceEvent) -> Result<()> {
        if !AGENT_WORKSPACE_EVENT_KINDS.contains(&event.kind.as_str()) {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID_EVENT,
                format!("unknown workspace event kind: {}", event.kind),
            ));
        }
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO agent_workspace_events (id, project_id, seq, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.project_id,
                event.seq as i64,
                event.kind,
                serde_json::to_string(&event.payload)?,
                event.created_at
            ],
        )?;
        Ok(())
    }

    pub fn insert_agent_workspace_snapshot(&self, workspace: &AgentWorkspace) -> Result<()> {
        validate_workspace_row(workspace)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_exists: Option<String> = tx
            .query_row(
                "SELECT id FROM projects WHERE id = ?1",
                params![workspace.project_id],
                |row| row.get(0),
            )
            .optional()?;
        if project_exists.is_none() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("project not found: {}", workspace.project_id),
            ));
        }
        insert_workspace_row_tx(&tx, workspace)?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_session_worker_workspace_snapshot(
        &self,
        session: &SessionWorkerWorkspace,
    ) -> Result<()> {
        validate_session_row(session)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = get_workspace_tx(&tx, &session.workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {}", session.workspace_id),
            )
        })?;
        if workspace.identity_id != session.identity_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_CONFLICT,
                "session workspace identity does not match its workspace",
            ));
        }
        insert_session_row_tx(&tx, session)?;
        tx.commit()?;
        Ok(())
    }

    /// Replace the project projection from replayed events. Never appends an
    /// event. All rows are derived; live lease tokens are not replayed and are
    /// cleared, so callers must only repair a quiescent project.
    pub fn replace_agent_workspace_projection(
        &self,
        project_id: &str,
        workspaces: &[AgentWorkspace],
        sessions: &[SessionWorkerWorkspace],
    ) -> Result<()> {
        let mut workspace_ids = BTreeSet::new();
        for workspace in workspaces {
            if workspace.project_id != project_id {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                    format!(
                        "replayed workspace {} belongs to another project",
                        workspace.id
                    ),
                ));
            }
            validate_workspace_row(workspace)?;
            workspace_ids.insert(workspace.id.clone());
        }
        for session in sessions {
            if !workspace_ids.contains(&session.workspace_id) {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_CONFLICT,
                    format!(
                        "replayed session workspace {} references a missing workspace",
                        session.id
                    ),
                ));
            }
            validate_session_row(session)?;
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM session_worker_workspaces WHERE workspace_id IN
                (SELECT id FROM agent_workspaces WHERE project_id = ?1)",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM agent_workspaces WHERE project_id = ?1",
            params![project_id],
        )?;
        for workspace in workspaces {
            insert_workspace_row_tx(&tx, workspace)?;
        }
        for session in sessions {
            insert_session_row_tx(&tx, session)?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn map_workspace_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentWorkspaceEvent> {
    let payload: String = row.get(4)?;
    Ok(AgentWorkspaceEvent {
        id: row.get(0)?,
        project_id: row.get(1)?,
        seq: row.get::<_, i64>(2)?.max(0) as u64,
        kind: row.get(3)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        created_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{AgentIdentityPolicy, CreateAgentIdentityParams};

    fn store() -> (TempDir, Store, String, String) {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        for (key, value) in [
            ("user.email", "workspace-store@test.local"),
            ("user.name", "Workspace Store"),
        ] {
            let status = std::process::Command::new("git")
                .args(["config", key, value])
                .current_dir(&repo)
                .status()
                .unwrap();
            assert!(status.success());
        }
        std::fs::write(repo.join("README.md"), "store\n").unwrap();
        let status = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args(["commit", "-q", "-m", "initial"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        let store = Store::open(dir.path()).unwrap();
        let project = store
            .add_project(repo.to_str().unwrap(), Some("workspace store"))
            .unwrap();
        store
            .set_agent_identity_policy(
                &project.id,
                &AgentIdentityPolicy {
                    enabled: true,
                    ..AgentIdentityPolicy::default()
                },
            )
            .unwrap();
        let identity = store
            .create_agent_identity(&CreateAgentIdentityParams {
                project_id: project.id.clone(),
                role: "worker".to_string(),
                component: Some("auth".to_string()),
                title: Some("Auth Worker".to_string()),
                profile: Some("coding".to_string()),
                ..CreateAgentIdentityParams::default()
            })
            .unwrap()
            .0
            .id;
        let session = store.create_session(&project.id, "workspace").unwrap();
        store
            .create_session_agent_assignment(&session.id, &identity, None, None, None)
            .unwrap();
        let _head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        (
            dir,
            store,
            project.id,
            format!("{}:{}", session.id, identity),
        )
    }

    fn ids(raw: &str) -> (String, String) {
        let (session, identity) = raw.split_once(':').unwrap();
        (session.to_string(), identity.to_string())
    }

    fn prepare<'a>(
        project_id: &'a str,
        identity_id: &'a str,
        session_id: Option<&'a str>,
        go: &'a str,
    ) -> PrepareWorkspaceRequest<'a> {
        PrepareWorkspaceRequest {
            project_id,
            identity_id,
            component: Some("auth"),
            branch: Box::leak(component_branch_name(identity_id, "auth").into_boxed_str()),
            base_commit: Box::leak(go.to_string().into_boxed_str()),
            worktree_path: "/tmp/verlinpark-workspace",
            session_id,
            lease_secs: Some(60),
            operator: Some("test"),
        }
    }

    #[test]
    fn prepares_attaches_and_rejects_a_second_session() {
        let (dir, store, project, raw) = store();
        let (session, identity) = ids(&raw);
        let go = git_head(dir.path().join("repo"));
        let prepared = store
            .prepare_agent_workspace(prepare(&project, &identity, Some(&session), &go))
            .unwrap();
        assert!(prepared.changed);
        assert_eq!(prepared.events.len(), 1);
        assert_eq!(prepared.events[0].kind, "agent.workspace.created");
        let workspace_id = prepared.workspace.id.clone();
        let branch = session_workspace_branch_name(&session, &identity);
        let worktree = dir.path().join("session-worktree");
        let attach = store
            .attach_session_workspace(AttachWorkspaceRequest {
                workspace_id: &workspace_id,
                session_id: &session,
                branch: &branch,
                worktree_path: worktree.to_str().unwrap(),
                base_commit: &go,
                lease_secs: Some(60),
                operator: Some("test"),
            })
            .unwrap();
        assert_eq!(attach.events[0].kind, "agent.workspace.session_attached");
        let binding = attach.session.unwrap();
        assert_eq!(binding.workspace_id, workspace_id);
        assert_eq!(binding.status, "active");

        // A second session cannot attach the same workspace.
        let other = store.create_session(&project, "other").unwrap();
        store
            .create_session_agent_assignment(&other.id, &identity, None, None, None)
            .unwrap_err();
        let second_identity = store
            .create_agent_identity(&CreateAgentIdentityParams {
                project_id: project.clone(),
                role: "worker".to_string(),
                component: Some("auth".to_string()),
                ..CreateAgentIdentityParams::default()
            })
            .unwrap()
            .0
            .id;
        let other_session = store.create_session(&project, "other-2").unwrap();
        store
            .create_session_agent_assignment(&other_session.id, &second_identity, None, None, None)
            .unwrap();
        let error = store
            .prepare_agent_workspace(prepare(
                &project,
                &second_identity,
                Some(&other_session.id),
                &go,
            ))
            .unwrap_err();
        let typed = error.downcast_ref::<AgentWorkspaceError>().unwrap();
        assert_eq!(typed.code, agent_workspace_codes::WORKSPACE_BUSY);
    }

    #[test]
    fn expires_releases_and_archives_without_deleting_evidence() {
        let (dir, store, project, raw) = store();
        let (session, identity) = ids(&raw);
        let go = git_head(dir.path().join("repo"));
        let prepared = store
            .prepare_agent_workspace(prepare(&project, &identity, Some(&session), &go))
            .unwrap();
        let workspace_id = prepared.workspace.id.clone();
        let branch = session_workspace_branch_name(&session, &identity);
        let worktree = dir.path().join("session-worktree");
        store
            .attach_session_workspace(AttachWorkspaceRequest {
                workspace_id: &workspace_id,
                session_id: &session,
                branch: &branch,
                worktree_path: worktree.to_str().unwrap(),
                base_commit: &go,
                lease_secs: Some(60),
                operator: None,
            })
            .unwrap();
        let expired = store
            .expire_agent_workspace_leases_at(
                OffsetDateTime::now_utc() + time::Duration::seconds(120),
                Some(10),
            )
            .unwrap();
        assert_eq!(expired.len(), 1);
        let workspace = store.get_agent_workspace(&workspace_id).unwrap().unwrap();
        assert!(!workspace.lease_active);
        let binding = store
            .get_session_worker_workspace(&session, &identity)
            .unwrap()
            .unwrap();
        assert_eq!(binding.status, "released");
        let archived = store
            .archive_agent_workspace(&workspace_id, false, Some("operator"), Some("done"))
            .unwrap();
        assert_eq!(archived.workspace.status, "archived");
        assert_eq!(archived.events[0].kind, "agent.workspace.archived");
        let retired = store
            .archive_agent_workspace(&workspace_id, true, Some("operator"), None)
            .unwrap_err();
        assert!(retired.to_string().contains("WORKSPACE_NOT_ACTIVE"));
        let rows = store
            .list_agent_workspaces(Some(&project), None, None, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "archived");
    }

    #[test]
    fn releases_an_active_binding_explicitly() {
        let (dir, store, project, raw) = store();
        let (session, identity) = ids(&raw);
        let go = git_head(dir.path().join("repo"));
        let prepared = store
            .prepare_agent_workspace(prepare(&project, &identity, Some(&session), &go))
            .unwrap();
        let workspace_id = prepared.workspace.id.clone();
        let branch = session_workspace_branch_name(&session, &identity);
        let worktree = dir.path().join("session-worktree");
        store
            .attach_session_workspace(AttachWorkspaceRequest {
                workspace_id: &workspace_id,
                session_id: &session,
                branch: &branch,
                worktree_path: worktree.to_str().unwrap(),
                base_commit: &go,
                lease_secs: Some(60),
                operator: None,
            })
            .unwrap();
        let events = store
            .release_session_worker_workspace(&session, None, "session_ended", None)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "agent.workspace.released");
        assert!(
            !store
                .get_agent_workspace(&workspace_id)
                .unwrap()
                .unwrap()
                .lease_active
        );
        let binding = store
            .get_session_worker_workspace(&session, &identity)
            .unwrap()
            .unwrap();
        assert_eq!(binding.status, "released");
        // Idempotent.
        assert!(
            store
                .release_session_worker_workspace(&session, None, "again", None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn manifest_carries_workspaces_bindings_and_events() {
        let (dir, store, project, raw) = store();
        let (session, identity) = ids(&raw);
        let go = git_head(dir.path().join("repo"));
        let prepared = store
            .prepare_agent_workspace(prepare(&project, &identity, Some(&session), &go))
            .unwrap();
        let branch = session_workspace_branch_name(&session, &identity);
        store
            .attach_session_workspace(AttachWorkspaceRequest {
                workspace_id: &prepared.workspace.id,
                session_id: &session,
                branch: &branch,
                worktree_path: "/tmp/verlinpark-session-worktree",
                base_commit: &go,
                lease_secs: Some(60),
                operator: None,
            })
            .unwrap();
        store.record_project_manifest().unwrap();
        let snapshots = super::super::read_project_manifest(store.data_dir()).unwrap();
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.project.id == project)
            .expect("project manifest snapshot");
        assert_eq!(snapshot.agent_workspaces.len(), 1);
        assert_eq!(snapshot.session_worker_workspaces.len(), 1);
        assert!(!snapshot.agent_workspace_events.is_empty());
        assert_eq!(
            snapshot.agent_workspaces[0].branch,
            prepared.workspace.branch
        );
        assert_eq!(
            snapshot.agent_workspace_events[0].project_id,
            snapshot.project.id
        );
    }

    fn git_head(path: std::path::PathBuf) -> String {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(path)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }
}
