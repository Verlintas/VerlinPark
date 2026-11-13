//! S8 portable team charter storage, apply links, and audit replay.
//!
//! The portable payload lives in `team_templates.json`; `project_team_templates`
//! records an explicit operator apply; `team_template_events` is a bounded
//! project audit stream. Applying a template never creates an identity. It
//! stores one template row, one link, and pending S6 hire requests, all in one
//! transaction. A re-apply of the same content hash is idempotent.
//!
//! Security invariants:
//!
//! - the stored JSON is validated by the core layer before this module sees it;
//! - events carry ids, names, hashes, and counts only, never the template body;
//! - no context, memory, inbox, worktree, branch, session, performance, MCP
//!   schema, path, URL, or credential column exists here;
//! - a pending hire request remains subject to S6 approval, caps, hierarchy,
//!   and the project identity policy.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use uuid::Uuid;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    AGENT_HIRE_REQUESTED_EVENT, AgentHireParams, AgentHireRequest, AgentIdentityError,
    ProjectTeamTemplate, TEAM_TEMPLATE_APPLIED_EVENT, TEAM_TEMPLATE_EVENT_KINDS,
    TEAM_TEMPLATE_EXPORTED_EVENT, TEAM_TEMPLATE_IMPORTED_EVENT, TEAM_TEMPLATE_MAX_EVENTS,
    TEAM_TEMPLATE_MAX_OPERATOR_CHARS, TeamTemplate, TeamTemplateEntry, TeamTemplateEvent,
    TeamTemplateRecord, team_template_codes,
};

use super::Store;
use super::employment::{
    build_hire_request, insert_employment_event_tx, insert_hire_request_tx,
    validate_hire_preflight_tx,
};
use super::normalize_identity_text;

/// One planned pending hire. It is a slot proposal, not an identity. The core
/// layer has already resolved the existing-parent id and the target title.
#[derive(Debug, Clone)]
pub struct PendingTemplateHire {
    pub slot: String,
    pub role: String,
    pub department: Option<String>,
    pub component: Option<String>,
    pub title: String,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub parent_identity_id: Option<String>,
}

fn template_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentIdentityError::new(code, message).into()
}

fn project_exists_tx(tx: &Transaction<'_>, project_id: &str) -> Result<()> {
    if project_id.trim().is_empty() {
        return Err(template_error(
            team_template_codes::TEMPLATE_PROJECT_NOT_FOUND,
            "project_id is required",
        ));
    }
    let exists: Option<String> = tx
        .query_row(
            "SELECT id FROM projects WHERE id = ?1",
            params![project_id.trim()],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(template_error(
            team_template_codes::TEMPLATE_PROJECT_NOT_FOUND,
            format!("project not found: {}", project_id.trim()),
        ));
    }
    Ok(())
}

fn normalize_operator(value: Option<&str>) -> Result<String> {
    let value = value.unwrap_or("operator").trim();
    if value.is_empty() {
        return Ok("operator".to_string());
    }
    if value.chars().count() > TEAM_TEMPLATE_MAX_OPERATOR_CHARS {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("operator label must be at most {TEAM_TEMPLATE_MAX_OPERATOR_CHARS} characters"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            "operator label must not contain control characters",
        ));
    }
    Ok(value.to_string())
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TeamTemplateRecord> {
    let raw: String = row.get(5)?;
    let template: TeamTemplate = serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(TeamTemplateRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        version: row.get(2)?,
        schema_version: row.get::<_, i64>(3)? as u32,
        content_hash: row.get(4)?,
        template,
        created_at: row.get(6)?,
        source_project_id: row.get(7)?,
    })
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<TeamTemplateEvent> {
    let payload: String = row.get(8)?;
    let payload: Value = serde_json::from_str(&payload).unwrap_or_else(|_| serde_json::json!({}));
    Ok(TeamTemplateEvent {
        id: row.get(0)?,
        project_id: row.get(1)?,
        seq: row.get::<_, i64>(2)? as u64,
        kind: row.get(3)?,
        template_id: row.get(4)?,
        name: row.get(5)?,
        version: row.get(6)?,
        content_hash: row.get(7)?,
        payload,
        created_at: row.get(9)?,
    })
}

/// Insert one bounded audit event. Payload construction is deliberately a
/// small allowlisted object; callers cannot pass a template body through.
fn insert_team_template_event_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    kind: &str,
    template: &TeamTemplate,
    payload: Value,
) -> Result<TeamTemplateEvent> {
    if !TEAM_TEMPLATE_EVENT_KINDS.contains(&kind) {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("unknown team template event kind: {kind}"),
        ));
    }
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM team_template_events WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    let event = TeamTemplateEvent {
        id: Uuid::now_v7().to_string(),
        project_id: project_id.to_string(),
        seq: seq.max(0) as u64,
        kind: kind.to_string(),
        template_id: template.template_id.clone(),
        name: template.name.clone(),
        version: template.version.clone(),
        content_hash: template.content_hash.clone(),
        payload,
        created_at: now_rfc3339(),
    };
    tx.execute(
        "INSERT INTO team_template_events
         (id, project_id, seq, kind, template_id, name, version, content_hash, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.kind,
            event.template_id,
            event.name,
            event.version,
            event.content_hash,
            serde_json::to_string(&event.payload)?,
            event.created_at
        ],
    )?;
    Ok(event)
}

