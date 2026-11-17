use super::*;

/// Resolve the pin trust for a generic `mcp action=call` request. A missing
/// server/tool is `Unknown` so the contract escalates rather than allowing.
pub(in crate::agent) async fn resolve_mcp_pin_trust(
    engine: &Engine,
    project_id: &str,
    input: &Value,
) -> crate::contract::McpPinTrust {
    let Some(selector) = input.get("server").and_then(Value::as_str) else {
        return crate::contract::McpPinTrust::Unknown;
    };
    let Some(tool) = input.get("tool").and_then(Value::as_str) else {
        return crate::contract::McpPinTrust::Unknown;
    };
    let server = match engine.find_mcp_server(selector).await {
        Ok(Some(server)) => server,
        Ok(None) | Err(_) => return crate::contract::McpPinTrust::Unknown,
    };
    engine
        .mcp_pin_trust_for_tool(project_id, &server.id, tool)
        .await
        .unwrap_or(crate::contract::McpPinTrust::Unknown)
}

#[allow(clippy::result_large_err)] // the typed failure carries the stable contract
pub(in crate::agent) async fn run_mcp_tool(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: serde_json::Value,
    effect_receipt_id: Option<&str>,
) -> Result<ToolOutput> {
    let role = spec.role;
    let project_id = session.project_id.as_str();
    let action = input
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("action is required"))?;
    if action == "call"
        && !matches!(
            role,
            AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
        )
    {
        return Ok(ToolOutput::error_coded(
            "mcp",
            crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
            "this role may inspect MCP servers but cannot execute MCP tools",
        ));
    }
    let servers = engine.list_mcp_servers().await?.servers;
    let server = input
        .get("server")
        .and_then(serde_json::Value::as_str)
        .map(|selector| {
            servers
                .iter()
                .find(|server| server.id == selector || server.name == selector)
                .cloned()
                .ok_or_else(|| {
                    crate::tooling::errors::ToolFailure::new(
                        crate::tooling::errors::ToolErrorCode::MCP_SERVER_NOT_FOUND,
                        format!("MCP server not found: {selector}"),
                    )
                    .with_tool("mcp")
                    .with_evidence(format!("server={selector}"))
                })
        })
        .transpose()?;
    match action {
        "list" => {
            if let Some(server) = server {
                let tools = engine.mcp_tools(&server.id).await?.tools;
                let mut text = format!(
                    "MCP server {} ({}):
",
                    server.name, server.id
                );
                for tool in tools {
                    text.push_str(&format!(
                        "- {}: {}
",
                        tool.name,
                        truncate_to(&tool.description, 300)
                    ));
                }
                if text.ends_with(":\n") {
                    text.push_str("(no tools)");
                }
                Ok(ToolOutput::ok(text))
            } else {
                let mut text = String::from(
                    "MCP servers:
",
                );
                for server in servers {
                    text.push_str(&format!(
                        "- {} ({}) [{}] {}
",
                        server.name,
                        server.id,
                        if server.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        server.command
                    ));
                }
                Ok(ToolOutput::ok(text))
            }
        }
        "call" => {
            let server = server.ok_or_else(|| anyhow!("server is required for action=call"))?;
            if !server.enabled {
                return Ok(ToolOutput::error_coded(
                    "mcp",
                    crate::tooling::errors::ToolErrorCode::MCP_DISABLED,
                    format!(
                        "MCP server {} is disabled; enable it before calling its tools",
                        server.name
                    ),
                ));
            }
            let tool = input
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("tool is required for action=call"))?;
            // Pin trust is enforced here, independent of ContractPolicy mode.
            // The generic `mcp` tool must not become a bypass for a drifted,
            // unverified, or removed tool.
            let trust = engine
                .mcp_pin_trust_for_tool(project_id, &server.id, tool)
                .await
                .unwrap_or(crate::contract::McpPinTrust::Unknown);
            if trust != crate::contract::McpPinTrust::Trusted {
                return Ok(ToolOutput::error_coded(
                    "mcp",
                    crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
                    format!(
                        "MCP tool {}/{} is not approved for execution (trust: {}); approve its current pin first",
                        server.name,
                        tool,
                        trust.label()
                    ),
                ));
            }
            let arguments = input
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let context = crate::mcp::tasks::McpTaskContext::from_session(
                &session.project_id,
                &session.id,
                &spec.id,
                &spec.name,
                spec.task_id.as_deref(),
                effect_receipt_id,
            );
            let result = engine
                .mcp_call_with_context(&server.id, tool, arguments, context)
                .await?;
            let mut text = format_mcp_tool_result(&result.result);
            if let Some(task) = &result.task {
                text.push_str(&format!(
                    "\nAsync MCP task {} is {}. Inspect explicitly with mcp action=task_get task_id={} or task_result.",
                    task.id, task.status, task.id
                ));
            }
            Ok(ToolOutput::ok(truncate_to(&text, 16_000)))
        }
        "task_list" => {
            let tasks = engine
                .mcp_task_list(vp_protocol::types::McpTaskListParams {
                    project_id: Some(session.project_id.clone()),
                    session_id: Some(session.id.clone()),
                    server_id: server.map(|server| server.id),
                    status: input
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    limit: input
                        .get("limit")
                        .and_then(serde_json::Value::as_u64)
                        .map(|limit| limit.clamp(1, 500) as u32),
                })
                .await?;
            let mut text = String::new();
            for task in &tasks.tasks {
                text.push_str(&format!(
                    "- {} {}/{} [{}] poll={}ms expires={}\n",
                    task.id,
                    task.server_name,
                    task.tool_name,
                    task.status,
                    task.poll_interval_ms,
                    task.expires_at.as_deref().unwrap_or("none")
                ));
            }
            if text.is_empty() {
                text.push_str("(no MCP async tasks)");
            }
            if tasks.truncated {
                text.push_str("\n(list truncated)");
            }
            Ok(ToolOutput::ok_structured(
                text,
                serde_json::json!({"tasks": tasks.tasks, "truncated": tasks.truncated}),
            ))
        }
        "task_get" | "task_result" => {
            let task_id = input
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("task_id is required for action={action}"))?;
            if action == "task_get" {
                let task = engine.mcp_task_get(task_id, true).await?;
                Ok(ToolOutput::ok_structured(
                    format!(
                        "MCP task {} {}/{} [{}] poll={}ms",
                        task.id,
                        task.server_name,
                        task.tool_name,
                        task.status,
                        task.poll_interval_ms
                    ),
                    serde_json::json!(task),
                ))
            } else {
                let result = engine
                    .mcp_task_result(vp_protocol::types::McpTaskResultParams {
                        task_id: task_id.to_string(),
                        refresh: input.get("refresh").and_then(serde_json::Value::as_bool),
                    })
                    .await?;
                let mut text = format!(
                    "MCP task {} {}/{} [{}]",
                    result.task.id,
                    result.task.server_name,
                    result.task.tool_name,
                    result.task.status
                );
                if result.pending {
                    text.push_str(&format!(
                        "\nStill pending; poll explicitly after {} ms.",
                        result.task.poll_interval_ms
                    ));
                }
                if let Some(value) = &result.result {
                    text.push('\n');
                    text.push_str(&format_mcp_tool_result(value));
                }
                Ok(ToolOutput::ok_structured(
                    truncate_to(&text, 16_000),
                    serde_json::json!(result),
                ))
            }
        }
        "task_cancel" => {
            if !matches!(
                role,
                AgentRole::Leader | AgentRole::Manager | AgentRole::Worker | AgentRole::Labour
            ) {
                return Ok(ToolOutput::error_coded(
                    "mcp",
                    crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
                    "this role may inspect MCP tasks but cannot cancel them",
                ));
            }
            let task_id = input
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("task_id is required for action=task_cancel"))?;
            let result = engine
                .mcp_task_cancel(
                    task_id,
                    input.get("reason").and_then(serde_json::Value::as_str),
                )
                .await?;
            Ok(ToolOutput::ok_structured(
                format!(
                    "MCP task {} [{}]: {}",
                    result.task.id, result.task.status, result.message
                ),
                serde_json::json!(result),
            ))
        }
        "resources" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=resources"))?;
            let result = engine.mcp_resources(&server.id).await?;
            let mut text = format!("MCP resources on {}:\n", server.name);
            for resource in &result.resources {
                text.push_str(&format!("- {} <{}>", resource.name, resource.uri));
                if let Some(mime_type) = &resource.mime_type {
                    text.push_str(&format!(" [{mime_type}]"));
                }
                if !resource.description.is_empty() {
                    text.push_str(&format!(": {}", truncate_to(&resource.description, 200)));
                }
                text.push('\n');
            }
            for template in &result.templates {
                text.push_str(&format!(
                    "- template {} <{}>: {}\n",
                    template.name,
                    template.uri_template,
                    truncate_to(&template.description, 200)
                ));
            }
            if result.resources.is_empty() && result.templates.is_empty() {
                text.push_str("(no resources)");
            }
            Ok(ToolOutput::ok(text))
        }
        "read_resource" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=read_resource"))?;
            let uri = input
                .get("uri")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("uri is required for action=read_resource"))?;
            let result = engine.mcp_read_resource(&server.id, uri).await?;
            let mut text = String::new();
            for content in result.contents {
                if let Some(body) = content.text {
                    text.push_str(&body);
                    text.push('\n');
                } else if let Some(blob) = content.blob {
                    text.push_str(&format!(
                        "[binary resource {}: {} base64 chars{}]\n",
                        content.uri,
                        blob.len(),
                        content
                            .mime_type
                            .map(|mime| format!(", {mime}"))
                            .unwrap_or_default()
                    ));
                }
            }
            if text.is_empty() {
                text.push_str("(empty resource)");
            }
            Ok(ToolOutput::ok(truncate_to(&text, 16_000)))
        }
        "prompts" => {
            let server = server.ok_or_else(|| anyhow!("server is required for action=prompts"))?;
            let result = engine.mcp_prompts(&server.id).await?;
            let mut text = format!("MCP prompts on {}:\n", server.name);
            for prompt in result.prompts {
                text.push_str(&format!(
                    "- {}: {}",
                    prompt.name,
                    truncate_to(&prompt.description, 200)
                ));
                if !prompt.arguments.is_empty() {
                    let arguments = prompt
                        .arguments
                        .iter()
                        .map(|argument| {
                            if argument.required {
                                format!("{}=?", argument.name)
                            } else {
                                format!("{}?", argument.name)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    text.push_str(&format!(" ({arguments})"));
                }
                text.push('\n');
            }
            if text.ends_with(":\n") {
                text.push_str("(no prompts)");
            }
            Ok(ToolOutput::ok(text))
        }
        "get_prompt" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=get_prompt"))?;
            let prompt = input
                .get("prompt")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("prompt is required for action=get_prompt"))?;
            let arguments = input
                .get("prompt_arguments")
                .and_then(serde_json::Value::as_object)
                .map(|arguments| {
                    arguments
                        .iter()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let result = engine.mcp_prompt(&server.id, prompt, arguments).await?;
            let mut text = String::new();
            if !result.description.is_empty() {
                text.push_str(&result.description);
                text.push_str("\n\n");
            }
            for message in result.messages {
                text.push_str(&format!("[{}] ", message.role));
                text.push_str(&truncate_to(
                    &serde_json::to_string(&message.content)?,
                    4_000,
                ));
                text.push('\n');
            }
            Ok(ToolOutput::ok(truncate_to(&text, 16_000)))
        }
        other => Ok(ToolOutput::error_coded(
            "mcp",
            crate::tooling::errors::ToolErrorCode::INVALID_ACTION,
            format!("unknown MCP action: {other}"),
        )
        .with_failure_action(
            "use action list, call, resources, read_resource, prompts, or get_prompt",
        )),
    }
}

pub(in crate::agent) fn skill_allows_tool(skill: &Skill, tool: &str) -> bool {
    if tool == "use_skill" {
        return true;
    }
    if skill
        .tool_deny
        .iter()
        .any(|denied| denied == "*" || denied == tool)
    {
        return false;
    }
    skill.tool_allow.is_empty()
        || skill
            .tool_allow
            .iter()
            .any(|allowed| allowed == "*" || allowed == tool)
}

pub(in crate::agent) async fn run_skill_tool(
    engine: &Engine,
    project: &Project,
    spec: &AgentSpec,
    input: Value,
    active: &mut Option<Skill>,
) -> Result<ToolOutput> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("name is required"))?;
    let skills = engine
        .skills_for(&project.id, spec.role.as_str(), spec.profile.as_str())
        .await?;
    let Some(skill) = skills.iter().find(|skill| {
        skill.name.eq_ignore_ascii_case(name) || skill.slug.eq_ignore_ascii_case(name)
    }) else {
        let available = skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(ToolOutput::error_coded(
            "use_skill",
            crate::tooling::errors::ToolErrorCode::SKILL_FAILED,
            if available.is_empty() {
                "No enabled skills apply to this role/profile.".to_string()
            } else {
                format!("Unknown skill '{name}'. Available skills: {available}")
            },
        ));
    };
    *active = Some(skill.clone());
    let allow = if skill.tool_allow.is_empty() {
        "unrestricted by this skill".to_string()
    } else {
        skill.tool_allow.join(", ")
    };
    let deny = if skill.tool_deny.is_empty() {
        "none".to_string()
    } else {
        skill.tool_deny.join(", ")
    };
    Ok(ToolOutput::ok(truncate_to(
        &format!(
            "Skill '{}' v{} activated.\nAllowed tools: {allow}\nDenied tools: {deny}\n\n{}\n\nFollow these instructions for the rest of this run. Use_skill always remains available to switch skills.",
            skill.name, skill.version, skill.instructions
        ),
        22_000,
    )))
}

