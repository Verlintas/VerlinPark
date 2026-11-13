//! C7 bounded dynamic fan-out.
//!
//! A fan-out is an explicit, bounded plan revision. It replaces one pending
//! template task with at most 20 child tasks and one shared join task. Child
//! scopes are normalized and proven pairwise disjoint before the revision is
//! written; existing dependents of the template are retargeted to the join.
//!
//! This module never dispatches a scheduler tick and never rewrites a plan
//! automatically. The only entry point is an approved `plan.fanout` call (tool
//! or RPC), and the resulting plan still passes the normal contract,
//! approval, sandbox, Verifier, capacity, and merge gates.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Result;
use vp_protocol::types::{PlanFanoutItem, PlanFanoutParams, PlanFanoutResult, PlanTask, TeamPlan};

use crate::agent::{normalize_scope, path_in_scope, scopes_overlap, validate_team_plan};
use crate::orchestration::{diff_plan, ensure_revision_safe, plan_revision};

pub const MAX_FANOUT_CHILDREN: usize = 20;
pub const MAX_FANOUT_KEY_CHARS: usize = 64;
pub const MAX_FANOUT_ITEM_BYTES: usize = 128 * 1024;
pub const MAX_FANOUT_REASON_CHARS: usize = 1_000;
pub const MAX_FANOUT_AUTHOR_CHARS: usize = 80;

pub mod codes {
    pub const FANOUT_APPROVAL_REQUIRED: &str = "FANOUT_APPROVAL_REQUIRED";
    pub const FANOUT_NO_PLAN: &str = "FANOUT_NO_PLAN";
    pub const FANOUT_NOT_FOUND: &str = "FANOUT_NOT_FOUND";
    pub const FANOUT_INVALID: &str = "FANOUT_INVALID";
    pub const FANOUT_CAP: &str = "FANOUT_CAP";
    pub const FANOUT_CONFLICT: &str = "FANOUT_CONFLICT";
    pub const FANOUT_SCOPE_OVERLAP: &str = "FANOUT_SCOPE_OVERLAP";
    pub const FANOUT_RECURSIVE: &str = "FANOUT_RECURSIVE";
    pub const FANOUT_PROTECTED: &str = "FANOUT_PROTECTED";
    pub const FANOUT_ARTIFACT_INVALID: &str = "FANOUT_ARTIFACT_INVALID";
}

/// Typed fan-out failure. The daemon maps this code to an RPC error and the
/// tool dispatcher maps it to a stable tool error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanoutError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for FanoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FanoutError {}

pub fn fanout_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(FanoutError {
        code,
        message: message.into(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct FanoutExpansion {
    pub plan: TeamPlan,
    pub parent_task_id: String,
    pub join_task_id: String,
    pub children: Vec<PlanTask>,
    pub deduped_keys: Vec<String>,
    pub idempotent: bool,
}

fn protected_status(status: &str) -> bool {
    matches!(
        status,
        "claimed"
            | "running"
            | "queued"
            | "paused"
            | "pausing"
            | "cancelling"
            | "waiting_input"
            | "completed"
            | "skipped"
    )
}

fn active_status(status: &str) -> bool {
    matches!(
        status,
        "claimed" | "running" | "queued" | "paused" | "pausing" | "cancelling" | "waiting_input"
    )
}

fn normalize_key(raw: &str) -> Result<String> {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty()
        || key.chars().count() > MAX_FANOUT_KEY_CHARS
        || matches!(key.as_str(), "." | "..")
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            format!(
                "fan-out key must be 1-{MAX_FANOUT_KEY_CHARS} ASCII letters/digits/'-'/'_'/'.', got '{raw}'"
            ),
        ));
    }
    Ok(key)
}

fn short_hash(value: &str) -> String {
    crate::sha256_hex(value.as_bytes())[..8].to_string()
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn child_task_id(template_id: &str, key: &str) -> String {
    let hash = short_hash(&format!("fanout\u{0}{template_id}\u{0}{key}"));
    format!(
        "{}--{}-{hash}",
        truncate_chars(template_id, 12),
        truncate_chars(key, 8)
    )
}

fn default_join_task_id(template_id: &str) -> String {
    let hash = short_hash(&format!("fanout-join\u{0}{template_id}"));
    format!("{}--join-{hash}", truncate_chars(template_id, 12))
}

fn validate_task_id(value: &str, label: &str) -> Result<()> {
    crate::worktree::validate_task_id(value)
        .map_err(|error| fanout_error(codes::FANOUT_INVALID, format!("{label}: {error}")))
}

fn normalize_acceptance(values: &[String], label: &str) -> Result<Vec<String>> {
    let values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if values.len() > 32 {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            format!("{label} has more than 32 acceptance items"),
        ));
    }
    for value in &values {
        if value.chars().count() > 400 {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                format!("{label} acceptance item is longer than 400 chars"),
            ));
        }
    }
    Ok(values)
}

