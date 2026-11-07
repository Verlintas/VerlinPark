//! Project-level fork forest and checkpoint integrity audit.
//!
//! The per-session tree in `session_fork.tree` is event-derived for lineage
//! display. The project forest also includes sessions whose source fork event
//! was lost, so it is built from `session.created` parent metadata instead.
//! Audit/repair never rewrites historical events: missing links are backfilled
//! as new events and checkpoint refs are only recreated/removed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use vp_protocol::types::{
    ForkAuditIssue, ForkAuditResult, ForkRepairResult, ProjectForkForestResult, Session,
    SessionForkTreeNode,
};

use crate::Engine;
use crate::worktree;

const MAX_TREE_DEPTH: u32 = 32;
const MAX_TREE_NODES: usize = 500;

struct SessionMeta {
    session: Session,
    parent_id: Option<String>,
    checkpoint_id: Option<String>,
    commit: Option<String>,
}

impl Engine {
    pub async fn project_fork_forest(&self, project_id: &str) -> Result<ProjectForkForestResult> {
        let project = self.load_project(project_id).await?;
        let sessions = self.list_sessions(Some(project_id.to_string())).await?;
        let metas = self.load_session_metas(&sessions).await?;
        let mut checkpoints = 0usize;
        let mut dirty_checkpoints = 0usize;
        for session in &sessions {
            let active = self.active_checkpoints(&session.id).await?;
            checkpoints = checkpoints.saturating_add(active.len());
            dirty_checkpoints = dirty_checkpoints
                .saturating_add(active.iter().filter(|checkpoint| checkpoint.dirty).count());
        }
        let (roots, total, truncated) = build_forest(&metas);
        let _ = project;
        Ok(ProjectForkForestResult {
            project_id: project_id.to_string(),
            roots,
            total,
            checkpoints,
            dirty_checkpoints,
            truncated,
        })
    }

