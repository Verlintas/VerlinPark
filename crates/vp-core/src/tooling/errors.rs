//! Stable, bounded tool-failure contract.
//!
//! Tools still return a human-readable `content` string for the provider and a
//! machine-readable `structured` value for events and UI. This module makes the
//! failure half of that pair uniform:
//!
//! ```json
//! {
//!   "code": "PATCH_CONFLICT",
//!   "message": "old_string was not found",
//!   "retryable": true,
//!   "hint": "Re-read the file and regenerate the patch from its current content.",
//!   "evidence": "path=src/lib.rs",
//!   "tool": "edit_file",
//!   "action": null
//! }
//! ```
//!
//! The catalog is static and deliberately small enough to document. Codes are
//! stable wire values; changing one is a protocol change. Unknown legacy codes
//! collapse to [`ToolErrorCode::TOOL_FAILED`] rather than being advertized as
//! retryable.
//!
//! Structured errors never carry full command output, file contents, request
//! bodies, headers, or credentials. Call sites may add a short safe evidence
//! string; [`ToolFailure::sanitize`] caps it and redacts common token shapes.
//! The provider-facing text is rendered from the same failure object, so a
//! model sees the code, retryability, hint, and tool name without parsing JSON.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Maximum characters retained in the structured failure message.
pub const MAX_FAILURE_MESSAGE_CHARS: usize = 2_000;
/// Maximum characters retained in an actionable hint.
pub const MAX_FAILURE_HINT_CHARS: usize = 600;
/// Maximum characters retained in an evidence summary.
pub const MAX_FAILURE_EVIDENCE_CHARS: usize = 800;
/// Maximum characters retained in a tool name.
pub const MAX_FAILURE_TOOL_CHARS: usize = 80;
/// Maximum characters retained in a suggested action.
pub const MAX_FAILURE_ACTION_CHARS: usize = 160;
/// Maximum number of original structured fields copied into evidence.
pub const MAX_FAILURE_EVIDENCE_FIELDS: usize = 8;
/// Maximum number of array items copied into one evidence value.
pub const MAX_FAILURE_EVIDENCE_ITEMS: usize = 8;

/// Stable failure code. It serializes as its uppercase wire string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ToolErrorCode(&'static str);

/// Declare the bounded catalog once. Keeping the code, retryability, category,
/// and default hint in one place makes it hard to add a code without a hint and
/// lets tests iterate every entry.
macro_rules! tool_error_codes {
    ($( $variant:ident => $code:literal, $retryable:literal, $category:literal, $hint:literal; )*) => {
        impl ToolErrorCode {
            $(pub const $variant: Self = Self($code);)*
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            fn catalog_entry(self) -> Option<CatalogEntry> {
                match self.0 {
                    $($code => Some(CatalogEntry {
                        code: Self::$variant,
                        retryable: $retryable,
                        category: $category,
                        hint: $hint,
                    }),)*
                    _ => None,
                }
            }
        }
    };
}

#[derive(Debug, Clone, Copy)]
struct CatalogEntry {
    code: ToolErrorCode,
    retryable: bool,
    category: &'static str,
    hint: &'static str,
}

/// Public, read-only catalog row used by `tool_help` and tests. It contains no
/// runtime data and is stable for a given build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolErrorCatalogEntry {
    pub code: ToolErrorCode,
    pub category: &'static str,
    pub retryable: bool,
    pub hint: &'static str,
}

/// The complete bounded catalog in declaration order.
pub fn catalog() -> Vec<ToolErrorCatalogEntry> {
    ToolErrorCode::ALL
        .iter()
        .filter_map(|code| code.catalog_entry())
        .map(|entry| ToolErrorCatalogEntry {
            code: entry.code,
            category: entry.category,
            retryable: entry.retryable,
            hint: entry.hint,
        })
        .collect()
}

/// Render one stable code with its default hint and retry decision.
pub fn render_code_detail(code: ToolErrorCode) -> String {
    let code = if code.is_known() {
        code
    } else {
        ToolErrorCode::TOOL_FAILED
    };
    format!(
        "## {}\ncategory: {}\nretryable: {}\nhint: {}\n\nThis is a static contract. Actual failures may add bounded, safe evidence; they never put command output or credentials in structured errors.",
        code.as_str(),
        code.category(),
        if code.default_retryable() {
            "yes"
        } else {
            "no"
        },
        code.default_hint()
    )
}

/// Bounded table of all codes, grouped by category. Used by `tool_help` so an
/// agent can look up a code without guessing or parsing an ADR.
pub fn render_catalog(max_chars: usize) -> String {
    let max_chars = max_chars.clamp(500, 20_000);
    let mut entries = catalog();
    entries.sort_by(|left, right| {
        left.category
            .cmp(right.category)
            .then_with(|| left.code.as_str().cmp(right.code.as_str()))
    });
    let mut text = String::from(
        "Tool error codes (stable; retryable means retry only after applying the hint):\n",
    );
    let mut category = "";
    for entry in entries {
        if entry.category != category {
            category = entry.category;
            text.push_str(&format!("\n[{category}]\n"));
        }
        text.push_str(&format!(
            "- {}{} — {}\n",
            entry.code.as_str(),
            if entry.retryable { " (retryable)" } else { "" },
            entry.hint
        ));
        if text.chars().count() >= max_chars {
            text.push_str("… catalog truncated at the response bound\n");
            break;
        }
    }
    truncate_chars(&text, max_chars)
}

/// Classification for tools that propagate a bare error. It is intentionally
/// conservative and based on stable error phrases, not on model output. A
/// classifier match only chooses a code; retryability still comes from the
/// catalog. An unrecognized message keeps the tool's fallback category.
pub fn classify_message(tool: &str, message: &str) -> Option<ToolErrorCode> {
    let lower = message.to_ascii_lowercase();
    let contains = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if contains(&["not a git repository", "not a git repo"]) {
        return Some(ToolErrorCode::GIT_NOT_REPOSITORY);
    }
    if contains(&[
        "does not exist",
        "no such file",
        "file not found",
        "path not found",
    ]) {
        return Some(ToolErrorCode::FILE_NOT_FOUND);
    }
    if contains(&[
        "permission denied",
        "operation not permitted",
        "access denied",
    ]) {
        return Some(ToolErrorCode::PERMISSION_DENIED);
    }
    if contains(&["symlink", "symbolic link"]) && contains(&["refus", "reject", "escape"]) {
        return Some(ToolErrorCode::SYMLINK_REFUSED);
    }
    if contains(&[
        "path escapes",
        "escapes the project root",
        "outside the worktree",
    ]) {
        return Some(if tool.starts_with("remote_") {
            ToolErrorCode::REMOTE_PATH_ESCAPE
        } else {
            ToolErrorCode::PATH_ESCAPE
        });
    }
    if tool.starts_with("remote_") && contains(&["does not belong", "escapes the remote"]) {
        return Some(ToolErrorCode::REMOTE_PATH_ESCAPE);
    }
    if contains(&["stale ref", "stale snapshot", "snapshot generation"]) {
        return Some(ToolErrorCode::STALE_REF);
    }
    if contains(&["no such process", "process not found", "unknown process"]) {
        return Some(ToolErrorCode::PROCESS_NOT_FOUND);
    }
    if contains(&["process does not belong"]) {
        return Some(ToolErrorCode::PATH_ESCAPE);
    }
    if contains(&["language server", "lsp server", "server not found in path"]) {
        return Some(ToolErrorCode::LSP_UNAVAILABLE);
    }
    if contains(&[
        "no ssh execution host",
        "local execution host; remote tools",
    ]) {
        return Some(ToolErrorCode::REMOTE_NOT_CONFIGURED);
    }
    if tool == "browser" && contains(&["helper sandbox is unavailable", "browser helper"]) {
        return Some(ToolErrorCode::BROWSER_UNAVAILABLE);
    }
    if tool == "computer" && contains(&["computer helper", "accessibility", "screen recording"]) {
        return Some(ToolErrorCode::COMPUTER_UNAVAILABLE);
    }
    if contains(&["mcp server not found", "unknown mcp server"]) {
        return Some(ToolErrorCode::MCP_SERVER_NOT_FOUND);
    }
    if contains(&["mcp tool not found", "unknown mcp tool"]) {
        return Some(ToolErrorCode::MCP_TOOL_NOT_FOUND);
    }
    if tool == "read_tool_output" && contains(&["expired"]) {
        return Some(ToolErrorCode::TOOL_OUTPUT_EXPIRED);
    }
    if tool == "read_tool_output" && contains(&["not found", "unknown output"]) {
        return Some(ToolErrorCode::TOOL_OUTPUT_NOT_FOUND);
    }
    if tool == "browser" && contains(&["navigation", "localhost", "file url", "scheme"]) {
        return Some(ToolErrorCode::BROWSER_NAVIGATION_DENIED);
    }
    if contains(&[
        "connection refused",
        "connection reset",
        "ssh",
        "remote host",
    ]) {
        return Some(ToolErrorCode::REMOTE_UNAVAILABLE);
    }
    if contains(&["timed out", "timeout"]) {
        return Some(match tool {
            "process" | "wait_for" => ToolErrorCode::WAIT_TIMEOUT,
            "lsp_query" => ToolErrorCode::LSP_TIMEOUT,
            "fetch_url" | "web_search" | "local_http" => ToolErrorCode::FETCH_TIMEOUT,
            "mcp" => ToolErrorCode::MCP_TIMEOUT,
            tool if tool.starts_with("remote_") => ToolErrorCode::REMOTE_TIMEOUT,
            _ => ToolErrorCode::TIMEOUT,
        });
    }
    if contains(&[
        "network is disabled",
        "network denied",
        "network access is disabled",
    ]) {
        return Some(ToolErrorCode::NETWORK_DENIED);
    }
    if contains(&[
        " is required",
        "must be provided",
        "missing required",
        "invalid argument",
    ]) || contains(&["invalid regex", "invalid glob"])
    {
        return Some(ToolErrorCode::INVALID_ARGUMENT);
    }
    if contains(&["too many bytes", "too large", "exceeds the limit"]) {
        return Some(ToolErrorCode::FILE_TOO_LARGE);
    }
    if contains(&["not valid utf-8", "invalid utf-8", "binary file"]) {
        return Some(ToolErrorCode::FILE_NOT_TEXT);
    }
    if contains(&["hash mismatch", "hash changed", "expected_sha256"]) {
        return Some(ToolErrorCode::HASH_MISMATCH);
    }
    if contains(&["conflict", "patch does not apply", "old_string"]) {
        return Some(ToolErrorCode::PATCH_CONFLICT);
    }
    if contains(&["response too large", "frame too large"]) {
        return Some(ToolErrorCode::MCP_RESPONSE_TOO_LARGE);
    }
    if contains(&["invalid mcp", "protocol error", "invalid json-rpc"]) {
        return Some(ToolErrorCode::MCP_PROTOCOL_ERROR);
    }
    None
}

