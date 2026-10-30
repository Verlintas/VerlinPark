use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    AgentRecord, Approval, Event, MergeQueueEntry, Project, Session, StoredMessage, TodoItem,
};

use crate::paths;

/// SQLite-backed metadata store plus an append-only JSONL event mirror.
///
/// The event log is the long-term source of truth; SQLite is the fast query
/// projection. For M0 the mirror is best-effort and SQLite is authoritative.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    data_dir: PathBuf,
}

impl Store {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        paths::ensure_private_dir(&data_dir)?;
        paths::ensure_private_dir(&data_dir.join("run"))?;
        paths::ensure_private_dir(&data_dir.join("sessions"))?;
        paths::ensure_private_dir(&data_dir.join("logs"))?;

        let conn = Connection::open(data_dir.join("verlinpark.db"))
            .with_context(|| format!("opening database in {}", data_dir.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                id            TEXT PRIMARY KEY,
                name          TEXT NOT NULL,
                root          TEXT NOT NULL UNIQUE,
                default_branch TEXT NOT NULL,
                created_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id         TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                goal       TEXT NOT NULL,
                status        TEXT NOT NULL,
                worktree      TEXT,
                branch        TEXT,
                token_budget  INTEGER,
                base_commit   TEXT,
                team_plan     TEXT,
                tokens_in     INTEGER NOT NULL DEFAULT 0,
                tokens_out    INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS events (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                type       TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (session_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_events_session_seq
                ON events(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_sessions_project
                ON sessions(project_id);

            CREATE TABLE IF NOT EXISTS agents (
                id         TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role       TEXT NOT NULL,
                name       TEXT NOT NULL DEFAULT 'Agent',
                title      TEXT,
                profile    TEXT,
                parent_id  TEXT,
                worktree   TEXT,
                branch     TEXT,
                status     TEXT NOT NULL,
                model      TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id           TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role         TEXT NOT NULL,
                content      TEXT NOT NULL,
                tool_calls   TEXT NOT NULL DEFAULT '[]',
                tool_call_id TEXT,
                created_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS approvals (
                id           TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                tool_call_id TEXT NOT NULL,
                tool         TEXT NOT NULL,
                input        TEXT NOT NULL,
                status       TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                resolved_at  TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id, created_at);
            CREATE TABLE IF NOT EXISTS todos (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                position   INTEGER NOT NULL,
                title      TEXT NOT NULL,
                status     TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (session_id, position)
            );

            CREATE INDEX IF NOT EXISTS idx_approvals_session
                ON approvals(session_id, created_at);

            CREATE TABLE IF NOT EXISTS merge_queue (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id    TEXT NOT NULL,
                agent_name  TEXT NOT NULL,
                task_id     TEXT,
                branch       TEXT NOT NULL,
                commit_hash  TEXT NOT NULL,
                base_commit  TEXT NOT NULL,
                status      TEXT NOT NULL,
                attempt     INTEGER NOT NULL DEFAULT 1,
                detail      TEXT,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_merge_queue_session
                ON merge_queue(session_id, created_at);
            "#,
        )?;
        ensure_column(&conn, "sessions", "worktree", "TEXT")?;
        ensure_column(&conn, "sessions", "branch", "TEXT")?;
        ensure_column(&conn, "sessions", "token_budget", "INTEGER")?;
        ensure_column(&conn, "sessions", "base_commit", "TEXT")?;
        ensure_column(&conn, "sessions", "team_plan", "TEXT")?;
        ensure_column(&conn, "sessions", "tokens_in", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(
            &conn,
            "sessions",
            "tokens_out",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            data_dir,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("database connection mutex poisoned"))
    }

    pub fn add_project(&self, root: &str, name: Option<&str>) -> Result<Project> {
        let root_path = PathBuf::from(root);
        if !root_path.is_dir() {
            return Err(anyhow!("project root is not a directory: {root}"));
        }
        let root_path = root_path
            .canonicalize()
            .with_context(|| format!("canonicalizing {root}"))?;
        let root = root_path.to_string_lossy().to_string();

        let name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                root_path
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "Untitled project".to_string());

        let default_branch = detect_default_branch(&root_path);
        let project = Project {
            id: Uuid::now_v7().to_string(),
            name,
            root: root.clone(),
            default_branch,
            created_at: now_rfc3339(),
        };

        let conn = self.lock()?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM projects WHERE root = ?1",
                params![project.root],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Err(anyhow!("project already added: {id}"));
        }
        conn.execute(
            "INSERT INTO projects (id, name, root, default_branch, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.id,
                project.name,
                project.root,
                project.default_branch,
                project.created_at
            ],
        )?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, root, default_branch, created_at
             FROM projects ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                root: row.get(2)?,
                default_branch: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, root, default_branch, created_at FROM projects WHERE id = ?1",
            params![id],
            |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    root: row.get(2)?,
                    default_branch: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_session(&self, project_id: &str, goal: &str) -> Result<Session> {
        self.insert_session(
            Uuid::now_v7().to_string(),
            project_id,
            goal,
            None,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_worktree(
        &self,
        id: &str,
        project_id: &str,
        goal: &str,
        worktree: &str,
        branch: &str,
        token_budget: Option<u64>,
        base_commit: &str,
    ) -> Result<Session> {
        self.insert_session(
            id.to_string(),
            project_id,
            goal,
            Some(worktree.to_string()),
            Some(branch.to_string()),
            token_budget,
            Some(base_commit.to_string()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_session(
        &self,
        id: String,
        project_id: &str,
        goal: &str,
        worktree: Option<String>,
        branch: Option<String>,
        token_budget: Option<u64>,
        base_commit: Option<String>,
    ) -> Result<Session> {
        if self.get_project(project_id)?.is_none() {
            return Err(anyhow!("project not found: {project_id}"));
        }
        let goal = goal.trim();
        if goal.is_empty() {
            return Err(anyhow!("session goal must not be empty"));
        }
        let session = Session {
            id,
            project_id: project_id.to_string(),
            goal: goal.to_string(),
            status: "draft".to_string(),
            worktree,
            branch,
            token_budget,
            base_commit,
            tokens_in: 0,
            tokens_out: 0,
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sessions
             (id, project_id, goal, status, worktree, branch, token_budget, base_commit, tokens_in, tokens_out, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, ?9)",
            params![
                session.id,
                session.project_id,
                session.goal,
                session.status,
                session.worktree,
                session.branch,
                session.token_budget,
                session.base_commit,
                session.created_at
            ],
        )?;
        Ok(session)
    }

    pub fn set_session_worktree(
        &self,
        session_id: &str,
        worktree: &str,
        branch: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET worktree = ?1, branch = ?2 WHERE id = ?3",
            params![worktree, branch, session_id],
        )?;
        Ok(())
    }

    pub fn add_session_usage(&self, session_id: &str, input: u64, output: u64) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions
             SET tokens_in = tokens_in + ?1, tokens_out = tokens_out + ?2
             WHERE id = ?3",
            params![input as i64, output as i64, session_id],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<Session>> {
        let conn = self.lock()?;
        let mut sessions = Vec::new();
        match project_id {
            Some(project_id) => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, worktree, branch, token_budget, base_commit, tokens_in, tokens_out, created_at
                     FROM sessions WHERE project_id = ?1
                     ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map(params![project_id], map_session)?;
                sessions.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, worktree, branch, token_budget, base_commit, tokens_in, tokens_out, created_at
                     FROM sessions ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map([], map_session)?;
                sessions.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
        }
        Ok(sessions)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, goal, status, worktree, branch, token_budget, base_commit, tokens_in, tokens_out, created_at
             FROM sessions WHERE id = ?1",
            params![id],
            map_session,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn append_event(
        &self,
        session_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Event> {
        if self.get_session(session_id)?.is_none() {
            return Err(anyhow!("session not found: {session_id}"));
        }

        let event = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let seq: u64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )?;
            let event = Event {
                session_id: session_id.to_string(),
                seq,
                kind: kind.to_string(),
                payload,
                created_at: now_rfc3339(),
            };
            tx.execute(
                "INSERT INTO events (session_id, seq, type, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    event.session_id,
                    event.seq as i64,
                    event.kind,
                    serde_json::to_string(&event.payload)?,
                    event.created_at
                ],
            )?;
            tx.commit()?;
            event
        };

        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    fn append_event_mirror(&self, event: &Event) -> Result<()> {
        let dir = self.data_dir.join("sessions").join(&event.session_id);
        paths::ensure_private_dir(&dir)?;
        let path = dir.join("events.jsonl");
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    pub fn list_events(&self, session_id: &str, since_seq: u64, limit: u32) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events
             WHERE session_id = ?1 AND seq >= ?2
             ORDER BY seq ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![session_id, since_seq as i64, limit.clamp(1, 1000) as i64],
            |row| {
                let payload: String = row.get(3)?;
                Ok(Event {
                    session_id: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn project_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn session_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn create_leader_agent(&self, session_id: &str, model: &str) -> Result<String> {
        Ok(self
            .create_agent(session_id, "leader", "Leader", None, None, model)?
            .id)
    }

    pub fn create_agent(
        &self,
        session_id: &str,
        role: &str,
        name: &str,
        title: Option<&str>,
        parent_id: Option<&str>,
        model: &str,
    ) -> Result<AgentRecord> {
        let agent = AgentRecord {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            name: name.to_string(),
            title: title.map(ToOwned::to_owned),
            profile: None,
            parent_id: parent_id.map(ToOwned::to_owned),
            worktree: None,
            branch: None,
            status: "running".to_string(),
            model: model.to_string(),
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO agents
             (id, session_id, role, name, title, parent_id, worktree, branch, status, model, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                agent.id,
                agent.session_id,
                agent.role,
                agent.name,
                agent.title,
                agent.parent_id,
                agent.worktree,
                agent.branch,
                agent.status,
                agent.model,
                agent.created_at
            ],
        )?;
        Ok(agent)
    }

    pub fn set_agent_worktree(&self, id: &str, worktree: &str, branch: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agents SET worktree = ?1, branch = ?2 WHERE id = ?3",
            params![worktree, branch, id],
        )?;
        Ok(())
    }

    pub fn set_agent_profile(&self, id: &str, profile: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agents SET profile = ?1 WHERE id = ?2",
            params![profile, id],
        )?;
        Ok(())
    }

    pub fn update_agent_status(&self, id: &str, status: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agents SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        Ok(())
    }

    pub fn list_agents(&self, session_id: &str) -> Result<Vec<AgentRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, name, title, profile, parent_id, worktree, branch, status, model, created_at
             FROM agents WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(AgentRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                name: row.get(3)?,
                title: row.get(4)?,
                profile: row.get(5)?,
                parent_id: row.get(6)?,
                worktree: row.get(7)?,
                branch: row.get(8)?,
                status: row.get(9)?,
                model: row.get(10)?,
                created_at: row.get(11)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn update_session_status(&self, session_id: &str, status: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET status = ?1 WHERE id = ?2",
            params![status, session_id],
        )?;
        Ok(())
    }

    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_calls: &serde_json::Value,
        tool_call_id: Option<&str>,
    ) -> Result<StoredMessage> {
        let message = StoredMessage {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: tool_calls.clone(),
            tool_call_id: tool_call_id.map(ToOwned::to_owned),
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO messages
             (id, session_id, role, content, tool_calls, tool_call_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                message.id,
                message.session_id,
                message.role,
                message.content,
                serde_json::to_string(&message.tool_calls)?,
                message.tool_call_id,
                message.created_at
            ],
        )?;
        Ok(message)
    }

    pub fn list_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, tool_calls, tool_call_id, created_at
             FROM messages WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let tool_calls: String = row.get(4)?;
            Ok(StoredMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                tool_calls: serde_json::from_str(&tool_calls)
                    .unwrap_or_else(|_| serde_json::json!([])),
                tool_call_id: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn create_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: &serde_json::Value,
    ) -> Result<Approval> {
        let approval = Approval {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool: tool.to_string(),
            input: input.clone(),
            status: "pending".to_string(),
            created_at: now_rfc3339(),
            resolved_at: None,
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approvals
             (id, session_id, tool_call_id, tool, input, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
            params![
                approval.id,
                approval.session_id,
                approval.tool_call_id,
                approval.tool,
                serde_json::to_string(&approval.input)?,
                approval.created_at
            ],
        )?;
        Ok(approval)
    }

    pub fn resolve_approval(&self, approval_id: &str, approved: bool) -> Result<Option<Approval>> {
        let status = if approved { "approved" } else { "denied" };
        let resolved_at = now_rfc3339();
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE approvals SET status = ?1, resolved_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![status, resolved_at, approval_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            "SELECT id, session_id, tool_call_id, tool, input, status, created_at, resolved_at
             FROM approvals WHERE id = ?1",
            params![approval_id],
            |row| {
                let input: String = row.get(4)?;
                Ok(Approval {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    tool_call_id: row.get(2)?,
                    tool: row.get(3)?,
                    input: serde_json::from_str(&input).unwrap_or(serde_json::Value::Null),
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                    resolved_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn interrupt_running_agents(&self) -> Result<usize> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE agents SET status = 'failed' WHERE status = 'running'",
            [],
        )?;
        Ok(changed)
    }

    pub fn set_team_plan(&self, session_id: &str, plan: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET team_plan = ?1 WHERE id = ?2",
            params![plan, session_id],
        )?;
        Ok(())
    }

    pub fn get_team_plan(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT team_plan FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.flatten())
        .map_err(Into::into)
    }

    pub fn set_todos(&self, session_id: &str, items: &[TodoItem]) -> Result<()> {
        if self.get_session(session_id)?.is_none() {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let updated_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM todos WHERE session_id = ?1",
            params![session_id],
        )?;
        for (position, item) in items.iter().enumerate() {
            tx.execute(
                "INSERT INTO todos (session_id, position, title, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    session_id,
                    position as i64,
                    item.title,
                    item.status,
                    updated_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT title, status FROM todos WHERE session_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(TodoItem {
                title: row.get(0)?,
                status: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn interrupt_running_sessions(&self) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id FROM sessions WHERE status = 'running'")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        conn.execute(
            "UPDATE sessions SET status = 'failed' WHERE status = 'running'",
            [],
        )?;
        Ok(ids)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_merge(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
    ) -> Result<MergeQueueEntry> {
        let id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO merge_queue
             (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', 1, ?9, ?10, ?10)",
            params![
                id,
                session_id,
                agent_id,
                agent_name,
                task_id,
                branch,
                commit,
                base_commit,
                detail,
                now
            ],
        )?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )
        .map_err(Into::into)
    }

    pub fn update_merge_queue_entry(
        &self,
        id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE merge_queue
             SET status = ?1, detail = COALESCE(?2, detail), updated_at = ?3
             WHERE id = ?4",
            params![status, detail, now_rfc3339(), id],
        )?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_merge_queue(&self, session_id: &str) -> Result<Vec<MergeQueueEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_merge_queue_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_merge_queue_entry(&self, id: &str) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn retry_merge_queue_entry(
        &self,
        id: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
    ) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE merge_queue
             SET status = 'queued', attempt = attempt + 1, commit_hash = ?1,
                 base_commit = ?2, detail = ?3, updated_at = ?4
             WHERE id = ?5 AND status IN ('conflict', 'failed', 'interrupted')",
            params![commit, base_commit, detail, now_rfc3339(), id],
        )?;
        drop(conn);
        self.get_merge_queue_entry(id)
    }

    pub fn interrupt_merge_queue(&self) -> Result<Vec<MergeQueueEntry>> {
        let conn = self.lock()?;
        let ids = {
            let mut stmt = conn.prepare(
                "SELECT id FROM merge_queue WHERE status IN ('queued', 'merging', 'checking')",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        conn.execute(
            "UPDATE merge_queue
             SET status = 'interrupted',
                 detail = 'daemon restarted during integration',
                 updated_at = ?1
             WHERE status IN ('queued', 'merging', 'checking')",
            params![now_rfc3339()],
        )?;
        let mut entries = Vec::with_capacity(ids.len());
        for id in ids {
            entries.push(conn.query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )?);
        }
        Ok(entries)
    }

    pub fn list_approvals(&self, session_id: Option<&str>) -> Result<Vec<Approval>> {
        let conn = self.lock()?;
        let sql = match session_id {
            Some(_) => {
                "SELECT id, session_id, tool_call_id, tool, input, status, created_at, resolved_at FROM approvals WHERE session_id = ?1 ORDER BY created_at ASC"
            }
            None => {
                "SELECT id, session_id, tool_call_id, tool, input, status, created_at, resolved_at FROM approvals ORDER BY created_at ASC"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match session_id {
            Some(id) => stmt.query(params![id])?,
            None => stmt.query([])?,
        };
        let mut approvals = Vec::new();
        while let Some(row) = rows.next()? {
            let input: String = row.get(4)?;
            approvals.push(Approval {
                id: row.get(0)?,
                session_id: row.get(1)?,
                tool_call_id: row.get(2)?,
                tool: row.get(3)?,
                input: serde_json::from_str(&input).unwrap_or(serde_json::Value::Null),
                status: row.get(5)?,
                created_at: row.get(6)?,
                resolved_at: row.get(7)?,
            });
        }
        Ok(approvals)
    }
}

fn map_merge_queue_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<MergeQueueEntry> {
    Ok(MergeQueueEntry {
        id: row.get(0)?,
        session_id: row.get(1)?,
        agent_id: row.get(2)?,
        agent_name: row.get(3)?,
        task_id: row.get(4)?,
        branch: row.get(5)?,
        commit: row.get(6)?,
        base_commit: row.get(7)?,
        status: row.get(8)?,
        attempt: row.get::<_, i64>(9)? as u32,
        detail: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        project_id: row.get(1)?,
        goal: row.get(2)?,
        status: row.get(3)?,
        worktree: row.get(4)?,
        branch: row.get(5)?,
        token_budget: row.get::<_, Option<i64>>(6)?.map(|value| value as u64),
        base_commit: row.get(7)?,
        tokens_in: row.get::<_, i64>(8)? as u64,
        tokens_out: row.get::<_, i64>(9)? as u64,
        created_at: row.get(10)?,
    })
}

fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let exists = names.filter_map(Result::ok).any(|name| name == column);
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

fn detect_default_branch(root: &Path) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if branch.is_empty() {
                "main".to_string()
            } else {
                branch
            }
        }
        _ => "main".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, Store, TempDir) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        (data, store, repo)
    }

    #[test]
    fn adds_and_lists_projects() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        assert_eq!(project.name, "demo");
        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, project.id);
        assert_eq!(store.project_count().unwrap(), 1);
    }

    #[test]
    fn rejects_duplicate_project() {
        let (_data, store, repo) = test_store();
        store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let error = store
            .add_project(repo.path().to_str().unwrap(), Some("again"))
            .unwrap_err();
        assert!(error.to_string().contains("already added"));
    }

    #[test]
    fn creates_sessions_and_events() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "ship a feature").unwrap();
        assert_eq!(session.status, "draft");

        let first = store
            .append_event(
                &session.id,
                "session.created",
                serde_json::json!({"goal": "ship a feature"}),
            )
            .unwrap();
        let second = store
            .append_event(&session.id, "note", serde_json::json!({"text": "hello"}))
            .unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);

        let events = store.list_events(&session.id, 1, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, "note");

        let path = store
            .data_dir()
            .join("sessions")
            .join(&session.id)
            .join("events.jsonl");
        let mirror = std::fs::read_to_string(path).unwrap();
        assert_eq!(mirror.lines().count(), 2);
    }

    #[test]
    fn creates_lists_and_updates_agents() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "team").unwrap();
        let leader = store.create_leader_agent(&session.id, "model").unwrap();
        let worker = store
            .create_agent(
                &session.id,
                "worker",
                "Worker 1",
                None,
                Some(&leader),
                "model",
            )
            .unwrap();
        assert_eq!(worker.status, "running");
        store.update_agent_status(&worker.id, "idle").unwrap();
        let agents = store.list_agents(&session.id).unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].role, "leader");
        assert_eq!(agents[1].name, "Worker 1");
        assert_eq!(agents[1].status, "idle");
        assert_eq!(agents[1].parent_id.as_deref(), Some(leader.as_str()));
    }

    #[test]
    fn replaces_session_todos() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "plan").unwrap();
        store
            .set_todos(
                &session.id,
                &[
                    TodoItem {
                        title: "Inspect".into(),
                        status: "completed".into(),
                    },
                    TodoItem {
                        title: "Implement".into(),
                        status: "in_progress".into(),
                    },
                ],
            )
            .unwrap();
        let todos = store.list_todos(&session.id).unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[1].title, "Implement");
        store
            .set_todos(
                &session.id,
                &[TodoItem {
                    title: "Ship".into(),
                    status: "pending".into(),
                }],
            )
            .unwrap();
        let todos = store.list_todos(&session.id).unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].title, "Ship");
    }

    #[test]
    fn accumulates_session_usage() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "usage").unwrap();
        store.add_session_usage(&session.id, 10, 5).unwrap();
        store.add_session_usage(&session.id, 20, 7).unwrap();
        let session = store.get_session(&session.id).unwrap().unwrap();
        assert_eq!(session.tokens_in, 30);
        assert_eq!(session.tokens_out, 12);
    }

    #[test]
    fn interrupts_running_sessions_on_recovery() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "run").unwrap();
        store.update_session_status(&session.id, "running").unwrap();
        let interrupted = store.interrupt_running_sessions().unwrap();
        assert_eq!(interrupted, vec![session.id.clone()]);
        assert_eq!(
            store.get_session(&session.id).unwrap().unwrap().status,
            "failed"
        );
        assert!(store.interrupt_running_sessions().unwrap().is_empty());
    }

    #[test]
    fn migrates_legacy_sessions_table_with_worktree_columns() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        {
            let conn = Connection::open(data.path().join("verlinpark.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    goal TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );",
            )
            .unwrap();
        }
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "legacy").unwrap();
        assert!(session.worktree.is_none());
        assert!(session.branch.is_none());
        store
            .set_session_worktree(&session.id, "/tmp/worktree", "verlinpark/test")
            .unwrap();
        let loaded = store.get_session(&session.id).unwrap().unwrap();
        assert_eq!(loaded.worktree.as_deref(), Some("/tmp/worktree"));
        assert_eq!(loaded.branch.as_deref(), Some("verlinpark/test"));
    }

    #[test]
    fn persists_approvals_and_resolves_once() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "ship a feature").unwrap();
        let approval = store
            .create_approval(
                &session.id,
                "call_1",
                "write_file",
                &serde_json::json!({"path": "README.md"}),
            )
            .unwrap();
        assert_eq!(store.list_approvals(Some(&session.id)).unwrap().len(), 1);

        let resolved = store.resolve_approval(&approval.id, true).unwrap().unwrap();
        assert_eq!(resolved.status, "approved");
        assert!(
            store
                .resolve_approval(&approval.id, true)
                .unwrap()
                .is_none()
        );
        let listed = store.list_approvals(Some(&session.id)).unwrap();
        assert_eq!(listed[0].status, "approved");
    }

    #[test]
    fn persists_and_updates_merge_queue_entries() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "merge").unwrap();
        let entry = store
            .enqueue_merge(
                &session.id,
                "agent_1",
                "Worker 1",
                Some("T1"),
                "verlinpark/task",
                "abc123",
                "def456",
                Some("verifier approved"),
            )
            .unwrap();
        assert_eq!(entry.status, "queued");
        assert_eq!(entry.attempt, 1);
        assert_eq!(entry.task_id.as_deref(), Some("T1"));

        let updated = store
            .update_merge_queue_entry(&entry.id, "merged", Some("ok"))
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, "merged");
        assert_eq!(updated.detail.as_deref(), Some("ok"));
        let listed = store.list_merge_queue(&session.id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, entry.id);
    }

    #[test]
    fn interrupts_in_flight_merge_queue_entries() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "recover").unwrap();
        let entry = store
            .enqueue_merge(
                &session.id,
                "agent_1",
                "Worker 1",
                None,
                "verlinpark/task",
                "abc123",
                "def456",
                None,
            )
            .unwrap();
        store
            .update_merge_queue_entry(&entry.id, "merging", None)
            .unwrap();
        let interrupted = store.interrupt_merge_queue().unwrap();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].status, "interrupted");
        assert!(store.interrupt_merge_queue().unwrap().is_empty());
    }

    #[test]
    fn lists_sessions_by_project() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        store.create_session(&project.id, "one").unwrap();
        store.create_session(&project.id, "two").unwrap();
        let sessions = store.list_sessions(Some(&project.id)).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(store.list_sessions(None).unwrap().len(), 2);
    }
}
