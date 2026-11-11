//! Deterministic approval templates, deduplication identity, and attention
//! policy.
//!
//! This module owns the *decision* half of H1. It never persists and never
//! waits: the store owns atomic use consumption and row state, while the engine
//! owns the waiter registry. Keeping the rule evaluation pure makes the
//! precedence matrix reproducible and testable without a database.
//!
//! Precedence is deliberately narrow and ordered:
//!
//! ```text
//! explicit deny (blocked assessment / sensitive deny policy)
//!   > template deny
//!   > exact ApprovalGrant
//!   > template allow (or escalate)
//!   > risk policy (smart/strict/unattended)
//! ```
//!
//! Deny always wins, unknown risk/tool/path fails closed, and templates can
//! never auto-approve Destructive, HostState, remote tools, or a sensitive GUI
//! target. An `escalate` template only forces a human; it never approves.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use vp_protocol::types::ApprovalMode;
use vp_protocol::types::{Approval, ApprovalGrant, ApprovalTemplate, AttentionPolicy};

use crate::permissions::{ApprovalDecision, PermissionAssessment, RiskLevel, decide};

/// Bound on templates returned/evaluated for one project. The store enforces
/// the same cap at creation so a hand-edited database cannot create an
/// unbounded rule engine.
pub const MAX_APPROVAL_TEMPLATES: usize = 100;
/// Bound on path globs in one template.
pub const MAX_TEMPLATE_PATH_GLOBS: usize = 16;
/// Bound on tool-pattern characters.
pub const MAX_TOOL_PATTERN_CHARS: usize = 120;
/// Bound on one path glob.
pub const MAX_PATH_GLOB_CHARS: usize = 300;
/// Bounded wildcard subset: a pattern with more metacharacters is rejected.
pub const MAX_TOOL_WILDCARDS: usize = 4;
/// Maximum path candidates extracted from one tool input.
pub const MAX_INPUT_PATHS: usize = 32;
/// Maximum number of automatic use caps accepted from one template.
pub const MAX_TEMPLATE_USES: u32 = 100_000;
/// Maximum future expiry accepted for a template (366 days).
pub const MAX_TEMPLATE_LIFETIME_SECS: i64 = 366 * 24 * 60 * 60;
/// Maximum number of approvals accepted in one batch resolution.
pub const MAX_BATCH_APPROVALS: usize = 100;

const LOW_RISKS: [&str; 2] = ["external", "workspace_write"];

pub const APPROVAL_NOT_FOUND: &str = "APPROVAL_NOT_FOUND";
pub const APPROVAL_NOT_PENDING: &str = "APPROVAL_NOT_PENDING";
pub const APPROVAL_TEMPLATE_INVALID: &str = "APPROVAL_TEMPLATE_INVALID";
pub const APPROVAL_TEMPLATE_EXHAUSTED: &str = "APPROVAL_TEMPLATE_EXHAUSTED";
pub const APPROVAL_TEMPLATE_EXPIRED: &str = "APPROVAL_TEMPLATE_EXPIRED";
pub const APPROVAL_TEMPLATE_FORBIDDEN_RISK: &str = "APPROVAL_TEMPLATE_FORBIDDEN_RISK";
pub const APPROVAL_POLICY_INVALID: &str = "APPROVAL_POLICY_INVALID";

/// Bounded hint/retryability for approval error codes. This keeps the approval
/// surface in the Q3 error *shape* without growing the static tool catalog or
/// changing `tool_help` output.
pub fn error_hint(code: &str) -> (&'static str, bool) {
    match code {
        APPROVAL_NOT_FOUND => (
            "Refresh the approval queue; the id no longer exists or was already pruned.",
            false,
        ),
        APPROVAL_NOT_PENDING => (
            "Refresh the queue before deciding; the approval already has a terminal status.",
            false,
        ),
        APPROVAL_TEMPLATE_INVALID => (
            "Use a bounded tool glob, known risk levels, relative path globs, and a positive use cap.",
            false,
        ),
        APPROVAL_TEMPLATE_EXHAUSTED => (
            "The template is inert; raise max_uses or create a new bounded template.",
            false,
        ),
        APPROVAL_TEMPLATE_EXPIRED => (
            "The template is inert; set a future expiry or remove the expiry.",
            false,
        ),
        APPROVAL_TEMPLATE_FORBIDDEN_RISK => (
            "Allow templates cover only external and workspace_write risk; Destructive, HostState, remote, and sensitive GUI targets always require the operator.",
            false,
        ),
        APPROVAL_POLICY_INVALID => (
            "Use bounded attention values: 1-500 pending, 0-3600s coalescing, 60s-7d stale, and a known risk threshold.",
            false,
        ),
        _ => (
            "Refresh the approval queue and retry with an explicit id.",
            false,
        ),
    }
}