// Conservative retryability: `true` only when retrying the same logical
// operation can reasonably succeed without changing inputs, and where a failed
// attempt has no known destructive side effect. Shell commands, writes, remote
// writes, MCP calls, and unknown failures are never marked retryable.
tool_error_codes! {
    // General
    TOOL_FAILED => "TOOL_FAILED", false, "general", "Inspect the error and evidence; do not retry an unknown failure unchanged.";
    INVALID_ARGUMENT => "INVALID_ARGUMENT", false, "general", "Correct the tool arguments against the schema, then retry.";
    NOT_FOUND => "NOT_FOUND", false, "general", "Use find_files, list_dir, or a search tool to locate the target before retrying.";
    PATH_ESCAPE => "PATH_ESCAPE", false, "general", "Use a path relative to the worktree that stays inside the project root.";
    PERMISSION_DENIED => "PERMISSION_DENIED", false, "general", "Request the missing permission or choose a workspace-scoped alternative; do not repeat the denied call.";
    APPROVAL_TIMEOUT => "APPROVAL_TIMEOUT", true, "general", "Ask the user to resolve the approval, then retry the same call.";
    SKILL_TOOL_DENIED => "SKILL_TOOL_DENIED", false, "general", "Use a tool allowed by the active skill, or deactivate the skill before retrying.";
    TOOL_NOT_AVAILABLE => "TOOL_NOT_AVAILABLE", false, "general", "Check the role/profile tool policy or call tool_search for an available alternative.";
    BUDGET_EXHAUSTED => "BUDGET_EXHAUSTED", false, "general", "Stop expanding the run; report the partial result and let the operator raise the budget.";
    CAPACITY_EXCEEDED => "CAPACITY_EXCEEDED", true, "general", "Wait for an active worker slot to free, or reduce parallel delegation.";
    CANCELLED => "CANCELLED", false, "general", "Do not retry; the run was deliberately cancelled.";
    TIMEOUT => "TIMEOUT", true, "general", "Retry with a smaller scope or a longer explicit timeout.";
    INTERNAL_ERROR => "INTERNAL_ERROR", false, "general", "Report the failure with evidence; do not loop on an internal error.";
    UNSUPPORTED_OPERATION => "UNSUPPORTED_OPERATION", false, "general", "Use a supported action from the tool schema.";
    READ_ONLY_VIOLATION => "READ_ONLY_VIOLATION", false, "general", "This role or surface is read-only; return findings instead of writing.";
    RESOURCE_LIMIT => "RESOURCE_LIMIT", false, "general", "Reduce the request size or split it into bounded steps.";
    CONFLICT => "CONFLICT", true, "general", "Re-read the current state and reconcile before retrying.";
    INVALID_ACTION => "INVALID_ACTION", false, "general", "Use one of the actions listed in the tool schema.";

    // File, read, write, edit, patch
    FILE_NOT_FOUND => "FILE_NOT_FOUND", false, "file", "Verify the path relative to the worktree; use find_files or list_dir instead of guessing.";
    FILE_TOO_LARGE => "FILE_TOO_LARGE", false, "file", "Read a bounded line range or search for the relevant region instead of loading the whole file.";
    FILE_NOT_TEXT => "FILE_NOT_TEXT", false, "file", "Use read_pdf, read_image, read_document, or a binary-aware viewer instead.";
    BINARY_FILE => "BINARY_FILE", false, "file", "Do not load binary data as UTF-8; use the matching media or office tool.";
    HASH_MISMATCH => "HASH_MISMATCH", true, "file", "Re-read the file to obtain the current sha256, then regenerate the edit.";
    PATCH_CONFLICT => "PATCH_CONFLICT", true, "file", "Re-read the file and regenerate the patch from its current content; do not retry the stale patch.";
    WRITE_FAILED => "WRITE_FAILED", false, "file", "Check the path and worktree permissions; use dry_run to validate the change first.";
    READ_FAILED => "READ_FAILED", false, "file", "Check the path and file encoding; use file_info to inspect metadata first.";
    SYMLINK_REFUSED => "SYMLINK_REFUSED", false, "file", "Target the real path inside the worktree; symlink writes are deliberately refused.";
    NOT_A_DIRECTORY => "NOT_A_DIRECTORY", false, "file", "Pass a directory path, or use a file tool for the file.";
    NOT_A_FILE => "NOT_A_FILE", false, "file", "Pass a file path; directory mutations use file_manage.";
    FILE_EXISTS => "FILE_EXISTS", false, "file", "Choose a different destination or remove the existing file explicitly.";
    RECURSIVE_REQUIRED => "RECURSIVE_REQUIRED", false, "file", "Set recursive=true for a directory operation, or operate on a single file.";
    UNSUPPORTED_CONTENT => "UNSUPPORTED_CONTENT", false, "file", "Use a tool that supports this content type; do not retry the same decoder.";
    FILE_DECODE_FAILED => "FILE_DECODE_FAILED", false, "file", "Inspect the encoding or use a bounded binary preview; do not retry the same decode.";

    // Git
    GIT_NOT_REPOSITORY => "GIT_NOT_REPOSITORY", false, "git", "Run Git tools only inside a repository worktree, or initialize one outside the agent run.";
    GIT_FAILED => "GIT_FAILED", false, "git", "Inspect git status and the exact failing action; do not retry unchanged.";
    GIT_INVALID_REF => "GIT_INVALID_REF", false, "git", "Use an existing branch, tag, or commit; resolve it with git action=log or files first.";
    GIT_CONFLICT => "GIT_CONFLICT", true, "git", "Resolve the reported conflict in the worktree, then retry the Git action.";
    GIT_UNAVAILABLE => "GIT_UNAVAILABLE", false, "git", "Install Git or run the operation outside the agent; do not retry.";
    GIT_DIRTY => "GIT_DIRTY", false, "git", "Commit or discard worktree changes before the requested Git operation.";

    // Shell and project tasks
    COMMAND_FAILED => "COMMAND_FAILED", false, "shell", "Read the exit code and stderr, fix the first real error, then rerun the smallest failing check.";
    COMMAND_TIMEOUT => "COMMAND_TIMEOUT", false, "shell", "Reduce the command scope or raise timeout_secs explicitly; do not blindly retry a command with side effects.";
    SHELL_DENIED => "SHELL_DENIED", false, "shell", "Use an allowed workspace command or request approval; do not bypass the policy.";
    SHELL_SANDBOX_FAILED => "SHELL_SANDBOX_FAILED", false, "shell", "Report the sandbox failure; do not run the command unsandboxed.";
    SHELL_NETWORK_DENIED => "SHELL_NETWORK_DENIED", false, "shell", "Use a configured fetch/search tool or ask the operator to allow network access.";
    PROJECT_UNSUPPORTED => "PROJECT_UNSUPPORTED", false, "shell", "Use explicit commands or configure the project profile; do not guess a test framework.";
    TASK_FAILED => "TASK_FAILED", false, "shell", "Inspect the reported task failure and fix the underlying command or code.";
    TEST_FAILED => "TEST_FAILED", false, "shell", "Fix the failing test or code path; rerun only after a concrete change.";

    // Process and wait
    PROCESS_EXITED => "PROCESS_EXITED", false, "process", "Start a new process or inspect the final output; do not wait on an exited process.";
    PROCESS_LIMIT => "PROCESS_LIMIT", false, "process", "Stop an idle process before starting another; the per-worktree cap is deliberate.";
    PROCESS_NOT_FOUND => "PROCESS_NOT_FOUND", false, "process", "List running processes and use a live process_id.";
    PROCESS_FAILED => "PROCESS_FAILED", false, "process", "Inspect the process output and exit state; do not retry unchanged.";
    PROCESS_LOST => "PROCESS_LOST", false, "process", "The process is gone and its exit status may be unknown after a daemon restart; inspect its logs and start a new process explicitly.";
    PROCESS_UNKNOWN => "PROCESS_UNKNOWN", true, "process", "Full PID/start-time/nonce evidence is unavailable; re-check status before acting and never signal an unverified pid.";
    PROCESS_STOP_TIMEOUT => "PROCESS_STOP_TIMEOUT", false, "process", "The process did not stop within the bounded grace period; inspect whether SIGKILL was delivered and do not assume it exited.";
    WAIT_TIMEOUT => "WAIT_TIMEOUT", true, "process", "Read available output and wait again with a longer timeout if the process is still running.";
    PROCESS_SPAWN_FAILED => "PROCESS_SPAWN_FAILED", false, "process", "Check the command path and policy; do not retry the same spawn.";
    PROCESS_IO_FAILED => "PROCESS_IO_FAILED", false, "process", "Check whether the process exited and reopen its output before retrying.";

    // LSP and code intelligence
    LSP_UNAVAILABLE => "LSP_UNAVAILABLE", false, "lsp", "Install the language server for this language or fall back to lexical tools; no server is auto-installed.";
    LSP_ERROR => "LSP_ERROR", false, "lsp", "Inspect the server response; restart the query after fixing the file or server state.";
    LSP_TIMEOUT => "LSP_TIMEOUT", true, "lsp", "Retry once for a large workspace, or use lexical find_symbol/find_references with explicit lower confidence.";
    SYMBOL_NOT_FOUND => "SYMBOL_NOT_FOUND", false, "code", "Search for the symbol name with repo_map or find_symbol; verify the file and symbol spelling.";
    DIAGNOSTICS_UNAVAILABLE => "DIAGNOSTICS_UNAVAILABLE", false, "code", "Run the project's check command or install the language server; do not pretend diagnostics are clean.";
    INDEX_UNAVAILABLE => "INDEX_UNAVAILABLE", false, "code", "Use path-based search or retry after the index is rebuilt; results are lexical unless source=lsp.";
    REFERENCE_NOT_FOUND => "REFERENCE_NOT_FOUND", false, "code", "Use find_symbol first, then request references for the resolved symbol id.";

    // Browser and native computer
    BROWSER_UNAVAILABLE => "BROWSER_UNAVAILABLE", false, "browser", "Use local_http or fetch_url for a non-GUI check; the browser helper may not be installed.";
    BROWSER_ERROR => "BROWSER_ERROR", false, "browser", "Inspect the page URL and console; navigate again after correcting the target.";
    BROWSER_NAVIGATION_DENIED => "BROWSER_NAVIGATION_DENIED", false, "browser", "Navigate only to localhost HTTP(S) or a worktree file: URL.";
    BROWSER_EVALUATION_FAILED => "BROWSER_EVALUATION_FAILED", false, "browser", "Use a DOM snapshot instead of page JavaScript, or fix the expression.";
    COMPUTER_UNAVAILABLE => "COMPUTER_UNAVAILABLE", false, "computer", "Grant Accessibility/Screen Recording or use a non-GUI verification path; the helper is not sandboxed.";
    COMPUTER_ERROR => "COMPUTER_ERROR", false, "computer", "Take a fresh snapshot and inspect the target app/window before retrying.";
    COMPUTER_PERMISSION_DENIED => "COMPUTER_PERMISSION_DENIED", false, "computer", "Ask the operator to grant the required macOS privacy permission; do not bypass TCC.";
    COMPUTER_BUSY => "COMPUTER_BUSY", true, "computer", "Wait for the GUI lease holder to finish, then retry with a fresh snapshot.";
    GUI_BUSY => "GUI_BUSY", true, "computer", "Read the holder and retry_after_ms hint, wait for the bounded GUI queue, then retry with a fresh snapshot.";
    GUI_LEASE_FAILED => "GUI_LEASE_FAILED", false, "computer", "Inspect the GUI lease error and runtime state; do not loop on a failed acquisition.";
    SENSITIVE_TARGET => "SENSITIVE_TARGET", false, "computer", "The target app/window is protected by policy; ask the operator instead of automating it.";
    COMPUTER_RENDER_FAILED => "COMPUTER_RENDER_FAILED", false, "computer", "Retry the capture after the screen settles, or use a DOM snapshot for localhost pages.";
    BROWSER_RENDER_FAILED => "BROWSER_RENDER_FAILED", false, "browser", "Retry the screenshot after the page settles, or use a DOM snapshot instead of a viewport capture.";
    STALE_REF => "STALE_REF", true, "computer", "Take a fresh snapshot and use refs from that snapshot; never reuse stale refs.";
    WINDOW_NOT_FOUND => "WINDOW_NOT_FOUND", false, "computer", "List windows/apps again and choose a live target.";
    APPS_REJECTED => "APPS_REJECTED", false, "computer", "The helper rejected the app list; inspect the error and request a bounded apps action.";
    SNAPSHOT_REJECTED => "SNAPSHOT_REJECTED", false, "computer", "Reduce max_depth/max_elements or target a specific window before snapshotting again.";
    WINDOWS_REJECTED => "WINDOWS_REJECTED", false, "computer", "Retry windows with bounded filters; do not request an unbounded window list.";

    // Remote workspace
    REMOTE_NOT_CONFIGURED => "REMOTE_NOT_CONFIGURED", false, "remote", "Configure an SSH execution host and retry; remote tools never fall back to local paths.";
    REMOTE_UNAVAILABLE => "REMOTE_UNAVAILABLE", true, "remote", "Check SSH connectivity and the helper installation, then retry.";
    REMOTE_AUTH_FAILED => "REMOTE_AUTH_FAILED", false, "remote", "Fix SSH credentials/agent forwarding; do not retry with the same rejected credentials.";
    REMOTE_PATH_ESCAPE => "REMOTE_PATH_ESCAPE", false, "remote", "Use a path relative to the configured remote root that does not escape it.";
    REMOTE_WRITE_FAILED => "REMOTE_WRITE_FAILED", false, "remote", "Re-read the remote file and verify the expected hash before writing again.";
    REMOTE_READ_FAILED => "REMOTE_READ_FAILED", false, "remote", "Verify the remote path and helper access; do not assume a local file.";
    REMOTE_EXEC_FAILED => "REMOTE_EXEC_FAILED", false, "remote", "Read the remote exit code and stderr; fix the command before retrying.";
    REMOTE_GIT_FAILED => "REMOTE_GIT_FAILED", false, "remote", "Inspect remote Git status; this result is not merged into the local session queue.";
    REMOTE_WORKTREE_FAILED => "REMOTE_WORKTREE_FAILED", false, "remote", "Inspect the remote worktree path and branch; remove or attach it explicitly.";
    REMOTE_NETWORK_DENIED => "REMOTE_NETWORK_DENIED", false, "remote", "The project policy denies remote exec because the remote host cannot be sandboxed.";
    REMOTE_TIMEOUT => "REMOTE_TIMEOUT", true, "remote", "Retry with a bounded operation or after checking SSH latency.";
    REMOTE_FAILED => "REMOTE_FAILED", false, "remote", "Inspect the remote helper response and SSH state; do not silently reroute locally.";
    REMOTE_PROTOCOL_ERROR => "REMOTE_PROTOCOL_ERROR", false, "remote", "The helper returned an invalid frame; verify helper version compatibility.";

    // Tool-output paging
    TOOL_OUTPUT_NOT_FOUND => "TOOL_OUTPUT_NOT_FOUND", false, "tool_output", "Use the output_ref from the spilled result; handles are session-scoped.";
    TOOL_OUTPUT_EXPIRED => "TOOL_OUTPUT_EXPIRED", false, "tool_output", "Re-run the original tool if the output was pruned; expired bodies are not recoverable.";
    TOOL_OUTPUT_INVALID => "TOOL_OUTPUT_INVALID", false, "tool_output", "Use a valid line/byte range and a literal pattern; see the read_tool_output schema.";
    TOOL_OUTPUT_FAILED => "TOOL_OUTPUT_FAILED", false, "tool_output", "Inspect the paging error; the original tool result may still be available in the transcript.";

    // MCP
    MCP_SERVER_NOT_FOUND => "MCP_SERVER_NOT_FOUND", false, "mcp", "List MCP servers and use an exact server id or name.";
    MCP_DISABLED => "MCP_DISABLED", false, "mcp", "Enable the MCP server in project settings before calling it.";
    MCP_TOOL_NOT_FOUND => "MCP_TOOL_NOT_FOUND", false, "mcp", "List the server's tools with mcp action=list and use an exact tool name.";
    MCP_TRANSPORT_ERROR => "MCP_TRANSPORT_ERROR", true, "mcp", "Check the server process/HTTP endpoint, then retry the call.";
    MCP_TIMEOUT => "MCP_TIMEOUT", true, "mcp", "Retry once with a smaller request or after the server is responsive.";
    MCP_PROTOCOL_ERROR => "MCP_PROTOCOL_ERROR", false, "mcp", "The server returned an invalid MCP response; verify server compatibility.";
    MCP_RESPONSE_TOO_LARGE => "MCP_RESPONSE_TOO_LARGE", false, "mcp", "Ask the server for a smaller result or use its paging/resource API.";
    MCP_FAILED => "MCP_FAILED", false, "mcp", "Inspect the server error and arguments; do not retry unchanged.";
    MCP_RESPONSE_INVALID => "MCP_RESPONSE_INVALID", false, "mcp", "Treat the response as untrusted and inspect the server logs.";

    // Fetch, HTTP, and web search
    FETCH_INVALID_URL => "FETCH_INVALID_URL", false, "network", "Use an absolute public http(s) URL; credentials and non-HTTP schemes are rejected.";
    FETCH_BLOCKED => "FETCH_BLOCKED", false, "network", "The URL resolves to a private/local address; use local_http for localhost or a public source.";
    FETCH_TIMEOUT => "FETCH_TIMEOUT", true, "network", "Retry once if the endpoint is transiently slow; otherwise use a different source.";
    FETCH_FAILED => "FETCH_FAILED", false, "network", "Check the URL and network policy; do not retry the same failing request indefinitely.";
    FETCH_HTTP_ERROR => "FETCH_HTTP_ERROR", true, "network", "Inspect the HTTP status; retry only for 429/5xx and with the server's retry guidance.";
    FETCH_TOO_LARGE => "FETCH_TOO_LARGE", false, "network", "Request a bounded max_chars range or use a paged/raw endpoint.";
    NETWORK_DENIED => "NETWORK_DENIED", false, "network", "Network access is disabled by policy; ask the operator or use a local source.";
    HTTP_ERROR => "HTTP_ERROR", false, "network", "Inspect the status/body and the request id; do not retry a 4xx unchanged.";
    WEB_SEARCH_UNAVAILABLE => "WEB_SEARCH_UNAVAILABLE", false, "network", "Configure a search provider or use fetch_url with a known source.";
    WEB_SEARCH_FAILED => "WEB_SEARCH_FAILED", true, "network", "Retry once for a transient provider error; otherwise inspect provider configuration.";

    // Office, media, and data
    PDF_EXTRACT_FAILED => "PDF_EXTRACT_FAILED", false, "media", "Use a page range or an external PDF tool; the file may be scanned or malformed.";
    UNSUPPORTED_FORMAT => "UNSUPPORTED_FORMAT", false, "media", "Use a supported document/media format or a shell/CLI decoder.";
    MALFORMED_XML => "MALFORMED_XML", false, "media", "Inspect the document package; the container XML is invalid.";
    MISSING_DOCUMENT_PART => "MISSING_DOCUMENT_PART", false, "media", "The document package is incomplete; do not retry the same decoder.";
    SHEET_NOT_FOUND => "SHEET_NOT_FOUND", false, "data", "List sheet names first and use an exact sheet name or index.";
    IMAGE_OCR_UNAVAILABLE => "IMAGE_OCR_UNAVAILABLE", false, "media", "Install tesseract or inspect image metadata only; OCR is optional.";
    DATA_QUERY_INVALID => "DATA_QUERY_INVALID", false, "data", "Use the documented deterministic path subset; this is not JMESPath/JQ.";
    DATA_DECODE_FAILED => "DATA_DECODE_FAILED", false, "data", "Inspect the file format/encoding; do not retry the same parser.";

    // Cross-cutting orchestration and session tools
    MAILBOX_FAILED => "MAILBOX_FAILED", false, "session", "Inspect the mailbox error and recipient; delivery is pull-based.";
    MEMORY_FAILED => "MEMORY_FAILED", false, "session", "Inspect memory scope/evidence requirements; do not invent evidence.";
    SOURCE_FAILED => "SOURCE_FAILED", false, "session", "Inspect the source id and indexing state; cite only returned keys.";
    TASK_CONTROL_FAILED => "TASK_CONTROL_FAILED", false, "session", "Inspect the task status and allowed transition before retrying.";
    TASK_NOT_FOUND => "TASK_NOT_FOUND", false, "session", "List the stored plan tasks and use an exact task id from this session.";
    ARTIFACT_FAILED => "ARTIFACT_FAILED", false, "session", "Check artifact size/type limits; do not retry an oversized publish.";
    SKILL_FAILED => "SKILL_FAILED", false, "session", "Inspect skill metadata and activation policy.";
    COORDINATION_FAILED => "COORDINATION_FAILED", false, "session", "Inspect the coordination note type and recipient.";
    QUESTION_TIMEOUT => "QUESTION_TIMEOUT", false, "session", "Proceed with the safest assumption or ask again; do not block forever.";
    VERDICT_INVALID => "VERDICT_INVALID", false, "session", "Submit a verdict with approved plus a concrete summary.";
    TODO_INVALID => "TODO_INVALID", false, "session", "Use 1-50 bounded todo items with valid statuses.";
    ACP_AGENT_FAILED => "ACP_AGENT_FAILED", false, "acp", "Inspect the external agent stderr/report; do not retry an unchanged session.";
    ACP_TOOL_FAILED => "ACP_TOOL_FAILED", false, "acp", "Inspect the external tool result; the ACP agent owns its own tools.";
    SUBAGENT_FAILED => "SUBAGENT_FAILED", false, "acp", "Inspect the child agent summary and task state before delegating again.";
}

