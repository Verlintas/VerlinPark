//! S8 portable team templates / team charter export, validation, preview, and
//! import.
//!
//! A template travels as structure, rules, conventions, budget suggestions,
//! explicit model role bindings, tool-policy references, and skill references.
//! It never contains an agent identity, a session, a message, project memory,
//! an inbox row, a worktree, a branch, a commit, tool output, performance
//! evidence, an MCP schema/pin, a provider key, an absolute path, a repository
//! URL, or a secret.
//!
//! Import is two-stage. Preview is read-only. Apply requires explicit approval,
//! stores a content-addressed template and an apply link, and creates only
//! **pending S6 hire requests**. It never creates an identity, approves a hire,
//! overwrites an existing identity or Leader, widens a budget, changes an
//! approval/sandbox/MCP/Verifier/Integrator policy, or selects a model route.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use vp_protocol::types::{
    AgentHireRequest, AgentIdentity, AgentIdentityError, AgentIdentityPolicy, ProjectBudget,
    TEAM_TEMPLATE_MAX_CONCURRENCY, TEAM_TEMPLATE_MAX_COST_BUDGET_USD,
    TEAM_TEMPLATE_MAX_DEPARTMENTS, TEAM_TEMPLATE_MAX_DESCRIPTION_CHARS,
    TEAM_TEMPLATE_MAX_JSON_BYTES, TEAM_TEMPLATE_MAX_LIST_ITEMS, TEAM_TEMPLATE_MAX_MODEL_BINDINGS,
    TEAM_TEMPLATE_MAX_MODEL_CHARS, TEAM_TEMPLATE_MAX_NAME_CHARS, TEAM_TEMPLATE_MAX_PROFILE_CHARS,
    TEAM_TEMPLATE_MAX_SKILLS, TEAM_TEMPLATE_MAX_SLOT_ID_CHARS, TEAM_TEMPLATE_MAX_TEXT_CHARS,
    TEAM_TEMPLATE_MAX_TITLE_CHARS, TEAM_TEMPLATE_MAX_TOKEN_BUDGET, TEAM_TEMPLATE_MAX_TOOL_NAMES,
    TEAM_TEMPLATE_MAX_VERSION_CHARS, TEAM_TEMPLATE_MAX_WORKERS_PER_DEPARTMENT,
    TEAM_TEMPLATE_MIN_CONCURRENCY, TEAM_TEMPLATE_SCHEMA_VERSION, TeamTemplate, TeamTemplateBudget,
    TeamTemplateBudgets, TeamTemplateConflict, TeamTemplateConventions, TeamTemplateDepartment,
    TeamTemplateDepartmentPreview, TeamTemplateDiffResult, TeamTemplateDiffRow,
    TeamTemplateExportParams, TeamTemplateExportResult, TeamTemplateGetResult,
    TeamTemplateImportParams, TeamTemplateImportResult, TeamTemplateLabourDefaults,
    TeamTemplateListResult, TeamTemplatePreview, TeamTemplateResolution, TeamTemplateRoleSlot,
    TeamTemplateRules, TeamTemplateSkillRef, TeamTemplateSlotPreview, TeamTemplateToolPolicy,
    team_template_codes,
};

use crate::Engine;
use crate::tool_bundle::{ToolBundleKind, catalog_entry, select_bundle};

const MAX_TEMPLATE_TOTAL_SLOTS: usize = 256;
const MAX_TEMPLATE_CONFLICT_ROWS: usize = 128;
const MAX_TEMPLATE_WARNINGS: usize = 64;
const MAX_TEMPLATE_DIFF_ROWS: usize = 256;
const DEFAULT_TEMPLATE_VERSION: &str = "1.0.0";
const DEFAULT_LEADER_CHARTER: &str =
    "Own the project goal, portfolio, budget, and final plan approval.";
const DEFAULT_MANAGER_CHARTER: &str =
    "Own the department architecture, interface contracts, and Worker review.";
const DEFAULT_WORKER_CHARTER: &str =
    "Own the component design, Labour decomposition, review, and integration.";
const LEADER_DECISION_RIGHTS: &[&str] = &[
    "Approve the project plan and priorities",
    "Allocate project budget",
    "Resolve cross-department escalation",
];
const LEADER_REVIEW_DUTIES: &[&str] = &[
    "Review department objectives",
    "Review final budget and acceptance evidence",
];
const MANAGER_DECISION_RIGHTS: &[&str] = &[
    "Define department interfaces and standards",
    "Sequence department work",
];
const MANAGER_REVIEW_DUTIES: &[&str] = &[
    "Review Worker designs and integration",
    "Approve handoffs within the department",
];
const WORKER_DECISION_RIGHTS: &[&str] = &[
    "Define component-level design",
    "Decompose implementation tasks for Labour",
];
const WORKER_REVIEW_DUTIES: &[&str] = &[
    "Review Labour changes",
    "Integrate accepted commits into the component workspace",
];
const DELEGATION_RULE: &str = "Leader delegates department goals to Managers; Managers delegate component work to Workers; Workers decompose implementation to Labour. Labour does not delegate.";
const REVIEW_RULE: &str = "Workers review Labour changes before integration; Managers review Worker designs and handoffs; the Verifier remains an independent gate.";
const ACCEPTANCE_RULE: &str = "Definition of done requires the declared acceptance checks, a passing verification verdict, and deterministic integration. Commands in this charter are suggestions until the target project confirms them.";
const ESCALATION_RULE: &str = "Blocked work escalates Worker to Manager to Leader. Unresolved budget, policy, or approval conflicts stop instead of bypassing a gate.";
const RETIREMENT_RULE: &str = "Retirement is explicit, evidence-based, and operator-approved. Import never retires, overwrites, or rehires an existing identity.";
const HANDOFF_RULE: &str = "A handoff names the task, changed paths, acceptance evidence, and open risks. It carries no conversation, memory, inbox, or tool-output content.";
const COMMUNICATION_RULE: &str = "Use typed coordination notes and bounded task summaries. Never embed credentials, absolute paths, repository URLs, or private context in a portable charter.";

fn template_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentIdentityError::new(code, message).into()
}

/// Bounded validation context. `known_models` comes from the daemon's configured
/// role models and model presets, never from the client request.
#[derive(Debug, Clone, Default)]
pub struct TeamTemplateValidationContext {
    pub known_models: Vec<String>,
    pub project_budget: ProjectBudget,
    pub identity_policy: AgentIdentityPolicy,
}

impl TeamTemplateValidationContext {
    fn knows_model(&self, model: &str) -> bool {
        self.known_models
            .iter()
            .any(|known| known.trim() == model.trim())
    }
}

// ---------------------------------------------------------------------------
// Canonical hashing and Markdown rendering
// ---------------------------------------------------------------------------

/// Canonical bytes for hashing: field order is the Rust declaration order,
/// maps are `BTreeMap`s, and `content_hash` is blanked.
pub fn canonical_team_template_bytes(template: &TeamTemplate) -> Result<Vec<u8>> {
    let mut canonical = template.clone();
    canonical.content_hash.clear();
    Ok(serde_json::to_vec(&canonical)?)
}

pub fn compute_team_template_hash(template: &TeamTemplate) -> Result<String> {
    Ok(crate::sha256_hex(&canonical_team_template_bytes(template)?))
}

/// Fill a deterministic template id (when absent) and the content hash. Export
/// uses this; import validates the supplied hash instead of silently repairing
/// it.
pub fn finalize_team_template(template: &mut TeamTemplate) -> Result<()> {
    template.schema_version = TEAM_TEMPLATE_SCHEMA_VERSION;
    if template.template_id.trim().is_empty() {
        template.content_hash.clear();
        template.template_id.clear();
        let body = serde_json::to_vec(template)?;
        let digest = crate::sha256_hex(&body);
        template.template_id = format!("tt_{}", &digest[..16]);
    }
    template.content_hash.clear();
    template.content_hash = compute_team_template_hash(template)?;
    Ok(())
}

/// Pretty `team.json`. The content hash is computed from the compact canonical
/// bytes, not this presentation, so whitespace cannot change identity.
pub fn team_template_json(template: &TeamTemplate) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(template)?))
}

fn md_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', " ")
        .trim()
        .to_string()
}

fn budget_line(label: &str, budget: &TeamTemplateBudget) -> String {
    let tokens = budget
        .token_budget
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unset".to_string());
    let cost = budget
        .cost_budget_usd
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "unset".to_string());
    format!("- {label}: tokens {tokens}, USD {cost}")
}

fn role_slot_markdown(slot: &TeamTemplateRoleSlot) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "#### {} ({})\n\n",
        md_escape(&slot.title),
        md_escape(&slot.role)
    ));
    if let Some(department) = &slot.department {
        out.push_str(&format!("- Department: {}\n", md_escape(department)));
    }
    if let Some(component) = &slot.component {
        out.push_str(&format!("- Component: {}\n", md_escape(component)));
    }
    out.push_str(&format!("- Profile: {}\n", md_escape(&slot.profile)));
    if let Some(model_role) = &slot.model_role {
        out.push_str(&format!("- Model role: {}\n", md_escape(model_role)));
    }
    if !slot.charter.is_empty() {
        out.push_str(&format!("\n{}\n", md_escape(&slot.charter)));
    }
    if !slot.decision_rights.is_empty() {
        out.push_str("\nDecision rights:\n");
        for right in &slot.decision_rights {
            out.push_str(&format!("- {}\n", md_escape(right)));
        }
    }
    if !slot.review_duties.is_empty() {
        out.push_str("\nReview duties:\n");
        for duty in &slot.review_duties {
            out.push_str(&format!("- {}\n", md_escape(duty)));
        }
    }
    if slot.default_budget.token_budget.is_some() || slot.default_budget.cost_budget_usd.is_some() {
        out.push('\n');
        out.push_str(&budget_line("Default budget", &slot.default_budget));
        out.push('\n');
    }
    out
}

/// Deterministic `TEAM.md`. It is a rendering of the same canonical struct; it
/// is not a second source of truth and never contains local context.
pub fn render_team_markdown(template: &TeamTemplate) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {} v{}\n\n",
        md_escape(&template.name),
        template.version
    ));
    if !template.description.is_empty() {
        out.push_str(&format!("{}\n\n", md_escape(&template.description)));
    }
    out.push_str(&format!("- Template: `{}`\n", template.template_id));
    out.push_str(&format!("- Schema: {}\n", template.schema_version));
    out.push_str(&format!("- Content hash: `{}`\n", template.content_hash));
    if let Some(source) = &template.provenance.source_project {
        out.push_str(&format!("- Source project: {}\n", md_escape(source)));
    }
    out.push('\n');

    out.push_str("## Leader\n\n");
    out.push_str(&role_slot_markdown(&template.leader));

    if !template.departments.is_empty() {
        out.push_str("## Departments\n\n");
        for department in &template.departments {
            out.push_str(&format!("### {}\n\n", md_escape(&department.name)));
            if !department.objective.is_empty() {
                out.push_str(&format!("{}\n\n", md_escape(&department.objective)));
            }
            out.push_str("#### Manager\n\n");
            out.push_str(&role_slot_markdown(&department.manager));
            if !department.workers.is_empty() {
                out.push_str("#### Workers\n\n");
                for worker in &department.workers {
                    out.push_str(&role_slot_markdown(worker));
                }
            }
        }
    }

    out.push_str("## Labour defaults\n\n");
    out.push_str(&format!(
        "- Profile: {}\n- Bundle: {}\n- Max concurrency: {}\n",
        md_escape(&template.labour_defaults.profile),
        md_escape(&template.labour_defaults.bundle),
        template.labour_defaults.max_concurrency
    ));
    if let Some(model_role) = &template.labour_defaults.model_role {
        out.push_str(&format!("- Model role: {}\n", md_escape(model_role)));
    }
    out.push('\n');

    out.push_str("## Rules\n\n");
    for (label, value) in [
        ("Delegation", &template.rules.delegation),
        ("Review", &template.rules.review),
        ("Acceptance", &template.rules.acceptance),
        ("Escalation", &template.rules.escalation),
        ("Retirement", &template.rules.retirement),
        ("Handoff", &template.rules.handoff),
        ("Communication", &template.rules.communication),
    ] {
        if !value.is_empty() {
            out.push_str(&format!("### {label}\n\n{}\n\n", md_escape(value)));
        }
    }

    out.push_str("## Budgets\n\n");
    out.push_str(&budget_line("Defaults", &template.budgets.defaults));
    out.push('\n');
    out.push_str(&budget_line("Caps", &template.budgets.caps));
    out.push_str(
        "\nTarget project policy always wins; importing never enables or widens a budget.\n\n",
    );

    if !template.model_bindings.is_empty() {
        out.push_str("## Model bindings\n\n");
        for (role, model) in &template.model_bindings {
            out.push_str(&format!("- {} → `{}`\n", md_escape(role), md_escape(model)));
        }
        out.push_str("\nBindings are explicit references, not model routing secrets.\n\n");
    }

    if !template.tool_policy.is_empty() {
        out.push_str("## Tool policy\n\n");
        for (role, policy) in &template.tool_policy {
            out.push_str(&format!("### {}\n\n", md_escape(role)));
            if let Some(bundle) = &policy.bundle {
                out.push_str(&format!("- Bundle: {}\n", md_escape(bundle)));
            }
            if !policy.allow.is_empty() {
                out.push_str(&format!("- Allow: {}\n", policy.allow.join(", ")));
            }
            if !policy.deny.is_empty() {
                out.push_str(&format!("- Deny: {}\n", policy.deny.join(", ")));
            }
            out.push('\n');
        }
        out.push_str("Tool names are references; target approvals, MCP pins, and role policy still decide execution.\n\n");
    }

    if !template.skills.is_empty() {
        out.push_str("## Skills\n\n");
        for skill in &template.skills {
            out.push_str(&format!(
                "- {} (`{}`)\n",
                md_escape(&skill.name),
                skill.version_hash
            ));
        }
        out.push('\n');
    }

    out.push_str("## Conventions\n\n");
    if !template.conventions.coding.is_empty() {
        out.push_str(&format!(
            "- Coding: {}\n",
            md_escape(&template.conventions.coding)
        ));
    }
    if !template.conventions.commit.is_empty() {
        out.push_str(&format!(
            "- Commit: {}\n",
            md_escape(&template.conventions.commit)
        ));
    }
    if !template.conventions.test.is_empty() {
        out.push_str(&format!(
            "- Test: {}\n",
            md_escape(&template.conventions.test)
        ));
    }
    if !template.conventions.review_checklist.is_empty() {
        out.push_str("- Review checklist:\n");
        for item in &template.conventions.review_checklist {
            out.push_str(&format!("  - {}\n", md_escape(item)));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Structural and target validation
// ---------------------------------------------------------------------------

fn ensure_chars(value: &str, label: &str, max: usize, code: &'static str) -> Result<()> {
    if value.chars().count() > max {
        return Err(template_error(
            code,
            format!("{label} must be at most {max} characters"),
        ));
    }
    if value.chars().any(|character| character == '\0') {
        return Err(template_error(
            code,
            format!("{label} must not contain NUL"),
        ));
    }
    Ok(())
}

fn ensure_nonempty(value: &str, label: &str, code: &'static str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(template_error(code, format!("{label} is required")));
    }
    Ok(())
}

fn looks_like_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

fn contains_absolute_path(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with('/')
        || lower.starts_with("~/")
        || lower.contains("/users/")
        || lower.contains("/home/")
        || lower.contains("/private/")
        || lower.contains("/var/")
        || lower.contains("/tmp/")
        || lower.contains("/opt/")
        || lower.contains("/etc/")
        || lower.contains("/usr/")
        || lower.contains("/root/")
        || lower.contains("/mnt/")
        || lower.contains("/srv/")
        || value.contains(":\\")
        || value.starts_with("\\\\")
}

fn contains_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("http://")
        || lower.contains("https://")
        || lower.contains("ssh://")
        || lower.contains("git://")
        || lower.contains("www.")
        || lower.starts_with("git@")
        || lower.contains("git@github")
        || lower.contains("bitbucket.org")
        || lower.contains("github.com")
        || lower.contains("gitlab.com")
}

fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "sk-",
        "sk_",
        "xoxb-",
        "xoxp-",
        "ghp_",
        "gho_",
        "github_pat_",
        "akia",
        "asia",
        "-----begin",
        "private key",
        "bearer ",
        "authorization:",
        "password=",
        "password:",
        "api_key=",
        "apikey=",
        "secret=",
        "token=",
        "vpg_",
        "vpr_",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

fn contains_context_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "session_id",
        "agent_id",
        "identity_id",
        "worktree_path",
        "worktree path",
        "inbox_id",
        "memory_id",
        "tool_output",
        "tool output",
        "performance evidence",
        "refs/heads/",
        ".git/",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
        || looks_like_uuid(value)
        || value
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| {
                token.starts_with("ses_")
                    || token.starts_with("sess_")
                    || token.starts_with("agt_")
                    || token.starts_with("idn_")
                    || token.starts_with("wrk_")
            })
}

/// Scan one human-readable string for context leakage. Structural identifiers
/// (template id, content hash, model/tool names, role names) are validated by
/// their own typed rules and are not passed here.
fn validate_portable_text(value: &str, label: &str, max: usize) -> Result<()> {
    ensure_chars(value, label, max, team_template_codes::TEMPLATE_INVALID)?;
    if contains_absolute_path(value) {
        return Err(template_error(
            team_template_codes::TEMPLATE_CONTEXT_LEAK,
            format!("{label} contains an absolute path"),
        ));
    }
    if contains_url(value) {
        return Err(template_error(
            team_template_codes::TEMPLATE_CONTEXT_LEAK,
            format!("{label} contains a repository or web URL"),
        ));
    }
    if contains_secret(value) {
        return Err(template_error(
            team_template_codes::TEMPLATE_SECRET_DETECTED,
            format!("{label} looks like a credential or secret"),
        ));
    }
    if contains_context_marker(value) {
        return Err(template_error(
            team_template_codes::TEMPLATE_CONTEXT_LEAK,
            format!("{label} contains a session, agent, worktree, or context identifier"),
        ));
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str, max: usize) -> Result<()> {
    ensure_nonempty(value, label, team_template_codes::TEMPLATE_INVALID)?;
    ensure_chars(value, label, max, team_template_codes::TEMPLATE_INVALID)?;
    if !value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
    }) {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("{label} may contain only ASCII letters, digits, '-', '_', '.', and ':'"),
        ));
    }
    Ok(())
}

fn validate_role(value: &str, expected: &str, label: &str) -> Result<()> {
    if value.trim().to_ascii_lowercase() != expected {
        return Err(template_error(
            team_template_codes::TEMPLATE_ROLE_INVALID,
            format!("{label} role must be {expected}, got {value}"),
        ));
    }
    Ok(())
}

fn validate_profile(value: &str, label: &str) -> Result<()> {
    ensure_chars(
        value,
        label,
        TEAM_TEMPLATE_MAX_PROFILE_CHARS,
        team_template_codes::TEMPLATE_PROFILE_INVALID,
    )?;
    if !vp_protocol::types::valid_agent_identity_profile(value.trim()) {
        return Err(template_error(
            team_template_codes::TEMPLATE_PROFILE_INVALID,
            format!("{label} profile must be coding, research, verification, or generic: {value}"),
        ));
    }
    Ok(())
}

fn validate_string_list(
    values: &[String],
    label: &str,
    max_items: usize,
    max_chars: usize,
) -> Result<()> {
    if values.len() > max_items {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("{label} must have at most {max_items} items"),
        ));
    }
    for (index, value) in values.iter().enumerate() {
        validate_portable_text(value, &format!("{label}[{index}]"), max_chars)?;
    }
    Ok(())
}

fn validate_budget(budget: &TeamTemplateBudget, label: &str) -> Result<()> {
    if let Some(tokens) = budget.token_budget
        && tokens > TEAM_TEMPLATE_MAX_TOKEN_BUDGET
    {
        return Err(template_error(
            team_template_codes::TEMPLATE_BUDGET_INVALID,
            format!("{label} token budget exceeds the hard bound"),
        ));
    }
    if let Some(cost) = budget.cost_budget_usd
        && (!cost.is_finite() || !(0.0..=TEAM_TEMPLATE_MAX_COST_BUDGET_USD).contains(&cost))
    {
        return Err(template_error(
            team_template_codes::TEMPLATE_BUDGET_INVALID,
            format!(
                "{label} cost budget must be finite and between 0 and {TEAM_TEMPLATE_MAX_COST_BUDGET_USD}"
            ),
        ));
    }
    Ok(())
}

fn validate_role_slot(slot: &TeamTemplateRoleSlot, role: &str, label: &str) -> Result<()> {
    validate_role(&slot.role, role, label)?;
    ensure_nonempty(
        &slot.title,
        &format!("{label}.title"),
        team_template_codes::TEMPLATE_INVALID,
    )?;
    validate_portable_text(
        &slot.title,
        &format!("{label}.title"),
        TEAM_TEMPLATE_MAX_TITLE_CHARS,
    )?;
    if let Some(department) = &slot.department {
        validate_portable_text(
            department,
            &format!("{label}.department"),
            TEAM_TEMPLATE_MAX_TEXT_CHARS,
        )?;
        ensure_nonempty(
            department,
            &format!("{label}.department"),
            team_template_codes::TEMPLATE_INVALID,
        )?;
    }
    if let Some(component) = &slot.component {
        validate_portable_text(
            component,
            &format!("{label}.component"),
            TEAM_TEMPLATE_MAX_TEXT_CHARS,
        )?;
        ensure_nonempty(
            component,
            &format!("{label}.component"),
            team_template_codes::TEMPLATE_INVALID,
        )?;
    }
    validate_profile(&slot.profile, label)?;
    if let Some(model_role) = &slot.model_role {
        validate_identifier(
            model_role,
            &format!("{label}.model_role"),
            TEAM_TEMPLATE_MAX_MODEL_CHARS,
        )?;
    }
    validate_portable_text(
        &slot.charter,
        &format!("{label}.charter"),
        TEAM_TEMPLATE_MAX_TEXT_CHARS,
    )?;
    validate_string_list(
        &slot.decision_rights,
        &format!("{label}.decision_rights"),
        TEAM_TEMPLATE_MAX_LIST_ITEMS,
        TEAM_TEMPLATE_MAX_TEXT_CHARS,
    )?;
    validate_string_list(
        &slot.review_duties,
        &format!("{label}.review_duties"),
        TEAM_TEMPLATE_MAX_LIST_ITEMS,
        TEAM_TEMPLATE_MAX_TEXT_CHARS,
    )?;
    validate_budget(&slot.default_budget, &format!("{label}.default_budget"))?;
    Ok(())
}

fn validate_tool_name(name: &str) -> bool {
    catalog_entry(name.trim()).is_some()
}

fn validate_bundle_name(name: &str) -> bool {
    ToolBundleKind::parse(name).is_some()
}

