use super::*;

impl Store {
    const SKILL_COLUMNS: &'static str = "id, project_id, name, slug, description, when_to_use, \
        version, instructions, tool_allow, tool_deny, profiles, roles, enabled, source, sha256, \
        created_at, updated_at";

    pub fn list_skills(
        &self,
        project_id: Option<&str>,
        include_disabled: bool,
    ) -> Result<Vec<Skill>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM skills
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 = 1 OR enabled = 1)
             ORDER BY name COLLATE NOCASE ASC, id ASC",
            Self::SKILL_COLUMNS
        ))?;
        let rows = stmt.query_map(
            params![project_id, if include_disabled { 1 } else { 0 }],
            map_skill,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_skill(&self, id: &str) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {} FROM skills WHERE id = ?1", Self::SKILL_COLUMNS),
            params![id],
            map_skill,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_skill_by_slug(&self, project_id: &str, slug: &str) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!(
                "SELECT {} FROM skills WHERE project_id = ?1 AND slug = ?2",
                Self::SKILL_COLUMNS
            ),
            params![project_id, slug],
            map_skill,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn insert_skill(&self, skill: &Skill) -> Result<Skill> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO skills
             (id, project_id, name, slug, description, when_to_use, version, instructions,
              tool_allow, tool_deny, profiles, roles, enabled, source, sha256, created_at,
              updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                slug = excluded.slug,
                description = excluded.description,
                when_to_use = excluded.when_to_use,
                version = excluded.version,
                instructions = excluded.instructions,
                tool_allow = excluded.tool_allow,
                tool_deny = excluded.tool_deny,
                profiles = excluded.profiles,
                roles = excluded.roles,
                enabled = excluded.enabled,
                source = excluded.source,
                sha256 = excluded.sha256,
                updated_at = excluded.updated_at",
            params![
                skill.id,
                skill.project_id,
                skill.name,
                skill.slug,
                skill.description,
                skill.when_to_use,
                skill.version,
                skill.instructions,
                serde_json::to_string(&skill.tool_allow)?,
                serde_json::to_string(&skill.tool_deny)?,
                serde_json::to_string(&skill.profiles)?,
                serde_json::to_string(&skill.roles)?,
                if skill.enabled { 1 } else { 0 },
                skill.source,
                skill.sha256,
                skill.created_at,
                skill.updated_at,
            ],
        )?;
        drop(conn);
        self.get_skill(&skill.id)?
            .ok_or_else(|| anyhow!("skill disappeared after insert"))
    }

    pub fn delete_skill(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute("DELETE FROM skills WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE skills SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, if enabled { 1 } else { 0 }, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_skill(id)
    }
}