/// Stable typed rule error. The code is a stable approval wire value rendered
/// with the Q3 `{code,message,retryable,hint}` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRuleError {
    pub code: &'static str,
    pub message: String,
}

impl ApprovalRuleError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(APPROVAL_TEMPLATE_INVALID, message)
    }

    pub fn forbidden_risk(message: impl Into<String>) -> Self {
        Self::new(APPROVAL_TEMPLATE_FORBIDDEN_RISK, message)
    }

    pub fn policy(message: impl Into<String>) -> Self {
        Self::new(APPROVAL_POLICY_INVALID, message)
    }

    /// Bounded JSON shape used by RPC error `data` and CLI `--json` failures.
    pub fn data(&self) -> serde_json::Value {
        let (hint, retryable) = error_hint(self.code);
        serde_json::json!({
            "code": self.code,
            "message": self.message,
            "retryable": retryable,
            "hint": hint,
        })
    }
}

impl fmt::Display for ApprovalRuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApprovalRuleError {}

/// Parsed template action. Unknown actions fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalTemplateAction {
    Allow,
    Deny,
    Escalate,
}

impl ApprovalTemplateAction {
    pub fn parse(value: &str) -> Result<Self, ApprovalRuleError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(Self::Allow),
            "deny" => Ok(Self::Deny),
            "escalate" => Ok(Self::Escalate),
            other => Err(ApprovalRuleError::invalid(format!(
                "approval template action '{other}' must be allow, deny, or escalate"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Escalate => "escalate",
        }
    }
}

/// Why a decision was made. This is persisted in audit events and shown in the
/// approval queue; it is not inferred later from the reason string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionSource {
    ExplicitDeny,
    TemplateDeny,
    Grant,
    TemplateAllow,
    TemplateEscalate,
    RiskPolicy,
}

impl ApprovalDecisionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitDeny => "explicit_deny",
            Self::TemplateDeny => "template_deny",
            Self::Grant => "grant",
            Self::TemplateAllow => "template_allow",
            Self::TemplateEscalate => "template_escalate",
            Self::RiskPolicy => "risk_policy",
        }
    }
}

/// One evaluated decision with enough context to audit it. `remaining_uses`
/// is `None` for unlimited or non-template sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalEvaluation {
    pub decision: ApprovalDecision,
    pub source: ApprovalDecisionSource,
    pub template_id: Option<String>,
    pub template_name: Option<String>,
    pub remaining_uses: Option<u32>,
    pub scope: Vec<String>,
    pub reason: String,
}

impl ApprovalEvaluation {
    fn new(
        decision: ApprovalDecision,
        source: ApprovalDecisionSource,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            decision,
            source,
            template_id: None,
            template_name: None,
            remaining_uses: None,
            scope: Vec::new(),
            reason: reason.into(),
        }
    }

    pub fn risk(&self) -> RiskLevel {
        self.decision.risk()
    }

    pub fn requires_approval(&self) -> bool {
        matches!(self.decision, ApprovalDecision::Ask { .. })
    }
}

/// Callback used by the engine to atomically consume a template use. `None`
/// means the template is inert (disabled, expired, or exhausted); `Some(None)`
/// means unlimited.
pub type TemplateConsume<'a> = &'a mut dyn FnMut(&ApprovalTemplate) -> Option<Option<u32>>;