impl ToolErrorCode {
    /// Stable wire string.
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// True when the code appears in the documented catalog.
    pub fn is_known(self) -> bool {
        self.catalog_entry().is_some()
    }

    /// Resolve an exact catalog code. Unknown strings return `None`; callers
    /// must decide whether to preserve or collapse them.
    pub fn from_code(code: &str) -> Option<Self> {
        let normalized = code.trim();
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.0 == normalized)
    }

    /// Resolve a catalog code, collapsing unknown strings to `TOOL_FAILED`.
    pub fn from_code_or_failed(code: &str) -> Self {
        Self::from_code(code).unwrap_or(Self::TOOL_FAILED)
    }

    /// Conservative default retryability for this code.
    pub fn default_retryable(self) -> bool {
        self.catalog_entry()
            .map(|entry| entry.retryable)
            .unwrap_or(false)
    }

    /// Stable category used by the dashboard and tests.
    pub fn category(self) -> &'static str {
        self.catalog_entry()
            .map(|entry| entry.category)
            .unwrap_or("general")
    }

    /// Actionable default hint. Never contains run-specific data.
    pub fn default_hint(self) -> &'static str {
        self.catalog_entry()
            .map(|entry| entry.hint)
            .unwrap_or("Inspect the error and evidence; do not retry an unknown failure unchanged.")
    }

    /// Fallback code for a tool whose implementation returned a bare error.
    /// This is a known category, not an unknown provider failure.
    pub fn for_tool_fallback(tool: &str) -> Self {
        match tool {
            "read_file" | "list_dir" | "grep" | "find_files" | "file_info" | "read_files" => {
                Self::READ_FAILED
            }
            "write_file" | "edit_file" | "apply_patch" | "file_manage" => Self::WRITE_FAILED,
            "git" => Self::GIT_FAILED,
            "shell" | "run_project_task" => Self::COMMAND_FAILED,
            "process" | "wait_for" => Self::PROCESS_FAILED,
            "lsp_query" => Self::LSP_ERROR,
            "browser" => Self::BROWSER_ERROR,
            "computer" => Self::COMPUTER_ERROR,
            "mcp" => Self::MCP_FAILED,
            "fetch_url" | "web_search" | "local_http" => Self::FETCH_FAILED,
            "read_tool_output" => Self::TOOL_OUTPUT_FAILED,
            "send_message" | "read_mailbox" => Self::MAILBOX_FAILED,
            "remember" | "recall" => Self::MEMORY_FAILED,
            "search_sources" | "read_source" | "list_sources" => Self::SOURCE_FAILED,
            tool if tool.starts_with("remote_") => Self::REMOTE_FAILED,
            _ => Self::TOOL_FAILED,
        }
    }
}