/// Structural validation: schema, hash, bounds, roles, profiles, role slots,
/// portable-text leakage, budgets, model keys, tool policy shape, skills,
/// conventions. It deliberately does not know the target project.
pub fn validate_team_template_shape(template: &TeamTemplate) -> Result<()> {
    if template.schema_version != TEAM_TEMPLATE_SCHEMA_VERSION {
        return Err(template_error(
            team_template_codes::TEMPLATE_SCHEMA_UNSUPPORTED,
            format!(
                "team template schema {} is not supported (expected {TEAM_TEMPLATE_SCHEMA_VERSION})",
                template.schema_version
            ),
        ));
    }
    validate_identifier(
        &template.template_id,
        "template_id",
        TEAM_TEMPLATE_MAX_SLOT_ID_CHARS,
    )?;
    ensure_nonempty(
        &template.name,
        "name",
        team_template_codes::TEMPLATE_INVALID,
    )?;
    validate_portable_text(&template.name, "name", TEAM_TEMPLATE_MAX_NAME_CHARS)?;
    ensure_nonempty(
        &template.version,
        "version",
        team_template_codes::TEMPLATE_INVALID,
    )?;
    ensure_chars(
        &template.version,
        "version",
        TEAM_TEMPLATE_MAX_VERSION_CHARS,
        team_template_codes::TEMPLATE_INVALID,
    )?;
    if !template.version.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '+')
    }) {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            "version may contain only ASCII letters, digits, '-', '_', '.', and '+'",
        ));
    }
    validate_portable_text(
        &template.description,
        "description",
        TEAM_TEMPLATE_MAX_DESCRIPTION_CHARS,
    )?;
    if template.content_hash.len() != 64
        || !template
            .content_hash
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            "content_hash must be a 64-character SHA-256 hex string",
        ));
    }
    let expected = compute_team_template_hash(template)?;
    if expected != template.content_hash.to_ascii_lowercase() {
        return Err(template_error(
            team_template_codes::TEMPLATE_HASH_MISMATCH,
            "team template content hash does not match its canonical body",
        ));
    }

    validate_role_slot(&template.leader, "leader", "leader")?;
    if template.departments.len() > TEAM_TEMPLATE_MAX_DEPARTMENTS {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("departments must have at most {TEAM_TEMPLATE_MAX_DEPARTMENTS} entries"),
        ));
    }
    let mut department_names = HashSet::new();
    let mut total_slots = 1usize;
    for (index, department) in template.departments.iter().enumerate() {
        let label = format!("departments[{index}]");
        ensure_nonempty(
            &department.name,
            &format!("{label}.name"),
            team_template_codes::TEMPLATE_INVALID,
        )?;
        validate_portable_text(
            &department.name,
            &format!("{label}.name"),
            TEAM_TEMPLATE_MAX_TEXT_CHARS,
        )?;
        if !department_names.insert(department.name.trim().to_ascii_lowercase()) {
            return Err(template_error(
                team_template_codes::TEMPLATE_INVALID,
                format!("department name is duplicated: {}", department.name),
            ));
        }
        validate_portable_text(
            &department.objective,
            &format!("{label}.objective"),
            TEAM_TEMPLATE_MAX_TEXT_CHARS,
        )?;
        validate_role_slot(&department.manager, "manager", &format!("{label}.manager"))?;
        if department.workers.len() > TEAM_TEMPLATE_MAX_WORKERS_PER_DEPARTMENT {
            return Err(template_error(
                team_template_codes::TEMPLATE_INVALID,
                format!(
                    "{label}.workers must have at most {TEAM_TEMPLATE_MAX_WORKERS_PER_DEPARTMENT} entries"
                ),
            ));
        }
        total_slots = total_slots.saturating_add(1 + department.workers.len());
        let mut worker_keys = HashSet::new();
        for (worker_index, worker) in department.workers.iter().enumerate() {
            let worker_label = format!("{label}.workers[{worker_index}]");
            validate_role_slot(worker, "worker", &worker_label)?;
            let key = format!(
                "{}|{}",
                worker
                    .component
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase(),
                worker.title.trim().to_ascii_lowercase()
            );
            if !worker_keys.insert(key) {
                return Err(template_error(
                    team_template_codes::TEMPLATE_INVALID,
                    format!(
                        "{worker_label} duplicates a worker component/title in {}",
                        department.name
                    ),
                ));
            }
        }
    }
    if total_slots > MAX_TEMPLATE_TOTAL_SLOTS {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("team template has more than {MAX_TEMPLATE_TOTAL_SLOTS} role slots"),
        ));
    }

    validate_profile(&template.labour_defaults.profile, "labour_defaults.profile")?;
    if !template.labour_defaults.bundle.trim().is_empty()
        && !validate_bundle_name(&template.labour_defaults.bundle)
    {
        return Err(template_error(
            team_template_codes::TEMPLATE_TOOL_INVALID,
            format!(
                "labour_defaults.bundle is not a known bundle: {}",
                template.labour_defaults.bundle
            ),
        ));
    }
    if !(TEAM_TEMPLATE_MIN_CONCURRENCY..=TEAM_TEMPLATE_MAX_CONCURRENCY)
        .contains(&template.labour_defaults.max_concurrency)
    {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!(
                "labour max_concurrency must be between {TEAM_TEMPLATE_MIN_CONCURRENCY} and {TEAM_TEMPLATE_MAX_CONCURRENCY}"
            ),
        ));
    }
    if let Some(model_role) = &template.labour_defaults.model_role {
        validate_identifier(
            model_role,
            "labour_defaults.model_role",
            TEAM_TEMPLATE_MAX_MODEL_CHARS,
        )?;
    }

    for (label, value) in [
        ("rules.delegation", &template.rules.delegation),
        ("rules.review", &template.rules.review),
        ("rules.acceptance", &template.rules.acceptance),
        ("rules.escalation", &template.rules.escalation),
        ("rules.retirement", &template.rules.retirement),
        ("rules.handoff", &template.rules.handoff),
        ("rules.communication", &template.rules.communication),
    ] {
        validate_portable_text(value, label, TEAM_TEMPLATE_MAX_TEXT_CHARS)?;
    }

    validate_budget(&template.budgets.defaults, "budgets.defaults")?;
    validate_budget(&template.budgets.caps, "budgets.caps")?;
    if let (Some(default), Some(cap)) = (
        template.budgets.defaults.token_budget,
        template.budgets.caps.token_budget,
    ) && default > cap
    {
        return Err(template_error(
            team_template_codes::TEMPLATE_BUDGET_INVALID,
            "budget default token allowance must not exceed the cap",
        ));
    }
    if let (Some(default), Some(cap)) = (
        template.budgets.defaults.cost_budget_usd,
        template.budgets.caps.cost_budget_usd,
    ) && default > cap
    {
        return Err(template_error(
            team_template_codes::TEMPLATE_BUDGET_INVALID,
            "budget default cost allowance must not exceed the cap",
        ));
    }

    if template.model_bindings.len() > TEAM_TEMPLATE_MAX_MODEL_BINDINGS {
        return Err(template_error(
            team_template_codes::TEMPLATE_MODEL_INVALID,
            format!("model_bindings must have at most {TEAM_TEMPLATE_MAX_MODEL_BINDINGS} entries"),
        ));
    }
    for (role, model) in &template.model_bindings {
        validate_identifier(role, "model_bindings key", TEAM_TEMPLATE_MAX_MODEL_CHARS)?;
        ensure_nonempty(
            model,
            "model binding",
            team_template_codes::TEMPLATE_MODEL_INVALID,
        )?;
        ensure_chars(
            model,
            "model binding",
            TEAM_TEMPLATE_MAX_MODEL_CHARS,
            team_template_codes::TEMPLATE_MODEL_INVALID,
        )?;
        validate_portable_text(model, "model binding", TEAM_TEMPLATE_MAX_MODEL_CHARS)?;
    }
    let mut referenced_model_roles = Vec::new();
    if let Some(model_role) = &template.leader.model_role {
        referenced_model_roles.push(model_role.clone());
    }
    for department in &template.departments {
        if let Some(model_role) = &department.manager.model_role {
            referenced_model_roles.push(model_role.clone());
        }
        for worker in &department.workers {
            if let Some(model_role) = &worker.model_role {
                referenced_model_roles.push(model_role.clone());
            }
        }
    }
    if let Some(model_role) = &template.labour_defaults.model_role {
        referenced_model_roles.push(model_role.clone());
    }
    for model_role in referenced_model_roles {
        if !template.model_bindings.contains_key(&model_role) {
            return Err(template_error(
                team_template_codes::TEMPLATE_MODEL_INVALID,
                format!("model role {model_role} has no explicit binding"),
            ));
        }
    }

    if template.tool_policy.len() > 8 {
        return Err(template_error(
            team_template_codes::TEMPLATE_TOOL_INVALID,
            "tool_policy must have at most 8 role entries",
        ));
    }
    for (role, policy) in &template.tool_policy {
        let role = role.trim().to_ascii_lowercase();
        if !matches!(
            role.as_str(),
            "leader" | "manager" | "worker" | "labour" | "verifier" | "scout"
        ) {
            return Err(template_error(
                team_template_codes::TEMPLATE_TOOL_INVALID,
                format!("tool_policy has an unknown role: {role}"),
            ));
        }
        if let Some(bundle) = &policy.bundle
            && !validate_bundle_name(bundle)
        {
            return Err(template_error(
                team_template_codes::TEMPLATE_TOOL_INVALID,
                format!("tool_policy.{role} uses an unknown bundle: {bundle}"),
            ));
        }
        if policy.allow.len() > TEAM_TEMPLATE_MAX_TOOL_NAMES
            || policy.deny.len() > TEAM_TEMPLATE_MAX_TOOL_NAMES
        {
            return Err(template_error(
                team_template_codes::TEMPLATE_TOOL_INVALID,
                format!("tool_policy.{role} has too many tool names"),
            ));
        }
        let mut seen = HashSet::new();
        for name in policy.allow.iter().chain(policy.deny.iter()) {
            if !seen.insert(name.trim()) {
                return Err(template_error(
                    team_template_codes::TEMPLATE_TOOL_INVALID,
                    format!("tool_policy.{role} repeats tool {name}"),
                ));
            }
            if !validate_tool_name(name) {
                return Err(template_error(
                    team_template_codes::TEMPLATE_TOOL_INVALID,
                    format!("tool_policy.{role} names an unknown tool: {name}"),
                ));
            }
        }
        for name in &policy.allow {
            if policy.deny.iter().any(|deny| deny == name) {
                return Err(template_error(
                    team_template_codes::TEMPLATE_TOOL_INVALID,
                    format!("tool_policy.{role} both allows and denies {name}"),
                ));
            }
        }
    }

    if template.skills.len() > TEAM_TEMPLATE_MAX_SKILLS {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("skills must have at most {TEAM_TEMPLATE_MAX_SKILLS} entries"),
        ));
    }
    let mut skill_names = HashSet::new();
    for (index, skill) in template.skills.iter().enumerate() {
        ensure_nonempty(
            &skill.name,
            &format!("skills[{index}].name"),
            team_template_codes::TEMPLATE_INVALID,
        )?;
        validate_portable_text(
            &skill.name,
            &format!("skills[{index}].name"),
            TEAM_TEMPLATE_MAX_NAME_CHARS,
        )?;
        if !skill_names.insert(skill.name.trim().to_ascii_lowercase()) {
            return Err(template_error(
                team_template_codes::TEMPLATE_INVALID,
                format!("skill name is duplicated: {}", skill.name),
            ));
        }
        if !skill.version_hash.is_empty()
            && (skill.version_hash.len() != 64
                || !skill
                    .version_hash
                    .chars()
                    .all(|character| character.is_ascii_hexdigit()))
        {
            return Err(template_error(
                team_template_codes::TEMPLATE_INVALID,
                format!("skills[{index}].version_hash must be a SHA-256 hex string"),
            ));
        }
    }

    validate_portable_text(
        &template.conventions.coding,
        "conventions.coding",
        TEAM_TEMPLATE_MAX_TEXT_CHARS,
    )?;
    validate_portable_text(
        &template.conventions.commit,
        "conventions.commit",
        TEAM_TEMPLATE_MAX_TEXT_CHARS,
    )?;
    validate_portable_text(
        &template.conventions.test,
        "conventions.test",
        TEAM_TEMPLATE_MAX_TEXT_CHARS,
    )?;
    validate_string_list(
        &template.conventions.review_checklist,
        "conventions.review_checklist",
        TEAM_TEMPLATE_MAX_LIST_ITEMS,
        TEAM_TEMPLATE_MAX_TEXT_CHARS,
    )?;

    if let Some(source_project) = &template.provenance.source_project {
        validate_portable_text(
            source_project,
            "provenance.source_project",
            TEAM_TEMPLATE_MAX_NAME_CHARS,
        )?;
    }
    Ok(())
}

/// Parse `team.json`, enforce the size bound, reject unknown fields, validate
/// shape, and verify the content hash. Target-specific model/tool/budget gaps
/// are reported by preview instead.
pub fn parse_team_template_json(raw: &str) -> Result<TeamTemplate> {
    if raw.len() > TEAM_TEMPLATE_MAX_JSON_BYTES {
        return Err(template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("team.json must be at most {TEAM_TEMPLATE_MAX_JSON_BYTES} bytes"),
        ));
    }
    let template: TeamTemplate = serde_json::from_str(raw).map_err(|error| {
        template_error(
            team_template_codes::TEMPLATE_INVALID,
            format!("team.json is invalid: {error}"),
        )
    })?;
    validate_team_template_shape(&template)?;
    Ok(template)
}

fn collect_target_gaps(
    template: &TeamTemplate,
    context: &TeamTemplateValidationContext,
    budget_changes: &mut Vec<String>,
    model_gaps: &mut Vec<String>,
    tool_gaps: &mut Vec<String>,
    policy_conflicts: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    for (role, model) in &template.model_bindings {
        if !context.knows_model(model) {
            model_gaps.push(format!(
                "{role} -> {model} is not configured in this project"
            ));
        }
    }
    for (role, policy) in &template.tool_policy {
        if let Some(bundle) = &policy.bundle
            && !validate_bundle_name(bundle)
        {
            tool_gaps.push(format!("{role} bundle {bundle} is unknown"));
        }
        for name in policy.allow.iter().chain(policy.deny.iter()) {
            if !validate_tool_name(name) {
                tool_gaps.push(format!("{role} tool {name} is unknown"));
            }
        }
    }
    budget_changes.push(format!(
        "template defaults: {} tokens / {} USD; project: {} / {}",
        template
            .budgets
            .defaults
            .token_budget
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unset".to_string()),
        template
            .budgets
            .defaults
            .cost_budget_usd
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "unset".to_string()),
        context
            .project_budget
            .token_budget
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unset".to_string()),
        context
            .project_budget
            .cost_budget_usd
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "unset".to_string()),
    ));
    if let (Some(target), Some(template_cap)) = (
        context.project_budget.token_budget,
        template.budgets.caps.token_budget,
    ) && template_cap > target
    {
        policy_conflicts.push(format!(
            "template token cap {template_cap} would widen the project cap {target}; the project wins"
        ));
    }
    if let (Some(target), Some(template_cap)) = (
        context.project_budget.cost_budget_usd,
        template.budgets.caps.cost_budget_usd,
    ) && template_cap > target
    {
        policy_conflicts.push(format!(
            "template USD cap {template_cap:.2} would widen the project cap {target:.2}; the project wins"
        ));
    }
    if let (Some(target), Some(template_default)) = (
        context.project_budget.token_budget,
        template.budgets.defaults.token_budget,
    ) && template_default > target
    {
        policy_conflicts.push(format!(
            "template default token budget {template_default} would widen the project cap {target}"
        ));
    }
    if let (Some(target), Some(template_default)) = (
        context.project_budget.cost_budget_usd,
        template.budgets.defaults.cost_budget_usd,
    ) && template_default > target
    {
        policy_conflicts.push(format!(
            "template default USD budget {template_default:.2} would widen the project cap {target:.2}"
        ));
    }
    if !context.identity_policy.enabled {
        warnings.push(
            "project identity policy is disabled; apply only creates pending hire requests, but approval still follows the project policy"
                .to_string(),
        );
    }
}

// ---------------------------------------------------------------------------
// Export derivation
// ---------------------------------------------------------------------------

fn role_charter(role: &str, department: Option<&str>, component: Option<&str>) -> String {
    match role {
        "leader" => DEFAULT_LEADER_CHARTER.to_string(),
        "manager" => match department {
            Some(department) if !department.is_empty() => format!(
                "Own the {department} department architecture, interface contracts, and Worker review."
            ),
            _ => DEFAULT_MANAGER_CHARTER.to_string(),
        },
        "worker" => match component {
            Some(component) if !component.is_empty() => format!(
                "Own the {component} component design, Labour decomposition, review, and integration."
            ),
            _ => DEFAULT_WORKER_CHARTER.to_string(),
        },
        _ => DEFAULT_WORKER_CHARTER.to_string(),
    }
}

fn role_decision_rights(role: &str) -> Vec<String> {
    match role {
        "leader" => LEADER_DECISION_RIGHTS
            .iter()
            .map(|value| value.to_string())
            .collect(),
        "manager" => MANAGER_DECISION_RIGHTS
            .iter()
            .map(|value| value.to_string())
            .collect(),
        _ => WORKER_DECISION_RIGHTS
            .iter()
            .map(|value| value.to_string())
            .collect(),
    }
}

fn role_review_duties(role: &str) -> Vec<String> {
    match role {
        "leader" => LEADER_REVIEW_DUTIES
            .iter()
            .map(|value| value.to_string())
            .collect(),
        "manager" => MANAGER_REVIEW_DUTIES
            .iter()
            .map(|value| value.to_string())
            .collect(),
        _ => WORKER_REVIEW_DUTIES
            .iter()
            .map(|value| value.to_string())
            .collect(),
    }
}

