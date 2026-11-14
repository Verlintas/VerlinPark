//! Tool bundle partition, freeze, and canonical schema serialization.
//!
//! The tool schema prefix is the largest part of an agent's stable prompt. A
//! single role-wide list is simple, but it makes every trivial task pay for
//! orchestration, GUI, media, remote, and document tools that it cannot use.
//! This module partitions the advertised schemas into a small number of named
//! bundles and freezes the chosen one for the whole run.
//!
//! The bundle is an **advertisement and cache-prefix optimization**, not an
//! authorization boundary. Role/profile policy is still enforced at dispatch
//! in `agent.rs`; a tool that is absent from the bundle but present in the
//! role policy can be recovered through `tool_search` and executed manually.
//! That keeps the reduction safe while preventing per-turn schema churn.
//!
//! Canonicalization rules:
//!
//! - tools are sorted by name;
//! - the serialized fields are `name`, `description`, `parameters` in that
//!   order;
//! - parameter objects are recursively key-sorted;
//! - no session, worktree, agent, timestamp, or other run data is included.
//!
//! `bundle_hash` covers the selected built-in schemas and the selection
//! identity. `tools_hash` covers the final advertised list including dynamic
//! MCP schemas. Two runs of the same bundle with the same available schemas
//! therefore produce identical bytes and hashes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use vp_providers::ToolSchema;

/// Version of the bundle partition and canonical schema format. It is folded
/// into `bundle_hash` so an intentional matrix change creates a new prefix.
pub const TOOL_BUNDLE_VERSION: u32 = 1;

/// Default Leader cap, down from the previous role-wide list (72 tools).
pub const LEADER_TOOL_LIMIT: usize = 51;
/// Default coding Worker/Labour cap, down from the previous list (64 tools).
pub const CODING_TOOL_LIMIT: usize = 40;
/// Minimal cap for a trivial one-file task.
pub const MINIMAL_TOOL_LIMIT: usize = 12;
/// Dynamic MCP schemas are appended with at most this many entries. This
/// matches the runtime collector's existing bound; the bundle layer never
/// widens it.
pub const MAX_BUNDLE_MCP_TOOLS: usize = 32;

/// Role/manager display names accepted by [`select_bundle`].
pub const ROLE_LEADER: &str = "leader";
pub const ROLE_MANAGER: &str = "manager";
pub const ROLE_WORKER: &str = "worker";
pub const ROLE_VERIFIER: &str = "verifier";
pub const ROLE_LABOUR: &str = "labour";
pub const ROLE_SCOUT: &str = "scout";

/// Profile display names accepted by [`select_bundle`].
pub const PROFILE_CODING: &str = "coding";
pub const PROFILE_RESEARCH: &str = "research";
pub const PROFILE_VERIFICATION: &str = "verification";
pub const PROFILE_GENERIC: &str = "generic";

/// A named tool partition. `Full` is the backward-compatible fallback and is
/// not intended for the default path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolBundleKind {
    Leader,
    Coding,
    Research,
    Verification,
    Minimal,
    Full,
}

impl ToolBundleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Coding => "coding",
            Self::Research => "research",
            Self::Verification => "verification",
            Self::Minimal => "minimal",
            Self::Full => "full",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "leader" => Some(Self::Leader),
            "coding" => Some(Self::Coding),
            "research" => Some(Self::Research),
            "verification" => Some(Self::Verification),
            "minimal" => Some(Self::Minimal),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// Size target for the built-in schema set. Dynamic MCP schemas are
    /// reported separately and never silently counted as headroom.
    pub fn target_limit(self) -> Option<usize> {
        match self {
            Self::Leader => Some(LEADER_TOOL_LIMIT),
            Self::Coding => Some(CODING_TOOL_LIMIT),
            Self::Minimal => Some(MINIMAL_TOOL_LIMIT),
            Self::Research | Self::Verification | Self::Full => None,
        }
    }

    /// True for the explicit partitions. `Full` is the legacy fallback.
    pub fn is_partitioned(self) -> bool {
        !matches!(self, Self::Full)
    }

    /// Whether this bundle may append dynamic MCP schemas. Minimal deliberately
    /// stays tiny: a one-file task must not inherit a 32-tool MCP prefix.
    pub fn allows_dynamic_mcp(self) -> bool {
        !matches!(self, Self::Minimal)
    }
}

impl std::fmt::Display for ToolBundleKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Coarse category used by the read-only CLI surface and by tests. Categories
/// are descriptive only; they never affect permission decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Files,
    Search,
    Shell,
    Process,
    Git,
    Code,
    Diagnostics,
    Project,
    External,
    Memory,
    Source,
    Mailbox,
    Artifact,
    Question,
    Team,
    Orchestration,
    Delegation,
    Task,
    Utility,
    Verifier,
    Mcp,
    Gui,
    Remote,
    Media,
    Archive,
    Document,
    Data,
}

impl ToolCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Search => "search",
            Self::Shell => "shell",
            Self::Process => "process",
            Self::Git => "git",
            Self::Code => "code",
            Self::Diagnostics => "diagnostics",
            Self::Project => "project",
            Self::External => "external",
            Self::Memory => "memory",
            Self::Source => "source",
            Self::Mailbox => "mailbox",
            Self::Artifact => "artifact",
            Self::Question => "question",
            Self::Team => "team",
            Self::Orchestration => "orchestration",
            Self::Delegation => "delegation",
            Self::Task => "task",
            Self::Utility => "utility",
            Self::Verifier => "verifier",
            Self::Mcp => "mcp",
            Self::Gui => "gui",
            Self::Remote => "remote",
            Self::Media => "media",
            Self::Archive => "archive",
            Self::Document => "document",
            Self::Data => "data",
        }
    }
}

/// Static metadata for one tool name. `workspace_write` means the tool can
/// modify the agent worktree (or start a process that can). It is used only to
/// audit the bundle matrix; dispatch policy remains authoritative.
#[derive(Debug, Clone, Copy)]
pub struct BundleToolSpec {
    pub name: &'static str,
    pub category: ToolCategory,
    pub workspace_write: bool,
    pub delegation: bool,
}

impl BundleToolSpec {
    const fn new(
        name: &'static str,
        category: ToolCategory,
        workspace_write: bool,
        delegation: bool,
    ) -> Self {
        Self {
            name,
            category,
            workspace_write,
            delegation,
        }
    }
}

/// Complete catalog of tools that appear in any explicit bundle. Tools that
/// are only reachable through the `full` fallback still get metadata so the
/// audit can distinguish "deliberately unbundled" from "typo in a constant".
pub const BUNDLE_TOOL_CATALOG: &[BundleToolSpec] = &[
    // Files and search.
    BundleToolSpec::new("read_file", ToolCategory::Files, false, false),
    BundleToolSpec::new("list_dir", ToolCategory::Files, false, false),
    BundleToolSpec::new("grep", ToolCategory::Search, false, false),
    BundleToolSpec::new("find_files", ToolCategory::Search, false, false),
    BundleToolSpec::new("file_info", ToolCategory::Files, false, false),
    BundleToolSpec::new("read_files", ToolCategory::Files, false, false),
    BundleToolSpec::new("edit_file", ToolCategory::Files, true, false),
    BundleToolSpec::new("write_file", ToolCategory::Files, true, false),
    BundleToolSpec::new("apply_patch", ToolCategory::Files, true, false),
    BundleToolSpec::new("file_manage", ToolCategory::Files, true, false),
    // Shell and process.
    BundleToolSpec::new("shell", ToolCategory::Shell, true, false),
    BundleToolSpec::new("process", ToolCategory::Process, true, false),
    BundleToolSpec::new("wait_for", ToolCategory::Process, false, false),
    BundleToolSpec::new("local_http", ToolCategory::External, false, false),
    // Git and code intelligence.
    BundleToolSpec::new("git", ToolCategory::Git, false, false),
    BundleToolSpec::new("repo_map", ToolCategory::Code, false, false),
    BundleToolSpec::new("find_symbol", ToolCategory::Code, false, false),
    BundleToolSpec::new("find_references", ToolCategory::Code, false, false),
    BundleToolSpec::new("code_diagnostics", ToolCategory::Diagnostics, false, false),
    BundleToolSpec::new("affected_tests", ToolCategory::Diagnostics, false, false),
    BundleToolSpec::new("dependency_graph", ToolCategory::Code, false, false),
    BundleToolSpec::new("build_context", ToolCategory::Code, false, false),
    BundleToolSpec::new("lsp_query", ToolCategory::Diagnostics, false, false),
    // Project tasks.
    BundleToolSpec::new("project_profile", ToolCategory::Project, false, false),
    BundleToolSpec::new("run_project_task", ToolCategory::Project, true, false),
    // External network reads.
    BundleToolSpec::new("fetch_url", ToolCategory::External, false, false),
    BundleToolSpec::new("web_search", ToolCategory::External, false, false),
    // Memory and source retrieval.
    BundleToolSpec::new("recall", ToolCategory::Memory, false, false),
    BundleToolSpec::new("remember", ToolCategory::Memory, false, false),
    BundleToolSpec::new("list_sources", ToolCategory::Source, false, false),
    BundleToolSpec::new("search_sources", ToolCategory::Source, false, false),
    BundleToolSpec::new("read_source", ToolCategory::Source, false, false),
    // Mailbox and artifacts.
    BundleToolSpec::new("send_message", ToolCategory::Mailbox, false, false),
    BundleToolSpec::new("read_mailbox", ToolCategory::Mailbox, false, false),
    BundleToolSpec::new("publish_artifact", ToolCategory::Artifact, false, false),
    BundleToolSpec::new(
        "publish_artifact_file",
        ToolCategory::Artifact,
        false,
        false,
    ),
    BundleToolSpec::new("list_artifacts", ToolCategory::Artifact, false, false),
    BundleToolSpec::new("read_artifact", ToolCategory::Artifact, false, false),
    BundleToolSpec::new("archive_list", ToolCategory::Archive, false, false),
    BundleToolSpec::new("archive_extract", ToolCategory::Archive, false, false),
    BundleToolSpec::new("media_info", ToolCategory::Media, false, false),
    BundleToolSpec::new("media_transcode", ToolCategory::Media, false, false),
    // Questions, team coordination, and skills.
    BundleToolSpec::new("ask_user", ToolCategory::Question, false, false),
    BundleToolSpec::new("coordinate", ToolCategory::Team, false, false),
    BundleToolSpec::new("team_status", ToolCategory::Team, false, false),
    BundleToolSpec::new("use_skill", ToolCategory::Utility, false, false),
    BundleToolSpec::new("mcp", ToolCategory::Mcp, false, false),
    // Orchestration and delegation.
    BundleToolSpec::new("todo_write", ToolCategory::Orchestration, false, false),
    BundleToolSpec::new("delegate", ToolCategory::Delegation, false, true),
    BundleToolSpec::new("delegate_many", ToolCategory::Delegation, false, true),
    BundleToolSpec::new("delegate_acp", ToolCategory::Delegation, false, true),
    BundleToolSpec::new(
        "submit_team_plan",
        ToolCategory::Orchestration,
        false,
        false,
    ),
    BundleToolSpec::new("plan_fanout", ToolCategory::Orchestration, false, false),
    BundleToolSpec::new("run_team_plan", ToolCategory::Orchestration, false, false),
    BundleToolSpec::new(
        "deliberate_team_plan",
        ToolCategory::Orchestration,
        false,
        false,
    ),
    BundleToolSpec::new("scout", ToolCategory::Orchestration, false, true),
    // Task inspection and utility.
    BundleToolSpec::new("budget_status", ToolCategory::Utility, false, false),
    BundleToolSpec::new("task_detail", ToolCategory::Task, false, false),
    BundleToolSpec::new("task_control", ToolCategory::Task, false, false),
    BundleToolSpec::new("wait_for_gate", ToolCategory::Orchestration, false, false),
    BundleToolSpec::new("read_tool_output", ToolCategory::Utility, false, false),
    BundleToolSpec::new("tool_help", ToolCategory::Utility, false, false),
    BundleToolSpec::new("tool_search", ToolCategory::Utility, false, false),
    BundleToolSpec::new("run_script", ToolCategory::Utility, true, false),
    BundleToolSpec::new("submit_verdict", ToolCategory::Verifier, false, false),
    // GUI surfaces.
    BundleToolSpec::new("browser", ToolCategory::Gui, false, false),
    BundleToolSpec::new("computer", ToolCategory::Gui, false, false),
    // Remote workspace tools are full-bundle only.
    BundleToolSpec::new("remote_read", ToolCategory::Remote, false, false),
    BundleToolSpec::new("remote_write", ToolCategory::Remote, true, false),
    BundleToolSpec::new("remote_list", ToolCategory::Remote, false, false),
    BundleToolSpec::new("remote_exec", ToolCategory::Remote, true, false),
    BundleToolSpec::new("remote_git", ToolCategory::Remote, false, false),
    BundleToolSpec::new("remote_worktree", ToolCategory::Remote, true, false),
    // Document and structured-data readers.
    BundleToolSpec::new("read_pdf", ToolCategory::Document, false, false),
    BundleToolSpec::new("read_image", ToolCategory::Document, false, false),
    BundleToolSpec::new("read_document", ToolCategory::Document, false, false),
    BundleToolSpec::new("data_query", ToolCategory::Data, false, false),
];