fn insert_team_template_row_tx(
    tx: &Transaction<'_>,
    template: &TeamTemplate,
    source_project_id: Option<&str>,
) -> Result<()> {
    let encoded = serde_json::to_string(template)?;
    tx.execute(
        "INSERT INTO team_templates
         (id, name, version, schema_version, content_hash, json, created_at, source_project_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            template.template_id,
            template.name,
            template.version,
            template.schema_version as i64,
            template.content_hash,
            encoded,
            now_rfc3339(),
            source_project_id
        ],
    )?;
    Ok(())
}

fn template_row_exists_tx(
    tx: &Transaction<'_>,
    template: &TeamTemplate,
) -> Result<Option<TeamTemplateRecord>> {
    tx.query_row(
        "SELECT id, name, version, schema_version, content_hash, json, created_at, source_project_id
         FROM team_templates WHERE id = ?1",
        params![template.template_id],
        map_record,
    )
    .optional()
    .map_err(Into::into)
}

fn entry_for_template(
    store: &Store,
    record: &TeamTemplateRecord,
    project_id: Option<&str>,
) -> Result<TeamTemplateEntry> {
    let conn = store.lock()?;
    let (applied_at, applied_by): (Option<String>, Option<String>) = match project_id {
        Some(project_id) => conn
            .query_row(
                "SELECT applied_at, applied_by FROM project_team_templates
                 WHERE project_id = ?1 AND template_id = ?2",
                params![project_id, record.id],
                |row| Ok((Some(row.get(0)?), row.get(1)?)),
            )
            .optional()?
            .unwrap_or((None, None)),
        None => (None, None),
    };
    let read_only = applied_at.is_none()
        && match project_id {
            Some(project) => record.source_project_id.as_deref() != Some(project),
            None => true,
        };
    // Never leak another project's local id through a shared library entry.
    let source_project = match project_id {
        Some(project) if record.source_project_id.as_deref() != Some(project) => None,
        _ => record.source_project_id.clone(),
    };
    Ok(TeamTemplateEntry {
        template_id: record.id.clone(),
        name: record.name.clone(),
        version: record.version.clone(),
        schema_version: record.schema_version,
        content_hash: record.content_hash.clone(),
        created_at: record.created_at.clone(),
        source_project,
        applied: applied_at.is_some(),
        applied_at,
        applied_by,
        read_only,
    })
}

/// Register a template in the project library. This is a metadata write only:
/// it never creates an identity, hire request, link, session, or policy update.
/// The exact same id/hash is idempotent; a same-id different body fails closed.
pub fn register_team_template(
    store: &Store,
    project_id: &str,
    template: &TeamTemplate,
    source_project_id: Option<&str>,
    event_kind: &str,
) -> Result<(TeamTemplateEntry, TeamTemplateEvent, bool)> {
    let project_id = project_id.trim();
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    project_exists_tx(&tx, project_id)?;
    if event_kind != TEAM_TEMPLATE_EXPORTED_EVENT && event_kind != TEAM_TEMPLATE_IMPORTED_EVENT {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            "register expects exported or imported as its event kind",
        ));
    }
    let changed = match template_row_exists_tx(&tx, template)? {
        Some(existing) => {
            if existing.content_hash != template.content_hash
                || existing.name != template.name
                || existing.version != template.version
                || existing.template != *template
            {
                return Err(template_error(
                    team_template_codes::TEMPLATE_APPLY_CONFLICT,
                    format!(
                        "template {} already exists with a different body",
                        template.template_id
                    ),
                ));
            }
            false
        }
        None => {
            insert_team_template_row_tx(&tx, template, source_project_id)?;
            true
        }
    };
    // A global template row can already exist from another project's export.
    // Registration is still a per-project event: without it, a later scoped
    // list/get/diff has no evidence that this project imported the template.
    let project_event_exists: bool = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM team_template_events
            WHERE project_id = ?1 AND template_id = ?2 AND kind = ?3)",
        params![project_id, template.template_id, event_kind],
        |row| row.get(0),
    )?;
    let changed = changed || !project_event_exists;
    let event = if changed {
        insert_team_template_event_tx(
            &tx,
            project_id,
            event_kind,
            template,
            serde_json::json!({
                "template_id": template.template_id,
                "name": template.name,
                "version": template.version,
                "content_hash": template.content_hash,
                "slot_count": 1 + template.departments.len(),
            }),
        )?
    } else {
        TeamTemplateEvent::default()
    };
    tx.commit()?;
    drop(conn);
    let record = get_team_template_record(store, &template.template_id)?.ok_or_else(|| {
        template_error(
            team_template_codes::TEMPLATE_NOT_FOUND,
            format!(
                "template not found after registration: {}",
                template.template_id
            ),
        )
    })?;
    let entry = entry_for_template(store, &record, Some(project_id))?;
    Ok((entry, event, changed))
}