/// Evaluate the full precedence matrix. The callback is called only for a
/// template that is structurally applicable and has passed the hard-risk
/// guards. It must perform an atomic store update; returning `None` makes the
/// evaluator continue to the next candidate.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_approval_rules(
    templates: &[ApprovalTemplate],
    grants: &[ApprovalGrant],
    assessment: &PermissionAssessment,
    tool: &str,
    input: &Value,
    mode: ApprovalMode,
    auto_approve: bool,
    now: OffsetDateTime,
    consume: TemplateConsume<'_>,
) -> ApprovalEvaluation {
    let risk = assessment.risk;
    let paths = extract_input_paths(input);

    // 1. Explicit deny. Blocked assessments never reach a human and never
    // consult a template or grant.
    if assessment.blocked {
        return ApprovalEvaluation::new(
            ApprovalDecision::Deny {
                risk,
                reason: assessment.reason.clone(),
            },
            ApprovalDecisionSource::ExplicitDeny,
            assessment.reason.clone(),
        );
    }

    // 2. Template deny, including high-risk and remote tools. Deny is safe to
    // apply broadly; the evaluator still requires a structurally valid match.
    let deny_candidates = matching_templates(
        templates,
        tool,
        input,
        risk,
        &paths,
        now,
        Some(ApprovalTemplateAction::Deny),
    );
    for template in deny_candidates {
        if let Some(remaining) = consume(template) {
            let mut evaluation = ApprovalEvaluation::new(
                ApprovalDecision::Deny {
                    risk,
                    reason: format!(
                        "approval template '{}' denies {} ({})",
                        template.name,
                        tool,
                        risk.as_str()
                    ),
                },
                ApprovalDecisionSource::TemplateDeny,
                format!("matched deny template '{}'", template.name),
            );
            evaluation.template_id = Some(template.id.clone());
            evaluation.template_name = Some(template.name.clone());
            evaluation.remaining_uses = remaining;
            evaluation.scope = template.path_globs.clone();
            return evaluation;
        }
    }

    // Internal auto-approval is an explicit bypass of routine prompts, not a
    // reason to widen template coverage. It still honors deny templates.
    if auto_approve {
        let decision = decide(mode, assessment, true);
        return ApprovalEvaluation::new(
            decision,
            ApprovalDecisionSource::RiskPolicy,
            assessment.reason.clone(),
        );
    }

    // 3. Exact project grant. Grants are local path prefixes; they cannot cover
    // high-risk operations or a remote host.
    if risk <= RiskLevel::External
        && !tool.starts_with("remote_")
        && let Some(grant) = match_grant(grants, tool, &paths)
    {
        let reason = format!(
            "matched project approval grant {} for {}{}",
            grant.id,
            grant.tool,
            if grant.include_children {
                " (subfolders)"
            } else {
                ""
            }
        );
        return ApprovalEvaluation::new(
            ApprovalDecision::Allow {
                risk,
                reason: reason.clone(),
            },
            ApprovalDecisionSource::Grant,
            reason,
        );
    }

    // 4. Template allow/escalate. Allow templates are hard-capped to low/medium
    // risk and are never applied to remote tools. An exhausted or expired
    // template is skipped; the next matching template may apply.
    let allow_candidates = matching_templates(templates, tool, input, risk, &paths, now, None);
    for template in allow_candidates {
        let Ok(action) = ApprovalTemplateAction::parse(&template.action) else {
            continue;
        };
        if action == ApprovalTemplateAction::Deny {
            continue;
        }
        if !template_can_allow(template, tool, risk) {
            continue;
        }
        if let Some(remaining) = consume(template) {
            let (decision, source) = match action {
                ApprovalTemplateAction::Allow => (
                    ApprovalDecision::Allow {
                        risk,
                        reason: format!(
                            "approval template '{}' allows {} ({})",
                            template.name,
                            tool,
                            risk.as_str()
                        ),
                    },
                    ApprovalDecisionSource::TemplateAllow,
                ),
                ApprovalTemplateAction::Escalate => (
                    ApprovalDecision::Ask {
                        risk,
                        reason: format!(
                            "approval template '{}' escalates {} to the operator",
                            template.name, tool
                        ),
                    },
                    ApprovalDecisionSource::TemplateEscalate,
                ),
                ApprovalTemplateAction::Deny => (
                    ApprovalDecision::Deny {
                        risk,
                        reason: format!(
                            "approval template '{}' denies {} ({})",
                            template.name,
                            tool,
                            risk.as_str()
                        ),
                    },
                    ApprovalDecisionSource::TemplateDeny,
                ),
            };
            let mut evaluation = ApprovalEvaluation::new(
                decision,
                source,
                format!("matched {} template '{}'", action.as_str(), template.name),
            );
            evaluation.template_id = Some(template.id.clone());
            evaluation.template_name = Some(template.name.clone());
            evaluation.remaining_uses = remaining;
            evaluation.scope = template.path_globs.clone();
            return evaluation;
        }
    }

    // 5. Risk policy.
    let decision = decide(mode, assessment, false);
    ApprovalEvaluation::new(
        decision,
        ApprovalDecisionSource::RiskPolicy,
        assessment.reason.clone(),
    )
}