/// A named bundle definition. The exact list is explicit; it is never derived
/// by subtracting a few names from the full registry.
#[derive(Debug, Clone, Copy)]
pub struct ToolBundleDefinition {
    pub kind: ToolBundleKind,
    pub title: &'static str,
    pub summary: &'static str,
    pub target_limit: Option<usize>,
    pub tools: &'static [&'static str],
}

/// The Leader bundle: orchestration, planning, delegation, files, shell,
/// Git/code intelligence, memory, artifacts, questions, and team surface.
/// Browser/computer are added only for a coding profile.
pub const LEADER_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "grep",
    "edit_file",
    "write_file",
    "find_files",
    "file_info",
    "read_files",
    "apply_patch",
    "shell",
    "process",
    "git",
    "repo_map",
    "find_symbol",
    "find_references",
    "code_diagnostics",
    "build_context",
    "lsp_query",
    "fetch_url",
    "web_search",
    "recall",
    "remember",
    "list_sources",
    "search_sources",
    "read_source",
    "send_message",
    "read_mailbox",
    "publish_artifact",
    "read_artifact",
    "ask_user",
    "coordinate",
    "team_status",
    "use_skill",
    "mcp",
    "todo_write",
    "delegate",
    "delegate_acp",
    "submit_team_plan",
    "plan_fanout",
    "run_team_plan",
    "deliberate_team_plan",
    "scout",
    "task_control",
    "wait_for_gate",
    "budget_status",
    "read_tool_output",
    "tool_help",
    "tool_search",
    "run_script",
];

/// The coding bundle. It deliberately excludes project-level orchestration,
/// planning, task control, research sources, and the GUI surfaces unless the
/// profile is coding. `delegate` is appended only for a Worker, never Labour.
pub const CODING_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "grep",
    "edit_file",
    "write_file",
    "find_files",
    "file_info",
    "read_files",
    "apply_patch",
    "shell",
    "process",
    "wait_for",
    "git",
    "repo_map",
    "find_symbol",
    "find_references",
    "code_diagnostics",
    "dependency_graph",
    "build_context",
    "lsp_query",
    "run_project_task",
    "fetch_url",
    "recall",
    "remember",
    "send_message",
    "read_mailbox",
    "publish_artifact",
    "read_artifact",
    "coordinate",
    "use_skill",
    "mcp",
    "read_tool_output",
    "wait_for_gate",
    "tool_help",
    "tool_search",
    "run_script",
];

/// The research bundle. Read, search, fetch, web, source, memory, code map, and
/// Git read operations only. No workspace writes, shell, process, or delegation.
pub const RESEARCH_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "grep",
    "find_files",
    "file_info",
    "read_files",
    "git",
    "repo_map",
    "find_symbol",
    "find_references",
    "dependency_graph",
    "build_context",
    "affected_tests",
    "fetch_url",
    "web_search",
    "list_sources",
    "search_sources",
    "read_source",
    "recall",
    "remember",
    "read_pdf",
    "read_image",
    "read_document",
    "archive_list",
    "media_info",
    "data_query",
    "use_skill",
    "read_tool_output",
    "wait_for_gate",
    "tool_help",
    "tool_search",
    "team_status",
    "send_message",
    "read_mailbox",
];

/// The verification bundle. Read/search/Git/shell/process/LSP/diagnostics and
/// the verdict surface. No write/edit/commit/merge/delegation schemas.
pub const VERIFICATION_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "grep",
    "find_files",
    "file_info",
    "read_files",
    "git",
    "repo_map",
    "find_symbol",
    "find_references",
    "dependency_graph",
    "build_context",
    "lsp_query",
    "code_diagnostics",
    "affected_tests",
    "shell",
    "process",
    "wait_for",
    "local_http",
    "run_project_task",
    "project_profile",
    "read_pdf",
    "read_image",
    "read_document",
    "archive_list",
    "media_info",
    "data_query",
    "read_tool_output",
    "wait_for_gate",
    "tool_help",
    "tool_search",
    "send_message",
    "read_mailbox",
    "team_status",
    "recall",
    "mcp",
    "budget_status",
    "task_detail",
    "submit_verdict",
];

/// The minimal bundle for a trivial one-file task. Six schemas, not sixty.
pub const MINIMAL_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "write_file",
    "edit_file",
    "shell",
    "read_tool_output",
];

pub const TOOL_BUNDLE_DEFINITIONS: &[ToolBundleDefinition] = &[
    ToolBundleDefinition {
        kind: ToolBundleKind::Leader,
        title: "Leader",
        summary: "Orchestration, planning, delegation, workspace, code, memory, artifacts, and team coordination.",
        target_limit: Some(LEADER_TOOL_LIMIT),
        tools: LEADER_TOOLS,
    },
    ToolBundleDefinition {
        kind: ToolBundleKind::Coding,
        title: "Coding",
        summary: "Workspace editing, shell/process, Git, code intelligence, tests, and coordination.",
        target_limit: Some(CODING_TOOL_LIMIT),
        tools: CODING_TOOLS,
    },
    ToolBundleDefinition {
        kind: ToolBundleKind::Research,
        title: "Research",
        summary: "Read-only exploration, web/source retrieval, memory, and code mapping.",
        target_limit: None,
        tools: RESEARCH_TOOLS,
    },
    ToolBundleDefinition {
        kind: ToolBundleKind::Verification,
        title: "Verification",
        summary: "Read-only inspection plus shell/tests, diagnostics, and the verdict surface.",
        target_limit: None,
        tools: VERIFICATION_TOOLS,
    },
    ToolBundleDefinition {
        kind: ToolBundleKind::Minimal,
        title: "Minimal",
        summary: "Trivial one-file read/write/shell task.",
        target_limit: Some(MINIMAL_TOOL_LIMIT),
        tools: MINIMAL_TOOLS,
    },
    ToolBundleDefinition {
        kind: ToolBundleKind::Full,
        title: "Full",
        summary: "Backward-compatible fallback when no task kind is known.",
        target_limit: None,
        tools: &[],
    },
];

/// Look up a static definition.
pub fn definition(kind: ToolBundleKind) -> &'static ToolBundleDefinition {
    TOOL_BUNDLE_DEFINITIONS
        .iter()
        .find(|definition| definition.kind == kind)
        .unwrap_or(&TOOL_BUNDLE_DEFINITIONS[TOOL_BUNDLE_DEFINITIONS.len() - 1])
}

/// Metadata for one tool name, or `None` when it is not in the catalog. A
/// missing entry means a tool is full-bundle-only; it is never a reason to
/// admit a name into a named bundle.
pub fn catalog_entry(name: &str) -> Option<&'static BundleToolSpec> {
    BUNDLE_TOOL_CATALOG.iter().find(|entry| entry.name == name)
}

/// Why a bundle was selected. The reason is persisted in prompt telemetry, so
/// operators can tell an intentional partition from a fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBundleSelection {
    pub kind: ToolBundleKind,
    pub role: String,
    pub profile: String,
    pub task_kind: Option<String>,
    pub reason: &'static str,
    /// True when the full legacy bundle was selected because no deterministic
    /// named bundle applied.
    pub fallback: bool,
}

impl ToolBundleSelection {
    pub fn new(
        kind: ToolBundleKind,
        role: impl Into<String>,
        profile: impl Into<String>,
        task_kind: Option<String>,
        reason: &'static str,
        fallback: bool,
    ) -> Self {
        Self {
            kind,
            role: role.into(),
            profile: profile.into(),
            task_kind,
            reason,
            fallback,
        }
    }

    pub fn payload(&self) -> Value {
        serde_json::json!({
            "bundle": self.kind.as_str(),
            "role": self.role,
            "profile": self.profile,
            "task_kind": self.task_kind,
            "reason": self.reason,
            "fallback": self.fallback,
        })
    }
}

/// Map a plan task `kind` to a bundle deterministically. The mapping is a
/// deliberate allowlist: unknown kinds return `None` and the caller falls back
/// to `Full` for backward compatibility.
pub fn task_kind_bundle(task_kind: &str) -> Option<ToolBundleKind> {
    let normalized = task_kind
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_");
    match normalized.as_str() {
        "implementation" | "implement" | "implementing" | "coding" | "code" | "edit" | "fix"
        | "bugfix" | "bug_fix" | "refactor" | "feature" | "test" | "testing" | "build"
        | "integration" | "integrate" => Some(ToolBundleKind::Coding),
        "verification" | "verify" | "review" | "audit" | "validate" | "validation" | "check"
        | "quality" | "quality_gate" => Some(ToolBundleKind::Verification),
        "research" | "investigate" | "investigation" | "discovery" | "explore" | "exploration"
        | "spike" | "analysis" | "analyze" | "design" | "docs" | "documentation" => {
            Some(ToolBundleKind::Research)
        }
        "minimal" | "trivial" | "typo" | "format" | "formatting" | "rename" | "comment"
        | "comments" | "lint" | "one_file" | "onefile" | "small" | "chore" => {
            Some(ToolBundleKind::Minimal)
        }
        _ => None,
    }
}

/// Choose the bundle for one run. Role decides the surface family; task kind
/// only partitions Worker/Labour work. A Leader remains Leader for every task,
/// and read-only roles never inherit a writable bundle.
pub fn select_bundle(role: &str, profile: &str, task_kind: Option<&str>) -> ToolBundleSelection {
    let role = role.trim().to_ascii_lowercase();
    let profile = profile.trim().to_ascii_lowercase();
    let task_kind = task_kind
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .map(|kind| kind.to_string());
    match role.as_str() {
        ROLE_LEADER | ROLE_MANAGER => ToolBundleSelection::new(
            ToolBundleKind::Leader,
            role,
            profile,
            task_kind,
            "role_leader",
            false,
        ),
        ROLE_VERIFIER => ToolBundleSelection::new(
            ToolBundleKind::Verification,
            role,
            profile,
            task_kind,
            "role_verifier",
            false,
        ),
        ROLE_SCOUT => ToolBundleSelection::new(
            ToolBundleKind::Research,
            role,
            profile,
            task_kind,
            "role_scout_read_only",
            false,
        ),
        ROLE_WORKER | ROLE_LABOUR => match task_kind.as_deref() {
            Some(kind) => match task_kind_bundle(kind) {
                Some(bundle) => ToolBundleSelection::new(
                    bundle,
                    role,
                    profile,
                    Some(kind.to_string()),
                    "task_kind",
                    false,
                ),
                None => ToolBundleSelection::new(
                    ToolBundleKind::Full,
                    role,
                    profile,
                    Some(kind.to_string()),
                    "task_kind_unknown_full",
                    true,
                ),
            },
            None => ToolBundleSelection::new(
                ToolBundleKind::Full,
                role,
                profile,
                None,
                "no_task_kind_full",
                true,
            ),
        },
        _ => ToolBundleSelection::new(
            ToolBundleKind::Full,
            role,
            profile,
            task_kind,
            "unknown_role_full",
            true,
        ),
    }
}

/// The exact advertised name list for a bundle, role, and profile before it is
/// intersected with the schemas the runtime actually built. Output is sorted so
/// membership is reproducible. `Full` returns an empty list: its membership is
/// "everything currently available", which the freeze step resolves.
pub fn bundle_tool_names(kind: ToolBundleKind, role: &str, profile: &str) -> Vec<&'static str> {
    if kind == ToolBundleKind::Full {
        return Vec::new();
    }
    let role = role.trim().to_ascii_lowercase();
    let profile = profile.trim().to_ascii_lowercase();
    let mut names = definition(kind).tools.to_vec();
    match kind {
        ToolBundleKind::Leader => {
            if role == ROLE_MANAGER {
                // Managers are read-only for workspace tools but own the
                // delegation/coordination surface: Worker batches, ACP
                // delegation, Scout, and blocker/task control.
                names.push("delegate_many");
            }
            if profile == PROFILE_CODING {
                names.push("browser");
                names.push("computer");
            }
        }
        ToolBundleKind::Coding => {
            // A Worker may delegate Labour; Labour itself is a leaf. This is
            // the only role-conditional delegation in the coding bundle.
            if role == ROLE_WORKER {
                names.push("delegate");
            }
            if profile == PROFILE_CODING {
                names.push("browser");
                names.push("computer");
            }
            if role != ROLE_WORKER {
                names.retain(|name| *name != "wait_for_gate");
            }
        }
        ToolBundleKind::Research => {
            // Scout is the only research-family role allowed to inspect
            // compiler diagnostics; a research Worker has no such policy.
            if role == ROLE_SCOUT {
                names.push("code_diagnostics");
                names.retain(|name| *name != "wait_for_gate");
            } else if role != ROLE_WORKER {
                names.retain(|name| *name != "wait_for_gate");
            }
        }
        ToolBundleKind::Verification => {
            if role != ROLE_WORKER {
                names.retain(|name| *name != "wait_for_gate");
            }
        }
        ToolBundleKind::Minimal | ToolBundleKind::Full => {}
    }
    // Read-only roles never even request write, shell, process, or delegation
    // schemas. Role policy remains the enforcement point, but pruning the
    // request here keeps `omitted` diagnostics and the read-only CLI view
    // honest instead of listing tools the role can never execute.
    names.retain(|name| !forbidden_for_read_only_role(&role, name));
    names.sort_unstable();
    names.dedup();
    names
}

