use super::*;

pub(super) fn source_tool_schemas() -> Vec<ToolSchema> {
    vec![
        ToolSchema {
            name: "search_sources".to_string(),
            description: "Search the project's indexed knowledge sources. Returns ranked chunks with citation keys such as [c1]. Cite every used chunk with its exact key. Mode defaults to hybrid when the project enabled semantic source embeddings, otherwise lexical; unavailable semantic retrieval falls back to lexical with a reason.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "maxLength": 400},
                    "source_ids": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"}
                    },
                    "collections": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"},
                        "description": "Restrict to these collections; an empty string selects uncollected sources"
                    },
                    "labels": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"},
                        "description": "Restrict to sources with any of these labels"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["lexical", "semantic", "hybrid"],
                        "description": "Retrieval lane. Omit for the project default (hybrid when source embeddings are enabled, otherwise lexical)."
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 20}
                },
                "required": ["query"]
            }),
        },
        ToolSchema {
            name: "read_source".to_string(),
            description: "Read one project source chunk in full. The returned citation key must be used when quoting it.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "source_id": {"type": "string"},
                    "chunk_no": {"type": "integer", "minimum": 1},
                    "max_chars": {"type": "integer", "minimum": 300, "maximum": 20000}
                },
                "required": ["source_id"]
            }),
        },
        ToolSchema {
            name: "list_sources".to_string(),
            description:
                "List indexed project sources and their ids. Use search_sources to read content."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50}
                }
            }),
        },
    ]
}

pub(super) fn archive_list_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "archive_list".to_string(),
        description: "List a zip, tar, or tar.gz artifact with hard entry/size/ratio caps. Entry bytes are never extracted and the workspace is not touched.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "artifact_id": {"type": "string"},
                "max_entries": {"type": "integer", "minimum": 1, "maximum": 2000}
            },
            "required": ["artifact_id"]
        }),
    }
}

pub(super) fn archive_extract_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "archive_extract".to_string(),
        description: "Extract selected safe entries from a zip/tar/tar.gz artifact into daemon-owned scratch, then publish bounded content-addressed artifacts with source provenance. Unsafe entries are rejected; nothing is written into the worktree.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "artifact_id": {"type": "string"},
                "entries": {"type": "array", "items": {"type": "string"}, "maxItems": 2000},
                "all": {"type": "boolean"},
                "max_entries": {"type": "integer", "minimum": 1, "maximum": 2000},
                "include_previews": {"type": "boolean"}
            },
            "required": ["artifact_id"]
        }),
    }
}

pub(super) fn media_info_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "media_info".to_string(),
        description: "Probe a media artifact or worktree file with ffprobe. Missing ffprobe returns MEDIA_TOOL_UNAVAILABLE; still images may fall back to explicitly labeled image metadata. No metadata is fabricated and no network is used.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "artifact_id": {"type": "string"},
                "path": {"type": "string", "description": "Worktree-relative media path when artifact_id is absent"}
            }
        }),
    }
}

pub(super) fn media_transcode_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "media_transcode".to_string(),
        description: "Explicitly transcode a file-backed media artifact with ffmpeg into a bounded, duration/byte-capped content-addressed artifact. Requires the approval broker and ffmpeg; never writes a workspace file.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "artifact_id": {"type": "string"},
                "format": {"type": "string", "enum": ["mp4", "webm", "mp3", "wav", "png", "jpeg"]},
                "max_duration_secs": {"type": "integer", "minimum": 1, "maximum": 600},
                "max_bytes": {"type": "integer", "minimum": 1048576, "maximum": 268435456}
            },
            "required": ["artifact_id", "format"]
        }),
    }
}

pub(super) fn recall_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "recall".to_string(),
        description: "Search durable project memory. Omit scope for the legacy project-memory store; scope=agent/department recalls this identity's scoped memory, and scope=project recalls project-wide org memory. Scope isolation is enforced.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "kind": {"type": "string", "description": "Legacy project-memory kind; ignored for identity memory"},
                "scope": {"type": "string", "enum": ["agent", "department", "project"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20}
            }
        }),
    }
}

pub(super) fn read_tool_output_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "read_tool_output".to_string(),
        description: "Page a full tool output that was spilled to the daemon-owned output store. Handles are opaque and only resolve inside this session. Offset is zero-based lines; pattern is a literal case-sensitive substring. The full transcript is retained; this tool returns a bounded window plus a continuation hint.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "handle": {"type": "string", "description": "Handle from an [output truncated] marker or an output_ref field"},
                "offset": {"type": "integer", "minimum": 0, "description": "Zero-based line offset in the filtered line set"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Maximum lines to return"},
                "max_bytes": {"type": "integer", "minimum": 256, "maximum": 65536, "description": "Maximum returned bytes; the daemon hard-caps this value"},
                "from_end": {"type": "boolean", "description": "Select from the end of the matching lines; output remains chronological"},
                "pattern": {"type": "string", "maxLength": 500, "description": "Literal case-sensitive substring filter"}
            },
            "required": ["handle"]
        }),
    }
}