/// Structurally matching templates in deterministic specificity order. A
/// template that names an unknown risk or has a malformed pattern never
/// appears here.
#[allow(clippy::too_many_arguments)]
pub fn matching_templates<'a>(
    templates: &'a [ApprovalTemplate],
    tool: &str,
    _input: &Value,
    risk: RiskLevel,
    paths: &[String],
    now: OffsetDateTime,
    action: Option<ApprovalTemplateAction>,
) -> Vec<&'a ApprovalTemplate> {
    let mut matches = templates
        .iter()
        .filter(|template| template_is_live(template, now))
        .filter(|template| {
            action
                .is_none_or(|wanted| ApprovalTemplateAction::parse(&template.action) == Ok(wanted))
        })
        .filter(|template| tool_pattern_matches(&template.tool_pattern, tool))
        .filter(|template| template_risk_matches(template, risk))
        .filter(|template| template_paths_match(template, paths))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        template_specificity(right)
            .cmp(&template_specificity(left))
            .then_with(|| left.id.cmp(&right.id))
    });
    matches
}

/// A live template is enabled, has a valid future expiry (if any), and has
/// remaining uses (if capped). Invalid expiry fails closed (inert).
pub fn template_is_live(template: &ApprovalTemplate, now: OffsetDateTime) -> bool {
    if !template.enabled {
        return false;
    }
    // Hand-edited rows are inert unless they satisfy the same bounded grammar
    // as creation-time templates. This prevents `*` or an absolute `..` glob
    // from becoming a broader rule after an upgrade or manual edit.
    if validate_tool_pattern(&template.tool_pattern).is_err()
        || validate_path_globs(&template.path_globs).is_err()
    {
        return false;
    }
    if let Some(max) = template.max_uses
        && (max == 0 || template.used_count >= max)
    {
        return false;
    }
    match template.expires_at.as_deref() {
        Some(value) => parse_timestamp(value).is_some_and(|expiry| expiry > now),
        None => true,
    }
}

/// Hard guards for an `allow` template. This is intentionally redundant with
/// creation validation: a hand-edited SQLite row must not widen approval.
pub fn template_can_allow(template: &ApprovalTemplate, tool: &str, risk: RiskLevel) -> bool {
    if risk > RiskLevel::External {
        return false;
    }
    if tool.starts_with("remote_") {
        return false;
    }
    if matches!(tool, "mcp" | "computer" | "browser") && risk > RiskLevel::WorkspaceWrite {
        return false;
    }
    template
        .risk_levels
        .iter()
        .filter_map(|level| parse_risk(level))
        .all(|level| level <= RiskLevel::External)
}

pub fn template_risk_matches(template: &ApprovalTemplate, risk: RiskLevel) -> bool {
    if template.risk_levels.is_empty() {
        return LOW_RISKS
            .iter()
            .filter_map(|level| parse_risk(level))
            .any(|level| level == risk);
    }
    template
        .risk_levels
        .iter()
        .any(|level| parse_risk(level) == Some(risk))
}

pub fn template_paths_match(template: &ApprovalTemplate, paths: &[String]) -> bool {
    if template.path_globs.is_empty() {
        return true;
    }
    if paths.is_empty() {
        return false;
    }
    paths.iter().any(|path| {
        template
            .path_globs
            .iter()
            .any(|glob| path_glob_matches(glob, path))
    })
}

fn template_specificity(template: &ApprovalTemplate) -> i64 {
    let pattern = &template.tool_pattern;
    let wildcards = pattern
        .chars()
        .filter(|character| matches!(character, '*' | '?'))
        .count() as i64;
    let pattern_score = if wildcards == 0 {
        3_000
    } else if pattern.ends_with('*') && !pattern.contains('?') && pattern.matches('*').count() == 1
    {
        2_000
    } else {
        1_000
    };
    let path_score = template
        .path_globs
        .iter()
        .map(|glob| {
            if glob.contains('*') || glob.contains('?') {
                10
            } else {
                50
            }
        })
        .sum::<i64>();
    pattern_score + path_score - wildcards * 5 + if template.enabled { 1 } else { 0 }
}