pub(in crate::agent) async fn run_source_tool(
    engine: &Engine,
    project: &Project,
    name: &str,
    input: Value,
    citations: &mut CitationRegistry,
) -> Result<ToolOutput> {
    match name {
        "search_sources" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("query is required"))?;
            let source_ids = input
                .get("source_ids")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let collections = input
                .get("collections")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let labels = input
                .get("labels")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(6)
                .clamp(1, 20) as u32;
            let mode = input
                .get("mode")
                .and_then(Value::as_str)
                .map(str::parse::<vp_protocol::types::SourceSearchMode>)
                .transpose()
                .map_err(|error| anyhow!("{error}"))?;
            let result = engine
                .search_sources_with_mode(
                    project.id.clone(),
                    query.to_string(),
                    source_ids,
                    collections,
                    labels,
                    Some(limit),
                    mode,
                )
                .await?;
            if result.hits.is_empty() {
                let reason = result
                    .semantic_unavailable
                    .as_deref()
                    .map(|reason| format!(" Semantic retrieval was unavailable ({reason})."))
                    .unwrap_or_default();
                return Ok(ToolOutput::ok(format!(
                    "No matching source chunks. Try a shorter query or list_sources.{reason}"
                )));
            }
            let mut output = String::new();
            for hit in &result.hits {
                let key = citations.add_hit(hit, "search");
                let heading = hit.heading.as_deref().unwrap_or("untitled section");
                output.push_str(&format!(
                    "[{key}] {} — {} (chunk {}, score {:.2})\n{}\n\n",
                    hit.source_title,
                    heading,
                    hit.chunk_no,
                    hit.score,
                    truncate_to(&hit.content, 1_400),
                ));
            }
            if let Some(reason) = &result.semantic_unavailable {
                output.push_str(&format!(
                    "Semantic retrieval was unavailable ({reason}); results are lexical.\n"
                ));
            } else if result.mode != "lexical" {
                output.push_str(&format!(
                    "Retrieval mode: {} (each chunk is labeled with its origin).\n",
                    result.mode
                ));
            }
            if result.degraded {
                output.push_str(
                    "Search used the lexical fallback because the FTS index had no direct match.\n",
                );
            }
            Ok(ToolOutput::ok(truncate_to(&output, 24_000)))
        }
        "read_source" => {
            let source_id = input
                .get("source_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("source_id is required"))?;
            let chunk_no = input
                .get("chunk_no")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 500) as u32;
            let max_chars = input
                .get("max_chars")
                .and_then(Value::as_u64)
                .unwrap_or(6_000)
                .clamp(300, 20_000) as usize;
            let result = engine
                .get_source(source_id.to_string(), Some(chunk_no - 1), Some(1))
                .await?;
            let Some(chunk) = result.chunks.first() else {
                return Ok(ToolOutput::error(format!(
                    "source {source_id} has no chunk {chunk_no}"
                )));
            };
            let hit = SourceSearchHit {
                source_id: result.source.id.clone(),
                source_title: result.source.title.clone(),
                collection: result.source.collection.clone(),
                labels: result.source.labels.clone(),
                chunk_id: chunk.id.clone(),
                chunk_no: chunk.chunk_no,
                heading: chunk.heading.clone(),
                content: chunk.content.clone(),
                score: 1.0,
                source: "lexical".to_string(),
                confidence: 1.0,
                origins: vec!["read".to_string()],
                lexical_score: Some(1.0),
                semantic_score: None,
                source_updated_at: Some(result.source.updated_at.clone()),
                ..SourceSearchHit::default()
            };
            let key = citations.add_hit(&hit, "read");
            let heading = chunk.heading.as_deref().unwrap_or("untitled section");
            Ok(ToolOutput::ok(format!(
                "[{key}] {} — {} (chunk {} of {})\n{}",
                result.source.title,
                heading,
                chunk.chunk_no,
                result.total_chunks,
                truncate_to(&chunk.content, max_chars),
            )))
        }
        "list_sources" => {
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 50) as u32;
            let result = engine
                .list_sources(project.id.clone(), None, None, Some(limit))
                .await?;
            if result.sources.is_empty() {
                return Ok(ToolOutput::ok("No project sources are indexed."));
            }
            let mut output = String::new();
            for source in result.sources {
                output.push_str(&format!(
                    "- {} · {} · {} chunks · status={}\n  id={}\n",
                    source.title, source.kind, source.chunk_count, source.status, source.id
                ));
            }
            Ok(ToolOutput::ok(truncate_to(&output, 12_000)))
        }
        other => Ok(ToolOutput::error_coded(
            "sources",
            crate::tooling::errors::ToolErrorCode::INVALID_ACTION,
            format!("unknown source tool: {other}"),
        )),
    }
}