pub(super) fn team_status_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "team_status".to_string(),
        description: "Read the current team board: agents by role, task progress, ready/blocked tasks, merge queue, and recent coordination notes. Read-only.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 100}
            }
        }),
    }
}

pub(super) fn tool_schemas(
    config: &AgentConfig,
    role: AgentRole,
    profile: AgentProfile,
    allow_delegation: bool,
) -> Vec<ToolSchema> {
    // Permissive compatibility path: every conditional surface stays visible,
    // so existing callers keep byte-identical schemas. A runtime that probes
    // MCP/remote/GUI/search/LSP/artifact availability calls
    // `tool_schemas_with_capabilities` with explicit flags.
    tool_schemas_with_capabilities(
        config,
        role,
        profile,
        allow_delegation,
        &crate::tool_bundle::ToolCapabilities::default(),
    )
}

/// Build the role policy schema set and drop conditional tools whose runtime
/// capability is absent.
///
/// Capability gating is advertisement-only and happens last, after role policy
/// and every special-tool branch, so a gated tool cannot reappear through a
/// different source. Dispatch policy, approvals, sandboxing, contracts, and
/// effect receipts remain authoritative; `tool_search` still knows the hidden
/// schema through the role policy surface.
pub(super) fn tool_schemas_with_capabilities(
    config: &AgentConfig,
    role: AgentRole,
    profile: AgentProfile,
    allow_delegation: bool,
    capabilities: &crate::tool_bundle::ToolCapabilities,
) -> Vec<ToolSchema> {
    let mut tools = config
        .tools
        .schemas()
        .into_iter()
        .filter(|tool| role.allows_tool(profile, &tool.name))
        .collect::<Vec<_>>();
    // Scouts receive a deliberately small, stable read-only bundle. Keeping
    // this branch before the special tools below makes writes and shell absent
    // from the schema instead of merely denied when called.
    if role == AgentRole::Scout {
        tools.extend(source_tool_schemas());
        tools.push(recall_tool_schema());
        tools.push(read_tool_output_tool_schema());
        tools.push(team_status_tool_schema());
        // Scout's policy allows metadata inspection but never extraction or
        // transcoding. Keep those two read-only schemas aligned with policy.
        if role.allows_tool(profile, "archive_list") {
            tools.push(archive_list_tool_schema());
        }
        if role.allows_tool(profile, "media_info") {
            tools.push(media_info_tool_schema());
        }
        retain_available_capabilities(&mut tools, capabilities);
        return tools;
    }
    tools.push(ToolSchema {
        name: "send_message".to_string(),
        description: "Send a typed message to another agent in this session. Do not address the human user; put the final answer in the normal assistant response.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "to": {"type": "string", "description": "Target agent id or unique name"},
                "kind": {"type": "string", "description": "e.g. question, handoff, result, review"},
                "body": {"type": "string"},
                "payload": {"type": "object"}
            },
            "required": ["to", "kind", "body"]
        }),
    });
    tools.push(ToolSchema {
        name: "read_mailbox".to_string(),
        description: "Read messages addressed to this agent. Marks returned unread messages read."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "unread_only": {"type": "boolean"}
            }
        }),
    });
    tools.push(ToolSchema {
        name: "publish_artifact".to_string(),
        description: "Publish a text artifact so other agents and the user can reference it."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "kind": {"type": "string"},
                "mime_type": {"type": "string"},
                "content": {"type": "string", "maxLength": 100000},
                "artifact_id": {"type": "string", "description": "Update this inline artifact instead of creating a new one"},
                "expected_version": {"type": "integer", "minimum": 1, "description": "Optimistic concurrency guard for updates"},
                "request_key": {"type": "string", "maxLength": 160, "description": "Idempotency key scoped to this session; a repeated key returns the first artifact"}
            },
            "required": ["name", "content"]
        }),
    });
    tools.push(ToolSchema {
        name: "list_artifacts".to_string(),
        description: "List artifacts published in this session.".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    });
    tools.push(ToolSchema {
        name: "read_artifact".to_string(),
        description: "Read one artifact by id.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"artifact_id": {"type": "string"}},
            "required": ["artifact_id"]
        }),
    });
    if role.allows_tool(profile, "archive_list") {
        tools.push(archive_list_tool_schema());
    }
    if role.allows_tool(profile, "archive_extract") {
        tools.push(archive_extract_tool_schema());
    }
    if role.allows_tool(profile, "media_info") {
        tools.push(media_info_tool_schema());
    }
    if role.allows_tool(profile, "media_transcode") {
        tools.push(media_transcode_tool_schema());
    }
    tools.extend(source_tool_schemas());
    tools.push(ToolSchema {
        name: "use_skill".to_string(),
        description: "Activate a project skill by its exact name. Returns the full instructions and any tool constraints; calling another skill replaces the active one.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "maxLength": 120}
            },
            "required": ["name"]
        }),
    });
    tools.push(recall_tool_schema());
    tools.push(read_tool_output_tool_schema());
    if role != AgentRole::Verifier {
        tools.push(ToolSchema {
            name: "remember".to_string(),
            description: "Record a durable memory. Omit scope for the legacy project-memory store; agent/department/project scope writes the project org memory table with evidence and lifecycle. Scope never switches silently: an unassigned identity fails closed.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string"},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "tags": {
                        "type": "array",
                        "maxItems": 10,
                        "items": {"type": "string"}
                    },
                    "scope": {"type": "string", "enum": ["agent", "department", "project"]},
                    "evidence": {
                        "type": "array",
                        "maxItems": 8,
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": {"type": "string", "enum": ["task", "session", "commit", "artifact", "event"]},
                                "reference": {"type": "string", "maxLength": 200},
                                "note": {"type": "string", "maxLength": 200}
                            },
                            "required": ["kind", "reference"]
                        }
                    },
                    "expires_at": {"type": "string", "description": "Optional RFC3339 expiry, bounded to ten years"},
                    "pinned": {"type": "boolean"}
                },
                "required": ["kind", "title", "body"]
            }),
        });
    }
    if role != AgentRole::Verifier {
        tools.push(ToolSchema {
            name: "ask_user".to_string(),
            description: "Ask the user a blocking question and wait for an answer. Use only when the task cannot proceed safely without a decision.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "maxLength": 2000},
                    "options": {
                        "type": "array",
                        "maxItems": 8,
                        "items": {"type": "string", "maxLength": 200}
                    }
                },
                "required": ["question"]
            }),
        });
    }
    if role != AgentRole::Verifier {
        tools.push(ToolSchema {
            name: "coordinate".to_string(),
            description: "Collaborate with the team: record a note, raise or clear a blocker, announce a handoff, escalate to the Leader, record a Manager decision, or list recent coordination notes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["note", "blocker", "unblock", "handoff", "escalate", "decision", "list"]
                    },
                    "task_id": {"type": "string", "maxLength": 40},
                    "target_agent_id": {"type": "string"},
                    "message": {"type": "string", "maxLength": 2000},
                    "severity": {
                        "type": "string",
                        "enum": ["info", "warning", "critical"]
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50}
                },
                "required": ["action"]
            }),
        });
    }
    tools.push(team_status_tool_schema());
    let mcp_actions = if matches!(role, AgentRole::Manager | AgentRole::Verifier) {
        serde_json::json!([
            "list",
            "resources",
            "read_resource",
            "prompts",
            "get_prompt",
            "task_list",
            "task_get",
            "task_result"
        ])
    } else {
        serde_json::json!([
            "list",
            "call",
            "resources",
            "read_resource",
            "prompts",
            "get_prompt",
            "task_list",
            "task_get",
            "task_result",
            "task_cancel"
        ])
    };
    tools.push(ToolSchema {
        name: "mcp".to_string(),
        description: "Inspect configured MCP servers and use their tools, resources, prompts, or asynchronous task handles. Tool executions require approval; Managers and Verifiers can inspect and read tasks but cannot execute or cancel. Async tasks are polled only when task_get/task_result is called explicitly.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": mcp_actions
                },
                "server": {"type": "string", "description": "MCP server id or name"},
                "tool": {"type": "string"},
                "arguments": {"type": "object", "description": "Tool arguments for action=call"},
                "uri": {"type": "string", "description": "Resource URI for action=read_resource"},
                "prompt": {"type": "string", "description": "Prompt name for action=get_prompt"},
                "prompt_arguments": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "String arguments for action=get_prompt"
                },
                "task_id": {"type": "string", "description": "Local MCP task id for task_get, task_result, or task_cancel"},
                "status": {"type": "string", "description": "task_list: optional status filter"},
                "refresh": {"type": "boolean", "description": "task_result: explicitly poll tasks/result once; default true. No hidden polling."},
                "reason": {"type": "string", "description": "task_cancel: optional bounded reason"}
            },
            "required": ["action"]
        }),
    });
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "todo_write".to_string(),
            description: "Replace the session plan/todo list. Use as work progresses.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "maxItems": 50,
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["title", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        });
    }
    if role.child_role().is_some() && allow_delegation {
        let child_role = role.child_role();
        let child_name = child_role.map(AgentRole::as_str).unwrap_or("worker");
        let description = match role {
            AgentRole::Worker => format!(
                "Delegate an implementation task to a {child_name}. Labour is the implementation worker: it writes code/tests, runs checks, commits, and reports back. You remain the technical lead and must review the result."
            ),
            AgentRole::Manager => format!(
                "Delegate a focused department objective to a {child_name}. Workers are technical leads: they design, decompose implementation to Labour, review it, and integrate."
            ),
            _ => format!(
                "Delegate a focused objective to a {child_name}. The child agent runs synchronously and returns a summary."
            ),
        };
        tools.push(ToolSchema {
            name: "delegate".to_string(),
            description,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "enum": [child_name]
                    },
                    "name": {"type": "string"},
                    "title": {
                        "type": "string",
                        "description": "Required for Managers: a distinct department role such as Discovery, Implementation, or Quality"
                    },
                    "objective": {"type": "string"},
                    "instructions": {"type": "string"},
                    "profile": {
                        "type": "string",
                        "enum": ["coding", "research", "verification", "generic"]
                    },
                    "write_scope": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"}
                    }
                },
                "required": ["role", "objective"]
            }),
        });
    }
    if matches!(role, AgentRole::Leader | AgentRole::Manager) && allow_delegation {
        tools.push(ToolSchema {
            name: "delegate_acp".to_string(),
            description: "Delegate a focused task to a configured external Agent Client Protocol (ACP) agent. A stdio agent runs in an isolated task worktree under the local write sandbox and its committed changes go through verification and merge; a remote agent is a trusted external service that is not sandboxed and does not write the local worktree. Remote delegation asks for host-state approval first.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Configured ACP agent id or name (see Settings > ACP)"
                    },
                    "name": {
                        "type": "string",
                        "description": "Optional Worker display name"
                    },
                    "objective": {"type": "string"},
                    "instructions": {"type": "string"},
                    "write_scope": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"}
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 30,
                        "description": "Optional timeout, capped by the configured agent timeout"
                    },
                    "session_id": {
                        "type": "string",
                        "maxLength": 256,
                        "description": "Optional existing agent session id to resume with session/load when the agent advertises loadSession"
                    }
                },
                "required": ["agent", "objective"]
            }),
        });
    }
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "submit_team_plan".to_string(),
            description: "Define the team hierarchy and task plan before delegating Managers. Departments select a Manager title and profile; tasks form a DAG.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "goal": {"type": "string"},
                    "departments": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 32,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "manager_title": {"type": "string"},
                                "profile": {"type": "string", "enum": ["coding", "research", "verification", "generic"]},
                                "objective": {"type": "string"},
                                "token_budget": {"type": "integer", "minimum": 1, "description": "Optional department token cap across all tasks"},
                                "cost_budget_usd": {"type": "number", "exclusiveMinimum": 0, "description": "Optional department USD cap across all tasks"}
                            },
                            "required": ["name", "manager_title", "profile", "objective"]
                        }
                    },
                    "tasks": {
                        "type": "array",
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "department": {"type": "string"},
                                "title": {"type": "string"},
                                "kind": {"type": "string"},
                                "priority": {"type": "integer", "minimum": -100, "maximum": 100, "description": "Higher priority runs first within a ready batch"},
                                "profile": {
                                    "type": "string",
                                    "enum": ["coding", "research", "verification", "generic"]
                                },
                                "deps": {"type": "array", "items": {"type": "string"}},
                                "write_scope": {"type": "array", "items": {"type": "string"}},
                                "acceptance": {"type": "array", "items": {"type": "string"}},
                                "token_budget": {"type": "integer", "minimum": 1, "description": "Optional task token cap"},
                                "cost_budget_usd": {"type": "number", "exclusiveMinimum": 0, "description": "Optional task USD cap"}
                            },
                            "required": ["id", "department", "title", "kind", "acceptance"]
                        }
                    }
                },
                "reason": {
                    "type": "string",
                    "maxLength": 1000,
                    "description": "Required when revising an existing plan; record why the team is changing course."
                },
                "required": ["goal", "departments", "tasks"]
            }),
        });
        tools.push(ToolSchema {
            name: "plan_fanout".to_string(),
            description: "Explicitly replace one pending template task with at most 20 disjoint child tasks and a shared join task in a new plan revision. This is destructive plan surgery and requires a HostState approval; it never runs automatically. Use items (or artifact_id containing a JSON array) with a unique key and a disjoint write_scope per child.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "template_task_id": {"type": "string", "description": "Pending template task id to replace"},
                    "items": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "properties": {
                                "key": {"type": "string", "maxLength": 64, "description": "Unique dedupe key for this child"},
                                "title": {"type": "string", "maxLength": 200},
                                "kind": {"type": "string"},
                                "profile": {"type": "string", "enum": ["coding", "research", "verification", "generic"]},
                                "priority": {"type": "integer", "minimum": -100, "maximum": 100},
                                "write_scope": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                                "acceptance": {"type": "array", "items": {"type": "string"}},
                                "token_budget": {"type": "integer", "minimum": 1},
                                "cost_budget_usd": {"type": "number", "exclusiveMinimum": 0}
                            },
                            "required": ["key", "write_scope"]
                        }
                    },
                    "artifact_id": {"type": "string", "description": "Optional text artifact containing a JSON array of items; mutually exclusive with items"},
                    "join_task_id": {"type": "string", "maxLength": 40},
                    "join_title": {"type": "string", "maxLength": 200},
                    "join_acceptance": {"type": "array", "items": {"type": "string"}},
                    "reason": {"type": "string", "maxLength": 1000},
                    "expected_revision": {"type": "integer", "minimum": 0, "description": "Optional optimistic plan revision guard"}
                },
                "required": ["template_task_id"]
            }),
        });
    }
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "run_team_plan".to_string(),
            description: "Execute the submitted TeamPlan automatically. Runs ready tasks in dependency batches, creates task workers and verifies their output.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        });
    }
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "deliberate_team_plan".to_string(),
            description: "Convene the planned Managers for a bounded roundtable. Each round shares peer positions; Managers cannot delegate or edit. Use before run_team_plan for multi-department plans.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "rounds": {"type": "integer", "minimum": 1, "maximum": 3}
                },
                "additionalProperties": false
            }),
        });
    }
    if role == AgentRole::Verifier {
        tools.push(ToolSchema {
            name: "submit_verdict".to_string(),
            description: "Submit the verification verdict. Approve only with evidence; do not edit files.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "approved": {"type": "boolean"},
                    "summary": {"type": "string", "description": "Evidence and decision, max 2000 chars"}
                },
                "required": ["approved", "summary"]
            }),
        });
    }
    if matches!(role, AgentRole::Leader | AgentRole::Manager) && allow_delegation {
        tools.push(ToolSchema {
            name: "scout".to_string(),
            description: "Run a read-only Scout in a fresh context: it explores the session worktree and returns bounded file:line evidence without writing, running shell, delegating, committing, or merging. Use it to map code or answer a focused question without polluting this conversation with raw file dumps. Not a coder."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "objective": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 4000,
                        "description": "The focused question or map to produce"
                    },
                    "files": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"},
                        "description": "Optional files or directories to start from"
                    },
                    "max_steps": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 12,
                        "description": "Maximum provider turns for the Scout; default 6"
                    }
                },
                "required": ["objective"],
                "additionalProperties": false
            }),
        });
    }
    if role.allows_tool(profile, "browser") {
        tools.push(ToolSchema {
            name: "browser".to_string(),
            description: "Drive a sandboxed local WKWebView: open localhost or worktree pages, inspect the accessibility/DOM tree, click/type/press/select, wait, read console logs, and save screenshots. Snapshot results include snapshot_generation; follow-up snapshots are delta-first (changed/new/removed refs plus a text delta) unless delta=false. Input actions that pass snapshot_generation are rejected with STALE_REF when the generation is old; take a fresh snapshot and use its refs. External navigation is refused. The browser and native computer helpers share one daemon-wide GUI lease; a second session waits briefly and then receives GUI_BUSY with the holder and retry hint. Use evaluate only when the user approves JavaScript execution."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["open", "snapshot", "click", "type", "press", "select", "wait_for", "screenshot", "console", "evaluate", "close"]
                    },
                    "url": {"type": "string", "description": "open: localhost HTTP(S) or a worktree file URL"},
                    "ref": {"type": "string", "description": "Element reference from snapshot, e.g. e12"},
                    "selector": {"type": "string", "description": "CSS selector alternative to ref"},
                    "text": {"type": "string", "description": "type: text to enter"},
                    "clear": {"type": "boolean", "description": "type: replace the current value; console: clear the bounded log buffer; default true for type"},
                    "key": {"type": "string", "description": "press: key such as Enter, Tab, Escape, ArrowDown"},
                    "value": {"type": "string", "description": "select: option value"},
                    "condition": {"type": "string", "enum": ["selector", "text", "url"]},
                    "timeout_ms": {"type": "integer", "minimum": 50, "maximum": 60000},
                    "max_chars": {"type": "integer", "minimum": 200, "maximum": 24000, "description": "snapshot: cap visible text"},
                    "max_nodes": {"type": "integer", "minimum": 1, "maximum": 1200},
                    "delta": {"type": "boolean", "description": "snapshot: return changed/new/removed refs and a text delta after the first full snapshot; default true"},
                    "snapshot_generation": {"type": "integer", "minimum": 1, "description": "Input actions: generation from the latest snapshot; a stale generation returns STALE_REF"},
                    "path": {"type": "string", "description": "screenshot: worktree-relative output path; defaults under .verlinpark/browser"},
                    "full_page": {"type": "boolean", "description": "screenshot: accepted for compatibility; v1 captures the viewport"},
                    "script": {"type": "string", "description": "evaluate: explicit approval-gated JavaScript"}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        });
    }
    if role.allows_tool(profile, "computer") {
        tools.push(ToolSchema {
            name: "computer".to_string(),
            description: "Drive native macOS applications through the daemon-owned Accessibility/CGEvent helper. Inspect permissions, apps, windows, and a bounded AX tree; focus; click/type/key/scroll/drag; wait; and capture a bounded screenshot artifact. Snapshot results include snapshot_generation; refs are bound to it. A ref from an older snapshot returns STALE_REF with the current generation and is never retried automatically: take a fresh snapshot. Every action shares the daemon-wide gui lease with the browser and is risk-classified. Sensitive targets such as System Settings, SecurityAgent, and password managers are denied or require explicit approval by project policy. Native GUI control is not sandboxed and requires Accessibility (and Screen Recording for screenshots) permission for VerlinParkComputerHelper. Prefer browser for web pages."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["permissions", "apps", "windows", "displays", "focus", "snapshot", "screenshot", "click", "type", "key", "scroll", "drag", "wait", "close"]
                    },
                    "bundle_id": {"type": "string", "description": "Target application bundle id; defaults to the frontmost app for snapshot/windows"},
                    "pid": {"type": "integer", "minimum": 1, "description": "Target process id alternative to bundle_id"},
                    "window_id": {"type": "integer", "minimum": 1, "description": "CGWindow id from windows"},
                    "window_title": {"type": "string", "description": "focus: substring of the window title"},
                    "ref": {"type": "string", "description": "Element reference from the latest snapshot, e.g. ax-00000007-0123456789abcdef; a ref from an older snapshot returns STALE_REF"},
                    "x": {"type": "number", "minimum": 0, "maximum": 1, "description": "click: normalized x within the target window"},
                    "y": {"type": "number", "minimum": 0, "maximum": 1, "description": "click: normalized y within the target window"},
                    "button": {"type": "string", "enum": ["left", "right"]},
                    "click_count": {"type": "integer", "minimum": 1, "maximum": 2},
                    "text": {"type": "string", "description": "type: text to enter (max 10 KiB)"},
                    "key": {"type": "string", "description": "key: whitelisted key such as Return, Tab, Escape, ArrowDown, q"},
                    "modifiers": {
                        "type": "array",
                        "maxItems": 4,
                        "items": {"type": "string", "enum": ["command", "shift", "option", "control", "cmd", "alt", "ctrl"]}
                    },
                    "dx": {"type": "integer", "minimum": -1000, "maximum": 1000, "description": "scroll: horizontal delta"},
                    "dy": {"type": "integer", "minimum": -1000, "maximum": 1000, "description": "scroll: vertical delta"},
                    "unit": {"type": "string", "enum": ["line", "pixel"]},
                    "from_x": {"type": "number", "minimum": 0, "maximum": 1},
                    "from_y": {"type": "number", "minimum": 0, "maximum": 1},
                    "to_x": {"type": "number", "minimum": 0, "maximum": 1},
                    "to_y": {"type": "number", "minimum": 0, "maximum": 1},
                    "duration_ms": {"type": "integer", "minimum": 0, "maximum": 2000},
                    "steps": {"type": "integer", "minimum": 1, "maximum": 60},
                    "condition": {"type": "string", "enum": ["ref", "element", "window", "app_frontmost"]},
                    "role": {"type": "string", "description": "wait element condition: AX role"},
                    "identifier": {"type": "string", "description": "wait element condition: AX identifier"},
                    "title": {"type": "string", "description": "wait element/window title"},
                    "timeout_ms": {"type": "integer", "minimum": 50, "maximum": 30000},
                    "interval_ms": {"type": "integer", "minimum": 50, "maximum": 1000},
                    "include_background": {"type": "boolean", "description": "apps: include accessory/background applications"},
                    "include_offscreen": {"type": "boolean", "description": "windows: include off-screen windows"},
                    "max_depth": {"type": "integer", "minimum": 1, "maximum": 20},
                    "max_elements": {"type": "integer", "minimum": 1, "maximum": 1200},
                    "max_bytes": {"type": "integer", "minimum": 1024, "maximum": 262144},
                    "max_chars": {"type": "integer", "minimum": 500, "maximum": 60000, "description": "snapshot: rendered transcript cap"},
                    "target": {"type": "string", "enum": ["window", "screen"], "description": "screenshot target; defaults to window"},
                    "display_index": {"type": "integer", "minimum": 0, "maximum": 15, "description": "screen screenshot: display index from computer displays"},
                    "display_id": {"type": "integer", "minimum": 1, "description": "screen screenshot: CGDirectDisplayID from computer displays"},
                    "max_width": {"type": "integer", "minimum": 320, "maximum": 2560},
                    "max_height": {"type": "integer", "minimum": 320, "maximum": 2560},
                    "name": {"type": "string", "description": "screenshot artifact display name"}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        });
    }
    tools.push(ToolSchema {
        name: "budget_status".to_string(),
        description: "Report remaining session, task, department, and project token/USD/time budgets, including whether unlimited mode is active."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    });
    tools.push(ToolSchema {
        name: "task_detail".to_string(),
        description: "Return the current or requested plan task: acceptance, dependencies, write scope, attempts, status, budgets, usage, and changed paths."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {"type": "string", "description": "Defaults to the agent's current task"}
            },
            "additionalProperties": false
        }),
    });
    if matches!(
        role,
        AgentRole::Leader | AgentRole::Manager | AgentRole::Worker
    ) && !(role == AgentRole::Manager && !allow_delegation)
    {
        tools.push(ToolSchema {
            name: "wait_for_gate".to_string(),
            description: "Wait for an existing external gate (CI, PR review, deploy, human, custom) without holding a Worker slot. This ends the current run and creates a durable continuation; the session resumes through the normal prompt path when the gate resolves, expires, or is cancelled. Never invent a gate id or ask for its token."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "gate_id": {"type": "string", "minLength": 1, "maxLength": 200},
                    "note": {"type": "string", "maxLength": 2000, "description": "Optional context for the resumed run"}
                },
                "required": ["gate_id"],
                "additionalProperties": false
            }),
        });
    }
    if matches!(role, AgentRole::Leader | AgentRole::Manager) {
        tools.push(ToolSchema {
            name: "task_control".to_string(),
            description: "Retry, skip, cancel, pause, or resume a task in this session's stored TeamPlan. Event-backed and audited."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["retry", "skip", "cancel", "pause", "resume"]},
                    "task_id": {"type": "string"}
                },
                "required": ["action", "task_id"],
                "additionalProperties": false
            }),
        });
    }
    tools.push(ToolSchema {
        name: "tool_help".to_string(),
        description: "Return a bounded, role-filtered recipe for using tools safely (read many files, symbol navigation, safe edit, tests, output paging, GUI verification, remote operations, memory). Read-only; does not execute or route any tool."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "recipe": {"type": "string", "description": "Optional recipe slug; empty lists visible recipes"},
                "error_code": {"type": "string", "description": "Optional stable error code to explain, such as PATCH_CONFLICT"},
                "include_error_codes": {"type": "boolean", "description": "Include the bounded stable error-code catalog; default false"},
                "max_chars": {"type": "integer", "minimum": 1000, "maximum": 14000, "description": "Response cap; default 14000"}
            },
            "additionalProperties": false
        }),
    });
    tools.push(ToolSchema {
        name: "tool_search".to_string(),
        description: "Search built-in tools and configured MCP tools by name or description. MCP results include the server/tool pair for the generic mcp call action."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Optional search text; empty lists available tools"},
                "task_kind": {"type": "string", "description": "Optional task kind used to boost the BM25 ranking"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Default 20"},
                "include_schema": {"type": "boolean", "description": "Include a bounded parameter schema preview; default false"},
                "promote": {"type": "boolean", "description": "Request that returned out-of-bundle builtins be promoted at the next turn boundary; the current request keeps its frozen schema set"}
            },
            "additionalProperties": false
        }),
    });
    if matches!(role, AgentRole::Leader | AgentRole::Manager) {
        tools.push(ToolSchema {
            name: "report_progress".to_string(),
            description: "Record one bounded advisory progress round for this session. Use it after a planning/review round to list what is satisfied, what is repeating in a loop, what is progressing, blockers, a suggested next task, and evidence pointers. This is evidence only: it never selects, skips, retries, cancels, replans, merges, or changes budgets/capacity. Round ids must follow a positive sequence (r1, r2, ...); omit round_id to let the runtime assign the next one."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "round_id": {"type": "string", "maxLength": 80, "description": "Optional sequence id such as r1; must increase by one"},
                    "satisfied": {"type": "array", "maxItems": 8, "items": {"type": "string", "maxLength": 200}},
                    "in_loop": {"type": "array", "maxItems": 8, "items": {"type": "string", "maxLength": 200}},
                    "progressing": {"type": "array", "maxItems": 8, "items": {"type": "string", "maxLength": 200}},
                    "blockers": {"type": "array", "maxItems": 8, "items": {"type": "string", "maxLength": 200}},
                    "next_task": {"type": "string", "maxLength": 1000},
                    "instruction": {"type": "string", "maxLength": 1000},
                    "evidence": {
                        "type": "array",
                        "maxItems": 8,
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": {"type": "string", "enum": ["task", "test", "commit", "artifact", "event", "note"]},
                                "reference": {"type": "string", "maxLength": 200},
                                "note": {"type": "string", "maxLength": 200}
                            },
                            "required": ["kind", "reference"],
                            "additionalProperties": false
                        }
                    }
                },
                "additionalProperties": false
            }),
        });
    }
    if matches!(
        role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    ) {
        tools.push(ToolSchema {
            name: "run_script".to_string(),
            description: "Compose the frozen read-only workspace tools in one bounded JavaScript sandbox. No ambient filesystem, network, shell, process, or native bindings exist; only the generated `tools.<name>(args)` functions call back through the normal contract, approval, journal, effect, and budget gates. Read-only by default; `allow_write` opts into separately approved file mutations with a write-call cap. Inner errors throw with `.code`; results return `{text,data,truncated,dataTruncated}`. Synchronous only: no async/await, timers, or modules."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string", "maxLength": 65536, "description": "JavaScript body; use `return` for the final JSON value"},
                    "input": {"type": ["object", "array"], "description": "Optional JSON input frozen and exposed as `input`"},
                    "allow_write": {"type": "boolean", "default": false, "description": "Bind write_file/edit_file/apply_patch; each inner call still needs normal approval and effect tracking. Default false."},
                    "max_calls": {"type": "integer", "minimum": 1, "maximum": 32, "description": "Pre-declared inner-call budget; default 16"},
                    "max_write_calls": {"type": "integer", "minimum": 1, "maximum": 8, "description": "Pre-declared inner write cap when allow_write is true; default 4"},
                    "timeout_ms": {"type": "integer", "minimum": 100, "maximum": 30000, "description": "CPU wall budget for the QuickJS thread; default 5000"},
                    "description": {"type": "string", "maxLength": 500, "description": "Optional short description for the transcript"}
                },
                "required": ["script"],
                "additionalProperties": false
            }),
        });
    }
    tools.push(ToolSchema {
        name: "publish_artifact_file".to_string(),
        description: "Publish an existing worktree file (screenshot, log, report, generated asset) as a session artifact. Max 5 MiB; content-addressed and retention-managed."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the worktree root"},
                "name": {"type": "string", "description": "Optional display name; defaults to the file name"},
                "kind": {"type": "string", "description": "Optional category such as image, log, report, diff"},
                "mime_type": {"type": "string", "description": "Optional MIME type; inferred from the extension when omitted"}
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    });
    if role == AgentRole::Manager && allow_delegation {
        tools.push(ToolSchema {
            name: "delegate_many".to_string(),
            description: "Run 1-5 Workers in parallel. Each Worker is a technical lead who decomposes implementation to Labour and reviews it. Each task must declare a non-overlapping write_scope when more than one task is provided.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 10,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "objective": {"type": "string"},
                                "instructions": {"type": "string"},
                                "profile": {
                                    "type": "string",
                                    "enum": ["coding", "research", "verification", "generic"]
                                },
                                "write_scope": {
                                    "type": "array",
                                    "maxItems": 20,
                                    "items": {"type": "string"}
                                }
                            },
                            "required": ["objective", "write_scope"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
        });
    }
    retain_available_capabilities(&mut tools, capabilities);
    tools
}

/// Drop conditional schemas whose capability was not advertised. Unconditional
/// schemas are untouched, and the relative order of the remaining tools is
/// preserved so the bundle freeze sees the same deterministic input order.
fn retain_available_capabilities(
    tools: &mut Vec<ToolSchema>,
    capabilities: &crate::tool_bundle::ToolCapabilities,
) {
    tools.retain(|tool| capabilities.allows_tool(&tool.name));
}