/// Apply a validated template. The core layer owns validation, preview, model
/// and tool checks, and slot resolution. This function owns the transaction:
///
/// 1. store the template row if it is new (or verify the existing body);
/// 2. if a project link exists, return the existing entry unchanged;
/// 3. create pending S6 hire requests only (never an identity);
/// 4. insert the project link and one `team.template.applied` event.
pub fn apply_team_template(
    store: &Store,
    project_id: &str,
    template: &TeamTemplate,
    source_project_id: Option<&str>,
    operator: Option<&str>,
    hires: &[PendingTemplateHire],
) -> Result<(TeamTemplateEntry, Vec<String>, bool)> {
    let project_id = project_id.trim();
    let operator = normalize_operator(operator)?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    project_exists_tx(&tx, project_id)?;

    match template_row_exists_tx(&tx, template)? {
        Some(existing) => {
            if existing.content_hash != template.content_hash
                || existing.name != template.name
                || existing.version != template.version
                || existing.template != *template
            {
                return Err(template_error(
                    team_template_codes::TEMPLATE_APPLY_CONFLICT,
                    format!(
                        "template {} already exists with a different body",
                        template.template_id
                    ),
                ));
            }
        }
        None => insert_team_template_row_tx(&tx, template, source_project_id)?,
    }

    let already_applied: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT applied_at, applied_by FROM project_team_templates
             WHERE project_id = ?1 AND template_id = ?2",
            params![project_id, template.template_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let was_applied = already_applied.is_some();
    if was_applied && hires.is_empty() {
        tx.commit()?;
        drop(conn);
        let record = get_team_template_record(store, &template.template_id)?.ok_or_else(|| {
            template_error(
                team_template_codes::TEMPLATE_NOT_FOUND,
                format!("template not found: {}", template.template_id),
            )
        })?;
        return Ok((
            entry_for_template(store, &record, Some(project_id))?,
            Vec::new(),
            false,
        ));
    }

    for hire in hires {
        let slot = normalize_identity_text(
            Some(hire.slot.as_str()),
            "slot",
            vp_protocol::types::TEAM_TEMPLATE_MAX_SLOT_ID_CHARS,
        )?
        .unwrap_or_default();
        let policy = serde_json::json!({
            "team_template": {
                "template_id": template.template_id,
                "version": template.version,
                "content_hash": template.content_hash,
                "slot": slot,
            }
        });
        let params = AgentHireParams {
            project_id: project_id.to_string(),
            role: hire.role.clone(),
            department: hire.department.clone(),
            component: hire.component.clone(),
            title: Some(hire.title.clone()),
            profile: hire.profile.clone(),
            model: hire.model.clone(),
            parent_identity_id: hire.parent_identity_id.clone(),
            policy: Some(policy),
            approved: false,
            operator: Some(operator.clone()),
        };
        let mut request = build_hire_request(
            &params,
            Uuid::now_v7().to_string(),
            &operator,
            &now_rfc3339(),
        )?;
        // Apply is not consent to hire. Force the pending state regardless of
        // any future project auto-approval policy; S6 approval creates the
        // identity explicitly.
        request.status = "pending".to_string();
        request.decided_at = None;
        request.decided_by = None;
        validate_hire_preflight_tx(&tx, &request)?;
        assert_no_matching_hire_tx(&tx, &request)?;
        insert_hire_request_tx(&tx, &request)?;
        insert_employment_event_tx(
            &tx,
            project_id,
            &request.identity_id,
            AGENT_HIRE_REQUESTED_EVENT,
            serde_json::json!({
                "request": request,
                "approval_required": true,
                "approval_risk": "host_state",
                "source": "team_template",
                "template_id": template.template_id,
                "template_version": template.version,
                "template_content_hash": template.content_hash,
            }),
        )?;
    }

    if !was_applied {
        let applied_at = now_rfc3339();
        tx.execute(
            "INSERT INTO project_team_templates (project_id, template_id, applied_at, applied_by)
             VALUES (?1, ?2, ?3, ?4)",
            params![project_id, template.template_id, applied_at, operator],
        )?;
    }
    insert_team_template_event_tx(
        &tx,
        project_id,
        TEAM_TEMPLATE_APPLIED_EVENT,
        template,
        serde_json::json!({
            "template_id": template.template_id,
            "name": template.name,
            "version": template.version,
            "content_hash": template.content_hash,
            "pending_hire_count": hires.len(),
            "applied_by": operator,
        }),
    )?;
    tx.commit()?;
    drop(conn);
    let request_ids = {
        let conn = store.lock()?;
        let mut statement = conn.prepare(
            "SELECT id FROM agent_hire_requests
             WHERE project_id = ?1 AND status = 'pending'
               AND json_extract(policy_json, '$.team_template.template_id') = ?2
             ORDER BY requested_at ASC, id ASC",
        )?;
        statement
            .query_map(params![project_id, template.template_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let record = get_team_template_record(store, &template.template_id)?.ok_or_else(|| {
        template_error(
            team_template_codes::TEMPLATE_NOT_FOUND,
            format!("template not found after apply: {}", template.template_id),
        )
    })?;
    Ok((
        entry_for_template(store, &record, Some(project_id))?,
        request_ids,
        true,
    ))
}

/// Refuse a second pending request for an exact same slot. This is defense in
/// depth; the core preview already classifies existing identities/requests.
fn assert_no_matching_hire_tx(tx: &Transaction<'_>, request: &AgentHireRequest) -> Result<()> {
    let existing_identity: Option<String> = tx
        .query_row(
            "SELECT id FROM agent_identities
             WHERE project_id = ?1 AND role = ?2 AND status <> 'retired'
               AND COALESCE(department, '') = COALESCE(?3, '')
               AND COALESCE(component, '') = COALESCE(?4, '')
               AND COALESCE(title, '') = COALESCE(?5, '')
             ORDER BY created_at ASC, id ASC LIMIT 1",
            params![
                request.project_id,
                request.role,
                request.department,
                request.component,
                request.title
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(identity_id) = existing_identity {
        return Err(template_error(
            team_template_codes::TEMPLATE_APPLY_CONFLICT,
            format!(
                "slot {} already has identity {identity_id}; resolve it explicitly instead of creating a duplicate hire request",
                request.title.clone().unwrap_or_default()
            ),
        ));
    }
    let pending: Option<String> = tx
        .query_row(
            "SELECT id FROM agent_hire_requests
             WHERE project_id = ?1 AND status = 'pending' AND role = ?2
               AND COALESCE(department, '') = COALESCE(?3, '')
               AND COALESCE(component, '') = COALESCE(?4, '')
               AND COALESCE(title, '') = COALESCE(?5, '')
             ORDER BY requested_at ASC, id ASC LIMIT 1",
            params![
                request.project_id,
                request.role,
                request.department,
                request.component,
                request.title
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(request_id) = pending {
        return Err(template_error(
            team_template_codes::TEMPLATE_APPLY_CONFLICT,
            format!(
                "slot {} already has pending hire request {request_id}",
                request.title.clone().unwrap_or_default()
            ),
        ));
    }
    Ok(())
}

pub fn get_team_template_record(
    store: &Store,
    template_id: &str,
) -> Result<Option<TeamTemplateRecord>> {
    let conn = store.lock()?;
    conn.query_row(
        "SELECT id, name, version, schema_version, content_hash, json, created_at, source_project_id
         FROM team_templates WHERE id = ?1",
        params![template_id.trim()],
        map_record,
    )
    .optional()
    .map_err(Into::into)
}

pub fn get_team_template(
    store: &Store,
    template_id: &str,
    project_id: Option<&str>,
) -> Result<Option<(TeamTemplateRecord, TeamTemplateEntry)>> {
    let Some(record) = get_team_template_record(store, template_id)? else {
        return Ok(None);
    };
    if let Some(project_id) = project_id {
        // A project-scoped read may only see its own exported templates or a
        // template it has registered/applied. The library is not a global
        // cross-project data pool.
        let conn = store.lock()?;
        let allowed: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM team_templates t
                WHERE t.id = ?1
                  AND (t.source_project_id = ?2
                       OR EXISTS (SELECT 1 FROM project_team_templates l
                                  WHERE l.template_id = t.id AND l.project_id = ?2)
                       OR EXISTS (SELECT 1 FROM team_template_events e
                                  WHERE e.template_id = t.id AND e.project_id = ?2)))",
            params![record.id, project_id],
            |row| row.get(0),
        )?;
        if !allowed {
            return Ok(None);
        }
    }
    let entry = entry_for_template(store, &record, project_id)?;
    Ok(Some((record, entry)))
}

pub fn list_team_templates(
    store: &Store,
    project_id: Option<&str>,
    limit: u32,
) -> Result<(Vec<TeamTemplateEntry>, bool)> {
    let limit = limit.clamp(1, vp_protocol::types::TEAM_TEMPLATE_MAX_LIMIT);
    let conn = store.lock()?;
    let fetch = limit as i64 + 1;
    let mut rows = match project_id {
        Some(project_id) => {
            let mut statement = conn.prepare(
                "SELECT t.id, t.name, t.version, t.schema_version, t.content_hash, t.json,
                        t.created_at, t.source_project_id,
                        p.applied_at, p.applied_by
                 FROM team_templates t
                 LEFT JOIN project_team_templates p
                   ON p.template_id = t.id AND p.project_id = ?1
                 WHERE t.source_project_id = ?1
                    OR EXISTS (SELECT 1 FROM project_team_templates l
                               WHERE l.template_id = t.id AND l.project_id = ?1)
                    OR EXISTS (SELECT 1 FROM team_template_events e
                               WHERE e.template_id = t.id AND e.project_id = ?1)
                 ORDER BY t.created_at DESC, t.id ASC
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![project_id, fetch], |row| {
                    map_record_with_applied(row)
                })?
                .collect::<Result<Vec<_>, _>>()?
        }
        None => {
            let mut statement = conn.prepare(
                "SELECT t.id, t.name, t.version, t.schema_version, t.content_hash, t.json,
                        t.created_at, t.source_project_id, NULL, NULL
                 FROM team_templates t
                 ORDER BY t.created_at DESC, t.id ASC
                 LIMIT ?1",
            )?;
            statement
                .query_map(params![fetch], map_record_with_applied)?
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    let truncated = rows.len() > limit as usize;
    if truncated {
        rows.truncate(limit as usize);
    }
    Ok((
        rows.into_iter()
            .map(|(record, applied_at, applied_by)| {
                let read_only = applied_at.is_none()
                    && match project_id {
                        Some(project) => record.source_project_id.as_deref() != Some(project),
                        None => true,
                    };
                let source_project = match project_id {
                    Some(project) if record.source_project_id.as_deref() != Some(project) => None,
                    _ => record.source_project_id,
                };
                TeamTemplateEntry {
                    template_id: record.id.clone(),
                    name: record.name,
                    version: record.version,
                    schema_version: record.schema_version,
                    content_hash: record.content_hash,
                    created_at: record.created_at,
                    source_project,
                    applied: applied_at.is_some(),
                    applied_at,
                    applied_by,
                    read_only,
                }
            })
            .collect(),
        truncated,
    ))
}

type AppliedRecordRow = (TeamTemplateRecord, Option<String>, Option<String>);

fn map_record_with_applied(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppliedRecordRow> {
    let record = map_record(row)?;
    Ok((record, row.get(8)?, row.get(9)?))
}

/// Every template relevant to one project: linked, exported from it, or
/// imported into it. Used by the manifest, bundle export, and rebuild.
pub fn list_team_template_records_for_project(
    store: &Store,
    project_id: &str,
) -> Result<Vec<TeamTemplateRecord>> {
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT DISTINCT t.id, t.name, t.version, t.schema_version, t.content_hash, t.json,
                t.created_at, t.source_project_id
         FROM team_templates t
         LEFT JOIN project_team_templates p
           ON p.template_id = t.id AND p.project_id = ?1
         LEFT JOIN team_template_events e
           ON e.template_id = t.id AND e.project_id = ?1
         WHERE p.template_id IS NOT NULL
            OR t.source_project_id = ?1
            OR e.template_id IS NOT NULL
         ORDER BY t.created_at ASC, t.id ASC",
    )?;
    let rows = statement.query_map(params![project_id], map_record)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn list_project_team_templates(
    store: &Store,
    project_id: &str,
) -> Result<Vec<ProjectTeamTemplate>> {
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT project_id, template_id, applied_at, applied_by
         FROM project_team_templates WHERE project_id = ?1
         ORDER BY applied_at ASC, template_id ASC",
    )?;
    let rows = statement.query_map(params![project_id], |row| {
        Ok(ProjectTeamTemplate {
            project_id: row.get(0)?,
            template_id: row.get(1)?,
            applied_at: row.get(2)?,
            applied_by: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn list_team_template_events_for_project(
    store: &Store,
    project_id: &str,
    limit: Option<usize>,
) -> Result<Vec<TeamTemplateEvent>> {
    let limit = limit
        .unwrap_or(TEAM_TEMPLATE_MAX_EVENTS)
        .min(TEAM_TEMPLATE_MAX_EVENTS) as i64;
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT id, project_id, seq, kind, template_id, name, version, content_hash, payload,
                created_at
         FROM team_template_events WHERE project_id = ?1
         ORDER BY seq ASC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_id, limit], map_event)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn insert_team_template_snapshot(store: &Store, record: &TeamTemplateRecord) -> Result<()> {
    if record.template.template_id != record.id
        || record.template.content_hash != record.content_hash
        || record.template.name != record.name
        || record.template.version != record.version
        || record.template.schema_version != record.schema_version
    {
        return Err(template_error(
            team_template_codes::TEMPLATE_REPLAY_INVALID,
            format!(
                "template snapshot {} metadata does not match its portable body",
                record.id
            ),
        ));
    }
    let encoded = serde_json::to_string(&record.template)?;
    let conn = store.lock()?;
    conn.execute(
        "INSERT INTO team_templates
         (id, name, version, schema_version, content_hash, json, created_at, source_project_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            version = excluded.version,
            schema_version = excluded.schema_version,
            content_hash = excluded.content_hash,
            json = excluded.json,
            created_at = excluded.created_at,
            source_project_id = excluded.source_project_id",
        params![
            record.id,
            record.name,
            record.version,
            record.schema_version as i64,
            record.content_hash,
            encoded,
            record.created_at,
            record.source_project_id
        ],
    )?;
    Ok(())
}

pub fn insert_project_team_template_snapshot(
    store: &Store,
    link: &ProjectTeamTemplate,
) -> Result<()> {
    let conn = store.lock()?;
    conn.execute(
        "INSERT INTO project_team_templates (project_id, template_id, applied_at, applied_by)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(project_id, template_id) DO UPDATE SET
            applied_at = excluded.applied_at,
            applied_by = excluded.applied_by",
        params![
            link.project_id,
            link.template_id,
            link.applied_at,
            link.applied_by
        ],
    )?;
    Ok(())
}

pub fn insert_team_template_event_snapshot(store: &Store, event: &TeamTemplateEvent) -> Result<()> {
    if !TEAM_TEMPLATE_EVENT_KINDS.contains(&event.kind.as_str()) {
        return Err(template_error(
            team_template_codes::TEMPLATE_REPLAY_INVALID,
            format!("template event {} has an unknown kind", event.id),
        ));
    }
    let conn = store.lock()?;
    conn.execute(
        "INSERT INTO team_template_events
         (id, project_id, seq, kind, template_id, name, version, content_hash, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
            project_id = excluded.project_id,
            seq = excluded.seq,
            kind = excluded.kind,
            template_id = excluded.template_id,
            name = excluded.name,
            version = excluded.version,
            content_hash = excluded.content_hash,
            payload = excluded.payload,
            created_at = excluded.created_at",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.kind,
            event.template_id,
            event.name,
            event.version,
            event.content_hash,
            serde_json::to_string(&event.payload)?,
            event.created_at
        ],
    )?;
    Ok(())
}

fn payload_is_bounded_audit(payload: &Value) -> bool {
    const ALLOWED: &[&str] = &[
        "template_id",
        "name",
        "version",
        "content_hash",
        "slot_count",
        "pending_hire_count",
        "applied_by",
    ];
    match payload {
        Value::Object(map) => map.keys().all(|key| ALLOWED.contains(&key.as_str())),
        _ => false,
    }
}

/// Compare the project audit stream with the stored link/template rows.
/// Repair only recreates a missing link from an existing `applied` event; it
/// never invents a template body, identity, hire request, or policy change.
pub fn replay_team_templates(
    store: &Store,
    project_id: &str,
    repair: bool,
) -> Result<vp_protocol::types::TeamTemplateReplayReport> {
    use vp_protocol::types::{TeamTemplateRecord, TeamTemplateReplayReport};

    let project_id = project_id.trim();
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    project_exists_tx(&tx, project_id)?;
    let events = {
        let mut statement = tx.prepare(
            "SELECT id, project_id, seq, kind, template_id, name, version, content_hash, payload,
                    created_at
             FROM team_template_events WHERE project_id = ?1 ORDER BY seq ASC",
        )?;
        statement
            .query_map(params![project_id], map_event)?
            .collect::<Result<Vec<_>, _>>()?
    };
    let records = {
        let mut statement = tx.prepare(
            "SELECT id, name, version, schema_version, content_hash, json, created_at,
                    source_project_id
             FROM team_templates ORDER BY id ASC",
        )?;
        statement
            .query_map([], map_record)?
            .collect::<Result<Vec<TeamTemplateRecord>, _>>()?
    };
    let mut links = {
        let mut statement = tx.prepare(
            "SELECT project_id, template_id, applied_at, applied_by
             FROM project_team_templates WHERE project_id = ?1",
        )?;
        statement
            .query_map(params![project_id], |row| {
                Ok(ProjectTeamTemplate {
                    project_id: row.get(0)?,
                    template_id: row.get(1)?,
                    applied_at: row.get(2)?,
                    applied_by: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut mismatches = Vec::new();
    let mut warnings = Vec::new();
    let mut repaired_links = 0u64;
    let mut expected_seq = 1u64;
    for event in &events {
        if event.seq != expected_seq {
            mismatches.push(format!(
                "template event stream is not contiguous at seq {} (expected {expected_seq})",
                event.seq
            ));
        }
        expected_seq = event.seq.saturating_add(1);
        if event.project_id != project_id {
            mismatches.push(format!(
                "template event {} belongs to another project",
                event.id
            ));
        }
        if !payload_is_bounded_audit(&event.payload) {
            mismatches.push(format!(
                "template event {} carries non-audit payload keys",
                event.id
            ));
        }
        if !TEAM_TEMPLATE_EVENT_KINDS.contains(&event.kind.as_str()) {
            mismatches.push(format!("template event {} has an unknown kind", event.id));
        }
    }
    let record_by_id = records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<HashMap<_, _>>();
    for record in &records {
        if record.template.content_hash != record.content_hash {
            mismatches.push(format!(
                "template {} body hash does not match its record",
                record.id
            ));
        }
    }
    for link in &links {
        if !record_by_id.contains_key(link.template_id.as_str()) {
            mismatches.push(format!(
                "project link references missing template {}",
                link.template_id
            ));
        }
        if !events.iter().any(|event| {
            event.kind == TEAM_TEMPLATE_APPLIED_EVENT && event.template_id == link.template_id
        }) {
            warnings.push(format!(
                "link {} has no applied event (legacy/manual row)",
                link.template_id
            ));
        }
    }
    for event in &events {
        if event.kind != TEAM_TEMPLATE_APPLIED_EVENT {
            continue;
        }
        if links
            .iter()
            .any(|link| link.template_id == event.template_id)
        {
            continue;
        }
        let Some(record) = record_by_id.get(event.template_id.as_str()) else {
            mismatches.push(format!(
                "applied event {} references missing template {}",
                event.id, event.template_id
            ));
            continue;
        };
        if record.content_hash != event.content_hash {
            mismatches.push(format!(
                "applied event {} hash does not match template {}",
                event.id, event.template_id
            ));
            continue;
        }
        if repair {
            let applied_by = event
                .payload
                .get("applied_by")
                .and_then(Value::as_str)
                .map(str::to_string);
            tx.execute(
                "INSERT OR IGNORE INTO project_team_templates
                 (project_id, template_id, applied_at, applied_by)
                 VALUES (?1, ?2, ?3, ?4)",
                params![project_id, event.template_id, event.created_at, applied_by],
            )?;
            repaired_links += 1;
            links.push(ProjectTeamTemplate {
                project_id: project_id.to_string(),
                template_id: event.template_id.clone(),
                applied_at: event.created_at.clone(),
                applied_by,
            });
        }
    }
    if repair {
        tx.commit()?;
    }
    Ok(TeamTemplateReplayReport {
        project_id: project_id.to_string(),
        events: events.len() as u64,
        templates: records.len() as u64,
        links: links.len() as u64,
        repaired_links,
        mismatches,
        warnings,
    })
}

/// Convert a record to the portable human/machine output pair. The core layer
/// owns validation and Markdown rendering; this exists for bundle tests and
/// future CLI helpers.
pub fn record_to_template(record: &TeamTemplateRecord) -> Result<TeamTemplate> {
    let mut template = record.template.clone();
    if template.template_id.is_empty() {
        template.template_id = record.id.clone();
    }
    Ok(template)
}

/// Fail closed if a persisted row was written by a version this build does not
/// understand. This is checked on read so a downgrade cannot silently parse a
/// newer schema as the current one.
pub fn ensure_supported_schema(template: &TeamTemplate) -> Result<()> {
    if template.schema_version != vp_protocol::types::TEAM_TEMPLATE_SCHEMA_VERSION {
        return Err(template_error(
            team_template_codes::TEMPLATE_SCHEMA_UNSUPPORTED,
            format!(
                "team template schema {} is not supported (expected {})",
                template.schema_version,
                vp_protocol::types::TEAM_TEMPLATE_SCHEMA_VERSION
            ),
        ));
    }
    Ok(())
}

/// A bounded event count used by tests and diagnostics.
pub fn count_team_template_events(store: &Store, project_id: &str) -> Result<u64> {
    let conn = store.lock()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM team_template_events WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

/// Compose a project library entry for a template without an apply link. This
/// is the read-only import path.
pub fn library_entry(
    store: &Store,
    project_id: &str,
    template: &TeamTemplate,
) -> Result<TeamTemplateEntry> {
    let Some(record) = get_team_template_record(store, &template.template_id)? else {
        return Err(template_error(
            team_template_codes::TEMPLATE_NOT_FOUND,
            format!("template not found: {}", template.template_id),
        ));
    };
    let mut entry = entry_for_template(store, &record, Some(project_id))?;
    entry.read_only = !entry.applied;
    Ok(entry)
}

/// Explicitly mark a library entry read-only when it was imported rather than
/// exported from this project.
pub fn mark_imported(entry: &mut TeamTemplateEntry) {
    if !entry.applied {
        entry.read_only = true;
    }
}

/// Internal helper used by the manifest writer: bounded template rows for one
/// project in deterministic order.
pub fn manifest_records(
    store: &Store,
    project_id: &str,
) -> Result<(
    Vec<TeamTemplateRecord>,
    Vec<ProjectTeamTemplate>,
    Vec<TeamTemplateEvent>,
)> {
    Ok((
        list_team_template_records_for_project(store, project_id)?,
        list_project_team_templates(store, project_id)?,
        list_team_template_events_for_project(store, project_id, None)?,
    ))
}

/// Export/import event names are centralized so callers cannot invent a kind
/// that replay will reject later.
pub const EXPORTED_EVENT: &str = TEAM_TEMPLATE_EXPORTED_EVENT;
pub const IMPORTED_EVENT: &str = TEAM_TEMPLATE_IMPORTED_EVENT;
pub const APPLIED_EVENT: &str = TEAM_TEMPLATE_APPLIED_EVENT;

/// A small borrow-friendly DTO for tests and the rebuild path.
pub fn records_by_hash(records: &[TeamTemplateRecord]) -> BTreeMap<String, &TeamTemplateRecord> {
    records
        .iter()
        .map(|record| (record.content_hash.clone(), record))
        .collect()
}

/// Return the canonical storage JSON, preserving field order. Used by tests to
/// prove that an apply never rewrites the portable body.
pub fn stored_json(store: &Store, template_id: &str) -> Result<Option<String>> {
    let conn = store.lock()?;
    conn.query_row(
        "SELECT json FROM team_templates WHERE id = ?1",
        params![template_id.trim()],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// The number of pending S6 requests created from one template, without
/// reading any request payload or context.
pub fn count_pending_template_hires(
    store: &Store,
    project_id: &str,
    template_id: &str,
) -> Result<u64> {
    let conn = store.lock()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_hire_requests
         WHERE project_id = ?1 AND status = 'pending'
           AND json_extract(policy_json, '$.team_template.template_id') = ?2",
        params![project_id, template_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{
        TeamTemplateBudgets, TeamTemplateDepartment, TeamTemplateLabourDefaults,
        TeamTemplateProvenance, TeamTemplateRoleSlot, TeamTemplateRules,
    };

    fn test_template(id: &str) -> TeamTemplate {
        TeamTemplate {
            schema_version: vp_protocol::types::TEAM_TEMPLATE_SCHEMA_VERSION,
            template_id: id.to_string(),
            name: "Platform Team".to_string(),
            version: "1.0.0".to_string(),
            description: "portable".to_string(),
            content_hash: "a".repeat(64),
            leader: TeamTemplateRoleSlot {
                title: "Leader".to_string(),
                role: "leader".to_string(),
                profile: "generic".to_string(),
                ..TeamTemplateRoleSlot::default()
            },
            departments: vec![TeamTemplateDepartment {
                name: "Platform".to_string(),
                objective: "Own platform".to_string(),
                manager: TeamTemplateRoleSlot {
                    title: "Platform Manager".to_string(),
                    role: "manager".to_string(),
                    department: Some("Platform".to_string()),
                    profile: "generic".to_string(),
                    ..TeamTemplateRoleSlot::default()
                },
                workers: Vec::new(),
            }],
            labour_defaults: TeamTemplateLabourDefaults {
                profile: "coding".to_string(),
                bundle: "coding".to_string(),
                max_concurrency: 4,
                ..TeamTemplateLabourDefaults::default()
            },
            rules: TeamTemplateRules::default(),
            budgets: TeamTemplateBudgets::default(),
            provenance: TeamTemplateProvenance::default(),
            ..TeamTemplate::default()
        }
    }

    fn test_store(dir: &TempDir) -> (Store, String) {
        let store = Store::open(dir.path()).expect("store");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).expect("repo");
        let project = store
            .add_project(root.to_str().expect("path"), Some("templates"))
            .expect("project");
        (store, project.id)
    }

    #[test]
    fn register_is_idempotent_and_rejects_a_same_id_body_change() {
        let dir = TempDir::new().expect("tempdir");
        let (store, project_id) = test_store(&dir);
        let template = test_template("tt_register");
        let (entry, event, changed) = register_team_template(
            &store,
            &project_id,
            &template,
            Some(&project_id),
            TEAM_TEMPLATE_EXPORTED_EVENT,
        )
        .expect("register");
        assert!(changed);
        assert_eq!(event.kind, TEAM_TEMPLATE_EXPORTED_EVENT);
        assert!(!entry.applied);
        let (_, _, changed) = register_team_template(
            &store,
            &project_id,
            &template,
            Some(&project_id),
            TEAM_TEMPLATE_EXPORTED_EVENT,
        )
        .expect("idempotent register");
        assert!(!changed);
        let mut changed_body = template.clone();
        changed_body.content_hash = "b".repeat(64);
        let error = register_team_template(
            &store,
            &project_id,
            &changed_body,
            Some(&project_id),
            TEAM_TEMPLATE_EXPORTED_EVENT,
        )
        .expect_err("same id different body");
        assert!(error.to_string().contains("different body"));
    }

    #[test]
    fn apply_is_idempotent_and_replay_repairs_a_missing_link() {
        let dir = TempDir::new().expect("tempdir");
        let (store, project_id) = test_store(&dir);
        let template = test_template("tt_apply");
        let (entry, _, _) = register_team_template(
            &store,
            &project_id,
            &template,
            Some(&project_id),
            TEAM_TEMPLATE_EXPORTED_EVENT,
        )
        .expect("register");
        assert!(!entry.applied);
        let (entry, request_ids, changed) = apply_team_template(
            &store,
            &project_id,
            &template,
            Some(&project_id),
            Some("operator"),
            &[],
        )
        .expect("apply");
        assert!(changed);
        assert!(request_ids.is_empty());
        assert!(entry.applied);
        let (_, _, changed) = apply_team_template(
            &store,
            &project_id,
            &template,
            Some(&project_id),
            Some("operator"),
            &[],
        )
        .expect("idempotent apply");
        assert!(!changed);

        // Simulate a projection loss, then repair it from the applied event.
        {
            let conn = store.lock().expect("lock");
            conn.execute(
                "DELETE FROM project_team_templates WHERE project_id = ?1 AND template_id = ?2",
                params![project_id, template.template_id],
            )
            .expect("delete link");
        }
        let report = replay_team_templates(&store, &project_id, false).expect("replay");
        assert_eq!(report.repaired_links, 0);
        let report = replay_team_templates(&store, &project_id, true).expect("repair");
        assert_eq!(report.repaired_links, 1);
        assert!(report.mismatches.is_empty());
        let links = list_project_team_templates(&store, &project_id).expect("links");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].template_id, template.template_id);
    }

    #[test]
    fn archive_snapshots_restore_template_record_and_link() {
        let dir = TempDir::new().expect("tempdir");
        let (store, project_id) = test_store(&dir);
        let template = test_template("tt_snapshot");
        let record = TeamTemplateRecord {
            id: template.template_id.clone(),
            name: template.name.clone(),
            version: template.version.clone(),
            schema_version: template.schema_version,
            content_hash: template.content_hash.clone(),
            template: template.clone(),
            created_at: "2026-11-13T00:00:00Z".to_string(),
            source_project_id: Some(project_id.clone()),
        };
        insert_team_template_snapshot(&store, &record).expect("template snapshot");
        let link = ProjectTeamTemplate {
            project_id: project_id.clone(),
            template_id: template.template_id.clone(),
            applied_at: "2026-11-13T00:00:00Z".to_string(),
            applied_by: Some("archive".to_string()),
        };
        insert_project_team_template_snapshot(&store, &link).expect("link snapshot");
        let loaded = get_team_template_record(&store, &template.template_id)
            .expect("read")
            .expect("row");
        assert_eq!(loaded, record);
        let links = list_project_team_templates(&store, &project_id).expect("links");
        assert_eq!(links, vec![link]);
    }

    #[test]
    fn replay_rejects_event_payloads_that_carry_a_body() {
        let dir = TempDir::new().expect("tempdir");
        let (store, project_id) = test_store(&dir);
        let template = test_template("tt_leak");
        register_team_template(
            &store,
            &project_id,
            &template,
            Some(&project_id),
            TEAM_TEMPLATE_IMPORTED_EVENT,
        )
        .expect("register");
        {
            let conn = store.lock().expect("lock");
            conn.execute(
                "UPDATE team_template_events SET payload = ?1 WHERE project_id = ?2",
                params![
                    serde_json::json!({"template_id": "tt_leak", "json": "{}"}).to_string(),
                    project_id
                ],
            )
            .expect("tamper");
        }
        let report = replay_team_templates(&store, &project_id, false).expect("replay");
        assert!(
            report
                .mismatches
                .iter()
                .any(|mismatch| mismatch.contains("non-audit payload"))
        );
    }

    #[test]
    fn pending_template_hire_count_reads_only_the_metadata_projection() {
        let dir = TempDir::new().expect("tempdir");
        let (store, project_id) = test_store(&dir);
        let template = test_template("tt_count");
        register_team_template(
            &store,
            &project_id,
            &template,
            Some(&project_id),
            TEAM_TEMPLATE_EXPORTED_EVENT,
        )
        .expect("register");
        assert_eq!(
            count_pending_template_hires(&store, &project_id, &template.template_id)
                .expect("count"),
            0
        );
    }
}
