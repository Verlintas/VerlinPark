use super::*;

pub(crate) fn validate_team_plan(plan: &TeamPlan, max_managers: usize) -> Result<()> {
    if plan.goal.trim().is_empty() {
        return Err(anyhow!("team plan goal must not be empty"));
    }
    if plan.departments.is_empty() || plan.departments.len() > max_managers {
        return Err(anyhow!("team plan needs 1 to {max_managers} departments"));
    }
    let mut names = std::collections::HashSet::new();
    let mut titles = std::collections::HashSet::new();
    for department in &plan.departments {
        let name = department.name.trim();
        let title = department.manager_title.trim();
        if name.is_empty() || name.len() > 40 {
            return Err(anyhow!("department name must be 1-40 chars"));
        }
        if title.is_empty() || title.len() > 60 {
            return Err(anyhow!("manager title must be 1-60 chars"));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(anyhow!("duplicate department name: {name}"));
        }
        if !titles.insert(title.to_lowercase()) {
            return Err(anyhow!("duplicate manager title: {title}"));
        }
        if AgentProfile::parse(&department.profile).is_none() {
            return Err(anyhow!(
                "unknown profile '{}' for department '{}'",
                department.profile,
                department.name
            ));
        }
        if department.objective.trim().is_empty() {
            return Err(anyhow!(
                "department '{}' needs an objective",
                department.name
            ));
        }
        if department.token_budget == Some(0) {
            return Err(anyhow!(
                "department '{}' token budget must be greater than zero",
                department.name
            ));
        }
        if department
            .cost_budget_usd
            .is_some_and(|budget| !budget.is_finite() || budget <= 0.0)
        {
            return Err(anyhow!(
                "department '{}' cost budget must be a positive finite number",
                department.name
            ));
        }
    }
    if plan.tasks.len() > 100 {
        return Err(anyhow!("team plan supports at most 100 tasks"));
    }
    let mut ids = std::collections::HashSet::new();
    for task in &plan.tasks {
        let id = task.id.trim();
        worktree::validate_task_id(id)?;
        if !ids.insert(id.to_string()) {
            return Err(anyhow!("duplicate task id: {id}"));
        }
        let has_parent = task.parent_task_id.is_some();
        let has_key = task.fanout_key.is_some();
        if has_parent != has_key {
            return Err(anyhow!(
                "task '{id}' must set parent_task_id and fanout_key together"
            ));
        }
        if let Some(parent) = task.parent_task_id.as_deref()
            && (parent.trim().is_empty() || parent == id)
        {
            return Err(anyhow!("task '{id}' has an invalid fan-out parent"));
        }
        if let Some(key) = task.fanout_key.as_deref()
            && (key.is_empty()
                || key.chars().count() > 64
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        {
            return Err(anyhow!("task '{id}' has an invalid fan-out key"));
        }
        if let Some(parent) = task.fanout_join_for.as_deref() {
            if parent.trim().is_empty() || parent == id {
                return Err(anyhow!("task '{id}' has an invalid fan-out join parent"));
            }
            if has_parent || has_key {
                return Err(anyhow!(
                    "task '{id}' cannot be both a fan-out join and a fan-out child"
                ));
            }
            if !task.write_scope.is_empty() {
                return Err(anyhow!(
                    "fan-out join task '{id}' must not declare a write scope"
                ));
            }
        }
        let Some(department) = plan
            .departments
            .iter()
            .find(|department| department.name == task.department)
        else {
            return Err(anyhow!(
                "task '{}' references unknown department '{}'",
                id,
                task.department
            ));
        };
        let needs_write = !task.write_scope.is_empty()
            || matches!(
                task.kind.to_ascii_lowercase().as_str(),
                "coding" | "code" | "implement" | "implementation" | "edit" | "fix"
            );
        let resolved_profile = task
            .profile
            .as_deref()
            .unwrap_or(department.profile.as_str());
        if needs_write && matches!(resolved_profile, "generic" | "verification") {
            return Err(anyhow!(
                "task '{}' needs a writable profile: set task.profile=\"coding\" or use a coding department",
                id
            ));
        }
        if task.title.trim().is_empty() {
            return Err(anyhow!("task '{id}' needs a title"));
        }
        if task.kind.trim().is_empty() {
            return Err(anyhow!("task '{id}' needs a kind"));
        }
        if !(-100..=100).contains(&task.priority) {
            return Err(anyhow!("task '{id}' priority must be between -100 and 100"));
        }
        if let Some(profile) = &task.profile
            && AgentProfile::parse(profile).is_none()
        {
            return Err(anyhow!("task '{id}' has unknown profile: {profile}"));
        }
        if task.acceptance.is_empty() {
            return Err(anyhow!("task '{id}' needs at least one acceptance item"));
        }
        for dep in &task.deps {
            if dep == id {
                return Err(anyhow!("task '{id}' depends on itself"));
            }
            if !ids.contains(dep) && !plan.tasks.iter().any(|candidate| candidate.id == *dep) {
                return Err(anyhow!("task '{id}' has unknown dependency '{dep}'"));
            }
        }
        let mut scopes = Vec::new();
        for scope in &task.write_scope {
            let normalized = normalize_scope(scope);
            if normalized.is_empty()
                || normalized.contains("..")
                || normalized.starts_with('/')
                || normalized.starts_with('~')
            {
                return Err(anyhow!("task '{id}' has invalid write scope: {scope}"));
            }
            scopes.push(normalized);
        }
        if task.token_budget == Some(0) {
            return Err(anyhow!(
                "task '{id}' token budget must be greater than zero"
            ));
        }
        if task
            .cost_budget_usd
            .is_some_and(|budget| !budget.is_finite() || budget <= 0.0)
        {
            return Err(anyhow!(
                "task '{id}' cost budget must be a positive finite number"
            ));
        }
        if let Some(department) = plan
            .departments
            .iter()
            .find(|department| department.name == task.department)
        {
            if let (Some(task_limit), Some(department_limit)) =
                (task.token_budget, department.token_budget)
                && task_limit > department_limit
            {
                return Err(anyhow!(
                    "task '{id}' token budget exceeds its department budget"
                ));
            }
            if let (Some(task_limit), Some(department_limit)) =
                (task.cost_budget_usd, department.cost_budget_usd)
                && task_limit > department_limit + f64::EPSILON
            {
                return Err(anyhow!(
                    "task '{id}' cost budget exceeds its department budget"
                ));
            }
        }
    }
    // Fan-out children and their shared join form one explicit group. The join
    // must depend on exactly the children and no child may have a second key.
    let mut fanout_keys: std::collections::HashMap<(&str, &str), &str> =
        std::collections::HashMap::new();
    for task in &plan.tasks {
        let (Some(parent), Some(key)) =
            (task.parent_task_id.as_deref(), task.fanout_key.as_deref())
        else {
            continue;
        };
        if let Some(existing) = fanout_keys.insert((parent, key), task.id.as_str()) {
            return Err(anyhow!(
                "fan-out parent '{parent}' repeats key '{key}' in tasks '{existing}' and '{}'",
                task.id
            ));
        }
        if !plan
            .tasks
            .iter()
            .any(|candidate| candidate.fanout_join_for.as_deref() == Some(parent))
        {
            return Err(anyhow!(
                "fan-out child '{}' has no shared join task for parent '{parent}'",
                task.id
            ));
        }
    }
    for join in plan
        .tasks
        .iter()
        .filter(|task| task.fanout_join_for.is_some())
    {
        let parent = join.fanout_join_for.as_deref().unwrap_or_default();
        let children = plan
            .tasks
            .iter()
            .filter(|task| task.parent_task_id.as_deref() == Some(parent))
            .collect::<Vec<_>>();
        if children.is_empty() {
            return Err(anyhow!(
                "fan-out join task '{}' has no children for parent '{parent}'",
                join.id
            ));
        }
        let child_ids = children
            .iter()
            .map(|child| child.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let dep_ids = join
            .deps
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if child_ids != dep_ids {
            return Err(anyhow!(
                "fan-out join task '{}' must depend on exactly its {} children",
                join.id,
                children.len()
            ));
        }
    }
    for department in &plan.departments {
        let task_tokens: u64 = plan
            .tasks
            .iter()
            .filter(|task| task.department == department.name)
            .filter_map(|task| task.token_budget)
            .fold(0, u64::saturating_add);
        if let Some(limit) = department.token_budget
            && task_tokens > limit
        {
            return Err(anyhow!(
                "department '{}' task token budgets exceed the department cap",
                department.name
            ));
        }
        let task_cost: f64 = plan
            .tasks
            .iter()
            .filter(|task| task.department == department.name)
            .filter_map(|task| task.cost_budget_usd)
            .sum();
        if let Some(limit) = department.cost_budget_usd
            && task_cost > limit + f64::EPSILON
        {
            return Err(anyhow!(
                "department '{}' task cost budgets exceed the department cap",
                department.name
            ));
        }
    }
    // Cycle detection over the task DAG.
    fn visit(
        id: &str,
        tasks: &[vp_protocol::types::PlanTask],
        state: &mut std::collections::HashMap<String, u8>,
    ) -> Result<()> {
        match state.get(id).copied().unwrap_or(0) {
            1 => return Err(anyhow!("team plan task graph contains a cycle at '{id}'")),
            2 => return Ok(()),
            _ => {}
        }
        state.insert(id.to_string(), 1);
        if let Some(task) = tasks.iter().find(|task| task.id == id) {
            for dep in &task.deps {
                visit(dep, tasks, state)?;
            }
        }
        state.insert(id.to_string(), 2);
        Ok(())
    }
    let mut state = std::collections::HashMap::new();
    for task in &plan.tasks {
        visit(&task.id, &plan.tasks, &mut state)?;
    }
    Ok(())
}
