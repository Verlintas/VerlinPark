use super::*;

impl Engine {
    /// Build the one-task TeamPlan used for an intake item. It is public for
    /// tests and for future import/migration tooling; dispatch uses it too.
    pub fn intake_plan(&self, task: &IntakeTask) -> TeamPlan {
        let department = if task.profile == "research" {
            "research"
        } else if task.profile == "verification" {
            "verification"
        } else {
            "delivery"
        };
        let manager_title = if department == "research" {
            "Research Manager"
        } else if department == "verification" {
            "Verification Manager"
        } else {
            "Delivery Manager"
        };
        let objective = format!("{}: {}", task.title, first_line(&task.instructions, 240));
        TeamPlan {
            goal: task.title.clone(),
            departments: vec![vp_protocol::types::PlanDepartment {
                name: department.to_string(),
                manager_title: manager_title.to_string(),
                profile: task.profile.clone(),
                objective,
                token_budget: task.token_budget,
                cost_budget_usd: task.cost_budget_usd,
            }],
            tasks: vec![vp_protocol::types::PlanTask {
                id: "T1".to_string(),
                department: department.to_string(),
                title: task.title.clone(),
                kind: task.kind.clone(),
                priority: task.priority,
                profile: Some(task.profile.clone()),
                deps: Vec::new(),
                write_scope: task.write_scope.clone(),
                acceptance: task.acceptance.clone(),
                token_budget: task.token_budget,
                cost_budget_usd: task.cost_budget_usd,
                parent_task_id: None,
                fanout_key: None,
                fanout_join_for: None,
            }],
        }
    }

    pub async fn list_intake_tasks(
        &self,
        project_id: Option<String>,
        status: Option<String>,
        limit: Option<u32>,
    ) -> Result<IntakeListResult> {
        let store = self.store.clone();
        let tasks = tokio::task::spawn_blocking(move || {
            store.list_intake_tasks(project_id.as_deref(), status.as_deref(), limit)
        })
        .await??;
        Ok(IntakeListResult { tasks })
    }

    pub async fn get_intake_task(&self, id: String) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.get_intake_task(&id)).await?
    }

    /// Validate and persist a standalone task. The daemon scheduler turns it
    /// into a deterministic one-task TeamPlan when a Worker slot is available.
    pub async fn create_intake_task(&self, params: IntakeCreateParams) -> Result<IntakeTask> {
        let draft = build_intake_draft(&params)?;
        let store = self.store.clone();
        let project_id = draft.project_id.clone();
        let deps = draft.deps.clone();
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            move || store.list_intake_tasks(Some(&project_id), None, None)
        })
        .await??;
        validate_intake_dependencies(&existing, None, &deps)?;
        let task = tokio::task::spawn_blocking(move || store.create_intake_task(&draft)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(task)
    }

    /// Edit a queued or failed intake task. Saving an edit re-queues it with a
    /// fresh manual attempt budget, matching the task-inbox workflow.
    pub async fn update_intake_task(&self, params: IntakeUpdateParams) -> Result<IntakeTask> {
        let store = self.store.clone();
        let id = params.id.clone();
        let current = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = id.clone();
            move || store.get_intake_task(&id)
        })
        .await??
        .ok_or_else(|| anyhow!("intake task not found: {id}"))?;
        if matches!(
            current.status.as_str(),
            "dispatching" | "running" | "completed"
        ) {
            anyhow::bail!(
                "intake task {id} cannot be edited while it is {}",
                current.status
            );
        }
        let draft = build_intake_draft(&IntakeCreateParams {
            project_id: current.project_id.clone(),
            title: params.title,
            instructions: params.instructions,
            profile: params.profile,
            kind: params.kind,
            priority: params.priority,
            write_scope: params.write_scope,
            acceptance: params.acceptance,
            deps: params.deps,
            token_budget: params.token_budget,
            cost_budget_usd: params.cost_budget_usd,
            time_budget_secs: params.time_budget_secs,
            max_attempts: params.max_attempts,
            labels: params.labels,
            origin: Some(current.origin.clone()),
        })?;
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = current.project_id.clone();
            move || store.list_intake_tasks(Some(&project_id), None, None)
        })
        .await??;
        validate_intake_dependencies(&existing, Some(&id), &draft.deps)?;
        let updated = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = id.clone();
            move || store.update_intake_task(&id, &draft)
        })
        .await??
        .ok_or_else(|| anyhow!("intake task {id} cannot be edited"))?;
        self.record_project_manifest_best_effort().await;
        Ok(updated)
    }

    pub async fn claim_intake_task(&self, id: String) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.claim_intake_task(&id)).await?
    }

    pub async fn start_intake_task(
        &self,
        id: String,
        session_id: String,
        task_id: String,
    ) -> Result<IntakeTask> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.start_intake_task(&id, &session_id, &task_id))
            .await?
    }

    pub async fn settle_intake_task(
        &self,
        id: String,
        status: String,
        result_summary: Option<String>,
        last_error: Option<String>,
        next_attempt_at: Option<String>,
    ) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || {
            store.settle_intake_task(
                &id,
                &status,
                result_summary.as_deref(),
                last_error.as_deref(),
                next_attempt_at.as_deref(),
            )
        })
        .await??;
        self.record_project_manifest_best_effort().await;
        Ok(task)
    }

    pub async fn retry_intake_task(&self, id: String) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || store.retry_intake_task(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(task)
    }

    pub async fn cancel_intake_task(&self, id: String) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || store.cancel_intake_task(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(task)
    }

    pub async fn delete_intake_task(&self, id: String) -> Result<bool> {
        let store = self.store.clone();
        let deleted = tokio::task::spawn_blocking(move || store.delete_intake_task(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(deleted)
    }

    pub async fn count_active_intake_tasks(&self, project_id: String) -> Result<u32> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.count_active_intake_tasks(&project_id)).await?
    }
}

