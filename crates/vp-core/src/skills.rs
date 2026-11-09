//! Parsing and validation for project skill packs.
//!
//! A skill is prompt-level guidance plus an optional tool policy. It is not
//! executable code, a model route, or a hidden permission grant.

use std::path::Path;

use anyhow::{Context, Result, anyhow};

pub const MAX_SKILL_FILE_BYTES: u64 = 128 * 1024;
pub const MAX_SKILL_INSTRUCTIONS_CHARS: usize = 20_000;
pub const MAX_SKILL_DESCRIPTION_CHARS: usize = 600;
pub const MAX_SKILL_WHEN_TO_USE_CHARS: usize = 1_000;
pub const MAX_SKILL_TOOLS: usize = 64;
pub const MAX_SKILL_NAME_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSkillFile {
    pub name: String,
    pub slug: String,
    pub description: String,
    pub when_to_use: String,
    pub version: String,
    pub instructions: String,
    pub tool_allow: Vec<String>,
    pub tool_deny: Vec<String>,
    pub profiles: Vec<String>,
    pub roles: Vec<String>,
}

pub fn parse_skill_markdown(content: &str, fallback_name: &str) -> Result<ParsedSkillFile> {
    if content.len() as u64 > MAX_SKILL_FILE_BYTES {
        return Err(anyhow!(
            "skill file is larger than {} bytes",
            MAX_SKILL_FILE_BYTES
        ));
    }
    let content = content.trim_start_matches('\u{feff}');
    let (metadata, instructions) = split_frontmatter(content)?;
    let mapping = metadata
        .as_ref()
        .map(|metadata| {
            serde_yaml::from_str::<serde_yaml::Value>(metadata)
                .context("invalid SKILL.md YAML frontmatter")?
                .as_mapping()
                .cloned()
                .ok_or_else(|| anyhow!("SKILL.md frontmatter must be a mapping"))
        })
        .transpose()?;
    let value = |keys: &[&str]| -> Option<&serde_yaml::Value> {
        let mapping = mapping.as_ref()?;
        keys.iter()
            .find_map(|key| mapping.get(serde_yaml::Value::String((*key).to_string())))
    };
    let name = value(&["name"])
        .and_then(yaml_string)
        .unwrap_or_else(|| fallback_name.to_string());
    let description = value(&["description"])
        .and_then(yaml_string)
        .unwrap_or_else(|| fallback_description(&instructions));
    let when_to_use = value(&["when_to_use", "when-to-use", "whenToUse"])
        .and_then(yaml_string)
        .unwrap_or_default();
    let version = value(&["version"])
        .and_then(yaml_string)
        .unwrap_or_else(|| "1.0.0".to_string());
    let tool_allow = value(&["tools", "allowed-tools", "allowed_tools", "tool_allow"])
        .map(yaml_string_list)
        .transpose()?
        .unwrap_or_default();
    let tool_deny = value(&[
        "disallowed-tools",
        "disallowed_tools",
        "tool_deny",
        "deny_tools",
    ])
    .map(yaml_string_list)
    .transpose()?
    .unwrap_or_default();
    let profiles = value(&["profiles"])
        .map(yaml_string_list)
        .transpose()?
        .unwrap_or_default();
    let roles = value(&["roles"])
        .map(yaml_string_list)
        .transpose()?
        .unwrap_or_default();
    validate_skill_fields(&name, &description, &when_to_use, &instructions, &version)?;
    Ok(ParsedSkillFile {
        slug: slugify(&name),
        name,
        description,
        when_to_use,
        version,
        instructions,
        tool_allow: normalize_tools(tool_allow)?,
        tool_deny: normalize_tools(tool_deny)?,
        profiles: normalize_strings(profiles, 16, 40)?,
        roles: normalize_strings(roles, 8, 30)?,
    })
}

pub fn validate_skill_fields(
    name: &str,
    description: &str,
    when_to_use: &str,
    instructions: &str,
    version: &str,
) -> Result<()> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_SKILL_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(anyhow!(
            "skill name must be 1-{MAX_SKILL_NAME_CHARS} characters without control characters"
        ));
    }
    if slugify(name).is_empty() {
        return Err(anyhow!("skill name must contain a letter or number"));
    }
    if description.trim().is_empty()
        || description.chars().count() > MAX_SKILL_DESCRIPTION_CHARS
        || description.chars().any(char::is_control)
    {
        return Err(anyhow!(
            "skill description must be 1-{MAX_SKILL_DESCRIPTION_CHARS} characters without control characters"
        ));
    }
    if when_to_use.chars().count() > MAX_SKILL_WHEN_TO_USE_CHARS
        || when_to_use.chars().any(char::is_control)
    {
        return Err(anyhow!(
            "skill when-to-use must be at most {MAX_SKILL_WHEN_TO_USE_CHARS} characters without control characters"
        ));
    }
    if instructions.trim().is_empty() || instructions.chars().count() > MAX_SKILL_INSTRUCTIONS_CHARS
    {
        return Err(anyhow!(
            "skill instructions must be 1-{MAX_SKILL_INSTRUCTIONS_CHARS} characters"
        ));
    }
    if version.trim().is_empty() || version.chars().count() > 40 {
        return Err(anyhow!("skill version must be 1-40 characters"));
    }
    Ok(())
}

