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
    AcpAgent, AcpAgentConfig, AcpTransport, AgentMessage, AgentRecord, Approval, ApprovalGrant,
    Artifact, ArtifactPolicy, ArtifactVersion, CheckpointRetentionPolicy, ContextCheckpoint,
    ContextCheckpointPolicy, ContinuationOutcome, Event, GuiActionInfo, GuiPolicy, IntakeTask,
    IntegrationPolicy, MailboxPolicy, McpServer, McpServerConfig, McpTransport, MemoryConflict,
    MemoryEmbeddingJobStatus, MemoryEmbeddingPolicy, MemoryEmbeddingUsage,
    MemoryEmbeddingsStatusResult, MemoryEvidence, MemoryHealthResult, MemoryItem, MemoryPolicy,
    MemoryStatCount, MergeQueueEntry, MetricCount, ModelPreset, OrchestrationDecision,
    OrchestrationNote, OrchestrationPolicy, OrchestrationRecovery, PlanTaskProgress, Program,
    ProgramEvent, ProgramMilestone, ProgramMilestoneStatus, ProgramPolicy, ProgramStatus,
    ProgramUsage, Project, ProjectBudget, ProjectUsageSession, Question, RunBudgetSnapshot,
    RunCheckpoint, RunStep, RunToolCall, ScheduledTask, SearchHit, Session, SessionBundle,
    SessionContinuation, SessionRun, SessionRunPolicy, ShellPolicy, Skill, Source, SourceChunk,
    SourceCollection, SourceSearchHit, SourceStats, SteeringMessage, StoredMessage,
    TaskSchedulerPolicy, TaskStall, TeamPlan, TodoItem, ToolOutputAuditResult, ToolOutputPolicy,
    ToolOutputToolStat, UsageEntry, WorktreeRetentionPolicy, WriteLease, cache_hit_ratio,
};

use vp_protocol::remote::ExecutionHost;

use crate::paths;

/// Hard ceiling for a single stored tool output. Tools already bound their own
/// results; this prevents a pathological producer from writing an unbounded
/// blob while still allowing large build logs and diffs to be paged.
pub const MAX_TOOL_OUTPUT_BLOB_BYTES: usize = 32 * 1024 * 1024;

/// Hard ceiling for one stored centroid blob. The longest supported embedding
/// (8,192 dims) at the maximum centroid count (1,024) is ~32 MiB, so this
/// leaves room for the versioned header while still failing closed on a
/// corrupt or hand-edited row.
pub const MAX_MEMORY_INDEX_BLOB_BYTES: usize = 64 * 1024 * 1024;

pub fn tool_output_line_count(content: &str) -> u64 {
    if content.is_empty() {
        0
    } else {
        content.bytes().filter(|byte| *byte == b'\n').count() as u64 + 1
    }
}

/// A live or expired runtime-lease projection. The unpredictable lease token
/// is deliberately absent: it is an internal fencing secret returned only to
/// the acquiring guard. RPC, CLI, logs, and this public projection expose the
/// owner, process instance, monotonic generation, and expiry instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLease {
    pub lease_key: String,
    pub owner_id: String,
    #[serde(default)]
    pub owner_instance: String,
    pub kind: String,
    pub session_id: Option<String>,
    #[serde(default)]
    pub fencing_token: u64,
    pub acquired_at: String,
    pub heartbeat_at: String,
    #[serde(default)]
    pub renewed_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub released_at: Option<String>,
}

fn map_runtime_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuntimeLease> {
    Ok(RuntimeLease {
        lease_key: row.get(0)?,
        owner_id: row.get(1)?,
        owner_instance: row.get(2)?,
        kind: row.get(3)?,
        session_id: row.get(4)?,
        fencing_token: row.get::<_, i64>(5)?.max(0) as u64,
        acquired_at: row.get(6)?,
        heartbeat_at: row.get(7)?,
        renewed_at: row.get(8)?,
        expires_at: row.get(9)?,
        released_at: row.get(10)?,
    })
}

pub(crate) const RUNTIME_LEASE_COLUMNS: &str = "lease_key, owner_id, owner_instance, kind,
    session_id, fencing_token, acquired_at, heartbeat_at, renewed_at, expires_at, released_at";

/// One daemon-owned tool-output handle. The output body is never stored in
/// SQLite; it lives in the content-addressed `tool-outputs` blob directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredToolOutput {
    pub entry: vp_protocol::types::ToolOutputEntry,
    pub preview: String,
}

/// A pruning pass over the tool-output metadata table. `active_entries` is
/// reported separately because active sessions can temporarily exceed
/// `max_entries`; their unexpired handles are never pruned.
#[derive(Debug, Clone, Default)]
pub struct ToolOutputPruneRequest {
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub older_than_secs: Option<u64>,
    pub max_entries: Option<u64>,
    pub dry_run: bool,
}

/// Bounded status projection for RPC/CLI/Swift. It contains no output bodies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOutputStats {
    pub entries: u64,
    pub total_bytes: u64,
    pub preview_bytes: u64,
    pub active_entries: u64,
    pub expired_entries: u64,
    pub oldest_created_at: Option<String>,
    pub newest_created_at: Option<String>,
}

fn map_tool_output(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredToolOutput> {
    let expires_at: Option<String> = row.get(11)?;
    Ok(StoredToolOutput {
        entry: vp_protocol::types::ToolOutputEntry {
            id: row.get(0)?,
            session_id: row.get(1)?,
            tool_call_id: row.get(2)?,
            tool_name: row.get(3)?,
            sha256: row.get(4)?,
            bytes: row.get::<_, i64>(5)?.max(0) as u64,
            lines: row.get::<_, i64>(6)?.max(0) as u64,
            preview_bytes: row.get::<_, i64>(7)?.max(0) as u64,
            created_at: row.get(8)?,
            expires_at,
        },
        preview: row.get(9)?,
    })
}

const TOOL_OUTPUT_COLUMNS: &str = "id, session_id, tool_call_id, tool_name, sha256, bytes, lines, \
     length(preview), created_at, preview, project_id, expires_at";

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
    /// Monotonic acquisition generation across the SQLite database. It
    /// survives daemon restarts and is recorded in GUI action events.
    pub generation: u64,
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
        generation: row.get::<_, i64>(9)?.max(0) as u64,
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
    /// Event-backed durable run rows. `None` leaves the projection untouched.
    pub runs: Option<Vec<SessionRun>>,
    pub run_steps: Option<Vec<RunStep>>,
    pub run_checkpoints: Option<Vec<RunCheckpoint>>,
    /// Event-backed durable context checkpoint rows. `None` leaves the
    /// projection untouched.
    pub context_checkpoints: Option<Vec<ContextCheckpoint>>,
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
    /// Cosine similarity in 0..=1 from a current embedding, when present.
    pub semantic: Option<f64>,
}

/// One memory that may need an embedding. The embedded columns describe the
/// existing row, if any; the job columns describe the derived queue row. None
/// of these values are exposed through tool or RPC output.
#[derive(Debug, Clone)]
pub struct MemoryEmbeddingIndexCandidate {
    pub memory: MemoryItem,
    pub embedded_model: Option<String>,
    pub embedded_dimensions: Option<u32>,
    pub embedded_content_hash: Option<String>,
    pub job_status: Option<String>,
    pub job_model: Option<String>,
    pub job_attempts: u32,
    pub job_updated_at: Option<String>,
}

/// A bounded semantic candidate: full memory plus the encoded vector BLOB.
/// Decoding and cosine math happen in `vp-core::memory`, never in the SQL
/// projection or protocol layer.
#[derive(Debug, Clone)]
pub struct MemorySemanticCandidate {
    pub memory: MemoryItem,
    pub model: String,
    pub dimensions: u32,
    pub content_hash: String,
    pub vector: Vec<u8>,
}

/// One persisted derived IVF index generation. `centroids` is the versioned
/// little-endian f32 blob described by `vp_core::memory_index`; it is never
/// serialized into an RPC, event, log, or UI payload.
#[derive(Debug, Clone)]
pub struct MemoryEmbeddingIndexRow {
    pub project_id: String,
    pub model: String,
    pub dimensions: u32,
    pub generation: u64,
    pub nlist: u32,
    pub cluster_count: u32,
    pub vector_count: u32,
    pub centroids: Vec<u8>,
    pub updated_at: String,
    pub last_error: Option<String>,
    pub fallback_count: u64,
    pub last_fallback_at: Option<String>,
    pub last_fallback_reason: Option<String>,
}

/// Live vector and assignment counts for one index generation. Both counts
/// are scoped to active/stale memories with structurally valid current-model
/// embeddings, so an archived row or a corrupt vector cannot inflate
/// coverage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryEmbeddingIndexCounts {
    pub vector_count: u32,
    pub assigned_vectors: u32,
}

/// Structural audit of assignment rows for one project/generation. A derived
/// assignment can become invalid after a manual database edit, a partially
/// restored database, or a cluster-count change; recall already joins only
/// valid current embeddings, but maintenance should remove the stale rows so
/// coverage is honest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryEmbeddingAssignmentAudit {
    pub total: u32,
    pub stale_generation: u32,
    pub out_of_range: u32,
    pub missing_vector: u32,
}