fn forbidden_for_read_only_role(role: &str, name: &str) -> bool {
    let Some(spec) = catalog_entry(name) else {
        return false;
    };
    match role {
        ROLE_MANAGER => {
            // Managers own authority operations (Worker/ACP delegation,
            // delegation batches, Scout, task control) but never workspace
            // writes, shell, process, or GUI execution.
            spec.workspace_write || matches!(name, "shell" | "process" | "browser" | "computer")
        }
        ROLE_SCOUT => {
            spec.workspace_write
                || spec.delegation
                || matches!(
                    name,
                    "shell" | "process" | "browser" | "computer" | "use_skill"
                )
        }
        ROLE_VERIFIER => {
            spec.delegation
                || matches!(
                    name,
                    "write_file"
                        | "edit_file"
                        | "apply_patch"
                        | "file_manage"
                        | "archive_extract"
                        | "media_transcode"
                        | "publish_artifact_file"
                        | "remember"
                        | "todo_write"
                        | "submit_team_plan"
                        | "run_team_plan"
                        | "deliberate_team_plan"
                        | "delegate_many"
                        | "delegate_acp"
                )
        }
        _ => false,
    }
}

/// Metadata for every tool an explicit bundle would request. Keeps the CLI
/// surface readable without shipping full schemas.
pub fn bundle_tool_specs(
    kind: ToolBundleKind,
    role: &str,
    profile: &str,
) -> Vec<&'static BundleToolSpec> {
    bundle_tool_names(kind, role, profile)
        .into_iter()
        .filter_map(catalog_entry)
        .collect()
}

/// Per-category summary used by the read-only CLI and protocol surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBundleCategoryCount {
    pub category: &'static str,
    pub tools: usize,
    pub schema_bytes: u64,
}

/// A warning produced while freezing a bundle. Warnings are diagnostic only and
/// never mutate permissions or schemas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBundleWarning {
    pub code: &'static str,
    pub message: String,
}

impl ToolBundleWarning {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn payload(&self) -> Value {
        serde_json::json!({"code": self.code, "message": self.message})
    }
}

/// The frozen, canonical tool prefix for one run.
#[derive(Debug, Clone)]
pub struct FrozenToolBundle {
    pub selection: ToolBundleSelection,
    /// Built-in schemas in advertised order (sorted by name).
    pub builtin_tools: Vec<ToolSchema>,
    /// Dynamic MCP schemas appended once, sorted by name.
    pub mcp_tools: Vec<ToolSchema>,
    /// Final advertised list: built-ins followed by MCP tools, both sorted and
    /// deduplicated. Provider requests and hashes use this order.
    pub tools: Vec<ToolSchema>,
    /// Sorted unique names requested by the named bundle before resolution.
    pub requested: Vec<String>,
    /// Requested names that the runtime did not produce (for example a
    /// delegation schema withheld by `allow_delegation`).
    pub omitted: Vec<String>,
    pub warnings: Vec<ToolBundleWarning>,
    pub bundle_hash: String,
    pub tools_hash: String,
    pub target_limit: Option<usize>,
}