impl std::fmt::Display for ToolErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl<'de> Deserialize<'de> for ToolErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_code_or_failed(&raw))
    }
}

/// The uniform failure object. Every field is bounded before it enters an
/// event or provider prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolFailure {
    pub code: ToolErrorCode,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl ToolFailure {
    /// Build a failure using the catalog's default retryability and hint.
    pub fn new(code: ToolErrorCode, message: impl Into<String>) -> Self {
        let code = if code.is_known() {
            code
        } else {
            ToolErrorCode::TOOL_FAILED
        };
        Self {
            code,
            message: message.into(),
            retryable: code.default_retryable(),
            hint: code.default_hint().to_string(),
            evidence: None,
            tool: None,
            action: None,
        }
    }

    /// Parse a legacy `structured` error object. Safe original fields are
    /// preserved by [`Self::enrich_structured`]; unsafe output, command, and
    /// credential fields are dropped.
    pub fn from_structured(
        fallback_message: impl Into<String>,
        structured: &Value,
        tool: Option<&str>,
    ) -> Self {
        let object = structured.as_object();
        let raw_code = object
            .and_then(|object| object.get("code"))
            .and_then(Value::as_str);
        let code = raw_code
            .map(ToolErrorCode::from_code_or_failed)
            .unwrap_or(ToolErrorCode::TOOL_FAILED);
        let message = object
            .and_then(|object| object.get("message"))
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| fallback_message.into());
        let retryable = object
            .and_then(|object| object.get("retryable"))
            .and_then(Value::as_bool)
            .unwrap_or_else(|| code.default_retryable());
        let hint = object
            .and_then(|object| object.get("hint"))
            .and_then(Value::as_str)
            .filter(|hint| !hint.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| code.default_hint().to_string());
        let action = object
            .and_then(|object| object.get("action"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                object
                    .and_then(|object| object.get("action"))
                    .filter(|value| value.is_number() || value.is_boolean())
                    .map(|value| value.to_string())
            });
        let evidence = object
            .and_then(|object| object.get("evidence"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| object.and_then(evidence_from_object));
        Self {
            code,
            message,
            retryable,
            hint,
            evidence,
            tool: tool.map(str::to_string),
            action,
        }
        .sanitize()
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self.sanitize()
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self.sanitize()
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = Some(evidence.into());
        self.sanitize()
    }

    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self.sanitize()
    }

    /// Cap every field and redact common secret shapes. Idempotent.
    pub fn sanitize(mut self) -> Self {
        self.message = sanitize_text(&self.message, MAX_FAILURE_MESSAGE_CHARS);
        self.hint = sanitize_text(&self.hint, MAX_FAILURE_HINT_CHARS);
        self.evidence = self.evidence.map(|evidence| sanitize_evidence(&evidence));
        self.tool = self.tool.map(|tool| {
            let tool = sanitize_text(&tool, MAX_FAILURE_TOOL_CHARS);
            if tool.is_empty() {
                "unknown".to_string()
            } else {
                tool
            }
        });
        self.action = self
            .action
            .map(|action| sanitize_text(&action, MAX_FAILURE_ACTION_CHARS));
        // The hint must stay actionable even if a caller passes an empty one.
        if self.hint.trim().is_empty() {
            self.hint = self.code.default_hint().to_string();
        }
        if self.message.trim().is_empty() {
            self.message = "tool failed".to_string();
        }
        // Unknown failures are never advertized as retryable, even if a legacy
        // payload or a caller supplied that flag.
        if self.code == ToolErrorCode::TOOL_FAILED {
            self.retryable = false;
        }
        self
    }

    /// Merge the failure contract into a legacy structured object while
    /// dropping unsafe output-bearing keys. Safe fields such as `path`,
    /// `action`, `exit_code`, and `process_id` survive for UI compatibility.
    pub fn enrich_structured(&self, structured: &Value) -> Value {
        // Non-object legacy payloads are intentionally not copied: they could
        // be an unbounded string or array of raw output.
        let mut object = match structured {
            Value::Object(object) => object.clone(),
            _ => Map::new(),
        };
        for key in BLOCKED_STRUCTURED_KEYS {
            object.remove(*key);
        }
        object.insert("code".to_string(), json!(self.code.as_str()));
        object.insert("message".to_string(), json!(self.message));
        object.insert("retryable".to_string(), json!(self.retryable));
        object.insert("hint".to_string(), json!(self.hint));
        match &self.evidence {
            Some(evidence) => {
                object.insert("evidence".to_string(), json!(evidence));
            }
            None => {
                object.remove("evidence");
            }
        }
        match &self.tool {
            Some(tool) => {
                object.insert("tool".to_string(), json!(tool));
            }
            None => {
                object.remove("tool");
            }
        }
        match &self.action {
            Some(action) => {
                object.insert("action".to_string(), json!(action));
            }
            None => {
                object.remove("action");
            }
        }
        Value::Object(object)
    }

    /// Structured representation used in `ToolOutput::structured` and events.
    pub fn to_structured(&self) -> Value {
        let mut object = Map::new();
        object.insert("code".to_string(), json!(self.code.as_str()));
        object.insert("message".to_string(), json!(self.message));
        object.insert("retryable".to_string(), json!(self.retryable));
        object.insert("hint".to_string(), json!(self.hint));
        if let Some(evidence) = &self.evidence {
            object.insert("evidence".to_string(), json!(evidence));
        }
        if let Some(tool) = &self.tool {
            object.insert("tool".to_string(), json!(tool));
        }
        if let Some(action) = &self.action {
            object.insert("action".to_string(), json!(action));
        }
        Value::Object(object)
    }

    /// Stable provider-facing text. It starts with `error[CODE]` so a model can
    /// recognize the contract without parsing the structured event.
    pub fn render(&self) -> String {
        let mut text = format!("error[{}] {}", self.code.as_str(), self.message);
        text.push_str(&format!(
            "\nretryable: {}",
            if self.retryable { "yes" } else { "no" }
        ));
        if !self.hint.trim().is_empty() {
            text.push_str(&format!("\nhint: {}", self.hint));
        }
        if let Some(tool) = &self.tool {
            text.push_str(&format!("\ntool: {tool}"));
        }
        if let Some(action) = &self.action {
            text.push_str(&format!("\naction: {action}"));
        }
        if let Some(evidence) = &self.evidence {
            text.push_str(&format!("\nevidence: {evidence}"));
        }
        truncate_chars(
            &text,
            MAX_FAILURE_MESSAGE_CHARS + MAX_FAILURE_HINT_CHARS + 1_200,
        )
    }
}

