use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    AcpAgent, AgentMessage, AgentRecord, Approval, ApprovalGrant, Artifact, ArtifactPolicy,
    ArtifactVersion, CheckpointRetentionPolicy, Event, IntakeTask, IntegrationPolicy,
    MailboxPolicy, McpServer, McpServerConfig, McpTransport, MemoryItem, MemoryPolicy,
    MergeQueueEntry, ModelPreset, OrchestrationDecision, OrchestrationNote, OrchestrationPolicy,
    PlanTaskProgress, Project, ProjectBudget, ProjectUsageSession, Question, ScheduledTask,
    SearchHit, Session, SessionBundle, ShellPolicy, Skill, Source, SourceChunk, SourceCollection,
    SourceSearchHit, SourceStats, SteeringMessage, StoredMessage, TaskSchedulerPolicy, TaskStall,
    TeamPlan, TodoItem, UsageEntry, WorktreeRetentionPolicy, WriteLease,
};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLease {
    pub lease_key: String,
    pub owner_id: String,
    pub kind: String,
    pub session_id: Option<String>,
    pub acquired_at: String,
    pub heartbeat_at: String,
    pub expires_at: String,
}

fn map_runtime_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuntimeLease> {
    Ok(RuntimeLease {
        lease_key: row.get(0)?,
        owner_id: row.get(1)?,
        kind: row.get(2)?,
        session_id: row.get(3)?,
        acquired_at: row.get(4)?,
        heartbeat_at: row.get(5)?,
        expires_at: row.get(6)?,
    })
}

/// A daemon-wide lease over an interactive computer surface (browser or GUI).
/// It is deliberately separate from the worktree write-scope leases: a
/// `ComputerLease` serializes visual/interactive state, not file writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerLease {
    pub lease_id: String,
    pub surface: String,
    pub owner_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub acquired_at: String,
    pub heartbeat_at: String,
    pub expires_at: String,
}

fn map_computer_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<ComputerLease> {
    Ok(ComputerLease {
        lease_id: row.get(0)?,
        surface: row.get(1)?,
        owner_id: row.get(2)?,
        session_id: row.get(3)?,
        agent_id: row.get(4)?,
        agent_name: row.get(5)?,
        acquired_at: row.get(6)?,
        heartbeat_at: row.get(7)?,
        expires_at: row.get(8)?,
    })
}

/// Outcome of a computer-lease acquisition. The event list contains any
/// expired leases removed in the same transaction plus the acquisition or
/// contention audit event, so the engine can broadcast them after commit.
#[derive(Debug, Clone)]
pub enum ComputerLeaseAttempt {
    Acquired {
        lease: ComputerLease,
        events: Vec<Event>,
    },
    Busy {
        holder: Option<ComputerLease>,
        events: Vec<Event>,
    },
}

/// Input for creating a standalone intake task. Kept in the store layer so
/// the engine and tests construct the same shape without a 15-argument call.
#[derive(Debug, Clone)]
pub struct NewIntakeTask {
    pub project_id: String,
    pub title: String,
    pub instructions: String,
    pub profile: String,
    pub kind: String,
    pub priority: i32,
    pub write_scope: Vec<String>,
    pub acceptance: Vec<String>,
    pub deps: Vec<String>,
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
    pub time_budget_secs: Option<u64>,
    pub max_attempts: u32,
    pub origin: String,
    pub labels: Vec<String>,
}

fn map_intake_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<IntakeTask> {
    let write_scope: String = row.get(7)?;
    let acceptance: String = row.get(8)?;
    let labels: String = row.get(21)?;
    let deps: String = row.get(24).unwrap_or_else(|_| "[]".to_string());
    Ok(IntakeTask {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        instructions: row.get(3)?,
        profile: row.get(4)?,
        kind: row.get(5)?,
        priority: row.get(6)?,
        write_scope: serde_json::from_str(&write_scope).unwrap_or_default(),
        acceptance: serde_json::from_str(&acceptance).unwrap_or_default(),
        deps: serde_json::from_str(&deps).unwrap_or_default(),
        token_budget: row
            .get::<_, Option<i64>>(9)?
            .map(|value| value.max(0) as u64),
        cost_budget_usd: row.get(10)?,
        time_budget_secs: row
            .get::<_, Option<i64>>(11)?
            .map(|value| value.max(0) as u64),
        status: row.get(12)?,
        attempts: row.get::<_, i64>(13)?.max(0) as u32,
        max_attempts: row.get::<_, i64>(14)?.max(1) as u32,
        next_attempt_at: row.get(15)?,
        session_id: row.get(16)?,
        task_id: row.get(17)?,
        last_error: row.get(18)?,
        result_summary: row.get(19)?,
        origin: row.get(20)?,
        labels: serde_json::from_str(&labels).unwrap_or_default(),
        created_at: row.get(22)?,
        updated_at: row.get(23)?,
    })
}

/// On-disk source snapshot. The JSON file is the durable copy used by
/// `db-rebuild`; SQLite is the searchable projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub version: u32,
    pub source: Source,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct SourceSearchQuery {
    pub all: String,
    pub any: String,
    pub terms: Vec<String>,
    pub raw: String,
    pub prefer_like: bool,
    /// Empty means all collections. The empty string selects uncollected rows.
    pub collections: Vec<String>,
    /// Empty means all labels; otherwise any requested label may match.
    pub labels: Vec<String>,
}

fn map_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<Source> {
    let labels: String = row.get(14)?;
    Ok(Source {
        id: row.get(0)?,
        project_id: row.get(1)?,
        kind: row.get(2)?,
        title: row.get(3)?,
        uri: row.get(4)?,
        mime_type: row.get(5)?,
        status: row.get(6)?,
        size_bytes: row.get::<_, i64>(7)?.max(0) as u64,
        sha256: row.get(8)?,
        revision: row.get::<_, i64>(9)?.max(1) as u32,
        chunk_count: row.get::<_, i64>(10)?.max(0) as usize,
        char_count: row.get::<_, i64>(11)?.max(0) as usize,
        token_count: row.get::<_, i64>(12)?.max(0) as u64,
        error: row.get(13)?,
        labels: serde_json::from_str(&labels).unwrap_or_default(),
        collection: row.get(18).unwrap_or(None),
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        indexed_at: row.get(17)?,
    })
}

