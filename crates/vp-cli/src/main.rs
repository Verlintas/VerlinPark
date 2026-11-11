use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::{Parser, Subcommand, ValueEnum};
use fs2::FileExt;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use vp_core::backup::anchor::AnchorStore;
use vp_core::backup::repository::{
    Compression, KdfConfig, Passphrase, RepositoryBackupOptions, RepositoryEncryption,
    RepositoryRestoreOptions, RepositorySigningOptions, RepositoryVerifyOptions, verify_repository,
};
use vp_core::backup::signing::{Keyring, SecretSource, default_key_dir};
use vp_protocol::discovery::Discovery;
use vp_protocol::remote::RemoteIntegrationPolicy;
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AcpTransportArg {
    Stdio,
    StreamableHttp,
    Websocket,
}

impl AcpTransportArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable_http",
            Self::Websocket => "websocket",
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
        /// Replace the additional sensitive lists instead of appending.
        #[arg(long)]
        replace_sensitive: bool,
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
    /// Stop a durable supervised process (SIGTERM, bounded wait, SIGKILL).
    ProcessStop {
        session: String,
        process: String,
        #[arg(long, default_value = "TERM")]
        signal: String,
        #[arg(long, default_value_t = 5)]
        grace_secs: u32,
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
    /// Show session cache-read telemetry and the prompt-prefix generation.
    Cache { session: String },
    /// Explicitly warm the session's stable prompt prefix and tool bundle.
    /// Audit cache telemetry for breakpoint, churn, and cost-leak patterns.
    #[command(name = "cache-audit")]
    CacheAudit { session: String },
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
    },
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
        /// Disable the template (update only).
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

/// Accept both `vp cache warm <session>` and `vp cache-warm <session>`.
/// The status command keeps its historical `vp cache <session>` shape, so the
/// two-word warm spelling is normalized before clap parses positional args.
fn normalize_cache_warm_args(mut args: Vec<String>) -> Vec<String> {
    if args.get(1).map(String::as_str) == Some("cache")
        && args.get(2).map(String::as_str) == Some("warm")
    {
        args.remove(2);
        args[1] = "cache-warm".to_string();
    }
    args
}

/// The generated clap parser and the command dispatch future have both grown
/// with the CLI surface. Run them on a dedicated, generous stack instead of the
/// platform's default main-thread stack; the tokio workers get the same budget.
const CLI_STACK_BYTES: usize = 32 * 1024 * 1024;

fn main() -> Result<()> {
    let worker = std::thread::Builder::new()
        .name("vp-cli".into())
        .stack_size(CLI_STACK_BYTES)
        .spawn(|| -> Result<()> {
            let cli = Cli::parse_from(normalize_cache_warm_args(std::env::args().collect()));
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(CLI_STACK_BYTES)
                .build()?
                .block_on(run(cli))
        })?;
    match worker.join() {
        Ok(result) => result,
        Err(_) => anyhow::bail!("vp CLI thread panicked"),
    }
}