pub fn slugify(value: &str) -> String {
    let mut output = String::new();
    let mut last_dash = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            output.push(character);
            last_dash = false;
        } else if !last_dash && !output.is_empty() {
            output.push('-');
            last_dash = true;
        }
    }
    output.trim_matches('-').chars().take(80).collect()
}

pub fn skill_file_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("skill")
        .to_string()
}

fn split_frontmatter(content: &str) -> Result<(Option<String>, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return Ok((None, content.trim().to_string()));
    }
    let opening_end = trimmed.find('\n').map(|index| index + 1).unwrap_or(0);
    let rest = &trimmed[opening_end..];
    let mut search = 0usize;
    while let Some(relative) = rest[search..].find("\n---") {
        let index = search + relative;
        let after = index + 4;
        let boundary = rest[after..]
            .chars()
            .next()
            .map(|character| character == '\n' || character == '\r')
            .unwrap_or(true);
        if boundary {
            let metadata = rest[..index].to_string();
            let body = rest[after..].trim_start_matches(['\r', '\n']).to_string();
            return Ok((Some(metadata), body));
        }
        search = after;
    }
    Err(anyhow!("SKILL.md frontmatter is not closed with ---"))
}

fn yaml_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(value) => Some(value.clone()),
        serde_yaml::Value::Number(value) => Some(value.to_string()),
        serde_yaml::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn yaml_string_list(value: &serde_yaml::Value) -> Result<Vec<String>> {
    match value {
        serde_yaml::Value::String(value) => Ok(value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToOwned::to_owned)
            .collect()),
        serde_yaml::Value::Sequence(values) => values
            .iter()
            .map(|value| {
                yaml_string(value).ok_or_else(|| anyhow!("skill list values must be strings"))
            })
            .collect(),
        serde_yaml::Value::Null => Ok(Vec::new()),
        _ => Err(anyhow!("skill list fields must be strings or arrays")),
    }
}

fn normalize_tools(tools: Vec<String>) -> Result<Vec<String>> {
    normalize_strings(tools, MAX_SKILL_TOOLS, 80)
}

fn normalize_strings(values: Vec<String>, max: usize, max_chars: usize) -> Result<Vec<String>> {
    if values.len() > max {
        return Err(anyhow!("list supports at most {max} entries"));
    }
    let mut output = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > max_chars {
            return Err(anyhow!("list entries must be 1-{max_chars} characters"));
        }
        if !output.iter().any(|existing| existing == value) {
            output.push(value.to_string());
        }
    }
    Ok(output)
}

fn fallback_description(instructions: &str) -> String {
    instructions
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Imported skill")
        .trim()
        .chars()
        .take(MAX_SKILL_DESCRIPTION_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let content = "---\nname: Release Review\ndescription: Review a release before publishing\nwhen_to_use: Before tagging a release\nversion: 1.2.0\ntools:\n  - read_file\n  - grep\nprofiles: [coding]\nroles: [leader, worker]\n---\n# Steps\n\n1. Read the diff.\n2. Run tests.\n";
        let parsed = parse_skill_markdown(content, "fallback").unwrap();
        assert_eq!(parsed.name, "Release Review");
        assert_eq!(parsed.slug, "release-review");
        assert_eq!(parsed.version, "1.2.0");
        assert_eq!(parsed.tool_allow, vec!["read_file", "grep"]);
        assert_eq!(parsed.profiles, vec!["coding"]);
        assert_eq!(parsed.roles, vec!["leader", "worker"]);
        assert!(parsed.instructions.contains("# Steps"));
        assert!(!parsed.instructions.contains("description:"));
    }

    #[test]
    fn accepts_plain_markdown_and_rejects_bad_frontmatter() {
        let plain = parse_skill_markdown("A concise review checklist.", "review").unwrap();
        assert_eq!(plain.name, "review");
        assert_eq!(plain.description, "A concise review checklist.");
        assert!(parse_skill_markdown("---\nname: [unclosed", "x").is_err());
    }

    #[test]
    fn slug_and_validation_are_predictable() {
        assert_eq!(slugify("  Hello, World!  "), "hello-world");
        assert_eq!(slugify("中文 技能"), "中文-技能");
        assert!(validate_skill_fields("", "desc", "", "body", "1").is_err());
        assert!(validate_skill_fields("Bad\nName", "desc", "", "body", "1").is_err());
        assert!(validate_skill_fields("Name", "bad\ndesc", "", "body", "1").is_err());
        assert!(validate_skill_fields("Name", "desc", "", "body", "1").is_ok());
    }
}