fn normalized_scopes(item: &PlanFanoutItem, template: &PlanTask) -> Result<Vec<String>> {
    if item.write_scope.is_empty() {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            format!(
                "fan-out item '{}' needs a non-empty, disjoint write_scope",
                item.key
            ),
        ));
    }
    if item.write_scope.len() > 32 {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            format!("fan-out item '{}' has more than 32 write scopes", item.key),
        ));
    }
    let mut scopes = item
        .write_scope
        .iter()
        .map(|scope| {
            let normalized = normalize_scope(scope);
            if normalized.is_empty()
                || normalized.chars().count() > 200
                || normalized.contains("..")
                || normalized.starts_with('/')
                || normalized.starts_with('~')
            {
                return Err(fanout_error(
                    codes::FANOUT_INVALID,
                    format!(
                        "fan-out item '{}' has invalid write scope '{scope}'",
                        item.key
                    ),
                ));
            }
            Ok(normalized)
        })
        .collect::<Result<Vec<_>>>()?;
    scopes.sort();
    scopes.dedup();
    if !template.write_scope.is_empty() {
        for scope in &scopes {
            if !path_in_scope(scope, &template.write_scope) {
                return Err(fanout_error(
                    codes::FANOUT_INVALID,
                    format!(
                        "fan-out item '{}' scope '{scope}' is outside template scope {}",
                        item.key,
                        template.write_scope.join(", ")
                    ),
                ));
            }
        }
    }
    Ok(scopes)
}

fn normalized_items(items: &[PlanFanoutItem]) -> Result<(Vec<PlanFanoutItem>, Vec<String>)> {
    if items.is_empty() {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            "plan.fanout needs at least one item",
        ));
    }
    if items.len() > MAX_FANOUT_CHILDREN {
        return Err(fanout_error(
            codes::FANOUT_CAP,
            format!("plan.fanout accepts at most {MAX_FANOUT_CHILDREN} items"),
        ));
    }
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut deduped = BTreeSet::new();
    let mut normalized = Vec::with_capacity(items.len());
    for item in items {
        let key = normalize_key(&item.key)?;
        let mut item = item.clone();
        item.key = key.clone();
        let signature = serde_json::to_string(&item)?;
        if let Some(existing) = seen.get(&key) {
            if existing == &signature {
                deduped.insert(key);
                continue;
            }
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("duplicate fan-out key '{key}' has conflicting item fields"),
            ));
        }
        seen.insert(key, signature);
        normalized.push(item);
    }
    normalized.sort_by(|left, right| left.key.cmp(&right.key));
    Ok((normalized, deduped.into_iter().collect()))
}

fn task_from_item(template: &PlanTask, item: &PlanFanoutItem, id: String) -> Result<PlanTask> {
    let title = match item
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        Some(title) if title.chars().count() > 200 => {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                format!("fan-out item '{}' title is longer than 200 chars", item.key),
            ));
        }
        Some(title) => title.to_string(),
        None => truncate_chars(&format!("{} · {}", template.title.trim(), item.key), 200),
    };
    let kind = item
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .unwrap_or(&template.kind)
        .to_string();
    if kind.chars().count() > 80 {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            format!("fan-out item '{}' kind is longer than 80 chars", item.key),
        ));
    }
    let profile = item.profile.clone().or_else(|| template.profile.clone());
    let priority = item.priority.unwrap_or(template.priority);
    if !(-100..=100).contains(&priority) {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            format!("fan-out item '{}' priority must be -100..100", item.key),
        ));
    }
    let write_scope = normalized_scopes(item, template)?;
    let mut acceptance = normalize_acceptance(&item.acceptance, "fan-out item")?;
    if acceptance.is_empty() {
        acceptance = template.acceptance.clone();
    }
    if acceptance.is_empty() {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            format!("fan-out item '{}' needs acceptance criteria", item.key),
        ));
    }
    Ok(PlanTask {
        id,
        department: template.department.clone(),
        title,
        kind,
        priority,
        profile,
        deps: template.deps.clone(),
        write_scope,
        acceptance,
        token_budget: item.token_budget.or(template.token_budget),
        cost_budget_usd: item.cost_budget_usd.or(template.cost_budget_usd),
        parent_task_id: Some(template.id.clone()),
        fanout_key: Some(item.key.clone()),
        fanout_join_for: None,
    })
}