/// Match an exact tool name, a bounded `*`/`?` glob, or a prefix glob. The
/// matcher is anchored at both ends and never treats `/` specially (tool names
/// are not paths).
pub fn tool_pattern_matches(pattern: &str, tool: &str) -> bool {
    wildcard_match(pattern.as_bytes(), tool.as_bytes(), false)
}

/// Match a relative path glob. `*` does not cross `/`; `**` does. `?` matches
/// one non-separator character. Patterns are anchored.
pub fn path_glob_matches(pattern: &str, path: &str) -> bool {
    let mut pattern = pattern.replace('\\', "/");
    while let Some(stripped) = pattern.strip_prefix("./") {
        pattern = stripped.to_string();
    }
    let path = path.replace('\\', "/").trim_start_matches("./").to_string();
    path_glob_matches_inner(&pattern, &path, 0)
}

fn path_glob_matches_inner(pattern: &str, path: &str, depth: usize) -> bool {
    // `**/` conventionally matches zero or more directories, including zero.
    // Expand the zero-directory alternative before the general wildcard DP.
    if depth <= 8
        && let Some(index) = pattern.find("**/")
    {
        let mut without = String::with_capacity(pattern.len().saturating_sub(3));
        without.push_str(&pattern[..index]);
        without.push_str(&pattern[index + 3..]);
        if path_glob_matches_inner(&without, path, depth + 1) {
            return true;
        }
    }
    wildcard_match(pattern.as_bytes(), path.as_bytes(), true)
}

fn wildcard_match(pattern: &[u8], text: &[u8], path_mode: bool) -> bool {
    // Bounded dynamic programming for a small glob subset. The lengths are
    // capped at creation, but the guard keeps a hand-edited row from
    // allocating without bound.
    if pattern.len() > 1_000 || text.len() > 8_192 {
        return false;
    }
    let mut previous = vec![false; text.len() + 1];
    previous[0] = true;
    let mut index = 0;
    while index < pattern.len() {
        let byte = pattern[index];
        let mut current = vec![false; text.len() + 1];
        match byte {
            b'*' => {
                let double = index + 1 < pattern.len() && pattern[index + 1] == b'*';
                if double {
                    index += 1;
                }
                current[0] = previous[0];
                for position in 1..=text.len() {
                    let crosses = !path_mode || double || text[position - 1] != b'/';
                    current[position] = previous[position] || (current[position - 1] && crosses);
                }
            }
            b'?' => {
                for position in 1..=text.len() {
                    let allowed = !path_mode || text[position - 1] != b'/';
                    current[position] = previous[position - 1] && allowed;
                }
            }
            literal => {
                for position in 1..=text.len() {
                    current[position] = previous[position - 1] && text[position - 1] == literal;
                }
            }
        }
        previous = current;
        index += 1;
    }
    previous[text.len()]
}

fn parse_risk(value: &str) -> Option<RiskLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "read_only" => Some(RiskLevel::ReadOnly),
        "workspace_write" => Some(RiskLevel::WorkspaceWrite),
        "external" => Some(RiskLevel::External),
        "host_state" => Some(RiskLevel::HostState),
        "destructive" => Some(RiskLevel::Destructive),
        _ => None,
    }
}

fn parse_timestamp(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
}

/// Exact project grant match. Only path-like inputs are considered, so a shell
/// command or opaque tool never gets a grant it did not explicitly receive.
pub fn match_grant<'a>(
    grants: &'a [ApprovalGrant],
    tool: &str,
    paths: &[String],
) -> Option<&'a ApprovalGrant> {
    if paths.is_empty() {
        return None;
    }
    grants.iter().find(|grant| {
        (grant.tool == tool || grant.tool == "*")
            && paths.iter().any(|path| {
                let path = path.trim().trim_start_matches("./");
                if grant.include_children {
                    path == grant.path_prefix
                        || path
                            .starts_with(&format!("{}/", grant.path_prefix.trim_end_matches('/')))
                } else {
                    path == grant.path_prefix
                }
            })
    })
}