fn map_skill(row: &rusqlite::Row<'_>) -> rusqlite::Result<Skill> {
    let tool_allow: String = row.get(8)?;
    let tool_deny: String = row.get(9)?;
    let profiles: String = row.get(10)?;
    let roles: String = row.get(11)?;
    Ok(Skill {
        id: row.get(0)?,
        project_id: row.get(1)?,
        name: row.get(2)?,
        slug: row.get(3)?,
        description: row.get(4)?,
        when_to_use: row.get(5)?,
        version: row.get(6)?,
        instructions: row.get(7)?,
        tool_allow: serde_json::from_str(&tool_allow).unwrap_or_default(),
        tool_deny: serde_json::from_str(&tool_deny).unwrap_or_default(),
        profiles: serde_json::from_str(&profiles).unwrap_or_default(),
        roles: serde_json::from_str(&roles).unwrap_or_default(),
        enabled: row.get::<_, i64>(12)? != 0,
        source: row.get(13)?,
        sha256: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

fn map_source_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceChunk> {
    Ok(SourceChunk {
        id: row.get(0)?,
        source_id: row.get(1)?,
        chunk_no: row.get::<_, i64>(2)?.max(0) as u32,
        heading: row.get(3)?,
        content: row.get(4)?,
        start_offset: row.get::<_, i64>(5)?.max(0) as usize,
        end_offset: row.get::<_, i64>(6)?.max(0) as usize,
        token_count: row.get::<_, i64>(7)?.max(0) as u32,
    })
}

/// Explicit projection repair derived from the event stream. Fields set to
/// `None` are left untouched; `replace_plan` distinguishes "clear the plan"
/// from "leave it alone".
#[derive(Debug, Clone)]
pub struct SessionRepair {
    pub status: String,
    pub priority: Option<i32>,
    pub unlimited: Option<bool>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_spent_usd: f64,
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
    pub tasks: Option<Vec<ScheduledTask>>,
    pub steering: Option<Vec<SteeringMessage>>,
    pub artifact_warnings: Vec<String>,
}

/// A steering row delivered at a turn boundary, plus the optional persisted
/// user message used by top-level runs.
#[derive(Debug, Clone)]
pub struct DeliveredSteering {
    pub steering: SteeringMessage,
    pub message: Option<StoredMessage>,
    pub content: String,
}

pub type PendingTaskSessionRow = (String, String, i32, usize, String, i32, u64);

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
    #[serde(default)]
    pub checkpoint_policy: CheckpointRetentionPolicy,
    pub budget: ProjectBudget,
    #[serde(default)]
    pub task_scheduler_policy: TaskSchedulerPolicy,
    #[serde(default)]
    pub memory_policy: MemoryPolicy,
    #[serde(default)]
    pub orchestration_policy: OrchestrationPolicy,
    #[serde(default)]
    pub approval_grants: Vec<ApprovalGrant>,
    /// Standalone task-inbox items. They are project-scoped rather than
    /// session-scoped, so the project manifest is their rebuild snapshot.
    #[serde(default)]
    pub intake_tasks: Vec<IntakeTask>,
    #[serde(default)]
    pub skills: Vec<Skill>,
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
        match serde_json::from_str::<ProjectManifestSnapshot>(&line) {
            Ok(snapshot) => {
                latest.insert(snapshot.project.id.clone(), snapshot);
            }
            Err(error) => {
                tracing::warn!(%error, "skipping malformed project manifest line");
            }
        }
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
    mirror_lock: Arc<Mutex<()>>,
    manifest_lock: Arc<Mutex<()>>,
}

/// Reject IDs that could escape the daemon's per-session storage paths or
/// inject rows into another session through an imported bundle. IDs are opaque
/// strings, not just UUIDs, so this is a path-component check rather than a
/// UUID regex.
pub fn validate_storage_id(kind: &str, id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 || id.trim() != id {
        return Err(anyhow!("{kind} id must be 1-128 non-padded characters"));
    }
    if id == "." || id == ".." || id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err(anyhow!("{kind} id is not a safe storage component: {id}"));
    }
    Ok(())
}

fn ensure_bundle_session(kind: &str, actual: &str, expected: &str) -> Result<()> {
    if actual != expected {
        return Err(anyhow!(
            "bundle {kind} belongs to session {actual}, expected {expected}"
        ));
    }
    Ok(())
}

fn validate_session_bundle(bundle: &SessionBundle) -> Result<()> {
    validate_storage_id("project", &bundle.project.id)?;
    validate_storage_id("session", &bundle.session.id)?;
    if bundle.session.project_id != bundle.project.id {
        return Err(anyhow!("bundle session/project ids do not match"));
    }
    for event in &bundle.events {
        ensure_bundle_session("event", &event.session_id, &bundle.session.id)?;
    }
    for agent in &bundle.agents {
        ensure_bundle_session("agent", &agent.session_id, &bundle.session.id)?;
    }
    for message in &bundle.messages {
        ensure_bundle_session("message", &message.session_id, &bundle.session.id)?;
    }
    for approval in &bundle.approvals {
        ensure_bundle_session("approval", &approval.session_id, &bundle.session.id)?;
    }
    for entry in &bundle.merge_queue {
        ensure_bundle_session("merge queue entry", &entry.session_id, &bundle.session.id)?;
    }
    for message in &bundle.agent_messages {
        ensure_bundle_session("agent message", &message.session_id, &bundle.session.id)?;
    }
    for artifact in &bundle.artifacts {
        ensure_bundle_session("artifact", &artifact.session_id, &bundle.session.id)?;
    }
    for entry in &bundle.usage {
        ensure_bundle_session("usage entry", &entry.session_id, &bundle.session.id)?;
    }
    for memory in &bundle.memories {
        ensure_bundle_session("memory", &memory.session_id, &bundle.session.id)?;
        if memory.project_id != bundle.project.id {
            return Err(anyhow!(
                "bundle memory {} belongs to a different project",
                memory.id
            ));
        }
    }
    for question in &bundle.questions {
        ensure_bundle_session("question", &question.session_id, &bundle.session.id)?;
    }
    for task in &bundle.tasks {
        ensure_bundle_session("task", &task.session_id, &bundle.session.id)?;
        if task.project_id != bundle.project.id {
            return Err(anyhow!(
                "bundle task {} belongs to a different project",
                task.task_id
            ));
        }
    }
    for agent in &bundle.agents {
        validate_storage_id("agent", &agent.id)?;
    }
    Ok(())
}

fn allowed_session_transition(current: &str, next: &str) -> bool {
    if current == next {
        return true;
    }
    match current {
        // Landed work is final until the user creates a new session or forks.
        "landed" => false,
        // Imported archives may only be rehydrated into an idle working session.
        "imported" => next == "idle",
        _ => true,
    }
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
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS store_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS projects (
                id            TEXT PRIMARY KEY,
                name          TEXT NOT NULL,
                root          TEXT NOT NULL UNIQUE,
                default_branch TEXT NOT NULL,
                created_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS model_presets (
                id                    TEXT PRIMARY KEY,
                name                  TEXT NOT NULL,
                provider              TEXT NOT NULL,
                model                 TEXT NOT NULL,
                context_window        INTEGER,
                input_usd_per_million REAL,
                output_usd_per_million REAL,
                notes                 TEXT NOT NULL DEFAULT '',
                created_at            TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_shell_policies (
                project_id      TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                network         TEXT NOT NULL,
                read_deny       TEXT NOT NULL,
                restrict_home   INTEGER NOT NULL DEFAULT 0,
                read_jail       INTEGER NOT NULL DEFAULT 0,
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
                priority      INTEGER NOT NULL DEFAULT 0,
                worktree      TEXT,
                branch        TEXT,
                token_budget  INTEGER,
                cost_budget_usd REAL,
                time_budget_secs INTEGER,
                unlimited     INTEGER NOT NULL DEFAULT 0,
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
            CREATE INDEX IF NOT EXISTS idx_events_session_type_seq
                ON events(session_id, type, seq);
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

            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                tool_calls,
                content='messages',
                content_rowid='rowid',
                tokenize='unicode61'
            );

            CREATE TRIGGER IF NOT EXISTS messages_fts_insert
            AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content, tool_calls)
                VALUES (new.rowid, new.content, new.tool_calls);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_fts_delete
            AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, tool_calls)
                VALUES ('delete', old.rowid, old.content, old.tool_calls);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_fts_update
            AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, tool_calls)
                VALUES ('delete', old.rowid, old.content, old.tool_calls);
                INSERT INTO messages_fts(rowid, content, tool_calls)
                VALUES (new.rowid, new.content, new.tool_calls);
            END;

            CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
                type,
                payload,
                content='events',
                content_rowid='rowid',
                tokenize='unicode61'
            );

            CREATE TRIGGER IF NOT EXISTS events_fts_insert
            AFTER INSERT ON events BEGIN
                INSERT INTO events_fts(rowid, type, payload)
                VALUES (new.rowid, new.type, new.payload);
            END;

            CREATE TRIGGER IF NOT EXISTS events_fts_delete
            AFTER DELETE ON events BEGIN
                INSERT INTO events_fts(events_fts, rowid, type, payload)
                VALUES ('delete', old.rowid, old.type, old.payload);
            END;

            CREATE TRIGGER IF NOT EXISTS events_fts_update
            AFTER UPDATE ON events BEGIN
                INSERT INTO events_fts(events_fts, rowid, type, payload)
                VALUES ('delete', old.rowid, old.type, old.payload);
                INSERT INTO events_fts(rowid, type, payload)
                VALUES (new.rowid, new.type, new.payload);
            END;

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
                version    INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS artifact_versions (
                artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
                version     INTEGER NOT NULL,
                name        TEXT NOT NULL,
                kind        TEXT NOT NULL,
                mime_type   TEXT NOT NULL,
                content     TEXT NOT NULL,
                storage     TEXT NOT NULL,
                compression TEXT NOT NULL,
                size_bytes  INTEGER NOT NULL,
                sha256      TEXT NOT NULL,
                path        TEXT,
                created_at  TEXT NOT NULL,
                PRIMARY KEY (artifact_id, version)
            );

            CREATE INDEX IF NOT EXISTS idx_artifacts_session
                ON artifacts(session_id, created_at);

            CREATE TABLE IF NOT EXISTS artifact_requests (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                request_key TEXT NOT NULL,
                artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                PRIMARY KEY (session_id, request_key)
            );

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

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                title,
                body,
                tags,
                content='memories',
                content_rowid='rowid',
                tokenize='unicode61'
            );

            CREATE TRIGGER IF NOT EXISTS memories_fts_insert
            AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, title, body, tags)
                VALUES (new.rowid, new.title, new.body, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_fts_delete
            AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, title, body, tags)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_fts_update
            AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, title, body, tags)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags);
                INSERT INTO memories_fts(rowid, title, body, tags)
                VALUES (new.rowid, new.title, new.body, new.tags);
            END;

            CREATE TABLE IF NOT EXISTS sources (
                id          TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                kind        TEXT NOT NULL,
                collection  TEXT,
                title       TEXT NOT NULL,
                uri         TEXT,
                mime_type   TEXT,
                status      TEXT NOT NULL,
                size_bytes  INTEGER NOT NULL DEFAULT 0,
                sha256      TEXT NOT NULL,
                revision    INTEGER NOT NULL DEFAULT 1,
                chunk_count INTEGER NOT NULL DEFAULT 0,
                char_count  INTEGER NOT NULL DEFAULT 0,
                token_count INTEGER NOT NULL DEFAULT 0,
                error       TEXT,
                labels      TEXT NOT NULL DEFAULT '[]',
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                indexed_at  TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_sources_project_status
                ON sources(project_id, status, updated_at);

            CREATE TABLE IF NOT EXISTS source_chunks (
                id            TEXT PRIMARY KEY,
                project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                source_id     TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                chunk_no      INTEGER NOT NULL,
                heading       TEXT,
                content       TEXT NOT NULL,
                start_offset  INTEGER NOT NULL DEFAULT 0,
                end_offset    INTEGER NOT NULL DEFAULT 0,
                token_count   INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL,
                UNIQUE(source_id, chunk_no)
            );

            CREATE INDEX IF NOT EXISTS idx_source_chunks_project
                ON source_chunks(project_id, source_id, chunk_no);

            CREATE VIRTUAL TABLE IF NOT EXISTS source_chunks_fts USING fts5(
                content,
                heading,
                content='source_chunks',
                content_rowid='rowid',
                tokenize='unicode61 remove_diacritics 2'
            );

            CREATE TRIGGER IF NOT EXISTS source_chunks_fts_insert
            AFTER INSERT ON source_chunks BEGIN
                INSERT INTO source_chunks_fts(rowid, content, heading)
                VALUES (new.rowid, new.content, new.heading);
            END;

            CREATE TRIGGER IF NOT EXISTS source_chunks_fts_delete
            AFTER DELETE ON source_chunks BEGIN
                INSERT INTO source_chunks_fts(source_chunks_fts, rowid, content, heading)
                VALUES ('delete', old.rowid, old.content, old.heading);
            END;

            CREATE TRIGGER IF NOT EXISTS source_chunks_fts_update
            AFTER UPDATE ON source_chunks BEGIN
                INSERT INTO source_chunks_fts(source_chunks_fts, rowid, content, heading)
                VALUES ('delete', old.rowid, old.content, old.heading);
                INSERT INTO source_chunks_fts(rowid, content, heading)
                VALUES (new.rowid, new.content, new.heading);
            END;

            CREATE TABLE IF NOT EXISTS skills (
                id          TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                name        TEXT NOT NULL,
                slug        TEXT NOT NULL,
                description TEXT NOT NULL,
                when_to_use TEXT NOT NULL DEFAULT '',
                version     TEXT NOT NULL DEFAULT '1.0.0',
                instructions TEXT NOT NULL,
                tool_allow  TEXT NOT NULL DEFAULT '[]',
                tool_deny   TEXT NOT NULL DEFAULT '[]',
                profiles    TEXT NOT NULL DEFAULT '[]',
                roles       TEXT NOT NULL DEFAULT '[]',
                enabled     INTEGER NOT NULL DEFAULT 1,
                source      TEXT NOT NULL DEFAULT 'user',
                sha256      TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                UNIQUE(project_id, slug)
            );

            CREATE INDEX IF NOT EXISTS idx_skills_project_enabled
                ON skills(project_id, enabled, updated_at);

            CREATE TABLE IF NOT EXISTS approval_grants (
                id               TEXT PRIMARY KEY,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                tool              TEXT NOT NULL,
                path_prefix       TEXT NOT NULL,
                include_children  INTEGER NOT NULL DEFAULT 0,
                created_by        TEXT NOT NULL,
                created_at        TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_approval_grants_project
                ON approval_grants(project_id, created_at);

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

            CREATE TABLE IF NOT EXISTS coordination_notes (
                note_id          TEXT PRIMARY KEY,
                session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq              INTEGER NOT NULL,
                action           TEXT NOT NULL,
                severity         TEXT NOT NULL,
                status           TEXT NOT NULL,
                task_id          TEXT,
                target_agent_id  TEXT,
                agent_id         TEXT NOT NULL,
                agent_name       TEXT NOT NULL,
                message          TEXT NOT NULL,
                created_at       TEXT NOT NULL,
                resolved_at      TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_coordination_notes_session
                ON coordination_notes(session_id, seq);

            CREATE TABLE IF NOT EXISTS orchestration_decisions (
                decision_id      TEXT PRIMARY KEY,
                session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq              INTEGER NOT NULL,
                action           TEXT NOT NULL,
                task_id          TEXT,
                detail           TEXT NOT NULL,
                attempt          INTEGER NOT NULL DEFAULT 0,
                agent_id         TEXT NOT NULL,
                created_at       TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_orchestration_decisions_session
                ON orchestration_decisions(session_id, seq);

            CREATE TABLE IF NOT EXISTS mcp_servers (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL UNIQUE,
                command     TEXT NOT NULL,
                args        TEXT NOT NULL,
                env         TEXT NOT NULL,
                enabled     INTEGER NOT NULL DEFAULT 1,
                created_at  TEXT NOT NULL,
                transport   TEXT NOT NULL DEFAULT 'stdio',
                url         TEXT,
                headers     TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS acp_agents (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL UNIQUE,
                command      TEXT NOT NULL,
                args         TEXT NOT NULL,
                env          TEXT NOT NULL,
                enabled      INTEGER NOT NULL DEFAULT 1,
                auto_approve INTEGER NOT NULL DEFAULT 0,
                timeout_secs INTEGER NOT NULL DEFAULT 1800,
                created_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_memory_policies (
                project_id   TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled      INTEGER NOT NULL,
                max_items    INTEGER,
                max_age_days INTEGER,
                updated_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_worktree_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                older_than_secs  INTEGER NOT NULL DEFAULT 604800,
                delete_branches  INTEGER NOT NULL DEFAULT 0,
                keep_failed      INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS project_checkpoint_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                older_than_secs  INTEGER NOT NULL DEFAULT 2592000,
                keep_latest      INTEGER NOT NULL DEFAULT 5
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

            CREATE TABLE IF NOT EXISTS project_task_scheduler_policies (
                project_id          TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled             INTEGER NOT NULL DEFAULT 1,
                cooldown_secs       INTEGER NOT NULL DEFAULT 60,
                dispatch_tasks      INTEGER NOT NULL DEFAULT 0,
                include_intake      INTEGER NOT NULL DEFAULT 1,
                max_intake_parallel INTEGER NOT NULL DEFAULT 2,
                updated_at          TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS intake_tasks (
                id              TEXT PRIMARY KEY,
                project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                title           TEXT NOT NULL,
                instructions    TEXT NOT NULL,
                profile         TEXT NOT NULL,
                kind            TEXT NOT NULL,
                priority        INTEGER NOT NULL DEFAULT 0,
                write_scope     TEXT NOT NULL DEFAULT '[]',
                acceptance      TEXT NOT NULL DEFAULT '[]',
                deps            TEXT NOT NULL DEFAULT '[]',
                token_budget    INTEGER,
                cost_budget_usd REAL,
                time_budget_secs INTEGER,
                status          TEXT NOT NULL,
                attempts        INTEGER NOT NULL DEFAULT 0,
                max_attempts    INTEGER NOT NULL DEFAULT 2,
                next_attempt_at TEXT,
                session_id      TEXT,
                task_id         TEXT,
                last_error      TEXT,
                result_summary  TEXT,
                origin          TEXT NOT NULL DEFAULT 'user',
                labels          TEXT NOT NULL DEFAULT '[]',
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_intake_tasks_project_status
                ON intake_tasks(project_id, status, created_at);
            CREATE INDEX IF NOT EXISTS idx_intake_tasks_runnable
                ON intake_tasks(status, next_attempt_at);

            CREATE TABLE IF NOT EXISTS project_orchestration_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
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
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                tool_call_id    TEXT NOT NULL,
                tool            TEXT NOT NULL,
                input           TEXT NOT NULL,
                status          TEXT NOT NULL,
                created_at      TEXT NOT NULL,
                resolved_at     TEXT,
                approval_risk   TEXT,
                approval_reason TEXT
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

            CREATE TABLE IF NOT EXISTS steering_messages (
                id           TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id     TEXT NOT NULL,
                task_id      TEXT,
                body         TEXT NOT NULL,
                status       TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                delivered_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_steering_messages_session
                ON steering_messages(session_id, agent_id, status, created_at);

            CREATE TABLE IF NOT EXISTS merge_queue (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id    TEXT NOT NULL,
                agent_name  TEXT NOT NULL,
                task_id     TEXT,
                branch       TEXT NOT NULL,
                commit_hash  TEXT NOT NULL,
                base_commit  TEXT NOT NULL,
                pre_merge_commit TEXT,
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

            CREATE TABLE IF NOT EXISTS plan_tasks (
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                plan_seq     INTEGER NOT NULL,
                task_id      TEXT NOT NULL,
                project_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                department   TEXT NOT NULL,
                title        TEXT NOT NULL,
                priority     INTEGER NOT NULL DEFAULT 0,
                status       TEXT NOT NULL,
                deps         TEXT NOT NULL DEFAULT '[]',
                write_scope  TEXT NOT NULL DEFAULT '[]',
                attempts     INTEGER NOT NULL DEFAULT 0,
                last_error   TEXT,
                next_attempt_at TEXT,
                enqueued_at  TEXT NOT NULL,
                started_at   TEXT,
                finished_at  TEXT,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (session_id, plan_seq, task_id)
            );

            CREATE INDEX IF NOT EXISTS idx_plan_tasks_queue
                ON plan_tasks(status, priority, enqueued_at);
            CREATE INDEX IF NOT EXISTS idx_plan_tasks_session
                ON plan_tasks(session_id, plan_seq);

            CREATE TABLE IF NOT EXISTS plan_task_progress (
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                plan_seq          INTEGER NOT NULL,
                task_id           TEXT NOT NULL,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                last_progress_at  TEXT NOT NULL,
                stall_count       INTEGER NOT NULL DEFAULT 0,
                last_turn         INTEGER,
                last_tool         TEXT,
                updated_at        TEXT NOT NULL,
                PRIMARY KEY (session_id, plan_seq, task_id)
            );

            CREATE INDEX IF NOT EXISTS idx_plan_task_progress_session
                ON plan_task_progress(session_id, task_id);

            CREATE TABLE IF NOT EXISTS write_leases (
                agent_id    TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                scopes      TEXT NOT NULL DEFAULT '[]',
                acquired_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_write_leases_project
                ON write_leases(project_id, acquired_at);

            CREATE TABLE IF NOT EXISTS runtime_leases (
                lease_key     TEXT PRIMARY KEY,
                owner_id      TEXT NOT NULL,
                kind          TEXT NOT NULL,
                session_id    TEXT,
                acquired_at   TEXT NOT NULL,
                heartbeat_at  TEXT NOT NULL,
                expires_at    TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_runtime_leases_expiry
                ON runtime_leases(expires_at);
            CREATE INDEX IF NOT EXISTS idx_runtime_leases_session
                ON runtime_leases(session_id);

            CREATE TABLE IF NOT EXISTS computer_leases (
                lease_id      TEXT PRIMARY KEY,
                surface       TEXT NOT NULL UNIQUE,
                owner_id      TEXT NOT NULL,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id      TEXT NOT NULL,
                agent_name    TEXT NOT NULL,
                acquired_at   TEXT NOT NULL,
                heartbeat_at  TEXT NOT NULL,
                expires_at    TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_computer_leases_expiry
                ON computer_leases(expires_at);
            CREATE INDEX IF NOT EXISTS idx_computer_leases_session
                ON computer_leases(session_id);
            "#,
        )?;
        let fts_migrated = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key = 'memory_fts'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if fts_migrated.is_none() {
            conn.execute(
                "INSERT INTO memories_fts(memories_fts) VALUES('rebuild')",
                [],
            )?;
            conn.execute(
                "INSERT INTO store_meta (key, value) VALUES ('memory_fts', '1')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        let search_fts_migrated = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key = 'search_fts'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if search_fts_migrated.is_none() {
            conn.execute(
                "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
                [],
            )?;
            conn.execute("INSERT INTO events_fts(events_fts) VALUES('rebuild')", [])?;
            conn.execute(
                "INSERT INTO store_meta (key, value) VALUES ('search_fts', '1')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(status);
             CREATE INDEX IF NOT EXISTS idx_questions_status ON questions(status);
             CREATE INDEX IF NOT EXISTS idx_merge_queue_status ON merge_queue(status);
             CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);",
        )?;
        ensure_column(
            &conn,
            "project_task_scheduler_policies",
            "dispatch_tasks",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_task_scheduler_policies",
            "include_intake",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(&conn, "intake_tasks", "deps", "TEXT NOT NULL DEFAULT '[]'")?;
        ensure_column(&conn, "sources", "collection", "TEXT")?;
        ensure_column(
            &conn,
            "project_task_scheduler_policies",
            "max_intake_parallel",
            "INTEGER NOT NULL DEFAULT 2",
        )?;
        ensure_column(&conn, "sessions", "priority", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "sessions", "worktree", "TEXT")?;
        ensure_column(&conn, "sessions", "branch", "TEXT")?;
        ensure_column(&conn, "sessions", "token_budget", "INTEGER")?;
        ensure_column(&conn, "sessions", "cost_budget_usd", "REAL")?;
        ensure_column(&conn, "sessions", "time_budget_secs", "INTEGER")?;
        ensure_column(&conn, "sessions", "unlimited", "INTEGER NOT NULL DEFAULT 0")?;
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
        ensure_column(&conn, "artifacts", "version", "INTEGER NOT NULL DEFAULT 1")?;
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
            "project_shell_policies",
            "read_jail",
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
        ensure_column(&conn, "agents", "name", "TEXT NOT NULL DEFAULT 'Agent'")?;
        ensure_column(&conn, "agents", "title", "TEXT")?;
        ensure_column(&conn, "agents", "profile", "TEXT")?;
        ensure_column(&conn, "agents", "parent_id", "TEXT")?;
        ensure_column(&conn, "agents", "worktree", "TEXT")?;
        ensure_column(&conn, "agents", "branch", "TEXT")?;
        ensure_column(&conn, "agents", "task_id", "TEXT")?;
        ensure_column(&conn, "agents", "department", "TEXT")?;
        ensure_column(&conn, "merge_queue", "pre_merge_commit", "TEXT")?;
        ensure_column(&conn, "plan_tasks", "next_attempt_at", "TEXT")?;
        ensure_column(&conn, "approvals", "approval_risk", "TEXT")?;
        ensure_column(&conn, "approvals", "approval_reason", "TEXT")?;
        ensure_column(&conn, "sessions", "tokens_in", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(
            &conn,
            "sessions",
            "tokens_out",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "mcp_servers",
            "transport",
            "TEXT NOT NULL DEFAULT 'stdio'",
        )?;
        ensure_column(&conn, "mcp_servers", "url", "TEXT")?;
        ensure_column(
            &conn,
            "mcp_servers",
            "headers",
            "TEXT NOT NULL DEFAULT '{}'",
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            data_dir,
            mirror_lock: Arc::new(Mutex::new(())),
            manifest_lock: Arc::new(Mutex::new(())),
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
        let _guard = self
            .manifest_lock
            .lock()
            .map_err(|_| anyhow!("project manifest mutex poisoned"))?;
        let mut line_buffer = Vec::new();
        for project in projects {
            let snapshot = ProjectManifestSnapshot {
                version: 1,
                recorded_at: now_rfc3339(),
                shell_policy: self.get_shell_policy(&project.id)?,
                integration_policy: self.get_integration_policy(&project.id)?,
                artifact_policy: self.get_artifact_policy(&project.id)?,
                mailbox_policy: self.get_mailbox_policy(&project.id)?,
                worktree_policy: self.get_worktree_policy(&project.id)?,
                checkpoint_policy: self.get_checkpoint_policy(&project.id)?,
                budget: self.get_project_budget(&project.id)?,
                task_scheduler_policy: self.get_task_scheduler_policy(&project.id)?,
                memory_policy: self.get_memory_policy(&project.id)?,
                orchestration_policy: self.get_orchestration_policy(&project.id)?,
                approval_grants: self.list_approval_grants(&project.id)?,
                intake_tasks: self.list_intake_tasks(Some(&project.id), None, None)?,
                skills: self.list_skills(Some(&project.id), true)?,
                project,
            };
            serde_json::to_writer(&mut line_buffer, &snapshot)?;
            line_buffer.push(b'\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening project manifest {}", path.display()))?;
        file.write_all(&line_buffer)?;
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
             (id, project_id, goal, status, priority, worktree, branch, token_budget,
              cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit,
              team_plan, tokens_in, tokens_out, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?14, ?15, ?16)",
            params![
                session.id,
                session.project_id,
                session.goal,
                session.status,
                session.priority,
                session.worktree,
                session.branch,
                session.token_budget,
                session.cost_budget_usd,
                session.time_budget_secs,
                if session.unlimited { 1 } else { 0 },
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
                "SELECT network, read_deny, restrict_home, read_jail
                 FROM project_shell_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, i64>(3)? != 0,
                    ))
                },
            )
            .optional()?;
        match stored {
            Some((network, read_deny, restrict_home, read_jail)) => Ok(ShellPolicy {
                network,
                read_deny: serde_json::from_str(&read_deny)
                    .context("parsing stored shell read-deny policy")?,
                restrict_home,
                read_jail,
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
             (project_id, network, read_deny, restrict_home, read_jail, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                network = excluded.network,
                read_deny = excluded.read_deny,
                restrict_home = excluded.restrict_home,
                read_jail = excluded.read_jail,
                updated_at = excluded.updated_at",
            params![
                project_id,
                policy.network,
                read_deny,
                if policy.restrict_home { 1 } else { 0 },
                if policy.read_jail { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn get_task_scheduler_policy(&self, project_id: &str) -> Result<TaskSchedulerPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, cooldown_secs, dispatch_tasks, include_intake, max_intake_parallel
                 FROM project_task_scheduler_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(TaskSchedulerPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        cooldown_secs: row.get::<_, i64>(1)?.max(1) as u64,
                        dispatch_tasks: row.get::<_, i64>(2).unwrap_or(0) != 0,
                        include_intake: row.get::<_, i64>(3).unwrap_or(1) != 0,
                        max_intake_parallel: row.get::<_, i64>(4).unwrap_or(2).clamp(1, 10) as u32,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn has_task_scheduler_policy(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM project_task_scheduler_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn set_task_scheduler_policy(
        &self,
        project_id: &str,
        policy: &TaskSchedulerPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_task_scheduler_policies
             (project_id, enabled, cooldown_secs, dispatch_tasks, include_intake,
              max_intake_parallel, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                cooldown_secs = excluded.cooldown_secs,
                dispatch_tasks = excluded.dispatch_tasks,
                include_intake = excluded.include_intake,
                max_intake_parallel = excluded.max_intake_parallel,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.cooldown_secs as i64,
                if policy.dispatch_tasks { 1 } else { 0 },
                if policy.include_intake { 1 } else { 0 },
                policy.max_intake_parallel.clamp(1, 10) as i64,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Project-scoped standalone task intake. Intake items are not attached
    /// to a TeamPlan until the dispatcher synthesizes a one-task plan for them.
    pub fn list_intake_tasks(
        &self,
        project_id: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<IntakeTask>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 2_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, instructions, profile, kind, priority,
                    write_scope, acceptance, token_budget, cost_budget_usd,
                    time_budget_secs, status, attempts, max_attempts, next_attempt_at,
                    session_id, task_id, last_error, result_summary, origin, labels,
                    created_at, updated_at, deps
             FROM intake_tasks
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR status = ?2)
             ORDER BY created_at DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![project_id, status, limit], map_intake_task)?;
        let mut tasks = rows.collect::<Result<Vec<_>, _>>()?;
        tasks.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(tasks)
    }

    pub fn get_intake_task(&self, id: &str) -> Result<Option<IntakeTask>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, title, instructions, profile, kind, priority,
                    write_scope, acceptance, token_budget, cost_budget_usd,
                    time_budget_secs, status, attempts, max_attempts, next_attempt_at,
                    session_id, task_id, last_error, result_summary, origin, labels,
                    created_at, updated_at, deps
             FROM intake_tasks WHERE id = ?1",
            params![id],
            map_intake_task,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Insert a new queued intake item. The engine validates the title,
    /// profile, scopes, and budgets before calling this store method.
    pub fn create_intake_task(&self, task: &NewIntakeTask) -> Result<IntakeTask> {
        let id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let write_scope = serde_json::to_string(&task.write_scope)?;
        let acceptance = serde_json::to_string(&task.acceptance)?;
        let labels = serde_json::to_string(&task.labels)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO intake_tasks
             (id, project_id, title, instructions, profile, kind, priority, write_scope,
              acceptance, deps, token_budget, cost_budget_usd, time_budget_secs, status, attempts,
              max_attempts, next_attempt_at, session_id, task_id, last_error, result_summary,
              origin, labels, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     'queued', 0, ?14, NULL, NULL, NULL, NULL, NULL, ?15, ?16, ?17, ?17)",
            params![
                id,
                task.project_id,
                task.title,
                task.instructions,
                task.profile,
                task.kind,
                task.priority,
                write_scope,
                acceptance,
                serde_json::to_string(&task.deps)?,
                task.token_budget.map(|value| value as i64),
                task.cost_budget_usd,
                task.time_budget_secs.map(|value| value as i64),
                task.max_attempts as i64,
                task.origin,
                labels,
                now,
            ],
        )?;
        drop(conn);
        self.get_intake_task(&id)?
            .ok_or_else(|| anyhow!("created intake task disappeared"))
    }

    /// Replace the editable fields of a non-active intake item. Editing a
    /// failed/cancelled item re-queues it with a fresh manual attempt budget,
    /// matching the "edit then run" workflow in the task inbox.
    pub fn update_intake_task(&self, id: &str, task: &NewIntakeTask) -> Result<Option<IntakeTask>> {
        let conn = self.lock()?;
        let now = now_rfc3339();
        let changed = conn.execute(
            "UPDATE intake_tasks
             SET title = ?2, instructions = ?3, profile = ?4, kind = ?5, priority = ?6,
                 write_scope = ?7, acceptance = ?8, deps = ?9, token_budget = ?10,
                 cost_budget_usd = ?11, time_budget_secs = ?12, max_attempts = ?13,
                 labels = ?14, status = 'queued', attempts = 0, next_attempt_at = NULL,
                 last_error = NULL, result_summary = NULL, updated_at = ?15
             WHERE id = ?1 AND status NOT IN ('dispatching', 'running', 'completed')",
            params![
                id,
                task.title,
                task.instructions,
                task.profile,
                task.kind,
                task.priority,
                serde_json::to_string(&task.write_scope)?,
                serde_json::to_string(&task.acceptance)?,
                serde_json::to_string(&task.deps)?,
                task.token_budget.map(|value| value as i64),
                task.cost_budget_usd,
                task.time_budget_secs.map(|value| value as i64),
                task.max_attempts as i64,
                serde_json::to_string(&task.labels)?,
                now,
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_intake_task(id)
    }

    /// Atomically take a runnable intake item. This is the only path that
    /// increments `attempts`, so two scheduler ticks or a manual dispatch
    /// cannot claim the same item.
    pub fn claim_intake_task(&self, id: &str) -> Result<Option<IntakeTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current = tx
            .query_row(
                "SELECT status, attempts, max_attempts FROM intake_tasks WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? as u32,
                        row.get::<_, i64>(2)? as u32,
                    ))
                },
            )
            .optional()?;
        let Some((status, attempts, max_attempts)) = current else {
            return Ok(None);
        };
        if !matches!(status.as_str(), "queued" | "failed" | "interrupted")
            || attempts >= max_attempts
        {
            return Ok(None);
        }
        let changed = tx.execute(
            "UPDATE intake_tasks
             SET status = 'dispatching', attempts = attempts + 1,
                 next_attempt_at = NULL, last_error = NULL, updated_at = ?2
             WHERE id = ?1 AND status = ?3 AND attempts = ?4",
            params![id, now, status, attempts as i64],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        tx.commit()?;
        drop(conn);
        self.get_intake_task(id)
    }

    pub fn start_intake_task(
        &self,
        id: &str,
        session_id: &str,
        task_id: &str,
    ) -> Result<IntakeTask> {
        let conn = self.lock()?;
        let now = now_rfc3339();
        let changed = conn.execute(
            "UPDATE intake_tasks
             SET status = 'running', session_id = ?2, task_id = ?3,
                 result_summary = NULL, updated_at = ?4
             WHERE id = ?1 AND status = 'dispatching'",
            params![id, session_id, task_id, now],
        )?;
        if changed == 0 {
            anyhow::bail!("intake task {id} is not dispatching");
        }
        drop(conn);
        self.get_intake_task(id)?
            .ok_or_else(|| anyhow!("intake task {id} disappeared"))
    }

    /// Settle a dispatch or run. `next_attempt_at` is supplied by the caller
    /// when the project orchestration policy wants an automatic retry.
    pub fn settle_intake_task(
        &self,
        id: &str,
        status: &str,
        result_summary: Option<&str>,
        last_error: Option<&str>,
        next_attempt_at: Option<&str>,
    ) -> Result<Option<IntakeTask>> {
        if !matches!(
            status,
            "queued" | "failed" | "cancelled" | "completed" | "interrupted" | "blocked"
        ) {
            anyhow::bail!("invalid intake status: {status}");
        }
        let conn = self.lock()?;
        conn.execute(
            "UPDATE intake_tasks
             SET status = ?2, result_summary = COALESCE(?3, result_summary),
                 last_error = ?4, next_attempt_at = ?5, updated_at = ?6
             WHERE id = ?1",
            params![
                id,
                status,
                result_summary,
                last_error,
                next_attempt_at,
                now_rfc3339()
            ],
        )?;
        drop(conn);
        self.get_intake_task(id)
    }

    /// Manual retry resets the attempt budget; automatic retries never do.
    pub fn retry_intake_task(&self, id: &str) -> Result<Option<IntakeTask>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE intake_tasks
             SET status = 'queued', attempts = 0, next_attempt_at = NULL,
                 last_error = NULL, result_summary = NULL, updated_at = ?2
             WHERE id = ?1 AND status IN ('failed', 'interrupted', 'cancelled', 'blocked')",
            params![id, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_intake_task(id)
    }

    pub fn cancel_intake_task(&self, id: &str) -> Result<Option<IntakeTask>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE intake_tasks
             SET status = 'cancelled', next_attempt_at = NULL, updated_at = ?2
             WHERE id = ?1 AND status NOT IN ('running', 'dispatching', 'completed', 'cancelled')",
            params![id, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_intake_task(id)
    }

    pub fn delete_intake_task(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "DELETE FROM intake_tasks
             WHERE id = ?1 AND status NOT IN ('running', 'dispatching')",
            params![id],
        )?;
        Ok(changed > 0)
    }

    pub fn count_active_intake_tasks(&self, project_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM intake_tasks
             WHERE project_id = ?1 AND status IN ('dispatching', 'running')",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u32)
    }

    /// Runnable intake rows, oldest first. The scheduler applies priority,
    /// aging, project policy, and round-robin ordering in memory.
    pub fn runnable_intake_tasks(&self, now: &str) -> Result<Vec<IntakeTask>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, instructions, profile, kind, priority,
                    write_scope, acceptance, token_budget, cost_budget_usd,
                    time_budget_secs, status, attempts, max_attempts, next_attempt_at,
                    session_id, task_id, last_error, result_summary, origin, labels,
                    created_at, updated_at, deps
             FROM intake_tasks
             WHERE status IN ('queued', 'failed', 'interrupted')
               AND attempts < max_attempts
               AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
             ORDER BY created_at ASC, id ASC
             LIMIT 1000",
        )?;
        let rows = stmt.query_map(params![now], map_intake_task)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn insert_intake_task_snapshot(&self, task: &IntakeTask) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO intake_tasks
             (id, project_id, title, instructions, profile, kind, priority, write_scope,
              acceptance, deps, token_budget, cost_budget_usd, time_budget_secs, status,
              attempts, max_attempts, next_attempt_at, session_id, task_id, last_error,
              result_summary, origin, labels, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                instructions = excluded.instructions,
                profile = excluded.profile,
                kind = excluded.kind,
                priority = excluded.priority,
                write_scope = excluded.write_scope,
                acceptance = excluded.acceptance,
                deps = excluded.deps,
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                time_budget_secs = excluded.time_budget_secs,
                status = excluded.status,
                attempts = excluded.attempts,
                max_attempts = excluded.max_attempts,
                next_attempt_at = excluded.next_attempt_at,
                session_id = excluded.session_id,
                task_id = excluded.task_id,
                last_error = excluded.last_error,
                result_summary = excluded.result_summary,
                origin = excluded.origin,
                labels = excluded.labels,
                updated_at = excluded.updated_at",
            params![
                task.id,
                task.project_id,
                task.title,
                task.instructions,
                task.profile,
                task.kind,
                task.priority,
                serde_json::to_string(&task.write_scope)?,
                serde_json::to_string(&task.acceptance)?,
                serde_json::to_string(&task.deps)?,
                task.token_budget.map(|value| value as i64),
                task.cost_budget_usd,
                task.time_budget_secs.map(|value| value as i64),
                task.status,
                task.attempts as i64,
                task.max_attempts as i64,
                task.next_attempt_at,
                task.session_id,
                task.task_id,
                task.last_error,
                task.result_summary,
                task.origin,
                serde_json::to_string(&task.labels)?,
                task.created_at,
                task.updated_at,
            ],
        )?;
        Ok(())
    }

    const SOURCE_COLUMNS: &'static str = "id, project_id, kind, title, uri, mime_type, status, \
        size_bytes, sha256, revision, chunk_count, char_count, token_count, error, labels, \
        created_at, updated_at, indexed_at, collection";

    /// Upsert one source and replace its chunks in a single transaction. The
    /// FTS triggers keep the external-content index in sync with the chunks.
    pub fn insert_source(&self, source: &Source, chunks: &[SourceChunk]) -> Result<Source> {
        let labels = serde_json::to_string(&source.labels)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sources
             (id, project_id, kind, collection, title, uri, mime_type, status, size_bytes,
              sha256, revision, chunk_count, char_count, token_count, error, labels,
              created_at, updated_at, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                collection = excluded.collection,
                title = excluded.title,
                uri = excluded.uri,
                mime_type = excluded.mime_type,
                status = excluded.status,
                size_bytes = excluded.size_bytes,
                sha256 = excluded.sha256,
                revision = excluded.revision,
                chunk_count = excluded.chunk_count,
                char_count = excluded.char_count,
                token_count = excluded.token_count,
                error = excluded.error,
                labels = excluded.labels,
                updated_at = excluded.updated_at,
                indexed_at = excluded.indexed_at",
            params![
                source.id,
                source.project_id,
                source.kind,
                source.collection,
                source.title,
                source.uri,
                source.mime_type,
                source.status,
                source.size_bytes as i64,
                source.sha256,
                source.revision as i64,
                source.chunk_count as i64,
                source.char_count as i64,
                source.token_count as i64,
                source.error,
                labels,
                source.created_at,
                source.updated_at,
                source.indexed_at,
            ],
        )?;
        tx.execute(
            "DELETE FROM source_chunks WHERE source_id = ?1",
            params![source.id],
        )?;
        for chunk in chunks {
            tx.execute(
                "INSERT INTO source_chunks
                 (id, project_id, source_id, chunk_no, heading, content, start_offset,
                  end_offset, token_count, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    chunk.id,
                    source.project_id,
                    chunk.source_id,
                    chunk.chunk_no as i64,
                    chunk.heading,
                    chunk.content,
                    chunk.start_offset as i64,
                    chunk.end_offset as i64,
                    chunk.token_count as i64,
                    source.updated_at,
                ],
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.get_source(&source.id)?
            .ok_or_else(|| anyhow!("source disappeared after insert"))
    }

    pub fn get_source(&self, source_id: &str) -> Result<Option<Source>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {} FROM sources WHERE id = ?1", Self::SOURCE_COLUMNS),
            params![source_id],
            map_source,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_sources(
        &self,
        project_id: &str,
        query: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<Source>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(200).clamp(1, 1_000) as i64;
        let query = query.map(|value| format!("%{}%", escape_like(value)));
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sources
             WHERE project_id = ?1
               AND (?2 IS NULL OR status = ?2)
               AND (?3 IS NULL OR title LIKE ?3 ESCAPE '\\' OR uri LIKE ?3 ESCAPE '\\')
             ORDER BY updated_at DESC, id DESC LIMIT ?4",
            Self::SOURCE_COLUMNS
        ))?;
        let rows = stmt.query_map(params![project_id, status, query, limit], map_source)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn source_chunks(
        &self,
        source_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<SourceChunk>> {
        let conn = self.lock()?;
        let offset = offset as i64;
        let limit = limit.clamp(1, 200) as i64;
        let mut stmt = conn.prepare(
            "SELECT id, source_id, chunk_no, heading, content, start_offset, end_offset,
                    token_count
             FROM source_chunks WHERE source_id = ?1
             ORDER BY chunk_no ASC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![source_id, limit, offset], map_source_chunk)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn source_chunk_count(&self, source_id: &str) -> Result<usize> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM source_chunks WHERE source_id = ?1",
            params![source_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn delete_source(&self, source_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute("DELETE FROM sources WHERE id = ?1", params![source_id])?;
        Ok(changed > 0)
    }

    pub fn source_stats(&self, project_id: &str) -> Result<SourceStats> {
        let conn = self.lock()?;
        let mut stats = conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN status = 'indexed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(chunk_count), 0),
                    COALESCE(SUM(size_bytes), 0),
                    COALESCE(SUM(token_count), 0)
             FROM sources WHERE project_id = ?1",
            params![project_id],
            |row| {
                Ok(SourceStats {
                    sources: row.get::<_, i64>(0)?.max(0) as usize,
                    indexed: row.get::<_, i64>(1)?.max(0) as usize,
                    failed: row.get::<_, i64>(2)?.max(0) as usize,
                    chunks: row.get::<_, i64>(3)?.max(0) as usize,
                    bytes: row.get::<_, i64>(4)?.max(0) as u64,
                    tokens: row.get::<_, i64>(5)?.max(0) as u64,
                })
            },
        )?;
        let chunk_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM source_chunks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        stats.chunks = chunk_count.max(0) as usize;
        Ok(stats)
    }

    /// List project collections with aggregate counts. Sources without a
    /// collection form a final `name: null` group. Labels are the distinct
    /// labels used by sources in the group.
    pub fn list_source_collections(&self, project_id: &str) -> Result<Vec<SourceCollection>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT NULLIF(COALESCE(NULLIF(collection, ''), ''), '') AS name,
                    COUNT(*),
                    COALESCE(SUM(chunk_count), 0),
                    COALESCE(SUM(size_bytes), 0),
                    COALESCE(SUM(token_count), 0),
                    COALESCE(GROUP_CONCAT(labels, char(31)), '[]')
             FROM sources
             WHERE project_id = ?1
             GROUP BY NULLIF(COALESCE(collection, ''), '')
             ORDER BY name IS NULL, name COLLATE NOCASE ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            let labels_blob: String = row.get(5)?;
            let mut labels = Vec::new();
            for value in labels_blob.split('\u{1f}') {
                if let Ok(values) = serde_json::from_str::<Vec<String>>(value) {
                    for label in values {
                        if !labels.iter().any(|existing| existing == &label) {
                            labels.push(label);
                        }
                    }
                }
            }
            labels.sort();
            Ok(SourceCollection {
                name: row.get(0)?,
                sources: row.get::<_, i64>(1)?.max(0) as usize,
                chunks: row.get::<_, i64>(2)?.max(0) as usize,
                bytes: row.get::<_, i64>(3)?.max(0) as u64,
                tokens: row.get::<_, i64>(4)?.max(0) as u64,
                labels,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_source_collection(
        &self,
        source_id: &str,
        collection: Option<&str>,
    ) -> Result<Option<Source>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE sources SET collection = ?2, updated_at = ?3 WHERE id = ?1",
            params![source_id, collection, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_source(source_id)
    }

    pub fn set_source_labels(&self, source_id: &str, labels: &[String]) -> Result<Option<Source>> {
        let labels = serde_json::to_string(labels)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE sources SET labels = ?2, updated_at = ?3 WHERE id = ?1",
            params![source_id, labels, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_source(source_id)
    }

    /// Update source metadata without touching chunks or the FTS index. The
    /// caller is responsible for keeping the durable snapshot in sync.
    pub fn update_source_metadata(&self, source: &Source) -> Result<Source> {
        let labels = serde_json::to_string(&source.labels)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE sources
             SET title = ?2, collection = ?3, labels = ?4, updated_at = ?5
             WHERE id = ?1",
            params![
                source.id,
                source.title,
                source.collection,
                labels,
                source.updated_at
            ],
        )?;
        if changed == 0 {
            anyhow::bail!("source not found: {}", source.id);
        }
        drop(conn);
        self.get_source(&source.id)?
            .ok_or_else(|| anyhow!("source disappeared after metadata update"))
    }

    /// Search chunks with FTS5 (all terms, then any term) and a bounded literal
    /// fallback for CJK, short queries, or an FTS miss. Literal results are
    /// fused with FTS results for mixed-script queries instead of discarding
    /// one side. Returns `(hits, degraded)` where degraded marks an any-term
    /// relaxation or a fallback scan.
    pub fn search_source_chunks(
        &self,
        project_id: &str,
        source_ids: &[String],
        query: &SourceSearchQuery,
        limit: Option<u32>,
    ) -> Result<(Vec<SourceSearchHit>, bool)> {
        let limit = limit.unwrap_or(8).clamp(1, 50) as usize;
        let source_ids = serde_json::to_string(source_ids)?;
        let collections = serde_json::to_string(&query.collections)?;
        let labels = serde_json::to_string(&query.labels)?;
        let conn = self.lock()?;
        let filter = " AND (?3 = '[]' OR c.source_id IN (SELECT value FROM json_each(?3)))
               AND (?4 = '[]' OR COALESCE(s.collection, '') IN (SELECT value FROM json_each(?4)))
               AND (?5 = '[]' OR EXISTS (
                   SELECT 1 FROM json_each(s.labels) requested
                   WHERE requested.value IN (SELECT value FROM json_each(?5))
               ))";
        let sql = format!(
            "SELECT c.source_id, s.title, s.collection, s.labels, c.id, c.chunk_no,
                    c.heading, c.content, bm25(source_chunks_fts) AS rank
             FROM source_chunks_fts
             JOIN source_chunks c ON c.rowid = source_chunks_fts.rowid
             JOIN sources s ON s.id = c.source_id
             WHERE source_chunks_fts MATCH ?1
               AND c.project_id = ?2
               AND s.status = 'indexed'{filter}
             ORDER BY rank ASC
             LIMIT ?6"
        );
        let mut fused: HashMap<String, SourceSearchHit> = HashMap::new();
        let mut degraded = false;
        let mut needs_literal_fallback = query.prefer_like;
        for (index, fts) in [&query.all, &query.any].into_iter().enumerate() {
            if fts.is_empty() {
                continue;
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![
                    fts,
                    project_id,
                    source_ids,
                    collections,
                    labels,
                    limit as i64
                ],
                |row| {
                    let rank: f64 = row.get(8).unwrap_or(0.0);
                    let labels_blob: String = row.get(3)?;
                    Ok(SourceSearchHit {
                        source_id: row.get(0)?,
                        source_title: row.get(1)?,
                        collection: row.get(2)?,
                        labels: serde_json::from_str(&labels_blob).unwrap_or_default(),
                        chunk_id: row.get(4)?,
                        chunk_no: row.get::<_, i64>(5)?.max(0) as u32,
                        heading: row.get(6)?,
                        content: row.get(7)?,
                        score: (-rank).max(0.0),
                    })
                },
            );
            let Ok(rows) = rows else {
                degraded = true;
                needs_literal_fallback = true;
                break;
            };
            let hits = rows.collect::<Result<Vec<_>, _>>()?;
            if hits.is_empty() {
                continue;
            }
            // An any-term hit is a relaxation, not an exact match.
            if index == 1 {
                degraded = true;
            }
            for hit in hits {
                fused.entry(hit.chunk_id.clone()).or_insert(hit);
            }
            // The all-terms query already found something; do not widen it a
            // second time unless the caller explicitly wants a literal merge.
            break;
        }

        if fused.is_empty() {
            needs_literal_fallback = true;
        }
        if !needs_literal_fallback {
            let mut hits = fused.into_values().collect::<Vec<_>>();
            hits.sort_by(|left, right| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.chunk_no.cmp(&right.chunk_no))
            });
            hits.truncate(limit);
            return Ok((hits, degraded));
        }

        // CJK and mixed-script queries always run the literal pass too. FTS5's
        // unicode61 tokenizer can treat a whole CJK run as one token, while a
        // mixed English/CJK query still benefits from the English FTS hits.
        degraded = true;
        let raw = query.raw.trim();
        let mut patterns = Vec::new();
        for value in std::iter::once(raw).chain(query.terms.iter().map(String::as_str)) {
            if value.is_empty() {
                continue;
            }
            let pattern = format!("%{}%", escape_like(value));
            if !patterns.contains(&pattern) {
                patterns.push(pattern);
            }
            if patterns.len() >= 40 {
                break;
            }
        }
        if patterns.is_empty() {
            return Ok((fused.into_values().collect(), degraded));
        }
        let patterns = serde_json::to_string(&patterns)?;
        let like_sql = "SELECT c.source_id, s.title, s.collection, s.labels, c.id, c.chunk_no,
                    c.heading, c.content
             FROM source_chunks c
             JOIN sources s ON s.id = c.source_id
             WHERE c.project_id = ?1
               AND s.status = 'indexed'
               AND (?2 = '[]' OR c.source_id IN (SELECT value FROM json_each(?2)))
               AND (?3 = '[]' OR COALESCE(s.collection, '') IN (SELECT value FROM json_each(?3)))
               AND (?4 = '[]' OR EXISTS (
                   SELECT 1 FROM json_each(s.labels) requested
                   WHERE requested.value IN (SELECT value FROM json_each(?4))
               ))
               AND EXISTS (
                   SELECT 1 FROM json_each(?5) pattern
                   WHERE c.content LIKE pattern.value ESCAPE '\\'
                      OR c.heading LIKE pattern.value ESCAPE '\\'
               )
             ORDER BY c.chunk_no ASC
             LIMIT 500";
        let mut stmt = conn.prepare(like_sql)?;
        let needle = raw.to_lowercase();
        let rows = stmt.query_map(
            params![project_id, source_ids, collections, labels, patterns],
            |row| {
                let labels_blob: String = row.get(3)?;
                Ok(SourceSearchHit {
                    source_id: row.get(0)?,
                    source_title: row.get(1)?,
                    collection: row.get(2)?,
                    labels: serde_json::from_str(&labels_blob).unwrap_or_default(),
                    chunk_id: row.get(4)?,
                    chunk_no: row.get::<_, i64>(5)?.max(0) as u32,
                    heading: row.get(6)?,
                    content: row.get(7)?,
                    score: 0.0,
                })
            },
        )?;
        for row in rows {
            let mut hit = row?;
            let content = hit.content.to_lowercase();
            let heading = hit.heading.as_deref().unwrap_or("").to_lowercase();
            let raw_matches = content.matches(&needle).count() as f64;
            let raw_heading_matches = heading.matches(&needle).count() as f64;
            let term_matches = query
                .terms
                .iter()
                .map(|term| content.matches(term).count() + heading.matches(term).count())
                .sum::<usize>() as f64;
            if raw_matches == 0.0 && raw_heading_matches == 0.0 && term_matches == 0.0 {
                continue;
            }
            let score = raw_matches * 4.0 + raw_heading_matches * 8.0 + term_matches;
            if let Some(existing) = fused.get_mut(&hit.chunk_id) {
                existing.score = existing.score.max(score);
            } else {
                hit.score = score;
                fused.insert(hit.chunk_id.clone(), hit);
            }
        }
        let mut hits = fused.into_values().collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.chunk_no.cmp(&right.chunk_no))
        });
        hits.truncate(limit);
        Ok((hits, degraded))
    }

    fn source_dir(&self, project_id: &str) -> PathBuf {
        self.data_dir.join("sources").join(project_id)
    }

    /// Persist the durable source snapshot before/after the SQLite projection.
    /// A failed append must not be treated as a successful source add.
    pub fn write_source_snapshot(&self, snapshot: &SourceSnapshot) -> Result<()> {
        let dir = self.source_dir(&snapshot.source.project_id);
        paths::ensure_private_dir(&dir)?;
        let path = dir.join(format!("{}.json", snapshot.source.id));
        let temp = dir.join(format!(".tmp-{}.json", Uuid::now_v7()));
        let bytes = serde_json::to_vec(snapshot)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("creating source snapshot {}", temp.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
            .with_context(|| format!("replacing source snapshot {}", path.display()))?;
        paths::ensure_private_file(&path)?;
        Ok(())
    }

    pub fn read_source_snapshot(
        &self,
        project_id: &str,
        source_id: &str,
    ) -> Result<Option<SourceSnapshot>> {
        let path = self
            .source_dir(project_id)
            .join(format!("{}.json", source_id));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("parsing source snapshot {}", path.display())
            })?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn delete_source_snapshot(&self, project_id: &str, source_id: &str) -> Result<()> {
        let path = self
            .source_dir(project_id)
            .join(format!("{}.json", source_id));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
        }
    }

    /// All durable snapshots, used by `db-rebuild` and source recovery.
    pub fn list_source_snapshots(&self) -> Result<Vec<SourceSnapshot>> {
        let root = self.data_dir.join("sources");
        let mut snapshots = Vec::new();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("reading {}", root.display())),
        };
        for project in entries {
            let project = match project {
                Ok(project) => project,
                Err(_) => continue,
            };
            if !project
                .file_type()
                .map(|kind| kind.is_dir())
                .unwrap_or(false)
            {
                continue;
            }
            let files = match std::fs::read_dir(project.path()) {
                Ok(files) => files,
                Err(_) => continue,
            };
            for file in files {
                let file = match file {
                    Ok(file) => file,
                    Err(_) => continue,
                };
                if file.path().extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let bytes = match std::fs::read(file.path()) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(%error, path = %file.path().display(), "could not read source snapshot");
                        continue;
                    }
                };
                match serde_json::from_slice::<SourceSnapshot>(&bytes) {
                    Ok(snapshot) => snapshots.push(snapshot),
                    Err(error) => {
                        tracing::warn!(%error, path = %file.path().display(), "skipping malformed source snapshot");
                    }
                }
            }
        }
        Ok(snapshots)
    }

    const SKILL_COLUMNS: &'static str = "id, project_id, name, slug, description, when_to_use, \
        version, instructions, tool_allow, tool_deny, profiles, roles, enabled, source, sha256, \
        created_at, updated_at";

    pub fn list_skills(
        &self,
        project_id: Option<&str>,
        include_disabled: bool,
    ) -> Result<Vec<Skill>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM skills
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 = 1 OR enabled = 1)
             ORDER BY name COLLATE NOCASE ASC, id ASC",
            Self::SKILL_COLUMNS
        ))?;
        let rows = stmt.query_map(
            params![project_id, if include_disabled { 1 } else { 0 }],
            map_skill,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_skill(&self, id: &str) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {} FROM skills WHERE id = ?1", Self::SKILL_COLUMNS),
            params![id],
            map_skill,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_skill_by_slug(&self, project_id: &str, slug: &str) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!(
                "SELECT {} FROM skills WHERE project_id = ?1 AND slug = ?2",
                Self::SKILL_COLUMNS
            ),
            params![project_id, slug],
            map_skill,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn insert_skill(&self, skill: &Skill) -> Result<Skill> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO skills
             (id, project_id, name, slug, description, when_to_use, version, instructions,
              tool_allow, tool_deny, profiles, roles, enabled, source, sha256, created_at,
              updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                slug = excluded.slug,
                description = excluded.description,
                when_to_use = excluded.when_to_use,
                version = excluded.version,
                instructions = excluded.instructions,
                tool_allow = excluded.tool_allow,
                tool_deny = excluded.tool_deny,
                profiles = excluded.profiles,
                roles = excluded.roles,
                enabled = excluded.enabled,
                source = excluded.source,
                sha256 = excluded.sha256,
                updated_at = excluded.updated_at",
            params![
                skill.id,
                skill.project_id,
                skill.name,
                skill.slug,
                skill.description,
                skill.when_to_use,
                skill.version,
                skill.instructions,
                serde_json::to_string(&skill.tool_allow)?,
                serde_json::to_string(&skill.tool_deny)?,
                serde_json::to_string(&skill.profiles)?,
                serde_json::to_string(&skill.roles)?,
                if skill.enabled { 1 } else { 0 },
                skill.source,
                skill.sha256,
                skill.created_at,
                skill.updated_at,
            ],
        )?;
        drop(conn);
        self.get_skill(&skill.id)?
            .ok_or_else(|| anyhow!("skill disappeared after insert"))
    }

    pub fn delete_skill(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute("DELETE FROM skills WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE skills SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, if enabled { 1 } else { 0 }, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_skill(id)
    }

    /// Record a coordination note and update its projection in one
    /// transaction. Unblock events resolve an existing note (or every open
    /// blocker for a task) and do not create a new note row.
    #[allow(clippy::too_many_arguments)]
    pub fn record_coordination_note_with_event(
        &self,
        session_id: &str,
        action: &str,
        severity: &str,
        task_id: Option<&str>,
        target_agent_id: Option<&str>,
        target_note_id: Option<&str>,
        agent_id: &str,
        agent_name: &str,
        agent_role: Option<&str>,
        message: &str,
    ) -> Result<Event> {
        let note_id = Uuid::now_v7().to_string();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let payload = serde_json::json!({
            "note_id": note_id,
            "action": action,
            "severity": severity,
            "task_id": task_id,
            "target_agent_id": target_agent_id,
            "target_note_id": target_note_id,
            "message": message,
            "agent_id": agent_id,
            "agent_name": agent_name,
            "agent_role": agent_role,
        });
        let event = insert_event_tx(&tx, session_id, "coordination.note", payload)?;
        if action == "unblock" {
            let changed = if let Some(target_note_id) = target_note_id {
                tx.execute(
                    "UPDATE coordination_notes
                     SET status = 'resolved', resolved_at = ?1
                     WHERE note_id = ?2 AND session_id = ?3 AND status = 'open'",
                    params![event.created_at, target_note_id, session_id],
                )?
            } else if let Some(task_id) = task_id {
                tx.execute(
                    "UPDATE coordination_notes
                     SET status = 'resolved', resolved_at = ?1
                     WHERE task_id = ?2 AND session_id = ?3 AND status = 'open'",
                    params![event.created_at, task_id, session_id],
                )?
            } else {
                return Err(anyhow!("an unblock note needs a task id or target note id"));
            };
            if changed == 0 {
                return Err(anyhow!("no open coordination blocker matched the unblock"));
            }
        }
        if action != "unblock" {
            let status = if action == "blocker" { "open" } else { "info" };
            tx.execute(
                "INSERT INTO coordination_notes
                 (note_id, session_id, seq, action, severity, status, task_id,
                  target_agent_id, agent_id, agent_name, message, created_at, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL)",
                params![
                    note_id,
                    session_id,
                    event.seq as i64,
                    action,
                    severity,
                    status,
                    task_id,
                    target_agent_id,
                    agent_id,
                    agent_name,
                    message,
                    event.created_at
                ],
            )?;
        }
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn list_coordination_notes(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT note_id, action, severity, status, task_id, target_agent_id,
                    agent_id, agent_name, message, created_at, resolved_at
             FROM coordination_notes
             WHERE session_id = ?1
             ORDER BY seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![session_id, i64::from(limit.clamp(1, 2_000))],
            |row| {
                Ok(OrchestrationNote {
                    note_id: row.get(0)?,
                    action: row.get(1)?,
                    severity: row.get(2)?,
                    status: row.get(3)?,
                    task_id: row.get(4)?,
                    target_agent_id: row.get(5)?,
                    agent_id: row.get(6)?,
                    agent_name: row.get(7)?,
                    message: row.get(8)?,
                    created_at: row.get(9)?,
                    resolved_at: row.get(10)?,
                })
            },
        )?;
        let mut notes = rows.collect::<Result<Vec<_>, _>>()?;
        notes.reverse();
        Ok(notes)
    }

    /// Notes that matter to one agent: direct targets, notes on its task,
    /// and shared handoffs/decisions/escalations. This keeps a Worker's brief
    /// focused instead of replaying every note in the session.
    pub fn list_coordination_notes_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT note_id, action, severity, status, task_id, target_agent_id,
                    agent_id, agent_name, message, created_at, resolved_at
             FROM coordination_notes
             WHERE session_id = ?1
               AND (
                    target_agent_id = ?2
                    OR (?3 IS NOT NULL AND task_id = ?3)
                    OR (target_agent_id IS NULL AND action IN ('handoff', 'decision', 'escalate'))
               )
             ORDER BY seq DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                session_id,
                agent_id,
                task_id,
                i64::from(limit.clamp(1, 500))
            ],
            |row| {
                Ok(OrchestrationNote {
                    note_id: row.get(0)?,
                    action: row.get(1)?,
                    severity: row.get(2)?,
                    status: row.get(3)?,
                    task_id: row.get(4)?,
                    target_agent_id: row.get(5)?,
                    agent_id: row.get(6)?,
                    agent_name: row.get(7)?,
                    message: row.get(8)?,
                    created_at: row.get(9)?,
                    resolved_at: row.get(10)?,
                })
            },
        )?;
        let mut notes = rows.collect::<Result<Vec<_>, _>>()?;
        notes.reverse();
        Ok(notes)
    }

    pub fn replace_coordination_notes(
        &self,
        session_id: &str,
        notes: &[OrchestrationNote],
        events: &[Event],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM coordination_notes WHERE session_id = ?1",
            params![session_id],
        )?;
        let seq_by_note = events
            .iter()
            .filter(|event| event.kind == "coordination.note")
            .filter_map(|event| {
                let note_id = event.payload.get("note_id")?.as_str()?;
                Some((note_id.to_string(), event.seq))
            })
            .collect::<std::collections::HashMap<_, _>>();
        for note in notes {
            tx.execute(
                "INSERT INTO coordination_notes
                 (note_id, session_id, seq, action, severity, status, task_id,
                  target_agent_id, agent_id, agent_name, message, created_at, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    note.note_id,
                    session_id,
                    *seq_by_note.get(&note.note_id).unwrap_or(&0) as i64,
                    note.action,
                    note.severity,
                    note.status,
                    note.task_id,
                    note.target_agent_id,
                    note.agent_id,
                    note.agent_name,
                    note.message,
                    note.created_at,
                    note.resolved_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn record_orchestration_decision_with_event(
        &self,
        session_id: &str,
        action: &str,
        task_id: Option<&str>,
        detail: &str,
        attempt: u32,
        agent_id: &str,
    ) -> Result<Event> {
        let decision_id = Uuid::now_v7().to_string();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "orchestration.decision",
            serde_json::json!({
                "decision_id": decision_id,
                "action": action,
                "task_id": task_id,
                "detail": detail,
                "attempt": attempt,
                "agent_id": agent_id,
            }),
        )?;
        tx.execute(
            "INSERT INTO orchestration_decisions
             (decision_id, session_id, seq, action, task_id, detail, attempt, agent_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                decision_id,
                session_id,
                event.seq as i64,
                action,
                task_id,
                detail,
                i64::from(attempt),
                agent_id,
                event.created_at
            ],
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn list_orchestration_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationDecision>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT decision_id, action, task_id, detail, attempt, agent_id, created_at
             FROM orchestration_decisions
             WHERE session_id = ?1
             ORDER BY seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![session_id, i64::from(limit.clamp(1, 2_000))],
            |row| {
                Ok(OrchestrationDecision {
                    decision_id: row.get(0)?,
                    action: row.get(1)?,
                    task_id: row.get(2)?,
                    detail: row.get(3)?,
                    attempt: row.get::<_, i64>(4)?.max(0) as u32,
                    agent_id: row.get(5)?,
                    created_at: row.get(6)?,
                })
            },
        )?;
        let mut decisions = rows.collect::<Result<Vec<_>, _>>()?;
        decisions.reverse();
        Ok(decisions)
    }

    pub fn replace_orchestration_decisions(
        &self,
        session_id: &str,
        decisions: &[OrchestrationDecision],
        events: &[Event],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM orchestration_decisions WHERE session_id = ?1",
            params![session_id],
        )?;
        let seq_by_decision = events
            .iter()
            .filter(|event| event.kind == "orchestration.decision")
            .filter_map(|event| {
                let decision_id = event.payload.get("decision_id")?.as_str()?;
                Some((decision_id.to_string(), event.seq))
            })
            .collect::<std::collections::HashMap<_, _>>();
        for decision in decisions {
            tx.execute(
                "INSERT INTO orchestration_decisions
                 (decision_id, session_id, seq, action, task_id, detail, attempt, agent_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    decision.decision_id,
                    session_id,
                    *seq_by_decision.get(&decision.decision_id).unwrap_or(&0) as i64,
                    decision.action,
                    decision.task_id,
                    decision.detail,
                    i64::from(decision.attempt),
                    decision.agent_id,
                    decision.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_approval_grants(&self, project_id: &str) -> Result<Vec<ApprovalGrant>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, tool, path_prefix, include_children, created_by, created_at
             FROM approval_grants WHERE project_id = ?1
             ORDER BY created_at DESC, id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(ApprovalGrant {
                id: row.get(0)?,
                project_id: row.get(1)?,
                tool: row.get(2)?,
                path_prefix: row.get(3)?,
                include_children: row.get::<_, i64>(4)? != 0,
                created_by: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn create_approval_grant(
        &self,
        project_id: &str,
        tool: &str,
        path_prefix: &str,
        include_children: bool,
        created_by: &str,
    ) -> Result<ApprovalGrant> {
        let grant = ApprovalGrant {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            tool: tool.to_string(),
            path_prefix: path_prefix.to_string(),
            include_children,
            created_by: created_by.to_string(),
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approval_grants
             (id, project_id, tool, path_prefix, include_children, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                grant.id,
                grant.project_id,
                grant.tool,
                grant.path_prefix,
                if grant.include_children { 1 } else { 0 },
                grant.created_by,
                grant.created_at
            ],
        )?;
        Ok(grant)
    }

    pub fn delete_approval_grant(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM approval_grants WHERE id = ?1", params![id])? > 0)
    }

    pub fn restore_approval_grants(
        &self,
        project_id: &str,
        grants: &[ApprovalGrant],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM approval_grants WHERE project_id = ?1",
            params![project_id],
        )?;
        for grant in grants {
            tx.execute(
                "INSERT INTO approval_grants
                 (id, project_id, tool, path_prefix, include_children, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    grant.id,
                    project_id,
                    grant.tool,
                    grant.path_prefix,
                    if grant.include_children { 1 } else { 0 },
                    grant.created_by,
                    grant.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_orchestration_policy(&self, project_id: &str) -> Result<OrchestrationPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_orchestration_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(stored
            .and_then(|raw| serde_json::from_str::<OrchestrationPolicy>(&raw).ok())
            .unwrap_or_default())
    }

    pub fn set_orchestration_policy(
        &self,
        project_id: &str,
        policy: &OrchestrationPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_orchestration_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, serde_json::to_string(policy)?, now_rfc3339()],
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
        self.create_session_with_limits_and_priority(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            0,
        )
    }

    pub fn create_session_with_limits_and_priority(
        &self,
        project_id: &str,
        goal: &str,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
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
            false,
            None,
            priority,
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
        unlimited: bool,
        base_commit: &str,
        priority: i32,
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
            unlimited,
            Some(base_commit.to_string()),
            priority,
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
        unlimited: bool,
        base_commit: Option<String>,
        priority: i32,
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
            priority,
            worktree,
            branch,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
            cost_spent_usd: 0.0,
            base_commit,
            tokens_in: 0,
            tokens_out: 0,
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sessions
             (id, project_id, goal, status, priority, worktree, branch, token_budget,
              cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit,
              tokens_in, tokens_out, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, 0, 0, ?13)",
            params![
                session.id,
                session.project_id,
                session.goal,
                session.status,
                session.priority,
                session.worktree,
                session.branch,
                session.token_budget,
                session.cost_budget_usd,
                session.time_budget_secs,
                if session.unlimited { 1 } else { 0 },
                session.base_commit,
                session.created_at
            ],
        )?;
        Ok(session)
    }

    pub fn set_session_priority(&self, session_id: &str, priority: i32) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET priority = ?1 WHERE id = ?2",
            params![priority, session_id],
        )?;
        Ok(())
    }

    pub fn set_session_priority_with_event(
        &self,
        session_id: &str,
        priority: i32,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET priority = ?1 WHERE id = ?2",
            params![priority, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "session.priority",
            serde_json::json!({"priority": priority}),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn set_session_unlimited_with_event(
        &self,
        session_id: &str,
        unlimited: bool,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET unlimited = ?1 WHERE id = ?2",
            params![if unlimited { 1 } else { 0 }, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "session.budget_mode_changed",
            serde_json::json!({
                "unlimited": unlimited,
                "budget_mode": if unlimited { "unlimited" } else { "limited" },
            }),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
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
            version: 1,
            created_at: now_rfc3339(),
        };
        tx.execute(
            "INSERT INTO artifacts
             (id, session_id, agent_id, agent_name, task_id, name, kind, mime_type,
              content, storage, compression, size_bytes, sha256, path, created_at, version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 1)",
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
        insert_artifact_version_tx(&tx, &artifact)?;
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
                "version": artifact.version,
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
            version: 1,
            created_at: now_rfc3339(),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO artifacts
             (id, session_id, agent_id, agent_name, task_id, name, kind, mime_type,
              content, storage, compression, size_bytes, sha256, path, created_at, version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '', 'file', 'zstd', ?9, ?10, ?11, ?12, 1)",
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
        insert_artifact_version_tx(&tx, &artifact)?;
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
                "version": artifact.version,
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

    /// Publish a new version of an inline text artifact. File-backed artifacts
    /// are intentionally immutable here because their blobs also participate
    /// in content-addressed GC.
    pub fn update_artifact_with_event(
        &self,
        artifact_id: &str,
        expected_version: Option<u32>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
    ) -> Result<Option<(Artifact, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                        mime_type, content, storage, compression, size_bytes, sha256, path,
                        created_at, version
                 FROM artifacts WHERE id = ?1",
                params![artifact_id],
                map_artifact,
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(None);
        };
        if current.storage != "inline" {
            anyhow::bail!("binary artifact versions are not editable");
        }
        if let Some(expected) = expected_version
            && expected != current.version
        {
            anyhow::bail!(
                "artifact version conflict: expected {expected}, current {}",
                current.version
            );
        }
        // Preserve the current version for legacy artifacts that predate the
        // version table before moving the row forward.
        insert_artifact_version_tx(&tx, &current)?;
        let sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        let updated = Artifact {
            name: name.to_string(),
            kind: kind.to_string(),
            mime_type: mime_type.to_string(),
            content: content.to_string(),
            size_bytes: content.len() as u64,
            sha256,
            version: current.version.saturating_add(1),
            created_at: current.created_at.clone(),
            ..current.clone()
        };
        tx.execute(
            "UPDATE artifacts
             SET name = ?2, kind = ?3, mime_type = ?4, content = ?5, size_bytes = ?6,
                 sha256 = ?7, version = ?8
             WHERE id = ?1",
            params![
                artifact_id,
                updated.name,
                updated.kind,
                updated.mime_type,
                updated.content,
                updated.size_bytes as i64,
                updated.sha256,
                updated.version as i64,
            ],
        )?;
        insert_artifact_version_tx(&tx, &updated)?;
        let event = insert_event_tx(
            &tx,
            &updated.session_id,
            "artifact.version_published",
            serde_json::json!({
                "artifact_id": updated.id,
                "name": updated.name,
                "kind": updated.kind,
                "agent_id": updated.agent_id,
                "agent_name": updated.agent_name,
                "task_id": updated.task_id,
                "mime_type": updated.mime_type,
                "storage": updated.storage,
                "compression": updated.compression,
                "size_bytes": updated.size_bytes,
                "sha256": updated.sha256,
                "path": updated.path,
                "content": updated.content,
                "version": updated.version,
                "previous_version": current.version,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((updated, event)))
    }

    pub fn list_artifact_versions(&self, artifact_id: &str) -> Result<Vec<ArtifactVersion>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT artifact_id, version, name, kind, mime_type, content, storage,
                    compression, size_bytes, sha256, path, created_at
             FROM artifact_versions WHERE artifact_id = ?1 ORDER BY version ASC",
        )?;
        let rows = stmt.query_map(params![artifact_id], map_artifact_version)?;
        let versions = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        drop(conn);
        if !versions.is_empty() {
            return Ok(versions);
        }
        // Artifacts created before version rows existed still have a valid
        // current version. Expose that snapshot rather than an empty history.
        Ok(self
            .get_artifact(artifact_id)?
            .map(|artifact| vec![artifact_version_from_artifact(&artifact)])
            .unwrap_or_default())
    }

    pub fn get_artifact_version(
        &self,
        artifact_id: &str,
        version: u32,
    ) -> Result<Option<ArtifactVersion>> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT artifact_id, version, name, kind, mime_type, content, storage,
                        compression, size_bytes, sha256, path, created_at
                 FROM artifact_versions WHERE artifact_id = ?1 AND version = ?2",
                params![artifact_id, version as i64],
                map_artifact_version,
            )
            .optional()?;
        drop(conn);
        if stored.is_some() {
            return Ok(stored);
        }
        let Some(artifact) = self.get_artifact(artifact_id)? else {
            return Ok(None);
        };
        if artifact.version != version {
            return Ok(None);
        }
        Ok(Some(artifact_version_from_artifact(&artifact)))
    }

    pub fn restore_artifact_version(
        &self,
        artifact_id: &str,
        version: u32,
    ) -> Result<Option<(Artifact, Event)>> {
        let Some(version_record) = self.get_artifact_version(artifact_id, version)? else {
            return Ok(None);
        };
        if version_record.storage != "inline" {
            anyhow::bail!("binary artifact versions cannot be restored");
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                        mime_type, content, storage, compression, size_bytes, sha256, path,
                        created_at, version
                 FROM artifacts WHERE id = ?1",
                params![artifact_id],
                map_artifact,
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(None);
        };
        if current.storage != "inline" {
            anyhow::bail!("binary artifact versions cannot be restored");
        }
        insert_artifact_version_tx(&tx, &current)?;
        let updated = Artifact {
            name: version_record.name,
            kind: version_record.kind,
            mime_type: version_record.mime_type,
            content: version_record.content,
            size_bytes: version_record.size_bytes,
            sha256: version_record.sha256,
            version: current.version.saturating_add(1),
            ..current.clone()
        };
        tx.execute(
            "UPDATE artifacts
             SET name = ?2, kind = ?3, mime_type = ?4, content = ?5, size_bytes = ?6,
                 sha256 = ?7, version = ?8
             WHERE id = ?1",
            params![
                artifact_id,
                updated.name,
                updated.kind,
                updated.mime_type,
                updated.content,
                updated.size_bytes as i64,
                updated.sha256,
                updated.version as i64,
            ],
        )?;
        insert_artifact_version_tx(&tx, &updated)?;
        let event = insert_event_tx(
            &tx,
            &updated.session_id,
            "artifact.version_restored",
            serde_json::json!({
                "artifact_id": updated.id,
                "name": updated.name,
                "kind": updated.kind,
                "agent_id": updated.agent_id,
                "agent_name": updated.agent_name,
                "task_id": updated.task_id,
                "mime_type": updated.mime_type,
                "storage": updated.storage,
                "compression": updated.compression,
                "size_bytes": updated.size_bytes,
                "sha256": updated.sha256,
                "path": updated.path,
                "content": updated.content,
                "version": updated.version,
                "restored_from_version": version,
                "previous_version": current.version,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((updated, event)))
    }

    pub fn get_artifact_by_request_key(
        &self,
        session_id: &str,
        request_key: &str,
    ) -> Result<Option<Artifact>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT a.id, a.session_id, a.agent_id, a.agent_name, a.task_id, a.name,
                    a.kind, a.mime_type, a.content, a.storage, a.compression,
                    a.size_bytes, a.sha256, a.path, a.created_at, a.version
             FROM artifact_requests r
             JOIN artifacts a ON a.id = r.artifact_id
             WHERE r.session_id = ?1 AND r.request_key = ?2",
            params![session_id, request_key],
            map_artifact,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn register_artifact_request(
        &self,
        session_id: &str,
        request_key: &str,
        artifact_id: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO artifact_requests (session_id, request_key, artifact_id, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, request_key) DO NOTHING",
            params![session_id, request_key, artifact_id, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn list_artifacts(&self, session_id: &str) -> Result<Vec<Artifact>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                    mime_type, content, storage, compression, size_bytes, sha256, path,
                    created_at, version
             FROM artifacts WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_artifact)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_artifact(&self, artifact_id: &str) -> Result<Option<Artifact>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                    mime_type, content, storage, compression, size_bytes, sha256, path,
                    created_at, version
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

    pub fn get_checkpoint_policy(&self, project_id: &str) -> Result<CheckpointRetentionPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, older_than_secs, keep_latest
                 FROM project_checkpoint_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(CheckpointRetentionPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        older_than_secs: row.get::<_, i64>(1)? as u64,
                        keep_latest: row.get::<_, i64>(2)? as u32,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_checkpoint_policy(
        &self,
        project_id: &str,
        policy: &CheckpointRetentionPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_checkpoint_policies
             (project_id, enabled, older_than_secs, keep_latest)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                older_than_secs = excluded.older_than_secs,
                keep_latest = excluded.keep_latest",
            params![
                project_id,
                i64::from(policy.enabled),
                policy.older_than_secs as i64,
                policy.keep_latest as i64
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_checkpoint_policies(
        &self,
    ) -> Result<Vec<(String, CheckpointRetentionPolicy)>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT project_id, enabled, older_than_secs, keep_latest
             FROM project_checkpoint_policies WHERE enabled = 1",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                CheckpointRetentionPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    older_than_secs: row.get::<_, i64>(2)? as u64,
                    keep_latest: row.get::<_, i64>(3)? as u32,
                },
            ))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            policies.push(row?);
        }
        Ok(policies)
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
                    a.size_bytes, a.sha256, a.path, a.created_at, a.version
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
        if let Some(query) = query
            && let Some(query) = fts_query(query)
        {
            match self.list_memories_fts(project_id, &query, kind, limit) {
                Ok(memories) if !memories.is_empty() => return Ok(memories),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "memory FTS search failed; falling back to LIKE");
                }
            }
        }
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

    fn list_memories_fts(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at
             FROM memories_fts
             JOIN memories m ON m.rowid = memories_fts.rowid
             WHERE memories_fts MATCH ?1
               AND m.project_id = ?2
               AND (?3 IS NULL OR m.kind = ?3)
             ORDER BY bm25(memories_fts), m.created_at DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![query, project_id, kind, limit.clamp(1, 500) as i64],
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

    pub fn delete_memories_with_events(&self, ids: &[String]) -> Result<Vec<(MemoryItem, Event)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut removed = Vec::new();
        for id in ids {
            let memory = tx
                .query_row(
                    "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                            tags, created_at
                     FROM memories WHERE id = ?1",
                    params![id],
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
                )
                .optional()?;
            let Some(memory) = memory else {
                continue;
            };
            tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            let event = insert_event_tx(
                &tx,
                &memory.session_id,
                "memory.deleted",
                serde_json::to_value(&memory)?,
            )?;
            removed.push((memory, event));
        }
        tx.commit()?;
        drop(conn);
        for (_, event) in &removed {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(removed)
    }

    pub fn memory_ids_for_prune(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
    ) -> Result<Vec<String>> {
        if older_than_days.is_none() && keep_latest.is_none() {
            return Ok(Vec::new());
        }
        let cutoff = older_than_days
            .map(|days| time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, created_at FROM memories
             WHERE project_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut ids = Vec::new();
        for (index, row) in rows.enumerate() {
            let (id, created_at) = row?;
            let beyond_keep = keep_latest.is_some_and(|keep| index >= keep as usize);
            let old = cutoff.is_some_and(|cutoff| {
                time::OffsetDateTime::parse(
                    &created_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .map(|created_at| created_at < cutoff)
                .unwrap_or(false)
            });
            if beyond_keep || old {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    pub fn get_memory_policy(&self, project_id: &str) -> Result<MemoryPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, max_items, max_age_days
                 FROM project_memory_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(MemoryPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        max_items: row
                            .get::<_, Option<i64>>(1)?
                            .and_then(|value| u32::try_from(value).ok()),
                        max_age_days: row
                            .get::<_, Option<i64>>(2)?
                            .and_then(|value| u32::try_from(value).ok()),
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_memory_policy(&self, project_id: &str, policy: &MemoryPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_memory_policies
             (project_id, enabled, max_items, max_age_days, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                max_items = excluded.max_items,
                max_age_days = excluded.max_age_days,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.max_items.map(i64::from),
                policy.max_age_days.map(i64::from),
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_memory_policies(&self) -> Result<Vec<(String, MemoryPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, max_items, max_age_days
             FROM project_memory_policies WHERE enabled = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MemoryPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    max_items: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|value| u32::try_from(value).ok()),
                    max_age_days: row
                        .get::<_, Option<i64>>(3)?
                        .and_then(|value| u32::try_from(value).ok()),
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_mcp_servers(&self) -> Result<Vec<McpServer>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, command, args, env, enabled, created_at, transport, url, headers
             FROM mcp_servers ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], map_mcp_server)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_mcp_server(&self, server_id: &str) -> Result<Option<McpServer>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, command, args, env, enabled, created_at, transport, url, headers
             FROM mcp_servers WHERE id = ?1",
            params![server_id],
            map_mcp_server,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn add_mcp_server(&self, config: &McpServerConfig) -> Result<McpServer> {
        let server = McpServer {
            id: Uuid::now_v7().to_string(),
            name: config.name.clone(),
            transport: config.transport,
            command: config.command.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
            url: config.url.clone(),
            headers: config.headers.clone(),
            enabled: config.enabled,
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO mcp_servers
                (id, name, command, args, env, enabled, created_at, transport, url, headers)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                server.id,
                server.name,
                server.command,
                serde_json::to_string(&server.args)?,
                serde_json::to_string(&server.env)?,
                if server.enabled { 1 } else { 0 },
                server.created_at,
                transport_name(server.transport),
                server.url,
                serde_json::to_string(&server.headers)?,
            ],
        )?;
        Ok(server)
    }

    pub fn update_mcp_server(
        &self,
        server_id: &str,
        config: &McpServerConfig,
    ) -> Result<Option<McpServer>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE mcp_servers
             SET name = ?1, command = ?2, args = ?3, env = ?4, enabled = ?5,
                 transport = ?6, url = ?7, headers = ?8
             WHERE id = ?9",
            params![
                config.name,
                config.command,
                serde_json::to_string(&config.args)?,
                serde_json::to_string(&config.env)?,
                if config.enabled { 1 } else { 0 },
                transport_name(config.transport),
                config.url,
                serde_json::to_string(&config.headers)?,
                server_id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_mcp_server(server_id)
    }

    pub fn remove_mcp_server(&self, server_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM mcp_servers WHERE id = ?1", params![server_id])? > 0)
    }

    pub fn list_acp_agents(&self) -> Result<Vec<AcpAgent>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, command, args, env, enabled, auto_approve, timeout_secs, created_at
             FROM acp_agents ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], map_acp_agent)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_acp_agent(&self, agent_id: &str) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, command, args, env, enabled, auto_approve, timeout_secs, created_at
             FROM acp_agents WHERE id = ?1",
            params![agent_id],
            map_acp_agent,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_acp_agent(&self, identifier: &str) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, command, args, env, enabled, auto_approve, timeout_secs, created_at
             FROM acp_agents WHERE id = ?1 OR name = ?1",
            params![identifier],
            map_acp_agent,
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_acp_agent(
        &self,
        name: &str,
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
        enabled: bool,
        auto_approve: bool,
        timeout_secs: u64,
    ) -> Result<AcpAgent> {
        let agent = AcpAgent {
            id: Uuid::now_v7().to_string(),
            name: name.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            env: env.clone(),
            enabled,
            auto_approve,
            timeout_secs,
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO acp_agents
             (id, name, command, args, env, enabled, auto_approve, timeout_secs, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                agent.id,
                agent.name,
                agent.command,
                serde_json::to_string(&agent.args)?,
                serde_json::to_string(&agent.env)?,
                if agent.enabled { 1 } else { 0 },
                if agent.auto_approve { 1 } else { 0 },
                agent.timeout_secs as i64,
                agent.created_at
            ],
        )?;
        Ok(agent)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_acp_agent(
        &self,
        agent_id: &str,
        name: &str,
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
        enabled: bool,
        auto_approve: bool,
        timeout_secs: u64,
    ) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE acp_agents SET name = ?1, command = ?2, args = ?3, env = ?4,
                enabled = ?5, auto_approve = ?6, timeout_secs = ?7
             WHERE id = ?8",
            params![
                name,
                command,
                serde_json::to_string(args)?,
                serde_json::to_string(env)?,
                if enabled { 1 } else { 0 },
                if auto_approve { 1 } else { 0 },
                timeout_secs as i64,
                agent_id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_acp_agent(agent_id)
    }

    pub fn remove_acp_agent(&self, agent_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM acp_agents WHERE id = ?1", params![agent_id])? > 0)
    }

    pub fn list_model_presets(&self) -> Result<Vec<ModelPreset>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, name, provider, model, context_window,
                    input_usd_per_million, output_usd_per_million, notes, created_at
             FROM model_presets ORDER BY name COLLATE NOCASE, created_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ModelPreset {
                id: row.get(0)?,
                name: row.get(1)?,
                provider: row.get(2)?,
                model: row.get(3)?,
                context_window: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                input_usd_per_million: row.get(5)?,
                output_usd_per_million: row.get(6)?,
                notes: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        let mut presets = Vec::new();
        for preset in rows {
            presets.push(preset?);
        }
        Ok(presets)
    }

    pub fn create_model_preset(&self, preset: &ModelPreset) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO model_presets
             (id, name, provider, model, context_window, input_usd_per_million,
              output_usd_per_million, notes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                preset.id,
                preset.name,
                preset.provider,
                preset.model,
                preset.context_window.map(|value| value as i64),
                preset.input_usd_per_million,
                preset.output_usd_per_million,
                preset.notes,
                preset.created_at
            ],
        )?;
        Ok(())
    }

    pub fn update_model_preset(&self, preset: &ModelPreset) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute(
            "UPDATE model_presets SET
                name = ?2, provider = ?3, model = ?4, context_window = ?5,
                input_usd_per_million = ?6, output_usd_per_million = ?7, notes = ?8
             WHERE id = ?1",
            params![
                preset.id,
                preset.name,
                preset.provider,
                preset.model,
                preset.context_window.map(|value| value as i64),
                preset.input_usd_per_million,
                preset.output_usd_per_million,
                preset.notes
            ],
        )? > 0)
    }

    pub fn delete_model_preset(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM model_presets WHERE id = ?1", params![id])? > 0)
    }

    pub fn get_model_preset(&self, id: &str) -> Result<Option<ModelPreset>> {
        let presets = self.list_model_presets()?;
        Ok(presets.into_iter().find(|preset| preset.id == id))
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
                    "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at, priority
                     FROM sessions WHERE project_id = ?1
                     ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map(params![project_id], map_session)?;
                sessions.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at, priority
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
            "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at, priority
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
        validate_storage_id("session", &event.session_id)?;
        let dir = self.data_dir.join("sessions").join(&event.session_id);
        paths::ensure_private_dir(&dir)?;
        let path = dir.join("events.jsonl");
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        let _guard = self
            .mirror_lock
            .lock()
            .map_err(|_| anyhow!("event mirror mutex poisoned"))?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(&line)?;
        file.flush()?;
        Ok(())
    }

    pub fn read_event_mirror(&self, session_id: &str) -> Result<Option<Vec<Event>>> {
        validate_storage_id("session", session_id)?;
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
        // Concurrent writers may commit SQLite in a different order than the
        // mirror append lock serializes lines. The event sequence is the
        // canonical order, so normalize on read before log-status/restore.
        events.sort_by_key(|event| event.seq);
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

    pub fn latest_event_seq(&self, session_id: &str, kind: &str) -> Result<u64> {
        let conn = self.lock()?;
        let seq = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1 AND type = ?2",
            params![session_id, kind],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(seq.max(0) as u64)
    }

    /// Latest events for a session in ascending order. Used by coordination
    /// views/tools that need the recent board without loading full history.
    pub fn recent_events(&self, session_id: &str, limit: u32) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1
             ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![session_id, limit.clamp(1, 2_000)], |row| {
            let payload: String = row.get(3)?;
            Ok(Event {
                session_id: row.get(0)?,
                seq: row.get::<_, i64>(1)? as u64,
                kind: row.get(2)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        events.reverse();
        Ok(events)
    }

    pub fn list_events_by_kind(
        &self,
        session_id: &str,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 AND type = ?2
             ORDER BY seq ASC LIMIT ?3",
        )?;
        let rows =
            statement.query_map(params![session_id, kind, limit.clamp(1, 10_000)], |row| {
                let payload: String = row.get(3)?;
                Ok(Event {
                    session_id: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: row.get(4)?,
                })
            })?;
        let mut events = Vec::new();
        for event in rows {
            events.push(event?);
        }
        Ok(events)
    }

    pub fn latest_event_by_kind(&self, session_id: &str, kind: &str) -> Result<Option<Event>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 AND type = ?2
             ORDER BY seq DESC LIMIT 1",
            params![session_id, kind],
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
        )
        .optional()
        .map_err(Into::into)
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
    /// Search message content and raw event payloads. The FTS5 index handles
    /// word/prefix queries; a plain `LIKE` scan is retained as a fallback for
    /// short or punctuation-only queries and substring-only matches.
    pub fn search(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>> {
        let limit = limit.clamp(1, 500);
        // unicode61 groups CJK runs into single tokens, so a substring query
        // such as "流程" would miss "部署流程" unless it is a token prefix.
        // Keep the substring scan for non-ASCII queries to preserve recall.
        if query.is_ascii() {
            let hits = self.search_fts(query, project_id, limit as i64)?;
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        self.search_like(query, project_id, limit)
    }

    fn search_fts(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SearchHit>> {
        let Some(fts) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let conn = self.lock()?;
        let mut hits = Vec::new();

        let mut message_stmt = conn.prepare(
            "SELECT m.id, m.session_id, s.project_id, s.goal, m.role, m.content, m.created_at
             FROM messages_fts
             JOIN messages m ON m.rowid = messages_fts.rowid
             JOIN sessions s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY m.created_at DESC LIMIT ?3",
        )?;
        let messages = message_stmt.query_map(params![fts, project_id, limit], |row| {
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
             FROM events_fts
             JOIN events e ON e.rowid = events_fts.rowid
             JOIN sessions s ON s.id = e.session_id
             WHERE events_fts MATCH ?1
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY e.created_at DESC LIMIT ?3",
        )?;
        let events = event_stmt.query_map(params![fts, project_id, limit], |row| {
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

    fn search_like(
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
            task_id: None,
            department: None,
            worktree: None,
            branch: None,
            status: "running".to_string(),
            model: model.to_string(),
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO agents
             (id, session_id, role, name, title, parent_id, worktree, branch, status, model, created_at,
              task_id, department)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                agent.created_at,
                agent.task_id,
                agent.department
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
            task_id: None,
            department: None,
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
             (id, session_id, role, name, title, parent_id, worktree, branch, status, model, created_at,
              task_id, department)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                agent.created_at,
                agent.task_id,
                agent.department
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

    pub fn set_agent_task_with_event(
        &self,
        id: &str,
        task_id: Option<&str>,
        department: Option<&str>,
    ) -> Result<Event> {
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
            "UPDATE agents SET task_id = ?1, department = ?2 WHERE id = ?3",
            params![task_id, department, id],
        )?;
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &session_id,
            "agent.updated",
            serde_json::json!({"agent_id": id, "agent": agent}),
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
            "SELECT id, session_id, role, name, title, profile, parent_id, task_id, department,
                    worktree, branch, status, model, created_at
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
                task_id: row.get(7)?,
                department: row.get(8)?,
                worktree: row.get(9)?,
                branch: row.get(10)?,
                status: row.get(11)?,
                model: row.get(12)?,
                created_at: row.get(13)?,
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
        let current: String = tx
            .query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if !allowed_session_transition(&current, status) {
            return Err(anyhow!(
                "invalid session status transition: {current} -> {status}"
            ));
        }
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

    /// Persist an operator steering request and its creation event in one
    /// transaction. The engine validates the target run before calling this.
    pub fn create_steering_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        body: &str,
    ) -> Result<(SteeringMessage, Event)> {
        let steering = SteeringMessage {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            task_id: task_id.map(str::to_string),
            body: body.to_string(),
            status: "pending".to_string(),
            created_at: now_rfc3339(),
            delivered_at: None,
        };
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
            "INSERT INTO steering_messages
             (id, session_id, agent_id, task_id, body, status, created_at, delivered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![
                steering.id,
                steering.session_id,
                steering.agent_id,
                steering.task_id,
                steering.body,
                steering.status,
                steering.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "steering.created",
            serde_json::to_value(&steering)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((steering, event))
    }

    pub fn list_steering(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<SteeringMessage>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 2_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
             FROM steering_messages
             WHERE session_id = ?1 AND (?2 IS NULL OR status = ?2)
             ORDER BY created_at ASC, id ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![session_id, status, limit], map_steering)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn cancel_steering_with_event(&self, id: &str) -> Result<(SteeringMessage, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let steering = tx
            .query_row(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages WHERE id = ?1",
                params![id],
                map_steering,
            )
            .optional()?
            .ok_or_else(|| anyhow!("steering message not found: {id}"))?;
        if steering.status != "pending" {
            anyhow::bail!("steering message {id} is already {}", steering.status);
        }
        tx.execute(
            "UPDATE steering_messages SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'",
            params![id],
        )?;
        let mut cancelled = steering;
        cancelled.status = "cancelled".to_string();
        let event = insert_event_tx(
            &tx,
            &cancelled.session_id,
            "steering.cancelled",
            serde_json::to_value(&cancelled)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((cancelled, event))
    }

    /// Cancel everything still pending for a run that is ending. This is the
    /// terminal-run cleanup path: an undelivered instruction must not leak to
    /// a later run that happens to reuse the same agent row.
    pub fn cancel_pending_steering_for_agent_with_events(
        &self,
        session_id: &str,
        agent_id: &str,
        reason: &str,
    ) -> Result<Vec<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let pending = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages
                 WHERE session_id = ?1 AND agent_id = ?2 AND status = 'pending'
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map(params![session_id, agent_id], map_steering)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::new();
        for steering in pending {
            tx.execute(
                "UPDATE steering_messages SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'",
                params![steering.id],
            )?;
            let mut cancelled = steering;
            cancelled.status = "cancelled".to_string();
            let mut payload = serde_json::to_value(&cancelled)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert("reason".into(), serde_json::json!(reason));
            }
            events.push(insert_event_tx(
                &tx,
                &cancelled.session_id,
                "steering.cancelled",
                payload,
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(events)
    }

    /// Deliver every pending instruction for the current agent (and task, when
    /// scoped) in creation order. Row update, optional persisted user message,
    /// and both audit events commit together.
    pub fn deliver_pending_steering_with_events(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        persist_messages: bool,
    ) -> Result<(Vec<DeliveredSteering>, Vec<Event>)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let pending = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages
                 WHERE session_id = ?1 AND agent_id = ?2 AND status = 'pending'
                   AND (task_id IS NULL OR task_id = ?3)
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map(params![session_id, agent_id, task_id], map_steering)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut delivered = Vec::new();
        let mut events = Vec::new();
        for steering in pending {
            let delivered_at = now_rfc3339();
            let changed = tx.execute(
                "UPDATE steering_messages
                 SET status = 'delivered', delivered_at = ?1
                 WHERE id = ?2 AND status = 'pending'",
                params![delivered_at, steering.id],
            )?;
            if changed == 0 {
                continue;
            }
            let mut steering = steering;
            steering.status = "delivered".to_string();
            steering.delivered_at = Some(delivered_at);
            let content = steering_content(&steering);
            let message = if persist_messages {
                let message = StoredMessage {
                    id: Uuid::now_v7().to_string(),
                    session_id: session_id.to_string(),
                    role: "user".to_string(),
                    content: content.clone(),
                    tool_calls: serde_json::json!([]),
                    tool_call_id: None,
                    created_at: now_rfc3339(),
                };
                tx.execute(
                    "INSERT INTO messages
                     (id, session_id, role, content, tool_calls, tool_call_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
                    params![
                        message.id,
                        message.session_id,
                        message.role,
                        message.content,
                        serde_json::to_string(&message.tool_calls)?,
                        message.created_at
                    ],
                )?;
                events.push(insert_event_tx(
                    &tx,
                    session_id,
                    "message.created",
                    serde_json::to_value(&message)?,
                )?);
                Some(message)
            } else {
                None
            };
            events.push(insert_event_tx(
                &tx,
                session_id,
                "steering.delivered",
                serde_json::to_value(&steering)?,
            )?);
            delivered.push(DeliveredSteering {
                steering,
                message,
                content,
            });
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((delivered, events))
    }

    /// Replace the steering projection from replayed events. Used by
    /// `session.replay {repair}` and `vp db-rebuild`.
    pub fn replace_steering_messages(
        &self,
        session_id: &str,
        messages: &[SteeringMessage],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM steering_messages WHERE session_id = ?1",
            params![session_id],
        )?;
        for message in messages {
            if message.session_id != session_id {
                return Err(anyhow!("steering message belongs to a different session"));
            }
            tx.execute(
                "INSERT INTO steering_messages
                 (id, session_id, agent_id, task_id, body, status, created_at, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    message.id,
                    message.session_id,
                    message.agent_id,
                    message.task_id,
                    message.body,
                    message.status,
                    message.created_at,
                    message.delivered_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn create_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: &serde_json::Value,
        approval_risk: Option<&str>,
        approval_reason: Option<&str>,
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
            approval_risk: approval_risk.map(str::to_string),
            approval_reason: approval_reason.map(str::to_string),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approvals
             (id, session_id, tool_call_id, tool, input, status, created_at, approval_risk, approval_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8)",
            params![
                approval.id,
                approval.session_id,
                approval.tool_call_id,
                approval.tool,
                serde_json::to_string(&approval.input)?,
                approval.created_at,
                approval.approval_risk,
                approval.approval_reason
            ],
        )?;
        Ok(approval)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_approval_with_event(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: &serde_json::Value,
        agent: Option<&serde_json::Value>,
        approval_risk: Option<&str>,
        approval_reason: Option<&str>,
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
            approval_risk: approval_risk.map(str::to_string),
            approval_reason: approval_reason.map(str::to_string),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO approvals
             (id, session_id, tool_call_id, tool, input, status, created_at, approval_risk, approval_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8)",
            params![
                approval.id,
                approval.session_id,
                approval.tool_call_id,
                approval.tool,
                serde_json::to_string(&approval.input)?,
                approval.created_at,
                approval.approval_risk,
                approval.approval_reason
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
            "SELECT id, session_id, tool_call_id, tool, input, status, created_at, resolved_at,
                    approval_risk, approval_reason
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
                    approval_risk: row.get(8)?,
                    approval_reason: row.get(9)?,
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
            "SELECT id, session_id, tool_call_id, tool, input, status, created_at, resolved_at,
                    approval_risk, approval_reason
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
                    approval_risk: row.get(8)?,
                    approval_reason: row.get(9)?,
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

    /// Persist a plan revision and its event atomically. The payload is
    /// supplied by the orchestration layer so the event carries the revision,
    /// author, reason, and task diff.
    pub fn set_team_plan_revision_with_event(
        &self,
        session_id: &str,
        plan: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Event> {
        if !matches!(kind, "team_plan.submitted" | "team_plan.revised") {
            return Err(anyhow!("invalid team plan event kind: {kind}"));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET team_plan = ?1 WHERE id = ?2",
            params![plan, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(&tx, session_id, kind, payload)?;
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

    /// Rebuild the persistent task-DAG projection from the latest plan and the
    /// plan lifecycle events. This is intentionally idempotent: a plan resume
    /// can repair stale queue rows before it schedules work.
    pub fn sync_plan_tasks(
        &self,
        session_id: &str,
        project_id: &str,
        plan_seq: u64,
        plan: &TeamPlan,
    ) -> Result<Vec<ScheduledTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        struct TaskEventState {
            status: String,
            attempts: u32,
            started_at: Option<String>,
            finished_at: Option<String>,
            last_error: Option<String>,
            next_attempt_at: Option<String>,
        }
        let previous = {
            let mut stmt = tx.prepare(
                "SELECT task_id, plan_seq, enqueued_at FROM plan_tasks
                 WHERE session_id = ?1 ORDER BY plan_seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let plan_enqueued_at = tx
            .query_row(
                "SELECT created_at FROM events WHERE session_id = ?1 AND seq = ?2",
                params![session_id, plan_seq as i64],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| now.clone());
        let mut states = std::collections::HashMap::<String, TaskEventState>::new();
        {
            let mut stmt = tx.prepare(
                "SELECT type, payload, created_at FROM events
                 WHERE session_id = ?1 AND seq > ?2
                   AND type IN ('plan_task.started', 'plan_task.finished',
                                'plan_task.interrupted', 'plan_task.retried',
                                'plan_task.skipped', 'plan_task.cancelling',
                                'plan_task.pausing', 'plan_task.paused', 'plan_task.resumed',
                                'plan_task.retry_scheduled', 'plan_task.blocked',
                                'plan_task.unblocked', 'plan_task.cancelled')
                 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id, plan_seq as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (kind, payload, created_at) = row?;
                let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
                let Some(task_id) = payload["task_id"].as_str() else {
                    continue;
                };
                let state = states.entry(task_id.to_string()).or_insert(TaskEventState {
                    status: "pending".to_string(),
                    attempts: 0,
                    started_at: None,
                    finished_at: None,
                    last_error: None,
                    next_attempt_at: None,
                });
                match kind.as_str() {
                    "plan_task.started" => {
                        if state.status != "completed" {
                            state.status = "interrupted".to_string();
                            state.finished_at = None;
                            state.last_error = None;
                        }
                        state.attempts = state.attempts.saturating_add(1);
                        state.started_at = Some(created_at);
                    }
                    "plan_task.finished" => {
                        let status = payload["task_status"].as_str().unwrap_or("failed");
                        state.status = status.to_string();
                        state.finished_at = Some(created_at);
                        state.next_attempt_at = None;
                        state.last_error = if status == "completed" {
                            None
                        } else {
                            payload["reason"]
                                .as_str()
                                .or_else(|| payload["summary"].as_str())
                                .map(str::to_string)
                        };
                    }
                    "plan_task.retry_scheduled" => {
                        state.status = "pending".to_string();
                        state.finished_at = None;
                        state.next_attempt_at =
                            payload["next_attempt_at"].as_str().map(str::to_string);
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.interrupted" => {
                        state.status = "interrupted".to_string();
                        state.finished_at = Some(created_at);
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.retried" => {
                        state.status = "pending".to_string();
                        state.finished_at = None;
                        state.last_error = None;
                    }
                    "plan_task.cancelling" => {
                        state.status = "cancelling".to_string();
                        state.finished_at = None;
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.pausing" => {
                        state.status = "pausing".to_string();
                        state.finished_at = None;
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.paused" => {
                        state.status = "paused".to_string();
                        state.finished_at = None;
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.resumed" => {
                        state.status = "running".to_string();
                        state.finished_at = None;
                        state.last_error = None;
                    }
                    "plan_task.skipped" => {
                        state.status = "skipped".to_string();
                        state.finished_at = Some(created_at);
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.blocked" => {
                        state.status = "blocked".to_string();
                        state.finished_at = None;
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.unblocked" => {
                        state.status = "pending".to_string();
                        state.finished_at = None;
                        state.last_error = None;
                        state.next_attempt_at = None;
                    }
                    "plan_task.cancelled" => {
                        state.status = "cancelled".to_string();
                        state.finished_at = Some(created_at);
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    _ => {}
                }
            }
        }
        let previous_map = previous
            .into_iter()
            .filter(|(_, previous_seq, _)| *previous_seq == plan_seq as i64)
            .map(|(task_id, _, enqueued_at)| (task_id, enqueued_at))
            .collect::<std::collections::HashMap<_, _>>();
        tx.execute(
            "DELETE FROM plan_tasks WHERE session_id = ?1",
            params![session_id],
        )?;
        // Progress is operational telemetry for the current revision; rows
        // from an older plan revision would never match a live task again.
        tx.execute(
            "DELETE FROM plan_task_progress WHERE session_id = ?1 AND plan_seq <> ?2",
            params![session_id, plan_seq as i64],
        )?;
        for task in &plan.tasks {
            let state = states.get(&task.id);
            let status = state
                .map(|state| state.status.as_str())
                .unwrap_or("pending");
            let attempts = state.map(|state| state.attempts).unwrap_or(0);
            let started_at = state.and_then(|state| state.started_at.as_deref());
            let finished_at = state.and_then(|state| state.finished_at.as_deref());
            let last_error = state.and_then(|state| state.last_error.as_deref());
            let enqueued_at = previous_map
                .get(&task.id)
                .cloned()
                .unwrap_or_else(|| plan_enqueued_at.clone());
            tx.execute(
                "INSERT INTO plan_tasks
                 (session_id, plan_seq, task_id, project_id, department, title, priority,
                  status, deps, write_scope, attempts, last_error, next_attempt_at, enqueued_at,
                  started_at, finished_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    session_id,
                    plan_seq as i64,
                    task.id,
                    project_id,
                    task.department,
                    task.title,
                    task.priority,
                    status,
                    serde_json::to_string(&task.deps)?,
                    serde_json::to_string(&task.write_scope)?,
                    i64::from(attempts),
                    last_error,
                    state.and_then(|state| state.next_attempt_at.as_deref()),
                    enqueued_at,
                    started_at,
                    finished_at,
                    now
                ],
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.list_plan_tasks(Some(project_id), Some(session_id))
    }

    pub fn list_plan_tasks(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<ScheduledTask>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks
             WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR session_id = ?2)
             ORDER BY project_id ASC, session_id ASC, plan_seq ASC, task_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_scheduled_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Operational progress rows, joined against the current plan-task
    /// projection so rows from an older plan revision are not reported.
    pub fn list_plan_task_progress(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<PlanTaskProgress>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT p.session_id, p.project_id, p.plan_seq, p.task_id,
                    p.last_progress_at, p.stall_count, p.last_turn, p.last_tool, p.updated_at
             FROM plan_task_progress p
             JOIN plan_tasks t
               ON t.session_id = p.session_id AND t.plan_seq = p.plan_seq AND t.task_id = p.task_id
             WHERE (?1 IS NULL OR p.project_id = ?1) AND (?2 IS NULL OR p.session_id = ?2)
             ORDER BY p.session_id ASC, p.plan_seq ASC, p.task_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_plan_task_progress)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Record a bounded turn/tool-boundary observation for a task-scoped run.
    /// Returns false when the task is no longer active, so a late tool result
    /// cannot resurrect progress for a finished task.
    pub fn touch_plan_task_progress(
        &self,
        session_id: &str,
        task_id: &str,
        turn: u32,
        tool: Option<&str>,
    ) -> Result<bool> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let active = conn
            .query_row(
                "SELECT plan_seq, project_id FROM plan_tasks
                 WHERE session_id = ?1 AND task_id = ?2
                   AND status IN ('running', 'claimed')
                 ORDER BY plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((plan_seq, project_id)) = active else {
            return Ok(false);
        };
        conn.execute(
            "INSERT INTO plan_task_progress
             (session_id, plan_seq, task_id, project_id, last_progress_at, stall_count,
              last_turn, last_tool, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?5)
             ON CONFLICT(session_id, plan_seq, task_id) DO UPDATE SET
                last_progress_at = excluded.last_progress_at,
                last_turn = excluded.last_turn,
                last_tool = COALESCE(excluded.last_tool, plan_task_progress.last_tool),
                updated_at = excluded.updated_at",
            params![
                session_id,
                plan_seq,
                task_id,
                project_id,
                now,
                i64::from(turn),
                tool
            ],
        )?;
        Ok(true)
    }

    /// Observability-only stall sweep. It finds running/claimed plan tasks
    /// whose last observation (progress, attempt start, or enqueue time) is
    /// older than the threshold, emits one `orchestration.stall` event per
    /// threshold window, increments `stall_count`, and refreshes the
    /// observation timestamp. It never cancels, retries, or edits the task.
    pub fn sweep_task_stalls(&self, threshold: std::time::Duration) -> Result<Vec<TaskStall>> {
        self.sweep_task_stalls_with_events(threshold)
            .map(|(stalls, _)| stalls)
    }

    /// Same sweep, but also returns the inserted `orchestration.stall` events
    /// so the caller can broadcast them to live subscribers.
    pub fn sweep_task_stalls_with_events(
        &self,
        threshold: std::time::Duration,
    ) -> Result<(Vec<TaskStall>, Vec<Event>)> {
        self.sweep_task_stalls_with_events_at(threshold, time::OffsetDateTime::now_utc())
    }

    pub fn sweep_task_stalls_at(
        &self,
        threshold: std::time::Duration,
        now: time::OffsetDateTime,
    ) -> Result<Vec<TaskStall>> {
        self.sweep_task_stalls_with_events_at(threshold, now)
            .map(|(stalls, _)| stalls)
    }

    pub fn sweep_task_stalls_with_events_at(
        &self,
        threshold: std::time::Duration,
        now: time::OffsetDateTime,
    ) -> Result<(Vec<TaskStall>, Vec<Event>)> {
        if threshold.is_zero() {
            return Ok((Vec::new(), Vec::new()));
        }
        let threshold_nanos = threshold.as_nanos().min(i64::MAX as u128) as i64;
        let threshold = time::Duration::nanoseconds(threshold_nanos);
        let cutoff = now - threshold;
        let now_string = now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| now_rfc3339());
        struct Candidate {
            session_id: String,
            project_id: String,
            plan_seq: i64,
            task_id: String,
            department: String,
            title: String,
            status: String,
            attempts: u32,
            observed_at: String,
            stall_count: u32,
            last_turn: Option<u32>,
            last_tool: Option<String>,
        }
        let candidates = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare(
                "SELECT t.session_id, t.project_id, t.plan_seq, t.task_id, t.department,
                        t.title, t.status, t.attempts,
                        COALESCE(p.last_progress_at, t.started_at, t.enqueued_at),
                        COALESCE(p.stall_count, 0), p.last_turn, p.last_tool
                 FROM plan_tasks t
                 LEFT JOIN plan_task_progress p
                   ON p.session_id = t.session_id AND p.plan_seq = t.plan_seq
                  AND p.task_id = t.task_id
                 WHERE t.status IN ('running', 'claimed')
                 ORDER BY t.session_id ASC, t.plan_seq ASC, t.task_id ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(Candidate {
                    session_id: row.get(0)?,
                    project_id: row.get(1)?,
                    plan_seq: row.get(2)?,
                    task_id: row.get(3)?,
                    department: row.get(4)?,
                    title: row.get(5)?,
                    status: row.get(6)?,
                    attempts: row.get::<_, i64>(7)?.max(0) as u32,
                    observed_at: row.get(8)?,
                    stall_count: row.get::<_, i64>(9)?.max(0) as u32,
                    last_turn: row
                        .get::<_, Option<i64>>(10)?
                        .and_then(|value| u32::try_from(value).ok()),
                    last_tool: row.get(11)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut stalls = Vec::new();
        let mut events = Vec::new();
        {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            for candidate in candidates {
                let Ok(observed) = time::OffsetDateTime::parse(
                    &candidate.observed_at,
                    &time::format_description::well_known::Rfc3339,
                ) else {
                    continue;
                };
                if observed > cutoff {
                    continue;
                }
                let still_active: bool = tx
                    .query_row(
                        "SELECT 1 FROM plan_tasks
                         WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3
                           AND status IN ('running', 'claimed')",
                        params![candidate.session_id, candidate.plan_seq, candidate.task_id],
                        |_| Ok(true),
                    )
                    .optional()?
                    .unwrap_or(false);
                if !still_active {
                    continue;
                }
                let age_secs = (now - observed).whole_seconds().max(0) as u64;
                let stall_count = candidate.stall_count.saturating_add(1);
                tx.execute(
                    "INSERT INTO plan_task_progress
                     (session_id, plan_seq, task_id, project_id, last_progress_at, stall_count,
                      last_turn, last_tool, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?5)
                     ON CONFLICT(session_id, plan_seq, task_id) DO UPDATE SET
                        last_progress_at = excluded.last_progress_at,
                        stall_count = excluded.stall_count,
                        last_turn = COALESCE(excluded.last_turn, plan_task_progress.last_turn),
                        last_tool = COALESCE(excluded.last_tool, plan_task_progress.last_tool),
                        updated_at = excluded.updated_at",
                    params![
                        candidate.session_id,
                        candidate.plan_seq,
                        candidate.task_id,
                        candidate.project_id,
                        now_string,
                        i64::from(stall_count),
                        candidate.last_turn.map(i64::from),
                        candidate.last_tool
                    ],
                )?;
                let stall = TaskStall {
                    session_id: candidate.session_id.clone(),
                    project_id: candidate.project_id,
                    plan_seq: candidate.plan_seq.max(0) as u64,
                    task_id: candidate.task_id,
                    department: candidate.department,
                    title: candidate.title,
                    status: candidate.status,
                    attempts: candidate.attempts,
                    age_secs,
                    stall_count,
                    last_progress_at: candidate.observed_at,
                    last_turn: candidate.last_turn,
                    last_tool: candidate.last_tool,
                    observed_at: now_string.clone(),
                };
                events.push(insert_event_tx(
                    &tx,
                    &stall.session_id,
                    "orchestration.stall",
                    serde_json::to_value(&stall)?,
                )?);
                stalls.push(stall);
            }
            tx.commit()?;
        }
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((stalls, events))
    }

    /// Sessions with retryable DAG tasks that are idle/failed/cancelled and
    /// therefore safe for the opt-in global resume scheduler to pick up.
    pub fn pending_task_sessions(
        &self,
        global_enabled: bool,
        default_cooldown_secs: u64,
    ) -> Result<Vec<PendingTaskSessionRow>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT t.session_id, t.project_id, s.priority, COUNT(*), MIN(t.enqueued_at),
                    MAX(t.priority), COALESCE(p.cooldown_secs, ?2)
             FROM plan_tasks t
             JOIN sessions s ON s.id = t.session_id
             LEFT JOIN project_task_scheduler_policies p ON p.project_id = t.project_id
             WHERE t.status IN ('pending', 'interrupted', 'failed', 'cancelled')
               AND s.status IN ('idle', 'failed', 'cancelled')
               AND (t.next_attempt_at IS NULL OR t.next_attempt_at <= ?3)
               AND COALESCE(p.enabled, ?1) = 1
             GROUP BY t.session_id, t.project_id, s.priority, p.cooldown_secs
             ORDER BY s.priority DESC, MIN(t.enqueued_at) ASC, t.session_id ASC",
        )?;
        let rows = stmt.query_map(
            params![
                if global_enabled { 1 } else { 0 },
                default_cooldown_secs as i64,
                now_rfc3339()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i64>(3)?.max(0) as usize,
                    row.get::<_, String>(4)?,
                    row.get::<_, i32>(5)?,
                    row.get::<_, i64>(6)?.max(1) as u64,
                ))
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn claim_plan_tasks(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_ids: &[String],
    ) -> Result<Vec<ScheduledTask>> {
        let (tasks, _events) = self.claim_plan_tasks_with_events(session_id, plan_seq, task_ids)?;
        Ok(tasks)
    }

    /// Claim tasks and append the `plan_task.started` events in the same
    /// transaction. A claim without its event would leave attempts/status
    /// ahead of the event log and force a replay repair after a crash.
    pub fn claim_plan_tasks_with_events(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_ids: &[String],
    ) -> Result<(Vec<ScheduledTask>, Vec<Event>)> {
        let now = now_rfc3339();
        let (claimed, events) = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let mut claimed = Vec::new();
            let mut events = Vec::new();
            for task_id in task_ids {
                let changed = tx.execute(
                    "UPDATE plan_tasks
                     SET status = 'running', attempts = attempts + 1, started_at = ?1,
                         finished_at = NULL, last_error = NULL, next_attempt_at = NULL,
                         updated_at = ?1
                     WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4
                       AND status IN ('pending', 'interrupted', 'failed', 'blocked', 'cancelled')
                       AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)",
                    params![now, session_id, plan_seq as i64, task_id],
                )?;
                if changed == 0 {
                    continue;
                }
                tx.execute(
                    "INSERT INTO plan_task_progress
                     (session_id, plan_seq, task_id, project_id, last_progress_at, stall_count,
                      last_turn, last_tool, updated_at)
                     SELECT session_id, plan_seq, task_id, project_id, ?1, 0, NULL, NULL, ?1
                     FROM plan_tasks
                     WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4
                     ON CONFLICT(session_id, plan_seq, task_id) DO UPDATE SET
                        last_progress_at = excluded.last_progress_at,
                        last_turn = NULL,
                        last_tool = NULL,
                        updated_at = excluded.updated_at",
                    params![now, session_id, plan_seq as i64, task_id],
                )?;
                let task = tx.query_row(
                    "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                            status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                            last_error, next_attempt_at
                     FROM plan_tasks
                     WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
                    params![session_id, plan_seq as i64, task_id],
                    map_scheduled_task,
                )?;
                events.push(insert_event_tx(
                    &tx,
                    session_id,
                    "plan_task.started",
                    serde_json::json!({
                        "task_id": task.task_id,
                        "task_status": "running",
                        "department": task.department,
                        "title": task.title,
                        "attempts": task.attempts,
                    }),
                )?);
                claimed.push(task);
            }
            tx.commit()?;
            (claimed, events)
        };
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((claimed, events))
    }

    pub fn finish_plan_task(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<Option<ScheduledTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE plan_tasks
             SET status = ?1, last_error = ?2, finished_at = ?3, next_attempt_at = NULL,
                 updated_at = ?3
             WHERE session_id = ?4 AND plan_seq = ?5 AND task_id = ?6",
            params![status, detail, now, session_id, plan_seq as i64, task_id],
        )?;
        let task = tx
            .query_row(
                "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                        status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                        last_error, next_attempt_at
                 FROM plan_tasks
                 WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
                params![session_id, plan_seq as i64, task_id],
                map_scheduled_task,
            )
            .optional()?;
        tx.commit()?;
        Ok(task)
    }

    /// Schedule an automatic retry: the task returns to pending with a
    /// next_attempt_at gate and an event that replay/repair can reconstruct.
    pub fn schedule_plan_task_retry_with_event(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        next_attempt_at: &str,
        reason: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = 'pending', finished_at = NULL, last_error = ?1,
                 next_attempt_at = ?2, updated_at = ?3
             WHERE session_id = ?4 AND plan_seq = ?5 AND task_id = ?6
               AND status IN ('failed', 'blocked', 'interrupted', 'cancelled')",
            params![
                reason,
                next_attempt_at,
                now,
                session_id,
                plan_seq as i64,
                task_id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.retry_scheduled",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "pending",
                "attempt": task.attempts,
                "next_attempt_at": next_attempt_at,
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    /// Move a task into `blocked` because an upstream dependency failed
    /// permanently. Returns `None` when the task has already moved on.
    pub fn block_plan_task_with_event(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
        dependency: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = 'blocked', last_error = ?1, finished_at = NULL,
                 next_attempt_at = NULL, updated_at = ?2
             WHERE session_id = ?3 AND plan_seq = ?4 AND task_id = ?5
               AND status IN ('pending', 'interrupted', 'retry_scheduled')",
            params![reason, now, session_id, plan_seq as i64, task_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq as i64, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.blocked",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "blocked",
                "dependency": dependency,
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    /// Return a blocked task to pending once all dependencies are satisfied.
    pub fn unblock_plan_task_with_event(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = 'pending', last_error = NULL, finished_at = NULL,
                 next_attempt_at = NULL, updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4
               AND status = 'blocked'",
            params![now, session_id, plan_seq as i64, task_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq as i64, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.unblocked",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "pending",
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    /// Cancel a pending/blocked task, used by the dependency-failure policy.
    pub fn cancel_plan_task_with_event(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
        dependency: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = 'cancelled', last_error = ?1, finished_at = ?2,
                 next_attempt_at = NULL, updated_at = ?2
             WHERE session_id = ?3 AND plan_seq = ?4 AND task_id = ?5
               AND status IN ('pending', 'blocked', 'interrupted', 'failed', 'retry_scheduled')",
            params![reason, now, session_id, plan_seq as i64, task_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq as i64, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.cancelled",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "cancelled",
                "dependency": dependency,
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    /// Retry a terminal/blocked task after the session is no longer active.
    /// The task returns to `pending`; the next plan resume or global scheduler
    /// tick can claim it. The projection update and event are atomic.
    pub fn retry_plan_task_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status, session_status) = tx
            .query_row(
                "SELECT t.plan_seq, t.status, s.status
                 FROM plan_tasks t JOIN sessions s ON s.id = t.session_id
                 WHERE t.session_id = ?1 AND t.task_id = ?2
                 ORDER BY t.plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if !matches!(
            session_status.as_str(),
            "idle" | "failed" | "cancelled" | "draft"
        ) {
            return Err(anyhow!(
                "session {session_id} is {session_status}; stop it before retrying a task"
            ));
        }
        if !matches!(
            status.as_str(),
            "failed" | "blocked" | "interrupted" | "cancelled" | "skipped"
        ) {
            return Err(anyhow!("task {task_id} is {status}, not retryable"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'pending', finished_at = NULL, last_error = NULL,
                 next_attempt_at = NULL, updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.retried",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "pending",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    /// Request a cooperative pause of a running plan task. The task enters
    /// `pausing`; the worker transitions it to `paused` at its next boundary.
    pub fn pause_plan_task_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status, session_status) = tx
            .query_row(
                "SELECT t.plan_seq, t.status, s.status
                 FROM plan_tasks t JOIN sessions s ON s.id = t.session_id
                 WHERE t.session_id = ?1 AND t.task_id = ?2
                 ORDER BY t.plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if session_status != "running" {
            return Err(anyhow!(
                "session {session_id} is {session_status}; pause is only available while running"
            ));
        }
        if status != "running" {
            return Err(anyhow!("task {task_id} is {status}, not running"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'pausing', last_error = 'pause requested by user', updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.pausing",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "pausing",
                "reason": "pause requested by user",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    pub fn mark_plan_task_paused_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status) = tx
            .query_row(
                "SELECT plan_seq, status FROM plan_tasks
                 WHERE session_id = ?1 AND task_id = ?2
                 ORDER BY plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if status != "pausing" {
            return Ok(None);
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'paused', last_error = 'paused by user', updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.paused",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "paused",
                "reason": "paused by user",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    pub fn resume_plan_task_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status, session_status) = tx
            .query_row(
                "SELECT t.plan_seq, t.status, s.status
                 FROM plan_tasks t JOIN sessions s ON s.id = t.session_id
                 WHERE t.session_id = ?1 AND t.task_id = ?2
                 ORDER BY t.plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if !matches!(session_status.as_str(), "running" | "paused") {
            return Err(anyhow!(
                "session {session_id} is {session_status}; resume is only available while active"
            ));
        }
        if !matches!(status.as_str(), "pausing" | "paused") {
            return Err(anyhow!("task {task_id} is {status}, not paused"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'running', last_error = NULL, updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.resumed",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "running",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    /// Request cooperative cancellation of a running plan task. The worker
    /// checks this status at turn/tool boundaries and exits without aborting
    /// sibling tasks.
    pub fn request_plan_task_cancel_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status) = tx
            .query_row(
                "SELECT plan_seq, status FROM plan_tasks
                 WHERE session_id = ?1 AND task_id = ?2
                 ORDER BY plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if !matches!(status.as_str(), "running" | "pausing" | "paused") {
            return Err(anyhow!("task {task_id} is {status}, not cancellable"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'cancelling', last_error = 'cancel requested by user', updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.cancelling",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "cancelling",
                "reason": "cancel requested by user",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    pub fn plan_task_status(&self, session_id: &str, task_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT status FROM plan_tasks
             WHERE session_id = ?1 AND task_id = ?2
             ORDER BY plan_seq DESC LIMIT 1",
            params![session_id, task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Skip a queued/blocked task for the current plan revision. A skipped task
    /// is terminal for scheduling and is not picked up by the global scheduler;
    /// downstream dependencies remain blocked rather than silently running.
    pub fn skip_plan_task_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status, session_status) = tx
            .query_row(
                "SELECT t.plan_seq, t.status, s.status
                 FROM plan_tasks t JOIN sessions s ON s.id = t.session_id
                 WHERE t.session_id = ?1 AND t.task_id = ?2
                 ORDER BY t.plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if !matches!(
            session_status.as_str(),
            "idle" | "failed" | "cancelled" | "draft"
        ) {
            return Err(anyhow!(
                "session {session_id} is {session_status}; stop it before skipping a task"
            ));
        }
        if !matches!(
            status.as_str(),
            "pending" | "failed" | "blocked" | "interrupted" | "cancelled"
        ) {
            return Err(anyhow!("task {task_id} is {status}, not skippable"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'skipped', last_error = 'skipped by user',
                 finished_at = ?1, updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.skipped",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "skipped",
                "reason": "skipped by user",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    /// Mark in-flight task rows as interrupted after a daemon restart. Returns
    /// the rows that changed so the caller can append audit events.
    pub fn interrupt_plan_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let interrupted = {
            let mut stmt = tx.prepare(
                "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                        status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                        last_error, next_attempt_at
                 FROM plan_tasks WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
                 ORDER BY session_id ASC, plan_seq ASC, task_id ASC",
            )?;
            let rows = stmt.query_map([], map_scheduled_task)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'interrupted',
                 last_error = 'daemon restarted while the task was active',
                 finished_at = ?1, updated_at = ?1
             WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')",
            params![now],
        )?;
        tx.execute("DELETE FROM write_leases", [])?;
        tx.commit()?;
        Ok(interrupted)
    }

    /// Clear task rows left running by an aborted plan run. Recovery uses the
    /// global variant; this one is called when a session reaches a terminal
    /// state during normal operation.
    pub fn interrupt_session_plan_tasks(
        &self,
        session_id: &str,
        reason: &str,
    ) -> Result<Vec<ScheduledTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let interrupted = {
            let mut stmt = tx.prepare(
                "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                        status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                        last_error, next_attempt_at
                 FROM plan_tasks
                 WHERE session_id = ?1 AND status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
                 ORDER BY plan_seq ASC, task_id ASC",
            )?;
            let rows = stmt.query_map(params![session_id], map_scheduled_task)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'interrupted', last_error = ?1, finished_at = ?2, updated_at = ?2
             WHERE session_id = ?3 AND status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')",
            params![reason, now, session_id],
        )?;
        tx.commit()?;
        Ok(interrupted)
    }

    /// Sessions whose plan has failed or blocked rows that the deterministic
    /// controller may be able to resolve. This is a bounded scan for the
    /// daemon tick, not a scheduler priority queue.
    pub fn orchestration_candidate_sessions(&self, limit: u32) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT s.id
             FROM sessions s
             JOIN plan_tasks t ON t.session_id = s.id
             WHERE s.team_plan IS NOT NULL
               AND t.status IN ('failed', 'blocked')
             ORDER BY s.created_at ASC, s.id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![i64::from(limit.clamp(1, 500))], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Acquire or refresh a daemon-wide runtime lease. Leases are SQLite-backed
    /// so two processes pointed at the same data directory cannot run the same
    /// session or integration step concurrently. Expired leases are removable
    /// without operator action.
    pub fn try_acquire_runtime_lease(
        &self,
        lease_key: &str,
        owner_id: &str,
        kind: &str,
        session_id: Option<&str>,
        ttl_secs: u64,
    ) -> Result<bool> {
        let now = time::OffsetDateTime::now_utc();
        let now_text = now
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting lease time")?;
        let expires = now + time::Duration::seconds(ttl_secs.min(86_400) as i64);
        let expires_text = expires
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting lease expiry")?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM runtime_leases WHERE expires_at <= ?1",
            params![now_text],
        )?;
        let existing = tx
            .query_row(
                "SELECT owner_id FROM runtime_leases WHERE lease_key = ?1",
                params![lease_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match existing {
            Some(owner) if owner != owner_id => {
                return Ok(false);
            }
            Some(_) => {
                tx.execute(
                    "UPDATE runtime_leases
                     SET heartbeat_at = ?1, expires_at = ?2, kind = ?3, session_id = ?4
                     WHERE lease_key = ?5",
                    params![now_text, expires_text, kind, session_id, lease_key],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO runtime_leases
                     (lease_key, owner_id, kind, session_id, acquired_at, heartbeat_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
                    params![
                        lease_key,
                        owner_id,
                        kind,
                        session_id,
                        now_text,
                        expires_text
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn renew_runtime_lease(
        &self,
        lease_key: &str,
        owner_id: &str,
        ttl_secs: u64,
    ) -> Result<bool> {
        let now = time::OffsetDateTime::now_utc();
        let now_text = now
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting lease time")?;
        let expires = now + time::Duration::seconds(ttl_secs.min(86_400) as i64);
        let expires_text = expires
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting lease expiry")?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE runtime_leases
             SET heartbeat_at = ?1, expires_at = ?2
             WHERE lease_key = ?3 AND owner_id = ?4",
            params![now_text, expires_text, lease_key, owner_id],
        )?;
        Ok(changed > 0)
    }

    pub fn release_runtime_lease(&self, lease_key: &str, owner_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "DELETE FROM runtime_leases WHERE lease_key = ?1 AND owner_id = ?2",
            params![lease_key, owner_id],
        )?;
        Ok(changed > 0)
    }

    pub fn runtime_lease_owner(&self, lease_key: &str) -> Result<Option<RuntimeLease>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT lease_key, owner_id, kind, session_id, acquired_at, heartbeat_at, expires_at
             FROM runtime_leases WHERE lease_key = ?1",
            params![lease_key],
            map_runtime_lease,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_runtime_leases(&self) -> Result<Vec<RuntimeLease>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT lease_key, owner_id, kind, session_id, acquired_at, heartbeat_at, expires_at
             FROM runtime_leases ORDER BY acquired_at ASC, lease_key ASC",
        )?;
        let rows = stmt.query_map([], map_runtime_lease)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn clear_expired_runtime_leases(&self) -> Result<usize> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let changed = conn.execute(
            "DELETE FROM runtime_leases WHERE expires_at <= ?1",
            params![now],
        )?;
        Ok(changed)
    }

    /// Acquire the daemon-wide lease for an interactive surface. Expired
    /// leases are removed and audited in the same transaction; an unexpired
    /// lease on the same surface returns the current holder without changing
    /// it. The caller must hold and renew the returned lease ID.
    pub fn acquire_computer_lease(
        &self,
        surface: &str,
        owner_id: &str,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        ttl_secs: u64,
    ) -> Result<ComputerLeaseAttempt> {
        let surface = surface.trim();
        if surface.is_empty() || surface.len() > 64 {
            anyhow::bail!("computer lease surface must be between 1 and 64 characters");
        }
        if owner_id.trim().is_empty() || agent_id.trim().is_empty() {
            anyhow::bail!("computer lease owner and agent are required");
        }
        let now = time::OffsetDateTime::now_utc();
        let now_text = now
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting computer lease time")?;
        let expires = now + time::Duration::seconds(ttl_secs.min(86_400) as i64);
        let expires_text = expires
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting computer lease expiry")?;
        let mut events = Vec::new();
        let outcome = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let expired = {
                let mut stmt = tx.prepare(
                    "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                            acquired_at, heartbeat_at, expires_at
                     FROM computer_leases WHERE expires_at <= ?1
                     ORDER BY expires_at ASC, lease_id ASC",
                )?;
                let rows = stmt.query_map(params![now_text], map_computer_lease)?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            for lease in &expired {
                tx.execute(
                    "DELETE FROM computer_leases WHERE lease_id = ?1",
                    params![&lease.lease_id],
                )?;
                events.push(insert_event_tx(
                    &tx,
                    &lease.session_id,
                    "computer.lease.expired",
                    serde_json::json!({
                        "lease": lease,
                        "reason": "expired before a new acquisition",
                    }),
                )?);
            }
            let holder = tx
                .query_row(
                    "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                            acquired_at, heartbeat_at, expires_at
                     FROM computer_leases WHERE surface = ?1",
                    params![surface],
                    map_computer_lease,
                )
                .optional()?;
            if let Some(holder) = holder {
                events.push(insert_event_tx(
                    &tx,
                    session_id,
                    "computer.lease.denied",
                    serde_json::json!({
                        "surface": surface,
                        "holder": holder,
                        "owner_id": owner_id,
                    }),
                )?);
                tx.commit()?;
                ComputerLeaseAttempt::Busy {
                    holder: Some(holder),
                    events,
                }
            } else {
                let lease = ComputerLease {
                    lease_id: Uuid::now_v7().to_string(),
                    surface: surface.to_string(),
                    owner_id: owner_id.to_string(),
                    session_id: session_id.to_string(),
                    agent_id: agent_id.to_string(),
                    agent_name: agent_name.to_string(),
                    acquired_at: now_text.clone(),
                    heartbeat_at: now_text.clone(),
                    expires_at: expires_text,
                };
                tx.execute(
                    "INSERT INTO computer_leases
                     (lease_id, surface, owner_id, session_id, agent_id, agent_name,
                      acquired_at, heartbeat_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        &lease.lease_id,
                        &lease.surface,
                        &lease.owner_id,
                        &lease.session_id,
                        &lease.agent_id,
                        &lease.agent_name,
                        &lease.acquired_at,
                        &lease.heartbeat_at,
                        &lease.expires_at,
                    ],
                )?;
                events.push(insert_event_tx(
                    &tx,
                    &lease.session_id,
                    "computer.lease.acquired",
                    serde_json::to_value(&lease)?,
                )?);
                tx.commit()?;
                ComputerLeaseAttempt::Acquired { lease, events }
            }
        };
        let mirror_events = match &outcome {
            ComputerLeaseAttempt::Acquired { events, .. }
            | ComputerLeaseAttempt::Busy { events, .. } => events.clone(),
        };
        for event in &mirror_events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append computer lease event mirror");
            }
        }
        Ok(outcome)
    }

    pub fn renew_computer_lease(
        &self,
        lease_id: &str,
        owner_id: &str,
        ttl_secs: u64,
    ) -> Result<bool> {
        let now = time::OffsetDateTime::now_utc();
        let now_text = now
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting computer lease time")?;
        let expires = now + time::Duration::seconds(ttl_secs.min(86_400) as i64);
        let expires_text = expires
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting computer lease expiry")?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE computer_leases
             SET heartbeat_at = ?1, expires_at = ?2
             WHERE lease_id = ?3 AND owner_id = ?4",
            params![now_text, expires_text, lease_id, owner_id],
        )?;
        Ok(changed > 0)
    }

    /// Release a computer lease. `owner_id` is enforced for runtime guards;
    /// `None` lets an operator clear a stuck lease through the CLI/RPC.
    pub fn release_computer_lease(
        &self,
        lease_id: &str,
        owner_id: Option<&str>,
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let lease = tx
            .query_row(
                "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                        acquired_at, heartbeat_at, expires_at
                 FROM computer_leases WHERE lease_id = ?1",
                params![lease_id],
                map_computer_lease,
            )
            .optional()?;
        let Some(lease) = lease else {
            tx.commit()?;
            return Ok(None);
        };
        if let Some(owner_id) = owner_id
            && lease.owner_id != owner_id
        {
            tx.commit()?;
            return Ok(None);
        }
        tx.execute(
            "DELETE FROM computer_leases WHERE lease_id = ?1",
            params![lease_id],
        )?;
        let event = insert_event_tx(
            &tx,
            &lease.session_id,
            "computer.lease.released",
            serde_json::json!({
                "lease": lease,
                "reason": if owner_id.is_some() { "released by owner" } else { "released by operator" },
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append computer lease release mirror");
        }
        Ok(Some(event))
    }

    pub fn list_computer_leases(&self) -> Result<Vec<ComputerLease>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                    acquired_at, heartbeat_at, expires_at
             FROM computer_leases ORDER BY surface ASC, acquired_at ASC",
        )?;
        let rows = stmt.query_map([], map_computer_lease)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn computer_lease(&self, lease_id: &str) -> Result<Option<ComputerLease>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                    acquired_at, heartbeat_at, expires_at
             FROM computer_leases WHERE lease_id = ?1",
            params![lease_id],
            map_computer_lease,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Remove expired computer leases and return their audit events. The
    /// engine broadcasts the events after the transaction commits.
    pub fn clear_expired_computer_leases(&self) -> Result<(usize, Vec<Event>)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let expired = {
            let mut stmt = tx.prepare(
                "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                        acquired_at, heartbeat_at, expires_at
                 FROM computer_leases WHERE expires_at <= ?1
                 ORDER BY expires_at ASC, lease_id ASC",
            )?;
            let rows = stmt.query_map(params![now], map_computer_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::new();
        for lease in &expired {
            tx.execute(
                "DELETE FROM computer_leases WHERE lease_id = ?1",
                params![&lease.lease_id],
            )?;
            events.push(insert_event_tx(
                &tx,
                &lease.session_id,
                "computer.lease.expired",
                serde_json::json!({
                    "lease": lease,
                    "reason": "expired during lease sweep",
                }),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append computer lease expiry mirror");
            }
        }
        Ok((expired.len(), events))
    }

    pub fn try_acquire_write_lease(
        &self,
        project_id: &str,
        session_id: &str,
        agent_id: &str,
        scopes: &[String],
    ) -> Result<bool> {
        let scopes = scopes
            .iter()
            .map(|scope| normalize_write_scope(scope))
            .collect::<Vec<_>>();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let conflicts = {
            let mut stmt = tx.prepare(
                "SELECT session_id, scopes FROM write_leases
                 WHERE project_id = ?1 AND agent_id <> ?2",
            )?;
            let rows = stmt.query_map(params![project_id, agent_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut conflicts = false;
            for row in rows {
                let (owner_session, stored) = row?;
                if owner_session == session_id {
                    continue;
                }
                let stored = serde_json::from_str::<Vec<String>>(&stored).unwrap_or_default();
                if write_scopes_overlap(&scopes, &stored) {
                    conflicts = true;
                    break;
                }
            }
            conflicts
        };
        if conflicts {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO write_leases (agent_id, project_id, session_id, scopes, acquired_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(agent_id) DO UPDATE SET
                project_id = excluded.project_id,
                session_id = excluded.session_id,
                scopes = excluded.scopes,
                acquired_at = excluded.acquired_at",
            params![
                agent_id,
                project_id,
                session_id,
                serde_json::to_string(&scopes)?,
                now_rfc3339()
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn release_write_lease(&self, agent_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM write_leases WHERE agent_id = ?1",
            params![agent_id],
        )?;
        Ok(())
    }

    pub fn list_write_leases(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<WriteLease>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, session_id, agent_id, scopes, acquired_at
             FROM write_leases
             WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR session_id = ?2)
             ORDER BY project_id ASC, session_id ASC, agent_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], |row| {
            let scopes = row.get::<_, String>(3)?;
            Ok(WriteLease {
                project_id: row.get(0)?,
                session_id: row.get(1)?,
                agent_id: row.get(2)?,
                scopes: serde_json::from_str(&scopes).unwrap_or_default(),
                acquired_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
            let mut stmt = tx.prepare(
                "SELECT id FROM sessions
                 WHERE status IN ('running', 'paused', 'waiting_input')",
            )?;
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
                     WHERE status IN ('running', 'waiting_input', 'queued')",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE agents SET status = 'failed'
             WHERE status IN ('running', 'waiting_input', 'queued')",
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
        let interrupted_tasks = {
            let mut stmt = tx.prepare(
                "SELECT session_id, task_id FROM plan_tasks
                 WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
                 ORDER BY session_id ASC, plan_seq ASC, task_id ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let interrupted_steering = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages WHERE status = 'pending'
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map([], map_steering)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE steering_messages SET status = 'cancelled' WHERE status = 'pending'",
            [],
        )?;
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'interrupted',
                 last_error = 'daemon restarted while the task was active',
                 finished_at = ?1, updated_at = ?1
             WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')",
            params![now],
        )?;
        tx.execute(
            "UPDATE intake_tasks
             SET status = 'failed',
                 last_error = 'daemon restarted while the intake task was active',
                 next_attempt_at = NULL, updated_at = ?1
             WHERE status IN ('dispatching', 'running')",
            params![now],
        )?;
        tx.execute("DELETE FROM write_leases", [])?;
        tx.execute("DELETE FROM runtime_leases", [])?;
        let interrupted_computer_leases = {
            let mut stmt = tx.prepare(
                "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                        acquired_at, heartbeat_at, expires_at
                 FROM computer_leases ORDER BY acquired_at ASC, lease_id ASC",
            )?;
            let rows = stmt.query_map([], map_computer_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute("DELETE FROM computer_leases", [])?;

        let mut events = Vec::new();
        for lease in &interrupted_computer_leases {
            events.push(insert_event_tx(
                &tx,
                &lease.session_id,
                "computer.lease.expired",
                serde_json::json!({
                    "lease": lease,
                    "reason": "daemon restarted while the computer surface was leased",
                }),
            )?);
        }
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
            let question = tx.query_row(
                "SELECT id, session_id, agent_id, agent_name, question, options, status,
                        answer, created_at, answered_at FROM questions WHERE id = ?1",
                params![question_id],
                map_question,
            )?;
            events.push(insert_event_tx(
                &tx,
                session_id,
                "question.expired",
                serde_json::to_value(&question)?,
            )?);
        }
        for (agent_id, session_id) in &agents {
            let agent = get_agent_tx(&tx, agent_id)?;
            events.push(insert_event_tx(
                &tx,
                session_id,
                "agent.updated",
                serde_json::json!({"agent": agent}),
            )?);
        }
        for (session_id, task_id) in &interrupted_tasks {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "plan_task.interrupted",
                serde_json::json!({
                    "task_id": task_id,
                    "task_status": "interrupted",
                    "reason": "daemon restarted while the task was active",
                }),
            )?);
        }
        for steering in &interrupted_steering {
            let mut cancelled = steering.clone();
            cancelled.status = "cancelled".to_string();
            let mut payload = serde_json::to_value(&cancelled)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "reason".into(),
                    serde_json::json!("daemon restarted before delivery"),
                );
            }
            events.push(insert_event_tx(
                &tx,
                &cancelled.session_id,
                "steering.cancelled",
                payload,
            )?);
        }
        for (approval_id, session_id) in &approvals {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "approval.resolved",
                serde_json::json!({
                    "approval_id": approval_id,
                    "approved": false,
                    "resolved_at": now,
                }),
            )?);
        }
        for id in &queue_ids {
            let entry = tx.query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
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
             SET status = ?1, priority = COALESCE(?2, priority),
                 tokens_in = ?3, tokens_out = ?4, cost_spent_usd = ?5,
                 unlimited = COALESCE(?7, unlimited)
             WHERE id = ?6",
            params![
                repair.status,
                repair.priority,
                repair.tokens_in as i64,
                repair.tokens_out as i64,
                repair.cost_spent_usd,
                session_id,
                repair.unlimited.map(|value| if value { 1 } else { 0 })
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
                     (id, session_id, role, name, title, profile, parent_id, worktree, branch, status, model, created_at,
                      task_id, department)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                        agent.created_at,
                        agent.task_id,
                        agent.department
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
                     (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, ?12, ?13)",
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
        if let Some(tasks) = &repair.tasks {
            tx.execute(
                "DELETE FROM plan_tasks WHERE session_id = ?1",
                params![session_id],
            )?;
            for task in tasks {
                tx.execute(
                    "INSERT INTO plan_tasks
                     (session_id, plan_seq, task_id, project_id, department, title, priority,
                      status, deps, write_scope, attempts, last_error, enqueued_at, started_at,
                      finished_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                    params![
                        task.session_id,
                        task.plan_seq as i64,
                        task.task_id,
                        task.project_id,
                        task.department,
                        task.title,
                        task.priority,
                        task.status,
                        serde_json::to_string(&task.deps)?,
                        serde_json::to_string(&task.write_scope)?,
                        i64::from(task.attempts),
                        task.last_error,
                        task.enqueued_at,
                        task.started_at,
                        task.finished_at,
                        now
                    ],
                )?;
            }
        }
        if let Some(steering) = &repair.steering {
            tx.execute(
                "DELETE FROM steering_messages WHERE session_id = ?1",
                params![session_id],
            )?;
            for message in steering {
                tx.execute(
                    "INSERT INTO steering_messages
                     (id, session_id, agent_id, task_id, body, status, created_at, delivered_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        message.id,
                        message.session_id,
                        message.agent_id,
                        message.task_id,
                        message.body,
                        message.status,
                        message.created_at,
                        message.delivered_at
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
                     (id, session_id, tool_call_id, tool, input, status, created_at, resolved_at,
                      approval_risk, approval_reason)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        approval.id,
                        approval.session_id,
                        approval.tool_call_id,
                        approval.tool,
                        serde_json::to_string(&approval.input)?,
                        approval.status,
                        approval.created_at,
                        approval.resolved_at,
                        approval.approval_risk,
                        approval.approval_reason
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn import_session_bundle(&self, bundle: &SessionBundle) -> Result<Event> {
        validate_session_bundle(bundle)?;
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
             (id, project_id, goal, status, priority, worktree, branch, token_budget,
              cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit,
              team_plan, tokens_in, tokens_out, created_at)
             VALUES (?1, ?2, ?3, 'imported', ?4, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                bundle.session.id,
                bundle.session.project_id,
                bundle.session.goal,
                bundle.session.priority,
                bundle.session.branch,
                bundle.session.token_budget,
                bundle.session.cost_budget_usd,
                bundle.session.time_budget_secs,
                if bundle.session.unlimited { 1 } else { 0 },
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
                 (id, session_id, role, name, title, profile, parent_id, worktree, branch, status, model, created_at,
                  task_id, department)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                    agent.created_at,
                    agent.task_id,
                    agent.department
                ],
            )?;
        }
        for task in &bundle.tasks {
            if task.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle task {} belongs to a different session",
                    task.task_id
                ));
            }
            if task.project_id != bundle.project.id {
                return Err(anyhow!(
                    "bundle task {} belongs to a different project",
                    task.task_id
                ));
            }
            tx.execute(
                "INSERT INTO plan_tasks
                 (session_id, plan_seq, task_id, project_id, department, title, priority,
                  status, deps, write_scope, attempts, last_error, enqueued_at, started_at,
                  finished_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    task.session_id,
                    task.plan_seq as i64,
                    task.task_id,
                    task.project_id,
                    task.department,
                    task.title,
                    task.priority,
                    task.status,
                    serde_json::to_string(&task.deps)?,
                    serde_json::to_string(&task.write_scope)?,
                    i64::from(task.attempts),
                    task.last_error,
                    task.enqueued_at,
                    task.started_at,
                    task.finished_at,
                    bundle.exported_at
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
        for steering in &bundle.steering {
            if steering.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle steering {} belongs to a different session",
                    steering.id
                ));
            }
            tx.execute(
                "INSERT INTO steering_messages
                 (id, session_id, agent_id, task_id, body, status, created_at, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    steering.id,
                    steering.session_id,
                    steering.agent_id,
                    steering.task_id,
                    steering.body,
                    steering.status,
                    steering.created_at,
                    steering.delivered_at
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
                 (id, session_id, tool_call_id, tool, input, status, created_at, resolved_at,
                  approval_risk, approval_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    approval.id,
                    approval.session_id,
                    approval.tool_call_id,
                    approval.tool,
                    serde_json::to_string(&approval.input)?,
                    approval.status,
                    approval.created_at,
                    approval.resolved_at,
                    approval.approval_risk,
                    approval.approval_reason
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
                 (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, ?12, ?13)",
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

    pub fn queued_session_prompts(&self) -> Result<Vec<(String, String, String, i32)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT e.session_id, s.project_id, s.priority, e.payload
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.status = 'queued' AND e.type = 'agent.queued'
             ORDER BY e.session_id, e.seq DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut prompts = std::collections::HashMap::new();
        for row in rows {
            let (session_id, project_id, priority, payload) = row?;
            if prompts.contains_key(&session_id) {
                continue;
            }
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
            if let Some(prompt) = payload.get("prompt").and_then(serde_json::Value::as_str)
                && !prompt.trim().is_empty()
            {
                prompts.insert(session_id, (project_id, prompt.to_string(), priority));
            }
        }
        let mut prompts = prompts
            .into_iter()
            .map(|(session_id, (project_id, prompt, priority))| {
                (session_id, project_id, prompt, priority)
            })
            .collect::<Vec<_>>();
        prompts.sort_by(|left, right| right.3.cmp(&left.3).then_with(|| left.0.cmp(&right.0)));
        Ok(prompts)
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
             (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 'queued', 1, ?9, ?10, ?10)",
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
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
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
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Mark a queue entry as merging and persist the exact pre-merge HEAD.
    pub fn begin_merge_queue_entry(
        &self,
        id: &str,
        pre_merge_commit: &str,
    ) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE merge_queue
             SET status = 'merging', pre_merge_commit = ?1, updated_at = ?2
             WHERE id = ?3",
            params![pre_merge_commit, now_rfc3339(), id],
        )?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
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
             (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 'queued', 1, ?9, ?10, ?10)",
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
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
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
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
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
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
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

    /// Interrupted integration entries that still have a recorded pre-merge
    /// HEAD. The engine uses this to restore a clean session worktree after a
    /// crash before the merge status was finalized.
    pub fn interrupted_merge_recoveries(&self) -> Result<Vec<MergeQueueEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash,
                    base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue
             WHERE status = 'interrupted' AND pre_merge_commit IS NOT NULL
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], map_merge_queue_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_merge_queue(&self, session_id: &str) -> Result<Vec<MergeQueueEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_merge_queue_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_merge_queue_entry(&self, id: &str) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
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
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
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
                "SELECT id, session_id, tool_call_id, tool, input, status, created_at, resolved_at, approval_risk, approval_reason FROM approvals WHERE session_id = ?1 ORDER BY created_at ASC"
            }
            None => {
                "SELECT id, session_id, tool_call_id, tool, input, status, created_at, resolved_at, approval_risk, approval_reason FROM approvals ORDER BY created_at ASC"
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
                approval_risk: row.get(8)?,
                approval_reason: row.get(9)?,
            });
        }
        Ok(approvals)
    }
}

fn get_agent_tx(tx: &Transaction<'_>, id: &str) -> Result<AgentRecord> {
    tx.query_row(
        "SELECT id, session_id, role, name, title, profile, parent_id, task_id, department,
                worktree, branch, status, model, created_at
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
        task_id: row.get(7)?,
        department: row.get(8)?,
        worktree: row.get(9)?,
        branch: row.get(10)?,
        status: row.get(11)?,
        model: row.get(12)?,
        created_at: row.get(13)?,
    })
}

fn normalize_write_scope(scope: &str) -> String {
    scope
        .trim()
        .trim_start_matches("./")
        .trim_end_matches("/**")
        .trim_end_matches('/')
        .to_string()
}

/// Mirrors the scheduler's path-overlap rule. An empty or wildcard scope is
/// unrestricted and conflicts with every other active scope in the project.
fn write_scopes_overlap(left: &[String], right: &[String]) -> bool {
    if left.is_empty() || right.is_empty() {
        return true;
    }
    let is_unrestricted = |scope: &str| matches!(normalize_write_scope(scope).as_str(), "*" | "**");
    if left.iter().any(|scope| is_unrestricted(scope))
        || right.iter().any(|scope| is_unrestricted(scope))
    {
        return true;
    }
    left.iter().any(|a| {
        let a = normalize_write_scope(a);
        right.iter().any(|b| {
            let b = normalize_write_scope(b);
            a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
        })
    })
}

fn map_scheduled_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledTask> {
    let deps = row.get::<_, String>(8)?;
    let write_scope = row.get::<_, String>(9)?;
    Ok(ScheduledTask {
        session_id: row.get(0)?,
        project_id: row.get(1)?,
        plan_seq: row.get::<_, i64>(2)?.max(0) as u64,
        task_id: row.get(3)?,
        department: row.get(4)?,
        title: row.get(5)?,
        priority: row.get(6)?,
        status: row.get(7)?,
        deps: serde_json::from_str(&deps).unwrap_or_default(),
        write_scope: serde_json::from_str(&write_scope).unwrap_or_default(),
        attempts: row.get::<_, i64>(10)?.max(0) as u32,
        enqueued_at: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
        last_error: row.get(14)?,
        next_attempt_at: row.get(15)?,
    })
}

fn map_plan_task_progress(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanTaskProgress> {
    Ok(PlanTaskProgress {
        session_id: row.get(0)?,
        project_id: row.get(1)?,
        plan_seq: row.get::<_, i64>(2)?.max(0) as u64,
        task_id: row.get(3)?,
        last_progress_at: row.get(4)?,
        stall_count: row.get::<_, i64>(5)?.max(0) as u32,
        last_turn: row
            .get::<_, Option<i64>>(6)?
            .and_then(|value| u32::try_from(value).ok()),
        last_tool: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn map_steering(row: &rusqlite::Row<'_>) -> rusqlite::Result<SteeringMessage> {
    Ok(SteeringMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        agent_id: row.get(2)?,
        task_id: row.get(3)?,
        body: row.get(4)?,
        status: row.get(5)?,
        created_at: row.get(6)?,
        delivered_at: row.get(7)?,
    })
}

pub(crate) fn steering_content(message: &SteeringMessage) -> String {
    format!("[Operator steering #{}] {}", message.id, message.body)
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
        pre_merge_commit: row.get(8)?,
        status: row.get(9)?,
        attempt: row.get::<_, i64>(10)? as u32,
        detail: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
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
        version: row.get::<_, i64>(15).unwrap_or(1).max(1) as u32,
        created_at: row.get(14)?,
    })
}

fn artifact_version_from_artifact(artifact: &Artifact) -> ArtifactVersion {
    ArtifactVersion {
        artifact_id: artifact.id.clone(),
        version: artifact.version,
        name: artifact.name.clone(),
        kind: artifact.kind.clone(),
        mime_type: artifact.mime_type.clone(),
        content: artifact.content.clone(),
        storage: artifact.storage.clone(),
        compression: artifact.compression.clone(),
        size_bytes: artifact.size_bytes,
        sha256: artifact.sha256.clone(),
        path: artifact.path.clone(),
        created_at: artifact.created_at.clone(),
    }
}

fn map_artifact_version(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactVersion> {
    Ok(ArtifactVersion {
        artifact_id: row.get(0)?,
        version: row.get::<_, i64>(1)?.max(1) as u32,
        name: row.get(2)?,
        kind: row.get(3)?,
        mime_type: row.get(4)?,
        content: row.get(5)?,
        storage: row.get(6)?,
        compression: row.get(7)?,
        size_bytes: row.get::<_, i64>(8)?.max(0) as u64,
        sha256: row.get(9)?,
        path: row.get(10)?,
        created_at: row.get(11)?,
    })
}

fn insert_artifact_version_tx(tx: &Transaction<'_>, artifact: &Artifact) -> Result<()> {
    tx.execute(
        "INSERT INTO artifact_versions
         (artifact_id, version, name, kind, mime_type, content, storage, compression,
          size_bytes, sha256, path, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(artifact_id, version) DO UPDATE SET
            name = excluded.name,
            kind = excluded.kind,
            mime_type = excluded.mime_type,
            content = excluded.content,
            storage = excluded.storage,
            compression = excluded.compression,
            size_bytes = excluded.size_bytes,
            sha256 = excluded.sha256,
            path = excluded.path",
        params![
            artifact.id,
            artifact.version as i64,
            artifact.name,
            artifact.kind,
            artifact.mime_type,
            artifact.content,
            artifact.storage,
            artifact.compression,
            artifact.size_bytes as i64,
            artifact.sha256,
            artifact.path,
            artifact.created_at,
        ],
    )?;
    Ok(())
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
        unlimited: row.get::<_, i64>(9)? != 0,
        cost_spent_usd: row.get(10)?,
        base_commit: row.get(11)?,
        tokens_in: row.get::<_, i64>(12)? as u64,
        tokens_out: row.get::<_, i64>(13)? as u64,
        created_at: row.get(14)?,
        priority: row.get(15)?,
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

fn fts_query(input: &str) -> Option<String> {
    let terms = input
        .split_whitespace()
        .filter_map(|term| {
            let term = term.trim_matches(|character: char| {
                !character.is_alphanumeric() && character != '_' && character != '-'
            });
            if term.is_empty() {
                None
            } else {
                Some(format!("\"{}\"*", term.replace('"', "\"\"")))
            }
        })
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

fn map_acp_agent(row: &rusqlite::Row<'_>) -> rusqlite::Result<AcpAgent> {
    let args: String = row.get(3)?;
    let env: String = row.get(4)?;
    Ok(AcpAgent {
        id: row.get(0)?,
        name: row.get(1)?,
        command: row.get(2)?,
        args: serde_json::from_str(&args).unwrap_or_default(),
        env: serde_json::from_str(&env).unwrap_or_default(),
        enabled: row.get::<_, i64>(5)? != 0,
        auto_approve: row.get::<_, i64>(6)? != 0,
        timeout_secs: row.get::<_, i64>(7)? as u64,
        created_at: row.get(8)?,
    })
}

fn map_mcp_server(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServer> {
    let args: String = row.get(3)?;
    let env: String = row.get(4)?;
    let transport: String = row.get(7)?;
    let headers: String = row.get(9)?;
    Ok(McpServer {
        id: row.get(0)?,
        name: row.get(1)?,
        command: row.get(2)?,
        args: serde_json::from_str(&args).unwrap_or_default(),
        env: serde_json::from_str(&env).unwrap_or_default(),
        enabled: row.get::<_, i64>(5)? != 0,
        created_at: row.get(6)?,
        transport: match transport.as_str() {
            "http" => McpTransport::Http,
            "sse" => McpTransport::Sse,
            _ => McpTransport::Stdio,
        },
        url: row.get(8)?,
        headers: serde_json::from_str(&headers).unwrap_or_default(),
    })
}

fn transport_name(transport: McpTransport) -> &'static str {
    match transport {
        McpTransport::Stdio => "stdio",
        McpTransport::Http => "http",
        McpTransport::Sse => "sse",
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
    let needle = query
        .split_whitespace()
        .map(str::to_lowercase)
        .find(|term| !term.is_empty() && lower.contains(term))
        .unwrap_or_else(|| query.to_lowercase());
    let start = lower.find(&needle).unwrap_or(0);
    let char_start = lower[..start].chars().count();
    let chars = text.chars().collect::<Vec<_>>();
    let from = char_start.saturating_sub(60).min(chars.len());
    let to = (char_start + needle.chars().count() + 120).min(chars.len());
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
#[path = "store/tests.rs"]
mod tests;