fn join_task(
    template: &PlanTask,
    join_task_id: String,
    join_title: Option<&str>,
    join_acceptance: &[String],
    children: &[PlanTask],
) -> Result<PlanTask> {
    let title = match join_title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) if title.chars().count() > 200 => {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                "join task title is longer than 200 chars",
            ));
        }
        Some(title) => title.to_string(),
        None => truncate_chars(&format!("Join fan-out {}", template.title.trim()), 200),
    };
    let mut acceptance = normalize_acceptance(join_acceptance, "join task")?;
    if acceptance.is_empty() {
        acceptance =
            vec!["All fan-out children completed and their results were reconciled.".to_string()];
    }
    Ok(PlanTask {
        id: join_task_id,
        department: template.department.clone(),
        title,
        kind: "join".to_string(),
        priority: template.priority,
        profile: template.profile.clone(),
        deps: children.iter().map(|child| child.id.clone()).collect(),
        write_scope: Vec::new(),
        acceptance,
        token_budget: template.token_budget,
        cost_budget_usd: template.cost_budget_usd,
        parent_task_id: None,
        fanout_key: None,
        fanout_join_for: Some(template.id.clone()),
    })
}

fn idempotent_expansion(
    plan: &TeamPlan,
    template_task_id: &str,
    items: &[PlanFanoutItem],
    join_task_id: Option<&str>,
    join_title: Option<&str>,
    join_acceptance: &[String],
    existing_children: &[PlanTask],
) -> Result<FanoutExpansion> {
    if plan.tasks.iter().any(|task| task.id == template_task_id) {
        return Err(fanout_error(
            codes::FANOUT_CONFLICT,
            "plan still contains the template alongside fan-out children",
        ));
    }
    if existing_children
        .iter()
        .any(|child| child.fanout_key.is_none())
    {
        return Err(fanout_error(
            codes::FANOUT_CONFLICT,
            "existing fan-out child is missing its fanout_key",
        ));
    }
    let (normalized, deduped_keys) = normalized_items(items)?;
    let mut existing_by_key = existing_children
        .iter()
        .filter_map(|child| child.fanout_key.clone().map(|key| (key, child)))
        .collect::<BTreeMap<_, _>>();
    let requested_keys = normalized
        .iter()
        .map(|item| item.key.clone())
        .collect::<BTreeSet<_>>();
    let existing_keys = existing_children
        .iter()
        .filter_map(|child| child.fanout_key.clone())
        .collect::<BTreeSet<_>>();
    if requested_keys != existing_keys {
        return Err(fanout_error(
            codes::FANOUT_CONFLICT,
            "template is already expanded with a different key set",
        ));
    }
    let join = plan
        .tasks
        .iter()
        .find(|task| task.fanout_join_for.as_deref() == Some(template_task_id))
        .ok_or_else(|| {
            fanout_error(
                codes::FANOUT_CONFLICT,
                "fan-out children exist without their shared join task",
            )
        })?;
    if let Some(requested_join) = join_task_id
        && requested_join != join.id
    {
        return Err(fanout_error(
            codes::FANOUT_CONFLICT,
            format!(
                "fan-out already uses join task '{}', not '{}'",
                join.id, requested_join
            ),
        ));
    }
    if let Some(title) = join_title.map(str::trim).filter(|title| !title.is_empty())
        && title != join.title
    {
        return Err(fanout_error(
            codes::FANOUT_CONFLICT,
            "fan-out join title differs from the existing expansion",
        ));
    }
    let requested_acceptance = normalize_acceptance(join_acceptance, "join task")?;
    if !requested_acceptance.is_empty() && requested_acceptance != join.acceptance {
        return Err(fanout_error(
            codes::FANOUT_CONFLICT,
            "fan-out join acceptance differs from the existing expansion",
        ));
    }
    for item in &normalized {
        let child = existing_by_key.remove(&item.key).ok_or_else(|| {
            fanout_error(
                codes::FANOUT_CONFLICT,
                format!(
                    "fan-out key '{}' is missing from the existing expansion",
                    item.key
                ),
            )
        })?;
        if let Some(title) = item
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            && title != child.title
        {
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("fan-out key '{}' title differs", item.key),
            ));
        }
        if let Some(kind) = item
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            && kind != child.kind
        {
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("fan-out key '{}' kind differs", item.key),
            ));
        }
        if let Some(profile) = item.profile.as_deref()
            && child.profile.as_deref() != Some(profile)
        {
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("fan-out key '{}' profile differs", item.key),
            ));
        }
        if let Some(priority) = item.priority
            && priority != child.priority
        {
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("fan-out key '{}' priority differs", item.key),
            ));
        }
        if !item.write_scope.is_empty() {
            let mut scopes = item
                .write_scope
                .iter()
                .map(|scope| normalize_scope(scope))
                .collect::<Vec<_>>();
            scopes.sort();
            scopes.dedup();
            if scopes != child.write_scope {
                return Err(fanout_error(
                    codes::FANOUT_CONFLICT,
                    format!("fan-out key '{}' write scope differs", item.key),
                ));
            }
        }
        if !item.acceptance.is_empty() {
            let acceptance = normalize_acceptance(&item.acceptance, "fan-out item")?;
            if acceptance != child.acceptance {
                return Err(fanout_error(
                    codes::FANOUT_CONFLICT,
                    format!("fan-out key '{}' acceptance differs", item.key),
                ));
            }
        }
        if item.token_budget.is_some() && item.token_budget != child.token_budget {
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("fan-out key '{}' token budget differs", item.key),
            ));
        }
        if item.cost_budget_usd.is_some() && item.cost_budget_usd != child.cost_budget_usd {
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("fan-out key '{}' cost budget differs", item.key),
            ));
        }
    }
    Ok(FanoutExpansion {
        plan: plan.clone(),
        parent_task_id: template_task_id.to_string(),
        join_task_id: join.id.clone(),
        children: existing_children.to_vec(),
        deduped_keys,
        idempotent: true,
    })
}