fn validate_intake_dependencies(
    tasks: &[IntakeTask],
    task_id: Option<&str>,
    deps: &[String],
) -> Result<()> {
    if deps.len() > 20 {
        anyhow::bail!("intake supports at most 20 dependencies");
    }
    let by_id = tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    for dep in deps {
        if Some(dep.as_str()) == task_id {
            anyhow::bail!("an intake task cannot depend on itself");
        }
        if !by_id.contains_key(dep.as_str()) {
            anyhow::bail!("unknown intake dependency: {dep}");
        }
    }
    let Some(task_id) = task_id else {
        return Ok(());
    };
    // Editing can introduce a cycle. Walk existing dependency edges from each
    // new dependency and reject a path back to the edited task.
    let mut stack = deps.to_vec();
    let mut visited = std::collections::HashSet::new();
    while let Some(current) = stack.pop() {
        if current == task_id {
            anyhow::bail!("intake dependencies would create a cycle");
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(task) = by_id.get(current.as_str()) {
            stack.extend(task.deps.iter().cloned());
        }
    }
    Ok(())
}

fn build_intake_draft(params: &IntakeCreateParams) -> Result<NewIntakeTask> {
    const MAX_TITLE_CHARS: usize = 200;
    const MAX_INSTRUCTIONS_CHARS: usize = 20_000;
    const MAX_SCOPES: usize = 32;
    const MAX_SCOPE_CHARS: usize = 500;
    const MAX_ACCEPTANCE: usize = 12;
    const MAX_ACCEPTANCE_CHARS: usize = 500;
    const MAX_LABELS: usize = 12;
    const MAX_LABEL_CHARS: usize = 40;
    let title = params.title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        anyhow::bail!("intake title must be 1-{MAX_TITLE_CHARS} characters");
    }
    let instructions = params.instructions.trim();
    if instructions.is_empty() || instructions.chars().count() > MAX_INSTRUCTIONS_CHARS {
        anyhow::bail!("intake instructions must be 1-{MAX_INSTRUCTIONS_CHARS} characters");
    }
    let profile = params
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("coding");
    if crate::agent::AgentProfile::parse(profile).is_none() {
        anyhow::bail!("unknown intake profile: {profile}");
    }
    let kind = params
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            match profile {
                "research" => "research",
                "verification" => "verification",
                "generic" => "analysis",
                _ => "coding",
            }
            .to_string()
        });
    if kind.chars().count() > 60 {
        anyhow::bail!("intake kind must be at most 60 characters");
    }
    let priority = params.priority.unwrap_or(0);
    if !(-100..=100).contains(&priority) {
        anyhow::bail!("intake priority must be between -100 and 100");
    }
    if params.write_scope.len() > MAX_SCOPES {
        anyhow::bail!("intake write scope supports at most {MAX_SCOPES} paths");
    }
    let mut write_scope = Vec::new();
    for scope in &params.write_scope {
        let raw = scope.trim();
        if raw.is_empty()
            || raw.starts_with('/')
            || raw.starts_with('~')
            || raw.contains("..")
            || Path::new(raw).is_absolute()
        {
            anyhow::bail!("invalid intake write scope: {scope}");
        }
        let scope = raw.trim_end_matches('/');
        if scope.is_empty() || scope.chars().count() > MAX_SCOPE_CHARS {
            anyhow::bail!("invalid intake write scope: {scope}");
        }
        if !write_scope.iter().any(|existing| existing == scope) {
            write_scope.push(scope.to_string());
        }
    }
    let needs_write = !write_scope.is_empty()
        || matches!(
            kind.to_ascii_lowercase().as_str(),
            "coding" | "code" | "implement" | "implementation" | "edit" | "fix"
        );
    if needs_write && matches!(profile, "generic" | "verification") {
        anyhow::bail!(
            "writable intake tasks need a writable profile; use coding or research, or remove the write scope"
        );
    }
    let mut acceptance = Vec::new();
    for item in &params.acceptance {
        let item = item.trim();
        if item.is_empty() || item.chars().count() > MAX_ACCEPTANCE_CHARS {
            anyhow::bail!("intake acceptance items must be 1-{MAX_ACCEPTANCE_CHARS} characters");
        }
        acceptance.push(item.to_string());
    }
    if acceptance.len() > MAX_ACCEPTANCE {
        anyhow::bail!("intake supports at most {MAX_ACCEPTANCE} acceptance items");
    }
    if acceptance.is_empty() {
        acceptance
            .push("The requested outcome is implemented, verified, and summarized.".to_string());
    }
    if params.token_budget == Some(0) {
        anyhow::bail!("intake token budget must be greater than zero");
    }
    if params
        .cost_budget_usd
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        anyhow::bail!("intake cost budget must be a positive finite number");
    }
    if params.time_budget_secs == Some(0) {
        anyhow::bail!("intake time budget must be greater than zero");
    }
    let max_attempts = params.max_attempts.unwrap_or(2);
    if !(1..=10).contains(&max_attempts) {
        anyhow::bail!("intake max attempts must be between 1 and 10");
    }
    let mut labels = Vec::new();
    for label in &params.labels {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
            anyhow::bail!("intake labels must be 1-{MAX_LABEL_CHARS} characters");
        }
        if !labels.iter().any(|existing| existing == label) {
            labels.push(label.to_string());
        }
    }
    if labels.len() > MAX_LABELS {
        anyhow::bail!("intake supports at most {MAX_LABELS} labels");
    }
    let origin = params
        .origin
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("user");
    if origin.chars().count() > 32 {
        anyhow::bail!("intake origin must be at most 32 characters");
    }
    let mut deps = Vec::new();
    for dep in &params.deps {
        let dep = dep.trim();
        if dep.is_empty() || dep.chars().count() > 80 {
            anyhow::bail!("intake dependency ids must be 1-80 characters");
        }
        if !deps.iter().any(|existing| existing == dep) {
            deps.push(dep.to_string());
        }
    }
    if deps.len() > 20 {
        anyhow::bail!("intake supports at most 20 dependencies");
    }
    Ok(NewIntakeTask {
        project_id: params.project_id.clone(),
        title: title.to_string(),
        instructions: instructions.to_string(),
        profile: profile.to_string(),
        kind,
        priority,
        write_scope,
        acceptance,
        deps,
        token_budget: params.token_budget,
        cost_budget_usd: params.cost_budget_usd,
        time_budget_secs: params.time_budget_secs,
        max_attempts,
        origin: origin.to_string(),
        labels,
    })
}
