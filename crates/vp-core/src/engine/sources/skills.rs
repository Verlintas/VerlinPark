use super::support::truncate_chars;
use super::*;

// ---- Project skill packs -------------------------------------------

impl Engine {
    /// Skill activations derived from the persistent `skill.activated` events.
    /// Events remain the source of truth; no second usage table can drift.
    pub async fn session_skill_activations(
        &self,
        session_id: &str,
    ) -> Result<SessionSkillActivationsResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut activations = Vec::new();
        for event in events {
            if event.kind != "skill.activated" {
                continue;
            }
            let payload = &event.payload;
            let Some(name) = payload.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let string = |key: &str| {
                payload
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let list = |key: &str| {
                payload
                    .get(key)
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            activations.push(SkillActivation {
                name: name.to_string(),
                slug: string("slug"),
                version: string("version"),
                role: string("role"),
                profile: string("profile"),
                tool_allow: list("tool_allow"),
                tool_deny: list("tool_deny"),
                agent_id: payload
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                agent_name: payload
                    .get("agent_name")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                task_id: payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                activated_at: event.created_at.clone(),
            });
        }
        Ok(SessionSkillActivationsResult { activations })
    }

    pub async fn list_skills(
        &self,
        project_id: String,
        include_disabled: bool,
    ) -> Result<SkillListResult> {
        self.project_for_sources(&project_id).await?;
        let store = self.store.clone();
        let skills = tokio::task::spawn_blocking(move || {
            store.list_skills(Some(&project_id), include_disabled)
        })
        .await??;
        Ok(SkillListResult { skills })
    }

    pub async fn create_skill(&self, params: SkillCreateParams) -> Result<Skill> {
        let project = self.project_for_sources(&params.project_id).await?;
        let skill = build_skill(
            &project.id,
            &params.name,
            &params.description,
            &params.when_to_use,
            params.version.as_deref(),
            &params.instructions,
            params.tool_allow,
            params.tool_deny,
            params.profiles,
            params.roles,
            "user",
            true,
        )?;
        self.ensure_skill_slug_free(&project.id, &skill.slug, None)
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&skill)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn import_skill(&self, params: SkillImportParams) -> Result<Skill> {
        let project = self.project_for_sources(&params.project_id).await?;
        let root = PathBuf::from(&project.root);
        let path = params.path.clone();
        let (content, fallback_name) = tokio::task::spawn_blocking(move || {
            read_project_text_file(&root, &path, crate::skills::MAX_SKILL_FILE_BYTES).map(
                |(content, relative)| {
                    (
                        content,
                        crate::skills::skill_file_name(Path::new(&relative)),
                    )
                },
            )
        })
        .await??;
        let parsed = crate::skills::parse_skill_markdown(&content, &fallback_name)?;
        let skill = build_skill(
            &project.id,
            &parsed.name,
            &parsed.description,
            &parsed.when_to_use,
            Some(&parsed.version),
            &parsed.instructions,
            parsed.tool_allow,
            parsed.tool_deny,
            parsed.profiles,
            parsed.roles,
            "import",
            true,
        )?;
        self.ensure_skill_slug_free(&project.id, &skill.slug, None)
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&skill)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn update_skill(&self, params: SkillUpdateParams) -> Result<Skill> {
        let store = self.store.clone();
        let id = params.id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_skill(&id))
            .await??
            .ok_or_else(|| anyhow!("skill not found: {}", params.id))?;
        let skill = build_skill(
            &current.project_id,
            &params.name,
            &params.description,
            &params.when_to_use,
            params.version.as_deref(),
            &params.instructions,
            params.tool_allow,
            params.tool_deny,
            params.profiles,
            params.roles,
            &current.source,
            current.enabled,
        )?;
        let mut updated = skill;
        updated.id = current.id;
        updated.created_at = current.created_at;
        self.ensure_skill_slug_free(&updated.project_id, &updated.slug, Some(&updated.id))
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&updated)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn set_skill_enabled(&self, params: SkillEnabledParams) -> Result<Skill> {
        let store = self.store.clone();
        let id = params.id.clone();
        let skill =
            tokio::task::spawn_blocking(move || store.set_skill_enabled(&id, params.enabled))
                .await??
                .ok_or_else(|| anyhow!("skill not found: {}", params.id))?;
        self.record_project_manifest_best_effort().await;
        Ok(skill)
    }

