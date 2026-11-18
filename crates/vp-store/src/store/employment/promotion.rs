use super::*;

/// Promote a Worker to Manager or a Manager to Leader. This is explicit and
/// approval-classified; it is never inferred from a plan or a model decision.
pub fn promote_agent(store: &Store, params: &AgentPromoteParams) -> Result<AgentPromoteResult> {
    if !params.approved {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_APPROVAL_REQUIRED,
            "promotion requires explicit approved: true; this operation is never automatic",
        ));
    }
    let operator = normalize_operator(params.operator.as_deref())?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_agent_identity_tx(&tx, params.identity_id.trim())?;
    if existing.origin != "local" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported identity is read-only until an explicit rehome",
        ));
    }
    if existing.status == "retired" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_RETIRED,
            format!("identity {} is retired and terminal", existing.id),
        ));
    }
    let target_role = params.target_role.trim().to_ascii_lowercase();
    if !AGENT_PROMOTION_TARGETS.contains(&target_role.as_str()) {
        return Err(employment_error(
            agent_identity_codes::PROMOTION_INVALID,
            "target_role must be manager or leader",
        ));
    }
    let legal = matches!(
        (existing.role.as_str(), target_role.as_str()),
        ("worker", "manager") | ("manager", "leader")
    );
    if !legal {
        return Err(employment_error(
            agent_identity_codes::PROMOTION_INVALID,
            format!(
                "identity {} cannot be promoted from {} to {target_role}",
                existing.id, existing.role
            ),
        ));
    }
    ensure_identity_role_capacity_tx(&tx, &existing.project_id, &target_role, Some(&existing.id))?;
    if target_role == "leader" {
        // Leader cannot be a parent in the four-tier hierarchy. Promoting a
        // Manager with live Workers would orphan them under a Leader parent.
        let direct_reports: i64 = tx.query_row(
            "SELECT COUNT(*) FROM agent_identities
             WHERE project_id = ?1 AND parent_identity_id = ?2 AND status <> 'retired'",
            params![existing.project_id, existing.id],
            |row| row.get(0),
        )?;
        if direct_reports > 0 {
            return Err(employment_error(
                agent_identity_codes::PROMOTION_CONFLICT,
                format!(
                    "identity {} still has {direct_reports} direct report(s); reparent them before promoting this Manager to Leader",
                    existing.id
                ),
            ));
        }
    }

    let parent = if target_role == "leader" {
        if params
            .parent_identity_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(employment_error(
                agent_identity_codes::PROMOTION_INVALID,
                "a Leader identity cannot have a parent identity",
            ));
        }
        None
    } else if let Some(parent) = params
        .parent_identity_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(parent.to_string())
    } else {
        // A Worker's existing parent is a Manager. Promoting to Manager keeps
        // the structural rule Manager→Leader, so select the project's live
        // Leader deterministically when the caller omitted the parent.
        tx.query_row(
            "SELECT id FROM agent_identities
             WHERE project_id = ?1 AND role = 'leader' AND status <> 'retired'
             ORDER BY created_at ASC, id ASC LIMIT 1",
            params![existing.project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    };
    validate_identity_parent_tx(&tx, &existing.project_id, &target_role, parent.as_deref())?;
    let title = match &params.title {
        Some(title) => normalize_identity_text(
            Some(title.as_str()),
            "title",
            vp_protocol::types::AGENT_IDENTITY_MAX_TITLE_CHARS,
        )?,
        None => existing.title.clone(),
    };

    let (active_assignments, active_workspaces, active_bindings) =
        identity_conflict_counts_tx(&tx, &existing.id)?;
    if (active_assignments > 0 || active_workspaces > 0 || active_bindings > 0) && !params.force {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_BUSY,
            format!(
                "identity {} still has {} active assignment(s), {} active workspace(s), and {} active workspace binding(s); release/archive them or pass force",
                existing.id, active_assignments, active_workspaces, active_bindings
            ),
        ));
    }
    let now = super::super::now_rfc3339();
    let mut released_assignments = 0;
    let mut archived_workspaces = 0;
    if params.force {
        let released = super::super::coordination::release_identity_assignments_tx(
            &tx,
            &existing.id,
            &operator,
            Some("promotion"),
            &now,
        )?;
        released_assignments = released.len() as u64;
        archived_workspaces =
            archive_identity_workspaces_tx(&tx, &existing.id, &now, &operator, Some("promotion"))?
                .len() as u64;
    }

    let mut next = existing.clone();
    next.role = target_role.clone();
    next.parent_identity_id = parent;
    next.title = title;
    next.generation = existing.generation.saturating_add(1);
    next.updated_at = now;
    let mut changed = vec!["role", "generation"];
    if next.parent_identity_id != existing.parent_identity_id {
        changed.push("parent_identity_id");
    }
    if next.title != existing.title {
        changed.push("title");
    }
    update_identity_and_event_tx(
        &tx,
        &existing,
        &next,
        &changed,
        vp_protocol::types::AGENT_IDENTITY_UPDATED_EVENT,
        serde_json::json!({
            "approved_by": operator,
            "approval_risk": "host_state",
        }),
    )?;
    let employment = insert_employment_event_tx(
        &tx,
        &existing.project_id,
        &existing.id,
        AGENT_PROMOTE_APPROVED_EVENT,
        serde_json::json!({
            "identity": next,
            "previous_role": existing.role,
            "previous_generation": existing.generation,
            "approved_by": operator,
            "approval_risk": "host_state",
            "released_assignments": released_assignments,
            "archived_workspaces": archived_workspaces,
        }),
    )?;
    tx.commit()?;
    Ok(AgentPromoteResult {
        identity: next,
        previous_role: existing.role,
        changed: true,
        released_assignments,
        archived_workspaces,
        employment_event_id: Some(employment.id),
    })
}