pub(in crate::agent) async fn run_memory_tool(
    engine: &Engine,
    session: &Session,
    project: &Project,
    spec: &AgentSpec,
    name: &str,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    match name {
        "remember" => {
            if matches!(spec.role, AgentRole::Verifier | AgentRole::Scout) {
                return Ok(ToolOutput::error(format!(
                    "{} cannot write project memory",
                    spec.role.default_name()
                )));
            }
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("kind is required"))?;
            let title = input
                .get("title")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("title is required"))?;
            let body = input
                .get("body")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("body is required"))?;
            let tags = input
                .get("tags")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            let scope = match tool_memory_scope(&input) {
                Ok(scope) => scope,
                Err(output) => return Ok(output),
            };
            let Some(scope) = scope else {
                // Omitted scope is the legacy project-memory path, unchanged.
                let memory = engine
                    .create_memory_as(&session.id, &spec.id, &spec.name, kind, title, body, tags)
                    .await?;
                return Ok(ToolOutput::ok(format!(
                    "remembered [{}] {} ({})",
                    memory.kind, memory.title, memory.id
                )));
            };
            let identity = match active_memory_identity(engine, session, spec).await {
                Ok(identity) => identity,
                Err(error) => return Ok(memory_tool_failure("remember", &error)),
            };
            let (identity_id, department) = match scope {
                "agent" => {
                    let Some(identity) = identity else {
                        return Ok(ToolOutput::error_coded(
                            "remember",
                            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                            "error[MEMORY_IDENTITY_REQUIRED] agent scope requires an assigned project identity; omit scope for legacy project memory",
                        ));
                    };
                    (Some(identity.id), None)
                }
                "department" => {
                    let Some(identity) = identity else {
                        return Ok(ToolOutput::error_coded(
                            "remember",
                            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                            "error[MEMORY_IDENTITY_REQUIRED] department scope requires an assigned project identity",
                        ));
                    };
                    let Some(department) = identity.department.clone() else {
                        return Ok(ToolOutput::error_coded(
                            "remember",
                            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                            "error[MEMORY_DEPARTMENT_REQUIRED] department scope requires an identity with a department",
                        ));
                    };
                    (None, Some(department))
                }
                "project" => (None, None),
                other => {
                    return Ok(ToolOutput::error_coded(
                        "remember",
                        crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                        format!(
                            "error[MEMORY_SCOPE_INVALID] scope must be agent, department, or project: {other}"
                        ),
                    ));
                }
            };
            let content = format!("{}: {}", title.trim(), body.trim());
            let mut evidence = match tool_memory_evidence(&input, session, spec) {
                Ok(evidence) => evidence,
                Err(output) => return Ok(output),
            };
            if evidence.len() > vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE {
                evidence.truncate(vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE);
            }
            let expires_at = input
                .get("expires_at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let pinned = input
                .get("pinned")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let memory = engine
                .remember_agent_memory(vp_protocol::types::CreateAgentMemoryParams {
                    project_id: project.id.clone(),
                    identity_id,
                    department,
                    scope: scope.to_string(),
                    content,
                    evidence,
                    source: Some("agent".to_string()),
                    expires_at,
                    pinned,
                    ..vp_protocol::types::CreateAgentMemoryParams::default()
                })
                .await;
            match memory {
                Ok(result) => Ok(ToolOutput::ok(format!(
                    "remembered [{}] {} ({})",
                    result.memory.scope,
                    truncate_to(&result.memory.content, 120),
                    result.memory.id
                ))),
                Err(error) => Ok(memory_tool_failure("remember", &error)),
            }
        }
        "recall" => {
            let query = input
                .get("query")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let limit = input
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 20) as u32;
            let scope = match tool_memory_scope(&input) {
                Ok(scope) => scope,
                Err(output) => return Ok(output),
            };
            let Some(scope) = scope else {
                let memories = engine
                    .list_memories(&project.id, query, kind, limit)
                    .await?
                    .memories;
                if memories.is_empty() {
                    return Ok(ToolOutput::ok("No matching project memories."));
                }
                let mut text = String::new();
                for memory in memories {
                    text.push_str(&format!(
                        "[{}] {} · {} · {}\n{}\n",
                        memory.kind,
                        memory.title,
                        memory.agent_name,
                        truncate_to(&memory.body, 1200),
                        memory.id
                    ));
                }
                return Ok(ToolOutput::ok(text));
            };
            let identity = match active_memory_identity(engine, session, spec).await {
                Ok(identity) => identity,
                Err(error) => return Ok(memory_tool_failure("recall", &error)),
            };
            let viewer = match scope {
                "agent" | "department" if identity.is_none() => {
                    return Ok(ToolOutput::error_coded(
                        "recall",
                        crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                        format!(
                            "error[MEMORY_IDENTITY_REQUIRED] {scope} scope requires an assigned project identity"
                        ),
                    ));
                }
                "agent" | "department" | "project" => {
                    identity.as_ref().map(|identity| identity.id.clone())
                }
                other => {
                    return Ok(ToolOutput::error_coded(
                        "recall",
                        crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                        format!(
                            "error[MEMORY_SCOPE_INVALID] scope must be agent, department, or project: {other}"
                        ),
                    ));
                }
            };
            let result = engine
                .list_agent_memories(vp_protocol::types::AgentMemoryListParams {
                    project_id: project.id.clone(),
                    identity_id: viewer,
                    scope: Some(scope.to_string()),
                    query,
                    limit: Some(limit),
                    ..vp_protocol::types::AgentMemoryListParams::default()
                })
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => return Ok(memory_tool_failure("recall", &error)),
            };
            if result.memories.is_empty() {
                return Ok(ToolOutput::ok("No matching identity memories."));
            }
            let mut text = String::new();
            for memory in result.memories {
                let status = if memory.expired {
                    "expired"
                } else {
                    memory.status.as_str()
                };
                let provenance = crate::identity_memory::memory_provenance_label(&memory);
                text.push_str(&format!(
                    "[{}·{}·{}] {}{}\n{}\n",
                    memory.scope,
                    status,
                    memory.source,
                    truncate_to(&memory.content, 1200),
                    if provenance.is_empty() {
                        String::new()
                    } else {
                        format!(" ({provenance})")
                    },
                    memory.id
                ));
            }
            Ok(ToolOutput::ok(text))
        }
        other => Ok(ToolOutput::error_coded(
            "memory",
            crate::tooling::errors::ToolErrorCode::INVALID_ACTION,
            format!("unknown memory tool: {other}"),
        )),
    }
}