    pub async fn delete_skill(&self, id: String) -> Result<SkillDeleteResult> {
        let store = self.store.clone();
        let deleted = tokio::task::spawn_blocking(move || store.delete_skill(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(SkillDeleteResult { deleted })
    }

    /// Enabled skills applicable to this role/profile.
    pub async fn skills_for(
        &self,
        project_id: &str,
        role: &str,
        profile: &str,
    ) -> Result<Vec<Skill>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let skills =
            tokio::task::spawn_blocking(move || store.list_skills(Some(&project_id), false))
                .await??;
        Ok(skills
            .into_iter()
            .filter(|skill| skill_applies(skill, role, profile))
            .collect())
    }

    /// Enabled-skill catalog for agent system prompts. Only a compact index is
    /// injected; the full instructions are loaded by the `use_skill` tool.
    pub async fn skill_catalog(
        &self,
        project_id: &str,
        role: &str,
        profile: &str,
    ) -> Result<String> {
        let skills = self.skills_for(project_id, role, profile).await?;
        let mut output = String::new();
        for skill in skills.iter().take(24) {
            let when = if skill.when_to_use.trim().is_empty() {
                String::new()
            } else {
                format!(" — {}", truncate_chars(&skill.when_to_use, 220))
            };
            let line = format!(
                "- {} ({}){}: {}\n",
                skill.name,
                skill.version,
                when,
                truncate_chars(&skill.description, 300)
            );
            if output.len() + line.len() > 4_000 {
                break;
            }
            output.push_str(&line);
        }
        Ok(output)
    }

    async fn ensure_skill_slug_free(
        &self,
        project_id: &str,
        slug: &str,
        excluding_id: Option<&str>,
    ) -> Result<()> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let slug = slug.to_string();
        let existing =
            tokio::task::spawn_blocking(move || store.get_skill_by_slug(&project_id, &slug))
                .await??;
        if let Some(existing) = existing
            && excluding_id != Some(existing.id.as_str())
        {
            anyhow::bail!("a skill with slug '{}' already exists", existing.slug);
        }
        Ok(())
    }
}

fn skill_applies(skill: &Skill, role: &str, profile: &str) -> bool {
    (skill.roles.is_empty()
        || skill
            .roles
            .iter()
            .any(|value| value.eq_ignore_ascii_case(role)))
        && (skill.profiles.is_empty()
            || skill
                .profiles
                .iter()
                .any(|value| value.eq_ignore_ascii_case(profile)))
}

#[allow(clippy::too_many_arguments)]
fn build_skill(
    project_id: &str,
    name: &str,
    description: &str,
    when_to_use: &str,
    version: Option<&str>,
    instructions: &str,
    tool_allow: Vec<String>,
    tool_deny: Vec<String>,
    profiles: Vec<String>,
    roles: Vec<String>,
    source: &str,
    enabled: bool,
) -> Result<Skill> {
    let name = name.trim();
    let description = description.trim();
    let when_to_use = when_to_use.trim();
    let instructions = instructions.trim();
    let version = version.unwrap_or("1.0.0").trim();
    crate::skills::validate_skill_fields(name, description, when_to_use, instructions, version)?;
    let slug = crate::skills::slugify(name);
    if slug.is_empty() {
        anyhow::bail!("skill name must contain a letter or number");
    }
    let tool_allow = normalize_skill_list(tool_allow, 64, 80, "tool")?;
    let tool_deny = normalize_skill_list(tool_deny, 64, 80, "tool")?;
    if tool_allow
        .iter()
        .any(|tool| tool_deny.iter().any(|denied| denied == tool))
    {
        anyhow::bail!("a skill tool cannot be both allowed and denied");
    }
    let profiles = normalize_skill_list(profiles, 4, 32, "profile")?;
    for profile in &profiles {
        if crate::agent::AgentProfile::parse(profile).is_none() {
            anyhow::bail!("unknown skill profile: {profile}");
        }
    }
    let roles = normalize_skill_list(roles, 5, 32, "role")?;
    for role in &roles {
        if !matches!(
            role.to_ascii_lowercase().as_str(),
            "leader" | "manager" | "worker" | "verifier" | "labour"
        ) {
            anyhow::bail!("unknown skill role: {role}");
        }
    }
    let now = now_rfc3339();
    Ok(Skill {
        id: Uuid::now_v7().to_string(),
        project_id: project_id.to_string(),
        name: name.to_string(),
        slug,
        description: description.to_string(),
        when_to_use: when_to_use.to_string(),
        version: version.to_string(),
        instructions: instructions.to_string(),
        tool_allow,
        tool_deny,
        profiles,
        roles,
        enabled,
        source: source.to_string(),
        sha256: sha256_hex(
            format!("{name}\n{description}\n{when_to_use}\n{instructions}\n{version}").as_bytes(),
        ),
        created_at: now.clone(),
        updated_at: now,
    })
}

fn normalize_skill_list(
    values: Vec<String>,
    max: usize,
    max_chars: usize,
    label: &str,
) -> Result<Vec<String>> {
    if values.len() > max {
        anyhow::bail!("skill supports at most {max} {label} entries");
    }
    let mut output = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > max_chars {
            anyhow::bail!("skill {label} entries must be 1-{max_chars} characters");
        }
        if !output.iter().any(|existing| existing == value) {
            output.push(value.to_string());
        }
    }
    Ok(output)
}

fn read_project_text_file(root: &Path, relative: &str, max_bytes: u64) -> Result<(String, String)> {
    let root = root.canonicalize().context("resolving project root")?;
    let relative_path = Path::new(relative.trim());
    if relative.trim().is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative_path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
    {
        anyhow::bail!("skill path must be relative, inside the project, and outside .git");
    }
    let path = root
        .join(relative_path)
        .canonicalize()
        .with_context(|| format!("resolving skill file {relative}"))?;
    if !path.starts_with(&root) {
        anyhow::bail!("skill path escapes the project root");
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        anyhow::bail!("skill path is not a file");
    }
    if metadata.len() > max_bytes {
        anyhow::bail!("skill file is larger than {max_bytes} bytes");
    }
    let content = std::fs::read_to_string(&path).context("skill file is not UTF-8")?;
    let relative = path
        .strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    Ok((content, relative))
}