async fn run(cli: Cli) -> Result<()> {
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
        repository,
        include_artifacts,
        incremental,
        plaintext,
        passphrase_env,
        passphrase_file,
        no_compress,
        signing_key,
        sign,
        keyring_dir,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        match (output, repository) {
            (Some(output), None) => {
                if *incremental
                    || *plaintext
                    || passphrase_env.is_some()
                    || passphrase_file.is_some()
                    || *no_compress
                    || signing_key.is_some()
                    || *sign
                    || keyring_dir.is_some()
                {
                    bail!(
                        "--incremental, --plaintext, --passphrase-env, --passphrase-file, --no-compress, --signing-key, --sign, and --keyring-dir are repository-only options; use --repository"
                    );
                }
                let report = vp_core::backup::create_backup(&data_dir, output, *include_artifacts)?;
                let value = serde_json::to_value(report)?;
                if cli.json {
                    print_json(&value)?;
                } else {
                    print_human(&value);
                }
            }
            (None, Some(repository)) => {
                let passphrase =
                    resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
                let encryption = match (plaintext, passphrase) {
                    (true, Some(_)) => {
                        bail!("--plaintext conflicts with a repository passphrase")
                    }
                    (true, None) => RepositoryEncryption::Plaintext,
                    (false, Some(passphrase)) => RepositoryEncryption::Encrypted(passphrase),
                    (false, None) => bail!(
                        "repository backups are encrypted by default; pass --passphrase-env NAME or --passphrase-file PATH, or --plaintext to write an unencrypted repository"
                    ),
                };
                let options = RepositoryBackupOptions {
                    incremental: *incremental,
                    include_artifacts: *include_artifacts,
                    compression: if *no_compress {
                        Compression::None
                    } else {
                        Compression::Zstd
                    },
                    encryption,
                    kdf: KdfConfig::default(),
                    signing: resolve_signing_options(
                        keyring_dir.as_deref(),
                        signing_key.as_deref(),
                        *sign,
                    )?,
                };
                let report = vp_core::backup::repository::create_repository_backup(
                    &data_dir, repository, options,
                )?;
                let value = serde_json::to_value(report)?;
                if cli.json {
                    print_json(&value)?;
                } else {
                    print_repository_backup(&value);
                }
            }
            (Some(_), Some(_)) => {
                bail!("pass exactly one of --output or --repository")
            }
            (None, None) => {
                bail!(
                    "pass --output <zip> for a v1 backup or --repository <dir> for a v2 repository"
                )
            }
        }
        return Ok(());
    }
    if let Command::Restore {
        input,
        repository,
        snapshot,
        force,
        passphrase_env,
        passphrase_file,
        keyring_dir,
        allow_rollback,
        anchor_file,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        match (input, repository) {
            (Some(input), None) => {
                if snapshot.is_some()
                    || passphrase_env.is_some()
                    || passphrase_file.is_some()
                    || keyring_dir.is_some()
                    || *allow_rollback
                    || anchor_file.is_some()
                {
                    bail!(
                        "--snapshot, --passphrase-env, --passphrase-file, --keyring-dir, --allow-rollback, and --anchor-file are repository-only options; use --repository"
                    );
                }
                let report = vp_core::backup::restore_backup(input, &data_dir, *force)?;
                let value = serde_json::to_value(report)?;
                if cli.json {
                    print_json(&value)?;
                } else {
                    print_human(&value);
                }
            }
            (None, Some(repository)) => {
                let snapshot = snapshot.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("--repository restore requires --snapshot <id>")
                })?;
                let passphrase =
                    resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
                let keyring = load_keyring_optional(keyring_dir.as_deref())?;
                let anchor = anchor_store(&data_dir, anchor_file.as_deref());
                let report = vp_core::backup::repository::restore_repository_snapshot_checked(
                    repository,
                    snapshot,
                    &data_dir,
                    *force,
                    RepositoryRestoreOptions {
                        passphrase: passphrase.as_ref(),
                        keyring: keyring.as_ref(),
                        anchor: Some(&anchor),
                        allow_rollback: *allow_rollback,
                    },
                )?;
                let value = serde_json::to_value(report)?;
                if cli.json {
                    print_json(&value)?;
                } else {
                    print_repository_restore(&value);
                }
            }
            (Some(_), Some(_)) => bail!("pass exactly one of --input or --repository"),
            (None, None) => bail!(
                "pass --input <zip> for a v1 restore or --repository <dir> --snapshot <id> for a repository restore"
            ),
        }
        return Ok(());
    }
    if let Command::BackupList {
        repository,
        verify,
        passphrase_env,
        passphrase_file,
        keyring_dir,
    } = &cli.command
    {
        let passphrase = resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
        let keyring = load_keyring_optional(keyring_dir.as_deref())?;
        let report = vp_core::backup::repository::list_repository_snapshots_with_keyring(
            repository,
            passphrase.as_ref(),
            keyring.as_ref(),
            *verify,
        )?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_repository_list(&value);
        }
        return Ok(());
    }
    if let Command::BackupPrune {
        repository,
        keep,
        dry_run,
        passphrase_env,
        passphrase_file,
        keyring_dir,
        signing_key,
        sign,
    } = &cli.command
    {
        let passphrase = resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
        let signing =
            resolve_signing_options(keyring_dir.as_deref(), signing_key.as_deref(), *sign)?;
        let fallback_keyring;
        let keyring = match signing.as_ref() {
            Some(signing) => Some(&signing.keyring),
            None => {
                fallback_keyring = load_keyring_optional(None)?;
                fallback_keyring.as_ref()
            }
        };
        let credential = match signing.as_ref() {
            Some(signing) => Some(
                signing
                    .keyring
                    .signing_credential(signing.key_id.as_deref())?,
            ),
            None => None,
        };
        let report = vp_core::backup::repository::prune_repository_with_signing(
            repository,
            *keep,
            *dry_run,
            passphrase.as_ref(),
            keyring,
            credential.as_ref(),
        )?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_repository_prune(&value);
        }
        return Ok(());
    }
    if let Command::BackupVerify {
        repository,
        passphrase_env,
        passphrase_file,
        keyring_dir,
        allow_rollback,
        anchor_file,
        presence_only,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let passphrase = resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
        let keyring = load_keyring_optional(keyring_dir.as_deref())?;
        let anchor = anchor_store(&data_dir, anchor_file.as_deref());
        let report = verify_repository(
            repository,
            RepositoryVerifyOptions {
                passphrase: passphrase.as_ref(),
                keyring: keyring.as_ref(),
                anchor: Some(&anchor),
                allow_rollback: *allow_rollback,
                verify_blobs: !*presence_only,
            },
        )?;
        let value = serde_json::to_value(report)?;
        let valid = value["valid"].as_bool().unwrap_or(false);
        if cli.json {
            print_json(&value)?;
        } else {
            print_repository_verify(&value);
        }
        if !valid {
            return Err(vp_core::backup::error::BackupSecurityError::VerificationFailed.into());
        }
        return Ok(());
    }
    if let Command::BackupKeyGenerate {
        key_id,
        keyring_dir,
        inactive,
        from_env,
    } = &cli.command
    {
        let dir = keyring_dir.clone().unwrap_or_else(default_key_dir);
        let report = match from_env {
            Some(variable) => {
                Keyring::register_environment_key(&dir, key_id, variable, !*inactive)?
            }
            None => Keyring::generate_key(
                &dir,
                key_id,
                !*inactive,
                SecretSource::File {
                    name: format!("{key_id}.key"),
                },
            )?,
        };
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_key_generation(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyList { keyring_dir } = &cli.command {
        let summary =
            load_keyring_optional(keyring_dir.as_deref())?.map(|keyring| keyring.summary());
        let value = match summary {
            Some(summary) => serde_json::to_value(summary)?,
            None => json!({
                "path": keyring_dir.clone().unwrap_or_else(default_key_dir).join("keyring.json").display().to_string(),
                "active_key_id": null,
                "key_count": 0,
                "keys": [],
                "present": false,
            }),
        };
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_keyring(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyRotate {
        key_id,
        keyring_dir,
        use_existing,
        from_env,
    } = &cli.command
    {
        let dir = keyring_dir.clone().unwrap_or_else(default_key_dir);
        let report = if let Some(variable) = from_env {
            Keyring::register_environment_key(&dir, key_id, variable, false)?;
            let mut keyring = Keyring::load(&dir)?;
            keyring.rotate_to(key_id)?
        } else if *use_existing {
            let mut keyring = Keyring::load(&dir)?;
            keyring.rotate_to(key_id)?
        } else {
            Keyring::generate_key(
                &dir,
                key_id,
                false,
                SecretSource::File {
                    name: format!("{key_id}.key"),
                },
            )?;
            let mut keyring = Keyring::load(&dir)?;
            keyring.rotate_to(key_id)?
        };
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_key_rotation(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyExportPublic {
        keyring_dir,
        key_id,
        output,
    } = &cli.command
    {
        let keyring = load_keyring_required(keyring_dir.as_deref())?;
        let export = keyring.export_public(key_id.as_deref())?;
        let value = serde_json::to_value(export)?;
        if let Some(output) = output {
            if output.exists() {
                bail!("public key output already exists at {}", output.display());
            }
            let bytes = serde_json::to_vec_pretty(&value)?;
            let parent = output.parent().unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent)?;
            std::fs::write(output, bytes)?;
            if cli.json {
                print_json(&value)?;
            } else {
                println!(
                    "exported {} public key to {}",
                    value["key_id"].as_str().unwrap_or("?"),
                    output.display()
                );
            }
        } else if cli.json {
            print_json(&value)?;
        } else {
            println!(
                "{}  {}  fingerprint {}",
                value["key_id"].as_str().unwrap_or("?"),
                value["public_key_b64"].as_str().unwrap_or("?"),
                value["fingerprint"].as_str().unwrap_or("?")
            );
        }
        return Ok(());
    }
    if let Command::BackupKeyImport {
        key_id,
        keyring_dir,
        seed_file,
        from_env,
        inactive,
    } = &cli.command
    {
        let dir = keyring_dir.clone().unwrap_or_else(default_key_dir);
        let report = match (seed_file, from_env) {
            (Some(path), None) => Keyring::import_key_file(&dir, key_id, path, !*inactive)?,
            (None, Some(variable)) => {
                Keyring::register_environment_key(&dir, key_id, variable, !*inactive)?
            }
            _ => bail!("pass exactly one of --seed-file or --from-env"),
        };
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_key_generation(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyring { keyring_dir } = &cli.command {
        let summary =
            load_keyring_optional(keyring_dir.as_deref())?.map(|keyring| keyring.summary());
        let value = match summary {
            Some(summary) => serde_json::to_value(summary)?,
            None => json!({
                "path": keyring_dir.clone().unwrap_or_else(default_key_dir).join("keyring.json").display().to_string(),
                "active_key_id": null,
                "key_count": 0,
                "keys": [],
                "present": false,
            }),
        };
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_keyring(&value);
        }
        return Ok(());
    }
    if let Command::BackupAnchor { anchor_file, reset } = &cli.command {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let anchor = anchor_store(&data_dir, anchor_file.as_deref());
        if *reset {
            let removed = anchor.reset()?;
            let value = json!({"reset": removed, "path": anchor.path().display().to_string()});
            if cli.json {
                print_json(&value)?;
            } else {
                println!(
                    "{} {}",
                    if removed { "removed" } else { "no anchor at" },
                    anchor.path().display()
                );
            }
            return Ok(());
        }
        let summary = anchor.summary()?;
        let value = serde_json::to_value(summary)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_anchor(&value);
        }
        return Ok(());
    }
    if let Command::ToolCodes {
        category,
        retryable,
    } = &cli.command
    {
        let entries = filtered_tool_codes(category.as_deref(), *retryable);
        if cli.json {
            print_json(&serde_json::to_value(&entries)?)?;
        } else if entries.is_empty() {
            println!("No matching tool error codes.");
        } else {
            for entry in entries {
                println!(
                    "{:<28} {:<10} {:<9} {}",
                    entry.code.as_str(),
                    entry.category,
                    if entry.retryable {
                        "retryable"
                    } else {
                        "terminal"
                    },
                    entry.hint
                );
            }
        }
        return Ok(());
    }
    let discovery = read_discovery(cli.data_dir.as_deref())?;
    let want_tool_activity_csv = matches!(&cli.command, Command::ToolActivity { csv: true, .. });
    let value = match cli.command {
        Command::DbRebuild { .. } => unreachable!("db rebuild is handled before discovery"),
        Command::Backup { .. } => unreachable!("backup is handled before discovery"),
        Command::Restore { .. } => unreachable!("restore is handled before discovery"),
        Command::BackupList { .. } => unreachable!("backup-list is handled before discovery"),
        Command::BackupPrune { .. } => unreachable!("backup-prune is handled before discovery"),
        Command::BackupVerify { .. } => unreachable!("backup-verify is handled before discovery"),
        Command::BackupKeyGenerate { .. } => {
            unreachable!("backup-key-generate is handled before discovery")
        }
        Command::BackupKeyList { .. } => {
            unreachable!("backup-key-list is handled before discovery")
        }
        Command::BackupKeyRotate { .. } => {
            unreachable!("backup-key-rotate is handled before discovery")
        }
        Command::BackupKeyExportPublic { .. } => {
            unreachable!("backup-key-export-public is handled before discovery")
        }
        Command::BackupKeyImport { .. } => {
            unreachable!("backup-key-import is handled before discovery")
        }
        Command::BackupKeyring { .. } => unreachable!("backup-keyring is handled before discovery"),
        Command::BackupAnchor { .. } => unreachable!("backup-anchor is handled before discovery"),
        Command::Bench { .. } => unreachable!("bench is handled before discovery"),
        Command::ToolCodes { .. } => unreachable!("tool-codes is handled before discovery"),
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
            transport,
            endpoint,
            auth_header_env,
            headers,
            allow_remote,
            connect_timeout,
            args,
            env,
            auto_approve,
            timeout,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_pairs(&headers, "--header")?;
            rpc(
                &discovery,
                "acp.agent.add",
                json!({
                    "name": name,
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "enabled": true,
                    "auto_approve": auto_approve,
                    "timeout_secs": timeout,
                    "transport": transport.as_str(),
                    "endpoint": endpoint,
                    "auth_header_env": auth_header_env,
                    "headers": headers,
                    "connect_timeout_secs": connect_timeout,
                    "allow_remote": allow_remote
                }),
            )
            .await?
        }
        Command::AcpUpdate {
            agent,
            name,
            command,
            transport,
            endpoint,
            clear_endpoint,
            auth_header_env,
            clear_auth_header_env,
            headers,
            clear_headers,
            allow_remote,
            no_allow_remote,
            connect_timeout,
            args,
            env,
            auto_approve,
            no_auto_approve,
            enabled,
            disabled,
            timeout,
        } => {
            if clear_endpoint && endpoint.is_some() {
                bail!("use either --endpoint or --clear-endpoint, not both");
            }
            if clear_auth_header_env && auth_header_env.is_some() {
                bail!("use either --auth-header-env or --clear-auth-header-env, not both");
            }
            if clear_headers && !headers.is_empty() {
                bail!("use either --header or --clear-headers, not both");
            }
            if allow_remote && no_allow_remote {
                bail!("use either --allow-remote or --no-allow-remote, not both");
            }
            if auto_approve && no_auto_approve {
                bail!("use either --auto-approve or --no-auto-approve, not both");
            }
            if enabled && disabled {
                bail!("use either --enabled or --disabled, not both");
            }
            let current = rpc(&discovery, "acp.agent.list", json!({})).await?;
            let agent_json = current
                .get("agents")
                .and_then(Value::as_array)
                .and_then(|agents| {
                    agents.iter().find(|candidate| {
                        candidate.get("id").and_then(Value::as_str) == Some(agent.as_str())
                            || candidate.get("name").and_then(Value::as_str) == Some(agent.as_str())
                    })
                })
                .cloned()
                .with_context(|| format!("ACP agent not found: {agent}"))?;
            let existing_transport = agent_json
                .get("transport")
                .and_then(Value::as_str)
                .unwrap_or("stdio");
            let transport = transport
                .map(AcpTransportArg::as_str)
                .unwrap_or(existing_transport)
                .to_string();
            let existing_args = agent_json.get("args").cloned().unwrap_or_else(|| json!([]));
            let existing_env = agent_json.get("env").cloned().unwrap_or_else(|| json!({}));
            let existing_headers = agent_json
                .get("headers")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let env = if env.is_empty() {
                existing_env
            } else {
                json!(parse_env_pairs(&env)?)
            };
            let headers = if !headers.is_empty() {
                json!(parse_pairs(&headers, "--header")?)
            } else if clear_headers {
                json!({})
            } else {
                existing_headers
            };
            let endpoint = if clear_endpoint {
                Value::Null
            } else if let Some(endpoint) = endpoint {
                json!(endpoint)
            } else {
                agent_json.get("endpoint").cloned().unwrap_or(Value::Null)
            };
            let auth_header_env = if clear_auth_header_env {
                Value::Null
            } else if let Some(name) = auth_header_env {
                json!(name)
            } else {
                agent_json
                    .get("auth_header_env")
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            let allow_remote = if allow_remote {
                true
            } else if no_allow_remote {
                false
            } else {
                agent_json
                    .get("allow_remote")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            let auto_approve = if auto_approve {
                true
            } else if no_auto_approve {
                false
            } else {
                agent_json
                    .get("auto_approve")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            let enabled = if enabled {
                true
            } else if disabled {
                false
            } else {
                agent_json
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            };
            rpc(
                &discovery,
                "acp.agent.update",
                json!({
                    "id": agent_json.get("id").cloned().unwrap_or(Value::Null),
                    "name": name.unwrap_or_else(|| {
                        agent_json
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }),
                    "command": command.unwrap_or_else(|| {
                        agent_json
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }),
                    "args": if args.is_empty() { existing_args } else { json!(args) },
                    "env": env,
                    "enabled": enabled,
                    "auto_approve": auto_approve,
                    "timeout_secs": timeout.unwrap_or_else(|| {
                        agent_json
                            .get("timeout_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(1800)
                    }),
                    "transport": transport,
                    "endpoint": endpoint,
                    "auth_header_env": auth_header_env,
                    "headers": headers,
                    "connect_timeout_secs": connect_timeout.unwrap_or_else(|| {
                        agent_json
                            .get("connect_timeout_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(30)
                    }),
                    "allow_remote": allow_remote
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
        Command::AcpProbe {
            agent,
            project,
            no_session,
        } => {
            rpc(
                &discovery,
                "acp.agent.probe",
                json!({
                    "agent_id": agent,
                    "project_id": project,
                    "create_session": !no_session
                }),
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
        Command::Lsp { project } => {
            rpc(&discovery, "lsp.status", json!({"project_id": project})).await?
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
        Command::RemoteIntegrate {
            project,
            session,
            branch,
            host,
            expected_commit,
            base_commit,
            task,
            objective,
            confirm,
            keep_import_refs,
        } => {
            rpc(
                &discovery,
                "remote.integrate",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "branch": branch,
                    "host": host,
                    "expected_commit": expected_commit,
                    "base_commit": base_commit,
                    "task_id": task,
                    "objective": objective,
                    "confirm": confirm,
                    "keep_import_refs": if keep_import_refs { Some(true) } else { None },
                }),
            )
            .await?
        }
        Command::RemoteIntegration { project, session } => {
            rpc(
                &discovery,
                "remote.integration.status",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::RemoteIntegrationCleanup {
            project,
            session,
            entry,
            force,
        } => {
            rpc(
                &discovery,
                "remote.integration.cleanup",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "entry_id": entry,
                    "force": force,
                }),
            )
            .await?
        }
        Command::RemoteIntegrationPolicy { project } => {
            rpc(
                &discovery,
                "remote.integration.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RemoteIntegrationPolicySet {
            project,
            enable,
            disable,
            max_bundle_bytes,
            require_verifier,
            no_require_verifier,
            allowed_hosts,
            clear_hosts,
            keep_import_refs,
            no_keep_import_refs,
            confirm,
        } => {
            if enable && disable {
                bail!("use either --enable or --disable, not both");
            }
            if require_verifier && no_require_verifier {
                bail!("use either --require-verifier or --no-require-verifier, not both");
            }
            if keep_import_refs && no_keep_import_refs {
                bail!("use either --keep-import-refs or --no-keep-import-refs, not both");
            }
            let current = rpc(
                &discovery,
                "remote.integration.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy: RemoteIntegrationPolicy = serde_json::from_value(
                current
                    .get("policy")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("policy response is missing `policy`"))?,
            )
            .context("decoding the current remote integration policy")?;
            let mut changed = false;
            if enable {
                policy.enabled = true;
                changed = true;
            }
            if disable {
                policy.enabled = false;
                changed = true;
            }
            if let Some(bytes) = max_bundle_bytes {
                policy.max_bundle_bytes = bytes;
                changed = true;
            }
            if require_verifier {
                policy.require_verifier = true;
                changed = true;
            }
            if no_require_verifier {
                policy.require_verifier = false;
                changed = true;
            }
            if clear_hosts {
                policy.allowed_hosts.clear();
                changed = true;
            }
            if !allowed_hosts.is_empty() {
                policy.allowed_hosts = allowed_hosts;
                changed = true;
            }
            if keep_import_refs {
                policy.keep_import_refs = true;
                changed = true;
            }
            if no_keep_import_refs {
                policy.keep_import_refs = false;
                changed = true;
            }
            if !changed {
                bail!(
                    "no policy change requested; pass --enable/--disable, --max-bundle-bytes, verifier flags, --allow-host, or keep-ref flags"
                );
            }
            policy
                .validate()
                .map_err(|error| anyhow::anyhow!("invalid remote integration policy: {error}"))?;
            rpc(
                &discovery,
                "remote.integration.policy.set",
                json!({
                    "project_id": project,
                    "policy": policy,
                    "confirm": confirm,
                }),
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
            mode,
        } => {
            rpc(
                &discovery,
                "source.search",
                json!({
                    "project_id": project,
                    "query": query,
                    "collections": collections,
                    "labels": labels,
                    "limit": limit,
                    "mode": mode
                }),
            )
            .await?
        }
        Command::SourceEmbedding { project } => {
            rpc(
                &discovery,
                "source.embedding.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingReindex { project } => {
            rpc(
                &discovery,
                "source.embedding.reindex",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingPolicy { project } => {
            rpc(
                &discovery,
                "source.embedding.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingPolicySet {
            project,
            enable,
            disable,
            endpoint_env,
            model,
            dimensions,
            batch_size,
            max_chunks_per_source,
            retention_days,
            timeout_secs,
            api_key_env,
            clear,
        } => {
            if enable && disable {
                bail!("--enable and --disable cannot be combined");
            }
            if clear
                && (enable
                    || disable
                    || endpoint_env.is_some()
                    || model.is_some()
                    || dimensions.is_some()
                    || batch_size.is_some()
                    || max_chunks_per_source.is_some()
                    || retention_days.is_some()
                    || timeout_secs.is_some()
                    || api_key_env.is_some())
            {
                bail!("--clear cannot be combined with other policy flags");
            }
            let current = if clear {
                json!({})
            } else {
                rpc(
                    &discovery,
                    "source.embedding.policy.get",
                    json!({"project_id": project}),
                )
                .await?
                .get("policy")
                .cloned()
                .unwrap_or_default()
            };
            let mut policy = if current.is_object() {
                current
            } else {
                json!({})
            };
            let defaults = [
                ("enabled", json!(false)),
                ("dimensions", json!(1536)),
                ("batch_size", json!(8)),
                ("max_chunks_per_source", json!(200)),
                ("retention_days", json!(30)),
                ("timeout_secs", json!(30)),
            ];
            for (key, value) in defaults {
                if policy.get(key).is_none() {
                    policy[key] = value;
                }
            }
            if enable {
                policy["enabled"] = json!(true);
            }
            if disable {
                policy["enabled"] = json!(false);
            }
            if let Some(value) = endpoint_env {
                policy["endpoint_env"] = json!(value);
            }
            if clear {
                policy["endpoint_env"] = Value::Null;
                policy["api_key_env"] = Value::Null;
            }
            if let Some(value) = model {
                policy["model"] = json!(value);
            }
            if clear {
                policy["model"] = Value::Null;
            }
            if let Some(value) = dimensions {
                policy["dimensions"] = json!(value);
            }
            if let Some(value) = batch_size {
                policy["batch_size"] = json!(value);
            }
            if let Some(value) = max_chunks_per_source {
                policy["max_chunks_per_source"] = json!(value);
            }
            if let Some(value) = retention_days {
                policy["retention_days"] = json!(value);
            }
            if let Some(value) = timeout_secs {
                policy["timeout_secs"] = json!(value);
            }
            if let Some(value) = api_key_env {
                policy["api_key_env"] = json!(value);
            }
            rpc(
                &discovery,
                "source.embedding.policy.set",
                json!({"project_id": project, "policy": policy}),
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
        Command::Governor { session, limit } => {
            rpc(
                &discovery,
                "session.budget.governor",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::CriticalPath {
            session,
            remaining_tokens,
            mode,
        } => {
            let mode = parse_governor_mode(mode.as_deref())?;
            rpc(
                &discovery,
                "governor.preview",
                json!({
                    "session_id": session,
                    "remaining_tokens": remaining_tokens,
                    "limit": 1,
                    "mode": mode,
                }),
            )
            .await?
        }
        Command::GovernorPreview {
            session,
            remaining_tokens,
            limit,
            mode,
        } => {
            let mode = parse_governor_mode(mode.as_deref())?;
            rpc(
                &discovery,
                "governor.preview",
                json!({
                    "session_id": session,
                    "remaining_tokens": remaining_tokens,
                    "limit": limit,
                    "mode": mode,
                }),
            )
            .await?
        }
        Command::GovernorPolicy { project } => {
            rpc(
                &discovery,
                "critical_path.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::GovernorPolicySet {
            project,
            enabled,
            reserve_ratio,
            throttle_at_ratio,
            critical_boost,
            min_task_budget,
            mode,
            max_paths,
            department_min_ratio,
            observed_weight_ratio,
            starvation_after_secs,
        } => {
            let current = rpc(
                &discovery,
                "critical_path.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(value) = enabled {
                policy["enabled"] = json!(value);
            }
            if let Some(value) = reserve_ratio {
                policy["reserve_ratio"] = json!(value);
            }
            if let Some(value) = throttle_at_ratio {
                policy["throttle_at_ratio"] = json!(value);
            }
            if let Some(value) = critical_boost {
                policy["critical_boost"] = json!(value);
            }
            if let Some(value) = min_task_budget {
                policy["min_task_budget"] = json!(value);
            }
            if let Some(value) = mode {
                let parsed =
                    vp_protocol::types::CriticalPathMode::parse(&value).ok_or_else(|| {
                        anyhow::anyhow!("invalid --mode {value:?}; use `single` or `multi`")
                    })?;
                policy["mode"] = json!(parsed.as_str());
            }
            if let Some(value) = max_paths {
                policy["max_paths"] = json!(value);
            }
            if let Some(value) = department_min_ratio {
                policy["department_min_ratio"] = json!(value);
            }
            if let Some(value) = observed_weight_ratio {
                policy["observed_weight_ratio"] = json!(value);
            }
            if let Some(value) = starvation_after_secs {
                policy["starvation_after_secs"] = json!(value);
            }
            rpc(
                &discovery,
                "critical_path.policy.set",
                json!({"project_id": project, "policy": policy}),
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
        Command::Scaling {
            project,
            session,
            limit,
        } => {
            rpc(
                &discovery,
                "scaling.status",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ScalingPreview { session } => {
            rpc(
                &discovery,
                "scaling.preview",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ScalingPolicy { project } => {
            rpc(
                &discovery,
                "scaling.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ScalingPolicySet {
            project,
            enabled,
            min_workers,
            max_workers,
            min_labour,
            max_labour,
            expand_ready_threshold,
            expand_critical_delay_secs,
            scale_down_idle_secs,
            cooldown_secs,
            max_step,
            budget_headroom_ratio,
            no_budget_check,
        } => {
            // Merge with the stored policy so a one-field edit does not reset
            // the rest to defaults.
            let current = rpc(
                &discovery,
                "scaling.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            let budget_headroom_ratio = if no_budget_check {
                Value::Null
            } else if let Some(ratio) = budget_headroom_ratio {
                json!(ratio)
            } else {
                policy
                    .get("budget_headroom_ratio")
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            rpc(
                &discovery,
                "scaling.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled
                            .unwrap_or_else(|| policy["enabled"].as_bool().unwrap_or(false)),
                        "min_workers": min_workers
                            .unwrap_or_else(|| policy["min_workers"].as_u64().unwrap_or(1) as u32),
                        "max_workers": max_workers
                            .unwrap_or_else(|| policy["max_workers"].as_u64().unwrap_or(4) as u32),
                        "min_labour": min_labour
                            .unwrap_or_else(|| policy["min_labour"].as_u64().unwrap_or(1) as u32),
                        "max_labour": max_labour
                            .unwrap_or_else(|| policy["max_labour"].as_u64().unwrap_or(8) as u32),
                        "expand_ready_threshold": expand_ready_threshold.unwrap_or_else(|| {
                            policy["expand_ready_threshold"].as_u64().unwrap_or(2) as u32
                        }),
                        "expand_critical_delay_secs": expand_critical_delay_secs.unwrap_or_else(
                            || policy["expand_critical_delay_secs"].as_u64().unwrap_or(120),
                        ),
                        "scale_down_idle_secs": scale_down_idle_secs.unwrap_or_else(|| {
                            policy["scale_down_idle_secs"].as_u64().unwrap_or(600)
                        }),
                        "cooldown_secs": cooldown_secs.unwrap_or_else(|| {
                            policy["cooldown_secs"].as_u64().unwrap_or(120)
                        }),
                        "max_step": max_step
                            .unwrap_or_else(|| policy["max_step"].as_u64().unwrap_or(2) as u32),
                        "budget_headroom_ratio": budget_headroom_ratio,
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
        Command::LeaseHistory { key, limit } => {
            rpc(
                &discovery,
                "runtime.leases.history",
                json!({"lease_key": key, "limit": limit}),
            )
            .await?
        }
        Command::Computer {
            sweep,
            release,
            status,
            refresh,
            session,
        } => {
            if status {
                rpc(
                    &discovery,
                    "computer.status",
                    json!({"session_id": session, "refresh": refresh}),
                )
                .await?
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
        Command::Gui {
            timeline,
            session,
            project,
            limit,
        } => {
            if timeline {
                rpc(
                    &discovery,
                    "gui.timeline",
                    json!({"session_id": session, "limit": limit}),
                )
                .await?
            } else {
                rpc(&discovery, "gui.status", json!({"project_id": project})).await?
            }
        }
        Command::GuiPolicy { project } => {
            rpc(&discovery, "gui.policy.get", json!({"project_id": project})).await?
        }
        Command::GuiPolicySet {
            project,
            grace_secs,
            wait_ms,
            status_ttl_secs,
            sensitive_mode,
            sensitive_bundle,
            sensitive_title,
            replace_sensitive,
        } => {
            let current = rpc(&discovery, "gui.policy.get", json!({"project_id": project})).await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(grace_secs) = grace_secs {
                policy["grace_secs"] = json!(grace_secs);
            }
            if let Some(wait_ms) = wait_ms {
                policy["wait_ms"] = json!(wait_ms);
            }
            if let Some(status_ttl_secs) = status_ttl_secs {
                policy["status_ttl_secs"] = json!(status_ttl_secs);
            }
            if let Some(sensitive_mode) = sensitive_mode {
                policy["sensitive_mode"] = json!(sensitive_mode);
            }
            if replace_sensitive {
                policy["sensitive_bundles"] = json!([]);
                policy["sensitive_titles"] = json!([]);
            }
            if !sensitive_bundle.is_empty() {
                let mut bundles = policy
                    .get("sensitive_bundles")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                bundles.extend(sensitive_bundle.into_iter().map(Value::String));
                policy["sensitive_bundles"] = json!(bundles);
            }
            if !sensitive_title.is_empty() {
                let mut titles = policy
                    .get("sensitive_titles")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                titles.extend(sensitive_title.into_iter().map(Value::String));
                policy["sensitive_titles"] = json!(titles);
            }
            rpc(
                &discovery,
                "gui.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
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
        Command::ContinuationStatus { id } => {
            rpc(&discovery, "session.continuation.status", json!({"id": id})).await?
        }
        Command::ContinuationReclaim { id, reason } => {
            rpc(
                &discovery,
                "session.continuation.reclaim",
                json!({"id": id, "reason": reason}),
            )
            .await?
        }
        Command::TaskStalls { session } => {
            rpc(&discovery, "session.stalls", json!({"session_id": session})).await?
        }
        Command::ToolActivity {
            session,
            limit,
            csv: _,
        } => {
            rpc(
                &discovery,
                "session.tool_activity",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
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
        Command::ProcessPolicy { project } => {
            rpc(
                &discovery,
                "process.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ProcessPolicySet {
            project,
            idle_sweep_enabled,
            max_processes,
            idle_ttl_secs,
            max_output_bytes,
            max_output_lines,
        } => {
            let current = rpc(
                &discovery,
                "process.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current["policy"].clone();
            if let Some(enabled) = idle_sweep_enabled {
                policy["idle_sweep_enabled"] = json!(enabled);
            }
            if let Some(value) = max_processes {
                policy["max_processes"] = json!(value);
            }
            if let Some(value) = idle_ttl_secs {
                policy["idle_ttl_secs"] = json!(value);
            }
            if let Some(value) = max_output_bytes {
                policy["max_output_bytes"] = json!(value);
            }
            if let Some(value) = max_output_lines {
                policy["max_output_lines"] = json!(value);
            }
            rpc(
                &discovery,
                "process.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Processes {
            session,
            include_finished,
            limit,
        } => {
            rpc(
                &discovery,
                "session.processes",
                json!({
                    "session_id": session,
                    "include_finished": include_finished,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ProcessLogs {
            session,
            process,
            offset,
            max_bytes,
            max_lines,
            from_end,
            follow,
            timeout_secs,
        } => {
            if follow {
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.max(1));
                let mut cursor = offset;
                loop {
                    let result = rpc(
                        &discovery,
                        "session.process.logs",
                        json!({
                            "session_id": session,
                            "process_id": process,
                            "offset": cursor,
                            "max_bytes": max_bytes,
                            "max_lines": max_lines,
                            "from_end": false,
                        }),
                    )
                    .await?;
                    if let Some(content) = result["content"].as_str()
                        && !content.is_empty()
                    {
                        print!("{content}");
                        let _ = std::io::stdout().flush();
                    }
                    cursor = result["next_offset"].as_u64().unwrap_or(cursor);
                    let state = result["state"].as_str().unwrap_or("running");
                    let eof = result["eof"].as_bool().unwrap_or(false);
                    if eof && matches!(state, "exited" | "stopped" | "lost") {
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        eprintln!("process log follow timed out after {timeout_secs}s");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(400));
                }
                json!({"process_streamed": true})
            } else {
                rpc(
                    &discovery,
                    "session.process.logs",
                    json!({
                        "session_id": session,
                        "process_id": process,
                        "offset": offset,
                        "max_bytes": max_bytes,
                        "max_lines": max_lines,
                        "from_end": from_end,
                    }),
                )
                .await?
            }
        }
        Command::ProcessStop {
            session,
            process,
            signal,
            grace_secs,
        } => {
            rpc(
                &discovery,
                "session.process.stop",
                json!({
                    "session_id": session,
                    "process_id": process,
                    "signal": signal,
                    "grace_secs": grace_secs,
                }),
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
        Command::ContextCheckpoints { session, limit } => {
            rpc(
                &discovery,
                "session.context.checkpoints",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::ContextCheckpoint { session, label } => {
            rpc(
                &discovery,
                "session.context.checkpoint",
                json!({"session_id": session, "label": label}),
            )
            .await?
        }
        Command::ContextCheckpointDetail {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.detail",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::ContextRestore {
            session,
            checkpoint,
            force,
        } => {
            rpc(
                &discovery,
                "session.context.restore",
                json!({
                    "session_id": session,
                    "checkpoint_id": checkpoint,
                    "force": force
                }),
            )
            .await?
        }
        Command::ContextCheckpointDelete {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.delete",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::ContextCheckpointPrune {
            session,
            older_than_days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.prune",
                json!({
                    "session_id": session,
                    "older_than_days": older_than_days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::ContextCheckpointPolicy { project } => {
            rpc(
                &discovery,
                "session.context.policy",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ContextCheckpointPolicySet {
            project,
            enabled,
            max_checkpoints,
            max_age_days,
        } => {
            rpc(
                &discovery,
                "session.context.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled,
                        "max_checkpoints": max_checkpoints,
                        "max_age_days": max_age_days
                    }
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
        Command::CacheAudit { session } => {
            rpc(
                &discovery,
                "session.cache.audit",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CachePlan { session, role } => {
            rpc(
                &discovery,
                "session.cache.plan",
                json!({"session_id": session, "roles": role}),
            )
            .await?
        }
        Command::CacheWarm { session, role } => {
            rpc(
                &discovery,
                "session.cache.warm",
                json!({"session_id": session, "role": role}),
            )
            .await?
        }
        Command::CachePolicy { project } => {
            rpc(
                &discovery,
                "project.cache_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::CachePolicySet {
            project,
            disabled,
            ttl,
            no_automatic,
            warm_on_resume,
            no_compact_aware,
            role_ttl,
        } => {
            // Read-modify-write preserves role overrides that the operator did
            // not mention instead of silently clearing them.
            let existing = rpc(
                &discovery,
                "project.cache_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut roles = existing["policy"]["role_ttl"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for entry in role_ttl {
                let Some((role, value)) = entry.split_once('=') else {
                    bail!("--role-ttl must be ROLE=TTL, got '{entry}'");
                };
                if role.trim().is_empty() || value.trim().is_empty() {
                    bail!("--role-ttl must be ROLE=TTL, got '{entry}'");
                }
                roles.insert(
                    role.trim().to_string(),
                    Value::String(value.trim().to_string()),
                );
            }
            rpc(
                &discovery,
                "project.cache_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": !disabled,
                        "ttl": ttl,
                        "automatic_caching": !no_automatic,
                        "warm_on_resume": warm_on_resume,
                        "compact_aware": !no_compact_aware,
                        "role_ttl": roles,
                    }
                }),
            )
            .await?
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
            lexical_only,
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
                    "include_stale": include_stale,
                    "lexical_only": lexical_only
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
            embedding_enable,
            embedding_disable,
            embedding_endpoint,
            embedding_model,
            embedding_dimensions,
            embedding_batch_size,
            embedding_timeout_secs,
            embedding_max_candidates,
            embedding_min_similarity,
            embedding_api_key_env,
            embedding_index,
            ivf_nlist,
            ivf_nprobe,
            ivf_min_vectors,
            ivf_rebuild_vectors,
            embedding_clear,
        } => {
            if embedding_enable && embedding_disable {
                bail!("--embedding-enable and --embedding-disable cannot be combined");
            }
            if embedding_clear
                && (embedding_enable
                    || embedding_disable
                    || embedding_endpoint.is_some()
                    || embedding_model.is_some()
                    || embedding_dimensions.is_some()
                    || embedding_batch_size.is_some()
                    || embedding_timeout_secs.is_some()
                    || embedding_max_candidates.is_some()
                    || embedding_min_similarity.is_some()
                    || embedding_api_key_env.is_some()
                    || embedding_index.is_some()
                    || ivf_nlist.is_some()
                    || ivf_nprobe.is_some()
                    || ivf_min_vectors.is_some()
                    || ivf_rebuild_vectors.is_some())
            {
                bail!("--embedding-clear cannot be combined with other embedding flags");
            }
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
            let current_embedding = policy.get("embedding").cloned().unwrap_or(Value::Null);
            let embedding_flags = embedding_enable
                || embedding_disable
                || embedding_endpoint.is_some()
                || embedding_model.is_some()
                || embedding_dimensions.is_some()
                || embedding_batch_size.is_some()
                || embedding_timeout_secs.is_some()
                || embedding_max_candidates.is_some()
                || embedding_min_similarity.is_some()
                || embedding_api_key_env.is_some()
                || embedding_index.is_some()
                || ivf_nlist.is_some()
                || ivf_nprobe.is_some()
                || ivf_min_vectors.is_some()
                || ivf_rebuild_vectors.is_some();
            let embedding = if embedding_clear {
                Value::Null
            } else if !embedding_flags {
                current_embedding
            } else {
                let mut object = if current_embedding.is_object() {
                    current_embedding
                } else {
                    json!({})
                };
                if object.get("enabled").is_none() {
                    object["enabled"] = json!(false);
                }
                if object.get("dimensions").is_none() {
                    object["dimensions"] = json!(1536);
                }
                if object.get("batch_size").is_none() {
                    object["batch_size"] = json!(16);
                }
                if object.get("timeout_secs").is_none() {
                    object["timeout_secs"] = json!(30);
                }
                if object.get("max_candidates").is_none() {
                    object["max_candidates"] = json!(64);
                }
                if object.get("min_similarity").is_none() {
                    object["min_similarity"] = json!(0.20);
                }
                if object.get("index").is_none() {
                    object["index"] = json!("scan");
                }
                if object.get("ivf_nlist").is_none() {
                    object["ivf_nlist"] = json!(64);
                }
                if object.get("ivf_nprobe").is_none() {
                    object["ivf_nprobe"] = json!(8);
                }
                if object.get("ivf_min_vectors").is_none() {
                    object["ivf_min_vectors"] = json!(256);
                }
                if object.get("ivf_rebuild_vectors").is_none() {
                    object["ivf_rebuild_vectors"] = json!(4096);
                }
                if embedding_enable {
                    object["enabled"] = json!(true);
                }
                if embedding_disable {
                    object["enabled"] = json!(false);
                }
                if let Some(endpoint) = embedding_endpoint {
                    object["endpoint"] = json!(endpoint);
                }
                if let Some(model) = embedding_model {
                    object["model"] = json!(model);
                }
                if let Some(dimensions) = embedding_dimensions {
                    object["dimensions"] = json!(dimensions);
                }
                if let Some(batch_size) = embedding_batch_size {
                    object["batch_size"] = json!(batch_size);
                }
                if let Some(timeout_secs) = embedding_timeout_secs {
                    object["timeout_secs"] = json!(timeout_secs);
                }
                if let Some(max_candidates) = embedding_max_candidates {
                    object["max_candidates"] = json!(max_candidates);
                }
                if let Some(min_similarity) = embedding_min_similarity {
                    object["min_similarity"] = json!(min_similarity);
                }
                if let Some(env_name) = embedding_api_key_env {
                    object["api_key_env"] = json!(env_name);
                }
                if let Some(index) = embedding_index {
                    object["index"] = json!(index);
                }
                if let Some(nlist) = ivf_nlist {
                    object["ivf_nlist"] = json!(nlist);
                }
                if let Some(nprobe) = ivf_nprobe {
                    object["ivf_nprobe"] = json!(nprobe);
                }
                if let Some(min_vectors) = ivf_min_vectors {
                    object["ivf_min_vectors"] = json!(min_vectors);
                }
                if let Some(rebuild_vectors) = ivf_rebuild_vectors {
                    object["ivf_rebuild_vectors"] = json!(rebuild_vectors);
                }
                object
            };
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
                        "sweep_stale": policy.get("sweep_stale").cloned().unwrap_or(json!(false)),
                        "embedding": embedding
                    }
                }),
            )
            .await?
        }
        Command::MemoryEmbeddings { project } => {
            rpc(
                &discovery,
                "memory.embeddings.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ToolOutputPolicy { project } => {
            rpc(
                &discovery,
                "tool_output.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsRebuild { project } => {
            rpc(
                &discovery,
                "memory.embeddings.rebuild",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsTest { project, text } => {
            rpc(
                &discovery,
                "memory.embeddings.test",
                json!({"project_id": project, "text": text}),
            )
            .await?
        }
        Command::MemoryEmbeddingsIndex { project } => {
            rpc(
                &discovery,
                "memory.embeddings.index.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsIndexRebuild { project } => {
            rpc(
                &discovery,
                "memory.embeddings.index.rebuild",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ToolOutputPolicySet {
            project,
            disable,
            enable,
            spill_threshold,
            preview_bytes,
            max_turn_bytes,
            retention_secs,
            max_entries,
            tool_thresholds,
        } => {
            if disable && enable {
                bail!("--disable and --enable are mutually exclusive");
            }
            let current = rpc(
                &discovery,
                "tool_output.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_default();
            if !policy.is_object() {
                policy = json!({});
            }
            if let Some(object) = policy.as_object_mut() {
                if disable {
                    object.insert("enabled".into(), json!(false));
                }
                if enable {
                    object.insert("enabled".into(), json!(true));
                }
                if let Some(value) = spill_threshold {
                    object.insert("spill_threshold_bytes".into(), json!(value));
                }
                if let Some(value) = preview_bytes {
                    object.insert("preview_bytes".into(), json!(value));
                }
                if let Some(value) = max_turn_bytes {
                    object.insert("max_total_bytes_per_turn".into(), json!(value));
                }
                if let Some(value) = retention_secs {
                    object.insert("retention_secs".into(), json!(value));
                }
                if let Some(value) = max_entries {
                    object.insert("max_entries".into(), json!(value));
                }
                if !tool_thresholds.is_empty() {
                    let mut thresholds = object
                        .get("tool_thresholds")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    for entry in parse_tool_thresholds(&tool_thresholds)? {
                        thresholds.insert(entry.0, json!(entry.1));
                    }
                    object.insert("tool_thresholds".into(), Value::Object(thresholds));
                }
            }
            rpc(
                &discovery,
                "tool_output.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::ToolOutputs {
            project,
            session,
            tool,
            before,
            include_expired,
            limit,
        } => {
            rpc(
                &discovery,
                "tool_output.list",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "tool_name": tool,
                    "before_created_at": before,
                    "include_expired": include_expired,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ToolOutputStatus { project, session } => {
            rpc(
                &discovery,
                "tool_output.status",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::ToolOutputPrune {
            project,
            session,
            older_than_secs,
            max_entries,
            dry_run,
        } => {
            rpc(
                &discovery,
                "tool_output.prune",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "older_than_secs": older_than_secs,
                    "max_entries": max_entries,
                    "dry_run": dry_run,
                }),
            )
            .await?
        }
        Command::ToolOutputAudit {
            project,
            verify_hashes,
            clean_orphans,
        } => {
            rpc(
                &discovery,
                "tool_output.audit",
                json!({
                    "project_id": project,
                    "verify_hashes": verify_hashes,
                    "clean_orphans": clean_orphans,
                }),
            )
            .await?
        }
        Command::TestCache { project } => {
            rpc(
                &discovery,
                "test_cache.stats",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::TestCacheEntries {
            project,
            session,
            kind,
            status,
            include_expired,
            limit,
        } => {
            rpc(
                &discovery,
                "test_cache.entries",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "kind": kind,
                    "status": status,
                    "include_expired": include_expired,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::TestCachePolicySet {
            project,
            enabled,
            max_entries,
            retention_days,
            max_output_bytes,
            reuse_lint,
            include_integration,
            failed_retention_secs,
        } => {
            let current = rpc(
                &discovery,
                "test_cache.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_default();
            if !policy.is_object() {
                policy = json!({});
            }
            if let Some(object) = policy.as_object_mut() {
                if let Some(value) = enabled {
                    object.insert("enabled".into(), json!(value));
                }
                if let Some(value) = max_entries {
                    object.insert("max_entries".into(), json!(value));
                }
                if let Some(value) = retention_days {
                    object.insert("retention_days".into(), json!(value));
                }
                if let Some(value) = max_output_bytes {
                    object.insert("max_output_bytes".into(), json!(value));
                }
                if let Some(value) = reuse_lint {
                    object.insert("reuse_lint".into(), json!(value));
                }
                if let Some(value) = include_integration {
                    object.insert("include_integration".into(), json!(value));
                }
                if let Some(value) = failed_retention_secs {
                    object.insert("failed_retention_secs".into(), json!(value));
                }
            }
            rpc(
                &discovery,
                "test_cache.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::TestCachePrune {
            project,
            older_than_secs,
            expired_only,
            kind,
            dry_run,
        } => {
            rpc(
                &discovery,
                "test_cache.prune",
                json!({
                    "project_id": project,
                    "older_than_secs": older_than_secs,
                    "expired_only": expired_only,
                    "kind": kind,
                    "dry_run": dry_run,
                }),
            )
            .await?
        }
        Command::TestCacheClear {
            project,
            session,
            kind,
            yes,
        } => {
            if !yes {
                bail!("refusing to clear test-cache metadata without --yes");
            }
            rpc(
                &discovery,
                "test_cache.clear",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "kind": kind,
                    "confirm": yes,
                }),
            )
            .await?
        }
        Command::TestImpact {
            session,
            paths,
            limit,
            no_history,
        } => {
            rpc(
                &discovery,
                "test_cache.impact",
                json!({
                    "session_id": session,
                    "changed_paths": paths,
                    "limit": limit,
                    "include_history": !no_history,
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
        Command::Approvals { session, project } => {
            let project = match project {
                Some(project) => Some(resolve_project(&discovery, Some(&project)).await?),
                None => None,
            };
            rpc(
                &discovery,
                "approval.queue",
                json!({"session_id": session, "project_id": project}),
            )
            .await?
        }
        Command::ApprovalBatch { approve, deny } => {
            if approve.is_empty() && deny.is_empty() {
                bail!("approval-batch needs at least one --approve or --deny id");
            }
            if let Some(id) = approve.iter().find(|id| deny.contains(id)) {
                bail!("approval id '{id}' appears in both --approve and --deny");
            }
            let mut decisions = Vec::new();
            decisions.extend(
                approve
                    .into_iter()
                    .map(|id| json!({"approval_id": id, "approved": true})),
            );
            decisions.extend(
                deny.into_iter()
                    .map(|id| json!({"approval_id": id, "approved": false})),
            );
            rpc(
                &discovery,
                "approval.batch.resolve",
                json!({"decisions": decisions}),
            )
            .await?
        }
        Command::ApprovalTemplate { project } => {
            let project = resolve_project(&discovery, project.as_deref()).await?;
            rpc(
                &discovery,
                "approval.templates",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ApprovalTemplateSet {
            project,
            id,
            name,
            tool_pattern,
            risk,
            path_glob,
            action,
            expires_at,
            max_uses,
            disabled,
        } => {
            let project = resolve_project(&discovery, project.as_deref()).await?;
            if let Some(id) = id {
                let current = rpc(
                    &discovery,
                    "approval.templates",
                    json!({"project_id": project}),
                )
                .await?;
                let existing = current
                    .get("templates")
                    .and_then(Value::as_array)
                    .and_then(|templates| {
                        templates.iter().find(|template| {
                            template.get("id").and_then(Value::as_str) == Some(&id)
                        })
                    })
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("approval template not found: {id}"))?;
                let name = name.unwrap_or_else(|| {
                    existing
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("template")
                        .to_string()
                });
                let tool_pattern = tool_pattern.unwrap_or_else(|| {
                    existing
                        .get("tool_pattern")
                        .and_then(Value::as_str)
                        .unwrap_or("write_file")
                        .to_string()
                });
                let action = action.unwrap_or_else(|| {
                    existing
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("allow")
                        .to_string()
                });
                let risk = if risk.is_empty() {
                    existing
                        .get("risk_levels")
                        .and_then(Value::as_array)
                        .map(|levels| {
                            levels
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    risk
                };
                let path_glob = if path_glob.is_empty() {
                    existing
                        .get("path_globs")
                        .and_then(Value::as_array)
                        .map(|globs| {
                            globs
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    path_glob
                };
                rpc(
                    &discovery,
                    "approval.template.update",
                    json!({
                        "id": id,
                        "name": name,
                        "enabled": !disabled,
                        "tool_pattern": tool_pattern,
                        "risk_levels": risk,
                        "path_globs": path_glob,
                        "action": action,
                        "expires_at": expires_at.or_else(|| {
                            existing.get("expires_at").cloned().and_then(|value| value.as_str().map(str::to_string))
                        }),
                        "max_uses": max_uses.or_else(|| {
                            existing.get("max_uses").and_then(Value::as_u64).map(|value| value as u32)
                        }),
                    }),
                )
                .await?
            } else {
                let name = name.ok_or_else(|| anyhow::anyhow!("--name is required"))?;
                let tool_pattern =
                    tool_pattern.ok_or_else(|| anyhow::anyhow!("--tool-pattern is required"))?;
                let action = action.ok_or_else(|| anyhow::anyhow!("--action is required"))?;
                rpc(
                    &discovery,
                    "approval.template.create",
                    json!({
                        "project_id": project,
                        "name": name,
                        "tool_pattern": tool_pattern,
                        "risk_levels": risk,
                        "path_globs": path_glob,
                        "action": action,
                        "expires_at": expires_at,
                        "max_uses": max_uses,
                    }),
                )
                .await?
            }
        }
        Command::ApprovalTemplateDelete { id } => {
            rpc(&discovery, "approval.template.delete", json!({"id": id})).await?
        }
        Command::ApprovalPolicy { project } => {
            rpc(
                &discovery,
                "approval.policy",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ApprovalPolicySet {
            project,
            enabled,
            max_pending,
            coalesce_secs,
            stale_secs,
            priority_boost_risk,
            notify_coalesce_secs,
        } => {
            let current = rpc(
                &discovery,
                "approval.policy",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(max_pending) = max_pending {
                policy["max_pending_per_session"] = json!(max_pending);
            }
            if let Some(coalesce_secs) = coalesce_secs {
                policy["coalesce_window_secs"] = json!(coalesce_secs);
            }
            if let Some(stale_secs) = stale_secs {
                policy["stale_after_secs"] = json!(stale_secs);
            }
            if let Some(priority_boost_risk) = priority_boost_risk {
                policy["priority_boost_risk"] = json!(priority_boost_risk);
            }
            if let Some(notify_coalesce_secs) = notify_coalesce_secs {
                policy["notify_coalesce_secs"] = json!(notify_coalesce_secs);
            }
            rpc(
                &discovery,
                "approval.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::ApprovalAudit { project, limit } => {
            let project = resolve_project(&discovery, Some(&project)).await?;
            rpc(
                &discovery,
                "approval.audit",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
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

    if want_tool_activity_csv {
        print!("{}", tool_activity_csv(&value));
        return Ok(());
    }
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

async fn resolve_project(discovery: &Discovery, requested: Option<&str>) -> Result<String> {
    let projects = rpc(discovery, "project.list", json!({})).await?;
    let projects = projects
        .get("projects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(requested) = requested {
        if let Some(project) = projects.iter().find(|project| {
            project.get("id").and_then(Value::as_str) == Some(requested)
                || project
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(requested))
        }) && let Some(id) = project.get("id").and_then(Value::as_str)
        {
            return Ok(id.to_string());
        }
        // Allow an explicit id that the daemon can resolve later.
        return Ok(requested.to_string());
    }
    match projects.as_slice() {
        [project] => project
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("project list returned a malformed project")),
        [] => bail!("no projects configured; pass --project"),
        _ => bail!("multiple projects configured; pass --project"),
    }
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

fn load_keyring_optional(directory: Option<&Path>) -> Result<Option<Keyring>> {
    match directory {
        Some(directory) => Ok(Some(Keyring::load(directory)?)),
        None => Keyring::load_optional(&default_key_dir()),
    }
}

fn load_keyring_required(directory: Option<&Path>) -> Result<Keyring> {
    match directory {
        Some(directory) => Keyring::load(directory),
        None => Keyring::load(&default_key_dir()),
    }
}

/// Resolve signing options from CLI flags.
///
/// Signing is explicit: no flag means an unsigned v2 repository. Passing
/// `--signing-key`, `--sign`, or `--keyring-dir` loads the keyring and signs
/// with the requested or active key.
fn resolve_signing_options(
    keyring_dir: Option<&Path>,
    signing_key: Option<&str>,
    sign: bool,
) -> Result<Option<RepositorySigningOptions>> {
    if keyring_dir.is_none() && signing_key.is_none() && !sign {
        return Ok(None);
    }
    if sign && signing_key.is_some() {
        bail!("pass only one of --signing-key or --sign");
    }
    let keyring = load_keyring_required(keyring_dir)?;
    if let Some(key_id) = signing_key {
        keyring.public_key(key_id)?;
    } else {
        keyring.resolve_key_id(None)?;
    }
    Ok(Some(RepositorySigningOptions {
        keyring,
        key_id: signing_key.map(str::to_string),
    }))
}

fn anchor_store(data_dir: &Path, anchor_file: Option<&Path>) -> AnchorStore {
    match anchor_file {
        Some(path) => AnchorStore::from_file(path),
        None => AnchorStore::new(data_dir),
    }
}

fn resolve_passphrase(
    environment: Option<&str>,
    file: Option<&Path>,
) -> Result<Option<Passphrase>> {
    match (environment, file) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => {
            bail!("pass only one of --passphrase-env or --passphrase-file")
        }
        (Some(name), None) => {
            if name.is_empty() {
                bail!("--passphrase-env requires a non-empty environment variable name");
            }
            let value = std::env::var(name).with_context(|| {
                format!("backup passphrase environment variable {name} is not set or is not UTF-8")
            })?;
            Ok(Some(Passphrase::new(value.into_bytes())?))
        }
        (None, Some(path)) => {
            let metadata = std::fs::symlink_metadata(path)
                .with_context(|| format!("reading backup passphrase file {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "refusing symlinked backup passphrase file {}",
                    path.display()
                );
            }
            if !metadata.is_file() {
                bail!(
                    "backup passphrase file {} is not a regular file",
                    path.display()
                );
            }
            if metadata.len() == 0 || metadata.len() > 4097 {
                bail!(
                    "backup passphrase file {} must contain 1..=4096 bytes",
                    path.display()
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    bail!(
                        "backup passphrase file {} must not be accessible by group or others (mode {:04o})",
                        path.display(),
                        mode
                    );
                }
            }
            let mut bytes = std::fs::read(path)
                .with_context(|| format!("reading backup passphrase file {}", path.display()))?;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            Ok(Some(Passphrase::new(bytes)?))
        }
    }
}

fn print_repository_backup(value: &Value) {
    println!(
        "snapshot {}  sequence {}  parent {}",
        value["snapshot_id"].as_str().unwrap_or("?"),
        value["sequence"].as_u64().unwrap_or(0),
        value["parent"].as_str().unwrap_or("-")
    );
    println!(
        "  {}  encrypted={}  incremental={}  artifacts={}  compression={}",
        value["repository"].as_str().unwrap_or("?"),
        value["encrypted"].as_bool().unwrap_or(false),
        value["incremental"].as_bool().unwrap_or(false),
        value["includes_artifacts"].as_bool().unwrap_or(false),
        value["compression"].as_str().unwrap_or("?")
    );
    println!(
        "  entries {} ({} sessions, {} artifacts)  logical {} bytes",
        value["entries"].as_u64().unwrap_or(0),
        value["sessions"].as_u64().unwrap_or(0),
        value["artifacts"].as_u64().unwrap_or(0),
        value["bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  written {} bytes  deduplicated {} bytes ({} parent, {} store)  stored blob bytes {}",
        value["written_bytes"].as_u64().unwrap_or(0),
        value["deduplicated_bytes"].as_u64().unwrap_or(0),
        value["deduplicated_from_parent_bytes"]
            .as_u64()
            .unwrap_or(0),
        value["deduplicated_from_store_bytes"].as_u64().unwrap_or(0),
        value["stored_bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  blobs written {}  reused {}  manifest {} bytes  sha256 {}",
        value["blobs_written"].as_u64().unwrap_or(0),
        value["blobs_reused"].as_u64().unwrap_or(0),
        value["manifest_bytes"].as_u64().unwrap_or(0),
        value["manifest_sha256"].as_str().unwrap_or("?")
    );
    println!(
        "  signed={}  key={}  algorithm={}  anchor generation {}",
        value["signed"].as_bool().unwrap_or(false),
        value["signing_key_id"].as_str().unwrap_or("-"),
        value["signature_algorithm"].as_str().unwrap_or("-"),
        value["anchor_update"]["highest_generation"]
            .as_u64()
            .unwrap_or(0)
    );
}

fn print_repository_restore(value: &Value) {
    println!(
        "restored {}  sequence {}  -> {}",
        value["snapshot_id"].as_str().unwrap_or("?"),
        value["sequence"].as_u64().unwrap_or(0),
        value["output"].as_str().unwrap_or("?")
    );
    println!(
        "  entries {} ({} sessions, {} artifacts)  {} bytes  encrypted={}  replaced_existing={}",
        value["entries"].as_u64().unwrap_or(0),
        value["sessions"].as_u64().unwrap_or(0),
        value["artifacts"].as_u64().unwrap_or(0),
        value["bytes"].as_u64().unwrap_or(0),
        value["encrypted"].as_bool().unwrap_or(false),
        value["replaced_existing"].as_bool().unwrap_or(false)
    );
    println!(
        "  validated blobs {}  stored blob bytes {}",
        value["validated_blobs"].as_u64().unwrap_or(0),
        value["stored_blob_bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  signed={}  key={}  signature={}  anchor highest={}",
        value["signed"].as_bool().unwrap_or(false),
        value["signing_key_id"].as_str().unwrap_or("-"),
        value["signature_status"].as_str().unwrap_or("-"),
        value["anchor_update"]["highest_generation"]
            .as_u64()
            .unwrap_or(0)
    );
    if value["rollback"]["rollback"].as_bool().unwrap_or(false) {
        println!(
            "  rollback accepted explicitly: restored generation {} below anchor {}",
            value["rollback"]["generation"].as_u64().unwrap_or(0),
            value["rollback"]["anchor_generation"].as_u64().unwrap_or(0)
        );
    }
    if let Some(issues) = value["head_issues"].as_array() {
        for issue in issues {
            println!("  signed-head issue: {}", issue.as_str().unwrap_or("?"));
        }
    }
}

fn print_repository_list(value: &Value) {
    println!(
        "{}  {} snapshots  encrypted={}  blobs={}  logical {} bytes",
        value["repository"].as_str().unwrap_or("?"),
        value["snapshot_count"].as_u64().unwrap_or(0),
        value["encrypted"].as_bool().unwrap_or(false),
        value["unique_blobs"].as_u64().unwrap_or(0),
        value["logical_bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  signed={}  status={}  format v{}  active key={}",
        value["signed"].as_bool().unwrap_or(false),
        value["signing_status"].as_str().unwrap_or("unsigned"),
        value["format_version"].as_u64().unwrap_or(0),
        value["keyring"]["active_key_id"].as_str().unwrap_or("-")
    );
    let validation = &value["validation"];
    println!(
        "  validation valid={}  blobs present={} missing={} verified={} corrupt={}  mode={}",
        validation["valid"].as_bool().unwrap_or(false),
        validation["blobs_present"].as_u64().unwrap_or(0),
        validation["blobs_missing"].as_u64().unwrap_or(0),
        validation["blobs_verified"].as_u64().unwrap_or(0),
        validation["blobs_corrupt"].as_u64().unwrap_or(0),
        validation["blob_verification"].as_str().unwrap_or("?")
    );
    if let Some(issues) = validation["issues"].as_array()
        && !issues.is_empty()
    {
        for issue in issues {
            println!("  issue: {}", issue.as_str().unwrap_or("?"));
        }
    }
    if let Some(snapshots) = value["snapshots"].as_array() {
        for snapshot in snapshots {
            println!(
                "{}  seq {:<4} parent {:<36} {:<8} {} bytes  {} entries  signed={} key={}  sha256 {}",
                snapshot["snapshot_id"].as_str().unwrap_or("?"),
                snapshot["sequence"].as_u64().unwrap_or(0),
                snapshot["parent"].as_str().unwrap_or("-"),
                if snapshot["incremental"].as_bool().unwrap_or(false) {
                    "incr"
                } else {
                    "full"
                },
                snapshot["bytes"].as_u64().unwrap_or(0),
                snapshot["entries"].as_u64().unwrap_or(0),
                snapshot["signed"].as_bool().unwrap_or(false),
                snapshot["signing_key_id"].as_str().unwrap_or("-"),
                snapshot["manifest_sha256"].as_str().unwrap_or("?")
            );
        }
    }
}

fn print_repository_prune(value: &Value) {
    println!(
        "{}  keep {}  dry_run={}  applied={}",
        value["repository"].as_str().unwrap_or("?"),
        value["keep"].as_u64().unwrap_or(0),
        value["dry_run"].as_bool().unwrap_or(false),
        value["applied"].as_bool().unwrap_or(false)
    );
    println!(
        "  snapshots total {}  kept {}  pruned {}  blobs kept {}  pruned {}",
        value["snapshots_total"].as_u64().unwrap_or(0),
        value["snapshots_kept"].as_u64().unwrap_or(0),
        value["snapshots_pruned"]
            .as_array()
            .map(|entries| entries.len())
            .unwrap_or(0),
        value["blobs_kept"].as_u64().unwrap_or(0),
        value["blobs_pruned"].as_u64().unwrap_or(0)
    );
    println!(
        "  reclaimed {} bytes  protected {} bytes",
        value["reclaimed_bytes"].as_u64().unwrap_or(0),
        value["protected_bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  signed={}  key={}",
        value["signed"].as_bool().unwrap_or(false),
        value["signing_key_id"].as_str().unwrap_or("-")
    );
    if let Some(snapshots) = value["snapshots_pruned"].as_array() {
        for snapshot in snapshots {
            println!(
                "  prune {}  seq {}  {} bytes",
                snapshot["snapshot_id"].as_str().unwrap_or("?"),
                snapshot["sequence"].as_u64().unwrap_or(0),
                snapshot["bytes"].as_u64().unwrap_or(0)
            );
        }
    }
}

fn print_repository_verify(value: &Value) {
    println!(
        "{}  format v{}  generation {}  snapshots {}",
        value["repository"].as_str().unwrap_or("?"),
        value["format_version"].as_u64().unwrap_or(0),
        value["generation"].as_u64().unwrap_or(0),
        value["snapshot_count"].as_u64().unwrap_or(0)
    );
    println!(
        "  valid={}  authenticity={}  signed={}  verified snapshots {}  unsigned {}",
        value["valid"].as_bool().unwrap_or(false),
        value["authenticity"].as_str().unwrap_or("?"),
        value["signed"].as_bool().unwrap_or(false),
        value["signed_snapshots"].as_u64().unwrap_or(0),
        value["unsigned_snapshots"].as_u64().unwrap_or(0)
    );
    println!(
        "  metadata signature={} key={}  head_consistent={}",
        value["metadata_signature"]["status"]
            .as_str()
            .unwrap_or("?"),
        value["metadata_signature"]["key_id"]
            .as_str()
            .unwrap_or("-"),
        value["signed_head_consistent"].as_bool().unwrap_or(false)
    );
    println!(
        "  chain_valid={}  tombstones_valid={}  entries_valid={}  blobs present={} missing={} verified={} corrupt={}",
        value["chain_valid"].as_bool().unwrap_or(false),
        value["tombstones_valid"].as_bool().unwrap_or(false),
        value["entries_valid"].as_bool().unwrap_or(false),
        value["blobs_present"].as_u64().unwrap_or(0),
        value["blobs_missing"].as_u64().unwrap_or(0),
        value["blobs_verified"].as_u64().unwrap_or(0),
        value["blobs_corrupt"].as_u64().unwrap_or(0)
    );
    if value["rollback"].is_object() {
        println!(
            "  anchor generation={}  accepted={}  rollback={}  allow_rollback={}",
            value["rollback"]["anchor_generation"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            value["rollback"]["accepted"].as_bool().unwrap_or(false),
            value["rollback"]["rollback"].as_bool().unwrap_or(false),
            value["rollback"]["allow_rollback"]
                .as_bool()
                .unwrap_or(false)
        );
    }
    if let Some(issues) = value["issues"].as_array() {
        for issue in issues {
            println!("  issue: {}", issue.as_str().unwrap_or("?"));
        }
    }
    if let Some(issues) = value["head_issues"].as_array() {
        for issue in issues {
            println!("  signed-head issue: {}", issue.as_str().unwrap_or("?"));
        }
    }
}

fn print_backup_key_generation(value: &Value) {
    println!(
        "key {}  active={}  algorithm={}  fingerprint {}",
        value["key_id"].as_str().unwrap_or("?"),
        value["active"].as_bool().unwrap_or(false),
        value["algorithm"].as_str().unwrap_or("?"),
        value["fingerprint"].as_str().unwrap_or("?")
    );
    println!(
        "  public key {}",
        value["public_key_b64"].as_str().unwrap_or("?")
    );
    println!(
        "  keyring {}  source {}",
        value["keyring"].as_str().unwrap_or("?"),
        value["source"].as_str().unwrap_or("?")
    );
}

fn print_backup_key_rotation(value: &Value) {
    println!(
        "active key {}  previous {}  changed={}",
        value["key_id"].as_str().unwrap_or("?"),
        value["previous_key_id"].as_str().unwrap_or("-"),
        value["changed"].as_bool().unwrap_or(false)
    );
    let retired = value["retired_public_keys"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    println!(
        "  retired public keys retained for verification: {}",
        if retired.is_empty() { "-" } else { &retired }
    );
    println!("  keyring {}", value["keyring"].as_str().unwrap_or("?"));
}

fn print_backup_keyring(value: &Value) {
    println!(
        "{}  {} keys  active={}",
        value["path"].as_str().unwrap_or("?"),
        value["key_count"].as_u64().unwrap_or(0),
        value["active_key_id"].as_str().unwrap_or("-")
    );
    if let Some(keys) = value["keys"].as_array() {
        for key in keys {
            println!(
                "  {}  {}  active={}  retired={}  public {}",
                key["key_id"].as_str().unwrap_or("?"),
                key["algorithm"].as_str().unwrap_or("?"),
                key["active"].as_bool().unwrap_or(false),
                key["retired_at"].as_str().unwrap_or("-"),
                key["public_key_b64"].as_str().unwrap_or("?")
            );
        }
    }
    if value["key_count"].as_u64().unwrap_or(0) == 0 {
        println!("  no signing keys yet; run `vp backup-key-generate <key-id>`");
    }
}

fn print_backup_anchor(value: &Value) {
    println!(
        "{}  present={}  updated={}",
        value["path"].as_str().unwrap_or("?"),
        value["present"].as_bool().unwrap_or(false),
        value["updated_at"].as_str().unwrap_or("-")
    );
    if let Some(repositories) = value["repositories"].as_array() {
        for repository in repositories {
            println!(
                "  {}  highest generation {}  last verified {}  operation {}  at {}",
                repository["repository_id"].as_str().unwrap_or("?"),
                repository["highest_generation"].as_u64().unwrap_or(0),
                repository["last_verified_generation"].as_u64().unwrap_or(0),
                repository["last_operation"].as_str().unwrap_or("?"),
                repository["last_verified_at"].as_str().unwrap_or("?")
            );
        }
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Short component-hash display for cache status/plan output. Full hashes stay
/// available in `--json`; the human view only needs a stable visual prefix.
fn short_hash(value: &Value) -> String {
    value
        .as_str()
        .map(|hash| hash.chars().take(12).collect::<String>() + "…")
        .unwrap_or_else(|| "—".to_string())
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
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

fn parse_tool_thresholds(values: &[String]) -> Result<Vec<(String, u64)>> {
    let mut thresholds = Vec::new();
    for value in values {
        let (tool, bytes) = value
            .split_once('=')
            .with_context(|| format!("--tool-threshold must be TOOL=BYTES: {value}"))?;
        let tool = tool.trim();
        if tool.is_empty() {
            bail!("--tool-threshold tool name must not be empty");
        }
        let bytes = bytes
            .trim()
            .parse::<u64>()
            .with_context(|| format!("--tool-threshold bytes must be an integer: {value}"))?;
        thresholds.push((tool.to_string(), bytes));
    }
    Ok(thresholds)
}

fn parse_env_pairs(values: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    parse_pairs(values, "--env")
}

fn parse_pairs(
    values: &[String],
    flag: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut pairs = std::collections::BTreeMap::new();
    for value in values {
        let (key, val) = value
            .split_once('=')
            .with_context(|| format!("{flag} must be KEY=VALUE: {value}"))?;
        if key.trim().is_empty() {
            bail!("{flag} key must not be empty");
        }
        pairs.insert(key.trim().to_string(), val.to_string());
    }
    Ok(pairs)
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

/// Human rendering for the durable context checkpoint surface. Returns
/// `None` for every other RPC shape so the generic printer owns them.
fn tool_activity_csv(value: &Value) -> String {
    const COLUMNS: &[&str] = &[
        "seq",
        "created_at",
        "tool_call_id",
        "name",
        "status",
        "error_code",
        "retryable",
        "duration_ms",
        "output_bytes",
        "truncated",
        "approval_risk",
        "approval_mode",
        "approval_wait_ms",
        "hint",
    ];
    let mut output = COLUMNS.join(",");
    output.push('\n');
    let Some(entries) = value.get("entries").and_then(Value::as_array) else {
        return output;
    };
    for entry in entries {
        let status = if entry["is_error"].as_bool().unwrap_or(false) {
            entry["error_code"].as_str().unwrap_or("TOOL_FAILED")
        } else {
            "ok"
        };
        let row = [
            entry["seq"].as_u64().unwrap_or(0).to_string(),
            entry["created_at"].as_str().unwrap_or("").to_string(),
            entry["tool_call_id"].as_str().unwrap_or("").to_string(),
            entry["name"].as_str().unwrap_or("").to_string(),
            status.to_string(),
            entry["error_code"].as_str().unwrap_or("").to_string(),
            entry["retryable"].as_bool().unwrap_or(false).to_string(),
            entry["duration_ms"].as_u64().unwrap_or(0).to_string(),
            entry["output_bytes"].as_u64().unwrap_or(0).to_string(),
            entry["truncated"].as_bool().unwrap_or(false).to_string(),
            entry["approval_risk"].as_str().unwrap_or("").to_string(),
            entry["approval_mode"].as_str().unwrap_or("").to_string(),
            entry["approval_wait_ms"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_default(),
            entry["hint"].as_str().unwrap_or("").to_string(),
        ];
        output.push_str(
            &row.iter()
                .map(|cell| csv_cell(cell))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push('\n');
    }
    output
}

fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn filtered_tool_codes(
    category: Option<&str>,
    retryable: Option<bool>,
) -> Vec<vp_core::tooling::errors::ToolErrorCatalogEntry> {
    let mut entries = vp_core::tooling::errors::catalog();
    if let Some(category) = category {
        let category = category.trim().to_ascii_lowercase();
        entries.retain(|entry| entry.category == category);
    }
    if let Some(retryable) = retryable {
        entries.retain(|entry| entry.retryable == retryable);
    }
    entries.sort_by(|left, right| {
        left.category
            .cmp(right.category)
            .then_with(|| left.code.as_str().cmp(right.code.as_str()))
    });
    entries
}

fn tool_activity_lines(value: &Value) -> Option<Vec<String>> {
    let entries = value.get("entries")?.as_array()?;
    let summary = value.get("summary")?;
    let calls = summary.get("calls")?.as_u64()?;
    value.get("total").and_then(Value::as_u64)?;
    if !entries
        .iter()
        .all(|entry| entry.get("duration_ms").is_some())
    {
        return None;
    }
    let mut lines = vec![format!(
        "{} tool call(s) · {} error(s) · {} retryable · avg {} ms · max {} ms · {} output bytes{}",
        calls,
        summary["errors"].as_u64().unwrap_or(0),
        summary["retryable_errors"].as_u64().unwrap_or(0),
        summary["average_duration_ms"].as_u64().unwrap_or(0),
        summary["max_duration_ms"].as_u64().unwrap_or(0),
        summary["output_bytes"].as_u64().unwrap_or(0),
        if value["truncated"].as_bool().unwrap_or(false) {
            " · older events omitted"
        } else {
            ""
        }
    )];
    if entries.is_empty() {
        lines.push("No tool activity recorded for this session.".to_string());
        return Some(lines);
    }
    for entry in entries {
        let status = if entry["is_error"].as_bool().unwrap_or(false) {
            entry["error_code"].as_str().unwrap_or("ERROR")
        } else {
            "ok"
        };
        let retry = if entry["is_error"].as_bool().unwrap_or(false) {
            if entry["retryable"].as_bool().unwrap_or(false) {
                " · retryable"
            } else {
                ""
            }
        } else {
            ""
        };
        let truncated = if entry["truncated"].as_bool().unwrap_or(false) {
            " · truncated"
        } else {
            ""
        };
        lines.push(format!(
            "{}  {:<18} {:<9} {:>6} ms  {:>8} bytes{}{}",
            entry["created_at"].as_str().unwrap_or("?"),
            entry["name"].as_str().unwrap_or("unknown"),
            status,
            entry["duration_ms"].as_u64().unwrap_or(0),
            entry["output_bytes"].as_u64().unwrap_or(0),
            truncated,
            retry
        ));
        if let Some(hint) = entry["hint"].as_str().filter(|hint| !hint.is_empty()) {
            lines.push(format!("    hint: {hint}"));
        }
    }
    Some(lines)
}

fn context_checkpoint_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(checkpoints) = value.get("checkpoints").and_then(Value::as_array)
        && value.get("latest_id").is_some()
    {
        let mut lines = Vec::new();
        lines.push(format!(
            "context checkpoints for {} ({} shown, latest {})",
            value["session_id"].as_str().unwrap_or("?"),
            checkpoints.len(),
            value["latest_id"].as_str().unwrap_or("none")
        ));
        let policy = &value["policy"];
        lines.push(format!(
            "retention: sweep {} · max {} · age {}d",
            if policy["enabled"].as_bool().unwrap_or(false) {
                "on"
            } else {
                "off"
            },
            policy["max_checkpoints"].as_u64().unwrap_or(20),
            policy["max_age_days"].as_u64().unwrap_or(30)
        ));
        for checkpoint in checkpoints {
            let before = checkpoint["tokens_before"].as_u64().unwrap_or(0);
            let after = checkpoint["tokens_after"].as_u64().unwrap_or(0);
            let delta = before.saturating_sub(after);
            let summary = checkpoint["summary"]
                .as_str()
                .or_else(|| checkpoint["label"].as_str())
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let preview = if summary.chars().count() > 80 {
                format!("{}…", summary.chars().take(80).collect::<String>())
            } else {
                summary
            };
            lines.push(format!(
                "{}  {:<8} rev {:<3} {:>7}→{:<7} −{:<7} {}  {}",
                checkpoint["id"].as_str().unwrap_or("?"),
                checkpoint["mode"].as_str().unwrap_or("?"),
                checkpoint["plan_revision"].as_u64().unwrap_or(0),
                before,
                after,
                delta,
                checkpoint["created_at"].as_str().unwrap_or(""),
                preview
            ));
        }
        return Some(lines);
    }
    if let Some(checkpoint) = value.get("checkpoint").filter(|checkpoint| {
        checkpoint.get("mode").is_some() && checkpoint.get("tokens_after").is_some()
    }) && let Some(preflight) = value.get("preflight")
    {
        let mut lines = vec![format!(
            "{}  {}  {}  rev {}  {} → {} tokens",
            checkpoint["id"].as_str().unwrap_or("?"),
            checkpoint["mode"].as_str().unwrap_or("?"),
            checkpoint["label"].as_str().unwrap_or(""),
            checkpoint["plan_revision"].as_u64().unwrap_or(0),
            checkpoint["tokens_before"].as_u64().unwrap_or(0),
            checkpoint["tokens_after"].as_u64().unwrap_or(0)
        )];
        lines.push(format!(
            "active boundary: {} · messages {} · estimate {} / hard {}",
            if value["is_active"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            },
            preflight["active_messages"].as_u64().unwrap_or(0),
            preflight["estimated_tokens"].as_u64().unwrap_or(0),
            preflight["hard_limit_tokens"].as_u64().unwrap_or(0)
        ));
        for error in preflight["errors"].as_array().into_iter().flatten() {
            if let Some(error) = error.as_str() {
                lines.push(format!("error: {error}"));
            }
        }
        for warning in preflight["warnings"].as_array().into_iter().flatten() {
            if let Some(warning) = warning.as_str() {
                lines.push(format!("warning: {warning}"));
            }
        }
        return Some(lines);
    }
    if let Some(restored) = value.get("restored_event_seq") {
        return Some(vec![format!(
            "restored {} ({}) · {} active messages · {} tokens · events {}/{}",
            value["checkpoint_id"].as_str().unwrap_or("?"),
            value["mode"].as_str().unwrap_or("?"),
            value["active_messages"].as_u64().unwrap_or(0),
            value["estimated_tokens"].as_u64().unwrap_or(0),
            value["compaction_event_seq"].as_u64().unwrap_or(0),
            restored.as_u64().unwrap_or(0)
        )]);
    }
    if value.get("removed").is_some()
        && let Some(checkpoint_id) = value.get("checkpoint_id").and_then(Value::as_str)
        && value.get("session_id").is_some()
    {
        return Some(vec![format!(
            "{} {}",
            checkpoint_id,
            if value["removed"].as_bool().unwrap_or(false) {
                "deleted"
            } else {
                "not found"
            }
        )]);
    }
    if let Some(candidates) = value.get("candidates").and_then(Value::as_u64)
        && value.get("session_id").is_some()
        && value
            .get("checkpoints")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| row.get("tokens_before").is_some()))
    {
        return Some(vec![format!(
            "{}pruned {} of {} candidate context checkpoint(s)",
            if value["dry_run"].as_bool().unwrap_or(false) {
                "dry run: "
            } else {
                ""
            },
            value["removed"].as_u64().unwrap_or(0),
            candidates
        )]);
    }
    if let Some(project_id) = value.get("project_id").and_then(Value::as_str)
        && let Some(policy) = value
            .get("policy")
            .filter(|policy| policy.get("max_checkpoints").is_some())
    {
        return Some(vec![format!(
            "{}: context checkpoint sweep {} · max {} · age {}d",
            project_id,
            if policy["enabled"].as_bool().unwrap_or(false) {
                "on"
            } else {
                "off"
            },
            policy["max_checkpoints"].as_u64().unwrap_or(20),
            policy["max_age_days"].as_u64().unwrap_or(30)
        )]);
    }
    None
}

/// Human rendering for `vp governor`, `vp critical-path`, and the project
/// policy commands. It reads only the real RPC payload; missing fields stay
/// absent instead of being invented.
/// Validate the optional CLI preview mode before the RPC. `None` means "use
/// the stored policy"; an unknown value is a local argument error, not a
/// silent fallback that would change admission semantics.
fn parse_governor_mode(value: Option<&str>) -> anyhow::Result<Option<&'static str>> {
    match value {
        None => Ok(None),
        Some(value) => vp_protocol::types::CriticalPathMode::parse(value)
            .map(|mode| Some(mode.as_str()))
            .ok_or_else(|| anyhow::anyhow!("invalid --mode {value:?}; use `single` or `multi`")),
    }
}

fn governor_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(policy) = value.get("policy")
        && value.get("session_id").is_none()
        && policy.get("reserve_ratio").is_some()
    {
        let enabled = policy["enabled"].as_bool().unwrap_or(false);
        let ratio = policy["reserve_ratio"].as_f64().unwrap_or(0.0) * 100.0;
        let throttle = policy["throttle_at_ratio"].as_f64().unwrap_or(0.0) * 100.0;
        let mode = policy["mode"].as_str().unwrap_or("single");
        let mut lines = vec![format!(
            "Critical-path governor: {} · {} · reserve {:.0}% · throttle {:.0}% · boost {} · min {} tokens",
            if enabled { "enabled" } else { "disabled" },
            mode,
            ratio.max(0.0),
            throttle.max(0.0),
            policy["critical_boost"].as_i64().unwrap_or(0),
            policy["min_task_budget"].as_u64().unwrap_or(0)
        )];
        if mode == "multi" {
            lines.push(format!(
                "Fairness: max {} paths · department floor {:.0}% · observed {:.0}% · starvation {} s",
                policy["max_paths"].as_u64().unwrap_or(4),
                policy["department_min_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                policy["observed_weight_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                policy["starvation_after_secs"].as_u64().unwrap_or(0)
            ));
        }
        return Some(lines);
    }
    let session = value.get("session_id")?.as_str()?;
    let reserve = value.get("reserve")?;
    let mut lines = Vec::new();
    lines.push(format!(
        "Governor {}{} · session {session} · plan r{} · {}{}",
        if value["enabled"].as_bool().unwrap_or(false) {
            "enabled"
        } else {
            "disabled"
        },
        reserve["state"]
            .as_str()
            .map(|state| format!(" ({state})"))
            .unwrap_or_default(),
        value["plan_revision"].as_u64().unwrap_or(0),
        value["session_status"].as_str().unwrap_or("unknown"),
        if value["unlimited"].as_bool().unwrap_or(false) {
            " · unlimited"
        } else {
            ""
        }
    ));
    if let Some(path) = value
        .get("critical_path")
        .filter(|path| path["task_ids"].is_array())
    {
        let tasks = path["task_ids"]
            .as_array()
            .map(|tasks| {
                tasks
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            })
            .unwrap_or_default();
        if tasks.is_empty() {
            lines.push(format!(
                "Path: complete · weight {} tokens",
                path["weight_tokens"].as_u64().unwrap_or(0)
            ));
        } else {
            lines.push(format!(
                "Path (weight {} tokens): {tasks}",
                path["weight_tokens"].as_u64().unwrap_or(0)
            ));
        }
    }
    lines.push(format!(
        "Reserve: protected {} of remaining {} · ratio {:.0}% · throttle {:.0}%{}",
        reserve["protected_tokens"].as_u64().unwrap_or(0),
        reserve["remaining_tokens"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unbounded".to_string()),
        reserve["reserve_ratio"].as_f64().unwrap_or(0.0) * 100.0,
        reserve["throttle_at_ratio"].as_f64().unwrap_or(0.0) * 100.0,
        if reserve["throttled"].as_bool().unwrap_or(false) {
            " · throttled"
        } else {
            ""
        }
    ));
    let mode = reserve["mode"]
        .as_str()
        .or_else(|| value["policy"]["mode"].as_str())
        .unwrap_or("single");
    if mode == "multi" {
        lines.push(format!(
            "Fairness: {} path(s) · allocated {} · floors {} · observed {}%{}{}",
            reserve["path_count"].as_u64().unwrap_or(0),
            reserve["allocated_tokens"].as_u64().unwrap_or(0),
            reserve["floor_tokens"].as_u64().unwrap_or(0),
            value["policy"]["observed_weight_ratio"]
                .as_f64()
                .unwrap_or(0.0)
                * 100.0,
            reserve["enumerated_paths"]
                .as_u64()
                .map(|value| format!(" · {value} enumerated"))
                .unwrap_or_default(),
            if reserve["paths_truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        ));
        if let Some(paths) = value["paths"].as_array().filter(|paths| !paths.is_empty()) {
            lines.push("Path table:".to_string());
            for path in paths.iter().take(4) {
                let tasks = path["task_ids"]
                    .as_array()
                    .map(|tasks| {
                        tasks
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" -> ")
                    })
                    .unwrap_or_default();
                let departments = path["departments"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "  #{} weight {} · floor {} · alloc {} · deficit {} · [{}] {}",
                    path["path_id"].as_u64().unwrap_or(0),
                    path["weight_tokens"].as_u64().unwrap_or(0),
                    path["floor_tokens"].as_u64().unwrap_or(0),
                    path["allocation_tokens"].as_u64().unwrap_or(0),
                    path["deficit_tokens"].as_u64().unwrap_or(0),
                    departments,
                    tasks
                ));
            }
            if paths.len() > 4 {
                lines.push(format!("  +{} more protected paths", paths.len() - 4));
            }
        }
        if let Some(departments) = value["departments"]
            .as_array()
            .filter(|departments| !departments.is_empty())
        {
            lines.push("Department floors:".to_string());
            for department in departments.iter().take(6) {
                let path_ids = department["path_ids"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(Value::as_u64)
                            .map(|id| format!("#{id}"))
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "  {} weight {} · floor {} · paths {}",
                    department["department"].as_str().unwrap_or("—"),
                    department["weight_tokens"].as_u64().unwrap_or(0),
                    department["floor_tokens"].as_u64().unwrap_or(0),
                    path_ids
                ));
            }
            if departments.len() > 6 {
                lines.push(format!("  +{} more departments", departments.len() - 6));
            }
        }
    }
    if let Some(admitted) = value["admitted"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        lines.push(format!(
            "Admit: {}",
            admitted
                .iter()
                .filter_map(Value::as_str)
                .take(8)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(deferred) = value["deferred"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        for deferral in deferred.iter().take(5) {
            lines.push(format!(
                "Defer: {} ({}, {})",
                deferral["task_id"].as_str().unwrap_or("?"),
                deferral["reason"].as_str().unwrap_or("?"),
                deferral["code"].as_str().unwrap_or("-")
            ));
        }
        if deferred.len() > 5 {
            lines.push(format!("  +{} more deferred tasks", deferred.len() - 5));
        }
        if let Some(order) = value["deferred_order"]
            .as_array()
            .filter(|order| !order.is_empty())
        {
            lines.push(format!(
                "Deferred order: {}",
                order
                    .iter()
                    .filter_map(Value::as_str)
                    .take(8)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Some(error) = value.get("error").filter(|error| error["code"].is_string()) {
        lines.push(format!(
            "Error: {} · {}",
            error["code"].as_str().unwrap_or("?"),
            error["message"].as_str().unwrap_or("")
        ));
        if let Some(hint) = error["hint"].as_str() {
            lines.push(format!("  hint: {hint}"));
        }
    }
    if let Some(last) = value["last_decisions"]
        .as_array()
        .and_then(|decisions| decisions.last())
    {
        lines.push(format!(
            "Last decision: {} {} at {} · {}",
            last["action"].as_str().unwrap_or("?"),
            last["reason"].as_str().unwrap_or("?"),
            last["created_at"].as_str().unwrap_or("?"),
            last["detail"].as_str().unwrap_or("")
        ));
    }
    Some(lines)
}

fn lsp_status_lines(value: &Value) -> Option<Vec<String>> {
    let languages = value.get("languages")?.as_array()?;
    value.get("supported_languages")?;
    let mut lines = vec![format!(
        "code intelligence · {} · {} running · {} installed",
        value["root"].as_str().unwrap_or("?"),
        value["running"].as_u64().unwrap_or(0),
        value["installed"].as_u64().unwrap_or(0)
    )];
    for language in languages {
        let state = if language["running"].as_bool().unwrap_or(false) {
            "running"
        } else if language["installed"].as_bool().unwrap_or(false) {
            "installed"
        } else {
            "missing"
        };
        let server = language["server"].as_str().unwrap_or("—");
        let capabilities = language["capabilities"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "—".to_string());
        lines.push(format!(
            "  {:<12} {:<10} {:<20} {}",
            language["display"].as_str().unwrap_or("?"),
            state,
            server,
            capabilities
        ));
        if state == "missing"
            && let Some(hint) = language["install_hint"].as_str()
            && !hint.is_empty()
        {
            lines.push(format!("    {hint}"));
        }
    }
    Some(lines)
}

fn process_console_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_processes").is_some())
    {
        let project = value["project_id"].as_str().unwrap_or("?");
        return Some(vec![
            format!("project {project} process policy"),
            format!(
                "  idle sweep {} · max {} · idle TTL {}s",
                if policy["idle_sweep_enabled"].as_bool().unwrap_or(false) {
                    "on"
                } else {
                    "off"
                },
                policy["max_processes"].as_u64().unwrap_or(0),
                policy["idle_ttl_secs"].as_u64().unwrap_or(0),
            ),
            format!(
                "  retained log {} bytes / {} lines",
                policy["max_output_bytes"].as_u64().unwrap_or(0),
                policy["max_output_lines"].as_u64().unwrap_or(0),
            ),
        ]);
    }
    if let Some(processes) = value.get("processes").and_then(Value::as_array)
        && value.get("active").is_some()
    {
        if processes.is_empty() {
            return Some(vec!["No supervised processes.".to_string()]);
        }
        let mut lines = Vec::new();
        for process in processes {
            let id = process["id"].as_str().unwrap_or("?");
            let state = process["state"].as_str().unwrap_or("?");
            let name = process["name"].as_str().unwrap_or("");
            let pid = process["pid"].as_i64().unwrap_or(0);
            let elapsed = process["started_at"].as_str().unwrap_or("");
            let exit = process["exit_code"]
                .as_i64()
                .map(|code| format!("  exit {code}"))
                .unwrap_or_default();
            lines.push(format!(
                "{id}  {state:<8}  pid {pid:<7}  {name}{exit}  started {elapsed}  {} bytes",
                process["output_bytes"].as_u64().unwrap_or(0)
            ));
            if let Some(detail) = process["detail"].as_str() {
                lines.push(format!("  {detail}"));
            }
        }
        return Some(lines);
    }
    if let Some(process_id) = value.get("process_id").and_then(Value::as_str)
        && let Some(content) = value.get("content").and_then(Value::as_str)
        && value.get("next_offset").is_some()
    {
        let mut lines = vec![format!(
            "{process_id}  {}  bytes {}/{}  offset {} -> {}{}",
            value["state"].as_str().unwrap_or("?"),
            value["content"].as_str().map(str::len).unwrap_or(0),
            value["total_bytes"].as_u64().unwrap_or(0),
            value["offset"].as_u64().unwrap_or(0),
            value["next_offset"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                "  (truncated)"
            } else {
                ""
            }
        )];
        if !content.is_empty() {
            lines.push(content.to_string());
        }
        return Some(lines);
    }
    if let Some(process) = value.get("process")
        && value.get("forced").is_some()
    {
        return Some(vec![format!(
            "{}  {}  exit {}{}  waited {}ms{}",
            process["id"].as_str().unwrap_or("?"),
            process["state"].as_str().unwrap_or("?"),
            process["exit_code"].as_i64().unwrap_or(-1),
            process["signal"]
                .as_i64()
                .map(|signal| format!("  signal {signal}"))
                .unwrap_or_default(),
            value["waited_ms"].as_u64().unwrap_or(0),
            if value["forced"].as_bool().unwrap_or(false) {
                "  (forced)"
            } else {
                ""
            }
        )]);
    }
    None
}

fn scaling_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(decision) = value
        .get("decision")
        .filter(|decision| decision.is_object())
    {
        let mut lines = Vec::new();
        let session = value
            .get("session_id")
            .and_then(Value::as_str)
            .or_else(|| decision.get("session_id").and_then(Value::as_str))
            .unwrap_or("?");
        lines.push(format!(
            "scaling preview  {session}  action={}  Workers {}→{}  Labour {}→{}",
            decision
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("hold"),
            decision
                .get("current_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("target_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("current_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("target_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ));
        lines.push(format!(
            "  ready {} · running {} · active Workers {} · active Labour {} · revision {} · applied {}",
            decision
                .get("ready_tasks")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("running_tasks")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("active_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("active_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("plan_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("applied")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ));
        if let Some(reasons) = decision.get("reasons").and_then(Value::as_array) {
            lines.push(format!(
                "  reasons: {}",
                reasons
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        if let Some(hash) = decision.get("inputs_hash").and_then(Value::as_str) {
            let prefix = hash.get(..12).unwrap_or(hash);
            lines.push(format!("  inputs {prefix}…"));
        }
        if let Some(evidence) = decision.get("manager_evidence").and_then(Value::as_str) {
            lines.push(format!("  manager evidence: {evidence}"));
        }
        return Some(lines);
    }
    if value.get("global_worker_cap").is_some()
        && let Some(projects) = value.get("projects").and_then(Value::as_array)
    {
        let mut lines = vec![format!(
            "scaling  global Workers {}/{} available · Labour {}/{} available",
            value
                .get("global_worker_available")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_worker_cap")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_labour_available")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_labour_cap")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )];
        if projects.is_empty() {
            lines.push("  no projects".to_string());
            return Some(lines);
        }
        for project in projects {
            let name = project
                .get("project_name")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let policy = project.get("policy").unwrap_or(&Value::Null);
            lines.push(format!(
                "{name}  scaling={}  Workers {}-{}  Labour {}-{}  threshold {}  cooldown {}s  step {}  budget {}",
                if policy
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "on"
                } else {
                    "off"
                },
                policy
                    .get("min_workers")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_workers")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("min_labour")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_labour")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("expand_ready_threshold")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("cooldown_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_step")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("budget_headroom_ratio")
                    .and_then(Value::as_f64)
                    .map(|ratio| format!("≥{:.0}%", ratio * 100.0))
                    .unwrap_or_else(|| "off".to_string())
            ));
            for session in project
                .get("sessions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let latest = session.get("latest").unwrap_or(&Value::Null);
                let action = latest
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("none");
                lines.push(format!(
                    "  {}  {:<10}  rev {}  tasks {}/{} ready  slots {}W/{}L  active {}W/{}L  latest {}  {}",
                    session
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    session
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    session
                        .get("plan_revision")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("ready_tasks")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("plan_tasks")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("current_workers")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("current_labour")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("active_workers")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("active_labour")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    action,
                    session
                        .get("goal")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                ));
                if latest.is_object()
                    && let Some(reasons) = latest.get("reasons").and_then(Value::as_array)
                    && !reasons.is_empty()
                {
                    lines.push(format!(
                        "    {}",
                        reasons
                            .iter()
                            .filter_map(Value::as_str)
                            .take(6)
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ));
                }
            }
        }
        return Some(lines);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_workers").is_some() && policy.get("max_labour").is_some())
    {
        let mut lines = vec![format!(
            "scaling policy  {}  enabled={}",
            value
                .get("project_id")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            policy
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )];
        lines.push(format!(
            "  Workers {}-{}  Labour {}-{}  threshold {}  critical delay {}s  idle {}s  cooldown {}s  step {}",
            policy
                .get("min_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("min_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("expand_ready_threshold")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("expand_critical_delay_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("scale_down_idle_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("cooldown_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_step")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
        lines.push(format!(
            "  budget headroom: {}",
            policy
                .get("budget_headroom_ratio")
                .and_then(Value::as_f64)
                .map(|ratio| format!("≥{:.0}%", ratio * 100.0))
                .unwrap_or_else(|| "check disabled".to_string())
        ));
        return Some(lines);
    }
    None
}

fn remote_integration_lines(value: &Value) -> Option<Vec<String>> {
    if value.get("removed_refs").is_some() && value.get("skipped").is_some() {
        let refs = value["removed_refs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let worktrees = value["removed_worktrees"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let skipped = value["skipped"].as_array().cloned().unwrap_or_default();
        let mut lines = vec![format!(
            "remote integration cleanup for {}: examined {} · removed {} ref(s) · {} worktree(s) · {} skipped",
            value["project_id"].as_str().unwrap_or("?"),
            value["entries_examined"].as_u64().unwrap_or(0),
            refs.len(),
            worktrees.len(),
            skipped.len()
        )];
        for reference in refs {
            if let Some(reference) = reference.as_str() {
                lines.push(format!("  removed {reference}"));
            }
        }
        for worktree in worktrees {
            if let Some(worktree) = worktree.as_str() {
                lines.push(format!("  removed worktree {worktree}"));
            }
        }
        for reason in skipped {
            if let Some(reason) = reason.as_str() {
                lines.push(format!("  skipped {reason}"));
            }
        }
        return Some(lines);
    }
    if let (Some(entry), Some(bundle), Some(verification)) = (
        value.get("entry"),
        value.get("bundle"),
        value.get("verification"),
    ) {
        let mut lines = Vec::new();
        lines.push(format!(
            "remote integration {}  {}  {}:{}",
            entry["status"].as_str().unwrap_or("?"),
            entry["id"].as_str().unwrap_or("?"),
            bundle["host"].as_str().unwrap_or("?"),
            bundle["branch"].as_str().unwrap_or("?")
        ));
        lines.push(format!(
            "  commit {}  bundle sha256 {}  bytes {}",
            entry["commit"].as_str().unwrap_or("?"),
            bundle["sha256"].as_str().unwrap_or("?"),
            bundle["bytes"].as_u64().unwrap_or(0)
        ));
        lines.push(format!(
            "  verifier required={} performed={} approved={}",
            verification["required"].as_bool().unwrap_or(false),
            verification["performed"].as_bool().unwrap_or(false),
            verification["approved"]
                .as_bool()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "—".into())
        ));
        if let Some(summary) = verification["summary"].as_str() {
            lines.push(format!("  verdict {summary}"));
        }
        lines.push(format!(
            "  imported_ref {}  refs_pruned {}",
            value["imported_ref"].as_str().unwrap_or("—"),
            value["refs_pruned"].as_bool().unwrap_or(false)
        ));
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat {caveat}"));
        }
        return Some(lines);
    }
    if let Some(policy) = value.get("policy")
        && let Some(entries) = value.get("entries").and_then(Value::as_array)
    {
        let mut lines = vec![format!(
            "remote integration policy enabled={} verifier_required={} max_bundle_bytes={} keep_refs={}",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["require_verifier"].as_bool().unwrap_or(true),
            policy["max_bundle_bytes"].as_u64().unwrap_or(0),
            policy["keep_import_refs"].as_bool().unwrap_or(false)
        )];
        let hosts = policy["allowed_hosts"]
            .as_array()
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        lines.push(format!(
            "  allowed hosts: {}",
            if hosts.is_empty() {
                "(none)".to_string()
            } else {
                hosts
            }
        ));
        lines.push(format!(
            "  queue total {} · pending {} · merged {} · conflicts {} · failed {} · refs retained {}",
            value["total"].as_u64().unwrap_or(0),
            value["pending"].as_u64().unwrap_or(0),
            value["merged"].as_u64().unwrap_or(0),
            value["conflicts"].as_u64().unwrap_or(0),
            value["failed"].as_u64().unwrap_or(0),
            value["retained_refs"].as_u64().unwrap_or(0)
        ));
        if entries.is_empty() {
            lines.push("  no remote queue entries".to_string());
        }
        for entry in entries {
            lines.push(format!(
                "  {}  {:<10}  {}  {}:{}  attempt {}  {}",
                entry["entry_id"].as_str().unwrap_or("?"),
                entry["status"].as_str().unwrap_or("?"),
                entry["commit"]
                    .as_str()
                    .unwrap_or("?")
                    .chars()
                    .take(12)
                    .collect::<String>(),
                entry["remote_host"].as_str().unwrap_or("?"),
                entry["remote_branch"].as_str().unwrap_or("?"),
                entry["attempt"].as_u64().unwrap_or(0),
                entry["imported_ref"].as_str().unwrap_or("—")
            ));
            if let Some(approved) = entry["verifier_approved"].as_bool() {
                lines.push(format!(
                    "    verifier {}: {}",
                    if approved { "approved" } else { "rejected" },
                    entry["verifier_summary"].as_str().unwrap_or("no summary")
                ));
            }
            if let Some(detail) = entry["detail"].as_str()
                && !detail.is_empty()
            {
                lines.push(format!("    {detail}"));
            }
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat {caveat}"));
        }
        return Some(lines);
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && value.get("entries").is_none()
    {
        let hosts = policy["allowed_hosts"]
            .as_array()
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        return Some(vec![
            format!(
                "remote integration policy for {} enabled={} verifier_required={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["enabled"].as_bool().unwrap_or(false),
                policy["require_verifier"].as_bool().unwrap_or(true)
            ),
            format!(
                "  max_bundle_bytes {} · keep_import_refs {} · allowed hosts {}",
                policy["max_bundle_bytes"].as_u64().unwrap_or(0),
                policy["keep_import_refs"].as_bool().unwrap_or(false),
                if hosts.is_empty() {
                    "(none)".to_string()
                } else {
                    hosts
                }
            ),
        ]);
    }
    None
}

fn test_cache_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(map) = value.get("map") {
        let mut lines = vec![format!(
            "test impact  session {}  project {}  {} target(s)  generation {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            map["targets"].as_array().map(Vec::len).unwrap_or(0),
            map["generation"].as_str().unwrap_or("?")
        )];
        if let Some(command) = value.get("test_command").and_then(Value::as_str) {
            lines.push(format!("  command: {command}"));
        }
        lines.push(format!(
            "  lsp_available={} lexical_only={} truncated={} changed={}",
            map["lsp_available"].as_bool().unwrap_or(false),
            map["lexical_only"].as_bool().unwrap_or(false),
            map["truncated"].as_bool().unwrap_or(false),
            map["changed_paths"].as_array().map(Vec::len).unwrap_or(0)
        ));
        for target in map["targets"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  {:<42} {:<8} {:>5.0}%  {}",
                target["path"].as_str().unwrap_or("?"),
                target["source"].as_str().unwrap_or("?"),
                target["confidence"].as_f64().unwrap_or(0.0) * 100.0,
                target["reason"].as_str().unwrap_or("")
            ));
        }
        for note in map["notes"].as_array().into_iter().flatten() {
            lines.push(format!("  note: {}", note.as_str().unwrap_or("?")));
        }
        return Some(lines);
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && value.get("total").is_some()
        && value.get("project_id").is_some()
    {
        let mut lines = vec![format!(
            "test cache entries  {}  {} of {} shown",
            value["project_id"].as_str().unwrap_or("?"),
            entries.len(),
            value["total"].as_u64().unwrap_or(0)
        )];
        for entry in entries {
            lines.push(format!(
                "  {}  {:<5} {:<6}  exit {:<3}  {:>7}ms  hits {:<4}  {}",
                entry["id"].as_str().unwrap_or("?"),
                entry["kind"].as_str().unwrap_or("?"),
                entry["status"].as_str().unwrap_or("?"),
                entry["exit_code"].as_i64().unwrap_or(0),
                entry["duration_ms"].as_u64().unwrap_or(0),
                entry["hit_count"].as_u64().unwrap_or(0),
                entry["command"].as_str().unwrap_or("")
            ));
        }
        if entries.is_empty() {
            lines.push("  no cached results".to_string());
        }
        return Some(lines);
    }
    if value.get("pruned").is_some() && value.get("affected_sessions").is_some() {
        let mut lines = vec![format!(
            "test cache prune  {}  {} removed · {} remaining · {} session(s) · dry_run={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["pruned"].as_u64().unwrap_or(0),
            value["remaining"].as_u64().unwrap_or(0),
            value["affected_sessions"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        )];
        if let Some(ids) = value.get("removed_ids").and_then(Value::as_array) {
            for id in ids.iter().take(20).filter_map(Value::as_str) {
                lines.push(format!("  removed {id}"));
            }
        }
        return Some(lines);
    }
    if value.get("cleared").is_some() && value.get("affected_sessions").is_some() {
        return Some(vec![format!(
            "test cache clear  {}  {} cleared · {} remaining · {} session(s)",
            value["project_id"].as_str().unwrap_or("?"),
            value["cleared"].as_u64().unwrap_or(0),
            value["remaining"].as_u64().unwrap_or(0),
            value["affected_sessions"].as_u64().unwrap_or(0)
        )]);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_output_bytes").is_some())
    {
        let mut lines = vec![format!(
            "test cache policy  {}  enabled={}  max-entries={}  retention={}d  output≤{}B  reuse-lint={}  integrator={}  failed-ttl={}s",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(true),
            policy["max_entries"].as_u64().unwrap_or(0),
            policy["retention_days"].as_u64().unwrap_or(0),
            policy["max_output_bytes"].as_u64().unwrap_or(0),
            policy["reuse_lint"].as_bool().unwrap_or(true),
            policy["include_integration"].as_bool().unwrap_or(false),
            policy["failed_retention_secs"].as_u64().unwrap_or(0)
        )];
        if value.get("entries").is_some_and(Value::is_u64) {
            lines.push(format!(
                "  entries={} passed={} failed={} error={} expired={} hits={} output={}B",
                value["entries"].as_u64().unwrap_or(0),
                value["passed"].as_u64().unwrap_or(0),
                value["failed"].as_u64().unwrap_or(0),
                value["error"].as_u64().unwrap_or(0),
                value["expired"].as_u64().unwrap_or(0),
                value["total_hits"].as_u64().unwrap_or(0),
                value["output_bytes"].as_u64().unwrap_or(0)
            ));
            if let Some(kinds) = value.get("by_kind").and_then(Value::as_object) {
                let summary = kinds
                    .iter()
                    .map(|(kind, count)| format!("{kind}={}", count.as_u64().unwrap_or(0)))
                    .collect::<Vec<_>>()
                    .join(" ");
                if !summary.is_empty() {
                    lines.push(format!("  kinds: {summary}"));
                }
            }
        }
        return Some(lines);
    }
    None
}

fn print_human(value: &Value) {
    if let Some(lines) = test_cache_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = process_console_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = governor_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = remote_integration_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = lsp_status_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = scaling_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = tool_activity_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = context_checkpoint_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
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
        && value.get("calls_checked").and_then(Value::as_u64).is_some()
        && value.get("findings").and_then(Value::as_array).is_some()
    {
        println!("cache audit  {session_id}");
        println!(
            "  calls            {} agent · {} warm · {} compaction resets",
            value["calls_checked"].as_u64().unwrap_or(0),
            value["warm_calls_checked"].as_u64().unwrap_or(0),
            value["compactions_checked"].as_u64().unwrap_or(0)
        );
        println!(
            "  hit ratio        {:.2}% · read {} · write {}",
            value["hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0)
        );
        if value["clean"].as_bool().unwrap_or(false) {
            println!("  result           clean (no warning or critical findings)");
        }
        for finding in value["findings"].as_array().into_iter().flatten() {
            println!(
                "  [{:<8}] {:<32} {}",
                finding["severity"].as_str().unwrap_or("info"),
                finding["code"].as_str().unwrap_or("?"),
                finding["message"].as_str().unwrap_or("")
            );
            if let Some(evidence) = finding["evidence"].as_object()
                && !evidence.is_empty()
            {
                let rendered = evidence
                    .iter()
                    .map(|(key, value)| format!("{key}={}", compact_json(value)))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("             evidence: {rendered}");
            }
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("policy").is_some()
        && value
            .get("roles")
            .and_then(Value::as_array)
            .is_some_and(|roles| roles.iter().any(|role| role.get("prefix_hash").is_some()))
    {
        println!("cache plan  {session_id}");
        if let Some(provider) = value["provider"].as_str() {
            println!("  provider         {provider}");
        }
        if let Some(policy) = value.get("policy") {
            println!(
                "  policy           enabled {} · ttl {} · automatic {} · role overrides {}",
                policy["enabled"].as_bool().unwrap_or(true),
                policy["ttl"].as_str().unwrap_or("5m"),
                policy["automatic_caching"].as_bool().unwrap_or(true),
                policy["role_ttl"]
                    .as_object()
                    .map_or(0, serde_json::Map::len)
            );
        }
        for role in value["roles"].as_array().into_iter().flatten() {
            println!(
                "  {:<9} {} · {} breakpoints · ttl {} · tail {}",
                role["role"].as_str().unwrap_or("?"),
                role["model"].as_str().unwrap_or("?"),
                role["cache_breakpoints"].as_u64().unwrap_or(0),
                role["cache_ttl"].as_str().unwrap_or("5m"),
                role["tail_message"].as_bool().unwrap_or(false)
            );
            println!(
                "            core {}… project {}… tools {}… prefix {}…",
                short_hash(&role["core_hash"]),
                short_hash(&role["project_hash"]),
                short_hash(&role["tools_hash"]),
                short_hash(&role["prefix_hash"])
            );
            println!(
                "            sections {} core / {} project chars · {} tools · ~{} + {} tokens",
                role["core_chars"].as_u64().unwrap_or(0),
                role["project_chars"].as_u64().unwrap_or(0),
                role["tool_count"].as_u64().unwrap_or(0),
                role["system_tokens_estimate"].as_u64().unwrap_or(0),
                role["tool_tokens_estimate"].as_u64().unwrap_or(0)
            );
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("warmed").and_then(Value::as_bool).is_some()
        && value.get("source").and_then(Value::as_str).is_some()
    {
        println!(
            "cache warm  {session_id}  [{}]  {}",
            value["source"].as_str().unwrap_or("manual"),
            if value["warmed"].as_bool().unwrap_or(false) {
                "warmed"
            } else {
                "not warmed"
            }
        );
        println!(
            "  provider         {} · {}",
            value["provider"].as_str().unwrap_or("?"),
            value["model"].as_str().unwrap_or("?")
        );
        println!(
            "  breakpoints      {} · ttl {} · automatic {}",
            value["cache_breakpoints"].as_u64().unwrap_or(0),
            value["cache_ttl"].as_str().unwrap_or("?"),
            value["automatic"].as_bool().unwrap_or(false)
        );
        println!(
            "  tokens           input {} · cache read {} · cache write {} · output {}",
            value["input_tokens"].as_u64().unwrap_or(0),
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0)
        );
        println!(
            "  cost             ${:.6}{}",
            value["cost_usd"].as_f64().unwrap_or(0.0),
            if value["estimated"].as_bool().unwrap_or(false) {
                " (estimated usage)"
            } else {
                ""
            }
        );
        if let Some(message) = value["message"]
            .as_str()
            .filter(|message| !message.is_empty())
        {
            println!("  message          {message}");
        }
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
        println!(
            "  savings          ${:.6} (estimated from configured prices; negative means cache writes cost more than reads saved)",
            value["estimated_savings_usd"].as_f64().unwrap_or(0.0)
        );
        println!(
            "  calls            {} cache-observed provider calls",
            value["cache_calls"].as_u64().unwrap_or(0)
        );
        println!(
            "  plan             {} breakpoints · ttl {}",
            value["last_breakpoints"].as_u64().unwrap_or(0),
            value["last_cache_ttl"].as_str().unwrap_or("unknown")
        );
        println!(
            "  last miss        {}",
            value["last_miss_reason"]
                .as_str()
                .unwrap_or("none recorded")
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
        if let Some(policy) = value.get("policy").filter(|policy| policy.is_object()) {
            println!(
                "  policy           enabled {} · ttl {} · automatic {} · warm-on-resume {} · compact-aware {}",
                policy["enabled"].as_bool().unwrap_or(true),
                policy["ttl"].as_str().unwrap_or("5m"),
                policy["automatic_caching"].as_bool().unwrap_or(true),
                policy["warm_on_resume"].as_bool().unwrap_or(false),
                policy["compact_aware"].as_bool().unwrap_or(true)
            );
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
        if let Some(instance) = value.get("daemon_instance") {
            println!(
                "  instance {}  pid {}  started {}",
                instance
                    .get("instance_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?"),
                instance
                    .get("pid")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                instance
                    .get("started_at")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?")
            );
        }
        for lease in leases {
            println!(
                "  {}  {}  {}  generation {}  {}  expires {}  owner {} ({})",
                lease["kind"].as_str().unwrap_or("?"),
                lease["lease_key"].as_str().unwrap_or("?"),
                lease["session_id"].as_str().unwrap_or("-"),
                lease["fencing_token"].as_u64().unwrap_or(0),
                lease["state"].as_str().unwrap_or("?"),
                lease["expires_at"].as_str().unwrap_or("?"),
                lease["owner_id"].as_str().unwrap_or("?"),
                lease["owner_instance"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && entries
            .iter()
            .all(|entry| entry.get("action").is_some() && entry.get("lease_key").is_some())
    {
        if entries.is_empty() {
            println!("No runtime lease history.");
        }
        for entry in entries {
            println!(
                "  {}  {}  {}  generation {}  {} ({})  {}",
                entry["occurred_at"].as_str().unwrap_or("?"),
                entry["action"].as_str().unwrap_or("?"),
                entry["lease_key"].as_str().unwrap_or("?"),
                entry["fencing_token"].as_u64().unwrap_or(0),
                entry["owner_id"].as_str().unwrap_or("?"),
                entry["owner_instance"].as_str().unwrap_or("?"),
                entry["detail"].as_str().unwrap_or("")
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
        if let Some(mode) = value["mode"].as_str().filter(|mode| !mode.is_empty()) {
            println!("mode={mode}");
        }
        if let Some(reason) = value["semantic_unavailable"].as_str() {
            println!("(semantic unavailable: {reason})");
        }
        return;
    }
    if value.get("indexable_chunks").is_some() && value.get("pending_jobs").is_some() {
        println!(
            "source embeddings · enabled={} configured={} · model={} · {} dims · coverage {:.0}%",
            value["enabled"].as_bool().unwrap_or(false),
            value["configured"].as_bool().unwrap_or(false),
            value["model"].as_str().unwrap_or("-"),
            value["dimensions"].as_u64().unwrap_or(0),
            value["coverage"].as_f64().unwrap_or(0.0) * 100.0,
        );
        println!(
            "chunks {} indexed · {} stale · {} invalid · cap {} · retention {}d",
            value["indexable_chunks"].as_u64().unwrap_or(0),
            value["stale_chunks"].as_u64().unwrap_or(0),
            value["invalid_chunks"].as_u64().unwrap_or(0),
            value["max_chunks_per_source"].as_u64().unwrap_or(0),
            value["retention_days"].as_u64().unwrap_or(0),
        );
        println!(
            "jobs {} pending · {} running · {} failed · {} dead · {} done",
            value["pending_jobs"].as_u64().unwrap_or(0),
            value["running_jobs"].as_u64().unwrap_or(0),
            value["failed_jobs"].as_u64().unwrap_or(0),
            value["dead_jobs"].as_u64().unwrap_or(0),
            value["done_jobs"].as_u64().unwrap_or(0),
        );
        let usage = &value["usage"];
        println!(
            "usage {} calls · {} input tokens ({} estimated) · {} bytes",
            usage["calls"].as_u64().unwrap_or(0),
            usage["input_tokens"].as_u64().unwrap_or(0),
            usage["estimated_tokens"].as_u64().unwrap_or(0),
            usage["bytes"].as_u64().unwrap_or(0),
        );
        if let Some(env) = value["endpoint_env"].as_str() {
            println!(
                "endpoint_env={env} present={}",
                value["endpoint_env_present"].as_bool().unwrap_or(false)
            );
        }
        if let Some(reason) = value["semantic_unavailable"].as_str() {
            println!("semantic unavailable: {reason}");
        }
        if let Some(error) = value["last_error"].as_str() {
            println!("last error: {error}");
        }
        for job in value["recent_jobs"].as_array().into_iter().flatten() {
            println!(
                "  {:<7} {}/{} chunks · attempts {} · {}",
                job["status"].as_str().unwrap_or("?"),
                job["chunks_done"].as_u64().unwrap_or(0),
                job["chunks_total"].as_u64().unwrap_or(0),
                job["attempts"].as_u64().unwrap_or(0),
                job["source_title"].as_str().unwrap_or("?"),
            );
        }
        return;
    }
    if value.get("enqueued_sources").is_some() && value.get("cleared_vectors").is_some() {
        println!(
            "source embedding reindex · enabled={} · cleared {} vectors / {} jobs · queued {} sources",
            value["enabled"].as_bool().unwrap_or(false),
            value["cleared_vectors"].as_u64().unwrap_or(0),
            value["cleared_jobs"].as_u64().unwrap_or(0),
            value["enqueued_sources"].as_u64().unwrap_or(0),
        );
        return;
    }
    if let Some(policy) = value.get("policy")
        && policy.get("max_chunks_per_source").is_some()
    {
        println!(
            "source embedding policy · enabled={} · model={} · endpoint_env={} · {} dims",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["model"].as_str().unwrap_or("-"),
            policy["endpoint_env"].as_str().unwrap_or("-"),
            policy["dimensions"].as_u64().unwrap_or(0),
        );
        println!(
            "batch {} · max chunks {} · retention {}d · timeout {}s · api_key_env={}",
            policy["batch_size"].as_u64().unwrap_or(0),
            policy["max_chunks_per_source"].as_u64().unwrap_or(0),
            policy["retention_days"].as_u64().unwrap_or(0),
            policy["timeout_secs"].as_u64().unwrap_or(0),
            policy["api_key_env"].as_str().unwrap_or("-"),
        );
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
                "{:>8.3} [{}·{}] {} · {} source={}{}{}",
                matched["score"].as_f64().unwrap_or(0.0),
                memory["kind"].as_str().unwrap_or("note"),
                memory["source"].as_str().unwrap_or("agent"),
                memory["title"].as_str().unwrap_or("?"),
                memory["id"].as_str().unwrap_or("?"),
                matched["source"].as_str().unwrap_or("lexical"),
                matched["lexical_score"]
                    .as_f64()
                    .map(|score| format!(" lexical={score:.3}"))
                    .unwrap_or_default(),
                matched["semantic_score"]
                    .as_f64()
                    .map(|score| format!(" semantic={score:.3}"))
                    .unwrap_or_default()
            );
            println!("  reasons: {reasons}");
            if let Some(reason) = matched["semantic_unavailable"].as_str() {
                println!("  semantic_unavailable: {reason}");
            }
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
    if value.get("indexable").is_some() && value.get("coverage").is_some() {
        println!(
            "memory embeddings: enabled={} configured={} model={} dimensions={}",
            value["enabled"].as_bool().unwrap_or(false),
            value["configured"].as_bool().unwrap_or(false),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0)
        );
        println!(
            "  coverage={:.1}% indexed={}/{} pending={} failed={} skipped={}",
            value["coverage"].as_f64().unwrap_or(0.0) * 100.0,
            value["indexed"].as_u64().unwrap_or(0),
            value["indexable"].as_u64().unwrap_or(0),
            value["pending"].as_u64().unwrap_or(0),
            value["failed"].as_u64().unwrap_or(0),
            value["skipped"].as_u64().unwrap_or(0)
        );
        if let Some(error) = value["last_error"].as_str() {
            println!("  last_error: {error}");
        }
        if let Some(usage) = value.get("usage") {
            println!(
                "  embedding_calls={} input_tokens={} estimated_tokens={} bytes={} provider_reported_calls={}",
                usage["calls"].as_u64().unwrap_or(0),
                usage["input_tokens"].as_u64().unwrap_or(0),
                usage["estimated_tokens"].as_u64().unwrap_or(0),
                usage["bytes"].as_u64().unwrap_or(0),
                usage["provider_reported_calls"].as_u64().unwrap_or(0)
            );
        }
        if let Some(jobs) = value.get("recent_jobs").and_then(Value::as_array) {
            for job in jobs.iter().take(10) {
                println!(
                    "  {} {} attempts={} updated={}{}",
                    job["status"].as_str().unwrap_or("?"),
                    job["title"].as_str().unwrap_or("?"),
                    job["attempts"].as_u64().unwrap_or(0),
                    job["updated_at"].as_str().unwrap_or("?"),
                    job["last_error"]
                        .as_str()
                        .map(|error| format!(" · {error}"))
                        .unwrap_or_default()
                );
            }
        }
        return;
    }
    if value.get("cleared").is_some() && value.get("enqueued").is_some() {
        println!(
            "memory embeddings rebuild: cleared={} enqueued={} model={} dimensions={} enabled={}",
            value["cleared"].as_u64().unwrap_or(0),
            value["enqueued"].as_u64().unwrap_or(0),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0),
            value["enabled"].as_bool().unwrap_or(false)
        );
        return;
    }
    if value.get("ok").is_some() && value.get("latency_ms").is_some() {
        println!(
            "memory embeddings test: ok={} model={} dimensions={} latency_ms={} input_tokens={} estimated_tokens={} provider_reported={}",
            value["ok"].as_bool().unwrap_or(false),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0),
            value["latency_ms"].as_u64().unwrap_or(0),
            value["input_tokens"].as_u64().unwrap_or(0),
            value["estimated_tokens"].as_u64().unwrap_or(0),
            value["provider_reported"].as_bool().unwrap_or(false)
        );
        if let Some(error) = value["error"].as_str() {
            println!("  error: {error}");
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
    if value.get("hash_mismatches").is_some() && value.get("orphan_blobs").is_some() {
        println!(
            "tool-output audit: {} row(s), {} blob(s), {} missing, {} mismatched, {} unsafe, {} orphan(s) ({} bytes, cleaned {})",
            value["rows"].as_u64().unwrap_or(0),
            value["blob_files"].as_u64().unwrap_or(0),
            value["missing_blobs"].as_u64().unwrap_or(0),
            value["hash_mismatches"].as_u64().unwrap_or(0),
            value["unsafe_blob_files"].as_u64().unwrap_or(0),
            value["orphan_blobs"].as_u64().unwrap_or(0),
            value["orphan_bytes"].as_u64().unwrap_or(0),
            value["cleaned_orphans"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && value.get("total").is_some()
    {
        if entries.is_empty() {
            println!("No spilled tool outputs.");
        } else {
            for entry in entries {
                println!(
                    "{}  {:<12}  {:>8}B  {} lines  {}",
                    entry["id"].as_str().unwrap_or("?"),
                    entry["tool_name"].as_str().unwrap_or("?"),
                    entry["bytes"].as_u64().unwrap_or(0),
                    entry["lines"].as_u64().unwrap_or(0),
                    entry["created_at"].as_str().unwrap_or("?")
                );
            }
            println!(
                "{} of {} handle(s) shown.",
                entries.len(),
                value["total"].as_u64().unwrap_or(0)
            );
        }
        return;
    }
    if value.get("entries").is_some_and(Value::is_u64)
        && value
            .get("policy")
            .is_some_and(|policy| policy.get("spill_threshold_bytes").is_some())
    {
        println!(
            "tool outputs: {} handle(s), {} bytes stored, {} active, {} expired",
            value["entries"].as_u64().unwrap_or(0),
            value["total_bytes"].as_u64().unwrap_or(0),
            value["active_entries"].as_u64().unwrap_or(0),
            value["expired_entries"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(policy) = value.get("policy") {
        if policy.get("spill_threshold_bytes").is_some() {
            println!(
                "project {}  tool-output={}  spill>{}  preview={}  turn-budget={}  retention={}s  max-entries={}",
                value["project_id"].as_str().unwrap_or("?"),
                if policy["enabled"].as_bool().unwrap_or(true) {
                    "enabled"
                } else {
                    "disabled"
                },
                policy["spill_threshold_bytes"].as_u64().unwrap_or(0),
                policy["preview_bytes"].as_u64().unwrap_or(0),
                policy["max_total_bytes_per_turn"].as_u64().unwrap_or(0),
                policy["retention_secs"].as_u64().unwrap_or(0),
                policy["max_entries"].as_u64().unwrap_or(0)
            );
            return;
        }
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
                "{}  {}  [{}]  risk={}  waiters={}  priority={}",
                approval["id"].as_str().unwrap_or("?"),
                approval["tool"].as_str().unwrap_or("?"),
                approval["status"].as_str().unwrap_or("?"),
                approval["approval_risk"].as_str().unwrap_or("-"),
                approval["waiter_count"].as_u64().unwrap_or(0),
                approval["priority"].as_i64().unwrap_or(0)
            );
            if let Some(template) = approval["template_id"].as_str() {
                println!("  template {template}");
            }
            if let Some(expires) = approval["expires_at"].as_str() {
                println!("  expires {expires}");
            }
            if let Some(reason) = approval["approval_reason"].as_str() {
                println!("  reason {reason}");
            }
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
    fn lsp_command_parses_project() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "lsp", "proj-1"]).unwrap();
                match cli.command {
                    Command::Lsp { project } => assert_eq!(project, "proj-1"),
                    other => panic!("expected lsp command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn lsp_status_lines_label_running_installed_and_missing() {
        let value = json!({
            "root": "/tmp/project",
            "running": 1,
            "installed": 2,
            "supported_languages": ["rust", "python"],
            "languages": [
                {
                    "language": "rust",
                    "display": "Rust",
                    "server": "rust-analyzer",
                    "installed": true,
                    "running": true,
                    "capabilities": ["workspace_symbols", "references"],
                    "install_hint": "install rust-analyzer"
                },
                {
                    "language": "python",
                    "display": "Python",
                    "server": "pyright-langserver",
                    "installed": false,
                    "running": false,
                    "capabilities": [],
                    "install_hint": "install pyright"
                }
            ]
        });
        let lines = lsp_status_lines(&value).unwrap();
        assert_eq!(
            lines[0],
            "code intelligence · /tmp/project · 1 running · 2 installed"
        );
        assert!(lines[1].contains("Rust"));
        assert!(lines[1].contains("running"));
        assert!(lines[1].contains("workspace_symbols,references"));
        assert!(lines[2].contains("Python"));
        assert!(lines[2].contains("missing"));
        assert_eq!(lines[3].trim(), "install pyright");
        assert!(lsp_status_lines(&json!({"languages": []})).is_none());
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

    #[test]
    fn cache_audit_parses_a_session() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "cache-audit", "ses-123"]).unwrap();
                match cli.command {
                    Command::CacheAudit { session } => assert_eq!(session, "ses-123"),
                    other => panic!("expected cache-audit command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_plan_accepts_repeated_roles() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "cache-plan",
                    "ses-123",
                    "--role",
                    "leader",
                    "--role",
                    "worker",
                ])
                .unwrap();
                match cli.command {
                    Command::CachePlan { session, role } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(role, vec!["leader".to_string(), "worker".to_string()]);
                    }
                    other => panic!("expected cache-plan command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_warm_accepts_both_spellings() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let direct =
                    Cli::try_parse_from(["vp", "cache-warm", "ses-123", "--role", "worker"])
                        .unwrap();
                match direct.command {
                    Command::CacheWarm { session, role } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(role.as_deref(), Some("worker"));
                    }
                    other => panic!("expected cache-warm command, got {other:?}"),
                }
                let normalized = normalize_cache_warm_args(vec![
                    "vp".into(),
                    "cache".into(),
                    "warm".into(),
                    "ses-123".into(),
                ]);
                let cli = Cli::try_parse_from(normalized).unwrap();
                match cli.command {
                    Command::CacheWarm { session, role } => {
                        assert_eq!(session, "ses-123");
                        assert!(role.is_none());
                    }
                    other => panic!("expected normalized cache-warm command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_policy_set_parses_ttl_and_compaction_flags() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "cache-policy-set",
                    "prj-1",
                    "--ttl",
                    "1h",
                    "--no-automatic",
                    "--warm-on-resume",
                    "--disabled",
                    "--role-ttl",
                    "worker=1h",
                ])
                .unwrap();
                match cli.command {
                    Command::CachePolicySet {
                        project,
                        disabled,
                        ttl,
                        no_automatic,
                        warm_on_resume,
                        no_compact_aware,
                        role_ttl,
                    } => {
                        assert_eq!(project, "prj-1");
                        assert!(disabled);
                        assert_eq!(ttl, "1h");
                        assert!(no_automatic);
                        assert!(warm_on_resume);
                        assert!(!no_compact_aware);
                        assert_eq!(role_ttl, vec!["worker=1h".to_string()]);
                    }
                    other => panic!("expected cache-policy-set command, got {other:?}"),
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

    #[test]
    fn parses_tool_output_threshold_overrides() {
        let parsed =
            parse_tool_thresholds(&["shell=131072".to_string(), "grep = 8192".to_string()])
                .unwrap();
        assert_eq!(parsed[0], ("shell".to_string(), 131_072));
        assert_eq!(parsed[1], ("grep".to_string(), 8_192));
        assert!(parse_tool_thresholds(&["shell".to_string()]).is_err());
        assert!(parse_tool_thresholds(&["=1024".to_string()]).is_err());
        assert!(parse_tool_thresholds(&["shell=not-a-number".to_string()]).is_err());
    }

    #[test]
    fn tool_output_cli_flags_parse_additively() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "tool-output-policy-set",
                    "proj-1",
                    "--spill-threshold",
                    "32768",
                    "--preview-bytes",
                    "8192",
                    "--tool-threshold",
                    "shell=131072",
                    "--max-entries",
                    "500",
                ])
                .unwrap();
                match cli.command {
                    Command::ToolOutputPolicySet {
                        project,
                        spill_threshold,
                        preview_bytes,
                        max_entries,
                        tool_thresholds,
                        ..
                    } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(spill_threshold, Some(32_768));
                        assert_eq!(preview_bytes, Some(8_192));
                        assert_eq!(max_entries, Some(500));
                        assert_eq!(tool_thresholds, vec!["shell=131072".to_string()]);
                    }
                    other => panic!("expected tool-output-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod hybrid_memory_cli_tests {
    use super::*;

    fn parse_cli(args: &[&str]) -> Cli {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn hybrid_memory_commands_and_policy_flags_parse() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "memory-embeddings", "proj-1"]).unwrap();
                match cli.command {
                    Command::MemoryEmbeddings { project } => assert_eq!(project, "proj-1"),
                    other => panic!("expected memory-embeddings, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "memory-embeddings-rebuild", "proj-1"]).unwrap();
                match cli.command {
                    Command::MemoryEmbeddingsRebuild { project } => {
                        assert_eq!(project, "proj-1")
                    }
                    other => panic!("expected memory-embeddings-rebuild, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "memory-embeddings-test",
                    "proj-1",
                    "--text",
                    "probe text",
                ])
                .unwrap();
                match cli.command {
                    Command::MemoryEmbeddingsTest { project, text } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(text.as_deref(), Some("probe text"));
                    }
                    other => panic!("expected memory-embeddings-test, got {other:?}"),
                }

                let cli = Cli::try_parse_from(["vp", "memory-embeddings-index", "proj-1"]).unwrap();
                match cli.command {
                    Command::MemoryEmbeddingsIndex { project } => assert_eq!(project, "proj-1"),
                    other => panic!("expected memory-embeddings-index, got {other:?}"),
                }

                let cli = Cli::try_parse_from(["vp", "memory-embeddings-index-rebuild", "proj-1"])
                    .unwrap();
                match cli.command {
                    Command::MemoryEmbeddingsIndexRebuild { project } => {
                        assert_eq!(project, "proj-1")
                    }
                    other => panic!("expected memory-embeddings-index-rebuild, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "memory-policy-set",
                    "proj-1",
                    "--embedding-index",
                    "ivf",
                    "--ivf-nlist",
                    "128",
                    "--ivf-nprobe",
                    "12",
                    "--ivf-min-vectors",
                    "300",
                    "--ivf-rebuild-vectors",
                    "6000",
                ])
                .unwrap();
                match cli.command {
                    Command::MemoryPolicySet {
                        embedding_index,
                        ivf_nlist,
                        ivf_nprobe,
                        ivf_min_vectors,
                        ivf_rebuild_vectors,
                        ..
                    } => {
                        assert_eq!(embedding_index.as_deref(), Some("ivf"));
                        assert_eq!(ivf_nlist, Some(128));
                        assert_eq!(ivf_nprobe, Some(12));
                        assert_eq!(ivf_min_vectors, Some(300));
                        assert_eq!(ivf_rebuild_vectors, Some(6000));
                    }
                    other => panic!("expected memory-policy-set, got {other:?}"),
                }

                assert!(
                    Cli::try_parse_from([
                        "vp",
                        "memory-policy-set",
                        "proj-1",
                        "--embedding-index",
                        "hnsw",
                    ])
                    .is_err(),
                    "unknown index modes are rejected by the CLI parser"
                );

                let cli = Cli::try_parse_from([
                    "vp",
                    "memory-search",
                    "proj-1",
                    "deployment release",
                    "--lexical-only",
                ])
                .unwrap();
                match cli.command {
                    Command::MemorySearch {
                        project,
                        query,
                        lexical_only,
                        ..
                    } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(query, "deployment release");
                        assert!(lexical_only);
                    }
                    other => panic!("expected memory-search, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "memory-policy-set",
                    "proj-1",
                    "--embedding-enable",
                    "--embedding-endpoint",
                    "http://127.0.0.1:8080/v1",
                    "--embedding-model",
                    "embed-small",
                    "--embedding-dimensions",
                    "512",
                    "--embedding-batch-size",
                    "8",
                    "--embedding-timeout-secs",
                    "12",
                    "--embedding-max-candidates",
                    "32",
                    "--embedding-min-similarity",
                    "0.35",
                    "--embedding-api-key-env",
                    "EMBED_API_KEY",
                ])
                .unwrap();
                match cli.command {
                    Command::MemoryPolicySet {
                        project,
                        embedding_enable,
                        embedding_disable,
                        embedding_endpoint,
                        embedding_model,
                        embedding_dimensions,
                        embedding_batch_size,
                        embedding_timeout_secs,
                        embedding_max_candidates,
                        embedding_min_similarity,
                        embedding_api_key_env,
                        embedding_clear,
                        ..
                    } => {
                        assert_eq!(project, "proj-1");
                        assert!(embedding_enable);
                        assert!(!embedding_disable);
                        assert_eq!(
                            embedding_endpoint.as_deref(),
                            Some("http://127.0.0.1:8080/v1")
                        );
                        assert_eq!(embedding_model.as_deref(), Some("embed-small"));
                        assert_eq!(embedding_dimensions, Some(512));
                        assert_eq!(embedding_batch_size, Some(8));
                        assert_eq!(embedding_timeout_secs, Some(12));
                        assert_eq!(embedding_max_candidates, Some(32));
                        assert_eq!(embedding_min_similarity, Some(0.35));
                        assert_eq!(embedding_api_key_env.as_deref(), Some("EMBED_API_KEY"));
                        assert!(!embedding_clear);
                    }
                    other => panic!("expected memory-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn acp_remote_add_flags_parse() {
        let cli = parse_cli(&[
            "vp",
            "acp-add",
            "remote",
            "--transport",
            "streamable-http",
            "--endpoint",
            "https://agent.example.test/acp",
            "--auth-header-env",
            "ACP_TOKEN",
            "--header",
            "x-project=demo",
            "--header",
            "x-token=${ACP_TOKEN}",
            "--allow-remote",
            "--connect-timeout",
            "15",
            "--timeout",
            "600",
        ]);
        match cli.command {
            Command::AcpAdd {
                name,
                command,
                transport,
                endpoint,
                auth_header_env,
                headers,
                allow_remote,
                connect_timeout,
                timeout,
                ..
            } => {
                assert_eq!(name, "remote");
                assert!(command.is_none());
                assert_eq!(transport.as_str(), "streamable_http");
                assert_eq!(endpoint.as_deref(), Some("https://agent.example.test/acp"));
                assert_eq!(auth_header_env.as_deref(), Some("ACP_TOKEN"));
                assert_eq!(headers.len(), 2);
                assert!(allow_remote);
                assert_eq!(connect_timeout, 15);
                assert_eq!(timeout, 600);
            }
            other => panic!("expected acp-add, got {other:?}"),
        }
    }

    #[test]
    fn acp_stdio_add_keeps_the_optional_command() {
        let cli = parse_cli(&["vp", "acp-add", "local", "npx"]);
        match cli.command {
            Command::AcpAdd {
                transport, command, ..
            } => {
                assert_eq!(transport.as_str(), "stdio");
                assert_eq!(command.as_deref(), Some("npx"));
            }
            other => panic!("expected acp-add, got {other:?}"),
        }
    }

    #[test]
    fn acp_update_flags_parse() {
        let cli = parse_cli(&[
            "vp",
            "acp-update",
            "agent-1",
            "--transport",
            "websocket",
            "--endpoint",
            "wss://agent.example.test/acp",
            "--allow-remote",
            "--clear-headers",
            "--no-auto-approve",
            "--disabled",
        ]);
        match cli.command {
            Command::AcpUpdate {
                agent,
                transport,
                endpoint,
                allow_remote,
                clear_headers,
                no_auto_approve,
                disabled,
                ..
            } => {
                assert_eq!(agent, "agent-1");
                assert_eq!(transport.unwrap().as_str(), "websocket");
                assert_eq!(endpoint.as_deref(), Some("wss://agent.example.test/acp"));
                assert!(allow_remote);
                assert!(clear_headers);
                assert!(no_auto_approve);
                assert!(disabled);
            }
            other => panic!("expected acp-update, got {other:?}"),
        }
    }

    #[test]
    fn acp_transport_names_are_stable() {
        assert_eq!(AcpTransportArg::Stdio.as_str(), "stdio");
        assert_eq!(AcpTransportArg::StreamableHttp.as_str(), "streamable_http");
        assert_eq!(AcpTransportArg::Websocket.as_str(), "websocket");
    }

    #[test]
    fn acp_header_pairs_reject_missing_equals_and_empty_keys() {
        assert_eq!(
            parse_pairs(&["x-project=demo".to_string()], "--header")
                .unwrap()
                .get("x-project")
                .map(String::as_str),
            Some("demo")
        );
        assert!(parse_pairs(&["missing".to_string()], "--header").is_err());
        assert!(parse_pairs(&["=value".to_string()], "--header").is_err());
        assert!(
            parse_pairs(&["x=a=b".to_string()], "--header")
                .unwrap()
                .get("x")
                .is_some_and(|value| value == "a=b")
        );
    }
}

#[cfg(test)]
mod context_checkpoint_cli_tests {
    use super::*;

    #[test]
    fn context_checkpoint_commands_parse() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli =
                    Cli::try_parse_from(["vp", "context-checkpoints", "ses-1", "--limit", "12"])
                        .unwrap();
                match cli.command {
                    Command::ContextCheckpoints { session, limit } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(limit, Some(12));
                    }
                    other => panic!("expected context-checkpoints, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "context-checkpoint",
                    "ses-1",
                    "--label",
                    "before refactor",
                ])
                .unwrap();
                match cli.command {
                    Command::ContextCheckpoint { session, label } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(label.as_deref(), Some("before refactor"));
                    }
                    other => panic!("expected context-checkpoint, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "context-checkpoint-detail", "ses-1", "ctx-1"])
                        .unwrap();
                match cli.command {
                    Command::ContextCheckpointDetail {
                        session,
                        checkpoint,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(checkpoint, "ctx-1");
                    }
                    other => panic!("expected context-checkpoint-detail, got {other:?}"),
                }

                let cli = Cli::try_parse_from(["vp", "context-restore", "ses-1", "ctx-1"]).unwrap();
                match cli.command {
                    Command::ContextRestore {
                        session,
                        checkpoint,
                        force,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(checkpoint, "ctx-1");
                        assert!(!force);
                    }
                    other => panic!("expected context-restore, got {other:?}"),
                }
                let cli =
                    Cli::try_parse_from(["vp", "context-restore", "ses-1", "ctx-1", "--force"])
                        .unwrap();
                match cli.command {
                    Command::ContextRestore { force, .. } => assert!(force),
                    other => panic!("expected forced context-restore, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "context-checkpoint-delete", "ses-1", "ctx-1"])
                        .unwrap();
                match cli.command {
                    Command::ContextCheckpointDelete {
                        session,
                        checkpoint,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(checkpoint, "ctx-1");
                    }
                    other => panic!("expected context-checkpoint-delete, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "context-checkpoint-prune",
                    "ses-1",
                    "--older-than-days",
                    "7",
                    "--keep-latest",
                    "2",
                    "--dry-run",
                ])
                .unwrap();
                match cli.command {
                    Command::ContextCheckpointPrune {
                        session,
                        older_than_days,
                        keep_latest,
                        dry_run,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(older_than_days, Some(7));
                        assert_eq!(keep_latest, Some(2));
                        assert!(dry_run);
                    }
                    other => panic!("expected context-checkpoint-prune, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "context-checkpoint-policy", "prj-1"]).unwrap();
                match cli.command {
                    Command::ContextCheckpointPolicy { project } => {
                        assert_eq!(project, "prj-1")
                    }
                    other => panic!("expected context-checkpoint-policy, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "context-checkpoint-policy-set",
                    "prj-1",
                    "--enabled",
                    "--max-checkpoints",
                    "4",
                    "--max-age-days",
                    "9",
                ])
                .unwrap();
                match cli.command {
                    Command::ContextCheckpointPolicySet {
                        project,
                        enabled,
                        max_checkpoints,
                        max_age_days,
                    } => {
                        assert_eq!(project, "prj-1");
                        assert!(enabled);
                        assert_eq!(max_checkpoints, 4);
                        assert_eq!(max_age_days, 9);
                    }
                    other => panic!("expected context-checkpoint-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod context_checkpoint_human_tests {
    use super::*;

    #[test]
    fn context_checkpoint_list_renders_token_deltas_and_previews() {
        let value = json!({
            "session_id": "ses-1",
            "latest_id": "ctx-2",
            "limit": 50,
            "policy": {"enabled": false, "max_checkpoints": 20, "max_age_days": 30},
            "checkpoints": [
                {
                    "id": "ctx-1",
                    "mode": "summary",
                    "label": "auto summary #1",
                    "summary": "Earlier conversation summary with enough text to become a preview",
                    "plan_revision": 2,
                    "tokens_before": 90000,
                    "tokens_after": 12000,
                    "created_at": "2026-11-11T00:00:00Z"
                },
                {
                    "id": "ctx-2",
                    "mode": "manual",
                    "label": "manual",
                    "plan_revision": 2,
                    "tokens_before": 12000,
                    "tokens_after": 12000,
                    "created_at": "2026-11-11T00:01:00Z"
                }
            ]
        });
        let lines = context_checkpoint_lines(&value).unwrap();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("ses-1"));
        assert!(lines[0].contains("latest ctx-2"));
        assert!(lines[1].contains("sweep off"));
        assert!(lines[2].contains("ctx-1"));
        assert!(lines[2].contains("−78000"));
        assert!(lines[2].contains("Earlier conversation summary"));
        assert!(lines[3].contains("ctx-2"));
        assert!(lines[3].contains("−0"));
    }

    #[test]
    fn context_checkpoint_detail_and_actions_render() {
        let detail = json!({
            "checkpoint": {
                "id": "ctx-1",
                "mode": "summary",
                "label": "before",
                "plan_revision": 1,
                "tokens_before": 5000,
                "tokens_after": 500
            },
            "preflight": {
                "errors": ["plan revision changed"],
                "warnings": ["estimate above threshold"],
                "active_messages": 4,
                "estimated_tokens": 1200,
                "hard_limit_tokens": 32000
            },
            "is_active": false,
            "summary_preview": "before"
        });
        let lines = context_checkpoint_lines(&detail).unwrap();
        assert!(lines[0].contains("ctx-1"));
        assert!(lines[1].contains("active boundary: no"));
        assert!(
            lines
                .iter()
                .any(|line| line == "error: plan revision changed")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "warning: estimate above threshold")
        );

        let restore = json!({
            "session_id": "ses-1",
            "checkpoint_id": "ctx-1",
            "mode": "summary",
            "active_messages": 3,
            "estimated_tokens": 500,
            "compaction_event_seq": 11,
            "restored_event_seq": 12
        });
        let lines = context_checkpoint_lines(&restore).unwrap();
        assert!(lines[0].contains("restored ctx-1"));
        assert!(lines[0].contains("events 11/12"));

        let delete = json!({
            "session_id": "ses-1",
            "checkpoint_id": "ctx-1",
            "removed": true
        });
        assert_eq!(
            context_checkpoint_lines(&delete).unwrap(),
            vec!["ctx-1 deleted".to_string()]
        );

        let prune = json!({
            "session_id": "ses-1",
            "candidates": 2,
            "removed": 1,
            "dry_run": true,
            "checkpoints": [{"id": "ctx-1", "tokens_before": 10}]
        });
        assert_eq!(
            context_checkpoint_lines(&prune).unwrap(),
            vec!["dry run: pruned 1 of 2 candidate context checkpoint(s)".to_string()]
        );

        let policy = json!({
            "project_id": "prj-1",
            "policy": {"enabled": true, "max_checkpoints": 5, "max_age_days": 7}
        });
        assert_eq!(
            context_checkpoint_lines(&policy).unwrap(),
            vec!["prj-1: context checkpoint sweep on · max 5 · age 7d".to_string()]
        );
    }

    #[test]
    fn unrelated_results_are_left_to_the_generic_printer() {
        assert!(context_checkpoint_lines(&json!({"checkpoints": []})).is_none());
        assert!(
            context_checkpoint_lines(&json!({
                "session_id": "ses-1",
                "removed": false
            }))
            .is_none()
        );
        assert!(
            context_checkpoint_lines(&json!({
                "project_id": "prj-1",
                "policy": {"enabled": true, "older_than_secs": 100, "keep_latest": 5}
            }))
            .is_none()
        );
    }
}

#[cfg(test)]
mod context_checkpoint_preview_tests {
    use super::*;

    #[test]
    fn long_summaries_are_truncated_in_the_human_list() {
        let summary = "word ".repeat(40);
        let value = json!({
            "session_id": "ses-1",
            "latest_id": "ctx-1",
            "policy": {"enabled": true, "max_checkpoints": 20, "max_age_days": 30},
            "checkpoints": [{
                "id": "ctx-1",
                "mode": "summary",
                "label": "long",
                "summary": summary,
                "plan_revision": 0,
                "tokens_before": 100,
                "tokens_after": 50,
                "created_at": "2026-11-11T00:00:00Z"
            }]
        });
        let lines = context_checkpoint_lines(&value).unwrap();
        let checkpoint_line = lines.last().unwrap();
        assert!(checkpoint_line.ends_with('…'), "{checkpoint_line}");
        assert!(checkpoint_line.chars().count() < 180);
        assert!(lines[1].contains("sweep on"));
    }
}

#[cfg(test)]
mod tool_activity_cli_tests {
    use super::*;

    fn activity(entries: Value, truncated: bool) -> Value {
        json!({
            "session_id": "ses-1",
            "entries": entries,
            "total": 12,
            "limit": 50,
            "truncated": truncated,
            "summary": {
                "calls": 2,
                "errors": 1,
                "retryable_errors": 1,
                "truncated_results": 1,
                "total_duration_ms": 140,
                "max_duration_ms": 100,
                "average_duration_ms": 70,
                "output_bytes": 2048,
                "by_tool": []
            }
        })
    }

    #[test]
    fn empty_activity_prints_a_clear_summary() {
        let lines = tool_activity_lines(&activity(json!([]), false)).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("2 tool call(s)"));
        assert!(lines[0].contains("1 error(s)"));
        assert!(lines[0].contains("avg 70 ms"));
        assert!(lines[1].contains("No tool activity"));
    }

    #[test]
    fn rows_show_code_hint_and_retryability_without_content() {
        let value = activity(
            json!([
                {
                    "tool_call_id": "call-1",
                    "name": "edit_file",
                    "seq": 7,
                    "is_error": true,
                    "error_code": "PATCH_CONFLICT",
                    "retryable": true,
                    "hint": "Re-read the file and regenerate the patch.",
                    "duration_ms": 100,
                    "output_bytes": 512,
                    "truncated": true,
                    "approval_risk": "workspace_write",
                    "approval_mode": "smart",
                    "approval_wait_ms": 40,
                    "created_at": "2026-11-11T00:00:00Z"
                },
                {
                    "tool_call_id": "call-2",
                    "name": "read_file",
                    "seq": 8,
                    "is_error": false,
                    "duration_ms": 40,
                    "output_bytes": 1536,
                    "truncated": false,
                    "created_at": "2026-11-11T00:00:01Z"
                }
            ]),
            true,
        );
        let lines = tool_activity_lines(&value).unwrap();
        assert!(lines[0].contains("older events omitted"));
        assert!(lines.iter().any(|line| line.contains("PATCH_CONFLICT")));
        assert!(lines.iter().any(|line| line.contains("retryable")));
        assert!(lines.iter().any(|line| line.contains("truncated")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Re-read the file and regenerate"))
        );
        assert!(lines.iter().any(|line| line.contains("read_file")));
        let rendered = lines.join("\n");
        assert!(!rendered.contains("provider output"));
    }

    #[test]
    fn tool_code_catalog_filters_are_stable() {
        let all = filtered_tool_codes(None, None);
        assert!(all.len() >= 100);
        assert!(all.windows(2).all(|pair| {
            pair[0]
                .category
                .cmp(pair[1].category)
                .then_with(|| pair[0].code.as_str().cmp(pair[1].code.as_str()))
                .is_le()
        }));
        let file = filtered_tool_codes(Some(" FILE "), None);
        assert!(!file.is_empty());
        assert!(file.iter().all(|entry| entry.category == "file"));
        assert!(
            file.iter()
                .any(|entry| entry.code.as_str() == "PATCH_CONFLICT")
        );
        let retryable = filtered_tool_codes(None, Some(true));
        assert!(!retryable.is_empty());
        assert!(retryable.iter().all(|entry| entry.retryable));
        assert!(
            retryable
                .iter()
                .any(|entry| entry.code.as_str() == "WAIT_TIMEOUT")
        );
        let terminal = filtered_tool_codes(Some("shell"), Some(false));
        assert!(!terminal.is_empty());
        assert!(terminal.iter().all(|entry| !entry.retryable));
        assert!(filtered_tool_codes(Some("not-a-category"), None).is_empty());
        assert!(
            filtered_tool_codes(Some("file"), Some(true)).len()
                < filtered_tool_codes(Some("file"), None).len()
        );
    }

    #[test]
    fn csv_export_is_stable_and_escaped() {
        let csv = tool_activity_csv(&activity(
            json!([
                {
                    "tool_call_id": "call,one",
                    "name": "shell",
                    "seq": 4,
                    "is_error": true,
                    "error_code": "COMMAND_FAILED",
                    "retryable": false,
                    "hint": "line one\nline \"two\"",
                    "duration_ms": 12,
                    "output_bytes": 34,
                    "truncated": false,
                    "approval_risk": "host_state",
                    "approval_mode": "strict",
                    "approval_wait_ms": 56,
                    "created_at": "2026-11-11T00:00:00Z"
                }
            ]),
            false,
        ));
        assert!(csv.starts_with("seq,created_at,tool_call_id,name,status,error_code,retryable,duration_ms,output_bytes,truncated,approval_risk,approval_mode,approval_wait_ms,hint\n"));
        assert!(csv.contains("\"call,one\""));
        assert!(csv.contains("COMMAND_FAILED"));
        assert!(csv.contains("\"line one\nline \"\"two\"\"\""));
        assert!(csv.contains(",strict,56,"));
        // The embedded newline is quoted, so the raw newline count is three:
        // header, the quoted hint line break, and the record terminator.
        assert_eq!(csv.matches('\n').count(), 3);
    }

    #[test]
    fn csv_export_handles_missing_entries() {
        let csv = tool_activity_csv(&json!({"session_id": "ses-1"}));
        assert_eq!(csv.lines().count(), 1);
        assert!(csv.ends_with('\n'));
        assert!(csv.starts_with("seq,"));
    }

    #[test]
    fn legacy_and_unrelated_payloads_are_not_misformatted() {
        let legacy = json!({
            "session_id": "ses-1",
            "entries": [{"name": "shell"}],
            "total": 1,
            "summary": {"calls": 1}
        });
        assert!(tool_activity_lines(&legacy).is_none());
        assert!(tool_activity_lines(&json!({"events": []})).is_none());
        assert!(
            tool_activity_lines(&json!({
                "session_id": "ses-1",
                "entries": [],
                "summary": {"calls": 0}
            }))
            .is_none()
        );
    }
}

#[cfg(test)]
mod source_embedding_cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn source_search_mode_parses_and_defaults_to_none() {
        let cli = parse(&["vp", "source-search", "proj-1", "leases"]);
        match cli.command {
            Command::SourceSearch { mode, limit, .. } => {
                assert!(mode.is_none(), "absent mode keeps the dynamic default");
                assert_eq!(limit, 8);
            }
            other => panic!("expected source-search, got {other:?}"),
        }
        let cli = parse(&[
            "vp",
            "source-search",
            "proj-1",
            "leases",
            "--mode",
            "semantic",
        ]);
        match cli.command {
            Command::SourceSearch { mode, .. } => assert_eq!(mode.as_deref(), Some("semantic")),
            other => panic!("expected source-search, got {other:?}"),
        }
        assert!(
            std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(|| {
                    Cli::try_parse_from([
                        "vp",
                        "source-search",
                        "proj-1",
                        "leases",
                        "--mode",
                        "vector",
                    ])
                    .is_err()
                })
                .unwrap()
                .join()
                .unwrap()
        );
    }

    #[test]
    fn source_embedding_commands_parse() {
        let cli = parse(&["vp", "source-embedding", "proj-1"]);
        match cli.command {
            Command::SourceEmbedding { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-embedding, got {other:?}"),
        }
        let cli = parse(&["vp", "source-embedding-reindex", "proj-1"]);
        match cli.command {
            Command::SourceEmbeddingReindex { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-embedding-reindex, got {other:?}"),
        }
        let cli = parse(&["vp", "source-embedding-policy", "proj-1"]);
        match cli.command {
            Command::SourceEmbeddingPolicy { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-embedding-policy, got {other:?}"),
        }
    }

    #[test]
    fn source_embedding_policy_set_parses_every_flag() {
        let cli = parse(&[
            "vp",
            "source-embedding-policy-set",
            "proj-1",
            "--enable",
            "--endpoint-env",
            "SOURCE_EMBED_ENDPOINT",
            "--model",
            "embed-small",
            "--dimensions",
            "512",
            "--batch-size",
            "4",
            "--max-chunks-per-source",
            "120",
            "--retention-days",
            "14",
            "--timeout-secs",
            "20",
            "--api-key-env",
            "SOURCE_EMBED_KEY",
        ]);
        match cli.command {
            Command::SourceEmbeddingPolicySet {
                project,
                enable,
                disable,
                endpoint_env,
                model,
                dimensions,
                batch_size,
                max_chunks_per_source,
                retention_days,
                timeout_secs,
                api_key_env,
                clear,
            } => {
                assert_eq!(project, "proj-1");
                assert!(enable);
                assert!(!disable);
                assert_eq!(endpoint_env.as_deref(), Some("SOURCE_EMBED_ENDPOINT"));
                assert_eq!(model.as_deref(), Some("embed-small"));
                assert_eq!(dimensions, Some(512));
                assert_eq!(batch_size, Some(4));
                assert_eq!(max_chunks_per_source, Some(120));
                assert_eq!(retention_days, Some(14));
                assert_eq!(timeout_secs, Some(20));
                assert_eq!(api_key_env.as_deref(), Some("SOURCE_EMBED_KEY"));
                assert!(!clear);
            }
            other => panic!("expected source-embedding-policy-set, got {other:?}"),
        }
    }

    #[test]
    fn source_embedding_status_printer_includes_fallback_and_counts() {
        let value = json!({
            "project_id": "prj-1",
            "enabled": true,
            "configured": true,
            "endpoint_env": "SOURCE_EMBED_ENDPOINT",
            "endpoint_env_present": false,
            "model": "embed-small",
            "dimensions": 512,
            "indexable_chunks": 20,
            "indexed_chunks": 16,
            "stale_chunks": 2,
            "invalid_chunks": 0,
            "pending_jobs": 1,
            "running_jobs": 0,
            "failed_jobs": 1,
            "dead_jobs": 0,
            "done_jobs": 3,
            "coverage": 0.8,
            "max_chunks_per_source": 200,
            "retention_days": 30,
            "semantic_unavailable": "no_indexed_vectors",
            "usage": {
                "calls": 4,
                "input_tokens": 120,
                "estimated_tokens": 20,
                "bytes": 480,
                "provider_reported_calls": 3
            },
            "recent_jobs": [{
                "source_id": "src-1",
                "source_title": "Runtime",
                "status": "failed",
                "attempts": 2,
                "generation": 2,
                "chunks_total": 10,
                "chunks_done": 8,
                "updated_at": "2026-11-11T00:00:00Z"
            }]
        });
        // The printer is a human surface; status is still strict JSON when
        // requested. This assertion keeps the recognizable fields present.
        assert_eq!(value["indexed_chunks"], 16);
        assert_eq!(value["semantic_unavailable"], "no_indexed_vectors");
        assert_eq!(value["recent_jobs"][0]["chunks_done"], 8);
        assert_eq!(value["endpoint_env_present"], false);
    }
}

#[cfg(test)]
mod process_command_tests {
    use super::*;

    #[test]
    fn clap_parses_process_commands() {
        // The CLI command enum is very large; parse on a thread with a
        // generous stack so this test is not sensitive to the default 2 MiB
        // libtest thread size.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "processes", "ses-1"]).unwrap();
                match cli.command {
                    Command::Processes {
                        session,
                        include_finished,
                        limit,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert!(include_finished.is_none());
                        assert_eq!(limit, 50);
                    }
                    other => panic!("expected processes, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "process-logs",
                    "ses-1",
                    "psup_1",
                    "--offset",
                    "1024",
                    "--max-bytes",
                    "2048",
                    "--max-lines",
                    "40",
                    "--from-end",
                    "--follow",
                ])
                .unwrap();
                match cli.command {
                    Command::ProcessLogs {
                        session,
                        process,
                        offset,
                        max_bytes,
                        max_lines,
                        from_end,
                        follow,
                        timeout_secs,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(process, "psup_1");
                        assert_eq!(offset, 1024);
                        assert_eq!(max_bytes, 2048);
                        assert_eq!(max_lines, Some(40));
                        assert!(from_end);
                        assert!(follow);
                        assert_eq!(timeout_secs, 300);
                    }
                    other => panic!("expected process-logs, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "process-stop",
                    "ses-1",
                    "psup_1",
                    "--signal",
                    "KILL",
                    "--grace-secs",
                    "2",
                ])
                .unwrap();
                match cli.command {
                    Command::ProcessStop {
                        session,
                        process,
                        signal,
                        grace_secs,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(process, "psup_1");
                        assert_eq!(signal, "KILL");
                        assert_eq!(grace_secs, 2);
                    }
                    other => panic!("expected process-stop, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn clap_parses_process_policy_commands() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "process-policy", "prj-1"]).unwrap();
                match cli.command {
                    Command::ProcessPolicy { project } => assert_eq!(project, "prj-1"),
                    other => panic!("expected process-policy, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "process-policy-set",
                    "prj-1",
                    "--idle-sweep-enabled",
                    "false",
                    "--max-processes",
                    "4",
                    "--idle-ttl-secs",
                    "900",
                    "--max-output-bytes",
                    "65536",
                    "--max-output-lines",
                    "2000",
                ])
                .unwrap();
                match cli.command {
                    Command::ProcessPolicySet {
                        project,
                        idle_sweep_enabled,
                        max_processes,
                        idle_ttl_secs,
                        max_output_bytes,
                        max_output_lines,
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(idle_sweep_enabled, Some(false));
                        assert_eq!(max_processes, Some(4));
                        assert_eq!(idle_ttl_secs, Some(900));
                        assert_eq!(max_output_bytes, Some(65536));
                        assert_eq!(max_output_lines, Some(2000));
                    }
                    other => panic!("expected process-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn process_policy_human_rendering_is_bounded() {
        let value = json!({
            "project_id": "prj-1",
            "policy": {
                "idle_sweep_enabled": true,
                "max_processes": 8,
                "idle_ttl_secs": 3600,
                "max_output_bytes": 1048576,
                "max_output_lines": 20000
            }
        });
        let lines = process_console_lines(&value).unwrap();
        assert!(lines[0].contains("prj-1"));
        assert!(lines[1].contains("idle sweep on"));
        assert!(lines[2].contains("1048576 bytes"));
    }

    #[test]
    fn human_process_list_and_logs_are_bounded() {
        let list = json!({
            "session_id": "ses-1",
            "processes": [{
                "id": "psup_1",
                "state": "running",
                "name": "server",
                "pid": 42,
                "started_at": "2026-11-11T00:00:00Z",
                "output_bytes": 128,
                "exit_code": null,
                "detail": null
            }],
            "active": 1,
            "limit": 8
        });
        let lines = process_console_lines(&list).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("psup_1"));
        assert!(lines[0].contains("running"));
        assert!(lines[0].contains("pid 42"));

        let logs = json!({
            "session_id": "ses-1",
            "process_id": "psup_1",
            "state": "running",
            "content": "hello\n",
            "offset": 0,
            "next_offset": 6,
            "total_bytes": 6,
            "eof": true,
            "truncated": false,
            "rotated": false
        });
        let lines = process_console_lines(&logs).unwrap();
        assert!(lines[0].contains("psup_1"));
        assert!(lines.iter().any(|line| line == "hello\n"));

        let stop = json!({
            "process": {
                "id": "psup_1",
                "state": "stopped",
                "exit_code": 0,
                "signal": 15
            },
            "forced": false,
            "waited_ms": 12
        });
        let lines = process_console_lines(&stop).unwrap();
        assert!(lines[0].contains("stopped"));
        assert!(lines[0].contains("signal 15"));
    }
}

#[cfg(test)]
mod scaling_cli_tests {
    use super::*;

    #[test]
    fn preview_lines_include_action_targets_reasons_and_hash() {
        let lines = scaling_lines(&json!({
            "session_id": "ses-1",
            "project_id": "proj-1",
            "policy": {"enabled": true},
            "decision": {
                "session_id": "ses-1",
                "action": "expand_workers",
                "current_workers": 2,
                "target_workers": 4,
                "current_labour": 2,
                "target_labour": 2,
                "ready_tasks": 5,
                "running_tasks": 2,
                "active_workers": 2,
                "active_labour": 1,
                "plan_revision": 3,
                "applied": false,
                "inputs_hash": "0123456789abcdef",
                "reasons": ["ready_tasks:5", "expand_workers:2->4"],
                "manager_evidence": "two departments would help"
            }
        }))
        .expect("preview lines");
        let text = lines.join("\n");
        assert!(text.contains("action=expand_workers"));
        assert!(text.contains("Workers 2→4"));
        assert!(text.contains("ready 5"));
        assert!(text.contains("inputs 0123456789ab"));
        assert!(text.contains("manager evidence: two departments would help"));
    }

    #[test]
    fn status_lines_report_policy_targets_and_latest_reasons() {
        let lines = scaling_lines(&json!({
            "global_worker_cap": 12,
            "global_labour_cap": 24,
            "global_worker_available": 9,
            "global_labour_available": 20,
            "projects": [{
                "project_id": "proj-1",
                "project_name": "demo",
                "policy": {
                    "enabled": true,
                    "min_workers": 1,
                    "max_workers": 4,
                    "min_labour": 1,
                    "max_labour": 8,
                    "expand_ready_threshold": 2,
                    "cooldown_secs": 120,
                    "max_step": 2,
                    "budget_headroom_ratio": 0.25
                },
                "sessions": [{
                    "session_id": "ses-1",
                    "goal": "ship",
                    "status": "idle",
                    "plan_revision": 2,
                    "plan_tasks": 6,
                    "ready_tasks": 3,
                    "running_tasks": 0,
                    "completed_tasks": 2,
                    "current_workers": 2,
                    "current_labour": 4,
                    "active_workers": 0,
                    "active_labour": 0,
                    "latest": {
                        "action": "scale_down_labour",
                        "reasons": ["scale_down_labour:4->2", "idle 900s"]
                    }
                }]
            }]
        }))
        .expect("status lines");
        let text = lines.join("\n");
        assert!(text.contains("global Workers 9/12"));
        assert!(text.contains("demo  scaling=on"));
        assert!(text.contains("budget ≥25%"));
        assert!(text.contains("slots 2W/4L"));
        assert!(text.contains("latest scale_down_labour"));
        assert!(text.contains("scale_down_labour:4->2"));
    }

    #[test]
    fn policy_lines_show_disabled_budget_check_and_bounds() {
        let lines = scaling_lines(&json!({
            "project_id": "proj-1",
            "policy": {
                "enabled": false,
                "min_workers": 1,
                "max_workers": 3,
                "min_labour": 1,
                "max_labour": 6,
                "expand_ready_threshold": 2,
                "expand_critical_delay_secs": 60,
                "scale_down_idle_secs": 300,
                "cooldown_secs": 90,
                "max_step": 1,
                "budget_headroom_ratio": null
            }
        }))
        .expect("policy lines");
        let text = lines.join("\n");
        assert!(text.contains("enabled=false"));
        assert!(text.contains("Workers 1-3"));
        assert!(text.contains("Labour 1-6"));
        assert!(text.contains("budget headroom: check disabled"));
    }

    #[test]
    fn unrelated_payload_is_not_misformatted() {
        assert!(scaling_lines(&json!({"session_id": "ses-1"})).is_none());
        assert!(scaling_lines(&json!({"projects": []})).is_none());
        assert!(scaling_lines(&json!({"policy": {"max_workers": 4}})).is_none());
    }
}

#[cfg(test)]
mod governor_cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn governor_commands_and_flags_parse() {
        let cli = parse(&["vp", "governor", "session-1", "--limit", "7"]);
        match cli.command {
            Command::Governor { session, limit } => {
                assert_eq!(session, "session-1");
                assert_eq!(limit, 7);
            }
            other => panic!("expected governor, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "critical-path",
            "session-1",
            "--remaining-tokens",
            "1234",
        ]);
        match cli.command {
            Command::CriticalPath {
                session,
                remaining_tokens,
                mode,
            } => {
                assert_eq!(session, "session-1");
                assert_eq!(remaining_tokens, Some(1_234));
                assert_eq!(mode, None);
            }
            other => panic!("expected critical-path, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "governor-preview",
            "session-1",
            "--remaining-tokens",
            "99",
            "--limit",
            "3",
        ]);
        match cli.command {
            Command::GovernorPreview {
                session,
                remaining_tokens,
                limit,
                mode,
            } => {
                assert_eq!(session, "session-1");
                assert_eq!(remaining_tokens, Some(99));
                assert_eq!(limit, 3);
                assert_eq!(mode, None);
            }
            other => panic!("expected governor-preview, got {other:?}"),
        }

        let cli = parse(&["vp", "governor-policy", "project-1"]);
        match cli.command {
            Command::GovernorPolicy { project } => assert_eq!(project, "project-1"),
            other => panic!("expected governor-policy, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "governor-policy-set",
            "project-1",
            "--enabled",
            "--reserve-ratio",
            "0.4",
            "--throttle-at-ratio",
            "0.2",
            "--critical-boost",
            "10",
            "--min-task-budget",
            "2000",
        ]);
        match cli.command {
            Command::GovernorPolicySet {
                project,
                enabled,
                reserve_ratio,
                throttle_at_ratio,
                critical_boost,
                min_task_budget,
                mode,
                max_paths,
                department_min_ratio,
                observed_weight_ratio,
                starvation_after_secs,
            } => {
                assert_eq!(project, "project-1");
                assert_eq!(enabled, Some(true));
                assert_eq!(reserve_ratio, Some(0.4));
                assert_eq!(throttle_at_ratio, Some(0.2));
                assert_eq!(critical_boost, Some(10));
                assert_eq!(min_task_budget, Some(2_000));
                assert_eq!(mode, None);
                assert_eq!(max_paths, None);
                assert_eq!(department_min_ratio, None);
                assert_eq!(observed_weight_ratio, None);
                assert_eq!(starvation_after_secs, None);
            }
            other => panic!("expected governor-policy-set, got {other:?}"),
        }
    }

    #[test]
    fn governor_human_renderer_explains_the_reserve() {
        let value = json!({
            "session_id": "session-1",
            "session_status": "idle",
            "plan_revision": 2,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "reserve_ratio": 0.25},
            "critical_path": {"task_ids": ["a", "b"], "weight_tokens": 5000},
            "reserve": {
                "state": "active",
                "protected_tokens": 7500,
                "remaining_tokens": 10000,
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.35,
                "throttled": false
            },
            "admitted": ["a"],
            "deferred": [{
                "task_id": "side",
                "reason": "reserve",
                "code": "BUDGET_RESERVED"
            }],
            "last_decisions": [],
            "generated_at": "2026-11-11T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("session session-1"));
        assert!(lines.contains("a -> b"));
        assert!(lines.contains("protected 7500 of remaining 10000"));
        assert!(lines.contains("Defer: side (reserve, BUDGET_RESERVED)"));
    }

    #[test]
    fn governor_renderer_surfaces_typed_errors_and_deferrals() {
        let value = json!({
            "session_id": "session-1",
            "session_status": "idle",
            "plan_revision": 1,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true},
            "critical_path": {"task_ids": [], "weight_tokens": 0},
            "reserve": {
                "state": "complete",
                "protected_tokens": 0,
                "remaining_tokens": 5000,
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.35,
                "throttled": false
            },
            "admitted": [],
            "deferred": [],
            "last_decisions": [],
            "error": {
                "code": "BUDGET_PATH_CYCLE",
                "message": "plan dependency cycle",
                "hint": "remove the cycle"
            },
            "generated_at": "2026-11-11T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("Path: complete"));
        assert!(lines.contains("BUDGET_PATH_CYCLE"));
        assert!(lines.contains("hint: remove the cycle"));
    }

    #[test]
    fn governor_policy_renderer_is_compact() {
        let value = json!({
            "project_id": "p1",
            "policy": {
                "enabled": true,
                "reserve_ratio": 0.5,
                "throttle_at_ratio": 0.2,
                "critical_boost": 10,
                "min_task_budget": 2000
            }
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("enabled"));
        assert!(lines.contains("reserve 50%"));
        assert!(lines.contains("throttle 20%"));
    }
}

#[cfg(test)]
mod remote_integration_cli_tests {
    use super::*;

    #[test]
    fn remote_integration_commands_parse_explicit_confirmation_and_bounds() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(parse_remote_integration_commands)
            .unwrap()
            .join()
            .unwrap();
    }

    fn parse_remote_integration_commands() {
        let cli = Cli::try_parse_from([
            "vp",
            "remote-integrate",
            "prj-1",
            "ses-1",
            "feature/remote",
            "--host",
            "build.example.com",
            "--expected-commit",
            "abcdef1234567890",
            "--task",
            "T1",
            "--objective",
            "integrate it",
            "--confirm",
            "--keep-import-refs",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteIntegrate {
                project,
                session,
                branch,
                host,
                expected_commit,
                base_commit,
                task,
                objective,
                confirm,
                keep_import_refs,
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(session, "ses-1");
                assert_eq!(branch, "feature/remote");
                assert_eq!(host.as_deref(), Some("build.example.com"));
                assert_eq!(expected_commit.as_deref(), Some("abcdef1234567890"));
                assert!(base_commit.is_none());
                assert_eq!(task.as_deref(), Some("T1"));
                assert_eq!(objective.as_deref(), Some("integrate it"));
                assert!(confirm);
                assert!(keep_import_refs);
            }
            other => panic!("expected remote-integrate, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "remote-integration-policy-set",
            "prj-1",
            "--enable",
            "--max-bundle-bytes",
            "1048576",
            "--no-require-verifier",
            "--allow-host",
            "build.example.com",
            "--no-keep-import-refs",
            "--confirm",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteIntegrationPolicySet {
                project,
                enable,
                max_bundle_bytes,
                no_require_verifier,
                allowed_hosts,
                no_keep_import_refs,
                confirm,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert!(enable);
                assert_eq!(max_bundle_bytes, Some(1_048_576));
                assert!(no_require_verifier);
                assert_eq!(allowed_hosts, vec!["build.example.com".to_string()]);
                assert!(no_keep_import_refs);
                assert!(confirm);
            }
            other => panic!("expected remote-integration-policy-set, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "remote-integration", "prj-1", "--session", "ses-1"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Command::RemoteIntegration {
                session: Some(ref session),
                ..
            } if session == "ses-1"
        ));

        let cli = Cli::try_parse_from([
            "vp",
            "remote-integration-cleanup",
            "prj-1",
            "--entry",
            "e1",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteIntegrationCleanup {
                project,
                session,
                entry,
                force,
            } => {
                assert_eq!(project, "prj-1");
                assert!(session.is_none());
                assert_eq!(entry.as_deref(), Some("e1"));
                assert!(force);
            }
            other => panic!("expected remote-integration-cleanup, got {other:?}"),
        }
    }

    #[test]
    fn remote_integration_human_output_reports_status_and_caveat() {
        let value = serde_json::json!({
            "entry": {
                "id": "e1",
                "status": "merged",
                "commit": "abcdef1234567890"
            },
            "bundle": {
                "host": "build.example.com",
                "branch": "feature/x",
                "sha256": "a".repeat(64),
                "bytes": 1024
            },
            "verification": {
                "required": true,
                "performed": true,
                "approved": true,
                "summary": "looks good"
            },
            "imported_ref": null,
            "refs_pruned": true,
            "environment_caveat": "local only"
        });
        let lines = remote_integration_lines(&value).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("merged"));
        assert!(text.contains("build.example.com"));
        assert!(text.contains("verifier required=true performed=true approved=true"));
        assert!(text.contains("local only"));

        let status = serde_json::json!({
            "project_id": "prj-1",
            "policy": {
                "enabled": true,
                "max_bundle_bytes": 1024,
                "require_verifier": true,
                "allowed_hosts": ["build.example.com"],
                "keep_import_refs": false
            },
            "entries": [{
                "entry_id": "e1",
                "status": "conflict",
                "commit": "abcdef1234567890",
                "remote_host": "build.example.com",
                "remote_branch": "feature/x",
                "attempt": 2,
                "imported_ref": "refs/verlinpark/remote/s/i",
                "detail": "conflict"
            }],
            "total": 1,
            "pending": 0,
            "merged": 0,
            "conflicts": 1,
            "failed": 0,
            "retained_refs": 1,
            "environment_caveat": "local only"
        });
        let lines = remote_integration_lines(&status).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("enabled=true"));
        assert!(text.contains("conflict"));
        assert!(text.contains("refs retained 1"));

        let cleanup = serde_json::json!({
            "project_id": "prj-1",
            "removed_refs": ["refs/verlinpark/remote/s/i"],
            "removed_worktrees": [],
            "skipped": ["e2: retryable entry (conflict) requires force"],
            "entries_examined": 2
        });
        let lines = remote_integration_lines(&cleanup).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("removed 1 ref(s)"));
        assert!(text.contains("requires force"));
    }

    #[test]
    fn backup_signing_flags_parse() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "backup",
                    "--repository",
                    "/tmp/repo",
                    "--signing-key",
                    "primary",
                    "--keyring-dir",
                    "/tmp/keys",
                ])
                .unwrap();
                match cli.command {
                    Command::Backup {
                        signing_key,
                        keyring_dir,
                        sign,
                        ..
                    } => {
                        assert_eq!(signing_key.as_deref(), Some("primary"));
                        assert_eq!(keyring_dir.as_deref(), Some(Path::new("/tmp/keys")));
                        assert!(!sign);
                    }
                    other => panic!("expected backup, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "restore",
                    "--repository",
                    "/tmp/repo",
                    "--snapshot",
                    "0190f000-0000-7000-8000-000000000000",
                    "--allow-rollback",
                    "--keyring-dir",
                    "/tmp/keys",
                ])
                .unwrap();
                match cli.command {
                    Command::Restore {
                        allow_rollback,
                        keyring_dir,
                        ..
                    } => {
                        assert!(allow_rollback);
                        assert_eq!(keyring_dir.as_deref(), Some(Path::new("/tmp/keys")));
                    }
                    other => panic!("expected restore, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "backup-verify",
                    "--repository",
                    "/tmp/repo",
                    "--presence-only",
                    "--allow-rollback",
                ])
                .unwrap();
                match cli.command {
                    Command::BackupVerify {
                        presence_only,
                        allow_rollback,
                        ..
                    } => {
                        assert!(presence_only);
                        assert!(allow_rollback);
                    }
                    other => panic!("expected backup-verify, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "backup-key-generate",
                    "primary",
                    "--keyring-dir",
                    "/tmp/keys",
                    "--inactive",
                ])
                .unwrap();
                match cli.command {
                    Command::BackupKeyGenerate {
                        key_id,
                        inactive,
                        from_env,
                        ..
                    } => {
                        assert_eq!(key_id, "primary");
                        assert!(inactive);
                        assert!(from_env.is_none());
                    }
                    other => panic!("expected backup-key-generate, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn signing_options_are_explicit_and_keyring_backed() {
        // No signing flags keeps the v2 unsigned path.
        assert!(
            resolve_signing_options(None, None, false)
                .unwrap()
                .is_none()
        );

        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("missing");
        let error = resolve_signing_options(Some(&missing), None, false).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEYRING_NOT_FOUND"));

        let keys = dir.path().join("keys");
        Keyring::generate_key(
            &keys,
            "primary",
            true,
            SecretSource::File {
                name: "primary.key".to_string(),
            },
        )
        .unwrap();
        let signing = resolve_signing_options(Some(&keys), None, true)
            .unwrap()
            .unwrap();
        assert_eq!(signing.keyring.active_key_id(), Some("primary"));
        assert!(signing.key_id.is_none());

        let signing = resolve_signing_options(Some(&keys), Some("primary"), false)
            .unwrap()
            .unwrap();
        assert_eq!(signing.key_id.as_deref(), Some("primary"));

        let error = resolve_signing_options(Some(&keys), Some("missing"), false).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_NOT_FOUND"));

        let error = resolve_signing_options(Some(&keys), Some("primary"), true).unwrap_err();
        assert!(format!("{error:#}").contains("only one of"));
    }

    #[test]
    fn anchor_store_resolution_honors_an_explicit_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let default = anchor_store(dir.path(), None);
        assert_eq!(default.path(), dir.path().join("backup-anchor.json"));
        let explicit = dir.path().join("custom-anchor.json");
        let store = anchor_store(dir.path(), Some(&explicit));
        assert_eq!(store.path(), explicit);
    }
}

#[cfg(test)]
mod test_cache_cli_tests {
    use super::*;

    #[test]
    fn parses_test_cache_commands_additively() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "test-cache-policy-set",
                    "prj-1",
                    "--max-entries",
                    "7",
                    "--retention-days",
                    "3",
                    "--max-output-bytes",
                    "65536",
                    "--reuse-lint",
                    "false",
                    "--include-integration",
                    "--failed-retention-secs",
                    "120",
                ])
                .unwrap();
                match cli.command {
                    Command::TestCachePolicySet {
                        project,
                        max_entries,
                        retention_days,
                        max_output_bytes,
                        reuse_lint,
                        include_integration,
                        failed_retention_secs,
                        ..
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(max_entries, Some(7));
                        assert_eq!(retention_days, Some(3));
                        assert_eq!(max_output_bytes, Some(65536));
                        assert_eq!(reuse_lint, Some(false));
                        assert_eq!(include_integration, Some(true));
                        assert_eq!(failed_retention_secs, Some(120));
                    }
                    other => panic!("expected test-cache-policy-set, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "test-cache-clear",
                    "prj-1",
                    "--kind",
                    "lint",
                    "--yes",
                ])
                .unwrap();
                match cli.command {
                    Command::TestCacheClear {
                        project, kind, yes, ..
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(kind.as_deref(), Some("lint"));
                        assert!(yes);
                    }
                    other => panic!("expected test-cache-clear, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "test-impact", "ses-1", "--no-history"]).unwrap();
                match cli.command {
                    Command::TestImpact {
                        session,
                        no_history,
                        ..
                    } => {
                        assert_eq!(session, "ses-1");
                        assert!(no_history);
                    }
                    other => panic!("expected test-impact, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn renders_stats_policy_entries_and_prune() {
        let stats = json!({
            "project_id": "prj-1",
            "entries": 2,
            "passed": 1,
            "failed": 1,
            "error": 0,
            "expired": 0,
            "total_hits": 4,
            "output_bytes": 2048,
            "by_kind": {"test": 2},
            "policy": {
                "enabled": true,
                "max_entries": 512,
                "retention_days": 14,
                "max_output_bytes": 262144,
                "reuse_lint": true,
                "include_integration": false,
                "failed_retention_secs": 900
            },
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = test_cache_lines(&stats).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("test cache policy"));
        assert!(text.contains("entries=2"));
        assert!(text.contains("integrator=false"));

        let entries = json!({
            "project_id": "prj-1",
            "entries": [
                {
                    "id": "tr-1",
                    "kind": "test",
                    "status": "passed",
                    "exit_code": 0,
                    "duration_ms": 42,
                    "hit_count": 2,
                    "command": "cargo test"
                }
            ],
            "total": 1,
            "truncated": false
        });
        let text = test_cache_lines(&entries).unwrap().join("\n");
        assert!(text.contains("test cache entries"));
        assert!(text.contains("cargo test"));

        let prune = json!({
            "project_id": "prj-1",
            "pruned": 1,
            "remaining": 0,
            "dry_run": true,
            "affected_sessions": 1,
            "removed_ids": ["tr-1"],
            "reason": "retention"
        });
        let text = test_cache_lines(&prune).unwrap().join("\n");
        assert!(text.contains("dry_run=true"));
        assert!(text.contains("removed tr-1"));

        let impact = json!({
            "session_id": "ses-1",
            "project_id": "prj-1",
            "test_command": "cargo test",
            "map": {
                "generation": "impact-v1:abc",
                "lsp_available": false,
                "lexical_only": true,
                "truncated": false,
                "changed_paths": ["src/lib.rs"],
                "targets": [{
                    "path": "tests/smoke.rs",
                    "source": "lexical",
                    "confidence": 0.9,
                    "reason": "reverse import"
                }],
                "notes": ["heuristic"]
            }
        });
        let text = test_cache_lines(&impact).unwrap().join("\n");
        assert!(text.contains("test impact"));
        assert!(text.contains("tests/smoke.rs"));
        assert!(text.contains("LEXICAL") || text.contains("lexical"));
    }
}

#[cfg(test)]
mod approval_optimizer_cli_tests {
    use super::*;
    use clap::Parser;

    /// The full CLI enum is large; parse on a generously sized thread so the
    /// test reflects parsing behavior rather than the test harness's 2 MiB
    /// stack.
    fn parse(args: &[&str]) -> Cli {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn parses_explicit_batch_decisions() {
        let cli = parse(&[
            "vp",
            "approval-batch",
            "--approve",
            "a1",
            "--approve",
            "a2",
            "--deny",
            "a3",
        ]);
        match cli.command {
            Command::ApprovalBatch { approve, deny } => {
                assert_eq!(approve, vec!["a1", "a2"]);
                assert_eq!(deny, vec!["a3"]);
            }
            _ => panic!("expected approval batch"),
        }
    }

    #[test]
    fn parses_template_create_and_update_flags() {
        let cli = parse(&[
            "vp",
            "approval-template-set",
            "--project",
            "p1",
            "--name",
            "src writes",
            "--tool-pattern",
            "write_*",
            "--risk",
            "workspace_write",
            "--path-glob",
            "src/**",
            "--action",
            "allow",
            "--max-uses",
            "5",
        ]);
        match cli.command {
            Command::ApprovalTemplateSet {
                project,
                name,
                tool_pattern,
                risk,
                path_glob,
                action,
                max_uses,
                ..
            } => {
                assert_eq!(project.as_deref(), Some("p1"));
                assert_eq!(name.as_deref(), Some("src writes"));
                assert_eq!(tool_pattern.as_deref(), Some("write_*"));
                assert_eq!(risk, vec!["workspace_write"]);
                assert_eq!(path_glob, vec!["src/**"]);
                assert_eq!(action.as_deref(), Some("allow"));
                assert_eq!(max_uses, Some(5));
            }
            _ => panic!("expected approval template set"),
        }
    }

    #[test]
    fn parses_attention_policy_updates() {
        let cli = parse(&[
            "vp",
            "approval-policy-set",
            "--project",
            "p1",
            "--max-pending",
            "12",
            "--coalesce-secs",
            "30",
            "--stale-secs",
            "600",
            "--priority-boost-risk",
            "destructive",
            "--notify-coalesce-secs",
            "15",
        ]);
        match cli.command {
            Command::ApprovalPolicySet {
                project,
                max_pending,
                coalesce_secs,
                stale_secs,
                priority_boost_risk,
                notify_coalesce_secs,
                ..
            } => {
                assert_eq!(project, "p1");
                assert_eq!(max_pending, Some(12));
                assert_eq!(coalesce_secs, Some(30));
                assert_eq!(stale_secs, Some(600));
                assert_eq!(priority_boost_risk.as_deref(), Some("destructive"));
                assert_eq!(notify_coalesce_secs, Some(15));
            }
            _ => panic!("expected approval policy set"),
        }
    }

    #[test]
    fn approval_queue_human_output_includes_attention_fields() {
        let value = serde_json::json!({
            "approvals": [{
                "id": "a1",
                "tool": "write_file",
                "status": "pending",
                "approval_risk": "workspace_write",
                "waiter_count": 3,
                "priority": 42,
                "input": {"path": "src/lib.rs"}
            }],
            "pending_count": 1
        });
        print_human(&value);
    }
}

#[cfg(test)]
mod governor_fairness_cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn fairness_policy_flags_parse() {
        let cli = parse(&[
            "vp",
            "governor-policy-set",
            "project-1",
            "--mode",
            "multi",
            "--max-paths",
            "6",
            "--department-min-ratio",
            "0.2",
            "--observed-weight-ratio",
            "0.5",
            "--starvation-after-secs",
            "600",
        ]);
        match cli.command {
            Command::GovernorPolicySet {
                mode,
                max_paths,
                department_min_ratio,
                observed_weight_ratio,
                starvation_after_secs,
                ..
            } => {
                assert_eq!(mode.as_deref(), Some("multi"));
                assert_eq!(max_paths, Some(6));
                assert_eq!(department_min_ratio, Some(0.2));
                assert_eq!(observed_weight_ratio, Some(0.5));
                assert_eq!(starvation_after_secs, Some(600));
            }
            other => panic!("expected governor-policy-set, got {other:?}"),
        }
    }

    #[test]
    fn preview_mode_flags_parse_and_validate() {
        let cli = parse(&[
            "vp",
            "governor-preview",
            "session-1",
            "--mode",
            "multi",
            "--remaining-tokens",
            "500",
        ]);
        match cli.command {
            Command::GovernorPreview {
                session,
                remaining_tokens,
                mode,
                ..
            } => {
                assert_eq!(session, "session-1");
                assert_eq!(remaining_tokens, Some(500));
                assert_eq!(mode.as_deref(), Some("multi"));
            }
            other => panic!("expected governor-preview, got {other:?}"),
        }
        let cli = parse(&["vp", "critical-path", "session-1", "--mode", "single"]);
        match cli.command {
            Command::CriticalPath { mode, .. } => assert_eq!(mode.as_deref(), Some("single")),
            other => panic!("expected critical-path, got {other:?}"),
        }
        assert_eq!(parse_governor_mode(None).unwrap(), None);
        assert_eq!(
            parse_governor_mode(Some("multi-path")).unwrap(),
            Some("multi")
        );
        assert_eq!(parse_governor_mode(Some("SINGLE")).unwrap(), Some("single"));
        assert!(parse_governor_mode(Some("weighted")).is_err());
    }

    #[test]
    fn policy_renderer_prints_fairness_fields_for_multi_mode() {
        let value = json!({
            "project_id": "p1",
            "policy": {
                "enabled": true,
                "mode": "multi",
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.1,
                "critical_boost": 10,
                "min_task_budget": 100,
                "max_paths": 6,
                "department_min_ratio": 0.2,
                "observed_weight_ratio": 0.5,
                "starvation_after_secs": 600
            }
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("multi"));
        assert!(lines.contains("max 6 paths"));
        assert!(lines.contains("department floor 20%"));
        assert!(lines.contains("observed 50%"));
        assert!(lines.contains("starvation 600 s"));
    }

    #[test]
    fn session_renderer_prints_the_path_table_and_deferred_order() {
        let value = json!({
            "session_id": "s1",
            "session_status": "idle",
            "plan_revision": 3,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "mode": "multi", "observed_weight_ratio": 0.5},
            "critical_path": {"task_ids": ["heavy"], "weight_tokens": 10000},
            "reserve": {
                "state": "active",
                "mode": "multi",
                "path_count": 2,
                "protected_tokens": 10000,
                "allocated_tokens": 10000,
                "floor_tokens": 500,
                "remaining_tokens": 10000,
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.0,
                "throttled": false
            },
            "paths": [
                {
                    "path_id": 0,
                    "task_ids": ["heavy", "tail"],
                    "departments": ["impl"],
                    "weight_tokens": 10000,
                    "floor_tokens": 400,
                    "allocation_tokens": 6000,
                    "deficit_tokens": 4000
                },
                {
                    "path_id": 1,
                    "task_ids": ["verify"],
                    "departments": ["qa"],
                    "weight_tokens": 8000,
                    "floor_tokens": 100,
                    "allocation_tokens": 4000,
                    "deficit_tokens": 4000
                }
            ],
            "departments": [
                {"department": "impl", "weight_tokens": 10000, "floor_tokens": 400, "path_ids": [0]},
                {"department": "qa", "weight_tokens": 8000, "floor_tokens": 100, "path_ids": [1]}
            ],
            "admitted": [],
            "deferred": [{
                "task_id": "side",
                "reason": "reserve",
                "code": "BUDGET_RESERVED"
            }],
            "deferred_order": ["side"],
            "last_decisions": [],
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("2 path(s) · allocated 10000 · floors 500"));
        assert!(lines.contains("#0 weight 10000 · floor 400 · alloc 6000 · deficit 4000"));
        assert!(lines.contains("[impl] heavy -> tail"));
        assert!(lines.contains("[qa] verify"));
        assert!(lines.contains("Department floors:"));
        assert!(lines.contains("impl weight 10000 · floor 400 · paths #0"));
        assert!(lines.contains("qa weight 8000 · floor 100 · paths #1"));
        assert!(lines.contains("Deferred order: side"));
    }

    #[test]
    fn session_renderer_reports_enumeration_truncation_and_bounded_rows() {
        let value = json!({
            "session_id": "s1",
            "session_status": "running",
            "plan_revision": 4,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "mode": "multi"},
            "critical_path": {"task_ids": ["t19"], "weight_tokens": 1000},
            "reserve": {
                "state": "active",
                "mode": "multi",
                "path_count": 16,
                "protected_tokens": 16000,
                "allocated_tokens": 16000,
                "floor_tokens": 0,
                "remaining_tokens": 50000,
                "reserve_ratio": 0.0,
                "throttle_at_ratio": 0.0,
                "throttled": false,
                "enumerated_paths": 4096,
                "paths_truncated": true
            },
            "paths": [
                {"path_id": 0, "task_ids": ["a"], "weight_tokens": 1000, "allocation_tokens": 1000},
                {"path_id": 1, "task_ids": ["b"], "weight_tokens": 1000, "allocation_tokens": 1000},
                {"path_id": 2, "task_ids": ["c"], "weight_tokens": 1000, "allocation_tokens": 1000},
                {"path_id": 3, "task_ids": ["d"], "weight_tokens": 1000, "allocation_tokens": 1000},
                {"path_id": 4, "task_ids": ["e"], "weight_tokens": 1000, "allocation_tokens": 1000}
            ],
            "departments": [],
            "admitted": ["a", "b", "c", "d", "e"],
            "deferred": [],
            "deferred_order": [],
            "last_decisions": [],
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("4096 enumerated"));
        assert!(lines.contains("truncated"));
        assert!(lines.contains("+1 more protected paths"));
        assert!(!lines.contains("Department floors:"));
    }

    #[test]
    fn department_renderer_bounds_a_wide_table() {
        let departments = (0..8)
            .map(|index| {
                json!({
                    "department": format!("dept-{index}"),
                    "weight_tokens": 100 - index,
                    "floor_tokens": 10,
                    "path_ids": [index]
                })
            })
            .collect::<Vec<_>>();
        let value = json!({
            "session_id": "s1",
            "session_status": "idle",
            "plan_revision": 1,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "mode": "multi"},
            "critical_path": {"task_ids": ["a"], "weight_tokens": 100},
            "reserve": {
                "state": "active",
                "mode": "multi",
                "path_count": 8,
                "protected_tokens": 800,
                "allocated_tokens": 800,
                "floor_tokens": 80,
                "remaining_tokens": 8_000,
                "reserve_ratio": 0.0,
                "throttle_at_ratio": 0.0,
                "throttled": false
            },
            "paths": [],
            "departments": departments,
            "admitted": [],
            "deferred": [],
            "deferred_order": [],
            "last_decisions": [],
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("Department floors:"));
        assert!(lines.contains("dept-0 weight 100 · floor 10 · paths #0"));
        assert!(lines.contains("dept-5 weight 95 · floor 10 · paths #5"));
        assert!(!lines.contains("dept-6 weight"));
        assert!(lines.contains("+2 more departments"));
    }

    #[test]
    fn single_mode_renderer_does_not_emit_a_fairness_block() {
        let value = json!({
            "session_id": "s1",
            "session_status": "idle",
            "plan_revision": 1,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "mode": "single"},
            "critical_path": {"task_ids": ["a"], "weight_tokens": 100},
            "reserve": {
                "state": "active",
                "mode": "single",
                "path_count": 1,
                "protected_tokens": 100,
                "remaining_tokens": 1000,
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.0,
                "throttled": false
            },
            "paths": [],
            "admitted": ["a"],
            "deferred": [],
            "deferred_order": [],
            "last_decisions": [],
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(!lines.contains("Path table"));
        assert!(!lines.contains("Fairness:"));
        assert!(lines.contains("Admit: a"));
    }
}