    pub async fn audit_project_forks(&self, project_id: &str) -> Result<ForkAuditResult> {
        let project = self.load_project(project_id).await?;
        let project_root = PathBuf::from(&project.root);
        let sessions = self.list_sessions(Some(project_id.to_string())).await?;
        let metas = self.load_session_metas(&sessions).await?;
        let meta_ids = metas.keys().cloned().collect::<HashSet<_>>();
        let mut fork_events = HashMap::new();
        for session in &sessions {
            let store = self.store().clone();
            let id = session.id.clone();
            let events = tokio::task::spawn_blocking(move || {
                store.list_events_by_kind(&id, "session.forked", 1000)
            })
            .await??;
            fork_events.insert(session.id.clone(), events);
        }
        let mut issues = Vec::new();
        let mut checkpoints = 0usize;
        for (session_id, meta) in &metas {
            if let Some(parent_id) = &meta.parent_id
                && !meta_ids.contains(parent_id)
            {
                issues.push(ForkAuditIssue {
                    kind: "missing_parent_session".to_string(),
                    session_id: session_id.clone(),
                    related_session_id: Some(parent_id.clone()),
                    checkpoint_id: meta.checkpoint_id.clone(),
                    commit: meta.commit.clone(),
                    detail: format!("fork parent {parent_id} is not present in this project"),
                    repairable: false,
                });
            }
            if let Some(parent_id) = &meta.parent_id
                && meta_ids.contains(parent_id)
            {
                let linked = fork_events.get(parent_id).is_some_and(|events| {
                    events.iter().any(|event| {
                        event
                            .payload
                            .get("fork_session_id")
                            .and_then(serde_json::Value::as_str)
                            == Some(session_id.as_str())
                    })
                });
                if !linked {
                    issues.push(ForkAuditIssue {
                        kind: "missing_fork_link".to_string(),
                        session_id: parent_id.clone(),
                        related_session_id: Some(session_id.clone()),
                        checkpoint_id: meta.checkpoint_id.clone(),
                        commit: meta.commit.clone(),
                        detail: "child session.created points at this parent, but the source has no session.forked event"
                            .to_string(),
                        repairable: true,
                    });
                }
            }
            let active = self.active_checkpoints(session_id).await?;
            checkpoints = checkpoints.saturating_add(active.len());
            for checkpoint in active {
                if !worktree::commit_exists(&project_root, &checkpoint.commit) {
                    issues.push(ForkAuditIssue {
                        kind: "missing_checkpoint_commit".to_string(),
                        session_id: session_id.clone(),
                        related_session_id: None,
                        checkpoint_id: Some(checkpoint.id.clone()),
                        commit: Some(checkpoint.commit.clone()),
                        detail: "checkpoint commit is missing from the repository object database"
                            .to_string(),
                        repairable: false,
                    });
                }
                if checkpoint.dirty
                    && !worktree::checkpoint_ref_exists(&project_root, session_id, &checkpoint.id)
                {
                    issues.push(ForkAuditIssue {
                        kind: "missing_checkpoint_ref".to_string(),
                        session_id: session_id.clone(),
                        related_session_id: None,
                        checkpoint_id: Some(checkpoint.id.clone()),
                        commit: Some(checkpoint.commit.clone()),
                        detail: "dirty snapshot commit is reachable only through a missing checkpoint ref"
                            .to_string(),
                        repairable: true,
                    });
                }
            }
            let store = self.store().clone();
            let id = session_id.clone();
            let deleted = tokio::task::spawn_blocking(move || {
                store.list_events_by_kind(&id, "session.checkpoint.deleted", 1000)
            })
            .await??;
            for event in deleted {
                let Some(checkpoint_id) = event
                    .payload
                    .get("checkpoint_id")
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                if worktree::checkpoint_ref_exists(&project_root, session_id, checkpoint_id) {
                    issues.push(ForkAuditIssue {
                        kind: "stale_checkpoint_ref".to_string(),
                        session_id: session_id.clone(),
                        related_session_id: None,
                        checkpoint_id: Some(checkpoint_id.to_string()),
                        commit: event
                            .payload
                            .get("commit")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        detail: "tombstoned checkpoint still has a snapshot ref".to_string(),
                        repairable: true,
                    });
                }
            }
        }
        for (source_id, events) in &fork_events {
            for event in events {
                let Some(child_id) = event
                    .payload
                    .get("fork_session_id")
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                if !meta_ids.contains(child_id) {
                    issues.push(ForkAuditIssue {
                        kind: "missing_child_session".to_string(),
                        session_id: source_id.clone(),
                        related_session_id: Some(child_id.to_string()),
                        checkpoint_id: event
                            .payload
                            .get("checkpoint_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        commit: event
                            .payload
                            .get("commit")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        detail: "source session.forked points at a session that is not present in this project"
                            .to_string(),
                        repairable: false,
                    });
                }
            }
        }
        Ok(ForkAuditResult {
            project_id: project_id.to_string(),
            sessions: sessions.len(),
            checkpoints,
            issues,
        })
    }

    pub async fn repair_project_forks(&self, project_id: &str) -> Result<ForkRepairResult> {
        let audit = self.audit_project_forks(project_id).await?;
        let project = self.load_project(project_id).await?;
        let project_root = PathBuf::from(&project.root);
        let sessions = self.list_sessions(Some(project_id.to_string())).await?;
        let metas = self.load_session_metas(&sessions).await?;
        let git_lock = self.git_lock.clone();
        let mut repaired = 0usize;
        let mut failed = 0usize;
        let mut details = Vec::new();
        for issue in audit.issues.iter().filter(|issue| issue.repairable) {
            let result = match issue.kind.as_str() {
                "missing_fork_link" => {
                    let child_id = issue.related_session_id.as_deref();
                    let child = child_id.and_then(|id| metas.get(id));
                    match child {
                        Some(child) => self
                            .append_event(
                                &issue.session_id,
                                "session.forked",
                                serde_json::json!({
                                    "fork_session_id": child.session.id,
                                    "fork_goal": child.session.goal,
                                    "checkpoint_id": issue.checkpoint_id,
                                    "commit": issue.commit,
                                    "repaired": true,
                                }),
                            )
                            .await
                            .map(|_| "backfilled session.forked link".to_string()),
                        None => Err(anyhow!("child session is unavailable")),
                    }
                }
                "missing_checkpoint_ref" => {
                    let checkpoint_id = issue.checkpoint_id.clone();
                    let commit = issue.commit.clone();
                    match (checkpoint_id, commit) {
                        (Some(checkpoint_id), Some(commit)) => {
                            let root = project_root.clone();
                            let id = issue.session_id.clone();
                            let git_lock = git_lock.clone();
                            tokio::task::spawn_blocking(move || {
                                let _guard =
                                    git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
                                worktree::ensure_checkpoint_ref(&root, &id, &checkpoint_id, &commit)
                            })
                            .await?
                            .map(|_| "recreated checkpoint ref".to_string())
                        }
                        _ => Err(anyhow!("checkpoint commit is unavailable")),
                    }
                }
                "stale_checkpoint_ref" => {
                    let checkpoint_id = issue.checkpoint_id.clone();
                    match checkpoint_id {
                        Some(checkpoint_id) => {
                            let root = project_root.clone();
                            let id = issue.session_id.clone();
                            let git_lock = git_lock.clone();
                            tokio::task::spawn_blocking(move || {
                                let _guard =
                                    git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
                                worktree::delete_checkpoint_ref(&root, &id, &checkpoint_id)
                            })
                            .await?
                            .map(|_| "removed stale checkpoint ref".to_string())
                        }
                        None => Err(anyhow!("checkpoint id is unavailable")),
                    }
                }
                _ => Err(anyhow!("issue is not repairable")),
            };
            match result {
                Ok(detail) => {
                    repaired += 1;
                    details.push(format!("{}: {detail}", issue.session_id));
                }
                Err(error) => {
                    failed += 1;
                    details.push(format!("{}: {error:#}", issue.session_id));
                }
            }
        }
        Ok(ForkRepairResult {
            project_id: project_id.to_string(),
            repaired,
            failed,
            details,
        })
    }

    async fn load_project(&self, project_id: &str) -> Result<vp_protocol::types::Project> {
        let store = self.store().clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&id))
            .await??
            .ok_or_else(|| anyhow!("project not found: {project_id}"))
    }

    async fn load_session_metas(
        &self,
        sessions: &[Session],
    ) -> Result<HashMap<String, SessionMeta>> {
        let mut metas = HashMap::new();
        for session in sessions {
            let store = self.store().clone();
            let id = session.id.clone();
            let events = tokio::task::spawn_blocking(move || {
                store.list_events_by_kind(&id, "session.created", 1)
            })
            .await??;
            let payload = events.first().map(|event| &event.payload);
            metas.insert(
                session.id.clone(),
                SessionMeta {
                    session: session.clone(),
                    parent_id: payload
                        .and_then(|payload| payload.get("forked_from_session_id"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    checkpoint_id: payload
                        .and_then(|payload| payload.get("forked_from_checkpoint_id"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    commit: payload
                        .and_then(|payload| payload.get("forked_from_commit"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                },
            );
        }
        Ok(metas)
    }
}

fn build_forest(metas: &HashMap<String, SessionMeta>) -> (Vec<SessionForkTreeNode>, usize, bool) {
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots = Vec::new();
    for (id, meta) in metas {
        match &meta.parent_id {
            Some(parent) if metas.contains_key(parent) => {
                children.entry(parent.clone()).or_default().push(id.clone());
            }
            _ => roots.push(id.clone()),
        }
    }
    for ids in children.values_mut() {
        ids.sort_by(|left, right| {
            metas
                .get(left)
                .map(|meta| meta.session.created_at.as_str())
                .cmp(
                    &metas
                        .get(right)
                        .map(|meta| meta.session.created_at.as_str()),
                )
        });
    }
    roots.sort_by(|left, right| {
        metas
            .get(left)
            .map(|meta| meta.session.created_at.as_str())
            .cmp(
                &metas
                    .get(right)
                    .map(|meta| meta.session.created_at.as_str()),
            )
    });
    let mut total = 0usize;
    let mut truncated = false;
    let roots = roots
        .into_iter()
        .filter_map(|id| build_node(&id, metas, &children, 0, &mut total, &mut truncated))
        .collect();
    (roots, total, truncated)
}

fn build_node(
    id: &str,
    metas: &HashMap<String, SessionMeta>,
    children: &HashMap<String, Vec<String>>,
    depth: u32,
    total: &mut usize,
    truncated: &mut bool,
) -> Option<SessionForkTreeNode> {
    if *total >= MAX_TREE_NODES {
        *truncated = true;
        return None;
    }
    let meta = metas.get(id)?;
    *total += 1;
    let mut node_children = Vec::new();
    if depth < MAX_TREE_DEPTH {
        if let Some(ids) = children.get(id) {
            for child_id in ids {
                if let Some(node) =
                    build_node(child_id, metas, children, depth + 1, total, truncated)
                {
                    node_children.push(node);
                }
            }
        }
    } else if children.contains_key(id) {
        *truncated = true;
    }
    Some(SessionForkTreeNode {
        session: meta.session.clone(),
        checkpoint_id: meta.checkpoint_id.clone(),
        commit: meta.commit.clone(),
        children: node_children,
    })
}