impl FrozenToolBundle {
    pub fn kind(&self) -> ToolBundleKind {
        self.selection.kind
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|tool| tool.name.clone()).collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name == name)
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    pub fn builtin_count(&self) -> usize {
        self.builtin_tools.len()
    }

    pub fn mcp_count(&self) -> usize {
        self.mcp_tools.len()
    }

    pub fn within_target(&self) -> Option<bool> {
        self.target_limit
            .map(|limit| self.builtin_tools.len() <= limit)
    }

    /// Exact canonical schema bytes for the final advertised list. This is the
    /// provider-facing prefix size before a specific adapter applies its own
    /// framing.
    pub fn schema_bytes(&self) -> u64 {
        canonical_tools_json(&self.tools).len() as u64
    }

    /// Character/4 schema-token estimate. Diagnostic only; context limits use
    /// the agent-loop estimator.
    pub fn estimated_tokens(&self) -> u64 {
        self.schema_bytes() / 4
    }

    /// Category for an advertised tool. Dynamic MCP schemas are labeled `mcp`
    /// even though they are not in the static catalog.
    pub fn category_for(&self, name: &str) -> &'static str {
        if self.mcp_tools.iter().any(|tool| tool.name == name) {
            "mcp"
        } else {
            catalog_entry(name)
                .map(|entry| entry.category.as_str())
                .unwrap_or("unbundled")
        }
    }

    /// Deterministic category histogram. Categories are sorted by name; tool
    /// counts and canonical schema bytes sum to the advertised list.
    pub fn category_counts(&self) -> Vec<ToolBundleCategoryCount> {
        let mut counts: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();
        for tool in &self.tools {
            let category = self.category_for(&tool.name);
            let entry = counts.entry(category).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += canonical_tool_json(tool).len() as u64;
        }
        counts
            .into_iter()
            .map(
                |(category, (tools, schema_bytes))| ToolBundleCategoryCount {
                    category,
                    tools,
                    schema_bytes,
                },
            )
            .collect()
    }

    /// Prompt-prefix telemetry. No schema text or session data is included.
    pub fn telemetry_payload(&self) -> Value {
        serde_json::json!({
            "tool_bundle": self.selection.kind.as_str(),
            "tool_bundle_hash": self.bundle_hash,
            "tool_bundle_tools_hash": self.tools_hash,
            "tool_bundle_tool_count": self.builtin_tools.len(),
            "tool_bundle_mcp_count": self.mcp_tools.len(),
            "tool_bundle_total_tool_count": self.tools.len(),
            "tool_bundle_schema_bytes": self.schema_bytes(),
            "tool_bundle_tokens_estimate": self.estimated_tokens(),
            "tool_bundle_target_limit": self.target_limit,
            "tool_bundle_within_target": self.within_target(),
            "tool_bundle_fallback": self.selection.fallback,
            "tool_bundle_reason": self.selection.reason,
            "tool_bundle_role": self.selection.role,
            "tool_bundle_profile": self.selection.profile,
            "tool_bundle_task_kind": self.selection.task_kind,
            "tool_bundle_omitted": self.omitted,
            "tool_bundle_warnings": self.warnings.iter().map(ToolBundleWarning::payload).collect::<Vec<_>>(),
            "tool_bundle_frozen": true,
        })
    }

    /// Durable per-run manifest for `context.tool_bundle`. It contains names,
    /// categories, flags, and per-tool schema hashes, but never description or
    /// parameter text. The list is bounded by the bundle caps.
    pub fn manifest_payload(&self) -> Value {
        let tools = self
            .tools
            .iter()
            .map(|tool| {
                let spec = catalog_entry(&tool.name);
                serde_json::json!({
                    "name": tool.name,
                    "category": self.category_for(&tool.name),
                    "workspace_write": spec.map(|spec| spec.workspace_write).unwrap_or(false),
                    "delegation": spec.map(|spec| spec.delegation).unwrap_or(false),
                    "schema_hash": tool_schema_hash(tool),
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "bundle": self.selection.kind.as_str(),
            "bundle_hash": self.bundle_hash,
            "tools_hash": self.tools_hash,
            "role": self.selection.role,
            "profile": self.selection.profile,
            "task_kind": self.selection.task_kind,
            "reason": self.selection.reason,
            "fallback": self.selection.fallback,
            "builtin_count": self.builtin_tools.len(),
            "mcp_count": self.mcp_tools.len(),
            "total_count": self.tools.len(),
            "schema_bytes": self.schema_bytes(),
            "tokens_estimate": self.estimated_tokens(),
            "target_limit": self.target_limit,
            "within_target": self.within_target(),
            "omitted": self.omitted,
            "warnings": self.warnings.iter().map(ToolBundleWarning::payload).collect::<Vec<_>>(),
            "tools": tools,
            "frozen": true,
        })
    }

    /// Human/CLI summary without schema bodies.
    pub fn summary_payload(&self, include_tools: bool) -> Value {
        let tools = if include_tools {
            self.tools
                .iter()
                .map(|tool| {
                    let spec = catalog_entry(&tool.name);
                    serde_json::json!({
                        "name": tool.name,
                        "category": self.category_for(&tool.name),
                        "workspace_write": spec.map(|spec| spec.workspace_write).unwrap_or(false),
                        "delegation": spec.map(|spec| spec.delegation).unwrap_or(false),
                        "schema_hash": tool_schema_hash(tool),
                        "description_chars": tool.description.chars().count(),
                        "parameter_fields": tool.parameters
                            .get("properties")
                            .and_then(Value::as_object)
                            .map_or(0, Map::len),
                    })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        serde_json::json!({
            "bundle": self.selection.kind.as_str(),
            "title": definition(self.selection.kind).title,
            "role": self.selection.role,
            "profile": self.selection.profile,
            "task_kind": self.selection.task_kind,
            "reason": self.selection.reason,
            "fallback": self.selection.fallback,
            "bundle_hash": self.bundle_hash,
            "tools_hash": self.tools_hash,
            "builtin_count": self.builtin_tools.len(),
            "mcp_count": self.mcp_tools.len(),
            "total_count": self.tools.len(),
            "schema_bytes": self.schema_bytes(),
            "estimated_tokens": self.estimated_tokens(),
            "target_limit": self.target_limit,
            "within_target": self.within_target(),
            "requested": self.requested,
            "omitted": self.omitted,
            "warnings": self.warnings.iter().map(ToolBundleWarning::payload).collect::<Vec<_>>(),
            "tools": tools,
        })
    }
}

/// Recursively canonicalize a JSON value by sorting object keys. Arrays keep
/// their order; tool parameter arrays are semantically ordered.
fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: BTreeMap<&str, Value> = object
                .iter()
                .map(|(key, value)| (key.as_str(), canonical_value(value)))
                .collect();
            let mut canonical = Map::new();
            for (key, value) in sorted {
                canonical.insert(key.to_string(), value);
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

/// Wire shape of one canonical tool. Field order is declaration order and is
/// part of the prefix identity.
#[derive(Debug, Serialize)]
struct CanonicalTool<'a> {
    name: &'a str,
    description: &'a str,
    parameters: Value,
}

fn canonical_tool<'a>(tool: &'a ToolSchema, parameters: Value) -> CanonicalTool<'a> {
    CanonicalTool {
        name: &tool.name,
        description: &tool.description,
        parameters,
    }
}

/// Canonical JSON for one tool schema. Useful for a bounded per-tool hash with
/// no session data.
pub fn canonical_tool_json(tool: &ToolSchema) -> String {
    let canonical = canonical_tool(tool, canonical_value(&tool.parameters));
    serde_json::to_string(&canonical).unwrap_or_else(|_| "{}".to_string())
}

/// Canonical JSON for a tool list. The input is sorted by name and
/// deduplicated by first occurrence; callers should not rely on input order.
pub fn canonical_tools_json(tools: &[ToolSchema]) -> String {
    let mut sorted: Vec<&ToolSchema> = tools.iter().collect();
    sorted.sort_by(|left, right| left.name.cmp(&right.name));
    let mut seen = BTreeSet::new();
    let canonical = sorted
        .into_iter()
        .filter(|tool| seen.insert(tool.name.as_str()))
        .map(|tool| canonical_tool(tool, canonical_value(&tool.parameters)))
        .collect::<Vec<_>>();
    serde_json::to_string(&canonical).unwrap_or_else(|_| "[]".to_string())
}

/// SHA-256 of [`canonical_tools_json`].
pub fn canonical_tools_hash(tools: &[ToolSchema]) -> String {
    crate::sha256_hex(canonical_tools_json(tools).as_bytes())
}

/// SHA-256 of one canonical tool schema.
pub fn tool_schema_hash(tool: &ToolSchema) -> String {
    crate::sha256_hex(canonical_tool_json(tool).as_bytes())
}

/// Bundle identity hash. It includes the format version and the selection
/// identity so two different partitions that happen to share a schema for one
/// role still have distinct prefix bytes.
pub fn bundle_hash(selection: &ToolBundleSelection, builtin_tools: &[ToolSchema]) -> String {
    let schema = canonical_tools_json(builtin_tools);
    crate::sha256_hex(
        format!(
            "{TOOL_BUNDLE_VERSION}\n{}\n{}\n{}\n{}\n{}",
            selection.kind.as_str(),
            selection.role,
            selection.profile,
            selection.task_kind.as_deref().unwrap_or(""),
            schema
        )
        .as_bytes(),
    )
}

/// Identity hash of a bundle's advertised **membership**, independent of the
/// dynamic schema bytes. Runtime pinning uses this to detect that a plan or run
/// was created under a different tool partition without forcing a provider or
/// MCP schema read during resume. The exact frozen schema bytes are still
/// recorded in `context.prompt_prefix` as `tools_hash`.
pub fn bundle_contract_hash(selection: &ToolBundleSelection) -> String {
    let names = bundle_tool_names(selection.kind, &selection.role, &selection.profile);
    crate::sha256_hex(
        format!(
            "vp.tool_bundle_contract/v1\n{TOOL_BUNDLE_VERSION}\n{}\n{}\n{}\n{}\n{}\n{}",
            selection.kind.as_str(),
            selection.role,
            selection.profile,
            selection.task_kind.as_deref().unwrap_or(""),
            selection.fallback,
            names.join(",")
        )
        .as_bytes(),
    )
}

/// Rough schema-token estimate used only for reporting. The agent loop keeps
/// its own estimator for hard context limits.
pub fn estimate_tool_tokens(tools: &[ToolSchema]) -> u64 {
    (canonical_tools_json(tools).chars().count() as u64) / 4
}

fn sort_tools(tools: Vec<ToolSchema>) -> Vec<ToolSchema> {
    let mut tools = tools;
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools
}

fn dedup_tools(tools: Vec<ToolSchema>) -> Vec<ToolSchema> {
    let mut seen = BTreeSet::new();
    tools
        .into_iter()
        .filter(|tool| seen.insert(tool.name.clone()))
        .collect()
}

fn read_only_role(role: &str) -> bool {
    matches!(role, ROLE_MANAGER | ROLE_VERIFIER | ROLE_SCOUT)
}

/// Freeze one bundle for the run.
///
/// `available` is the complete schema set the runtime built for the role,
/// including special tools. `mcp` is the already server/tool-sorted dynamic
/// list. Named bundles keep only their explicit names; `Full` keeps everything.
/// The result is sorted, deduplicated, and hashed exactly once.
pub fn freeze(
    selection: ToolBundleSelection,
    available: Vec<ToolSchema>,
    mcp: Vec<ToolSchema>,
) -> FrozenToolBundle {
    let kind = selection.kind;
    let requested = if kind.is_partitioned() {
        bundle_tool_names(kind, &selection.role, &selection.profile)
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut warnings = Vec::new();
    let (builtin_tools, omitted) = if kind.is_partitioned() {
        let requested_set: BTreeSet<&str> = requested.iter().map(String::as_str).collect();
        let mut selected = Vec::new();
        let mut present = BTreeSet::new();
        for tool in available {
            if requested_set.contains(tool.name.as_str()) {
                present.insert(tool.name.clone());
                selected.push(tool);
            }
        }
        let omitted = requested
            .iter()
            .filter(|name| !present.contains(name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        (sort_tools(selected), omitted)
    } else {
        (sort_tools(available), Vec::new())
    };

    if !omitted.is_empty() {
        warnings.push(ToolBundleWarning::new(
            "tool_bundle_omitted",
            format!(
                "requested tools are not available for this role/profile: {}",
                omitted.join(", ")
            ),
        ));
    }

    let builtin_tools = dedup_tools(builtin_tools);
    if builtin_tools.is_empty() {
        warnings.push(ToolBundleWarning::new(
            "tool_bundle_empty",
            "the selected bundle produced no schemas; check role policy and bundle constants",
        ));
    }

    if let Some(limit) = kind.target_limit()
        && builtin_tools.len() > limit
    {
        warnings.push(ToolBundleWarning::new(
            "tool_bundle_over_target",
            format!(
                "bundle '{}' has {} built-in tools, above the target of {limit}",
                kind.as_str(),
                builtin_tools.len()
            ),
        ));
    }

    let mut mcp_tools = if kind.allows_dynamic_mcp() {
        sort_tools(mcp)
    } else {
        if !mcp.is_empty() {
            warnings.push(ToolBundleWarning::new(
                "tool_bundle_mcp_excluded",
                "the minimal bundle deliberately excludes dynamic MCP schemas",
            ));
        }
        Vec::new()
    };
    mcp_tools.truncate(MAX_BUNDLE_MCP_TOOLS);

    // A dynamic tool must never shadow a built-in schema or another dynamic
    // name. Sorting first makes the winner deterministic across runs.
    let builtin_names: BTreeSet<String> =
        builtin_tools.iter().map(|tool| tool.name.clone()).collect();
    let mut seen = builtin_names;
    mcp_tools.retain(|tool| seen.insert(tool.name.clone()));
    let mut tools = builtin_tools.clone();
    tools.extend(mcp_tools.iter().cloned());

    for tool in &tools {
        if forbidden_for_read_only_role(&selection.role, &tool.name) {
            warnings.push(ToolBundleWarning::new(
                "tool_bundle_read_only_violation",
                format!(
                    "read-only role '{}' would receive tool '{}'",
                    selection.role, tool.name
                ),
            ));
        }
    }
    if kind.is_partitioned() {
        let catalog_names: BTreeSet<&str> =
            BUNDLE_TOOL_CATALOG.iter().map(|entry| entry.name).collect();
        let missing = tools
            .iter()
            .filter(|tool| !catalog_names.contains(tool.name.as_str()))
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            warnings.push(ToolBundleWarning::new(
                "tool_bundle_unclassified",
                format!(
                    "selected tools without bundle metadata: {}",
                    missing.join(", ")
                ),
            ));
        }
    }

    let target_limit = kind.target_limit();
    let bundle_hash = bundle_hash(&selection, &builtin_tools);
    let tools_hash = canonical_tools_hash(&tools);
    FrozenToolBundle {
        selection,
        builtin_tools,
        mcp_tools,
        tools,
        requested,
        omitted,
        warnings,
        bundle_hash,
        tools_hash,
        target_limit,
    }
}

/// A static catalog row for the read-only CLI surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBundleCatalogEntry {
    pub kind: ToolBundleKind,
    pub title: &'static str,
    pub summary: &'static str,
    pub target_limit: Option<usize>,
    /// Requested count for the representative role/profile, before runtime
    /// filtering.
    pub requested_count: usize,
    pub representative_role: &'static str,
    pub representative_profile: &'static str,
    pub allows_dynamic_mcp: bool,
}

/// Bundle catalog with a representative role/profile per kind. This is a
/// static planning view; `session.tool_bundle` reports the actual runtime set.
pub fn catalog_entries() -> Vec<ToolBundleCatalogEntry> {
    TOOL_BUNDLE_DEFINITIONS
        .iter()
        .map(|definition| {
            let (role, profile) = match definition.kind {
                ToolBundleKind::Leader => (ROLE_LEADER, PROFILE_GENERIC),
                ToolBundleKind::Coding => (ROLE_WORKER, PROFILE_CODING),
                ToolBundleKind::Research => (ROLE_WORKER, PROFILE_RESEARCH),
                ToolBundleKind::Verification => (ROLE_VERIFIER, PROFILE_VERIFICATION),
                ToolBundleKind::Minimal => (ROLE_WORKER, PROFILE_CODING),
                ToolBundleKind::Full => (ROLE_WORKER, PROFILE_CODING),
            };
            let requested_count = if definition.kind == ToolBundleKind::Full {
                0
            } else {
                bundle_tool_names(definition.kind, role, profile).len()
            };
            ToolBundleCatalogEntry {
                kind: definition.kind,
                title: definition.title,
                summary: definition.summary,
                target_limit: definition.target_limit,
                requested_count,
                representative_role: role,
                representative_profile: profile,
                allows_dynamic_mcp: definition.kind.allows_dynamic_mcp(),
            }
        })
        .collect()
}

/// Audit the static matrix. Production calls this only in tests and in the
/// read-only bundle surface; a failure is a programming error, not a runtime
/// permission decision.
pub fn audit_matrix() -> Vec<ToolBundleWarning> {
    let mut warnings = Vec::new();
    let mut catalog = BTreeSet::new();
    for entry in BUNDLE_TOOL_CATALOG {
        if !catalog.insert(entry.name) {
            warnings.push(ToolBundleWarning::new(
                "tool_bundle_catalog_duplicate",
                format!("duplicate catalog entry '{}'", entry.name),
            ));
        }
    }
    for definition in TOOL_BUNDLE_DEFINITIONS {
        if definition.kind == ToolBundleKind::Full {
            continue;
        }
        let mut seen = BTreeSet::new();
        for name in definition.tools {
            if !seen.insert(*name) {
                warnings.push(ToolBundleWarning::new(
                    "tool_bundle_definition_duplicate",
                    format!(
                        "bundle '{}' lists '{}' more than once",
                        definition.kind.as_str(),
                        name
                    ),
                ));
            }
            if !catalog.contains(name) {
                warnings.push(ToolBundleWarning::new(
                    "tool_bundle_definition_unknown",
                    format!(
                        "bundle '{}' references uncatalogued tool '{}'",
                        definition.kind.as_str(),
                        name
                    ),
                ));
            }
        }
        for (role, profile) in [
            (ROLE_LEADER, PROFILE_GENERIC),
            (ROLE_LEADER, PROFILE_CODING),
            (ROLE_MANAGER, PROFILE_CODING),
            (ROLE_WORKER, PROFILE_CODING),
            (ROLE_WORKER, PROFILE_RESEARCH),
            (ROLE_WORKER, PROFILE_VERIFICATION),
            (ROLE_LABOUR, PROFILE_CODING),
            (ROLE_VERIFIER, PROFILE_VERIFICATION),
            (ROLE_SCOUT, PROFILE_RESEARCH),
        ] {
            let names = bundle_tool_names(definition.kind, role, profile);
            if let Some(limit) = definition.target_limit
                && names.len() > limit
            {
                warnings.push(ToolBundleWarning::new(
                    "tool_bundle_definition_over_target",
                    format!(
                        "bundle '{}' for {role}/{profile} requests {} tools, target {limit}",
                        definition.kind.as_str(),
                        names.len()
                    ),
                ));
            }
            for name in &names {
                if read_only_role(role) && forbidden_for_read_only_role(role, name) {
                    warnings.push(ToolBundleWarning::new(
                        "tool_bundle_definition_read_only_violation",
                        format!(
                            "bundle '{}' requests write/delegation tool '{}' for read-only role {role}",
                            definition.kind.as_str(),
                            name
                        ),
                    ));
                }
            }
        }
    }
    warnings
}

/// Count of distinct tools covered by the static catalog. Used by tests and the
/// diagnostics surface to catch a forgotten entry.
pub fn catalog_size() -> usize {
    BUNDLE_TOOL_CATALOG.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: format!("description for {name}"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "z": {"type": "string"},
                    "a": {"type": "integer"}
                },
                "required": ["a"]
            }),
        }
    }

    fn schemas(names: &[&str]) -> Vec<ToolSchema> {
        names.iter().map(|name| schema(name)).collect()
    }

    fn selected_names(kind: ToolBundleKind, role: &str, profile: &str) -> Vec<&'static str> {
        bundle_tool_names(kind, role, profile)
    }

    #[test]
    fn task_kind_mapping_is_deterministic_and_case_insensitive() {
        for (kind, expected) in [
            ("implementation", ToolBundleKind::Coding),
            ("IMPLEMENT", ToolBundleKind::Coding),
            (" code ", ToolBundleKind::Coding),
            ("bug-fix", ToolBundleKind::Coding),
            ("bug_fix", ToolBundleKind::Coding),
            ("refactor", ToolBundleKind::Coding),
            ("verification", ToolBundleKind::Verification),
            ("review", ToolBundleKind::Verification),
            ("AUDIT", ToolBundleKind::Verification),
            ("research", ToolBundleKind::Research),
            ("investigate", ToolBundleKind::Research),
            ("discovery", ToolBundleKind::Research),
            ("minimal", ToolBundleKind::Minimal),
            ("trivial", ToolBundleKind::Minimal),
            ("one-file", ToolBundleKind::Minimal),
            ("format", ToolBundleKind::Minimal),
        ] {
            assert_eq!(task_kind_bundle(kind), Some(expected), "kind {kind}");
        }
        assert_eq!(task_kind_bundle(""), None);
        assert_eq!(task_kind_bundle("   "), None);
        assert_eq!(task_kind_bundle("unknown-engineering-thing"), None);
    }

    #[test]
    fn selection_respects_role_and_task_kind() {
        let leader = select_bundle("leader", "coding", Some("implementation"));
        assert_eq!(leader.kind, ToolBundleKind::Leader);
        assert_eq!(leader.reason, "role_leader");
        assert!(!leader.fallback);

        let manager = select_bundle("manager", "generic", Some("minimal"));
        assert_eq!(manager.kind, ToolBundleKind::Leader);

        let verifier = select_bundle("verifier", "verification", Some("implementation"));
        assert_eq!(verifier.kind, ToolBundleKind::Verification);

        let scout = select_bundle("scout", "research", Some("implementation"));
        assert_eq!(scout.kind, ToolBundleKind::Research);

        let worker = select_bundle("worker", "coding", Some("implementation"));
        assert_eq!(worker.kind, ToolBundleKind::Coding);
        assert_eq!(worker.reason, "task_kind");
        assert!(!worker.fallback);

        let worker_missing = select_bundle("worker", "coding", None);
        assert_eq!(worker_missing.kind, ToolBundleKind::Full);
        assert!(worker_missing.fallback);
        assert_eq!(worker_missing.reason, "no_task_kind_full");

        let worker_unknown = select_bundle("worker", "coding", Some("esoteric"));
        assert_eq!(worker_unknown.kind, ToolBundleKind::Full);
        assert!(worker_unknown.fallback);
        assert_eq!(worker_unknown.reason, "task_kind_unknown_full");

        let labour = select_bundle("labour", "coding", Some("review"));
        assert_eq!(labour.kind, ToolBundleKind::Verification);
    }

    #[test]
    fn leader_bundle_membership_is_exact() {
        let generic = selected_names(ToolBundleKind::Leader, "leader", "generic");
        assert_eq!(generic.len(), 49);
        assert_eq!(
            generic,
            vec![
                "apply_patch",
                "ask_user",
                "budget_status",
                "build_context",
                "code_diagnostics",
                "coordinate",
                "delegate",
                "delegate_acp",
                "deliberate_team_plan",
                "edit_file",
                "fetch_url",
                "file_info",
                "find_files",
                "find_references",
                "find_symbol",
                "git",
                "grep",
                "list_dir",
                "list_sources",
                "lsp_query",
                "mcp",
                "plan_fanout",
                "process",
                "publish_artifact",
                "read_artifact",
                "read_file",
                "read_files",
                "read_mailbox",
                "read_source",
                "read_tool_output",
                "recall",
                "remember",
                "repo_map",
                "run_script",
                "run_team_plan",
                "scout",
                "search_sources",
                "send_message",
                "shell",
                "submit_team_plan",
                "task_control",
                "team_status",
                "todo_write",
                "tool_help",
                "tool_search",
                "use_skill",
                "wait_for_gate",
                "web_search",
                "write_file",
            ]
        );
        let coding = selected_names(ToolBundleKind::Leader, "leader", "coding");
        assert!(coding.len() <= LEADER_TOOL_LIMIT);
        assert_eq!(coding.len(), 51);
        assert!(coding.contains(&"browser"));
        assert!(coding.contains(&"computer"));
        let research = selected_names(ToolBundleKind::Leader, "leader", "research");
        assert!(!research.contains(&"browser"));
        assert!(!research.contains(&"computer"));
    }

    #[test]
    fn coding_bundle_membership_is_exact_and_role_conditional() {
        let worker = selected_names(ToolBundleKind::Coding, "worker", "coding");
        assert!(worker.len() <= CODING_TOOL_LIMIT);
        assert_eq!(worker.len(), 39);
        assert!(worker.contains(&"delegate"));
        assert!(worker.contains(&"browser"));
        assert!(worker.contains(&"computer"));
        for required in [
            "read_file",
            "write_file",
            "edit_file",
            "apply_patch",
            "find_files",
            "grep",
            "shell",
            "process",
            "git",
            "repo_map",
            "code_diagnostics",
            "lsp_query",
            "run_project_task",
            "run_script",
            "publish_artifact",
            "recall",
            "remember",
            "coordinate",
            "read_tool_output",
            "tool_help",
        ] {
            assert!(
                worker.contains(&required),
                "coding bundle missing {required}"
            );
        }
        let labour = selected_names(ToolBundleKind::Coding, "labour", "coding");
        assert!(!labour.contains(&"delegate"));
        assert!(labour.len() <= CODING_TOOL_LIMIT);
        let research_profile = selected_names(ToolBundleKind::Coding, "worker", "research");
        assert!(!research_profile.contains(&"browser"));
        assert!(!research_profile.contains(&"computer"));
        assert!(research_profile.contains(&"delegate"));
    }

    #[test]
    fn research_bundle_is_read_only_and_exact() {
        let names = selected_names(ToolBundleKind::Research, "worker", "research");
        assert_eq!(names.len(), 34);
        for required in [
            "read_file",
            "grep",
            "fetch_url",
            "web_search",
            "search_sources",
            "recall",
            "repo_map",
            "git",
            "read_pdf",
            "read_document",
            "use_skill",
            "read_tool_output",
        ] {
            assert!(
                names.contains(&required),
                "research bundle missing {required}"
            );
        }
        for forbidden in [
            "write_file",
            "edit_file",
            "apply_patch",
            "file_manage",
            "shell",
            "process",
            "delegate",
            "delegate_many",
            "delegate_acp",
            "submit_team_plan",
            "run_team_plan",
            "todo_write",
            "archive_extract",
            "media_transcode",
        ] {
            assert!(
                !names.contains(&forbidden),
                "research bundle must not advertise {forbidden}"
            );
        }
    }

    #[test]
    fn verification_bundle_is_read_only_and_exact() {
        let names = selected_names(ToolBundleKind::Verification, "verifier", "verification");
        assert_eq!(names.len(), 38);
        assert!(names.contains(&"submit_verdict"));
        assert!(names.contains(&"shell"));
        for forbidden in [
            "write_file",
            "edit_file",
            "apply_patch",
            "file_manage",
            "delegate",
            "delegate_many",
            "delegate_acp",
            "submit_team_plan",
            "run_team_plan",
            "deliberate_team_plan",
            "todo_write",
            "archive_extract",
            "media_transcode",
            "publish_artifact_file",
            "remember",
        ] {
            assert!(
                !names.contains(&forbidden),
                "verification bundle must not advertise {forbidden}"
            );
        }
        for required in [
            "read_file",
            "grep",
            "git",
            "shell",
            "process",
            "lsp_query",
            "code_diagnostics",
            "read_tool_output",
            "tool_help",
        ] {
            assert!(names.contains(&required), "verification missing {required}");
        }
    }

    #[test]
    fn minimal_bundle_is_exact_and_small() {
        let names = selected_names(ToolBundleKind::Minimal, "worker", "coding");
        assert_eq!(
            names,
            vec![
                "edit_file",
                "list_dir",
                "read_file",
                "read_tool_output",
                "shell",
                "write_file",
            ]
        );
        assert!(names.len() <= MINIMAL_TOOL_LIMIT);
        let leader_minimal = selected_names(ToolBundleKind::Minimal, "leader", "generic");
        assert_eq!(leader_minimal, names);
    }

    #[test]
    fn leader_is_far_smaller_than_the_full_legacy_set() {
        let full = tool_schemas_for_test();
        let leader = bundle_tool_names(ToolBundleKind::Leader, "leader", "generic");
        assert!(
            full.len() > 60,
            "legacy set unexpectedly small: {}",
            full.len()
        );
        assert!(leader.len() <= LEADER_TOOL_LIMIT);
        assert!(leader.len() + 15 < full.len());
    }

    /// A synthetic full list with every legacy special tool. This keeps the
    /// size assertion meaningful without constructing an `AgentConfig`.
    fn tool_schemas_for_test() -> Vec<ToolSchema> {
        BUNDLE_TOOL_CATALOG
            .iter()
            .map(|entry| schema(entry.name))
            .collect()
    }

    #[test]
    fn catalog_is_unique_and_every_bundle_name_is_catalogued() {
        assert!(audit_matrix().is_empty(), "{:?}", audit_matrix());
        assert!(catalog_size() >= 70);
        for definition in TOOL_BUNDLE_DEFINITIONS {
            for name in definition.tools {
                assert!(
                    catalog_entry(name).is_some(),
                    "bundle {} references {name} without metadata",
                    definition.kind.as_str()
                );
            }
        }
    }

    #[test]
    fn read_only_roles_never_receive_write_shell_or_delegation_tools() {
        for (role, profile, kind) in [
            ("manager", "coding", ToolBundleKind::Leader),
            ("scout", "research", ToolBundleKind::Research),
            ("verifier", "verification", ToolBundleKind::Verification),
        ] {
            for name in bundle_tool_names(kind, role, profile) {
                assert!(
                    !forbidden_for_read_only_role(role, name),
                    "{role}/{profile} got {name}"
                );
            }
        }
    }

    #[test]
    fn canonical_json_sorts_tools_and_parameter_keys() {
        let left = vec![schema("zeta"), schema("alpha")];
        let mut right = vec![schema("alpha"), schema("zeta")];
        right[0].parameters = json!({
            "required": ["a"],
            "properties": {
                "a": {"type": "integer"},
                "z": {"type": "string"}
            },
            "type": "object"
        });
        let left_json = canonical_tools_json(&left);
        let right_json = canonical_tools_json(&right);
        assert_eq!(left_json, right_json, "input order must not matter");
        assert!(left_json.starts_with(r#"[{"name":"alpha","description":"#));
        let alpha = &left_json[..left_json.find("zeta").unwrap()];
        assert!(alpha.find("description").unwrap() < alpha.find("parameters").unwrap());
        assert!(
            left_json.contains(r#""properties":{"a":{"type":"integer"},"z":{"type":"string"}}"#)
        );
        assert_eq!(
            canonical_tools_hash(&left),
            canonical_tools_hash(&right),
            "hash must be order independent"
        );
    }

    #[test]
    fn canonical_fixture_bytes_and_hash_are_locked() {
        // This fixture is the contract for the canonical serializer. A change
        // here is a deliberate prompt-prefix epoch change, not an accident.
        let tools = vec![schema("zeta"), schema("alpha")];
        let expected = concat!(
            r#"[{"name":"alpha","description":"description for alpha","parameters":"#,
            r#"{"properties":{"a":{"type":"integer"},"z":{"type":"string"}},"required":["a"],"type":"object"}},"#,
            r#"{"name":"zeta","description":"description for zeta","parameters":"#,
            r#"{"properties":{"a":{"type":"integer"},"z":{"type":"string"}},"required":["a"],"type":"object"}}]"#
        );
        assert_eq!(canonical_tools_json(&tools), expected);
        assert_eq!(
            canonical_tools_hash(&tools),
            "de27c9a7e774452238f63e36f439d6a0d096a014945ca28cbafab9a0926eed4b"
        );
        let mut permuted = tools;
        permuted.reverse();
        assert_eq!(canonical_tools_json(&permuted), expected);
    }

    #[test]
    fn canonical_json_deduplicates_by_name_deterministically() {
        let mut tools = vec![schema("read_file"), schema("read_file")];
        tools[1].description = "second description".into();
        let json = canonical_tools_json(&tools);
        assert_eq!(json.matches(r#""name":"read_file""#).count(), 1);
        assert_eq!(canonical_tools_json(&tools), canonical_tools_json(&tools));
    }

    #[test]
    fn canonical_hash_changes_when_description_or_parameters_change() {
        let base = vec![schema("read_file")];
        let base_hash = canonical_tools_hash(&base);
        let mut description = base.clone();
        description[0].description = "changed".into();
        assert_ne!(base_hash, canonical_tools_hash(&description));
        let mut parameters = base.clone();
        parameters[0].parameters = json!({"type": "object"});
        assert_ne!(base_hash, canonical_tools_hash(&parameters));
    }

    #[test]
    fn canonical_hash_contains_no_session_data() {
        let tool = schema("read_file");
        let json = canonical_tool_json(&tool);
        assert!(!json.contains("session"));
        assert!(!json.contains("worktree"));
        assert!(!json.contains("2026-"));
    }

    #[test]
    fn freeze_filters_to_the_named_bundle_and_sorts() {
        let available = schemas(&[
            "write_file",
            "read_file",
            "shell",
            "delegate",
            "submit_verdict",
            "totally_unrelated",
        ]);
        let selection = select_bundle("worker", "coding", Some("trivial"));
        let frozen = freeze(selection, available, Vec::new());
        assert_eq!(frozen.kind(), ToolBundleKind::Minimal);
        assert_eq!(
            frozen.tool_names(),
            vec!["read_file", "shell", "write_file"],
            "minimal bundle keeps only its explicit names and sorts"
        );
        assert!(frozen.requested.contains(&"edit_file".to_string()));
        assert!(frozen.omitted.contains(&"edit_file".to_string()));
        assert!(frozen.omitted.contains(&"list_dir".to_string()));
        assert!(frozen.omitted.contains(&"read_tool_output".to_string()));
        assert_eq!(frozen.tool_count(), 3);
        assert_eq!(frozen.mcp_count(), 0);
        assert!(frozen.within_target().unwrap_or(false));
    }

    #[test]
    fn full_fallback_keeps_every_available_schema() {
        let available = schemas(&["write_file", "read_file", "shell", "remote_exec"]);
        let selection = select_bundle("worker", "coding", Some("make-it-up"));
        let frozen = freeze(selection, available.clone(), Vec::new());
        assert_eq!(frozen.kind(), ToolBundleKind::Full);
        assert!(frozen.selection.fallback);
        assert_eq!(frozen.tool_count(), available.len());
        assert_eq!(
            frozen.tool_names(),
            vec!["read_file", "remote_exec", "shell", "write_file"]
        );
        assert_eq!(frozen.target_limit, None);
        assert_eq!(frozen.requested, Vec::<String>::new());
    }

    #[test]
    fn freeze_is_stable_across_input_order_and_sessions() {
        let available_a = schemas(&["write_file", "read_file", "shell", "edit_file"]);
        let available_b = schemas(&["shell", "edit_file", "read_file", "write_file"]);
        let first = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            available_a,
            Vec::new(),
        );
        let second = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            available_b,
            Vec::new(),
        );
        assert_eq!(first.tool_names(), second.tool_names());
        assert_eq!(first.bundle_hash, second.bundle_hash);
        assert_eq!(first.tools_hash, second.tools_hash);
        assert_eq!(first.requested, second.requested);
    }

    #[test]
    fn bundle_hash_changes_when_selection_or_schema_changes() {
        let available = schemas(&[
            "read_file",
            "write_file",
            "shell",
            "edit_file",
            "run_project_task",
        ]);
        let minimal = freeze(
            select_bundle("worker", "coding", Some("trivial")),
            available.clone(),
            Vec::new(),
        );
        let coding = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            available.clone(),
            Vec::new(),
        );
        assert_ne!(minimal.bundle_hash, coding.bundle_hash);
        assert_ne!(minimal.tools_hash, coding.tools_hash);

        let mut changed = available;
        changed[0].description = "different stable bytes".into();
        let changed = freeze(
            select_bundle("worker", "coding", Some("trivial")),
            changed,
            Vec::new(),
        );
        assert_ne!(minimal.bundle_hash, changed.bundle_hash);
        assert_ne!(minimal.tools_hash, changed.tools_hash);
    }

    #[test]
    fn mcp_tools_are_sorted_deduplicated_and_frozen_once() {
        let available = schemas(&["read_file", "write_file", "shell"]);
        let mcp = schemas(&["mcp__z__beta", "mcp__a__alpha", "mcp__z__beta"]);
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            available,
            mcp,
        );
        let names = frozen.tool_names();
        let mcp_names = frozen
            .mcp_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(mcp_names, vec!["mcp__a__alpha", "mcp__z__beta"]);
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("mcp__"))
                .count(),
            1 + 1
        );
        assert_eq!(frozen.tools_hash, canonical_tools_hash(&frozen.tools));
        // A second freeze with shuffled MCP input produces the same bytes.
        let again = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["shell", "write_file", "read_file"]),
            schemas(&["mcp__a__alpha", "mcp__z__beta"]),
        );
        assert_eq!(frozen.tools_hash, again.tools_hash);
    }

    #[test]
    fn mcp_tools_cannot_shadow_a_builtin_name() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file"]),
            vec![schema("read_file"), schema("mcp__a__alpha")],
        );
        assert_eq!(
            frozen
                .tools
                .iter()
                .filter(|tool| tool.name == "read_file")
                .count(),
            1
        );
        assert!(frozen.contains("mcp__a__alpha"));
    }

    #[test]
    fn minimal_bundle_excludes_dynamic_mcp_and_warns() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("typo")),
            schemas(&["read_file", "write_file", "edit_file", "shell", "list_dir"]),
            schemas(&["mcp__a__alpha"]),
        );
        assert_eq!(frozen.mcp_count(), 0);
        assert!(
            frozen
                .warnings
                .iter()
                .any(|warning| warning.code == "tool_bundle_mcp_excluded")
        );
        assert!(!frozen.contains("mcp__a__alpha"));
    }

    #[test]
    fn read_only_selection_drops_writes_even_if_policy_leaks_one() {
        let frozen = freeze(
            select_bundle("verifier", "verification", Some("review")),
            schemas(&["read_file", "write_file", "shell", "process"]),
            Vec::new(),
        );
        // write_file is not in the verification bundle, so it is dropped.
        assert!(!frozen.contains("write_file"));
        assert!(frozen.contains("shell"));
        assert!(frozen.contains("process"));
        assert!(
            !frozen
                .warnings
                .iter()
                .any(|warning| warning.code == "tool_bundle_read_only_violation"),
            "{:?}",
            frozen.warnings
        );
    }

    #[test]
    fn telemetry_payload_has_no_schema_text_and_has_stable_fields() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file", "write_file", "shell", "edit_file"]),
            schemas(&["mcp__a__alpha"]),
        );
        let payload = frozen.telemetry_payload();
        assert_eq!(payload["tool_bundle"], "coding");
        assert_eq!(payload["tool_bundle_frozen"], true);
        assert_eq!(payload["tool_bundle_tool_count"], 4);
        assert_eq!(payload["tool_bundle_mcp_count"], 1);
        assert_eq!(payload["tool_bundle_total_tool_count"], 5);
        assert_eq!(payload["tool_bundle_target_limit"], 40);
        assert_eq!(payload["tool_bundle_within_target"], true);
        assert!(payload["tool_bundle_hash"].as_str().unwrap().len() == 64);
        assert!(payload["tool_bundle_tools_hash"].as_str().unwrap().len() == 64);
        let text = payload.to_string();
        assert!(!text.contains("description for"));
    }

    #[test]
    fn category_counts_are_sorted_and_sum_to_the_bundle() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file", "write_file", "shell", "edit_file", "delegate"]),
            schemas(&["mcp__a__alpha"]),
        );
        let counts = frozen.category_counts();
        let mut sorted = counts
            .iter()
            .map(|count| count.category)
            .collect::<Vec<_>>();
        sorted.sort();
        let actual = counts
            .iter()
            .map(|count| count.category)
            .collect::<Vec<_>>();
        assert_eq!(actual, sorted);
        assert_eq!(
            counts.iter().map(|count| count.tools).sum::<usize>(),
            frozen.tool_count()
        );
        assert_eq!(
            counts.iter().map(|count| count.schema_bytes).sum::<u64>(),
            frozen
                .tools
                .iter()
                .map(|tool| canonical_tool_json(tool).len() as u64)
                .sum::<u64>()
        );
        assert!(counts.iter().any(|count| count.category == "files"));
        assert!(counts.iter().any(|count| count.category == "mcp"));
        assert!(counts.iter().any(|count| count.category == "delegation"));
    }

    #[test]
    fn manifest_payload_has_names_hashes_and_no_schema_text() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file", "write_file", "shell", "edit_file"]),
            schemas(&["mcp__a__alpha"]),
        );
        let manifest = frozen.manifest_payload();
        assert_eq!(manifest["bundle"], "coding");
        assert_eq!(manifest["frozen"], true);
        assert_eq!(manifest["builtin_count"], 4);
        assert_eq!(manifest["mcp_count"], 1);
        assert_eq!(manifest["total_count"], 5);
        let tools = manifest["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5);
        for tool in tools {
            assert!(tool["name"].as_str().is_some());
            assert!(tool["schema_hash"].as_str().unwrap().len() == 64);
            assert!(tool["category"].as_str().is_some());
        }
        let text = manifest.to_string();
        assert!(!text.contains("description for"));
        assert!(!text.contains(r#""parameters""#));
        assert_eq!(manifest["schema_bytes"], frozen.schema_bytes());
        assert_eq!(manifest["tokens_estimate"], frozen.estimated_tokens());
    }

    #[test]
    fn summary_payload_bounds_tool_metadata() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file", "write_file", "shell", "edit_file"]),
            Vec::new(),
        );
        let summary = frozen.summary_payload(true);
        assert_eq!(summary["bundle"], "coding");
        assert_eq!(summary["builtin_count"], 4);
        assert_eq!(summary["tools"].as_array().unwrap().len(), 4);
        for tool in summary["tools"].as_array().unwrap() {
            assert!(tool["schema_hash"].as_str().unwrap().len() == 64);
            assert!(tool["description_chars"].as_u64().unwrap() > 0);
        }
        let without = frozen.summary_payload(false);
        assert_eq!(without["tools"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn catalog_entries_are_bounded_and_sorted() {
        let entries = catalog_entries();
        assert_eq!(entries.len(), TOOL_BUNDLE_DEFINITIONS.len());
        let minimal = entries
            .iter()
            .find(|entry| entry.kind == ToolBundleKind::Minimal)
            .unwrap();
        assert_eq!(minimal.requested_count, 6);
        assert_eq!(minimal.target_limit, Some(MINIMAL_TOOL_LIMIT));
        let leader = entries
            .iter()
            .find(|entry| entry.kind == ToolBundleKind::Leader)
            .unwrap();
        assert_eq!(leader.requested_count, 49);
        let coding = entries
            .iter()
            .find(|entry| entry.kind == ToolBundleKind::Coding)
            .unwrap();
        assert_eq!(coding.requested_count, 39);
        assert_eq!(coding.target_limit, Some(CODING_TOOL_LIMIT));
        let full = entries
            .iter()
            .find(|entry| entry.kind == ToolBundleKind::Full)
            .unwrap();
        assert_eq!(full.requested_count, 0);
        assert_eq!(full.target_limit, None);
    }

    #[test]
    fn estimate_tool_tokens_is_monotonic() {
        let small = schemas(&["read_file"]);
        let large = schemas(&["read_file", "write_file", "shell"]);
        assert!(estimate_tool_tokens(&small) < estimate_tool_tokens(&large));
        assert!(estimate_tool_tokens(&small) > 0);
    }

    #[test]
    fn kind_names_round_trip_and_display() {
        for kind in [
            ToolBundleKind::Leader,
            ToolBundleKind::Coding,
            ToolBundleKind::Research,
            ToolBundleKind::Verification,
            ToolBundleKind::Minimal,
            ToolBundleKind::Full,
        ] {
            assert_eq!(ToolBundleKind::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(
            ToolBundleKind::parse(" LEADER "),
            Some(ToolBundleKind::Leader)
        );
        assert_eq!(ToolBundleKind::parse("nope"), None);
        assert!(ToolBundleKind::Leader.is_partitioned());
        assert!(!ToolBundleKind::Full.is_partitioned());
    }

    #[test]
    fn target_limits_and_mcp_semantics_are_explicit() {
        assert_eq!(ToolBundleKind::Leader.target_limit(), Some(51));
        assert_eq!(ToolBundleKind::Coding.target_limit(), Some(40));
        assert_eq!(ToolBundleKind::Minimal.target_limit(), Some(12));
        assert_eq!(ToolBundleKind::Research.target_limit(), None);
        assert_eq!(ToolBundleKind::Verification.target_limit(), None);
        assert_eq!(ToolBundleKind::Full.target_limit(), None);
        assert!(ToolBundleKind::Leader.allows_dynamic_mcp());
        assert!(ToolBundleKind::Coding.allows_dynamic_mcp());
        assert!(!ToolBundleKind::Minimal.allows_dynamic_mcp());
        assert!(ToolBundleKind::Full.allows_dynamic_mcp());
    }

    #[test]
    fn bundle_tool_specs_expose_categories_and_write_flags() {
        let specs = bundle_tool_specs(ToolBundleKind::Coding, "worker", "coding");
        let by_name = specs
            .iter()
            .map(|spec| (spec.name, *spec))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_name["write_file"].category, ToolCategory::Files);
        assert!(by_name["write_file"].workspace_write);
        assert_eq!(by_name["git"].category, ToolCategory::Git);
        assert!(!by_name["git"].workspace_write);
        assert_eq!(by_name["delegate"].category, ToolCategory::Delegation);
        assert!(by_name["delegate"].delegation);
        assert_eq!(by_name["lsp_query"].category, ToolCategory::Diagnostics);
        let minimal = bundle_tool_specs(ToolBundleKind::Minimal, "worker", "coding");
        assert_eq!(minimal.len(), 6);
        assert!(
            minimal
                .iter()
                .all(|spec| catalog_entry(spec.name).is_some())
        );
    }

    #[test]
    fn freeze_deduplicates_builtin_schemas_by_name() {
        let mut duplicate = schema("read_file");
        duplicate.description = "second copy".into();
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            vec![schema("read_file"), duplicate, schema("write_file")],
            Vec::new(),
        );
        assert_eq!(
            frozen
                .tools
                .iter()
                .filter(|tool| tool.name == "read_file")
                .count(),
            1
        );
        assert_eq!(frozen.tool_count(), 2);
    }

    #[test]
    fn full_bundle_appends_dynamic_mcp_but_minimal_does_not() {
        let full = freeze(
            select_bundle("worker", "coding", None),
            schemas(&["read_file", "remote_exec"]),
            schemas(&["mcp__z__z", "mcp__a__a"]),
        );
        assert_eq!(full.kind(), ToolBundleKind::Full);
        assert_eq!(full.mcp_count(), 2);
        assert_eq!(
            full.mcp_tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__a__a", "mcp__z__z"]
        );
        let minimal = freeze(
            select_bundle("worker", "coding", Some("trivial")),
            schemas(&["read_file", "write_file", "edit_file", "shell", "list_dir"]),
            schemas(&["mcp__a__a"]),
        );
        assert_eq!(minimal.mcp_count(), 0);
        assert!(
            minimal
                .warnings
                .iter()
                .any(|warning| warning.code == "tool_bundle_mcp_excluded")
        );
    }

    #[test]
    fn dynamic_mcp_cap_is_enforced_deterministically() {
        let mcp = (0..(MAX_BUNDLE_MCP_TOOLS + 5))
            .map(|index| schema(&format!("mcp__srv__tool_{index:02}")))
            .collect::<Vec<_>>();
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file"]),
            mcp,
        );
        assert_eq!(frozen.mcp_count(), MAX_BUNDLE_MCP_TOOLS);
        assert_eq!(
            frozen.mcp_tools.last().map(|tool| tool.name.as_str()),
            Some("mcp__srv__tool_31")
        );
        let mut reversed = (0..(MAX_BUNDLE_MCP_TOOLS + 5))
            .rev()
            .map(|index| schema(&format!("mcp__srv__tool_{index:02}")))
            .collect::<Vec<_>>();
        reversed.truncate(MAX_BUNDLE_MCP_TOOLS + 5);
        let again = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file"]),
            reversed,
        );
        assert_eq!(frozen.tools_hash, again.tools_hash);
    }

    #[test]
    fn empty_bundle_produces_a_warning_not_a_panic() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("trivial")),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(frozen.tool_count(), 0);
        assert!(
            frozen
                .warnings
                .iter()
                .any(|warning| warning.code == "tool_bundle_empty")
        );
    }

    #[test]
    fn requested_and_omitted_lists_are_sorted_and_bounded() {
        let frozen = freeze(
            select_bundle("leader", "generic", None),
            schemas(&["read_file", "shell"]),
            Vec::new(),
        );
        let mut sorted = frozen.requested.clone();
        sorted.sort();
        assert_eq!(frozen.requested, sorted);
        let mut omitted = frozen.omitted.clone();
        omitted.sort();
        assert_eq!(frozen.omitted, omitted);
        assert!(frozen.omitted.contains(&"write_file".to_string()));
        assert!(!frozen.omitted.contains(&"read_file".to_string()));
        assert_eq!(
            frozen.requested.len(),
            frozen.builtin_count() + frozen.omitted.len()
        );
    }

    #[test]
    fn manager_bundle_keeps_authority_delegation_and_prunes_workspace_writes() {
        let manager = bundle_tool_names(ToolBundleKind::Leader, "manager", "coding");
        for forbidden in [
            "write_file",
            "edit_file",
            "apply_patch",
            "file_manage",
            "shell",
            "process",
            "browser",
            "computer",
        ] {
            assert!(
                !manager.contains(&forbidden),
                "Manager requested {forbidden}"
            );
        }
        for authority in [
            "delegate",
            "delegate_many",
            "delegate_acp",
            "scout",
            "task_control",
        ] {
            assert!(
                manager.contains(&authority),
                "Manager must retain authority tool {authority}"
            );
        }
        assert!(manager.contains(&"read_file"));
        assert!(manager.contains(&"git"));
        assert!(manager.contains(&"team_status"));
        assert!(manager.contains(&"task_control"));
    }

    #[test]
    fn scout_bundle_is_read_only_and_keeps_exploration_tools() {
        let scout = bundle_tool_names(ToolBundleKind::Research, "scout", "research");
        for required in [
            "read_file",
            "grep",
            "repo_map",
            "find_symbol",
            "code_diagnostics",
            "list_sources",
            "search_sources",
            "recall",
            "team_status",
            "read_tool_output",
        ] {
            assert!(scout.contains(&required), "Scout missing {required}");
        }
        for forbidden in [
            "shell",
            "write_file",
            "edit_file",
            "apply_patch",
            "process",
            "delegate",
            "ask_user",
            "use_skill",
            "browser",
            "computer",
        ] {
            assert!(!scout.contains(&forbidden), "Scout advertised {forbidden}");
        }
    }

    #[test]
    fn unknown_role_falls_back_to_full() {
        let selection = select_bundle("auditor", "generic", Some("research"));
        assert_eq!(selection.kind, ToolBundleKind::Full);
        assert!(selection.fallback);
        assert_eq!(selection.reason, "unknown_role_full");
        assert_eq!(
            bundle_tool_names(selection.kind, "auditor", "generic"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn selection_normalizes_role_and_profile_case_and_whitespace() {
        let selection = select_bundle("  WoRkEr ", " CODING ", Some("  Trivial  "));
        assert_eq!(selection.kind, ToolBundleKind::Minimal);
        assert_eq!(selection.role, "worker");
        assert_eq!(selection.profile, "coding");
        assert_eq!(selection.task_kind.as_deref(), Some("Trivial"));
    }

    #[test]
    fn canonical_json_handles_nested_arrays_and_unicode() {
        let mut tool = schema("unicode");
        tool.description = "héllo 世界".into();
        tool.parameters = json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "z": {"type": "string"},
                            "a": {"type": "number"}
                        }
                    }
                },
                "enum": ["β", "α"]
            }
        });
        let json = canonical_tool_json(&tool);
        assert!(json.contains("héllo 世界"));
        let outer = json.find(r#""properties":{"enum""#).unwrap();
        let inner = json.find(r#""properties":{"a""#).unwrap();
        assert!(outer < inner);
        // Array order is semantic and must be preserved.
        assert!(json.find("\"β\"").unwrap() < json.find("\"α\"").unwrap());
        assert_eq!(json, canonical_tool_json(&tool));
        assert_eq!(tool_schema_hash(&tool), tool_schema_hash(&tool));
    }

    #[test]
    fn canonical_tool_json_field_order_is_name_description_parameters() {
        let json = canonical_tool_json(&schema("ordered"));
        let name = json.find(r#""name""#).unwrap();
        let description = json.find(r#""description""#).unwrap();
        let parameters = json.find(r#""parameters""#).unwrap();
        assert!(name < description);
        assert!(description < parameters);
        assert!(json.starts_with(r#"{"name":"ordered""#));
    }

    #[test]
    fn canonical_hash_is_invariant_under_all_permutations() {
        let base = schemas(&["alpha", "beta", "gamma", "delta", "epsilon"]);
        let expected = canonical_tools_hash(&base);
        let permutations = [
            vec![4, 3, 2, 1, 0],
            vec![1, 0, 3, 4, 2],
            vec![2, 4, 0, 1, 3],
            vec![0, 2, 1, 3, 4],
            vec![3, 1, 4, 0, 2],
        ];
        for order in permutations {
            let permuted = order
                .into_iter()
                .map(|index| base[index].clone())
                .collect::<Vec<_>>();
            assert_eq!(canonical_tools_hash(&permuted), expected);
            assert_eq!(canonical_tools_json(&permuted), canonical_tools_json(&base));
        }
    }

    #[test]
    fn bundle_hash_includes_role_and_profile_identity() {
        let tools = schemas(&["read_file", "write_file", "shell", "edit_file"]);
        let coding = freeze(
            select_bundle("worker", "coding", Some("trivial")),
            tools.clone(),
            Vec::new(),
        );
        let research = freeze(
            select_bundle("worker", "research", Some("trivial")),
            tools,
            Vec::new(),
        );
        assert_eq!(coding.tools_hash, research.tools_hash);
        assert_ne!(coding.bundle_hash, research.bundle_hash);
    }

    #[test]
    fn leader_schema_estimate_is_much_smaller_than_full_catalog() {
        let leader = bundle_tool_names(ToolBundleKind::Leader, "leader", "generic")
            .into_iter()
            .map(|name| {
                let mut tool = schema(name);
                tool.description = format!(
                    "{} description that is intentionally long enough to model a real schema",
                    name
                );
                tool
            })
            .collect::<Vec<_>>();
        let full = BUNDLE_TOOL_CATALOG
            .iter()
            .map(|entry| {
                let mut tool = schema(entry.name);
                tool.description = format!(
                    "{} description that is intentionally long enough to model a real schema",
                    entry.name
                );
                tool
            })
            .collect::<Vec<_>>();
        let leader_tokens = estimate_tool_tokens(&leader);
        let full_tokens = estimate_tool_tokens(&full);
        assert!(leader_tokens < full_tokens);
        assert!(leader_tokens * 2 < full_tokens * 3);
        assert!(canonical_tools_json(&leader).len() < canonical_tools_json(&full).len());
    }

    #[test]
    fn summary_payload_marks_unbundled_tools_without_panicking() {
        let mut custom = schema("custom_tool");
        custom.description = "custom".into();
        let frozen = freeze(
            select_bundle("worker", "coding", None),
            vec![custom],
            Vec::new(),
        );
        let summary = frozen.summary_payload(true);
        assert_eq!(summary["tools"][0]["category"], "unbundled");
        assert_eq!(summary["tools"][0]["workspace_write"], false);
        assert_eq!(summary["requested"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn telemetry_warnings_are_structured_and_bounded() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("trivial")),
            schemas(&["read_file"]),
            Vec::new(),
        );
        let payload = frozen.telemetry_payload();
        let warnings = payload["tool_bundle_warnings"].as_array().unwrap();
        assert!(!warnings.is_empty());
        for warning in warnings {
            assert!(warning["code"].as_str().is_some());
            assert!(warning["message"].as_str().is_some());
        }
        assert_eq!(payload["tool_bundle_omitted"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn catalog_rows_have_consistent_representatives() {
        for entry in catalog_entries() {
            assert!(!entry.title.is_empty());
            assert!(!entry.summary.is_empty());
            if let Some(limit) = entry.target_limit {
                assert!(
                    entry.requested_count <= limit,
                    "{} requested {} > {limit}",
                    entry.kind.as_str(),
                    entry.requested_count
                );
            }
            match entry.kind {
                ToolBundleKind::Leader => assert_eq!(entry.representative_role, "leader"),
                ToolBundleKind::Verification => assert_eq!(entry.representative_role, "verifier"),
                ToolBundleKind::Coding | ToolBundleKind::Minimal => {
                    assert_eq!(entry.representative_role, "worker")
                }
                ToolBundleKind::Research => assert_eq!(entry.representative_profile, "research"),
                ToolBundleKind::Full => assert_eq!(entry.requested_count, 0),
            }
        }
    }

    #[test]
    fn matrix_audit_is_clean_and_catalog_covers_every_partitioned_name() {
        assert!(audit_matrix().is_empty(), "{:?}", audit_matrix());
        for kind in [
            ToolBundleKind::Leader,
            ToolBundleKind::Coding,
            ToolBundleKind::Research,
            ToolBundleKind::Verification,
            ToolBundleKind::Minimal,
        ] {
            for (role, profile) in [
                ("leader", "coding"),
                ("manager", "coding"),
                ("worker", "coding"),
                ("worker", "research"),
                ("worker", "verification"),
                ("labour", "coding"),
                ("verifier", "verification"),
                ("scout", "research"),
            ] {
                let names = bundle_tool_names(kind, role, profile);
                let mut deduped = names.clone();
                deduped.dedup();
                assert_eq!(names, deduped);
                for name in names {
                    assert!(
                        catalog_entry(name).is_some(),
                        "{kind:?} {role}/{profile} missing metadata for {name}"
                    );
                }
            }
        }
    }

    #[test]
    fn definitions_are_stable_and_full_has_no_explicit_tools() {
        assert_eq!(definition(ToolBundleKind::Leader).tools, LEADER_TOOLS);
        assert_eq!(definition(ToolBundleKind::Coding).tools, CODING_TOOLS);
        assert_eq!(definition(ToolBundleKind::Research).tools, RESEARCH_TOOLS);
        assert_eq!(
            definition(ToolBundleKind::Verification).tools,
            VERIFICATION_TOOLS
        );
        assert_eq!(definition(ToolBundleKind::Minimal).tools, MINIMAL_TOOLS);
        assert!(definition(ToolBundleKind::Full).tools.is_empty());
        assert_eq!(definition(ToolBundleKind::Full).kind, ToolBundleKind::Full);
    }

    #[test]
    fn frozen_bundle_reports_schema_bytes_and_token_estimates() {
        let frozen = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file", "write_file", "shell", "edit_file"]),
            schemas(&["mcp__a__alpha"]),
        );
        assert!(frozen.schema_bytes() > 0);
        assert_eq!(
            frozen.schema_bytes(),
            canonical_tools_json(&frozen.tools).len() as u64
        );
        assert_eq!(frozen.estimated_tokens(), frozen.schema_bytes() / 4);
        let payload = frozen.telemetry_payload();
        assert_eq!(payload["tool_bundle_schema_bytes"], frozen.schema_bytes());
        assert_eq!(
            payload["tool_bundle_tokens_estimate"],
            frozen.estimated_tokens()
        );
        let summary = frozen.summary_payload(true);
        assert_eq!(summary["schema_bytes"], frozen.schema_bytes());
        assert_eq!(summary["estimated_tokens"], frozen.estimated_tokens());
    }

    #[test]
    fn every_task_kind_alias_maps_to_the_expected_bundle() {
        for kind in [
            "implementation",
            "implement",
            "implementing",
            "coding",
            "code",
            "edit",
            "fix",
            "bugfix",
            "bug_fix",
            "bug-fix",
            "refactor",
            "feature",
            "test",
            "testing",
            "build",
            "integration",
            "integrate",
        ] {
            assert_eq!(
                task_kind_bundle(kind),
                Some(ToolBundleKind::Coding),
                "{kind}"
            );
        }
        for kind in [
            "verification",
            "verify",
            "review",
            "audit",
            "validate",
            "validation",
            "check",
            "quality",
            "quality-gate",
        ] {
            assert_eq!(
                task_kind_bundle(kind),
                Some(ToolBundleKind::Verification),
                "{kind}"
            );
        }
        for kind in [
            "research",
            "investigate",
            "investigation",
            "discovery",
            "explore",
            "exploration",
            "spike",
            "analysis",
            "analyze",
            "design",
            "docs",
            "documentation",
        ] {
            assert_eq!(
                task_kind_bundle(kind),
                Some(ToolBundleKind::Research),
                "{kind}"
            );
        }
        for kind in [
            "minimal",
            "trivial",
            "typo",
            "format",
            "formatting",
            "rename",
            "comment",
            "comments",
            "lint",
            "one_file",
            "one-file",
            "onefile",
            "small",
            "chore",
        ] {
            assert_eq!(
                task_kind_bundle(kind),
                Some(ToolBundleKind::Minimal),
                "{kind}"
            );
        }
        for kind in ["", " ", "unknown", "epic", "discovery-of-unknowns"] {
            assert_eq!(task_kind_bundle(kind), None, "{kind}");
        }
    }

    #[test]
    fn requested_counts_for_standard_combinations_are_locked() {
        // These are the static request counts before role-policy resolution.
        // Actual runtime counts are asserted by the agent matrix test.
        let expected = [
            (ToolBundleKind::Leader, "leader", "generic", 49),
            (ToolBundleKind::Leader, "leader", "coding", 51),
            (ToolBundleKind::Leader, "manager", "coding", 44),
            (ToolBundleKind::Coding, "worker", "coding", 39),
            (ToolBundleKind::Coding, "labour", "coding", 37),
            (ToolBundleKind::Coding, "worker", "research", 37),
            (ToolBundleKind::Research, "worker", "research", 34),
            (ToolBundleKind::Research, "scout", "research", 33),
            (ToolBundleKind::Verification, "verifier", "verification", 38),
            (ToolBundleKind::Minimal, "worker", "coding", 6),
        ];
        for (kind, role, profile, count) in expected {
            assert_eq!(
                bundle_tool_names(kind, role, profile).len(),
                count,
                "{kind:?} {role}/{profile}"
            );
        }
    }

    #[test]
    fn every_bundle_name_list_is_sorted_and_duplicate_free() {
        for kind in TOOL_BUNDLE_DEFINITIONS
            .iter()
            .map(|definition| definition.kind)
        {
            for (role, profile) in [
                ("leader", "generic"),
                ("leader", "coding"),
                ("leader", "research"),
                ("manager", "coding"),
                ("worker", "coding"),
                ("worker", "research"),
                ("worker", "verification"),
                ("worker", "generic"),
                ("labour", "coding"),
                ("verifier", "verification"),
                ("scout", "research"),
            ] {
                let names = bundle_tool_names(kind, role, profile);
                let mut sorted = names.clone();
                sorted.sort_unstable();
                sorted.dedup();
                assert_eq!(names, sorted, "{kind:?} {role}/{profile}");
            }
        }
    }

    #[test]
    fn canonical_json_handles_empty_and_scalar_parameters() {
        for parameters in [
            json!({}),
            json!(null),
            json!(true),
            json!(42),
            json!("scalar"),
            json!([1, 2, 3]),
        ] {
            let mut tool = schema("edge");
            tool.parameters = parameters;
            let first = canonical_tool_json(&tool);
            assert_eq!(first, canonical_tool_json(&tool));
            assert!(first.starts_with(r#"{"name":"edge","description":"#));
            assert_eq!(tool_schema_hash(&tool).len(), 64);
        }
    }

    #[test]
    fn canonical_json_preserves_array_order_inside_parameter_objects() {
        let mut tool = schema("array_order");
        tool.parameters = json!({
            "type": "object",
            "properties": {
                "values": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"b": {"type": "string"}, "a": {"type": "string"}}
                    }
                }
            }
        });
        let json = canonical_tool_json(&tool);
        // Object keys are sorted recursively, but array order is untouched.
        assert!(json.contains(r#""properties":{"a":{"type":"string"},"b":{"type":"string"}}"#));
        assert_eq!(json.matches(r#""type":"array""#).count(), 1);
    }

    #[test]
    fn canonical_json_handles_numbers_booleans_and_null_members() {
        let mut tool = schema("mixed");
        tool.parameters = json!({
            "type": "object",
            "properties": {
                "flag": {"type": "boolean", "default": true},
                "none": {"type": "null"},
                "count": {"type": "integer", "default": 7},
                "ratio": {"type": "number", "default": 1.5}
            },
            "additionalProperties": false
        });
        let json = canonical_tool_json(&tool);
        assert!(json.contains(r#""additionalProperties":false"#));
        assert!(json.contains(r#""default":true"#));
        assert!(json.contains(r#""default":7"#));
        assert!(json.contains(r#""default":1.5"#));
        assert!(json.contains(r#""type":"null""#));
        let again = canonical_tool_json(&tool);
        assert_eq!(json, again);
    }

    #[test]
    fn warning_payload_shape_is_stable() {
        let warning = ToolBundleWarning::new("tool_bundle_omitted", "missing write_file");
        let payload = warning.payload();
        assert_eq!(payload["code"], "tool_bundle_omitted");
        assert_eq!(payload["message"], "missing write_file");
        assert_eq!(payload.as_object().unwrap().len(), 2);
    }

    #[test]
    fn catalog_lookup_is_exact_and_static() {
        assert!(catalog_entry("read_file").is_some());
        assert!(catalog_entry("Read_File").is_none());
        assert!(catalog_entry("not_a_tool").is_none());
        let before = catalog_entry("read_file").unwrap().name;
        let _ = bundle_tool_names(ToolBundleKind::Leader, "leader", "generic");
        let after = catalog_entry("read_file").unwrap().name;
        assert_eq!(before, after);
    }

    #[test]
    fn tool_schema_hash_binds_the_tool_name() {
        let mut alpha = schema("alpha");
        let mut beta = schema("beta");
        alpha.parameters = json!({"type": "object"});
        beta.parameters = json!({"type": "object"});
        assert_ne!(tool_schema_hash(&alpha), tool_schema_hash(&beta));
    }

    #[test]
    fn full_bundle_has_no_target_and_no_explicit_requests() {
        let frozen = freeze(
            select_bundle("worker", "coding", None),
            schemas(&["read_file", "write_file"]),
            Vec::new(),
        );
        assert_eq!(frozen.target_limit, None);
        assert_eq!(frozen.within_target(), None);
        assert!(frozen.requested.is_empty());
        assert!(frozen.omitted.is_empty());
        assert!(!frozen.selection.kind.is_partitioned());
    }

    #[test]
    fn requested_names_are_always_catalogued_for_partitioned_bundles() {
        for kind in [
            ToolBundleKind::Leader,
            ToolBundleKind::Coding,
            ToolBundleKind::Research,
            ToolBundleKind::Verification,
            ToolBundleKind::Minimal,
        ] {
            for (role, profile) in [
                ("leader", "generic"),
                ("leader", "coding"),
                ("manager", "coding"),
                ("worker", "coding"),
                ("worker", "research"),
                ("worker", "verification"),
                ("labour", "coding"),
                ("verifier", "verification"),
                ("scout", "research"),
            ] {
                for name in bundle_tool_names(kind, role, profile) {
                    assert!(
                        catalog_entry(name).is_some(),
                        "{kind:?} {role}/{profile} requests uncatalogued {name}"
                    );
                }
            }
        }
    }

    #[test]
    fn minimal_never_accepts_dynamic_mcp_even_under_the_cap() {
        let mcp = (0..MAX_BUNDLE_MCP_TOOLS)
            .map(|index| schema(&format!("mcp__srv__tool_{index:02}")))
            .collect::<Vec<_>>();
        let frozen = freeze(
            select_bundle("worker", "coding", Some("trivial")),
            schemas(&["read_file", "write_file", "edit_file", "shell", "list_dir"]),
            mcp,
        );
        assert_eq!(frozen.mcp_count(), 0);
        assert_eq!(frozen.tool_count(), 5);
        assert!(
            frozen
                .warnings
                .iter()
                .any(|warning| warning.code == "tool_bundle_mcp_excluded")
        );
    }

    #[test]
    fn selection_payload_includes_fallback_and_task_kind() {
        let fallback = select_bundle("worker", "coding", Some("unknown"));
        let payload = fallback.payload();
        assert_eq!(payload["bundle"], "full");
        assert_eq!(payload["fallback"], true);
        assert_eq!(payload["task_kind"], "unknown");
        assert_eq!(payload["reason"], "task_kind_unknown_full");
    }

    #[test]
    fn bundle_hash_excludes_mcp_but_tools_hash_includes_it() {
        let selection = select_bundle("worker", "coding", Some("implementation"));
        let builtins = schemas(&["read_file", "write_file", "shell", "edit_file"]);
        let without = freeze(selection.clone(), builtins.clone(), Vec::new());
        let with = freeze(
            selection,
            builtins,
            schemas(&["mcp__a__alpha", "mcp__z__zeta"]),
        );
        assert_eq!(without.bundle_hash, with.bundle_hash);
        assert_ne!(without.tools_hash, with.tools_hash);
        assert_eq!(without.builtin_count(), with.builtin_count());
        assert_eq!(with.mcp_count(), 2);
    }

    #[test]
    fn category_counts_cover_mcp_and_unbundled_tools() {
        let frozen = freeze(
            select_bundle("worker", "coding", None),
            vec![schema("custom_tool")],
            vec![schema("mcp__a__alpha")],
        );
        let counts = frozen.category_counts();
        assert!(counts.iter().any(|count| count.category == "unbundled"));
        assert!(counts.iter().any(|count| count.category == "mcp"));
        assert_eq!(counts.iter().map(|count| count.tools).sum::<usize>(), 2);
    }

    #[test]
    fn canonical_hash_is_stable_for_a_large_nested_schema() {
        let mut tool = schema("large");
        let mut properties = serde_json::Map::new();
        for index in 0..100 {
            properties.insert(
                format!("field_{index:03}"),
                json!({"type": "string", "description": format!("field {index}")}),
            );
        }
        tool.parameters = json!({
            "type": "object",
            "properties": properties,
            "required": (0..100).map(|index| format!("field_{index:03}")).collect::<Vec<_>>()
        });
        let first = canonical_tool_json(&tool);
        let mut rebuilt = tool.clone();
        // Reverse the in-memory object order; canonicalization must restore it.
        if let Some(object) = rebuilt
            .parameters
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        {
            let mut entries = object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>();
            entries.reverse();
            object.clear();
            for (key, value) in entries {
                object.insert(key, value);
            }
        }
        let second = canonical_tool_json(&rebuilt);
        assert_eq!(first, second);
        assert_eq!(tool_schema_hash(&tool), tool_schema_hash(&rebuilt));
        assert!(first.contains("field_000"));
        assert!(first.contains("field_099"));
    }

    #[test]
    fn empty_tool_list_has_a_stable_empty_array_hash() {
        assert_eq!(canonical_tools_json(&[]), "[]");
        assert_eq!(
            canonical_tools_hash(&[]),
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
        );
        assert_eq!(estimate_tool_tokens(&[]), 0);
    }

    #[test]
    fn bundle_hash_changes_with_task_kind_even_when_tools_match() {
        let tools = schemas(&["read_file"]);
        let minimal = freeze(
            select_bundle("worker", "coding", Some("trivial")),
            tools.clone(),
            Vec::new(),
        );
        let coding = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            tools,
            Vec::new(),
        );
        // Both previews select the same available schema, but the selection
        // identity (kind + task kind) differs and must change the bundle hash.
        assert_eq!(minimal.tool_names(), coding.tool_names());
        assert_ne!(minimal.bundle_hash, coding.bundle_hash);
        assert_eq!(minimal.tools_hash, coding.tools_hash);
    }

    #[test]
    fn freeze_warning_order_is_deterministic() {
        let run = || {
            freeze(
                select_bundle("worker", "coding", Some("trivial")),
                schemas(&["read_file"]),
                Vec::new(),
            )
        };
        let first = run();
        let second = run();
        assert_eq!(first.warnings, second.warnings);
        assert_eq!(first.omitted, second.omitted);
        assert_eq!(first.bundle_hash, second.bundle_hash);
    }

    #[test]
    fn freeze_does_not_mutate_the_static_definitions() {
        let before = LEADER_TOOLS.len();
        let _ = freeze(
            select_bundle("worker", "coding", Some("implementation")),
            schemas(&["read_file"]),
            Vec::new(),
        );
        let _ = bundle_tool_names(ToolBundleKind::Leader, "leader", "coding");
        assert_eq!(LEADER_TOOLS.len(), before);
        assert_eq!(LEADER_TOOLS, definition(ToolBundleKind::Leader).tools);
    }

    #[test]
    fn selection_payload_is_bounded() {
        let selection = select_bundle("worker", "coding", Some("implementation"));
        let payload = selection.payload();
        assert_eq!(payload["bundle"], "coding");
        assert_eq!(payload["role"], "worker");
        assert_eq!(payload["profile"], "coding");
        assert_eq!(payload["task_kind"], "implementation");
        assert_eq!(payload["fallback"], false);
    }
}