impl MemoryEmbeddingAssignmentAudit {
    pub fn invalid(&self) -> u32 {
        self.stale_generation
            .saturating_add(self.out_of_range)
            .saturating_add(self.missing_vector)
    }
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
    /// Unified browser/computer GUI lease, grace, wait, and sensitive-target
    /// policy. Older manifests decode as the bounded default.
    #[serde(default)]
    pub gui_policy: GuiPolicy,
    pub artifact_policy: ArtifactPolicy,
    #[serde(default)]
    pub tool_output_policy: ToolOutputPolicy,
    pub mailbox_policy: MailboxPolicy,
    pub worktree_policy: WorktreeRetentionPolicy,
    #[serde(default)]
    pub checkpoint_policy: CheckpointRetentionPolicy,
    /// Project-scoped active-context checkpoint retention. Older manifests
    /// decode as the bounded default (sweep disabled).
    #[serde(default)]
    pub context_checkpoint_policy: ContextCheckpointPolicy,
    pub budget: ProjectBudget,
    #[serde(default)]
    pub task_scheduler_policy: TaskSchedulerPolicy,
    #[serde(default)]
    pub session_run_policy: SessionRunPolicy,
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
    daemon_identity: DaemonIdentity,
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
    let run_ids = bundle
        .session_runs
        .iter()
        .map(|run| run.run_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    for run in &bundle.session_runs {
        ensure_bundle_session("run", &run.session_id, &bundle.session.id)?;
        if run.project_id != bundle.project.id {
            return Err(anyhow!(
                "bundle run {} belongs to a different project",
                run.run_id
            ));
        }
        validate_run_status(
            &run.status,
            &["running", "completed", "failed", "interrupted", "resumed"],
        )?;
    }
    for step in &bundle.run_steps {
        ensure_bundle_session("run step", &step.session_id, &bundle.session.id)?;
        if !run_ids.contains(step.run_id.as_str()) {
            return Err(anyhow!(
                "bundle run step {} references a missing run",
                step.step_id
            ));
        }
    }
    for checkpoint in &bundle.run_checkpoints {
        ensure_bundle_session("run checkpoint", &checkpoint.session_id, &bundle.session.id)?;
        if !run_ids.contains(checkpoint.run_id.as_str()) {
            return Err(anyhow!(
                "bundle run checkpoint {} references a missing run",
                checkpoint.checkpoint_id
            ));
        }
    }
    let mut context_checkpoint_ids = std::collections::HashSet::new();
    for checkpoint in &bundle.context_checkpoints {
        ensure_bundle_session(
            "context checkpoint",
            &checkpoint.session_id,
            &bundle.session.id,
        )?;
        if checkpoint.project_id != bundle.project.id {
            return Err(anyhow!(
                "bundle context checkpoint {} belongs to a different project",
                checkpoint.id
            ));
        }
        if !context_checkpoint_ids.insert(checkpoint.id.clone()) {
            return Err(anyhow!(
                "bundle context checkpoint {} is duplicated",
                checkpoint.id
            ));
        }
        validate_context_checkpoint_row(checkpoint)?;
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

            CREATE TABLE IF NOT EXISTS project_gui_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
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

            -- Full tool output is operational state, not a replay projection
            -- and not bundle content. The body lives in
            -- `<data_dir>/tool-outputs/<sha256>.txt`; this table is the
            -- daemon-owned handle registry.
            CREATE TABLE IF NOT EXISTS tool_outputs (
                id           TEXT PRIMARY KEY,
                project_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                tool_call_id TEXT,
                tool_name    TEXT NOT NULL,
                sha256       TEXT NOT NULL,
                bytes        INTEGER NOT NULL,
                lines        INTEGER NOT NULL,
                created_at   TEXT NOT NULL,
                expires_at   TEXT,
                preview      TEXT NOT NULL DEFAULT ''
            );

            CREATE INDEX IF NOT EXISTS idx_tool_outputs_project
                ON tool_outputs(project_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_tool_outputs_session
                ON tool_outputs(session_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_tool_outputs_expires
                ON tool_outputs(expires_at);

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

            CREATE TABLE IF NOT EXISTS memory_embeddings (
                memory_id    TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                model        TEXT NOT NULL,
                dimensions   INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                vector       BLOB NOT NULL,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_embeddings_model
                ON memory_embeddings(model, dimensions);

            CREATE TABLE IF NOT EXISTS memory_embedding_jobs (
                memory_id       TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                model           TEXT NOT NULL,
                status          TEXT NOT NULL,
                attempts        INTEGER NOT NULL DEFAULT 0,
                last_error      TEXT,
                next_attempt_at TEXT,
                updated_at      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_embedding_jobs_status
                ON memory_embedding_jobs(status, next_attempt_at, updated_at);

            CREATE TABLE IF NOT EXISTS memory_embedding_usage (
                id                 TEXT PRIMARY KEY,
                project_id         TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                model              TEXT NOT NULL,
                calls              INTEGER NOT NULL,
                input_tokens       INTEGER NOT NULL,
                estimated_tokens   INTEGER NOT NULL,
                bytes              INTEGER NOT NULL,
                provider_reported  INTEGER NOT NULL,
                created_at         TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_embedding_usage_project
                ON memory_embedding_usage(project_id, model, created_at);

            CREATE TABLE IF NOT EXISTS memory_embedding_index (
                project_id           TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                model                TEXT NOT NULL,
                dimensions           INTEGER NOT NULL,
                generation           INTEGER NOT NULL,
                nlist                INTEGER NOT NULL,
                cluster_count        INTEGER NOT NULL,
                vector_count         INTEGER NOT NULL,
                centroids            BLOB NOT NULL,
                updated_at           TEXT NOT NULL,
                last_error           TEXT,
                fallback_count       INTEGER NOT NULL DEFAULT 0,
                last_fallback_at     TEXT,
                last_fallback_reason TEXT
            );

            CREATE TABLE IF NOT EXISTS memory_embedding_assignments (
                memory_id  TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                generation INTEGER NOT NULL,
                cluster    INTEGER NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_embedding_assignments_generation
                ON memory_embedding_assignments(generation, cluster);

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
                id                   TEXT PRIMARY KEY,
                name                 TEXT NOT NULL UNIQUE,
                command              TEXT NOT NULL,
                args                 TEXT NOT NULL,
                env                  TEXT NOT NULL,
                enabled              INTEGER NOT NULL DEFAULT 1,
                auto_approve         INTEGER NOT NULL DEFAULT 0,
                timeout_secs         INTEGER NOT NULL DEFAULT 1800,
                created_at           TEXT NOT NULL,
                transport            TEXT NOT NULL DEFAULT 'stdio',
                endpoint             TEXT,
                auth_header_env      TEXT,
                headers              TEXT NOT NULL DEFAULT '{}',
                connect_timeout_secs INTEGER NOT NULL DEFAULT 30,
                allow_remote         INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS project_memory_policies (
                project_id     TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled        INTEGER NOT NULL,
                max_items      INTEGER,
                max_age_days   INTEGER,
                embedding_json TEXT,
                updated_at     TEXT NOT NULL
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

            CREATE TABLE IF NOT EXISTS project_tool_output_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
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

            CREATE TABLE IF NOT EXISTS project_session_run_policies (
                project_id     TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                auto_resume    INTEGER NOT NULL DEFAULT 0,
                max_attempts   INTEGER NOT NULL DEFAULT 1,
                max_age_hours  INTEGER NOT NULL DEFAULT 24,
                updated_at     TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_runs (
                run_id                 TEXT PRIMARY KEY,
                session_id             TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id             TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                agent_id               TEXT NOT NULL,
                agent_name             TEXT NOT NULL DEFAULT '',
                agent_role             TEXT NOT NULL DEFAULT '',
                task_id                TEXT,
                department             TEXT,
                objective              TEXT NOT NULL DEFAULT '',
                provider               TEXT NOT NULL DEFAULT '',
                model                  TEXT NOT NULL DEFAULT '',
                status                 TEXT NOT NULL,
                attempt                INTEGER NOT NULL DEFAULT 1,
                resumed_from_run_id    TEXT,
                resume_root_id         TEXT NOT NULL DEFAULT '',
                top_level              INTEGER NOT NULL DEFAULT 0,
                started_at             TEXT NOT NULL,
                updated_at             TEXT NOT NULL,
                ended_at               TEXT,
                budget_json            TEXT NOT NULL DEFAULT '{}',
                worktree_head          TEXT NOT NULL DEFAULT '',
                plan_revision          INTEGER NOT NULL DEFAULT 0,
                last_event_seq         INTEGER NOT NULL DEFAULT 0,
                failure_json           TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_session_runs_session
                ON session_runs(session_id, started_at DESC);
            CREATE INDEX IF NOT EXISTS idx_session_runs_status
                ON session_runs(status, started_at);
            CREATE INDEX IF NOT EXISTS idx_session_runs_root
                ON session_runs(resume_root_id, started_at);
            CREATE INDEX IF NOT EXISTS idx_session_runs_resumable
                ON session_runs(top_level, status, started_at);

            CREATE TABLE IF NOT EXISTS run_steps (
                step_id      TEXT PRIMARY KEY,
                run_id       TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq          INTEGER NOT NULL,
                turn         INTEGER NOT NULL DEFAULT 0,
                kind         TEXT NOT NULL,
                status       TEXT NOT NULL,
                input_hash   TEXT NOT NULL DEFAULT '',
                output_hash  TEXT NOT NULL DEFAULT '',
                payload_json TEXT NOT NULL DEFAULT '{}',
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                UNIQUE (run_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_run_steps_run
                ON run_steps(run_id, seq);
            CREATE INDEX IF NOT EXISTS idx_run_steps_session
                ON run_steps(session_id, created_at);

            CREATE TABLE IF NOT EXISTS run_tool_calls (
                run_id             TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                session_id         TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                step_id            TEXT NOT NULL,
                tool_call_id       TEXT NOT NULL,
                tool_name          TEXT NOT NULL,
                input_fingerprint  TEXT NOT NULL,
                status             TEXT NOT NULL,
                result_json        TEXT,
                error              TEXT,
                consumed_by_run_id TEXT,
                consumed_at        TEXT,
                created_at         TEXT NOT NULL,
                updated_at         TEXT NOT NULL,
                PRIMARY KEY (run_id, tool_call_id)
            );

            CREATE INDEX IF NOT EXISTS idx_run_tool_calls_session
                ON run_tool_calls(session_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_run_tool_calls_match
                ON run_tool_calls(tool_name, input_fingerprint, status);
            CREATE INDEX IF NOT EXISTS idx_run_tool_calls_step
                ON run_tool_calls(run_id, step_id);

            CREATE TABLE IF NOT EXISTS run_checkpoints (
                checkpoint_id  TEXT PRIMARY KEY,
                run_id         TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq            INTEGER NOT NULL,
                turn           INTEGER NOT NULL DEFAULT 0,
                message_cursor INTEGER NOT NULL DEFAULT 0,
                event_cursor   INTEGER NOT NULL DEFAULT 0,
                budget_json    TEXT NOT NULL DEFAULT '{}',
                worktree_head  TEXT NOT NULL DEFAULT '',
                plan_revision  INTEGER NOT NULL DEFAULT 0,
                created_at     TEXT NOT NULL,
                UNIQUE (run_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_run_checkpoints_run
                ON run_checkpoints(run_id, seq);

            -- Durable active-context boundaries. The full checkpoint is also
            -- carried by context.checkpointed events, so this table is an
            -- event-backed projection that replay/repair can rebuild.
            CREATE TABLE IF NOT EXISTS context_checkpoints (
                id                  TEXT PRIMARY KEY,
                session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id          TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                label               TEXT NOT NULL DEFAULT '',
                mode                TEXT NOT NULL DEFAULT 'manual',
                summary             TEXT,
                through_message_id  TEXT,
                message_cursor      INTEGER NOT NULL DEFAULT 0,
                plan_revision       INTEGER NOT NULL DEFAULT 0,
                provider            TEXT NOT NULL DEFAULT '',
                model               TEXT NOT NULL DEFAULT '',
                prompt_prefix_hash  TEXT NOT NULL DEFAULT '',
                worktree_head       TEXT NOT NULL DEFAULT '',
                tokens_before       INTEGER NOT NULL DEFAULT 0,
                tokens_after        INTEGER NOT NULL DEFAULT 0,
                structured_state    TEXT,
                source_compaction_seq INTEGER,
                automatic           INTEGER NOT NULL DEFAULT 0,
                created_at          TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_context_checkpoints_session
                ON context_checkpoints(session_id, created_at);

            CREATE TABLE IF NOT EXISTS project_context_checkpoint_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                max_checkpoints  INTEGER NOT NULL DEFAULT 20,
                max_age_days     INTEGER NOT NULL DEFAULT 30
            );

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
                run_count       INTEGER NOT NULL DEFAULT 0,
                lease_owner     TEXT,
                lease_token     TEXT,
                lease_expires_at TEXT,
                delivery_attempt INTEGER NOT NULL DEFAULT 0
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
                lease_key      TEXT PRIMARY KEY,
                owner_id       TEXT NOT NULL,
                owner_instance TEXT NOT NULL DEFAULT '',
                kind           TEXT NOT NULL,
                session_id     TEXT,
                lease_token    TEXT NOT NULL DEFAULT '',
                fencing_token  INTEGER NOT NULL DEFAULT 0,
                acquired_at    TEXT NOT NULL,
                heartbeat_at   TEXT NOT NULL,
                renewed_at     TEXT NOT NULL DEFAULT '',
                expires_at     TEXT NOT NULL,
                released_at    TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_runtime_leases_expiry
                ON runtime_leases(expires_at);
            CREATE INDEX IF NOT EXISTS idx_runtime_leases_session
                ON runtime_leases(session_id);
            -- Fencing generations survive lease-row cleanup. A released or
            -- expired row may be deleted, but the next acquisition for the key
            -- must still advance monotonically.
            CREATE TABLE IF NOT EXISTS runtime_lease_fences (
                lease_key     TEXT PRIMARY KEY,
                fencing_token INTEGER NOT NULL DEFAULT 0,
                updated_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runtime_lease_audit (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                lease_key     TEXT NOT NULL,
                kind          TEXT NOT NULL,
                action        TEXT NOT NULL,
                owner_id      TEXT NOT NULL,
                owner_instance TEXT NOT NULL DEFAULT '',
                fencing_token INTEGER NOT NULL DEFAULT 0,
                detail        TEXT,
                occurred_at   TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_runtime_lease_audit_key
                ON runtime_lease_audit(lease_key, id DESC);
            CREATE INDEX IF NOT EXISTS idx_runtime_lease_audit_time
                ON runtime_lease_audit(occurred_at);

            CREATE TABLE IF NOT EXISTS computer_leases (
                lease_id      TEXT PRIMARY KEY,
                surface       TEXT NOT NULL UNIQUE,
                owner_id      TEXT NOT NULL,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id      TEXT NOT NULL,
                agent_name    TEXT NOT NULL,
                acquired_at   TEXT NOT NULL,
                heartbeat_at  TEXT NOT NULL,
                expires_at    TEXT NOT NULL,
                generation    INTEGER NOT NULL DEFAULT 0
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
        ensure_column(&conn, "session_continuations", "lease_owner", "TEXT")?;
        ensure_column(&conn, "session_continuations", "lease_token", "TEXT")?;
        ensure_column(&conn, "session_continuations", "lease_expires_at", "TEXT")?;
        ensure_column(
            &conn,
            "session_continuations",
            "delivery_attempt",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "runtime_leases",
            "owner_instance",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &conn,
            "runtime_leases",
            "lease_token",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &conn,
            "runtime_leases",
            "fencing_token",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "runtime_leases",
            "renewed_at",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(&conn, "runtime_leases", "released_at", "TEXT")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_runtime_leases_instance
                 ON runtime_leases(owner_instance, expires_at);
             CREATE INDEX IF NOT EXISTS idx_session_continuations_claim
                 ON session_continuations(status, lease_expires_at);
             CREATE TABLE IF NOT EXISTS runtime_lease_fences (
                 lease_key     TEXT PRIMARY KEY,
                 fencing_token INTEGER NOT NULL DEFAULT 0,
                 updated_at    TEXT NOT NULL
             );",
        )?;
        ensure_column(
            &conn,
            "computer_leases",
            "generation",
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
            "acp_agents",
            "transport",
            "TEXT NOT NULL DEFAULT 'stdio'",
        )?;
        ensure_column(&conn, "acp_agents", "endpoint", "TEXT")?;
        ensure_column(&conn, "acp_agents", "auth_header_env", "TEXT")?;
        ensure_column(&conn, "acp_agents", "headers", "TEXT NOT NULL DEFAULT '{}'")?;
        ensure_column(
            &conn,
            "acp_agents",
            "connect_timeout_secs",
            "INTEGER NOT NULL DEFAULT 30",
        )?;
        ensure_column(
            &conn,
            "acp_agents",
            "allow_remote",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
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
                 ON memory_conflicts(project_id, status, detected_at);
             CREATE TABLE IF NOT EXISTS memory_embeddings (
                 memory_id    TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                 model        TEXT NOT NULL,
                 dimensions   INTEGER NOT NULL,
                 content_hash TEXT NOT NULL,
                 vector       BLOB NOT NULL,
                 created_at   TEXT NOT NULL,
                 updated_at   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_embeddings_model
                 ON memory_embeddings(model, dimensions);
             CREATE TABLE IF NOT EXISTS memory_embedding_jobs (
                 memory_id       TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                 model           TEXT NOT NULL,
                 status          TEXT NOT NULL,
                 attempts        INTEGER NOT NULL DEFAULT 0,
                 last_error      TEXT,
                 next_attempt_at TEXT,
                 updated_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_embedding_jobs_status
                 ON memory_embedding_jobs(status, next_attempt_at, updated_at);
             CREATE TABLE IF NOT EXISTS memory_embedding_usage (
                 id                 TEXT PRIMARY KEY,
                 project_id         TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                 model              TEXT NOT NULL,
                 calls              INTEGER NOT NULL,
                 input_tokens       INTEGER NOT NULL,
                 estimated_tokens   INTEGER NOT NULL,
                 bytes              INTEGER NOT NULL,
                 provider_reported  INTEGER NOT NULL,
                 created_at         TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_embedding_usage_project
                 ON memory_embedding_usage(project_id, model, created_at);
             CREATE TABLE IF NOT EXISTS memory_embedding_index (
                 project_id           TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                 model                TEXT NOT NULL,
                 dimensions           INTEGER NOT NULL,
                 generation           INTEGER NOT NULL,
                 nlist                INTEGER NOT NULL,
                 cluster_count        INTEGER NOT NULL,
                 vector_count         INTEGER NOT NULL,
                 centroids            BLOB NOT NULL,
                 updated_at           TEXT NOT NULL,
                 last_error           TEXT,
                 fallback_count       INTEGER NOT NULL DEFAULT 0,
                 last_fallback_at     TEXT,
                 last_fallback_reason TEXT
             );
             CREATE TABLE IF NOT EXISTS memory_embedding_assignments (
                 memory_id  TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                 generation INTEGER NOT NULL,
                 cluster    INTEGER NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_embedding_assignments_generation
                 ON memory_embedding_assignments(generation, cluster);",
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
        ensure_column(&conn, "project_memory_policies", "embedding_json", "TEXT")?;
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
        let daemon_identity = crate::store::lease::load_or_create_daemon_identity(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            data_dir,
            mirror_lock: Arc::new(Mutex::new(())),
            manifest_lock: Arc::new(Mutex::new(())),
            daemon_identity,
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
                gui_policy: self.get_gui_policy(&project.id)?,
                artifact_policy: self.get_artifact_policy(&project.id)?,
                tool_output_policy: self.get_tool_output_policy(&project.id)?,
                mailbox_policy: self.get_mailbox_policy(&project.id)?,
                worktree_policy: self.get_worktree_policy(&project.id)?,
                checkpoint_policy: self.get_checkpoint_policy(&project.id)?,
                context_checkpoint_policy: self.get_context_checkpoint_policy(&project.id)?,
                budget: self.get_project_budget(&project.id)?,
                task_scheduler_policy: self.get_task_scheduler_policy(&project.id)?,
                session_run_policy: self.get_session_run_policy(&project.id)?,
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

    /// Aggregate spend for one milestone across every attempt. The current
    /// `program_milestones.session_id` is included as a fallback so legacy rows
    /// created before the dispatch ledger are still counted once.
    pub fn program_milestone_usage(&self, milestone_id: &str) -> Result<ProgramUsage> {
        let conn = self.lock()?;
        let (tokens, cost) = conn.query_row(
            "SELECT COALESCE(SUM(s.tokens_in + s.tokens_out), 0),
                    COALESCE(SUM(s.cost_spent_usd), 0.0)
             FROM sessions s
             WHERE s.id IN (
                 SELECT session_id FROM program_milestone_sessions
                 WHERE milestone_id = ?1
                 UNION
                 SELECT session_id FROM program_milestones
                 WHERE id = ?1 AND session_id IS NOT NULL
             )",
            params![milestone_id],
            |row| Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, f64>(1)?)),
        )?;
        Ok(ProgramUsage {
            token_spent: tokens,
            cost_spent_usd: cost,
        })
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

    /// Project GUI session policy. A missing row decodes as the bounded
    /// default: 45s grace, 15s wait, 30s status cache, sensitive targets
    /// denied. Older databases gain the table on open.
    pub fn get_gui_policy(&self, project_id: &str) -> Result<GuiPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_gui_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(stored
            .and_then(|raw| serde_json::from_str::<GuiPolicy>(&raw).ok())
            .unwrap_or_default()
            .normalized())
    }

    pub fn set_gui_policy(&self, project_id: &str, policy: &GuiPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_gui_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![
                project_id,
                serde_json::to_string(&policy.clone().normalized())?,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Resolve the GUI policy for the project that owns a session without
    /// loading the whole session row in the caller.
    pub fn gui_policy_for_session(&self, session_id: &str) -> Result<GuiPolicy> {
        let conn = self.lock()?;
        let project_id = conn
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        drop(conn);
        match project_id {
            Some(project_id) => self.get_gui_policy(&project_id),
            None => Ok(GuiPolicy::default()),
        }
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

    // ------------------------------------------------------------------
    // Tool output store
    //
    // Full tool outputs are operational state. They are content-addressed,
    // daemon-owned, and never addressed by a user-supplied path. Metadata rows
    // are intentionally not part of replay/repair or bundle content.
    // ------------------------------------------------------------------

    fn tool_output_blob_dir(&self) -> PathBuf {
        self.data_dir.join("tool-outputs")
    }

    /// Resolve a validated content hash to the blob path. The only accepted
    /// input is a 64-character lowercase hex SHA-256; there is no user path
    /// parameter anywhere in this API.
    pub fn tool_output_blob_path(&self, sha256: &str) -> Result<PathBuf> {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(anyhow!("invalid tool-output content hash"));
        }
        Ok(self.tool_output_blob_dir().join(format!("{sha256}.txt")))
    }

    fn write_tool_output_blob(&self, sha256: &str, content: &[u8]) -> Result<PathBuf> {
        let dir = self.tool_output_blob_dir();
        if let Ok(metadata) = std::fs::symlink_metadata(&dir)
            && metadata.file_type().is_symlink()
        {
            return Err(anyhow!(
                "tool-output directory {} is a symlink; refusing to follow",
                dir.display()
            ));
        }
        paths::ensure_private_dir(&dir)
            .with_context(|| format!("creating tool-output directory {}", dir.display()))?;
        let path = self.tool_output_blob_path(sha256)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(anyhow!(
                        "tool-output blob {} is not a regular file",
                        path.display()
                    ));
                }
                let existing = std::fs::read(&path).with_context(|| {
                    format!("reading existing tool-output blob {}", path.display())
                })?;
                let existing_sha = format!("{:x}", Sha256::digest(&existing));
                if existing_sha != sha256 {
                    return Err(anyhow!(
                        "tool-output blob {} content does not match its hash",
                        path.display()
                    ));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspecting tool-output blob {}", path.display()));
            }
        }
        let temp = dir.join(format!(".{sha256}.{}.tmp", Uuid::now_v7()));
        let result = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temp)
                .with_context(|| format!("creating tool-output blob {}", temp.display()))?;
            file.write_all(content)
                .with_context(|| format!("writing tool-output blob {}", temp.display()))?;
            file.sync_all()
                .with_context(|| format!("syncing tool-output blob {}", temp.display()))?;
            drop(file);
            paths::ensure_private_file(&temp)?;
            std::fs::rename(&temp, &path).with_context(|| {
                format!(
                    "renaming tool-output blob {} to {}",
                    temp.display(),
                    path.display()
                )
            })?;
            paths::ensure_private_file(&path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result.map(|()| path)
    }

    /// Persist one full tool output and its daemon-owned handle. The caller
    /// supplies a bounded preview; `content` is the exact full text that must
    /// survive until retention expires.
    #[allow(clippy::too_many_arguments)]
    pub fn spill_tool_output(
        &self,
        project_id: &str,
        session_id: &str,
        tool_call_id: Option<&str>,
        tool_name: &str,
        content: &str,
        preview: &str,
        retention_secs: u64,
    ) -> Result<StoredToolOutput> {
        if content.len() > MAX_TOOL_OUTPUT_BLOB_BYTES {
            return Err(anyhow!(
                "tool output is {} bytes; the store limit is {} bytes",
                content.len(),
                MAX_TOOL_OUTPUT_BLOB_BYTES
            ));
        }
        let sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        self.write_tool_output_blob(&sha256, content.as_bytes())?;
        let created_at = now_rfc3339();
        let created = OffsetDateTime::parse(&created_at, &Rfc3339)
            .context("parsing tool-output creation timestamp")?;
        let expires = created + time::Duration::seconds(retention_secs.min(31_536_000) as i64);
        let expires_at = expires
            .format(&Rfc3339)
            .context("formatting tool-output expiry")?;
        let id = format!("out_{}", Uuid::now_v7());
        let lines = tool_output_line_count(content);
        let entry = vp_protocol::types::ToolOutputEntry {
            id: id.clone(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.map(str::to_string),
            tool_name: tool_name.to_string(),
            sha256,
            bytes: content.len() as u64,
            lines,
            preview_bytes: preview.len() as u64,
            created_at: created_at.clone(),
            expires_at: Some(expires_at.clone()),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tool_outputs
             (id, project_id, session_id, tool_call_id, tool_name, sha256, bytes, lines,
              created_at, expires_at, preview)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                project_id,
                session_id,
                entry.tool_call_id,
                entry.tool_name,
                entry.sha256,
                entry.bytes as i64,
                entry.lines as i64,
                created_at,
                expires_at,
                preview
            ],
        )?;
        Ok(StoredToolOutput {
            entry,
            preview: preview.to_string(),
        })
    }

    /// Force one handle's expiry. This is a maintenance/test seam: normal
    /// runtime code always derives expiry from the policy at spill time.
    pub fn set_tool_output_expiry(&self, id: &str, expires_at: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE tool_outputs SET expires_at = ?1 WHERE id = ?2",
            params![expires_at, id],
        )?;
        Ok(changed > 0)
    }

    pub fn get_tool_output(&self, id: &str) -> Result<Option<StoredToolOutput>> {
        if id.len() > 128 || id.is_empty() {
            return Ok(None);
        }
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {TOOL_OUTPUT_COLUMNS} FROM tool_outputs WHERE id = ?1"),
            params![id],
            map_tool_output,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Read and verify the full body for one handle. The caller must already
    /// have validated session ownership; this method never treats the handle
    /// as a path and re-checks the content hash before returning it.
    pub fn read_tool_output_text(&self, output: &StoredToolOutput) -> Result<String> {
        let path = self.tool_output_blob_path(&output.entry.sha256)?;
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("inspecting tool-output blob {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(anyhow!(
                "tool-output blob {} is not a regular file",
                path.display()
            ));
        }
        if metadata.len() != output.entry.bytes {
            return Err(anyhow!(
                "tool-output blob {} size does not match metadata",
                path.display()
            ));
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading tool-output blob {}", path.display()))?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != output.entry.sha256 {
            return Err(anyhow!(
                "tool-output blob {} content does not match its hash",
                path.display()
            ));
        }
        String::from_utf8(bytes).context("tool-output blob is not valid UTF-8")
    }

    #[allow(clippy::too_many_arguments)]
    pub fn list_tool_outputs(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
        tool_name: Option<&str>,
        before_created_at: Option<&str>,
        include_expired: bool,
        limit: usize,
    ) -> Result<Vec<StoredToolOutput>> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let limit = limit.clamp(1, 10_000) as i64;
        let mut stmt = conn.prepare(&format!(
            "SELECT {TOOL_OUTPUT_COLUMNS} FROM tool_outputs
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR session_id = ?2)
               AND (?3 IS NULL OR tool_name = ?3)
               AND (?4 IS NULL OR created_at < ?4)
               AND (?5 = 1 OR expires_at IS NULL OR expires_at > ?6)
             ORDER BY created_at DESC, id DESC LIMIT ?7"
        ))?;
        let rows = stmt
            .query_map(
                params![
                    project_id,
                    session_id,
                    tool_name,
                    before_created_at,
                    if include_expired { 1 } else { 0 },
                    now,
                    limit
                ],
                map_tool_output,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn tool_output_total(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM tool_outputs
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
            params![project_id, session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn tool_output_stats(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<ToolOutputStats> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let mut stats = ToolOutputStats::default();
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(t.bytes), 0), COALESCE(SUM(length(t.preview)), 0),
                    COALESCE(SUM(CASE WHEN t.expires_at IS NOT NULL AND t.expires_at <= ?3
                                      THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN s.status IN ('queued', 'running', 'paused', 'waiting_input')
                                      THEN 1 ELSE 0 END), 0),
                    MIN(t.created_at), MAX(t.created_at)
             FROM tool_outputs t JOIN sessions s ON s.id = t.session_id
             WHERE (?1 IS NULL OR t.project_id = ?1)
               AND (?2 IS NULL OR t.session_id = ?2)",
            params![project_id, session_id, now],
            |row| {
                stats.entries = row.get::<_, i64>(0)?.max(0) as u64;
                stats.total_bytes = row.get::<_, i64>(1)?.max(0) as u64;
                stats.preview_bytes = row.get::<_, i64>(2)?.max(0) as u64;
                stats.expired_entries = row.get::<_, i64>(3)?.max(0) as u64;
                stats.active_entries = row.get::<_, i64>(4)?.max(0) as u64;
                stats.oldest_created_at = row.get(5)?;
                stats.newest_created_at = row.get(6)?;
                Ok(())
            },
        )?;
        Ok(stats)
    }

    pub fn tool_output_tool_stats(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<ToolOutputToolStat>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT tool_name, COUNT(*), COALESCE(SUM(bytes), 0),
                    COALESCE(SUM(length(preview)), 0)
             FROM tool_outputs
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR session_id = ?2)
             GROUP BY tool_name
             ORDER BY tool_name ASC",
        )?;
        let rows = stmt
            .query_map(params![project_id, session_id], |row| {
                Ok(ToolOutputToolStat {
                    tool_name: row.get(0)?,
                    entries: row.get::<_, i64>(1)?.max(0) as u64,
                    bytes: row.get::<_, i64>(2)?.max(0) as u64,
                    preview_bytes: row.get::<_, i64>(3)?.max(0) as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn prune_tool_outputs(
        &self,
        request: &ToolOutputPruneRequest,
    ) -> Result<vp_protocol::types::ToolOutputPruneResult> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        struct Candidate {
            id: String,
            sha256: String,
            bytes: u64,
            created_at: String,
            expires_at: Option<String>,
            active: bool,
        }
        let mut stmt = tx.prepare(
            "SELECT t.id, t.sha256, t.bytes, t.created_at, t.expires_at,
                    CASE WHEN s.status IN ('queued', 'running', 'paused', 'waiting_input')
                         THEN 1 ELSE 0 END
             FROM tool_outputs t JOIN sessions s ON s.id = t.session_id
             WHERE (?1 IS NULL OR t.project_id = ?1)
               AND (?2 IS NULL OR t.session_id = ?2)
             ORDER BY t.created_at ASC, t.id ASC",
        )?;
        let rows = stmt
            .query_map(params![request.project_id, request.session_id], |row| {
                Ok(Candidate {
                    id: row.get(0)?,
                    sha256: row.get(1)?,
                    bytes: row.get::<_, i64>(2)?.max(0) as u64,
                    created_at: row.get(3)?,
                    expires_at: row.get(4)?,
                    active: row.get::<_, i64>(5)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let cutoff = request
            .older_than_secs
            .map(|seconds| {
                let now = OffsetDateTime::parse(&now, &Rfc3339)?;
                let cutoff = now - time::Duration::seconds(seconds.min(31_536_000) as i64);
                cutoff
                    .format(&Rfc3339)
                    .context("formatting tool-output cutoff")
            })
            .transpose()?;
        let mut candidates = std::collections::BTreeSet::new();
        let mut overflow_candidates = std::collections::BTreeSet::new();
        let mut expired_removed = 0u64;
        let protected_active = rows.iter().filter(|row| row.active).count() as u64;
        for row in &rows {
            let expired = row
                .expires_at
                .as_deref()
                .is_some_and(|expires| expires <= now.as_str());
            if expired {
                candidates.insert(row.id.clone());
                expired_removed += 1;
                continue;
            }
            if row.active {
                continue;
            }
            if cutoff
                .as_deref()
                .is_some_and(|cutoff| row.created_at.as_str() <= cutoff)
            {
                candidates.insert(row.id.clone());
            }
        }
        if let Some(max_entries) = request.max_entries {
            let remaining = rows
                .iter()
                .filter(|row| !candidates.contains(&row.id))
                .collect::<Vec<_>>();
            if remaining.len() as u64 > max_entries {
                let mut keep = remaining
                    .iter()
                    .map(|row| (row.created_at.clone(), row.id.clone()))
                    .collect::<Vec<_>>();
                // Newest-first keeps the cap useful; creation time and id are
                // deterministic tie-breakers.
                keep.sort_by(|left, right| right.cmp(left));
                let keep = keep
                    .into_iter()
                    .take(max_entries as usize)
                    .map(|(_, id)| id)
                    .collect::<std::collections::BTreeSet<_>>();
                for row in remaining {
                    if !keep.contains(&row.id) && !row.active {
                        candidates.insert(row.id.clone());
                        overflow_candidates.insert(row.id.clone());
                    }
                }
            }
        }
        let removed_rows = rows
            .iter()
            .filter(|row| candidates.contains(&row.id))
            .collect::<Vec<_>>();
        let overflow_removed = overflow_candidates
            .iter()
            .filter(|id| candidates.contains(*id))
            .count() as u64;
        let removed_bytes = removed_rows
            .iter()
            .fold(0u64, |total, row| total.saturating_add(row.bytes));
        if !request.dry_run {
            for row in &removed_rows {
                tx.execute("DELETE FROM tool_outputs WHERE id = ?1", params![row.id])?;
            }
        }
        let result = vp_protocol::types::ToolOutputPruneResult {
            project_id: request.project_id.clone(),
            session_id: request.session_id.clone(),
            candidates: removed_rows.len() as u64,
            removed: if request.dry_run {
                0
            } else {
                removed_rows.len() as u64
            },
            removed_bytes: if request.dry_run { 0 } else { removed_bytes },
            expired_removed,
            overflow_removed,
            protected_active,
            dry_run: request.dry_run,
        };
        tx.commit()?;
        if !request.dry_run {
            let mut checked = std::collections::BTreeSet::new();
            for row in &removed_rows {
                if !checked.insert(row.sha256.clone()) {
                    continue;
                }
                let references: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM tool_outputs WHERE sha256 = ?1",
                    params![row.sha256],
                    |row| row.get(0),
                )?;
                if references == 0
                    && let Ok(path) = self.tool_output_blob_path(&row.sha256)
                    && let Ok(metadata) = std::fs::symlink_metadata(&path)
                    && metadata.is_file()
                    && !metadata.file_type().is_symlink()
                {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        Ok(result)
    }

    pub fn get_tool_output_policy(&self, project_id: &str) -> Result<ToolOutputPolicy> {
        let conn = self.lock()?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT policy FROM project_tool_output_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        match stored {
            Some(policy) => {
                serde_json::from_str(&policy).context("parsing stored tool-output policy")
            }
            None => Ok(ToolOutputPolicy::default()),
        }
    }

    pub fn set_tool_output_policy(
        &self,
        project_id: &str,
        policy: &ToolOutputPolicy,
    ) -> Result<()> {
        let text = serde_json::to_string(policy).context("serializing tool-output policy")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_tool_output_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, text, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Verify the metadata/blob boundary. Metadata is never deleted here;
    /// optional cleanup removes only unreferenced, hash-shaped blob files.
    pub fn audit_tool_outputs(
        &self,
        project_id: Option<&str>,
        verify_hashes: bool,
        clean_orphans: bool,
    ) -> Result<ToolOutputAuditResult> {
        let checked_at = now_rfc3339();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT sha256, bytes FROM tool_outputs
             WHERE (?1 IS NULL OR project_id = ?1)
             ORDER BY sha256 ASC",
        )?;
        let rows = stmt
            .query_map(params![project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut all_hashes = std::collections::BTreeSet::new();
        {
            let mut stmt = conn.prepare("SELECT DISTINCT sha256 FROM tool_outputs")?;
            let hashes = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for hash in hashes {
                all_hashes.insert(hash?);
            }
        }
        let mut audit = ToolOutputAuditResult {
            project_id: project_id.map(str::to_string),
            rows: rows.len() as u64,
            checked_at,
            ..ToolOutputAuditResult::default()
        };
        let dir = self.tool_output_blob_dir();
        let mut referenced = std::collections::BTreeSet::new();
        for (sha256, bytes) in &rows {
            referenced.insert(sha256.clone());
            let Ok(path) = self.tool_output_blob_path(sha256) else {
                audit.missing_blobs += 1;
                continue;
            };
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        audit.unsafe_blob_files += 1;
                        continue;
                    }
                    if metadata.len() != *bytes {
                        audit.hash_mismatches += 1;
                        continue;
                    }
                    if verify_hashes {
                        let data = std::fs::read(&path).with_context(|| {
                            format!("reading tool-output blob {}", path.display())
                        })?;
                        let actual = format!("{:x}", Sha256::digest(&data));
                        if actual != *sha256 {
                            audit.hash_mismatches += 1;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    audit.missing_blobs += 1;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("inspecting tool-output blob {}", path.display())
                    });
                }
            }
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading tool-output directory {}", dir.display()));
            }
        };
        if let Some(entries) = entries {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Some(hash) = name.strip_suffix(".txt") else {
                    continue;
                };
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    audit.unsafe_blob_files += 1;
                    continue;
                }
                let file_type = entry.file_type()?;
                if file_type.is_symlink() || !file_type.is_file() {
                    audit.unsafe_blob_files += 1;
                    continue;
                }
                let metadata = entry.metadata()?;
                audit.blob_files += 1;
                if !all_hashes.contains(hash) {
                    audit.orphan_blobs += 1;
                    audit.orphan_bytes = audit.orphan_bytes.saturating_add(metadata.len());
                    // A concurrent spill writes the blob just before inserting
                    // its metadata row. Do not delete a fresh unreferenced file:
                    // it may be in that narrow window. Old crash leftovers are
                    // safe to remove.
                    let old_enough = metadata
                        .modified()
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age >= std::time::Duration::from_secs(300));
                    if clean_orphans && old_enough {
                        let path = entry.path();
                        std::fs::remove_file(&path).with_context(|| {
                            format!("removing orphan tool-output blob {}", path.display())
                        })?;
                        audit.cleaned_orphans += 1;
                        audit.cleaned_bytes = audit.cleaned_bytes.saturating_add(metadata.len());
                    }
                }
            }
        }
        Ok(audit)
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

    pub fn get_context_checkpoint_policy(
        &self,
        project_id: &str,
    ) -> Result<ContextCheckpointPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, max_checkpoints, max_age_days
                 FROM project_context_checkpoint_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(ContextCheckpointPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        max_checkpoints: row.get::<_, i64>(1)?.clamp(1, 100) as u32,
                        max_age_days: row.get::<_, i64>(2)?.clamp(0, 365) as u32,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_context_checkpoint_policy(
        &self,
        project_id: &str,
        policy: &ContextCheckpointPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_context_checkpoint_policies
             (project_id, enabled, max_checkpoints, max_age_days)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                max_checkpoints = excluded.max_checkpoints,
                max_age_days = excluded.max_age_days",
            params![
                project_id,
                i64::from(policy.enabled),
                i64::from(policy.max_checkpoints),
                i64::from(policy.max_age_days)
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_context_checkpoint_policies(
        &self,
    ) -> Result<Vec<(String, ContextCheckpointPolicy)>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT project_id, enabled, max_checkpoints, max_age_days
             FROM project_context_checkpoint_policies WHERE enabled = 1",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ContextCheckpointPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    max_checkpoints: row.get::<_, i64>(2)?.clamp(1, 100) as u32,
                    max_age_days: row.get::<_, i64>(3)?.clamp(0, 365) as u32,
                },
            ))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            policies.push(row?);
        }
        Ok(policies)
    }

    /// Record a manual checkpoint row and its full-payload event in one
    /// transaction. The event is inserted before the projection row so a
    /// crash between the two statements cannot leave a projection with no
    /// audit event.
    pub fn record_context_checkpoint_with_event(
        &self,
        session_id: &str,
        checkpoint: &ContextCheckpoint,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut checkpoint = checkpoint.clone();
        if checkpoint.session_id != session_id {
            return Err(anyhow!(
                "context checkpoint belongs to a different session: {}",
                checkpoint.session_id
            ));
        }
        checkpoint.message_cursor = context_message_cursor_tx(
            &tx,
            session_id,
            checkpoint.through_message_id.as_deref(),
            checkpoint.message_cursor,
        )?;
        validate_context_checkpoint_row(&checkpoint)?;
        let payload = serde_json::to_value(&checkpoint)?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "context.checkpointed",
            payload_with_checkpoint_id(payload, &checkpoint.id)?,
        )?;
        insert_context_checkpoint_tx(&tx, &checkpoint)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    /// Append a successful compaction and its automatic checkpoint in one
    /// transaction. The compaction event is first (it is the compatibility
    /// boundary used by `load_conversation`), then the full checkpoint event,
    /// then the projection row.
    pub fn append_context_compaction_with_checkpoint(
        &self,
        session_id: &str,
        mut compaction_payload: serde_json::Value,
        checkpoint: &ContextCheckpoint,
    ) -> Result<(Event, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut checkpoint = checkpoint.clone();
        if checkpoint.session_id != session_id {
            return Err(anyhow!(
                "context checkpoint belongs to a different session: {}",
                checkpoint.session_id
            ));
        }
        if let Some(object) = compaction_payload.as_object_mut() {
            object.insert(
                "context_checkpoint_id".into(),
                serde_json::json!(checkpoint.id),
            );
        }
        let compaction = insert_event_tx(&tx, session_id, "context.compacted", compaction_payload)?;
        if checkpoint.source_compaction_seq.is_none() {
            checkpoint.source_compaction_seq = Some(compaction.seq);
        }
        checkpoint.message_cursor = context_message_cursor_tx(
            &tx,
            session_id,
            checkpoint.through_message_id.as_deref(),
            checkpoint.message_cursor,
        )?;
        validate_context_checkpoint_row(&checkpoint)?;
        let payload = serde_json::to_value(&checkpoint)?;
        let checkpoint_event = insert_event_tx(
            &tx,
            session_id,
            "context.checkpointed",
            payload_with_checkpoint_id(payload, &checkpoint.id)?,
        )?;
        insert_context_checkpoint_tx(&tx, &checkpoint)?;
        tx.commit()?;
        for event in [&compaction, &checkpoint_event] {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((compaction, checkpoint_event))
    }

    /// Append a reconstructed compaction boundary and the restore audit event.
    /// No checkpoint row, message row, or worktree state is modified.
    pub fn append_context_restore_events(
        &self,
        session_id: &str,
        compaction_payload: serde_json::Value,
        mut restored_payload: serde_json::Value,
    ) -> Result<(Event, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let compaction = insert_event_tx(&tx, session_id, "context.compacted", compaction_payload)?;
        if let Some(object) = restored_payload.as_object_mut() {
            object.insert("compaction_seq".into(), serde_json::json!(compaction.seq));
        }
        let restored = insert_event_tx(&tx, session_id, "context.restored", restored_payload)?;
        tx.commit()?;
        for event in [&compaction, &restored] {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((compaction, restored))
    }

    pub fn list_context_checkpoints(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<ContextCheckpoint>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, project_id, label, mode, summary,
                    through_message_id, message_cursor, plan_revision, provider,
                    model, prompt_prefix_hash, worktree_head, tokens_before,
                    tokens_after, structured_state, source_compaction_seq,
                    automatic, created_at
             FROM context_checkpoints
             WHERE session_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![session_id, i64::from(limit.clamp(1, 10_000))],
            map_context_checkpoint,
        )?;
        // The limit means "most recent N"; callers receive them oldest-first
        // so prune, replay comparison, and the UI all share one ordering.
        let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    pub fn get_context_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<Option<ContextCheckpoint>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, project_id, label, mode, summary,
                    through_message_id, message_cursor, plan_revision, provider,
                    model, prompt_prefix_hash, worktree_head, tokens_before,
                    tokens_after, structured_state, source_compaction_seq,
                    automatic, created_at
             FROM context_checkpoints
             WHERE session_id = ?1 AND id = ?2",
            params![session_id, checkpoint_id],
            map_context_checkpoint,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn context_checkpoint_count(&self, session_id: &str) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM context_checkpoints WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn latest_context_checkpoint(&self, session_id: &str) -> Result<Option<ContextCheckpoint>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, project_id, label, mode, summary,
                    through_message_id, message_cursor, plan_revision, provider,
                    model, prompt_prefix_hash, worktree_head, tokens_before,
                    tokens_after, structured_state, source_compaction_seq,
                    automatic, created_at
             FROM context_checkpoints
             WHERE session_id = ?1
             ORDER BY created_at DESC, id DESC LIMIT 1",
            params![session_id],
            map_context_checkpoint,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Remove one checkpoint row and append its deletion event atomically.
    /// Messages and worktree state are untouched.
    pub fn delete_context_checkpoint_with_event(
        &self,
        session_id: &str,
        checkpoint_id: &str,
        reason: &str,
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = tx
            .query_row(
                "SELECT id, session_id, project_id, label, mode, summary,
                        through_message_id, message_cursor, plan_revision, provider,
                        model, prompt_prefix_hash, worktree_head, tokens_before,
                        tokens_after, structured_state, source_compaction_seq,
                        automatic, created_at
                 FROM context_checkpoints WHERE session_id = ?1 AND id = ?2",
                params![session_id, checkpoint_id],
                map_context_checkpoint,
            )
            .optional()?;
        let Some(checkpoint) = existing else {
            return Ok(None);
        };
        let event = insert_event_tx(
            &tx,
            session_id,
            "context.checkpoint.deleted",
            serde_json::json!({
                "checkpoint_id": checkpoint_id,
                "label": checkpoint.label,
                "reason": reason,
                "checkpoint": checkpoint,
            }),
        )?;
        tx.execute(
            "DELETE FROM context_checkpoints WHERE session_id = ?1 AND id = ?2",
            params![session_id, checkpoint_id],
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Replace the event-backed checkpoint projection during replay/repair.
    pub fn replace_context_checkpoints(
        &self,
        session_id: &str,
        checkpoints: &[ContextCheckpoint],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_context_checkpoints_tx(&tx, session_id, checkpoints)?;
        tx.commit()?;
        Ok(())
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
                    semantic: None,
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
                    semantic: None,
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
            // Explicit cleanup keeps behavior correct even for databases opened
            // without foreign-key enforcement.
            tx.execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM memory_embedding_jobs WHERE memory_id = ?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM memory_embedding_assignments WHERE memory_id = ?1",
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
        let previous_status = memory.status.clone();
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
        let reactivated = memory.status == "active"
            && matches!(previous_status.as_str(), "archived" | "superseded");
        if tags.is_some() || reactivated {
            enqueue_memory_embedding_job_tx(&tx, &memory)?;
        }
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

    /// Projects with a configured embedding policy. Disabled-but-configured
    /// rows are returned so `memory.embeddings.status` can explain the state;
    /// the daemon reconciler skips them without any network call.
    pub fn list_memory_embedding_policies(&self) -> Result<Vec<(String, MemoryEmbeddingPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, embedding_json FROM project_memory_policies
             WHERE embedding_json IS NOT NULL ORDER BY project_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            let (project_id, raw) = row?;
            if let Some(policy) = decode_memory_embedding_policy(raw)
                && policy.is_configured()
            {
                policies.push((project_id, policy));
            }
        }
        Ok(policies)
    }

    /// Bounded candidate window for the embedding reconciler. Includes rows
    /// whose embedding is missing or belongs to another model/dimension, rows
    /// with a pending content-update job, and due failed retries. The caller
    /// compares content hashes and skips unchanged rows before any network
    /// call.
    pub fn memory_embedding_index_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_attempts: u32,
        limit: u32,
    ) -> Result<Vec<MemoryEmbeddingIndexCandidate>> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    e.model, e.dimensions, e.content_hash,
                    j.status, j.model, j.attempts, j.updated_at
             FROM memories m
             LEFT JOIN memory_embeddings e ON e.memory_id = m.id
             LEFT JOIN memory_embedding_jobs j ON j.memory_id = m.id
             WHERE m.project_id = ?1
               AND m.status IN ('active', 'stale')
               AND (
                   (
                       e.memory_id IS NULL
                       AND (
                           j.memory_id IS NULL
                           OR j.status != 'failed'
                           OR j.model != ?2
                           OR (
                               j.attempts < ?4
                               AND j.next_attempt_at IS NOT NULL
                               AND j.next_attempt_at <= ?5
                           )
                       )
                   )
                   OR e.model != ?2
                   OR e.dimensions != ?3
                   OR substr(e.vector, 1, 4) != x'5650454d'
                   OR substr(e.vector, 5, 1) != x'01'
                   OR length(e.vector) != 9 + 4 * ?3
                   OR j.status = 'pending'
                   OR (
                       j.status = 'failed'
                       AND j.attempts < ?4
                       AND j.next_attempt_at IS NOT NULL
                       AND j.next_attempt_at <= ?5
                   )
               )
             ORDER BY COALESCE(j.updated_at, m.created_at) ASC, m.id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_attempts),
                now,
                i64::from(limit.clamp(1, 512))
            ],
            |row| {
                Ok(MemoryEmbeddingIndexCandidate {
                    memory: map_memory_row(row)?,
                    embedded_model: row.get(21)?,
                    embedded_dimensions: row
                        .get::<_, Option<i64>>(22)?
                        .and_then(|value| u32::try_from(value).ok()),
                    embedded_content_hash: row.get(23)?,
                    job_status: row.get(24)?,
                    job_model: row.get(25)?,
                    job_attempts: row
                        .get::<_, Option<i64>>(26)?
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(0),
                    job_updated_at: row.get(27)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Active/stale memories with a current-model embedding row. `limit` is
    /// clamped to 256; vector bytes are returned as-is and decoded by the
    /// caller under the documented versioned encoding.
    #[allow(clippy::too_many_arguments)]
    pub fn memory_semantic_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemorySemanticCandidate>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    e.model, e.dimensions, e.content_hash, e.vector
             FROM memories m
             JOIN memory_embeddings e ON e.memory_id = m.id
             WHERE m.project_id = ?1
               AND e.model = ?2
               AND e.dimensions = ?3
               AND (
                   m.status = 'active'
                   OR (?4 = 1 AND m.status = 'stale')
               )
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                if include_stale { 1 } else { 0 },
                i64::from(limit.clamp(1, 256))
            ],
            |row| {
                Ok(MemorySemanticCandidate {
                    memory: map_memory_row(row)?,
                    model: row.get(21)?,
                    dimensions: u32::try_from(row.get::<_, i64>(22)?).unwrap_or(u32::MAX),
                    content_hash: row.get(23)?,
                    vector: row.get(24)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Increment the attempt counter before a network call. If the row was
    /// selected because its embedding was missing, this creates the job row.
    pub fn mark_memory_embedding_attempted(
        &self,
        memory_id: &str,
        model: &str,
        reset_attempts: bool,
    ) -> Result<u32> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             VALUES (?1, ?2, 'pending', 1, NULL, NULL, ?4)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'pending',
                attempts = CASE WHEN ?3 = 1
                    THEN 1
                    ELSE memory_embedding_jobs.attempts + 1
                END,
                last_error = NULL,
                next_attempt_at = NULL,
                updated_at = excluded.updated_at",
            params![memory_id, model, if reset_attempts { 1 } else { 0 }, now],
        )?;
        let attempts: i64 = tx.query_row(
            "SELECT attempts FROM memory_embedding_jobs WHERE memory_id = ?1",
            params![memory_id],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(u32::try_from(attempts).unwrap_or(u32::MAX))
    }

    /// Persist one successful vector plus its job transition and audit event.
    /// `vector` must already be validated and encoded by `vp-core`.
    #[allow(clippy::too_many_arguments)]
    pub fn record_memory_embedding_success(
        &self,
        memory_id: &str,
        model: &str,
        dimensions: u32,
        content_hash: &str,
        vector: &[u8],
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(memory) = get_memory_tx(&tx, memory_id)? else {
            tx.commit()?;
            return Ok(None);
        };
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embeddings
             (memory_id, model, dimensions, content_hash, vector, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                dimensions = excluded.dimensions,
                content_hash = excluded.content_hash,
                vector = excluded.vector,
                updated_at = excluded.updated_at",
            params![
                memory_id,
                model,
                i64::from(dimensions),
                content_hash,
                vector,
                now
            ],
        )?;
        // The vector may have changed cluster membership. Removing the old
        // assignment keeps recall from scoring a stale cluster until the
        // bounded maintenance pass assigns the new vector.
        tx.execute(
            "DELETE FROM memory_embedding_assignments WHERE memory_id = ?1",
            params![memory_id],
        )?;
        tx.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             VALUES (?1, ?2, 'indexed', 0, NULL, NULL, ?3)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'indexed',
                attempts = 0,
                last_error = NULL,
                next_attempt_at = NULL,
                updated_at = excluded.updated_at",
            params![memory_id, model, now],
        )?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.embedding.indexed",
            serde_json::json!({
                "memory_id": memory.id,
                "project_id": memory.project_id,
                "model": model,
                "dimensions": dimensions,
                "content_hash": content_hash,
            }),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Record a failed attempt. `next_attempt_at = None` marks the retry budget
    /// exhausted; the row stays visible as `failed` until an explicit rebuild.
    pub fn record_memory_embedding_failure(
        &self,
        memory_id: &str,
        model: &str,
        attempts: u32,
        error: &str,
        next_attempt_at: Option<&str>,
    ) -> Result<Option<Event>> {
        let error = truncate_chars(error.trim(), 2_000);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(memory) = get_memory_tx(&tx, memory_id)? else {
            tx.commit()?;
            return Ok(None);
        };
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             VALUES (?1, ?2, 'failed', ?3, ?4, ?5, ?6)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'failed',
                attempts = excluded.attempts,
                last_error = excluded.last_error,
                next_attempt_at = excluded.next_attempt_at,
                updated_at = excluded.updated_at",
            params![
                memory_id,
                model,
                i64::from(attempts),
                error,
                next_attempt_at,
                now
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.embedding.failed",
            serde_json::json!({
                "memory_id": memory.id,
                "project_id": memory.project_id,
                "model": model,
                "attempts": attempts,
                "error": error,
                "next_attempt_at": next_attempt_at,
            }),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Mark a job skipped without a network call: unchanged content,
    /// archived/superseded memory, or an explicitly disabled lane.
    pub fn record_memory_embedding_skip(
        &self,
        memory_id: &str,
        model: &str,
        reason: &str,
    ) -> Result<Option<Event>> {
        let reason = truncate_chars(reason.trim(), 500);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(memory) = get_memory_tx(&tx, memory_id)? else {
            tx.commit()?;
            return Ok(None);
        };
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             VALUES (?1, ?2, 'skipped', 0, NULL, NULL, ?3)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'skipped',
                attempts = 0,
                last_error = NULL,
                next_attempt_at = NULL,
                updated_at = excluded.updated_at",
            params![memory_id, model, now],
        )?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.embedding.skipped",
            serde_json::json!({
                "memory_id": memory.id,
                "project_id": memory.project_id,
                "model": model,
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Aggregate call/token accounting. This table is intentionally separate
    /// from `usage_ledger`: embedding calls are project infrastructure, not a
    /// session's task spend, and are never charged to a session budget.
    #[allow(clippy::too_many_arguments)]
    pub fn record_memory_embedding_usage(
        &self,
        project_id: &str,
        model: &str,
        calls: u64,
        input_tokens: u64,
        estimated_tokens: u64,
        bytes: u64,
        provider_reported: bool,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO memory_embedding_usage
             (id, project_id, model, calls, input_tokens, estimated_tokens, bytes,
              provider_reported, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                Uuid::now_v7().to_string(),
                project_id,
                model,
                i64::try_from(calls).unwrap_or(i64::MAX),
                i64::try_from(input_tokens).unwrap_or(i64::MAX),
                i64::try_from(estimated_tokens).unwrap_or(i64::MAX),
                i64::try_from(bytes).unwrap_or(i64::MAX),
                if provider_reported { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Health snapshot for the semantic lane. Counts are bounded SQL
    /// aggregates; recent jobs are capped at 20, and no vector or credential
    /// is included.
    pub fn memory_embedding_status(
        &self,
        project_id: &str,
        policy: Option<&MemoryEmbeddingPolicy>,
    ) -> Result<MemoryEmbeddingsStatusResult> {
        let enabled = policy.is_some_and(|policy| policy.enabled);
        let configured = policy.is_some_and(MemoryEmbeddingPolicy::is_configured);
        let model = policy.and_then(|policy| policy.model.clone());
        let dimensions = policy.map_or(1536, |policy| policy.dimensions);
        let conn = self.lock()?;
        let indexable: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memories
             WHERE project_id = ?1 AND status IN ('active', 'stale')",
            params![project_id],
            |row| row.get(0),
        )?;
        let (indexed, invalid): (i64, i64) = match model.as_deref() {
            Some(model) => conn.query_row(
                "SELECT
                     COALESCE(SUM(CASE WHEN
                         substr(e.vector, 1, 4) = x'5650454d'
                         AND substr(e.vector, 5, 1) = x'01'
                         AND length(e.vector) = 9 + 4 * ?3
                     THEN 1 ELSE 0 END), 0),
                     COALESCE(SUM(CASE WHEN
                         substr(e.vector, 1, 4) != x'5650454d'
                         OR substr(e.vector, 5, 1) != x'01'
                         OR length(e.vector) != 9 + 4 * ?3
                     THEN 1 ELSE 0 END), 0)
                 FROM memories m
                 JOIN memory_embeddings e ON e.memory_id = m.id
                 LEFT JOIN memory_embedding_jobs j ON j.memory_id = m.id
                 WHERE m.project_id = ?1
                   AND m.status IN ('active', 'stale')
                   AND e.model = ?2 AND e.dimensions = ?3
                   AND (j.status IS NULL OR j.status NOT IN ('pending', 'failed'))",
                params![project_id, model, i64::from(dimensions)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?,
            None => (0, 0),
        };
        let count_jobs = |status: &str| -> Result<u32> {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_embedding_jobs j
                 JOIN memories m ON m.id = j.memory_id
                 WHERE m.project_id = ?1 AND j.status = ?2
                   AND (?3 IS NULL OR j.model = ?3)",
                params![project_id, status, model.as_deref()],
                |row| row.get(0),
            )?;
            Ok(u32::try_from(count).unwrap_or(u32::MAX))
        };
        let pending = count_jobs("pending")?;
        let failed = count_jobs("failed")?;
        let skipped = count_jobs("skipped")?;
        let last_error: Option<String> = conn
            .query_row(
                "SELECT j.last_error FROM memory_embedding_jobs j
                 JOIN memories m ON m.id = j.memory_id
                 WHERE m.project_id = ?1 AND j.status = 'failed' AND j.last_error IS NOT NULL
                   AND (?2 IS NULL OR j.model = ?2)
                 ORDER BY j.updated_at DESC, j.memory_id ASC LIMIT 1",
                params![project_id, model.as_deref()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let usage = conn.query_row(
            "SELECT COALESCE(SUM(calls), 0), COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(estimated_tokens), 0), COALESCE(SUM(bytes), 0),
                    COALESCE(SUM(provider_reported), 0)
             FROM memory_embedding_usage WHERE project_id = ?1",
            params![project_id],
            |row| {
                Ok(MemoryEmbeddingUsage {
                    calls: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(u64::MAX),
                    input_tokens: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(u64::MAX),
                    estimated_tokens: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(u64::MAX),
                    bytes: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(u64::MAX),
                    provider_reported_calls: u64::try_from(row.get::<_, i64>(4)?)
                        .unwrap_or(u64::MAX),
                })
            },
        )?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.title, j.status, j.attempts, j.last_error, j.next_attempt_at,
                    j.updated_at
             FROM memory_embedding_jobs j
             JOIN memories m ON m.id = j.memory_id
             WHERE m.project_id = ?1
               AND (?2 IS NULL OR j.model = ?2)
             ORDER BY j.updated_at DESC, m.id ASC
             LIMIT 20",
        )?;
        let recent = stmt
            .query_map(params![project_id, model.as_deref()], |row| {
                Ok(MemoryEmbeddingJobStatus {
                    memory_id: row.get(0)?,
                    title: row.get(1)?,
                    status: row.get(2)?,
                    attempts: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                    last_error: row.get(4)?,
                    next_attempt_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let indexable = u32::try_from(indexable).unwrap_or(u32::MAX);
        let indexed = u32::try_from(indexed).unwrap_or(u32::MAX);
        let invalid = u32::try_from(invalid).unwrap_or(u32::MAX);
        Ok(MemoryEmbeddingsStatusResult {
            project_id: project_id.to_string(),
            enabled,
            configured,
            model,
            dimensions,
            indexable,
            indexed,
            invalid,
            pending,
            failed,
            skipped,
            coverage: if indexable == 0 {
                0.0
            } else {
                f64::from(indexed) / f64::from(indexable)
            },
            last_error: last_error.map(|error| truncate_chars(&error, 2_000)),
            usage,
            recent_jobs: recent,
            index: None,
        })
    }

    /// Remove every derived embedding and job row for a project. Usage history
    /// is accounting, not derived content, and is intentionally retained.
    pub fn clear_memory_embeddings(&self, project_id: &str) -> Result<u32> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let embeddings = tx.execute(
            "DELETE FROM memory_embeddings
             WHERE memory_id IN (SELECT id FROM memories WHERE project_id = ?1)",
            params![project_id],
        )?;
        let jobs = tx.execute(
            "DELETE FROM memory_embedding_jobs
             WHERE memory_id IN (SELECT id FROM memories WHERE project_id = ?1)",
            params![project_id],
        )?;
        // The ANN index is derived state too. The explicit rebuild/clear path
        // is also the privacy cleanup path, so it must remove centroids and
        // assignments along with vectors and jobs.
        let assignments = tx.execute(
            "DELETE FROM memory_embedding_assignments
             WHERE memory_id IN (SELECT id FROM memories WHERE project_id = ?1)",
            params![project_id],
        )?;
        let index = tx.execute(
            "DELETE FROM memory_embedding_index WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.commit()?;
        Ok(u32::try_from(
            embeddings
                .saturating_add(jobs)
                .saturating_add(assignments)
                .saturating_add(index),
        )
        .unwrap_or(u32::MAX))
    }

    /// Queue every active/stale memory for indexing. Existing jobs are reset
    /// so an explicit rebuild clears exhausted retry budgets.
    pub fn enqueue_memory_embedding_backlog(&self, project_id: &str, model: &str) -> Result<u32> {
        let conn = self.lock()?;
        let now = now_rfc3339();
        let changed = conn.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             SELECT id, ?2, 'pending', 0, NULL, NULL, ?3
             FROM memories
             WHERE project_id = ?1 AND status IN ('active', 'stale') AND true
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'pending',
                attempts = 0,
                last_error = NULL,
                next_attempt_at = NULL,
                updated_at = excluded.updated_at",
            params![project_id, model, now],
        )?;
        Ok(u32::try_from(changed).unwrap_or(u32::MAX))
    }

    /// Current derived IVF index row for one project. `None` means no index
    /// has ever been built (or it was explicitly removed). The centroid blob
    /// is returned only to `vp-core`, which owns validation and decoding; it
    /// must never be copied into an RPC, event, log, or UI payload.
    pub fn memory_embedding_index_row(
        &self,
        project_id: &str,
    ) -> Result<Option<MemoryEmbeddingIndexRow>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT project_id, model, dimensions, generation, nlist, cluster_count,
                        vector_count, centroids, updated_at, last_error, fallback_count,
                        last_fallback_at, last_fallback_reason
                 FROM memory_embedding_index WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(MemoryEmbeddingIndexRow {
                        project_id: row.get(0)?,
                        model: row.get(1)?,
                        dimensions: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                        generation: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                        nlist: u32::try_from(row.get::<_, i64>(4)?).unwrap_or(u32::MAX),
                        cluster_count: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(u32::MAX),
                        vector_count: u32::try_from(row.get::<_, i64>(6)?).unwrap_or(u32::MAX),
                        centroids: row.get(7)?,
                        updated_at: row.get(8)?,
                        last_error: row.get(9)?,
                        fallback_count: u64::try_from(row.get::<_, i64>(10)?).unwrap_or(u64::MAX),
                        last_fallback_at: row.get(11)?,
                        last_fallback_reason: row.get(12)?,
                    })
                },
            )
            .optional()?;
        if let Some(row) = &row
            && row.centroids.len() > MAX_MEMORY_INDEX_BLOB_BYTES
        {
            anyhow::bail!(
                "stored memory embedding index exceeds the {MAX_MEMORY_INDEX_BLOB_BYTES}-byte ceiling"
            );
        }
        Ok(row)
    }

    /// Live vector and assignment counts. A vector counts only when the
    /// memory is active/stale, the embedding belongs to the requested
    /// model/dimensions, and its header passes the same structural checks as
    /// status. Assignments are counted only for the requested generation.
    pub fn memory_embedding_index_counts(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        cluster_count: u32,
    ) -> Result<MemoryEmbeddingIndexCounts> {
        let conn = self.lock()?;
        let (vectors, assigned): (i64, i64) = conn.query_row(
            "SELECT
                 (SELECT COUNT(*)
                    FROM memories m
                    JOIN memory_embeddings e ON e.memory_id = m.id
                   WHERE m.project_id = ?1
                     AND m.status IN ('active', 'stale')
                     AND e.model = ?2
                     AND e.dimensions = ?3
                     AND substr(e.vector, 1, 4) = x'5650454d'
                     AND substr(e.vector, 5, 1) = x'01'
                     AND length(e.vector) = 9 + 4 * ?3),
                 (SELECT COUNT(*)
                    FROM memory_embedding_assignments a
                    JOIN memories m ON m.id = a.memory_id
                    JOIN memory_embeddings e ON e.memory_id = m.id
                   WHERE m.project_id = ?1
                     AND a.generation = ?4
                     AND a.cluster >= 0
                     AND a.cluster < ?5
                     AND m.status IN ('active', 'stale')
                     AND e.model = ?2
                     AND e.dimensions = ?3
                     AND substr(e.vector, 1, 4) = x'5650454d'
                     AND substr(e.vector, 5, 1) = x'01'
                     AND length(e.vector) = 9 + 4 * ?3)",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(cluster_count)
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(MemoryEmbeddingIndexCounts {
            vector_count: u32::try_from(vectors).unwrap_or(u32::MAX),
            assigned_vectors: u32::try_from(assigned).unwrap_or(u32::MAX),
        })
    }

    /// Audit assignments against one generation, the current model/dimensions,
    /// and the centroid count. Counts are bounded SQL aggregates; no vector is
    /// read.
    pub fn memory_embedding_assignment_audit(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        cluster_count: u32,
    ) -> Result<MemoryEmbeddingAssignmentAudit> {
        let conn = self.lock()?;
        let (total, stale, out_of_range, missing): (i64, i64, i64, i64) = conn.query_row(
            "SELECT
                 COUNT(*),
                 COALESCE(SUM(CASE WHEN a.generation != ?4 THEN 1 ELSE 0 END), 0),
                 COALESCE(SUM(CASE WHEN a.cluster < 0 OR a.cluster >= ?5 THEN 1 ELSE 0 END), 0),
                 COALESCE(SUM(CASE WHEN a.generation = ?4
                                       AND a.cluster >= 0 AND a.cluster < ?5
                                       AND (e.memory_id IS NULL
                                            OR e.model != ?2
                                            OR e.dimensions != ?3
                                            OR substr(e.vector, 1, 4) != x'5650454d'
                                            OR substr(e.vector, 5, 1) != x'01'
                                            OR length(e.vector) != 9 + 4 * ?3
                                            OR m.status NOT IN ('active', 'stale'))
                                  THEN 1 ELSE 0 END), 0)
             FROM memory_embedding_assignments a
             JOIN memories m ON m.id = a.memory_id
             LEFT JOIN memory_embeddings e ON e.memory_id = a.memory_id
             WHERE m.project_id = ?1",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(cluster_count)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        Ok(MemoryEmbeddingAssignmentAudit {
            total: u32::try_from(total).unwrap_or(u32::MAX),
            stale_generation: u32::try_from(stale).unwrap_or(u32::MAX),
            out_of_range: u32::try_from(out_of_range).unwrap_or(u32::MAX),
            missing_vector: u32::try_from(missing).unwrap_or(u32::MAX),
        })
    }

    /// Delete assignments that are not usable by the current generation. This
    /// is bounded by the number of assignment rows for one project and is only
    /// called from maintenance/rebuild, never from recall.
    pub fn delete_invalid_memory_embedding_assignments(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        cluster_count: u32,
    ) -> Result<u32> {
        let conn = self.lock()?;
        let deleted = conn.execute(
            "DELETE FROM memory_embedding_assignments
             WHERE memory_id IN (
                 SELECT a.memory_id
                 FROM memory_embedding_assignments a
                 JOIN memories m ON m.id = a.memory_id
                 LEFT JOIN memory_embeddings e ON e.memory_id = a.memory_id
                 WHERE m.project_id = ?1
                   AND (a.generation != ?4
                        OR a.cluster < 0
                        OR a.cluster >= ?5
                        OR e.memory_id IS NULL
                        OR e.model != ?2
                        OR e.dimensions != ?3
                        OR substr(e.vector, 1, 4) != x'5650454d'
                        OR substr(e.vector, 5, 1) != x'01'
                        OR length(e.vector) != 9 + 4 * ?3
                        OR m.status NOT IN ('active', 'stale'))
             )",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(cluster_count)
            ],
        )?;
        Ok(u32::try_from(deleted).unwrap_or(u32::MAX))
    }

    /// Deterministic bounded sample of current vectors for a k-means rebuild.
    ///
    /// Rows are ordered by a prefix of their SHA-256 content hash, so the
    /// sample is stable for an unchanged database and does not simply take the
    /// first memory ids. `limit` is additionally capped by `max_bytes`, which
    /// bounds the transient allocation regardless of embedding dimension.
    pub fn memory_embedding_vectors_for_index(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        limit: u32,
        max_bytes: usize,
    ) -> Result<Vec<MemorySemanticCandidate>> {
        let row_bytes = 9usize.saturating_add(dimensions as usize * 4).max(1);
        let rows_by_bytes = max_bytes / row_bytes;
        let effective_limit = (limit as usize).min(rows_by_bytes.max(1)).clamp(1, 16_384);
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    e.model, e.dimensions, e.content_hash, e.vector
             FROM memories m
             JOIN memory_embeddings e ON e.memory_id = m.id
             WHERE m.project_id = ?1
               AND m.status IN ('active', 'stale')
               AND e.model = ?2
               AND e.dimensions = ?3
               AND substr(e.vector, 1, 4) = x'5650454d'
               AND substr(e.vector, 5, 1) = x'01'
               AND length(e.vector) = 9 + 4 * ?3
             ORDER BY substr(e.content_hash, 1, 16) ASC, m.id ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(effective_limit).unwrap_or(i64::MAX)
            ],
            map_memory_semantic_candidate,
        )?;
        let mut candidates = Vec::new();
        let mut bytes = 0usize;
        for row in rows {
            let candidate = row?;
            bytes = bytes.saturating_add(candidate.vector.len());
            if bytes > max_bytes && !candidates.is_empty() {
                break;
            }
            candidates.push(candidate);
        }
        Ok(candidates)
    }

    /// Current vectors that have no assignment, or whose assignment belongs
    /// to an older generation. Used by bounded incremental maintenance and by
    /// explicit rebuilds. Ordered by memory id for deterministic batches.
    pub fn memory_embedding_unassigned_vectors(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        limit: u32,
    ) -> Result<Vec<MemorySemanticCandidate>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    e.model, e.dimensions, e.content_hash, e.vector
             FROM memories m
             JOIN memory_embeddings e ON e.memory_id = m.id
             LEFT JOIN memory_embedding_assignments a ON a.memory_id = m.id
             WHERE m.project_id = ?1
               AND m.status IN ('active', 'stale')
               AND e.model = ?2
               AND e.dimensions = ?3
               AND substr(e.vector, 1, 4) = x'5650454d'
               AND substr(e.vector, 5, 1) = x'01'
               AND length(e.vector) = 9 + 4 * ?3
               AND (a.memory_id IS NULL OR a.generation != ?4)
             ORDER BY m.id ASC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(limit.clamp(1, 1_024))
            ],
            map_memory_semantic_candidate,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Vectors assigned to the probed clusters in one index generation.
    /// `limit` is a hard candidate window; `vp-core` scores at most that many
    /// vectors exactly and then keeps the policy candidate budget.
    #[allow(clippy::too_many_arguments)]
    pub fn memory_ivf_semantic_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        clusters: &[u32],
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemorySemanticCandidate>> {
        if clusters.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    e.model, e.dimensions, e.content_hash, e.vector
             FROM memory_embedding_assignments a
             JOIN memories m ON m.id = a.memory_id
             JOIN memory_embeddings e ON e.memory_id = m.id
             WHERE m.project_id = ?
               AND a.generation = ?
               AND e.model = ?
               AND e.dimensions = ?
               AND (? = 1 OR m.status = 'active')
               AND m.status IN ('active', 'stale')
               AND substr(e.vector, 1, 4) = x'5650454d'
               AND substr(e.vector, 5, 1) = x'01'
               AND length(e.vector) = 9 + 4 * ?
               AND a.cluster IN (",
        );
        let placeholders = std::iter::repeat_n("?", clusters.len())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&placeholders);
        sql.push_str(
            ")
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?",
        );
        let mut values = vec![
            rusqlite::types::Value::Text(project_id.to_string()),
            rusqlite::types::Value::Integer(i64::try_from(generation).unwrap_or(i64::MAX)),
            rusqlite::types::Value::Text(model.to_string()),
            rusqlite::types::Value::Integer(i64::from(dimensions)),
            rusqlite::types::Value::Integer(if include_stale { 1 } else { 0 }),
            rusqlite::types::Value::Integer(i64::from(dimensions)),
        ];
        values.extend(
            clusters
                .iter()
                .map(|cluster| rusqlite::types::Value::Integer(i64::from(*cluster))),
        );
        values.push(rusqlite::types::Value::Integer(i64::from(
            limit.clamp(1, 4_096),
        )));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(values),
            map_memory_semantic_candidate,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Persist a complete centroid generation transactionally. The returned
    /// generation is strictly monotonic even if two builds race: a request
    /// below the stored generation is bumped to `stored + 1`. Assignments from
    /// older generations are removed in the same transaction, so a recall
    /// either sees the new generation or falls back; it cannot mix them.
    #[allow(clippy::too_many_arguments)]
    pub fn save_memory_embedding_index(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        requested_generation: u64,
        nlist: u32,
        cluster_count: u32,
        vector_count: u32,
        centroids: &[u8],
    ) -> Result<u64> {
        if centroids.is_empty() {
            anyhow::bail!("refusing to persist an empty centroid blob");
        }
        if centroids.len() > MAX_MEMORY_INDEX_BLOB_BYTES {
            anyhow::bail!("centroid blob exceeds the {MAX_MEMORY_INDEX_BLOB_BYTES}-byte ceiling");
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT generation FROM memory_embedding_index WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        let floor = current
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0)
            .saturating_add(1);
        let generation = requested_generation.max(floor).max(1);
        let generation = i64::try_from(generation).unwrap_or(i64::MAX);
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embedding_index
             (project_id, model, dimensions, generation, nlist, cluster_count, vector_count,
              centroids, updated_at, last_error, fallback_count, last_fallback_at,
              last_fallback_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL,
                     COALESCE((SELECT fallback_count FROM memory_embedding_index WHERE project_id = ?1), 0),
                     (SELECT last_fallback_at FROM memory_embedding_index WHERE project_id = ?1),
                     (SELECT last_fallback_reason FROM memory_embedding_index WHERE project_id = ?1))
             ON CONFLICT(project_id) DO UPDATE SET
                model = excluded.model,
                dimensions = excluded.dimensions,
                generation = excluded.generation,
                nlist = excluded.nlist,
                cluster_count = excluded.cluster_count,
                vector_count = excluded.vector_count,
                centroids = excluded.centroids,
                updated_at = excluded.updated_at,
                last_error = NULL",
            params![
                project_id,
                model,
                i64::from(dimensions),
                generation,
                i64::from(nlist),
                i64::from(cluster_count),
                i64::from(vector_count),
                centroids,
                now
            ],
        )?;
        tx.execute(
            "DELETE FROM memory_embedding_assignments
             WHERE generation != ?1
               AND memory_id IN (SELECT id FROM memories WHERE project_id = ?2)",
            params![generation, project_id],
        )?;
        tx.commit()?;
        Ok(u64::try_from(generation).unwrap_or(u64::MAX))
    }

    /// Persist cluster assignments for the current generation. If another
    /// build committed a newer generation first, this batch is discarded and
    /// returns 0 instead of writing assignments that recall would ignore.
    pub fn save_memory_embedding_assignments(
        &self,
        project_id: &str,
        generation: u64,
        assignments: &[(String, u32)],
    ) -> Result<u32> {
        if assignments.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT generation FROM memory_embedding_index WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        if current.and_then(|value| u64::try_from(value).ok()) != Some(generation) {
            tx.commit()?;
            return Ok(0);
        }
        let now = now_rfc3339();
        let generation = i64::try_from(generation).unwrap_or(i64::MAX);
        let mut written = 0u32;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO memory_embedding_assignments
                 (memory_id, generation, cluster, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(memory_id) DO UPDATE SET
                    generation = excluded.generation,
                    cluster = excluded.cluster,
                    updated_at = excluded.updated_at",
            )?;
            for (memory_id, cluster) in assignments {
                stmt.execute(params![memory_id, generation, i64::from(*cluster), now])?;
                written = written.saturating_add(1);
            }
        }
        tx.commit()?;
        Ok(written)
    }

    /// Remove the derived index and all of its assignments for one project.
    /// Memories and stored embeddings are untouched.
    pub fn delete_memory_embedding_index(&self, project_id: &str) -> Result<u32> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let assignments = tx.execute(
            "DELETE FROM memory_embedding_assignments
             WHERE memory_id IN (SELECT id FROM memories WHERE project_id = ?1)",
            params![project_id],
        )?;
        let index = tx.execute(
            "DELETE FROM memory_embedding_index WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.commit()?;
        Ok(u32::try_from(index.saturating_add(assignments)).unwrap_or(u32::MAX))
    }

    /// Record a sanitized build/maintenance error. A valid previous generation
    /// is preserved: recall keeps using it only if model/dimensions/nlist still
    /// match, otherwise it fails closed to the exact scan.
    pub fn record_memory_embedding_index_error(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        nlist: u32,
        error: &str,
    ) -> Result<()> {
        let error = truncate_chars(error.trim(), 2_000);
        let conn = self.lock()?;
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO memory_embedding_index
             (project_id, model, dimensions, generation, nlist, cluster_count, vector_count,
              centroids, updated_at, last_error)
             VALUES (?1, ?2, ?3, 0, ?4, 0, 0, x'', ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                last_error = excluded.last_error,
                updated_at = excluded.updated_at",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(nlist),
                now,
                error
            ],
        )?;
        Ok(())
    }

    /// Increment the diagnostic fallback counter. This is the only write the
    /// recall read path performs, and only when an IVF probe actually falls
    /// back; a scan-mode project never touches these rows.
    ///
    /// A generation-0 diagnostic row is created when no index exists so
    /// "below minimum" and "not built" fallbacks are observable in status.
    /// It has an empty centroid blob and can never be used by recall.
    #[allow(clippy::too_many_arguments)]
    pub fn note_memory_embedding_index_fallback(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        nlist: u32,
        reason: &str,
    ) -> Result<()> {
        let reason = truncate_chars(reason.trim(), 500);
        let conn = self.lock()?;
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO memory_embedding_index
             (project_id, model, dimensions, generation, nlist, cluster_count, vector_count,
              centroids, updated_at, last_error, fallback_count, last_fallback_at,
              last_fallback_reason)
             VALUES (?1, ?2, ?3, 0, ?4, 0, 0, x'', ?5, NULL, 1, ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                fallback_count = memory_embedding_index.fallback_count + 1,
                last_fallback_at = excluded.last_fallback_at,
                last_fallback_reason = excluded.last_fallback_reason",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(nlist),
                now,
                reason
            ],
        )?;
        Ok(())
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
                        default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                        embedding_json
                 FROM project_memory_policies WHERE project_id = ?1",
                params![project_id],
                map_memory_policy_row,
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_memory_policy(&self, project_id: &str, policy: &MemoryPolicy) -> Result<()> {
        let conn = self.lock()?;
        let embedding_json = match &policy.embedding {
            Some(embedding) => Some(serde_json::to_string(embedding)?),
            None => None,
        };
        conn.execute(
            "INSERT INTO project_memory_policies
             (project_id, enabled, max_items, max_age_days, task_result_verify_days,
              default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
              embedding_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                max_items = excluded.max_items,
                max_age_days = excluded.max_age_days,
                task_result_verify_days = excluded.task_result_verify_days,
                default_verify_days = excluded.default_verify_days,
                auto_supersede = excluded.auto_supersede,
                brief_max_chars = excluded.brief_max_chars,
                sweep_stale = excluded.sweep_stale,
                embedding_json = excluded.embedding_json,
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
                embedding_json,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_memory_policies(&self) -> Result<Vec<(String, MemoryPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, max_items, max_age_days, task_result_verify_days,
                    default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                    embedding_json
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
                    embedding: decode_memory_embedding_policy(row.get::<_, Option<String>>(9)?),
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
                    default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                    embedding_json
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
                    embedding: decode_memory_embedding_policy(row.get::<_, Option<String>>(9)?),
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

    const ACP_AGENT_COLUMNS: &'static str = "id, name, command, args, env, enabled, auto_approve, \
         timeout_secs, created_at, transport, endpoint, auth_header_env, headers, \
         connect_timeout_secs, allow_remote";

    pub fn list_acp_agents(&self) -> Result<Vec<AcpAgent>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM acp_agents ORDER BY name ASC",
            Self::ACP_AGENT_COLUMNS
        ))?;
        let rows = stmt.query_map([], map_acp_agent)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_acp_agent(&self, agent_id: &str) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!(
                "SELECT {} FROM acp_agents WHERE id = ?1",
                Self::ACP_AGENT_COLUMNS
            ),
            params![agent_id],
            map_acp_agent,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_acp_agent(&self, identifier: &str) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!(
                "SELECT {} FROM acp_agents WHERE id = ?1 OR name = ?1",
                Self::ACP_AGENT_COLUMNS
            ),
            params![identifier],
            map_acp_agent,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Insert an ACP agent with the full transport-aware configuration.
    pub fn add_acp_agent_config(&self, config: &AcpAgentConfig) -> Result<AcpAgent> {
        let agent = AcpAgent {
            id: Uuid::now_v7().to_string(),
            name: config.name.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
            enabled: config.enabled,
            auto_approve: config.auto_approve,
            timeout_secs: config.timeout_secs,
            transport: config.transport,
            endpoint: config.endpoint.clone(),
            auth_header_env: config.auth_header_env.clone(),
            headers: config.headers.clone(),
            connect_timeout_secs: config.connect_timeout_secs,
            allow_remote: config.allow_remote,
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO acp_agents
             (id, name, command, args, env, enabled, auto_approve, timeout_secs, created_at,
              transport, endpoint, auth_header_env, headers, connect_timeout_secs, allow_remote)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                agent.id,
                agent.name,
                agent.command,
                serde_json::to_string(&agent.args)?,
                serde_json::to_string(&agent.env)?,
                if agent.enabled { 1 } else { 0 },
                if agent.auto_approve { 1 } else { 0 },
                agent.timeout_secs as i64,
                agent.created_at,
                agent.transport.as_str(),
                agent.endpoint,
                agent.auth_header_env,
                serde_json::to_string(&agent.headers)?,
                agent.connect_timeout_secs as i64,
                if agent.allow_remote { 1 } else { 0 }
            ],
        )?;
        Ok(agent)
    }

    /// Backward-compatible stdio-only insert used by older internal callers.
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
        self.add_acp_agent_config(&AcpAgentConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            env: env.clone(),
            enabled,
            auto_approve,
            timeout_secs,
            transport: AcpTransport::Stdio,
            endpoint: None,
            auth_header_env: None,
            headers: Default::default(),
            connect_timeout_secs: 30,
            allow_remote: false,
        })
    }

    /// Update every transport-aware field of an ACP agent.
    pub fn update_acp_agent_config(
        &self,
        agent_id: &str,
        config: &AcpAgentConfig,
    ) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE acp_agents SET name = ?1, command = ?2, args = ?3, env = ?4,
                enabled = ?5, auto_approve = ?6, timeout_secs = ?7, transport = ?8,
                endpoint = ?9, auth_header_env = ?10, headers = ?11,
                connect_timeout_secs = ?12, allow_remote = ?13
             WHERE id = ?14",
            params![
                config.name,
                config.command,
                serde_json::to_string(&config.args)?,
                serde_json::to_string(&config.env)?,
                if config.enabled { 1 } else { 0 },
                if config.auto_approve { 1 } else { 0 },
                config.timeout_secs as i64,
                config.transport.as_str(),
                config.endpoint,
                config.auth_header_env,
                serde_json::to_string(&config.headers)?,
                config.connect_timeout_secs as i64,
                if config.allow_remote { 1 } else { 0 },
                agent_id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_acp_agent(agent_id)
    }

    /// Backward-compatible stdio-only update used by older internal callers.
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
        self.update_acp_agent_config(
            agent_id,
            &AcpAgentConfig {
                name: name.to_string(),
                command: command.to_string(),
                args: args.to_vec(),
                env: env.clone(),
                enabled,
                auto_approve,
                timeout_secs,
                transport: AcpTransport::Stdio,
                endpoint: None,
                auth_header_env: None,
                headers: Default::default(),
                connect_timeout_secs: 30,
                allow_remote: false,
            },
        )
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

    /// Bounded GUI action timeline. Entries are derived from `gui.action`
    /// events; only action/target/generation/latency/result metadata is
    /// stored. Screen content and typed text are never included.
    pub fn recent_gui_actions(
        &self,
        session_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<GuiActionInfo>> {
        let limit = limit.clamp(1, 500);
        let conn = self.lock()?;
        let mut actions = Vec::new();
        match session_id {
            Some(session_id) => {
                let mut statement = conn.prepare(
                    "SELECT payload FROM events
                     WHERE session_id = ?1 AND type = 'gui.action'
                     ORDER BY seq DESC LIMIT ?2",
                )?;
                let rows = statement
                    .query_map(params![session_id, limit], |row| row.get::<_, String>(0))?;
                for payload in rows {
                    if let Ok(action) = serde_json::from_str::<GuiActionInfo>(&payload?) {
                        actions.push(action);
                    }
                }
            }
            None => {
                let mut statement = conn.prepare(
                    "SELECT payload FROM events
                     WHERE type = 'gui.action'
                     ORDER BY created_at DESC LIMIT ?1",
                )?;
                let rows = statement.query_map(params![limit], |row| row.get::<_, String>(0))?;
                for payload in rows {
                    if let Ok(action) = serde_json::from_str::<GuiActionInfo>(&payload?) {
                        actions.push(action);
                    }
                }
            }
        }
        actions.reverse();
        Ok(actions)
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

    /// Newest-first bounded window of one event kind. Used by the read-only
    /// tool-activity projection; callers never load an unbounded session log.
    pub fn list_recent_events_by_kind(
        &self,
        session_id: &str,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 AND type = ?2
             ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows =
            statement.query_map(params![session_id, kind, limit.clamp(1, 1_000)], |row| {
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
        events.sort_by_key(|event| std::cmp::Reverse(event.seq));
        Ok(events)
    }

    pub fn event_count_by_kind(&self, session_id: &str, kind: &str) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND type = ?2",
            params![session_id, kind],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
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

    pub fn get_session_run_policy(&self, project_id: &str) -> Result<SessionRunPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT auto_resume, max_attempts, max_age_hours
                 FROM project_session_run_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(SessionRunPolicy {
                        auto_resume: row.get::<_, i64>(0)? != 0,
                        max_attempts: row.get::<_, i64>(1)?.clamp(1, 3) as u32,
                        max_age_hours: row.get::<_, i64>(2)?.clamp(1, 168) as u32,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn has_session_run_policy(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM project_session_run_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn set_session_run_policy(
        &self,
        project_id: &str,
        policy: &SessionRunPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_session_run_policies
             (project_id, auto_resume, max_attempts, max_age_hours, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                auto_resume = excluded.auto_resume,
                max_attempts = excluded.max_attempts,
                max_age_hours = excluded.max_age_hours,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.auto_resume { 1 } else { 0 },
                policy.max_attempts.clamp(1, 3) as i64,
                policy.max_age_hours.clamp(1, 168) as i64,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Insert a run row and its `session.run.started` event atomically.
    pub fn start_session_run(&self, run: &SessionRun) -> Result<(SessionRun, Event)> {
        validate_run_status(&run.status, &["running"])?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let project_id: String = tx
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![run.session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {}", run.session_id))?;
        if project_id != run.project_id {
            return Err(anyhow!(
                "run project {} does not match session project {project_id}",
                run.project_id
            ));
        }
        let event_seq = next_event_seq_tx(&tx, &run.session_id)?;
        let mut stored = run.clone();
        stored.last_event_seq = event_seq;
        insert_session_run_tx(&tx, &stored)?;
        let event = insert_event_tx(
            &tx,
            &stored.session_id,
            "session.run.started",
            serde_json::json!({"run": serde_json::to_value(&stored)?}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((stored, event))
    }

    /// Record a checkpoint at a safe turn boundary. The cursor values are the
    /// durable event sequence and message-event count observed by the caller
    /// immediately before the checkpoint event was appended.
    pub fn record_run_checkpoint(
        &self,
        checkpoint: &RunCheckpoint,
    ) -> Result<(RunCheckpoint, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stored = checkpoint.clone();
        if stored.seq == 0 {
            stored.seq = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_checkpoints WHERE run_id = ?1",
                params![stored.run_id],
                |row| row.get::<_, i64>(0),
            )? as u64;
        }
        let event_seq = next_event_seq_tx(&tx, &stored.session_id)?;
        tx.execute(
            "INSERT INTO run_checkpoints
             (checkpoint_id, run_id, session_id, seq, turn, message_cursor, event_cursor,
              budget_json, worktree_head, plan_revision, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                stored.checkpoint_id,
                stored.run_id,
                stored.session_id,
                stored.seq as i64,
                i64::from(stored.turn),
                stored.message_cursor as i64,
                stored.event_cursor as i64,
                serde_json::to_string(&stored.budget)?,
                stored.worktree_head,
                i64::from(stored.plan_revision),
                stored.created_at
            ],
        )?;
        tx.execute(
            "UPDATE session_runs SET last_event_seq = ?2 WHERE run_id = ?1",
            params![stored.run_id, event_seq as i64],
        )?;
        let event = insert_event_tx(
            &tx,
            &stored.session_id,
            "session.run.checkpointed",
            serde_json::json!({"checkpoint": serde_json::to_value(&stored)?}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((stored, event))
    }

    /// Insert a step intent and, for a tool call, its operational tool-call row.
    /// Both journal events commit in the same transaction.
    pub fn record_run_step_intent(
        &self,
        step: &RunStep,
        tool: Option<&RunToolCall>,
    ) -> Result<(RunStep, Vec<Event>)> {
        validate_step_status(&step.status, &["intent"])?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stored = step.clone();
        if stored.seq == 0 {
            stored.seq = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_steps WHERE run_id = ?1",
                params![stored.run_id],
                |row| row.get::<_, i64>(0),
            )? as u64;
        }
        insert_run_step_tx(&tx, &stored)?;
        if let Some(tool) = tool
            && (tool.step_id != stored.step_id || tool.run_id != stored.run_id)
        {
            return Err(anyhow!("tool call does not match its step intent"));
        }
        let step_event = insert_event_tx(
            &tx,
            &stored.session_id,
            "session.run.step_intent",
            serde_json::json!({"step": serde_json::to_value(&stored)?}),
        )?;
        let mut events = vec![step_event];
        if let Some(tool) = tool {
            insert_run_tool_call_tx(&tx, tool)?;
            events.push(insert_event_tx(
                &tx,
                &stored.session_id,
                "session.run.tool_intent",
                serde_json::json!({"tool": serde_json::to_value(tool)?}),
            )?);
        }
        let last_seq = events.last().map(|event| event.seq).unwrap_or(0);
        tx.execute(
            "UPDATE session_runs SET last_event_seq = ?2 WHERE run_id = ?1",
            params![stored.run_id, last_seq as i64],
        )?;
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((stored, events))
    }

    /// Complete or fail a step. For a tool step the operational tool row is
    /// updated in the same transaction and a bounded result/error is attached.
    pub fn record_run_step_completed(
        &self,
        step: &RunStep,
        tool: Option<&RunToolCall>,
    ) -> Result<(RunStep, Option<RunToolCall>, Vec<Event>)> {
        validate_step_status(&step.status, &["completed", "failed", "unknown"])?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE run_steps
             SET status = ?2, input_hash = ?3, output_hash = ?4, payload_json = ?5,
                 updated_at = ?6
             WHERE step_id = ?1",
            params![
                step.step_id,
                step.status,
                step.input_hash,
                step.output_hash,
                serde_json::to_string(&step.payload)?,
                step.updated_at
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!("run step not found: {}", step.step_id));
        }
        let mut events = vec![insert_event_tx(
            &tx,
            &step.session_id,
            "session.run.step_completed",
            serde_json::json!({"step": serde_json::to_value(step)?}),
        )?];
        let mut stored_tool = None;
        if let Some(tool) = tool {
            let changed = tx.execute(
                "UPDATE run_tool_calls
                 SET status = ?3, result_json = ?4, error = ?5, updated_at = ?6
                 WHERE run_id = ?1 AND tool_call_id = ?2",
                params![
                    tool.run_id,
                    tool.tool_call_id,
                    tool.status,
                    tool.result
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    tool.error,
                    tool.updated_at
                ],
            )?;
            if changed == 0 {
                return Err(anyhow!(
                    "run tool call not found: {} / {}",
                    tool.run_id,
                    tool.tool_call_id
                ));
            }
            events.push(insert_event_tx(
                &tx,
                &step.session_id,
                "session.run.tool_completed",
                serde_json::json!({"tool": serde_json::to_value(tool)?}),
            )?);
            stored_tool = Some(tool.clone());
        }
        let last_seq = events.last().map(|event| event.seq).unwrap_or(0);
        tx.execute(
            "UPDATE session_runs SET last_event_seq = ?2 WHERE run_id = ?1",
            params![step.run_id, last_seq as i64],
        )?;
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((step.clone(), stored_tool, events))
    }

    /// Finish a run. `completed`, `failed`, and `interrupted` are the only
    /// terminal statuses; cancellation is recorded as `interrupted` with a
    /// `cancelled` failure kind so it cannot be resumed implicitly.
    pub fn finish_session_run(
        &self,
        run_id: &str,
        status: &str,
        failure: Option<&vp_protocol::types::RunFailure>,
    ) -> Result<(SessionRun, Event)> {
        validate_run_status(status, &["completed", "failed", "interrupted"])?;
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let session_id: String = tx
            .query_row(
                "SELECT session_id FROM session_runs WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("run not found: {run_id}"))?;
        let event_seq = next_event_seq_tx(&tx, &session_id)?;
        let changed = tx.execute(
            "UPDATE session_runs
             SET status = ?2, failure_json = ?3, ended_at = ?4, updated_at = ?4,
                 last_event_seq = ?5
             WHERE run_id = ?1 AND status = 'running'",
            params![
                run_id,
                status,
                failure.map(serde_json::to_string).transpose()?,
                now,
                event_seq as i64
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!(
                "run {run_id} is not running; terminal status cannot be overwritten"
            ));
        }
        if status != "completed" {
            // A replay claim is only final once the resumed attempt completed.
            // If it failed or was interrupted, release the claim so the next
            // attempt can return the journaled result instead of re-running a
            // write whose result never reached the persisted conversation.
            tx.execute(
                "UPDATE run_tool_calls
                 SET consumed_by_run_id = NULL, consumed_at = NULL, updated_at = ?2
                 WHERE consumed_by_run_id = ?1",
                params![run_id, now],
            )?;
        }
        let run = load_session_run_tx(&tx, run_id)?;
        let (kind, payload) = match status {
            "completed" => (
                "session.run.completed",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            ),
            "failed" => (
                "session.run.failed",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            ),
            _ => (
                "session.run.interrupted",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            ),
        };
        let event = insert_event_tx(&tx, &session_id, kind, payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((run, event))
    }

    /// Mark the previous run `resumed` and insert the linked new attempt in one
    /// transaction, emitting both `session.run.resumed` and
    /// `session.run.started`.
    pub fn mark_session_run_resumed(
        &self,
        previous_run_id: &str,
        new_run: &SessionRun,
    ) -> Result<(SessionRun, Event, Event)> {
        validate_run_status(&new_run.status, &["running"])?;
        if new_run.resumed_from_run_id.as_deref() != Some(previous_run_id) {
            return Err(anyhow!(
                "resumed run must point at {previous_run_id} as its source"
            ));
        }
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let previous = load_session_run_tx(&tx, previous_run_id)?;
        if !matches!(previous.status.as_str(), "interrupted" | "failed") {
            return Err(anyhow!(
                "run {previous_run_id} is {} and cannot be resumed",
                previous.status
            ));
        }
        if previous
            .failure
            .as_ref()
            .is_some_and(|failure| matches!(failure.kind.as_str(), "cancelled" | "superseded"))
        {
            return Err(anyhow!(
                "run {previous_run_id} is {} and cannot be resumed",
                previous
                    .failure
                    .as_ref()
                    .map(|failure| failure.kind.as_str())
                    .unwrap_or("cancelled")
            ));
        }
        if previous.session_id != new_run.session_id {
            return Err(anyhow!("resumed run belongs to a different session"));
        }
        let resumed_seq = next_event_seq_tx(&tx, &new_run.session_id)?;
        let started_seq = resumed_seq.saturating_add(1);
        tx.execute(
            "UPDATE session_runs
             SET status = 'resumed', updated_at = ?2, last_event_seq = ?3
             WHERE run_id = ?1",
            params![previous_run_id, now, resumed_seq as i64],
        )?;
        let mut stored_new = new_run.clone();
        stored_new.last_event_seq = started_seq;
        insert_session_run_tx(&tx, &stored_new)?;
        let previous = load_session_run_tx(&tx, previous_run_id)?;
        let resumed = insert_event_tx(
            &tx,
            &new_run.session_id,
            "session.run.resumed",
            serde_json::json!({
                "previous_run": previous,
                "run_id": stored_new.run_id,
                "attempt": stored_new.attempt,
            }),
        )?;
        let started = insert_event_tx(
            &tx,
            &new_run.session_id,
            "session.run.started",
            serde_json::json!({"run": stored_new}),
        )?;
        tx.commit()?;
        for event in [&resumed, &started] {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((previous, resumed, started))
    }

    /// A new top-level prompt is an explicit abandonment of older unresolved
    /// attempts in the same session. They are not deleted (their journal stays
    /// inspectable); their failure kind becomes `superseded`, which resume
    /// preflight refuses. Status is preserved so replay remains truthful about
    /// whether the prior attempt was interrupted or failed.
    pub fn supersede_resumable_session_runs(
        &self,
        session_id: &str,
        new_run_id: &str,
    ) -> Result<Vec<Event>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT run_id, status, failure_json FROM session_runs
             WHERE session_id = ?1 AND top_level = 1
               AND status IN ('interrupted', 'failed')
             ORDER BY started_at ASC, run_id ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut events = Vec::new();
        for (run_id, status, failure_json) in rows {
            let failure = failure_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<vp_protocol::types::RunFailure>(json).ok());
            if failure
                .as_ref()
                .is_some_and(|failure| matches!(failure.kind.as_str(), "cancelled" | "superseded"))
            {
                continue;
            }
            let previous_kind = failure
                .as_ref()
                .map(|failure| failure.kind.as_str())
                .unwrap_or("interrupted");
            let superseded = serde_json::to_string(&vp_protocol::types::RunFailure {
                kind: "superseded".to_string(),
                message: format!(
                    "superseded by new prompt run {new_run_id} (previous failure: {previous_kind})"
                ),
            })?;
            let event_seq = next_event_seq_tx(&tx, session_id)?;
            tx.execute(
                "UPDATE session_runs
                 SET failure_json = ?2, updated_at = ?3, last_event_seq = ?4
                 WHERE run_id = ?1 AND status = ?5",
                params![run_id, superseded, now, event_seq as i64, status],
            )?;
            let run = load_session_run_tx(&tx, &run_id)?;
            let kind = if status == "failed" {
                "session.run.failed"
            } else {
                "session.run.interrupted"
            };
            events.push(insert_event_tx(
                &tx,
                session_id,
                kind,
                serde_json::json!({"run": run}),
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

    /// Startup recovery. Every `running` run becomes `interrupted`; tool-call
    /// intents become `unknown` because the process cannot know whether the
    /// side effect landed. Model-step intents stay `intent` and are labeled
    /// retryable by the resume classifier.
    pub fn interrupt_running_session_runs(&self) -> Result<(Vec<SessionRun>, Vec<Event>)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT run_id FROM session_runs WHERE status = 'running' ORDER BY started_at ASC, run_id ASC",
        )?;
        let run_ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut cancel_stmt =
            tx.prepare("SELECT payload FROM events WHERE type = 'session.run.cancel_requested'")?;
        let cancelled_runs = cancel_stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(Result::ok)
            .filter_map(|payload| {
                serde_json::from_str::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("run_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
            })
            .collect::<std::collections::HashSet<_>>();
        drop(cancel_stmt);
        tx.execute(
            "UPDATE run_tool_calls
             SET status = 'unknown', error = COALESCE(error, 'daemon stopped before the tool outcome was committed'),
                 consumed_by_run_id = NULL, consumed_at = NULL, updated_at = ?1
             WHERE status = 'intent'",
            params![now],
        )?;
        tx.execute(
            "UPDATE run_tool_calls
             SET consumed_by_run_id = NULL, consumed_at = NULL, updated_at = ?1
             WHERE consumed_by_run_id IS NOT NULL",
            params![now],
        )?;
        let mut runs = Vec::new();
        let mut events = Vec::new();
        for run_id in run_ids {
            let session_id: String = tx.query_row(
                "SELECT session_id FROM session_runs WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )?;
            let (kind, message) = if cancelled_runs.contains(&run_id) {
                (
                    "cancelled",
                    "the operator requested cancellation before the daemon restarted",
                )
            } else {
                (
                    "interrupted",
                    "the daemon restarted while this run was active",
                )
            };
            let failure = serde_json::to_string(&vp_protocol::types::RunFailure {
                kind: kind.to_string(),
                message: message.to_string(),
            })?;
            let event_seq = next_event_seq_tx(&tx, &session_id)?;
            tx.execute(
                "UPDATE session_runs
                 SET status = 'interrupted', failure_json = ?2, ended_at = ?3, updated_at = ?3,
                     last_event_seq = ?4
                 WHERE run_id = ?1 AND status = 'running'",
                params![run_id, failure, now, event_seq as i64],
            )?;
            let run = load_session_run_tx(&tx, &run_id)?;
            events.push(insert_event_tx(
                &tx,
                &run.session_id,
                "session.run.interrupted",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            )?);
            runs.push(run);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((runs, events))
    }

    /// Latest event sequence and the number of persisted `message.created`
    /// events. Checkpoints store these cursors without reading the message or
    /// event bodies into memory.
    pub fn session_has_imported_event(&self, session_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND type IN ('session.imported', 'session.rehydrated')",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn plan_revision_count(&self, session_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events
             WHERE session_id = ?1 AND type IN ('team_plan.submitted', 'team_plan.revised')",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.clamp(0, i64::from(u32::MAX)) as u32)
    }

    pub fn journal_cursors(&self, session_id: &str) -> Result<(u64, u64)> {
        let conn = self.lock()?;
        let (event_cursor, message_cursor) = conn.query_row(
            "SELECT COALESCE((SELECT MAX(seq) FROM events WHERE session_id = ?1), 0),
                    COALESCE((SELECT COUNT(*) FROM messages WHERE session_id = ?1), 0)",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?.max(0) as u64,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            },
        )?;
        Ok((event_cursor, message_cursor))
    }

    pub fn list_all_session_runs(&self, session_id: &str) -> Result<Vec<SessionRun>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                    department, objective, provider, model, status, attempt, resumed_from_run_id,
                    resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                    worktree_head, plan_revision, last_event_seq, failure_json
             FROM session_runs WHERE session_id = ?1
             ORDER BY started_at ASC, run_id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_session_run)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_runs(&self, session_id: &str, limit: u32) -> Result<Vec<SessionRun>> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                    department, objective, provider, model, status, attempt, resumed_from_run_id,
                    resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                    worktree_head, plan_revision, last_event_seq, failure_json
             FROM session_runs WHERE session_id = ?1
             ORDER BY started_at DESC, run_id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit], map_session_run)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_resumable_session_runs(&self, limit: u32) -> Result<Vec<SessionRun>> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                    department, objective, provider, model, status, attempt, resumed_from_run_id,
                    resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                    worktree_head, plan_revision, last_event_seq, failure_json
             FROM session_runs
             WHERE top_level = 1 AND status IN ('interrupted', 'failed')
               AND (failure_json IS NULL
                    OR (failure_json NOT LIKE '%\"kind\":\"cancelled\"%'
                        AND failure_json NOT LIKE '%\"kind\":\"superseded\"%'))
             ORDER BY started_at DESC, run_id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], map_session_run)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_session_run(&self, run_id: &str) -> Result<Option<SessionRun>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                    department, objective, provider, model, status, attempt, resumed_from_run_id,
                    resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                    worktree_head, plan_revision, last_event_seq, failure_json
             FROM session_runs WHERE run_id = ?1",
            params![run_id],
            map_session_run,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_run_steps(&self, run_id: &str) -> Result<Vec<RunStep>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT step_id, run_id, session_id, seq, turn, kind, status, input_hash,
                    output_hash, payload_json, created_at, updated_at
             FROM run_steps WHERE run_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_step)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_run_tool_calls(&self, run_id: &str) -> Result<Vec<RunToolCall>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, session_id, step_id, tool_call_id, tool_name, input_fingerprint,
                    status, result_json, error, consumed_by_run_id, consumed_at, created_at,
                    updated_at
             FROM run_tool_calls WHERE run_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_tool_call)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_run_checkpoints(&self, run_id: &str) -> Result<Vec<RunCheckpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT checkpoint_id, run_id, session_id, seq, turn, message_cursor, event_cursor,
                    budget_json, worktree_head, plan_revision, created_at
             FROM run_checkpoints WHERE run_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_checkpoint)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_run_steps(&self, session_id: &str) -> Result<Vec<RunStep>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT step_id, run_id, session_id, seq, turn, kind, status, input_hash,
                    output_hash, payload_json, created_at, updated_at
             FROM run_steps WHERE session_id = ?1 ORDER BY run_id ASC, seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_run_step)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_lineage_run_checkpoints(&self, run_id: &str) -> Result<Vec<RunCheckpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT c.checkpoint_id, c.run_id, c.session_id, c.seq, c.turn,
                    c.message_cursor, c.event_cursor, c.budget_json, c.worktree_head,
                    c.plan_revision, c.created_at
             FROM run_checkpoints c
             JOIN session_runs r ON r.run_id = c.run_id
             WHERE COALESCE(NULLIF(r.resume_root_id, ''), r.run_id) = (
                 SELECT COALESCE(NULLIF(resume_root_id, ''), run_id)
                 FROM session_runs WHERE run_id = ?1
             )
             ORDER BY c.created_at ASC, c.run_id ASC, c.seq ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_checkpoint)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_run_checkpoints(&self, session_id: &str) -> Result<Vec<RunCheckpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT checkpoint_id, run_id, session_id, seq, turn, message_cursor, event_cursor,
                    budget_json, worktree_head, plan_revision, created_at
             FROM run_checkpoints WHERE session_id = ?1 ORDER BY run_id ASC, seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_run_checkpoint)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Every tool row in the resume lineage whose root is the root of
    /// `run_id`. Used to build the bounded journal note and the duplicate-write
    /// gate. Results are newest-first but the caller re-orders for display.
    pub fn list_run_lineage_tool_calls(&self, run_id: &str) -> Result<Vec<RunToolCall>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT tc.run_id, tc.session_id, tc.step_id, tc.tool_call_id, tc.tool_name,
                    tc.input_fingerprint, tc.status, tc.result_json, tc.error,
                    tc.consumed_by_run_id, tc.consumed_at, tc.created_at, tc.updated_at
             FROM run_tool_calls tc
             JOIN session_runs r ON r.run_id = tc.run_id
             WHERE COALESCE(NULLIF(r.resume_root_id, ''), r.run_id) = (
                 SELECT COALESCE(NULLIF(resume_root_id, ''), run_id)
                 FROM session_runs WHERE run_id = ?1
             )
             ORDER BY tc.created_at ASC, tc.rowid ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_tool_call)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Consume one completed journaled tool result for a resumed attempt.
    /// The compare-and-set on `consumed_by_run_id IS NULL` makes this
    /// consume-once across concurrent/duplicate calls: a second identical call
    /// in the same attempt will not short-circuit on the same row again.
    pub fn claim_completed_tool_result(
        &self,
        run_id: &str,
        tool_name: &str,
        input_fingerprint: &str,
        new_run_id: &str,
    ) -> Result<Option<RunToolCall>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let candidate: Option<i64> = tx
            .query_row(
                "SELECT tc.rowid
                 FROM run_tool_calls tc
                 JOIN session_runs r ON r.run_id = tc.run_id
                 WHERE COALESCE(NULLIF(r.resume_root_id, ''), r.run_id) = (
                     SELECT COALESCE(NULLIF(resume_root_id, ''), run_id)
                     FROM session_runs WHERE run_id = ?1
                 )
                   AND tc.run_id <> ?4
                   AND tc.status = 'completed'
                   AND tc.tool_name = ?2
                   AND tc.input_fingerprint = ?3
                   AND tc.consumed_by_run_id IS NULL
                 ORDER BY tc.created_at ASC, tc.rowid ASC
                 LIMIT 1",
                params![run_id, tool_name, input_fingerprint, new_run_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(rowid) = candidate else {
            return Ok(None);
        };
        let changed = tx.execute(
            "UPDATE run_tool_calls
             SET consumed_by_run_id = ?2, consumed_at = ?3, updated_at = ?3
             WHERE rowid = ?1 AND consumed_by_run_id IS NULL",
            params![rowid, new_run_id, now],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let tool = tx.query_row(
            "SELECT run_id, session_id, step_id, tool_call_id, tool_name, input_fingerprint,
                    status, result_json, error, consumed_by_run_id, consumed_at, created_at,
                    updated_at
             FROM run_tool_calls WHERE rowid = ?1",
            params![rowid],
            map_run_tool_call,
        )?;
        tx.commit()?;
        Ok(Some(tool))
    }

    /// Replace event-backed run/step/checkpoint projections from replay.
    /// Operational tool-call rows are preserved; a run that still has tool
    /// rows is never dropped just because it is absent from the expected set.
    pub fn replace_run_journal_projections(
        &self,
        session_id: &str,
        runs: &[SessionRun],
        steps: &[RunStep],
        checkpoints: &[RunCheckpoint],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_run_journal_projections_tx(&tx, session_id, runs, steps, checkpoints)?;
        tx.commit()?;
        Ok(())
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
            lease_owner: None,
            lease_expires_at: None,
            delivery_attempt: 0,
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

    /// Read one continuation by id. This is an operational lookup used by the
    /// status RPC; it does not change ownership.
    pub fn get_session_continuation(&self, id: &str) -> Result<Option<SessionContinuation>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {SESSION_CONTINUATION_COLUMNS}
                     FROM session_continuations WHERE id = ?1"
                ),
                params![id],
                map_session_continuation,
            )
            .optional()?)
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

    /// All pending and ready rows across sessions, plus delivering rows whose
    /// claim has expired. The scheduler tick evaluates this snapshot; it never
    /// sees terminal rows and never touches a live foreign delivery claim.
    pub fn list_open_session_continuations(&self) -> Result<Vec<SessionContinuation>> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_CONTINUATION_COLUMNS}
             FROM session_continuations
             WHERE status IN ('pending', 'ready')
                OR (status = 'delivering'
                    AND lease_expires_at IS NOT NULL
                    AND julianday(lease_expires_at) <= julianday(?1))
             ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map(params![now], map_session_continuation)?;
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
                 next_attempt_at = NULL, updated_at = ?3,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
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

    // Claim/ack/fail/retry transitions live in `store::continuation_lease`;
    // they are token-fenced and require an explicit delivery owner.

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
             SET status = 'cancelled', last_error = ?1, next_attempt_at = NULL, updated_at = ?2,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
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

    // Runtime lease acquisition, fencing, renewal, release, and cleanup live in
    // `store::lease` so the ownership state machine stays in one auditable module.

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
                            acquired_at, heartbeat_at, expires_at, generation
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
            // Browser, computer, and gui are one physical surface. A legacy
            // row from an older daemon stays authoritative until it expires,
            // so a new `gui` acquisition cannot drive the GUI concurrently
            // with an old helper. Operators can release a stuck row.
            let holder = tx
                .query_row(
                    "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                            acquired_at, heartbeat_at, expires_at, generation
                     FROM computer_leases
                     WHERE surface = ?1 OR surface IN ('gui', 'browser', 'computer')
                     ORDER BY acquired_at ASC, lease_id ASC LIMIT 1",
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
                let stored_generation = tx
                    .query_row(
                        "SELECT value FROM store_meta WHERE key = 'computer_lease_generation'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                let table_generation = tx.query_row(
                    "SELECT COALESCE(MAX(generation), 0) FROM computer_leases",
                    [],
                    |row| row.get::<_, i64>(0),
                )? as u64;
                let generation = stored_generation.max(table_generation) + 1;
                tx.execute(
                    "INSERT INTO store_meta (key, value)
                     VALUES ('computer_lease_generation', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![generation.to_string()],
                )?;
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
                    generation,
                };
                tx.execute(
                    "INSERT INTO computer_leases
                     (lease_id, surface, owner_id, session_id, agent_id, agent_name,
                      acquired_at, heartbeat_at, expires_at, generation)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                        lease.generation as i64,
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
                        acquired_at, heartbeat_at, expires_at, generation
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
                    acquired_at, heartbeat_at, expires_at, generation
             FROM computer_leases ORDER BY surface ASC, acquired_at ASC",
        )?;
        let rows = stmt.query_map([], map_computer_lease)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn computer_lease(&self, lease_id: &str) -> Result<Option<ComputerLease>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                    acquired_at, heartbeat_at, expires_at, generation
             FROM computer_leases WHERE lease_id = ?1",
            params![lease_id],
            map_computer_lease,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Lowest unexpired lease on the shared physical GUI surface, including
    /// legacy `browser`/`computer` rows. The coordinator uses this to produce
    /// a typed busy error with a holder and retry hint.
    pub fn gui_lease_holder(&self) -> Result<Option<ComputerLease>> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        conn.query_row(
            "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                    acquired_at, heartbeat_at, expires_at, generation
             FROM computer_leases
             WHERE surface IN ('gui', 'browser', 'computer')
               AND julianday(expires_at) > julianday(?1)
             ORDER BY acquired_at ASC, lease_id ASC LIMIT 1",
            params![now],
            map_computer_lease,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Retire legacy per-helper lease rows at daemon startup. Helper processes
    /// are children of the daemon, so no legacy helper survives a restart. The
    /// rows are still audited rather than deleted silently.
    pub fn clear_legacy_gui_leases(&self, reason: &str) -> Result<(usize, Vec<Event>)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let legacy = {
            let mut stmt = tx.prepare(
                "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                        acquired_at, heartbeat_at, expires_at, generation
                 FROM computer_leases WHERE surface IN ('browser', 'computer')
                 ORDER BY acquired_at ASC, lease_id ASC",
            )?;
            let rows = stmt.query_map([], map_computer_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::new();
        for lease in &legacy {
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
                    "reason": reason,
                    "legacy_surface": true,
                }),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append legacy GUI lease expiry mirror");
            }
        }
        Ok((legacy.len(), events))
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
                        acquired_at, heartbeat_at, expires_at, generation
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
        let process_instance = self.process_owner_instance();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let sessions = {
            let mut stmt = tx.prepare(
                "SELECT id FROM sessions
                 WHERE (status IN ('running', 'paused', 'waiting_input')
                    OR (status = 'queued' AND (
                        EXISTS (
                            SELECT 1 FROM session_runs r
                            WHERE r.session_id = sessions.id AND r.status = 'running'
                        )
                        OR EXISTS (
                            SELECT 1 FROM events e
                            WHERE e.session_id = sessions.id
                              AND e.type = 'agent.queued'
                              AND e.payload LIKE '%\"resumed_run_id\"%'
                        )
                    )))
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || sessions.id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE sessions SET status = 'failed'
             WHERE (status IN ('running', 'paused', 'waiting_input')
                OR (status = 'queued' AND (
                    EXISTS (
                        SELECT 1 FROM session_runs r
                        WHERE r.session_id = sessions.id AND r.status = 'running'
                    )
                    OR EXISTS (
                        SELECT 1 FROM events e
                        WHERE e.session_id = sessions.id
                          AND e.type = 'agent.queued'
                          AND e.payload LIKE '%\"resumed_run_id\"%'
                    )
                )))
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || sessions.id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?1
                     AND julianday(l.expires_at) > julianday(?2)
               )",
            params![process_instance, now],
        )?;
        let agents = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id FROM agents
                 WHERE status IN ('running', 'waiting_input', 'queued')
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || agents.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE agents SET status = 'failed'
             WHERE status IN ('running', 'waiting_input', 'queued')
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || agents.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?1
                     AND julianday(l.expires_at) > julianday(?2)
               )",
            params![process_instance, now],
        )?;
        let approvals = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id FROM approvals
                 WHERE status = 'pending'
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || approvals.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE approvals SET status = 'denied', resolved_at = ?1
             WHERE status = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || approvals.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               )",
            params![now, process_instance],
        )?;
        let expired_questions = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id FROM questions
                 WHERE status = 'pending'
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || questions.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE questions SET status = 'expired', answered_at = ?1
             WHERE status = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || questions.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               )",
            params![now, process_instance],
        )?;
        let queue_ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM merge_queue
                 WHERE status IN ('queued', 'merging', 'checking')
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'integration:' || merge_queue.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE merge_queue
             SET status = 'interrupted',
                 detail = 'daemon restarted during integration',
                 updated_at = ?1
             WHERE status IN ('queued', 'merging', 'checking')
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'integration:' || merge_queue.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               )",
            params![now, process_instance],
        )?;
        let interrupted_tasks = {
            let mut stmt = tx.prepare(
                "SELECT session_id, task_id FROM plan_tasks
                 WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || plan_tasks.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )
                 ORDER BY session_id ASC, plan_seq ASC, task_id ASC",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let interrupted_steering = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages
                 WHERE status = 'pending'
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || steering_messages.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map(params![process_instance, now], map_steering)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE steering_messages SET status = 'cancelled'
             WHERE status = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || steering_messages.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?1
                     AND julianday(l.expires_at) > julianday(?2)
               )",
            params![process_instance, now],
        )?;
        let legacy_continuations = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {SESSION_CONTINUATION_COLUMNS}
                 FROM session_continuations
                 WHERE status = 'delivering' AND lease_expires_at IS NULL
                 ORDER BY created_at ASC, id ASC"
            ))?;
            let rows = stmt.query_map([], map_session_continuation)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE session_continuations
             SET status = 'failed',
                 last_error = 'daemon restarted while delivering; not retried automatically',
                 next_attempt_at = NULL, updated_at = ?1,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE status = 'delivering' AND lease_expires_at IS NULL",
            params![now],
        )?;
        let expired_continuations = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {SESSION_CONTINUATION_COLUMNS}
                 FROM session_continuations
                 WHERE status = 'delivering'
                   AND lease_expires_at IS NOT NULL
                   AND julianday(lease_expires_at) <= julianday(?1)
                 ORDER BY created_at ASC, id ASC"
            ))?;
            let rows = stmt.query_map(params![now], map_session_continuation)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE session_continuations
             SET status = 'ready',
                 last_error = 'delivery claim expired before acknowledgement; reclaimed',
                 next_attempt_at = NULL, updated_at = ?1,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE status = 'delivering'
               AND lease_expires_at IS NOT NULL
               AND julianday(lease_expires_at) <= julianday(?1)",
            params![now],
        )?;
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'interrupted',
                 last_error = 'daemon restarted while the task was active',
                 finished_at = ?1, updated_at = ?1
             WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || plan_tasks.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               )",
            params![now, process_instance],
        )?;
        tx.execute(
            "UPDATE intake_tasks
             SET status = 'failed',
                 last_error = 'daemon restarted while the intake task was active',
                 next_attempt_at = NULL, updated_at = ?1
             WHERE status IN ('dispatching', 'running')
               AND (session_id IS NULL OR NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || intake_tasks.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               ))",
            params![now, process_instance],
        )?;
        tx.execute("DELETE FROM write_leases", [])?;
        // Only this process's leases (and pre-fencing legacy rows) are
        // cleared. Another daemon sharing the data directory keeps its live
        // ownership; its expired rows are swept later by the normal sweep.
        // Record a token-free audit row for each so a restart does not erase
        // the ownership trail.
        let own_leases = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {RUNTIME_LEASE_COLUMNS}
                 FROM runtime_leases
                 WHERE owner_instance = ?1 OR owner_instance = ''
                 ORDER BY lease_key ASC"
            ))?;
            let rows = stmt.query_map(params![process_instance], map_runtime_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for lease in &own_leases {
            crate::store::lease::insert_audit_tx(
                &tx,
                &lease.lease_key,
                &lease.kind,
                "startup_cleanup",
                &lease.owner_id,
                &lease.owner_instance,
                lease.fencing_token,
                Some("daemon restarted while the lease row was held"),
                &now,
            )?;
        }
        tx.execute(
            "DELETE FROM runtime_leases
             WHERE owner_instance = ?1 OR owner_instance = ''",
            params![process_instance],
        )?;
        let interrupted_computer_leases = {
            let mut stmt = tx.prepare(
                "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                        acquired_at, heartbeat_at, expires_at, generation
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
        for continuation in &legacy_continuations {
            let mut failed = continuation.clone();
            failed.status = "failed".to_string();
            failed.last_error =
                Some("daemon restarted while delivering; not retried automatically".to_string());
            failed.next_attempt_at = None;
            failed.updated_at = now.clone();
            failed.lease_owner = None;
            failed.lease_expires_at = None;
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
        for continuation in &expired_continuations {
            let mut reclaimed = continuation.clone();
            reclaimed.status = "ready".to_string();
            reclaimed.last_error =
                Some("delivery claim expired before acknowledgement; reclaimed".to_string());
            reclaimed.next_attempt_at = None;
            reclaimed.updated_at = now.clone();
            reclaimed.lease_owner = None;
            reclaimed.lease_expires_at = None;
            let mut payload = serde_json::to_value(&reclaimed)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "reason".into(),
                    serde_json::json!("delivery claim expired before acknowledgement"),
                );
            }
            events.push(insert_event_tx(
                &tx,
                &reclaimed.session_id,
                "session.continuation.reclaimed",
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
        if let Some(runs) = &repair.runs {
            replace_run_journal_projections_tx(
                &tx,
                session_id,
                runs,
                repair.run_steps.as_deref().unwrap_or(&[]),
                repair.run_checkpoints.as_deref().unwrap_or(&[]),
            )?;
        }
        if let Some(context_checkpoints) = &repair.context_checkpoints {
            replace_context_checkpoints_tx(&tx, session_id, context_checkpoints)?;
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

        // Tool-output bodies are operational and never part of a bundle. Drop
        // any stale handle rows for this session id before registering the
        // imported archive; imported event markers keep their preview text but
        // cannot resolve a source handle.
        tx.execute(
            "DELETE FROM tool_outputs WHERE session_id = ?1",
            params![bundle.session.id],
        )?;
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
        replace_run_journal_projections_tx(
            &tx,
            &bundle.session.id,
            &bundle.session_runs,
            &bundle.run_steps,
            &bundle.run_checkpoints,
        )?;
        replace_context_checkpoints_tx(&tx, &bundle.session.id, &bundle.context_checkpoints)?;
        let has_context_checkpoint_events = bundle.events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                "context.checkpointed" | "context.checkpoint.deleted"
            )
        });
        if !has_context_checkpoint_events {
            // A pre-D1 producer could carry only the projection. Synthesize
            // the full-payload event so the imported archive remains
            // event-replayable instead of holding an orphaned row.
            for checkpoint in &bundle.context_checkpoints {
                let payload =
                    payload_with_checkpoint_id(serde_json::to_value(checkpoint)?, &checkpoint.id)?;
                insert_event_tx(&tx, &bundle.session.id, "context.checkpointed", payload)?;
            }
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
            // A durable resume already created its linked run row. Re-running
            // the queued journal note as a plain prompt would bypass the
            // duplicate-write gate; startup recovery marks that session failed
            // and leaves it to the explicit/opt-in resume path.
            if payload.get("resumed_run_id").is_some() {
                continue;
            }
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
    series_id, parent_continuation_id, recurrence_every_secs, recurrence_max_runs, run_count,
    lease_owner, lease_expires_at, delivery_attempt";

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
        lease_owner: row.get(23)?,
        lease_expires_at: row.get(24)?,
        delivery_attempt: row.get::<_, i64>(25)?.max(0) as u32,
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
        lease_owner: None,
        lease_expires_at: None,
        delivery_attempt: 0,
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
          run_count, lease_owner, lease_expires_at, delivery_attempt)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
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
            i64::from(continuation.run_count),
            continuation.lease_owner,
            continuation.lease_expires_at,
            i64::from(continuation.delivery_attempt)
        ],
    )?;
    Ok(())
}

pub(crate) fn steering_content(message: &SteeringMessage) -> String {
    format!("[Operator steering #{}] {}", message.id, message.body)
}

fn replace_run_journal_projections_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    runs: &[SessionRun],
    steps: &[RunStep],
    checkpoints: &[RunCheckpoint],
) -> Result<()> {
    for run in runs {
        if run.session_id != session_id {
            return Err(anyhow!("run {} belongs to a different session", run.run_id));
        }
        tx.execute(
            "INSERT INTO session_runs
             (run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
              department, objective, provider, model, status, attempt, resumed_from_run_id,
              resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
              worktree_head, plan_revision, last_event_seq, failure_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
             ON CONFLICT(run_id) DO UPDATE SET
                session_id = excluded.session_id,
                project_id = excluded.project_id,
                agent_id = excluded.agent_id,
                agent_name = excluded.agent_name,
                agent_role = excluded.agent_role,
                task_id = excluded.task_id,
                department = excluded.department,
                objective = excluded.objective,
                provider = excluded.provider,
                model = excluded.model,
                status = excluded.status,
                attempt = excluded.attempt,
                resumed_from_run_id = excluded.resumed_from_run_id,
                resume_root_id = excluded.resume_root_id,
                top_level = excluded.top_level,
                started_at = excluded.started_at,
                updated_at = excluded.updated_at,
                ended_at = excluded.ended_at,
                budget_json = excluded.budget_json,
                worktree_head = excluded.worktree_head,
                plan_revision = excluded.plan_revision,
                last_event_seq = excluded.last_event_seq,
                failure_json = excluded.failure_json",
            params![
                run.run_id,
                run.session_id,
                run.project_id,
                run.agent_id,
                run.agent_name,
                run.agent_role,
                run.task_id,
                run.department,
                run.objective,
                run.provider,
                run.model,
                run.status,
                i64::from(run.attempt),
                run.resumed_from_run_id,
                run.resume_root_id,
                if run.top_level { 1 } else { 0 },
                run.started_at,
                run.updated_at,
                run.ended_at,
                serde_json::to_string(&run.budget)?,
                run.worktree_head,
                i64::from(run.plan_revision),
                run.last_event_seq as i64,
                run.failure
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
            ],
        )?;
    }
    let run_ids = runs
        .iter()
        .map(|run| run.run_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut stmt = tx.prepare("SELECT run_id FROM session_runs WHERE session_id = ?1")?;
    let projected = stmt
        .query_map(params![session_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for run_id in projected {
        if run_ids.contains(&run_id) {
            continue;
        }
        let has_tools: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM run_tool_calls WHERE run_id = ?1)",
            params![run_id],
            |row| row.get(0),
        )?;
        if !has_tools {
            tx.execute(
                "DELETE FROM session_runs WHERE run_id = ?1",
                params![run_id],
            )?;
        }
    }
    tx.execute(
        "DELETE FROM run_steps WHERE session_id = ?1",
        params![session_id],
    )?;
    for step in steps {
        if step.session_id != session_id {
            return Err(anyhow!(
                "run step {} belongs to a different session",
                step.step_id
            ));
        }
        insert_run_step_tx(tx, step)?;
    }
    tx.execute(
        "DELETE FROM run_checkpoints WHERE session_id = ?1",
        params![session_id],
    )?;
    for checkpoint in checkpoints {
        if checkpoint.session_id != session_id {
            return Err(anyhow!(
                "run checkpoint {} belongs to a different session",
                checkpoint.checkpoint_id
            ));
        }
        tx.execute(
            "INSERT INTO run_checkpoints
             (checkpoint_id, run_id, session_id, seq, turn, message_cursor, event_cursor,
              budget_json, worktree_head, plan_revision, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                checkpoint.checkpoint_id,
                checkpoint.run_id,
                checkpoint.session_id,
                checkpoint.seq as i64,
                i64::from(checkpoint.turn),
                checkpoint.message_cursor as i64,
                checkpoint.event_cursor as i64,
                serde_json::to_string(&checkpoint.budget)?,
                checkpoint.worktree_head,
                i64::from(checkpoint.plan_revision),
                checkpoint.created_at
            ],
        )?;
    }
    Ok(())
}

fn next_event_seq_tx(tx: &Transaction<'_>, session_id: &str) -> Result<u64> {
    Ok(tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )?)
}

fn validate_run_status(status: &str, allowed: &[&str]) -> Result<()> {
    if !allowed.contains(&status) {
        return Err(anyhow!(
            "invalid run status {status}; expected {}",
            allowed.join(", ")
        ));
    }
    Ok(())
}

fn validate_step_status(status: &str, allowed: &[&str]) -> Result<()> {
    if !allowed.contains(&status) {
        return Err(anyhow!(
            "invalid run step status {status}; expected {}",
            allowed.join(", ")
        ));
    }
    Ok(())
}

fn insert_session_run_tx(tx: &Transaction<'_>, run: &SessionRun) -> Result<()> {
    tx.execute(
        "INSERT INTO session_runs
         (run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
          department, objective, provider, model, status, attempt, resumed_from_run_id,
          resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
          worktree_head, plan_revision, last_event_seq, failure_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
        params![
            run.run_id,
            run.session_id,
            run.project_id,
            run.agent_id,
            run.agent_name,
            run.agent_role,
            run.task_id,
            run.department,
            run.objective,
            run.provider,
            run.model,
            run.status,
            i64::from(run.attempt),
            run.resumed_from_run_id,
            run.resume_root_id,
            if run.top_level { 1 } else { 0 },
            run.started_at,
            run.updated_at,
            run.ended_at,
            serde_json::to_string(&run.budget)?,
            run.worktree_head,
            i64::from(run.plan_revision),
            run.last_event_seq as i64,
            run.failure
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?
        ],
    )?;
    Ok(())
}

fn insert_run_step_tx(tx: &Transaction<'_>, step: &RunStep) -> Result<()> {
    tx.execute(
        "INSERT INTO run_steps
         (step_id, run_id, session_id, seq, turn, kind, status, input_hash, output_hash,
          payload_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            step.step_id,
            step.run_id,
            step.session_id,
            step.seq as i64,
            i64::from(step.turn),
            step.kind,
            step.status,
            step.input_hash,
            step.output_hash,
            serde_json::to_string(&step.payload)?,
            step.created_at,
            step.updated_at
        ],
    )?;
    Ok(())
}

fn insert_run_tool_call_tx(tx: &Transaction<'_>, tool: &RunToolCall) -> Result<()> {
    tx.execute(
        "INSERT INTO run_tool_calls
         (run_id, session_id, step_id, tool_call_id, tool_name, input_fingerprint, status,
          result_json, error, consumed_by_run_id, consumed_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            tool.run_id,
            tool.session_id,
            tool.step_id,
            tool.tool_call_id,
            tool.tool_name,
            tool.input_fingerprint,
            tool.status,
            tool.result
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            tool.error,
            tool.consumed_by_run_id,
            tool.consumed_at,
            tool.created_at,
            tool.updated_at
        ],
    )?;
    Ok(())
}

fn load_session_run_tx(tx: &Transaction<'_>, run_id: &str) -> Result<SessionRun> {
    tx.query_row(
        "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                department, objective, provider, model, status, attempt, resumed_from_run_id,
                resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                worktree_head, plan_revision, last_event_seq, failure_json
         FROM session_runs WHERE run_id = ?1",
        params![run_id],
        map_session_run,
    )
    .optional()?
    .ok_or_else(|| anyhow!("run not found: {run_id}"))
}

fn map_session_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRun> {
    let budget_json: String = row.get(19)?;
    let failure_json: Option<String> = row.get(23)?;
    Ok(SessionRun {
        run_id: row.get(0)?,
        session_id: row.get(1)?,
        project_id: row.get(2)?,
        agent_id: row.get(3)?,
        agent_name: row.get(4)?,
        agent_role: row.get(5)?,
        task_id: row.get(6)?,
        department: row.get(7)?,
        objective: row.get(8)?,
        provider: row.get(9)?,
        model: row.get(10)?,
        status: row.get(11)?,
        attempt: row.get::<_, i64>(12)?.max(1) as u32,
        resumed_from_run_id: row.get(13)?,
        resume_root_id: row.get(14)?,
        top_level: row.get::<_, i64>(15)? != 0,
        started_at: row.get(16)?,
        updated_at: row.get(17)?,
        ended_at: row.get(18)?,
        budget: serde_json::from_str::<RunBudgetSnapshot>(&budget_json).unwrap_or_default(),
        worktree_head: row.get(20)?,
        plan_revision: row.get::<_, i64>(21)?.max(0) as u32,
        last_event_seq: row.get::<_, i64>(22)?.max(0) as u64,
        failure: failure_json
            .and_then(|json| serde_json::from_str::<vp_protocol::types::RunFailure>(&json).ok()),
    })
}

fn map_run_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunStep> {
    let payload_json: String = row.get(9)?;
    Ok(RunStep {
        step_id: row.get(0)?,
        run_id: row.get(1)?,
        session_id: row.get(2)?,
        seq: row.get::<_, i64>(3)?.max(0) as u64,
        turn: row.get::<_, i64>(4)?.max(0) as u32,
        kind: row.get(5)?,
        status: row.get(6)?,
        input_hash: row.get(7)?,
        output_hash: row.get(8)?,
        payload: serde_json::from_str(&payload_json).unwrap_or(serde_json::Value::Null),
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn map_run_tool_call(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunToolCall> {
    let result_json: Option<String> = row.get(7)?;
    Ok(RunToolCall {
        run_id: row.get(0)?,
        session_id: row.get(1)?,
        step_id: row.get(2)?,
        tool_call_id: row.get(3)?,
        tool_name: row.get(4)?,
        input_fingerprint: row.get(5)?,
        status: row.get(6)?,
        result: result_json.and_then(|json| serde_json::from_str(&json).ok()),
        error: row.get(8)?,
        consumed_by_run_id: row.get(9)?,
        consumed_at: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn map_context_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContextCheckpoint> {
    let structured_state: Option<String> = row.get(15)?;
    Ok(ContextCheckpoint {
        id: row.get(0)?,
        session_id: row.get(1)?,
        project_id: row.get(2)?,
        label: row.get(3)?,
        mode: row.get(4)?,
        summary: row.get(5)?,
        through_message_id: row.get(6)?,
        message_cursor: row.get::<_, i64>(7)?.max(0) as usize,
        plan_revision: row.get::<_, i64>(8)?.max(0) as u32,
        provider: row.get(9)?,
        model: row.get(10)?,
        prompt_prefix_hash: row.get(11)?,
        worktree_head: row.get(12)?,
        tokens_before: row.get::<_, i64>(13)?.max(0) as u64,
        tokens_after: row.get::<_, i64>(14)?.max(0) as u64,
        structured_state: structured_state.and_then(|value| serde_json::from_str(&value).ok()),
        source_compaction_seq: row
            .get::<_, Option<i64>>(16)?
            .map(|value| value.max(0) as u64),
        automatic: row.get::<_, i64>(17)? != 0,
        created_at: row.get(18)?,
    })
}

fn insert_context_checkpoint_tx(
    tx: &Transaction<'_>,
    checkpoint: &ContextCheckpoint,
) -> Result<()> {
    let structured_state = checkpoint
        .structured_state
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    tx.execute(
        "INSERT INTO context_checkpoints
         (id, session_id, project_id, label, mode, summary, through_message_id,
          message_cursor, plan_revision, provider, model, prompt_prefix_hash,
          worktree_head, tokens_before, tokens_after, structured_state,
          source_compaction_seq, automatic, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19)",
        params![
            checkpoint.id,
            checkpoint.session_id,
            checkpoint.project_id,
            checkpoint.label,
            checkpoint.mode,
            checkpoint.summary,
            checkpoint.through_message_id,
            checkpoint.message_cursor as i64,
            i64::from(checkpoint.plan_revision),
            checkpoint.provider,
            checkpoint.model,
            checkpoint.prompt_prefix_hash,
            checkpoint.worktree_head,
            checkpoint.tokens_before as i64,
            checkpoint.tokens_after as i64,
            structured_state,
            checkpoint.source_compaction_seq.map(|value| value as i64),
            if checkpoint.automatic { 1 } else { 0 },
            checkpoint.created_at,
        ],
    )?;
    Ok(())
}

fn replace_context_checkpoints_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    checkpoints: &[ContextCheckpoint],
) -> Result<()> {
    tx.execute(
        "DELETE FROM context_checkpoints WHERE session_id = ?1",
        params![session_id],
    )?;
    for checkpoint in checkpoints {
        if checkpoint.session_id != session_id {
            return Err(anyhow!(
                "context checkpoint {} belongs to a different session",
                checkpoint.id
            ));
        }
        insert_context_checkpoint_tx(tx, checkpoint)?;
    }
    Ok(())
}

fn context_message_cursor_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    through_message_id: Option<&str>,
    fallback: usize,
) -> Result<usize> {
    let Some(through_message_id) = through_message_id else {
        return Ok(fallback);
    };
    let cursor = tx
        .query_row(
            "SELECT COUNT(*) FROM messages
             WHERE session_id = ?1
               AND rowid <= (SELECT rowid FROM messages WHERE session_id = ?1 AND id = ?2)",
            params![session_id, through_message_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    Ok(cursor.unwrap_or(fallback as i64).max(0) as usize)
}

fn payload_with_checkpoint_id(
    mut payload: serde_json::Value,
    checkpoint_id: &str,
) -> Result<serde_json::Value> {
    let object = payload
        .as_object_mut()
        .ok_or_else(|| anyhow!("context checkpoint payload must be an object"))?;
    object.insert("checkpoint_id".into(), serde_json::json!(checkpoint_id));
    Ok(payload)
}

fn validate_context_checkpoint_row(checkpoint: &ContextCheckpoint) -> Result<()> {
    if checkpoint.id.trim().is_empty()
        || checkpoint.session_id.trim().is_empty()
        || checkpoint.project_id.trim().is_empty()
    {
        return Err(anyhow!("context checkpoint row is missing required ids"));
    }
    if !matches!(checkpoint.mode.as_str(), "summary" | "trim" | "manual") {
        return Err(anyhow!(
            "unsupported context checkpoint mode: {}",
            checkpoint.mode
        ));
    }
    if checkpoint
        .summary
        .as_ref()
        .is_some_and(|summary| summary.len() > 128 * 1024)
    {
        return Err(anyhow!("context checkpoint summary exceeds 128 KiB"));
    }
    if let Some(state) = &checkpoint.structured_state {
        let encoded = serde_json::to_vec(state)?;
        if encoded.len() > 16 * 1024 {
            return Err(anyhow!(
                "context checkpoint structured state exceeds 16 KiB"
            ));
        }
    }
    Ok(())
}

fn map_run_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunCheckpoint> {
    let budget_json: String = row.get(7)?;
    Ok(RunCheckpoint {
        checkpoint_id: row.get(0)?,
        run_id: row.get(1)?,
        session_id: row.get(2)?,
        seq: row.get::<_, i64>(3)?.max(0) as u64,
        turn: row.get::<_, i64>(4)?.max(0) as u32,
        message_cursor: row.get::<_, i64>(5)?.max(0) as u64,
        event_cursor: row.get::<_, i64>(6)?.max(0) as u64,
        budget: serde_json::from_str::<RunBudgetSnapshot>(&budget_json).unwrap_or_default(),
        worktree_head: row.get(8)?,
        plan_revision: row.get::<_, i64>(9)?.max(0) as u32,
        created_at: row.get(10)?,
    })
}

fn insert_event_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    kind: &str,
    payload: serde_json::Value,
) -> Result<Event> {
    let seq = next_event_seq_tx(tx, session_id)?;
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

fn map_memory_semantic_candidate(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<MemorySemanticCandidate> {
    Ok(MemorySemanticCandidate {
        memory: map_memory_row(row)?,
        model: row.get(21)?,
        dimensions: u32::try_from(row.get::<_, i64>(22)?).unwrap_or(u32::MAX),
        content_hash: row.get(23)?,
        vector: row.get(24)?,
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
        embedding: decode_memory_embedding_policy(row.get::<_, Option<String>>(8)?),
    })
}

fn decode_memory_embedding_policy(value: Option<String>) -> Option<MemoryEmbeddingPolicy> {
    let value = value?;
    match serde_json::from_str::<MemoryEmbeddingPolicy>(&value) {
        Ok(policy) => Some(policy),
        Err(error) => {
            // A malformed policy must not silently enable network access. The
            // daemon rejects invalid policies at set time; this is defense in
            // depth for hand-edited databases.
            tracing::warn!(%error, "ignoring malformed project embedding policy");
            None
        }
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn configured_embedding_model_tx(tx: &Transaction<'_>, project_id: &str) -> Result<Option<String>> {
    let raw: Option<Option<String>> = tx
        .query_row(
            "SELECT embedding_json FROM project_memory_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(Some(raw)) = raw else {
        return Ok(None);
    };
    let Ok(policy) = serde_json::from_str::<MemoryEmbeddingPolicy>(&raw) else {
        return Ok(None);
    };
    if !policy.is_configured() {
        return Ok(None);
    }
    Ok(policy.model.filter(|model| !model.trim().is_empty()))
}

fn enqueue_memory_embedding_job_tx(tx: &Transaction<'_>, memory: &MemoryItem) -> Result<()> {
    if !matches!(memory.status.as_str(), "active" | "stale") {
        return Ok(());
    }
    let Some(model) = configured_embedding_model_tx(tx, &memory.project_id)? else {
        return Ok(());
    };
    tx.execute(
        "INSERT INTO memory_embedding_jobs
         (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
         VALUES (?1, ?2, 'pending', 0, NULL, NULL, ?3)
         ON CONFLICT(memory_id) DO UPDATE SET
            model = excluded.model,
            status = 'pending',
            attempts = 0,
            last_error = NULL,
            next_attempt_at = NULL,
            updated_at = excluded.updated_at",
        params![memory.id, model, now_rfc3339()],
    )?;
    Ok(())
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
    // Derived indexing is queued in the same transaction as the memory write.
    // A failure to enqueue is a real local error; the memory write rolls back
    // rather than silently missing its job. Embedding *calls* are never made
    // here and therefore can never fail the write.
    enqueue_memory_embedding_job_tx(tx, memory)?;
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
    if matches!(memory.status.as_str(), "archived" | "superseded") {
        tx.execute(
            "DELETE FROM memory_embeddings WHERE memory_id = ?1",
            params![&memory.id],
        )?;
        tx.execute(
            "DELETE FROM memory_embedding_jobs WHERE memory_id = ?1",
            params![&memory.id],
        )?;
        tx.execute(
            "DELETE FROM memory_embedding_assignments WHERE memory_id = ?1",
            params![&memory.id],
        )?;
    }
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
    let transport: String = row.get(9)?;
    let headers: String = row.get(12)?;
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
        transport: AcpTransport::parse(&transport).unwrap_or_default(),
        endpoint: row.get(10)?,
        auth_header_env: row.get(11)?,
        headers: serde_json::from_str(&headers).unwrap_or_default(),
        connect_timeout_secs: row.get::<_, i64>(13)?.max(1) as u64,
        allow_remote: row.get::<_, i64>(14)? != 0,
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

mod continuation_lease;
mod lease;

pub use continuation_lease::{
    ContinuationAckLostReason, ContinuationClaimToken, SessionContinuationAckError,
    SessionContinuationClaim, SessionContinuationClaimStatus,
};
pub use lease::{
    DaemonIdentity, LeaseLostReason, MAX_RUNTIME_LEASE_TTL_SECS, RuntimeLeaseAcquisition,
    RuntimeLeaseAuditEntry, RuntimeLeaseError, RuntimeLeaseState, RuntimeLeaseToken,
};

#[cfg(test)]
#[path = "store/tests.rs"]
mod tests;