/// Transfer a non-Leader identity to a different department/component/title or
/// parent. A role change is promotion and is rejected here. Active work must be
/// released first unless `force` is explicit; even then, branches and evidence
/// are preserved.
pub fn transfer_agent(store: &Store, params: &AgentTransferParams) -> Result<AgentTransferResult> {
    if !params.approved {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_APPROVAL_REQUIRED,
            "transfer requires explicit approved: true; this operation is never automatic",
        ));
    }
    if params.department.is_none()
        && params.component.is_none()
        && params.title.is_none()
        && params.parent_identity_id.is_none()
    {
        return Err(employment_error(
            agent_identity_codes::TRANSFER_INVALID,
            "transfer requires at least one of department, component, title, or parent_identity_id",
        ));
    }
    let operator = normalize_operator(params.operator.as_deref())?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_agent_identity_tx(&tx, params.identity_id.trim())?;
    if existing.origin != "local" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported identity is read-only until an explicit rehome",
        ));
    }
    if existing.status == "retired" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_RETIRED,
            format!("identity {} is retired and terminal", existing.id),
        ));
    }
    if existing.role == "leader"
        && params.component.as_ref().is_some_and(|value| {
            value
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        })
    {
        return Err(employment_error(
            agent_identity_codes::TRANSFER_INVALID,
            "a Leader identity is project-wide and cannot own a component",
        ));
    }
    let mut next = existing.clone();
    let mut changed = Vec::new();
    if let Some(department) = &params.department {
        let value = normalize_identity_text(
            department.as_deref(),
            "department",
            vp_protocol::types::AGENT_IDENTITY_MAX_DEPARTMENT_CHARS,
        )?;
        if value != existing.department {
            next.department = value;
            changed.push("department");
        }
    }
    if let Some(component) = &params.component {
        let value = normalize_identity_text(
            component.as_deref(),
            "component",
            vp_protocol::types::AGENT_IDENTITY_MAX_COMPONENT_CHARS,
        )?;
        if value != existing.component {
            next.component = value;
            changed.push("component");
        }
    }
    if let Some(title) = &params.title {
        let value = normalize_identity_text(
            title.as_deref(),
            "title",
            vp_protocol::types::AGENT_IDENTITY_MAX_TITLE_CHARS,
        )?;
        if value != existing.title {
            next.title = value;
            changed.push("title");
        }
    }
    if let Some(parent) = &params.parent_identity_id {
        let value = normalize_identity_text(
            parent.as_deref(),
            "parent_identity_id",
            AGENT_IDENTITY_MAX_ID_CHARS,
        )?;
        if value != existing.parent_identity_id {
            next.parent_identity_id = value;
            changed.push("parent_identity_id");
        }
    }
    validate_identity_parent_tx(
        &tx,
        &existing.project_id,
        &existing.role,
        next.parent_identity_id.as_deref(),
    )?;
    if changed.is_empty() {
        return Ok(AgentTransferResult {
            identity: existing,
            changed: false,
            released_assignments: 0,
            archived_workspaces: 0,
            employment_event_id: None,
        });
    }

    let (active_assignments, active_workspaces, active_bindings) =
        identity_conflict_counts_tx(&tx, &existing.id)?;
    let component_changed = changed.contains(&"component");
    let needs_workspace_release = component_changed && active_workspaces > 0;
    if (active_assignments > 0 || active_bindings > 0 || needs_workspace_release) && !params.force {
        return Err(employment_error(
            agent_identity_codes::TRANSFER_CONFLICT,
            format!(
                "identity {} still has {} active assignment(s), {} workspace(s), and {} binding(s); release/archive them or pass force",
                existing.id, active_assignments, active_workspaces, active_bindings
            ),
        ));
    }
    let now = super::super::now_rfc3339();
    let mut released_assignments = 0;
    let mut archived_workspaces = 0;
    if params.force {
        let released = super::super::coordination::release_identity_assignments_tx(
            &tx,
            &existing.id,
            &operator,
            Some("transfer"),
            &now,
        )?;
        released_assignments = released.len() as u64;
        if component_changed || active_bindings > 0 {
            archived_workspaces = archive_identity_workspaces_tx(
                &tx,
                &existing.id,
                &now,
                &operator,
                Some("transfer"),
            )?
            .len() as u64;
        }
    }
    next.generation = existing.generation.saturating_add(1);
    next.updated_at = now;
    changed.push("generation");
    update_identity_and_event_tx(
        &tx,
        &existing,
        &next,
        &changed,
        vp_protocol::types::AGENT_IDENTITY_UPDATED_EVENT,
        serde_json::json!({
            "approved_by": operator,
            "approval_risk": "host_state",
        }),
    )?;
    let employment = insert_employment_event_tx(
        &tx,
        &existing.project_id,
        &existing.id,
        AGENT_TRANSFER_APPROVED_EVENT,
        serde_json::json!({
            "identity": next,
            "changed_fields": changed,
            "previous_generation": existing.generation,
            "approved_by": operator,
            "approval_risk": "host_state",
            "released_assignments": released_assignments,
            "archived_workspaces": archived_workspaces,
        }),
    )?;
    tx.commit()?;
    Ok(AgentTransferResult {
        identity: next,
        changed: true,
        released_assignments,
        archived_workspaces,
        employment_event_id: Some(employment.id),
    })
}
