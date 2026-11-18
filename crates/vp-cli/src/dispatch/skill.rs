//! Skill catalog commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Skills { project, all } => {
            rpc(
                &discovery,
                "skill.list",
                json!({"project_id": project, "include_disabled": all}),
            )
            .await?
        }
        Command::SkillCreate {
            project,
            name,
            description,
            instructions,
            when_to_use,
            version,
            tool_allow,
            tool_deny,
            profiles,
            roles,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading skill instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "skill.create",
                json!({
                    "project_id": project,
                    "name": name,
                    "description": description,
                    "when_to_use": when_to_use,
                    "version": version,
                    "instructions": instructions,
                    "tool_allow": tool_allow,
                    "tool_deny": tool_deny,
                    "profiles": profiles,
                    "roles": roles,
                }),
            )
            .await?
        }
        Command::SkillActivations { session } => {
            rpc(
                &discovery,
                "skill.activations",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::SkillImport { project, path } => {
            rpc(
                &discovery,
                "skill.import",
                json!({"project_id": project, "path": path}),
            )
            .await?
        }
        Command::SkillEnabled { id, enabled } => {
            rpc(
                &discovery,
                "skill.set_enabled",
                json!({"id": id, "enabled": enabled.unwrap_or(true)}),
            )
            .await?
        }
        Command::SkillDelete { id } => rpc(&discovery, "skill.delete", json!({"id": id})).await?,
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