fn default_conventions() -> TeamTemplateConventions {
    TeamTemplateConventions {
        coding: "Follow the repository language, formatting, and review conventions.".to_string(),
        commit: "Commit one coherent change with a descriptive message and no unrelated files.".to_string(),
        test: "Run the declared project checks; acceptance commands require target-project confirmation.".to_string(),
        review_checklist: vec![
            "Scope matches the task contract".to_string(),
            "Acceptance evidence is current".to_string(),
            "No secrets, credentials, or unrelated files".to_string(),
            "Verifier and deterministic integration gates remain in force".to_string(),
        ],
    }
}

fn default_rules() -> TeamTemplateRules {
    TeamTemplateRules {
        delegation: DELEGATION_RULE.to_string(),
        review: REVIEW_RULE.to_string(),
        acceptance: ACCEPTANCE_RULE.to_string(),
        escalation: ESCALATION_RULE.to_string(),
        retirement: RETIREMENT_RULE.to_string(),
        handoff: HANDOFF_RULE.to_string(),
        communication: COMMUNICATION_RULE.to_string(),
    }
}

fn slot_from_identity(
    identity: Option<&AgentIdentity>,
    role: &str,
    department: Option<&str>,
    component: Option<&str>,
) -> TeamTemplateRoleSlot {
    let title = identity
        .and_then(|identity| identity.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| match role {
            "leader" => "Leader".to_string(),
            "manager" => "Manager".to_string(),
            _ => "Worker".to_string(),
        });
    let profile = identity
        .and_then(|identity| identity.profile.clone())
        .filter(|profile| vp_protocol::types::valid_agent_identity_profile(profile))
        .unwrap_or_else(|| {
            if role == "worker" {
                "coding".to_string()
            } else {
                "generic".to_string()
            }
        });
    TeamTemplateRoleSlot {
        title,
        role: role.to_string(),
        department: department.map(str::to_string),
        component: component.map(str::to_string),
        profile,
        model_role: None,
        charter: role_charter(role, department, component),
        decision_rights: role_decision_rights(role),
        review_duties: role_review_duties(role),
        default_budget: TeamTemplateBudget::default(),
    }
}

struct ExportSlot {
    role: String,
    slot: TeamTemplateRoleSlot,
    model: Option<String>,
}

fn assign_model_bindings(
    slots: &mut [ExportSlot],
    bindings: &mut BTreeMap<String, String>,
    warnings: &mut Vec<String>,
    labour_model_role: &mut Option<String>,
) {
    let mut role_models: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut by_role: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for slot in slots.iter() {
        let Some(model) = slot
            .model
            .as_ref()
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        by_role
            .entry(slot.role.clone())
            .or_default()
            .push(model.to_string());
    }
    for (role, models) in &by_role {
        let mut unique = BTreeSet::new();
        for model in models {
            unique.insert(model.clone());
        }
        for (index, model) in unique.into_iter().enumerate() {
            if bindings.len() >= TEAM_TEMPLATE_MAX_MODEL_BINDINGS {
                warnings.push(format!(
                    "model bindings exceeded the {TEAM_TEMPLATE_MAX_MODEL_BINDINGS}-entry bound; remaining slots have no explicit model role"
                ));
                break;
            }
            let key = if index == 0 {
                role.clone()
            } else {
                format!("{role}:{}", index + 1)
            };
            bindings.insert(key.clone(), model.clone());
            role_models
                .entry(role.clone())
                .or_default()
                .insert(model, key);
        }
    }
    for slot in slots.iter_mut() {
        let Some(model) = slot
            .model
            .as_ref()
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        if let Some(key) = role_models
            .get(&slot.role)
            .and_then(|models| models.get(model))
        {
            slot.slot.model_role = Some(key.clone());
            if slot.role == "worker" && labour_model_role.is_none() {
                *labour_model_role = Some(key.clone());
            }
        }
    }
}

fn tool_policy_for_roles() -> BTreeMap<String, TeamTemplateToolPolicy> {
    let mut policy = BTreeMap::new();
    for (role, profile) in [
        ("leader", "generic"),
        ("manager", "generic"),
        ("worker", "coding"),
        ("labour", "coding"),
    ] {
        let task_kind = matches!(role, "worker" | "labour").then_some("coding");
        let bundle = select_bundle(role, profile, task_kind)
            .kind
            .as_str()
            .to_string();
        policy.insert(
            role.to_string(),
            TeamTemplateToolPolicy {
                bundle: Some(bundle),
                allow: Vec::new(),
                deny: Vec::new(),
            },
        );
    }
    policy
}

fn build_exported_template(
    project_name: &str,
    identities: &[AgentIdentity],
    skills: &[vp_protocol::types::Skill],
    budget: &ProjectBudget,
    params: &TeamTemplateExportParams,
    warnings: &mut Vec<String>,
) -> Result<TeamTemplate> {
    let mut live = identities
        .iter()
        .filter(|identity| identity.status != "retired")
        .collect::<Vec<_>>();
    live.sort_by(|left, right| {
        (left.created_at.as_str(), left.id.as_str())
            .cmp(&(right.created_at.as_str(), right.id.as_str()))
    });
    let leader = live
        .iter()
        .find(|identity| identity.role == "leader")
        .copied();
    let leader_slot = slot_from_identity(leader, "leader", None, None);
    let mut export_slots = vec![ExportSlot {
        role: "leader".to_string(),
        slot: leader_slot.clone(),
        model: leader.and_then(|identity| identity.model.clone()),
    }];
    let mut departments = BTreeMap::<String, (Option<&AgentIdentity>, Vec<&AgentIdentity>)>::new();
    for identity in &live {
        match identity.role.as_str() {
            "manager" => {
                let name = identity
                    .department
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "general".to_string());
                departments.entry(name).or_insert((None, Vec::new())).0 = Some(identity);
            }
            "worker" => {
                let name = identity
                    .department
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "general".to_string());
                departments
                    .entry(name)
                    .or_insert((None, Vec::new()))
                    .1
                    .push(identity);
            }
            _ => {}
        }
    }
    let mut department_rows = Vec::new();
    for (name, (manager, mut workers)) in departments {
        workers.sort_by(|left, right| {
            (
                left.component.as_deref().unwrap_or(""),
                left.title.as_deref().unwrap_or(""),
                left.id.as_str(),
            )
                .cmp(&(
                    right.component.as_deref().unwrap_or(""),
                    right.title.as_deref().unwrap_or(""),
                    right.id.as_str(),
                ))
        });
        let manager_slot = slot_from_identity(manager, "manager", Some(&name), None);
        export_slots.push(ExportSlot {
            role: "manager".to_string(),
            slot: manager_slot.clone(),
            model: manager.and_then(|identity| identity.model.clone()),
        });
        let mut worker_slots = Vec::new();
        for worker in workers {
            let slot = slot_from_identity(
                Some(worker),
                "worker",
                Some(&name),
                worker.component.as_deref(),
            );
            export_slots.push(ExportSlot {
                role: "worker".to_string(),
                slot: slot.clone(),
                model: worker.model.clone(),
            });
            worker_slots.push(slot);
        }
        department_rows.push(TeamTemplateDepartment {
            name: name.clone(),
            objective: format!("Own {name} architecture, interfaces, and delivery."),
            manager: manager_slot,
            workers: worker_slots,
        });
    }
    if department_rows.is_empty() {
        // A template can represent a project with no departments yet. The
        // manager/worker slots stay empty rather than inventing identities.
    }
    let mut bindings = BTreeMap::new();
    let mut labour_model_role = None;
    assign_model_bindings(
        &mut export_slots,
        &mut bindings,
        warnings,
        &mut labour_model_role,
    );
    let mut leader_slot = export_slots
        .first()
        .map(|slot| slot.slot.clone())
        .unwrap_or(leader_slot);
    leader_slot.department = None;
    let mut department_export = Vec::new();
    let mut cursor = 1usize;
    for department in department_rows {
        let manager = export_slots
            .get(cursor)
            .map(|slot| slot.slot.clone())
            .unwrap_or(department.manager);
        cursor += 1;
        let mut workers = Vec::new();
        for fallback in department.workers {
            let slot = export_slots
                .get(cursor)
                .map(|slot| slot.slot.clone())
                .unwrap_or(fallback);
            cursor += 1;
            workers.push(slot);
        }
        department_export.push(TeamTemplateDepartment {
            name: department.name,
            objective: department.objective,
            manager,
            workers,
        });
    }
    let version = params
        .version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .unwrap_or(DEFAULT_TEMPLATE_VERSION)
        .to_string();
    let description = params
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Portable team charter exported from {project_name}."));
    let mut skills = skills
        .iter()
        .filter(|skill| skill.enabled)
        .map(|skill| TeamTemplateSkillRef {
            name: skill.name.clone(),
            version_hash: skill.sha256.clone(),
        })
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    skills.truncate(TEAM_TEMPLATE_MAX_SKILLS);
    if skills.len() == TEAM_TEMPLATE_MAX_SKILLS {
        warnings.push(format!(
            "skill references were capped at {TEAM_TEMPLATE_MAX_SKILLS}"
        ));
    }
    let mut template = TeamTemplate {
        schema_version: TEAM_TEMPLATE_SCHEMA_VERSION,
        template_id: String::new(),
        name: params.name.trim().to_string(),
        version,
        description,
        content_hash: String::new(),
        leader: leader_slot,
        departments: department_export,
        labour_defaults: TeamTemplateLabourDefaults {
            profile: "coding".to_string(),
            model_role: labour_model_role,
            bundle: "coding".to_string(),
            max_concurrency: 6,
        },
        rules: default_rules(),
        budgets: TeamTemplateBudgets {
            defaults: TeamTemplateBudget {
                token_budget: budget.token_budget,
                cost_budget_usd: budget.cost_budget_usd,
            },
            caps: TeamTemplateBudget {
                token_budget: budget.token_budget,
                cost_budget_usd: budget.cost_budget_usd,
            },
        },
        model_bindings: bindings,
        tool_policy: tool_policy_for_roles(),
        skills,
        conventions: default_conventions(),
        provenance: vp_protocol::types::TeamTemplateProvenance {
            source_project: Some(project_name.to_string()),
        },
    };
    finalize_team_template(&mut template)?;
    Ok(template)
}

// ---------------------------------------------------------------------------
// Preview planning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct SlotPlan {
    preview: TeamTemplateSlotPreview,
    hire: Option<PendingHirePlan>,
}

#[derive(Debug, Clone, Default)]
struct PreviewPlan {
    preview: TeamTemplatePreview,
    hires: Vec<crate::team_template::PendingHirePlan>,
}

#[derive(Debug, Clone)]
pub struct PendingHirePlan {
    pub slot: String,
    pub role: String,
    pub department: Option<String>,
    pub component: Option<String>,
    pub title: String,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub parent_identity_id: Option<String>,
}

fn slot_key(role: &str, department: Option<&str>, component: Option<&str>, title: &str) -> String {
    match role {
        "manager" => format!(
            "manager:{}:{}",
            department.unwrap_or(""),
            component.unwrap_or("")
        ),
        _ => format!(
            "worker:{}:{}:{}",
            department.unwrap_or(""),
            component.unwrap_or(""),
            title
        ),
    }
}

fn identity_matches_slot(
    identity: &AgentIdentity,
    role: &str,
    department: Option<&str>,
    component: Option<&str>,
    title: &str,
) -> bool {
    identity.status != "retired"
        && identity.role == role
        && opt_eq(identity.department.as_deref(), department)
        // A Manager owns a department, not a component workspace. Ignore a
        // stray component label when matching the department slot.
        && (role == "manager" || opt_eq(identity.component.as_deref(), component))
        && opt_eq(identity.title.as_deref(), Some(title))
}

fn identity_matches_department(
    identity: &AgentIdentity,
    role: &str,
    department: Option<&str>,
    component: Option<&str>,
) -> bool {
    identity.status != "retired"
        && identity.role == role
        && opt_eq(identity.department.as_deref(), department)
        && (role == "manager" || opt_eq(identity.component.as_deref(), component))
}

fn request_matches_slot(
    request: &AgentHireRequest,
    role: &str,
    department: Option<&str>,
    component: Option<&str>,
    title: &str,
) -> bool {
    request.status == "pending"
        && request.role == role
        && opt_eq(request.department.as_deref(), department)
        && (role == "manager" || opt_eq(request.component.as_deref(), component))
        && opt_eq(request.title.as_deref(), Some(title))
}

fn opt_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.trim() == right.trim(),
        _ => false,
    }
}

fn resolution_for<'a>(
    resolutions: &'a [TeamTemplateResolution],
    slot: &str,
) -> Option<&'a TeamTemplateResolution> {
    resolutions
        .iter()
        .find(|resolution| resolution.slot == slot)
}

