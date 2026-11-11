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
    /// Show daemon metrics as JSON, or Prometheus text with --prometheus.
    Metrics {
        /// Print Prometheus text exposition format instead of JSON.
        #[arg(long)]
        prometheus: bool,
    },
    /// Run deterministic cost-per-solved-task benchmarks (no daemon required).
    Bench {
        #[command(subcommand)]
        command: vp_bench::cli::BenchCommand,
    },
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
    /// Back up the local data directory to a checksummed ZIP archive.
    Backup {
        /// Destination ZIP path.
        #[arg(short, long)]
        output: PathBuf,
        /// Include content-addressed artifact blobs.
        #[arg(long)]
        include_artifacts: bool,
    },
    /// Restore a data-directory backup. The daemon must be stopped.
    Restore {
        /// Backup ZIP path.
        #[arg(short, long)]
        input: PathBuf,
        /// Replace an existing database, instance lock, or non-empty target.
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
    /// List project execution hosts (local and ssh).
    HostList,
    /// Set a project's execution host from an ExecutionHost JSON object.
    HostSet {
        project: String,
        /// JSON object, a path to a .json file, or '-' for stdin.
        #[arg(long)]
        config: String,
    },
    /// Reset a project to the local execution host.
    HostRemove { project: String },
    /// Connect and probe a project's remote helper.
    HostProbe {
        project: String,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Show cached remote workspace health.
    RemoteStatus { project: String },
    /// List Git worktrees on a project's remote host.
    RemoteWorktrees { project: String },
    /// Show the bounded remote operation ledger.
    RemoteLedger {
        project: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
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
        /// Stall recovery mode: observe, escalate, or retry.
        #[arg(long)]
        stall_recovery: Option<String>,
        /// Consecutive stall observations before recovery (1–10).
        #[arg(long)]
        stall_min_observations: Option<u32>,
        /// Maximum automatic recovery retries per task (0–5).
        #[arg(long)]
        stall_max_attempts: Option<u32>,
        /// Seconds to wait for cancellation and an idle session (0–86400).
        #[arg(long)]
        stall_grace_secs: Option<u64>,
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
    /// Show daemon-wide computer browser/GUI leases; optionally sweep or release.
    Computer {
        /// Remove expired computer leases.
        #[arg(long)]
        sweep: bool,
        /// Release a stuck lease by id (operator override).
        #[arg(long)]
        release: Option<String>,
        /// Report helper installation and Accessibility/Screen Recording state.
        #[arg(long)]
        status: bool,
    },
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
    /// List project programs and their milestone counts.
    Programs {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show a program, its milestones, usage, and recent program events.
    ProgramStatus {
        id: String,
        #[arg(long, default_value_t = 50)]
        events: u32,
    },
    /// Create a project program. Draft programs do not dispatch until activated.
    ProgramCreate {
        project: String,
        goal: String,
        /// Activate immediately. With no explicit policy this opts into dispatch.
        #[arg(long)]
        activate: bool,
        /// Explicitly enable autonomous milestone dispatch.
        #[arg(long)]
        auto_dispatch: bool,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
        #[arg(long, default_value_t = 2)]
        max_attempts: u32,
        #[arg(long, default_value_t = 60)]
        retry_backoff_secs: u64,
        #[arg(long, default_value_t = 2)]
        max_parallel: u32,
        /// block, cancel, or retry.
        #[arg(long, default_value = "block")]
        dependency_failure: String,
    },
    /// Update a program goal, policy, or aggregate budgets.
    ProgramUpdate {
        id: String,
        goal: String,
        #[arg(long)]
        auto_dispatch: bool,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
        #[arg(long, default_value_t = 2)]
        max_attempts: u32,
        #[arg(long, default_value_t = 60)]
        retry_backoff_secs: u64,
        #[arg(long, default_value_t = 2)]
        max_parallel: u32,
        #[arg(long, default_value = "block")]
        dependency_failure: String,
    },
    /// Pause new milestone dispatch; in-flight work continues.
    ProgramPause {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Resume a paused or draft program.
    ProgramResume { id: String },
    /// Cooperatively cancel all non-terminal milestones and fail the program.
    ProgramCancel {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Delete a draft or terminal program.
    ProgramDelete { id: String },
    /// Add a milestone to a program.
    ProgramMilestoneAdd {
        program: String,
        title: String,
        /// Milestone objective; use '-' to read from stdin.
        objective: String,
        #[arg(long)]
        key: Option<String>,
        #[arg(long = "accept")]
        acceptance: Vec<String>,
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        #[arg(long, default_value_t = 0)]
        priority: i32,
        #[arg(long, default_value_t = 2)]
        max_attempts: u32,
        #[arg(long, default_value = "coding")]
        profile: String,
        #[arg(long = "scope")]
        scope: Vec<String>,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
    },
    /// Reset a failed, blocked, or cancelled milestone for another attempt.
    ProgramMilestoneRetry { id: String },
    /// Skip a milestone (recorded as cancelled with a skipped event).
    ProgramMilestoneSkip {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Start one ready milestone now through the normal session path.
    ProgramRun { milestone: String },
    /// Run one deterministic program controller tick (may start ready work).
    ProgramTick {
        #[arg(long)]
        project: Option<String>,
    },
    /// Replay a program projection from its append-only event log.
    ProgramReplay {
        id: String,
        #[arg(long)]
        repair: bool,
    },
    /// List the persistent global task-DAG queue and write leases.
    Tasks {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// List durable agent runs for a session.
    Runs {
        session: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Show one durable run with steps, tool calls, and checkpoints.
    RunDetail { run: String },
    /// Explicitly resume an interrupted or failed top-level run.
    RunResume {
        session: String,
        /// Run id; omitted selects the newest resumable top-level run.
        #[arg(long)]
        run: Option<String>,
        /// Idempotency key for the resume RPC.
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// Cooperatively cancel an active run.
    RunCancel {
        session: String,
        #[arg(long)]
        run: Option<String>,
    },
    /// Show a project's durable-run resume policy.
    RunPolicy { project: String },
    /// Update a project's durable-run resume policy (unspecified fields keep
    /// their current values).
    RunPolicySet {
        project: String,
        /// Explicitly enable or disable auto resume.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        auto_resume: Option<bool>,
        #[arg(long)]
        max_attempts: Option<u32>,
        #[arg(long)]
        max_age_hours: Option<u32>,
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
    /// Send operator steering to a running agent (defaults to the Leader).
    Steer {
        session: String,
        text: String,
        /// Target a specific running agent id.
        #[arg(long)]
        agent: Option<String>,
        /// Require the target agent to be running this plan task.
        #[arg(long)]
        task: Option<String>,
        /// Idempotency key; retrying with the same key replays the first result.
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// List persisted steering messages for a session.
    Steering {
        session: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Cancel a pending steering message before delivery.
    SteerCancel { id: String },
    /// Defer a continuation prompt until watched plan tasks settle or a deadline.
    Defer {
        session: String,
        prompt: String,
        /// Plan task to watch. Repeat for more than one task.
        #[arg(long = "watch")]
        watch: Vec<String>,
        /// Wake when all watched tasks settle, or as soon as any one settles.
        #[arg(long, default_value = "all", value_parser = ["all", "any"])]
        watch_mode: String,
        /// Relative deadline in seconds.
        #[arg(long, conflicts_with = "at")]
        after_secs: Option<u64>,
        /// Absolute RFC3339 deadline.
        #[arg(long)]
        at: Option<String>,
        /// Repeat every N seconds after a successful delivery (60..=2592000).
        /// The first wake still needs a watch task or deadline.
        #[arg(long)]
        every_secs: Option<u64>,
        /// Maximum delivered runs in the recurrence series (2..=100, default 10).
        #[arg(long)]
        max_runs: Option<u32>,
        /// Idempotency key; retrying with the same key replays the first result.
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// List deferred continuations for a session.
    Continuations {
        session: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Cancel a pending or ready deferred continuation.
    ContinuationCancel { id: String },
    /// Show plan-task progress and stall observations for a session.
    TaskStalls { session: String },
    /// Show automatic stall recovery state for a session.
    Recoveries { session: String },
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
    Prompt {
        session: String,
        content: String,
        /// Idempotency key; retrying with the same key replays the first result.
        #[arg(long)]
        operation_id: Option<String>,
    },
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
    /// Show session cache-read telemetry and the prompt-prefix generation.
    Cache { session: String },
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
        /// Include stale memories when no exact status is requested.
        #[arg(long = "include-stale")]
        include_stale: bool,
        /// Filter by exact status (active, stale, superseded, archived).
        #[arg(long)]
        status: Option<String>,
    },
    /// Rank project memories with deterministic lexical/structural scoring.
    MemorySearch {
        project: String,
        query: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        department: Option<String>,
        #[arg(long = "path")]
        paths: Vec<String>,
        #[arg(long = "kind")]
        kinds: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long = "include-stale")]
        include_stale: bool,
    },
    /// Print the exact memory brief that would be injected for a session.
    MemoryBrief {
        session: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Update memory lifecycle metadata (title and body stay immutable).
    MemoryUpdate {
        memory: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        pin: bool,
        #[arg(long)]
        unpin: bool,
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// Verify memory evidence locally and update its status.
    MemoryVerify { memory: String },
    /// Mark expired active memories stale (never deletes).
    MemorySweep {
        project: String,
        #[arg(long)]
        days: Option<u32>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show memory counts by status, source, and kind.
    MemoryStats { project: String },
    /// List deterministic same-scope memory conflicts.
    MemoryConflicts {
        project: String,
        /// Filter by exact status (open or resolved).
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Detect new same-scope memory conflicts. Never changes memory.
    MemoryConflictScan { project: String },
    /// Resolve one memory conflict explicitly.
    MemoryConflictResolve {
        id: String,
        #[arg(long, value_parser = ["keep-a", "keep-b", "keep-both", "archive-both"])]
        action: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Show memory health metrics for a project.
    MemoryHealth { project: String },
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
    /// Push the session branch and open a pull request (GitHub gh, GitLab glab,
    /// or Gitea/Forgejo tea with VERLINPARK_PR_PROVIDER=gitea|forgejo).
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
    if let Command::Bench { command } = &cli.command {
        match vp_bench::cli::execute(command.clone(), cli.json).await {
            Ok(outcome) => {
                if cli.json {
                    if let Some(value) = outcome.json {
                        print_json(&value)?;
                    }
                } else {
                    print!("{}", outcome.human);
                }
                std::process::exit(outcome.exit_code);
            }
            Err(error) => {
                eprintln!("vp bench: {error:#}");
                std::process::exit(2);
            }
        };
    }
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
    if let Command::Backup {
        output,
        include_artifacts,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let report = vp_core::backup::create_backup(&data_dir, output, *include_artifacts)?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_human(&value);
        }
        return Ok(());
    }
    if let Command::Restore { input, force } = &cli.command {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let report = vp_core::backup::restore_backup(input, &data_dir, *force)?;
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
        Command::Backup { .. } => unreachable!("backup is handled before discovery"),
        Command::Restore { .. } => unreachable!("restore is handled before discovery"),
        Command::Bench { .. } => unreachable!("bench is handled before discovery"),
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
        Command::Metrics { prometheus } => {
            let result = rpc(&discovery, "daemon.metrics", json!({})).await?;
            if prometheus {
                let text = result
                    .get("prometheus")
                    .and_then(Value::as_str)
                    .context("daemon.metrics returned no Prometheus text")?;
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
            } else {
                print_json(result.get("metrics").unwrap_or(&result))?;
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
        Command::HostList => rpc(&discovery, "host.list", json!({})).await?,
        Command::HostSet { project, config } => {
            let text = if config == "-" {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                text
            } else if Path::new(&config).is_file() {
                std::fs::read_to_string(&config)
                    .with_context(|| format!("reading execution host config {config}"))?
            } else {
                config.clone()
            };
            let config: Value = serde_json::from_str(&text)
                .context("execution host config must be a JSON object")?;
            rpc(
                &discovery,
                "host.update",
                json!({"project_id": project, "config": config}),
            )
            .await?
        }
        Command::HostRemove { project } => {
            rpc(&discovery, "host.remove", json!({"project_id": project})).await?
        }
        Command::HostProbe { project, timeout } => {
            rpc(
                &discovery,
                "host.probe",
                json!({"project_id": project, "timeout_secs": timeout}),
            )
            .await?
        }
        Command::RemoteStatus { project } => {
            rpc(&discovery, "remote.status", json!({"project_id": project})).await?
        }
        Command::RemoteWorktrees { project } => {
            rpc(
                &discovery,
                "remote.worktree_list",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RemoteLedger { project, limit } => {
            rpc(
                &discovery,
                "remote.ledger",
                json!({"project_id": project, "limit": limit}),
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
            stall_recovery,
            stall_min_observations,
            stall_max_attempts,
            stall_grace_secs,
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
            if let Some(value) = stall_recovery {
                policy["stall_recovery"] = json!(value);
            }
            if let Some(value) = stall_min_observations {
                policy["stall_min_observations"] = json!(value);
            }
            if let Some(value) = stall_max_attempts {
                policy["stall_max_attempts"] = json!(value);
            }
            if let Some(value) = stall_grace_secs {
                policy["stall_grace_secs"] = json!(value);
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
        Command::Computer {
            sweep,
            release,
            status,
        } => {
            if status {
                rpc(&discovery, "computer.status", json!({})).await?
            } else if let Some(lease_id) = release {
                rpc(
                    &discovery,
                    "computer.leases.release",
                    json!({"lease_id": lease_id}),
                )
                .await?
            } else if sweep {
                rpc(&discovery, "computer.leases.sweep", json!({})).await?
            } else {
                rpc(&discovery, "computer.leases", json!({})).await?
            }
        }
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
        Command::Programs {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "program.list",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::ProgramStatus { id, events } => {
            rpc(
                &discovery,
                "program.status",
                json!({"program_id": id, "event_limit": events}),
            )
            .await?
        }
        Command::ProgramCreate {
            project,
            goal,
            activate,
            auto_dispatch,
            token_budget,
            cost_budget_usd,
            max_attempts,
            retry_backoff_secs,
            max_parallel,
            dependency_failure,
        } => {
            let dependency_failure = parse_dependency_failure(&dependency_failure)?;
            rpc(
                &discovery,
                "program.create",
                json!({
                    "project_id": project,
                    "goal": goal,
                    "activate": activate,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "policy": {
                        "auto_dispatch": auto_dispatch || activate,
                        "max_attempts": max_attempts,
                        "retry_backoff_secs": retry_backoff_secs,
                        "dependency_failure": dependency_failure,
                        "max_parallel_milestones": max_parallel,
                    },
                }),
            )
            .await?
        }
        Command::ProgramUpdate {
            id,
            goal,
            auto_dispatch,
            token_budget,
            cost_budget_usd,
            max_attempts,
            retry_backoff_secs,
            max_parallel,
            dependency_failure,
        } => {
            let dependency_failure = parse_dependency_failure(&dependency_failure)?;
            rpc(
                &discovery,
                "program.update",
                json!({
                    "program_id": id,
                    "goal": goal,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "policy": {
                        "auto_dispatch": auto_dispatch,
                        "max_attempts": max_attempts,
                        "retry_backoff_secs": retry_backoff_secs,
                        "dependency_failure": dependency_failure,
                        "max_parallel_milestones": max_parallel,
                    },
                }),
            )
            .await?
        }
        Command::ProgramPause { id, reason } => {
            rpc(
                &discovery,
                "program.pause",
                json!({"program_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramResume { id } => {
            rpc(&discovery, "program.resume", json!({"program_id": id})).await?
        }
        Command::ProgramCancel { id, reason } => {
            rpc(
                &discovery,
                "program.cancel",
                json!({"program_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramDelete { id } => {
            rpc(&discovery, "program.delete", json!({"program_id": id})).await?
        }
        Command::ProgramMilestoneAdd {
            program,
            title,
            objective,
            key,
            acceptance,
            depends_on,
            priority,
            max_attempts,
            profile,
            scope,
            token_budget,
            cost_budget_usd,
        } => {
            let objective = if objective == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading milestone objective from stdin")?;
                buffer
            } else {
                objective
            };
            rpc(
                &discovery,
                "program.milestone.create",
                json!({
                    "program_id": program,
                    "key": key,
                    "title": title,
                    "objective": objective,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "priority": priority,
                    "max_attempts": max_attempts,
                    "profile": profile,
                    "write_scope": scope,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                }),
            )
            .await?
        }
        Command::ProgramMilestoneRetry { id } => {
            rpc(
                &discovery,
                "program.milestone.retry",
                json!({"milestone_id": id}),
            )
            .await?
        }
        Command::ProgramMilestoneSkip { id, reason } => {
            rpc(
                &discovery,
                "program.milestone.skip",
                json!({"milestone_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramRun { milestone } => {
            rpc(
                &discovery,
                "program.milestone.start",
                json!({"milestone_id": milestone}),
            )
            .await?
        }
        Command::ProgramTick { project } => {
            rpc(&discovery, "program.tick", json!({"project_id": project})).await?
        }
        Command::ProgramReplay { id, repair } => {
            rpc(
                &discovery,
                "program.replay",
                json!({"program_id": id, "repair": repair}),
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
        Command::Runs { session, limit } => {
            rpc(
                &discovery,
                "session.runs",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::RunDetail { run } => {
            rpc(&discovery, "session.run.detail", json!({"run_id": run})).await?
        }
        Command::RunResume {
            session,
            run,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.run.resume",
                json!({
                    "session_id": session,
                    "run_id": run,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::RunCancel { session, run } => {
            rpc(
                &discovery,
                "session.run.cancel",
                json!({"session_id": session, "run_id": run}),
            )
            .await?
        }
        Command::RunPolicy { project } => {
            rpc(
                &discovery,
                "session.run.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RunPolicySet {
            project,
            auto_resume,
            max_attempts,
            max_age_hours,
        } => {
            let current = rpc(
                &discovery,
                "session.run.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(object) = policy.as_object_mut() {
                if let Some(auto_resume) = auto_resume {
                    object.insert("auto_resume".into(), json!(auto_resume));
                }
                if let Some(max_attempts) = max_attempts {
                    object.insert("max_attempts".into(), json!(max_attempts));
                }
                if let Some(max_age_hours) = max_age_hours {
                    object.insert("max_age_hours".into(), json!(max_age_hours));
                }
            }
            rpc(
                &discovery,
                "session.run.policy.set",
                json!({"project_id": project, "policy": policy}),
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
        Command::Steer {
            session,
            text,
            agent,
            task,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.steer",
                json!({
                    "session_id": session,
                    "body": text,
                    "agent_id": agent,
                    "task_id": task,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::Steering {
            session,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "session.steering",
                json!({"session_id": session, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::SteerCancel { id } => {
            rpc(&discovery, "session.steering.cancel", json!({"id": id})).await?
        }
        Command::Defer {
            session,
            prompt,
            watch,
            watch_mode,
            after_secs,
            at,
            every_secs,
            max_runs,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.defer",
                json!({
                    "session_id": session,
                    "body": prompt,
                    "watch_task_ids": watch,
                    "watch_mode": watch_mode,
                    "after_secs": after_secs,
                    "deadline_at": at,
                    "every_secs": every_secs,
                    "max_runs": max_runs,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::Continuations {
            session,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "session.continuations",
                json!({"session_id": session, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::ContinuationCancel { id } => {
            rpc(&discovery, "session.continuation.cancel", json!({"id": id})).await?
        }
        Command::TaskStalls { session } => {
            rpc(&discovery, "session.stalls", json!({"session_id": session})).await?
        }
        Command::Recoveries { session } => {
            rpc(
                &discovery,
                "orchestration.recoveries",
                json!({"session_id": session}),
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
        Command::Prompt {
            session,
            content,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.prompt",
                json!({"session_id": session, "content": content, "operation_id": operation_id}),
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
        Command::Cache { session } => {
            rpc(&discovery, "session.cache", json!({"session_id": session})).await?
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
            include_stale,
            status,
        } => {
            rpc(
                &discovery,
                "memory.list",
                json!({
                    "project_id": project,
                    "query": query,
                    "kind": kind,
                    "limit": limit,
                    "include_stale": include_stale,
                    "status": status
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
        Command::MemorySearch {
            project,
            query,
            task,
            department,
            paths,
            kinds,
            limit,
            include_stale,
        } => {
            rpc(
                &discovery,
                "memory.search",
                json!({
                    "project_id": project,
                    "query": query,
                    "task_id": task,
                    "department": department,
                    "paths": paths,
                    "kinds": kinds,
                    "limit": limit,
                    "include_stale": include_stale
                }),
            )
            .await?
        }
        Command::MemoryBrief { session, task } => {
            rpc(
                &discovery,
                "memory.brief",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::MemoryUpdate {
            memory,
            status,
            pin,
            unpin,
            tags,
        } => {
            if pin && unpin {
                bail!("--pin and --unpin cannot be combined");
            }
            let pinned = if pin {
                Some(true)
            } else if unpin {
                Some(false)
            } else {
                None
            };
            let tags = if tags.is_empty() { None } else { Some(tags) };
            if status.is_none() && pinned.is_none() && tags.is_none() {
                bail!("memory update requires --status, --pin/--unpin, or --tag");
            }
            rpc(
                &discovery,
                "memory.update",
                json!({
                    "id": memory,
                    "status": status,
                    "pinned": pinned,
                    "tags": tags
                }),
            )
            .await?
        }
        Command::MemoryVerify { memory } => {
            rpc(&discovery, "memory.verify", json!({"id": memory})).await?
        }
        Command::MemorySweep {
            project,
            days,
            dry_run,
        } => {
            rpc(
                &discovery,
                "memory.sweep",
                json!({
                    "project_id": project,
                    "older_than_days": days,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::MemoryStats { project } => {
            rpc(&discovery, "memory.stats", json!({"project_id": project})).await?
        }
        Command::MemoryConflicts {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "memory.conflicts",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::MemoryConflictScan { project } => {
            rpc(
                &discovery,
                "memory.conflict.scan",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryConflictResolve { id, action, note } => {
            let action = action.replace('-', "_");
            rpc(
                &discovery,
                "memory.conflict.resolve",
                json!({"id": id, "action": action, "note": note}),
            )
            .await?
        }
        Command::MemoryHealth { project } => {
            rpc(&discovery, "memory.health", json!({"project_id": project})).await?
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
            // Preserve v2 lifecycle settings that this command does not edit.
            let current = rpc(
                &discovery,
                "memory.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current.get("policy").cloned().unwrap_or_default();
            rpc(
                &discovery,
                "memory.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": !clear && !disable,
                        "max_items": max_items,
                        "max_age_days": max_age_days,
                        "task_result_verify_days": policy.get("task_result_verify_days").cloned().unwrap_or(json!(90)),
                        "default_verify_days": policy.get("default_verify_days").cloned().unwrap_or(json!(365)),
                        "auto_supersede": policy.get("auto_supersede").cloned().unwrap_or(json!(true)),
                        "brief_max_chars": policy.get("brief_max_chars").cloned().unwrap_or(json!(4000)),
                        "sweep_stale": policy.get("sweep_stale").cloned().unwrap_or(json!(false))
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

fn parse_dependency_failure(value: &str) -> Result<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "block" => Ok("block"),
        "cancel" => Ok("cancel"),
        "retry" => Ok("retry"),
        other => bail!("dependency failure policy must be block, cancel, or retry (got {other})"),
    }
}

fn print_program_line(program: &Value) {
    let id = program["id"].as_str().unwrap_or("?");
    let status = program["status"].as_str().unwrap_or("?");
    let goal = program["goal"].as_str().unwrap_or("");
    println!("{id}  {status:<10}  {goal}");
    let mut budget = Vec::new();
    if let Some(tokens) = program["token_budget"].as_u64() {
        budget.push(format!("{tokens} tokens"));
    }
    if let Some(cost) = program["cost_budget_usd"].as_f64() {
        budget.push(format!("${cost:.2}"));
    }
    if !budget.is_empty() {
        println!("  budget {}", budget.join(" / "));
    }
    if let Some(reason) = program["reason"].as_str() {
        println!("  {reason}");
    }
}

fn print_human(value: &Value) {
    if let Some(programs) = value.get("programs").and_then(Value::as_array) {
        if programs.is_empty() {
            println!("No programs.");
            return;
        }
        for entry in programs {
            let program = entry.get("program").unwrap_or(entry);
            let id = program["id"].as_str().unwrap_or("?");
            let status = program["status"].as_str().unwrap_or("?");
            let goal = program["goal"].as_str().unwrap_or("");
            let counts = &entry["counts"];
            println!(
                "{id}  {status:<10}  {goal}  milestones {}/{} done · {} running · {} blocked",
                counts["completed"].as_u64().unwrap_or(0),
                counts["pending"].as_u64().unwrap_or(0)
                    + counts["ready"].as_u64().unwrap_or(0)
                    + counts["running"].as_u64().unwrap_or(0)
                    + counts["verifying"].as_u64().unwrap_or(0)
                    + counts["completed"].as_u64().unwrap_or(0)
                    + counts["failed"].as_u64().unwrap_or(0)
                    + counts["blocked"].as_u64().unwrap_or(0)
                    + counts["cancelled"].as_u64().unwrap_or(0),
                counts["running"].as_u64().unwrap_or(0) + counts["verifying"].as_u64().unwrap_or(0),
                counts["blocked"].as_u64().unwrap_or(0),
            );
            print_program_line(program);
        }
        return;
    }
    if let Some(program) = value
        .get("program")
        .filter(|program| program["id"].is_string())
    {
        print_program_line(program);
        if let Some(milestones) = value.get("milestones").and_then(Value::as_array) {
            for milestone in milestones {
                let key = milestone["key"].as_str().unwrap_or("?");
                let id = milestone["id"].as_str().unwrap_or("?");
                let status = milestone["status"].as_str().unwrap_or("?");
                let title = milestone["title"].as_str().unwrap_or("");
                println!(
                    "  {key:<8} {status:<10} {title}  attempts {}/{}{}",
                    milestone["attempts"].as_u64().unwrap_or(0),
                    milestone["max_attempts"].as_u64().unwrap_or(0),
                    milestone["session_id"]
                        .as_str()
                        .map(|session| format!("  session {session}"))
                        .unwrap_or_default()
                );
                if let Some(deps) = milestone["deps"].as_array().filter(|deps| !deps.is_empty()) {
                    println!(
                        "    deps {}",
                        deps.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if let Some(error) = milestone["last_error"].as_str() {
                    println!("    {error}");
                }
                if let Some(next) = milestone["next_attempt_at"].as_str() {
                    println!("    retry after {next}");
                }
                let _ = id;
            }
        }
        if let Some(usage) = value.get("usage") {
            println!(
                "  usage {} tokens / ${:.4}",
                usage["token_spent"].as_u64().unwrap_or(0),
                usage["cost_spent_usd"].as_f64().unwrap_or(0.0)
            );
        }
        return;
    }
    if value.as_object().is_some_and(|object| {
        object.len() == 2
            && object.contains_key("session_id")
            && object.contains_key("continuations")
    }) && let Some(continuations) = value.get("continuations").and_then(Value::as_array)
    {
        if continuations.is_empty() {
            println!("No continuations.");
            return;
        }
        for continuation in continuations {
            let id = continuation["id"].as_str().unwrap_or("?");
            let series = continuation["series_id"]
                .as_str()
                .filter(|series| !series.is_empty())
                .unwrap_or(id);
            let run_count = continuation["run_count"].as_u64().unwrap_or(0);
            let max_runs = continuation["recurrence_max_runs"].as_u64().unwrap_or(1);
            let mut header = format!(
                "{id}  {}  run {run_count}/{max_runs}  series {series}",
                continuation["status"].as_str().unwrap_or("?")
            );
            if let Some(every_secs) = continuation["recurrence_every_secs"].as_u64() {
                header.push_str(&format!("  every {every_secs}s"));
            }
            println!("{header}");
            if let Some(deadline) = continuation["deadline_at"].as_str() {
                println!("  deadline {deadline}");
            }
            let watch = continuation["watch_task_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if !watch.is_empty() {
                println!(
                    "  watch {}: {watch}",
                    continuation["watch_mode"].as_str().unwrap_or("all")
                );
            }
            if let Some(error) = continuation["last_error"].as_str() {
                println!("  {error}");
            }
            println!("  {}", continuation["body"].as_str().unwrap_or(""));
        }
        return;
    }
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
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value
            .get("cache_hit_ratio")
            .and_then(Value::as_f64)
            .is_some()
        && value.get("input_tokens").is_some()
    {
        let ratio = value["cache_hit_ratio"].as_f64().unwrap_or(0.0);
        println!("cache  {session_id}");
        println!(
            "  hit ratio        {:.2}%  (cache read / ledger input buckets)",
            ratio * 100.0
        );
        println!(
            "  tokens           input {} · cache read {} · cache write {} · output {}",
            value["input_tokens"].as_u64().unwrap_or(0),
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0)
        );
        match value["preamble_tokens_estimate"].as_u64() {
            Some(tokens) => println!("  preamble         ~{tokens} tokens (estimate)"),
            None => println!("  preamble         unknown"),
        }
        match value["prompt_prefix"].as_str() {
            Some(prefix) => println!("  prompt prefix    {prefix}"),
            None => println!("  prompt prefix    unknown"),
        }
        match value["last_call_at"].as_str() {
            Some(last_call) => println!("  last call        {last_call}"),
            None => println!("  last call        never"),
        }
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
    if let Some(steering) = value.get("steering").and_then(Value::as_object) {
        println!(
            "{}  {}  agent={}  task={}",
            steering["id"].as_str().unwrap_or("?"),
            steering["status"].as_str().unwrap_or("?"),
            steering["agent_id"].as_str().unwrap_or("?"),
            steering["task_id"].as_str().unwrap_or("-")
        );
        println!("  {}", steering["body"].as_str().unwrap_or(""));
        if let Some(delivered_at) = steering["delivered_at"].as_str() {
            println!("  delivered at {delivered_at}");
        }
        return;
    }
    if let Some(messages) = value.get("steering").and_then(Value::as_array) {
        if messages.is_empty() {
            println!("No steering messages.");
            return;
        }
        for steering in messages {
            println!(
                "{}  {:<9}  agent={}  task={}  {}",
                steering["id"].as_str().unwrap_or("?"),
                steering["status"].as_str().unwrap_or("?"),
                steering["agent_id"].as_str().unwrap_or("?"),
                steering["task_id"].as_str().unwrap_or("-"),
                steering["created_at"].as_str().unwrap_or("?")
            );
            println!("  {}", steering["body"].as_str().unwrap_or(""));
        }
        return;
    }
    if value.get("session_id").is_some()
        && let Some(recoveries) = value.get("recoveries").and_then(Value::as_array)
    {
        println!(
            "session {}  recoveries={}",
            value["session_id"].as_str().unwrap_or("?"),
            recoveries.len()
        );
        if recoveries.is_empty() {
            println!("No stall recovery records.");
            return;
        }
        for recovery in recoveries {
            println!(
                "{}  {:<14}  action={}  attempts={}  last_stall={}",
                recovery["task_id"].as_str().unwrap_or("?"),
                recovery["status"].as_str().unwrap_or("?"),
                recovery["action"].as_str().unwrap_or("?"),
                recovery["attempts"].as_u64().unwrap_or(0),
                recovery["last_stall_count"].as_u64().unwrap_or(0)
            );
            println!(
                "  requested={}  updated={}  next={}",
                recovery["requested_at"].as_str().unwrap_or("-"),
                recovery["updated_at"].as_str().unwrap_or("-"),
                recovery["next_action_at"].as_str().unwrap_or("-")
            );
            if let Some(error) = recovery["last_error"].as_str() {
                println!("  {error}");
            }
        }
        return;
    }
    if value.get("stalls").is_some() && value.get("progress").is_some() {
        println!(
            "session {}  stall threshold={}s",
            value["session_id"].as_str().unwrap_or("?"),
            value["threshold_secs"].as_u64().unwrap_or(0)
        );
        let progress = value["progress"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if progress.is_empty() {
            println!("No task progress recorded.");
        }
        for task in progress {
            println!(
                "  {}  stalls={}  last={}  turn={}  tool={}",
                task["task_id"].as_str().unwrap_or("?"),
                task["stall_count"].as_u64().unwrap_or(0),
                task["last_progress_at"].as_str().unwrap_or("-"),
                task["last_turn"].as_u64().unwrap_or(0),
                task["last_tool"].as_str().unwrap_or("-")
            );
        }
        let stalls = value["stalls"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if stalls.is_empty() {
            println!("No stall observations.");
        }
        for stall in stalls {
            println!(
                "  stall {}  age={}s  attempt={}  status={}  observed={}",
                stall["task_id"].as_str().unwrap_or("?"),
                stall["age_secs"].as_u64().unwrap_or(0),
                stall["attempts"].as_u64().unwrap_or(0),
                stall["status"].as_str().unwrap_or("?"),
                stall["observed_at"].as_str().unwrap_or("?")
            );
        }
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
            "{} sessions · {} running · {} attention · {} blocked tasks · {} failed · {} retrying · {} stalled · {} conflicts",
            totals["sessions"].as_u64().unwrap_or(0),
            totals["running"].as_u64().unwrap_or(0),
            totals["attention"].as_u64().unwrap_or(0),
            totals["blocked_tasks"].as_u64().unwrap_or(0),
            totals["failed_tasks"].as_u64().unwrap_or(0),
            totals["retrying_tasks"].as_u64().unwrap_or(0),
            totals["stalled_tasks"].as_u64().unwrap_or(0),
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
            "tasks={} completed={} active={} ready={} blocked={} failed={} retrying={} stalled={}",
            metrics["total_tasks"].as_u64().unwrap_or(0),
            metrics["completed_tasks"].as_u64().unwrap_or(0),
            metrics["active_tasks"].as_u64().unwrap_or(0),
            metrics["ready_tasks"].as_u64().unwrap_or(0),
            metrics["blocked_tasks"].as_u64().unwrap_or(0),
            metrics["failed_tasks"].as_u64().unwrap_or(0),
            metrics["retrying_tasks"].as_u64().unwrap_or(0),
            metrics["stalled_tasks"].as_u64().unwrap_or(0)
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
            "project {}  sessions={}  calls={}  in={}  out={}  cache_read={}  cache_write={}  cost=${:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            sessions.len(),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cache_read_tokens"].as_u64().unwrap_or(0),
            totals["cache_write_tokens"].as_u64().unwrap_or(0),
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
            "session {}  calls={}  in={}  out={}  cache_read={}  cache_write={}  cost=${:.4}",
            value["session_id"].as_str().unwrap_or("?"),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cache_read_tokens"].as_u64().unwrap_or(0),
            totals["cache_write_tokens"].as_u64().unwrap_or(0),
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
                    "  {label:5} {:<20} calls={:<3} in={:<7} out={:<7} cache={}/{} ${:.4}",
                    bucket["key"].as_str().unwrap_or("?"),
                    bucket["calls"].as_u64().unwrap_or(0),
                    bucket["input_tokens"].as_u64().unwrap_or(0),
                    bucket["output_tokens"].as_u64().unwrap_or(0),
                    bucket["cache_read_tokens"].as_u64().unwrap_or(0),
                    bucket["cache_write_tokens"].as_u64().unwrap_or(0),
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
    if let Some(checks) = value.get("checks").and_then(Value::as_array)
        && value.get("ok").is_some()
    {
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
    if let Some(text) = value.get("text").and_then(Value::as_str)
        && let Some(memories) = value.get("memories").and_then(Value::as_array)
    {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        if !memories.is_empty() {
            let ids = memories
                .iter()
                .filter_map(|memory| memory.get("memory"))
                .filter_map(|memory| memory["id"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("selected: {ids}");
        }
        return;
    }
    if let Some(memories) = value.get("memories").and_then(Value::as_array)
        && memories.iter().any(|memory| memory.get("score").is_some())
    {
        for matched in memories {
            let memory = &matched["memory"];
            let reasons = matched["reasons"]
                .as_array()
                .map(|reasons| {
                    reasons
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            println!(
                "{:>8.3} [{}·{}] {} · {}",
                matched["score"].as_f64().unwrap_or(0.0),
                memory["kind"].as_str().unwrap_or("note"),
                memory["source"].as_str().unwrap_or("agent"),
                memory["title"].as_str().unwrap_or("?"),
                memory["id"].as_str().unwrap_or("?")
            );
            println!("  reasons: {reasons}");
            println!("  {}", memory["body"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(checks) = value.get("checks").and_then(Value::as_array)
        && let Some(memory) = value.get("memory")
    {
        for check in checks {
            println!(
                "{} {} · {} · {}",
                check["status"].as_str().unwrap_or("unchecked"),
                check["evidence"]["kind"].as_str().unwrap_or("?"),
                check["evidence"]["reference"].as_str().unwrap_or("?"),
                check["detail"].as_str().unwrap_or("")
            );
        }
        println!(
            "memory {} status={} verified_at={} stale_reason={}",
            memory["id"].as_str().unwrap_or("?"),
            memory["status"].as_str().unwrap_or("active"),
            memory["last_verified_at"].as_str().unwrap_or("never"),
            memory["stale_reason"].as_str().unwrap_or("none")
        );
        return;
    }
    if let Some(conflicts) = value.get("conflicts").and_then(Value::as_array) {
        if let Some(detected) = value.get("detected").and_then(Value::as_u64) {
            println!(
                "memory conflict scan: detected={} open={}",
                detected,
                value["open"].as_u64().unwrap_or(0)
            );
        }
        if conflicts.is_empty() {
            println!("no memory conflicts");
        }
        for conflict in conflicts {
            println!(
                "[{}·{}] {} · {} · {}",
                conflict["memory_a_kind"].as_str().unwrap_or("?"),
                conflict["memory_b_kind"].as_str().unwrap_or("?"),
                conflict["memory_a_title"].as_str().unwrap_or("?"),
                conflict["memory_b_title"].as_str().unwrap_or("?"),
                conflict["status"].as_str().unwrap_or("open")
            );
            println!(
                "  {} {} · id {}",
                conflict["scope_kind"].as_str().unwrap_or("?"),
                conflict["scope_value"].as_str().unwrap_or("?"),
                conflict["id"].as_str().unwrap_or("?")
            );
            println!("  {}", conflict["reason"].as_str().unwrap_or(""));
            for side in ["a", "b"] {
                let key = format!("memory_{side}_excerpt");
                if let Some(excerpt) = conflict[&key].as_str()
                    && !excerpt.is_empty()
                {
                    println!("  {}: {}", side.to_uppercase(), excerpt);
                }
            }
        }
        return;
    }
    if let Some(conflict) = value.get("conflict") {
        println!(
            "memory conflict {} resolved={} action={} at={}",
            conflict["id"].as_str().unwrap_or("?"),
            conflict["status"].as_str().unwrap_or("?"),
            conflict["resolution"].as_str().unwrap_or("?"),
            conflict["resolved_at"].as_str().unwrap_or("?")
        );
        if let Some(note) = conflict["note"].as_str() {
            println!("  note: {note}");
        }
        if let Some(memories) = value.get("memories").and_then(Value::as_array) {
            for memory in memories {
                println!(
                    "  [{}·{}] {} · {} · {}",
                    memory["kind"].as_str().unwrap_or("?"),
                    memory["source"].as_str().unwrap_or("agent"),
                    memory["title"].as_str().unwrap_or("?"),
                    memory["status"].as_str().unwrap_or("active"),
                    memory["id"].as_str().unwrap_or("?")
                );
            }
        }
        return;
    }
    if value.get("verification_due").is_some() && value.get("evidence_covered").is_some() {
        println!(
            "memory health: total={} active={} stale={} superseded={} archived={} pinned={}",
            value["total"].as_u64().unwrap_or(0),
            value["active"].as_u64().unwrap_or(0),
            value["stale"].as_u64().unwrap_or(0),
            value["superseded"].as_u64().unwrap_or(0),
            value["archived"].as_u64().unwrap_or(0),
            value["pinned"].as_u64().unwrap_or(0)
        );
        println!(
            "  evidence_covered={} verified={} verification_due={} open_conflicts={}",
            value["evidence_covered"].as_u64().unwrap_or(0),
            value["verified"].as_u64().unwrap_or(0),
            value["verification_due"].as_u64().unwrap_or(0),
            value["open_conflicts"].as_u64().unwrap_or(0)
        );
        if let Some(kinds) = value.get("by_kind").and_then(Value::as_array) {
            let counts = kinds
                .iter()
                .map(|count| {
                    format!(
                        "{}={}",
                        count["key"].as_str().unwrap_or("?"),
                        count["count"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("  by_kind: {counts}");
        }
        return;
    }
    if value.get("by_status").and_then(Value::as_array).is_some() && value.get("total").is_some() {
        println!(
            "memory stats: total={}",
            value["total"].as_u64().unwrap_or(0)
        );
        for key in ["by_status", "by_source", "by_kind"] {
            let counts = value[key]
                .as_array()
                .map(|counts| {
                    counts
                        .iter()
                        .map(|count| {
                            format!(
                                "{}={}",
                                count["key"].as_str().unwrap_or("?"),
                                count["count"].as_u64().unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            println!("  {key}: {counts}");
        }
        return;
    }
    if value.get("marked_stale").is_some() && value.get("candidates").is_some() {
        println!(
            "memory sweep: candidates={} marked_stale={} dry_run={}",
            value["candidates"].as_u64().unwrap_or(0),
            value["marked_stale"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        );
        if let Some(ids) = value.get("memory_ids").and_then(Value::as_array)
            && !ids.is_empty()
        {
            println!(
                "  ids: {}",
                ids.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        return;
    }
    if let Some(memories) = value.get("memories").and_then(Value::as_array) {
        for memory in memories {
            println!(
                "[{}·{}] {} · {} · {}",
                memory["kind"].as_str().unwrap_or("note"),
                memory["source"].as_str().unwrap_or("agent"),
                memory["title"].as_str().unwrap_or("?"),
                memory["status"].as_str().unwrap_or("active"),
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
            "memory policy enabled={} max_items={} max_age_days={} task_result_verify_days={} default_verify_days={} auto_supersede={} brief_max_chars={} sweep_stale={}",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["max_items"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["max_age_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["task_result_verify_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["default_verify_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["auto_supersede"].as_bool().unwrap_or(true),
            policy["brief_max_chars"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["sweep_stale"].as_bool().unwrap_or(false)
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
            "[{}·{}] {} · {} · status={} source={}",
            memory["kind"].as_str().unwrap_or("note"),
            memory["source"].as_str().unwrap_or("agent"),
            memory["title"].as_str().unwrap_or("?"),
            memory["id"].as_str().unwrap_or("?"),
            memory["status"].as_str().unwrap_or("active"),
            memory["source"].as_str().unwrap_or("agent")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_operation_id_flags_parse() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "prompt",
                    "ses-1",
                    "continue",
                    "--operation-id",
                    "op-prompt",
                ])
                .unwrap();
                match cli.command {
                    Command::Prompt { operation_id, .. } => {
                        assert_eq!(operation_id.as_deref(), Some("op-prompt"))
                    }
                    other => panic!("expected prompt command, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "defer",
                    "ses-1",
                    "follow up",
                    "--after-secs",
                    "60",
                    "--operation-id",
                    "op-defer",
                ])
                .unwrap();
                match cli.command {
                    Command::Defer { operation_id, .. } => {
                        assert_eq!(operation_id.as_deref(), Some("op-defer"))
                    }
                    other => panic!("expected defer command, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "steer",
                    "ses-1",
                    "inspect tests",
                    "--operation-id",
                    "op-steer",
                ])
                .unwrap();
                match cli.command {
                    Command::Steer { operation_id, .. } => {
                        assert_eq!(operation_id.as_deref(), Some("op-steer"))
                    }
                    other => panic!("expected steer command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn metrics_command_parses_prometheus_flag() {
        // The full CLI command tree is large; parse on a test thread with more
        // stack than libtest's default rather than relying on runner defaults.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "metrics", "--prometheus"]).unwrap();
                match cli.command {
                    Command::Metrics { prometheus } => assert!(prometheus),
                    other => panic!("expected metrics command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn bench_command_parses_without_daemon_discovery() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "bench",
                    "run",
                    "--scenario",
                    "baseline-single-session",
                    "--repetitions",
                    "2",
                ])
                .unwrap();
                match cli.command {
                    Command::Bench {
                        command: vp_bench::cli::BenchCommand::Run(args),
                    } => {
                        assert_eq!(args.scenarios, vec!["baseline-single-session"]);
                        assert_eq!(args.repetitions, 2);
                    }
                    other => panic!("expected bench run command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_command_parses_session_and_json_flag() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "cache", "ses-123", "--json"]).unwrap();
                assert!(cli.json);
                match cli.command {
                    Command::Cache { session } => assert_eq!(session, "ses-123"),
                    other => panic!("expected cache command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod durable_run_cli_tests {
    use super::*;

    #[test]
    fn durable_run_commands_parse() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "run-policy-set",
                    "proj-1",
                    "--auto-resume",
                    "--max-attempts",
                    "3",
                    "--max-age-hours",
                    "12",
                ])
                .unwrap();
                match cli.command {
                    Command::RunPolicySet {
                        project,
                        auto_resume,
                        max_attempts,
                        max_age_hours,
                    } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(auto_resume, Some(true));
                        assert_eq!(max_attempts, Some(3));
                        assert_eq!(max_age_hours, Some(12));
                    }
                    other => panic!("expected run-policy-set, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "run-resume",
                    "ses-1",
                    "--run",
                    "run-1",
                    "--operation-id",
                    "op-resume",
                ])
                .unwrap();
                match cli.command {
                    Command::RunResume {
                        session,
                        run,
                        operation_id,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(run.as_deref(), Some("run-1"));
                        assert_eq!(operation_id.as_deref(), Some("op-resume"));
                    }
                    other => panic!("expected run-resume, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
