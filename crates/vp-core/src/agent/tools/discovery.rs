use super::*;

/// Read-only guidance. It never executes a tool, calls a provider, or changes
/// the advertised schema. The catalog is process-wide and deterministic.
pub(in crate::agent) fn run_tool_help(spec: &AgentSpec, input: &Value) -> ToolOutput {
    let slug = input
        .get("recipe")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|slug| !slug.is_empty());
    let requested_code = input
        .get("error_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|code| !code.is_empty());
    let include_error_codes = input
        .get("include_error_codes")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_chars = input
        .get("max_chars")
        .and_then(Value::as_u64)
        .unwrap_or(crate::tooling::recipes::MAX_HELP_RENDER_CHARS as u64)
        .clamp(1_000, crate::tooling::recipes::MAX_HELP_RENDER_CHARS as u64)
        as usize;
    let role = spec.role.as_str();
    let profile = spec.profile.as_str();
    let (mut rendered, recipes) = if let Some(raw_code) = requested_code {
        match crate::tooling::errors::ToolErrorCode::from_code(raw_code) {
            Some(code) => (crate::tooling::errors::render_code_detail(code), Vec::new()),
            None => {
                return ToolOutput::error_coded(
                    "tool_help",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    format!(
                        "unknown error code '{raw_code}'; call tool_help with include_error_codes=true to list stable codes"
                    ),
                )
                .with_failure_evidence(format!("error_code={raw_code}"));
            }
        }
    } else if include_error_codes && slug.is_none() {
        // A pure code lookup should not be crowded out by the full recipe
        // catalog. Request a recipe explicitly to combine both.
        (String::new(), Vec::new())
    } else {
        crate::tooling::recipes::render_help(role, profile, slug)
    };
    if include_error_codes {
        if !rendered.is_empty() {
            rendered.push_str("\n\n");
        }
        rendered.push_str(&crate::tooling::errors::render_catalog(max_chars));
    }
    if rendered.chars().count() > max_chars {
        rendered = rendered
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        rendered.push('…');
    }
    let error_code = requested_code
        .and_then(crate::tooling::errors::ToolErrorCode::from_code)
        .map(|code| {
            json!({
                "code": code.as_str(),
                "category": code.category(),
                "retryable": code.default_retryable(),
                "hint": code.default_hint(),
            })
        });
    let structured = json!({
        "version": 1,
        "catalog_hash": crate::tooling::recipes::catalog_hash(),
        "role": role,
        "profile": profile,
        "requested_recipe": slug,
        "requested_error_code": requested_code,
        "error_code": error_code,
        "error_codes_included": include_error_codes,
        "count": recipes.len(),
        "recipes": recipes
            .iter()
            .map(|recipe| json!({
                "slug": recipe.slug,
                "title": recipe.title,
                "intent": recipe.intent,
                "tools": recipe.tools,
                "steps": recipe.steps,
                "example": recipe.example,
                "caveats": recipe.caveats,
            }))
            .collect::<Vec<_>>(),
        "executed_tool": false,
        "note": "Recipes and error codes are static guidance. This tool did not execute or route any tool.",
    });
    if recipes.is_empty() && requested_code.is_none() && !include_error_codes {
        return ToolOutput::error_coded(
            "tool_help",
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            rendered,
        )
        .with_structured(structured);
    }
    ToolOutput::ok_structured(rendered, structured)
}