/// Parse the optional S5 scope. An absent/blank value means the legacy
/// project-memory path; an unknown non-empty value fails closed instead of
/// silently switching stores.
#[allow(clippy::result_large_err)] // carries the bounded Q3 tool failure
pub(in crate::agent) fn tool_memory_scope(
    input: &serde_json::Value,
) -> std::result::Result<Option<&str>, ToolOutput> {
    let Some(scope) = input.get("scope").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let scope = scope.trim();
    if scope.is_empty() {
        return Ok(None);
    }
    if vp_protocol::types::agent_memory_scope(scope).is_none() {
        return Err(ToolOutput::error_coded(
            "memory",
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            format!(
                "error[MEMORY_SCOPE_INVALID] scope must be agent, department, or project: {scope}"
            ),
        ));
    }
    Ok(Some(scope))
}

pub(in crate::agent) async fn active_memory_identity(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
) -> Result<Option<vp_protocol::types::AgentIdentity>> {
    engine
        .prompt_identity_for_run(
            &session.id,
            spec.role.as_str(),
            spec.title.as_deref(),
            spec.department.as_deref(),
        )
        .await
}

#[allow(clippy::result_large_err)] // carries the bounded Q3 tool failure
pub(in crate::agent) fn tool_memory_evidence(
    input: &serde_json::Value,
    session: &Session,
    spec: &AgentSpec,
) -> std::result::Result<Vec<vp_protocol::types::MemoryEvidence>, ToolOutput> {
    let mut evidence = Vec::new();
    if let Some(items) = input.get("evidence").and_then(serde_json::Value::as_array) {
        for item in items
            .iter()
            .take(vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE)
        {
            let kind = item
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let reference = item
                .get("reference")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if kind.is_empty() || reference.is_empty() {
                return Err(ToolOutput::error_coded(
                    "remember",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    "error[MEMORY_EVIDENCE_INVALID] evidence items require kind and reference",
                ));
            }
            evidence.push(vp_protocol::types::MemoryEvidence {
                kind: kind.to_string(),
                reference: reference.to_string(),
                note: item
                    .get("note")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    // Automatic session/task provenance, deduplicated and still bounded.
    for (kind, reference) in [
        ("session", Some(session.id.as_str())),
        ("task", spec.task_id.as_deref()),
    ] {
        if let Some(reference) = reference
            && !evidence
                .iter()
                .any(|entry| entry.kind == kind && entry.reference == reference)
        {
            evidence.push(vp_protocol::types::MemoryEvidence {
                kind: kind.to_string(),
                reference: reference.to_string(),
                note: None,
            });
        }
    }
    if evidence.len() > vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE {
        evidence.truncate(vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE);
    }
    Ok(evidence)
}

pub(in crate::agent) fn memory_tool_failure(tool: &str, error: &anyhow::Error) -> ToolOutput {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentMemoryError>() {
        let code = if matches!(
            typed.code,
            vp_protocol::types::agent_memory_codes::MEMORY_CONFLICT
                | vp_protocol::types::agent_memory_codes::MEMORY_IMPORTED_READONLY
                | vp_protocol::types::agent_memory_codes::MEMORY_IDENTITY_RETIRED
        ) {
            crate::tooling::errors::ToolErrorCode::CONFLICT
        } else {
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT
        };
        return ToolOutput::error_coded(
            tool,
            code,
            format!("error[{}] {}", typed.code, typed.message),
        );
    }
    ToolOutput::error_coded(
        tool,
        crate::tooling::errors::ToolErrorCode::TOOL_FAILED,
        format!("{tool}: {error:#}"),
    )
}

pub(in crate::agent) async fn run_mailbox_tool(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    name: &str,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    match name {
        "send_message" => {
            let to = input
                .get("to")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("to is required"))?;
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("kind is required"))?;
            let body = input
                .get("body")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("body is required"))?;
            let payload = input
                .get("payload")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let agents = engine.list_agents(&session.id).await?;
            let target = agents
                .iter()
                .find(|agent| {
                    agent.id == to || agent.name == to || agent.name.eq_ignore_ascii_case(to)
                })
                .ok_or_else(|| {
                    anyhow!(
                        "agent not found in this session: {to}. Address another agent by id/name; answer the human directly instead of sending to 'user'."
                    )
                })?;
            let message = engine
                .send_agent_message(
                    &session.id,
                    &spec.id,
                    &spec.name,
                    &target.id,
                    &target.name,
                    kind,
                    body,
                    payload,
                )
                .await?;
            Ok(ToolOutput::ok(format!(
                "message {} sent to {} ({})",
                message.id, message.to_name, message.kind
            )))
        }
        "read_mailbox" => {
            let unread_only = input
                .get("unread_only")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            let messages = engine
                .list_agent_messages(&session.id, Some(spec.id.clone()), unread_only)
                .await?;
            let mut text = String::new();
            for message in &messages {
                if message.status == "unread" {
                    let _ = engine.mark_agent_message_read(&message.id).await;
                }
                text.push_str(&format!(
                    "[{}] {} · {} · {}
{}
",
                    message.id, message.from_name, message.kind, message.created_at, message.body
                ));
            }
            if text.is_empty() {
                text.push_str("No messages.");
            }
            Ok(ToolOutput::ok(text))
        }
        "publish_artifact" => {
            let name = input
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("name is required"))?;
            let content = input
                .get("content")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("content is required"))?;
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("note");
            let mime_type = input
                .get("mime_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("text/plain");
            let requested_key = input
                .get("request_key")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if requested_key.is_some()
                && input
                    .get("artifact_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
            {
                return Ok(ToolOutput::error(
                    "publish_artifact cannot update an artifact and use a request_key at the same time",
                ));
            }
            let artifact = if let Some(artifact_id) =
                input.get("artifact_id").and_then(serde_json::Value::as_str)
            {
                let expected = input
                    .get("expected_version")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok());
                engine
                    .publish_artifact_version(artifact_id, expected, name, kind, mime_type, content)
                    .await?
            } else if let Some(request_key) = requested_key {
                engine
                    .publish_artifact_keyed(
                        &session.id,
                        &spec.id,
                        &spec.name,
                        spec.task_id.clone(),
                        name,
                        kind,
                        mime_type,
                        content,
                        request_key,
                    )
                    .await?
            } else {
                engine
                    .publish_artifact(
                        &session.id,
                        &spec.id,
                        &spec.name,
                        spec.task_id.clone(),
                        name,
                        kind,
                        mime_type,
                        content,
                    )
                    .await?
            };
            Ok(ToolOutput::ok(format!(
                "artifact {} version {} published: {}",
                artifact.id, artifact.version, artifact.name
            )))
        }
        "list_artifacts" => {
            let artifacts = engine.list_artifacts(&session.id).await?;
            if artifacts.is_empty() {
                return Ok(ToolOutput::ok("No artifacts."));
            }
            let text = artifacts
                .iter()
                .map(|artifact| {
                    format!(
                        "{}  {}  {}  {}  {}  {} bytes  {}",
                        artifact.id,
                        artifact.name,
                        artifact.kind,
                        artifact.storage,
                        artifact.compression,
                        artifact.size_bytes,
                        artifact.agent_name
                    )
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            Ok(ToolOutput::ok(text))
        }
        "read_artifact" => {
            let id = input
                .get("artifact_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("artifact_id is required"))?;
            let artifact = engine
                .get_artifact(id)
                .await?
                .ok_or_else(|| anyhow!("artifact not found: {id}"))?;
            if artifact.storage == "file" {
                let materialized = engine
                    .materialize_artifact(id)
                    .await?
                    .ok_or_else(|| anyhow!("artifact {id} has no materialized file"))?;
                Ok(ToolOutput::ok(format!(
                    "{} [{}] · {} · {} · {} bytes · {}",
                    artifact.name,
                    artifact.mime_type,
                    artifact.compression,
                    artifact.storage,
                    artifact.size_bytes,
                    materialized.path
                )))
            } else {
                Ok(ToolOutput::ok(format!(
                    "{} [{}]
{}",
                    artifact.name, artifact.mime_type, artifact.content
                )))
            }
        }
        other => Err(anyhow!("unknown mailbox tool: {other}")),
    }
}