/// Pure expansion. It does not read or write the store, call a provider, or
/// dispatch work. The engine validates the resulting plan again before the
/// revision transaction.
pub fn expand(
    plan: &TeamPlan,
    statuses: &HashMap<String, String>,
    template_task_id: &str,
    items: &[PlanFanoutItem],
    join_task_id: Option<&str>,
    join_title: Option<&str>,
    join_acceptance: &[String],
) -> Result<FanoutExpansion> {
    let template_id = template_task_id.trim();
    if template_id.is_empty() {
        return Err(fanout_error(
            codes::FANOUT_INVALID,
            "template_task_id is required",
        ));
    }
    let existing_children = plan
        .tasks
        .iter()
        .filter(|task| task.parent_task_id.as_deref() == Some(template_id))
        .cloned()
        .collect::<Vec<_>>();
    if !existing_children.is_empty() {
        return idempotent_expansion(
            plan,
            template_id,
            items,
            join_task_id,
            join_title,
            join_acceptance,
            &existing_children,
        );
    }
    let template = plan
        .tasks
        .iter()
        .find(|task| task.id == template_id)
        .ok_or_else(|| {
            fanout_error(
                codes::FANOUT_NOT_FOUND,
                format!("template task not found: {template_id}"),
            )
        })?;
    if template.parent_task_id.is_some() || template.fanout_join_for.is_some() {
        return Err(fanout_error(
            codes::FANOUT_RECURSIVE,
            "a fan-out child or join task cannot be used as another template",
        ));
    }
    let template_status = statuses
        .get(template_id)
        .map(String::as_str)
        .unwrap_or("pending");
    if protected_status(template_status) {
        return Err(fanout_error(
            codes::FANOUT_PROTECTED,
            format!(
                "template task '{template_id}' is {template_status}; only pending/failed/blocked/cancelled templates may be expanded"
            ),
        ));
    }
    let (normalized, deduped_keys) = normalized_items(items)?;

    let mut occupied = plan
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let join_id = match join_task_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            validate_task_id(id, "join_task_id")?;
            if id == template_id {
                return Err(fanout_error(
                    codes::FANOUT_INVALID,
                    "join_task_id must differ from template_task_id",
                ));
            }
            if !occupied.insert(id.to_string()) {
                return Err(fanout_error(
                    codes::FANOUT_CONFLICT,
                    format!("join task id already exists: {id}"),
                ));
            }
            id.to_string()
        }
        None => {
            let id = default_join_task_id(template_id);
            if !occupied.insert(id.clone()) {
                return Err(fanout_error(
                    codes::FANOUT_CONFLICT,
                    format!("generated join task id already exists: {id}"),
                ));
            }
            id
        }
    };

    let mut children = Vec::with_capacity(normalized.len());
    for item in &normalized {
        let id = child_task_id(template_id, &item.key);
        validate_task_id(&id, "generated child task id")?;
        if !occupied.insert(id.clone()) {
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("generated child task id already exists: {id}"),
            ));
        }
        children.push(task_from_item(template, item, id)?);
    }
    for (index, left) in children.iter().enumerate() {
        for right in children.iter().skip(index + 1) {
            if scopes_overlap(&left.write_scope, &right.write_scope) {
                return Err(fanout_error(
                    codes::FANOUT_SCOPE_OVERLAP,
                    format!(
                        "fan-out children '{}' and '{}' have overlapping write scopes",
                        left.fanout_key.as_deref().unwrap_or(&left.id),
                        right.fanout_key.as_deref().unwrap_or(&right.id)
                    ),
                ));
            }
        }
    }
    let join = join_task(
        template,
        join_id.clone(),
        join_title,
        join_acceptance,
        &children,
    )?;

    // Existing dependents must wait for the join, not the removed template.
    // A protected dependent is a hard conflict: rewriting an active or
    // completed task's dependencies would change already-scheduled semantics.
    let mut retargeted = HashMap::new();
    for task in &plan.tasks {
        if task.id == template_id || !task.deps.iter().any(|dep| dep == template_id) {
            continue;
        }
        let status = statuses
            .get(&task.id)
            .map(String::as_str)
            .unwrap_or("pending");
        if protected_status(status) {
            return Err(fanout_error(
                codes::FANOUT_PROTECTED,
                format!(
                    "dependent task '{}' is {status}; cannot retarget it to the fan-out join",
                    task.id
                ),
            ));
        }
        let mut next = task.clone();
        let mut deps = Vec::with_capacity(task.deps.len());
        for dep in &task.deps {
            let value = if dep == template_id {
                join_id.clone()
            } else {
                dep.clone()
            };
            if !deps.contains(&value) {
                deps.push(value);
            }
        }
        next.deps = deps;
        retargeted.insert(task.id.clone(), next);
    }

    let mut tasks = Vec::with_capacity(plan.tasks.len() + children.len());
    for task in &plan.tasks {
        if task.id == template_id {
            tasks.extend(children.iter().cloned());
            tasks.push(join.clone());
        } else if let Some(next) = retargeted.remove(&task.id) {
            tasks.push(next);
        } else {
            tasks.push(task.clone());
        }
    }
    let expanded = TeamPlan {
        goal: plan.goal.clone(),
        departments: plan.departments.clone(),
        tasks,
    };
    Ok(FanoutExpansion {
        plan: expanded,
        parent_task_id: template_id.to_string(),
        join_task_id: join_id,
        children,
        deduped_keys,
        idempotent: false,
    })
}