fn resolution_action(resolution: &TeamTemplateResolution) -> Result<&'static str> {
    match resolution.action.trim().to_ascii_lowercase().as_str() {
        "skip" => Ok("skip"),
        "merge" => Ok("merge"),
        "rename" => Ok("rename"),
        "retire" => Err(template_error(
            team_template_codes::TEMPLATE_RESOLUTION_INVALID,
            "retire is not an apply action; retire the identity through the S6 agent.retire RPC, then re-apply",
        )),
        other => Err(template_error(
            team_template_codes::TEMPLATE_RESOLUTION_INVALID,
            format!("unknown conflict resolution: {other}"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_department(
    template: &TeamTemplate,
    department: &TeamTemplateDepartment,
    identities: &[AgentIdentity],
    pending_requests: &[AgentHireRequest],
    resolutions: &[TeamTemplateResolution],
    leader_identity_id: Option<&str>,
    preview: &mut TeamTemplatePreview,
    hires: &mut Vec<PendingHirePlan>,
) -> Result<TeamTemplateDepartmentPreview> {
    let department_name = department.name.trim();
    let manager_slot_name = slot_key(
        "manager",
        Some(department_name),
        department.manager.component.as_deref(),
        "",
    );
    let manager_plan = plan_one_slot(
        template,
        "manager",
        manager_slot_name.as_str(),
        department_name,
        department.manager.component.as_deref(),
        &department.manager,
        identities,
        pending_requests,
        resolutions,
        leader_identity_id,
        preview,
    )?;
    if let Some(hire) = manager_plan.hire.clone() {
        hires.push(hire);
    }
    let manager_preview = manager_plan.preview.clone();
    let mut workers = Vec::new();
    for worker in &department.workers {
        let key = slot_key(
            "worker",
            Some(department_name),
            worker.component.as_deref(),
            &worker.title,
        );
        let plan = plan_one_slot(
            template,
            "worker",
            key.as_str(),
            department_name,
            worker.component.as_deref(),
            worker,
            identities,
            pending_requests,
            resolutions,
            manager_plan.preview.existing_identity_id.as_deref(),
            preview,
        )?;
        if let Some(hire) = plan.hire.clone() {
            hires.push(hire);
        }
        workers.push(plan.preview);
    }
    Ok(TeamTemplateDepartmentPreview {
        name: department_name.to_string(),
        objective: department.objective.clone(),
        manager: manager_preview,
        workers,
    })
}

#[allow(clippy::too_many_arguments)]
fn plan_one_slot(
    template: &TeamTemplate,
    role: &str,
    slot: &str,
    department: &str,
    component: Option<&str>,
    spec: &TeamTemplateRoleSlot,
    identities: &[AgentIdentity],
    pending_requests: &[AgentHireRequest],
    resolutions: &[TeamTemplateResolution],
    parent_identity_id: Option<&str>,
    preview: &mut TeamTemplatePreview,
) -> Result<SlotPlan> {
    let mut title = spec.title.trim().to_string();
    let mut action = "create_pending_hire".to_string();
    let mut existing_identity_id = None;
    let mut pending_hire_request_id = None;
    let mut conflict_reason = None;
    let mut planned_hire = None;
    let resolution = resolution_for(resolutions, slot);
    if let Some(resolution) = resolution {
        match resolution_action(resolution)? {
            "skip" => {
                action = "skipped".to_string();
                preview.skipped_slots.push(slot.to_string());
                return Ok(SlotPlan {
                    preview: TeamTemplateSlotPreview {
                        slot: slot.to_string(),
                        role: role.to_string(),
                        department: Some(department.to_string()),
                        component: component.map(str::to_string),
                        title,
                        profile: spec.profile.clone(),
                        model: None,
                        action,
                        existing_identity_id,
                        pending_hire_request_id,
                        conflict_reason,
                    },
                    hire: None,
                });
            }
            "rename" => {
                let Some(new_title) = resolution
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                else {
                    return Err(template_error(
                        team_template_codes::TEMPLATE_RESOLUTION_INVALID,
                        format!("rename resolution for {slot} requires a title"),
                    ));
                };
                title = new_title.to_string();
                action = "renamed".to_string();
            }
            "merge" => {
                let Some(identity_id) = resolution
                    .identity_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                else {
                    return Err(template_error(
                        team_template_codes::TEMPLATE_RESOLUTION_INVALID,
                        format!("merge resolution for {slot} requires an identity_id"),
                    ));
                };
                let Some(identity) = identities
                    .iter()
                    .find(|identity| identity.id == identity_id)
                else {
                    return Err(template_error(
                        team_template_codes::TEMPLATE_RESOLUTION_INVALID,
                        format!("merge target identity not found: {identity_id}"),
                    ));
                };
                if identity.status == "retired" || identity.role != role {
                    return Err(template_error(
                        team_template_codes::TEMPLATE_RESOLUTION_INVALID,
                        format!("merge target {identity_id} is not a live {role}"),
                    ));
                }
                if !opt_eq(identity.department.as_deref(), Some(department)) {
                    return Err(template_error(
                        team_template_codes::TEMPLATE_RESOLUTION_INVALID,
                        format!("merge target {identity_id} belongs to another department"),
                    ));
                }
                existing_identity_id = Some(identity.id.clone());
                action = "existing".to_string();
                preview.existing_slots.push(slot.to_string());
                let model = spec
                    .model_role
                    .as_ref()
                    .and_then(|key| template.model_bindings.get(key))
                    .cloned();
                return Ok(SlotPlan {
                    preview: TeamTemplateSlotPreview {
                        slot: slot.to_string(),
                        role: role.to_string(),
                        department: Some(department.to_string()),
                        component: component.map(str::to_string),
                        title,
                        profile: identity
                            .profile
                            .clone()
                            .unwrap_or_else(|| spec.profile.clone()),
                        model,
                        action,
                        existing_identity_id,
                        pending_hire_request_id,
                        conflict_reason,
                    },
                    hire: None,
                });
            }
            _ => unreachable!(),
        }
    }

    if let Some(identity) = identities
        .iter()
        .find(|identity| identity_matches_slot(identity, role, Some(department), component, &title))
    {
        existing_identity_id = Some(identity.id.clone());
        action = "existing".to_string();
        preview.existing_slots.push(slot.to_string());
        let model = spec
            .model_role
            .as_ref()
            .and_then(|key| template.model_bindings.get(key))
            .cloned();
        return Ok(SlotPlan {
            preview: TeamTemplateSlotPreview {
                slot: slot.to_string(),
                role: role.to_string(),
                department: Some(department.to_string()),
                component: component.map(str::to_string),
                title,
                profile: identity
                    .profile
                    .clone()
                    .unwrap_or_else(|| spec.profile.clone()),
                model,
                action,
                existing_identity_id,
                pending_hire_request_id,
                conflict_reason,
            },
            hire: None,
        });
    }
    if let Some(request) = pending_requests
        .iter()
        .find(|request| request_matches_slot(request, role, Some(department), component, &title))
    {
        pending_hire_request_id = Some(request.id.clone());
        action = "existing".to_string();
        preview.existing_slots.push(slot.to_string());
        return Ok(SlotPlan {
            preview: TeamTemplateSlotPreview {
                slot: slot.to_string(),
                role: role.to_string(),
                department: Some(department.to_string()),
                component: component.map(str::to_string),
                title,
                profile: spec.profile.clone(),
                model: None,
                action,
                existing_identity_id,
                pending_hire_request_id,
                conflict_reason,
            },
            hire: None,
        });
    }
    let conflicting = identities
        .iter()
        .find(|identity| identity_matches_department(identity, role, Some(department), component));
    if let Some(identity) = conflicting {
        conflict_reason = Some(format!(
            "a live {role} already owns this department/component with title {}",
            identity.title.as_deref().unwrap_or("(untitled)")
        ));
        action = "conflict".to_string();
        existing_identity_id = Some(identity.id.clone());
        if preview.conflicts.len() < MAX_TEMPLATE_CONFLICT_ROWS {
            preview.conflicts.push(TeamTemplateConflict {
                slot: slot.to_string(),
                code: team_template_codes::TEMPLATE_CONFLICT.to_string(),
                existing_identity_id: Some(identity.id.clone()),
                proposed_title: title.clone(),
                message: conflict_reason.clone().unwrap_or_default(),
            });
        }
    } else if role != "leader" && parent_identity_id.is_none() {
        action = "blocked".to_string();
        preview.blocked_slots.push(slot.to_string());
        if preview.warnings.len() < MAX_TEMPLATE_WARNINGS {
            preview.warnings.push(format!(
                "{slot} is blocked: its required parent identity does not exist yet"
            ));
        }
    } else {
        preview.new_hire_slots.push(slot.to_string());
        planned_hire = Some(PendingHirePlan {
            slot: slot.to_string(),
            role: role.to_string(),
            department: Some(department.to_string()),
            component: component.map(str::to_string),
            title: title.clone(),
            profile: Some(spec.profile.clone()),
            model: spec
                .model_role
                .as_ref()
                .and_then(|key| template.model_bindings.get(key))
                .cloned(),
            parent_identity_id: parent_identity_id.map(str::to_string),
        });
    }
    Ok(SlotPlan {
        preview: TeamTemplateSlotPreview {
            slot: slot.to_string(),
            role: role.to_string(),
            department: Some(department.to_string()),
            component: component.map(str::to_string),
            title: title.clone(),
            profile: spec.profile.clone(),
            model: None,
            action,
            existing_identity_id,
            pending_hire_request_id,
            conflict_reason,
        },
        hire: planned_hire,
    })
}

fn build_preview_plan(
    project_id: &str,
    template: &TeamTemplate,
    identities: &[AgentIdentity],
    pending_requests: &[AgentHireRequest],
    resolutions: &[TeamTemplateResolution],
    context: &TeamTemplateValidationContext,
) -> Result<PreviewPlan> {
    let mut preview = TeamTemplatePreview {
        project_id: project_id.to_string(),
        template_id: template.template_id.clone(),
        name: template.name.clone(),
        version: template.version.clone(),
        content_hash: template.content_hash.clone(),
        valid: true,
        apply_allowed: true,
        requires_approval: true,
        leader_overlay: false,
        leader_identity_id: None,
        departments: Vec::new(),
        proposed_org: Vec::new(),
        new_hire_slots: Vec::new(),
        existing_slots: Vec::new(),
        skipped_slots: Vec::new(),
        blocked_slots: Vec::new(),
        conflicts: Vec::new(),
        budget_changes: Vec::new(),
        model_gaps: Vec::new(),
        tool_gaps: Vec::new(),
        policy_conflicts: Vec::new(),
        warnings: Vec::new(),
    };
    let live_leader = identities
        .iter()
        .find(|identity| identity.role == "leader" && identity.status != "retired");
    if let Some(leader) = live_leader {
        preview.leader_overlay = true;
        preview.leader_identity_id = Some(leader.id.clone());
        preview.existing_slots.push("leader".to_string());
        preview.proposed_org.push(format!(
            "Leader — {} (existing overlay; never overwritten)",
            leader.title.as_deref().unwrap_or("Leader")
        ));
    } else {
        preview
            .warnings
            .push("target project has no live Leader; apply cannot create one".to_string());
        preview.proposed_org.push(format!(
            "Leader — {} (charter overlay; no live Leader exists)",
            template.leader.title
        ));
    }
    let mut hires = Vec::new();
    for department in &template.departments {
        let planned = plan_department(
            template,
            department,
            identities,
            pending_requests,
            resolutions,
            live_leader.map(|leader| leader.id.as_str()),
            &mut preview,
            &mut hires,
        )?;
        preview.proposed_org.push(format!(
            "Department — {} ({})",
            planned.name, planned.objective
        ));
        if !planned.manager.title.is_empty() {
            preview.proposed_org.push(format!(
                "  Manager — {} [{}]",
                planned.manager.title, planned.manager.action
            ));
        }
        for worker in &planned.workers {
            preview.proposed_org.push(format!(
                "  Worker — {} [{}]{}",
                worker.title,
                worker.action,
                worker
                    .component
                    .as_ref()
                    .map(|component| format!(" ({component})"))
                    .unwrap_or_default()
            ));
        }
        preview.departments.push(planned);
    }
    collect_target_gaps(
        template,
        context,
        &mut preview.budget_changes,
        &mut preview.model_gaps,
        &mut preview.tool_gaps,
        &mut preview.policy_conflicts,
        &mut preview.warnings,
    );
    if !preview.conflicts.is_empty()
        || !preview.model_gaps.is_empty()
        || !preview.tool_gaps.is_empty()
        || !preview.policy_conflicts.is_empty()
    {
        preview.apply_allowed = false;
    }
    if template
        .departments
        .iter()
        .any(|department| !department.workers.is_empty())
        && live_leader.is_none()
    {
        preview.apply_allowed = false;
        preview.conflicts.push(TeamTemplateConflict {
            slot: "leader".to_string(),
            code: team_template_codes::TEMPLATE_APPLY_CONFLICT.to_string(),
            existing_identity_id: None,
            proposed_title: template.leader.title.clone(),
            message: "a live Leader is required before Manager/Worker hire requests can be created"
                .to_string(),
        });
    }
    if preview.apply_allowed && !preview.blocked_slots.is_empty() {
        // Blocked slots are staged: applying creates the requests that have a
        // resolved parent and leaves the rest visible for a later re-apply.
        preview.warnings.push(
            "some slots are blocked by a pending parent; apply can create the resolvable requests now"
                .to_string(),
        );
    }
    if preview.warnings.len() > MAX_TEMPLATE_WARNINGS {
        preview.warnings.truncate(MAX_TEMPLATE_WARNINGS);
    }
    Ok(PreviewPlan { preview, hires })
}

// ---------------------------------------------------------------------------
// Engine surface
// ---------------------------------------------------------------------------

impl Engine {
    pub async fn export_team_template(
        &self,
        params: TeamTemplateExportParams,
    ) -> Result<TeamTemplateExportResult> {
        let project_id = params.project_id.trim().to_string();
        if project_id.is_empty() {
            return Err(template_error(
                team_template_codes::TEMPLATE_PROJECT_NOT_FOUND,
                "project_id is required",
            ));
        }
        if params.name.trim().is_empty() {
            return Err(template_error(
                team_template_codes::TEMPLATE_INVALID,
                "template name is required",
            ));
        }
        let store = self.store.clone();
        let export_params = params.clone();
        let (project, identities, skills, budget) = tokio::task::spawn_blocking(move || {
            let project = store.get_project(&project_id)?.ok_or_else(|| {
                template_error(
                    team_template_codes::TEMPLATE_PROJECT_NOT_FOUND,
                    format!("project not found: {project_id}"),
                )
            })?;
            let identities = store.list_agent_identities(&project.id, None, None)?;
            let skills = store.list_skills(Some(&project.id), true)?;
            let budget = store.get_project_budget(&project.id)?;
            Ok::<_, anyhow::Error>((project, identities, skills, budget))
        })
        .await??;
        let mut warnings = Vec::new();
        let template = build_exported_template(
            &project.name,
            &identities,
            &skills,
            &budget,
            &export_params,
            &mut warnings,
        )?;
        // An export is a portable artifact, so run the same structural check
        // import will run. A project name/title that leaks a path or URL fails
        // closed instead of silently producing a non-portable template.
        validate_team_template_shape(&template)?;
        let json = team_template_json(&template)?;
        let markdown = render_team_markdown(&template);
        let source_project_id = project.id.clone();
        let register = template.clone();
        let store = self.store.clone();
        let (entry, _event, _changed) = tokio::task::spawn_blocking(move || {
            vp_store::store::team_template::register_team_template(
                &store,
                &source_project_id,
                &register,
                Some(&source_project_id),
                vp_protocol::types::TEAM_TEMPLATE_EXPORTED_EVENT,
            )
        })
        .await??;
        let _ = entry;
        Ok(TeamTemplateExportResult {
            template,
            json,
            markdown,
            warnings,
        })
    }

    pub async fn preview_team_template(
        &self,
        params: TeamTemplateImportParams,
        known_models: Vec<String>,
    ) -> Result<TeamTemplateImportResult> {
        self.import_team_template_inner(params, known_models, true)
            .await
    }

    pub async fn import_team_template(
        &self,
        params: TeamTemplateImportParams,
        known_models: Vec<String>,
    ) -> Result<TeamTemplateImportResult> {
        self.import_team_template_inner(params, known_models, false)
            .await
    }

    async fn import_team_template_inner(
        &self,
        params: TeamTemplateImportParams,
        known_models: Vec<String>,
        force_preview: bool,
    ) -> Result<TeamTemplateImportResult> {
        let project_id = params.project_id.trim().to_string();
        if project_id.is_empty() {
            return Err(template_error(
                team_template_codes::TEMPLATE_PROJECT_NOT_FOUND,
                "project_id is required",
            ));
        }
        // `dry_run` is a preview request, never a way to skip the approval
        // gate. Apply always requires explicit approval; the preview entry
        // point passes `force_preview` instead of `apply`.
        if params.apply && !params.approved && !force_preview {
            return Err(template_error(
                team_template_codes::TEMPLATE_APPROVAL_REQUIRED,
                "applying a team template requires explicit approved: true",
            ));
        }
        let template = parse_team_template_json(&params.json)?;
        let store = self.store.clone();
        let project = store.get_project(&project_id)?.ok_or_else(|| {
            template_error(
                team_template_codes::TEMPLATE_PROJECT_NOT_FOUND,
                format!("project not found: {project_id}"),
            )
        })?;
        let identities = store.list_agent_identities(&project.id, None, None)?;
        let pending_requests = vp_store::store::employment::list_agent_hire_requests(
            &store,
            &vp_protocol::types::AgentHireListParams {
                identity_id: None,
                project_id: project.id.clone(),
                status: Some("pending".to_string()),
                limit: Some(500),
            },
        )?
        .requests;
        let budget = store.get_project_budget(&project.id)?;
        let identity_policy = store.get_agent_identity_policy(&project.id)?;
        let context = TeamTemplateValidationContext {
            known_models,
            project_budget: budget,
            identity_policy,
        };
        let plan = build_preview_plan(
            &project.id,
            &template,
            &identities,
            &pending_requests,
            &params.resolutions,
            &context,
        )?;
        let mut warnings = plan.preview.warnings.clone();
        // A dry run always returns the preview, even if `apply` was also set.
        // Mutations happen only on the explicit apply path below.
        if force_preview || params.dry_run {
            return Ok(TeamTemplateImportResult {
                project_id: project.id,
                template,
                preview: plan.preview,
                applied: false,
                changed: false,
                entry: None,
                pending_hire_request_ids: Vec::new(),
                warnings,
            });
        }
        if !params.apply {
            // Import-only: register the read-only library record. It creates no
            // link, identity, hire request, or policy change.
            let register = template.clone();
            let project_id = project.id.clone();
            let store = self.store.clone();
            let (entry, _event, changed) = tokio::task::spawn_blocking(move || {
                vp_store::store::team_template::register_team_template(
                    &store,
                    &project_id,
                    &register,
                    None,
                    vp_protocol::types::TEAM_TEMPLATE_IMPORTED_EVENT,
                )
            })
            .await??;
            return Ok(TeamTemplateImportResult {
                project_id: project.id,
                template,
                preview: plan.preview,
                applied: false,
                changed,
                entry: Some(entry),
                pending_hire_request_ids: Vec::new(),
                warnings,
            });
        }
        if !plan.preview.apply_allowed {
            if !plan.preview.model_gaps.is_empty() {
                return Err(template_error(
                    team_template_codes::TEMPLATE_MODEL_INVALID,
                    plan.preview.model_gaps.join("; "),
                ));
            }
            if !plan.preview.tool_gaps.is_empty() {
                return Err(template_error(
                    team_template_codes::TEMPLATE_TOOL_INVALID,
                    plan.preview.tool_gaps.join("; "),
                ));
            }
            if !plan.preview.policy_conflicts.is_empty() {
                return Err(template_error(
                    team_template_codes::TEMPLATE_BUDGET_WIDENING,
                    plan.preview.policy_conflicts.join("; "),
                ));
            }
            return Err(template_error(
                team_template_codes::TEMPLATE_APPLY_CONFLICT,
                format!(
                    "template has {} unresolved conflict(s); resolve with skip/merge/rename or S6 retire and re-apply",
                    plan.preview.conflicts.len()
                ),
            ));
        }
        if !plan.preview.conflicts.is_empty() {
            return Err(template_error(
                team_template_codes::TEMPLATE_APPLY_CONFLICT,
                format!(
                    "template has {} unresolved conflict(s)",
                    plan.preview.conflicts.len()
                ),
            ));
        }
        let pending_plan = plan.hires.clone();
        let store_apply = self.store.clone();
        let project_id = project.id.clone();
        let template_apply = template.clone();
        let operator = params.operator.clone();
        let (entry, pending_hire_request_ids, changed) = tokio::task::spawn_blocking(move || {
            let hires = pending_plan
                .into_iter()
                .map(|hire| vp_store::store::team_template::PendingTemplateHire {
                    slot: hire.slot,
                    role: hire.role,
                    department: hire.department,
                    component: hire.component,
                    title: hire.title,
                    profile: hire.profile,
                    model: hire.model,
                    parent_identity_id: hire.parent_identity_id,
                })
                .collect::<Vec<_>>();
            vp_store::store::team_template::apply_team_template(
                &store_apply,
                &project_id,
                &template_apply,
                None,
                operator.as_deref(),
                &hires,
            )
        })
        .await??;
        if changed {
            warnings.push(format!(
                "applied {} pending hire request(s); no identity was created",
                pending_hire_request_ids.len()
            ));
        } else {
            warnings.push("template was already applied; no new request was created".to_string());
        }
        Ok(TeamTemplateImportResult {
            project_id: project.id,
            template,
            preview: plan.preview,
            applied: true,
            changed,
            entry: Some(entry),
            pending_hire_request_ids,
            warnings,
        })
    }

    pub async fn list_team_templates(
        &self,
        project_id: Option<String>,
        limit: Option<u32>,
    ) -> Result<TeamTemplateListResult> {
        let store = self.store.clone();
        let project_id = project_id.map(|value| value.trim().to_string());
        let limit = limit.unwrap_or(vp_protocol::types::TEAM_TEMPLATE_DEFAULT_LIMIT);
        let (templates, truncated) = tokio::task::spawn_blocking(move || {
            vp_store::store::team_template::list_team_templates(
                &store,
                project_id.as_deref(),
                limit,
            )
        })
        .await??;
        Ok(TeamTemplateListResult {
            templates,
            truncated,
        })
    }

    pub async fn get_team_template(
        &self,
        template_id: &str,
        project_id: Option<&str>,
    ) -> Result<TeamTemplateGetResult> {
        let store = self.store.clone();
        let template_id = template_id.trim().to_string();
        let project_id = project_id.map(str::to_string);
        let lookup_id = template_id.clone();
        let lookup_project = project_id.clone();
        let (record, entry) = tokio::task::spawn_blocking(move || {
            vp_store::store::team_template::get_team_template(
                &store,
                &lookup_id,
                lookup_project.as_deref(),
            )
        })
        .await??
        .ok_or_else(|| {
            template_error(
                team_template_codes::TEMPLATE_NOT_FOUND,
                format!("team template not found: {template_id}"),
            )
        })?;
        validate_team_template_shape(&record.template)?;
        Ok(TeamTemplateGetResult {
            entry,
            json: team_template_json(&record.template)?,
            markdown: render_team_markdown(&record.template),
        })
    }

    pub async fn diff_team_template(
        &self,
        project_id: &str,
        template_id: &str,
        known_models: Vec<String>,
    ) -> Result<TeamTemplateDiffResult> {
        let project_id = project_id.trim().to_string();
        let template_id = template_id.trim().to_string();
        let store = self.store.clone();
        let lookup_template = template_id.clone();
        let lookup_project = project_id.clone();
        let (record, entry) = tokio::task::spawn_blocking(move || {
            vp_store::store::team_template::get_team_template(
                &store,
                &lookup_template,
                Some(&lookup_project),
            )
        })
        .await??
        .ok_or_else(|| {
            template_error(
                team_template_codes::TEMPLATE_NOT_FOUND,
                format!("team template not found: {template_id}"),
            )
        })?;
        validate_team_template_shape(&record.template)?;
        let store = self.store.clone();
        let project = store.get_project(&project_id)?.ok_or_else(|| {
            template_error(
                team_template_codes::TEMPLATE_PROJECT_NOT_FOUND,
                format!("project not found: {project_id}"),
            )
        })?;
        let identities = store.list_agent_identities(&project.id, None, None)?;
        let pending_requests = vp_store::store::employment::list_agent_hire_requests(
            &store,
            &vp_protocol::types::AgentHireListParams {
                identity_id: None,
                project_id: project.id.clone(),
                status: Some("pending".to_string()),
                limit: Some(500),
            },
        )?
        .requests;
        let budget = store.get_project_budget(&project.id)?;
        let identity_policy = store.get_agent_identity_policy(&project.id)?;
        let context = TeamTemplateValidationContext {
            known_models,
            project_budget: budget,
            identity_policy,
        };
        let plan = build_preview_plan(
            &project.id,
            &record.template,
            &identities,
            &pending_requests,
            &[],
            &context,
        )?;
        let mut rows = Vec::new();
        let mut push = |kind: &str, slot: &str, detail: String| {
            if rows.len() < MAX_TEMPLATE_DIFF_ROWS {
                rows.push(TeamTemplateDiffRow {
                    kind: kind.to_string(),
                    slot: slot.to_string(),
                    detail,
                });
            }
        };
        if plan.preview.leader_overlay {
            push(
                "leader_overlay",
                "leader",
                "an existing Leader is kept and never overwritten".to_string(),
            );
        } else {
            push(
                "leader_missing",
                "leader",
                "the charter has a Leader slot but the project has no live Leader".to_string(),
            );
        }
        for department in &plan.preview.departments {
            push(
                "department",
                &department.name,
                format!("{} Manager slot", department.manager.action),
            );
            for worker in &department.workers {
                push("worker", &worker.slot, worker.action.clone());
            }
        }
        for conflict in &plan.preview.conflicts {
            push("conflict", &conflict.slot, conflict.message.clone());
        }
        for gap in &plan.preview.model_gaps {
            push("model_gap", "model", gap.clone());
        }
        for gap in &plan.preview.tool_gaps {
            push("tool_gap", "tool", gap.clone());
        }
        for change in &plan.preview.budget_changes {
            push("budget", "budget", change.clone());
        }
        for conflict in &plan.preview.policy_conflicts {
            push("policy", "policy", conflict.clone());
        }
        if !entry.applied {
            push(
                "not_applied",
                "template",
                "library record only; no apply link exists".to_string(),
            );
        }
        Ok(TeamTemplateDiffResult {
            project_id: project_id.clone(),
            template_id,
            content_hash: record.content_hash,
            truncated: rows.len() >= MAX_TEMPLATE_DIFF_ROWS,
            rows,
            warnings: plan.preview.warnings,
        })
    }

    pub async fn replay_team_templates(
        &self,
        project_id: &str,
        repair: bool,
    ) -> Result<vp_protocol::types::TeamTemplateReplayReport> {
        let store = self.store.clone();
        let project_id = project_id.trim().to_string();
        tokio::task::spawn_blocking(move || {
            vp_store::store::team_template::replay_team_templates(&store, &project_id, repair)
        })
        .await?
    }

    pub async fn team_template_record(
        &self,
        template_id: &str,
    ) -> Result<Option<vp_protocol::types::TeamTemplateRecord>> {
        let store = self.store.clone();
        let template_id = template_id.trim().to_string();
        tokio::task::spawn_blocking(move || {
            vp_store::store::team_template::get_team_template_record(&store, &template_id)
        })
        .await?
    }
}

/// Build a minimal explicit model list for the daemon. The client never
/// supplies this list, so a template cannot declare a model that the daemon is
/// not configured to use.
pub fn configured_model_references(info: &vp_protocol::types::AgentInfo) -> Vec<String> {
    let mut models = BTreeSet::new();
    if let Some(model) = info
        .model
        .as_ref()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
    {
        models.insert(model.to_string());
    }
    for role in [
        &info.role_models.leader,
        &info.role_models.manager,
        &info.role_models.worker,
        &info.role_models.verifier,
        &info.role_models.labour,
        &info.role_models.scout,
    ] {
        if let Some(model) = role
            .model
            .as_ref()
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
        {
            models.insert(model.to_string());
        }
    }
    models.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{
        AgentHireApproveParams, AgentIdentityPolicy, CreateAgentIdentityParams,
        TeamTemplateExportParams, TeamTemplateImportParams, TeamTemplateResolution,
    };
    use vp_store::Store;

    fn init_repo(path: &std::path::Path) {
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "team@test.local"],
            vec!["config", "user.name", "Team Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = std::process::Command::new("git")
                .args(&args)
                .current_dir(path)
                .status()
                .expect("git");
            assert!(status.success(), "git {:?} failed", args);
        }
    }

    fn test_engine(dir: &TempDir, name: &str) -> (Engine, String) {
        let store = Store::open(dir.path()).expect("store");
        let engine = Engine::new(store);
        let root = dir.path().join(format!("repo-{name}"));
        std::fs::create_dir_all(&root).expect("repo");
        std::fs::write(root.join("README.md"), "team template test\n").expect("readme");
        init_repo(&root);
        let project = engine
            .store()
            .add_project(root.to_str().expect("path"), Some(name))
            .expect("project");
        (engine, project.id)
    }

    async fn enable_identity_policy(engine: &Engine, project_id: &str) {
        engine
            .set_agent_identity_policy(
                project_id,
                AgentIdentityPolicy {
                    enabled: true,
                    ..AgentIdentityPolicy::default()
                },
            )
            .await
            .expect("policy");
    }

    async fn create_identity(
        engine: &Engine,
        project_id: &str,
        role: &str,
        title: &str,
        department: Option<&str>,
        component: Option<&str>,
        parent: Option<&str>,
    ) -> String {
        engine
            .create_agent_identity(CreateAgentIdentityParams {
                project_id: project_id.to_string(),
                role: role.to_string(),
                title: Some(title.to_string()),
                department: department.map(str::to_string),
                component: component.map(str::to_string),
                profile: Some(
                    if role == "worker" {
                        "coding"
                    } else {
                        "generic"
                    }
                    .to_string(),
                ),
                model: Some("test-model".to_string()),
                parent_identity_id: parent.map(str::to_string),
                policy: None,
            })
            .await
            .expect("identity")
            .identity
            .id
    }

    async fn source_project(engine: &Engine, project_id: &str) -> (String, String, String) {
        enable_identity_policy(engine, project_id).await;
        let leader =
            create_identity(engine, project_id, "leader", "Leader", None, None, None).await;
        let manager = create_identity(
            engine,
            project_id,
            "manager",
            "Platform Manager",
            Some("Platform"),
            Some("platform"),
            Some(&leader),
        )
        .await;
        let worker = create_identity(
            engine,
            project_id,
            "worker",
            "Auth Worker",
            Some("Platform"),
            Some("auth"),
            Some(&manager),
        )
        .await;
        (leader, manager, worker)
    }

    fn export_params(project_id: &str, name: &str) -> TeamTemplateExportParams {
        TeamTemplateExportParams {
            project_id: project_id.to_string(),
            name: name.to_string(),
            version: Some("1.2.3".to_string()),
            description: Some("Portable platform team".to_string()),
        }
    }

    fn import_params(
        project_id: &str,
        json: &str,
        apply: bool,
        approved: bool,
    ) -> TeamTemplateImportParams {
        TeamTemplateImportParams {
            project_id: project_id.to_string(),
            json: json.to_string(),
            dry_run: !apply,
            apply,
            approved,
            operator: Some("test-operator".to_string()),
            resolutions: Vec::new(),
        }
    }

    fn typed_code(error: anyhow::Error) -> String {
        error
            .downcast_ref::<AgentIdentityError>()
            .map(|error| error.code.to_string())
            .unwrap_or_else(|| error.to_string())
    }

    #[tokio::test]
    async fn export_is_deterministic_and_contains_no_context() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir, "source");
        let (leader, manager, worker) = source_project(&engine, &project_id).await;
        engine
            .set_project_budget(
                &project_id,
                ProjectBudget {
                    token_budget: Some(250_000),
                    cost_budget_usd: Some(12.5),
                },
            )
            .await
            .expect("budget");

        let first = engine
            .export_team_template(export_params(&project_id, "Platform Team"))
            .await
            .expect("export");
        let second = engine
            .export_team_template(export_params(&project_id, "Platform Team"))
            .await
            .expect("export again");
        assert_eq!(first.template.content_hash, second.template.content_hash);
        assert_eq!(first.json, second.json);
        assert_eq!(first.template.template_id, second.template.template_id);
        assert_eq!(first.template.schema_version, TEAM_TEMPLATE_SCHEMA_VERSION);
        assert_eq!(first.template.departments.len(), 1);
        assert_eq!(first.template.departments[0].name, "Platform");
        assert_eq!(first.template.departments[0].workers.len(), 1);
        assert_eq!(
            first.template.provenance.source_project.as_deref(),
            Some("source")
        );
        assert_eq!(first.template.budgets.defaults.token_budget, Some(250_000));
        assert_eq!(
            first.template.model_bindings.get("leader"),
            Some(&"test-model".to_string())
        );
        assert!(first.json.contains(&first.template.content_hash));
        assert!(first.markdown.contains("Platform Team"));
        assert!(first.markdown.contains(&first.template.content_hash));
        assert_eq!(render_team_markdown(&first.template), first.markdown);

        for forbidden in [
            leader.as_str(),
            manager.as_str(),
            worker.as_str(),
            project_id.as_str(),
            "\"session_id\"",
            "\"worktree\"",
            "\"branch\"",
            "\"agent_id\"",
            "\"identity_id\"",
            "\"performance\"",
            "\"tool_output\"",
            "/tmp/",
            "repo-source",
        ] {
            assert!(
                !first.json.contains(forbidden),
                "export leaked {forbidden}:\n{}",
                first.json
            );
        }
        let parsed = parse_team_template_json(&first.json).expect("parse round trip");
        assert_eq!(parsed, first.template);
    }

    #[tokio::test]
    async fn preview_reports_leader_overlay_and_pending_hires_without_writes() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");

        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        let leader = create_identity(
            &engine,
            &target_id,
            "leader",
            "Local Leader",
            None,
            None,
            None,
        )
        .await;
        let preview = engine
            .preview_team_template(
                import_params(&target_id, &export.json, false, false),
                vec!["test-model".to_string()],
            )
            .await
            .expect("preview");
        assert!(preview.preview.valid);
        assert!(preview.preview.leader_overlay);
        assert_eq!(
            preview.preview.leader_identity_id.as_deref(),
            Some(leader.as_str())
        );
        assert_eq!(preview.preview.departments.len(), 1);
        assert_eq!(
            preview.preview.departments[0].manager.action,
            "create_pending_hire"
        );
        assert_eq!(preview.preview.departments[0].workers[0].action, "blocked");
        assert_eq!(preview.preview.new_hire_slots.len(), 1);
        assert!(preview.pending_hire_request_ids.is_empty());
        assert!(preview.entry.is_none());
        // Preview creates no hire request and no library record.
        let hires = engine
            .store()
            .list_agent_identities(&target_id, None, None)
            .expect("identities");
        assert_eq!(hires.len(), 1);
    }

    #[tokio::test]
    async fn apply_requires_approval_and_creates_only_pending_requests() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");

        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        let leader = create_identity(
            &engine,
            &target_id,
            "leader",
            "Local Leader",
            None,
            None,
            None,
        )
        .await;
        let denied = engine
            .import_team_template(
                import_params(&target_id, &export.json, true, false),
                vec!["test-model".to_string()],
            )
            .await
            .expect_err("approval required");
        assert_eq!(
            typed_code(denied),
            team_template_codes::TEMPLATE_APPROVAL_REQUIRED
        );

        let applied = engine
            .import_team_template(
                import_params(&target_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect("apply");
        assert!(applied.applied);
        assert!(applied.changed);
        assert_eq!(applied.pending_hire_request_ids.len(), 1);
        let entry = applied.entry.expect("entry");
        assert!(entry.applied);
        assert!(!entry.read_only);
        assert!(
            engine
                .store()
                .get_agent_identity(&leader)
                .expect("leader")
                .is_some()
        );
        let identities = engine
            .store()
            .list_agent_identities(&target_id, None, None)
            .expect("identities");
        assert_eq!(identities.len(), 1);
        let requests = vp_store::store::employment::list_agent_hire_requests(
            engine.store(),
            &vp_protocol::types::AgentHireListParams {
                identity_id: None,
                project_id: target_id.clone(),
                status: Some("pending".to_string()),
                limit: Some(50),
            },
        )
        .expect("hire requests");
        assert_eq!(requests.requests.len(), 1);
        assert_eq!(requests.requests[0].status, "pending");
        assert_eq!(
            requests.requests[0]
                .policy
                .get("team_template")
                .and_then(|value| value.get("content_hash"))
                .and_then(|value| value.as_str()),
            Some(export.template.content_hash.as_str())
        );
        let leader_after = engine
            .store()
            .get_agent_identity(&leader)
            .expect("leader")
            .expect("leader row");
        assert_eq!(leader_after.title.as_deref(), Some("Local Leader"));

        // Re-apply is idempotent: no duplicate request and no new identity.
        let again = engine
            .import_team_template(
                import_params(&target_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect("idempotent apply");
        assert!(again.applied);
        assert!(!again.changed);
        assert!(again.pending_hire_request_ids.is_empty());
        let requests = vp_store::store::employment::list_agent_hire_requests(
            engine.store(),
            &vp_protocol::types::AgentHireListParams {
                identity_id: None,
                project_id: target_id.clone(),
                status: Some("pending".to_string()),
                limit: Some(50),
            },
        )
        .expect("hire requests");
        assert_eq!(requests.requests.len(), 1);
    }

    #[tokio::test]
    async fn approving_manager_then_reapplying_creates_the_worker_request() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");
        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;
        let applied = engine
            .import_team_template(
                import_params(&target_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect("apply");
        assert_eq!(applied.pending_hire_request_ids.len(), 1);
        let manager_request = applied.pending_hire_request_ids[0].clone();
        let approved = engine
            .approve_agent_hire(AgentHireApproveParams {
                request_id: manager_request,
                operator: Some("reviewer".to_string()),
                approval_risk: Some("host_state".to_string()),
            })
            .await
            .expect("approve manager");
        let approved_identity = approved.identity.expect("manager identity");
        assert_eq!(
            approved_identity
                .policy
                .get("team_template")
                .and_then(|value| value.get("content_hash"))
                .and_then(|value| value.as_str()),
            Some(export.template.content_hash.as_str())
        );
        assert_eq!(
            approved_identity
                .policy
                .get("team_template")
                .and_then(|value| value.get("slot"))
                .and_then(|value| value.as_str()),
            Some("manager:Platform:")
        );
        let reapply = engine
            .import_team_template(
                import_params(&target_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect("staged re-apply");
        assert!(reapply.applied);
        assert!(reapply.changed);
        assert_eq!(reapply.pending_hire_request_ids.len(), 1);
        let identities = engine
            .store()
            .list_agent_identities(&target_id, None, None)
            .expect("identities");
        assert_eq!(identities.len(), 2);
        assert_eq!(
            identities
                .iter()
                .filter(|identity| identity.role == "manager")
                .count(),
            1
        );
        assert_eq!(
            identities
                .iter()
                .filter(|identity| identity.role == "worker")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn budget_widening_is_rejected_and_target_budget_is_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        engine
            .set_project_budget(
                &source_id,
                ProjectBudget {
                    token_budget: Some(500_000),
                    cost_budget_usd: Some(25.0),
                },
            )
            .await
            .expect("source budget");
        let export = engine
            .export_team_template(export_params(&source_id, "Large Team"))
            .await
            .expect("export");

        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;
        engine
            .set_project_budget(
                &target_id,
                ProjectBudget {
                    token_budget: Some(1_000),
                    cost_budget_usd: Some(0.5),
                },
            )
            .await
            .expect("target budget");
        let preview = engine
            .preview_team_template(
                import_params(&target_id, &export.json, false, false),
                vec!["test-model".to_string()],
            )
            .await
            .expect("preview");
        assert!(!preview.preview.apply_allowed);
        assert!(!preview.preview.policy_conflicts.is_empty());
        let error = engine
            .import_team_template(
                import_params(&target_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect_err("widening rejected");
        assert_eq!(
            typed_code(error),
            team_template_codes::TEMPLATE_BUDGET_WIDENING
        );
        let budget = engine
            .store()
            .get_project_budget(&target_id)
            .expect("budget");
        assert_eq!(budget.token_budget, Some(1_000));
        assert_eq!(budget.cost_budget_usd, Some(0.5));
    }

    #[tokio::test]
    async fn unknown_model_fails_closed_and_preview_shows_the_gap() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");
        let mut template = export.template.clone();
        template
            .model_bindings
            .insert("worker".to_string(), "unconfigured-model".to_string());
        finalize_team_template(&mut template).expect("hash");
        let json = team_template_json(&template).expect("json");
        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;
        let preview = engine
            .preview_team_template(
                import_params(&target_id, &json, false, false),
                vec!["test-model".to_string()],
            )
            .await
            .expect("preview");
        assert!(!preview.preview.apply_allowed);
        assert!(!preview.preview.model_gaps.is_empty());
        let error = engine
            .import_team_template(
                import_params(&target_id, &json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect_err("model rejected");
        assert_eq!(
            typed_code(error),
            team_template_codes::TEMPLATE_MODEL_INVALID
        );
    }

    #[tokio::test]
    async fn invalid_tool_bundle_and_unknown_fields_fail_closed() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");
        let mut template = export.template.clone();
        template
            .tool_policy
            .get_mut("leader")
            .expect("leader policy")
            .bundle = Some("unknown-bundle".to_string());
        finalize_team_template(&mut template).expect("hash");
        let json = team_template_json(&template).expect("json");
        let (engine, target_id) = test_engine(&dir, "target");
        let error = engine
            .preview_team_template(
                import_params(&target_id, &json, false, false),
                vec!["test-model".to_string()],
            )
            .await
            .expect_err("tool rejected");
        assert_eq!(
            typed_code(error),
            team_template_codes::TEMPLATE_TOOL_INVALID
        );

        let leaked = export
            .json
            .replacen("\"name\":", "\"worktree\": \"/tmp/repo\", \"name\":", 1);
        let error = engine
            .preview_team_template(
                import_params(&target_id, &leaked, false, false),
                vec!["test-model".to_string()],
            )
            .await
            .expect_err("unknown field rejected");
        assert_eq!(typed_code(error), team_template_codes::TEMPLATE_INVALID);
    }

    #[tokio::test]
    async fn target_identity_caps_block_apply_before_any_request() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Capped Team"))
            .await
            .expect("export");
        let (engine, target_id) = test_engine(&dir, "target");
        engine
            .set_agent_identity_policy(
                &target_id,
                AgentIdentityPolicy {
                    enabled: true,
                    max_managers: 0,
                    max_workers: 0,
                    hire_requires_approval: true,
                    retirement_requires_approval: true,
                },
            )
            .await
            .expect("policy");
        create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;
        let error = engine
            .import_team_template(
                import_params(&target_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect_err("cap must reject apply");
        let code = typed_code(error);
        assert!(
            code == "HIRE_CAP_EXCEEDED" || code == "IDENTITY_CAP_EXCEEDED",
            "unexpected cap code: {code}"
        );
        let identities = engine
            .store()
            .list_agent_identities(&target_id, None, None)
            .expect("identities");
        assert_eq!(identities.len(), 1);
        let requests = vp_store::store::employment::list_agent_hire_requests(
            engine.store(),
            &vp_protocol::types::AgentHireListParams {
                identity_id: None,
                project_id: target_id.clone(),
                status: None,
                limit: Some(50),
            },
        )
        .expect("hire requests");
        assert!(requests.requests.is_empty());
    }

    #[tokio::test]
    async fn context_leakage_and_invalid_tool_names_fail_closed() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Leak Team"))
            .await
            .expect("export");
        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;

        for (needle, code) in [
            (
                "https://github.com/example/repo",
                team_template_codes::TEMPLATE_CONTEXT_LEAK,
            ),
            (
                "sk-1234567890",
                team_template_codes::TEMPLATE_SECRET_DETECTED,
            ),
            (
                "/Users/example/repo",
                team_template_codes::TEMPLATE_CONTEXT_LEAK,
            ),
        ] {
            let mut template = export.template.clone();
            template.description = format!("leak {needle}");
            finalize_team_template(&mut template).expect("hash");
            let json = team_template_json(&template).expect("json");
            let error = engine
                .preview_team_template(
                    import_params(&target_id, &json, false, false),
                    vec!["test-model".to_string()],
                )
                .await
                .expect_err("leak rejected");
            assert_eq!(typed_code(error), code);
        }

        let mut template = export.template.clone();
        template
            .tool_policy
            .get_mut("worker")
            .expect("worker policy")
            .allow = vec!["not_a_real_tool".to_string()];
        finalize_team_template(&mut template).expect("hash");
        let json = team_template_json(&template).expect("json");
        let error = engine
            .preview_team_template(
                import_params(&target_id, &json, false, false),
                vec!["test-model".to_string()],
            )
            .await
            .expect_err("tool rejected");
        assert_eq!(
            typed_code(error),
            team_template_codes::TEMPLATE_TOOL_INVALID
        );
    }

    #[tokio::test]
    async fn conflict_requires_merge_and_never_overwrites_the_existing_manager() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");
        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        let leader =
            create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;
        let existing_manager = create_identity(
            &engine,
            &target_id,
            "manager",
            "Other Manager",
            Some("Platform"),
            Some("platform"),
            Some(&leader),
        )
        .await;
        let preview = engine
            .preview_team_template(
                import_params(&target_id, &export.json, false, false),
                vec!["test-model".to_string()],
            )
            .await
            .expect("preview");
        assert!(!preview.preview.apply_allowed);
        assert!(!preview.preview.conflicts.is_empty());
        let slot = preview.preview.departments[0].manager.slot.clone();
        let error = engine
            .import_team_template(
                import_params(&target_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect_err("conflict rejected");
        assert_eq!(
            typed_code(error),
            team_template_codes::TEMPLATE_APPLY_CONFLICT
        );

        let mut resolved = import_params(&target_id, &export.json, true, true);
        resolved.resolutions = vec![TeamTemplateResolution {
            slot,
            action: "merge".to_string(),
            identity_id: Some(existing_manager.clone()),
            title: None,
        }];
        let applied = engine
            .import_team_template(resolved, vec!["test-model".to_string()])
            .await
            .expect("merge apply");
        assert!(applied.applied);
        let manager = engine
            .store()
            .get_agent_identity(&existing_manager)
            .expect("manager")
            .expect("manager row");
        assert_eq!(manager.title.as_deref(), Some("Other Manager"));
        assert_eq!(manager.generation, 1);
    }

    #[tokio::test]
    async fn import_only_is_read_only_and_diff_reports_the_missing_link() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");
        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;
        let imported = engine
            .import_team_template(
                TeamTemplateImportParams {
                    project_id: target_id.clone(),
                    json: export.json.clone(),
                    dry_run: false,
                    apply: false,
                    approved: false,
                    operator: Some("test".to_string()),
                    resolutions: Vec::new(),
                },
                vec!["test-model".to_string()],
            )
            .await
            .expect("import only");
        assert!(!imported.applied);
        let entry = imported.entry.expect("entry");
        assert!(!entry.applied);
        assert!(entry.read_only);
        let diff = engine
            .diff_team_template(
                &target_id,
                &export.template.template_id,
                vec!["test-model".to_string()],
            )
            .await
            .expect("diff");
        assert!(diff.rows.iter().any(|row| row.kind == "not_applied"));
        assert!(!diff.rows.is_empty());
        let list = engine
            .list_team_templates(Some(target_id.clone()), Some(10))
            .await
            .expect("list");
        assert_eq!(list.templates.len(), 1);
        assert!(list.templates[0].read_only);
        let get = engine
            .get_team_template(&export.template.template_id, Some(&target_id))
            .await
            .expect("get");
        assert_eq!(get.entry.template_id, export.template.template_id);
        assert!(get.json.contains(&export.template.content_hash));
        assert!(get.markdown.contains(&export.template.name));
        let replay = engine
            .replay_team_templates(&target_id, false)
            .await
            .expect("replay");
        assert!(replay.mismatches.is_empty());
        assert_eq!(replay.templates, 1);
    }

    #[tokio::test]
    async fn session_bundle_import_restores_template_evidence_without_hiring() {
        let data_dir = TempDir::new().expect("data dir");
        let (source, source_id) = test_engine(&data_dir, "bundle-source");
        source_project(&source, &source_id).await;
        let export = source
            .export_team_template(export_params(&source_id, "Bundle Team"))
            .await
            .expect("export");
        let (target, target_id) = test_engine(&data_dir, "bundle-target");
        enable_identity_policy(&target, &target_id).await;
        create_identity(&target, &target_id, "leader", "Leader", None, None, None).await;
        let applied = target
            .import_team_template(
                import_params(&target_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect("apply");
        assert!(applied.applied);
        assert_eq!(applied.pending_hire_request_ids.len(), 1);
        let session = target
            .create_session(target_id.clone(), "bundle template".to_string(), None)
            .await
            .expect("session");
        let bundle = target.export_session(&session.id).await.expect("bundle");
        assert!(!bundle.team_templates.is_empty());
        assert!(!bundle.project_team_templates.is_empty());
        assert!(!bundle.team_template_events.is_empty());
        assert_eq!(bundle.agent_hire_requests.len(), 1);

        let import_dir = TempDir::new().expect("import dir");
        let import_store = Store::open(import_dir.path()).expect("import store");
        let import_engine = Engine::new(import_store.clone());
        import_engine
            .import_session(bundle)
            .await
            .expect("import bundle");
        let record = vp_store::store::team_template::get_team_template_record(
            &import_store,
            &export.template.template_id,
        )
        .expect("template")
        .expect("template row");
        assert_eq!(record.content_hash, export.template.content_hash);
        let links =
            vp_store::store::team_template::list_project_team_templates(&import_store, &target_id)
                .expect("links");
        assert_eq!(links.len(), 1);
        let identities = import_store
            .list_agent_identities(&target_id, None, None)
            .expect("identities");
        assert_eq!(identities.len(), 1);
        let requests = vp_store::store::employment::list_agent_hire_requests(
            &import_store,
            &vp_protocol::types::AgentHireListParams {
                identity_id: None,
                project_id: target_id.clone(),
                status: None,
                limit: Some(50),
            },
        )
        .expect("hire requests");
        assert_eq!(requests.requests.len(), 1);
        assert_eq!(requests.requests[0].origin, "imported");
    }

    #[tokio::test]
    async fn db_rebuild_restores_templates_apply_links_and_events() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir, "rebuild");
        enable_identity_policy(&engine, &project_id).await;
        create_identity(&engine, &project_id, "leader", "Leader", None, None, None).await;
        let export = engine
            .export_team_template(export_params(&project_id, "Rebuild Team"))
            .await
            .expect("export");
        let applied = engine
            .import_team_template(
                import_params(&project_id, &export.json, true, true),
                vec!["test-model".to_string()],
            )
            .await
            .expect("apply");
        assert!(applied.applied);
        engine.store().record_project_manifest().expect("manifest");
        drop(engine);
        let report = crate::rebuild::rebuild_database(dir.path(), false)
            .await
            .expect("rebuild");
        assert_eq!(report.projects, 1);
        let store = Store::open(dir.path()).expect("reopened store");
        let record = vp_store::store::team_template::get_team_template_record(
            &store,
            &export.template.template_id,
        )
        .expect("template")
        .expect("template row");
        assert_eq!(record.content_hash, export.template.content_hash);
        let links = vp_store::store::team_template::list_project_team_templates(
            &store,
            &record
                .source_project_id
                .clone()
                .unwrap_or_else(|| project_id.clone()),
        )
        .expect("links");
        assert_eq!(links.len(), 1);
        let events = vp_store::store::team_template::list_team_template_events_for_project(
            &store,
            &project_id,
            None,
        )
        .expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.kind == vp_protocol::types::TEAM_TEMPLATE_APPLIED_EVENT)
        );
        let replay =
            vp_store::store::team_template::replay_team_templates(&store, &project_id, false)
                .expect("replay");
        assert!(replay.mismatches.is_empty());
    }

    #[tokio::test]
    async fn export_bundle_carries_templates_but_not_identity_context() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");
        let session = engine
            .create_session(source_id.clone(), "bundle template".to_string(), None)
            .await
            .expect("session");
        let bundle = engine.export_session(&session.id).await.expect("bundle");
        assert!(
            bundle
                .team_templates
                .iter()
                .any(|record| record.content_hash == export.template.content_hash)
        );
        let encoded = serde_json::to_string(&bundle).expect("encode");
        assert!(!encoded.contains("\"worktree_path\""));
        let decoded: vp_protocol::types::SessionBundle =
            serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded.team_templates.len(), bundle.team_templates.len());
    }

    #[tokio::test]
    async fn apply_with_dry_run_still_requires_approval() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");
        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;
        let error = engine
            .import_team_template(
                TeamTemplateImportParams {
                    project_id: target_id.clone(),
                    json: export.json.clone(),
                    dry_run: true,
                    apply: true,
                    approved: false,
                    operator: Some("test".to_string()),
                    resolutions: Vec::new(),
                },
                vec!["test-model".to_string()],
            )
            .await
            .unwrap_err();
        let typed = error.downcast_ref::<AgentIdentityError>().expect("typed");
        assert_eq!(
            typed.code,
            vp_protocol::types::team_template_codes::TEMPLATE_APPROVAL_REQUIRED
        );
        // A dry run returns a preview even when approval is supplied.
        let preview = engine
            .import_team_template(
                TeamTemplateImportParams {
                    project_id: target_id.clone(),
                    json: export.json.clone(),
                    dry_run: true,
                    apply: true,
                    approved: true,
                    operator: Some("test".to_string()),
                    resolutions: Vec::new(),
                },
                vec!["test-model".to_string()],
            )
            .await
            .expect("preview");
        assert!(!preview.applied);
        assert!(preview.entry.is_none());
        assert!(
            vp_store::store::employment::list_agent_hire_requests(
                engine.store(),
                &vp_protocol::types::AgentHireListParams {
                    project_id: target_id.clone(),
                    ..vp_protocol::types::AgentHireListParams::default()
                },
            )
            .expect("requests")
            .requests
            .is_empty()
        );
    }

    #[tokio::test]
    async fn template_reads_are_project_scoped() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, source_id) = test_engine(&dir, "source");
        source_project(&engine, &source_id).await;
        let export = engine
            .export_team_template(export_params(&source_id, "Platform Team"))
            .await
            .expect("export");
        let (engine, target_id) = test_engine(&dir, "target");
        enable_identity_policy(&engine, &target_id).await;
        create_identity(&engine, &target_id, "leader", "Leader", None, None, None).await;
        let error = engine
            .get_team_template(&export.template.template_id, Some(&target_id))
            .await
            .expect_err("a project that never imported the template must not read it");
        assert_eq!(
            error
                .downcast_ref::<AgentIdentityError>()
                .map(|typed| typed.code),
            Some(vp_protocol::types::team_template_codes::TEMPLATE_NOT_FOUND)
        );
        assert!(
            engine
                .list_team_templates(Some(target_id.clone()), Some(10))
                .await
                .expect("list")
                .templates
                .is_empty()
        );
    }
}
