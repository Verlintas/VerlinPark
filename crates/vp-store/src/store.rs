use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    AcpAgent, AgentMessage, AgentRecord, Approval, ApprovalGrant, Artifact, ArtifactPolicy,
    ArtifactVersion, CheckpointRetentionPolicy, ContinuationOutcome, Event, IntakeTask,
    IntegrationPolicy, MailboxPolicy, McpServer, McpServerConfig, McpTransport, MemoryConflict,
    MemoryEvidence, MemoryHealthResult, MemoryItem, MemoryPolicy, MemoryStatCount, MergeQueueEntry,
    MetricCount, ModelPreset, OrchestrationDecision, OrchestrationNote, OrchestrationPolicy,
    OrchestrationRecovery, PlanTaskProgress, Program, ProgramEvent, ProgramMilestone,
    ProgramMilestoneStatus, ProgramPolicy, ProgramStatus, ProgramUsage, Project, ProjectBudget,
    ProjectUsageSession, Question, ScheduledTask, SearchHit, Session, SessionBundle,
    SessionContinuation, ShellPolicy, Skill, Source, SourceChunk, SourceCollection,
    SourceSearchHit, SourceStats, SteeringMessage, StoredMessage, TaskSchedulerPolicy, TaskStall,
    TeamPlan, TodoItem, UsageEntry, WorktreeRetentionPolicy, WriteLease, cache_hit_ratio,
};

use vp_protocol::remote::ExecutionHost;

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

/// Result of claiming an idempotent session operation. `pending` rows from a
/// previous daemon process are promoted to `unknown` during startup recovery
/// and never silently replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// No row existed, or a failed row with the same fingerprint was reset to
    /// `pending` for a safe retry.
    New,
    /// The operation already completed with the same method and payload.
    /// The stored JSON result should be returned without repeating side
    /// effects.
    Replay(String),
    /// Another caller already holds the same operation in `pending`.
    InProgress,
    /// The operation id exists with a different method or payload.
    Conflict,
    /// The previous daemon stopped while the operation was in flight. The
    /// side effects may or may not have landed.
    Unknown,
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

/// Input for creating a project program. The engine validates all bounds and
/// passes a pre-normalized policy; the store still enforces uniqueness and
/// foreign keys.
#[derive(Debug, Clone)]
pub struct NewProgram {
    pub project_id: String,
    pub goal: String,
    pub policy: ProgramPolicy,
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
    pub status: ProgramStatus,
    pub archived: bool,
}

#[derive(Debug, Clone)]
pub struct ProgramUpdate {
    pub goal: String,
    pub policy: ProgramPolicy,
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct NewProgramMilestone {
    pub program_id: String,
    pub key: String,
    pub title: String,
    pub objective: String,
    pub acceptance: Vec<String>,
    pub deps: Vec<String>,
    pub priority: i32,
    pub max_attempts: u32,
    pub profile: String,
    pub write_scope: Vec<String>,
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ProgramMilestoneUpdate {
    pub key: String,
    pub title: String,
    pub objective: String,
    pub acceptance: Vec<String>,
    pub deps: Vec<String>,
    pub priority: i32,
    pub max_attempts: u32,
    pub profile: String,
    pub write_scope: Vec<String>,
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
}

fn map_program(row: &rusqlite::Row<'_>) -> rusqlite::Result<Program> {
    let status: String = row.get(3)?;
    let policy: String = row.get(4)?;
    Ok(Program {
        id: row.get(0)?,
        project_id: row.get(1)?,
        goal: row.get(2)?,
        status: ProgramStatus::parse(&status).unwrap_or_default(),
        policy: serde_json::from_str(&policy).unwrap_or_default(),
        token_budget: row
            .get::<_, Option<i64>>(5)?
            .map(|value| value.max(0) as u64),
        cost_budget_usd: row.get(6)?,
        reason: row.get(7)?,
        archived: row.get::<_, i64>(8)? != 0,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        completed_at: row.get(11)?,
    })
}

fn map_program_milestone(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProgramMilestone> {
    let acceptance: String = row.get(4)?;
    let deps: String = row.get(5)?;
    let status: String = row.get(7)?;
    let write_scope: String = row.get(15)?;
    Ok(ProgramMilestone {
        id: row.get(0)?,
        program_id: row.get(1)?,
        key: row.get(2)?,
        title: row.get(3)?,
        objective: row.get(16)?,
        acceptance: serde_json::from_str(&acceptance).unwrap_or_default(),
        deps: serde_json::from_str(&deps).unwrap_or_default(),
        priority: row.get(6)?,
        status: ProgramMilestoneStatus::parse(&status).unwrap_or_default(),
        session_id: row.get(8)?,
        plan_revision: row
            .get::<_, Option<i64>>(9)?
            .map(|value| value.max(1) as u32),
        attempts: row.get::<_, i64>(10)?.max(0) as u32,
        max_attempts: row.get::<_, i64>(11)?.max(1) as u32,
        next_attempt_at: row.get(12)?,
        result: row.get(13)?,
        last_error: row.get(14)?,
        profile: row.get(17)?,
        write_scope: serde_json::from_str(&write_scope).unwrap_or_default(),
        token_budget: row
            .get::<_, Option<i64>>(18)?
            .map(|value| value.max(0) as u64),
        cost_budget_usd: row.get(19)?,
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
        completed_at: row.get(22)?,
    })
}

fn map_program_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProgramEvent> {
    let payload: String = row.get(4)?;
    Ok(ProgramEvent {
        program_id: row.get(0)?,
        project_id: row.get(1)?,
        seq: row.get::<_, i64>(2)?.max(0) as u64,
        kind: row.get(3)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        created_at: row.get(5)?,
    })
}

/// Append one program-scoped event and return the full row. The caller must
/// use the same transaction for the projection update; this helper never
/// commits on its own.
fn insert_program_event_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    program_id: &str,
    kind: &str,
    payload: &serde_json::Value,
) -> Result<ProgramEvent> {
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM program_events WHERE program_id = ?1",
        params![program_id],
        |row| row.get(0),
    )?;
    let created_at = now_rfc3339();
    let payload_text = serde_json::to_string(payload)?;
    tx.execute(
        "INSERT INTO program_events (project_id, program_id, seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![project_id, program_id, seq, kind, payload_text, created_at],
    )?;
    Ok(ProgramEvent {
        program_id: program_id.to_string(),
        project_id: project_id.to_string(),
        seq: seq.max(0) as u64,
        kind: kind.to_string(),
        payload: payload.clone(),
        created_at,
    })
}

const PROGRAM_COLUMNS: &str = "id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason, archived, \
     created_at, updated_at, completed_at";

const PROGRAM_MILESTONE_COLUMNS: &str = "id, program_id, key, title, acceptance, deps, priority, status, session_id, plan_revision, \
     attempts, max_attempts, next_attempt_at, result, last_error, write_scope, objective, \
     profile, token_budget, cost_budget_usd, created_at, updated_at, completed_at";

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
    pub memory_conflicts: Option<Vec<MemoryConflict>>,
    pub questions: Option<Vec<Question>>,
    pub tasks: Option<Vec<ScheduledTask>>,
    pub steering: Option<Vec<SteeringMessage>>,
    pub continuations: Option<Vec<SessionContinuation>>,
    pub recoveries: Option<Vec<OrchestrationRecovery>>,
    pub artifact_warnings: Vec<String>,
}

/// A memory creation request with provenance. The plain
/// `create_memory_with_event` shim delegates here with source `agent` and no
/// evidence.
pub struct NewMemory<'a> {
    pub session_id: &'a str,
    pub project_id: &'a str,
    pub agent_id: &'a str,
    pub agent_name: &'a str,
    pub kind: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub tags: &'a [String],
    pub source: &'a str,
    pub evidence: &'a [MemoryEvidence],
    pub pinned: bool,
    pub verify_after: Option<&'a str>,
    /// Collapse an exact same-key/same-content create into the existing row.
    pub dedupe: bool,
    /// Allow a same-key create with different content to supersede the active
    /// row. Ignored when `dedupe` is false.
    pub auto_supersede: bool,
}

#[derive(Debug, Clone)]
pub struct MemoryCreateOutcome {
    pub memory: MemoryItem,
    pub created: bool,
    /// `memory.superseded` for the old row (if any) followed by
    /// `memory.created` for the new row. Empty when the create was idempotent.
    pub events: Vec<Event>,
}