impl crate::Engine {
    /// Explicitly expand one template task. This is the only fan-out entry
    /// point; there is no automatic scheduler expansion.
    pub async fn fanout_plan(&self, params: PlanFanoutParams) -> Result<PlanFanoutResult> {
        if !params.approved {
            return Err(fanout_error(
                codes::FANOUT_APPROVAL_REQUIRED,
                "plan.fanout requires explicit approved=true approval",
            ));
        }
        let session_id = params.session_id.trim().to_string();
        if session_id.is_empty() {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                "session_id is required",
            ));
        }
        let session = {
            let store = self.store.clone();
            let session_id = session_id.clone();
            tokio::task::spawn_blocking(move || store.get_session(&session_id)).await??
        }
        .ok_or_else(|| {
            fanout_error(
                codes::FANOUT_NOT_FOUND,
                format!("session not found: {session_id}"),
            )
        })?;
        if session.status == "imported" {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                "imported sessions are read-only archives",
            ));
        }
        let current = self.get_team_plan(&session_id).await?.ok_or_else(|| {
            fanout_error(codes::FANOUT_NO_PLAN, "session has no TeamPlan to expand")
        })?;
        let scheduled = {
            let store = self.store.clone();
            let session_id = session_id.clone();
            tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&session_id)))
                .await??
        };
        let statuses = scheduled
            .into_iter()
            .map(|task| (task.task_id, task.status))
            .collect::<HashMap<_, _>>();
        if let Some((task_id, status)) = statuses.iter().find(|(_, status)| active_status(status)) {
            return Err(fanout_error(
                codes::FANOUT_PROTECTED,
                format!(
                    "plan task '{task_id}' is {status}; expand fan-out between batches, not during active work"
                ),
            ));
        }
        let events = self.recent_events(&session_id, 5_000).await?;
        let current_revision = plan_revision(&events);
        if let Some(expected) = params.expected_revision
            && expected != current_revision
        {
            return Err(fanout_error(
                codes::FANOUT_CONFLICT,
                format!("plan revision conflict: expected {expected}, current {current_revision}"),
            ));
        }
        let items = self.fanout_items(&session, &params).await?;
        let expansion = expand(
            &current,
            &statuses,
            &params.template_task_id,
            &items,
            params.join_task_id.as_deref(),
            params.join_title.as_deref(),
            &params.join_acceptance,
        )?;
        if expansion.idempotent {
            return Ok(fanout_result(&expansion, current_revision));
        }
        validate_team_plan(&expansion.plan, current.departments.len().max(1)).map_err(|error| {
            fanout_error(
                codes::FANOUT_INVALID,
                format!("expanded plan is invalid: {error}"),
            )
        })?;
        ensure_revision_safe(&current, &expansion.plan, &statuses)
            .map_err(|error| fanout_error(codes::FANOUT_CONFLICT, error.to_string()))?;

        let reason = params
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("explicit bounded `plan.fanout` expansion")
            .to_string();
        if reason.chars().count() > MAX_FANOUT_REASON_CHARS {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                format!("fan-out reason must be at most {MAX_FANOUT_REASON_CHARS} chars"),
            ));
        }
        let author = params
            .author
            .as_deref()
            .map(str::trim)
            .filter(|author| !author.is_empty())
            .unwrap_or("operator")
            .to_string();
        if author.chars().count() > MAX_FANOUT_AUTHOR_CHARS {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                format!("fan-out author must be at most {MAX_FANOUT_AUTHOR_CHARS} chars"),
            ));
        }
        let revision = current_revision.saturating_add(1);
        let snapshot = self
            .pin_plan_snapshot(&session, &expansion.plan, revision)
            .await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let diff = diff_plan(&current, &expansion.plan);
        let child_keys = expansion
            .children
            .iter()
            .filter_map(|child| child.fanout_key.clone())
            .collect::<Vec<_>>();
        let child_task_ids = expansion
            .children
            .iter()
            .map(|child| child.id.clone())
            .collect::<Vec<_>>();
        let payload = serde_json::json!({
            "plan": expansion.plan,
            "revision": revision,
            "reason": reason,
            "author": author,
            "task_count": expansion.plan.tasks.len(),
            "added": diff.added,
            "removed": diff.removed,
            "changed": diff.changed,
            "fanout": {
                "parent_task_id": expansion.parent_task_id,
                "join_task_id": expansion.join_task_id,
                "child_task_ids": child_task_ids,
                "child_keys": child_keys,
                "deduped_keys": expansion.deduped_keys,
                "item_source": if params.artifact_id.is_some() { "artifact" } else { "inline" },
                "artifact_id": params.artifact_id,
            },
            "runtime_snapshot": snapshot,
        });
        let plan_json = serde_json::to_string(&expansion.plan)?;
        let session_for_store = session_id.clone();
        let event = {
            let store = self.store.clone();
            tokio::task::spawn_blocking(move || {
                store.set_team_plan_revision_with_runtime_snapshot_and_event(
                    &session_for_store,
                    &plan_json,
                    "team_plan.revised",
                    payload,
                    Some(&snapshot_json),
                )
            })
            .await??
        };
        let _ = self.events.send(event);
        self.scheduler.sync_plan(&session, &expansion.plan).await?;
        Ok(fanout_result(&expansion, revision))
    }

    async fn fanout_items(
        &self,
        session: &vp_protocol::types::Session,
        params: &PlanFanoutParams,
    ) -> Result<Vec<PlanFanoutItem>> {
        if !params.items.is_empty() && params.artifact_id.is_some() {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                "pass either inline items or artifact_id, not both",
            ));
        }
        let bytes = if let Some(artifact_id) = params.artifact_id.as_deref() {
            let artifact = self.get_artifact(artifact_id).await?.ok_or_else(|| {
                fanout_error(
                    codes::FANOUT_ARTIFACT_INVALID,
                    format!("artifact not found: {artifact_id}"),
                )
            })?;
            if artifact.session_id != session.id {
                return Err(fanout_error(
                    codes::FANOUT_ARTIFACT_INVALID,
                    "artifact belongs to another session",
                ));
            }
            if !artifact.content.is_empty() {
                artifact.content.into_bytes()
            } else {
                let materialized =
                    self.materialize_artifact(artifact_id)
                        .await?
                        .ok_or_else(|| {
                            fanout_error(
                                codes::FANOUT_ARTIFACT_INVALID,
                                format!("artifact has no readable content: {artifact_id}"),
                            )
                        })?;
                let metadata = tokio::fs::metadata(&materialized.path)
                    .await
                    .map_err(|error| {
                        fanout_error(
                            codes::FANOUT_ARTIFACT_INVALID,
                            format!("artifact cannot be read: {error}"),
                        )
                    })?;
                if metadata.len() > MAX_FANOUT_ITEM_BYTES as u64 {
                    return Err(fanout_error(
                        codes::FANOUT_CAP,
                        format!("fan-out artifact exceeds {MAX_FANOUT_ITEM_BYTES} bytes"),
                    ));
                }
                tokio::fs::read(&materialized.path).await.map_err(|error| {
                    fanout_error(
                        codes::FANOUT_ARTIFACT_INVALID,
                        format!("artifact cannot be read: {error}"),
                    )
                })?
            }
        } else {
            serde_json::to_vec(&params.items)?
        };
        if bytes.len() > MAX_FANOUT_ITEM_BYTES {
            return Err(fanout_error(
                codes::FANOUT_CAP,
                format!("fan-out item list exceeds {MAX_FANOUT_ITEM_BYTES} bytes"),
            ));
        }
        let items = serde_json::from_slice::<Vec<PlanFanoutItem>>(&bytes).map_err(|error| {
            fanout_error(
                codes::FANOUT_INVALID,
                format!("fan-out items must be a JSON array: {error}"),
            )
        })?;
        if items.is_empty() {
            return Err(fanout_error(
                codes::FANOUT_INVALID,
                "fan-out items must not be empty",
            ));
        }
        Ok(items)
    }
}

