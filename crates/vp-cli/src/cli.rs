//! Clap command-line model for the `vp` binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "vp", about = "VerlinPark CLI", version)]
pub(crate) struct Cli {
    /// Print raw JSON results instead of human-readable output.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Override the daemon data directory (must match the daemon's data dir).
    #[arg(long, global = true)]
    pub(crate) data_dir: Option<PathBuf>,
    /// Bound every daemon RPC request (1-3600 seconds).
    #[arg(long, global = true, value_name = "SECS", default_value_t = 300)]
    pub(crate) rpc_timeout_secs: u64,
    /// Bound the TCP/TLS connection phase of every daemon RPC (1-3600 seconds).
    #[arg(long, global = true, value_name = "SECS", default_value_t = 10)]
    pub(crate) rpc_connect_timeout_secs: u64,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum ForgeProviderArg {
    Github,
    Gitlab,
    Gitea,
    Forgejo,
    Bitbucket,
}

impl ForgeProviderArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Gitea => "gitea",
            Self::Forgejo => "forgejo",
            Self::Bitbucket => "bitbucket",
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum ForgeAction {
    /// Report detected origin, provider availability, and credential source.
    Status {
        /// Restrict origin detection to one project.
        #[arg(long)]
        project: Option<String>,
    },
    /// Run the provider's read-only auth/repository preflight without pushing.
    Check {
        /// Project whose origin should be checked.
        #[arg(long)]
        project: Option<String>,
        /// Force one provider instead of environment/origin detection.
        #[arg(long, value_enum)]
        provider: Option<ForgeProviderArg>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum McpTransportArg {
    Stdio,
    Http,
    Sse,
}

impl McpTransportArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::Sse => "sse",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum AcpTransportArg {
    Stdio,
    StreamableHttp,
    Websocket,
}

impl AcpTransportArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable_http",
            Self::Websocket => "websocket",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum BudgetModeArg {
    Fixed,
    Auto,
    Unlimited,
}

impl BudgetModeArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Auto => "auto",
            Self::Unlimited => "unlimited",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum NoProgressActionArg {
    Pause,
    Escalate,
}

impl NoProgressActionArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Escalate => "escalate",
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum DaemonAgentAction {
    /// Write ~/Library/LaunchAgents/com.verlinpark.daemon.plist and bootstrap it.
    Install,
    /// Boot the job out and remove the user plist.
    Uninstall,
    /// Report installed/loaded/disabled/running state without starting anything.
    Status,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GateCommand {
    /// Open a gate and print its one-time token exactly once.
    Open {
        session: String,
        /// Plan task this gate belongs to, when applicable.
        #[arg(long)]
        task: Option<String>,
        /// ci, pr_review, deploy, human, or custom.
        #[arg(long, default_value = "custom")]
        kind: String,
        #[arg(long)]
        label: Option<String>,
        /// Bounded JSON context (PR number, workflow label, etc.). Never put a
        /// secret or the gate token in this payload.
        #[arg(long)]
        payload: Option<String>,
        /// Relative expiry in seconds.
        #[arg(long, conflicts_with = "at")]
        after_secs: Option<u64>,
        /// Absolute RFC3339 expiry.
        #[arg(long)]
        at: Option<String>,
    },
    /// Resolve an open gate with its one-time scoped token.
    Resolve {
        gate: String,
        /// One-time token. Prefer --token-stdin or --token-file; command-line
        /// arguments are visible to local process listings.
        #[arg(long, conflicts_with_all = ["token_stdin", "token_file"])]
        token: Option<String>,
        /// Read the one-time token from stdin (no trailing newline required).
        #[arg(long, conflicts_with = "token_file")]
        token_stdin: bool,
        /// Read the one-time token from a 0600 file.
        #[arg(long)]
        token_file: Option<std::path::PathBuf>,
        /// resolved (default), rejected, or failed.
        #[arg(long, default_value = "resolved")]
        outcome: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Cancel an open gate. Cancellation is terminal and wakes the wait.
    Cancel {
        gate: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        operator: Option<String>,
    },
    /// List gates for a session.
    List {
        session: String,
        #[arg(long)]
        state: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one gate and its watching continuation.
    Status { gate: String },
    /// Expire due open gates and wake their continuations.
    Sweep {
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show the project gate policy.
    Policy { project: String },
    /// Enable or disable the optional loopback resolve endpoint.
    PolicySet {
        project: String,
        #[arg(long)]
        loopback: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
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
    /// Show the OTel GenAI projection policy and bounded span counts.
    OtelStatus {
        project: String,
        /// Optional session whose projection counts are reported.
        #[arg(long)]
        session: Option<String>,
    },
    /// Show the project OTel export policy.
    OtelPolicy { project: String },
    /// Update the project OTel export policy. Export remains explicit only.
    OtelPolicySet {
        project: String,
        #[arg(long)]
        enable: bool,
        #[arg(long)]
        disable: bool,
        /// Loopback-only OTLP/HTTP base endpoint (for example http://127.0.0.1:4318).
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long = "service-name")]
        service_name: Option<String>,
        #[arg(long = "batch-limit")]
        batch_limit: Option<u32>,
        #[arg(long = "timeout-secs")]
        timeout_secs: Option<u64>,
    },
    /// Explicitly export one session's event projection as OTLP/HTTP JSON.
    OtelExport {
        session: String,
        /// Span cap for this request (1-1000). Defaults to the project policy.
        #[arg(long)]
        limit: Option<u32>,
        /// Zero-based span offset for paging.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Send one synthetic span to the configured local collector.
    OtelTest { project: String },
    /// Run deterministic cost-per-solved-task benchmarks (no daemon required).
    Bench {
        #[command(subcommand)]
        command: vp_bench::cli::BenchCommand,
    },
    /// Run the deterministic prompt-cache benchmark, or the explicit live mode.
    ///
    /// With no subcommand this runs the deterministic simulator. `run` is the
    /// explicit spelling; `compare` and `report` operate on saved JSON.
    #[command(name = "cache-bench")]
    CacheBench {
        #[command(subcommand)]
        command: Option<vp_bench::cache_bench::CacheBenchCommand>,
        #[command(flatten)]
        run: vp_bench::cache_bench::CacheBenchRunArgs,
    },
    /// Explicitly install, remove, or inspect the user LaunchAgent for verlinpadd.
    DaemonAgent {
        #[command(subcommand)]
        action: DaemonAgentAction,
        /// Skip the interactive confirmation prompt.
        #[arg(long, global = true)]
        yes: bool,
        /// Print the plist and audit commands without writing or calling launchctl.
        #[arg(long, global = true)]
        dry_run: bool,
        /// Override the daemon data directory recorded in the plist.
        #[arg(long, global = true)]
        data_dir: Option<std::path::PathBuf>,
        /// Absolute path to verlinpadd (defaults to the sibling of vp or PATH).
        #[arg(long, global = true)]
        binary: Option<std::path::PathBuf>,
        /// Replace an existing not-loaded plist. Never overrides a running job.
        #[arg(long, global = true)]
        force: bool,
    },
    /// List configured external ACP agents.
    AcpAgents,
    /// Add an external Agent Client Protocol (ACP) agent.
    AcpAdd {
        name: String,
        /// Stdio command. Omit for a remote transport.
        command: Option<String>,
        #[arg(long, value_enum, default_value_t = AcpTransportArg::Stdio)]
        transport: AcpTransportArg,
        /// Remote endpoint (http/https for streamable_http, ws/wss for websocket).
        #[arg(long)]
        endpoint: Option<String>,
        /// Environment variable name holding the Authorization header value.
        #[arg(long = "auth-header-env")]
        auth_header_env: Option<String>,
        /// Extra HTTP header as KEY=VALUE (repeatable; use ${ENV_VAR} for secrets).
        #[arg(long = "header")]
        headers: Vec<String>,
        /// Required opt-in before contacting a remote ACP agent.
        #[arg(long = "allow-remote")]
        allow_remote: bool,
        /// Remote connect/handshake timeout in seconds.
        #[arg(long = "connect-timeout", default_value_t = 30)]
        connect_timeout: u64,
        #[arg(long = "arg")]
        args: Vec<String>,
        /// Environment override as KEY=VALUE (repeatable, stdio only).
        #[arg(long = "env")]
        env: Vec<String>,
        /// Auto-approve this agent's own permission requests.
        #[arg(long)]
        auto_approve: bool,
        /// Per-delegation timeout in seconds.
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
    },
    /// Update an ACP agent. Unspecified fields keep their current values.
    AcpUpdate {
        /// Agent id or unique name.
        agent: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        command: Option<String>,
        #[arg(long, value_enum)]
        transport: Option<AcpTransportArg>,
        #[arg(long)]
        endpoint: Option<String>,
        /// Clear the remote endpoint.
        #[arg(long = "clear-endpoint")]
        clear_endpoint: bool,
        #[arg(long = "auth-header-env")]
        auth_header_env: Option<String>,
        /// Clear the Authorization environment variable reference.
        #[arg(long = "clear-auth-header-env")]
        clear_auth_header_env: bool,
        /// Replacement header map (repeatable; replaces all headers).
        #[arg(long = "header")]
        headers: Vec<String>,
        /// Clear all extra headers.
        #[arg(long = "clear-headers")]
        clear_headers: bool,
        /// Enable remote transport for this agent.
        #[arg(long = "allow-remote")]
        allow_remote: bool,
        /// Disable remote transport for this agent.
        #[arg(long = "no-allow-remote")]
        no_allow_remote: bool,
        #[arg(long = "connect-timeout")]
        connect_timeout: Option<u64>,
        /// Replacement positional arguments (repeatable; replaces all arguments).
        #[arg(long = "arg")]
        args: Vec<String>,
        /// Replacement environment map (repeatable; replaces all variables).
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        auto_approve: bool,
        #[arg(long = "no-auto-approve")]
        no_auto_approve: bool,
        #[arg(long)]
        enabled: bool,
        #[arg(long)]
        disabled: bool,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Remove an ACP agent by id.
    AcpRemove { agent_id: String },
    /// Initialize an ACP agent and create a session without prompting.
    AcpProbe {
        agent: String,
        #[arg(long)]
        project: Option<String>,
        /// Skip session creation; only initialize and negotiate capabilities.
        #[arg(long = "no-session")]
        no_session: bool,
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
        /// Optional session context for elicitation and durable async tasks.
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        agent_id: Option<String>,
        #[arg(long)]
        agent_name: Option<String>,
    },
    /// List durable MCP async task handles.
    McpTasks {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one durable MCP async task, explicitly refreshing tasks/get once.
    McpTask { task_id: String },
    /// Read one MCP async task result, explicitly polling tasks/result once.
    McpTaskResult {
        task_id: String,
        #[arg(long)]
        no_refresh: bool,
    },
    /// Cancel one MCP async task through tasks/cancel.
    McpTaskCancel {
        task_id: String,
        #[arg(long)]
        reason: Option<String>,
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
    /// Back up the local data directory to a v1 ZIP or a v2 encrypted repository.
    Backup {
        /// Destination ZIP path (v1 format). Conflicts with --repository.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// V2 backup repository directory. Created on the first backup.
        #[arg(long, conflicts_with = "output")]
        repository: Option<PathBuf>,
        /// Include content-addressed artifact blobs.
        #[arg(long)]
        include_artifacts: bool,
        /// Reuse blobs from the latest snapshot (repository mode only).
        #[arg(long)]
        incremental: bool,
        /// Write an explicitly unencrypted repository (repository mode only).
        #[arg(long, conflicts_with_all = ["passphrase_env", "passphrase_file"])]
        plaintext: bool,
        /// Read the repository passphrase from this environment variable.
        #[arg(long, conflicts_with = "passphrase_file")]
        passphrase_env: Option<String>,
        /// Read the repository passphrase from a 0600 file.
        #[arg(long, conflicts_with = "passphrase_env")]
        passphrase_file: Option<PathBuf>,
        /// Store repository blobs without zstd compression.
        #[arg(long)]
        no_compress: bool,
        /// Sign with this key id from the keyring (creates/upgrades a v3 repository).
        #[arg(long = "signing-key")]
        signing_key: Option<String>,
        /// Sign with the keyring's active key.
        #[arg(long)]
        sign: bool,
        /// Keyring directory (defaults to ~/.verlinpark/backup-keys).
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
    },
    /// Restore a data-directory backup. The daemon must be stopped.
    Restore {
        /// Backup ZIP path (v1 format). Conflicts with --repository.
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// V2 backup repository directory.
        #[arg(long, conflicts_with = "input")]
        repository: Option<PathBuf>,
        /// Snapshot id to restore from a repository.
        #[arg(long)]
        snapshot: Option<String>,
        /// Replace an existing database, instance lock, or non-empty target.
        #[arg(long)]
        force: bool,
        /// Read the repository passphrase from this environment variable.
        #[arg(long, conflicts_with = "passphrase_file")]
        passphrase_env: Option<String>,
        /// Read the repository passphrase from a 0600 file.
        #[arg(long, conflicts_with = "passphrase_env")]
        passphrase_file: Option<PathBuf>,
        /// Keyring directory used to verify signatures.
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Accept a repository generation older than the local anchor.
        #[arg(long = "allow-rollback")]
        allow_rollback: bool,
        /// Accept signatures made by keys marked revoked (warns and audits).
        #[arg(long = "allow-revoked")]
        allow_revoked: bool,
        /// Anchor file override (defaults to <data-dir>/backup-anchor.json).
        #[arg(long = "anchor-file")]
        anchor_file: Option<PathBuf>,
    },
    /// List and validate snapshots in a v2/v3 backup repository.
    BackupList {
        /// V2/V3 backup repository directory.
        #[arg(long)]
        repository: PathBuf,
        /// Decrypt and hash every referenced blob.
        #[arg(long)]
        verify: bool,
        /// Read the repository passphrase from this environment variable.
        #[arg(long, conflicts_with = "passphrase_file")]
        passphrase_env: Option<String>,
        /// Read the repository passphrase from a 0600 file.
        #[arg(long, conflicts_with = "passphrase_env")]
        passphrase_file: Option<PathBuf>,
        /// Keyring directory used to verify signatures.
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Accept signatures made by keys marked revoked (warns).
        #[arg(long = "allow-revoked")]
        allow_revoked: bool,
    },
    /// Prune old backup snapshots, retaining the latest N.
    BackupPrune {
        /// V2/V3 backup repository directory.
        #[arg(long)]
        repository: PathBuf,
        /// Number of latest snapshots to retain.
        #[arg(long)]
        keep: usize,
        /// Report what would be removed without modifying the repository.
        #[arg(long)]
        dry_run: bool,
        /// Read the repository passphrase from this environment variable.
        #[arg(long, conflicts_with = "passphrase_file")]
        passphrase_env: Option<String>,
        /// Read the repository passphrase from a 0600 file.
        #[arg(long, conflicts_with = "passphrase_env")]
        passphrase_file: Option<PathBuf>,
        /// Keyring directory used to verify and re-sign a v3 repository.
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Sign the post-prune metadata with this key id.
        #[arg(long = "signing-key")]
        signing_key: Option<String>,
        /// Sign the post-prune metadata with the active key.
        #[arg(long)]
        sign: bool,
    },
    /// Verify a repository's signatures, chain, tombstones, generation, and blobs.
    BackupVerify {
        /// V2/V3 backup repository directory.
        #[arg(long)]
        repository: PathBuf,
        /// Read the repository passphrase from this environment variable.
        #[arg(long, conflicts_with = "passphrase_file")]
        passphrase_env: Option<String>,
        /// Read the repository passphrase from a 0600 file.
        #[arg(long, conflicts_with = "passphrase_env")]
        passphrase_file: Option<PathBuf>,
        /// Keyring directory used to verify signatures.
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Accept a repository generation older than the local anchor.
        #[arg(long = "allow-rollback")]
        allow_rollback: bool,
        /// Accept signatures made by keys marked revoked (warns and audits).
        #[arg(long = "allow-revoked")]
        allow_revoked: bool,
        /// Anchor file override (defaults to <data-dir>/backup-anchor.json).
        #[arg(long = "anchor-file")]
        anchor_file: Option<PathBuf>,
        /// Check blob presence and sizes without decrypting every blob.
        #[arg(long = "presence-only")]
        presence_only: bool,
    },
    /// Generate an Ed25519 backup signing key and add it to the keyring.
    BackupKeyGenerate {
        key_id: String,
        /// Keyring directory (defaults to ~/.verlinpark/backup-keys).
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Do not change the active key; the first key is always active.
        #[arg(long)]
        inactive: bool,
        /// Register the seed already present in this environment variable
        /// instead of generating a key file.
        #[arg(long = "from-env")]
        from_env: Option<String>,
    },
    /// List backup signing key ids and public keys.
    BackupKeyList {
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
    },
    /// Show the bounded keyring lifecycle audit log (never private material).
    BackupKeyEvents {
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Maximum events to show, newest first.
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Rotate the active backup signing key, keeping old public keys.
    BackupKeyRotate {
        key_id: String,
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Activate an existing key instead of generating a new one.
        #[arg(long = "use-existing")]
        use_existing: bool,
        /// Register and activate the seed already present in this environment variable.
        #[arg(long = "from-env")]
        from_env: Option<String>,
    },
    /// Revoke a backup signing key while keeping its public key for history.
    BackupKeyRevoke {
        key_id: String,
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Revoke the active key, disabling signing until another key is active.
        #[arg(long)]
        force: bool,
    },
    /// Re-sign a v3 repository with the active key without rewriting blobs.
    BackupRekey {
        /// V3 backup repository directory.
        #[arg(long)]
        repository: PathBuf,
        /// Read the repository passphrase from this environment variable.
        #[arg(long, conflicts_with = "passphrase_file")]
        passphrase_env: Option<String>,
        /// Read the repository passphrase from a 0600 file.
        #[arg(long, conflicts_with = "passphrase_env")]
        passphrase_file: Option<PathBuf>,
        /// Keyring directory holding the active signing key.
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Re-sign at most this many snapshots in this run (resumable batch).
        #[arg(long = "max-snapshots")]
        max_snapshots: Option<usize>,
    },
    /// Export one public signing key without any private material.
    BackupKeyExportPublic {
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Key id to export; defaults to the active key.
        #[arg(long = "key-id")]
        key_id: Option<String>,
        /// Write the public-key JSON to this path instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Import an existing 32-byte Ed25519 seed as a 0600 key file.
    BackupKeyImport {
        key_id: String,
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// 0600 file containing a JSON key file or a bare base64 seed.
        #[arg(long = "seed-file")]
        seed_file: Option<PathBuf>,
        /// Register a seed from this environment variable instead of a file.
        #[arg(long = "from-env")]
        from_env: Option<String>,
        #[arg(long)]
        inactive: bool,
    },
    /// Show the keyring summary (never private material).
    BackupKeyring {
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
    },
    /// Show or reset the local anti-rollback anchor.
    BackupAnchor {
        #[arg(long = "anchor-file")]
        anchor_file: Option<PathBuf>,
        /// Remove the anchor file (discards local rollback protection).
        #[arg(long)]
        reset: bool,
    },
    /// Export a signed portable anchor document for another host.
    BackupAnchorExport {
        #[arg(long = "anchor-file")]
        anchor_file: Option<PathBuf>,
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Signing key id; defaults to the keyring's active key.
        #[arg(long = "key-id")]
        key_id: Option<String>,
        /// Write the signed document to this path instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Verify a portable anchor document and preview its raise-only merge.
    BackupAnchorVerify {
        #[arg(long = "anchor-file")]
        anchor_file: Option<PathBuf>,
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Portable anchor document written by `vp backup-anchor-export`.
        #[arg(long)]
        input: PathBuf,
        /// Accept a document signed by a key marked revoked.
        #[arg(long = "allow-revoked")]
        allow_revoked: bool,
    },
    /// Verify and merge a signed portable anchor document (raise-only).
    BackupAnchorImport {
        #[arg(long = "anchor-file")]
        anchor_file: Option<PathBuf>,
        #[arg(long = "keyring-dir")]
        keyring_dir: Option<PathBuf>,
        /// Portable anchor document written by `vp backup-anchor-export`.
        #[arg(long)]
        input: PathBuf,
        /// Accept a document signed by a key marked revoked.
        #[arg(long = "allow-revoked")]
        allow_revoked: bool,
        /// Reject a document older than the local anchor instead of merging.
        #[arg(long = "require-newer")]
        require_newer: bool,
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
    /// Show installed/running language servers and capabilities for a project.
    Lsp { project: String },
    /// List Git worktrees on a project's remote host.
    RemoteWorktrees { project: String },
    /// Show the bounded remote operation ledger.
    RemoteLedger {
        project: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Import a remote branch through a bounded Git bundle, verify it locally,
    /// and enqueue it for deterministic integration.
    #[command(name = "remote-integrate")]
    RemoteIntegrate {
        project: String,
        session: String,
        branch: String,
        /// Assert the exact configured SSH host.
        #[arg(long)]
        host: Option<String>,
        /// Required remote head; the exported bundle must match it exactly.
        #[arg(long)]
        expected_commit: Option<String>,
        /// Local prerequisite commit for a thin bundle (`--not <base>`).
        #[arg(long)]
        base_commit: Option<String>,
        /// Attach the queue entry to this plan task.
        #[arg(long)]
        task: Option<String>,
        /// Extra objective text for the Verifier.
        #[arg(long)]
        objective: Option<String>,
        /// Explicit operator confirmation for this host-level action.
        #[arg(long)]
        confirm: bool,
        /// Retain the temporary refs/verlinpark/remote/... namespace.
        #[arg(long)]
        keep_import_refs: bool,
    },
    /// Show remote integration policy and remote-sourced queue entries.
    #[command(name = "remote-integration")]
    RemoteIntegration {
        project: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// Remove retained remote-integration refs. Retryable entries require
    /// --force; queued/merging entries are never touched.
    #[command(name = "remote-integration-cleanup")]
    RemoteIntegrationCleanup {
        project: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        entry: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Upload and install the local vp-remote-agent on a project's SSH host.
    /// This is an explicit host-state action: it requires --confirm and never
    /// uses sudo.
    #[command(name = "remote-bootstrap")]
    RemoteBootstrap {
        project: String,
        /// Install path on the remote host (default: the configured helper path
        /// or ~/.verlinpark/bin/vp-remote-agent). Paths outside the SSH user's
        /// home are rejected; sudo is never used.
        #[arg(long)]
        install_path: Option<String>,
        /// Pin the expected SHA-256 of the local helper binary.
        #[arg(long)]
        expected_sha256: Option<String>,
        /// Pin the expected helper version (default: this build).
        #[arg(long)]
        expected_version: Option<String>,
        /// Replace an existing helper. Without this flag bootstrap refuses.
        #[arg(long)]
        force: bool,
        /// Required explicit operator confirmation for this host-level install.
        #[arg(long)]
        confirm: bool,
        /// Bounded SSH/bootstrap timeout in seconds.
        #[arg(long)]
        timeout: Option<u64>,
        /// Optional session that receives a remote.bootstrap.completed event.
        #[arg(long)]
        session: Option<String>,
    },
    /// Show the last recorded helper install for a project without connecting.
    #[command(name = "remote-bootstrap-status")]
    RemoteBootstrapStatus { project: String },
    /// Run one bounded remote project check and record supplemental evidence.
    #[command(name = "remote-test")]
    RemoteTest {
        project: String,
        /// Session that receives the remote_test.completed event. Required
        /// unless --entry derives it.
        #[arg(long)]
        session: Option<String>,
        /// Remote merge-queue entry to attach the evidence digest to.
        #[arg(long)]
        entry: Option<String>,
        /// Named profile from the project remote test policy.
        #[arg(long)]
        profile: Option<String>,
        /// Explicit one-off command (requires an enabled policy).
        #[arg(long)]
        command: Option<String>,
        /// Working directory relative to the remote root.
        #[arg(long)]
        cwd: Option<String>,
        /// Bounded command timeout in seconds.
        #[arg(long)]
        timeout: Option<u64>,
        /// Per-stream output cap in bytes.
        #[arg(long = "output-cap")]
        output_cap: Option<u64>,
        /// Assert the exact remote HEAD before running.
        #[arg(long)]
        expected_commit: Option<String>,
    },
    /// Show remote test evidence and the project policy.
    #[command(name = "remote-test-status")]
    RemoteTestStatus {
        project: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        entry: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Read a bounded page of a retained remote test output body.
    #[command(name = "remote-test-output")]
    RemoteTestOutput {
        project: String,
        evidence: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 65536)]
        limit: u64,
    },
    /// Show the project remote test policy.
    #[command(name = "remote-test-policy")]
    RemoteTestPolicy { project: String },
    /// Update the project remote test policy. Profile syntax is
    /// `name:kind:command` with kind check|lint|test|other.
    #[command(name = "remote-test-policy-set")]
    RemoteTestPolicySet {
        project: String,
        #[arg(long)]
        enable: bool,
        #[arg(long)]
        disable: bool,
        #[arg(long = "default")]
        default_profile: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long = "output-cap")]
        output_cap: Option<u64>,
        /// Add or replace a profile (repeatable): name:kind:command.
        #[arg(long = "profile")]
        profiles: Vec<String>,
        /// Remove a named profile (repeatable).
        #[arg(long = "remove-profile")]
        remove_profiles: Vec<String>,
        #[arg(long)]
        clear_profiles: bool,
    },
    /// Show the project's remote integration policy.
    #[command(name = "remote-integration-policy")]
    RemoteIntegrationPolicy { project: String },
    /// Update the project's remote integration policy. Unspecified fields keep
    /// their current values.
    #[command(name = "remote-integration-policy-set")]
    RemoteIntegrationPolicySet {
        project: String,
        #[arg(long)]
        enable: bool,
        #[arg(long)]
        disable: bool,
        #[arg(long)]
        max_bundle_bytes: Option<u64>,
        #[arg(long)]
        require_verifier: bool,
        #[arg(long = "no-require-verifier")]
        no_require_verifier: bool,
        /// Replace the allowlist with these exact hosts (repeatable).
        #[arg(long = "allow-host")]
        allowed_hosts: Vec<String>,
        #[arg(long = "clear-hosts")]
        clear_hosts: bool,
        #[arg(long)]
        keep_import_refs: bool,
        #[arg(long = "no-keep-import-refs")]
        no_keep_import_refs: bool,
        /// Required to enable remote integration.
        #[arg(long)]
        confirm: bool,
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
        /// Retrieval lane: lexical, semantic, or hybrid. Omit for the project
        /// default (hybrid when source embeddings are enabled).
        #[arg(long, value_parser = ["lexical", "semantic", "hybrid"])]
        mode: Option<String>,
    },
    /// Show semantic source-index coverage, jobs, usage, and fallback reason.
    SourceEmbedding { project: String },
    /// Clear derived source vectors and queue every indexed source again.
    SourceEmbeddingReindex { project: String },
    /// Show the opt-in source embedding policy.
    SourceEmbeddingPolicy { project: String },
    /// Update the opt-in source embedding policy.
    SourceEmbeddingPolicySet {
        project: String,
        /// Enable the semantic source lane. endpoint_env and model are required.
        #[arg(long)]
        enable: bool,
        /// Disable the lane without clearing its configuration.
        #[arg(long)]
        disable: bool,
        /// Environment variable name holding the embeddings endpoint URL.
        #[arg(long = "endpoint-env")]
        endpoint_env: Option<String>,
        /// Embedding model name. There is no hidden model selection.
        #[arg(long)]
        model: Option<String>,
        /// Vector dimensions expected from the endpoint (8-8192).
        #[arg(long)]
        dimensions: Option<u32>,
        /// Chunks embedded per request/tick (1-64).
        #[arg(long = "batch-size")]
        batch_size: Option<u32>,
        /// Per-source chunk cap for the derived index (1-500).
        #[arg(long = "max-chunks-per-source")]
        max_chunks_per_source: Option<u32>,
        /// Days to retain stale vectors and dead jobs (0 keeps them).
        #[arg(long = "retention-days")]
        retention_days: Option<u32>,
        /// Per-request timeout in seconds (1-300).
        #[arg(long = "timeout-secs")]
        timeout_secs: Option<u64>,
        /// Environment variable name holding an optional API key.
        #[arg(long = "api-key-env")]
        api_key_env: Option<String>,
        /// Reset the policy to all defaults (disabled).
        #[arg(long)]
        clear: bool,
    },
    /// Show retrieval v2 controls: ANN, deterministic rerank, lexical expansion.
    SourceRetrievalPolicy { project: String },
    /// Update retrieval v2 controls. Omitted flags keep the stored value.
    SourceRetrievalPolicySet {
        project: String,
        /// Enable the derived source IVF index (exact scan remains the fallback).
        #[arg(long)]
        enable_ann: bool,
        /// Disable ANN while keeping the derived index on disk.
        #[arg(long)]
        disable_ann: bool,
        /// Minimum current vectors before ANN is used (1-100000).
        #[arg(long = "ann-min-vectors")]
        ann_min_vectors: Option<u32>,
        /// Centroids probed per query (1-32).
        #[arg(long = "ann-nprobe")]
        ann_nprobe: Option<u32>,
        /// Rerank mode: off or deterministic. No model is called.
        #[arg(long, value_parser = ["off", "deterministic"])]
        rerank: Option<String>,
        /// Query expansion: off or lexical. No model is called.
        #[arg(long = "query-expansion", value_parser = ["off", "lexical"])]
        query_expansion: Option<String>,
        /// Maximum expansion terms extracted from the first pass (1-16).
        #[arg(long = "max-expanded-terms")]
        max_expanded_terms: Option<u32>,
        /// Reset retrieval controls to their defaults (ANN off, rerank on,
        /// expansion off).
        #[arg(long)]
        clear: bool,
    },
    /// Show the derived source IVF index generation, coverage, and fallback.
    SourceIndex { project: String },
    /// Explicitly rebuild the derived source IVF index. No embedding request.
    SourceIndexRebuild { project: String },
    /// Show the project-wide shared budget and spend.
    ProjectBudget { project: String },
    /// Show token/cost usage grouped by session for a project.
    ProjectUsage { project: String },
    /// Show aggregate prompt-cache telemetry across a project's sessions.
    #[command(name = "project-cache")]
    ProjectCache {
        project: String,
        /// Bounded event scan (default 5000, max 50000).
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Set or clear the project-wide shared budget.
    ProjectBudgetSet {
        project: String,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
    },
    /// Show the critical-path budget governor for a session.
    Governor {
        session: String,
        /// Number of recent decision events to include (1–100).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Show the deterministic critical path and governor preview for a session.
    CriticalPath {
        session: String,
        /// Deterministic preview override for remaining tokens.
        #[arg(long)]
        remaining_tokens: Option<u64>,
        /// Preview-only mode override: `single` or `multi`.
        #[arg(long)]
        mode: Option<String>,
    },
    /// Preview governor admission without writing a decision event.
    GovernorPreview {
        session: String,
        /// Deterministic preview override for remaining tokens.
        #[arg(long)]
        remaining_tokens: Option<u64>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Preview-only mode override: `single` or `multi`.
        #[arg(long)]
        mode: Option<String>,
    },
    /// Show the project's critical-path budget governor policy.
    GovernorPolicy { project: String },
    /// Update the project's critical-path budget governor policy.
    GovernorPolicySet {
        project: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        enabled: Option<bool>,
        /// Fraction of remaining session budget held for the critical path (0.0–1.0).
        #[arg(long)]
        reserve_ratio: Option<f64>,
        /// Remaining-budget fraction that starts the non-critical throttle (0.0–1.0).
        #[arg(long)]
        throttle_at_ratio: Option<f64>,
        /// Priority boost for critical tasks under pressure (0–100).
        #[arg(long)]
        critical_boost: Option<i32>,
        /// Hard floor below which no task is admitted.
        #[arg(long)]
        min_task_budget: Option<u64>,
        /// Protection mode: `single` (default, F5) or `multi`.
        #[arg(long)]
        mode: Option<String>,
        /// Maximum protected root-to-leaf paths in multi mode (2–16).
        #[arg(long)]
        max_paths: Option<u32>,
        /// Per-department minimum reserve share in multi mode (0.0–1.0).
        #[arg(long)]
        department_min_ratio: Option<f64>,
        /// Blend of observed ledger spend into path weights (0.0–1.0).
        #[arg(long)]
        observed_weight_ratio: Option<f64>,
        /// Seconds queued before deferred tasks receive aging credit (0 disables).
        #[arg(long)]
        starvation_after_secs: Option<u64>,
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
    /// Show deterministic Worker/Labour scaling status.
    Scaling {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Preview the deterministic scaling decision for one session.
    ScalingPreview { session: String },
    /// Show the project's deterministic org-scaling policy.
    ScalingPolicy { project: String },
    /// Update the project's deterministic org-scaling policy.
    ScalingPolicySet {
        project: String,
        /// Enable or disable automatic Worker/Labour scaling.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        enabled: Option<bool>,
        /// Scale-down floor for Worker slots (1–10).
        #[arg(long)]
        min_workers: Option<u32>,
        /// Hard ceiling for Worker slots (1–10).
        #[arg(long)]
        max_workers: Option<u32>,
        /// Scale-down floor for Labour slots (1–40).
        #[arg(long)]
        min_labour: Option<u32>,
        /// Hard ceiling for Labour slots (1–40).
        #[arg(long)]
        max_labour: Option<u32>,
        /// Ready tasks required before an expansion (1–100).
        #[arg(long)]
        expand_ready_threshold: Option<u32>,
        /// Critical-delay hysteresis for idle-plan pre-expansion (0–86400s).
        #[arg(long)]
        expand_critical_delay_secs: Option<u64>,
        /// Continuous idle window before scale-down (0–604800s).
        #[arg(long)]
        scale_down_idle_secs: Option<u64>,
        /// Minimum seconds between applied decisions (0–86400).
        #[arg(long)]
        cooldown_secs: Option<u64>,
        /// Maximum slots moved per decision (1–10).
        #[arg(long)]
        max_step: Option<u32>,
        /// Optional budget headroom ratio required for expansion (0.0–1.0).
        #[arg(long)]
        budget_headroom_ratio: Option<f64>,
        /// Disable the budget headroom check.
        #[arg(long = "no-budget-check")]
        no_budget_check: bool,
    },
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
        /// JSON task object file (or stdin with `-`; stdin is also the default).
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        expected_revision: Option<u32>,
    },
    /// Explicitly expand one pending plan template into bounded fan-out
    /// children plus a shared join task. Requires --yes.
    PlanFanout {
        session: String,
        #[arg(long)]
        template: String,
        /// Inline JSON array of fan-out items.
        #[arg(long, conflicts_with = "file")]
        items: Option<String>,
        /// JSON file (or stdin with `-`) containing the item array.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Session artifact containing the item array.
        #[arg(long)]
        artifact: Option<String>,
        #[arg(long)]
        join_task: Option<String>,
        #[arg(long)]
        join_title: Option<String>,
        #[arg(long = "join-acceptance")]
        join_acceptance: Vec<String>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        expected_revision: Option<u32>,
        #[arg(long)]
        yes: bool,
    },
    /// Revise a stored TeamPlan from a JSON file (or stdin with `-`).
    TeamPlanRevise {
        session: String,
        #[arg(long)]
        reason: String,
        /// JSON plan file (or stdin with `-`; stdin is also the default).
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        expected_revision: Option<u32>,
    },
    /// List daemon-wide runtime leases (session runs and integration steps).
    Leases,
    /// Remove expired runtime leases left by a crashed process.
    LeasesSweep,
    /// Show token-free runtime-lease ownership history.
    LeaseHistory {
        /// Restrict to one lease key.
        #[arg(long)]
        key: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
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
        /// Bypass the short-lived helper/TCC status cache.
        #[arg(long)]
        refresh: bool,
        /// Use this session's project policy for status.
        #[arg(long)]
        session: Option<String>,
    },
    /// Show the unified GUI lease, holder, queue, helper status, and policy.
    Gui {
        /// Include the bounded GUI action timeline.
        #[arg(long)]
        timeline: bool,
        /// Restrict the timeline to one session.
        #[arg(long)]
        session: Option<String>,
        /// Project whose GUI policy should be reported.
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show the project's GUI session policy.
    GuiPolicy { project: String },
    /// Update the project's GUI session policy (omitted fields stay unchanged).
    GuiPolicySet {
        project: String,
        #[arg(long)]
        grace_secs: Option<u64>,
        #[arg(long)]
        wait_ms: Option<u64>,
        #[arg(long)]
        status_ttl_secs: Option<u64>,
        /// deny or require_approval.
        #[arg(long)]
        sensitive_mode: Option<String>,
        /// Additional sensitive bundle id (repeatable).
        #[arg(long = "sensitive-bundle")]
        sensitive_bundle: Vec<String>,
        /// Additional sensitive title/app pattern (repeatable).
        #[arg(long = "sensitive-title")]
        sensitive_title: Vec<String>,
        /// Manager-style GUI action timeline retention in days; 0 keeps forever.
        #[arg(long)]
        timeline_retention_days: Option<u32>,
        /// Replace the additional sensitive lists instead of appending.
        #[arg(long)]
        replace_sensitive: bool,
    },
    /// Show bounded audit rows for previous GUI timeline prunes.
    GuiTimelineAudit {
        project: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Show GUI action timeline counts and the retention cutoff.
    GuiTimelineStatus {
        project: String,
        /// Override the project policy for this read.
        #[arg(long)]
        older_than_days: Option<u32>,
    },
    /// Prune old `gui.action` timeline events for a project (bounded, audited).
    GuiTimelinePrune {
        project: String,
        /// Override the project policy for this call; 0 is rejected.
        #[arg(long)]
        older_than_days: Option<u32>,
        #[arg(long, default_value_t = 1000)]
        limit: u32,
        #[arg(long)]
        dry_run: bool,
        /// Confirm a destructive non-dry-run prune.
        #[arg(long)]
        yes: bool,
    },
    /// Show the project's shell permission policy.
    ShellPolicy { project: String },
    /// Update the project's shell network/read-deny policy. Only explicitly
    /// supplied flags are changed; omitted settings keep their stored values.
    ShellPolicySet {
        project: String,
        /// Network access for shell commands: allow or deny.
        #[arg(long)]
        network: Option<String>,
        /// Additional absolute (or ~/...) path to deny for reads. Additive.
        #[arg(long = "read-deny")]
        read_deny: Vec<String>,
        /// Replace the stored read-deny list with the supplied paths.
        #[arg(long)]
        replace_read_deny: bool,
        /// Clear the stored read-deny list.
        #[arg(long)]
        clear_read_deny: bool,
        /// Deny reads under HOME except worktree/scratch/Git metadata/toolchains.
        #[arg(long, conflicts_with = "no_restrict_home")]
        restrict_home: bool,
        /// Explicitly clear the HOME read restriction.
        #[arg(long)]
        no_restrict_home: bool,
        /// Deny sibling user homes and /Volumes outside the worktree/toolchains.
        #[arg(long, conflicts_with = "no_read_jail")]
        read_jail: bool,
        /// Explicitly clear the user/volume read jail.
        #[arg(long)]
        no_read_jail: bool,
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
    /// Update a model preset. Only supplied fields change.
    ModelPresetUpdate {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        context_window: Option<u64>,
        /// Clear the stored context window.
        #[arg(long)]
        clear_context_window: bool,
        #[arg(long)]
        input_price: Option<f64>,
        /// Clear the stored input price.
        #[arg(long)]
        clear_input_price: bool,
        #[arg(long)]
        output_price: Option<f64>,
        /// Clear the stored output price.
        #[arg(long)]
        clear_output_price: bool,
        #[arg(long)]
        notes: Option<String>,
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
        /// Watch an existing external gate id. The gate must be open.
        #[arg(long = "gate")]
        gate: Option<String>,
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
    /// Show the token-free delivery claim state for one continuation.
    ContinuationStatus { id: String },
    /// Reclaim an expired continuation delivery claim.
    ContinuationReclaim {
        id: String,
        /// Audit reason recorded with the reclaimed event.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Show plan-task progress and stall observations for a session.
    TaskStalls { session: String },
    /// Show a bounded per-tool activity timeline (latency, errors, output bytes).
    ToolActivity {
        session: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Emit CSV instead of the human timeline.
        #[arg(long)]
        csv: bool,
    },
    /// List the stable tool error-code catalog (no daemon required).
    ToolCodes {
        /// Filter by category, such as file, git, shell, remote, or mcp.
        #[arg(long)]
        category: Option<String>,
        /// Only show codes whose retryable flag matches this value.
        #[arg(long)]
        retryable: Option<bool>,
    },
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
        /// Explicit budget mode: fixed, auto, or unlimited.
        #[arg(long, value_enum)]
        budget_mode: Option<BudgetModeArg>,
        /// Auto preset: use the project auto policy and optional hard caps.
        #[arg(long)]
        auto: bool,
        /// Hard token cap in auto mode (effective token budget in fixed mode).
        #[arg(long)]
        max_token: Option<u64>,
        /// Hard cost cap in USD in auto mode (effective cost budget in fixed mode).
        #[arg(long)]
        max_usd: Option<f64>,
        /// Hard wall-clock cap in hours in auto mode (effective limit in fixed mode).
        #[arg(long)]
        max_hours: Option<f64>,
    },
    /// Show a session's budget mode, effective/hard limits, spend, and forecast.
    SessionBudget {
        session: String,
        /// Emit one CSV row instead of the human summary.
        #[arg(long)]
        csv: bool,
    },
    /// Update a session's budget mode, effective budgets, or hard caps.
    SessionBudgetSet {
        session: String,
        /// Budget mode: fixed, auto, or unlimited.
        #[arg(long, value_enum)]
        mode: Option<BudgetModeArg>,
        /// Effective token budget. Omitted fields keep their current value.
        #[arg(long)]
        token_budget: Option<u64>,
        /// Effective cost budget in USD. Omitted fields keep their current value.
        #[arg(long)]
        cost_budget_usd: Option<f64>,
        /// Effective wall-clock budget in seconds. Omitted fields keep their value.
        #[arg(long)]
        time_budget_secs: Option<u64>,
        /// Hard token cap used by auto mode.
        #[arg(long)]
        hard_token_cap: Option<u64>,
        /// Hard cost cap in USD used by auto mode.
        #[arg(long)]
        hard_cost_cap_usd: Option<f64>,
        /// Hard wall-clock cap in seconds used by auto mode.
        #[arg(long)]
        hard_wall_clock_secs: Option<u64>,
        #[arg(long)]
        clear_token_budget: bool,
        #[arg(long)]
        clear_cost_budget: bool,
        #[arg(long)]
        clear_time_budget: bool,
        #[arg(long)]
        clear_hard_token_cap: bool,
        #[arg(long)]
        clear_hard_cost_cap: bool,
        #[arg(long)]
        clear_hard_wall_clock_cap: bool,
        /// Compatibility alias for `--mode unlimited`; `false` restores the stored mode.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        unlimited: Option<bool>,
    },
    /// Preview a session's deterministic budget forecast and runway.
    SessionBudgetForecast {
        session: String,
        /// Emit one CSV row instead of the human summary.
        #[arg(long)]
        csv: bool,
    },
    /// Show the project's default auto-budget policy.
    ProjectAutoBudget {
        project: String,
        /// Emit one CSV row instead of the human summary.
        #[arg(long)]
        csv: bool,
    },
    /// Update the project auto-budget policy. Unspecified fields keep their current values.
    ProjectAutoBudgetSet {
        project: String,
        /// Enable or disable the project default. Pass `--enabled false` to disable.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        enabled: Option<bool>,
        #[arg(long)]
        initial_token_budget: Option<u64>,
        #[arg(long)]
        initial_cost_budget_usd: Option<f64>,
        #[arg(long)]
        initial_wall_clock_secs: Option<u64>,
        #[arg(long)]
        hard_token_cap: Option<u64>,
        #[arg(long)]
        hard_cost_cap_usd: Option<f64>,
        #[arg(long)]
        hard_wall_clock_secs: Option<u64>,
        #[arg(long)]
        extend_ratio: Option<f64>,
        #[arg(long)]
        max_multiplier: Option<f64>,
        #[arg(long)]
        forecast_calls: Option<u32>,
        #[arg(long)]
        min_runway_turns: Option<u32>,
        #[arg(long)]
        warn_at_ratio: Option<f64>,
        #[arg(long)]
        pause_at_ratio: Option<f64>,
        #[arg(long)]
        max_turns_per_run: Option<u32>,
        #[arg(long)]
        max_total_turns: Option<u32>,
        #[arg(long)]
        no_progress_turns: Option<u32>,
        #[arg(long, value_enum)]
        no_progress_action: Option<NoProgressActionArg>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        auto_tool_bundle: Option<bool>,
        #[arg(long)]
        clear_initial_token_budget: bool,
        #[arg(long)]
        clear_initial_cost_budget_usd: bool,
        #[arg(long)]
        clear_initial_wall_clock_secs: bool,
        #[arg(long)]
        clear_hard_token_cap: bool,
        #[arg(long)]
        clear_hard_cost_cap_usd: bool,
        #[arg(long)]
        clear_hard_wall_clock_secs: bool,
        #[arg(long)]
        clear_max_total_turns: bool,
        /// Read a full policy JSON object from a file, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        policy_file: Option<String>,
        /// Emit one CSV row instead of the human summary.
        #[arg(long)]
        csv: bool,
    },
    /// Show a project's default Run Contract policy.
    #[command(name = "project-run-policy")]
    ProjectRunPolicy { project: String },
    /// Update a project's default Run Contract policy. Unspecified fields keep
    /// their current values.
    #[command(name = "project-run-policy-set")]
    ProjectRunPolicySet {
        project: String,
        /// Default profile: quality, balanced, fast, cheap, or unattended.
        #[arg(long)]
        profile: Option<String>,
        /// Completion reserve ratio (0.0-0.5); verification/merge may spend it.
        #[arg(long)]
        reserve: Option<f64>,
        /// Maximum weighted operator interrupts before dispatch/turn pause.
        #[arg(long)]
        attention_cap: Option<u32>,
        /// No-progress turn limit inherited by the contract view.
        #[arg(long)]
        no_progress_turns: Option<u32>,
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
    /// Show the project-scoped durable-process policy.
    ProcessPolicy { project: String },
    /// Update the project-scoped durable-process policy. Unspecified fields keep
    /// their current values.
    ProcessPolicySet {
        project: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        idle_sweep_enabled: Option<bool>,
        #[arg(long)]
        max_processes: Option<u32>,
        #[arg(long)]
        idle_ttl_secs: Option<u64>,
        #[arg(long)]
        max_output_bytes: Option<u64>,
        #[arg(long)]
        max_output_lines: Option<u64>,
        /// Restart a still-running-at-boot process when its old pid is gone.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        restart_on_boot: Option<bool>,
        /// Consecutive boot-restart attempts allowed (0..=10).
        #[arg(long)]
        restart_max_attempts: Option<u32>,
        /// Base exponential backoff in seconds (0..=300).
        #[arg(long)]
        restart_backoff_secs: Option<u64>,
    },
    /// List durable supervised processes for a session.
    Processes {
        session: String,
        /// Include exited/stopped/lost rows (default true).
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        include_finished: Option<bool>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Page a durable supervised process log by byte offset.
    ProcessLogs {
        session: String,
        process: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 65536)]
        max_bytes: u32,
        #[arg(long)]
        max_lines: Option<u32>,
        /// Read the newest retained bytes instead of a forward page.
        #[arg(long)]
        from_end: bool,
        /// Poll for new output until the process reaches a terminal state.
        #[arg(long)]
        follow: bool,
        /// Follow deadline in seconds; default 300.
        #[arg(long, default_value_t = 300)]
        timeout_secs: u64,
    },
    /// Explicitly retry a blocked/lost supervised process under the same
    /// sandbox, env sidecar, and bounded attempt policy.
    ProcessRestart {
        session: String,
        process: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Stop a durable supervised process (SIGTERM, bounded wait, SIGKILL).
    ProcessStop {
        session: String,
        process: String,
        #[arg(long, default_value = "TERM")]
        signal: String,
        #[arg(long, default_value_t = 5)]
        grace_secs: u32,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
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
    /// Hard-preempt the running Leader or Worker, aborting an in-flight provider stream or tool call.
    Preempt {
        session: String,
        #[arg(long)]
        reason: Option<String>,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Show hard-preemption history and the project policy for a session.
    Preemption {
        session: String,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show a project's hard-preemption policy.
    PreemptionPolicy { project: String },
    /// Show project-wide terminal preemptions across sessions.
    Preemptions {
        project: String,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Update a project's hard-preemption policy; unspecified values are kept.
    PreemptionPolicySet {
        project: String,
        /// Explicitly enable hard preemption.
        #[arg(long, conflicts_with = "disabled")]
        enabled: bool,
        #[arg(long)]
        disabled: bool,
        #[arg(long)]
        provider_grace_ms: Option<u64>,
        #[arg(long)]
        tool_grace_ms: Option<u64>,
        #[arg(long)]
        kill_escalation_ms: Option<u64>,
        #[arg(long)]
        tool_deadline_secs: Option<u64>,
        #[arg(long)]
        clear_tool_deadline: bool,
        #[arg(long)]
        no_checkpoint: bool,
    },
    /// Pause the running Leader between turns/tools.
    Pause { session: String },
    /// Resume a paused Leader.
    Resume { session: String },
    /// Resume the stored TeamPlan, skipping completed tasks.
    ResumePlan { session: String },
    /// Compare the stored plan/run runtime snapshot with the current daemon.
    Compatibility {
        session: String,
        /// Inspect one durable run instead of the plan snapshot.
        #[arg(long)]
        run: Option<String>,
    },
    /// Read-only failure attribution and bounded provenance diagnosis.
    Diagnose {
        session: String,
        /// Attribute one plan task instead of the latest failure.
        #[arg(long)]
        task: Option<String>,
        /// Bounded event window (500..=50000), or ledger row limit with --history.
        #[arg(long)]
        limit: Option<u32>,
        /// Include the bounded provenance graph in the JSON/report output.
        #[arg(long)]
        graph: bool,
        /// Explicitly record a bounded `failure.attribution` ledger event.
        #[arg(long)]
        record: bool,
        /// List explicitly recorded attribution snapshots instead of diagnosing.
        #[arg(long)]
        history: bool,
    },
    /// Revalidate a stored plan and update its runtime snapshot after explicit
    /// operator approval. Never rewrites tasks, scopes, or budgets.
    PlanMigrate {
        session: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        expected_revision: Option<u32>,
        /// Required acknowledgement for this host-state operator action.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        approved_by: Option<String>,
    },
    /// Read-only deterministic controller replay with matched/mismatched diff.
    OrchestrateReplay {
        session: String,
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// Show estimated context usage and the last compaction/trim.
    Context { session: String },
    /// Show the bounded advisory progress ledger for a session.
    Progress {
        session: String,
        #[arg(long)]
        plan_revision: Option<u64>,
        #[arg(long, default_value_t = 64)]
        limit: u32,
    },
    /// Record one advisory progress round from a JSON payload.
    ProgressReport {
        session: String,
        /// JSON object with satisfied/in_loop/progressing/blockers/next_task/instruction/evidence.
        #[arg(long)]
        payload: String,
    },
    /// Show loopback-only A2A facade status (never the bearer token).
    A2aStatus,
    /// Show the running A2A Agent Card as JSON.
    A2aCard,
    /// Update the stored A2A policy while the facade is stopped.
    A2aPolicySet {
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
        #[arg(long)]
        allow_inbound: bool,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        max_tasks: Option<u32>,
        #[arg(long)]
        task_ttl_secs: Option<u64>,
    },
    /// List bounded in-memory A2A inbound tasks.
    A2aTasks {
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        state: Option<String>,
    },
    /// Start the loopback A2A facade and print the one-time bearer token.
    A2aStart {
        /// Explicit host-state approval is required to bind the loopback listener.
        #[arg(long)]
        approved: bool,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        max_tasks: Option<u32>,
        #[arg(long)]
        task_ttl_secs: Option<u64>,
        #[arg(long)]
        allow_inbound: bool,
        /// Enable the policy; omitted policy fields remain bounded defaults.
        #[arg(long)]
        enable: bool,
    },
    /// Stop the loopback A2A facade.
    A2aStop {
        #[arg(long)]
        reason: Option<String>,
    },
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
    CheckpointRestore {
        session: String,
        checkpoint: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
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
    /// List durable active-context checkpoints for a session.
    ContextCheckpoints {
        session: String,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Capture the current active-context boundary without running a prompt.
    ContextCheckpoint {
        session: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Show one context checkpoint and its deterministic restore preflight.
    ContextCheckpointDetail { session: String, checkpoint: String },
    /// Restore an idle session's active conversation to a context checkpoint.
    ContextRestore {
        session: String,
        checkpoint: String,
        /// Accept an above-threshold token estimate. Safety checks still apply.
        #[arg(long)]
        force: bool,
    },
    /// Delete one context checkpoint. Messages and worktree state are kept.
    ContextCheckpointDelete { session: String, checkpoint: String },
    /// Prune old context checkpoints, protecting the latest active checkpoint.
    ContextCheckpointPrune {
        session: String,
        #[arg(long)]
        older_than_days: Option<u32>,
        #[arg(long)]
        keep_latest: Option<u32>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the project-scoped context checkpoint retention policy.
    ContextCheckpointPolicy { project: String },
    /// Set the project-scoped context checkpoint retention policy.
    ContextCheckpointPolicySet {
        project: String,
        #[arg(long)]
        enabled: bool,
        #[arg(long, default_value_t = 20)]
        max_checkpoints: u32,
        #[arg(long, default_value_t = 30)]
        max_age_days: u32,
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
    /// Show the derived multi-resource Run Contract snapshot.
    Resources { session: String },
    /// Show the current shippable/mergeable readiness level.
    Readiness { session: String },
    /// Show the latest explicit partial-delivery record and current readiness.
    Delivery { session: String },
    /// Show the rescue eligibility, allocation reserve, and near-completion preview.
    RescueStatus { session: String },
    /// Grant a short-lived completion-reserve release for near-completion work.
    Rescue {
        session: String,
        /// Operator reason stored with the grant. Bounded to 500 characters.
        #[arg(long)]
        reason: Option<String>,
        /// Operator label stored with the grant (defaults to `operator`).
        #[arg(long)]
        recorded_by: Option<String>,
        /// Required confirmation; does not change budgets, caps, or verification.
        #[arg(long)]
        yes: bool,
    },
    /// Record an explicit partial delivery for an inactive session.
    PartialDelivery {
        session: String,
        /// Operator summary of what was delivered. Bounded to 2000 characters.
        #[arg(long)]
        summary: Option<String>,
        /// One remaining work item; repeat the flag for multiple items.
        #[arg(long = "remaining", value_name = "TEXT")]
        remaining: Vec<String>,
        /// Operator label stored with the record (defaults to `operator`).
        #[arg(long)]
        recorded_by: Option<String>,
        /// Required confirmation; does not merge, verify, or complete work.
        #[arg(long)]
        yes: bool,
    },
    /// Show the derived Run Contract envelope.
    RunContract { session: String },
    /// Set a bounded session Run Contract override (profile/reserve/attention).
    RunContractSet {
        session: String,
        #[arg(long)]
        profile: String,
        /// Completion reserve ratio (0.0-0.5); verification/merge may spend it.
        #[arg(long)]
        reserve: Option<f64>,
        /// Maximum weighted operator interrupts before dispatch/turn pause.
        #[arg(long)]
        attention_cap: Option<u32>,
        /// No-progress turn limit for the contract view.
        #[arg(long)]
        no_progress_turns: Option<u32>,
    },
    /// Add budget and optionally resume a paused/blocked session. Never removes caps.
    TopUp {
        session: String,
        #[arg(long)]
        tokens: Option<u64>,
        #[arg(long)]
        cost: Option<f64>,
        #[arg(long)]
        time: Option<u64>,
        #[arg(long)]
        resume: bool,
    },
    /// Show session cache-read telemetry and the prompt-prefix generation.
    Cache { session: String },
    /// Show the finalized stable cache epoch, cross-session reuse, and the
    /// bounded prefix/compaction timeline. Read-only; never calls a provider.
    #[command(name = "cache-epoch")]
    CacheEpoch {
        session: String,
        /// Maximum timeline and epoch rows. Clamped to 1..=200.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// List project-wide cache epochs, tool-bundle buckets, and reuse totals.
    /// Read-only; never calls a provider.
    #[command(name = "cache-epochs")]
    CacheEpochs {
        project: String,
        /// Maximum epoch rows. Clamped to 1..=100.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Prune old cache-epoch telemetry. Active-session epochs and the newest
    /// N epochs are always retained.
    #[command(name = "cache-epochs-prune")]
    CacheEpochsPrune {
        project: String,
        #[arg(long, default_value_t = 90)]
        older_than_days: u32,
        #[arg(long, default_value_t = 50)]
        keep_epochs: u32,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show a bounded per-call cache table with optional role/reason filters.
    #[command(name = "cache-calls")]
    CacheCalls {
        session: String,
        /// Newest-first row limit (1-100, default 20).
        #[arg(long)]
        limit: Option<u32>,
        /// Exact role filter (`leader`, `worker`, ...).
        #[arg(long)]
        role: Option<String>,
        /// Exact miss-reason filter (`tail_delta`, `provider_cache_miss`, ...).
        #[arg(long)]
        miss_reason: Option<String>,
        /// Include explicit warm calls.
        #[arg(long)]
        include_warm: bool,
    },
    /// Show the latest trusted task contract metadata for a session. No
    /// contract text, tool output, or secrets are printed.
    Contract {
        session: String,
        /// Inspect one plan task's contract instead of the session-scoped one.
        #[arg(long)]
        task: Option<String>,
    },
    /// Show the project task-contract policy.
    #[command(name = "contract-policy")]
    ContractPolicy { project: String },
    /// Set the project task-contract policy. Omitted flags keep the current
    /// value; list flags replace the list.
    #[command(name = "contract-policy-set")]
    ContractPolicySet {
        project: String,
        #[arg(long)]
        enabled: Option<bool>,
        /// `advisory` or `enforce`.
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        enforce_shell: Option<bool>,
        #[arg(long = "forbidden-path")]
        forbidden_paths: Vec<String>,
        #[arg(long = "egress-allow")]
        egress_allowlist: Vec<String>,
        #[arg(long)]
        max_writes: Option<u64>,
        #[arg(long)]
        max_bytes: Option<u64>,
        #[arg(long)]
        max_commits: Option<u64>,
    },
    /// List project-scoped agent identities (Leader, Managers, Workers). A
    /// legacy session id still lists that session's run-scoped agents.
    Agents {
        /// Project id, or a session id when `--session` is used.
        project: String,
        /// Force the legacy session run-roster view.
        #[arg(long)]
        session: Option<String>,
        /// Exact role filter: leader, manager, or worker.
        #[arg(long)]
        role: Option<String>,
        /// Exact status filter: active, idle, paused, or retired.
        #[arg(long)]
        status: Option<String>,
    },
    /// Show the bounded project org chart: Leader, departments, Managers,
    /// Workers, active assignments, workspaces, inbox/memory counts, and
    /// advisory retirement state. Read-only.
    AgentOrg {
        project: String,
        /// Include retired identities and their preserved evidence.
        #[arg(long)]
        include_retired: bool,
        /// Hard bound on identity nodes (1-200).
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// Show one identity's bounded detail: assignments, inbox metadata, memory,
    /// performance, retirement proposals, workspaces, and lifecycle evidence.
    AgentDetail {
        identity: String,
        /// Optional project guard; a mismatch fails closed.
        #[arg(long)]
        project: Option<String>,
        /// Include a short known-field inbox summary. Metadata is always shown.
        #[arg(long)]
        include_inbox: bool,
        #[arg(long, default_value_t = 20)]
        inbox_limit: u32,
        #[arg(long, default_value_t = 20)]
        memory_limit: u32,
        #[arg(long, default_value_t = 50)]
        assignment_limit: u32,
    },
    /// Inspect one Agent's bounded context, recent timeline, steering, and
    /// project-inbox state. Read-only; never returns raw prompts or tool bytes.
    #[command(name = "agent-inspect")]
    AgentInspect {
        session: String,
        agent: String,
        /// Newest bounded timeline rows to request (1-200, default 80).
        #[arg(long, default_value_t = 80)]
        timeline_limit: u32,
    },
    /// Send a direct operator message to one Agent. Running/waiting Agents use
    /// cooperative steering; idle identity Agents use the durable project inbox;
    /// idle ephemeral runs fail with AGENT_NOT_ACTIVE. This never approves an
    /// approval, resolves a gate, or changes budgets/routing.
    #[command(name = "agent-message")]
    AgentMessage {
        session: String,
        agent: String,
        /// Operator message text (1-4000 characters).
        text: String,
        /// Require the target Agent to be working on this plan task.
        #[arg(long)]
        task: Option<String>,
        /// Request the existing deferred wake when the target uses the inbox.
        #[arg(long)]
        wake: bool,
        /// Idempotency key; retrying with the same key and payload replays the
        /// first delivery result.
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// Export a portable, content-addressed team charter. Writes team.json and
    /// TEAM.md into the output directory; never includes context, memory,
    /// inbox, worktree, performance, MCP, path, URL, or secret content.
    #[command(name = "team-export")]
    TeamExport {
        project: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "1.0.0")]
        version: String,
        #[arg(long)]
        description: Option<String>,
        /// Directory that receives team.json and TEAM.md.
        #[arg(long)]
        out: PathBuf,
    },
    /// Validate a team.json. Default is a read-only library import; --dry-run
    /// prints the preview without writing, and --apply requires --approved.
    #[command(name = "team-import")]
    TeamImport {
        project: String,
        #[arg(long)]
        file: PathBuf,
        /// Print the preview without writing a library record or hire request.
        #[arg(long)]
        dry_run: bool,
        /// Explicitly apply the template. Requires --approved.
        #[arg(long)]
        apply: bool,
        /// Operator confirmation for apply. Never implied by --apply.
        #[arg(long)]
        approved: bool,
        #[arg(long)]
        operator: Option<String>,
        /// JSON array of {slot, action, identity_id?, title?} resolutions.
        #[arg(long = "resolution-json")]
        resolution_json: Option<String>,
    },
    /// List the team-template library, optionally scoped to one project.
    #[command(name = "team-templates")]
    TeamTemplates {
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Diff one stored team template against a project's current roster.
    #[command(name = "team-diff")]
    TeamDiff {
        project: String,
        #[arg(long)]
        template: String,
    },
    /// Show one stored team template as team.json and TEAM.md.
    #[command(name = "team-template")]
    TeamTemplateShow {
        template: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Verify (and optionally repair missing apply links in) the project
    /// template audit stream. Never creates a template, identity, or hire.
    #[command(name = "team-replay")]
    TeamReplay {
        project: String,
        #[arg(long)]
        repair: bool,
    },
    /// Create one explicit project identity. No session is created or assigned.
    AgentCreate {
        project: String,
        #[arg(long)]
        role: String,
        #[arg(long)]
        department: Option<String>,
        #[arg(long)]
        component: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// coding, research, verification, or generic.
        #[arg(long)]
        profile: Option<String>,
        /// Explicit model reference. It never routes a provider call by itself.
        #[arg(long)]
        model: Option<String>,
        /// Parent identity id (Manager→Leader, Worker→Manager, same project).
        #[arg(long)]
        parent: Option<String>,
        /// Bounded non-secret metadata as a JSON object. Defaults to `{}`.
        #[arg(long = "policy-json")]
        policy: Option<String>,
        /// Explicit operator confirmation for a Manager/Worker hire. Leader
        /// bootstrap ignores it.
        #[arg(long)]
        approved: bool,
    },
    /// Update an identity. Omitted fields keep their current value.
    AgentUpdate {
        identity: String,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        department: Option<String>,
        #[arg(long = "clear-department")]
        clear_department: bool,
        #[arg(long)]
        component: Option<String>,
        #[arg(long = "clear-component")]
        clear_component: bool,
        #[arg(long)]
        title: Option<String>,
        #[arg(long = "clear-title")]
        clear_title: bool,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long = "clear-profile")]
        clear_profile: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long = "clear-model")]
        clear_model: bool,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long = "clear-parent")]
        clear_parent: bool,
        #[arg(long)]
        successor: Option<String>,
        #[arg(long = "clear-successor")]
        clear_successor: bool,
        /// Bounded non-secret metadata as a JSON object. `{}` clears it.
        #[arg(long = "policy-json")]
        policy: Option<String>,
    },
    /// Hire a Manager or Worker through the S6 approval-aware surface. The
    /// Leader is bootstrapped through `agent-create --role leader`.
    AgentHire {
        project: String,
        /// manager or worker.
        #[arg(long)]
        role: String,
        #[arg(long)]
        department: Option<String>,
        #[arg(long)]
        component: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// coding, research, verification, or generic.
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long = "policy-json")]
        policy: Option<String>,
        /// Explicit operator confirmation. Without it, policy may create a
        /// pending request that still requires `agent-hire-approve`.
        #[arg(long)]
        approved: bool,
        #[arg(long)]
        operator: Option<String>,
    },
    /// List pending/decided hire requests.
    AgentHires {
        project: String,
        /// pending, approved, or rejected.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Approve a pending hire request and create its reserved identity.
    AgentHireApprove {
        request: String,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Reject a pending hire request. No identity is created.
    AgentHireReject {
        request: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Promote a Worker to Manager or a Manager to Leader. Always explicit.
    AgentPromote {
        identity: String,
        /// manager or leader.
        #[arg(long)]
        role: String,
        /// Required for Worker→Manager unless exactly one live Leader exists.
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Release active assignments/workspaces after preserving evidence.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Transfer department/component/title/parent. Role changes are promotion.
    AgentTransfer {
        identity: String,
        #[arg(long)]
        department: Option<String>,
        #[arg(long = "clear-department")]
        clear_department: bool,
        #[arg(long)]
        component: Option<String>,
        #[arg(long = "clear-component")]
        clear_component: bool,
        #[arg(long)]
        title: Option<String>,
        #[arg(long = "clear-title")]
        clear_title: bool,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long = "clear-parent")]
        clear_parent: bool,
        /// Release active assignments/workspaces after preserving evidence.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Pause an active identity. Pausing an already paused identity is a no-op.
    AgentPause { identity: String },
    /// Resume a paused identity.
    AgentResume { identity: String },
    /// Retire an identity. Retirement is terminal and never deletes evidence.
    AgentRetire {
        identity: String,
        /// Optional same-project, compatible successor.
        #[arg(long)]
        successor: Option<String>,
        /// Optional approved retirement proposal.
        #[arg(long)]
        proposal: Option<String>,
        /// Explicit operator/Leader confirmation.
        #[arg(long)]
        approved: bool,
        /// Release active assignments/workspaces after preserving evidence.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Read-only per-identity performance ledger, or the project summary.
    AgentPerformance {
        project: String,
        /// One identity. Omit for the project summary.
        #[arg(long)]
        identity: Option<String>,
        #[arg(long = "lookback-days")]
        lookback_days: Option<u32>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Read retirement candidates/proposals, or record one explicit proposal
    /// decision. Proposals never retire an identity by themselves.
    AgentRetirement {
        project: String,
        /// List deterministic candidates (default).
        #[arg(long)]
        candidates: bool,
        /// List proposals.
        #[arg(long)]
        proposals: bool,
        /// Propose retirement for one identity (must cross a threshold).
        #[arg(long)]
        propose: Option<String>,
        /// Approve a pending proposal.
        #[arg(long)]
        approve: Option<String>,
        /// Reject a pending proposal.
        #[arg(long)]
        reject: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        identity: Option<String>,
        /// pending, approved, rejected, executed, or expired.
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        operator: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show the project retirement policy.
    RetirementPolicy { project: String },
    /// Set the project retirement policy. Omitted flags keep their value.
    RetirementPolicySet {
        project: String,
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long = "lookback-days")]
        lookback_days: Option<u32>,
        #[arg(long = "min-completed-tasks")]
        min_completed_tasks: Option<u32>,
        #[arg(long = "max-consecutive-failures")]
        max_consecutive_failures: Option<u32>,
        #[arg(long = "max-verifier-rejections")]
        max_verifier_rejections: Option<u32>,
        #[arg(long = "max-unsubstantiated-handoffs")]
        max_unsubstantiated_handoffs: Option<u32>,
        #[arg(long = "max-false-consensus")]
        max_false_consensus: Option<u32>,
        #[arg(long = "cooldown-secs")]
        cooldown_secs: Option<u64>,
        #[arg(long = "require-approval")]
        require_approval: Option<bool>,
    },
    /// Show the project agent-identity policy.
    AgentPolicy { project: String },
    /// Set the project agent-identity policy. Omitted flags keep their value.
    AgentPolicySet {
        project: String,
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long = "max-managers")]
        max_managers: Option<u32>,
        #[arg(long = "max-workers")]
        max_workers: Option<u32>,
        #[arg(long = "hire-requires-approval")]
        hire_requires_approval: Option<bool>,
        #[arg(long = "retirement-requires-approval")]
        retirement_requires_approval: Option<bool>,
    },
    /// List identity/department/project memory. Content is bounded by the daemon.
    AgentMemory {
        project: String,
        /// Viewer identity; scope isolation is enforced when present.
        #[arg(long)]
        identity: Option<String>,
        /// agent, department, or project.
        #[arg(long)]
        scope: Option<String>,
        /// active, superseded, or stale.
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        department: Option<String>,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long = "include-expired")]
        include_expired: bool,
    },
    /// Record one explicit identity/department/project memory.
    AgentMemoryRemember {
        project: String,
        /// agent, department, or project.
        #[arg(long)]
        scope: String,
        #[arg(long)]
        content: String,
        /// Agent-scope owner; department scope derives from it when omitted.
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        department: Option<String>,
        #[arg(long)]
        component: Option<String>,
        /// operator, agent, task_result, verifier, or integration.
        #[arg(long)]
        source: Option<String>,
        /// Bounded pointer as kind:reference[:note] or a JSON object;
        /// repeatable. URL references keep their colons.
        #[arg(long = "evidence")]
        evidence: Vec<String>,
        #[arg(long = "expires-at")]
        expires_at: Option<String>,
        #[arg(long)]
        pinned: bool,
    },
    /// Supersede one active memory with an existing or newly created memory.
    AgentMemorySupersede {
        memory: String,
        /// Existing replacement memory in the exact same scope.
        #[arg(long)]
        replacement: Option<String>,
        /// Or create a replacement with this bounded content.
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long = "expected-hash")]
        expected_hash: Option<String>,
    },
    /// Soft-forget a memory, or prune a stale/superseded one.
    AgentMemoryForget {
        memory: String,
        /// Hard-delete after the row has left active state.
        #[arg(long)]
        prune: bool,
    },
    /// Logically erase a memory and all prior event payloads (destructive).
    AgentMemoryErase {
        memory: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        /// Required confirmation for logical erasure.
        #[arg(long)]
        yes: bool,
    },
    /// Expire due memory and optionally prune old stale/superseded rows.
    AgentMemorySweep {
        project: String,
        #[arg(long = "older-than-days")]
        older_than_days: Option<u32>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        prune: bool,
    },
    /// List session→identity assignments. Requires one of session, identity, or
    /// project.
    Assignments {
        session: Option<String>,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        project: Option<String>,
        /// active, released, or queued.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Assign a project identity to a session. This is a durable coordination
    /// record, not a run.
    Assign {
        session: String,
        identity: String,
        /// Optional consistency check against the identity role.
        #[arg(long)]
        role: Option<String>,
        /// Identity-lease seconds (60-86400, default 600).
        #[arg(long)]
        lease_secs: Option<u64>,
        /// Create a deferred wake continuation when the session is idle,
        /// failed, or cancelled. It never blocks or holds a Worker slot.
        #[arg(long)]
        wake: bool,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Heartbeat/renew a live identity lease.
    AssignmentRenew {
        session: String,
        identity: String,
        #[arg(long)]
        lease_secs: Option<u64>,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Release an assignment owned by the named session.
    Release {
        session: String,
        identity: String,
        #[arg(long)]
        operator: bool,
        #[arg(long)]
        reason: Option<String>,
    },
    /// List Worker component workspaces and session attachments.
    Workspaces {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        identity: Option<String>,
        /// active, archived, or retired.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one workspace by id, or the workspace bindings of a session.
    Workspace {
        /// Workspace id. Omit when using --session.
        workspace: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Explicitly land accepted workspace commits onto the component branch.
    /// This never runs automatically and requires --yes.
    WorkspaceLand {
        workspace: String,
        /// Session whose accepted commits are landed. Defaults to the
        /// workspace's last session.
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        operator: Option<String>,
        /// Confirm the irreversible component-branch merge.
        #[arg(long)]
        yes: bool,
    },
    /// Archive a released workspace, preserving branch and commit evidence.
    WorkspaceArchive {
        workspace: String,
        /// Retire instead of archiving.
        #[arg(long)]
        retire: bool,
        #[arg(long)]
        operator: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// List project inbox rows. Unread rows sort first.
    Inbox {
        identity: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
        /// unread, read, done, or expired.
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        unread: bool,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Send one project-scoped inbox message to a Leader/Manager/Worker.
    InboxSend {
        identity: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long = "from")]
        from_identity: Option<String>,
        /// task, message, question, handoff, decision, or review.
        #[arg(long, default_value = "message")]
        kind: String,
        /// Convenience text; stored as {"body": ...}.
        #[arg(long)]
        body: Option<String>,
        /// Structured JSON object (max 16 KiB, no credential-like keys).
        #[arg(long = "payload-json")]
        payload: Option<String>,
        #[arg(long = "expires-in")]
        expires_in_secs: Option<u64>,
        #[arg(long)]
        wake: bool,
    },
    /// Mark an inbox message read.
    InboxRead {
        message: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        identity: Option<String>,
    },
    /// Acknowledge an inbox message as done.
    InboxAck {
        message: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        identity: Option<String>,
    },
    /// Read-only project coordination replay, or explicit projection repair.
    CoordinationReplay {
        project: String,
        #[arg(long)]
        repair: bool,
    },
    /// Show local effect receipts for a session. `done`/`failed` rows are
    /// settled evidence; `intent`/`unknown` rows block merge until reconciled.
    Effects {
        session: String,
        /// Inspect one plan task's receipts (session-scoped rows are included
        /// in the blocking check but not in this filtered view).
        #[arg(long)]
        task: Option<String>,
        /// Newest-first row limit (1-500, default 100).
        #[arg(long)]
        limit: Option<u32>,
        /// Only unresolved intent/unknown rows.
        #[arg(long)]
        unresolved: bool,
    },
    /// Explicitly resolve an intent/unknown effect to done or failed. Marking
    /// done requires bounded evidence (hash, commit, external id, status, or a
    /// note of at least 8 characters).
    #[command(name = "effect-reconcile")]
    EffectReconcile {
        effect: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        done: bool,
        #[arg(long)]
        failed: bool,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        commit: Option<String>,
        #[arg(long = "external-id")]
        external_id: Option<String>,
        #[arg(long = "http-status")]
        http_status: Option<u16>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        operator: Option<String>,
    },
    /// Show the project effect-receipt policy.
    #[command(name = "effect-policy")]
    EffectPolicy { project: String },
    /// Set the project effect-receipt policy. Omitted flags keep the current
    /// value.
    #[command(name = "effect-policy-set")]
    EffectPolicySet {
        project: String,
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long = "unknown-blocks-merge")]
        unknown_blocks_merge: Option<bool>,
        #[arg(long = "stale-grace-secs")]
        stale_intent_grace_secs: Option<u64>,
        /// `external_only` (default) or `all`.
        #[arg(long = "shell-mode")]
        shell_mode: Option<String>,
        #[arg(long = "track-mutating-http")]
        track_mutating_http: Option<bool>,
        #[arg(long = "track-read-http")]
        track_read_http: Option<bool>,
        #[arg(long = "track-mcp-mutations")]
        track_mcp_mutations: Option<bool>,
        #[arg(long = "track-workspace-writes")]
        track_workspace_writes: Option<bool>,
    },
    /// Move stale effect intents to unknown after the project grace period.
    #[command(name = "effect-sweep")]
    EffectSweep {
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Open a durable external gate (ci, pr_review, deploy, human, custom).
    /// The one-time token is printed once and is never stored in plaintext.
    #[command(name = "gate")]
    Gate {
        #[command(subcommand)]
        command: GateCommand,
    },
    /// List project-scoped MCP tool schema pins and their trust status.
    #[command(name = "mcp-pins")]
    McpPins {
        project: String,
        #[arg(long)]
        server: Option<String>,
    },
    /// Approve the currently observed schema/description of one MCP tool.
    #[command(name = "mcp-pin-approve")]
    McpPinApprove {
        project: String,
        server: String,
        tool: String,
        #[arg(long)]
        schema_hash: String,
        #[arg(long)]
        description_hash: String,
        #[arg(long)]
        approved_by: Option<String>,
    },
    /// Reject the observed MCP tool pin without overwriting its approved
    /// hashes.
    #[command(name = "mcp-pin-reject")]
    McpPinReject {
        project: String,
        server: String,
        tool: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Show the ordered content-addressed instruction sections and hashes.
    /// Section text is never printed.
    Instructions {
        session: String,
        /// Inspect one task-scoped agent state instead of every state.
        #[arg(long)]
        agent: Option<String>,
    },
    /// Set the session prompt layout: 2 (legacy combined) or 3 (instruction
    /// store). Existing instruction state is preserved for rollback.
    #[command(name = "prompt-layout")]
    PromptLayout { session: String, layout: u32 },
    /// Explicitly warm the session's stable prompt prefix and tool bundle.
    /// Audit cache telemetry for breakpoint, churn, and cost-leak patterns.
    #[command(name = "cache-audit")]
    CacheAudit { session: String },
    /// Show bounded append-only cache-break history (never message text).
    #[command(name = "cache-breaks")]
    CacheBreaks {
        session: String,
        /// Newest-first cap (1-200).
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Preview per-role prompt-cache plans and component hashes without a
    /// provider call.
    #[command(name = "cache-plan")]
    CachePlan {
        session: String,
        /// Role to preview. Repeatable; defaults to leader.
        #[arg(long)]
        role: Vec<String>,
    },
    #[command(name = "cache-warm")]
    CacheWarm {
        session: String,
        /// Role whose stable prompt/tool bundle to warm: leader, manager,
        /// worker, verifier, labour, or scout. Defaults to leader.
        #[arg(long)]
        role: Option<String>,
    },
    /// List the static tool-bundle matrix without a session.
    #[command(name = "tool-bundles")]
    ToolBundles {
        /// Evaluate every standard role/profile/task-kind combination against
        /// the configured registry instead of printing the static catalog.
        #[arg(long)]
        matrix: bool,
        /// Task kinds to evaluate with --matrix. Repeatable; defaults to the
        /// built-in deterministic set.
        #[arg(long)]
        task_kind: Vec<String>,
        /// Diff two bundles as FROM,TO, for example leader,minimal.
        #[arg(long, value_name = "FROM,TO")]
        diff: Option<String>,
        /// Role used with --diff. Defaults to leader.
        #[arg(long)]
        role: Option<String>,
        /// Profile used with --diff. Defaults to the role preview profile.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Inspect the frozen tool bundle for a session. Read-only: it does not
    /// call a provider or start a run.
    #[command(name = "tool-bundle")]
    ToolBundle {
        session: String,
        /// Role to inspect. Repeatable; defaults to every agent in the session
        /// plus the leader.
        #[arg(long)]
        role: Vec<String>,
        /// Plan task kind used to partition a Worker/Labour bundle, for
        /// example implementation, verification, research, or trivial.
        #[arg(long)]
        task_kind: Option<String>,
        /// Include per-tool names and schema hashes.
        #[arg(long)]
        tools: bool,
        /// Show the bounded persisted bundle-observation history instead of
        /// the current preview.
        #[arg(long)]
        history: bool,
    },
    /// Show the project's prompt-cache policy.
    CachePolicy { project: String },
    /// Update the project's prompt-cache policy.
    CachePolicySet {
        project: String,
        /// Disable prompt caching for this project.
        #[arg(long)]
        disabled: bool,
        /// Anthropic-compatible TTL pool: 5m or 1h.
        #[arg(long, default_value = "5m")]
        ttl: String,
        /// Disable the provider-managed moving breakpoint.
        #[arg(long)]
        no_automatic: bool,
        /// Opt into one warm call before resuming a journaled run.
        #[arg(long)]
        warm_on_resume: bool,
        /// Disable compaction cache-generation and invalidation telemetry.
        #[arg(long)]
        no_compact_aware: bool,
        /// Per-role TTL override, for example `worker=1h`. Repeatable.
        #[arg(long = "role-ttl", value_name = "ROLE=TTL")]
        role_ttl: Vec<String>,
        /// Prompt layout for newly created sessions: 2 (legacy) or 3
        /// (instruction store). Existing sessions keep their own layout.
        #[arg(long)]
        prompt_layout: Option<u32>,
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
        /// Force lexical-only ranking for this request, even when the project
        /// has an enabled semantic lane. Performs no embedding request.
        #[arg(long = "lexical-only")]
        lexical_only: bool,
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
    /// Show semantic embedding coverage and the last indexing error.
    MemoryEmbeddings { project: String },
    /// Clear derived embeddings. When the lane is enabled, also queue every
    /// active memory for re-indexing.
    MemoryEmbeddingsRebuild { project: String },
    /// Probe the configured embedding endpoint with one request.
    MemoryEmbeddingsTest {
        project: String,
        #[arg(long)]
        text: Option<String>,
    },
    /// Show derived IVF index mode, generation, coverage, and fallback state.
    MemoryEmbeddingsIndex { project: String },
    /// Rebuild the derived IVF index from stored vectors. Never calls the
    /// embedding endpoint and never changes memory truth.
    MemoryEmbeddingsIndexRebuild { project: String },
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
    /// Update the opt-in project memory retention policy. Only explicitly
    /// supplied flags are changed; embedding-only updates keep retention.
    MemoryPolicySet {
        project: String,
        /// Explicitly enable automatic memory pruning.
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
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
        /// Enable the optional semantic lane. Endpoint and model must already
        /// be set or supplied in this command.
        #[arg(long = "embedding-enable")]
        embedding_enable: bool,
        /// Disable the semantic lane without clearing its configuration.
        #[arg(long = "embedding-disable")]
        embedding_disable: bool,
        /// OpenAI-compatible embeddings base URL (or full /embeddings URL).
        #[arg(long = "embedding-endpoint")]
        embedding_endpoint: Option<String>,
        /// Embedding model name. There is no hidden model selection.
        #[arg(long = "embedding-model")]
        embedding_model: Option<String>,
        /// Vector dimensions expected from the endpoint.
        #[arg(long = "embedding-dimensions")]
        embedding_dimensions: Option<u32>,
        /// Maximum texts per indexing request (1-64).
        #[arg(long = "embedding-batch-size")]
        embedding_batch_size: Option<u32>,
        /// Per-request timeout in seconds (1-300).
        #[arg(long = "embedding-timeout-secs")]
        embedding_timeout_secs: Option<u64>,
        /// Maximum semantic candidates scored per recall (1-256).
        #[arg(long = "embedding-max-candidates")]
        embedding_max_candidates: Option<u32>,
        /// Minimum cosine similarity to contribute (0-1).
        #[arg(long = "embedding-min-similarity")]
        embedding_min_similarity: Option<f32>,
        /// Environment variable name holding the API key. The value is never
        /// stored or printed.
        #[arg(long = "embedding-api-key-env")]
        embedding_api_key_env: Option<String>,
        /// Retrieval index mode: exact bounded scan or opt-in IVF.
        #[arg(long = "embedding-index", value_parser = ["scan", "ivf"])]
        embedding_index: Option<String>,
        /// IVF centroid count (4-1024).
        #[arg(long = "ivf-nlist")]
        ivf_nlist: Option<u32>,
        /// IVF centroids probed per recall (1..=nlist).
        #[arg(long = "ivf-nprobe")]
        ivf_nprobe: Option<u32>,
        /// Below this many vectors recall uses the exact scan.
        #[arg(long = "ivf-min-vectors")]
        ivf_min_vectors: Option<u32>,
        /// Vector growth that triggers a deterministic centroid rebuild.
        #[arg(long = "ivf-rebuild-vectors")]
        ivf_rebuild_vectors: Option<u32>,
        /// Remove the entire embedding policy object.
        #[arg(long = "embedding-clear")]
        embedding_clear: bool,
    },
    /// Show the project tool-output spill and paging policy.
    ToolOutputPolicy { project: String },
    /// Update tool-output spill thresholds and retention.
    ToolOutputPolicySet {
        project: String,
        /// Disable spilling while keeping legacy bounded output.
        #[arg(long)]
        disable: bool,
        /// Re-enable spilling.
        #[arg(long)]
        enable: bool,
        /// Spill results larger than this many bytes.
        #[arg(long)]
        spill_threshold: Option<u64>,
        /// Provider-facing preview body size in bytes.
        #[arg(long)]
        preview_bytes: Option<u64>,
        /// Aggregate preview budget per assistant turn.
        #[arg(long)]
        max_turn_bytes: Option<u64>,
        /// Handle retention in seconds.
        #[arg(long)]
        retention_secs: Option<u64>,
        /// Maximum stored handles per project.
        #[arg(long)]
        max_entries: Option<u64>,
        /// Per-tool spill threshold as TOOL=BYTES (repeatable).
        #[arg(long = "tool-threshold")]
        tool_thresholds: Vec<String>,
    },
    /// List daemon-owned tool-output handles (metadata only).
    ToolOutputs {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        tool: Option<String>,
        #[arg(long)]
        before: Option<String>,
        #[arg(long)]
        include_expired: bool,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Show tool-output store counts and bytes.
    ToolOutputStatus {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Prune expired or excess tool-output handles.
    ToolOutputPrune {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        older_than_secs: Option<u64>,
        #[arg(long)]
        max_entries: Option<u64>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Verify tool-output metadata and blob integrity.
    ToolOutputAudit {
        #[arg(long)]
        project: Option<String>,
        /// Re-hash every selected blob (slow).
        #[arg(long)]
        verify_hashes: bool,
        /// Remove blob files with no metadata row.
        #[arg(long)]
        clean_orphans: bool,
    },
    /// Show the project test-result cache policy and counters.
    TestCache { project: String },
    /// List cached test/lint/check metadata (never output bodies).
    TestCacheEntries {
        project: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        include_expired: bool,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Update the project test-result cache policy.
    TestCachePolicySet {
        project: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        enabled: Option<bool>,
        #[arg(long)]
        max_entries: Option<u32>,
        #[arg(long)]
        retention_days: Option<u32>,
        /// Maximum stored output body size in bytes (4096–8388608).
        #[arg(long)]
        max_output_bytes: Option<u64>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        reuse_lint: Option<bool>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        include_integration: Option<bool>,
        #[arg(long)]
        failed_retention_secs: Option<u64>,
        /// Extra environment variable name included in the cache key (repeatable).
        #[arg(long = "extra-env")]
        extra_env: Vec<String>,
        /// Extra worktree-relative file included in the cache key (repeatable).
        #[arg(long = "extra-path")]
        extra_path: Vec<String>,
        /// Replace the extra env/path lists instead of appending.
        #[arg(long)]
        replace_extra_scopes: bool,
    },
    /// Preview operator-declared external cache scopes without running a task.
    TestCacheScopes { project: String },
    /// Prune cached test-result metadata by age, expiry, or kind.
    TestCachePrune {
        project: String,
        #[arg(long)]
        older_than_secs: Option<u64>,
        #[arg(long)]
        expired_only: bool,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Clear cached test-result metadata. Requires --yes.
    TestCacheClear {
        project: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        /// Explicit confirmation; clearing cannot be undone.
        #[arg(long)]
        yes: bool,
    },
    /// Preview the bounded changed-path test impact map for a session.
    TestImpact {
        session: String,
        /// Changed path (repeatable). Defaults to git status.
        #[arg(long = "path")]
        paths: Vec<String>,
        #[arg(long, default_value_t = 32)]
        limit: u32,
        /// Exclude historical cache rows.
        #[arg(long)]
        no_history: bool,
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
    /// List entries in a zip/tar/tar.gz artifact with safe flags and caps.
    ArtifactArchiveList {
        artifact: String,
        #[arg(long)]
        max_entries: Option<usize>,
    },
    /// Extract selected safe archive entries into daemon scratch and publish artifacts.
    ArtifactArchiveExtract {
        artifact: String,
        /// Normalized entry path (repeatable).
        #[arg(long = "entry")]
        entries: Vec<String>,
        /// Select every safe regular file.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        max_entries: Option<usize>,
        /// Omit bounded text/hex previews.
        #[arg(long)]
        no_previews: bool,
    },
    /// Probe a media artifact with ffprobe (or still-image metadata fallback).
    MediaInfo { artifact: String },
    /// Explicitly transcode a media artifact into a bounded artifact. Requires --yes.
    MediaTranscode {
        artifact: String,
        /// One of mp4, webm, mp3, wav, png, jpeg.
        format: String,
        #[arg(long, default_value_t = 60)]
        max_duration_secs: u64,
        #[arg(long)]
        max_bytes: Option<u64>,
        /// Explicit confirmation of the host ffmpeg operation.
        #[arg(long)]
        yes: bool,
    },
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
    /// Gitea/Forgejo tea, or Bitbucket Cloud REST). Provider selection falls
    /// back to VERLINPARK_PR_PROVIDER and then origin detection.
    Pr {
        session: String,
        /// Force one provider instead of environment/origin detection.
        #[arg(long, value_enum)]
        provider: Option<ForgeProviderArg>,
    },
    /// Inspect pull-request forge providers and credential sources.
    Forge {
        #[command(subcommand)]
        action: ForgeAction,
    },
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
    RestoreLog {
        session: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Discard uncommitted changes in the session worktree.
    Discard {
        session: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Commit the session worktree.
    Commit {
        session: String,
        #[arg(short, long)]
        message: String,
    },
    /// Show the session worktree diff.
    Diff { session: String },
    /// List pending approvals from the attention-ordered queue.
    Approvals {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        project: Option<String>,
    },
    /// Resolve an explicit list of approvals, one decision per id.
    ApprovalBatch {
        /// Approve this approval id (repeatable).
        #[arg(long = "approve")]
        approve: Vec<String>,
        /// Deny this approval id (repeatable).
        #[arg(long = "deny")]
        deny: Vec<String>,
    },
    /// List project approval templates.
    ApprovalTemplate {
        #[arg(long)]
        project: Option<String>,
    },
    /// Create or replace a project approval template.
    ApprovalTemplateSet {
        #[arg(long)]
        project: Option<String>,
        /// Existing template id. Omit to create.
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// Exact tool, prefix (`write_*`), or bounded `*`/`?` glob.
        #[arg(long = "tool-pattern")]
        tool_pattern: Option<String>,
        /// Known risk name (repeatable; defaults to external/workspace_write).
        #[arg(long = "risk")]
        risk: Vec<String>,
        /// Relative path glob (repeatable).
        #[arg(long = "path-glob")]
        path_glob: Vec<String>,
        /// allow, deny, or escalate.
        #[arg(long)]
        action: Option<String>,
        /// RFC3339 expiry.
        #[arg(long = "expires-at")]
        expires_at: Option<String>,
        #[arg(long = "max-uses")]
        max_uses: Option<u32>,
        /// Enable the template (create default; update preserves unless set).
        #[arg(long, conflicts_with = "disabled")]
        enabled: bool,
        /// Disable the template (update preserves unless set; create honors).
        #[arg(long)]
        disabled: bool,
    },
    /// Delete an approval template by id.
    ApprovalTemplateDelete { id: String },
    /// Show the project approval attention policy.
    ApprovalPolicy {
        #[arg(long)]
        project: String,
    },
    /// Update the project approval attention policy.
    ApprovalPolicySet {
        #[arg(long)]
        project: String,
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long = "max-pending")]
        max_pending: Option<u32>,
        #[arg(long = "coalesce-secs")]
        coalesce_secs: Option<u64>,
        #[arg(long = "stale-secs")]
        stale_secs: Option<u64>,
        #[arg(long = "priority-boost-risk")]
        priority_boost_risk: Option<String>,
        #[arg(long = "notify-coalesce-secs")]
        notify_coalesce_secs: Option<u64>,
    },
    /// List project approval audit rows (template matches and mutations).
    ApprovalAudit {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = 100)]
        limit: u32,
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