pub(in crate::agent) async fn run_tool_search(
    config: &AgentConfig,
    engine: &Engine,
    spec: &AgentSpec,
    project_id: &str,
    frozen_bundle: &crate::tool_bundle::FrozenToolBundle,
    input: Value,
) -> Result<ToolOutput> {
    use crate::tooling::search::{
        MissingPrerequisite, SearchDocument, completeness_estimate, prerequisite_closure,
        rank_documents,
    };

    let query = input
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 50) as usize;
    let include_schema = input
        .get("include_schema")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let promote = input
        .get("promote")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let requested_task_kind = input
        .get("task_kind")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| spec.task_kind.clone());
    let advertised = frozen_bundle
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::HashSet<_>>();

    struct SearchEntry {
        key: String,
        name: String,
        description: String,
        task_kind: String,
        category: String,
        kind: &'static str,
        callable: bool,
        in_bundle: bool,
        pinned: bool,
        server_id: Option<String>,
        server_name: Option<String>,
        tool_name: Option<String>,
        trust: Option<String>,
        schema: Option<Value>,
    }

    let builtin_schemas = tool_schemas(config, spec.role, spec.profile, spec.allow_delegation)
        .into_iter()
        .filter(|tool| tool.name != "tool_search")
        .collect::<Vec<_>>();
    let available_builtins = builtin_schemas
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut entries = builtin_schemas
        .iter()
        .map(|tool| SearchEntry {
            key: format!("builtin:{}", tool.name),
            name: tool.name.clone(),
            description: tool.description.chars().take(4_000).collect(),
            task_kind: spec.task_kind.clone().unwrap_or_default(),
            category: crate::tool_bundle::catalog_entry(&tool.name)
                .map(|entry| entry.category.as_str().to_string())
                .unwrap_or_else(|| "unbundled".to_string()),
            kind: "builtin",
            callable: true,
            in_bundle: advertised.contains(tool.name.as_str()),
            pinned: true,
            server_id: None,
            server_name: None,
            tool_name: None,
            trust: None,
            schema: Some(tool.parameters.clone()),
        })
        .collect::<Vec<_>>();

    let mcp_callable = matches!(
        spec.role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    );
    let pin_map = engine
        .project_mcp_pins(project_id, None)
        .await
        .map(|result| {
            result
                .pins
                .into_iter()
                .map(|pin| ((pin.server_id.clone(), pin.tool_name.clone()), pin))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let servers = engine.list_mcp_servers().await?.servers;
    for server in servers.into_iter().filter(|server| server.enabled).take(20) {
        let Ok(tools) = engine.mcp_tools(&server.id).await else {
            continue;
        };
        let server_has_pins = pin_map.keys().any(|(server_id, _)| *server_id == server.id);
        for tool in tools.tools.into_iter().take(100) {
            let exposed = dynamic_mcp_tool_name(&server.name, &tool.name);
            let pin = pin_map.get(&(server.id.clone(), tool.name.clone()));
            let trust = match pin {
                Some(pin) if crate::mcp_pins::pin_is_callable(pin) => pin.trust(),
                Some(pin) if pin.status == "pinned" => "drift",
                Some(pin) => pin.trust(),
                None if !server_has_pins => "approved",
                None => "unverified",
            };
            let pinned = pin.is_none_or(crate::mcp_pins::pin_is_callable);
            entries.push(SearchEntry {
                key: format!("mcp:{}:{}", server.id, tool.name),
                name: exposed.clone(),
                description: tool.description.chars().take(4_000).collect(),
                task_kind: String::new(),
                category: "mcp".to_string(),
                kind: "mcp",
                callable: mcp_callable && pinned,
                in_bundle: advertised.contains(exposed.as_str()),
                pinned,
                server_id: Some(server.id.clone()),
                server_name: Some(server.name.clone()),
                tool_name: Some(tool.name.clone()),
                trust: Some(trust.to_string()),
                schema: Some(tool.input_schema.clone()),
            });
        }
    }

    let documents = entries
        .iter()
        .map(|entry| SearchDocument {
            key: &entry.key,
            name: &entry.name,
            description: &entry.description,
            task_kind: &entry.task_kind,
            category: &entry.category,
        })
        .collect::<Vec<_>>();
    let ranked = rank_documents(&documents, &query, requested_task_kind.as_deref(), 100);
    let ranked_truncated = ranked.len() > limit;
    let selected_keys = ranked
        .iter()
        .take(limit)
        .map(|hit| hit.key.clone())
        .collect::<Vec<_>>();
    let omitted_hits = ranked
        .iter()
        .skip(limit)
        .filter_map(|hit| entries.iter().find(|entry| entry.key == hit.key))
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    let selected_builtin_names = selected_keys
        .iter()
        .filter_map(|key| {
            entries
                .iter()
                .find(|entry| &entry.key == key && entry.kind == "builtin")
                .map(|entry| entry.name.clone())
        })
        .collect::<Vec<_>>();
    let closure = prerequisite_closure(
        &selected_builtin_names,
        &available_builtins,
        crate::tooling::search::MAX_PREREQUISITE_ADDITIONS,
    );
    let mut selected = selected_keys
        .iter()
        .filter_map(|key| entries.iter().position(|entry| &entry.key == key))
        .collect::<Vec<_>>();
    let mut prerequisite_names = std::collections::BTreeSet::new();
    for addition in &closure.additions {
        if let Some(index) = entries
            .iter()
            .position(|entry| entry.name == addition.name && entry.kind == "builtin")
        {
            prerequisite_names.insert(entries[index].name.clone());
            if !selected.contains(&index) {
                selected.push(index);
            }
        }
    }
    // MCP calls require the generic `mcp` bridge. Include it in the returned
    // set when available, or report it missing; the MCP tool itself is not
    // promoted as a builtin schema.
    let selected_mcp = selected.iter().any(|index| entries[*index].kind == "mcp");
    if selected_mcp
        && let Some(index) = entries
            .iter()
            .position(|entry| entry.name == "mcp" && entry.kind == "builtin")
    {
        prerequisite_names.insert(entries[index].name.clone());
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    let promoted_candidates = if promote {
        selected
            .iter()
            .filter_map(|index| {
                let entry = &entries[*index];
                (entry.kind == "builtin" && entry.callable && !entry.in_bundle)
                    .then(|| entry.name.clone())
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut missing = closure.missing;
    if selected_mcp && !available_builtins.contains("mcp") {
        missing.push(MissingPrerequisite {
            name: "mcp".to_string(),
            reason: "not_available_for_role".to_string(),
            required_by: selected
                .iter()
                .filter(|index| entries[**index].kind == "mcp")
                .map(|index| entries[*index].name.clone())
                .take(8)
                .collect(),
        });
    }
    for index in selected
        .iter()
        .filter(|index| entries[**index].kind == "mcp")
    {
        let entry = &entries[*index];
        if !entry.callable {
            missing.push(MissingPrerequisite {
                name: entry.name.clone(),
                reason: "mcp_pin_untrusted".to_string(),
                required_by: vec![entry.name.clone()],
            });
        }
    }
    for name in omitted_hits {
        if missing.iter().any(|entry| entry.name == name) {
            continue;
        }
        missing.push(MissingPrerequisite {
            name,
            reason: "result_limit".to_string(),
            required_by: vec!["query".to_string()],
        });
    }
    if ranked_truncated && ranked.len() >= 100 {
        missing.push(MissingPrerequisite {
            name: "search_result_cap".to_string(),
            reason: "result_cap".to_string(),
            required_by: vec!["query".to_string()],
        });
    }
    missing.sort_by(|left, right| left.name.cmp(&right.name));
    missing.dedup_by(|left, right| left.name == right.name);
    missing.truncate(crate::tooling::search::MAX_MISSING_PREREQUISITES);

    let hit_scores = ranked
        .iter()
        .map(|hit| (hit.key.clone(), hit.score))
        .collect::<HashMap<_, _>>();
    let mut results = Vec::new();
    for index in selected {
        let entry = &entries[index];
        let prerequisite = prerequisite_names.contains(&entry.name);
        let mut value = json!({
            "name": entry.name,
            "kind": entry.kind,
            "description": entry.description.chars().take(400).collect::<String>(),
            "callable": entry.callable,
            "in_bundle": entry.in_bundle,
            "bundle": frozen_bundle.selection.kind.as_str(),
            "category": entry.category,
            "score": hit_scores.get(&entry.key).copied().unwrap_or(0.0),
            "prerequisite": prerequisite,
        });
        if let Some(server_id) = &entry.server_id {
            value["server_id"] = json!(server_id);
            value["server_name"] = json!(entry.server_name);
            value["tool_name"] = json!(entry.tool_name);
            value["trust"] = json!(entry.trust);
            value["pinned"] = json!(entry.pinned);
        }
        if prerequisite
            && let Some(addition) = closure
                .additions
                .iter()
                .find(|addition| addition.name == entry.name)
        {
            value["required_by"] = json!(addition.required_by);
        }
        if include_schema && let Some(schema) = &entry.schema {
            value["parameters_preview"] = json!(bounded_schema(schema));
        }
        results.push(value);
    }
    results.sort_by(|left, right| {
        right["score"]
            .as_f64()
            .partial_cmp(&left["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left["prerequisite"]
                    .as_bool()
                    .cmp(&right["prerequisite"].as_bool())
            })
            .then_with(|| left["kind"].as_str().cmp(&right["kind"].as_str()))
            .then_with(|| left["name"].as_str().cmp(&right["name"].as_str()))
    });
    let final_names = results
        .iter()
        .filter_map(|result| result["name"].as_str())
        .collect::<Vec<_>>();
    let selected_count = final_names.len();
    let estimate = if selected_count == 0 && !query.is_empty() {
        0.0
    } else {
        completeness_estimate(selected_count, missing.len())
    };
    let rendered = results
        .iter()
        .map(|result| {
            format!(
                "{} · {}{}{}{} · {}",
                result["name"].as_str().unwrap_or("?"),
                result["kind"].as_str().unwrap_or("tool"),
                if result["kind"] == "mcp" {
                    result["server_id"]
                        .as_str()
                        .map(|server| format!(" ({server})"))
                        .unwrap_or_default()
                } else {
                    String::new()
                },
                if result["prerequisite"].as_bool().unwrap_or(false) {
                    " [prerequisite]"
                } else {
                    ""
                },
                if result["in_bundle"].as_bool().unwrap_or(false) {
                    ""
                } else {
                    " [outside frozen bundle; callable if role policy allows]"
                },
                result["description"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mcp_hint = if results.iter().any(|result| result["kind"] == "mcp") {
        if mcp_callable {
            "\nCall an MCP result with mcp action=call server=<server_id> tool=<tool_name> arguments={...}."
        } else {
            "\nThis role can inspect MCP tools but cannot call them."
        }
    } else {
        ""
    };
    let promotion = if promote {
        json!({
            "requested": true,
            "candidates": promoted_candidates,
            "note": "Promoted schemas are applied only at the next turn boundary; the current request keeps its frozen tool set.",
        })
    } else {
        json!({"requested": false})
    };
    Ok(ToolOutput::ok_structured(
        truncate(&format!("{rendered}{mcp_hint}")),
        json!({
            "query": query,
            "task_kind": requested_task_kind,
            "tools": results,
            "set": final_names,
            "count": selected_count,
            "missing": missing,
            "completeness_estimate": estimate,
            "truncated": ranked_truncated,
            "prerequisite_additions": closure.additions,
            "mcp_callable": mcp_callable,
            "bundle": frozen_bundle.selection.kind.as_str(),
            "bundle_hash": frozen_bundle.bundle_hash,
            "advertised_count": advertised.len(),
            "outside_bundle": results.iter().filter(|result| !result["in_bundle"].as_bool().unwrap_or(false)).count(),
            "promotion": promotion,
        }),
    ))
}

pub(in crate::agent) fn bounded_schema(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .chars()
        .take(800)
        .collect()
}

pub(in crate::agent) async fn run_read_tool_output(
    engine: &Engine,
    session: &Session,
    input: serde_json::Value,
) -> ToolOutput {
    let params = match crate::tool_output::ReadToolOutputParams::from_input(&input) {
        Ok(params) => params,
        Err(error) => {
            return ToolOutput::error_coded(
                "read_tool_output",
                crate::tooling::errors::ToolErrorCode::TOOL_OUTPUT_INVALID,
                format!("read_tool_output: {error}"),
            );
        }
    };
    let store = crate::tool_output::ToolOutputStore::new(engine.store().clone());
    let session_id = session.id.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::tool_output::page_tool_output(&store, &session_id, &params)
    })
    .await;
    match result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            let message = error.to_string();
            let lower = message.to_ascii_lowercase();
            let code = if lower.contains("expired") {
                crate::tooling::errors::ToolErrorCode::TOOL_OUTPUT_EXPIRED
            } else if lower.contains("not found") || lower.contains("unknown") {
                crate::tooling::errors::ToolErrorCode::TOOL_OUTPUT_NOT_FOUND
            } else {
                crate::tooling::errors::ToolErrorCode::TOOL_OUTPUT_INVALID
            };
            ToolOutput::error_coded(
                "read_tool_output",
                code,
                format!("read_tool_output: {message}"),
            )
        }
        Err(error) => ToolOutput::error_coded(
            "read_tool_output",
            crate::tooling::errors::ToolErrorCode::INTERNAL_ERROR,
            format!("read_tool_output task failed: {error}"),
        ),
    }
}