fn fanout_result(expansion: &FanoutExpansion, revision: u32) -> PlanFanoutResult {
    PlanFanoutResult {
        revision,
        plan: expansion.plan.clone(),
        parent_task_id: expansion.parent_task_id.clone(),
        join_task_id: expansion.join_task_id.clone(),
        child_task_ids: expansion
            .children
            .iter()
            .map(|child| child.id.clone())
            .collect(),
        child_keys: expansion
            .children
            .iter()
            .filter_map(|child| child.fanout_key.clone())
            .collect(),
        deduped_keys: expansion.deduped_keys.clone(),
        idempotent: expansion.idempotent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::PlanDepartment;

    fn plan(tasks: Vec<PlanTask>) -> TeamPlan {
        TeamPlan {
            goal: "fan out".to_string(),
            departments: vec![PlanDepartment {
                name: "impl".to_string(),
                manager_title: "Implementation".to_string(),
                profile: "coding".to_string(),
                objective: "implement".to_string(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks,
        }
    }

    fn template() -> PlanTask {
        PlanTask {
            id: "T1".to_string(),
            department: "impl".to_string(),
            title: "Build components".to_string(),
            kind: "coding".to_string(),
            priority: 0,
            profile: Some("coding".to_string()),
            deps: Vec::new(),
            write_scope: vec!["src".to_string()],
            acceptance: vec!["done".to_string()],
            token_budget: Some(1_000),
            cost_budget_usd: Some(1.0),
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        }
    }

    fn item(key: &str, scope: &str) -> PlanFanoutItem {
        PlanFanoutItem {
            key: key.to_string(),
            write_scope: vec![scope.to_string()],
            ..PlanFanoutItem::default()
        }
    }

    fn statuses(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, status)| ((*id).to_string(), (*status).to_string()))
            .collect()
    }

    #[test]
    fn expands_children_and_a_shared_join_with_disjoint_scopes() {
        let mut task = template();
        task.deps = vec!["T0".to_string()];
        let expansion = expand(
            &plan(vec![task]),
            &statuses(&[("T1", "pending")]),
            "T1",
            &[item("auth", "src/auth"), item("api", "src/api")],
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(!expansion.idempotent);
        assert_eq!(expansion.children.len(), 2);
        assert!(expansion.children.iter().all(|child| {
            child.parent_task_id.as_deref() == Some("T1")
                && child.deps == vec!["T0".to_string()]
                && child.token_budget == Some(1_000)
                && child.cost_budget_usd == Some(1.0)
                && child.write_scope.len() == 1
        }));
        let join = expansion
            .plan
            .tasks
            .iter()
            .find(|task| task.id == expansion.join_task_id)
            .unwrap();
        assert_eq!(join.fanout_join_for.as_deref(), Some("T1"));
        assert_eq!(join.deps.len(), 2);
        assert!(join.write_scope.is_empty());
        assert!(!expansion.plan.tasks.iter().any(|task| task.id == "T1"));
    }

    #[test]
    fn retargets_dependents_to_the_join() {
        let mut dependent = PlanTask {
            id: "T2".to_string(),
            department: "impl".to_string(),
            title: "Verify".to_string(),
            kind: "coding".to_string(),
            priority: 0,
            profile: Some("coding".to_string()),
            deps: vec!["T1".to_string()],
            write_scope: vec!["docs".to_string()],
            acceptance: vec!["done".to_string()],
            token_budget: None,
            cost_budget_usd: None,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        };
        dependent.deps = vec!["T1".to_string()];
        let expansion = expand(
            &plan(vec![template(), dependent]),
            &statuses(&[("T1", "pending"), ("T2", "pending")]),
            "T1",
            &[item("one", "src/one")],
            None,
            None,
            &[],
        )
        .unwrap();
        let retargeted = expansion
            .plan
            .tasks
            .iter()
            .find(|task| task.id == "T2")
            .unwrap();
        assert_eq!(retargeted.deps, vec![expansion.join_task_id.clone()]);
    }

    #[test]
    fn caps_dedupes_and_rejects_conflicting_duplicate_keys() {
        let items = (0..=MAX_FANOUT_CHILDREN)
            .map(|index| item(&format!("k{index}"), &format!("src/k{index}")))
            .collect::<Vec<_>>();
        let error = expand(
            &plan(vec![template()]),
            &HashMap::new(),
            "T1",
            &items,
            None,
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<FanoutError>().unwrap().code,
            codes::FANOUT_CAP
        );

        let duplicate = vec![item("same", "src/a"), item("same", "src/a")];
        let expansion = expand(
            &plan(vec![template()]),
            &HashMap::new(),
            "T1",
            &duplicate,
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(expansion.children.len(), 1);
        assert_eq!(expansion.deduped_keys, vec!["same".to_string()]);

        let conflicting = vec![item("same", "src/a"), item("same", "src/b")];
        let error = expand(
            &plan(vec![template()]),
            &HashMap::new(),
            "T1",
            &conflicting,
            None,
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<FanoutError>().unwrap().code,
            codes::FANOUT_CONFLICT
        );
    }

    #[test]
    fn rejects_overlapping_scopes_and_scope_escape() {
        let error = expand(
            &plan(vec![template()]),
            &HashMap::new(),
            "T1",
            &[item("a", "src/auth"), item("b", "src/auth/api")],
            None,
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<FanoutError>().unwrap().code,
            codes::FANOUT_SCOPE_OVERLAP
        );

        let error = expand(
            &plan(vec![template()]),
            &HashMap::new(),
            "T1",
            &[item("a", "docs")],
            None,
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<FanoutError>().unwrap().code,
            codes::FANOUT_INVALID
        );
    }

    #[test]
    fn replay_of_the_same_key_set_is_idempotent_but_conflicts_are_rejected() {
        let template = template();
        let first = expand(
            &plan(vec![template]),
            &HashMap::new(),
            "T1",
            &[item("a", "src/a"), item("b", "src/b")],
            None,
            None,
            &[],
        )
        .unwrap();
        let replay = expand(
            &first.plan,
            &HashMap::new(),
            "T1",
            &[item("a", "src/a"), item("b", "src/b")],
            Some(&first.join_task_id),
            None,
            &[],
        )
        .unwrap();
        assert!(replay.idempotent);
        assert_eq!(replay.plan, first.plan);
        assert_eq!(replay.join_task_id, first.join_task_id);

        let error = expand(
            &first.plan,
            &HashMap::new(),
            "T1",
            &[item("a", "src/a"), item("c", "src/c")],
            None,
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<FanoutError>().unwrap().code,
            codes::FANOUT_CONFLICT
        );
    }

    #[test]
    fn rejects_recursive_protected_and_active_dependents() {
        let mut child = template();
        child.id = "T1--child".to_string();
        child.parent_task_id = Some("T1".to_string());
        child.fanout_key = Some("child".to_string());
        let error = expand(
            &plan(vec![template(), child]),
            &HashMap::new(),
            "T1--child",
            &[item("a", "src/a")],
            None,
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<FanoutError>().unwrap().code,
            codes::FANOUT_RECURSIVE
        );

        let error = expand(
            &plan(vec![template()]),
            &statuses(&[("T1", "running")]),
            "T1",
            &[item("a", "src/a")],
            None,
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<FanoutError>().unwrap().code,
            codes::FANOUT_PROTECTED
        );

        let dependent = PlanTask {
            id: "T3".to_string(),
            department: "impl".to_string(),
            title: "Verify".to_string(),
            kind: "coding".to_string(),
            priority: 0,
            profile: Some("coding".to_string()),
            deps: vec!["T1".to_string()],
            write_scope: vec!["docs".to_string()],
            acceptance: vec!["done".to_string()],
            token_budget: None,
            cost_budget_usd: None,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        };
        let error = expand(
            &plan(vec![template(), dependent]),
            &statuses(&[("T1", "pending"), ("T3", "completed")]),
            "T1",
            &[item("a", "src/a")],
            None,
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<FanoutError>().unwrap().code,
            codes::FANOUT_PROTECTED
        );
    }
}
