use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    AgentMessage, AgentRecord, Approval, Artifact, ArtifactPolicy, Event, IntegrationPolicy,
    MailboxPolicy, MemoryItem, MergeQueueEntry, Project, ProjectBudget, ProjectUsageSession,
    Question, SearchHit, Session, SessionBundle, ShellPolicy, StoredMessage, TodoItem, UsageEntry,
    WorktreeRetentionPolicy,
};

use crate::paths;

/// Explicit projection repair derived from the event stream. Fields set to
/// `None` are left untouched; `replace_plan` distinguishes "clear the plan"
/// from "leave it alone".
#[derive(Debug, Clone)]
pub struct SessionRepair {
    pub status: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub replace_plan: bool,
    pub plan: Option<String>,
    pub todos: Option<Vec<TodoItem>>,
    pub messages: Option<Vec<StoredMessage>>,
    pub agent_messages: Option<Vec<AgentMessage>>,
    pub agents: Option<Vec<AgentRecord>>,
    pub queue: Option<Vec<MergeQueueEntry>>,
    pub artifacts: Option<Vec<Artifact>>,
    pub approvals: Option<Vec<Approval>>,
    pub memories: Option<Vec<MemoryItem>>,
    pub questions: Option<Vec<Question>>,
    pub artifact_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifestSnapshot {
    pub version: u32,
    pub recorded_at: String,
    pub project: Project,
    pub shell_policy: ShellPolicy,
    pub integration_policy: IntegrationPolicy,
    pub artifact_policy: ArtifactPolicy,
    pub mailbox_policy: MailboxPolicy,
    pub worktree_policy: WorktreeRetentionPolicy,
    pub budget: ProjectBudget,
}

/// Read the latest project snapshot for each project from `projects.jsonl`.
pub fn read_project_manifest(data_dir: &Path) -> Result<Vec<ProjectManifestSnapshot>> {
    let path = data_dir.join("projects.jsonl");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("opening project manifest {}", path.display()));
        }
    };
    let mut latest: std::collections::HashMap<String, ProjectManifestSnapshot> =
        std::collections::HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let snapshot: ProjectManifestSnapshot =
            serde_json::from_str(&line).context("parsing project manifest snapshot")?;
        latest.insert(snapshot.project.id.clone(), snapshot);
    }
    let mut snapshots = latest.into_values().collect::<Vec<_>>();
    snapshots.sort_by(|left, right| left.project.created_at.cmp(&right.project.created_at));
    Ok(snapshots)
}

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

            CREATE TABLE IF NOT EXISTS project_shell_policies (
                project_id      TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                network         TEXT NOT NULL,
                read_deny       TEXT NOT NULL,
                restrict_home   INTEGER NOT NULL DEFAULT 0,
                updated_at      TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_integration_policies (
                project_id         TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                auto_resolve       INTEGER NOT NULL,
                max_auto_attempts  INTEGER NOT NULL,
                max_resolve_tokens INTEGER,
                updated_at         TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id         TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                goal       TEXT NOT NULL,
                status        TEXT NOT NULL,
                worktree      TEXT,
                branch        TEXT,
                token_budget  INTEGER,
                cost_budget_usd REAL,
                time_budget_secs INTEGER,
                cost_spent_usd REAL NOT NULL DEFAULT 0,
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

            CREATE TABLE IF NOT EXISTS agent_messages (
                id            TEXT PRIMARY KEY,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                from_agent_id TEXT NOT NULL,
                from_name     TEXT NOT NULL,
                to_agent_id   TEXT NOT NULL,
                to_name       TEXT NOT NULL,
                kind          TEXT NOT NULL,
                body          TEXT NOT NULL,
                payload       TEXT NOT NULL DEFAULT '{}',
                status        TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                read_at       TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_agent_messages_session
                ON agent_messages(session_id, created_at);

            CREATE TABLE IF NOT EXISTS artifacts (
                id         TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id   TEXT NOT NULL,
                agent_name TEXT NOT NULL,
                task_id    TEXT,
                name       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                mime_type  TEXT NOT NULL,
                content    TEXT NOT NULL,
                storage    TEXT NOT NULL DEFAULT 'inline',
                compression TEXT NOT NULL DEFAULT 'none',
                size_bytes INTEGER NOT NULL DEFAULT 0,
                sha256     TEXT NOT NULL DEFAULT '',
                path       TEXT,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_artifacts_session
                ON artifacts(session_id, created_at);

            CREATE TABLE IF NOT EXISTS usage_ledger (
                id            TEXT PRIMARY KEY,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id      TEXT NOT NULL,
                agent_name    TEXT NOT NULL,
                role          TEXT NOT NULL,
                task_id       TEXT,
                department    TEXT,
                model         TEXT NOT NULL,
                input_tokens  INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                estimated     INTEGER NOT NULL,
                cost_usd      REAL NOT NULL,
                created_at    TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_usage_ledger_session
                ON usage_ledger(session_id, created_at);

            CREATE TABLE IF NOT EXISTS memories (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                agent_id    TEXT NOT NULL,
                agent_name  TEXT NOT NULL,
                kind        TEXT NOT NULL,
                title       TEXT NOT NULL,
                body        TEXT NOT NULL,
                tags        TEXT NOT NULL,
                created_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memories_project
                ON memories(project_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_memories_session
                ON memories(session_id, created_at);

            CREATE TABLE IF NOT EXISTS questions (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id    TEXT NOT NULL,
                agent_name  TEXT NOT NULL,
                question    TEXT NOT NULL,
                options     TEXT NOT NULL,
                status      TEXT NOT NULL,
                answer      TEXT,
                created_at  TEXT NOT NULL,
                answered_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_questions_session
                ON questions(session_id, created_at);

            CREATE TABLE IF NOT EXISTS project_worktree_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                older_than_secs  INTEGER NOT NULL DEFAULT 604800,
                delete_branches  INTEGER NOT NULL DEFAULT 0,
                keep_failed      INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS project_artifact_policies (
                project_id   TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                auto_gc      INTEGER NOT NULL,
                max_bytes    INTEGER,
                max_age_secs INTEGER,
                max_count    INTEGER,
                updated_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_budgets (
                project_id     TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                token_budget   INTEGER,
                cost_budget_usd REAL,
                updated_at     TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_mailbox_policies (
                project_id    TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                max_age_secs  INTEGER,
                max_count     INTEGER,
                delete_unread INTEGER NOT NULL DEFAULT 0,
                auto_gc       INTEGER NOT NULL DEFAULT 0,
                updated_at    TEXT NOT NULL
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

            CREATE TABLE IF NOT EXISTS merge_auto_attempts (
                entry_id TEXT PRIMARY KEY REFERENCES merge_queue(id) ON DELETE CASCADE,
                attempts INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )?;
        ensure_column(&conn, "sessions", "worktree", "TEXT")?;
        ensure_column(&conn, "sessions", "branch", "TEXT")?;
        ensure_column(&conn, "sessions", "token_budget", "INTEGER")?;
        ensure_column(&conn, "sessions", "cost_budget_usd", "REAL")?;
        ensure_column(&conn, "sessions", "time_budget_secs", "INTEGER")?;
        ensure_column(
            &conn,
            "sessions",
            "cost_spent_usd",
            "REAL NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "sessions", "base_commit", "TEXT")?;
        ensure_column(&conn, "sessions", "team_plan", "TEXT")?;
        ensure_column(
            &conn,
            "artifacts",
            "storage",
            "TEXT NOT NULL DEFAULT 'inline'",
        )?;
        ensure_column(
            &conn,
            "artifacts",
            "size_bytes",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "artifacts", "sha256", "TEXT NOT NULL DEFAULT ''")?;
        ensure_column(&conn, "artifacts", "path", "TEXT")?;
        ensure_column(
            &conn,
            "artifacts",
            "compression",
            "TEXT NOT NULL DEFAULT 'none'",
        )?;
        ensure_column(&conn, "usage_ledger", "department", "TEXT")?;
        ensure_column(
            &conn,
            "project_shell_policies",
            "restrict_home",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_mailbox_policies",
            "delete_unread",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_mailbox_policies",
            "auto_gc",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
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

    /// Append a full project/policy snapshot to `projects.jsonl`. The latest
    /// line per project is used by `read_project_manifest` for rebuilds.
    pub fn record_project_manifest(&self) -> Result<()> {
        let projects = self.list_projects()?;
        if projects.is_empty() {
            return Ok(());
        }
        let path = self.data_dir.join("projects.jsonl");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening project manifest {}", path.display()))?;
        for project in projects {
            let snapshot = ProjectManifestSnapshot {
                version: 1,
                recorded_at: now_rfc3339(),
                shell_policy: self.get_shell_policy(&project.id)?,
                integration_policy: self.get_integration_policy(&project.id)?,
                artifact_policy: self.get_artifact_policy(&project.id)?,
                mailbox_policy: self.get_mailbox_policy(&project.id)?,
                worktree_policy: self.get_worktree_policy(&project.id)?,
                budget: self.get_project_budget(&project.id)?,
                project,
            };
            serde_json::to_writer(&mut file, &snapshot)?;
            file.write_all(b"\n")?;
        }
        file.flush()?;
        paths::ensure_private_file(&path)?;
        Ok(())
    }

    pub fn insert_project_snapshot(&self, project: &Project) -> Result<()> {
        let conn = self.lock()?;
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
        Ok(())
    }

    pub fn insert_session_snapshot(&self, session: &Session) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sessions
             (id, project_id, goal, status, worktree, branch, token_budget,
              cost_budget_usd, time_budget_secs, cost_spent_usd, base_commit,
              team_plan, tokens_in, tokens_out, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, ?13, ?14)",
            params![
                session.id,
                session.project_id,
                session.goal,
                session.status,
                session.worktree,
                session.branch,
                session.token_budget,
                session.cost_budget_usd,
                session.time_budget_secs,
                session.cost_spent_usd,
                session.base_commit,
                session.tokens_in as i64,
                session.tokens_out as i64,
                session.created_at
            ],
        )?;
        Ok(())
    }

    pub fn get_shell_policy(&self, project_id: &str) -> Result<ShellPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT network, read_deny, restrict_home
                 FROM project_shell_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()?;
        match stored {
            Some((network, read_deny, restrict_home)) => Ok(ShellPolicy {
                network,
                read_deny: serde_json::from_str(&read_deny)
                    .context("parsing stored shell read-deny policy")?,
                restrict_home,
            }),
            None => Ok(ShellPolicy::default()),
        }
    }

    pub fn set_shell_policy(&self, project_id: &str, policy: &ShellPolicy) -> Result<()> {
        let read_deny = serde_json::to_string(&policy.read_deny)
            .context("serializing shell read-deny policy")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_shell_policies
             (project_id, network, read_deny, restrict_home, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                network = excluded.network,
                read_deny = excluded.read_deny,
                restrict_home = excluded.restrict_home,
                updated_at = excluded.updated_at",
            params![
                project_id,
                policy.network,
                read_deny,
                if policy.restrict_home { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn get_integration_policy(&self, project_id: &str) -> Result<IntegrationPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT auto_resolve, max_auto_attempts, max_resolve_tokens
                 FROM project_integration_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(IntegrationPolicy {
                        auto_resolve: row.get::<_, i64>(0)? != 0,
                        max_auto_attempts: row.get::<_, i64>(1)? as u32,
                        max_resolve_tokens: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_integration_policy(
        &self,
        project_id: &str,
        policy: &IntegrationPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_integration_policies
                (project_id, auto_resolve, max_auto_attempts, max_resolve_tokens, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                auto_resolve = excluded.auto_resolve,
                max_auto_attempts = excluded.max_auto_attempts,
                max_resolve_tokens = excluded.max_resolve_tokens,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.auto_resolve { 1 } else { 0 },
                policy.max_auto_attempts as i64,
                policy.max_resolve_tokens.map(|value| value as i64),
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn auto_resolve_attempts(&self, entry_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let attempts = conn
            .query_row(
                "SELECT attempts FROM merge_auto_attempts WHERE entry_id = ?1",
                params![entry_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(attempts as u32)
    }

    pub fn bump_auto_resolve_attempts(&self, entry_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO merge_auto_attempts (entry_id, attempts) VALUES (?1, 1)
             ON CONFLICT(entry_id) DO UPDATE SET attempts = attempts + 1",
            params![entry_id],
        )?;
        let attempts = conn.query_row(
            "SELECT attempts FROM merge_auto_attempts WHERE entry_id = ?1",
            params![entry_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(attempts as u32)
    }

    pub fn create_session(&self, project_id: &str, goal: &str) -> Result<Session> {
        self.create_session_with_limits(project_id, goal, None, None, None)
    }

    pub fn create_session_with_limits(
        &self,
        project_id: &str,
        goal: &str,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
    ) -> Result<Session> {
        self.insert_session(
            Uuid::now_v7().to_string(),
            project_id,
            goal,
            None,
            None,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
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
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        base_commit: &str,
    ) -> Result<Session> {
        self.insert_session(
            id.to_string(),
            project_id,
            goal,
            Some(worktree.to_string()),
            Some(branch.to_string()),
            token_budget,
            cost_budget_usd,
            time_budget_secs,
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
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
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
            cost_budget_usd,
            time_budget_secs,
            cost_spent_usd: 0.0,
            base_commit,
            tokens_in: 0,
            tokens_out: 0,
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sessions
             (id, project_id, goal, status, worktree, branch, token_budget,
              cost_budget_usd, time_budget_secs, cost_spent_usd, base_commit,
              tokens_in, tokens_out, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, 0, 0, ?11)",
            params![
                session.id,
                session.project_id,
                session.goal,
                session.status,
                session.worktree,
                session.branch,
                session.token_budget,
                session.cost_budget_usd,
                session.time_budget_secs,
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

    #[allow(clippy::too_many_arguments)]
    pub fn create_agent_message_with_event(
        &self,
        session_id: &str,
        from_agent_id: &str,
        from_name: &str,
        to_agent_id: &str,
        to_name: &str,
        kind: &str,
        body: &str,
        payload: &serde_json::Value,
    ) -> Result<(AgentMessage, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let message = AgentMessage {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            from_agent_id: from_agent_id.to_string(),
            from_name: from_name.to_string(),
            to_agent_id: to_agent_id.to_string(),
            to_name: to_name.to_string(),
            kind: kind.to_string(),
            body: body.to_string(),
            payload: payload.clone(),
            status: "unread".to_string(),
            created_at: now_rfc3339(),
            read_at: None,
        };
        tx.execute(
            "INSERT INTO agent_messages
             (id, session_id, from_agent_id, from_name, to_agent_id, to_name,
              kind, body, payload, status, created_at, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)",
            params![
                message.id,
                message.session_id,
                message.from_agent_id,
                message.from_name,
                message.to_agent_id,
                message.to_name,
                message.kind,
                message.body,
                serde_json::to_string(&message.payload)?,
                message.status,
                message.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "mailbox.message_sent",
            serde_json::to_value(&message)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((message, event))
    }

    pub fn list_agent_messages(
        &self,
        session_id: &str,
        to_agent_id: Option<&str>,
        unread_only: bool,
    ) -> Result<Vec<AgentMessage>> {
        let conn = self.lock()?;
        let mut sql = String::from(
            "SELECT id, session_id, from_agent_id, from_name, to_agent_id, to_name,
                    kind, body, payload, status, created_at, read_at
             FROM agent_messages WHERE session_id = ?1",
        );
        if to_agent_id.is_some() {
            sql.push_str(" AND to_agent_id = ?2");
        }
        if unread_only {
            sql.push_str(" AND status = 'unread'");
        }
        sql.push_str(" ORDER BY created_at ASC, id ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = match to_agent_id {
            Some(agent_id) => stmt.query_map(params![session_id, agent_id], map_agent_message)?,
            None => stmt.query_map(params![session_id], map_agent_message)?,
        };
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn mark_agent_message_read(
        &self,
        message_id: &str,
    ) -> Result<Option<(AgentMessage, Option<Event>)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_rfc3339();
        let changed = tx.execute(
            "UPDATE agent_messages SET status = 'read', read_at = ?1
             WHERE id = ?2 AND status = 'unread'",
            params![now, message_id],
        )?;
        let message = tx
            .query_row(
                "SELECT id, session_id, from_agent_id, from_name, to_agent_id, to_name,
                        kind, body, payload, status, created_at, read_at
                 FROM agent_messages WHERE id = ?1",
                params![message_id],
                map_agent_message,
            )
            .optional()?;
        let Some(message) = message else {
            tx.commit()?;
            return Ok(None);
        };
        if changed == 0 {
            tx.commit()?;
            return Ok(Some((message, None)));
        }
        let event = insert_event_tx(
            &tx,
            &message.session_id,
            "mailbox.message_read",
            serde_json::json!({
                "message_id": message.id,
                "to_agent_id": message.to_agent_id,
                "read_at": message.read_at,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((message, Some(event))))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_artifact_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
        storage: &str,
        size_bytes: u64,
        sha256: &str,
        path: Option<&str>,
    ) -> Result<(Artifact, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let artifact = Artifact {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            task_id: task_id.map(str::to_string),
            name: name.to_string(),
            kind: kind.to_string(),
            mime_type: mime_type.to_string(),
            content: content.to_string(),
            storage: storage.to_string(),
            compression: "none".to_string(),
            size_bytes,
            sha256: sha256.to_string(),
            path: path.map(str::to_string),
            data_base64: None,
            created_at: now_rfc3339(),
        };
        tx.execute(
            "INSERT INTO artifacts
             (id, session_id, agent_id, agent_name, task_id, name, kind, mime_type,
              content, storage, compression, size_bytes, sha256, path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                artifact.id,
                artifact.session_id,
                artifact.agent_id,
                artifact.agent_name,
                artifact.task_id,
                artifact.name,
                artifact.kind,
                artifact.mime_type,
                artifact.content,
                artifact.storage,
                artifact.compression,
                artifact.size_bytes as i64,
                artifact.sha256,
                artifact.path,
                artifact.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "artifact.published",
            serde_json::json!({
                "artifact_id": artifact.id,
                "name": artifact.name,
                "kind": artifact.kind,
                "agent_id": artifact.agent_id,
                "agent_name": artifact.agent_name,
                "task_id": artifact.task_id,
                "mime_type": artifact.mime_type,
                "storage": artifact.storage,
                "compression": artifact.compression,
                "size_bytes": artifact.size_bytes,
                "sha256": artifact.sha256,
                "path": artifact.path,
                "content": artifact.content,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((artifact, event))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_artifact_file_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        name: &str,
        kind: &str,
        mime_type: &str,
        data: &[u8],
        sha256: &str,
    ) -> Result<(Artifact, Event)> {
        let id = Uuid::now_v7().to_string();
        let dir = self.data_dir.join("artifacts").join("blobs");
        paths::ensure_private_dir(&dir)?;
        let path = dir.join(format!("{sha256}.zst"));
        let created_file = !path.is_file();
        if created_file {
            let compressed = zstd::stream::encode_all(data, 3)
                .with_context(|| format!("compressing artifact {}", id))?;
            std::fs::write(&path, compressed)
                .with_context(|| format!("writing artifact blob {}", path.display()))?;
            paths::ensure_private_file(&path)?;
        }
        let artifact = Artifact {
            id: id.clone(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            task_id: task_id.map(str::to_string),
            name: name.to_string(),
            kind: kind.to_string(),
            mime_type: mime_type.to_string(),
            content: String::new(),
            storage: "file".to_string(),
            compression: "zstd".to_string(),
            size_bytes: data.len() as u64,
            sha256: sha256.to_string(),
            path: Some(path.to_string_lossy().to_string()),
            data_base64: None,
            created_at: now_rfc3339(),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO artifacts
             (id, session_id, agent_id, agent_name, task_id, name, kind, mime_type,
              content, storage, compression, size_bytes, sha256, path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '', 'file', 'zstd', ?9, ?10, ?11, ?12)",
            params![
                artifact.id,
                artifact.session_id,
                artifact.agent_id,
                artifact.agent_name,
                artifact.task_id,
                artifact.name,
                artifact.kind,
                artifact.mime_type,
                artifact.size_bytes as i64,
                artifact.sha256,
                artifact.path,
                artifact.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "artifact.published",
            serde_json::json!({
                "artifact_id": artifact.id,
                "name": artifact.name,
                "kind": artifact.kind,
                "agent_id": artifact.agent_id,
                "agent_name": artifact.agent_name,
                "task_id": artifact.task_id,
                "mime_type": artifact.mime_type,
                "storage": artifact.storage,
                "compression": artifact.compression,
                "size_bytes": artifact.size_bytes,
                "sha256": artifact.sha256,
                "path": artifact.path,
                "content": "",
            }),
        )?;
        if let Err(error) = tx.commit() {
            if created_file {
                let _ = std::fs::remove_file(&path);
            }
            return Err(error.into());
        }
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((artifact, event))
    }

    pub fn list_artifacts(&self, session_id: &str) -> Result<Vec<Artifact>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                    mime_type, content, storage, compression, size_bytes, sha256, path, created_at
             FROM artifacts WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_artifact)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_artifact(&self, artifact_id: &str) -> Result<Option<Artifact>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                    mime_type, content, storage, compression, size_bytes, sha256, path, created_at
             FROM artifacts WHERE id = ?1",
            params![artifact_id],
            map_artifact,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn materialize_artifact(&self, artifact_id: &str) -> Result<Option<std::path::PathBuf>> {
        let Some(artifact) = self.get_artifact(artifact_id)? else {
            return Ok(None);
        };
        if artifact.storage != "file" {
            return Ok(None);
        }
        let Some(source) = artifact.path.as_deref() else {
            return Err(anyhow!("artifact {artifact_id} has no stored path"));
        };
        if artifact.compression != "zstd" {
            return Ok(Some(std::path::PathBuf::from(source)));
        }
        let compressed = std::fs::read(source)
            .with_context(|| format!("reading compressed artifact {source}"))?;
        let data = zstd::stream::decode_all(&compressed[..])
            .with_context(|| format!("decompressing artifact {artifact_id}"))?;
        use sha2::{Digest, Sha256};
        let sha256 = format!("{:x}", Sha256::digest(&data));
        if !artifact.sha256.is_empty() && sha256 != artifact.sha256 {
            return Err(anyhow!(
                "artifact {artifact_id} content does not match its SHA-256"
            ));
        }
        let dir = self.data_dir.join("artifacts").join("materialized");
        paths::ensure_private_dir(&dir)?;
        let path = dir.join(&sha256);
        if !path.is_file() {
            std::fs::write(&path, data)
                .with_context(|| format!("writing materialized artifact {}", path.display()))?;
            paths::ensure_private_file(&path)?;
        }
        Ok(Some(path))
    }

    pub fn get_artifact_policy(&self, project_id: &str) -> Result<ArtifactPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT auto_gc, max_bytes, max_age_secs, max_count
                 FROM project_artifact_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(ArtifactPolicy {
                        auto_gc: row.get::<_, i64>(0)? != 0,
                        max_bytes: row.get::<_, Option<i64>>(1)?.map(|value| value as u64),
                        max_age_secs: row.get::<_, Option<i64>>(2)?.map(|value| value as u64),
                        max_count: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_artifact_policy(&self, project_id: &str, policy: &ArtifactPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_artifact_policies
                (project_id, auto_gc, max_bytes, max_age_secs, max_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                auto_gc = excluded.auto_gc,
                max_bytes = excluded.max_bytes,
                max_age_secs = excluded.max_age_secs,
                max_count = excluded.max_count,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.auto_gc { 1 } else { 0 },
                policy.max_bytes.map(|value| value as i64),
                policy.max_age_secs.map(|value| value as i64),
                policy.max_count.map(|value| value as i64),
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_artifacts_for_project(&self, project_id: &str) -> Result<Vec<Artifact>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT a.id, a.session_id, a.agent_id, a.agent_name, a.task_id, a.name,
                    a.kind, a.mime_type, a.content, a.storage, a.compression,
                    a.size_bytes, a.sha256, a.path, a.created_at
             FROM artifacts a
             JOIN sessions s ON s.id = a.session_id
             WHERE s.project_id = ?1
             ORDER BY a.created_at ASC, a.id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], map_artifact)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_artifact(&self, artifact_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute("DELETE FROM artifacts WHERE id = ?1", params![artifact_id])?;
        Ok(changed > 0)
    }

    pub fn get_project_budget(&self, project_id: &str) -> Result<ProjectBudget> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT token_budget, cost_budget_usd
                 FROM project_budgets WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(ProjectBudget {
                        token_budget: row.get::<_, Option<i64>>(0)?.map(|value| value as u64),
                        cost_budget_usd: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_project_budget(&self, project_id: &str, budget: &ProjectBudget) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_budgets
                (project_id, token_budget, cost_budget_usd, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id) DO UPDATE SET
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                updated_at = excluded.updated_at",
            params![
                project_id,
                budget.token_budget.map(|value| value as i64),
                budget.cost_budget_usd,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn project_usage(&self, project_id: &str) -> Result<(u64, f64)> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT COALESCE(SUM(tokens_in + tokens_out), 0),
                    COALESCE(SUM(cost_spent_usd), 0)
             FROM sessions WHERE project_id = ?1",
            params![project_id],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
        )
        .map_err(Into::into)
    }

    pub fn project_usage_by_session(&self, project_id: &str) -> Result<Vec<ProjectUsageSession>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.goal, s.status, s.tokens_in, s.tokens_out, s.cost_spent_usd,
                    (SELECT COUNT(*) FROM usage_ledger u WHERE u.session_id = s.id)
             FROM sessions s
             WHERE s.project_id = ?1
             ORDER BY s.created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(ProjectUsageSession {
                session_id: row.get(0)?,
                goal: row.get(1)?,
                status: row.get(2)?,
                calls: row.get::<_, i64>(6)? as u64,
                input_tokens: row.get::<_, i64>(3)? as u64,
                output_tokens: row.get::<_, i64>(4)? as u64,
                cost_usd: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn create_question_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        question: &str,
        options: &[String],
    ) -> Result<(Question, Event)> {
        let item = Question {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            question: question.to_string(),
            options: options.to_vec(),
            status: "pending".to_string(),
            answer: None,
            created_at: now_rfc3339(),
            answered_at: None,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO questions
             (id, session_id, agent_id, agent_name, question, options, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![
                item.id,
                item.session_id,
                item.agent_id,
                item.agent_name,
                item.question,
                serde_json::to_string(&item.options)?,
                item.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "question.requested",
            serde_json::to_value(&item)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((item, event))
    }

    pub fn answer_question_with_event(
        &self,
        question_id: &str,
        answer: &str,
    ) -> Result<Option<(Question, Event)>> {
        let answered_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE questions SET status = 'answered', answer = ?1, answered_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![answer, answered_at, question_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let mut question = tx.query_row(
            "SELECT id, session_id, agent_id, agent_name, question, options, status,
                    answer, created_at, answered_at FROM questions WHERE id = ?1",
            params![question_id],
            map_question,
        )?;
        let event = insert_event_tx(
            &tx,
            &question.session_id,
            "question.answered",
            serde_json::to_value(&question)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        question.status = "answered".to_string();
        Ok(Some((question, event)))
    }

    pub fn close_question_with_event(
        &self,
        question_id: &str,
        status: &str,
    ) -> Result<Option<(Question, Event)>> {
        if !matches!(status, "cancelled" | "expired") {
            return Err(anyhow!("question status must be cancelled or expired"));
        }
        let answered_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE questions SET status = ?1, answered_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![status, answered_at, question_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let question = tx.query_row(
            "SELECT id, session_id, agent_id, agent_name, question, options, status,
                    answer, created_at, answered_at FROM questions WHERE id = ?1",
            params![question_id],
            map_question,
        )?;
        let event = insert_event_tx(
            &tx,
            &question.session_id,
            if status == "cancelled" {
                "question.cancelled"
            } else {
                "question.expired"
            },
            serde_json::to_value(&question)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((question, event)))
    }

    pub fn list_questions(&self, session_id: Option<&str>) -> Result<Vec<Question>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, question, options, status,
                    answer, created_at, answered_at
             FROM questions
             WHERE (?1 IS NULL OR session_id = ?1)
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_question)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_session_questions(&self, session_id: &str) -> Result<Vec<Question>> {
        self.list_questions(Some(session_id))
    }

    pub fn replace_session_questions(
        &self,
        session_id: &str,
        questions: &[Question],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM questions WHERE session_id = ?1",
            params![session_id],
        )?;
        for question in questions {
            tx.execute(
                "INSERT INTO questions
                 (id, session_id, agent_id, agent_name, question, options, status, answer,
                  created_at, answered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    question.id,
                    question.session_id,
                    question.agent_id,
                    question.agent_name,
                    question.question,
                    serde_json::to_string(&question.options)?,
                    question.status,
                    question.answer,
                    question.created_at,
                    question.answered_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_memory_with_event(
        &self,
        session_id: &str,
        project_id: &str,
        agent_id: &str,
        agent_name: &str,
        kind: &str,
        title: &str,
        body: &str,
        tags: &[String],
    ) -> Result<(MemoryItem, Event)> {
        let memory = MemoryItem {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            project_id: project_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            tags: tags.to_vec(),
            created_at: now_rfc3339(),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO memories
             (id, session_id, project_id, agent_id, agent_name, kind, title, body, tags, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                memory.id,
                memory.session_id,
                memory.project_id,
                memory.agent_id,
                memory.agent_name,
                memory.kind,
                memory.title,
                memory.body,
                serde_json::to_string(&memory.tags)?,
                memory.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "memory.created",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((memory, event))
    }

    pub fn list_memories(
        &self,
        project_id: &str,
        query: Option<&str>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        let conn = self.lock()?;
        let pattern = query.map(|query| format!("%{query}%"));
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at
             FROM memories
             WHERE project_id = ?1
               AND (?2 IS NULL OR title LIKE ?2 OR body LIKE ?2 OR tags LIKE ?2)
               AND (?3 IS NULL OR kind = ?3)
             ORDER BY created_at DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![project_id, pattern, kind, limit.clamp(1, 500) as i64],
            |row| {
                let tags: String = row.get(8)?;
                Ok(MemoryItem {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    project_id: row.get(2)?,
                    agent_id: row.get(3)?,
                    agent_name: row.get(4)?,
                    kind: row.get(5)?,
                    title: row.get(6)?,
                    body: row.get(7)?,
                    tags: serde_json::from_str(&tags).unwrap_or_default(),
                    created_at: row.get(9)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_session_memories(&self, session_id: &str) -> Result<Vec<MemoryItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at
             FROM memories WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let tags: String = row.get(8)?;
            Ok(MemoryItem {
                id: row.get(0)?,
                session_id: row.get(1)?,
                project_id: row.get(2)?,
                agent_id: row.get(3)?,
                agent_name: row.get(4)?,
                kind: row.get(5)?,
                title: row.get(6)?,
                body: row.get(7)?,
                tags: serde_json::from_str(&tags).unwrap_or_default(),
                created_at: row.get(9)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn replace_session_memories(
        &self,
        session_id: &str,
        memories: &[MemoryItem],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM memories WHERE session_id = ?1",
            params![session_id],
        )?;
        for memory in memories {
            tx.execute(
                "INSERT INTO memories
                 (id, session_id, project_id, agent_id, agent_name, kind, title, body, tags, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    memory.id,
                    memory.session_id,
                    memory.project_id,
                    memory.agent_id,
                    memory.agent_name,
                    memory.kind,
                    memory.title,
                    memory.body,
                    serde_json::to_string(&memory.tags)?,
                    memory.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_worktree_policy(&self, project_id: &str) -> Result<WorktreeRetentionPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, older_than_secs, delete_branches, keep_failed
                 FROM project_worktree_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(WorktreeRetentionPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        older_than_secs: row.get::<_, i64>(1)? as u64,
                        delete_branches: row.get::<_, i64>(2)? != 0,
                        keep_failed: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_worktree_policy(
        &self,
        project_id: &str,
        policy: &WorktreeRetentionPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_worktree_policies
             (project_id, enabled, older_than_secs, delete_branches, keep_failed)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                older_than_secs = excluded.older_than_secs,
                delete_branches = excluded.delete_branches,
                keep_failed = excluded.keep_failed",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.older_than_secs as i64,
                if policy.delete_branches { 1 } else { 0 },
                if policy.keep_failed { 1 } else { 0 }
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_worktree_policies(&self) -> Result<Vec<(String, WorktreeRetentionPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, older_than_secs, delete_branches, keep_failed
             FROM project_worktree_policies WHERE enabled != 0",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                WorktreeRetentionPolicy {
                    enabled: true,
                    older_than_secs: row.get::<_, i64>(2)? as u64,
                    delete_branches: row.get::<_, i64>(3)? != 0,
                    keep_failed: row.get::<_, i64>(4)? != 0,
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_mailbox_policy(&self, project_id: &str) -> Result<MailboxPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT max_age_secs, max_count, delete_unread, auto_gc
                 FROM project_mailbox_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(MailboxPolicy {
                        max_age_secs: row.get::<_, Option<i64>>(0)?.map(|value| value as u64),
                        max_count: row.get::<_, Option<i64>>(1)?.map(|value| value as u64),
                        delete_unread: row.get::<_, i64>(2)? != 0,
                        auto_gc: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_mailbox_policy(&self, project_id: &str, policy: &MailboxPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_mailbox_policies
                (project_id, max_age_secs, max_count, delete_unread, auto_gc, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                max_age_secs = excluded.max_age_secs,
                max_count = excluded.max_count,
                delete_unread = excluded.delete_unread,
                auto_gc = excluded.auto_gc,
                updated_at = excluded.updated_at",
            params![
                project_id,
                policy.max_age_secs.map(|value| value as i64),
                policy.max_count.map(|value| value as i64),
                if policy.delete_unread { 1 } else { 0 },
                if policy.auto_gc { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_agent_messages_for_project(&self, project_id: &str) -> Result<Vec<AgentMessage>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.from_agent_id, m.from_name, m.to_agent_id,
                    m.to_name, m.kind, m.body, m.payload, m.status, m.created_at, m.read_at
             FROM agent_messages m
             JOIN sessions s ON s.id = m.session_id
             WHERE s.project_id = ?1
             ORDER BY m.created_at ASC, m.id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], map_agent_message)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_agent_message(&self, message_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "DELETE FROM agent_messages WHERE id = ?1",
            params![message_id],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_agent_message_with_event(&self, message_id: &str) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let message = tx
            .query_row(
                "SELECT session_id, from_agent_id, from_name, to_agent_id, to_name, kind
                 FROM agent_messages WHERE id = ?1",
                params![message_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((session_id, from_agent_id, from_name, to_agent_id, to_name, kind)) = message
        else {
            return Ok(None);
        };
        tx.execute(
            "DELETE FROM agent_messages WHERE id = ?1",
            params![message_id],
        )?;
        let event = insert_event_tx(
            &tx,
            &session_id,
            "mailbox.pruned",
            serde_json::json!({
                "message_id": message_id,
                "from_agent_id": from_agent_id,
                "from_name": from_name,
                "to_agent_id": to_agent_id,
                "to_name": to_name,
                "kind": kind,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    pub fn delete_artifact_with_event(
        &self,
        artifact_id: &str,
    ) -> Result<Option<(Event, Option<String>)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let artifact = tx
            .query_row(
                "SELECT session_id, name, kind, storage, size_bytes, sha256, path
                 FROM artifacts WHERE id = ?1",
                params![artifact_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((session_id, name, kind, storage, size_bytes, sha256, path)) = artifact else {
            return Ok(None);
        };
        tx.execute("DELETE FROM artifacts WHERE id = ?1", params![artifact_id])?;
        let remove_path = if let Some(path) = &path {
            let remaining: i64 = tx.query_row(
                "SELECT COUNT(*) FROM artifacts WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )?;
            (remaining == 0).then(|| path.clone())
        } else {
            None
        };
        let event = insert_event_tx(
            &tx,
            &session_id,
            "artifact.pruned",
            serde_json::json!({
                "artifact_id": artifact_id,
                "name": name,
                "kind": kind,
                "storage": storage,
                "size_bytes": size_bytes,
                "sha256": sha256,
                "path": path,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((event, remove_path)))
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

    pub fn add_session_cost_with_event(&self, session_id: &str, cost_usd: f64) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET cost_spent_usd = cost_spent_usd + ?1 WHERE id = ?2",
            params![cost_usd, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let total = tx.query_row(
            "SELECT cost_spent_usd FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, f64>(0),
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "agent.cost",
            serde_json::json!({
                "amount_usd": cost_usd,
                "total_usd": total,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_usage_entry_with_events(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        role: &str,
        task_id: Option<&str>,
        department: Option<&str>,
        model: &str,
        input: u64,
        output: u64,
        estimated: bool,
        cost_usd: f64,
    ) -> Result<(UsageEntry, Vec<Event>)> {
        let entry = UsageEntry {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            role: role.to_string(),
            task_id: task_id.map(str::to_string),
            department: department.map(str::to_string),
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            estimated,
            cost_usd,
            created_at: now_rfc3339(),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET tokens_in = tokens_in + ?1,
                 tokens_out = tokens_out + ?2,
                 cost_spent_usd = cost_spent_usd + ?3
             WHERE id = ?4",
            params![input as i64, output as i64, cost_usd, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        tx.execute(
            "INSERT INTO usage_ledger
             (id, session_id, agent_id, agent_name, role, task_id, department, model,
              input_tokens, output_tokens, estimated, cost_usd, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                entry.id,
                entry.session_id,
                entry.agent_id,
                entry.agent_name,
                entry.role,
                entry.task_id,
                entry.department,
                entry.model,
                entry.input_tokens as i64,
                entry.output_tokens as i64,
                if entry.estimated { 1 } else { 0 },
                entry.cost_usd,
                entry.created_at
            ],
        )?;
        let usage_event = insert_event_tx(
            &tx,
            session_id,
            "agent.usage",
            serde_json::json!({
                "input_tokens": input,
                "output_tokens": output,
                "estimated": estimated,
                "agent_id": agent_id,
                "agent_name": agent_name,
                "agent_role": role,
                "usage_id": entry.id,
                "task_id": task_id,
                "department": department,
                "model": model,
                "cost_usd": cost_usd,
            }),
        )?;
        let mut events = vec![usage_event];
        if cost_usd > 0.0 {
            let total = tx.query_row(
                "SELECT cost_spent_usd FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, f64>(0),
            )?;
            events.push(insert_event_tx(
                &tx,
                session_id,
                "agent.cost",
                serde_json::json!({
                    "amount_usd": cost_usd,
                    "total_usd": total,
                    "agent_id": agent_id,
                    "agent_name": agent_name,
                    "agent_role": role,
                    "task_id": task_id,
                    "department": department,
                    "model": model,
                }),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((entry, events))
    }

    pub fn usage_spent(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        department: Option<&str>,
    ) -> Result<(u64, f64, u64, f64)> {
        let conn = self.lock()?;
        let (task_tokens, task_cost) = if let Some(task_id) = task_id {
            conn.query_row(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0),
                        COALESCE(SUM(cost_usd), 0)
                 FROM usage_ledger WHERE session_id = ?1 AND task_id = ?2",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
            )?
        } else {
            (0, 0.0)
        };
        let (department_tokens, department_cost) = if let Some(department) = department {
            conn.query_row(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0),
                        COALESCE(SUM(cost_usd), 0)
                 FROM usage_ledger WHERE session_id = ?1 AND department = ?2",
                params![session_id, department],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
            )?
        } else {
            (0, 0.0)
        };
        Ok((task_tokens, task_cost, department_tokens, department_cost))
    }

    pub fn list_usage_entries(&self, session_id: &str) -> Result<Vec<UsageEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, role, task_id, department,
                    model, input_tokens, output_tokens, estimated, cost_usd, created_at
             FROM usage_ledger
             WHERE session_id = ?1
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(UsageEntry {
                id: row.get(0)?,
                session_id: row.get(1)?,
                agent_id: row.get(2)?,
                agent_name: row.get(3)?,
                role: row.get(4)?,
                task_id: row.get(5)?,
                department: row.get(6)?,
                model: row.get(7)?,
                input_tokens: row.get::<_, i64>(8)? as u64,
                output_tokens: row.get::<_, i64>(9)? as u64,
                estimated: row.get::<_, i64>(10)? != 0,
                cost_usd: row.get(11)?,
                created_at: row.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn replace_usage_ledger(&self, session_id: &str, entries: &[UsageEntry]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM usage_ledger WHERE session_id = ?1",
            params![session_id],
        )?;
        for entry in entries {
            tx.execute(
                "INSERT INTO usage_ledger
                 (id, session_id, agent_id, agent_name, role, task_id, department, model,
                  input_tokens, output_tokens, estimated, cost_usd, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    entry.id,
                    session_id,
                    entry.agent_id,
                    entry.agent_name,
                    entry.role,
                    entry.task_id,
                    entry.department,
                    entry.model,
                    entry.input_tokens as i64,
                    entry.output_tokens as i64,
                    if entry.estimated { 1 } else { 0 },
                    entry.cost_usd,
                    entry.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn add_session_usage_with_event(
        &self,
        session_id: &str,
        input: u64,
        output: u64,
        estimated: bool,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET tokens_in = tokens_in + ?1, tokens_out = tokens_out + ?2
             WHERE id = ?3",
            params![input as i64, output as i64, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "agent.usage",
            serde_json::json!({
                "input_tokens": input,
                "output_tokens": output,
                "estimated": estimated,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<Session>> {
        let conn = self.lock()?;
        let mut sessions = Vec::new();
        match project_id {
            Some(project_id) => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at
                     FROM sessions WHERE project_id = ?1
                     ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map(params![project_id], map_session)?;
                sessions.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at
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
            "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at
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
            let event = insert_event_tx(&tx, session_id, kind, payload)?;
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

    pub fn read_event_mirror(&self, session_id: &str) -> Result<Option<Vec<Event>>> {
        let path = self
            .data_dir
            .join("sessions")
            .join(session_id)
            .join("events.jsonl");
        if !path.is_file() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading event mirror {}", path.display()))?;
        let mut events = Vec::new();
        for (index, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event = serde_json::from_str::<Event>(line).with_context(|| {
                format!("parsing event mirror {} line {}", path.display(), index + 1)
            })?;
            events.push(event);
        }
        Ok(Some(events))
    }

    pub fn replace_events(&self, session_id: &str, events: &[Event]) -> Result<()> {
        if events.iter().any(|event| event.session_id != session_id) {
            return Err(anyhow!(
                "event mirror contains an event for a different session"
            ));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM events WHERE session_id = ?1",
            params![session_id],
        )?;
        for event in events {
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
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_all_events(&self, session_id: &str) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let payload: String = row.get(3)?;
            Ok(Event {
                session_id: row.get(0)?,
                seq: row.get::<_, i64>(1)? as u64,
                kind: row.get(2)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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

    /// Search session messages and event payloads. Returns up to `limit` rows,
    /// newest first. Callers that need to know whether more rows exist should
    /// pass `limit + 1`.
    pub fn search(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>> {
        let pattern = format!("%{}%", escape_like(query));
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;

        let mut hits = Vec::new();

        let mut message_stmt = conn.prepare(
            "SELECT m.id, m.session_id, s.project_id, s.goal, m.role, m.content, m.created_at
             FROM messages m JOIN sessions s ON s.id = m.session_id
             WHERE m.content LIKE ?1 ESCAPE '\\'
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY m.created_at DESC LIMIT ?3",
        )?;
        let messages = message_stmt.query_map(params![pattern, project_id, limit], |row| {
            let content: String = row.get(5)?;
            Ok(SearchHit {
                id: row.get(0)?,
                session_id: row.get(1)?,
                project_id: row.get(2)?,
                session_goal: row.get(3)?,
                source: "message".to_string(),
                role: Some(row.get(4)?),
                kind: None,
                seq: None,
                snippet: search_snippet(&content, query),
                created_at: row.get(6)?,
            })
        })?;
        for hit in messages {
            hits.push(hit?);
        }

        let mut event_stmt = conn.prepare(
            "SELECT e.session_id, s.project_id, s.goal, e.type, e.payload, e.seq, e.created_at
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE e.payload LIKE ?1 ESCAPE '\\'
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY e.created_at DESC LIMIT ?3",
        )?;
        let events = event_stmt.query_map(params![pattern, project_id, limit], |row| {
            let payload: String = row.get(4)?;
            let session_id: String = row.get(0)?;
            let seq = row.get::<_, i64>(5)? as u64;
            Ok(SearchHit {
                id: format!("{session_id}:{seq}"),
                session_id,
                project_id: row.get(1)?,
                session_goal: row.get(2)?,
                source: "event".to_string(),
                role: None,
                kind: Some(row.get(3)?),
                seq: Some(seq),
                snippet: search_snippet(&payload, query),
                created_at: row.get(6)?,
            })
        })?;
        for hit in events {
            hits.push(hit?);
        }

        hits.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        hits.truncate(limit as usize);
        Ok(hits)
    }

    pub fn integrity_check(&self) -> Result<String> {
        let conn = self.lock()?;
        conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(Into::into)
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

    pub fn create_agent_with_snapshot(
        &self,
        session_id: &str,
        role: &str,
        name: &str,
        title: Option<&str>,
        parent_id: Option<&str>,
        model: &str,
    ) -> Result<(AgentRecord, Event)> {
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
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
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
        let event = insert_event_tx(
            &tx,
            &agent.session_id,
            "agent.snapshot",
            serde_json::to_value(&agent)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((agent, event))
    }

    pub fn create_leader_agent_with_snapshot(
        &self,
        session_id: &str,
        model: &str,
    ) -> Result<(AgentRecord, Event)> {
        self.create_agent_with_snapshot(session_id, "leader", "Leader", None, None, model)
    }

    pub fn set_agent_worktree_with_snapshot(
        &self,
        id: &str,
        worktree: &str,
        branch: &str,
    ) -> Result<Option<(AgentRecord, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE agents SET worktree = ?1, branch = ?2 WHERE id = ?3",
            params![worktree, branch, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &agent.session_id,
            "agent.snapshot",
            serde_json::to_value(&agent)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((agent, event)))
    }

    pub fn set_agent_profile_with_snapshot(
        &self,
        id: &str,
        profile: &str,
    ) -> Result<Option<(AgentRecord, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE agents SET profile = ?1 WHERE id = ?2",
            params![profile, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &agent.session_id,
            "agent.snapshot",
            serde_json::to_value(&agent)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((agent, event)))
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

    pub fn update_agent_status_with_event(&self, id: &str, status: &str) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let session_id: String = tx
            .query_row(
                "SELECT session_id FROM agents WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("agent not found: {id}"))?;
        tx.execute(
            "UPDATE agents SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &session_id,
            "agent.updated",
            serde_json::json!({"agent_id": id, "status": status, "agent": agent}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn list_agents(&self, session_id: &str) -> Result<Vec<AgentRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, name, title, profile, parent_id, worktree, branch, status, model, created_at
             FROM agents WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC",
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

    pub fn rehydrate_session_with_events(
        &self,
        session_id: &str,
        worktree: &str,
        branch: &str,
    ) -> Result<Vec<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET worktree = ?1, branch = ?2, status = 'idle'
             WHERE id = ?3 AND status = 'imported'",
            params![worktree, branch, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!(
                "session is not an imported archive or does not exist: {session_id}"
            ));
        }
        let status_event = insert_event_tx(
            &tx,
            session_id,
            "session.updated",
            serde_json::json!({"status": "idle"}),
        )?;
        let rehydrated = insert_event_tx(
            &tx,
            session_id,
            "session.rehydrated",
            serde_json::json!({"worktree": worktree, "branch": branch}),
        )?;
        tx.commit()?;
        let events = vec![status_event, rehydrated];
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(events)
    }

    pub fn update_session_status_with_event(
        &self,
        session_id: &str,
        status: &str,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET status = ?1 WHERE id = ?2",
            params![status, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "session.updated",
            serde_json::json!({"status": status}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn append_message_with_event(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_calls: &serde_json::Value,
        tool_call_id: Option<&str>,
    ) -> Result<(StoredMessage, Event)> {
        let message = StoredMessage {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: tool_calls.clone(),
            tool_call_id: tool_call_id.map(ToOwned::to_owned),
            created_at: now_rfc3339(),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
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
        let event = insert_event_tx(
            &tx,
            session_id,
            "message.created",
            serde_json::to_value(&message)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((message, event))
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

    pub fn create_approval_with_event(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: &serde_json::Value,
        agent: Option<&serde_json::Value>,
    ) -> Result<(Approval, Event)> {
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
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
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
        let mut payload = serde_json::to_value(&approval)?;
        if let (Some(payload), Some(agent)) = (payload.as_object_mut(), agent)
            && let Some(agent) = agent.as_object()
        {
            for (key, value) in agent {
                payload.insert(key.clone(), value.clone());
            }
        }
        let event = insert_event_tx(&tx, session_id, "approval.requested", payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((approval, event))
    }

    pub fn resolve_approval_with_event(
        &self,
        approval_id: &str,
        approved: bool,
    ) -> Result<Option<(Approval, Event)>> {
        let status = if approved { "approved" } else { "denied" };
        let resolved_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE approvals SET status = ?1, resolved_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![status, resolved_at, approval_id],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(None);
        }
        let approval = tx.query_row(
            "SELECT id, session_id, tool_call_id, tool, input, status, created_at, resolved_at
             FROM approvals WHERE id = ?1",
            params![approval_id],
            |row| {
                Ok(Approval {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    tool_call_id: row.get(2)?,
                    tool: row.get(3)?,
                    input: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or(serde_json::Value::Null),
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                    resolved_at: row.get(7)?,
                })
            },
        )?;
        let event = insert_event_tx(
            &tx,
            &approval.session_id,
            "approval.resolved",
            serde_json::json!({
                "approval_id": approval.id,
                "approved": approved,
                "resolved_at": approval.resolved_at,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((approval, event)))
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
            "UPDATE agents SET status = 'failed' WHERE status IN ('running', 'waiting_input')",
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

    pub fn set_team_plan_with_event(&self, session_id: &str, plan: &str) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET team_plan = ?1 WHERE id = ?2",
            params![plan, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let payload = serde_json::from_str(plan).unwrap_or(serde_json::Value::Null);
        let event = insert_event_tx(&tx, session_id, "team_plan.submitted", payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
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

    pub fn set_todos_with_event(&self, session_id: &str, items: &[TodoItem]) -> Result<Event> {
        let updated_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(anyhow!("session not found: {session_id}"));
        }
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
        let event = insert_event_tx(
            &tx,
            session_id,
            "todo.updated",
            serde_json::json!({"items": items}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
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

    /// Atomically repair all in-flight state after a daemon restart and return
    /// the session ids plus the events that must be broadcast.
    pub fn recover_state(&self) -> Result<(Vec<String>, Vec<Event>)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let sessions = {
            let mut stmt =
                tx.prepare("SELECT id FROM sessions WHERE status IN ('running', 'paused')")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE sessions SET status = 'failed' WHERE status IN ('running', 'paused', 'waiting_input')",
            [],
        )?;
        let agents = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id FROM agents
                     WHERE status IN ('running', 'waiting_input')",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE agents SET status = 'failed' WHERE status = 'running'",
            [],
        )?;
        let approvals = {
            let mut stmt =
                tx.prepare("SELECT id, session_id FROM approvals WHERE status = 'pending'")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE approvals SET status = 'denied', resolved_at = ?1
             WHERE status = 'pending'",
            params![now],
        )?;
        let expired_questions = {
            let mut stmt =
                tx.prepare("SELECT id, session_id FROM questions WHERE status = 'pending'")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE questions SET status = 'expired', answered_at = ?1
             WHERE status = 'pending'",
            params![now],
        )?;
        let queue_ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM merge_queue WHERE status IN ('queued', 'merging', 'checking')",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE merge_queue
             SET status = 'interrupted',
                 detail = 'daemon restarted during integration',
                 updated_at = ?1
             WHERE status IN ('queued', 'merging', 'checking')",
            params![now],
        )?;

        let mut events = Vec::new();
        for session_id in &sessions {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "session.updated",
                serde_json::json!({"status": "failed"}),
            )?);
            events.push(insert_event_tx(
                &tx,
                session_id,
                "agent.failed",
                serde_json::json!({
                    "error": "The daemon restarted while this run was active."
                }),
            )?);
        }
        for (question_id, session_id) in &expired_questions {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "question.expired",
                serde_json::json!({
                    "question_id": question_id,
                    "status": "expired",
                    "error": "The daemon restarted before the question was answered."
                }),
            )?);
        }
        for (agent_id, session_id) in &agents {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "agent.updated",
                serde_json::json!({"agent_id": agent_id, "status": "failed"}),
            )?);
        }
        for (approval_id, session_id) in &approvals {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "approval.resolved",
                serde_json::json!({"approval_id": approval_id, "approved": false}),
            )?);
        }
        for id in &queue_ids {
            let entry = tx.query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )?;
            events.push(insert_event_tx(
                &tx,
                &entry.session_id,
                "merge_queue.updated",
                serde_json::json!({"entry": entry}),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((sessions, events))
    }

    pub fn repair_session_projections(
        &self,
        session_id: &str,
        repair: &SessionRepair,
    ) -> Result<()> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET status = ?1, tokens_in = ?2, tokens_out = ?3
             WHERE id = ?4",
            params![
                repair.status,
                repair.tokens_in as i64,
                repair.tokens_out as i64,
                session_id
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        if repair.replace_plan {
            tx.execute(
                "UPDATE sessions SET team_plan = ?1 WHERE id = ?2",
                params![repair.plan, session_id],
            )?;
        }
        if let Some(todos) = &repair.todos {
            tx.execute(
                "DELETE FROM todos WHERE session_id = ?1",
                params![session_id],
            )?;
            for (position, item) in todos.iter().enumerate() {
                tx.execute(
                    "INSERT INTO todos (session_id, position, title, status, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![session_id, position as i64, item.title, item.status, now],
                )?;
            }
        }
        if let Some(messages) = &repair.messages {
            tx.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                params![session_id],
            )?;
            for message in messages {
                tx.execute(
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
            }
        }
        if let Some(agent_messages) = &repair.agent_messages {
            tx.execute(
                "DELETE FROM agent_messages WHERE session_id = ?1",
                params![session_id],
            )?;
            for message in agent_messages {
                tx.execute(
                    "INSERT INTO agent_messages
                     (id, session_id, from_agent_id, from_name, to_agent_id, to_name,
                      kind, body, payload, status, created_at, read_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        message.id,
                        message.session_id,
                        message.from_agent_id,
                        message.from_name,
                        message.to_agent_id,
                        message.to_name,
                        message.kind,
                        message.body,
                        serde_json::to_string(&message.payload)?,
                        message.status,
                        message.created_at,
                        message.read_at
                    ],
                )?;
            }
        }
        if let Some(agents) = &repair.agents {
            tx.execute(
                "DELETE FROM agents WHERE session_id = ?1",
                params![session_id],
            )?;
            for agent in agents {
                tx.execute(
                    "INSERT INTO agents
                     (id, session_id, role, name, title, profile, parent_id, worktree, branch, status, model, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        agent.id,
                        agent.session_id,
                        agent.role,
                        agent.name,
                        agent.title,
                        agent.profile,
                        agent.parent_id,
                        agent.worktree,
                        agent.branch,
                        agent.status,
                        agent.model,
                        agent.created_at
                    ],
                )?;
            }
        }
        if let Some(queue) = &repair.queue {
            tx.execute(
                "DELETE FROM merge_queue WHERE session_id = ?1",
                params![session_id],
            )?;
            for entry in queue {
                tx.execute(
                    "INSERT INTO merge_queue
                     (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        entry.id,
                        entry.session_id,
                        entry.agent_id,
                        entry.agent_name,
                        entry.task_id,
                        entry.branch,
                        entry.commit,
                        entry.base_commit,
                        entry.status,
                        entry.attempt as i64,
                        entry.detail,
                        entry.created_at,
                        entry.updated_at
                    ],
                )?;
            }
        }
        if let Some(memories) = &repair.memories {
            tx.execute(
                "DELETE FROM memories WHERE session_id = ?1",
                params![session_id],
            )?;
            for memory in memories {
                tx.execute(
                    "INSERT INTO memories
                     (id, session_id, project_id, agent_id, agent_name, kind, title, body, tags, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        memory.id,
                        memory.session_id,
                        memory.project_id,
                        memory.agent_id,
                        memory.agent_name,
                        memory.kind,
                        memory.title,
                        memory.body,
                        serde_json::to_string(&memory.tags)?,
                        memory.created_at
                    ],
                )?;
            }
        }
        if let Some(questions) = &repair.questions {
            tx.execute(
                "DELETE FROM questions WHERE session_id = ?1",
                params![session_id],
            )?;
            for question in questions {
                tx.execute(
                    "INSERT INTO questions
                     (id, session_id, agent_id, agent_name, question, options, status, answer,
                      created_at, answered_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        question.id,
                        question.session_id,
                        question.agent_id,
                        question.agent_name,
                        question.question,
                        serde_json::to_string(&question.options)?,
                        question.status,
                        question.answer,
                        question.created_at,
                        question.answered_at
                    ],
                )?;
            }
        }
        if let Some(artifacts) = &repair.artifacts {
            tx.execute(
                "DELETE FROM artifacts WHERE session_id = ?1",
                params![session_id],
            )?;
            for artifact in artifacts {
                tx.execute(
                    "INSERT INTO artifacts
                     (id, session_id, agent_id, agent_name, task_id, name, kind, mime_type,
                      content, storage, compression, size_bytes, sha256, path, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        artifact.id,
                        artifact.session_id,
                        artifact.agent_id,
                        artifact.agent_name,
                        artifact.task_id,
                        artifact.name,
                        artifact.kind,
                        artifact.mime_type,
                        artifact.content,
                        artifact.storage,
                        artifact.compression,
                        artifact.size_bytes as i64,
                        artifact.sha256,
                        artifact.path,
                        artifact.created_at
                    ],
                )?;
            }
        }
        if let Some(approvals) = &repair.approvals {
            tx.execute(
                "DELETE FROM approvals WHERE session_id = ?1",
                params![session_id],
            )?;
            for approval in approvals {
                tx.execute(
                    "INSERT INTO approvals
                     (id, session_id, tool_call_id, tool, input, status, created_at, resolved_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        approval.id,
                        approval.session_id,
                        approval.tool_call_id,
                        approval.tool,
                        serde_json::to_string(&approval.input)?,
                        approval.status,
                        approval.created_at,
                        approval.resolved_at
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn import_session_bundle(&self, bundle: &SessionBundle) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let existing_root: Option<String> = tx
            .query_row(
                "SELECT root FROM projects WHERE id = ?1",
                params![bundle.project.id],
                |row| row.get(0),
            )
            .optional()?;
        match existing_root {
            Some(root) if root != bundle.project.root => {
                return Err(anyhow!(
                    "project {} already exists with a different root",
                    bundle.project.id
                ));
            }
            Some(_) => {}
            None => {
                tx.execute(
                    "INSERT INTO projects (id, name, root, default_branch, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        bundle.project.id,
                        bundle.project.name,
                        bundle.project.root,
                        bundle.project.default_branch,
                        bundle.project.created_at
                    ],
                )?;
            }
        }

        let session_exists: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![bundle.session.id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if session_exists {
            return Err(anyhow!("session already exists: {}", bundle.session.id));
        }

        let plan_json = bundle
            .team_plan
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        tx.execute(
            "INSERT INTO sessions
             (id, project_id, goal, status, worktree, branch, token_budget,
              cost_budget_usd, time_budget_secs, cost_spent_usd, base_commit,
              team_plan, tokens_in, tokens_out, created_at)
             VALUES (?1, ?2, ?3, 'imported', NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                bundle.session.id,
                bundle.session.project_id,
                bundle.session.goal,
                bundle.session.branch,
                bundle.session.token_budget,
                bundle.session.cost_budget_usd,
                bundle.session.time_budget_secs,
                bundle.session.cost_spent_usd,
                bundle.session.base_commit,
                plan_json,
                bundle.session.tokens_in as i64,
                bundle.session.tokens_out as i64,
                bundle.session.created_at
            ],
        )?;
        for agent in &bundle.agents {
            tx.execute(
                "INSERT INTO agents
                 (id, session_id, role, name, title, profile, parent_id, worktree, branch, status, model, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    agent.id,
                    agent.session_id,
                    agent.role,
                    agent.name,
                    agent.title,
                    agent.profile,
                    agent.parent_id,
                    agent.worktree,
                    agent.branch,
                    agent.status,
                    agent.model,
                    agent.created_at
                ],
            )?;
        }
        for message in &bundle.messages {
            tx.execute(
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
        }
        for (position, item) in bundle.todos.iter().enumerate() {
            tx.execute(
                "INSERT INTO todos (session_id, position, title, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    bundle.session.id,
                    position as i64,
                    item.title,
                    item.status,
                    bundle.exported_at
                ],
            )?;
        }
        for approval in &bundle.approvals {
            tx.execute(
                "INSERT INTO approvals
                 (id, session_id, tool_call_id, tool, input, status, created_at, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    approval.id,
                    approval.session_id,
                    approval.tool_call_id,
                    approval.tool,
                    serde_json::to_string(&approval.input)?,
                    approval.status,
                    approval.created_at,
                    approval.resolved_at
                ],
            )?;
        }
        for memory in &bundle.memories {
            tx.execute(
                "INSERT INTO memories
                 (id, session_id, project_id, agent_id, agent_name, kind, title, body, tags, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    memory.id,
                    memory.session_id,
                    memory.project_id,
                    memory.agent_id,
                    memory.agent_name,
                    memory.kind,
                    memory.title,
                    memory.body,
                    serde_json::to_string(&memory.tags)?,
                    memory.created_at
                ],
            )?;
        }
        for question in &bundle.questions {
            tx.execute(
                "INSERT INTO questions
                 (id, session_id, agent_id, agent_name, question, options, status, answer,
                  created_at, answered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    question.id,
                    question.session_id,
                    question.agent_id,
                    question.agent_name,
                    question.question,
                    serde_json::to_string(&question.options)?,
                    question.status,
                    question.answer,
                    question.created_at,
                    question.answered_at
                ],
            )?;
        }
        for entry in &bundle.merge_queue {
            tx.execute(
                "INSERT INTO merge_queue
                 (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    entry.id,
                    entry.session_id,
                    entry.agent_id,
                    entry.agent_name,
                    entry.task_id,
                    entry.branch,
                    entry.commit,
                    entry.base_commit,
                    entry.status,
                    entry.attempt as i64,
                    entry.detail,
                    entry.created_at,
                    entry.updated_at
                ],
            )?;
        }
        for message in &bundle.agent_messages {
            tx.execute(
                "INSERT INTO agent_messages
                 (id, session_id, from_agent_id, from_name, to_agent_id, to_name,
                  kind, body, payload, status, created_at, read_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    message.id,
                    message.session_id,
                    message.from_agent_id,
                    message.from_name,
                    message.to_agent_id,
                    message.to_name,
                    message.kind,
                    message.body,
                    serde_json::to_string(&message.payload)?,
                    message.status,
                    message.created_at,
                    message.read_at
                ],
            )?;
        }
        for artifact in &bundle.artifacts {
            let (content, storage, compression, size_bytes, sha256, path) =
                if let Some(encoded) = &artifact.data_base64 {
                    use base64::Engine as _;
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .context("decoding artifact data")?;
                    use sha2::{Digest, Sha256};
                    let sha256 = format!("{:x}", Sha256::digest(&data));
                    if !artifact.sha256.is_empty() && artifact.sha256 != sha256 {
                        return Err(anyhow!(
                            "artifact {} sha256 does not match its bundle content",
                            artifact.id
                        ));
                    }
                    let dir = self.data_dir.join("artifacts").join("blobs");
                    paths::ensure_private_dir(&dir)?;
                    let file = dir.join(format!("{sha256}.zst"));
                    if !file.is_file() {
                        let compressed = zstd::stream::encode_all(&data[..], 3)?;
                        std::fs::write(&file, compressed)?;
                        paths::ensure_private_file(&file)?;
                    }
                    (
                        String::new(),
                        "file".to_string(),
                        "zstd".to_string(),
                        data.len() as u64,
                        sha256,
                        Some(file.to_string_lossy().to_string()),
                    )
                } else {
                    (
                        artifact.content.clone(),
                        artifact.storage.clone(),
                        "none".to_string(),
                        artifact.size_bytes,
                        artifact.sha256.clone(),
                        None,
                    )
                };
            tx.execute(
                "INSERT INTO artifacts
                 (id, session_id, agent_id, agent_name, task_id, name, kind,
                  mime_type, content, storage, compression, size_bytes, sha256, path, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    artifact.id,
                    artifact.session_id,
                    artifact.agent_id,
                    artifact.agent_name,
                    artifact.task_id,
                    artifact.name,
                    artifact.kind,
                    artifact.mime_type,
                    content,
                    storage,
                    compression,
                    size_bytes as i64,
                    sha256,
                    path,
                    artifact.created_at
                ],
            )?;
        }
        for event in &bundle.events {
            if event.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle event belongs to a different session: {}",
                    event.session_id
                ));
            }
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
        }
        for entry in &bundle.usage {
            tx.execute(
                "INSERT INTO usage_ledger
                 (id, session_id, agent_id, agent_name, role, task_id, department, model,
                  input_tokens, output_tokens, estimated, cost_usd, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    entry.id,
                    bundle.session.id,
                    entry.agent_id,
                    entry.agent_name,
                    entry.role,
                    entry.task_id,
                    entry.department,
                    entry.model,
                    entry.input_tokens as i64,
                    entry.output_tokens as i64,
                    if entry.estimated { 1 } else { 0 },
                    entry.cost_usd,
                    entry.created_at
                ],
            )?;
        }
        let status_event = insert_event_tx(
            &tx,
            &bundle.session.id,
            "session.updated",
            serde_json::json!({"status": "imported"}),
        )?;
        let imported_event = insert_event_tx(
            &tx,
            &bundle.session.id,
            "session.imported",
            serde_json::json!({
                "exported_at": bundle.exported_at,
                "source_project_id": bundle.project.id,
            }),
        )?;
        tx.commit()?;
        for event in &bundle.events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        for event in [&status_event, &imported_event] {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(imported_event)
    }

    pub fn queued_session_prompts(&self) -> Result<Vec<(String, String)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT e.session_id, e.payload
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.status = 'queued' AND e.type = 'agent.queued'
             ORDER BY e.session_id, e.seq DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut prompts = std::collections::HashMap::new();
        for row in rows {
            let (session_id, payload) = row?;
            if prompts.contains_key(&session_id) {
                continue;
            }
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
            if let Some(prompt) = payload.get("prompt").and_then(serde_json::Value::as_str)
                && !prompt.trim().is_empty()
            {
                prompts.insert(session_id, prompt.to_string());
            }
        }
        Ok(prompts.into_iter().collect())
    }

    pub fn interrupt_running_sessions(&self) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id FROM sessions WHERE status IN ('running', 'paused', 'queued')")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        conn.execute(
            "UPDATE sessions SET status = 'failed' WHERE status IN ('running', 'paused', 'queued')",
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

    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_merge_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
    ) -> Result<(MergeQueueEntry, Event)> {
        let id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
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
        let entry = tx.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "merge_queue.enqueued",
            serde_json::json!({"entry": entry}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((entry, event))
    }

    pub fn update_merge_queue_entry_with_event(
        &self,
        id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<Option<(MergeQueueEntry, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE merge_queue
             SET status = ?1, detail = COALESCE(?2, detail), updated_at = ?3
             WHERE id = ?4",
            params![status, detail, now_rfc3339(), id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let entry = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )
            .optional()?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let event = insert_event_tx(
            &tx,
            &entry.session_id,
            "merge_queue.updated",
            serde_json::json!({"entry": entry}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((entry, event)))
    }

    pub fn retry_merge_queue_entry_with_event(
        &self,
        id: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
    ) -> Result<Option<(MergeQueueEntry, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE merge_queue
             SET status = 'queued', attempt = attempt + 1, commit_hash = ?1,
                 base_commit = ?2, detail = ?3, updated_at = ?4
             WHERE id = ?5 AND status IN ('conflict', 'failed', 'interrupted')",
            params![commit, base_commit, detail, now_rfc3339(), id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let entry = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, status, attempt, detail, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )
            .optional()?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let event = insert_event_tx(
            &tx,
            &entry.session_id,
            "merge_queue.updated",
            serde_json::json!({"entry": entry}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((entry, event)))
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

fn get_agent_tx(tx: &Transaction<'_>, id: &str) -> Result<AgentRecord> {
    tx.query_row(
        "SELECT id, session_id, role, name, title, profile, parent_id, worktree, branch, status, model, created_at
         FROM agents WHERE id = ?1",
        params![id],
        map_agent_record,
    )
    .optional()?
    .ok_or_else(|| anyhow!("agent not found: {id}"))
}

fn map_agent_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRecord> {
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
}

fn insert_event_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    kind: &str,
    payload: serde_json::Value,
) -> Result<Event> {
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
    Ok(event)
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

fn map_agent_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMessage> {
    let payload: String = row.get(8)?;
    Ok(AgentMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        from_agent_id: row.get(2)?,
        from_name: row.get(3)?,
        to_agent_id: row.get(4)?,
        to_name: row.get(5)?,
        kind: row.get(6)?,
        body: row.get(7)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        status: row.get(9)?,
        created_at: row.get(10)?,
        read_at: row.get(11)?,
    })
}

fn map_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: row.get(0)?,
        session_id: row.get(1)?,
        agent_id: row.get(2)?,
        agent_name: row.get(3)?,
        task_id: row.get(4)?,
        name: row.get(5)?,
        kind: row.get(6)?,
        mime_type: row.get(7)?,
        content: row.get(8)?,
        storage: row.get(9)?,
        compression: row.get(10)?,
        size_bytes: row.get::<_, i64>(11)? as u64,
        sha256: row.get(12)?,
        path: row.get(13)?,
        data_base64: None,
        created_at: row.get(14)?,
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
        cost_budget_usd: row.get(7)?,
        time_budget_secs: row.get::<_, Option<i64>>(8)?.map(|value| value as u64),
        cost_spent_usd: row.get(9)?,
        base_commit: row.get(10)?,
        tokens_in: row.get::<_, i64>(11)? as u64,
        tokens_out: row.get::<_, i64>(12)? as u64,
        created_at: row.get(13)?,
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

fn map_question(row: &rusqlite::Row<'_>) -> rusqlite::Result<Question> {
    let options: String = row.get(5)?;
    Ok(Question {
        id: row.get(0)?,
        session_id: row.get(1)?,
        agent_id: row.get(2)?,
        agent_name: row.get(3)?,
        question: row.get(4)?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        status: row.get(6)?,
        answer: row.get(7)?,
        created_at: row.get(8)?,
        answered_at: row.get(9)?,
    })
}

fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn search_snippet(text: &str, query: &str) -> String {
    let lower = text.to_lowercase();
    let needle = query.to_lowercase();
    let start = lower.find(&needle).unwrap_or(0);
    let char_start = lower[..start].chars().count();
    let chars = text.chars().collect::<Vec<_>>();
    let from = char_start.saturating_sub(60).min(chars.len());
    let to = (char_start + query.chars().count() + 120).min(chars.len());
    let mut snippet = chars[from..to]
        .iter()
        .collect::<String>()
        .replace(['\n', '\r', '\t'], " ");
    if from > 0 {
        snippet.insert(0, '…');
    }
    if to < chars.len() {
        snippet.push('…');
    }
    snippet
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
    fn searches_messages_and_events_with_a_project_filter() {
        let (_data, store, repo) = test_store();
        let other_repo = TempDir::new().unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let other = store
            .add_project(other_repo.path().to_str().unwrap(), Some("other"))
            .unwrap();
        let session = store
            .create_session(&project.id, "add rate limiting")
            .unwrap();
        let other_session = store
            .create_session(&other.id, "other rate limiting work")
            .unwrap();
        store
            .append_message(
                &session.id,
                "user",
                "Implement rate limiting for the API",
                &serde_json::json!([]),
                None,
            )
            .unwrap();
        store
            .append_event(
                &session.id,
                "tool.finished",
                serde_json::json!({"output": "rate limiting enabled"}),
            )
            .unwrap();
        store
            .append_message(
                &other_session.id,
                "user",
                "rate limiting in another project",
                &serde_json::json!([]),
                None,
            )
            .unwrap();
        store
            .append_message(
                &session.id,
                "user",
                "progress is 100% done",
                &serde_json::json!([]),
                None,
            )
            .unwrap();

        let all = store.search("rate limiting", None, 10).unwrap();
        assert_eq!(all.len(), 3);
        let scoped = store
            .search("rate limiting", Some(&project.id), 10)
            .unwrap();
        assert_eq!(scoped.len(), 2);
        assert!(scoped.iter().all(|hit| hit.project_id == project.id));
        assert!(scoped.iter().any(|hit| hit.source == "message"));
        assert!(scoped.iter().any(|hit| hit.source == "event"));

        let literal = store.search("100%", Some(&project.id), 10).unwrap();
        assert_eq!(literal.len(), 1);
        assert!(literal[0].snippet.contains("100%"));
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
        let leader_agent = agents.iter().find(|agent| agent.role == "leader").unwrap();
        let worker_agent = agents.iter().find(|agent| agent.role == "worker").unwrap();
        assert_eq!(worker_agent.name, "Worker 1");
        assert_eq!(worker_agent.status, "idle");
        assert_eq!(worker_agent.parent_id.as_deref(), Some(leader.as_str()));
        assert_eq!(leader_agent.id, leader);
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
    fn round_trips_worktree_retention_policy() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        assert_eq!(
            store.get_worktree_policy(&project.id).unwrap(),
            WorktreeRetentionPolicy::default()
        );
        let policy = WorktreeRetentionPolicy {
            enabled: true,
            older_than_secs: 86_400,
            delete_branches: true,
            keep_failed: false,
        };
        store.set_worktree_policy(&project.id, &policy).unwrap();
        assert_eq!(store.get_worktree_policy(&project.id).unwrap(), policy);
        let enabled = store.list_enabled_worktree_policies().unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].0, project.id);
        store
            .set_worktree_policy(
                &project.id,
                &WorktreeRetentionPolicy {
                    enabled: false,
                    ..policy
                },
            )
            .unwrap();
        assert!(store.list_enabled_worktree_policies().unwrap().is_empty());
    }

    #[test]
    fn records_and_reads_the_latest_project_manifest_snapshot() {
        let (data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        store
            .set_shell_policy(
                &project.id,
                &ShellPolicy {
                    network: "deny".into(),
                    read_deny: vec!["~/secret".into()],
                    restrict_home: true,
                },
            )
            .unwrap();
        store
            .set_project_budget(
                &project.id,
                &ProjectBudget {
                    token_budget: Some(1234),
                    cost_budget_usd: Some(2.5),
                },
            )
            .unwrap();
        store.record_project_manifest().unwrap();
        store
            .set_shell_policy(
                &project.id,
                &ShellPolicy {
                    network: "allow".into(),
                    read_deny: Vec::new(),
                    restrict_home: false,
                },
            )
            .unwrap();
        store.record_project_manifest().unwrap();

        let snapshots = read_project_manifest(data.path()).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].project.id, project.id);
        assert_eq!(snapshots[0].shell_policy.network, "allow");
        assert!(!snapshots[0].shell_policy.restrict_home);
        assert_eq!(snapshots[0].budget.token_budget, Some(1234));
        assert_eq!(snapshots[0].budget.cost_budget_usd, Some(2.5));
    }

    #[test]
    fn groups_project_usage_by_session() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let alpha = store.create_session(&project.id, "alpha").unwrap();
        let beta = store.create_session(&project.id, "beta").unwrap();
        store
            .add_usage_entry_with_events(
                &alpha.id, "a1", "Alpha", "worker", None, None, "m", 10, 5, false, 0.5,
            )
            .unwrap();
        store
            .add_usage_entry_with_events(
                &alpha.id, "a1", "Alpha", "worker", None, None, "m", 2, 1, true, 0.0,
            )
            .unwrap();
        store
            .add_usage_entry_with_events(
                &beta.id, "b1", "Beta", "leader", None, None, "m", 7, 3, false, 0.25,
            )
            .unwrap();

        let usage = store.project_usage_by_session(&project.id).unwrap();
        assert_eq!(usage.len(), 2);
        let alpha_usage = usage.iter().find(|row| row.session_id == alpha.id).unwrap();
        assert_eq!(alpha_usage.calls, 2);
        assert_eq!(alpha_usage.input_tokens, 12);
        assert_eq!(alpha_usage.output_tokens, 6);
        assert!((alpha_usage.cost_usd - 0.5).abs() < 1e-9);
        let beta_usage = usage.iter().find(|row| row.session_id == beta.id).unwrap();
        assert_eq!(beta_usage.calls, 1);
        assert_eq!(beta_usage.input_tokens, 7);
    }

    #[test]
    fn creates_answers_and_expires_questions() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "questions").unwrap();
        let (question, event) = store
            .create_question_with_event(
                &session.id,
                "agent-1",
                "Leader",
                "Which database should I use?",
                &["SQLite".to_string(), "Postgres".to_string()],
            )
            .unwrap();
        assert_eq!(event.kind, "question.requested");
        assert_eq!(question.status, "pending");
        assert_eq!(store.list_questions(Some(&session.id)).unwrap().len(), 1);

        let (answered, answer_event) = store
            .answer_question_with_event(&question.id, "SQLite")
            .unwrap()
            .unwrap();
        assert_eq!(answered.status, "answered");
        assert_eq!(answered.answer.as_deref(), Some("SQLite"));
        assert_eq!(answer_event.kind, "question.answered");
        assert!(
            store
                .answer_question_with_event(&question.id, "again")
                .unwrap()
                .is_none()
        );

        let (expired, expired_event) = store
            .create_question_with_event(&session.id, "agent-1", "Leader", "Continue?", &[])
            .unwrap();
        let closed = store
            .close_question_with_event(&expired.id, "expired")
            .unwrap()
            .unwrap();
        assert_eq!(closed.0.status, "expired");
        assert_eq!(expired_event.kind, "question.requested");
        assert_eq!(closed.1.kind, "question.expired");
    }

    #[test]
    fn creates_and_filters_project_memories() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "memory").unwrap();
        let (first, event) = store
            .create_memory_with_event(
                &session.id,
                &project.id,
                "agent-1",
                "Leader",
                "decision",
                "Use deterministic merge",
                "Keep the Integrator deterministic.",
                &["architecture".to_string()],
            )
            .unwrap();
        assert_eq!(event.kind, "memory.created");
        assert_eq!(event.payload["title"], "Use deterministic merge");
        store
            .create_memory_with_event(
                &session.id,
                &project.id,
                "agent-1",
                "Leader",
                "source",
                "Hermes paper",
                "A source about agent memory.",
                &["research".to_string()],
            )
            .unwrap();

        let all = store.list_memories(&project.id, None, None, 10).unwrap();
        assert_eq!(all.len(), 2);
        let decisions = store
            .list_memories(&project.id, Some("deterministic"), None, 10)
            .unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].id, first.id);
        let sources = store
            .list_memories(&project.id, None, Some("source"), 10)
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(store.list_session_memories(&session.id).unwrap().len(), 2);

        store.replace_session_memories(&session.id, &[]).unwrap();
        assert!(store.list_session_memories(&session.id).unwrap().is_empty());
    }

    #[test]
    fn keeps_queued_sessions_and_their_prompts_across_recovery() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "queued").unwrap();
        store
            .append_event(
                &session.id,
                "agent.queued",
                serde_json::json!({"prompt": "continue the queue"}),
            )
            .unwrap();
        store.update_session_status(&session.id, "queued").unwrap();
        let (recovered, _) = store.recover_state().unwrap();
        assert!(recovered.is_empty());
        assert_eq!(
            store.get_session(&session.id).unwrap().unwrap().status,
            "queued"
        );
        let prompts = store.queued_session_prompts().unwrap();
        assert_eq!(
            prompts,
            vec![(session.id, "continue the queue".to_string())]
        );
    }

    #[test]
    fn writes_approval_projections_and_events_atomically() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "approval").unwrap();
        let (approval, requested) = store
            .create_approval_with_event(
                &session.id,
                "call-1",
                "shell",
                &serde_json::json!({"command": "true"}),
                Some(&serde_json::json!({"agent_id": "agent-1"})),
            )
            .unwrap();
        assert_eq!(requested.kind, "approval.requested");
        assert_eq!(requested.payload["agent_id"], "agent-1");
        let (resolved, event) = store
            .resolve_approval_with_event(&approval.id, true)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.status, "approved");
        assert_eq!(event.kind, "approval.resolved");
        assert_eq!(event.payload["approved"], true);
        assert!(
            store
                .resolve_approval_with_event(&approval.id, false)
                .unwrap()
                .is_none()
        );
        let events = store.list_all_events(&session.id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "approval.requested")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "approval.resolved")
                .count(),
            1
        );
    }

    #[test]
    fn interrupts_running_sessions_on_recovery() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "run").unwrap();
        store.update_session_status(&session.id, "running").unwrap();
        let paused = store.create_session(&project.id, "pause").unwrap();
        store.update_session_status(&paused.id, "paused").unwrap();
        let queued = store.create_session(&project.id, "queue").unwrap();
        store.update_session_status(&queued.id, "queued").unwrap();
        let interrupted = store.interrupt_running_sessions().unwrap();
        assert!(interrupted.contains(&session.id));
        assert!(interrupted.contains(&paused.id));
        assert!(interrupted.contains(&queued.id));
        assert_eq!(
            store.get_session(&session.id).unwrap().unwrap().status,
            "failed"
        );
        assert_eq!(
            store.get_session(&paused.id).unwrap().unwrap().status,
            "failed"
        );
        assert_eq!(
            store.get_session(&queued.id).unwrap().unwrap().status,
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
    fn fresh_schema_keeps_artifact_and_mailbox_policy_columns_distinct() {
        let (_data, store, _repo) = test_store();
        let conn = store.lock().unwrap();
        let columns = |table: &str| {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        };
        let artifact = columns("project_artifact_policies");
        assert_eq!(
            artifact
                .iter()
                .filter(|column| column.as_str() == "auto_gc")
                .count(),
            1
        );
        assert!(!artifact.iter().any(|column| column == "delete_unread"));
        let mailbox = columns("project_mailbox_policies");
        assert_eq!(
            mailbox
                .iter()
                .filter(|column| column.as_str() == "auto_gc")
                .count(),
            1
        );
        assert!(mailbox.iter().any(|column| column == "delete_unread"));
    }

    #[test]
    fn integrity_check_reports_ok() {
        let (_data, store, _repo) = test_store();
        assert_eq!(store.integrity_check().unwrap(), "ok");
    }

    #[test]
    fn persists_mailbox_and_artifacts() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "mailbox").unwrap();
        let (message, event) = store
            .create_agent_message_with_event(
                &session.id,
                "agent-a",
                "Leader",
                "agent-b",
                "Worker",
                "handoff",
                "Please implement the API",
                &serde_json::json!({"task": "T1"}),
            )
            .unwrap();
        assert_eq!(message.status, "unread");
        assert_eq!(event.kind, "mailbox.message_sent");
        let messages = store
            .list_agent_messages(&session.id, Some("agent-b"), true)
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "Please implement the API");
        let (read, event) = store.mark_agent_message_read(&message.id).unwrap().unwrap();
        assert_eq!(read.status, "read");
        assert_eq!(event.unwrap().kind, "mailbox.message_read");
        assert!(
            store
                .list_agent_messages(&session.id, Some("agent-b"), true)
                .unwrap()
                .is_empty()
        );

        let (artifact, event) = store
            .create_artifact_with_event(
                &session.id,
                "agent-b",
                "Worker",
                Some("T1"),
                "api-notes.md",
                "note",
                "text/markdown",
                "# API notes\n",
                "inline",
                12,
                "test-sha",
                None,
            )
            .unwrap();
        assert_eq!(event.kind, "artifact.published");
        let artifacts = store.list_artifacts(&session.id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].name, "api-notes.md");
        assert_eq!(
            store.get_artifact(&artifact.id).unwrap().unwrap().content,
            "# API notes\n"
        );
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