/// Path-like values from a tool input. This is deliberately a key-name
/// heuristic with a hard cap; it is not a filesystem parser.
pub fn extract_input_paths(input: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_paths(input, None, &mut paths, 0);
    paths.sort();
    paths.dedup();
    paths.truncate(MAX_INPUT_PATHS);
    paths
}

fn collect_paths(value: &Value, key: Option<&str>, paths: &mut Vec<String>, depth: usize) {
    if paths.len() >= MAX_INPUT_PATHS || depth > 8 {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                collect_paths(value, Some(key), paths, depth + 1);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_paths(item, key, paths, depth + 1);
            }
        }
        Value::String(text)
            if key.is_some_and(path_like_key)
                && !text.is_empty()
                && text.len() <= MAX_PATH_GLOB_CHARS =>
        {
            paths.push(text.trim().replace('\\', "/"));
        }
        _ => {}
    }
}

fn path_like_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "path"
            | "paths"
            | "file"
            | "files"
            | "file_path"
            | "filepath"
            | "filename"
            | "worktree_path"
            | "cwd"
            | "target"
            | "destination"
            | "workdir"
    ) || key.ends_with("_path")
        || key.ends_with("_paths")
}

/// Canonical JSON with recursively sorted object keys. Used only for hashing;
/// the original input remains the reviewable payload.
pub fn canonical_json(value: &Value) -> String {
    let mut output = String::new();
    write_canonical(value, &mut output);
    output
}

fn write_canonical(value: &Value, output: &mut String) {
    match value {
        Value::Object(object) => {
            let sorted = object
                .iter()
                .map(|(key, value)| (key.clone(), value))
                .collect::<BTreeMap<_, _>>();
            output.push('{');
            for (index, (key, value)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(&key).unwrap_or_default());
                output.push(':');
                write_canonical(value, output);
            }
            output.push('}');
        }
        Value::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical(item, output);
            }
            output.push(']');
        }
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => output.push_str(&number.to_string()),
        Value::String(text) => output.push_str(&serde_json::to_string(text).unwrap_or_default()),
    }
}

pub fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Canonical input fingerprint. Command text and file contents are hashed, not
/// copied into this value.
pub fn approval_scope_hash(input: &Value) -> String {
    sha256_hex(&canonical_json(input))
}

/// Stable identity for deduplication: session + agent + tool + canonical input
/// scope + risk. The final SHA-256 keeps the key bounded and avoids injecting
/// tool names or paths directly into indexes.
pub fn approval_dedup_key(
    session_id: &str,
    agent: Option<&Value>,
    tool: &str,
    scope_hash: &str,
    risk: &str,
) -> String {
    let agent_id = agent_identity(agent);
    sha256_hex(&format!(
        "{session_id}\u{1f}{agent_id}\u{1f}{tool}\u{1f}{scope_hash}\u{1f}{risk}"
    ))
}

pub fn agent_identity(agent: Option<&Value>) -> String {
    agent
        .and_then(|agent| {
            agent
                .get("agent_id")
                .and_then(Value::as_str)
                .or_else(|| agent.get("agent_name").and_then(Value::as_str))
        })
        .filter(|value| !value.is_empty())
        .unwrap_or("system")
        .to_string()
}

/// Validate and normalize a tool pattern. Rejecting a pattern is safer than
/// silently treating it as an exact tool name.
pub fn validate_tool_pattern(value: &str) -> Result<String, ApprovalRuleError> {
    let pattern = value.trim();
    if pattern.is_empty() || pattern.chars().count() > MAX_TOOL_PATTERN_CHARS {
        return Err(ApprovalRuleError::invalid(format!(
            "tool pattern must be 1-{MAX_TOOL_PATTERN_CHARS} characters"
        )));
    }
    if pattern.chars().any(char::is_control) {
        return Err(ApprovalRuleError::invalid(
            "tool pattern cannot contain control characters",
        ));
    }
    let wildcards = pattern
        .chars()
        .filter(|character| matches!(character, '*' | '?'))
        .count();
    if wildcards > MAX_TOOL_WILDCARDS {
        return Err(ApprovalRuleError::invalid(format!(
            "tool pattern supports at most {MAX_TOOL_WILDCARDS} wildcards"
        )));
    }
    if pattern.contains("**") {
        return Err(ApprovalRuleError::invalid(
            "tool patterns support single-character `*` only, not `**`",
        ));
    }
    let literal = pattern
        .chars()
        .filter(|character| !matches!(character, '*' | '?'))
        .count();
    if literal < 2 {
        return Err(ApprovalRuleError::invalid(
            "tool pattern needs at least two literal characters so `*` cannot match every tool",
        ));
    }
    if !pattern.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':' | '*' | '?')
    }) {
        return Err(ApprovalRuleError::invalid(
            "tool pattern contains an unsupported character",
        ));
    }
    Ok(pattern.to_string())
}