/// Keys that may contain unbounded command output, file content, request
/// bodies, or credentials. They are removed from structured failures even when
/// a legacy call site supplied them.
const BLOCKED_STRUCTURED_KEYS: &[&str] = &[
    "command",
    "commands",
    "output",
    "stdout",
    "stderr",
    "tail",
    "content",
    "text",
    "body",
    "input",
    "arguments",
    "env",
    "environment",
    "headers",
    "authorization",
    "token",
    "access_token",
    "api_key",
    "secret",
    "password",
    "credential",
    "credentials",
    "request_body",
    "response_body",
    "raw",
];

/// Evidence is limited to short, operational identity fields. A path, status,
/// process id, or line range helps the model recover; a full command or diff
/// does not belong in the structured failure.
const SAFE_EVIDENCE_KEYS: &[&str] = &[
    "path",
    "paths",
    "source",
    "destination",
    "action",
    "name",
    "symbol",
    "symbol_id",
    "line",
    "lines",
    "line_count",
    "start_line",
    "end_line",
    "offset",
    "limit",
    "matches",
    "files",
    "file_count",
    "process_id",
    "pid",
    "exit_code",
    "status",
    "status_code",
    "url",
    "host",
    "provider",
    "format",
    "size_bytes",
    "bytes",
    "limit_bytes",
    "elapsed_ms",
    "duration_ms",
    "timeout_secs",
    "retry_after_ms",
    "attempt",
    "attempts",
    "request_id",
    "operation_id",
    "server_id",
    "server_name",
    "tool_name",
    "task_id",
    "session_id",
    "commit",
    "rev",
    "branch",
    "expected_sha256",
    "actual_sha256",
];

fn evidence_from_object(object: &Map<String, Value>) -> Option<String> {
    let mut parts = Vec::new();
    for key in SAFE_EVIDENCE_KEYS {
        if parts.len() >= MAX_FAILURE_EVIDENCE_FIELDS {
            break;
        }
        let Some(value) = object.get(*key) else {
            continue;
        };
        let Some(rendered) = evidence_value(key, value) else {
            continue;
        };
        if rendered.is_empty() {
            continue;
        }
        parts.push(format!("{key}={rendered}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(sanitize_evidence(&parts.join("; ")))
    }
}

fn evidence_value(key: &str, value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            if key == "url" {
                Some(sanitize_url(text))
            } else {
                Some(text.clone())
            }
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        Value::Array(items) => {
            let rendered = items
                .iter()
                .take(MAX_FAILURE_EVIDENCE_ITEMS)
                .filter_map(|item| match item {
                    Value::String(text) => Some(text.clone()),
                    Value::Number(number) => Some(number.to_string()),
                    Value::Bool(boolean) => Some(boolean.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(", ");
            if rendered.is_empty() {
                None
            } else {
                Some(rendered)
            }
        }
        _ => None,
    }
}

fn sanitize_evidence(evidence: &str) -> String {
    let collapsed = evidence
        .chars()
        .map(|character| {
            if character.is_control() && character != '\n' && character != '\t' {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    truncate_chars(&redact_secrets(&collapsed), MAX_FAILURE_EVIDENCE_CHARS)
}

fn sanitize_text(text: &str, max_chars: usize) -> String {
    let cleaned = text
        .chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .collect::<String>();
    truncate_chars(&redact_secrets(&cleaned), max_chars)
}

fn sanitize_url(raw: &str) -> String {
    let without_fragment = raw.split('#').next().unwrap_or(raw);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    match without_query.split_once("://") {
        Some((scheme, rest)) => {
            let host_path = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
            format!("{scheme}://{host_path}")
        }
        None => without_query.to_string(),
    }
}

fn redact_secrets(text: &str) -> String {
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static URL_USERINFO: OnceLock<Regex> = OnceLock::new();
    static TOKEN_SHAPE: OnceLock<Regex> = OnceLock::new();
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r"(?i)\b(authorization|token|access[_-]?token|api[_-]?key|secret|password|passwd|credential)\b\s*[:=]\s*(?:bearer\s+)?[^\s,;]+",
        )
        .expect("validated secret assignment regex")
    });
    let bearer = BEARER
        .get_or_init(|| Regex::new(r"(?i)\bbearer\s+[^\s,;]+").expect("validated bearer regex"));
    let url_userinfo = URL_USERINFO.get_or_init(|| {
        Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s@]+@").expect("validated URL userinfo regex")
    });
    let token_shape = TOKEN_SHAPE.get_or_init(|| {
        Regex::new(r"\b(sk-[A-Za-z0-9_-]{8,}|ghp_[A-Za-z0-9]{8,}|github_pat_[A-Za-z0-9_]{8,}|xox[baprs]-[A-Za-z0-9-]{8,}|AKIA[0-9A-Z]{12,})\b")
            .expect("validated token shape regex")
    });
    let redacted = assignment.replace_all(text, "$1=[redacted]");
    let redacted = bearer.replace_all(&redacted, "Bearer [redacted]");
    let redacted = url_userinfo.replace_all(&redacted, "$1[redacted]@");
    token_shape
        .replace_all(&redacted, "[redacted-token]")
        .to_string()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut output = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

/// Convert an arbitrary tool error into a coded failure. A downcast
/// [`ToolFailure`] keeps its specific code; everything else gets the tool's
/// conservative fallback category.
pub fn failure_from_anyhow(tool: &str, error: &anyhow::Error) -> ToolFailure {
    if let Some(failure) = error.downcast_ref::<ToolFailure>() {
        let mut failure = failure.clone();
        if failure.tool.is_none() {
            failure.tool = Some(tool.to_string());
        }
        return failure.sanitize();
    }
    let message = error.to_string();
    let code =
        classify_message(tool, &message).unwrap_or_else(|| ToolErrorCode::for_tool_fallback(tool));
    ToolFailure::new(code, format!("{tool} failed: {message}")).with_tool(tool)
}

/// Wire shape for decoding. Every field is optional so an older/partial
/// failure object still produces a valid, conservative contract.
#[derive(Deserialize)]
struct RawToolFailure {
    #[serde(default)]
    code: Option<ToolErrorCode>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    retryable: Option<bool>,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    action: Option<String>,
}

impl<'de> Deserialize<'de> for ToolFailure {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawToolFailure::deserialize(deserializer)?;
        let code = raw
            .code
            .map(|code| {
                if code.is_known() {
                    code
                } else {
                    ToolErrorCode::TOOL_FAILED
                }
            })
            .unwrap_or(ToolErrorCode::TOOL_FAILED);
        let mut failure = ToolFailure::new(code, raw.message.unwrap_or_default());
        if let Some(retryable) = raw.retryable {
            failure.retryable = retryable;
        }
        if let Some(hint) = raw.hint {
            failure.hint = hint;
        }
        failure.evidence = raw.evidence;
        failure.tool = raw.tool;
        failure.action = raw.action;
        Ok(failure.sanitize())
    }
}

impl std::fmt::Display for ToolFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.render())
    }
}

