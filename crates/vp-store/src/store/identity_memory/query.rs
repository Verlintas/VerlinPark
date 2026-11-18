use super::*;

/// Safe read used by the RPC/tool surface. Visibility is enforced by the caller
/// with the loaded identity row.
pub fn get_agent_memory(store: &Store, memory_id: &str) -> Result<Option<AgentMemory>> {
    let conn = store.lock()?;
    conn.query_row(
        &format!("SELECT {AGENT_MEMORY_COLUMNS} FROM agent_memories WHERE id = ?1"),
        params![memory_id],
        map_memory,
    )
    .optional()
    .map_err(Into::into)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Bounded, filterable project memory read. Viewer scope isolation is applied
/// in SQL so a busy project cannot starve an identity's own memory before the
/// engine filters the result.
#[allow(clippy::too_many_arguments)]
pub fn list_agent_memories(
    store: &Store,
    project_id: &str,
    scope: Option<&str>,
    status: Option<&str>,
    department: Option<&str>,
    query: Option<&str>,
    include_expired: bool,
    limit: Option<u32>,
    viewer_identity_id: Option<&str>,
    viewer_department: Option<&str>,
) -> Result<Vec<AgentMemory>> {
    if let Some(scope) = scope
        && agent_memory_scope(scope).is_none()
    {
        return Err(memory_error(
            agent_memory_codes::MEMORY_SCOPE_INVALID,
            format!("scope must be agent, department, or project: {scope}"),
        ));
    }
    if let Some(status) = status
        && !AGENT_MEMORY_STATUSES.contains(&status)
    {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("status must be active, superseded, or stale: {status}"),
        ));
    }
    let limit = limit.unwrap_or(50).clamp(1, AGENT_MEMORY_MAX_LIST);
    let department = department.map(str::trim).filter(|value| !value.is_empty());
    let query = query.map(str::trim).filter(|value| !value.is_empty());
    let like = query.map(|value| format!("%{}%", escape_like(value)));
    let now = now_rfc3339();
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {AGENT_MEMORY_COLUMNS}
         FROM agent_memories
         WHERE project_id = ?1
           AND (?2 IS NULL OR scope = ?2)
           AND (?3 IS NULL OR status = ?3)
           AND (?4 IS NULL OR department = ?4 COLLATE NOCASE)
           AND (?5 IS NULL OR content LIKE ?5 ESCAPE '\\')
           AND (?6 = 1 OR expires_at IS NULL OR expires_at > ?7)
           AND (
             ?9 IS NULL
             OR (scope = 'agent' AND identity_id = ?9)
             OR (scope = 'department' AND ?10 IS NOT NULL
                 AND department = ?10 COLLATE NOCASE)
             OR scope = 'project'
           )
         ORDER BY created_at DESC, id ASC
         LIMIT ?8"
    ))?;
    let rows = statement.query_map(
        params![
            project_id,
            scope,
            status,
            department,
            like,
            if include_expired { 1 } else { 0 },
            now,
            limit as i64,
            viewer_identity_id,
            viewer_department
        ],
        map_memory,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Active agent-scope memory counts grouped by identity. This is the org-chart
/// count, not a content read; expired and superseded rows are excluded.
pub fn active_agent_memory_counts_by_identity(
    store: &Store,
    project_id: &str,
) -> Result<HashMap<String, u64>> {
    let now = now_rfc3339();
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT identity_id, COUNT(*) FROM agent_memories
         WHERE project_id = ?1
           AND scope = 'agent'
           AND status = 'active'
           AND identity_id IS NOT NULL
           AND (expires_at IS NULL OR expires_at > ?2)
         GROUP BY identity_id",
    )?;
    let rows = statement.query_map(params![project_id, now], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut counts = HashMap::new();
    for row in rows {
        let (identity_id, count) = row?;
        if count > 0 {
            counts.insert(identity_id, count as u64);
        }
    }
    Ok(counts)
}

/// Count the memory rows visible to one identity through the S5 scope rules:
/// its own agent memory, its department memory, and project memory. The count
/// excludes superseded and expired rows.
pub fn visible_agent_memory_count(
    store: &Store,
    project_id: &str,
    identity_id: &str,
    department: Option<&str>,
) -> Result<u64> {
    let now = now_rfc3339();
    let department = department.map(str::trim).filter(|value| !value.is_empty());
    let conn = store.lock()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_memories
         WHERE project_id = ?1
           AND status = 'active'
           AND (expires_at IS NULL OR expires_at > ?2)
           AND (
             (scope = 'agent' AND identity_id = ?3)
             OR (scope = 'department' AND ?4 IS NOT NULL AND department = ?4 COLLATE NOCASE)
             OR scope = 'project'
           )",
        params![project_id, now, identity_id, department],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

/// Bounded memory read for one identity using the S5 scope rules: its own agent
/// memory, its department memory, and project memory. This is an operator
/// projection, not a permission bypass; it still excludes superseded/expired
/// rows unless the caller explicitly asks for them.
pub fn list_visible_agent_memories(
    store: &Store,
    project_id: &str,
    identity_id: &str,
    department: Option<&str>,
    status: Option<&str>,
    include_expired: bool,
    limit: Option<u32>,
) -> Result<Vec<AgentMemory>> {
    if let Some(status) = status
        && !AGENT_MEMORY_STATUSES.contains(&status)
    {
        return Err(memory_error(
            agent_memory_codes::MEMORY_INVALID,
            format!("status must be active, superseded, or stale: {status}"),
        ));
    }
    let limit = limit.unwrap_or(50).clamp(1, AGENT_MEMORY_MAX_LIST);
    let department = department.map(str::trim).filter(|value| !value.is_empty());
    let now = now_rfc3339();
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {AGENT_MEMORY_COLUMNS}
         FROM agent_memories
         WHERE project_id = ?1
           AND (?2 IS NULL OR status = ?2)
           AND (?3 = 1 OR expires_at IS NULL OR expires_at > ?4)
           AND (
             (scope = 'agent' AND identity_id = ?5)
             OR (scope = 'department' AND ?6 IS NOT NULL AND department = ?6 COLLATE NOCASE)
             OR scope = 'project'
           )
         ORDER BY created_at DESC, id ASC
         LIMIT ?7"
    ))?;
    let rows = statement.query_map(
        params![
            project_id,
            status,
            if include_expired { 1 } else { 0 },
            now,
            identity_id,
            department,
            limit as i64
        ],
        map_memory,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn count_agent_memories(
    store: &Store,
    project_id: &str,
    scope: Option<&str>,
    status: Option<&str>,
    department: Option<&str>,
    query: Option<&str>,
    include_expired: bool,
) -> Result<u64> {
    let department = department.map(str::trim).filter(|value| !value.is_empty());
    let query = query.map(str::trim).filter(|value| !value.is_empty());
    let like = query.map(|value| format!("%{}%", escape_like(value)));
    let now = now_rfc3339();
    let conn = store.lock()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_memories
         WHERE project_id = ?1
           AND (?2 IS NULL OR scope = ?2)
           AND (?3 IS NULL OR status = ?3)
           AND (?4 IS NULL OR department = ?4 COLLATE NOCASE)
           AND (?5 IS NULL OR content LIKE ?5 ESCAPE '\\')
           AND (?6 = 1 OR expires_at IS NULL OR expires_at > ?7)",
        params![
            project_id,
            scope,
            status,
            department,
            like,
            if include_expired { 1 } else { 0 },
            now
        ],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

/// Candidates for the bounded prompt brief. Only local, active, non-expired
/// rows are returned. Scope filtering and deterministic ranking happen in
/// `vp_core::identity_memory`.
///
/// The candidate window is partitioned per visible scope with reserved
/// capacity. A plain `ORDER BY updated_at DESC LIMIT 128` lets a busy agent's
/// private rows consume the entire window before the core ranker can consider
/// department/project rows (including older pinned ones). Each scope is read
/// separately with `pinned` first, so shared memory always reaches ranking.
pub fn list_agent_memories_for_brief(
    store: &Store,
    project_id: &str,
    viewer_identity_id: Option<&str>,
    viewer_department: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<AgentMemory>> {
    let limit = limit.unwrap_or(128).clamp(1, 256);
    let now = now_rfc3339();
    let conn = store.lock()?;
    if viewer_identity_id.is_none() || limit < 4 {
        // Trusted local operator view (no viewer) or a tiny explicit limit:
        // keep the single bounded query.
        let mut statement = conn.prepare(&format!(
            "SELECT {AGENT_MEMORY_COLUMNS}
             FROM agent_memories
             WHERE project_id = ?1
               AND status = 'active'
               AND origin = 'local'
               AND (expires_at IS NULL OR expires_at > ?2)
               AND (
                 ?3 IS NULL
                 OR (scope = 'agent' AND identity_id = ?3)
                 OR (scope = 'department' AND ?4 IS NOT NULL
                     AND department = ?4 COLLATE NOCASE)
                 OR scope = 'project'
               )
             ORDER BY pinned DESC, updated_at DESC, content_hash ASC, id ASC
             LIMIT ?5"
        ))?;
        let rows = statement.query_map(
            params![
                project_id,
                now,
                viewer_identity_id,
                viewer_department,
                limit as i64
            ],
            map_memory,
        )?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }
    let department_quota = (limit / 4).max(1);
    let project_quota = (limit / 4).max(1);
    let agent_quota = limit
        .saturating_sub(department_quota)
        .saturating_sub(project_quota)
        .max(1);
    let mut candidates = Vec::new();
    candidates.extend(brief_scope_rows(
        &conn,
        project_id,
        &now,
        "agent",
        viewer_identity_id,
        viewer_department,
        agent_quota,
    )?);
    if viewer_department.is_some() {
        candidates.extend(brief_scope_rows(
            &conn,
            project_id,
            &now,
            "department",
            viewer_identity_id,
            viewer_department,
            department_quota,
        )?);
    }
    candidates.extend(brief_scope_rows(
        &conn,
        project_id,
        &now,
        "project",
        viewer_identity_id,
        viewer_department,
        project_quota,
    )?);
    Ok(candidates)
}

#[allow(clippy::too_many_arguments)]
fn brief_scope_rows(
    conn: &rusqlite::Connection,
    project_id: &str,
    now: &str,
    scope: &str,
    viewer_identity_id: Option<&str>,
    viewer_department: Option<&str>,
    limit: u32,
) -> Result<Vec<AgentMemory>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {AGENT_MEMORY_COLUMNS}
         FROM agent_memories
         WHERE project_id = ?1
           AND status = 'active'
           AND origin = 'local'
           AND (expires_at IS NULL OR expires_at > ?2)
           AND scope = ?3
           AND (
             (?3 = 'agent' AND identity_id = ?4)
             OR (?3 = 'department' AND ?5 IS NOT NULL
                 AND department = ?5 COLLATE NOCASE)
             OR ?3 = 'project'
           )
         ORDER BY pinned DESC, updated_at DESC, content_hash ASC, id ASC
         LIMIT ?6"
    ))?;
    let rows = statement.query_map(
        params![
            project_id,
            now,
            scope,
            viewer_identity_id,
            viewer_department,
            limit as i64
        ],
        map_memory,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn list_agent_memories_for_replay(store: &Store, project_id: &str) -> Result<Vec<AgentMemory>> {
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "SELECT {AGENT_MEMORY_COLUMNS} FROM agent_memories
         WHERE project_id = ?1 ORDER BY id ASC"
    ))?;
    let rows = statement.query_map(params![project_id], map_memory)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    fn memory(id: &str, scope: &str, updated_at: &str) -> AgentMemory {
        let content = format!("content for {id}");
        AgentMemory {
            id: id.to_string(),
            project_id: "project".to_string(),
            identity_id: (scope == "agent").then(|| "agent-1".to_string()),
            department: (scope == "department").then(|| "Platform".to_string()),
            component: None,
            scope: scope.to_string(),
            content: content.clone(),
            content_hash: format!("{:x}", Sha256::digest(content.as_bytes())),
            evidence: Vec::new(),
            source: "agent".to_string(),
            status: "active".to_string(),
            pinned: false,
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            expires_at: None,
            verified_at: None,
            superseded_by: None,
            stale_reason: None,
            origin: "local".to_string(),
            expired: false,
        }
    }

    #[test]
    fn brief_candidates_reserve_department_capacity_under_agent_pressure() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("brief"))
            .unwrap();
        // `agent_memories.identity_id` is a real foreign key. Create the
        // owner identity before inserting agent-scoped rows so this test does
        // not rely on a dangling projection id.
        let leader = store
            .create_agent_identity(&vp_protocol::types::CreateAgentIdentityParams {
                project_id: project.id.clone(),
                role: "leader".into(),
                department: Some("Platform".into()),
                title: Some("Leader".into()),
                ..Default::default()
            })
            .unwrap()
            .0;
        for index in 0..200 {
            let mut row = memory(
                &format!("agent-{index:03}"),
                "agent",
                "2026-11-12T23:59:00Z",
            );
            row.project_id = project.id.clone();
            row.identity_id = Some(leader.id.clone());
            store.insert_agent_memory_snapshot(&row).unwrap();
        }
        let mut pinned = memory("dept-pinned", "department", "2026-01-01T00:00:00Z");
        pinned.project_id = project.id.clone();
        pinned.pinned = true;
        store.insert_agent_memory_snapshot(&pinned).unwrap();
        let candidates = list_agent_memories_for_brief(
            &store,
            &project.id,
            Some(leader.id.as_str()),
            Some("Platform"),
            Some(128),
        )
        .unwrap();
        assert!(candidates.iter().any(|row| row.id == "dept-pinned"));
        assert!(candidates.len() <= 128);
    }
}