/// Validate and normalize path globs. Paths are relative to the worktree; no
/// absolute path, home expansion, parent traversal, or control character is
/// accepted.
pub fn validate_path_globs(globs: &[String]) -> Result<Vec<String>, ApprovalRuleError> {
    if globs.len() > MAX_TEMPLATE_PATH_GLOBS {
        return Err(ApprovalRuleError::invalid(format!(
            "at most {MAX_TEMPLATE_PATH_GLOBS} path globs are allowed"
        )));
    }
    let mut normalized = Vec::with_capacity(globs.len());
    for glob in globs {
        let mut glob = glob.trim().replace('\\', "/");
        while let Some(stripped) = glob.strip_prefix("./") {
            glob = stripped.to_string();
        }
        if glob.is_empty() || glob.chars().count() > MAX_PATH_GLOB_CHARS {
            return Err(ApprovalRuleError::invalid(format!(
                "path glob must be 1-{MAX_PATH_GLOB_CHARS} characters"
            )));
        }
        if glob.chars().any(char::is_control) {
            return Err(ApprovalRuleError::invalid(
                "path glob cannot contain control characters",
            ));
        }
        if glob.starts_with('/') || glob.starts_with('~') || glob.contains(':') {
            return Err(ApprovalRuleError::invalid(
                "path glob must be relative to the session worktree",
            ));
        }
        if glob.split('/').any(|component| component == "..") {
            return Err(ApprovalRuleError::invalid("path glob cannot contain `..`"));
        }
        if glob.split('/').any(|component| component.is_empty()) {
            return Err(ApprovalRuleError::invalid(
                "path glob cannot contain an empty path component",
            ));
        }
        normalized.push(glob);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

/// Validate risk names and apply the allow-action cap. Empty defaults to the
/// two low/medium risks. Deny/escalate templates may name high risks, but an
/// allow template that names one is rejected rather than silently clamped.
pub fn validate_risk_levels(
    levels: &[String],
    action: ApprovalTemplateAction,
) -> Result<Vec<String>, ApprovalRuleError> {
    let mut normalized = if levels.is_empty() {
        LOW_RISKS.iter().map(|level| level.to_string()).collect()
    } else {
        levels
            .iter()
            .map(|level| level.trim().to_ascii_lowercase())
            .collect::<Vec<_>>()
    };
    for level in &normalized {
        if parse_risk(level).is_none() {
            return Err(ApprovalRuleError::invalid(format!(
                "unknown approval risk '{level}'"
            )));
        }
    }
    normalized.sort();
    normalized.dedup();
    if action == ApprovalTemplateAction::Allow
        && normalized
            .iter()
            .any(|level| parse_risk(level).is_some_and(|risk| risk > RiskLevel::External))
    {
        return Err(ApprovalRuleError::forbidden_risk(
            "allow templates may cover only external or workspace_write risk",
        ));
    }
    Ok(normalized)
}

pub fn validate_template_expiry(
    expires_at: Option<&str>,
    now: OffsetDateTime,
) -> Result<Option<String>, ApprovalRuleError> {
    let Some(expires_at) = expires_at else {
        return Ok(None);
    };
    let parsed = parse_timestamp(expires_at)
        .ok_or_else(|| ApprovalRuleError::invalid("expires_at must be an RFC3339 timestamp"))?;
    if parsed <= now {
        return Err(ApprovalRuleError::new(
            APPROVAL_TEMPLATE_EXPIRED,
            "expires_at must be in the future",
        ));
    }
    if parsed - now > time::Duration::seconds(MAX_TEMPLATE_LIFETIME_SECS) {
        return Err(ApprovalRuleError::invalid(
            "template expiry must be at most 366 days in the future",
        ));
    }
    Ok(Some(expires_at.to_string()))
}

pub fn validate_max_uses(max_uses: Option<u32>) -> Result<Option<u32>, ApprovalRuleError> {
    if let Some(max) = max_uses
        && (max == 0 || max > MAX_TEMPLATE_USES)
    {
        return Err(ApprovalRuleError::invalid(format!(
            "max_uses must be 1-{MAX_TEMPLATE_USES}"
        )));
    }
    Ok(max_uses)
}

/// Build a low/medium-risk template from an existing approval. This is the
/// deterministic backing for the Swift "save as template" action. High-risk,
/// remote, opaque host, and sensitive GUI approvals are refused.
pub fn template_from_approval(
    approval: &Approval,
    name: Option<&str>,
    max_uses: Option<u32>,
    expires_at: Option<&str>,
    created_by: &str,
    now: OffsetDateTime,
) -> Result<ApprovalTemplate, ApprovalRuleError> {
    let risk = approval
        .approval_risk
        .as_deref()
        .and_then(parse_risk)
        .ok_or_else(|| ApprovalRuleError::invalid("approval has no known risk"))?;
    if risk > RiskLevel::External {
        return Err(ApprovalRuleError::forbidden_risk(format!(
            "{} approvals cannot become auto-approval templates",
            risk.as_str()
        )));
    }
    if approval.tool.starts_with("remote_") {
        return Err(ApprovalRuleError::forbidden_risk(
            "remote tools cannot become auto-approval templates",
        ));
    }
    if matches!(
        approval.tool.as_str(),
        "mcp"
            | "computer"
            | "shell"
            | "process"
            | "run_project_task"
            | "local_http"
            | "code_diagnostics"
            | "task_control"
    ) {
        return Err(ApprovalRuleError::forbidden_risk(format!(
            "{} approvals cannot become one-click templates; create a manual template with an explicit scope",
            approval.tool
        )));
    }
    let name = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("From {} approval", approval.tool));
    if name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err(ApprovalRuleError::invalid(
            "template name must be at most 120 characters without control characters",
        ));
    }
    let mut path_globs = extract_input_paths(&approval.input)
        .into_iter()
        .filter(|path| !path.contains('*') && !path.contains('?'))
        .map(|path| path.trim_start_matches("./").to_string())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    path_globs.sort();
    path_globs.dedup();
    if path_globs.is_empty() {
        return Err(ApprovalRuleError::invalid(
            "one-click templates require a path-scoped approval; create a manual template with an explicit tool scope",
        ));
    }
    let path_globs = validate_path_globs(&path_globs)?;
    let expires_at = validate_template_expiry(expires_at, now)?;
    let max_uses = validate_max_uses(Some(max_uses.unwrap_or(10)))?;
    Ok(ApprovalTemplate {
        id: uuid::Uuid::now_v7().to_string(),
        project_id: String::new(),
        name,
        enabled: true,
        tool_pattern: validate_tool_pattern(&approval.tool)?,
        risk_levels: validate_risk_levels(
            &[risk.as_str().to_string()],
            ApprovalTemplateAction::Allow,
        )?,
        path_globs,
        action: ApprovalTemplateAction::Allow.as_str().to_string(),
        expires_at,
        max_uses,
        used_count: 0,
        created_by: created_by.to_string(),
        created_at: now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()),
        updated_at: None,
    })
}

pub fn validate_attention_policy(policy: AttentionPolicy) -> AttentionPolicy {
    policy.normalized()
}

/// Stale deadline for a row. A risk at or above `priority_boost_risk` gets a
/// four-times longer window; it is still never auto-approved.
pub fn approval_expires_at(
    created_at: OffsetDateTime,
    risk: Option<&str>,
    policy: &AttentionPolicy,
) -> String {
    let multiplier =
        if crate::approvals::risk_rank(risk) >= risk_rank(Some(&policy.priority_boost_risk)) {
            4
        } else {
            1
        };
    let deadline = created_at
        + time::Duration::seconds(policy.stale_after_secs.saturating_mul(multiplier) as i64);
    deadline
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn risk_rank(risk: Option<&str>) -> i32 {
    match risk {
        Some("destructive") => 4,
        Some("host_state") => 3,
        Some("external") => 2,
        Some("workspace_write") => 1,
        Some("read_only") => 0,
        _ => -1,
    }
}

#[cfg(test)]
#[path = "approvals/tests.rs"]
mod tests;
