//! Remote workspace transport, protocol, path jail, and helper server.
//!
//! This crate deliberately knows nothing about providers, models, sessions, or
//! the daemon store. It provides:
//!
//! - a bounded newline-delimited JSON-RPC 2.0 protocol;
//! - `StdioTransport` and `SshTransport` child-process transports;
//! - a client that performs the mandatory hello handshake, multiplexes
//!   requests, applies timeouts, and sends cancellations;
//! - the helper-side server (`vp-remote-agent`) with a canonical root path
//!   jail, atomic writes, structured Git operations, a process-group exec
//!   runner, and an idempotent operation ledger.
//!
//! The daemon treats a configured remote host as trusted infrastructure. This
//! crate does not claim to sandbox the remote user; see ADR-0156.

pub mod client;
pub mod exec;
pub mod framing;
pub mod git;
pub mod jail;
pub mod ledger;
pub mod protocol;
pub mod server;
pub mod transport;

pub use client::{RemoteClient, RemoteClientConfig};
pub use jail::PathJail;
pub use ledger::{
    DEFAULT_LEDGER_ENTRIES, LedgerBegin, LedgerEntry, LedgerReplay, LedgerStats,
    MAX_LEDGER_ENTRIES, OperationLedger,
};
pub use protocol::{
    CancelParams, CancelParams as RemoteCancelParams, DirEntry, EntryKind, ExecParams, ExecResult,
    GitDiffParams, GitDiffResult, GitLogEntry, GitLogParams, GitLogResult, GitRevParseParams,
    GitRevParseResult, GitStatusEntry, GitStatusParams, GitStatusResult, GitWorktreeAddParams,
    GitWorktreeAddResult, GitWorktreeListParams, GitWorktreeListResult, GitWorktreeRemoveParams,
    GitWorktreeRemoveResult, HelloParams, HelloResult, LedgerEntrySummary, LedgerStatsParams,
    LedgerStatsResult, ListDirParams, ListDirResult, MAX_FRAME_BYTES, MkdirParams, MkdirResult,
    ProbeResult, ReadEncoding, ReadFileParams, ReadFileResult, RemoteError, RemoteErrorCode,
    RemoveParams, RemoveResult, RenameParams, RenameResult, ShutdownParams, StatParams, StatResult,
    WorktreeEntry, WriteEncoding, WriteFileParams, WriteFileResult,
};
pub use server::{ServerConfig, serve, serve_stdio};
pub use transport::{
    RemoteProcess, RemoteTransport, SshConfig, SshTransport, StdioTransport, shell_quote,
    which_helper,
};

/// Protocol version understood by this crate.
pub const REMOTE_PROTOCOL_VERSION: u32 = protocol::REMOTE_PROTOCOL_VERSION;
