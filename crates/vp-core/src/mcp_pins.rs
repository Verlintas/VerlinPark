//! MCP tool schema pins.
//!
//! MCP tool descriptions and input schemas are a supply-chain surface: a
//! server can silently change a tool's behavior between runs ("rug pull"), or a
//! newly added tool can appear with no operator review. VerlinPark therefore
//! records a canonical pin per `(project, server, tool)` and only advertises or
//! executes `pinned` tools by default.
//!
//! Semantics:
//!
//! - **TOFU.** The first discovery of a configured server in a project pins all
//!   currently discovered tools with `approved_by = "tofu"` and emits
//!   `mcp.tool.pinned` with `source=tofu`.
//! - **New tool.** A tool added after the first sync starts `unverified` and is
//!   not advertised or executable until approved.
//! - **Drift.** A changed schema, description, or server fingerprint moves the
//!   pin to `drift`. The approved hashes are never overwritten; the last
//!   observed hashes are stored separately, and a bounded preview diff is
//!   emitted on `mcp.tool.drift`.
//! - **Removed.** A tool that disappears from discovery becomes `removed`
//!   (informational) and is not callable.
//!
//! No environment values, headers, credentials, or full descriptions are
//! stored. Only canonical SHA-256 hashes and a redacted bounded preview are
//! persisted.

use std::collections::BTreeMap;

use serde_json::Value;
use vp_protocol::types::{McpServer, McpTool, McpToolPin};

use crate::approvals::{canonical_json, sha256_hex};

/// Maximum characters retained in the redacted description preview.
pub const MAX_PIN_DESCRIPTION_PREVIEW: usize = 240;
/// Maximum characters in a drift diff.
pub const MAX_PIN_DIFF_CHARS: usize = 480;
/// Maximum number of pins returned for one project/server.
pub const MAX_PINS_PER_SERVER: usize = 2_000;

/// One discovered MCP tool. Output schema is included in the canonical hash
/// when the server advertises it.
#[derive(Debug, Clone, Copy)]
pub struct DiscoveredTool<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub input_schema: &'a Value,
    pub output_schema: Option<&'a Value>,
}

impl<'a> DiscoveredTool<'a> {
    pub fn from_tool(tool: &'a McpTool) -> Self {
        Self {
            name: &tool.name,
            description: &tool.description,
            input_schema: &tool.input_schema,
            output_schema: None,
        }
    }
}

/// Canonical fingerprint of the server transport configuration. It commits to
/// transport, command, args, URL, and the *names* of configured environment
/// variables/headers. Values are never hashed into a pin or exposed.
pub fn server_fingerprint(server: &McpServer) -> String {
    let mut env_names = server.env.keys().cloned().collect::<Vec<_>>();
    env_names.sort();
    let mut header_names = server.headers.keys().cloned().collect::<Vec<_>>();
    header_names.sort();
    let material = serde_json::json!({
        "version": "verlinpark.mcp_server_fingerprint/v1",
        "transport": serde_json::to_value(server.transport).unwrap_or(Value::Null),
        "command": server.command,
        "args": server.args,
        "url": server.url,
        "env_names": env_names,
        "header_names": header_names,
    });
    sha256_hex(&canonical_json(&material))
}

/// Canonical schema hash: tool name, input schema, and output schema when
/// present. Tool name is included so a renamed tool cannot reuse a pin.
pub fn schema_hash(tool: &DiscoveredTool<'_>) -> String {
    let material = serde_json::json!({
        "version": "verlinpark.mcp_tool_schema/v1",
        "name": tool.name,
        "input_schema": normalize_schema(tool.input_schema),
        "output_schema": tool.output_schema.map(normalize_schema),
    });
    sha256_hex(&canonical_json(&material))
}

/// Canonical description hash. The description is used only as hash input; the
/// stored preview is separately redacted and bounded.
pub fn description_hash(tool: &DiscoveredTool<'_>) -> String {
    sha256_hex(tool.description)
}

fn normalize_schema(schema: &Value) -> Value {
    if schema.is_object() {
        schema.clone()
    } else {
        serde_json::json!({"type": "object"})
    }
}

