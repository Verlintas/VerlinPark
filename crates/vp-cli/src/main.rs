use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::{Parser, Subcommand, ValueEnum};
use fs2::FileExt;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use vp_protocol::discovery::Discovery;
use vp_protocol::rpc::RpcResponse;
use vp_store::paths;

#[derive(Debug, Parser)]
#[command(name = "vp", about = "VerlinPark CLI", version)]
struct Cli {
    /// Print raw JSON results instead of human-readable output.
    #[arg(long, global = true)]
    json: bool,
    /// Override the daemon data directory (must match the daemon's data dir).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum McpTransportArg {
    Stdio,
    Http,
    Sse,
}

impl McpTransportArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::Sse => "sse",
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check that the daemon is reachable.
    Ping,
    /// Show daemon information.
    Info,
    /// List configured external ACP agents.
    AcpAgents,
    /// Add an external Agent Client Protocol (ACP) agent.
    AcpAdd {
        name: String,
        command: String,
        #[arg(long = "arg")]
        args: Vec<String>,
        /// Environment override as KEY=VALUE (repeatable).
        #[arg(long = "env")]
        env: Vec<String>,
        /// Auto-approve this agent's own permission requests.
        #[arg(long)]
        auto_approve: bool,
        /// Per-delegation timeout in seconds.
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
    },
    /// Remove an ACP agent by id.
    AcpRemove { agent_id: String },
    /// Initialize an ACP agent and create a session without prompting.
    AcpProbe {
        agent: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// List configured MCP servers.
    McpServers,
    /// Add an MCP server (stdio by default, or HTTP/SSE with --transport).
    McpAdd {
        name: String,
        /// Stdio command. Omit for HTTP/SSE.
        command: Option<String>,
        #[arg(long, value_enum, default_value_t = McpTransportArg::Stdio)]
        transport: McpTransportArg,
        #[arg(long = "arg")]
        args: Vec<String>,
        /// Environment override as KEY=VALUE (repeatable, stdio only).
        #[arg(long = "env")]
        env: Vec<String>,
        /// HTTP/SSE endpoint URL.
        #[arg(long)]
        url: Option<String>,
        /// HTTP header as KEY=VALUE (repeatable; use ${ENV_VAR} for secrets).
        #[arg(long = "header")]
        headers: Vec<String>,
    },
    /// Update an MCP server's full configuration.
    McpUpdate {
        server_id: String,
        name: String,
        #[arg(long, value_enum)]
        transport: McpTransportArg,
        /// Stdio command. Omit for HTTP/SSE.
        command: Option<String>,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long = "header")]
        headers: Vec<String>,
        /// Explicitly enable or disable; omitted preserves the current value.
        #[arg(long)]
        enabled: Option<bool>,
    },
    /// Initialize an MCP server and count the tools/resources/prompts it exposes.
    McpProbe { server_id: String },
    /// Remove an MCP server by id.
    McpRemove { server_id: String },
    /// List tools exposed by an MCP server.
    McpTools { server: String },
    /// Call an MCP tool with a JSON arguments object.
    McpCall {
        server: String,
        tool: String,
        #[arg(long, default_value = "{}")]
        arguments: String,
    },
    /// List resources exposed by an MCP server.
    McpResources { server: String },
    /// Read one MCP resource by URI.
    McpReadResource { server: String, uri: String },
    /// List prompts exposed by an MCP server.
    McpPrompts { server: String },
    /// Render one MCP prompt with KEY=VALUE arguments.
    McpPrompt {
        server: String,
        name: String,
        #[arg(long = "arg")]
        args: Vec<String>,
    },
    /// Run local diagnostics.
    Doctor,
    /// Rebuild the metadata database from the project manifest and event mirrors.
    DbRebuild {
        /// Validate and report without moving the database.
        #[arg(long)]
        dry_run: bool,
        /// Proceed even if a daemon discovery file exists.
        #[arg(long)]
        force: bool,
    },
    /// Send a tiny live request to the configured provider.
    ProviderTest,
    /// List projects.
    Projects,
    /// Add a local repository as a project.
    ProjectAdd {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// List project skill packs.
    Skills {
        project: String,
        /// Include disabled skills.
        #[arg(long)]
        all: bool,
    },
    /// Create a project skill pack.
    SkillCreate {
        project: String,
        name: String,
        description: String,
        /// Instructions; use '-' to read from stdin.
        instructions: String,
        #[arg(long = "when", default_value = "")]
        when_to_use: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long = "tool-allow")]
        tool_allow: Vec<String>,
        #[arg(long = "tool-deny")]
        tool_deny: Vec<String>,
        #[arg(long = "profile")]
        profiles: Vec<String>,
        #[arg(long = "role")]
        roles: Vec<String>,
    },
    /// Import a SKILL.md file from the project root.
    SkillImport { project: String, path: String },
    /// List skill activations recorded for a session.
    SkillActivations { session: String },
    /// Enable or disable a project skill.
    SkillEnabled {
        id: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        enabled: Option<bool>,
    },
    /// Delete a project skill.
    SkillDelete { id: String },
    /// List indexed project knowledge sources.
    Sources {
        project: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one source and a page of its chunks.
    SourceGet {
        source_id: String,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Add a Markdown/text note to the project knowledge base.
    SourceNote {
        project: String,
        title: String,
        /// Note body; use '-' to read from stdin.
        content: String,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Index one UTF-8 text file from the project root.
    SourceAddFile {
        project: String,
        path: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Fetch a public URL and index its readable text.
    SourceAddUrl {
        project: String,
        url: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Index text-like files under the project root.
    SourceImport {
        project: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 100)]
        max_files: u32,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Re-chunk and re-index one source from its durable snapshot.
    SourceReindex { source_id: String },
    /// Re-read a file or URL source and replace its indexed content.
    SourceRefresh { source_id: String },
    /// Delete one source and its chunks.
    SourceDelete { source_id: String },
    /// List project source collections and aggregate counts.
    SourceCollections { project: String },
    /// Replace a source's title, collection, and labels.
    SourceUpdate {
        source_id: String,
        title: String,
        #[arg(long)]
        collection: Option<String>,
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Move a source into a collection; omit --collection to clear it.
    SourceMove {
        source_id: String,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Replace a source's labels.
    SourceLabels {
        source_id: String,
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Search source chunks with the same lexical retrieval used by agents.
    SourceSearch {
        project: String,
        query: String,
        #[arg(long = "collection")]
        collections: Vec<String>,
        #[arg(long = "label")]
        labels: Vec<String>,
        #[arg(long, default_value_t = 8)]
        limit: u32,
    },
    /// Show the project-wide shared budget and spend.
    ProjectBudget { project: String },
    /// Show token/cost usage grouped by session for a project.
    ProjectUsage { project: String },
    /// Set or clear the project-wide shared budget.
    ProjectBudgetSet {
        project: String,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
    },
    /// Remove old terminal session worktrees while keeping branches by default.
    CleanupWorktrees {
        project: String,
        #[arg(long, default_value_t = 7)]
        older_than_days: u64,
        /// Report what would be removed without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
        /// Remove worktrees with uncommitted changes.
        #[arg(long)]
        force: bool,
        /// Also delete the session branches.
        #[arg(long)]
        delete_branches: bool,
        /// Also remove failed/cancelled worktrees. Off by default.
        #[arg(long = "drop-failed")]
        drop_failed: bool,
    },
    /// Show the project's automatic task-resume policy.
    TaskScheduler { project: String },
    /// Update the project's automatic task-resume policy.
    TaskSchedulerSet {
        project: String,
        /// Enable or disable the autonomous scheduler.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        enabled: Option<bool>,
        /// Minimum seconds between scheduler selections (1–86400).
        #[arg(long)]
        cooldown_secs: Option<u64>,
        /// Dispatch one ready task per tick instead of resuming the whole plan.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        dispatch_tasks: Option<bool>,
        /// Include standalone task-inbox items.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        include_intake: Option<bool>,
        /// Maximum dispatching/running intake items per project (1–10).
        #[arg(long)]
        max_intake_parallel: Option<u32>,
    },
    /// Show the project's deterministic orchestration policy.
    OrchestrationPolicy { project: String },
    /// Update the project's orchestration policy.
    OrchestrationPolicySet {
        project: String,
        #[arg(long)]
        auto_retry: Option<bool>,
        #[arg(long)]
        max_attempts: Option<u32>,
        #[arg(long)]
        retry_backoff_secs: Option<u64>,
        #[arg(long)]
        retry_priority_boost: Option<i32>,
        #[arg(long)]
        dependency_failure: Option<String>,
        #[arg(long)]
        escalate_after_retries: Option<bool>,
        #[arg(long)]
        auto_resolve_blockers: Option<bool>,
    },
    /// Show the live orchestration snapshot for a session.
    Orchestration { session: String },
    /// Show a cross-session orchestration overview.
    OrchestrationOverview {
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Apply one deterministic controller pass (retry/block/unblock/escalate).
    Orchestrate {
        session: String,
        /// Show the decisions without changing any task.
        #[arg(long)]
        dry_run: bool,
    },
    /// Record a coordination note, blocker, decision, handoff, or escalation.
    Coordination {
        session: String,
        #[arg(long, default_value = "note")]
        action: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long, default_value = "info")]
        severity: String,
        message: String,
    },
    /// Resolve a coordination blocker for a task.
    Unblock {
        session: String,
        task: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long, default_value = "resolved from CLI")]
        message: String,
    },
    /// List the stored plan revisions for a session.
    PlanRevisions { session: String },
    /// Show one plan revision and its stored plan.
    PlanRevision { session: String, revision: u32 },
    /// Restore a previous plan revision as a new revision.
    PlanRestore {
        session: String,
        revision: u32,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        expected_revision: Option<u32>,
    },
    /// Edit one plan task through a validated plan revision.
    TaskEdit {
        session: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        expected_revision: Option<u32>,
    },
    /// Revise a stored TeamPlan from a JSON file (or stdin).
    TeamPlanRevise {
        session: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        expected_revision: Option<u32>,
    },
    /// List daemon-wide runtime leases (session runs and integration steps).
    Leases,
    /// Remove expired runtime leases left by a crashed process.
    LeasesSweep,
    /// Show the project's shell permission policy.
    ShellPolicy { project: String },
    /// Update the project's shell network/read-deny policy.
    ShellPolicySet {
        project: String,
        /// Network access for shell commands: allow or deny.
        #[arg(long, default_value = "allow")]
        network: String,
        /// Additional absolute (or ~/...) path to deny for reads.
        #[arg(long = "read-deny")]
        read_deny: Vec<String>,
        /// Deny reads under HOME except worktree/scratch/Git metadata/toolchains.
        #[arg(long)]
        restrict_home: bool,
        /// Deny sibling user homes and /Volumes outside the worktree/toolchains.
        #[arg(long)]
        read_jail: bool,
    },
    /// Show the cross-project Manage Dash snapshot and alerts.
    Dashboard {
        #[arg(long)]
        project: Option<String>,
    },
    /// List saved model presets.
    ModelPresets,
    /// Create a model preset (no API key is stored).
    ModelPresetCreate {
        name: String,
        #[arg(long, default_value = "openai-compatible")]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        context_window: Option<u64>,
        #[arg(long)]
        input_price: Option<f64>,
        #[arg(long)]
        output_price: Option<f64>,
        #[arg(long, default_value = "")]
        notes: String,
    },
    /// Update a model preset.
    ModelPresetUpdate {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "openai-compatible")]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        context_window: Option<u64>,
        #[arg(long)]
        input_price: Option<f64>,
        #[arg(long)]
        output_price: Option<f64>,
        #[arg(long, default_value = "")]
        notes: String,
    },
    /// Delete a model preset.
    ModelPresetDelete { id: String },
    /// Check the latest GitHub release without installing it.
    UpdateCheck {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Show the project's automatic checkpoint retention policy.
    CheckpointPolicy { project: String },
    /// Update the project's automatic checkpoint retention policy.
    CheckpointPolicySet {
        project: String,
        /// Enable the background sweep (omitting this disables it).
        #[arg(long)]
        enabled: bool,
        #[arg(long, default_value_t = 30)]
        older_than_days: u64,
        #[arg(long, default_value_t = 5)]
        keep_latest: u32,
    },
    /// Show the project's automatic worktree retention policy.
    WorktreePolicy { project: String },
    /// Update the project's automatic worktree retention policy.
    WorktreePolicySet {
        project: String,
        /// Enable the background sweep (omitting this disables it).
        #[arg(long)]
        enabled: bool,
        #[arg(long, default_value_t = 7)]
        older_than_days: u64,
        #[arg(long)]
        delete_branches: bool,
        /// Also remove failed/cancelled worktrees (default keeps them).
        #[arg(long)]
        drop_failed: bool,
    },
    /// Show the project's merge/integration policy.
    IntegrationPolicy { project: String },
    /// Update automatic conflict resolution and its caps.
    IntegrationPolicySet {
        project: String,
        /// Enable automatic conflict resolution for this project.
        #[arg(long)]
        auto_resolve: bool,
        /// Maximum automatic attempts per conflict entry (0-5).
        #[arg(long, default_value_t = 1)]
        max_auto_attempts: u32,
        /// Token cap for each automatic resolution (resolver + verifier).
        #[arg(long)]
        max_resolve_tokens: Option<u64>,
    },
    /// List sessions, optionally scoped to a project.
    Sessions {
        #[arg(long)]
        project: Option<String>,
    },
    /// Show scheduler occupancy, caps, and per-project policy.
    Scheduler {
        #[arg(long)]
        project: Option<String>,
    },
    /// List the persistent global task-DAG queue and write leases.
    Tasks {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// List standalone intake tasks (project task inbox).
    Intake {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Queue a standalone task that runs through a synthesized one-task plan.
    IntakeAdd {
        project: String,
        title: String,
        /// Task instructions; use '-' to read from stdin.
        instructions: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 0)]
        priority: i32,
        /// Writable path scope (repeatable).
        #[arg(long = "scope")]
        scope: Vec<String>,
        /// Acceptance criterion (repeatable).
        #[arg(long = "accept")]
        acceptance: Vec<String>,
        /// Intake task id that must complete first (repeatable).
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
        #[arg(long)]
        time_budget_secs: Option<u64>,
        #[arg(long, default_value_t = 2)]
        max_attempts: u32,
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Edit a queued/failed intake task and re-queue it.
    IntakeEdit {
        id: String,
        title: String,
        /// Task instructions; use '-' to read from stdin.
        instructions: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 0)]
        priority: i32,
        #[arg(long = "scope")]
        scope: Vec<String>,
        #[arg(long = "accept")]
        acceptance: Vec<String>,
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
        #[arg(long)]
        time_budget_secs: Option<u64>,
        #[arg(long, default_value_t = 2)]
        max_attempts: u32,
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Dispatch one intake task now, bypassing the schedule toggle.
    IntakeRun { id: String },
    /// Cancel a queued or running intake task.
    IntakeCancel { id: String },
    /// Reset a failed/cancelled intake task and queue it again.
    IntakeRetry { id: String },
    /// Delete a terminal intake task.
    IntakeDelete { id: String },
    /// Dispatch one ready task from a session's stored TeamPlan.
    TaskDispatch { session: String },
    /// Retry a failed, blocked, interrupted, cancelled, or skipped plan task.
    TaskRetry { session: String, task: String },
    /// Skip a queued plan task for the current plan revision.
    TaskSkip { session: String, task: String },
    /// Request cooperative cancellation of a running plan task.
    TaskCancel { session: String, task: String },
    /// Pause a running plan task at its next turn/tool boundary.
    TaskPause { session: String, task: String },
    /// Resume a paused plan task.
    TaskResume { session: String, task: String },
    /// Create a session for a project.
    SessionNew {
        project: String,
        goal: String,
        /// Session priority from -100 to 100; higher runs are admitted first.
        #[arg(long, default_value_t = 0)]
        priority: i32,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
        #[arg(long)]
        time_budget_secs: Option<u64>,
        /// Ignore session, plan, department, and project budget gates.
        #[arg(long)]
        unlimited: bool,
    },
    /// Enable or disable unlimited budget mode for a session.
    SessionUnlimited {
        session: String,
        /// Omit to enable; pass --enabled false to restore the stored limits.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        enabled: Option<bool>,
    },
    /// Run one command in the session console under the shell sandbox.
    Console { session: String, command: String },
    /// List files in a session worktree.
    Files { session: String },
    /// Show one session worktree file.
    File { session: String, path: String },
    /// Search file contents in a session worktree.
    Grep {
        session: String,
        query: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        case_sensitive: bool,
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// Run one command in a sandboxed PTY terminal.
    Terminal { session: String, command: String },
    /// Show recent commits on the session branch.
    History {
        session: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Show one commit's stat and patch.
    Show { session: String, commit: String },
    /// Write a session worktree file (content from --content or stdin).
    WriteFile {
        session: String,
        path: String,
        #[arg(long)]
        content: Option<String>,
    },
    /// Set a session priority from -100 to 100.
    SessionPriority { session: String, priority: i32 },
    /// Search messages and event payloads across sessions.
    Search {
        query: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: u32,
    },
    /// List events for a session.
    Events {
        session: String,
        #[arg(long, default_value_t = 0)]
        since: u64,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Send a follow-up prompt to the session Leader.
    Prompt { session: String, content: String },
    /// Ask the running Leader to stop.
    Cancel { session: String },
    /// Pause the running Leader between turns/tools.
    Pause { session: String },
    /// Resume a paused Leader.
    Resume { session: String },
    /// Resume the stored TeamPlan, skipping completed tasks.
    ResumePlan { session: String },
    /// Show estimated context usage and the last compaction/trim.
    Context { session: String },
    /// Summarize or trim an idle session's active conversation now.
    Compact { session: String },
    /// Create a committed, clean-worktree checkpoint of an idle session.
    Checkpoint {
        session: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// List checkpoints for a session.
    Checkpoints { session: String },
    /// Show approximate checkpoint storage stats for a session.
    CheckpointStats { session: String },
    /// Restore a session worktree branch to a checkpoint commit.
    CheckpointRestore { session: String, checkpoint: String },
    /// Delete one checkpoint and its snapshot ref.
    CheckpointDelete { session: String, checkpoint: String },
    /// Prune checkpoints older than --older-than-days while keeping the latest N.
    CheckpointPrune {
        session: String,
        #[arg(long)]
        older_than_days: Option<u32>,
        #[arg(long, default_value_t = 5)]
        keep_latest: u32,
        #[arg(long)]
        dry_run: bool,
    },
    /// Fork a new session from the latest checkpoint (or --checkpoint).
    Fork {
        session: String,
        #[arg(long)]
        checkpoint: Option<String>,
        #[arg(long)]
        goal: Option<String>,
    },
    /// Show parent/sibling/child fork lineage for a session.
    Lineage { session: String },
    /// Show the recursive fork tree rooted at a session.
    ForkTree { session: String },
    /// Show the whole project's fork forest and checkpoint count.
    ForkForest { project: String },
    /// Audit project fork/checkpoint integrity.
    ForkAudit { project: String },
    /// Repair missing fork links and checkpoint refs.
    ForkRepair { project: String },
    /// Compare two session branches in the same project.
    Compare { session: String, other: String },
    /// Merge a direct child fork branch into the source session.
    ForkMerge { session: String, fork: String },
    /// Show the session plan/todo list.
    Todos { session: String },
    /// Show the per-call usage/cost ledger.
    Usage {
        session: String,
        /// Emit CSV instead of the grouped human-readable ledger.
        #[arg(long)]
        csv: bool,
    },
    /// Show the validated team plan.
    TeamPlan { session: String },
    /// Print a Markdown transcript of the session's plan and messages.
    Transcript { session: String },
    /// Audit citation coverage and dangling markers for a session.
    CitationAudit { session: String },
    /// List source citations attached to assistant messages.
    Citations {
        session: String,
        #[arg(long)]
        message: Option<String>,
    },
    /// List the agents spawned for a session.
    Agents { session: String },
    /// List typed agent messages.
    Mailbox {
        session: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        unread: bool,
    },
    /// Mark a typed agent message as read.
    ReadMessage { message: String },
    /// List published artifacts.
    Artifacts { session: String },
    /// Backfill full publish events for legacy artifacts in a session.
    BackfillArtifacts { session: String },
    /// List project memories across sessions.
    Memory {
        project: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Add a project memory tied to a session.
    Remember {
        session: String,
        kind: String,
        title: String,
        body: String,
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// Delete one project memory.
    MemoryDelete { memory: String },
    /// Prune old or excess project memories.
    MemoryPrune {
        project: String,
        /// Delete memories older than this many days.
        #[arg(long)]
        days: Option<u32>,
        /// Keep at most this many newest memories.
        #[arg(long = "keep-latest")]
        keep_latest: Option<u32>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the project memory retention policy.
    MemoryPolicy { project: String },
    /// Update the opt-in project memory retention policy.
    MemoryPolicySet {
        project: String,
        /// Disable automatic memory pruning.
        #[arg(long)]
        disable: bool,
        /// Keep at most this many newest memories.
        #[arg(long = "max-items")]
        max_items: Option<u32>,
        /// Delete memories older than this many days.
        #[arg(long = "max-age-days")]
        max_age_days: Option<u32>,
        /// Disable the policy and clear both limits.
        #[arg(long)]
        clear: bool,
    },
    /// Show the project artifact retention policy.
    ArtifactPolicy { project: String },
    /// Update artifact retention and optional automatic GC.
    ArtifactPolicySet {
        project: String,
        #[arg(long)]
        auto_gc: bool,
        #[arg(long)]
        max_bytes: Option<u64>,
        #[arg(long)]
        max_age_secs: Option<u64>,
        #[arg(long)]
        max_count: Option<u64>,
    },
    /// Run artifact retention GC for a project.
    Gc { project: String },
    /// Show the project mailbox retention policy.
    MailboxPolicy { project: String },
    /// Update mailbox retention limits.
    MailboxPolicySet {
        project: String,
        #[arg(long)]
        max_age_secs: Option<u64>,
        #[arg(long)]
        max_count: Option<u64>,
        /// Also delete unread messages (default: read messages only).
        #[arg(long)]
        delete_unread: bool,
        /// Run mailbox GC after sending or marking a message read.
        #[arg(long)]
        auto_gc: bool,
    },
    /// Run mailbox retention GC for a project.
    MailboxGc { project: String },
    /// Read an artifact by id.
    Artifact { artifact: String },
    /// List saved versions of an inline artifact.
    ArtifactVersions { artifact: String },
    /// Restore an inline artifact version as a new version.
    ArtifactRestore { artifact: String, version: u32 },
    /// Compare two inline artifact versions, or a version with the current one.
    ArtifactDiff {
        artifact: String,
        from_version: u32,
        #[arg(long)]
        to_version: Option<u32>,
        #[arg(long, default_value_t = 3)]
        context: u32,
    },
    /// Show the session merge queue and Integrator results.
    Queue { session: String },
    /// Retry a conflicted or failed merge queue entry after rebasing it.
    Retry { entry: String },
    /// Resolve a merge conflict with an internal agent, verify, and merge.
    Resolve { entry: String },
    /// Land the session branch into the project default branch.
    Land { session: String },
    /// Push the session branch and open a GitHub pull request with gh.
    Pr { session: String },
    /// Export a portable session bundle (events + projections).
    Export {
        session: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Import an archived session bundle into the local daemon.
    Import { bundle: PathBuf },
    /// Recreate a worktree for an imported archive session.
    Rehydrate {
        session: String,
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Replay the event stream and check projection consistency.
    Replay {
        session: String,
        #[arg(long)]
        repair: bool,
        #[arg(long)]
        rebuild: bool,
    },
    /// Compare the JSONL event mirror with the database.
    LogStatus { session: String },
    /// Restore the event table from the JSONL mirror and rebuild projections.
    RestoreLog { session: String },
    /// Discard uncommitted changes in the session worktree.
    Discard { session: String },
    /// Commit the session worktree.
    Commit {
        session: String,
        #[arg(short, long)]
        message: String,
    },
    /// Show the session worktree diff.
    Diff { session: String },
    /// List approvals, optionally scoped to a session.
    Approvals {
        #[arg(long)]
        session: Option<String>,
    },
    /// List blocking user questions.
    Questions {
        #[arg(long)]
        session: Option<String>,
    },
    /// Answer a blocking user question.
    Answer { question: String, answer: String },
    /// Approve a pending tool call.
    Approve { approval: String },
    /// Deny a pending tool call.
    Deny { approval: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::DbRebuild { dry_run, force } = &cli.command {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        if data_dir.join("run/daemon.json").exists() && !force {
            bail!(
                "daemon discovery file exists at {}; stop the daemon or pass --force",
                data_dir.join("run/daemon.json").display()
            );
        }
        std::fs::create_dir_all(data_dir.join("run"))?;
        let lock_path = data_dir.join("run/daemon.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if lock_file.try_lock_exclusive().is_err() {
            bail!(
                "daemon lock is held at {}; stop the daemon before rebuilding",
                lock_path.display()
            );
        }
        let report = vp_core::rebuild_database(&data_dir, *dry_run).await?;
        let _ = lock_file.unlock();
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_human(&value);
        }
        return Ok(());
    }
    let discovery = read_discovery(cli.data_dir.as_deref())?;
    let value = match cli.command {
        Command::DbRebuild { .. } => unreachable!("db rebuild is handled before discovery"),
        Command::Ping => {
            let result = rpc(&discovery, "ping", json!({})).await?;
            if cli.json {
                print_json(&result)?;
            } else {
                println!(
                    "pong · v{} · protocol {} · up {}s",
                    result["version"].as_str().unwrap_or("?"),
                    result["protocol"].as_u64().unwrap_or(0),
                    result["uptime_secs"].as_u64().unwrap_or(0)
                );
            }
            return Ok(());
        }
        Command::Info => rpc(&discovery, "server.info", json!({})).await?,
        Command::AcpAgents => rpc(&discovery, "acp.agent.list", json!({})).await?,
        Command::AcpAdd {
            name,
            command,
            args,
            env,
            auto_approve,
            timeout,
        } => {
            let env = parse_env_pairs(&env)?;
            rpc(
                &discovery,
                "acp.agent.add",
                json!({
                    "name": name,
                    "command": command,
                    "args": args,
                    "env": env,
                    "enabled": true,
                    "auto_approve": auto_approve,
                    "timeout_secs": timeout
                }),
            )
            .await?
        }
        Command::AcpRemove { agent_id } => {
            rpc(
                &discovery,
                "acp.agent.remove",
                json!({"agent_id": agent_id}),
            )
            .await?
        }
        Command::AcpProbe { agent, project } => {
            rpc(
                &discovery,
                "acp.agent.probe",
                json!({"agent_id": agent, "project_id": project}),
            )
            .await?
        }
        Command::McpServers => rpc(&discovery, "mcp.server.list", json!({})).await?,
        Command::McpAdd {
            name,
            command,
            transport,
            args,
            env,
            url,
            headers,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_env_pairs(&headers)?;
            rpc(
                &discovery,
                "mcp.server.add",
                json!({
                    "name": name,
                    "transport": transport.as_str(),
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "url": url,
                    "headers": headers,
                }),
            )
            .await?
        }
        Command::McpUpdate {
            server_id,
            name,
            transport,
            command,
            args,
            env,
            url,
            headers,
            enabled,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_env_pairs(&headers)?;
            let enabled = match enabled {
                Some(enabled) => enabled,
                None => mcp_server_enabled(&discovery, &server_id).await?,
            };
            rpc(
                &discovery,
                "mcp.server.update",
                json!({
                    "id": server_id,
                    "name": name,
                    "transport": transport.as_str(),
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "url": url,
                    "headers": headers,
                    "enabled": enabled,
                }),
            )
            .await?
        }
        Command::McpProbe { server_id } => {
            rpc(
                &discovery,
                "mcp.server.probe",
                json!({"server_id": server_id}),
            )
            .await?
        }
        Command::McpRemove { server_id } => {
            rpc(
                &discovery,
                "mcp.server.remove",
                json!({"server_id": server_id}),
            )
            .await?
        }
        Command::McpTools { server } => {
            rpc(&discovery, "mcp.tools", json!({"server_id": server})).await?
        }
        Command::McpCall {
            server,
            tool,
            arguments,
        } => {
            let arguments: serde_json::Value =
                serde_json::from_str(&arguments).context("parsing --arguments as JSON")?;
            rpc(
                &discovery,
                "mcp.call",
                json!({"server_id": server, "tool": tool, "arguments": arguments}),
            )
            .await?
        }
        Command::McpResources { server } => {
            rpc(&discovery, "mcp.resources", json!({"server_id": server})).await?
        }
        Command::McpReadResource { server, uri } => {
            rpc(
                &discovery,
                "mcp.resource.read",
                json!({"server_id": server, "uri": uri}),
            )
            .await?
        }
        Command::McpPrompts { server } => {
            rpc(&discovery, "mcp.prompts", json!({"server_id": server})).await?
        }
        Command::McpPrompt { server, name, args } => {
            let arguments = parse_env_pairs(&args)?;
            rpc(
                &discovery,
                "mcp.prompt.get",
                json!({"server_id": server, "name": name, "arguments": arguments}),
            )
            .await?
        }
        Command::Doctor => rpc(&discovery, "server.doctor", json!({})).await?,
        Command::ProviderTest => rpc(&discovery, "agent.probe", json!({})).await?,
        Command::Projects => rpc(&discovery, "project.list", json!({})).await?,
        Command::ProjectAdd { path, name } => {
            let root = path
                .canonicalize()
                .with_context(|| format!("resolving {}", path.display()))?;
            rpc(
                &discovery,
                "project.add",
                json!({"root": root, "name": name}),
            )
            .await?
        }
        Command::ProjectBudget { project } => {
            rpc(
                &discovery,
                "project.budget.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Skills { project, all } => {
            rpc(
                &discovery,
                "skill.list",
                json!({"project_id": project, "include_disabled": all}),
            )
            .await?
        }
        Command::SkillCreate {
            project,
            name,
            description,
            instructions,
            when_to_use,
            version,
            tool_allow,
            tool_deny,
            profiles,
            roles,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading skill instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "skill.create",
                json!({
                    "project_id": project,
                    "name": name,
                    "description": description,
                    "when_to_use": when_to_use,
                    "version": version,
                    "instructions": instructions,
                    "tool_allow": tool_allow,
                    "tool_deny": tool_deny,
                    "profiles": profiles,
                    "roles": roles,
                }),
            )
            .await?
        }
        Command::SkillActivations { session } => {
            rpc(
                &discovery,
                "skill.activations",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::SkillImport { project, path } => {
            rpc(
                &discovery,
                "skill.import",
                json!({"project_id": project, "path": path}),
            )
            .await?
        }
        Command::SkillEnabled { id, enabled } => {
            rpc(
                &discovery,
                "skill.set_enabled",
                json!({"id": id, "enabled": enabled.unwrap_or(true)}),
            )
            .await?
        }
        Command::SkillDelete { id } => rpc(&discovery, "skill.delete", json!({"id": id})).await?,
        Command::Sources {
            project,
            query,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "source.list",
                json!({
                    "project_id": project,
                    "query": query,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::SourceGet {
            source_id,
            offset,
            limit,
        } => {
            rpc(
                &discovery,
                "source.get",
                json!({"source_id": source_id, "offset": offset, "limit": limit}),
            )
            .await?
        }
        Command::SourceNote {
            project,
            title,
            content,
            collection,
        } => {
            let content = if content == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading source note from stdin")?;
                buffer
            } else {
                content
            };
            rpc(
                &discovery,
                "source.add_note",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "title": title,
                    "content": content
                }),
            )
            .await?
        }
        Command::SourceAddFile {
            project,
            path,
            title,
            collection,
        } => {
            rpc(
                &discovery,
                "source.add_file",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "path": path,
                    "title": title
                }),
            )
            .await?
        }
        Command::SourceAddUrl {
            project,
            url,
            title,
            collection,
        } => {
            rpc(
                &discovery,
                "source.add_url",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "url": url,
                    "title": title
                }),
            )
            .await?
        }
        Command::SourceImport {
            project,
            path,
            max_files,
            collection,
        } => {
            rpc(
                &discovery,
                "source.import",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "path": path,
                    "max_files": max_files
                }),
            )
            .await?
        }
        Command::SourceRefresh { source_id } => {
            rpc(
                &discovery,
                "source.refresh",
                json!({"source_id": source_id}),
            )
            .await?
        }
        Command::SourceReindex { source_id } => {
            rpc(
                &discovery,
                "source.reindex",
                json!({"source_id": source_id}),
            )
            .await?
        }
        Command::SourceDelete { source_id } => {
            rpc(&discovery, "source.delete", json!({"source_id": source_id})).await?
        }
        Command::SourceUpdate {
            source_id,
            title,
            collection,
            labels,
        } => {
            rpc(
                &discovery,
                "source.update",
                json!({
                    "source_id": source_id,
                    "title": title,
                    "collection": collection,
                    "labels": labels
                }),
            )
            .await?
        }
        Command::SourceCollections { project } => {
            rpc(
                &discovery,
                "source.collections",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceMove {
            source_id,
            collection,
        } => {
            rpc(
                &discovery,
                "source.set_collection",
                json!({"source_id": source_id, "collection": collection}),
            )
            .await?
        }
        Command::SourceLabels { source_id, labels } => {
            rpc(
                &discovery,
                "source.set_labels",
                json!({"source_id": source_id, "labels": labels}),
            )
            .await?
        }
        Command::SourceSearch {
            project,
            query,
            collections,
            labels,
            limit,
        } => {
            rpc(
                &discovery,
                "source.search",
                json!({
                    "project_id": project,
                    "query": query,
                    "collections": collections,
                    "labels": labels,
                    "limit": limit
                }),
            )
            .await?
        }
        Command::ProjectUsage { project } => {
            rpc(&discovery, "project.usage", json!({"project_id": project})).await?
        }
        Command::ProjectBudgetSet {
            project,
            token_budget,
            cost_budget_usd,
        } => {
            rpc(
                &discovery,
                "project.budget.set",
                json!({
                    "project_id": project,
                    "budget": {
                        "token_budget": token_budget,
                        "cost_budget_usd": cost_budget_usd,
                    }
                }),
            )
            .await?
        }
        Command::CleanupWorktrees {
            project,
            older_than_days,
            dry_run,
            force,
            delete_branches,
            drop_failed,
        } => {
            rpc(
                &discovery,
                "project.cleanup_worktrees",
                json!({
                    "project_id": project,
                    "older_than_secs": older_than_days.saturating_mul(86_400),
                    "dry_run": dry_run,
                    "force": force,
                    "delete_branches": delete_branches,
                    "keep_failed": !drop_failed,
                }),
            )
            .await?
        }
        Command::TaskScheduler { project } => {
            rpc(
                &discovery,
                "project.task_scheduler.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::TaskSchedulerSet {
            project,
            enabled,
            cooldown_secs,
            dispatch_tasks,
            include_intake,
            max_intake_parallel,
        } => {
            // Merge with the stored policy so a one-field CLI edit does not
            // silently reset intake or per-task behavior to defaults.
            let current = rpc(
                &discovery,
                "project.task_scheduler.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            rpc(
                &discovery,
                "project.task_scheduler.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled
                            .unwrap_or_else(|| policy["enabled"].as_bool().unwrap_or(true)),
                        "cooldown_secs": cooldown_secs
                            .unwrap_or_else(|| policy["cooldown_secs"].as_u64().unwrap_or(60)),
                        "dispatch_tasks": dispatch_tasks
                            .unwrap_or_else(|| policy["dispatch_tasks"].as_bool().unwrap_or(false)),
                        "include_intake": include_intake
                            .unwrap_or_else(|| policy["include_intake"].as_bool().unwrap_or(true)),
                        "max_intake_parallel": max_intake_parallel
                            .unwrap_or_else(|| {
                                policy["max_intake_parallel"].as_u64().unwrap_or(2) as u32
                            }),
                    }
                }),
            )
            .await?
        }
        Command::OrchestrationPolicy { project } => {
            rpc(
                &discovery,
                "orchestration.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::OrchestrationPolicySet {
            project,
            auto_retry,
            max_attempts,
            retry_backoff_secs,
            retry_priority_boost,
            dependency_failure,
            escalate_after_retries,
            auto_resolve_blockers,
        } => {
            let current = rpc(
                &discovery,
                "orchestration.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(value) = auto_retry {
                policy["auto_retry_failed"] = json!(value);
            }
            if let Some(value) = max_attempts {
                policy["max_attempts"] = json!(value);
            }
            if let Some(value) = retry_backoff_secs {
                policy["retry_backoff_secs"] = json!(value);
            }
            if let Some(value) = retry_priority_boost {
                policy["retry_priority_boost"] = json!(value);
            }
            if let Some(value) = dependency_failure {
                policy["dependency_failure"] = json!(value);
            }
            if let Some(value) = escalate_after_retries {
                policy["escalate_after_retries"] = json!(value);
            }
            if let Some(value) = auto_resolve_blockers {
                policy["auto_resolve_blockers"] = json!(value);
            }
            rpc(
                &discovery,
                "orchestration.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Orchestration { session } => {
            rpc(
                &discovery,
                "orchestration.snapshot",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::PlanRevisions { session } => {
            rpc(
                &discovery,
                "team_plan.revisions",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::OrchestrationOverview { project, limit } => {
            let mut params = json!({"limit": limit});
            if let Some(project) = project {
                params["project_id"] = json!(project);
            }
            rpc(&discovery, "orchestration.overview", params).await?
        }
        Command::Orchestrate { session, dry_run } => {
            rpc(
                &discovery,
                if dry_run {
                    "orchestration.preview"
                } else {
                    "orchestration.advance"
                },
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Coordination {
            session,
            action,
            task,
            agent,
            note,
            severity,
            message,
        } => {
            rpc(
                &discovery,
                "coordination.note",
                json!({
                    "session_id": session,
                    "action": action,
                    "task_id": task,
                    "target_agent_id": agent,
                    "target_note_id": note,
                    "severity": severity,
                    "message": message,
                }),
            )
            .await?
        }
        Command::Unblock {
            session,
            task,
            note,
            message,
        } => {
            rpc(
                &discovery,
                "coordination.note",
                json!({
                    "session_id": session,
                    "action": "unblock",
                    "task_id": task,
                    "target_note_id": note,
                    "severity": "info",
                    "message": message,
                }),
            )
            .await?
        }
        Command::PlanRevision { session, revision } => {
            rpc(
                &discovery,
                "team_plan.revision",
                json!({"session_id": session, "revision": revision}),
            )
            .await?
        }
        Command::PlanRestore {
            session,
            revision,
            reason,
            expected_revision,
        } => {
            rpc(
                &discovery,
                "team_plan.restore",
                json!({
                    "session_id": session,
                    "revision": revision,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::TaskEdit {
            session,
            file,
            reason,
            expected_revision,
        } => {
            let raw = read_json_input(file.as_deref())?;
            let task: Value = serde_json::from_str(&raw).context("parsing task JSON")?;
            rpc(
                &discovery,
                "team_plan.task_edit",
                json!({
                    "session_id": session,
                    "task": task,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::TeamPlanRevise {
            session,
            reason,
            file,
            expected_revision,
        } => {
            let raw = read_json_input(file.as_deref())?;
            let plan: Value = serde_json::from_str(&raw).context("parsing plan JSON")?;
            rpc(
                &discovery,
                "team_plan.revise",
                json!({
                    "session_id": session,
                    "plan": plan,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::Leases => rpc(&discovery, "runtime.leases", json!({})).await?,
        Command::LeasesSweep => rpc(&discovery, "runtime.leases.sweep", json!({})).await?,
        Command::ShellPolicy { project } => {
            rpc(
                &discovery,
                "project.shell_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ShellPolicySet {
            project,
            network,
            read_deny,
            restrict_home,
            read_jail,
        } => {
            rpc(
                &discovery,
                "project.shell_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "network": network,
                        "read_deny": read_deny,
                        "restrict_home": restrict_home,
                        "read_jail": read_jail,
                    }
                }),
            )
            .await?
        }
        Command::Dashboard { project } => {
            rpc(
                &discovery,
                "dashboard.snapshot",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ModelPresets => rpc(&discovery, "model.preset.list", json!({})).await?,
        Command::ModelPresetCreate {
            name,
            provider,
            model,
            context_window,
            input_price,
            output_price,
            notes,
        } => {
            rpc(
                &discovery,
                "model.preset.create",
                json!({
                    "name": name,
                    "provider": provider,
                    "model": model,
                    "context_window": context_window,
                    "input_usd_per_million": input_price,
                    "output_usd_per_million": output_price,
                    "notes": notes
                }),
            )
            .await?
        }
        Command::ModelPresetUpdate {
            id,
            name,
            provider,
            model,
            context_window,
            input_price,
            output_price,
            notes,
        } => {
            rpc(
                &discovery,
                "model.preset.update",
                json!({
                    "id": id,
                    "name": name,
                    "provider": provider,
                    "model": model,
                    "context_window": context_window,
                    "input_usd_per_million": input_price,
                    "output_usd_per_million": output_price,
                    "notes": notes
                }),
            )
            .await?
        }
        Command::ModelPresetDelete { id } => {
            rpc(&discovery, "model.preset.delete", json!({"id": id})).await?
        }
        Command::UpdateCheck { repo } => {
            rpc(&discovery, "update.check", json!({"repo": repo})).await?
        }
        Command::CheckpointPolicy { project } => {
            rpc(
                &discovery,
                "project.checkpoint_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::CheckpointPolicySet {
            project,
            enabled,
            older_than_days,
            keep_latest,
        } => {
            let policy = json!({
                "enabled": enabled,
                "older_than_secs": older_than_days.saturating_mul(86_400),
                "keep_latest": keep_latest
            });
            rpc(
                &discovery,
                "project.checkpoint_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::WorktreePolicy { project } => {
            rpc(
                &discovery,
                "project.worktree_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::WorktreePolicySet {
            project,
            enabled,
            older_than_days,
            delete_branches,
            drop_failed,
        } => {
            let policy = json!({
                "enabled": enabled,
                "older_than_secs": older_than_days.saturating_mul(86_400),
                "delete_branches": delete_branches,
                "keep_failed": !drop_failed
            });
            rpc(
                &discovery,
                "project.worktree_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::IntegrationPolicy { project } => {
            rpc(
                &discovery,
                "project.integration_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::IntegrationPolicySet {
            project,
            auto_resolve,
            max_auto_attempts,
            max_resolve_tokens,
        } => {
            rpc(
                &discovery,
                "project.integration_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "auto_resolve": auto_resolve,
                        "max_auto_attempts": max_auto_attempts,
                        "max_resolve_tokens": max_resolve_tokens,
                    }
                }),
            )
            .await?
        }
        Command::Sessions { project } => {
            rpc(&discovery, "session.list", json!({"project_id": project})).await?
        }
        Command::Scheduler { project } => {
            rpc(
                &discovery,
                "scheduler.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Tasks { project, session } => {
            rpc(
                &discovery,
                "task.queue",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::Intake {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "intake.list",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::IntakeAdd {
            project,
            title,
            instructions,
            profile,
            kind,
            priority,
            scope,
            acceptance,
            depends_on,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            max_attempts,
            labels,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading intake instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "intake.create",
                json!({
                    "project_id": project,
                    "title": title,
                    "instructions": instructions,
                    "profile": profile,
                    "kind": kind,
                    "priority": priority,
                    "write_scope": scope,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                    "max_attempts": max_attempts,
                    "labels": labels,
                    "origin": "cli",
                }),
            )
            .await?
        }
        Command::IntakeEdit {
            id,
            title,
            instructions,
            profile,
            kind,
            priority,
            scope,
            acceptance,
            depends_on,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            max_attempts,
            labels,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading intake instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "intake.update",
                json!({
                    "id": id,
                    "title": title,
                    "instructions": instructions,
                    "profile": profile,
                    "kind": kind,
                    "priority": priority,
                    "write_scope": scope,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                    "max_attempts": max_attempts,
                    "labels": labels,
                }),
            )
            .await?
        }
        Command::IntakeRun { id } => rpc(&discovery, "intake.dispatch", json!({"id": id})).await?,
        Command::IntakeCancel { id } => rpc(&discovery, "intake.cancel", json!({"id": id})).await?,
        Command::IntakeRetry { id } => rpc(&discovery, "intake.retry", json!({"id": id})).await?,
        Command::IntakeDelete { id } => rpc(&discovery, "intake.delete", json!({"id": id})).await?,
        Command::TaskDispatch { session } => {
            rpc(&discovery, "task.dispatch", json!({"session_id": session})).await?
        }
        Command::TaskRetry { session, task } => {
            rpc(
                &discovery,
                "task.retry",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskSkip { session, task } => {
            rpc(
                &discovery,
                "task.skip",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskCancel { session, task } => {
            rpc(
                &discovery,
                "task.cancel",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskPause { session, task } => {
            rpc(
                &discovery,
                "task.pause",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskResume { session, task } => {
            rpc(
                &discovery,
                "task.resume",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::SessionNew {
            project,
            goal,
            priority,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
        } => {
            rpc(
                &discovery,
                "session.create",
                json!({
                    "project_id": project,
                    "goal": goal,
                    "priority": priority,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                    "unlimited": unlimited,
                }),
            )
            .await?
        }
        Command::SessionUnlimited { session, enabled } => {
            rpc(
                &discovery,
                "session.budget.set",
                json!({"session_id": session, "unlimited": enabled.unwrap_or(true)}),
            )
            .await?
        }
        Command::Console { session, command } => {
            let opened = rpc(&discovery, "console.open", json!({"session_id": session})).await?;
            let console_id = opened["console_id"]
                .as_str()
                .context("console.open returned no console_id")?
                .to_string();
            println!(
                "console ready in {}",
                opened["worktree"].as_str().unwrap_or("?")
            );
            rpc(
                &discovery,
                "console.write",
                json!({
                    "console_id": console_id,
                    "data": format!("{command}\nexit\n")
                }),
            )
            .await?;
            let mut after_seq = 0_u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                let result = rpc(
                    &discovery,
                    "console.read",
                    json!({"console_id": console_id, "after_seq": after_seq}),
                )
                .await?;
                if let Some(chunks) = result["chunks"].as_array() {
                    for chunk in chunks {
                        if let Some(text) = chunk["text"].as_str() {
                            print!("{text}");
                        }
                    }
                    let _ = std::io::stdout().flush();
                }
                after_seq = result["next_seq"].as_u64().unwrap_or(after_seq);
                let status = result["status"].as_str().unwrap_or("running");
                if status != "running" {
                    if let Some(exit_code) = result["exit_code"].as_i64() {
                        println!("[console {status}, exit {exit_code}]");
                    }
                    break;
                }
                if std::time::Instant::now() > deadline {
                    let _ = rpc(
                        &discovery,
                        "console.signal",
                        json!({"console_id": console_id, "signal": "kill"}),
                    )
                    .await;
                    eprintln!("console command timed out after 300s");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let _ = rpc(
                &discovery,
                "console.close",
                json!({"console_id": console_id}),
            )
            .await;
            json!({"console_streamed": true})
        }
        Command::Files { session } => {
            rpc(&discovery, "workspace.tree", json!({"session_id": session})).await?
        }
        Command::File { session, path } => {
            rpc(
                &discovery,
                "workspace.file",
                json!({"session_id": session, "path": path}),
            )
            .await?
        }
        Command::Grep {
            session,
            query,
            path,
            case_sensitive,
            limit,
        } => {
            rpc(
                &discovery,
                "workspace.grep",
                json!({
                    "session_id": session,
                    "query": query,
                    "path": path,
                    "case_sensitive": case_sensitive,
                    "max_results": limit,
                }),
            )
            .await?
        }
        Command::WriteFile {
            session,
            path,
            content,
        } => {
            let content = if let Some(content) = content {
                content
            } else {
                let mut content = String::new();
                std::io::stdin()
                    .read_to_string(&mut content)
                    .context("reading file content from stdin")?;
                content
            };
            rpc(
                &discovery,
                "workspace.write",
                json!({"session_id": session, "path": path, "content": content}),
            )
            .await?
        }
        Command::History { session, limit } => {
            rpc(
                &discovery,
                "git.log",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::Show { session, commit } => {
            rpc(
                &discovery,
                "git.show",
                json!({"session_id": session, "commit": commit}),
            )
            .await?
        }
        Command::SessionPriority { session, priority } => {
            rpc(
                &discovery,
                "session.priority.set",
                json!({"session_id": session, "priority": priority}),
            )
            .await?
        }
        Command::Terminal { session, command } => {
            let opened = rpc(
                &discovery,
                "terminal.open",
                json!({"session_id": session, "cols": 100, "rows": 30}),
            )
            .await?;
            let terminal_id = opened["terminal_id"]
                .as_str()
                .context("terminal.open returned no terminal_id")?
                .to_string();
            println!(
                "terminal ready in {}",
                opened["worktree"].as_str().unwrap_or("?")
            );
            let data = BASE64.encode(format!("{command}\nexit\n"));
            rpc(
                &discovery,
                "terminal.write",
                json!({"terminal_id": terminal_id, "data": data}),
            )
            .await?;
            let mut after_seq = 0_u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                let result = rpc(
                    &discovery,
                    "terminal.read",
                    json!({"terminal_id": terminal_id, "after_seq": after_seq}),
                )
                .await?;
                if let Some(chunks) = result["chunks"].as_array() {
                    for chunk in chunks {
                        if let Some(data) = chunk["data"].as_str()
                            && let Ok(bytes) = BASE64.decode(data)
                        {
                            print!("{}", String::from_utf8_lossy(&bytes));
                        }
                    }
                    let _ = std::io::stdout().flush();
                }
                after_seq = result["next_seq"].as_u64().unwrap_or(after_seq);
                let status = result["status"].as_str().unwrap_or("running");
                if status != "running" {
                    if let Some(exit_code) = result["exit_code"].as_i64() {
                        println!("[terminal {status}, exit {exit_code}]");
                    }
                    break;
                }
                if std::time::Instant::now() > deadline {
                    let _ = rpc(
                        &discovery,
                        "terminal.close",
                        json!({"terminal_id": terminal_id}),
                    )
                    .await;
                    eprintln!("terminal command timed out after 300s");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let _ = rpc(
                &discovery,
                "terminal.close",
                json!({"terminal_id": terminal_id}),
            )
            .await;
            json!({"terminal_streamed": true})
        }
        Command::Search {
            query,
            project,
            limit,
        } => {
            rpc(
                &discovery,
                "session.search",
                json!({"query": query, "project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::Events {
            session,
            since,
            limit,
        } => {
            rpc(
                &discovery,
                "event.list",
                json!({"session_id": session, "since_seq": since, "limit": limit}),
            )
            .await?
        }
        Command::Prompt { session, content } => {
            rpc(
                &discovery,
                "session.prompt",
                json!({"session_id": session, "content": content}),
            )
            .await?
        }
        Command::Cancel { session } => {
            rpc(&discovery, "session.cancel", json!({"session_id": session})).await?
        }
        Command::Pause { session } => {
            rpc(&discovery, "session.pause", json!({"session_id": session})).await?
        }
        Command::Resume { session } => {
            rpc(&discovery, "session.resume", json!({"session_id": session})).await?
        }
        Command::ResumePlan { session } => {
            rpc(
                &discovery,
                "session.plan_resume",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Context { session } => {
            rpc(
                &discovery,
                "session.context",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Compact { session } => {
            rpc(
                &discovery,
                "session.compact",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Checkpoint { session, label } => {
            rpc(
                &discovery,
                "session.checkpoint",
                json!({"session_id": session, "label": label}),
            )
            .await?
        }
        Command::Checkpoints { session } => {
            rpc(
                &discovery,
                "session.checkpoints",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CheckpointStats { session } => {
            rpc(
                &discovery,
                "session.checkpoint.stats",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CheckpointRestore {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.restore",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::CheckpointDelete {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.delete",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::CheckpointPrune {
            session,
            older_than_days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.prune",
                json!({
                    "session_id": session,
                    "older_than_days": older_than_days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::Fork {
            session,
            checkpoint,
            goal,
        } => {
            rpc(
                &discovery,
                "session.fork",
                json!({
                    "session_id": session,
                    "checkpoint_id": checkpoint,
                    "goal": goal
                }),
            )
            .await?
        }
        Command::Lineage { session } => {
            rpc(
                &discovery,
                "session.lineage",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ForkTree { session } => {
            rpc(
                &discovery,
                "session.fork.tree",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ForkForest { project } => {
            rpc(
                &discovery,
                "project.fork.forest",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ForkAudit { project } => {
            rpc(
                &discovery,
                "project.fork.audit",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ForkRepair { project } => {
            rpc(
                &discovery,
                "project.fork.repair",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Compare { session, other } => {
            rpc(
                &discovery,
                "session.compare",
                json!({"session_id": session, "other_session_id": other}),
            )
            .await?
        }
        Command::ForkMerge { session, fork } => {
            rpc(
                &discovery,
                "session.fork.merge",
                json!({"source_session_id": session, "fork_session_id": fork}),
            )
            .await?
        }
        Command::Todos { session } => {
            rpc(&discovery, "session.todos", json!({"session_id": session})).await?
        }
        Command::TeamPlan { session } => {
            rpc(&discovery, "team_plan.get", json!({"session_id": session})).await?
        }
        Command::CitationAudit { session } => {
            rpc(
                &discovery,
                "session.citation_audit",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Citations { session, message } => {
            rpc(
                &discovery,
                "session.citations",
                json!({"session_id": session, "message_id": message}),
            )
            .await?
        }
        Command::Transcript { session } => {
            rpc(
                &discovery,
                "session.transcript",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Usage { session, csv } => {
            let method = if csv {
                "session.usage_csv"
            } else {
                "session.usage"
            };
            rpc(&discovery, method, json!({"session_id": session})).await?
        }
        Command::Agents { session } => {
            rpc(&discovery, "agent.list", json!({"session_id": session})).await?
        }
        Command::Mailbox {
            session,
            agent,
            unread,
        } => {
            rpc(
                &discovery,
                "mailbox.list",
                json!({
                    "session_id": session,
                    "agent_id": agent,
                    "unread_only": unread,
                }),
            )
            .await?
        }
        Command::ReadMessage { message } => {
            rpc(&discovery, "mailbox.read", json!({"message_id": message})).await?
        }
        Command::Artifacts { session } => {
            rpc(&discovery, "artifact.list", json!({"session_id": session})).await?
        }
        Command::ArtifactVersions { artifact } => {
            rpc(
                &discovery,
                "artifact.versions",
                json!({"artifact_id": artifact}),
            )
            .await?
        }
        Command::ArtifactDiff {
            artifact,
            from_version,
            to_version,
            context,
        } => {
            rpc(
                &discovery,
                "artifact.diff",
                json!({
                    "artifact_id": artifact,
                    "from_version": from_version,
                    "to_version": to_version,
                    "context": context
                }),
            )
            .await?
        }
        Command::ArtifactRestore { artifact, version } => {
            rpc(
                &discovery,
                "artifact.restore",
                json!({"artifact_id": artifact, "version": version}),
            )
            .await?
        }
        Command::Artifact { artifact } => {
            let value = rpc(&discovery, "artifact.get", json!({"artifact_id": artifact})).await?;
            if value["artifact"]["storage"].as_str() == Some("file") {
                rpc(
                    &discovery,
                    "artifact.materialize",
                    json!({"artifact_id": artifact}),
                )
                .await?
            } else {
                value
            }
        }
        Command::BackfillArtifacts { session } => {
            rpc(
                &discovery,
                "artifact.backfill",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Memory {
            project,
            query,
            kind,
            limit,
        } => {
            rpc(
                &discovery,
                "memory.list",
                json!({
                    "project_id": project,
                    "query": query,
                    "kind": kind,
                    "limit": limit
                }),
            )
            .await?
        }
        Command::Remember {
            session,
            kind,
            title,
            body,
            tags,
        } => {
            rpc(
                &discovery,
                "memory.create",
                json!({
                    "session_id": session,
                    "kind": kind,
                    "title": title,
                    "body": body,
                    "tags": tags
                }),
            )
            .await?
        }
        Command::MemoryDelete { memory } => {
            rpc(&discovery, "memory.delete", json!({"id": memory})).await?
        }
        Command::MemoryPrune {
            project,
            days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "memory.prune",
                json!({
                    "project_id": project,
                    "older_than_days": days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::MemoryPolicy { project } => {
            rpc(
                &discovery,
                "memory.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryPolicySet {
            project,
            disable,
            max_items,
            max_age_days,
            clear,
        } => {
            let (max_items, max_age_days) = if clear {
                (None, None)
            } else if max_items.is_none() && max_age_days.is_none() {
                (Some(1000), Some(180))
            } else {
                (max_items, max_age_days)
            };
            rpc(
                &discovery,
                "memory.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": !clear && !disable,
                        "max_items": max_items,
                        "max_age_days": max_age_days
                    }
                }),
            )
            .await?
        }
        Command::ArtifactPolicy { project } => {
            rpc(
                &discovery,
                "artifact.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ArtifactPolicySet {
            project,
            auto_gc,
            max_bytes,
            max_age_secs,
            max_count,
        } => {
            rpc(
                &discovery,
                "artifact.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "auto_gc": auto_gc,
                        "max_bytes": max_bytes,
                        "max_age_secs": max_age_secs,
                        "max_count": max_count,
                    }
                }),
            )
            .await?
        }
        Command::Gc { project } => {
            rpc(&discovery, "artifact.gc", json!({"project_id": project})).await?
        }
        Command::MailboxPolicy { project } => {
            rpc(
                &discovery,
                "mailbox.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MailboxPolicySet {
            project,
            max_age_secs,
            max_count,
            delete_unread,
            auto_gc,
        } => {
            rpc(
                &discovery,
                "mailbox.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "max_age_secs": max_age_secs,
                        "max_count": max_count,
                        "delete_unread": delete_unread,
                        "auto_gc": auto_gc,
                    }
                }),
            )
            .await?
        }
        Command::MailboxGc { project } => {
            rpc(&discovery, "mailbox.gc", json!({"project_id": project})).await?
        }
        Command::Queue { session } => {
            rpc(
                &discovery,
                "merge_queue.list",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Retry { entry } => {
            rpc(&discovery, "merge_queue.retry", json!({"entry_id": entry})).await?
        }
        Command::Resolve { entry } => {
            rpc(
                &discovery,
                "merge_queue.resolve",
                json!({"entry_id": entry}),
            )
            .await?
        }
        Command::Land { session } => {
            rpc(&discovery, "session.land", json!({"session_id": session})).await?
        }
        Command::Pr { session } => {
            rpc(&discovery, "session.pr", json!({"session_id": session})).await?
        }
        Command::Export { session, output } => {
            let value = rpc(&discovery, "session.export", json!({"session_id": session})).await?;
            if let Some(path) = output {
                std::fs::write(&path, serde_json::to_string_pretty(&value)?)?;
                println!("exported {}", path.display());
                return Ok(());
            }
            value
        }
        Command::Import { bundle } => {
            let data =
                std::fs::read(&bundle).with_context(|| format!("reading {}", bundle.display()))?;
            let parsed: Value = serde_json::from_slice(&data).context("parsing session bundle")?;
            rpc(&discovery, "session.import", json!({"bundle": parsed})).await?
        }
        Command::Rehydrate { session, worktree } => {
            rpc(
                &discovery,
                "session.rehydrate",
                json!({"session_id": session, "worktree": worktree}),
            )
            .await?
        }
        Command::LogStatus { session } => {
            rpc(
                &discovery,
                "session.log_status",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::RestoreLog { session } => {
            rpc(
                &discovery,
                "session.restore_log",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Replay {
            session,
            repair,
            rebuild,
        } => {
            rpc(
                &discovery,
                "session.replay",
                json!({"session_id": session, "repair": repair, "rebuild": rebuild}),
            )
            .await?
        }
        Command::Discard { session } => {
            rpc(
                &discovery,
                "session.discard",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Commit { session, message } => {
            rpc(
                &discovery,
                "session.commit",
                json!({"session_id": session, "message": message}),
            )
            .await?
        }
        Command::Diff { session } => {
            rpc(&discovery, "session.diff", json!({"session_id": session})).await?
        }
        Command::Approvals { session } => {
            rpc(&discovery, "approval.list", json!({"session_id": session})).await?
        }
        Command::Questions { session } => {
            rpc(&discovery, "question.list", json!({"session_id": session})).await?
        }
        Command::Answer { question, answer } => {
            rpc(
                &discovery,
                "question.answer",
                json!({"question_id": question, "answer": answer}),
            )
            .await?
        }
        Command::Approve { approval } => {
            rpc(
                &discovery,
                "approval.resolve",
                json!({"approval_id": approval, "approved": true}),
            )
            .await?
        }
        Command::Deny { approval } => {
            rpc(
                &discovery,
                "approval.resolve",
                json!({"approval_id": approval, "approved": false}),
            )
            .await?
        }
    };

    if cli.json {
        print_json(&value)?;
    } else {
        print_human(&value);
    }
    Ok(())
}

fn read_discovery(data_dir: Option<&Path>) -> Result<Discovery> {
    let path = match data_dir {
        Some(dir) => dir.join("run/daemon.json"),
        None => paths::discovery_path(),
    };
    let data = std::fs::read(&path).with_context(|| {
        format!(
            "daemon discovery file not found at {}. Start verlinpadd first (just daemon).",
            path.display()
        )
    })?;
    let discovery: Discovery =
        serde_json::from_slice(&data).context("parsing daemon discovery file")?;
    Ok(discovery)
}

async fn rpc(discovery: &Discovery, method: &str, params: Value) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": "cli",
        "method": method,
        "params": params,
    });
    let response = reqwest::Client::new()
        .post(discovery.rpc_url())
        .bearer_auth(&discovery.token)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("connecting to {}", discovery.rpc_url()))?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        bail!("daemon returned HTTP {status}: {text}");
    }
    let response: RpcResponse = serde_json::from_str(&text)?;
    if let Some(error) = response.error {
        bail!("{} (code {})", error.message, error.code);
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn mcp_server_enabled(discovery: &Discovery, server_id: &str) -> Result<bool> {
    let value = rpc(discovery, "mcp.server.list", json!({})).await?;
    value
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server.get("id").and_then(Value::as_str) == Some(server_id))
        })
        .and_then(|server| server.get("enabled").and_then(Value::as_bool))
        .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))
}

fn parse_env_pairs(values: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut env = std::collections::BTreeMap::new();
    for value in values {
        let (key, val) = value
            .split_once('=')
            .with_context(|| format!("--env must be KEY=VALUE: {value}"))?;
        if key.trim().is_empty() {
            bail!("--env key must not be empty");
        }
        env.insert(key.trim().to_string(), val.to_string());
    }
    Ok(env)
}

fn read_json_input(file: Option<&std::path::Path>) -> Result<String> {
    match file {
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
        }
        None => {
            let mut buffer = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)?;
            Ok(buffer)
        }
    }
}

fn print_human(value: &Value) {
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("goal").is_none()
        && let Some(unlimited) = value.get("unlimited").and_then(Value::as_bool)
    {
        println!(
            "session {} budget mode: {}",
            session_id,
            if unlimited { "unlimited" } else { "limited" }
        );
        return;
    }
    if value.get("console_streamed").and_then(Value::as_bool) == Some(true)
        || value.get("terminal_streamed").and_then(Value::as_bool) == Some(true)
    {
        return;
    }
    if let Some(csv) = value.get("csv").and_then(Value::as_str) {
        print!("{csv}");
        return;
    }
    if let Some(markdown) = value.get("markdown").and_then(Value::as_str) {
        print!("{markdown}");
        return;
    }
    if let Some(actions) = value.get("actions").and_then(Value::as_array)
        && value.get("session_id").is_some()
        && value.get("snapshot").is_none()
    {
        if actions.is_empty() {
            println!("No orchestration action was needed.");
        } else {
            for action in actions {
                println!(
                    "{}  {}  {}  {}",
                    action["action"].as_str().unwrap_or("?"),
                    action["task_id"].as_str().unwrap_or("-"),
                    action["attempt"].as_u64().unwrap_or(0),
                    action["detail"].as_str().unwrap_or("")
                );
            }
        }
        return;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array)
        && let Some(totals) = value.get("totals")
    {
        println!(
            "{} sessions · {} running · {} attention · {} blocked tasks · {} failed · {} retrying · {} conflicts",
            totals["sessions"].as_u64().unwrap_or(0),
            totals["running"].as_u64().unwrap_or(0),
            totals["attention"].as_u64().unwrap_or(0),
            totals["blocked_tasks"].as_u64().unwrap_or(0),
            totals["failed_tasks"].as_u64().unwrap_or(0),
            totals["retrying_tasks"].as_u64().unwrap_or(0),
            totals["merge_conflicts"].as_u64().unwrap_or(0)
        );
        for session in sessions {
            println!(
                "  {}  [{}]  {}  r{}  tasks {}/{}  ready {}  blocked {}  failed {}",
                session["project_name"].as_str().unwrap_or("?"),
                session["status"].as_str().unwrap_or("?"),
                session["goal"].as_str().unwrap_or(""),
                session["plan_revision"].as_u64().unwrap_or(0),
                session["metrics"]["completed_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["total_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["ready_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["blocked_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["failed_tasks"].as_u64().unwrap_or(0)
            );
            if let Some(blocker) = session["blockers"]
                .as_array()
                .and_then(|values| values.first())
            {
                println!("      blocker: {}", blocker.as_str().unwrap_or(""));
            }
            if let Some(decision) = session["last_decision"].as_str() {
                println!("      last: {}", decision);
            }
        }
        return;
    }
    if let Some(snapshot) = value.get("snapshot")
        && snapshot.get("metrics").is_some()
    {
        let metrics = &snapshot["metrics"];
        println!(
            "session {}  status={}  plan={} rev={}",
            snapshot["session_id"].as_str().unwrap_or("?"),
            snapshot["status"].as_str().unwrap_or("?"),
            if snapshot["plan"].is_null() {
                "none"
            } else {
                "present"
            },
            snapshot["plan_revision"].as_u64().unwrap_or(0)
        );
        println!(
            "agents={} running={} queued={} failed={}",
            metrics["total_agents"].as_u64().unwrap_or(0),
            metrics["running_agents"].as_u64().unwrap_or(0),
            metrics["queued_agents"].as_u64().unwrap_or(0),
            metrics["failed_agents"].as_u64().unwrap_or(0)
        );
        println!(
            "tasks={} completed={} active={} ready={} blocked={} failed={} retrying={}",
            metrics["total_tasks"].as_u64().unwrap_or(0),
            metrics["completed_tasks"].as_u64().unwrap_or(0),
            metrics["active_tasks"].as_u64().unwrap_or(0),
            metrics["ready_tasks"].as_u64().unwrap_or(0),
            metrics["blocked_tasks"].as_u64().unwrap_or(0),
            metrics["failed_tasks"].as_u64().unwrap_or(0),
            metrics["retrying_tasks"].as_u64().unwrap_or(0)
        );
        for task in snapshot["tasks"].as_array().into_iter().flatten() {
            println!(
                "  {} [{}/{}] {} · attempts={} {}",
                task["task_id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["status"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or(""),
                task["attempts"].as_u64().unwrap_or(0),
                task["blocked_by"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        for blocker in snapshot["blockers"].as_array().into_iter().flatten() {
            println!(
                "  blocker {} · {}",
                blocker["kind"].as_str().unwrap_or("?"),
                blocker["message"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if let Some(revision) = value.get("revision")
        && let Some(plan) = value.get("plan")
    {
        println!(
            "r{}  {}  {}  tasks={}  {}",
            revision["revision"].as_u64().unwrap_or(0),
            revision["kind"].as_str().unwrap_or("?"),
            revision["author"].as_str().unwrap_or("?"),
            revision["task_count"].as_u64().unwrap_or(0),
            revision["reason"].as_str().unwrap_or("")
        );
        println!(
            "diff: +{}  -{}  ~{}",
            revision["added"].as_array().map(Vec::len).unwrap_or(0),
            revision["removed"].as_array().map(Vec::len).unwrap_or(0),
            revision["changed"].as_array().map(Vec::len).unwrap_or(0)
        );
        for task in plan["tasks"].as_array().into_iter().flatten() {
            println!(
                "  {} [{}] {}  deps={}",
                task["id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or(""),
                task["deps"]
                    .as_array()
                    .map(|deps| deps
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        return;
    }
    if let Some(revisions) = value.get("revisions").and_then(Value::as_array) {
        if revisions.is_empty() {
            println!("No plan revisions.");
        }
        for revision in revisions {
            println!(
                "r{}  {}  {}  tasks={}  +{} -{} ~{}  {}",
                revision["revision"].as_u64().unwrap_or(0),
                revision["kind"].as_str().unwrap_or("?"),
                revision["author"].as_str().unwrap_or("?"),
                revision["task_count"].as_u64().unwrap_or(0),
                revision["added"].as_array().map(Vec::len).unwrap_or(0),
                revision["removed"].as_array().map(Vec::len).unwrap_or(0),
                revision["changed"].as_array().map(Vec::len).unwrap_or(0),
                revision["reason"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
    {
        println!(
            "{}",
            serde_json::to_string_pretty(policy).unwrap_or_default()
        );
        return;
    }
    if let Some(leases) = value.get("leases").and_then(Value::as_array) {
        println!(
            "runtime {} · {} lease(s)",
            value
                .get("runtime_id")
                .and_then(|value| value.as_str())
                .unwrap_or("?"),
            leases.len()
        );
        for lease in leases {
            println!(
                "  {}  {}  {}  expires {}  owner {}",
                lease["kind"].as_str().unwrap_or("?"),
                lease["lease_key"].as_str().unwrap_or("?"),
                lease["session_id"].as_str().unwrap_or("-"),
                lease["expires_at"].as_str().unwrap_or("?"),
                lease["owner_id"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if value.get("removed").is_some() {
        println!(
            "Removed {} expired runtime lease(s).",
            value["removed"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(hits) = value.get("hits").and_then(Value::as_array)
        && value.get("query").is_some()
    {
        if hits.is_empty() {
            println!("No matches.");
            return;
        }
        for hit in hits {
            let source = hit["source"].as_str().unwrap_or("?");
            let detail = hit["role"]
                .as_str()
                .or_else(|| hit["kind"].as_str())
                .unwrap_or("");
            println!(
                "{} {:<18} {} · {}",
                source,
                detail,
                hit["session_goal"].as_str().unwrap_or("?"),
                hit["snippet"].as_str().unwrap_or("")
            );
            println!(
                "  {} · session {}",
                hit["created_at"].as_str().unwrap_or("?"),
                hit["session_id"].as_str().unwrap_or("?")
            );
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            println!("… more matches exist; increase --limit or narrow the query.");
        }
        return;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array)
        && value.get("project_id").is_some()
        && let Some(totals) = value.get("totals")
    {
        println!(
            "project {}  sessions={}  calls={}  in={}  out={}  cost=${:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            sessions.len(),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        );
        for session in sessions {
            println!(
                "  {:<10} calls={:<4} in={:<8} out={:<8} ${:.4}  {}",
                session["status"].as_str().unwrap_or("?"),
                session["calls"].as_u64().unwrap_or(0),
                session["input_tokens"].as_u64().unwrap_or(0),
                session["output_tokens"].as_u64().unwrap_or(0),
                session["cost_usd"].as_f64().unwrap_or(0.0),
                session["goal"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(tasks) = value.get("tasks").and_then(Value::as_array)
        && tasks
            .first()
            .is_none_or(|task| task.get("attempts").is_some() && task.get("instructions").is_some())
        && value.get("leases").is_none()
        && tasks
            .iter()
            .all(|task| task.get("attempts").is_some() && task.get("instructions").is_some())
    {
        if tasks.is_empty() {
            println!("No intake tasks.");
            return;
        }
        for task in tasks {
            println!(
                "{:<12} p{:<4} {:<10} {:<9} {}/{}  {}",
                task["status"].as_str().unwrap_or("?"),
                task["priority"].as_i64().unwrap_or(0),
                task["profile"].as_str().unwrap_or("?"),
                task["kind"].as_str().unwrap_or("?"),
                task["attempts"].as_u64().unwrap_or(0),
                task["max_attempts"].as_u64().unwrap_or(0),
                task["title"].as_str().unwrap_or("")
            );
            println!("  id={}", task["id"].as_str().unwrap_or("?"));
            if let Some(session) = task["session_id"].as_str() {
                println!("  session={session}");
            }
            if let Some(deps) = task["deps"].as_array()
                && !deps.is_empty()
            {
                println!(
                    "  depends on: {}",
                    deps.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(error) = task["last_error"].as_str() {
                println!("  last error: {error}");
            } else if let Some(summary) = task["result_summary"].as_str() {
                println!("  result: {summary}");
            }
        }
        return;
    }
    if let Some(task) = value.get("task")
        && task.get("attempts").is_some()
        && task.get("instructions").is_some()
    {
        println!(
            "{}  p{}  {}  {}/{}  {}",
            task["status"].as_str().unwrap_or("?"),
            task["priority"].as_i64().unwrap_or(0),
            task["profile"].as_str().unwrap_or("?"),
            task["attempts"].as_u64().unwrap_or(0),
            task["max_attempts"].as_u64().unwrap_or(0),
            task["title"].as_str().unwrap_or("")
        );
        println!("id={}", task["id"].as_str().unwrap_or("?"));
        if let Some(session) = task["session_id"].as_str() {
            println!("session={session}");
        }
        if let Some(error) = task["last_error"].as_str() {
            println!("last error: {error}");
        }
        return;
    }
    if let Some(activations) = value.get("activations").and_then(Value::as_array) {
        if activations.is_empty() {
            println!("No skill activations.");
            return;
        }
        for activation in activations {
            println!(
                "{} v{}  role={} profile={}  {}",
                activation["name"].as_str().unwrap_or("?"),
                activation["version"].as_str().unwrap_or(""),
                activation["role"].as_str().unwrap_or(""),
                activation["profile"].as_str().unwrap_or(""),
                activation["activated_at"].as_str().unwrap_or("")
            );
            if let Some(allow) = activation["tool_allow"].as_array()
                && !allow.is_empty()
            {
                println!(
                    "  allow: {}",
                    allow
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(deny) = activation["tool_deny"].as_array()
                && !deny.is_empty()
            {
                println!(
                    "  deny: {}",
                    deny.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        return;
    }
    if let Some(skills) = value.get("skills").and_then(Value::as_array) {
        if skills.is_empty() {
            println!("No skills.");
            return;
        }
        for skill in skills {
            println!(
                "{}  {} {}  [{}]  roles={} profiles={}",
                skill["enabled"].as_bool().unwrap_or(false),
                skill["name"].as_str().unwrap_or("?"),
                skill["version"].as_str().unwrap_or(""),
                skill["source"].as_str().unwrap_or("?"),
                skill["roles"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
                skill["profiles"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
            );
            println!("  id={}", skill["id"].as_str().unwrap_or("?"));
            if let Some(allow) = skill["tool_allow"].as_array()
                && !allow.is_empty()
            {
                println!(
                    "  allow: {}",
                    allow
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(deny) = skill["tool_deny"].as_array()
                && !deny.is_empty()
            {
                println!(
                    "  deny: {}",
                    deny.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        return;
    }
    if let Some(skill) = value.get("name").and_then(Value::as_str)
        && value.get("instructions").is_some()
        && value.get("slug").is_some()
    {
        println!(
            "{} {}  enabled={}  id={}",
            skill,
            value["version"].as_str().unwrap_or(""),
            value["enabled"].as_bool().unwrap_or(false),
            value["id"].as_str().unwrap_or("?")
        );
        return;
    }
    if let Some(collections) = value.get("collections").and_then(Value::as_array)
        && value.get("total_sources").is_some()
    {
        println!("{} sources", value["total_sources"].as_u64().unwrap_or(0));
        for collection in collections {
            println!(
                "{:<24} {:>4} sources {:>5} chunks {:>8} bytes {:>8} tokens  labels={}",
                collection["name"].as_str().unwrap_or("(uncollected)"),
                collection["sources"].as_u64().unwrap_or(0),
                collection["chunks"].as_u64().unwrap_or(0),
                collection["bytes"].as_u64().unwrap_or(0),
                collection["tokens"].as_u64().unwrap_or(0),
                collection["labels"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        return;
    }
    if let Some(sources) = value.get("sources").and_then(Value::as_array)
        && value.get("stats").is_some()
    {
        let stats = &value["stats"];
        println!(
            "{} sources · {} indexed · {} failed · {} chunks · {} bytes · {} tokens",
            stats["sources"].as_u64().unwrap_or(0),
            stats["indexed"].as_u64().unwrap_or(0),
            stats["failed"].as_u64().unwrap_or(0),
            stats["chunks"].as_u64().unwrap_or(0),
            stats["bytes"].as_u64().unwrap_or(0),
            stats["tokens"].as_u64().unwrap_or(0),
        );
        for source in sources {
            println!(
                "{:<10} {:<8} {:>5} chunks  {}  ({})",
                source["status"].as_str().unwrap_or("?"),
                source["kind"].as_str().unwrap_or("?"),
                source["chunk_count"].as_u64().unwrap_or(0),
                source["title"].as_str().unwrap_or(""),
                source["id"].as_str().unwrap_or("?"),
            );
        }
        return;
    }
    if let Some(source) = value.get("source")
        && let Some(chunks) = value.get("chunks").and_then(Value::as_array)
    {
        println!(
            "{} · {} · {} chunks · {}",
            source["kind"].as_str().unwrap_or("?"),
            source["status"].as_str().unwrap_or("?"),
            source["chunk_count"].as_u64().unwrap_or(0),
            source["title"].as_str().unwrap_or(""),
        );
        println!("id={}", source["id"].as_str().unwrap_or("?"));
        if let Some(uri) = source["uri"].as_str() {
            println!("uri={uri}");
        }
        for chunk in chunks {
            println!(
                "\n[{}] {}",
                chunk["chunk_no"].as_u64().unwrap_or(0),
                chunk["heading"].as_str().unwrap_or("untitled section")
            );
            println!(
                "{}",
                chunk["content"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1200)
                    .collect::<String>()
            );
        }
        return;
    }
    if let Some(hits) = value.get("hits").and_then(Value::as_array)
        && value.get("degraded").is_some()
    {
        if hits.is_empty() {
            println!("No source matches.");
            return;
        }
        for hit in hits {
            println!(
                "{} chunk {} · {:.2} · {}",
                hit["source_title"].as_str().unwrap_or("?"),
                hit["chunk_no"].as_u64().unwrap_or(0),
                hit["score"].as_f64().unwrap_or(0.0),
                hit["heading"].as_str().unwrap_or("")
            );
            println!(
                "  {}",
                hit["content"]
                    .as_str()
                    .unwrap_or("")
                    .replace('\n', " ")
                    .chars()
                    .take(240)
                    .collect::<String>()
            );
        }
        if value["degraded"].as_bool() == Some(true) {
            println!("(lexical fallback)");
        }
        return;
    }
    if let Some(imported) = value.get("imported").and_then(Value::as_array)
        && value.get("skipped").is_some()
    {
        println!(
            "imported {} · skipped {} · errors {}",
            imported.len(),
            value["skipped"].as_u64().unwrap_or(0),
            value["errors"].as_array().map(Vec::len).unwrap_or(0)
        );
        for source in imported {
            println!(
                "  {} ({})",
                source["title"].as_str().unwrap_or("?"),
                source["id"].as_str().unwrap_or("?")
            );
        }
        for error in value["errors"].as_array().into_iter().flatten() {
            println!(
                "  error {}: {}",
                error["path"].as_str().unwrap_or("?"),
                error["error"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(source) = value.get("title").and_then(Value::as_str)
        && value.get("chunk_count").is_some()
        && value.get("sha256").is_some()
    {
        println!(
            "{} · {} · {} chunks · {} · id={}",
            value["status"].as_str().unwrap_or("?"),
            source,
            value["chunk_count"].as_u64().unwrap_or(0),
            value["kind"].as_str().unwrap_or("?"),
            value["id"].as_str().unwrap_or("?")
        );
        return;
    }
    if let Some(matches) = value.get("matches").and_then(Value::as_array)
        && value.get("files_scanned").is_some()
    {
        if matches.is_empty() {
            println!("No matches.");
            return;
        }
        for item in matches {
            println!(
                "{}:{}:{}  {}",
                item["path"].as_str().unwrap_or("?"),
                item["line"].as_u64().unwrap_or(0),
                item["column"].as_u64().unwrap_or(0),
                item["preview"].as_str().unwrap_or("")
            );
        }
        println!(
            "{} matches across {} files{}",
            matches.len(),
            value["files_scanned"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool() == Some(true) {
                " (truncated)"
            } else {
                ""
            }
        );
        return;
    }
    if let Some(deleted) = value.get("deleted").and_then(Value::as_bool) {
        println!("{}", if deleted { "deleted." } else { "not deleted." });
        return;
    }
    if let Some(task) = value.get("task")
        && task.get("session_id").is_some()
        && task.get("task_id").is_some()
    {
        println!(
            "{} {} -> {}",
            task["session_id"].as_str().unwrap_or("?"),
            task["task_id"].as_str().unwrap_or("?"),
            task["status"].as_str().unwrap_or("?")
        );
        return;
    }
    if let Some(projects) = value.get("projects").and_then(Value::as_array)
        && value.get("global_session_cap").is_some()
    {
        println!(
            "sessions: active={} queued={} paused={} cap={}",
            value["active_sessions"].as_u64().unwrap_or(0),
            value["queued_sessions"].as_u64().unwrap_or(0),
            value["paused_sessions"].as_u64().unwrap_or(0),
            value["global_session_cap"].as_u64().unwrap_or(0)
        );
        println!(
            "workers: {}  labour: {}  plan tasks: pending={} running={}",
            value["global_worker_cap"].as_u64().unwrap_or(0),
            value["global_labour_cap"].as_u64().unwrap_or(0),
            value["pending_plan_tasks"].as_u64().unwrap_or(0),
            value["running_plan_tasks"].as_u64().unwrap_or(0)
        );
        println!(
            "intake: queued={} active={} failed={}",
            value["queued_intake"].as_u64().unwrap_or(0),
            value["active_intake"].as_u64().unwrap_or(0),
            value["failed_intake"].as_u64().unwrap_or(0)
        );
        for project in projects {
            println!(
                "{}  enabled={} intake={} cap={} cooldown={}s  sessions={} plan={}/{}  intake={}/{}/{}",
                project["project_name"].as_str().unwrap_or("?"),
                project["enabled"].as_bool().unwrap_or(false),
                project["include_intake"].as_bool().unwrap_or(false),
                project["max_intake_parallel"].as_u64().unwrap_or(0),
                project["cooldown_secs"].as_u64().unwrap_or(0),
                project["pending_sessions"].as_u64().unwrap_or(0),
                project["running_plan_tasks"].as_u64().unwrap_or(0),
                project["pending_plan_tasks"].as_u64().unwrap_or(0),
                project["active_intake"].as_u64().unwrap_or(0),
                project["queued_intake"].as_u64().unwrap_or(0),
                project["failed_intake"].as_u64().unwrap_or(0)
            );
        }
        return;
    }
    if let Some(tasks) = value.get("tasks").and_then(Value::as_array)
        && value.get("leases").is_some()
    {
        if tasks.is_empty() && value["leases"].as_array().is_none_or(Vec::is_empty) {
            println!("No scheduled tasks or write leases.");
            return;
        }
        for task in tasks {
            println!(
                "{:<10} {:<8} p{:<4} {:<12} {:<24} {}",
                task["status"].as_str().unwrap_or("?"),
                task["task_id"].as_str().unwrap_or("?"),
                task["priority"].as_i64().unwrap_or(0),
                task["department"].as_str().unwrap_or("?"),
                task["session_id"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or("")
            );
            if let Some(error) = task["last_error"].as_str() {
                println!("  last error: {error}");
            }
        }
        for lease in value["leases"].as_array().into_iter().flatten() {
            println!(
                "lease {:<14} {:<20} scopes={}",
                lease["agent_id"].as_str().unwrap_or("?"),
                lease["session_id"].as_str().unwrap_or("?"),
                lease["scopes"]
                    .as_array()
                    .map(|scopes| scopes
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        return;
    }
    if let Some(totals) = value.get("totals")
        && value.get("entries").is_some()
    {
        println!(
            "session {}  calls={}  in={}  out={}  cost=${:.4}",
            value["session_id"].as_str().unwrap_or("?"),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        );
        for (label, field) in [
            ("role", "by_role"),
            ("dept", "by_department"),
            ("task", "by_task"),
            ("agent", "by_agent"),
            ("model", "by_model"),
        ] {
            for bucket in totals[field].as_array().into_iter().flatten() {
                println!(
                    "  {label:5} {:<20} calls={:<3} in={:<7} out={:<7} ${:.4}",
                    bucket["key"].as_str().unwrap_or("?"),
                    bucket["calls"].as_u64().unwrap_or(0),
                    bucket["input_tokens"].as_u64().unwrap_or(0),
                    bucket["output_tokens"].as_u64().unwrap_or(0),
                    bucket["cost_usd"].as_f64().unwrap_or(0.0)
                );
            }
        }
        return;
    }
    if value.get("session_id").is_some()
        && value.get("worktree").is_some()
        && value.get("commit").is_some()
    {
        println!(
            "rehydrated {} at {} · branch {} · {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["worktree"].as_str().unwrap_or("?"),
            value["branch"].as_str().unwrap_or("?"),
            value["commit"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("session_id").is_some() && value.get("events").is_some() {
        println!(
            "imported session {} into project {} · events={} · status={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            value["events"].as_u64().unwrap_or(0),
            value["status"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("version").is_some() && value.get("exported_at").is_some() {
        println!(
            "session {}  events={}  agents={}  messages={}  queue={}",
            value["session"]["id"].as_str().unwrap_or("?"),
            value["events"].as_array().map_or(0, Vec::len),
            value["agents"].as_array().map_or(0, Vec::len),
            value["messages"].as_array().map_or(0, Vec::len),
            value["merge_queue"].as_array().map_or(0, Vec::len)
        );
        return;
    }
    if value.get("latency_ms").is_some() && value.get("provider").is_some() {
        println!(
            "provider {} / {}  {} ms  in={} out={}  {}",
            value["provider"].as_str().unwrap_or("?"),
            value["model"].as_str().unwrap_or("?"),
            value["latency_ms"].as_u64().unwrap_or(0),
            value["input_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0),
            value["text"].as_str().unwrap_or("")
        );
        return;
    }
    if let Some(checks) = value.get("checks").and_then(Value::as_array) {
        println!(
            "{}",
            if value["ok"].as_bool().unwrap_or(false) {
                "doctor: ok"
            } else {
                "doctor: errors"
            }
        );
        for check in checks {
            println!(
                "  {:<8} {:<18} {}",
                check["status"].as_str().unwrap_or("?"),
                check["name"].as_str().unwrap_or("?"),
                check["detail"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if value.get("budget").is_some() && value.get("spent_tokens").is_some() {
        println!(
            "project {}  token-budget={}  cost-budget={}  spent-tokens={}  spent-usd={:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            value["budget"]["token_budget"].as_u64().unwrap_or(0),
            value["budget"]["cost_budget_usd"].as_f64().unwrap_or(0.0),
            value["spent_tokens"].as_u64().unwrap_or(0),
            value["spent_cost_usd"].as_f64().unwrap_or(0.0)
        );
        return;
    }
    if value.get("scanned").is_some() && value.get("entries").is_some() {
        println!(
            "project {}  scanned={}  removed={}  skipped={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["scanned"].as_u64().unwrap_or(0),
            value["removed"].as_u64().unwrap_or(0),
            value["skipped"].as_u64().unwrap_or(0)
        );
        for entry in value["entries"].as_array().into_iter().flatten() {
            println!(
                "{}  {}  {}  {}",
                if entry["removed"].as_bool().unwrap_or(false) {
                    "removed"
                } else {
                    "skipped"
                },
                entry["session_id"].as_str().unwrap_or("?"),
                entry["branch"].as_str().unwrap_or("?"),
                entry["reason"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if value.get("artifact_id").is_some() && value.get("path").is_some() {
        println!(
            "materialized {} at {}",
            value["artifact_id"].as_str().unwrap_or("?"),
            value["path"].as_str().unwrap_or("?")
        );
        return;
    }
    if let Some(servers) = value.get("servers").and_then(Value::as_array) {
        for server in servers {
            let transport = server["transport"].as_str().unwrap_or("stdio");
            let target = server["url"]
                .as_str()
                .or_else(|| server["command"].as_str())
                .unwrap_or("?");
            println!(
                "{}  {}  {}  {}  enabled={}",
                server["id"].as_str().unwrap_or("?"),
                server["name"].as_str().unwrap_or("?"),
                transport,
                target,
                server["enabled"].as_bool().unwrap_or(false)
            );
        }
        return;
    }
    if let Some(server_name) = value.get("server_name").and_then(Value::as_str)
        && value.get("tools").is_some()
    {
        let name = value["name"].as_str().unwrap_or("?");
        let transport = value["transport"].as_str().unwrap_or("?");
        let version = value["server_version"].as_str().unwrap_or("");
        let protocol = value["protocol_version"].as_str().unwrap_or("?");
        let resources = value["resources"].as_u64().unwrap_or(0);
        let prompts = value["prompts"].as_u64().unwrap_or(0);
        let resource_note = if value["resources_supported"].as_bool().unwrap_or(false) {
            format!("{resources} resources")
        } else {
            "resources unsupported".to_string()
        };
        let prompt_note = if value["prompts_supported"].as_bool().unwrap_or(false) {
            format!("{prompts} prompts")
        } else {
            "prompts unsupported".to_string()
        };
        println!(
            "{name}  {transport}  {server_name} {version}  protocol={protocol}  {} tools  {resource_note}  {prompt_note}",
            value["tools"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(server) = value.get("server") {
        println!(
            "{}  {}  {}  {}",
            server["id"].as_str().unwrap_or("?"),
            server["name"].as_str().unwrap_or("?"),
            server["transport"].as_str().unwrap_or("stdio"),
            server["url"]
                .as_str()
                .or_else(|| server["command"].as_str())
                .unwrap_or("?")
        );
        return;
    }
    if let Some(resources) = value.get("resources").and_then(Value::as_array) {
        for resource in resources {
            println!(
                "{}  <{}>  {}",
                resource["name"].as_str().unwrap_or("?"),
                resource["uri"].as_str().unwrap_or("?"),
                resource["mime_type"].as_str().unwrap_or("")
            );
            if let Some(description) = resource["description"].as_str()
                && !description.is_empty()
            {
                println!("  {description}");
            }
        }
        if let Some(templates) = value.get("templates").and_then(Value::as_array) {
            for template in templates {
                println!(
                    "template {}  <{}>",
                    template["name"].as_str().unwrap_or("?"),
                    template["uri_template"].as_str().unwrap_or("?")
                );
            }
        }
        return;
    }
    if let Some(contents) = value.get("contents").and_then(Value::as_array) {
        for content in contents {
            if let Some(text) = content["text"].as_str() {
                println!("{}", text);
            } else if let Some(blob) = content["blob"].as_str() {
                println!(
                    "[binary {}: {} base64 chars]",
                    content["uri"].as_str().unwrap_or("?"),
                    blob.len()
                );
            }
        }
        return;
    }
    if let Some(prompts) = value.get("prompts").and_then(Value::as_array) {
        for prompt in prompts {
            println!(
                "{}  {}",
                prompt["name"].as_str().unwrap_or("?"),
                prompt["description"].as_str().unwrap_or("")
            );
            if let Some(arguments) = prompt["arguments"].as_array() {
                for argument in arguments {
                    println!(
                        "  {}{}",
                        argument["name"].as_str().unwrap_or("?"),
                        if argument["required"].as_bool().unwrap_or(false) {
                            " (required)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        return;
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array)
        && value.get("server_id").is_some()
    {
        if let Some(description) = value["description"].as_str()
            && !description.is_empty()
        {
            println!("{description}\n");
        }
        for message in messages {
            println!(
                "[{}] {}",
                message["role"].as_str().unwrap_or("user"),
                serde_json::to_string(&message["content"])
                    .unwrap_or_else(|_| message["content"].to_string())
            );
        }
        return;
    }
    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for tool in tools {
            println!(
                "{:<30} {}",
                tool["name"].as_str().unwrap_or("?"),
                tool["description"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if let Some(result) = value.get("result")
        && value.get("tool").is_some()
    {
        println!(
            "{}",
            serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
        );
        return;
    }
    if let Some(memory_id) = value.get("memory_id").and_then(Value::as_str) {
        println!(
            "memory {} removed={}",
            memory_id,
            value["removed"].as_bool().unwrap_or(false)
        );
        return;
    }
    if let Some(removed) = value.get("removed").and_then(Value::as_bool) {
        println!("MCP server removed={removed}");
        return;
    }
    if let Some(questions) = value.get("questions").and_then(Value::as_array) {
        for question in questions {
            println!(
                "[{}] {} · {} · {}",
                question["status"].as_str().unwrap_or("?"),
                question["question"].as_str().unwrap_or("?"),
                question["agent_name"].as_str().unwrap_or("?"),
                question["id"].as_str().unwrap_or("?")
            );
            if let Some(options) = question["options"].as_array()
                && !options.is_empty()
            {
                println!(
                    "  options: {}",
                    options
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
            }
            if let Some(answer) = question["answer"].as_str() {
                println!("  answer: {answer}");
            }
        }
        return;
    }
    if value.get("answered").is_some() && value.get("started").is_none() {
        println!(
            "question answered={}",
            value["answered"].as_bool().unwrap_or(false)
        );
        return;
    }
    if let Some(sha256) = value.get("sha256").and_then(Value::as_str)
        && value.get("size").is_some()
        && value.get("path").is_some()
        && value.get("content").is_none()
    {
        println!(
            "wrote {} ({} bytes, sha256 {})",
            value["path"].as_str().unwrap_or("?"),
            value["size"].as_u64().unwrap_or(0),
            &sha256[..sha256.len().min(12)]
        );
        return;
    }
    if value.get("attempts").is_some() && value.get("instructions").is_some() {
        println!(
            "{}  p{}  {}  {}/{}  {}",
            value["status"].as_str().unwrap_or("?"),
            value["priority"].as_i64().unwrap_or(0),
            value["profile"].as_str().unwrap_or("?"),
            value["attempts"].as_u64().unwrap_or(0),
            value["max_attempts"].as_u64().unwrap_or(0),
            value["title"].as_str().unwrap_or("")
        );
        println!("id={}", value["id"].as_str().unwrap_or("?"));
        if let Some(session) = value["session_id"].as_str() {
            println!("session={session}");
        }
        if let Some(error) = value["last_error"].as_str() {
            println!("last error: {error}");
        }
        return;
    }
    if let Some(priority) = value.get("priority").and_then(Value::as_i64)
        && value.get("session_id").is_some()
        && value.get("commits").is_none()
    {
        println!(
            "session {} priority={}",
            value["session_id"].as_str().unwrap_or("?"),
            priority
        );
        return;
    }
    if let Some(commits) = value.get("commits").and_then(Value::as_array) {
        for commit in commits {
            println!(
                "{}  {}  {}  {}",
                commit["short_hash"].as_str().unwrap_or("?"),
                commit["author_date"].as_str().unwrap_or("?"),
                commit["author"].as_str().unwrap_or("?"),
                commit["subject"].as_str().unwrap_or("?")
            );
            if let Some(refs) = commit["refs"].as_str()
                && !refs.is_empty()
            {
                println!("         {}", refs);
            }
        }
        return;
    }
    if let Some(output) = value.get("output").and_then(Value::as_str)
        && value.get("commit").is_some()
    {
        println!("{output}");
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        for entry in entries {
            println!(
                "{:<4} {:>10}  {}",
                entry["kind"].as_str().unwrap_or("?"),
                entry["size"].as_u64().unwrap_or(0),
                entry["path"].as_str().unwrap_or("?")
            );
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            println!("(truncated)");
        }
        return;
    }
    if let Some(content) = value.get("content").and_then(Value::as_str)
        && value.get("size").is_some()
        && value.get("binary").is_some()
    {
        print!("{content}");
        return;
    }
    if let Some(memories) = value.get("memories").and_then(Value::as_array) {
        for memory in memories {
            println!(
                "[{}] {} · {} · {}",
                memory["kind"].as_str().unwrap_or("note"),
                memory["title"].as_str().unwrap_or("?"),
                memory["agent_name"].as_str().unwrap_or("?"),
                memory["id"].as_str().unwrap_or("?")
            );
            println!("  {}", memory["body"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && policy.get("cooldown_secs").is_some()
    {
        println!(
            "project {}  task-scheduler enabled={} cooldown={}s",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(false),
            policy["cooldown_secs"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && policy.get("enabled").is_some()
    {
        println!(
            "memory policy enabled={} max_items={} max_age_days={}",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["max_items"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["max_age_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string())
        );
        return;
    }
    if let Some(candidates) = value.get("candidates").and_then(Value::as_u64) {
        println!(
            "memory prune: candidates={} removed={} dry_run={}",
            candidates,
            value["removed"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        );
        return;
    }
    if let Some(memory) = value.get("memory") {
        println!(
            "remembered [{}] {} · {}",
            memory["kind"].as_str().unwrap_or("note"),
            memory["title"].as_str().unwrap_or("?"),
            memory["id"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("dry_run").is_some() && value.get("projects").is_some() {
        println!(
            "db rebuild{}: projects={} sessions={} events={} backup={}",
            if value["dry_run"].as_bool().unwrap_or(false) {
                " (dry run)"
            } else {
                ""
            },
            value["projects"].as_u64().unwrap_or(0),
            value["sessions"].as_u64().unwrap_or(0),
            value["events"].as_u64().unwrap_or(0),
            value["backup"].as_str().unwrap_or("none")
        );
        for warning in value["skipped"].as_array().into_iter().flatten() {
            println!("  skipped: {}", warning.as_str().unwrap_or("?"));
        }
        return;
    }
    if let Some(started) = value.get("started").and_then(Value::as_bool) {
        println!(
            "session {}  plan-resume-started={}",
            value["session_id"].as_str().unwrap_or("?"),
            started
        );
        return;
    }
    if value.get("session_id").is_some() && value.get("backfilled").is_some() {
        println!(
            "session {}  scanned={}  backfilled={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["scanned"].as_u64().unwrap_or(0),
            value["backfilled"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(policy) = value.get("policy") {
        if policy.get("delete_unread").is_some() {
            println!(
                "project {}  mailbox-max-age={}  mailbox-max-count={}  delete-unread={}  auto-gc={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["max_age_secs"].as_u64().unwrap_or(0),
                policy["max_count"].as_u64().unwrap_or(0),
                policy["delete_unread"].as_bool().unwrap_or(false),
                policy["auto_gc"].as_bool().unwrap_or(false)
            );
            return;
        }
        if policy.get("auto_gc").is_some() {
            println!(
                "project {}  auto-gc={}  max-bytes={}  max-age={}  max-count={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["auto_gc"].as_bool().unwrap_or(false),
                policy["max_bytes"].as_u64().unwrap_or(0),
                policy["max_age_secs"].as_u64().unwrap_or(0),
                policy["max_count"].as_u64().unwrap_or(0)
            );
            return;
        }
        if policy.get("enabled").is_some() && policy.get("older_than_secs").is_some() {
            println!(
                "project {}  auto-cleanup={}  older-than={}d  delete-branches={}  keep-failed={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["enabled"].as_bool().unwrap_or(false),
                policy["older_than_secs"].as_u64().unwrap_or(0) / 86_400,
                policy["delete_branches"].as_bool().unwrap_or(false),
                policy["keep_failed"].as_bool().unwrap_or(true)
            );
            return;
        }
        if policy.get("auto_resolve").is_some() {
            println!(
                "project {}  auto-resolve={}  max-attempts={}  max-tokens={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["auto_resolve"].as_bool().unwrap_or(false),
                policy["max_auto_attempts"].as_u64().unwrap_or(0),
                policy["max_resolve_tokens"]
                    .as_u64()
                    .map_or_else(|| "session budget".to_string(), |value| value.to_string())
            );
            return;
        }
        println!(
            "project {}  network={}  restrict-home={}  read-jail={}",
            value["project_id"].as_str().unwrap_or("?"),
            policy["network"].as_str().unwrap_or("?"),
            policy["restrict_home"].as_bool().unwrap_or(false),
            policy["read_jail"].as_bool().unwrap_or(false)
        );
        let paths = policy["read_deny"].as_array();
        if paths.is_none_or(|paths| paths.is_empty()) {
            println!("read-deny: none");
        } else {
            for path in paths.into_iter().flatten() {
                println!("read-deny: {}", path.as_str().unwrap_or("?"));
            }
        }
        return;
    }
    if let Some(projects) = value.get("projects").and_then(Value::as_array) {
        if projects.is_empty() {
            println!("No projects. Add one with: vp project-add <path>");
        }
        for project in projects {
            println!(
                "{}  {}  [{}]",
                project["id"].as_str().unwrap_or("?"),
                project["name"].as_str().unwrap_or("?"),
                project["default_branch"].as_str().unwrap_or("?")
            );
            println!("  {}", project["root"].as_str().unwrap_or("?"));
        }
        return;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array) {
        if sessions.is_empty() {
            println!("No sessions.");
        }
        for session in sessions {
            println!(
                "{}  {}  [{}]",
                session["id"].as_str().unwrap_or("?"),
                session["goal"].as_str().unwrap_or("?"),
                session["status"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(plan) = value.get("plan").filter(|plan| !plan.is_null()) {
        println!("GOAL  {}", plan["goal"].as_str().unwrap_or("?"));
        for department in plan["departments"].as_array().into_iter().flatten() {
            println!(
                "  {:<16} manager {:<16} profile {}",
                department["name"].as_str().unwrap_or("?"),
                department["manager_title"].as_str().unwrap_or("?"),
                department["profile"].as_str().unwrap_or("?")
            );
        }
        for task in plan["tasks"].as_array().into_iter().flatten() {
            println!(
                "  task {:<12} {:<16} kind {}",
                task["id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["kind"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(entry) = value.get("entry").filter(|entry| !entry.is_null()) {
        println!(
            "{}  {}  {}",
            entry["status"].as_str().unwrap_or("?"),
            entry["agent_name"].as_str().unwrap_or("?"),
            entry["commit"].as_str().unwrap_or("?")
        );
        if let Some(detail) = entry["detail"].as_str() {
            println!("{detail}");
        }
        return;
    }
    if value.get("mirror_events").is_some() && value.get("database_events").is_some() {
        println!(
            "{}  mirror={} database={}  {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["mirror_events"].as_u64().unwrap_or(0),
            value["database_events"].as_u64().unwrap_or(0),
            value["detail"].as_str().unwrap_or("")
        );
        return;
    }
    if let Some(consistent) = value.get("consistent").and_then(Value::as_bool) {
        println!(
            "{}  events={}  status={} usage={} ledger={} mailbox={} artifacts={} approvals={} plan={} todos={} messages={} agents={} queue={}",
            if consistent { "consistent" } else { "MISMATCH" },
            value["event_count"].as_u64().unwrap_or(0),
            value["status_matches"].as_bool().unwrap_or(false),
            value["usage_matches"].as_bool().unwrap_or(false),
            value["ledger_matches"].as_bool().unwrap_or(false),
            value["mailbox_matches"].as_bool().unwrap_or(false),
            value["artifacts_matches"].as_bool().unwrap_or(false),
            value["approvals_matches"].as_bool().unwrap_or(false),
            value["plan_matches"].as_bool().unwrap_or(false),
            value["todos_matches"].as_bool().unwrap_or(false),
            value["messages_matches"].as_bool().unwrap_or(false),
            value["agents_matches"].as_bool().unwrap_or(false),
            value["queue_matches"].as_bool().unwrap_or(false)
        );
        if value["rebuilt"].as_bool().unwrap_or(false) {
            println!("rebuilt projections from the event stream.");
        } else if value["repaired"].as_bool().unwrap_or(false) {
            println!("repaired projections from the event stream.");
        }
        for mismatch in value["mismatches"].as_array().into_iter().flatten() {
            if let Some(mismatch) = mismatch.as_str() {
                println!("  - {mismatch}");
            }
        }
        return;
    }
    if let Some(url) = value.get("url").and_then(Value::as_str) {
        println!(
            "PR opened: {url}  ({} → {})",
            value["branch"].as_str().unwrap_or("?"),
            value["base_branch"].as_str().unwrap_or("?")
        );
        return;
    }
    if let Some(target) = value.get("target_branch").and_then(Value::as_str) {
        println!(
            "landed {} → {} at {}",
            value["branch"].as_str().unwrap_or("?"),
            target,
            value["commit"].as_str().unwrap_or("?")
        );
        if let Some(detail) = value["detail"].as_str() {
            println!("{detail}");
        }
        return;
    }
    if value.get("removed").is_some() && value.get("bytes_freed").is_none() {
        println!(
            "mailbox GC removed {} message(s)",
            value["removed"].as_u64().unwrap_or(0)
        );
        return;
    }
    if value.get("removed").is_some() && value.get("bytes_freed").is_some() {
        println!(
            "artifact GC removed {} artifact(s), freed {} bytes",
            value["removed"].as_u64().unwrap_or(0),
            value["bytes_freed"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        if messages.is_empty() {
            println!("Mailbox is empty.");
        }
        for message in messages {
            println!(
                "{}  {} → {}  {}  [{}]",
                message["id"].as_str().unwrap_or("?"),
                message["from_name"].as_str().unwrap_or("?"),
                message["to_name"].as_str().unwrap_or("?"),
                message["kind"].as_str().unwrap_or("?"),
                message["status"].as_str().unwrap_or("?")
            );
            if let Some(body) = message["body"].as_str() {
                println!("  {body}");
            }
        }
        return;
    }
    if let Some(message) = value.get("message") {
        println!(
            "{} → {}  {}  [{}]",
            message["from_name"].as_str().unwrap_or("?"),
            message["to_name"].as_str().unwrap_or("?"),
            message["kind"].as_str().unwrap_or("?"),
            message["status"].as_str().unwrap_or("?")
        );
        if let Some(body) = message["body"].as_str() {
            println!("{body}");
        }
        return;
    }
    if let Some(artifacts) = value.get("artifacts").and_then(Value::as_array) {
        if artifacts.is_empty() {
            println!("No artifacts.");
        }
        for artifact in artifacts {
            println!(
                "{}  {:<24} {:<10} {}",
                artifact["id"].as_str().unwrap_or("?"),
                artifact["name"].as_str().unwrap_or("?"),
                artifact["kind"].as_str().unwrap_or("?"),
                artifact["agent_name"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(audit) = value.get("audit")
        && audit.get("messages_with_citations").is_some()
    {
        println!(
            "{} messages with citations · {} without · {} citations · {} sources · dangling {} · unused {}",
            audit["messages_with_citations"].as_u64().unwrap_or(0),
            audit["messages_without_citations"].as_u64().unwrap_or(0),
            audit["total_citations"].as_u64().unwrap_or(0),
            audit["unique_sources"].as_u64().unwrap_or(0),
            audit["dangling_key_count"].as_u64().unwrap_or(0),
            audit["unused_citation_count"].as_u64().unwrap_or(0),
        );
        for message in audit["messages"].as_array().into_iter().flatten() {
            let dangling = message["dangling_keys"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            let unused = message["unused_keys"].as_array().map(Vec::len).unwrap_or(0);
            if dangling == 0 && unused == 0 {
                continue;
            }
            println!(
                "  {} · citations {} · dangling {} · unused {}",
                message["message_id"].as_str().unwrap_or("?"),
                message["citation_count"].as_u64().unwrap_or(0),
                message["dangling_keys"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
                message["unused_keys"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
            println!(
                "    {}",
                message["excerpt"]
                    .as_str()
                    .unwrap_or("")
                    .replace('\n', " ")
                    .chars()
                    .take(180)
                    .collect::<String>()
            );
        }
        return;
    }
    if let Some(citations) = value.get("citations").and_then(Value::as_array)
        && citations.iter().all(|item| item.get("citation").is_some())
    {
        if citations.is_empty() {
            println!("No citations.");
            return;
        }
        for item in citations {
            let citation = &item["citation"];
            println!(
                "[{}] {} · {} · chunk {} · {}",
                citation["key"].as_str().unwrap_or("?"),
                citation["source_title"].as_str().unwrap_or("?"),
                citation["heading"].as_str().unwrap_or(""),
                citation["chunk_no"].as_u64().unwrap_or(0),
                item["message_id"].as_str().unwrap_or("?"),
            );
            println!("  {}", citation["excerpt"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(diff) = value.get("diff") {
        println!(
            "{}  v{} -> v{}  +{} -{} unchanged {}",
            value["name"].as_str().unwrap_or("?"),
            value["from_version"].as_u64().unwrap_or(0),
            value["to_version"].as_u64().unwrap_or(0),
            diff["additions"].as_u64().unwrap_or(0),
            diff["deletions"].as_u64().unwrap_or(0),
            diff["unchanged"].as_u64().unwrap_or(0),
        );
        if diff["identical"].as_bool() == Some(true) {
            println!("identical");
            return;
        }
        for hunk in diff["hunks"].as_array().into_iter().flatten() {
            println!(
                "@@ -{},{} +{},{} @@",
                hunk["old_start"].as_u64().unwrap_or(0),
                hunk["old_lines"].as_u64().unwrap_or(0),
                hunk["new_start"].as_u64().unwrap_or(0),
                hunk["new_lines"].as_u64().unwrap_or(0)
            );
            for line in hunk["lines"].as_array().into_iter().flatten() {
                let prefix = match line["kind"].as_str() {
                    Some("add") => '+',
                    Some("delete") => '-',
                    _ => ' ',
                };
                let old = line["old_line"]
                    .as_u64()
                    .map(|value| format!("{value:>4}"))
                    .unwrap_or_else(|| "    ".to_string());
                let new = line["new_line"]
                    .as_u64()
                    .map(|value| format!("{value:>4}"))
                    .unwrap_or_else(|| "    ".to_string());
                println!(
                    "{old} {new} {prefix} {}",
                    line["text"].as_str().unwrap_or("")
                );
            }
        }
        if diff["truncated"].as_bool() == Some(true) {
            println!("(diff truncated by the line/edit budget)");
        }
        return;
    }
    if let Some(versions) = value.get("versions").and_then(Value::as_array) {
        if versions.is_empty() {
            println!("No saved versions.");
            return;
        }
        for version in versions {
            println!(
                "v{}  {}  {} bytes  {}",
                version["version"].as_u64().unwrap_or(0),
                version["name"].as_str().unwrap_or("?"),
                version["size_bytes"].as_u64().unwrap_or(0),
                version["created_at"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(artifact) = value.get("artifact") {
        println!(
            "{}  {}  [{}]  {}  {} bytes",
            artifact["name"].as_str().unwrap_or("?"),
            artifact["kind"].as_str().unwrap_or("?"),
            artifact["mime_type"].as_str().unwrap_or("?"),
            artifact["storage"].as_str().unwrap_or("?"),
            artifact["size_bytes"].as_u64().unwrap_or(0)
        );
        if artifact["storage"].as_str() == Some("file") {
            println!(
                "stored at {}",
                artifact["path"].as_str().unwrap_or("(path unavailable)")
            );
        } else {
            println!("{}", artifact["content"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        if entries.is_empty() {
            println!("Merge queue is empty.");
        }
        for entry in entries {
            println!(
                "{:<10} {:<10} {:<16} {}",
                entry["status"].as_str().unwrap_or("?"),
                entry["task_id"].as_str().unwrap_or("—"),
                entry["agent_name"].as_str().unwrap_or("?"),
                entry["commit"].as_str().unwrap_or("?")
            );
            if let Some(detail) = entry["detail"].as_str() {
                println!("  {detail}");
            }
        }
        return;
    }
    if let Some(agents) = value.get("agents").and_then(Value::as_array) {
        if agents.is_empty() {
            println!("No agents.");
        }
        for agent in agents {
            println!(
                "{:<8} {:<16} {:<10} {}",
                agent["role"].as_str().unwrap_or("?"),
                agent["name"].as_str().unwrap_or("?"),
                agent["status"].as_str().unwrap_or("?"),
                agent["id"].as_str().unwrap_or("?")
            );
            if let Some(title) = agent["title"].as_str() {
                println!("         title {title}");
            }
            if let Some(profile) = agent["profile"].as_str() {
                println!("         profile {profile}");
            }
            if let Some(branch) = agent["branch"].as_str() {
                println!("         branch {branch}");
            }
        }
        return;
    }
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        if items.is_empty() {
            println!("No plan yet.");
        }
        for (index, item) in items.iter().enumerate() {
            let status = item["status"].as_str().unwrap_or("?");
            let marker = match status {
                "completed" => "✓",
                "in_progress" => "•",
                _ => "○",
            };
            println!(
                "{marker} {}. {}  [{}]",
                index + 1,
                item["title"].as_str().unwrap_or("?"),
                status
            );
        }
        return;
    }
    if let Some(paused) = value.get("paused").and_then(Value::as_bool) {
        println!(
            "{}",
            if paused {
                "paused the active run."
            } else {
                "no active run to pause."
            }
        );
        return;
    }
    if let Some(resumed) = value.get("resumed").and_then(Value::as_bool) {
        println!(
            "{}",
            if resumed {
                "resumed the active run."
            } else {
                "no paused run to resume."
            }
        );
        return;
    }
    if let Some(cancelled) = value.get("cancelled").and_then(Value::as_bool) {
        println!(
            "{}",
            if cancelled {
                "cancellation requested."
            } else {
                "no active run for that session."
            }
        );
        return;
    }
    if value.get("discarded").and_then(Value::as_bool) == Some(true) {
        println!("discarded uncommitted changes.");
        return;
    }
    if let Some(commit) = value.get("commit").and_then(Value::as_str) {
        println!("committed {commit}");
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            println!("  {message}");
        }
        if let Some(summary) = value.get("summary").and_then(Value::as_str) {
            println!("{summary}");
        }
        return;
    }
    if value.get("patch").is_some() {
        if let Some(status) = value.get("status").and_then(Value::as_str)
            && !status.is_empty()
        {
            println!("{status}\n");
        }
        if let Some(patch) = value.get("patch").and_then(Value::as_str) {
            if patch.is_empty() {
                println!("No changes.");
            } else {
                println!("{patch}");
            }
        }
        return;
    }
    if let Some(approvals) = value.get("approvals").and_then(Value::as_array) {
        if approvals.is_empty() {
            println!("No approvals.");
        }
        for approval in approvals {
            println!(
                "{}  {}  [{}]",
                approval["id"].as_str().unwrap_or("?"),
                approval["tool"].as_str().unwrap_or("?"),
                approval["status"].as_str().unwrap_or("?")
            );
            println!("  {}", approval["input"]);
        }
        return;
    }
    if let Some(events) = value.get("events").and_then(Value::as_array) {
        if events.is_empty() {
            println!("No events.");
        }
        for event in events {
            println!(
                "#{:04}  {}  {}",
                event["seq"].as_u64().unwrap_or(0),
                event["created_at"].as_str().unwrap_or("?"),
                event["type"].as_str().unwrap_or("?")
            );
            if !event["payload"].is_null() {
                println!("      {}", event["payload"]);
            }
        }
        return;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}