impl std::error::Error for ToolFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_entry_is_uppercase_unique_and_has_a_hint() {
        assert!(ToolErrorCode::ALL.len() >= 100);
        let mut seen = std::collections::HashSet::new();
        for code in ToolErrorCode::ALL {
            assert!(code.is_known(), "{code} is missing from the catalog");
            assert_eq!(code.as_str(), code.as_str().to_ascii_uppercase());
            assert!(!code.as_str().contains(' '));
            assert!(
                seen.insert(code.as_str()),
                "duplicate error code {}",
                code.as_str()
            );
            assert!(!code.category().is_empty());
            assert!(code.default_hint().len() >= 20, "{code} hint is too short");
            assert!(code.default_hint().chars().count() <= MAX_FAILURE_HINT_CHARS);
            assert!(!code.default_hint().contains('\0'));
        }
    }

    #[test]
    fn retryability_is_conservative_for_destructive_and_unknown_codes() {
        for code in [
            ToolErrorCode::TOOL_FAILED,
            ToolErrorCode::WRITE_FAILED,
            ToolErrorCode::COMMAND_FAILED,
            ToolErrorCode::COMMAND_TIMEOUT,
            ToolErrorCode::REMOTE_WRITE_FAILED,
            ToolErrorCode::MCP_FAILED,
            ToolErrorCode::PERMISSION_DENIED,
            ToolErrorCode::INVALID_ARGUMENT,
            ToolErrorCode::FILE_NOT_FOUND,
        ] {
            assert!(!code.default_retryable(), "{code} must not be retryable");
        }
        for code in [
            ToolErrorCode::PATCH_CONFLICT,
            ToolErrorCode::HASH_MISMATCH,
            ToolErrorCode::WAIT_TIMEOUT,
            ToolErrorCode::STALE_REF,
            ToolErrorCode::FETCH_TIMEOUT,
            ToolErrorCode::MCP_TIMEOUT,
            ToolErrorCode::LSP_TIMEOUT,
        ] {
            assert!(code.default_retryable(), "{code} should be retryable");
        }
    }

    #[test]
    fn unknown_codes_collapse_to_tool_failed() {
        assert_eq!(
            ToolErrorCode::from_code_or_failed("NOT_A_REAL_CODE"),
            ToolErrorCode::TOOL_FAILED
        );
        assert_eq!(
            ToolErrorCode::from_code("PATCH_CONFLICT"),
            Some(ToolErrorCode::PATCH_CONFLICT)
        );
        let failure = ToolFailure::from_structured("boom", &json!({"code": "MADE_UP"}), None);
        assert_eq!(failure.code, ToolErrorCode::TOOL_FAILED);
        assert!(!failure.retryable);
    }

    #[test]
    fn coded_failure_renders_stable_prefix_and_actionable_hint() {
        let failure = ToolFailure::new(ToolErrorCode::PATCH_CONFLICT, "old_string was not found")
            .with_tool("edit_file")
            .with_evidence("path=src/lib.rs");
        let text = failure.render();
        assert!(text.starts_with("error[PATCH_CONFLICT] old_string was not found\n"));
        assert!(text.contains("retryable: yes"));
        assert!(text.contains("hint: Re-read the file"));
        assert!(text.contains("tool: edit_file"));
        assert!(text.contains("evidence: path=src/lib.rs"));
        let structured = failure.to_structured();
        assert_eq!(structured["code"], "PATCH_CONFLICT");
        assert_eq!(structured["retryable"], true);
        assert!(structured["message"].as_str().is_some());
        assert!(structured["hint"].as_str().is_some());
        assert_eq!(structured["tool"], "edit_file");
    }

    #[test]
    fn structured_contract_caps_provider_text_and_evidence() {
        let failure = ToolFailure::new(ToolErrorCode::COMMAND_FAILED, "x".repeat(50_000))
            .with_evidence("y".repeat(50_000));
        let text = failure.render();
        assert!(text.len() <= MAX_FAILURE_MESSAGE_CHARS + MAX_FAILURE_HINT_CHARS + 1_300);
        let structured = failure.to_structured();
        assert!(
            structured["message"].as_str().unwrap().chars().count()
                <= MAX_FAILURE_MESSAGE_CHARS + 1
        );
        assert!(
            structured["evidence"].as_str().unwrap().chars().count()
                <= MAX_FAILURE_EVIDENCE_CHARS + 1
        );
    }

    #[test]
    fn legacy_structured_error_is_enriched_and_strips_output_fields() {
        let original = json!({
            "code": "PROCESS_EXITED",
            "process_id": "proc-1",
            "exit_code": 2,
            "tail": "secret output from the process",
            "command": "curl -H 'Authorization: Bearer abcdefghijklmnop' https://example.test/?token=secret",
            "stdout": "full stdout",
            "api_key": "sk-abcdefghijklmnop",
            "path": "src/main.rs"
        });
        let failure = ToolFailure::from_structured("process exited", &original, Some("process"));
        let enriched = failure.enrich_structured(&original);
        assert_eq!(enriched["code"], "PROCESS_EXITED");
        assert_eq!(enriched["retryable"], false);
        assert_eq!(enriched["process_id"], "proc-1");
        assert_eq!(enriched["exit_code"], 2);
        assert_eq!(enriched["path"], "src/main.rs");
        assert_eq!(enriched["tool"], "process");
        for key in [
            "tail", "command", "stdout", "api_key", "input", "headers", "secret",
        ] {
            assert!(enriched.get(key).is_none(), "unsafe key {key} survived");
        }
        let rendered = serde_json::to_string(&enriched).unwrap();
        assert!(!rendered.contains("Bearer abcdefghijklmnop"));
        assert!(!rendered.contains("sk-abcdefghijklmnop"));
        assert!(!rendered.contains("full stdout"));
    }

    #[test]
    fn every_blocked_structured_key_is_removed() {
        let mut object = serde_json::Map::new();
        for key in BLOCKED_STRUCTURED_KEYS {
            object.insert((*key).to_string(), json!("must not survive"));
        }
        object.insert("path".to_string(), json!("src/lib.rs"));
        object.insert("exit_code".to_string(), json!(2));
        let failure = ToolFailure::new(ToolErrorCode::COMMAND_FAILED, "failed");
        let enriched = failure.enrich_structured(&Value::Object(object));
        for key in BLOCKED_STRUCTURED_KEYS {
            assert!(enriched.get(*key).is_none(), "blocked key {key} survived");
        }
        assert_eq!(enriched["path"], "src/lib.rs");
        assert_eq!(enriched["exit_code"], 2);
        assert_eq!(enriched["code"], "COMMAND_FAILED");
    }

    #[test]
    fn evidence_is_limited_to_safe_identity_fields() {
        let failure = ToolFailure::from_structured(
            "failed",
            &json!({
                "code": "REMOTE_EXEC_FAILED",
                "path": "src/main.rs",
                "paths": ["a.rs", "b.rs"],
                "action": "exec",
                "exit_code": 1,
                "elapsed_ms": 22,
                "timeout_secs": 30,
                "request_id": "req-1",
                "server_id": "srv-1",
                "task_id": "T1",
                "url": "https://user:pass@example.test/a?token=secret#frag",
                "stdout": "must not appear",
                "content": "must not appear",
                "headers": {"authorization": "Bearer secret"},
                "metadata": {"nested": "must not appear"}
            }),
            Some("remote_exec"),
        );
        let evidence = failure.evidence.unwrap_or_default();
        for expected in [
            "path=src/main.rs",
            "paths=a.rs, b.rs",
            "action=exec",
            "exit_code=1",
            "elapsed_ms=22",
            "timeout_secs=30",
            "request_id=req-1",
        ] {
            assert!(
                evidence.contains(expected),
                "missing {expected} in {evidence}"
            );
        }
        assert!(evidence.contains("url=https://example.test/a"));
        for forbidden in [
            "must not appear",
            "Bearer secret",
            "nested",
            "user:pass",
            "token=secret",
        ] {
            assert!(!evidence.contains(forbidden), "leaked {forbidden}");
        }
        assert!(evidence.chars().count() <= MAX_FAILURE_EVIDENCE_CHARS);
    }

    #[test]
    fn secret_shapes_are_redacted_from_message_hint_and_evidence() {
        let failure = ToolFailure::new(
            ToolErrorCode::FETCH_FAILED,
            "request failed with Authorization: Bearer supersecrettoken",
        )
        .with_hint("check token=abc123 before retrying")
        .with_evidence(
            "url=https://user:pass@example.test/path?token=abc#frag; api_key=sk-1234567890",
        );
        let rendered = failure.render();
        let structured = serde_json::to_string(&failure.to_structured()).unwrap();
        assert!(!rendered.contains("supersecrettoken"));
        assert!(!structured.contains("supersecrettoken"));
        assert!(!rendered.contains("abc123"));
        assert!(!rendered.contains("user:pass"));
        assert!(!rendered.contains("sk-1234567890"));
        assert!(rendered.contains("[redacted]"));
        assert!(structured.contains("[redacted]"));
    }

    #[test]
    fn evidence_allowlist_ignores_arbitrary_objects() {
        let failure = ToolFailure::from_structured(
            "failed",
            &json!({
                "code": "INVALID_ARGUMENT",
                "path": ["a.rs", "b.rs"],
                "elapsed_ms": 12,
                "metadata": {"secret": "no"},
                "url": "https://example.test/a?token=secret#frag"
            }),
            None,
        );
        let evidence = failure.evidence.unwrap_or_default();
        assert!(evidence.contains("path=a.rs, b.rs"));
        assert!(evidence.contains("elapsed_ms=12"));
        assert!(evidence.contains("url=https://example.test/a"));
        assert!(!evidence.contains("secret"));
        assert!(!evidence.contains("frag"));
    }

    #[test]
    fn deserialization_collapses_unknown_codes_and_applies_defaults() {
        let failure: ToolFailure =
            serde_json::from_value(json!({"code": "MYSTERY", "message": "bad"})).unwrap();
        assert_eq!(failure.code, ToolErrorCode::TOOL_FAILED);
        assert!(!failure.retryable);
        assert!(!failure.hint.is_empty());
        assert_eq!(failure.tool, None);
    }

    #[test]
    fn public_catalog_matches_the_private_declarations() {
        let entries = catalog();
        assert_eq!(entries.len(), ToolErrorCode::ALL.len());
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            assert_eq!(
                entry.code.as_str(),
                entry.code.as_str().to_ascii_uppercase()
            );
            assert!(!entry.hint.is_empty());
            assert!(seen.insert(entry.code.as_str()));
            assert_eq!(entry.retryable, entry.code.default_retryable());
            assert_eq!(entry.category, entry.code.category());
        }
        assert!(
            entries
                .iter()
                .any(|entry| entry.code == ToolErrorCode::PATCH_CONFLICT)
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.code == ToolErrorCode::STALE_REF && entry.retryable)
        );
    }

    #[test]
    fn bare_error_classification_is_conservative_and_tool_aware() {
        assert_eq!(
            classify_message("read_file", "path does not exist: x"),
            Some(ToolErrorCode::FILE_NOT_FOUND)
        );
        assert_eq!(
            classify_message("git", "fatal: not a git repository"),
            Some(ToolErrorCode::GIT_NOT_REPOSITORY)
        );
        assert_eq!(
            classify_message("shell", "operation not permitted"),
            Some(ToolErrorCode::PERMISSION_DENIED)
        );
        assert_eq!(
            classify_message("process", "wait timed out after 5s"),
            Some(ToolErrorCode::WAIT_TIMEOUT)
        );
        assert_eq!(
            classify_message("fetch_url", "request timed out"),
            Some(ToolErrorCode::FETCH_TIMEOUT)
        );
        assert_eq!(
            classify_message("remote_exec", "ssh connection reset"),
            Some(ToolErrorCode::REMOTE_UNAVAILABLE)
        );
        assert_eq!(
            classify_message("lsp_query", "language server not found in PATH"),
            Some(ToolErrorCode::LSP_UNAVAILABLE)
        );
        assert_eq!(
            classify_message("unknown", "something novel happened"),
            None
        );

        // A classifier match still uses catalog retryability.
        let classified = classify_message("shell", "command timed out").unwrap();
        assert_eq!(classified, ToolErrorCode::TIMEOUT);
        assert!(classified.default_retryable());
        let error = anyhow::anyhow!("path does not exist: src/missing.rs");
        let failure = failure_from_anyhow("read_file", &error);
        assert_eq!(failure.code, ToolErrorCode::FILE_NOT_FOUND);
        assert!(!failure.retryable);
    }

    #[test]
    fn tool_output_coded_failure_keeps_structured_and_provider_text_in_sync() {
        use crate::tools::ToolOutput;
        let output = ToolOutput::error_coded(
            "edit_file",
            ToolErrorCode::PATCH_CONFLICT,
            "old_string was not found",
        )
        .with_failure_evidence("path=src/lib.rs");
        assert!(output.is_error);
        assert!(output.content.starts_with("error[PATCH_CONFLICT]"));
        assert!(output.content.contains("retryable: yes"));
        assert!(output.content.contains("hint:"));
        assert!(output.content.contains("tool: edit_file"));
        let structured = output.structured.unwrap();
        assert_eq!(structured["code"], "PATCH_CONFLICT");
        assert_eq!(structured["retryable"], true);
        assert_eq!(structured["tool"], "edit_file");
        assert_eq!(structured["evidence"], "path=src/lib.rs");
        assert!(structured["hint"].as_str().is_some());
    }

    #[test]
    fn legacy_tool_output_error_becomes_tool_failed_and_is_not_retried() {
        use crate::tools::ToolOutput;
        let output = ToolOutput::error("something went wrong");
        assert!(output.is_error);
        assert!(output.content.starts_with("error[TOOL_FAILED]"));
        let structured = output.structured.unwrap();
        assert_eq!(structured["code"], "TOOL_FAILED");
        assert_eq!(structured["retryable"], false);
        assert!(
            structured["hint"]
                .as_str()
                .unwrap()
                .contains("do not retry")
                || structured["hint"]
                    .as_str()
                    .unwrap()
                    .contains("do not retry an unknown")
        );
    }

    #[test]
    fn normalize_failure_adds_tool_name_and_keeps_command_body() {
        use crate::tools::ToolOutput;
        let mut output = ToolOutput::ok_structured(
            "exit 1\nboom\n",
            json!({"ok": false, "command": "false", "stdout": "boom\n", "exit_code": 1}),
        );
        output.is_error = true;
        output.normalize_failure("shell");
        assert!(output.content.starts_with("error[COMMAND_FAILED]"));
        assert!(output.content.contains("boom"));
        let structured = output.structured.unwrap();
        assert_eq!(structured["code"], "COMMAND_FAILED");
        assert_eq!(structured["exit_code"], 1);
        assert!(structured.get("command").is_none());
        assert!(structured.get("stdout").is_none());
    }

    #[test]
    fn from_tool_error_preserves_downcast_code_and_falls_back_otherwise() {
        use crate::tools::ToolOutput;
        let specific: anyhow::Error = ToolFailure::new(ToolErrorCode::FILE_NOT_FOUND, "missing")
            .with_tool("read_file")
            .into();
        let output = ToolOutput::from_tool_error("read_file", &specific);
        assert_eq!(output.structured.unwrap()["code"], "FILE_NOT_FOUND");

        let generic = anyhow::anyhow!("socket closed");
        let output = ToolOutput::from_tool_error("remote_read", &generic);
        assert_eq!(output.structured.unwrap()["code"], "REMOTE_FAILED");

        let unknown = anyhow::anyhow!("mystery");
        let output = ToolOutput::from_tool_error("some_new_tool", &unknown);
        assert_eq!(output.structured.unwrap()["code"], "TOOL_FAILED");
    }

    #[test]
    fn per_tool_fallback_is_conservative_and_documented() {
        assert_eq!(
            ToolErrorCode::for_tool_fallback("read_file"),
            ToolErrorCode::READ_FAILED
        );
        assert_eq!(
            ToolErrorCode::for_tool_fallback("write_file"),
            ToolErrorCode::WRITE_FAILED
        );
        assert_eq!(
            ToolErrorCode::for_tool_fallback("remote_exec"),
            ToolErrorCode::REMOTE_FAILED
        );
        assert_eq!(
            ToolErrorCode::for_tool_fallback("mcp"),
            ToolErrorCode::MCP_FAILED
        );
        assert_eq!(
            ToolErrorCode::for_tool_fallback("tool_help"),
            ToolErrorCode::TOOL_FAILED
        );
        for tool in [
            "read_file",
            "write_file",
            "shell",
            "process",
            "git",
            "mcp",
            "remote_exec",
            "read_tool_output",
        ] {
            assert!(!ToolErrorCode::for_tool_fallback(tool).default_retryable());
        }
    }

    #[test]
    fn code_detail_and_catalog_rendering_are_bounded_and_grouped() {
        let detail = render_code_detail(ToolErrorCode::STALE_REF);
        assert!(detail.contains("## STALE_REF"));
        assert!(detail.contains("retryable: yes"));
        assert!(detail.contains("Take a fresh snapshot"));
        assert!(detail.contains("static contract"));

        let catalog = render_catalog(20_000);
        assert!(catalog.contains("[file]"));
        assert!(catalog.contains("[remote]"));
        assert!(catalog.contains("PATCH_CONFLICT (retryable)"));
        assert!(catalog.contains("TOOL_FAILED"));
        assert!(catalog.chars().count() <= 20_000);
        let tiny = render_catalog(700);
        assert!(tiny.chars().count() <= 700);
        assert!(tiny.contains("Tool error codes"));
    }

    #[test]
    fn rendered_catalog_names_every_code_and_category() {
        let rendered = render_catalog(20_000);
        for code in ToolErrorCode::ALL {
            assert!(
                rendered.contains(code.as_str()),
                "catalog missing {}",
                code.as_str()
            );
        }
        for category in [
            "general",
            "file",
            "git",
            "shell",
            "process",
            "lsp",
            "code",
            "browser",
            "computer",
            "remote",
            "tool_output",
            "mcp",
            "network",
            "media",
            "data",
            "session",
            "acp",
        ] {
            assert!(
                rendered.contains(&format!("[{category}]")),
                "catalog missing category {category}"
            );
        }
    }

    #[test]
    fn tool_failure_round_trips_through_wire_json() {
        let failure = ToolFailure::new(ToolErrorCode::COMMAND_FAILED, "exit 2")
            .with_tool("shell")
            .with_evidence("exit_code=2")
            .with_action("inspect stderr");
        let encoded = serde_json::to_value(&failure).unwrap();
        assert_eq!(encoded["code"], "COMMAND_FAILED");
        assert_eq!(encoded["retryable"], false);
        assert_eq!(encoded["tool"], "shell");
        assert_eq!(encoded["action"], "inspect stderr");
        let decoded: ToolFailure = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, failure);
    }

    #[test]
    fn code_deserialization_is_stable_and_unknown_is_failed() {
        let known: ToolErrorCode = serde_json::from_value(json!("PATCH_CONFLICT")).unwrap();
        assert_eq!(known, ToolErrorCode::PATCH_CONFLICT);
        let unknown: ToolErrorCode = serde_json::from_value(json!("NEW_CODE")).unwrap();
        assert_eq!(unknown, ToolErrorCode::TOOL_FAILED);
        assert_eq!(
            serde_json::to_value(known).unwrap(),
            json!("PATCH_CONFLICT")
        );
    }

    #[test]
    fn mark_failure_preserves_body_and_sanitizes_structured_payload() {
        use crate::tools::ToolOutput;
        let output = ToolOutput::ok_structured(
            "exit 2\nvisible body\n",
            json!({
                "exit_code": 2,
                "command": "curl -H 'Authorization: Bearer supersecret'",
                "stdout": "visible body",
                "stderr": "diagnostic",
                "warnings": ["stdout capped"]
            }),
        )
        .mark_failure(
            "shell",
            ToolErrorCode::COMMAND_FAILED,
            "command exited with status 2",
        )
        .with_failure_evidence("exit_code=2");
        assert!(output.is_error);
        assert!(output.content.starts_with("error[COMMAND_FAILED]"));
        assert!(output.content.contains("visible body"));
        let structured = output.structured.unwrap();
        assert_eq!(structured["code"], "COMMAND_FAILED");
        assert_eq!(structured["exit_code"], 2);
        assert_eq!(structured["warnings"][0], "stdout capped");
        assert!(structured.get("command").is_none());
        assert!(structured.get("stdout").is_none());
        assert!(structured.get("stderr").is_none());
        let rendered = serde_json::to_string(&structured).unwrap();
        assert!(!rendered.contains("supersecret"));
    }

    #[test]
    fn builder_methods_update_structured_without_duplicating_prefix() {
        use crate::tools::ToolOutput;
        let output = ToolOutput::error_coded(
            "remote_write",
            ToolErrorCode::REMOTE_WRITE_FAILED,
            "write rejected",
        )
        .with_failure_hint("Re-read the remote file and verify the hash.")
        .with_failure_retryable(false)
        .with_failure_action("retry after remote_read");
        assert_eq!(
            output.content.matches("error[REMOTE_WRITE_FAILED]").count(),
            1
        );
        assert!(output.content.contains("retryable: no"));
        assert!(output.content.contains("hint: Re-read the remote file"));
        assert!(output.content.contains("action: retry after remote_read"));
        let structured = output.structured.unwrap();
        assert_eq!(structured["retryable"], false);
        assert_eq!(
            structured["hint"],
            "Re-read the remote file and verify the hash."
        );
        assert_eq!(structured["action"], "retry after remote_read");
    }

    #[test]
    fn every_code_round_trips_through_failure_and_wire_json() {
        for code in ToolErrorCode::ALL {
            let failure = ToolFailure::new(*code, format!("failure for {}", code.as_str()))
                .with_tool("sample_tool")
                .with_evidence("path=src/lib.rs")
                .with_action("inspect");
            let encoded = serde_json::to_value(&failure).unwrap();
            assert_eq!(encoded["code"], code.as_str());
            assert_eq!(encoded["retryable"], code.default_retryable());
            assert_eq!(encoded["tool"], "sample_tool");
            assert_eq!(encoded["action"], "inspect");
            let decoded: ToolFailure = serde_json::from_value(encoded).unwrap();
            assert_eq!(&decoded, &failure);
            let structured = failure.to_structured();
            assert_eq!(structured["code"], code.as_str());
            assert_eq!(structured["message"], failure.message);
            assert!(!failure.render().is_empty());
            assert!(
                failure
                    .render()
                    .starts_with(&format!("error[{}]", code.as_str()))
            );
        }
    }

    #[test]
    fn fallback_cover_every_required_tool_surface() {
        for (tool, expected) in [
            ("read_file", ToolErrorCode::READ_FAILED),
            ("list_dir", ToolErrorCode::READ_FAILED),
            ("grep", ToolErrorCode::READ_FAILED),
            ("find_files", ToolErrorCode::READ_FAILED),
            ("file_info", ToolErrorCode::READ_FAILED),
            ("read_files", ToolErrorCode::READ_FAILED),
            ("write_file", ToolErrorCode::WRITE_FAILED),
            ("edit_file", ToolErrorCode::WRITE_FAILED),
            ("apply_patch", ToolErrorCode::WRITE_FAILED),
            ("file_manage", ToolErrorCode::WRITE_FAILED),
            ("git", ToolErrorCode::GIT_FAILED),
            ("shell", ToolErrorCode::COMMAND_FAILED),
            ("run_project_task", ToolErrorCode::COMMAND_FAILED),
            ("process", ToolErrorCode::PROCESS_FAILED),
            ("wait_for", ToolErrorCode::PROCESS_FAILED),
            ("lsp_query", ToolErrorCode::LSP_ERROR),
            ("browser", ToolErrorCode::BROWSER_ERROR),
            ("computer", ToolErrorCode::COMPUTER_ERROR),
            ("mcp", ToolErrorCode::MCP_FAILED),
            ("fetch_url", ToolErrorCode::FETCH_FAILED),
            ("web_search", ToolErrorCode::FETCH_FAILED),
            ("local_http", ToolErrorCode::FETCH_FAILED),
            ("read_tool_output", ToolErrorCode::TOOL_OUTPUT_FAILED),
            ("send_message", ToolErrorCode::MAILBOX_FAILED),
            ("read_mailbox", ToolErrorCode::MAILBOX_FAILED),
            ("remember", ToolErrorCode::MEMORY_FAILED),
            ("recall", ToolErrorCode::MEMORY_FAILED),
            ("search_sources", ToolErrorCode::SOURCE_FAILED),
            ("read_source", ToolErrorCode::SOURCE_FAILED),
            ("list_sources", ToolErrorCode::SOURCE_FAILED),
            ("remote_read", ToolErrorCode::REMOTE_FAILED),
            ("remote_write", ToolErrorCode::REMOTE_FAILED),
            ("remote_list", ToolErrorCode::REMOTE_FAILED),
            ("remote_exec", ToolErrorCode::REMOTE_FAILED),
            ("remote_git", ToolErrorCode::REMOTE_FAILED),
            ("remote_worktree", ToolErrorCode::REMOTE_FAILED),
            ("some_future_tool", ToolErrorCode::TOOL_FAILED),
        ] {
            assert_eq!(
                ToolErrorCode::for_tool_fallback(tool),
                expected,
                "wrong fallback for {tool}"
            );
        }
    }

    #[test]
    fn sanitize_removes_control_characters_and_empty_required_fields() {
        let failure = ToolFailure::new(
            ToolErrorCode::TOOL_FAILED,
            "bad\u{0}message\nwith\tcontrols\u{7}",
        )
        .with_hint("")
        .with_tool("");
        assert!(!failure.message.contains('\u{0}'));
        assert!(!failure.message.contains('\u{7}'));
        assert!(failure.message.contains('\n'));
        assert!(failure.message.contains('\t'));
        assert!(!failure.hint.is_empty());
        assert_eq!(failure.tool.as_deref(), Some("unknown"));
        let encoded = serde_json::to_string(&failure).unwrap();
        assert!(!encoded.contains("\\u{0}"));
    }

    #[test]
    fn invalid_structured_error_without_code_still_gets_a_contract() {
        let output = crate::tools::ToolOutput::error_structured(
            "something failed",
            json!({"path": "src/lib.rs", "retryable": true}),
        );
        assert!(output.content.starts_with("error[TOOL_FAILED]"));
        let structured = output.structured.as_ref().unwrap();
        assert_eq!(structured["code"], "TOOL_FAILED");
        // A caller cannot make an unknown failure retryable by hand.
        assert_eq!(structured["retryable"], false);
        let mut output = output;
        output.normalize_failure("future_tool");
        let structured = output.structured.unwrap();
        assert_eq!(structured["tool"], "future_tool");
        assert_eq!(structured["retryable"], false);
        assert!(structured["hint"].as_str().is_some());
    }

    #[test]
    fn action_is_bounded_but_preserved_in_structured_and_text() {
        let failure = ToolFailure::new(ToolErrorCode::INVALID_ACTION, "bad action")
            .with_action("choose one of read, write, list");
        assert!(
            failure
                .render()
                .contains("action: choose one of read, write, list")
        );
        assert_eq!(
            failure.to_structured()["action"],
            "choose one of read, write, list"
        );
    }
}