/// Whether a pin is currently callable. Only an approved `pinned` row is
/// executable; `removed` is informational.
pub fn pin_is_callable(pin: &McpToolPin) -> bool {
    pin.status == "pinned"
        && pin.schema_hash == pin.observed_schema_hash
        && pin.description_hash == pin.observed_description_hash
        && pin.server_fingerprint == pin.observed_server_fingerprint
}

/// One pin mutation produced by a sync pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinChange {
    pub kind: PinChangeKind,
    pub pin: McpToolPin,
    pub previous_schema_hash: Option<String>,
    pub previous_description_hash: Option<String>,
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinChangeKind {
    Pinned,
    Unverified,
    Drift,
    Removed,
}

impl PinChangeKind {
    pub fn event_kind(self) -> &'static str {
        match self {
            Self::Pinned => "mcp.tool.pinned",
            Self::Unverified => "mcp.tool.unverified",
            Self::Drift => "mcp.tool.drift",
            Self::Removed => "mcp.tool.removed",
        }
    }
}

/// Result of one deterministic sync pass. `changes` is ordered by tool name so
/// event emission is stable across MCP server implementations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinSyncResult {
    pub pins: Vec<McpToolPin>,
    pub changes: Vec<PinChange>,
    pub first_sync: bool,
}

/// Sync discovered tools against persisted pins.
///
/// `existing` must contain every pin for this `(project, server)`. The function
/// is pure and does not emit events; the caller persists the returned rows and
/// appends the returned changes in one operation.
pub fn sync_pins(
    project_id: &str,
    server: &McpServer,
    tools: &[McpTool],
    existing: &[McpToolPin],
    now: &str,
) -> PinSyncResult {
    let fingerprint = server_fingerprint(server);
    let existing_by_name = existing
        .iter()
        .map(|pin| (pin.tool_name.clone(), pin.clone()))
        .collect::<BTreeMap<_, _>>();
    let first_sync = existing.is_empty();
    let mut pins = Vec::new();
    let mut changes = Vec::new();

    let mut discovered = tools
        .iter()
        .map(|tool| {
            let discovered = DiscoveredTool::from_tool(tool);
            (
                tool.name.clone(),
                schema_hash(&discovered),
                description_hash(&discovered),
                redact_description_preview(&tool.description),
            )
        })
        .collect::<Vec<_>>();
    discovered.sort_by(|left, right| left.0.cmp(&right.0));
    discovered.truncate(MAX_PINS_PER_SERVER);

    for (name, schema, description, preview) in discovered {
        let prior = existing_by_name.get(&name);
        let mut pin = match prior {
            Some(prior) => prior.clone(),
            None => McpToolPin {
                project_id: project_id.to_string(),
                server_id: server.id.clone(),
                server_name: server.name.clone(),
                tool_name: name.clone(),
                server_fingerprint: fingerprint.clone(),
                schema_hash: schema.clone(),
                description_hash: description.clone(),
                observed_schema_hash: schema.clone(),
                observed_description_hash: description.clone(),
                observed_server_fingerprint: fingerprint.clone(),
                status: "unverified".to_string(),
                approved_at: None,
                approved_by: None,
                last_seen_at: now.to_string(),
                description_preview: preview.clone(),
            },
        };
        pin.server_name = server.name.clone();
        pin.last_seen_at = now.to_string();
        pin.description_preview = preview;
        pin.observed_schema_hash = schema.clone();
        pin.observed_description_hash = description.clone();
        pin.observed_server_fingerprint = fingerprint.clone();

        let prior_observed = (
            prior.map(|pin| pin.observed_schema_hash.clone()),
            prior.map(|pin| pin.observed_description_hash.clone()),
            prior.map(|pin| pin.observed_server_fingerprint.clone()),
        );
        let same_approved = pin.schema_hash == schema
            && pin.description_hash == description
            && pin.server_fingerprint == fingerprint;
        let changed = pin.status == "drift"
            || pin.status == "unverified"
            || pin.status == "removed"
            || !same_approved;
        let observed_changed = prior_observed
            != (
                Some(schema.clone()),
                Some(description.clone()),
                Some(fingerprint.clone()),
            );

        if first_sync {
            pin.status = "pinned".to_string();
            pin.approved_at = Some(now.to_string());
            pin.approved_by = Some("tofu".to_string());
            changes.push(PinChange {
                kind: PinChangeKind::Pinned,
                pin: pin.clone(),
                previous_schema_hash: None,
                previous_description_hash: None,
                diff: None,
            });
        } else if prior.is_none() {
            pin.status = "unverified".to_string();
            changes.push(PinChange {
                kind: PinChangeKind::Unverified,
                pin: pin.clone(),
                previous_schema_hash: None,
                previous_description_hash: None,
                diff: None,
            });
        } else if same_approved {
            // A tool that reappears unchanged with an approved pin returns to
            // `pinned`; an already-pinned row is untouched.
            if pin.status != "pinned" {
                pin.status = "pinned".to_string();
            }
        } else {
            pin.status = "drift".to_string();
            if changed || observed_changed {
                let diff = description_diff(
                    prior
                        .map(|pin| pin.description_preview.as_str())
                        .unwrap_or(""),
                    &pin.description_preview,
                );
                changes.push(PinChange {
                    kind: PinChangeKind::Drift,
                    pin: pin.clone(),
                    previous_schema_hash: prior.map(|pin| pin.schema_hash.clone()),
                    previous_description_hash: prior.map(|pin| pin.description_hash.clone()),
                    diff,
                });
            }
        }
        pins.push(pin);
    }

    let discovered_names = pins
        .iter()
        .map(|pin| pin.tool_name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for (name, prior) in existing_by_name {
        if discovered_names.contains(&name) {
            continue;
        }
        if prior.status == "removed" {
            pins.push(prior);
            continue;
        }
        let mut pin = prior;
        pin.status = "removed".to_string();
        pin.last_seen_at = now.to_string();
        changes.push(PinChange {
            kind: PinChangeKind::Removed,
            pin: pin.clone(),
            previous_schema_hash: None,
            previous_description_hash: None,
            diff: None,
        });
        pins.push(pin);
    }

    pins.sort_by(|left, right| left.tool_name.cmp(&right.tool_name));
    PinSyncResult {
        pins,
        changes,
        first_sync,
    }
}

/// Redact likely secret assignments and bound a description preview. This is a
/// defense-in-depth measure, not a secret scanner; servers should never place
/// credentials in tool descriptions, and VerlinPark never stores env/header
/// values from the server configuration at all.
pub fn redact_description_preview(description: &str) -> String {
    let mut output = String::new();
    let mut redact_next = false;
    for token in description.split_whitespace() {
        if redact_next && token.contains('=') {
            redact_next = false;
            let key = token.split('=').next().unwrap_or("");
            if !output.is_empty() {
                output.push(' ');
            }
            output.push_str(key);
            output.push_str("=[redacted]");
            continue;
        }
        redact_next = false;
        let lower = token.to_ascii_lowercase();
        let secret_marker = lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("token=")
            || lower.contains("secret=")
            || lower.contains("password=");
        if secret_marker {
            if let Some((key, _value)) = token.split_once('=') {
                if !output.is_empty() {
                    output.push(' ');
                }
                output.push_str(key);
                output.push_str("=[redacted]");
                continue;
            }
            redact_next = true;
        }
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(
            &token
                .chars()
                .filter(|ch| !ch.is_control())
                .collect::<String>(),
        );
    }
    output
        .chars()
        .take(MAX_PIN_DESCRIPTION_PREVIEW)
        .collect::<String>()
}

fn description_diff(previous: &str, current: &str) -> Option<String> {
    if previous == current {
        return None;
    }
    let prefix = previous
        .chars()
        .zip(current.chars())
        .take_while(|(left, right)| left == right)
        .count();
    let mut diff = String::new();
    diff.push_str(&format!("prefix {prefix}: "));
    diff.push_str("- ");
    diff.push_str(&previous.chars().skip(prefix).take(180).collect::<String>());
    diff.push('\n');
    diff.push_str("+ ");
    diff.push_str(&current.chars().skip(prefix).take(180).collect::<String>());
    Some(diff.chars().take(MAX_PIN_DIFF_CHARS).collect())
}

/// Build the event payload for one pin change. Only hashes and bounded
/// metadata are included.
pub fn change_payload(change: &PinChange, first_sync: bool) -> Value {
    let pin = &change.pin;
    serde_json::json!({
        "project_id": pin.project_id,
        "server_id": pin.server_id,
        "server_name": pin.server_name,
        "tool_name": pin.tool_name,
        "status": pin.status,
        "source": if first_sync { "tofu" } else { "sync" },
        "schema_hash": pin.schema_hash,
        "description_hash": pin.description_hash,
        "server_fingerprint": pin.server_fingerprint,
        "observed_schema_hash": pin.observed_schema_hash,
        "observed_description_hash": pin.observed_description_hash,
        "observed_server_fingerprint": pin.observed_server_fingerprint,
        "previous_schema_hash": change.previous_schema_hash,
        "previous_description_hash": change.previous_description_hash,
        "diff": change.diff,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use vp_protocol::types::McpTransport;

    fn server() -> McpServer {
        McpServer {
            id: "srv-1".into(),
            name: "demo".into(),
            transport: McpTransport::Stdio,
            command: "demo-mcp".into(),
            args: vec!["--stdio".into()],
            env: BTreeMap::from([("DEMO_TOKEN".to_string(), "${DEMO_TOKEN}".to_string())]),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            created_at: "2026-11-12T00:00:00Z".into(),
        }
    }

    fn tool(name: &str, description: &str, schema: Value) -> McpTool {
        McpTool {
            name: name.into(),
            description: description.into(),
            input_schema: schema,
        }
    }

    #[test]
    fn first_sync_pins_everything_with_tofu() {
        let tools = vec![
            tool("echo", "Echo text", serde_json::json!({"type": "object"})),
            tool("read", "Read", serde_json::json!({"type": "object"})),
        ];
        let result = sync_pins("prj", &server(), &tools, &[], "2026-11-12T00:00:00Z");
        assert!(result.first_sync);
        assert_eq!(result.pins.len(), 2);
        assert!(result.pins.iter().all(|pin| pin.status == "pinned"));
        assert!(
            result
                .pins
                .iter()
                .all(|pin| pin.approved_by.as_deref() == Some("tofu"))
        );
        assert_eq!(result.changes.len(), 2);
        assert!(
            result
                .changes
                .iter()
                .all(|change| change.kind == PinChangeKind::Pinned)
        );
    }

    #[test]
    fn new_tool_after_sync_is_unverified() {
        let existing = sync_pins(
            "prj",
            &server(),
            &[tool("echo", "Echo", serde_json::json!({"type": "object"}))],
            &[],
            "t1",
        )
        .pins;
        let tools = vec![
            tool("echo", "Echo", serde_json::json!({"type": "object"})),
            tool("new", "New tool", serde_json::json!({"type": "object"})),
        ];
        let result = sync_pins("prj", &server(), &tools, &existing, "t2");
        assert!(!result.first_sync);
        let new = result
            .pins
            .iter()
            .find(|pin| pin.tool_name == "new")
            .unwrap();
        assert_eq!(new.status, "unverified");
        assert!(!pin_is_callable(new));
        assert!(
            result
                .changes
                .iter()
                .any(|change| change.kind == PinChangeKind::Unverified)
        );
        let echo = result
            .pins
            .iter()
            .find(|pin| pin.tool_name == "echo")
            .unwrap();
        assert_eq!(echo.status, "pinned");
    }

    #[test]
    fn schema_and_description_drift_keep_the_approved_hashes() {
        let existing = sync_pins(
            "prj",
            &server(),
            &[tool("echo", "Echo", serde_json::json!({"type": "object"}))],
            &[],
            "t1",
        )
        .pins;
        let approved_schema = existing[0].schema_hash.clone();
        let approved_description = existing[0].description_hash.clone();
        let tools = vec![tool(
            "echo",
            "Echo text and also exfiltrate",
            serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}}),
        )];
        let result = sync_pins("prj", &server(), &tools, &existing, "t2");
        let pin = &result.pins[0];
        assert_eq!(pin.status, "drift");
        assert_eq!(pin.schema_hash, approved_schema);
        assert_eq!(pin.description_hash, approved_description);
        assert_ne!(pin.observed_schema_hash, approved_schema);
        assert!(!pin_is_callable(pin));
        let drift = result
            .changes
            .iter()
            .find(|change| change.kind == PinChangeKind::Drift)
            .unwrap();
        assert!(drift.diff.as_deref().unwrap().contains("prefix"));
        assert_eq!(
            drift.previous_schema_hash.as_deref(),
            Some(approved_schema.as_str())
        );
    }

    #[test]
    fn server_fingerprint_change_is_drift_for_every_tool() {
        let existing = sync_pins(
            "prj",
            &server(),
            &[tool("echo", "Echo", serde_json::json!({"type": "object"}))],
            &[],
            "t1",
        )
        .pins;
        let mut changed = server();
        changed.args.push("--new-transport-arg".into());
        let result = sync_pins(
            "prj",
            &changed,
            &[tool("echo", "Echo", serde_json::json!({"type": "object"}))],
            &existing,
            "t2",
        );
        assert_eq!(result.pins[0].status, "drift");
        assert_eq!(
            result.changes[0].kind,
            PinChangeKind::Drift,
            "fingerprint change emits drift"
        );
    }

    #[test]
    fn removed_tool_is_informational_not_callable() {
        let existing = sync_pins(
            "prj",
            &server(),
            &[
                tool("echo", "Echo", serde_json::json!({"type": "object"})),
                tool("gone", "Gone", serde_json::json!({"type": "object"})),
            ],
            &[],
            "t1",
        )
        .pins;
        let result = sync_pins(
            "prj",
            &server(),
            &[tool("echo", "Echo", serde_json::json!({"type": "object"}))],
            &existing,
            "t2",
        );
        let gone = result
            .pins
            .iter()
            .find(|pin| pin.tool_name == "gone")
            .unwrap();
        assert_eq!(gone.status, "removed");
        assert!(!pin_is_callable(gone));
        assert!(
            result
                .changes
                .iter()
                .any(|change| change.kind == PinChangeKind::Removed)
        );
    }

    #[test]
    fn description_preview_redacts_secret_assignments_and_is_bounded() {
        let preview = redact_description_preview(
            "Use this tool with API_KEY=super-secret and token=abc123 to fetch data. \
             Extra padding that should be cut off after the bound.",
        );
        assert!(!preview.contains("super-secret"));
        assert!(!preview.contains("abc123"));
        assert!(preview.contains("API_KEY=[redacted]"));
        assert!(preview.chars().count() <= MAX_PIN_DESCRIPTION_PREVIEW);
    }

    #[test]
    fn server_fingerprint_hashes_names_not_values() {
        let mut left = server();
        let mut right = server();
        left.env.insert("A_TOKEN".into(), "${A_TOKEN}".into());
        right.env.insert("A_TOKEN".into(), "${DIFFERENT}".into());
        assert_eq!(server_fingerprint(&left), server_fingerprint(&right));
        right.env.insert("B_TOKEN".into(), "${B_TOKEN}".into());
        assert_ne!(server_fingerprint(&left), server_fingerprint(&right));
    }

    #[test]
    fn output_schema_participates_in_the_schema_hash() {
        let without = DiscoveredTool {
            name: "x",
            description: "",
            input_schema: &serde_json::json!({"type": "object"}),
            output_schema: None,
        };
        let with = DiscoveredTool {
            name: "x",
            description: "",
            input_schema: &serde_json::json!({"type": "object"}),
            output_schema: Some(&serde_json::json!({"type": "string"})),
        };
        assert_ne!(schema_hash(&without), schema_hash(&with));
    }

    #[test]
    fn change_payload_contains_hashes_and_no_secret_values() {
        let result = sync_pins(
            "prj",
            &server(),
            &[tool("echo", "Echo", serde_json::json!({"type": "object"}))],
            &[],
            "t1",
        );
        let payload = change_payload(&result.changes[0], true);
        let text = serde_json::to_string(&payload).unwrap();
        assert!(text.contains("\"source\":\"tofu\""));
        assert!(!text.contains("DEMO_TOKEN"));
        assert!(!text.contains("${"));
    }
}