#[derive(Debug, Clone)]
pub struct MemoryCandidate {
    pub memory: MemoryItem,
    /// Lexical relevance in 0..=1 derived from FTS bm25 or the LIKE fallback.
    pub lexical: f64,
    /// True when the candidate came from FTS5 rather than LIKE.
    pub fts: bool,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryStatsSnapshot {
    pub total: u32,
    pub by_status: Vec<MemoryStatCount>,
    pub by_source: Vec<MemoryStatCount>,
    pub by_kind: Vec<MemoryStatCount>,
}

/// One active memory that cites a `path` or `task` evidence reference. Rows
/// are grouped by `(scope_kind, scope_value)` before pairing.
#[derive(Debug, Clone)]
struct ConflictCandidate {
    memory: MemoryItem,
    scope_kind: &'static str,
    scope_value: String,
}

/// Result of a detection pass. `new` is empty when every current conflict id
/// already exists, including resolved rows.
#[derive(Debug, Clone, Default)]
pub struct MemoryConflictScan {
    pub new: Vec<MemoryConflict>,
    pub open: Vec<MemoryConflict>,
    pub events: Vec<Event>,
}

/// Result of an explicit operator resolution.
#[derive(Debug, Clone)]
pub struct MemoryConflictResolution {
    pub conflict: MemoryConflict,
    /// The two memories in A/B order after any lifecycle mutation.
    pub memories: Vec<MemoryItem>,
    pub events: Vec<Event>,
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

/// Input for creating the first row of a deferred continuation. The store
/// generates the row id and uses it as the series id when none is supplied.
#[derive(Debug, Clone)]
pub struct NewSessionContinuation {
    pub session_id: String,
    pub project_id: String,
    pub body: String,
    pub watch_task_ids: Vec<String>,
    pub watch_mode: String,
    pub deadline_at: Option<String>,
    pub max_attempts: u32,
    pub series_id: Option<String>,
    pub parent_continuation_id: Option<String>,
    pub recurrence_every_secs: Option<u64>,
    pub recurrence_max_runs: u32,
    pub run_count: u32,
}

/// Result of committing one successful continuation delivery. `scheduled` is
/// the next pending run when the series still has budget, and both events are
/// emitted in the same transaction as the projection change.
#[derive(Debug, Clone)]
pub struct ContinuationDelivery {
    pub delivered: SessionContinuation,
    pub scheduled: Option<SessionContinuation>,
    pub events: Vec<Event>,
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
    pub execution_host: ExecutionHost,
    #[serde(default)]
    pub approval_grants: Vec<ApprovalGrant>,
    /// Standalone task-inbox items. They are project-scoped rather than
    /// session-scoped, so the project manifest is their rebuild snapshot.
    #[serde(default)]
    pub intake_tasks: Vec<IntakeTask>,
    /// Project-scoped program ledgers and their append-only event logs. The
    /// program projection is rebuilt from these rows by `db-rebuild`; the
    /// events preserve replay/audit across a database restore.
    #[serde(default)]
    pub programs: Vec<Program>,
    #[serde(default)]
    pub program_milestones: Vec<ProgramMilestone>,
    #[serde(default)]
    pub program_events: Vec<ProgramEvent>,
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

/// A read-only, low-cardinality snapshot for daemon metrics. Status/kind
/// vectors are ordered lexicographically and capped so no export can grow with
/// the number of sessions or tasks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DaemonMetricsSnapshot {
    pub projects: u64,
    pub sessions: Vec<MetricCount>,
    pub plan_tasks: Vec<MetricCount>,
    pub merge_queue: Vec<MetricCount>,
    pub intake: Vec<MetricCount>,
    pub continuations: Vec<MetricCount>,
    pub recoveries: Vec<MetricCount>,
    pub memories: Vec<MetricCount>,
    pub memory_conflicts: Vec<MetricCount>,
    pub runtime_leases: Vec<MetricCount>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Latest prompt-prefix estimate observed across sessions, if any run has
    /// emitted the field (or the legacy character count fallback).
    pub preamble_tokens_estimate: Option<u64>,
    pub cost_spent_usd: f64,
}

/// Copy a SQLite database with `VACUUM INTO`.
///
/// The source is opened read-only and the copy is a consistent point-in-time
/// snapshot, including committed pages still living in the WAL. Only the
/// database file is created at `destination`; no `-wal`/`-shm` sidecars are
/// copied. The caller owns the destination and must ensure it does not exist.
pub fn snapshot_database(database: &Path, destination: &Path) -> Result<()> {
    if !database.is_file() {
        return Err(anyhow!("database not found at {}", database.display()));
    }
    if destination.exists() {
        return Err(anyhow!(
            "refusing to overwrite existing database snapshot {}",
            destination.display()
        ));
    }
    let database_str = database
        .to_str()
        .ok_or_else(|| anyhow!("database path is not valid UTF-8: {}", database.display()))?;
    let destination_str = destination.to_str().ok_or_else(|| {
        anyhow!(
            "snapshot path is not valid UTF-8: {}",
            destination.display()
        )
    })?;
    let conn = Connection::open_with_flags(database_str, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for snapshot", database.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute("VACUUM INTO ?1", params![destination_str])
        .with_context(|| format!("snapshotting {} with VACUUM INTO", database.display()))?;
    drop(conn);
    paths::ensure_private_file(destination)
        .with_context(|| format!("setting permissions on {}", destination.display()))?;
    Ok(())
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
    for conflict in &bundle.memory_conflicts {
        ensure_bundle_session("memory conflict", &conflict.session_id, &bundle.session.id)?;
        if conflict.project_id != bundle.project.id {
            return Err(anyhow!(
                "bundle memory conflict {} belongs to a different project",
                conflict.id
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
    for recovery in &bundle.recoveries {
        ensure_bundle_session("recovery", &recovery.session_id, &bundle.session.id)?;
        if recovery.project_id != bundle.project.id {
            return Err(anyhow!(
                "bundle recovery {} belongs to a different project",
                recovery.task_id
            ));
        }
    }
    let mut program_ids = std::collections::HashSet::new();
    for program in &bundle.programs {
        validate_storage_id("program", &program.id)?;
        if program.project_id != bundle.project.id {
            return Err(anyhow!(
                "bundle program {} belongs to a different project",
                program.id
            ));
        }
        if !program_ids.insert(program.id.clone()) {
            return Err(anyhow!("bundle program {} is duplicated", program.id));
        }
    }
    let mut milestone_keys = std::collections::HashSet::new();
    for milestone in &bundle.program_milestones {
        validate_storage_id("program milestone", &milestone.id)?;
        if !program_ids.contains(&milestone.program_id) {
            return Err(anyhow!(
                "bundle milestone {} references a program not in the bundle",
                milestone.id
            ));
        }
        if !milestone_keys.insert((milestone.program_id.clone(), milestone.key.clone())) {
            return Err(anyhow!(
                "bundle milestone key '{}' is duplicated in program {}",
                milestone.key,
                milestone.program_id
            ));
        }
    }
    for event in &bundle.program_events {
        if event.project_id != bundle.project.id {
            return Err(anyhow!(
                "bundle program event {} belongs to a different project",
                event.program_id
            ));
        }
        if !program_ids.contains(&event.program_id) {
            return Err(anyhow!(
                "bundle program event {} references a program not in the bundle",
                event.program_id
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

            CREATE TABLE IF NOT EXISTS project_execution_hosts (
                project_id      TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                config          TEXT NOT NULL,
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
            CREATE INDEX IF NOT EXISTS idx_events_type_created
                ON events(type, created_at DESC);
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
                cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
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
                created_at  TEXT NOT NULL,
                status      TEXT NOT NULL DEFAULT 'active',
                source      TEXT NOT NULL DEFAULT 'agent',
                evidence    TEXT NOT NULL DEFAULT '[]',
                supersedes  TEXT NOT NULL DEFAULT '[]',
                superseded_by TEXT,
                pinned      INTEGER NOT NULL DEFAULT 0,
                use_count   INTEGER NOT NULL DEFAULT 0,
                last_used_at TEXT,
                last_verified_at TEXT,
                verify_after TEXT,
                stale_reason TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_memories_project
                ON memories(project_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_memories_session
                ON memories(session_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_memories_project_status
                ON memories(project_id, status, created_at);

            CREATE INDEX IF NOT EXISTS idx_memories_project_kind_title
                ON memories(project_id, kind, title);

            CREATE TABLE IF NOT EXISTS memory_usage (
                memory_id  TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL,
                used_at    TEXT NOT NULL,
                PRIMARY KEY(memory_id, session_id)
            );

            CREATE TABLE IF NOT EXISTS memory_conflicts (
                id               TEXT PRIMARY KEY,
                project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                memory_a_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                memory_b_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                memory_a_title   TEXT NOT NULL,
                memory_b_title   TEXT NOT NULL,
                memory_a_kind    TEXT NOT NULL,
                memory_b_kind    TEXT NOT NULL,
                memory_a_excerpt TEXT NOT NULL,
                memory_b_excerpt TEXT NOT NULL,
                scope_kind       TEXT NOT NULL,
                scope_value      TEXT NOT NULL,
                reason           TEXT NOT NULL,
                status           TEXT NOT NULL DEFAULT 'open',
                detected_at      TEXT NOT NULL,
                resolved_at      TEXT,
                resolution       TEXT,
                note             TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_memory_conflicts_project_status
                ON memory_conflicts(project_id, status, detected_at);

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

            CREATE TABLE IF NOT EXISTS programs (
                id              TEXT PRIMARY KEY,
                project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                goal            TEXT NOT NULL,
                status          TEXT NOT NULL,
                policy          TEXT NOT NULL DEFAULT '{}',
                token_budget    INTEGER,
                cost_budget_usd REAL,
                reason          TEXT,
                archived        INTEGER NOT NULL DEFAULT 0,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                completed_at    TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_programs_project_status
                ON programs(project_id, status, created_at);

            CREATE TABLE IF NOT EXISTS program_milestones (
                id              TEXT PRIMARY KEY,
                program_id      TEXT NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
                key             TEXT NOT NULL,
                title           TEXT NOT NULL,
                objective       TEXT NOT NULL,
                acceptance      TEXT NOT NULL DEFAULT '[]',
                deps            TEXT NOT NULL DEFAULT '[]',
                priority        INTEGER NOT NULL DEFAULT 0,
                status          TEXT NOT NULL,
                session_id      TEXT,
                plan_revision   INTEGER,
                attempts        INTEGER NOT NULL DEFAULT 0,
                max_attempts    INTEGER NOT NULL DEFAULT 2,
                next_attempt_at TEXT,
                result          TEXT,
                last_error      TEXT,
                profile         TEXT NOT NULL DEFAULT 'coding',
                write_scope     TEXT NOT NULL DEFAULT '[]',
                token_budget    INTEGER,
                cost_budget_usd REAL,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                completed_at    TEXT,
                UNIQUE(program_id, key)
            );

            CREATE INDEX IF NOT EXISTS idx_program_milestones_program_status
                ON program_milestones(program_id, status, next_attempt_at);
            CREATE INDEX IF NOT EXISTS idx_program_milestones_session
                ON program_milestones(session_id);

            CREATE TABLE IF NOT EXISTS program_events (
                project_id  TEXT NOT NULL,
                program_id  TEXT NOT NULL,
                seq         INTEGER NOT NULL,
                kind        TEXT NOT NULL,
                payload     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                PRIMARY KEY (program_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_program_events_project
                ON program_events(project_id, created_at);

            -- Append-only accounting ledger for every milestone dispatch.
            -- `program_milestones.session_id` only points at the current
            -- attempt; retries clear it. Program budgets must still count the
            -- spend of every earlier attempt, so links are never deleted on
            -- retry and are rebuilt from `program.milestone.updated` events.
            CREATE TABLE IF NOT EXISTS program_milestone_sessions (
                program_id    TEXT NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
                milestone_id  TEXT NOT NULL REFERENCES program_milestones(id) ON DELETE CASCADE,
                session_id    TEXT NOT NULL,
                attempt       INTEGER NOT NULL,
                dispatched_at TEXT NOT NULL,
                PRIMARY KEY (milestone_id, session_id)
            );

            CREATE INDEX IF NOT EXISTS idx_program_milestone_sessions_program
                ON program_milestone_sessions(program_id, session_id);
            CREATE INDEX IF NOT EXISTS idx_program_milestone_sessions_session
                ON program_milestone_sessions(session_id);

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

            CREATE TABLE IF NOT EXISTS session_operations (
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                operation_id TEXT NOT NULL,
                method       TEXT NOT NULL,
                fingerprint  TEXT NOT NULL,
                status       TEXT NOT NULL,
                result_json  TEXT,
                error        TEXT,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (session_id, operation_id)
            );

            CREATE INDEX IF NOT EXISTS idx_session_operations_status
                ON session_operations(status, updated_at);

            CREATE TABLE IF NOT EXISTS session_continuations (
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                body            TEXT NOT NULL,
                watch_task_ids  TEXT NOT NULL DEFAULT '[]',
                watch_mode      TEXT NOT NULL DEFAULT 'all',
                deadline_at     TEXT,
                status          TEXT NOT NULL,
                attempts        INTEGER NOT NULL DEFAULT 0,
                max_attempts    INTEGER NOT NULL DEFAULT 3,
                wake_reason     TEXT,
                outcomes        TEXT NOT NULL DEFAULT '[]',
                last_error      TEXT,
                next_attempt_at TEXT,
                created_at      TEXT NOT NULL,
                ready_at        TEXT,
                delivered_at    TEXT,
                updated_at      TEXT NOT NULL,
                series_id       TEXT NOT NULL DEFAULT '',
                parent_continuation_id TEXT,
                recurrence_every_secs INTEGER,
                recurrence_max_runs INTEGER NOT NULL DEFAULT 1,
                run_count       INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_session_continuations_status
                ON session_continuations(status, next_attempt_at, deadline_at);
            CREATE INDEX IF NOT EXISTS idx_session_continuations_session
                ON session_continuations(session_id, status, created_at);

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

            CREATE TABLE IF NOT EXISTS orchestration_recoveries (
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                plan_seq          INTEGER NOT NULL,
                task_id           TEXT NOT NULL,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                department        TEXT NOT NULL,
                action            TEXT NOT NULL,
                status            TEXT NOT NULL,
                attempts          INTEGER NOT NULL DEFAULT 0,
                last_stall_count  INTEGER NOT NULL DEFAULT 0,
                requested_at      TEXT NOT NULL,
                updated_at        TEXT NOT NULL,
                last_error        TEXT,
                next_action_at    TEXT,
                PRIMARY KEY (session_id, plan_seq, task_id)
            );

            CREATE INDEX IF NOT EXISTS idx_orchestration_recoveries_session
                ON orchestration_recoveries(session_id, plan_seq, task_id);
            CREATE INDEX IF NOT EXISTS idx_orchestration_recoveries_status
                ON orchestration_recoveries(status, updated_at);

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
        ensure_column(
            &conn,
            "session_continuations",
            "series_id",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "parent_continuation_id",
            "TEXT",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "recurrence_every_secs",
            "INTEGER",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "recurrence_max_runs",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "run_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
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
            "usage_ledger",
            "cache_read_tokens",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "usage_ledger",
            "cache_write_tokens",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
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
        ensure_column(
            &conn,
            "memories",
            "status",
            "TEXT NOT NULL DEFAULT 'active'",
        )?;
        ensure_column(&conn, "memories", "source", "TEXT NOT NULL DEFAULT 'agent'")?;
        ensure_column(&conn, "memories", "evidence", "TEXT NOT NULL DEFAULT '[]'")?;
        ensure_column(
            &conn,
            "memories",
            "supersedes",
            "TEXT NOT NULL DEFAULT '[]'",
        )?;
        ensure_column(&conn, "memories", "superseded_by", "TEXT")?;
        ensure_column(&conn, "memories", "pinned", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "memories", "use_count", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "memories", "last_used_at", "TEXT")?;
        ensure_column(&conn, "memories", "last_verified_at", "TEXT")?;
        ensure_column(&conn, "memories", "verify_after", "TEXT")?;
        ensure_column(&conn, "memories", "stale_reason", "TEXT")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_memories_project_status
                 ON memories(project_id, status, created_at);
             CREATE INDEX IF NOT EXISTS idx_memories_project_kind_title
                 ON memories(project_id, kind, title);
             CREATE TABLE IF NOT EXISTS memory_usage (
                 memory_id  TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                 session_id TEXT NOT NULL,
                 used_at    TEXT NOT NULL,
                 PRIMARY KEY(memory_id, session_id)
             );
             CREATE TABLE IF NOT EXISTS memory_conflicts (
                 id               TEXT PRIMARY KEY,
                 project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                 session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 memory_a_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                 memory_b_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                 memory_a_title   TEXT NOT NULL,
                 memory_b_title   TEXT NOT NULL,
                 memory_a_kind    TEXT NOT NULL,
                 memory_b_kind    TEXT NOT NULL,
                 memory_a_excerpt TEXT NOT NULL,
                 memory_b_excerpt TEXT NOT NULL,
                 scope_kind       TEXT NOT NULL,
                 scope_value      TEXT NOT NULL,
                 reason           TEXT NOT NULL,
                 status           TEXT NOT NULL DEFAULT 'open',
                 detected_at      TEXT NOT NULL,
                 resolved_at      TEXT,
                 resolution       TEXT,
                 note             TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_memory_conflicts_project_status
                 ON memory_conflicts(project_id, status, detected_at);",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "task_result_verify_days",
            "INTEGER NOT NULL DEFAULT 90",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "default_verify_days",
            "INTEGER NOT NULL DEFAULT 365",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "auto_supersede",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "brief_max_chars",
            "INTEGER NOT NULL DEFAULT 4000",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "sweep_stale",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
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
                execution_host: self.get_execution_host(&project.id)?,
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
                programs: self.list_programs(Some(&project.id), None, None)?,
                program_milestones: self.list_program_milestones_for_project(&project.id)?,
                program_events: self.list_program_events_for_project(&project.id, Some(100_000))?,
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

    pub fn get_execution_host(&self, project_id: &str) -> Result<ExecutionHost> {
        let conn = self.lock()?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT config FROM project_execution_hosts WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        match stored {
            Some(config) => {
                serde_json::from_str(&config).context("parsing stored project execution host")
            }
            None => Ok(ExecutionHost::default()),
        }
    }

    pub fn execution_host_updated_at(&self, project_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT updated_at FROM project_execution_hosts WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn set_execution_host(&self, project_id: &str, config: &ExecutionHost) -> Result<()> {
        let encoded =
            serde_json::to_string(config).context("serializing project execution host")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_execution_hosts (project_id, config, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                config = excluded.config,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn remove_execution_host(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let removed = conn.execute(
            "DELETE FROM project_execution_hosts WHERE project_id = ?1",
            params![project_id],
        )?;
        Ok(removed > 0)
    }

    pub fn list_execution_hosts(&self) -> Result<Vec<ExecutionHost>> {
        let projects = self.list_projects()?;
        projects
            .into_iter()
            .map(|project| self.get_execution_host(&project.id))
            .collect()
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

    pub fn list_programs(
        &self,
        project_id: Option<&str>,
        status: Option<ProgramStatus>,
        limit: Option<u32>,
    ) -> Result<Vec<Program>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 2_000) as i64;
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROGRAM_COLUMNS}
             FROM programs
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR status = ?2)
             ORDER BY created_at DESC, id DESC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![project_id, status.map(ProgramStatus::as_str), limit],
            map_program,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_program(&self, id: &str) -> Result<Option<Program>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
            params![id],
            map_program,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_program_milestones(&self, program_id: &str) -> Result<Vec<ProgramMilestone>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROGRAM_MILESTONE_COLUMNS}
             FROM program_milestones
             WHERE program_id = ?1
             ORDER BY created_at ASC, key ASC, id ASC"
        ))?;
        let rows = stmt.query_map(params![program_id], map_program_milestone)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_program_milestones_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProgramMilestone>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {}
             FROM program_milestones m
             JOIN programs p ON p.id = m.program_id
             WHERE p.project_id = ?1
             ORDER BY m.created_at ASC, m.key ASC, m.id ASC",
            PROGRAM_MILESTONE_COLUMNS
                .split(", ")
                .map(|column| format!("m.{column}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![project_id], map_program_milestone)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_program_milestone(&self, id: &str) -> Result<Option<ProgramMilestone>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"),
            params![id],
            map_program_milestone,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_program(&self, input: &NewProgram) -> Result<(Program, ProgramEvent)> {
        let id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let program = Program {
            id,
            project_id: input.project_id.clone(),
            goal: input.goal.clone(),
            status: input.status,
            policy: input.policy.clone(),
            token_budget: input.token_budget,
            cost_budget_usd: input.cost_budget_usd,
            reason: None,
            archived: input.archived,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        };
        let payload = serde_json::to_value(&program)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if tx.query_row(
            "SELECT COUNT(*) FROM projects WHERE id = ?1",
            params![program.project_id],
            |row| row.get::<_, i64>(0),
        )? == 0
        {
            anyhow::bail!("project not found: {}", program.project_id);
        }
        let event = insert_program_event_tx(
            &tx,
            &program.project_id,
            &program.id,
            "program.created",
            &payload,
        )?;
        tx.execute(
            "INSERT INTO programs
             (id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason,
              archived, created_at, updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                program.id,
                program.project_id,
                program.goal,
                program.status.as_str(),
                serde_json::to_string(&program.policy)?,
                program.token_budget.map(|value| value as i64),
                program.cost_budget_usd,
                program.reason,
                if program.archived { 1 } else { 0 },
                program.created_at,
                program.updated_at,
                program.completed_at,
            ],
        )?;
        tx.commit()?;
        Ok((program, event))
    }

    pub fn update_program(
        &self,
        id: &str,
        update: &ProgramUpdate,
    ) -> Result<Option<(Program, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.archived || program.status.is_terminal() {
            anyhow::bail!("program {id} is archived or terminal and cannot be updated");
        }
        program.goal = update.goal.clone();
        program.policy = update.policy.clone();
        program.token_budget = update.token_budget;
        program.cost_budget_usd = update.cost_budget_usd;
        program.updated_at = now;
        let payload = serde_json::to_value(&program)?;
        let event =
            insert_program_event_tx(&tx, &program.project_id, id, "program.updated", &payload)?;
        tx.execute(
            "UPDATE programs
             SET goal = ?2, policy = ?3, token_budget = ?4, cost_budget_usd = ?5, updated_at = ?6
             WHERE id = ?1",
            params![
                id,
                program.goal,
                serde_json::to_string(&program.policy)?,
                program.token_budget.map(|value| value as i64),
                program.cost_budget_usd,
                program.updated_at,
            ],
        )?;
        tx.commit()?;
        Ok(Some((program, event)))
    }

    pub fn pause_program(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<Option<(Program, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.status == ProgramStatus::Paused {
            return Ok(None);
        }
        if program.status != ProgramStatus::Active {
            anyhow::bail!("program {id} is not active");
        }
        program.status = ProgramStatus::Paused;
        program.reason = reason.map(str::to_string);
        program.updated_at = now_rfc3339();
        let payload = serde_json::to_value(&program)?;
        let event =
            insert_program_event_tx(&tx, &program.project_id, id, "program.paused", &payload)?;
        tx.execute(
            "UPDATE programs SET status = ?2, reason = ?3, updated_at = ?4 WHERE id = ?1",
            params![
                id,
                program.status.as_str(),
                program.reason,
                program.updated_at
            ],
        )?;
        tx.commit()?;
        Ok(Some((program, event)))
    }

    pub fn resume_program(&self, id: &str) -> Result<Option<(Program, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.archived {
            anyhow::bail!("program {id} is an imported archive and cannot be activated");
        }
        if program.status.is_terminal() {
            anyhow::bail!("program {id} is terminal and cannot be resumed");
        }
        if program.status == ProgramStatus::Active {
            return Ok(None);
        }
        program.status = ProgramStatus::Active;
        program.reason = None;
        program.updated_at = now_rfc3339();
        let payload = serde_json::to_value(&program)?;
        let event =
            insert_program_event_tx(&tx, &program.project_id, id, "program.resumed", &payload)?;
        tx.execute(
            "UPDATE programs SET status = ?2, reason = NULL, updated_at = ?3 WHERE id = ?1",
            params![id, program.status.as_str(), program.updated_at],
        )?;
        tx.commit()?;
        Ok(Some((program, event)))
    }

    pub fn complete_program(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(Program, ProgramEvent)>> {
        self.finish_program(id, ProgramStatus::Completed, reason)
    }

    pub fn fail_program(&self, id: &str, reason: &str) -> Result<Option<(Program, ProgramEvent)>> {
        self.finish_program(id, ProgramStatus::Failed, reason)
    }

    fn finish_program(
        &self,
        id: &str,
        status: ProgramStatus,
        reason: &str,
    ) -> Result<Option<(Program, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.status.is_terminal() {
            return Ok(None);
        }
        if program.archived {
            anyhow::bail!("program {id} is an imported archive");
        }
        let now = now_rfc3339();
        program.status = status;
        program.reason = Some(reason.to_string());
        program.updated_at = now.clone();
        program.completed_at = Some(now.clone());
        let payload = serde_json::to_value(&program)?;
        let kind = match status {
            ProgramStatus::Completed => "program.completed",
            ProgramStatus::Failed => "program.failed",
            _ => anyhow::bail!("invalid terminal program status: {status}"),
        };
        let event = insert_program_event_tx(&tx, &program.project_id, id, kind, &payload)?;
        tx.execute(
            "UPDATE programs
             SET status = ?2, reason = ?3, completed_at = ?4, updated_at = ?4
             WHERE id = ?1",
            params![id, program.status.as_str(), program.reason, now],
        )?;
        tx.commit()?;
        Ok(Some((program, event)))
    }

    pub fn delete_program(&self, id: &str) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(false);
        };
        let active = tx.query_row(
            "SELECT COUNT(*) FROM program_milestones
             WHERE program_id = ?1 AND status IN ('running', 'verifying')",
            params![id],
            |row| row.get::<_, i64>(0),
        )?;
        if active > 0 {
            anyhow::bail!("program {id} still has running or verifying milestones");
        }
        if !program.status.is_terminal() && program.status != ProgramStatus::Draft {
            anyhow::bail!("only draft or terminal programs can be deleted");
        }
        let payload = serde_json::json!({"id": id});
        insert_program_event_tx(&tx, &program.project_id, id, "program.deleted", &payload)?;
        tx.execute("DELETE FROM programs WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn create_program_milestone(
        &self,
        input: &NewProgramMilestone,
    ) -> Result<(ProgramMilestone, ProgramEvent)> {
        let now = now_rfc3339();
        let id = Uuid::now_v7().to_string();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![input.program_id],
                map_program,
            )
            .optional()?
        else {
            anyhow::bail!("program not found: {}", input.program_id);
        };
        if program.archived || program.status.is_terminal() {
            anyhow::bail!("program {} is archived or terminal", program.id);
        }
        if tx.query_row(
            "SELECT COUNT(*) FROM program_milestones WHERE program_id = ?1 AND key = ?2",
            params![input.program_id, input.key],
            |row| row.get::<_, i64>(0),
        )? > 0
        {
            anyhow::bail!("milestone key '{}' already exists", input.key);
        }
        let milestone = ProgramMilestone {
            id,
            program_id: input.program_id.clone(),
            key: input.key.clone(),
            title: input.title.clone(),
            objective: input.objective.clone(),
            acceptance: input.acceptance.clone(),
            deps: input.deps.clone(),
            priority: input.priority,
            status: ProgramMilestoneStatus::Pending,
            session_id: None,
            plan_revision: None,
            attempts: 0,
            max_attempts: input.max_attempts,
            next_attempt_at: None,
            result: None,
            last_error: None,
            profile: input.profile.clone(),
            write_scope: input.write_scope.clone(),
            token_budget: input.token_budget,
            cost_budget_usd: input.cost_budget_usd,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        };
        let payload = serde_json::to_value(&milestone)?;
        let event = insert_program_event_tx(
            &tx,
            &program.project_id,
            &program.id,
            "program.milestone.created",
            &payload,
        )?;
        tx.execute(
            "INSERT INTO program_milestones
             (id, program_id, key, title, acceptance, deps, priority, status, session_id,
              plan_revision, attempts, max_attempts, next_attempt_at, result, last_error,
              write_scope, objective, profile, token_budget, cost_budget_usd, created_at,
              updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, 0, ?9, NULL, NULL, NULL,
                     ?10, ?11, ?12, ?13, ?14, ?15, ?16, NULL)",
            params![
                milestone.id,
                milestone.program_id,
                milestone.key,
                milestone.title,
                serde_json::to_string(&milestone.acceptance)?,
                serde_json::to_string(&milestone.deps)?,
                milestone.priority,
                milestone.status.as_str(),
                milestone.max_attempts as i64,
                serde_json::to_string(&milestone.write_scope)?,
                milestone.objective,
                milestone.profile,
                milestone.token_budget.map(|value| value as i64),
                milestone.cost_budget_usd,
                milestone.created_at,
                milestone.updated_at,
            ],
        )?;
        tx.commit()?;
        Ok((milestone, event))
    }

    pub fn update_program_milestone(
        &self,
        id: &str,
        update: &ProgramMilestoneUpdate,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if matches!(
            milestone.status,
            ProgramMilestoneStatus::Running
                | ProgramMilestoneStatus::Verifying
                | ProgramMilestoneStatus::Completed
        ) {
            anyhow::bail!(
                "milestone {id} is {} and cannot be edited",
                milestone.status
            );
        }
        if tx.query_row(
            "SELECT COUNT(*) FROM program_milestones
                 WHERE program_id = ?1 AND key = ?2 AND id <> ?3",
            params![milestone.program_id, update.key, id],
            |row| row.get::<_, i64>(0),
        )? > 0
        {
            anyhow::bail!("milestone key '{}' already exists", update.key);
        }
        milestone.key = update.key.clone();
        milestone.title = update.title.clone();
        milestone.objective = update.objective.clone();
        milestone.acceptance = update.acceptance.clone();
        milestone.deps = update.deps.clone();
        milestone.priority = update.priority;
        milestone.max_attempts = update.max_attempts;
        milestone.profile = update.profile.clone();
        milestone.write_scope = update.write_scope.clone();
        milestone.token_budget = update.token_budget;
        milestone.cost_budget_usd = update.cost_budget_usd;
        milestone.updated_at = now;
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.updated",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET key = ?2, title = ?3, objective = ?4, acceptance = ?5, deps = ?6,
                 priority = ?7, max_attempts = ?8, profile = ?9, write_scope = ?10,
                 token_budget = ?11, cost_budget_usd = ?12, updated_at = ?13
             WHERE id = ?1",
            params![
                id,
                milestone.key,
                milestone.title,
                milestone.objective,
                serde_json::to_string(&milestone.acceptance)?,
                serde_json::to_string(&milestone.deps)?,
                milestone.priority,
                milestone.max_attempts as i64,
                milestone.profile,
                serde_json::to_string(&milestone.write_scope)?,
                milestone.token_budget.map(|value| value as i64),
                milestone.cost_budget_usd,
                milestone.updated_at,
            ],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn mark_program_milestone_ready(
        &self,
        id: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if !matches!(
            milestone.status,
            ProgramMilestoneStatus::Pending | ProgramMilestoneStatus::Failed
        ) || milestone.attempts >= milestone.max_attempts
        {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Ready;
        milestone.next_attempt_at = None;
        milestone.updated_at = now;
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.updated",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET status = 'ready', next_attempt_at = NULL, updated_at = ?2
             WHERE id = ?1",
            params![id, milestone.updated_at],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    /// Atomically claim a milestone for dispatch. The caller creates the
    /// session only after this succeeds, so two scheduler ticks cannot launch
    /// duplicate sessions for the same milestone.
    pub fn claim_program_milestone_start(
        &self,
        id: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = OffsetDateTime::now_utc();
        let now_text = now.format(&Rfc3339)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(program) = tx
            .query_row(
                "SELECT p.id, p.project_id, p.status, p.archived
                 FROM programs p
                 JOIN program_milestones m ON m.program_id = p.id
                 WHERE m.id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)? != 0,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.2 != ProgramStatus::Active.as_str() || program.3 {
            return Ok(None);
        }
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let retry_due = milestone
            .next_attempt_at
            .as_deref()
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
            .is_some_and(|next| next <= now);
        let runnable = matches!(
            milestone.status,
            ProgramMilestoneStatus::Pending
                | ProgramMilestoneStatus::Ready
                | ProgramMilestoneStatus::Failed
        );
        if !runnable || milestone.attempts >= milestone.max_attempts {
            return Ok(None);
        }
        if milestone.status == ProgramMilestoneStatus::Failed
            && milestone.next_attempt_at.is_some()
            && !retry_due
        {
            return Ok(None);
        }
        let statuses = tx
            .prepare("SELECT id, status FROM program_milestones WHERE program_id = ?1")?
            .query_map(params![milestone.program_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        let deps_ready = milestone.deps.iter().all(|dep| {
            statuses.get(dep).map(String::as_str)
                == Some(ProgramMilestoneStatus::Completed.as_str())
        });
        if !deps_ready {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Running;
        milestone.session_id = None;
        milestone.plan_revision = None;
        milestone.attempts = milestone.attempts.saturating_add(1);
        milestone.next_attempt_at = None;
        milestone.result = None;
        milestone.last_error = None;
        milestone.updated_at = now_text;
        milestone.completed_at = None;
        let payload = serde_json::to_value(&milestone)?;
        let event = insert_program_event_tx(
            &tx,
            &program.1,
            &milestone.program_id,
            "program.milestone.started",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET status = 'running', session_id = NULL, plan_revision = NULL,
                 attempts = ?2, next_attempt_at = NULL, result = NULL, last_error = NULL,
                 completed_at = NULL, updated_at = ?3
             WHERE id = ?1",
            params![id, milestone.attempts as i64, milestone.updated_at],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn attach_program_milestone_session(
        &self,
        id: &str,
        session_id: &str,
        plan_revision: u32,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if milestone.status != ProgramMilestoneStatus::Running {
            anyhow::bail!("milestone {id} is not running");
        }
        milestone.session_id = Some(session_id.to_string());
        milestone.plan_revision = Some(plan_revision.max(1));
        milestone.updated_at = now_rfc3339();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.updated",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET session_id = ?2, plan_revision = ?3, updated_at = ?4
             WHERE id = ?1",
            params![id, session_id, plan_revision as i64, milestone.updated_at],
        )?;
        // The link ledger is written in the same transaction as the event and
        // projection. It survives retries, so program budgets keep counting
        // every attempt instead of only the current session.
        tx.execute(
            "INSERT OR IGNORE INTO program_milestone_sessions
             (program_id, milestone_id, session_id, attempt, dispatched_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                milestone.program_id,
                id,
                session_id,
                milestone.attempts as i64,
                milestone.updated_at,
            ],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn mark_program_milestone_verifying(
        &self,
        id: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if milestone.status != ProgramMilestoneStatus::Running {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Verifying;
        milestone.updated_at = now_rfc3339();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.verified",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones SET status = 'verifying', updated_at = ?2 WHERE id = ?1",
            params![id, milestone.updated_at],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn complete_program_milestone(
        &self,
        id: &str,
        result: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if !matches!(
            milestone.status,
            ProgramMilestoneStatus::Verifying | ProgramMilestoneStatus::Running
        ) {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Completed;
        milestone.result = Some(result.trim().chars().take(8_000).collect());
        milestone.last_error = None;
        milestone.next_attempt_at = None;
        milestone.completed_at = Some(now.clone());
        milestone.updated_at = now.clone();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.completed",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET status = 'completed', result = ?2, last_error = NULL, next_attempt_at = NULL,
                 completed_at = ?3, updated_at = ?3
             WHERE id = ?1",
            params![id, milestone.result, now],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    /// Record a failed attempt. When `next_attempt_at` is set the row stays
    /// `failed` until the backoff becomes due; otherwise the caller follows up
    /// with `block_program_milestone` so the milestone becomes permanently
    /// terminal.
    pub fn fail_program_milestone(
        &self,
        id: &str,
        reason: &str,
        next_attempt_at: Option<&str>,
    ) -> Result<Option<(ProgramMilestone, Vec<ProgramEvent>)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if matches!(
            milestone.status,
            ProgramMilestoneStatus::Completed
                | ProgramMilestoneStatus::Blocked
                | ProgramMilestoneStatus::Cancelled
        ) {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Failed;
        milestone.last_error = Some(reason.trim().chars().take(4_000).collect());
        milestone.next_attempt_at = next_attempt_at.map(str::to_string);
        milestone.result = None;
        milestone.completed_at = None;
        milestone.updated_at = now.clone();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let mut events = vec![insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.failed",
            &payload,
        )?];
        if next_attempt_at.is_some() {
            events.push(insert_program_event_tx(
                &tx,
                &project_id,
                &milestone.program_id,
                "program.milestone.retried",
                &payload,
            )?);
        }
        tx.execute(
            "UPDATE program_milestones
             SET status = 'failed', last_error = ?2, next_attempt_at = ?3,
                 result = NULL, completed_at = NULL, updated_at = ?4
             WHERE id = ?1",
            params![id, milestone.last_error, next_attempt_at, now],
        )?;
        tx.commit()?;
        Ok(Some((milestone, events)))
    }

    pub fn block_program_milestone(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        self.set_program_milestone_terminal(
            id,
            ProgramMilestoneStatus::Blocked,
            reason,
            "program.milestone.blocked",
        )
    }

    pub fn cancel_program_milestone(
        &self,
        id: &str,
        reason: &str,
        skipped: bool,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        self.set_program_milestone_terminal(
            id,
            ProgramMilestoneStatus::Cancelled,
            reason,
            if skipped {
                "program.milestone.skipped"
            } else {
                "program.milestone.cancelled"
            },
        )
    }

    fn set_program_milestone_terminal(
        &self,
        id: &str,
        status: ProgramMilestoneStatus,
        reason: &str,
        kind: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if matches!(
            milestone.status,
            ProgramMilestoneStatus::Completed | ProgramMilestoneStatus::Cancelled
        ) {
            return Ok(None);
        }
        milestone.status = status;
        milestone.last_error = Some(reason.trim().chars().take(4_000).collect());
        milestone.next_attempt_at = None;
        milestone.completed_at = Some(now.clone());
        milestone.updated_at = now.clone();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event =
            insert_program_event_tx(&tx, &project_id, &milestone.program_id, kind, &payload)?;
        tx.execute(
            "UPDATE program_milestones
             SET status = ?2, last_error = ?3, next_attempt_at = NULL,
                 completed_at = ?4, updated_at = ?4
             WHERE id = ?1",
            params![id, status.as_str(), milestone.last_error, now],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    /// Explicit operator retry. Resets the automatic attempt budget but keeps
    /// the configured maximum; the next controller tick revalidates every
    /// dependency, budget, and admission gate.
    pub fn retry_program_milestone(
        &self,
        id: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if !matches!(
            milestone.status,
            ProgramMilestoneStatus::Failed
                | ProgramMilestoneStatus::Blocked
                | ProgramMilestoneStatus::Cancelled
        ) {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Pending;
        milestone.attempts = 0;
        milestone.next_attempt_at = None;
        milestone.last_error = None;
        milestone.result = None;
        milestone.session_id = None;
        milestone.plan_revision = None;
        milestone.completed_at = None;
        milestone.updated_at = now.clone();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.retried",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET status = 'pending', attempts = 0, next_attempt_at = NULL, last_error = NULL,
                 result = NULL, session_id = NULL, plan_revision = NULL, completed_at = NULL,
                 updated_at = ?2
             WHERE id = ?1",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn list_programs_for_session(&self, session_id: &str) -> Result<Vec<Program>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT {}
             FROM programs p
             JOIN program_milestones m ON m.program_id = p.id
             WHERE m.session_id = ?1
                OR EXISTS (
                    SELECT 1 FROM program_milestone_sessions l
                    WHERE l.milestone_id = m.id AND l.session_id = ?1
                )
             ORDER BY p.created_at ASC, p.id ASC",
            PROGRAM_COLUMNS
                .split(", ")
                .map(|column| format!("p.{column}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![session_id], map_program)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn append_program_event(
        &self,
        project_id: &str,
        program_id: &str,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<ProgramEvent> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let event = insert_program_event_tx(&tx, project_id, program_id, kind, payload)?;
        tx.commit()?;
        Ok(event)
    }

    pub fn list_program_events(
        &self,
        program_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ProgramEvent>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(1_000).clamp(1, 10_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT program_id, project_id, seq, kind, payload, created_at
             FROM program_events
             WHERE program_id = ?1
             ORDER BY seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![program_id, limit], map_program_event)?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        events.reverse();
        Ok(events)
    }

    pub fn list_program_events_for_project(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ProgramEvent>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(10_000).clamp(1, 100_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT program_id, project_id, seq, kind, payload, created_at
             FROM program_events
             WHERE project_id = ?1
             ORDER BY created_at ASC, program_id ASC, seq ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_id, limit], map_program_event)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn program_usage(&self, program_id: &str) -> Result<ProgramUsage> {
        let conn = self.lock()?;
        let (tokens, cost) = conn.query_row(
            "SELECT COALESCE(SUM(s.tokens_in + s.tokens_out), 0),
                    COALESCE(SUM(s.cost_spent_usd), 0.0)
             FROM sessions s
             WHERE s.id IN (
                 SELECT session_id FROM program_milestone_sessions WHERE program_id = ?1
                 UNION
                 SELECT session_id FROM program_milestones
                 WHERE program_id = ?1 AND session_id IS NOT NULL
             )",
            params![program_id],
            |row| Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, f64>(1)?)),
        )?;
        Ok(ProgramUsage {
            token_spent: tokens,
            cost_spent_usd: cost,
        })
    }

    /// Rebuild the append-only dispatch ledger from program events. This is
    /// idempotent and is used by bundle import and `db-rebuild`; live attaches
    /// write the same rows transactionally. `program.milestone.updated` is the
    /// attach event and carries the full milestone row.
    pub fn rebuild_program_session_links(&self, program_id: &str) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let inserted = Self::rebuild_program_session_links_tx(&tx, program_id)?;
        tx.commit()?;
        Ok(inserted)
    }

    fn rebuild_program_session_links_tx(tx: &Transaction<'_>, program_id: &str) -> Result<usize> {
        let mut inserted = 0usize;
        {
            let mut stmt = tx.prepare(
                "SELECT created_at, payload FROM program_events
                 WHERE program_id = ?1 AND kind = 'program.milestone.updated'
                 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![program_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (created_at, payload) = row?;
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    continue;
                };
                let Some(session_id) = value.get("session_id").and_then(|value| value.as_str())
                else {
                    continue;
                };
                let Some(milestone_id) = value.get("id").and_then(|value| value.as_str()) else {
                    continue;
                };
                let attempt = value
                    .get("attempts")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .min(i64::MAX as u64) as i64;
                inserted += tx.execute(
                    "INSERT OR IGNORE INTO program_milestone_sessions
                     (program_id, milestone_id, session_id, attempt, dispatched_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![program_id, milestone_id, session_id, attempt, created_at],
                )?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, attempts, updated_at FROM program_milestones
                 WHERE program_id = ?1 AND session_id IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![program_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (milestone_id, session_id, attempt, updated_at) = row?;
                inserted += tx.execute(
                    "INSERT OR IGNORE INTO program_milestone_sessions
                     (program_id, milestone_id, session_id, attempt, dispatched_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![program_id, milestone_id, session_id, attempt, updated_at],
                )?;
            }
        }
        Ok(inserted)
    }

    pub fn count_active_program_milestones(&self, program_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM program_milestones
             WHERE program_id = ?1 AND status IN ('running', 'verifying')",
            params![program_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u32)
    }

    pub fn insert_program_snapshot(&self, program: &Program) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO programs
             (id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason,
              archived, created_at, updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                goal = excluded.goal,
                status = excluded.status,
                policy = excluded.policy,
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                reason = excluded.reason,
                archived = excluded.archived,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at",
            params![
                program.id,
                program.project_id,
                program.goal,
                program.status.as_str(),
                serde_json::to_string(&program.policy)?,
                program.token_budget.map(|value| value as i64),
                program.cost_budget_usd,
                program.reason,
                if program.archived { 1 } else { 0 },
                program.created_at,
                program.updated_at,
                program.completed_at,
            ],
        )?;
        Ok(())
    }

    pub fn insert_program_milestone_snapshot(&self, milestone: &ProgramMilestone) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO program_milestones
             (id, program_id, key, title, acceptance, deps, priority, status, session_id,
              plan_revision, attempts, max_attempts, next_attempt_at, result, last_error,
              write_scope, objective, profile, token_budget, cost_budget_usd, created_at,
              updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
             ON CONFLICT(id) DO UPDATE SET
                key = excluded.key,
                title = excluded.title,
                acceptance = excluded.acceptance,
                deps = excluded.deps,
                priority = excluded.priority,
                status = excluded.status,
                session_id = excluded.session_id,
                plan_revision = excluded.plan_revision,
                attempts = excluded.attempts,
                max_attempts = excluded.max_attempts,
                next_attempt_at = excluded.next_attempt_at,
                result = excluded.result,
                last_error = excluded.last_error,
                write_scope = excluded.write_scope,
                objective = excluded.objective,
                profile = excluded.profile,
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at",
            params![
                milestone.id,
                milestone.program_id,
                milestone.key,
                milestone.title,
                serde_json::to_string(&milestone.acceptance)?,
                serde_json::to_string(&milestone.deps)?,
                milestone.priority,
                milestone.status.as_str(),
                milestone.session_id,
                milestone.plan_revision.map(|value| value as i64),
                milestone.attempts as i64,
                milestone.max_attempts as i64,
                milestone.next_attempt_at,
                milestone.result,
                milestone.last_error,
                serde_json::to_string(&milestone.write_scope)?,
                milestone.objective,
                milestone.profile,
                milestone.token_budget.map(|value| value as i64),
                milestone.cost_budget_usd,
                milestone.created_at,
                milestone.updated_at,
                milestone.completed_at,
            ],
        )?;
        Ok(())
    }

    pub fn insert_program_event_snapshot(&self, event: &ProgramEvent) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO program_events
             (project_id, program_id, seq, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.project_id,
                event.program_id,
                event.seq as i64,
                event.kind,
                serde_json::to_string(&event.payload)?,
                event.created_at,
            ],
        )?;
        Ok(())
    }

    /// Replace the whole program projection with an event-derived expectation.
    /// This is used by explicit replay repair and never appends new events.
    pub fn replace_program_projection(
        &self,
        program_id: &str,
        program: Option<&Program>,
        milestones: &[ProgramMilestone],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let expected_ids = milestones
            .iter()
            .map(|milestone| milestone.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let existing = tx
            .prepare("SELECT id FROM program_milestones WHERE program_id = ?1")?
            .query_map(params![program_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in existing {
            if !expected_ids.contains(&id) {
                tx.execute("DELETE FROM program_milestones WHERE id = ?1", params![id])?;
            }
        }
        match program {
            Some(program) => {
                tx.execute(
                    "INSERT INTO programs
                     (id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason,
                      archived, created_at, updated_at, completed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                     ON CONFLICT(id) DO UPDATE SET
                        project_id = excluded.project_id,
                        goal = excluded.goal,
                        status = excluded.status,
                        policy = excluded.policy,
                        token_budget = excluded.token_budget,
                        cost_budget_usd = excluded.cost_budget_usd,
                        reason = excluded.reason,
                        archived = excluded.archived,
                        updated_at = excluded.updated_at,
                        completed_at = excluded.completed_at",
                    params![
                        program.id,
                        program.project_id,
                        program.goal,
                        program.status.as_str(),
                        serde_json::to_string(&program.policy)?,
                        program.token_budget.map(|value| value as i64),
                        program.cost_budget_usd,
                        program.reason,
                        if program.archived { 1 } else { 0 },
                        program.created_at,
                        program.updated_at,
                        program.completed_at,
                    ],
                )?;
                for milestone in milestones {
                    Self::insert_program_milestone_tx(&tx, milestone)?;
                }
            }
            None => {
                tx.execute("DELETE FROM programs WHERE id = ?1", params![program_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn insert_program_milestone_tx(
        tx: &Transaction<'_>,
        milestone: &ProgramMilestone,
    ) -> Result<()> {
        tx.execute(
            "INSERT INTO program_milestones
             (id, program_id, key, title, acceptance, deps, priority, status, session_id,
              plan_revision, attempts, max_attempts, next_attempt_at, result, last_error,
              write_scope, objective, profile, token_budget, cost_budget_usd, created_at,
              updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
             ON CONFLICT(id) DO UPDATE SET
                key = excluded.key,
                title = excluded.title,
                acceptance = excluded.acceptance,
                deps = excluded.deps,
                priority = excluded.priority,
                status = excluded.status,
                session_id = excluded.session_id,
                plan_revision = excluded.plan_revision,
                attempts = excluded.attempts,
                max_attempts = excluded.max_attempts,
                next_attempt_at = excluded.next_attempt_at,
                result = excluded.result,
                last_error = excluded.last_error,
                write_scope = excluded.write_scope,
                objective = excluded.objective,
                profile = excluded.profile,
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at",
            params![
                milestone.id,
                milestone.program_id,
                milestone.key,
                milestone.title,
                serde_json::to_string(&milestone.acceptance)?,
                serde_json::to_string(&milestone.deps)?,
                milestone.priority,
                milestone.status.as_str(),
                milestone.session_id,
                milestone.plan_revision.map(|value| value as i64),
                milestone.attempts as i64,
                milestone.max_attempts as i64,
                milestone.next_attempt_at,
                milestone.result,
                milestone.last_error,
                serde_json::to_string(&milestone.write_scope)?,
                milestone.objective,
                milestone.profile,
                milestone.token_budget.map(|value| value as i64),
                milestone.cost_budget_usd,
                milestone.created_at,
                milestone.updated_at,
                milestone.completed_at,
            ],
        )?;
        Ok(())
    }

    const SOURCE_COLUMNS: &'static str = "id, project_id, kind, title, uri, mime_type, status, \
        size_bytes, sha256, revision, chunk_count, char_count, token_count, error, labels, \
        created_at, updated_at, indexed_at, collection";

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
                    (SELECT COUNT(*) FROM usage_ledger u WHERE u.session_id = s.id),
                    COALESCE((SELECT SUM(cache_read_tokens) FROM usage_ledger u
                              WHERE u.session_id = s.id), 0),
                    COALESCE((SELECT SUM(cache_write_tokens) FROM usage_ledger u
                              WHERE u.session_id = s.id), 0)
             FROM sessions s
             WHERE s.project_id = ?1
             ORDER BY s.created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            let input_tokens = row.get::<_, i64>(3)? as u64;
            let cache_read_tokens = row.get::<_, i64>(7)? as u64;
            let cache_write_tokens = row.get::<_, i64>(8)? as u64;
            Ok(ProjectUsageSession {
                session_id: row.get(0)?,
                goal: row.get(1)?,
                status: row.get(2)?,
                calls: row.get::<_, i64>(6)? as u64,
                input_tokens,
                output_tokens: row.get::<_, i64>(4)? as u64,
                cache_read_tokens,
                cache_write_tokens,
                cache_hit_ratio: cache_hit_ratio(
                    input_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                ),
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

    /// Compatibility shim for callers that predate evidence-backed memory.
    /// Delegates with source `agent`, no evidence, and dedupe enabled. When the
    /// create is idempotent no event is written, so the existing
    /// `memory.created` event is returned if one exists (or a non-persisted
    /// payload-only placeholder for imported rows).
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
        let outcome = self.create_memory_with_metadata_event(NewMemory {
            session_id,
            project_id,
            agent_id,
            agent_name,
            kind,
            title,
            body,
            tags,
            source: "agent",
            evidence: &[],
            pinned: false,
            verify_after: None,
            dedupe: true,
            auto_supersede: true,
        })?;
        let event = outcome
            .events
            .iter()
            .find(|event| event.kind == "memory.created")
            .cloned()
            .or_else(|| outcome.events.into_iter().next())
            .unwrap_or_else(|| Event {
                session_id: outcome.memory.session_id.clone(),
                seq: 0,
                kind: "memory.created".to_string(),
                payload: serde_json::to_value(&outcome.memory).unwrap_or_default(),
                created_at: outcome.memory.created_at.clone(),
            });
        Ok((outcome.memory, event))
    }

    /// Create a memory with provenance in one transaction. A normalized
    /// (kind, title) key may have at most one active row: an exact duplicate is
    /// idempotent, and conflicting content supersedes the previous row when
    /// both `dedupe` and `auto_supersede` are set.
    pub fn create_memory_with_metadata_event(
        &self,
        new: NewMemory<'_>,
    ) -> Result<MemoryCreateOutcome> {
        let new_id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let mut supersedes = Vec::new();
        let mut events = Vec::new();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if new.dedupe
            && let Some(mut existing) =
                active_memory_for_key_tx(&tx, new.project_id, new.kind, new.title)?
        {
            let same_content = existing.body.trim() == new.body.trim()
                && existing.evidence == new.evidence
                && existing.source == new.source;
            if same_content || !new.auto_supersede {
                tx.commit()?;
                return Ok(MemoryCreateOutcome {
                    memory: existing,
                    created: false,
                    events: Vec::new(),
                });
            }
            existing.status = "superseded".to_string();
            existing.superseded_by = Some(new_id.clone());
            update_memory_projection_tx(&tx, &existing)?;
            let event = insert_event_tx(
                &tx,
                &existing.session_id,
                "memory.superseded",
                serde_json::to_value(&existing)?,
            )?;
            supersedes.push(existing.id.clone());
            events.push(event);
        }
        let memory = MemoryItem {
            id: new_id,
            session_id: new.session_id.to_string(),
            project_id: new.project_id.to_string(),
            agent_id: new.agent_id.to_string(),
            agent_name: new.agent_name.to_string(),
            kind: new.kind.to_string(),
            title: new.title.to_string(),
            body: new.body.to_string(),
            tags: new.tags.to_vec(),
            created_at: now,
            status: "active".to_string(),
            source: new.source.to_string(),
            evidence: new.evidence.to_vec(),
            supersedes,
            superseded_by: None,
            pinned: new.pinned,
            use_count: 0,
            last_used_at: None,
            last_verified_at: None,
            verify_after: new.verify_after.map(str::to_string),
            stale_reason: None,
        };
        insert_memory_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.created",
            serde_json::to_value(&memory)?,
        )?;
        events.push(event);
        tx.commit()?;
        drop(conn);
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(MemoryCreateOutcome {
            memory,
            created: true,
            events,
        })
    }

    pub fn list_memories(
        &self,
        project_id: &str,
        query: Option<&str>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        self.list_memories_filtered(project_id, query, kind, None, false, limit)
    }

    /// Active memories by default; `stale` only when `include_stale` is set.
    /// An exact `status` filter takes precedence over `include_stale`.
    #[allow(clippy::too_many_arguments)]
    pub fn list_memories_filtered(
        &self,
        project_id: &str,
        query: Option<&str>,
        kind: Option<&str>,
        status: Option<&str>,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        if let Some(query) = query
            && let Some(query) = fts_query(query)
        {
            match self.list_memories_fts(project_id, &query, kind, status, include_stale, limit) {
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
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories m
             WHERE m.project_id = ?1
               AND (?2 IS NULL OR m.title LIKE ?2 OR m.body LIKE ?2 OR m.tags LIKE ?2)
               AND (?3 IS NULL OR m.kind = ?3)
               AND (
                   (?4 IS NOT NULL AND m.status = ?4)
                   OR (?4 IS NULL AND (m.status = 'active' OR (?5 = 1 AND m.status = 'stale')))
               )
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                pattern,
                kind,
                status,
                if include_stale { 1 } else { 0 },
                limit.clamp(1, 500) as i64
            ],
            map_memory_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    fn list_memories_fts(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        status: Option<&str>,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason
             FROM memories_fts
             JOIN memories m ON m.rowid = memories_fts.rowid
             WHERE memories_fts MATCH ?1
               AND m.project_id = ?2
               AND (?3 IS NULL OR m.kind = ?3)
               AND (
                   (?4 IS NOT NULL AND m.status = ?4)
                   OR (?4 IS NULL AND (m.status = 'active' OR (?5 = 1 AND m.status = 'stale')))
               )
             ORDER BY bm25(memories_fts), m.created_at DESC, m.id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                query,
                project_id,
                kind,
                status,
                if include_stale { 1 } else { 0 },
                limit.clamp(1, 500) as i64
            ],
            map_memory_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Ranked candidates for deterministic retrieval. Up to 200 rows; status
    /// filtering matches `list_memories_filtered`.
    #[allow(clippy::too_many_arguments)]
    pub fn memory_candidates(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        kinds: &[String],
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryCandidate>> {
        let mut candidates = Vec::new();
        if let Some(fts) = fts_query(query) {
            match self.memory_candidates_fts(project_id, &fts, kind, include_stale, limit) {
                Ok(found) if !found.is_empty() => candidates = found,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "memory candidate FTS search failed; falling back to LIKE");
                }
            }
        }
        if candidates.is_empty() {
            candidates =
                self.memory_candidates_like(project_id, query, kind, include_stale, limit)?;
        }
        if !kinds.is_empty() {
            candidates.retain(|candidate| kinds.iter().any(|kind| kind == &candidate.memory.kind));
        }
        candidates.truncate(limit.clamp(1, 200) as usize);
        Ok(candidates)
    }

    fn memory_candidates_fts(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryCandidate>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    bm25(memories_fts, 3.0, 1.0, 2.0) AS rank
             FROM memories_fts
             JOIN memories m ON m.rowid = memories_fts.rowid
             WHERE memories_fts MATCH ?1
               AND m.project_id = ?2
               AND (?3 IS NULL OR m.kind = ?3)
               AND (m.status = 'active' OR (?4 = 1 AND m.status = 'stale'))
             ORDER BY rank ASC, m.created_at DESC, m.id ASC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                query,
                project_id,
                kind,
                if include_stale { 1 } else { 0 },
                limit.clamp(1, 200) as i64
            ],
            |row| {
                let rank: f64 = row.get(21)?;
                let relevance = (-rank).max(0.0);
                Ok(MemoryCandidate {
                    memory: map_memory_row(row)?,
                    lexical: if relevance <= 0.0 {
                        0.01
                    } else {
                        relevance / (1.0 + relevance)
                    },
                    fts: true,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn memory_candidates_like(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryCandidate>> {
        let conn = self.lock()?;
        let pattern = format!("%{}%", query.trim());
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories m
             WHERE m.project_id = ?1
               AND (?2 = '%%' OR m.title LIKE ?2 OR m.body LIKE ?2 OR m.tags LIKE ?2)
               AND (?3 IS NULL OR m.kind = ?3)
               AND (m.status = 'active' OR (?4 = 1 AND m.status = 'stale'))
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                pattern,
                kind,
                if include_stale { 1 } else { 0 },
                limit.clamp(1, 200) as i64
            ],
            |row| {
                let memory = map_memory_row(row)?;
                Ok(MemoryCandidate {
                    lexical: like_memory_lexical(&memory, query),
                    memory,
                    fts: false,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_memory(&self, id: &str) -> Result<Option<MemoryItem>> {
        let conn = self.lock()?;
        get_memory_tx(&conn, id)
    }

    /// Structural supplement for `memory.search`: active/stale rows whose task
    /// evidence or tag exactly matches `task_id`, even when the lexical query
    /// does not. This keeps task-specific memories eligible for the structural
    /// boost in ranking.
    pub fn memory_task_candidates(
        &self,
        project_id: &str,
        task_id: &str,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("%{}%", escape_like(task_id));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories m
             WHERE m.project_id = ?1
               AND (m.status = 'active' OR (?2 = 1 AND m.status = 'stale'))
               AND (m.tags LIKE ?3 ESCAPE '\\' OR m.evidence LIKE ?3 ESCAPE '\\')
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                if include_stale { 1 } else { 0 },
                pattern,
                limit.clamp(1, 200) as i64
            ],
            map_memory_row,
        )?;
        let mut candidates =
            rows.collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|memory| {
                    memory.evidence.iter().any(|evidence| {
                        evidence.kind == "task" && evidence.reference.trim() == task_id
                    }) || memory.tags.iter().any(|tag| tag.trim() == task_id)
                })
                .collect::<Vec<_>>();
        candidates.truncate(limit.clamp(1, 200) as usize);
        Ok(candidates)
    }

    pub fn list_session_memories(&self, session_id: &str) -> Result<Vec<MemoryItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_memory_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn replace_session_memories(
        &self,
        session_id: &str,
        memories: &[MemoryItem],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_session_memories_tx(&tx, session_id, memories)?;
        tx.commit()?;
        Ok(())
    }

    pub fn replace_session_memory_conflicts(
        &self,
        session_id: &str,
        conflicts: &[MemoryConflict],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_session_memory_conflicts_tx(&tx, session_id, conflicts)?;
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
            let Some(memory) = get_memory_tx(&tx, id)? else {
                continue;
            };
            // A conflict cannot be resolved once either side is gone. The
            // `memory.deleted` event removes the memory from replay, and this
            // tombstone keeps the conflict projection consistent too.
            tx.execute(
                "DELETE FROM memory_conflicts WHERE memory_a_id = ?1 OR memory_b_id = ?1",
                params![id],
            )?;
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

    /// Full-item update. `memory.updated` carries the entire updated row so
    /// replay is last-write-wins.
    pub fn update_memory_with_event(
        &self,
        id: &str,
        status: Option<&str>,
        pinned: Option<bool>,
        tags: Option<&[String]>,
    ) -> Result<Option<(MemoryItem, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut memory) = get_memory_tx(&tx, id)? else {
            return Ok(None);
        };
        if let Some(status) = status {
            memory.status = status.to_string();
            if status == "stale" {
                if memory.stale_reason.is_none() {
                    memory.stale_reason = Some("operator marked stale".to_string());
                }
            } else {
                memory.stale_reason = None;
            }
        }
        if let Some(pinned) = pinned {
            memory.pinned = pinned;
        }
        if let Some(tags) = tags {
            memory.tags = tags.to_vec();
        }
        update_memory_projection_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.updated",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((memory, event)))
    }

    pub fn mark_memory_verified_with_event(
        &self,
        id: &str,
        verified_at: &str,
        verify_after: Option<&str>,
    ) -> Result<Option<(MemoryItem, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut memory) = get_memory_tx(&tx, id)? else {
            return Ok(None);
        };
        memory.status = "active".to_string();
        memory.stale_reason = None;
        memory.last_verified_at = Some(verified_at.to_string());
        memory.verify_after = verify_after.map(str::to_string);
        update_memory_projection_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.verified",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((memory, event)))
    }

    pub fn mark_memory_stale_with_event(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(MemoryItem, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut memory) = get_memory_tx(&tx, id)? else {
            return Ok(None);
        };
        memory.status = "stale".to_string();
        memory.stale_reason = Some(reason.to_string());
        update_memory_projection_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.stale",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((memory, event)))
    }

    /// Observed usage. Idempotent per (memory, session): replaying the same
    /// selection never increments `use_count` twice.
    pub fn record_memory_use_with_event(
        &self,
        id: &str,
        using_session_id: &str,
    ) -> Result<Option<(MemoryItem, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut memory) = get_memory_tx(&tx, id)? else {
            return Ok(None);
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO memory_usage (memory_id, session_id, used_at)
             VALUES (?1, ?2, ?3)",
            params![id, using_session_id, now_rfc3339()],
        )?;
        if inserted == 0 {
            tx.commit()?;
            return Ok(None);
        }
        memory.use_count = memory.use_count.saturating_add(1);
        memory.last_used_at = Some(now_rfc3339());
        update_memory_projection_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.used",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((memory, event)))
    }

    /// Active, non-pinned rows whose explicit TTL expired, or (when the
    /// override is supplied) rows older than the cutoff.
    pub fn memory_sweep_candidates(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
    ) -> Result<Vec<MemoryItem>> {
        let now = time::OffsetDateTime::now_utc();
        let cutoff = older_than_days.map(|days| now - time::Duration::days(i64::from(days)));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories
             WHERE project_id = ?1 AND status = 'active' AND pinned = 0
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], map_memory_row)?;
        let mut candidates = Vec::new();
        for row in rows {
            let memory = row?;
            let expired = memory.verify_after.as_deref().is_some_and(|verify_after| {
                parse_rfc3339(verify_after).is_some_and(|verify_after| verify_after < now)
            });
            let too_old = cutoff.is_some_and(|cutoff| {
                parse_rfc3339(&memory.created_at).is_some_and(|created_at| created_at < cutoff)
            });
            if expired || too_old {
                candidates.push(memory);
            }
        }
        Ok(candidates)
    }

    pub fn memory_stats(&self, project_id: &str) -> Result<MemoryStatsSnapshot> {
        let conn = self.lock()?;
        let total = conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(MemoryStatsSnapshot {
            total: u32::try_from(total).unwrap_or(u32::MAX),
            by_status: memory_count_group(&conn, project_id, "status")?,
            by_source: memory_count_group(&conn, project_id, "source")?,
            by_kind: memory_count_group(&conn, project_id, "kind")?,
        })
    }

    /// List deterministic memory conflicts. `status` filters exactly (`open` or
    /// `resolved`); without a limit the newest 500 are returned.
    pub fn list_memory_conflicts(
        &self,
        project_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<MemoryConflict>> {
        let conn = self.lock()?;
        list_memory_conflicts_conn(&conn, project_id, status, limit)
    }

    pub fn get_memory_conflict(&self, id: &str) -> Result<Option<MemoryConflict>> {
        let conn = self.lock()?;
        get_memory_conflict_tx(&conn, id)
    }

    pub fn list_session_memory_conflicts(&self, session_id: &str) -> Result<Vec<MemoryConflict>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMORY_CONFLICT_COLUMNS}
             FROM memory_conflicts WHERE session_id = ?1
             ORDER BY scope_kind ASC, scope_value ASC, memory_a_id ASC, memory_b_id ASC,
                      detected_at DESC, id ASC"
        ))?;
        let rows = stmt.query_map(params![session_id], map_memory_conflict_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Detect deterministic scope conflicts. New rows are inserted as `open`
    /// with one `memory.conflict.detected` event in the same transaction. An
    /// existing id (including a resolved row) is never recreated. Detection
    /// never mutates either memory.
    pub fn scan_memory_conflicts(&self, project_id: &str) -> Result<MemoryConflictScan> {
        let detected_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let memories = active_memories_with_evidence_tx(&tx, project_id, MAX_CONFLICT_MEMORIES)?;
        let candidates = detect_memory_conflicts(project_id, &memories, &detected_at);
        let mut scan = MemoryConflictScan::default();
        for conflict in candidates {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO memory_conflicts
                 (id, project_id, session_id, memory_a_id, memory_b_id, memory_a_title,
                  memory_b_title, memory_a_kind, memory_b_kind, memory_a_excerpt,
                  memory_b_excerpt, scope_kind, scope_value, reason, status, detected_at,
                  resolved_at, resolution, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19)",
                params![
                    &conflict.id,
                    &conflict.project_id,
                    &conflict.session_id,
                    &conflict.memory_a_id,
                    &conflict.memory_b_id,
                    &conflict.memory_a_title,
                    &conflict.memory_b_title,
                    &conflict.memory_a_kind,
                    &conflict.memory_b_kind,
                    &conflict.memory_a_excerpt,
                    &conflict.memory_b_excerpt,
                    &conflict.scope_kind,
                    &conflict.scope_value,
                    &conflict.reason,
                    &conflict.status,
                    &conflict.detected_at,
                    &conflict.resolved_at,
                    &conflict.resolution,
                    &conflict.note,
                ],
            )?;
            if inserted == 0 {
                continue;
            }
            let event = insert_event_tx(
                &tx,
                &conflict.session_id,
                "memory.conflict.detected",
                serde_json::to_value(&conflict)?,
            )?;
            scan.new.push(conflict);
            scan.events.push(event);
        }
        scan.open = list_memory_conflicts_conn(&tx, project_id, Some("open"), None)?;
        tx.commit()?;
        drop(conn);
        for event in &scan.events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(scan)
    }

    /// Resolve one conflict explicitly. The memory mutation, the conflict row,
    /// and every event commit atomically. `keep_both` changes no memory.
    pub fn resolve_memory_conflict(
        &self,
        project_id: &str,
        id: &str,
        action: &str,
        note: Option<&str>,
    ) -> Result<MemoryConflictResolution> {
        if !matches!(action, "keep_a" | "keep_b" | "keep_both" | "archive_both") {
            return Err(anyhow!(
                "memory conflict action must be keep_a, keep_b, keep_both, or archive_both"
            ));
        }
        let note = note
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(str::to_string);
        if note
            .as_deref()
            .is_some_and(|note| note.chars().count() > 2_000)
        {
            return Err(anyhow!(
                "memory conflict note is limited to 2000 characters"
            ));
        }
        let resolved_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut conflict) = get_memory_conflict_tx(&tx, id)? else {
            return Err(anyhow!(
                "memory conflict {id} not found: memory no longer exists or the conflict was removed with a deleted memory"
            ));
        };
        if conflict.project_id != project_id {
            return Err(anyhow!(
                "memory conflict {id} belongs to project {}, not {project_id}",
                conflict.project_id
            ));
        }
        if conflict.status == "resolved" {
            return Err(anyhow!("memory conflict {id} is already resolved"));
        }
        let Some(mut memory_a) = get_memory_tx(&tx, &conflict.memory_a_id)? else {
            return Err(anyhow!(
                "memory conflict {id} references memory {} which no longer exists",
                conflict.memory_a_id
            ));
        };
        let Some(mut memory_b) = get_memory_tx(&tx, &conflict.memory_b_id)? else {
            return Err(anyhow!(
                "memory conflict {id} references memory {} which no longer exists",
                conflict.memory_b_id
            ));
        };
        if memory_a.project_id != project_id || memory_b.project_id != project_id {
            return Err(anyhow!(
                "memory conflict {id} references memories from a different project"
            ));
        }

        let mut events = Vec::new();
        match action {
            "keep_a" => {
                memory_b.status = "superseded".to_string();
                memory_b.superseded_by = Some(memory_a.id.clone());
                update_memory_projection_tx(&tx, &memory_b)?;
                events.push(insert_event_tx(
                    &tx,
                    &memory_b.session_id,
                    "memory.superseded",
                    serde_json::to_value(&memory_b)?,
                )?);
            }
            "keep_b" => {
                memory_a.status = "superseded".to_string();
                memory_a.superseded_by = Some(memory_b.id.clone());
                update_memory_projection_tx(&tx, &memory_a)?;
                events.push(insert_event_tx(
                    &tx,
                    &memory_a.session_id,
                    "memory.superseded",
                    serde_json::to_value(&memory_a)?,
                )?);
            }
            "archive_both" => {
                for memory in [&mut memory_a, &mut memory_b] {
                    memory.status = "archived".to_string();
                    update_memory_projection_tx(&tx, memory)?;
                    events.push(insert_event_tx(
                        &tx,
                        &memory.session_id,
                        "memory.updated",
                        serde_json::to_value(&*memory)?,
                    )?);
                }
            }
            "keep_both" => {}
            _ => unreachable!("validated above"),
        }

        conflict.status = "resolved".to_string();
        conflict.resolved_at = Some(resolved_at);
        conflict.resolution = Some(action.to_string());
        conflict.note = note;
        update_memory_conflict_tx(&tx, &conflict)?;
        events.push(insert_event_tx(
            &tx,
            &conflict.session_id,
            "memory.conflict.resolved",
            serde_json::to_value(&conflict)?,
        )?);

        tx.commit()?;
        drop(conn);
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(MemoryConflictResolution {
            conflict,
            memories: vec![memory_a, memory_b],
            events,
        })
    }

    /// Operator-facing health snapshot. `verification_due` counts active,
    /// non-pinned rows whose explicit TTL has elapsed.
    pub fn memory_health(&self, project_id: &str) -> Result<MemoryHealthResult> {
        let now = time::OffsetDateTime::now_utc();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT kind, status, evidence, pinned, last_verified_at, verify_after
             FROM memories WHERE project_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut health = MemoryHealthResult {
            project_id: project_id.to_string(),
            total: 0,
            active: 0,
            stale: 0,
            superseded: 0,
            archived: 0,
            pinned: 0,
            evidence_covered: 0,
            verified: 0,
            verification_due: 0,
            open_conflicts: 0,
            by_kind: Vec::new(),
        };
        let mut kinds: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
        for row in rows {
            let (kind, status, evidence, pinned, last_verified_at, verify_after) = row?;
            health.total += 1;
            match status.as_str() {
                "active" => health.active += 1,
                "stale" => health.stale += 1,
                "superseded" => health.superseded += 1,
                "archived" => health.archived += 1,
                _ => {}
            }
            if pinned {
                health.pinned += 1;
            }
            if status == "active"
                && !serde_json::from_str::<Vec<MemoryEvidence>>(&evidence)
                    .unwrap_or_default()
                    .is_empty()
            {
                health.evidence_covered += 1;
            }
            if last_verified_at.is_some() {
                health.verified += 1;
            }
            if status == "active"
                && !pinned
                && verify_after.as_deref().is_some_and(|verify_after| {
                    parse_rfc3339(verify_after).is_some_and(|verify_after| verify_after < now)
                })
            {
                health.verification_due += 1;
            }
            *kinds.entry(kind).or_insert(0) += 1;
        }
        health.by_kind = kinds
            .into_iter()
            .map(|(key, count)| MemoryStatCount { key, count })
            .collect();
        let open_conflicts = conn.query_row(
            "SELECT COUNT(*) FROM memory_conflicts WHERE project_id = ?1 AND status = 'open'",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        health.open_conflicts = u32::try_from(open_conflicts).unwrap_or(u32::MAX);
        Ok(health)
    }

    pub fn plan_task_exists(&self, project_id: &str, task_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM plan_tasks WHERE project_id = ?1 AND task_id = ?2",
            params![project_id, task_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn event_exists(&self, session_id: &str, seq: u64) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND seq = ?2",
            params![session_id, seq as i64],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn latest_merged_entry_for_task(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash,
                    base_commit, pre_merge_commit, status, attempt, detail, created_at, updated_at
             FROM merge_queue
             WHERE session_id = ?1 AND task_id = ?2 AND status = 'merged'
             ORDER BY updated_at DESC, created_at DESC, id ASC
             LIMIT 1",
            params![session_id, task_id],
            map_merge_queue_entry,
        )
        .optional()
        .map_err(Into::into)
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
        // Pinning is explicit operator intent to retain a memory. Prune must
        // never silently delete a pinned row, and `keep_latest` counts only
        // unpinned rows so pinning one item cannot push another pinned item
        // out of the budget.
        let mut stmt = conn.prepare(
            "SELECT id, created_at FROM memories
             WHERE project_id = ?1 AND pinned = 0 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut ids = Vec::new();
        for (index, row) in rows.enumerate() {
            let (id, created_at) = row?;
            let beyond_keep = keep_latest.is_some_and(|keep| index >= keep as usize);
            let old = cutoff.is_some_and(|cutoff| {
                parse_rfc3339(&created_at).is_some_and(|created_at| created_at < cutoff)
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
                "SELECT enabled, max_items, max_age_days, task_result_verify_days,
                        default_verify_days, auto_supersede, brief_max_chars, sweep_stale
                 FROM project_memory_policies WHERE project_id = ?1",
                params![project_id],
                map_memory_policy_row,
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_memory_policy(&self, project_id: &str, policy: &MemoryPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_memory_policies
             (project_id, enabled, max_items, max_age_days, task_result_verify_days,
              default_verify_days, auto_supersede, brief_max_chars, sweep_stale, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                max_items = excluded.max_items,
                max_age_days = excluded.max_age_days,
                task_result_verify_days = excluded.task_result_verify_days,
                default_verify_days = excluded.default_verify_days,
                auto_supersede = excluded.auto_supersede,
                brief_max_chars = excluded.brief_max_chars,
                sweep_stale = excluded.sweep_stale,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.max_items.map(i64::from),
                policy.max_age_days.map(i64::from),
                optional_count_to_db(policy.task_result_verify_days),
                optional_count_to_db(policy.default_verify_days),
                if policy.auto_supersede { 1 } else { 0 },
                optional_count_to_db(policy.brief_max_chars),
                if policy.sweep_stale { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_memory_policies(&self) -> Result<Vec<(String, MemoryPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, max_items, max_age_days, task_result_verify_days,
                    default_verify_days, auto_supersede, brief_max_chars, sweep_stale
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
                    task_result_verify_days: optional_count_from_db(row.get::<_, i64>(4)?),
                    default_verify_days: optional_count_from_db(row.get::<_, i64>(5)?),
                    auto_supersede: row.get::<_, i64>(6)? != 0,
                    brief_max_chars: optional_count_from_db(row.get::<_, i64>(7)?),
                    sweep_stale: row.get::<_, i64>(8)? != 0,
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Projects that opted into daemon staleness sweeping. Independent of the
    /// retention (`enabled`) toggle.
    pub fn list_memory_sweep_policies(&self) -> Result<Vec<(String, MemoryPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, max_items, max_age_days, task_result_verify_days,
                    default_verify_days, auto_supersede, brief_max_chars, sweep_stale
             FROM project_memory_policies WHERE sweep_stale = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MemoryPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    max_items: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value > 0),
                    max_age_days: row
                        .get::<_, Option<i64>>(3)?
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value > 0),
                    task_result_verify_days: optional_count_from_db(row.get::<_, i64>(4)?),
                    default_verify_days: optional_count_from_db(row.get::<_, i64>(5)?),
                    auto_supersede: row.get::<_, i64>(6)? != 0,
                    brief_max_chars: optional_count_from_db(row.get::<_, i64>(7)?),
                    sweep_stale: row.get::<_, i64>(8)? != 0,
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
        cache_read: u64,
        cache_write: u64,
        estimated: bool,
        cost_usd: f64,
    ) -> Result<(UsageEntry, Vec<Event>)> {
        let cache_read = cache_read.min(input);
        let cache_write = cache_write.min(input.saturating_sub(cache_read));
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
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
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
              input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
              estimated, cost_usd, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                entry.cache_read_tokens as i64,
                entry.cache_write_tokens as i64,
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
                "cache_read_tokens": entry.cache_read_tokens,
                "cache_write_tokens": entry.cache_write_tokens,
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
                    model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    estimated, cost_usd, created_at
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
                cache_read_tokens: row.get::<_, i64>(10)? as u64,
                cache_write_tokens: row.get::<_, i64>(11)? as u64,
                estimated: row.get::<_, i64>(12)? != 0,
                cost_usd: row.get(13)?,
                created_at: row.get(14)?,
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
                  input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                  estimated, cost_usd, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                    entry.cache_read_tokens as i64,
                    entry.cache_write_tokens as i64,
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

    /// Read-only aggregate counts for daemon metrics. This deliberately returns
    /// only bounded status/kind label sets and numeric totals: no session ids,
    /// task ids, goals, paths, model names, or agent names.
    pub fn daemon_metrics_snapshot(&self) -> Result<DaemonMetricsSnapshot> {
        let conn = self.lock()?;
        let projects: i64 =
            conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
        let usage = conn.query_row(
            "SELECT COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(cost_usd), 0)
             FROM usage_ledger",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?.max(0) as u64,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                    row.get::<_, i64>(3)?.max(0) as u64,
                    row.get::<_, f64>(4)?,
                ))
            },
        )?;
        let (tokens_in, tokens_out, cache_read_tokens, cache_write_tokens, cost_spent_usd) = usage;
        let preamble_payload = conn
            .query_row(
                "SELECT payload FROM events
                 WHERE type = 'context.prompt_prefix'
                 ORDER BY created_at DESC, seq DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let preamble_tokens_estimate = preamble_payload
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
            .and_then(|payload| {
                payload
                    .get("preamble_tokens_estimate")
                    .and_then(serde_json::Value::as_u64)
                    .or_else(|| {
                        payload
                            .get("stable_prefix_chars")
                            .and_then(serde_json::Value::as_u64)
                            .map(|chars| chars / 4)
                    })
            });
        Ok(DaemonMetricsSnapshot {
            projects: projects.max(0) as u64,
            sessions: bounded_metric_counts(&conn, "sessions", "status")?,
            plan_tasks: bounded_metric_counts(&conn, "plan_tasks", "status")?,
            merge_queue: bounded_metric_counts(&conn, "merge_queue", "status")?,
            intake: bounded_metric_counts(&conn, "intake_tasks", "status")?,
            continuations: bounded_metric_counts(&conn, "session_continuations", "status")?,
            recoveries: bounded_metric_counts(&conn, "orchestration_recoveries", "status")?,
            memories: bounded_metric_counts(&conn, "memories", "status")?,
            memory_conflicts: bounded_metric_counts(&conn, "memory_conflicts", "status")?,
            runtime_leases: bounded_metric_counts(&conn, "runtime_leases", "kind")?,
            tokens_in,
            tokens_out,
            cache_read_tokens,
            cache_write_tokens,
            preamble_tokens_estimate,
            cost_spent_usd: if cost_spent_usd.is_finite() {
                cost_spent_usd.max(0.0)
            } else {
                0.0
            },
        })
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

    /// Atomically claim an operation receipt for one session. The comparison
    /// and insert/update happen in one immediate transaction so two callers
    /// cannot both start the same operation.
    pub fn claim_session_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        method: &str,
        fingerprint: &str,
    ) -> Result<Claim> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT method, fingerprint, status, result_json
                 FROM session_operations
                 WHERE session_id = ?1 AND operation_id = ?2",
                params![session_id, operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((existing_method, existing_fingerprint, status, result_json)) = existing else {
            tx.execute(
                "INSERT INTO session_operations
                 (session_id, operation_id, method, fingerprint, status, result_json, error,
                  created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', NULL, NULL, ?5, ?5)",
                params![session_id, operation_id, method, fingerprint, now],
            )?;
            tx.commit()?;
            return Ok(Claim::New);
        };
        if existing_method != method || existing_fingerprint != fingerprint {
            return Ok(Claim::Conflict);
        }
        match status.as_str() {
            "completed" => {
                let result = result_json.ok_or_else(|| {
                    anyhow!(
                        "session operation {operation_id} is completed but has no stored result"
                    )
                })?;
                Ok(Claim::Replay(result))
            }
            "pending" => Ok(Claim::InProgress),
            "unknown" => Ok(Claim::Unknown),
            "failed" => {
                tx.execute(
                    "UPDATE session_operations
                     SET status = 'pending', result_json = NULL, error = NULL, updated_at = ?3
                     WHERE session_id = ?1 AND operation_id = ?2 AND status = 'failed'",
                    params![session_id, operation_id, now],
                )?;
                tx.commit()?;
                Ok(Claim::New)
            }
            other => Err(anyhow!(
                "session operation {operation_id} has invalid status {other}"
            )),
        }
    }

    pub fn complete_session_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        result_json: &str,
    ) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE session_operations
             SET status = 'completed', result_json = ?3, error = NULL, updated_at = ?4
             WHERE session_id = ?1 AND operation_id = ?2 AND status = 'pending'",
            params![session_id, operation_id, result_json, now],
        )?;
        if changed > 0 {
            return Ok(());
        }
        let status = conn
            .query_row(
                "SELECT status FROM session_operations
                 WHERE session_id = ?1 AND operation_id = ?2",
                params![session_id, operation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match status {
            Some(status) => Err(anyhow!(
                "session operation {operation_id} is {status}; cannot mark it completed"
            )),
            None => Err(anyhow!("session operation {operation_id} was not found")),
        }
    }

    pub fn fail_session_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        error: &str,
    ) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE session_operations
             SET status = 'failed', result_json = NULL, error = ?3, updated_at = ?4
             WHERE session_id = ?1 AND operation_id = ?2 AND status = 'pending'",
            params![session_id, operation_id, error, now],
        )?;
        if changed > 0 {
            return Ok(());
        }
        let status = conn
            .query_row(
                "SELECT status FROM session_operations
                 WHERE session_id = ?1 AND operation_id = ?2",
                params![session_id, operation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match status {
            Some(status) => Err(anyhow!(
                "session operation {operation_id} is {status}; cannot mark it failed"
            )),
            None => Err(anyhow!("session operation {operation_id} was not found")),
        }
    }

    /// Startup recovery: a `pending` row means the previous daemon stopped
    /// while the operation was in flight. Its side effects may or may not
    /// have landed, so fail closed until an operator inspects the session.
    pub fn mark_pending_session_operations_unknown(&self) -> Result<usize> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE session_operations
             SET status = 'unknown',
                 error = 'daemon restarted while the operation was pending; outcome unknown',
                 updated_at = ?1
             WHERE status = 'pending'",
            params![now],
        )?;
        Ok(changed)
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

    /// Persist a deferred continuation and its creation event atomically. The
    /// engine validates body, watch ids, task states, recurrence, and the
    /// deadline before calling this. The first row of a series owns the series
    /// id; a child row is only created by a successful delivery.
    pub fn create_session_continuation_with_event(
        &self,
        new: &NewSessionContinuation,
    ) -> Result<(SessionContinuation, Event)> {
        let now = now_rfc3339();
        let id = Uuid::now_v7().to_string();
        let series_id = new.series_id.clone().unwrap_or_else(|| id.clone());
        let continuation = SessionContinuation {
            id,
            session_id: new.session_id.clone(),
            project_id: new.project_id.clone(),
            body: new.body.clone(),
            watch_task_ids: new.watch_task_ids.clone(),
            watch_mode: new.watch_mode.clone(),
            deadline_at: new.deadline_at.clone(),
            status: "pending".to_string(),
            attempts: 0,
            max_attempts: new.max_attempts.max(1),
            wake_reason: None,
            outcomes: Vec::new(),
            last_error: None,
            next_attempt_at: None,
            created_at: now.clone(),
            ready_at: None,
            delivered_at: None,
            updated_at: now,
            series_id,
            parent_continuation_id: new.parent_continuation_id.clone(),
            recurrence_every_secs: new.recurrence_every_secs,
            recurrence_max_runs: new.recurrence_max_runs.max(1),
            run_count: new.run_count,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let actual_project: String = tx
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![continuation.session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {}", continuation.session_id))?;
        if actual_project != continuation.project_id {
            return Err(anyhow!(
                "session {} belongs to {actual_project}",
                continuation.session_id
            ));
        }
        insert_session_continuation_tx(&tx, &continuation)?;
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.created",
            serde_json::to_value(&continuation)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((continuation, event))
    }

    pub fn list_session_continuations(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<SessionContinuation>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 2_000) as i64;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_CONTINUATION_COLUMNS}
             FROM session_continuations
             WHERE session_id = ?1 AND (?2 IS NULL OR status = ?2)
             ORDER BY created_at ASC, id ASC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![session_id, status, limit], map_session_continuation)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// All pending and ready rows across sessions, ordered deterministically.
    /// The scheduler tick evaluates this snapshot; it never sees terminal rows.
    pub fn list_open_session_continuations(&self) -> Result<Vec<SessionContinuation>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_CONTINUATION_COLUMNS}
             FROM session_continuations
             WHERE status IN ('pending', 'ready')
             ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map([], map_session_continuation)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn mark_session_continuation_ready_with_event(
        &self,
        id: &str,
        reason: &str,
        outcomes: &[ContinuationOutcome],
    ) -> Result<Option<(SessionContinuation, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'ready', wake_reason = ?1, outcomes = ?2, ready_at = ?3,
                 next_attempt_at = NULL, updated_at = ?3
             WHERE id = ?4 AND status = 'pending'",
            params![reason, serde_json::to_string(outcomes)?, now, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during ready transition: {id}"))?;
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.ready",
            serde_json::to_value(&continuation)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    /// Atomically claim a ready row for one delivery attempt. Only the caller
    /// whose claim commits may call `start`; a racing pass sees zero rows and
    /// skips. A crash after this transition leaves `delivering`, which daemon
    /// recovery fails closed instead of delivering again.
    pub fn claim_session_continuation_with_event(
        &self,
        id: &str,
    ) -> Result<Option<(SessionContinuation, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'delivering', next_attempt_at = NULL, updated_at = ?1
             WHERE id = ?2 AND status = 'ready'",
            params![now, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during claim: {id}"))?;
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.delivering",
            serde_json::to_value(&continuation)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    /// Atomically mark a claimed row delivered, increment its run count, and
    /// schedule the next run when the series still has budget. Projection,
    /// delivery event, and child event commit together. If inserting the child
    /// fails the transaction rolls back, so the claimed row remains
    /// `delivering`; daemon recovery then fails it closed and no duplicate
    /// child can exist.
    pub fn mark_session_continuation_delivered_with_event(
        &self,
        id: &str,
    ) -> Result<Option<ContinuationDelivery>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'delivered', run_count = run_count + 1, delivered_at = ?1,
                 next_attempt_at = NULL, updated_at = ?1
             WHERE id = ?2 AND status = 'delivering'",
            params![now, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let delivered = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during delivery: {id}"))?;
        let mut events = vec![insert_event_tx(
            &tx,
            &delivered.session_id,
            "session.continuation.delivered",
            serde_json::to_value(&delivered)?,
        )?];
        let scheduled = next_recurring_continuation(&delivered, &now)?;
        if let Some(child) = &scheduled {
            insert_session_continuation_tx(&tx, child)?;
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

    /// Record a failed start attempt without exhausting the cap. The row stays
    /// ready and the scheduler waits for `next_attempt_at`.
    pub fn retry_session_continuation_with_event(
        &self,
        id: &str,
        error: &str,
        next_attempt_at: &str,
    ) -> Result<Option<(SessionContinuation, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'ready', attempts = attempts + 1, last_error = ?1,
                 next_attempt_at = ?2, updated_at = ?3
             WHERE id = ?4 AND status IN ('ready', 'delivering')",
            params![error, next_attempt_at, now, id],
        )?;
        if changed == 0 {
            return Ok(None);
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

    pub fn fail_session_continuation_with_event(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<(SessionContinuation, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'failed', attempts = attempts + 1, last_error = ?1,
                 next_attempt_at = NULL, updated_at = ?2
             WHERE id = ?3 AND status IN ('pending', 'ready', 'delivering')",
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

    pub fn cancel_session_continuation_with_event(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(SessionContinuation, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'cancelled', last_error = ?1, next_attempt_at = NULL, updated_at = ?2
             WHERE id = ?3 AND status IN ('pending', 'ready')",
            params![reason, now, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during cancellation: {id}"))?;
        let mut payload = serde_json::to_value(&continuation)?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("reason".into(), serde_json::json!(reason));
        }
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.cancelled",
            payload,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    /// Replace the continuation projection from replayed events. Used by
    /// `session.replay {repair}` and `vp db-rebuild`.
    pub fn replace_session_continuations(
        &self,
        session_id: &str,
        continuations: &[SessionContinuation],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM session_continuations WHERE session_id = ?1",
            params![session_id],
        )?;
        for continuation in continuations {
            if continuation.session_id != session_id {
                return Err(anyhow!("continuation belongs to a different session"));
            }
            insert_session_continuation_tx(&tx, continuation)?;
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

    pub fn get_orchestration_recovery(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
    ) -> Result<Option<OrchestrationRecovery>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, action, status,
                    attempts, last_stall_count, requested_at, updated_at, last_error,
                    next_action_at
             FROM orchestration_recoveries
             WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq as i64, task_id],
            map_orchestration_recovery,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Every recovery row for the session, including rows from older plan
    /// revisions. Replay compares this historical set with the event stream.
    pub fn list_orchestration_recoveries(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<OrchestrationRecovery>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, project_id, plan_seq, task_id, department, action, status,
                    attempts, last_stall_count, requested_at, updated_at, last_error,
                    next_action_at
             FROM orchestration_recoveries
             WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR session_id = ?2)
             ORDER BY session_id ASC, plan_seq ASC, task_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_orchestration_recovery)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Recovery rows joined against the current plan-task projection so rows
    /// from an older plan revision are not reported by the live surfaces.
    pub fn list_current_orchestration_recoveries(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<OrchestrationRecovery>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT r.session_id, r.project_id, r.plan_seq, r.task_id, r.department,
                    r.action, r.status, r.attempts, r.last_stall_count, r.requested_at,
                    r.updated_at, r.last_error, r.next_action_at
             FROM orchestration_recoveries r
             JOIN plan_tasks t
               ON t.session_id = r.session_id AND t.plan_seq = r.plan_seq
              AND t.task_id = r.task_id
             WHERE (?1 IS NULL OR r.project_id = ?1) AND (?2 IS NULL OR r.session_id = ?2)
             ORDER BY r.session_id ASC, r.plan_seq ASC, r.task_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_orchestration_recovery)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Recovery requests that still wait for cooperative cancellation and
    /// session quiescence. This is the bounded scan for the recovery tick.
    pub fn list_active_orchestration_recoveries(
        &self,
        limit: u32,
    ) -> Result<Vec<OrchestrationRecovery>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT r.session_id, r.project_id, r.plan_seq, r.task_id, r.department,
                    r.action, r.status, r.attempts, r.last_stall_count, r.requested_at,
                    r.updated_at, r.last_error, r.next_action_at
             FROM orchestration_recoveries r
             JOIN plan_tasks t
               ON t.session_id = r.session_id AND t.plan_seq = r.plan_seq
              AND t.task_id = r.task_id
             WHERE r.status IN ('requested', 'waiting_cancel')
             ORDER BY r.updated_at ASC, r.requested_at ASC, r.session_id ASC,
                      r.plan_seq ASC, r.task_id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            params![i64::from(limit.clamp(1, 500))],
            map_orchestration_recovery,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn replace_orchestration_recoveries(
        &self,
        session_id: &str,
        recoveries: &[OrchestrationRecovery],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM orchestration_recoveries WHERE session_id = ?1",
            params![session_id],
        )?;
        for recovery in recoveries {
            if recovery.session_id != session_id {
                return Err(anyhow!(
                    "recovery row {} belongs to a different session",
                    recovery.task_id
                ));
            }
            upsert_orchestration_recovery(&tx, recovery)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record one per-window escalation. The task keeps running; this only
    /// writes the recovery projection, its event, and the caller's optional
    /// coordination note.
    pub fn escalate_stall_recovery(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = recovery_row_tx(&tx, &stall.session_id, stall.plan_seq, &stall.task_id)?;
        if let Some(existing) = &existing
            && matches!(
                existing.status.as_str(),
                "requested" | "waiting_cancel" | "failed" | "unnecessary"
            )
        {
            return Ok(None);
        }
        if existing
            .as_ref()
            .is_some_and(|existing| existing.last_stall_count >= stall.stall_count)
        {
            return Ok(None);
        }
        let recovery = OrchestrationRecovery {
            session_id: stall.session_id.clone(),
            project_id: stall.project_id.clone(),
            plan_seq: stall.plan_seq,
            task_id: stall.task_id.clone(),
            department: stall.department.clone(),
            action: "escalate".to_string(),
            status: "escalated".to_string(),
            attempts: existing.as_ref().map(|row| row.attempts).unwrap_or(0),
            last_stall_count: stall.stall_count,
            requested_at: existing
                .as_ref()
                .map(|row| row.requested_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
            last_error: Some(reason.to_string()),
            next_action_at: None,
        };
        upsert_orchestration_recovery(&tx, &recovery)?;
        let event = insert_event_tx(
            &tx,
            &recovery.session_id,
            "orchestration.recovery_escalated",
            serde_json::to_value(&recovery)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((recovery, event)))
    }

    /// Persist a recovery intent before the caller requests cooperative task
    /// cancellation. Returns `None` when another request is already in flight,
    /// the stall window was already handled, or the task left `running`/
    /// `claimed` before the transaction started.
    pub fn begin_stall_retry_recovery(
        &self,
        stall: &TaskStall,
        max_attempts: u32,
        grace_secs: u64,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let still_active: bool = tx
            .query_row(
                "SELECT 1 FROM plan_tasks
                 WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3
                   AND status IN ('running', 'claimed')",
                params![stall.session_id, stall.plan_seq as i64, stall.task_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !still_active {
            return Ok(None);
        }
        let existing = recovery_row_tx(&tx, &stall.session_id, stall.plan_seq, &stall.task_id)?;
        if let Some(existing) = &existing {
            if matches!(
                existing.status.as_str(),
                "requested" | "waiting_cancel" | "failed" | "unnecessary"
            ) {
                return Ok(None);
            }
            if existing.last_stall_count >= stall.stall_count || existing.attempts >= max_attempts {
                return Ok(None);
            }
        }
        if max_attempts == 0 {
            return Ok(None);
        }
        let attempts = existing
            .as_ref()
            .map(|row| row.attempts)
            .unwrap_or(0)
            .saturating_add(1);
        let deadline = time::OffsetDateTime::now_utc()
            + time::Duration::seconds(grace_secs.min(86_400) as i64);
        let next_action_at = deadline
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| now.clone());
        let recovery = OrchestrationRecovery {
            session_id: stall.session_id.clone(),
            project_id: stall.project_id.clone(),
            plan_seq: stall.plan_seq,
            task_id: stall.task_id.clone(),
            department: stall.department.clone(),
            action: "retry".to_string(),
            status: "requested".to_string(),
            attempts,
            last_stall_count: stall.stall_count,
            requested_at: now.clone(),
            updated_at: now,
            last_error: Some(reason.to_string()),
            next_action_at: Some(next_action_at),
        };
        upsert_orchestration_recovery(&tx, &recovery)?;
        let event = insert_event_tx(
            &tx,
            &recovery.session_id,
            "orchestration.recovery_requested",
            serde_json::to_value(&recovery)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((recovery, event)))
    }

    pub fn mark_stall_recovery_waiting_cancel(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        self.transition_stall_recovery(
            session_id,
            plan_seq,
            task_id,
            &["requested"],
            "waiting_cancel",
            "orchestration.recovery_waiting_cancel",
            Some(reason),
            false,
        )
    }

    pub fn finish_stall_recovery_unnecessary(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        self.transition_stall_recovery(
            session_id,
            plan_seq,
            task_id,
            &["requested", "waiting_cancel"],
            "unnecessary",
            "orchestration.recovery_unnecessary",
            Some(reason),
            true,
        )
    }

    pub fn complete_stall_recovery_retry(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        self.transition_stall_recovery(
            session_id,
            plan_seq,
            task_id,
            &["requested", "waiting_cancel"],
            "retried",
            "orchestration.recovery_retried",
            None,
            true,
        )
    }

    pub fn fail_stall_recovery(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        self.transition_stall_recovery(
            session_id,
            plan_seq,
            task_id,
            &["requested", "waiting_cancel", "retried"],
            "failed",
            "orchestration.recovery_failed",
            Some(reason),
            true,
        )
    }

    /// Fail a stall recovery that never got a request row, for example when
    /// `stall_max_attempts` is zero or the session budget is already exhausted.
    /// The event is still durable so replay and operators can see why no retry
    /// was attempted.
    pub fn fail_stall_recovery_for_stall(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = recovery_row_tx(&tx, &stall.session_id, stall.plan_seq, &stall.task_id)?;
        let Some(existing) = existing else {
            let recovery = OrchestrationRecovery {
                session_id: stall.session_id.clone(),
                project_id: stall.project_id.clone(),
                plan_seq: stall.plan_seq,
                task_id: stall.task_id.clone(),
                department: stall.department.clone(),
                action: "retry".to_string(),
                status: "failed".to_string(),
                attempts: 0,
                last_stall_count: stall.stall_count,
                requested_at: now.clone(),
                updated_at: now,
                last_error: Some(reason.to_string()),
                next_action_at: None,
            };
            upsert_orchestration_recovery(&tx, &recovery)?;
            let event = insert_event_tx(
                &tx,
                &recovery.session_id,
                "orchestration.recovery_failed",
                serde_json::to_value(&recovery)?,
            )?;
            tx.commit()?;
            if let Err(error) = self.append_event_mirror(&event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
            return Ok(Some((recovery, event)));
        };
        if !matches!(
            existing.status.as_str(),
            "requested" | "waiting_cancel" | "retried"
        ) {
            return Ok(None);
        }
        let recovery = OrchestrationRecovery {
            status: "failed".to_string(),
            updated_at: now,
            last_error: Some(reason.to_string()),
            next_action_at: None,
            ..existing
        };
        upsert_orchestration_recovery(&tx, &recovery)?;
        let event = insert_event_tx(
            &tx,
            &recovery.session_id,
            "orchestration.recovery_failed",
            serde_json::to_value(&recovery)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((recovery, event)))
    }

    #[allow(clippy::too_many_arguments)]
    fn transition_stall_recovery(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        from: &[&str],
        status: &str,
        event_kind: &str,
        last_error: Option<&str>,
        clear_next_action: bool,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut recovery) = recovery_row_tx(&tx, session_id, plan_seq, task_id)? else {
            return Ok(None);
        };
        if !from.contains(&recovery.status.as_str()) {
            return Ok(None);
        }
        recovery.status = status.to_string();
        recovery.updated_at = now;
        recovery.last_error = last_error.map(str::to_string);
        if clear_next_action {
            recovery.next_action_at = None;
        }
        upsert_orchestration_recovery(&tx, &recovery)?;
        let event = insert_event_tx(
            &tx,
            session_id,
            event_kind,
            serde_json::to_value(&recovery)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((recovery, event)))
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
            "DELETE FROM runtime_leases WHERE julianday(expires_at) <= julianday(?1)",
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
            "DELETE FROM runtime_leases WHERE julianday(expires_at) <= julianday(?1)",
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
                     FROM computer_leases WHERE julianday(expires_at) <= julianday(?1)
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
                 FROM computer_leases WHERE julianday(expires_at) <= julianday(?1)
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
        let interrupted_continuations = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {SESSION_CONTINUATION_COLUMNS}
                 FROM session_continuations WHERE status = 'delivering'
                 ORDER BY created_at ASC, id ASC"
            ))?;
            let rows = stmt.query_map([], map_session_continuation)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE session_continuations
             SET status = 'failed',
                 last_error = 'daemon restarted while delivering; not retried automatically',
                 next_attempt_at = NULL, updated_at = ?1
             WHERE status = 'delivering'",
            params![now],
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
        for continuation in &interrupted_continuations {
            let mut failed = continuation.clone();
            failed.status = "failed".to_string();
            failed.last_error =
                Some("daemon restarted while delivering; not retried automatically".to_string());
            failed.next_attempt_at = None;
            failed.updated_at = now.clone();
            let mut payload = serde_json::to_value(&failed)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "reason".into(),
                    serde_json::json!(
                        "daemon restarted while delivering; not retried automatically"
                    ),
                );
            }
            events.push(insert_event_tx(
                &tx,
                &failed.session_id,
                "session.continuation.failed",
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
            replace_session_memories_tx(&tx, session_id, memories)?;
        }
        if let Some(conflicts) = &repair.memory_conflicts {
            replace_session_memory_conflicts_tx(&tx, session_id, conflicts)?;
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
        if let Some(continuations) = &repair.continuations {
            tx.execute(
                "DELETE FROM session_continuations WHERE session_id = ?1",
                params![session_id],
            )?;
            for continuation in continuations {
                insert_session_continuation_tx(&tx, continuation)?;
            }
        }
        if let Some(recoveries) = &repair.recoveries {
            tx.execute(
                "DELETE FROM orchestration_recoveries WHERE session_id = ?1",
                params![session_id],
            )?;
            for recovery in recoveries {
                upsert_orchestration_recovery(&tx, recovery)?;
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
        for continuation in &bundle.continuations {
            if continuation.session_id != bundle.session.id {
                return Err(anyhow!(
                    "bundle continuation {} belongs to a different session",
                    continuation.id
                ));
            }
            if continuation.project_id != bundle.project.id {
                return Err(anyhow!(
                    "bundle continuation {} belongs to a different project",
                    continuation.id
                ));
            }
            insert_session_continuation_tx(&tx, continuation)?;
        }
        for recovery in &bundle.recoveries {
            upsert_orchestration_recovery(&tx, recovery)?;
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
            insert_memory_tx(&tx, memory)?;
        }
        for conflict in &bundle.memory_conflicts {
            upsert_memory_conflict_tx(&tx, conflict)?;
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
                  input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                  estimated, cost_usd, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                    entry.cache_read_tokens as i64,
                    entry.cache_write_tokens as i64,
                    if entry.estimated { 1 } else { 0 },
                    entry.cost_usd,
                    entry.created_at
                ],
            )?;
        }
        for program in &bundle.programs {
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM programs WHERE id = ?1",
                    params![program.id],
                    |row| row.get(0),
                )
                .optional()?;
            if existing.is_some() {
                // Imported archives never overwrite a live program ledger.
                continue;
            }
            let mut archived = program.clone();
            archived.archived = true;
            if matches!(
                archived.status,
                ProgramStatus::Active | ProgramStatus::Draft
            ) {
                archived.status = ProgramStatus::Paused;
            }
            archived.reason = Some("imported archive".to_string());
            archived.updated_at = now_rfc3339();
            tx.execute(
                "INSERT INTO programs
                 (id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason,
                  archived, created_at, updated_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?11)",
                params![
                    archived.id,
                    archived.project_id,
                    archived.goal,
                    archived.status.as_str(),
                    serde_json::to_string(&archived.policy)?,
                    archived.token_budget.map(|value| value as i64),
                    archived.cost_budget_usd,
                    archived.reason,
                    archived.created_at,
                    archived.updated_at,
                    archived.completed_at,
                ],
            )?;
            for milestone in bundle
                .program_milestones
                .iter()
                .filter(|milestone| milestone.program_id == archived.id)
            {
                Self::insert_program_milestone_tx(&tx, milestone)?;
            }
            for event in bundle
                .program_events
                .iter()
                .filter(|event| event.program_id == archived.id)
            {
                tx.execute(
                    "INSERT OR REPLACE INTO program_events
                     (project_id, program_id, seq, kind, payload, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        event.project_id,
                        event.program_id,
                        event.seq as i64,
                        event.kind,
                        serde_json::to_string(&event.payload)?,
                        event.created_at,
                    ],
                )?;
            }
            let archive_payload = serde_json::to_value(&archived)?;
            insert_program_event_tx(
                &tx,
                &archived.project_id,
                &archived.id,
                "program.updated",
                &archive_payload,
            )?;
            for milestone in bundle
                .program_milestones
                .iter()
                .filter(|milestone| milestone.program_id == archived.id)
            {
                let payload = serde_json::to_value(milestone)?;
                insert_program_event_tx(
                    &tx,
                    &archived.project_id,
                    &archived.id,
                    "program.milestone.updated",
                    &payload,
                )?;
            }
            // Archive rows carry the full event history. Rebuild the dispatch
            // accounting ledger so imported programs keep their historical
            // spend even though imported sessions are read-only archives.
            Self::rebuild_program_session_links_tx(&tx, &archived.id)?;
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

fn map_orchestration_recovery(row: &rusqlite::Row<'_>) -> rusqlite::Result<OrchestrationRecovery> {
    Ok(OrchestrationRecovery {
        session_id: row.get(0)?,
        project_id: row.get(1)?,
        plan_seq: row.get::<_, i64>(2)?.max(0) as u64,
        task_id: row.get(3)?,
        department: row.get(4)?,
        action: row.get(5)?,
        status: row.get(6)?,
        attempts: row.get::<_, i64>(7)?.max(0) as u32,
        last_stall_count: row.get::<_, i64>(8)?.max(0) as u32,
        requested_at: row.get(9)?,
        updated_at: row.get(10)?,
        last_error: row.get(11)?,
        next_action_at: row.get(12)?,
    })
}

fn recovery_row_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    plan_seq: u64,
    task_id: &str,
) -> Result<Option<OrchestrationRecovery>> {
    tx.query_row(
        "SELECT session_id, project_id, plan_seq, task_id, department, action, status,
                attempts, last_stall_count, requested_at, updated_at, last_error,
                next_action_at
         FROM orchestration_recoveries
         WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
        params![session_id, plan_seq as i64, task_id],
        map_orchestration_recovery,
    )
    .optional()
    .map_err(Into::into)
}

fn upsert_orchestration_recovery(
    tx: &Transaction<'_>,
    recovery: &OrchestrationRecovery,
) -> Result<()> {
    tx.execute(
        "INSERT INTO orchestration_recoveries
         (session_id, plan_seq, task_id, project_id, department, action, status,
          attempts, last_stall_count, requested_at, updated_at, last_error, next_action_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(session_id, plan_seq, task_id) DO UPDATE SET
            project_id = excluded.project_id,
            department = excluded.department,
            action = excluded.action,
            status = excluded.status,
            attempts = excluded.attempts,
            last_stall_count = excluded.last_stall_count,
            requested_at = excluded.requested_at,
            updated_at = excluded.updated_at,
            last_error = excluded.last_error,
            next_action_at = excluded.next_action_at",
        params![
            recovery.session_id,
            recovery.plan_seq as i64,
            recovery.task_id,
            recovery.project_id,
            recovery.department,
            recovery.action,
            recovery.status,
            i64::from(recovery.attempts),
            i64::from(recovery.last_stall_count),
            recovery.requested_at,
            recovery.updated_at,
            recovery.last_error,
            recovery.next_action_at
        ],
    )?;
    Ok(())
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

const SESSION_CONTINUATION_COLUMNS: &str = "id, session_id, project_id, body, watch_task_ids,
    watch_mode, deadline_at, status, attempts, max_attempts, wake_reason, outcomes,
    last_error, next_attempt_at, created_at, ready_at, delivered_at, updated_at,
    series_id, parent_continuation_id, recurrence_every_secs, recurrence_max_runs, run_count";

fn map_session_continuation(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionContinuation> {
    let watch_task_ids = row.get::<_, String>(4)?;
    let outcomes = row.get::<_, String>(11)?;
    Ok(SessionContinuation {
        id: row.get(0)?,
        session_id: row.get(1)?,
        project_id: row.get(2)?,
        body: row.get(3)?,
        watch_task_ids: serde_json::from_str(&watch_task_ids).unwrap_or_default(),
        watch_mode: row.get(5)?,
        deadline_at: row.get(6)?,
        status: row.get(7)?,
        attempts: row.get::<_, i64>(8)?.max(0) as u32,
        max_attempts: row.get::<_, i64>(9)?.max(1) as u32,
        wake_reason: row.get(10)?,
        outcomes: serde_json::from_str(&outcomes).unwrap_or_default(),
        last_error: row.get(12)?,
        next_attempt_at: row.get(13)?,
        created_at: row.get(14)?,
        ready_at: row.get(15)?,
        delivered_at: row.get(16)?,
        updated_at: row.get(17)?,
        series_id: row.get(18)?,
        parent_continuation_id: row.get(19)?,
        recurrence_every_secs: row
            .get::<_, Option<i64>>(20)?
            .map(|value| value.max(0) as u64),
        recurrence_max_runs: row.get::<_, i64>(21)?.max(1) as u32,
        run_count: row.get::<_, i64>(22)?.max(0) as u32,
    })
}

fn next_recurring_continuation(
    delivered: &SessionContinuation,
    delivered_at: &str,
) -> Result<Option<SessionContinuation>> {
    let Some(every_secs) = delivered.recurrence_every_secs else {
        return Ok(None);
    };
    if delivered.run_count >= delivered.recurrence_max_runs.max(1) {
        return Ok(None);
    }
    let delivered_at =
        time::OffsetDateTime::parse(delivered_at, &time::format_description::well_known::Rfc3339)
            .map_err(|error| anyhow!("parsing delivery time: {error}"))?;
    let seconds =
        i64::try_from(every_secs).map_err(|_| anyhow!("recurrence interval is too large"))?;
    let deadline = (delivered_at + time::Duration::seconds(seconds))
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| anyhow!("formatting recurrence deadline: {error}"))?;
    let series_id = if delivered.series_id.is_empty() {
        delivered.id.clone()
    } else {
        delivered.series_id.clone()
    };
    Ok(Some(SessionContinuation {
        id: Uuid::now_v7().to_string(),
        session_id: delivered.session_id.clone(),
        project_id: delivered.project_id.clone(),
        body: delivered.body.clone(),
        watch_task_ids: Vec::new(),
        watch_mode: "all".to_string(),
        deadline_at: Some(deadline),
        status: "pending".to_string(),
        attempts: 0,
        max_attempts: delivered.max_attempts.max(1),
        wake_reason: None,
        outcomes: Vec::new(),
        last_error: None,
        next_attempt_at: None,
        created_at: delivered.updated_at.clone(),
        ready_at: None,
        delivered_at: None,
        updated_at: delivered.updated_at.clone(),
        series_id,
        parent_continuation_id: Some(delivered.id.clone()),
        recurrence_every_secs: Some(every_secs),
        recurrence_max_runs: delivered.recurrence_max_runs.max(1),
        run_count: delivered.run_count,
    }))
}

fn query_session_continuation_tx(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<Option<SessionContinuation>> {
    Ok(tx
        .query_row(
            &format!(
                "SELECT {SESSION_CONTINUATION_COLUMNS} FROM session_continuations WHERE id = ?1"
            ),
            params![id],
            map_session_continuation,
        )
        .optional()?)
}

fn insert_session_continuation_tx(
    tx: &Transaction<'_>,
    continuation: &SessionContinuation,
) -> Result<()> {
    tx.execute(
        "INSERT INTO session_continuations
         (id, session_id, project_id, body, watch_task_ids, watch_mode, deadline_at,
          status, attempts, max_attempts, wake_reason, outcomes, last_error,
          next_attempt_at, created_at, ready_at, delivered_at, updated_at,
          series_id, parent_continuation_id, recurrence_every_secs, recurrence_max_runs,
          run_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
        params![
            continuation.id,
            continuation.session_id,
            continuation.project_id,
            continuation.body,
            serde_json::to_string(&continuation.watch_task_ids)?,
            continuation.watch_mode,
            continuation.deadline_at,
            continuation.status,
            i64::from(continuation.attempts),
            i64::from(continuation.max_attempts),
            continuation.wake_reason,
            serde_json::to_string(&continuation.outcomes)?,
            continuation.last_error,
            continuation.next_attempt_at,
            continuation.created_at,
            continuation.ready_at,
            continuation.delivered_at,
            continuation.updated_at,
            continuation.series_id,
            continuation.parent_continuation_id,
            continuation
                .recurrence_every_secs
                .map(|value| i64::try_from(value).unwrap_or(i64::MAX)),
            i64::from(continuation.recurrence_max_runs),
            i64::from(continuation.run_count)
        ],
    )?;
    Ok(())
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

fn map_memory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryItem> {
    let tags: String = row.get(8)?;
    let evidence: String = row.get(12)?;
    let supersedes: String = row.get(13)?;
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
        status: row.get(10)?,
        source: row.get(11)?,
        evidence: serde_json::from_str(&evidence).unwrap_or_default(),
        supersedes: serde_json::from_str(&supersedes).unwrap_or_default(),
        superseded_by: row.get(14)?,
        pinned: row.get::<_, i64>(15)? != 0,
        use_count: u32::try_from(row.get::<_, i64>(16)?).unwrap_or(u32::MAX),
        last_used_at: row.get(17)?,
        last_verified_at: row.get(18)?,
        verify_after: row.get(19)?,
        stale_reason: row.get(20)?,
    })
}

fn map_memory_policy_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryPolicy> {
    Ok(MemoryPolicy {
        enabled: row.get::<_, i64>(0)? != 0,
        max_items: row
            .get::<_, Option<i64>>(1)?
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0),
        max_age_days: row
            .get::<_, Option<i64>>(2)?
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0),
        task_result_verify_days: optional_count_from_db(row.get::<_, i64>(3)?),
        default_verify_days: optional_count_from_db(row.get::<_, i64>(4)?),
        auto_supersede: row.get::<_, i64>(5)? != 0,
        brief_max_chars: optional_count_from_db(row.get::<_, i64>(6)?),
        sweep_stale: row.get::<_, i64>(7)? != 0,
    })
}

fn optional_count_to_db(value: Option<u32>) -> i64 {
    value.map_or(0, i64::from)
}

fn optional_count_from_db(value: i64) -> Option<u32> {
    u32::try_from(value).ok().filter(|value| *value > 0)
}

fn insert_memory_tx(tx: &Transaction<'_>, memory: &MemoryItem) -> Result<()> {
    tx.execute(
        "INSERT INTO memories
         (id, session_id, project_id, agent_id, agent_name, kind, title, body, tags, created_at,
          status, source, evidence, supersedes, superseded_by, pinned, use_count, last_used_at,
          last_verified_at, verify_after, stale_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21)",
        params![
            &memory.id,
            &memory.session_id,
            &memory.project_id,
            &memory.agent_id,
            &memory.agent_name,
            &memory.kind,
            &memory.title,
            &memory.body,
            serde_json::to_string(&memory.tags)?,
            &memory.created_at,
            if memory.status.is_empty() {
                "active"
            } else {
                &memory.status
            },
            if memory.source.is_empty() {
                "agent"
            } else {
                &memory.source
            },
            serde_json::to_string(&memory.evidence)?,
            serde_json::to_string(&memory.supersedes)?,
            &memory.superseded_by,
            if memory.pinned { 1 } else { 0 },
            i64::from(memory.use_count),
            &memory.last_used_at,
            &memory.last_verified_at,
            &memory.verify_after,
            &memory.stale_reason
        ],
    )?;
    Ok(())
}

fn update_memory_projection_tx(tx: &Transaction<'_>, memory: &MemoryItem) -> Result<()> {
    tx.execute(
        "UPDATE memories SET
             tags = ?2,
             status = ?3,
             source = ?4,
             evidence = ?5,
             supersedes = ?6,
             superseded_by = ?7,
             pinned = ?8,
             use_count = ?9,
             last_used_at = ?10,
             last_verified_at = ?11,
             verify_after = ?12,
             stale_reason = ?13
         WHERE id = ?1",
        params![
            &memory.id,
            serde_json::to_string(&memory.tags)?,
            &memory.status,
            &memory.source,
            serde_json::to_string(&memory.evidence)?,
            serde_json::to_string(&memory.supersedes)?,
            &memory.superseded_by,
            if memory.pinned { 1 } else { 0 },
            i64::from(memory.use_count),
            &memory.last_used_at,
            &memory.last_verified_at,
            &memory.verify_after,
            &memory.stale_reason
        ],
    )?;
    Ok(())
}

fn replace_session_memories_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    memories: &[MemoryItem],
) -> Result<()> {
    // Usage rows cascade with memory deletion. Preserve the observed
    // (memory, session) pairs across repair so replaying a brief stays
    // idempotent for rows that still exist.
    let usage = {
        let mut stmt = tx.prepare(
            "SELECT u.memory_id, u.session_id, u.used_at
             FROM memory_usage u
             JOIN memories m ON m.id = u.memory_id
             WHERE m.session_id = ?1",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    tx.execute(
        "DELETE FROM memories WHERE session_id = ?1",
        params![session_id],
    )?;
    for memory in memories {
        insert_memory_tx(tx, memory)?;
    }
    for (memory_id, using_session_id, used_at) in usage {
        tx.execute(
            "INSERT OR IGNORE INTO memory_usage (memory_id, session_id, used_at)
             SELECT id, ?2, ?3 FROM memories WHERE id = ?1",
            params![memory_id, using_session_id, used_at],
        )?;
    }
    Ok(())
}

fn get_memory_tx(conn: &Connection, id: &str) -> Result<Option<MemoryItem>> {
    conn.query_row(
        "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                tags, created_at, status, source, evidence, supersedes, superseded_by,
                pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
         FROM memories WHERE id = ?1",
        params![id],
        map_memory_row,
    )
    .optional()
    .map_err(Into::into)
}

const MEMORY_CONFLICT_COLUMNS: &str = "id, project_id, session_id, memory_a_id, memory_b_id,
    memory_a_title, memory_b_title, memory_a_kind, memory_b_kind, memory_a_excerpt,
    memory_b_excerpt, scope_kind, scope_value, reason, status, detected_at, resolved_at,
    resolution, note";
const MAX_CONFLICT_MEMORIES: usize = 5_000;
const MAX_CONFLICT_PAIRS_PER_GROUP: usize = 50;
const MAX_CONFLICT_PAIRS_TOTAL: usize = 500;
const MEMORY_CONFLICT_EXCERPT_CHARS: usize = 600;

fn map_memory_conflict_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryConflict> {
    Ok(MemoryConflict {
        id: row.get(0)?,
        project_id: row.get(1)?,
        session_id: row.get(2)?,
        memory_a_id: row.get(3)?,
        memory_b_id: row.get(4)?,
        memory_a_title: row.get(5)?,
        memory_b_title: row.get(6)?,
        memory_a_kind: row.get(7)?,
        memory_b_kind: row.get(8)?,
        memory_a_excerpt: row.get(9)?,
        memory_b_excerpt: row.get(10)?,
        scope_kind: row.get(11)?,
        scope_value: row.get(12)?,
        reason: row.get(13)?,
        status: row.get(14)?,
        detected_at: row.get(15)?,
        resolved_at: row.get(16)?,
        resolution: row.get(17)?,
        note: row.get(18)?,
    })
}

fn get_memory_conflict_tx(conn: &Connection, id: &str) -> Result<Option<MemoryConflict>> {
    conn.query_row(
        &format!("SELECT {MEMORY_CONFLICT_COLUMNS} FROM memory_conflicts WHERE id = ?1"),
        params![id],
        map_memory_conflict_row,
    )
    .optional()
    .map_err(Into::into)
}

fn list_memory_conflicts_conn(
    conn: &Connection,
    project_id: &str,
    status: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<MemoryConflict>> {
    let limit = i64::from(limit.unwrap_or(500).clamp(1, 1_000));
    let mut stmt = conn.prepare(&format!(
        "SELECT {MEMORY_CONFLICT_COLUMNS}
         FROM memory_conflicts
         WHERE project_id = ?1 AND (?2 IS NULL OR status = ?2)
         ORDER BY scope_kind ASC, scope_value ASC, memory_a_id ASC, memory_b_id ASC,
                  detected_at DESC, id ASC
         LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![project_id, status, limit], map_memory_conflict_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn update_memory_conflict_tx(tx: &Transaction<'_>, conflict: &MemoryConflict) -> Result<()> {
    tx.execute(
        "UPDATE memory_conflicts SET
             memory_a_title = ?2,
             memory_b_title = ?3,
             memory_a_kind = ?4,
             memory_b_kind = ?5,
             memory_a_excerpt = ?6,
             memory_b_excerpt = ?7,
             scope_kind = ?8,
             scope_value = ?9,
             reason = ?10,
             status = ?11,
             detected_at = ?12,
             resolved_at = ?13,
             resolution = ?14,
             note = ?15
         WHERE id = ?1",
        params![
            &conflict.id,
            &conflict.memory_a_title,
            &conflict.memory_b_title,
            &conflict.memory_a_kind,
            &conflict.memory_b_kind,
            &conflict.memory_a_excerpt,
            &conflict.memory_b_excerpt,
            &conflict.scope_kind,
            &conflict.scope_value,
            &conflict.reason,
            &conflict.status,
            &conflict.detected_at,
            &conflict.resolved_at,
            &conflict.resolution,
            &conflict.note,
        ],
    )?;
    Ok(())
}

fn upsert_memory_conflict_tx(tx: &Transaction<'_>, conflict: &MemoryConflict) -> Result<()> {
    tx.execute(
        "INSERT INTO memory_conflicts
         (id, project_id, session_id, memory_a_id, memory_b_id, memory_a_title, memory_b_title,
          memory_a_kind, memory_b_kind, memory_a_excerpt, memory_b_excerpt, scope_kind,
          scope_value, reason, status, detected_at, resolved_at, resolution, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18, ?19)
         ON CONFLICT(id) DO UPDATE SET
             memory_a_title = excluded.memory_a_title,
             memory_b_title = excluded.memory_b_title,
             memory_a_kind = excluded.memory_a_kind,
             memory_b_kind = excluded.memory_b_kind,
             memory_a_excerpt = excluded.memory_a_excerpt,
             memory_b_excerpt = excluded.memory_b_excerpt,
             scope_kind = excluded.scope_kind,
             scope_value = excluded.scope_value,
             reason = excluded.reason,
             status = excluded.status,
             detected_at = excluded.detected_at,
             resolved_at = excluded.resolved_at,
             resolution = excluded.resolution,
             note = excluded.note",
        params![
            &conflict.id,
            &conflict.project_id,
            &conflict.session_id,
            &conflict.memory_a_id,
            &conflict.memory_b_id,
            &conflict.memory_a_title,
            &conflict.memory_b_title,
            &conflict.memory_a_kind,
            &conflict.memory_b_kind,
            &conflict.memory_a_excerpt,
            &conflict.memory_b_excerpt,
            &conflict.scope_kind,
            &conflict.scope_value,
            &conflict.reason,
            &conflict.status,
            &conflict.detected_at,
            &conflict.resolved_at,
            &conflict.resolution,
            &conflict.note,
        ],
    )?;
    Ok(())
}

fn replace_session_memory_conflicts_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    conflicts: &[MemoryConflict],
) -> Result<()> {
    tx.execute(
        "DELETE FROM memory_conflicts WHERE session_id = ?1",
        params![session_id],
    )?;
    for conflict in conflicts {
        upsert_memory_conflict_tx(tx, conflict)?;
    }
    Ok(())
}

fn active_memories_with_evidence_tx(
    conn: &Connection,
    project_id: &str,
    limit: usize,
) -> Result<Vec<MemoryItem>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                tags, created_at, status, source, evidence, supersedes, superseded_by,
                pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
         FROM memories
         WHERE project_id = ?1 AND status = 'active'
         ORDER BY id ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project_id, limit as i64], map_memory_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn detect_memory_conflicts(
    project_id: &str,
    memories: &[MemoryItem],
    detected_at: &str,
) -> Vec<MemoryConflict> {
    let mut groups: std::collections::BTreeMap<(String, String), Vec<ConflictCandidate>> =
        std::collections::BTreeMap::new();
    for memory in memories {
        let mut seen_scopes = std::collections::BTreeSet::new();
        for evidence in &memory.evidence {
            let scope_kind = match evidence.kind.as_str() {
                "path" if matches!(memory.kind.as_str(), "decision" | "convention" | "fact") => {
                    "path"
                }
                "task" if matches!(memory.kind.as_str(), "verification" | "task_result") => "task",
                _ => continue,
            };
            let scope_value = evidence.reference.trim();
            if scope_value.is_empty()
                || !seen_scopes.insert((scope_kind.to_string(), scope_value.to_string()))
            {
                continue;
            }
            groups
                .entry((scope_kind.to_string(), scope_value.to_string()))
                .or_default()
                .push(ConflictCandidate {
                    memory: memory.clone(),
                    scope_kind,
                    scope_value: scope_value.to_string(),
                });
        }
    }

    let mut conflicts = Vec::new();
    let mut total_pairs = 0usize;
    for ((scope_kind, scope_value), mut group) in groups {
        group.sort_by(|left, right| left.memory.id.cmp(&right.memory.id));
        let mut group_pairs = 0usize;
        'group: for left in 0..group.len() {
            for right in (left + 1)..group.len() {
                if total_pairs >= MAX_CONFLICT_PAIRS_TOTAL
                    || group_pairs >= MAX_CONFLICT_PAIRS_PER_GROUP
                {
                    break 'group;
                }
                let memory_a = &group[left].memory;
                let memory_b = &group[right].memory;
                if normalize_memory_body(&memory_a.body) == normalize_memory_body(&memory_b.body) {
                    continue;
                }
                let reason = format!(
                    "Both active memories cite {scope_kind} {scope_value} and have different bodies."
                );
                conflicts.push(MemoryConflict {
                    id: memory_conflict_id(
                        project_id,
                        &scope_kind,
                        &scope_value,
                        memory_a,
                        memory_b,
                    ),
                    project_id: project_id.to_string(),
                    session_id: memory_a.session_id.clone(),
                    memory_a_id: memory_a.id.clone(),
                    memory_b_id: memory_b.id.clone(),
                    memory_a_title: memory_a.title.clone(),
                    memory_b_title: memory_b.title.clone(),
                    memory_a_kind: memory_a.kind.clone(),
                    memory_b_kind: memory_b.kind.clone(),
                    memory_a_excerpt: bounded_excerpt(
                        &memory_a.body,
                        MEMORY_CONFLICT_EXCERPT_CHARS,
                    ),
                    memory_b_excerpt: bounded_excerpt(
                        &memory_b.body,
                        MEMORY_CONFLICT_EXCERPT_CHARS,
                    ),
                    scope_kind: group[left].scope_kind.to_string(),
                    scope_value: group[left].scope_value.clone(),
                    reason,
                    status: "open".to_string(),
                    detected_at: detected_at.to_string(),
                    resolved_at: None,
                    resolution: None,
                    note: None,
                });
                total_pairs += 1;
                group_pairs += 1;
            }
        }
    }
    conflicts.sort_by(|left, right| {
        left.scope_kind
            .cmp(&right.scope_kind)
            .then_with(|| left.scope_value.cmp(&right.scope_value))
            .then_with(|| left.memory_a_id.cmp(&right.memory_a_id))
            .then_with(|| left.memory_b_id.cmp(&right.memory_b_id))
    });
    conflicts
}

fn memory_conflict_id(
    project_id: &str,
    scope_kind: &str,
    scope_value: &str,
    memory_a: &MemoryItem,
    memory_b: &MemoryItem,
) -> String {
    let (first, second) = if memory_a.id <= memory_b.id {
        (memory_a, memory_b)
    } else {
        (memory_b, memory_a)
    };
    let mut hasher = Sha256::new();
    hasher.update(project_id.as_bytes());
    hasher.update(b"|");
    hasher.update(scope_kind.as_bytes());
    hasher.update(b"|");
    hasher.update(scope_value.as_bytes());
    hasher.update(b"|");
    hasher.update(first.id.as_bytes());
    hasher.update(b"|");
    hasher.update(second.id.as_bytes());
    hasher.update(b"|");
    hasher.update(memory_body_hash(&first.body).as_bytes());
    hasher.update(b"|");
    hasher.update(memory_body_hash(&second.body).as_bytes());
    format!("{:x}", hasher.finalize())
        .chars()
        .take(32)
        .collect()
}

fn memory_body_hash(body: &str) -> String {
    let normalized = normalize_memory_body(body);
    format!("{:x}", Sha256::digest(normalized.as_bytes()))
}

fn normalize_memory_body(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn bounded_excerpt(value: &str, max_chars: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut excerpt = collapsed
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    excerpt.push('…');
    excerpt
}

fn normalize_memory_key(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn active_memory_for_key_tx(
    conn: &Connection,
    project_id: &str,
    kind: &str,
    title: &str,
) -> Result<Option<MemoryItem>> {
    let key_kind = normalize_memory_key(kind);
    let key_title = normalize_memory_key(title);
    let mut stmt = conn.prepare(
        "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                tags, created_at, status, source, evidence, supersedes, superseded_by,
                pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
         FROM memories
         WHERE project_id = ?1 AND status = 'active'
         ORDER BY created_at DESC, id ASC",
    )?;
    let rows = stmt.query_map(params![project_id], map_memory_row)?;
    for row in rows {
        let memory = row?;
        if normalize_memory_key(&memory.kind) == key_kind
            && normalize_memory_key(&memory.title) == key_title
        {
            return Ok(Some(memory));
        }
    }
    Ok(None)
}

fn like_memory_lexical(memory: &MemoryItem, query: &str) -> f64 {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 0.0;
    }
    let title = memory.title.to_lowercase();
    let body = memory.body.to_lowercase();
    let tags = memory.tags.join(" ").to_lowercase();
    let mut score: f64 = 0.0;
    if title.contains(&query) {
        score += 0.7;
    } else if body.contains(&query) {
        score += 0.4;
    } else if tags.contains(&query) {
        score += 0.3;
    }
    for term in query.split_whitespace() {
        if title.contains(term) {
            score += 0.15;
        }
        if body.contains(term) {
            score += 0.05;
        }
        if tags.contains(term) {
            score += 0.05;
        }
    }
    score.min(1.0)
}

/// Status/kind vocabularies are application-controlled, but cap the number of
/// groups anyway so a corrupted or imported database cannot create unbounded
/// metric cardinality. `table` and `column` are fixed call-site literals.
const MAX_METRIC_GROUPS: usize = 64;

fn bounded_metric_counts(conn: &Connection, table: &str, column: &str) -> Result<Vec<MetricCount>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {column}, COUNT(*) FROM {table}
         GROUP BY {column} ORDER BY {column} ASC LIMIT ?1"
    ))?;
    let rows = stmt.query_map(params![MAX_METRIC_GROUPS as i64], |row| {
        let count: i64 = row.get(1)?;
        Ok(MetricCount {
            key: row.get(0)?,
            count: count.max(0) as u64,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn memory_count_group(
    conn: &Connection,
    project_id: &str,
    column: &str,
) -> Result<Vec<MemoryStatCount>> {
    // `column` is only ever a fixed call-site literal, never user input.
    let mut stmt = conn.prepare(&format!(
        "SELECT {column}, COUNT(*) FROM memories WHERE project_id = ?1 GROUP BY {column}"
    ))?;
    let rows = stmt.query_map(params![project_id], |row| {
        let count: i64 = row.get(1)?;
        Ok(MemoryStatCount {
            key: row.get(0)?,
            count: u32::try_from(count).unwrap_or(u32::MAX),
        })
    })?;
    let mut counts = rows.collect::<Result<Vec<_>, _>>()?;
    counts.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(counts)
}

fn parse_rfc3339(value: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
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
