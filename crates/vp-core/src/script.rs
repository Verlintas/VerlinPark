//! Code Mode: a bounded JavaScript composition surface over frozen tools.
//!
//! The runner is deliberately capability-minimal. QuickJS starts with no host
//! bindings except one opaque dispatch function, no module loader, no
//! filesystem, network, process, shell, timer, or native bridge. The only way
//! for a script to affect anything is to call a tool binding generated from the
//! current advertised tool set. Every such call is routed back to the agent
//! dispatcher, which independently re-runs role policy, task contracts,
//! approvals, effect receipts, journaling, and `tool.finished` events.
//!
//! Static validation is defense-in-depth, not the sandbox boundary: `oxc`
//! rejects syntax errors and forbidden ambient identifiers, while the native
//! bridge rejects any name that is not in the bind time allowlist. Even a
//! script that reaches `eval` or constructs `Function` cannot name a tool that
//! was not bound, and cannot perform I/O on its own.
//!
//! Budgets are process-local: 64 MiB QuickJS heap, a fixed stack, a wall-clock
//! CPU deadline enforced by QuickJS's interrupt handler, and a pre-declared
//! inner-call cap. Write bindings are opt-in and separately capped so a script
//! cannot turn one approval into an unbounded edit loop.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use oxc_allocator::Allocator;
use oxc_ast::ast::{IdentifierReference, ImportExpression};
use oxc_ast_visit::{Visit, walk};
use oxc_diagnostics::Severity;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use rquickjs::{Context as JsContext, Exception, Function as JsFunction, Runtime as JsRuntime};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use vp_providers::ToolSchema;

use crate::tools::ToolOutput;

/// Hard cap on user script source. Scripts are glue, not programs: anything
/// larger is an application and belongs in a file edited by normal tools.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;
/// Hard cap on the optional JSON input passed into a script.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
/// Hard cap on one inner tool's arguments.
pub const MAX_CALL_ARGUMENT_BYTES: usize = 64 * 1024;
/// Hard cap on the final returned JSON value.
pub const MAX_RESULT_BYTES: usize = 256 * 1024;
/// Hard cap on captured stdout.
pub const MAX_STDOUT_BYTES: usize = 64 * 1024;
/// Hard cap on the structured value returned to one inner call.
pub const MAX_CALL_VALUE_BYTES: usize = 32 * 1024;
const MAX_CALL_TEXT_BYTES: usize = 128 * 1024;
const DEFAULT_CALLS: u32 = 16;
const MAX_CALLS: u32 = 32;
const DEFAULT_WRITE_CALLS: u32 = 4;
const MAX_WRITE_CALLS: u32 = 8;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 30_000;
const HEAP_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const STACK_LIMIT_BYTES: usize = 512 * 1024;
const MAX_SYNTAX_DEPTH: usize = 512;

/// Local, read-only tools that a script may compose. Every name must be a
/// registry tool because inner dispatch executes `AgentTool::execute`; tools
/// that need bespoke agent-loop plumbing (mailbox, MCP, delegation, artifacts)
/// are intentionally absent.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "grep",
    "find_files",
    "file_info",
    "read_files",
    "git",
    "repo_map",
    "find_symbol",
    "find_references",
    "dependency_graph",
    "build_context",
    "affected_tests",
    "read_pdf",
    "read_image",
    "read_document",
    "data_query",
];

/// File mutation tools available only when the caller sets `allow_write` and
/// accepts the outer host-state approval. Destructive management tools
/// (`file_manage`), shell, process, network, remote, GUI, MCP, and delegation
/// are never bound.
pub const WRITE_TOOLS: &[&str] = &["write_file", "edit_file", "apply_patch"];

/// Ambient names that are rejected at parse time. The native capability model
/// already prevents these from reaching host I/O; this list keeps scripts
/// deterministic and catches accidental Node/browser idioms early.
pub const FORBIDDEN_GLOBALS: &[&str] = &[
    "eval",
    "Function",
    "globalThis",
    "window",
    "self",
    "process",
    "require",
    "module",
    "exports",
    "Deno",
    "Bun",
    "XMLHttpRequest",
    "fetch",
    "WebSocket",
    "Worker",
    "SharedWorker",
    "importScripts",
    "WebAssembly",
    "Atomics",
    "SharedArrayBuffer",
    "os",
    "std",
    "scriptArgs",
    "stdin",
    "stdout",
    "stderr",
];

/// One tool binding visible to a script.
#[derive(Debug, Clone)]
pub struct ScriptBinding {
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub read_only: bool,
}

/// Budgets for one script run. Values are clamped by `ScriptLimits::from_input`.
#[derive(Debug, Clone)]
pub struct ScriptLimits {
    pub wall_ms: u64,
    pub max_calls: u32,
    pub max_write_calls: u32,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            wall_ms: DEFAULT_TIMEOUT_MS,
            max_calls: DEFAULT_CALLS,
            max_write_calls: DEFAULT_WRITE_CALLS,
        }
    }
}

impl ScriptLimits {
    pub fn from_input(input: &Value) -> Self {
        let wall_ms = input
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
        let max_calls = input
            .get("max_calls")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(DEFAULT_CALLS))
            .clamp(1, u64::from(MAX_CALLS)) as u32;
        let max_write_calls = input
            .get("max_write_calls")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(DEFAULT_WRITE_CALLS))
            .clamp(1, u64::from(MAX_WRITE_CALLS)) as u32;
        Self {
            wall_ms,
            max_calls,
            max_write_calls,
        }
    }
}

/// Bindings available from the advertised tool list. A tool that is not in the
/// current frozen bundle is not bound, even if it is in the role policy; the
/// agent can use `tool_search` promotion in a later turn to make it visible.
pub fn bindings(advertised: &[ToolSchema], allow_write: bool) -> BTreeMap<String, ScriptBinding> {
    let allowed = READ_ONLY_TOOLS
        .iter()
        .copied()
        .chain(
            allow_write
                .then_some(WRITE_TOOLS)
                .into_iter()
                .flatten()
                .copied(),
        )
        .collect::<BTreeSet<_>>();
    advertised
        .iter()
        .filter(|tool| allowed.contains(tool.name.as_str()))
        .map(|tool| {
            (
                tool.name.clone(),
                ScriptBinding {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    schema: canonical_schema(&tool.parameters),
                    read_only: READ_ONLY_TOOLS.contains(&tool.name.as_str()),
                },
            )
        })
        .collect()
}

/// Map a runner error string to the stable tool error catalog. The message is
/// produced only by this module, so substring matching is deterministic.
pub fn classify_error(message: &str) -> crate::tooling::errors::ToolErrorCode {
    use crate::tooling::errors::ToolErrorCode;
    let lower = message.to_ascii_lowercase();
    if lower.contains("cpu budget") || lower.contains("timed out") {
        ToolErrorCode::SCRIPT_TIMEOUT
    } else if lower.contains("heap limit") || lower.contains("out of memory") {
        ToolErrorCode::SCRIPT_MEMORY_LIMIT
    } else if lower.contains("tool-call budget") {
        ToolErrorCode::SCRIPT_CALL_BUDGET
    } else if lower.contains("write-call budget") {
        ToolErrorCode::SCRIPT_WRITE_BUDGET
    } else if lower.contains("return value exceeds") || lower.contains("stdout") {
        ToolErrorCode::SCRIPT_OUTPUT_LIMIT
    } else if lower.contains("cancelled") {
        ToolErrorCode::SCRIPT_CANCELLED
    } else if lower.contains("forbidden ambient")
        || lower.contains("syntax error")
        || lower.contains("exceeds 64 kib")
        || lower.contains("nesting exceeds")
    {
        ToolErrorCode::SCRIPT_INVALID
    } else {
        ToolErrorCode::SCRIPT_FAILED
    }
}

fn canonical_schema(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical_schema(&object[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_schema).collect()),
        other => other.clone(),
    }
}

/// One inner call request sent from the QuickJS thread to the async dispatcher.
pub struct ScriptCallRequest {
    pub seq: u32,
    pub tool: String,
    pub arguments: Value,
    pub read_only: bool,
    pub reply: oneshot::Sender<ScriptReply>,
}

/// Result of one inner call. `abort` tells the script runner to stop the script
/// immediately (user cancellation), rather than raising a catchable tool error.
pub struct ScriptReply {
    pub output: ToolOutput,
    pub aborted: bool,
}

/// Final script outcome. Call records are assembled by the dispatcher because
/// it owns approval/effect metadata.
#[derive(Debug, Clone)]
pub struct ScriptResult {
    pub value: Option<Value>,
    pub stdout: String,
    pub stdout_truncated: bool,
    pub elapsed_ms: u64,
    pub host_calls: u32,
    pub write_calls: u32,
}

impl ScriptResult {
    pub fn summary(&self) -> String {
        let mut text = format!(
            "Script completed in {} ms with {} tool call(s){}.",
            self.elapsed_ms,
            self.host_calls,
            if self.write_calls > 0 {
                format!(", including {} write(s)", self.write_calls)
            } else {
                String::new()
            }
        );
        if self.stdout_truncated {
            text.push_str(" stdout was truncated.");
        }
        text
    }
}

pub enum ScriptEvent {
    Finished(Result<ScriptResult>),
    Call(ScriptCallRequest),
}

/// Owns the blocking QuickJS thread and its request channel. Dropping it sets
/// the cancellation flag and closes the reply path so a cancelled agent turn
/// does not leak a runner.
pub struct ScriptRun {
    handle: Option<tokio::task::JoinHandle<Result<ScriptResult>>>,
    requests: mpsc::UnboundedReceiver<ScriptCallRequest>,
    cancel: Arc<AtomicBool>,
}

impl ScriptRun {
    pub async fn next_event(&mut self) -> Option<ScriptEvent> {
        let mut handle = self.handle.take()?;
        tokio::select! {
            joined = &mut handle => {
                let result = match joined {
                    Ok(result) => result,
                    Err(error) => Err(anyhow!("script thread failed: {error}")),
                };
                Some(ScriptEvent::Finished(result))
            }
            maybe = self.requests.recv() => {
                if let Some(request) = maybe {
                    self.handle = Some(handle);
                    return Some(ScriptEvent::Call(request));
                }
                // The sender is dropped only after the blocking runner exits.
                // Wait for the join result without spinning on a closed queue.
                let joined = handle.await;
                let result = match joined {
                    Ok(result) => result,
                    Err(error) => Err(anyhow!("script thread failed: {error}")),
                };
                Some(ScriptEvent::Finished(result))
            }
        }
    }
}

impl Drop for ScriptRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Validate and start a script. Static errors are returned before a QuickJS
/// runtime is created.
pub fn spawn(
    source: &str,
    input: Value,
    bindings: BTreeMap<String, ScriptBinding>,
    limits: ScriptLimits,
) -> Result<ScriptRun> {
    validate_source(source)?;
    validate_input(&input)?;
    let advertised = bindings
        .values()
        .map(|binding| binding.name.clone())
        .collect::<Vec<_>>();
    if advertised.is_empty() {
        bail!(
            "no script-safe tools are available in this frozen bundle; use the normal tool surface"
        );
    }
    let (request_tx, requests) = mpsc::unbounded_channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_thread = Arc::clone(&cancel);
    let source = source.to_string();
    let limits_thread = limits.clone();
    let bindings_thread = bindings.clone();
    let handle = tokio::task::spawn_blocking(move || {
        run_sync(
            &source,
            input,
            bindings_thread,
            limits_thread,
            request_tx,
            cancel_thread,
        )
    });
    Ok(ScriptRun {
        handle: Some(handle),
        requests,
        cancel,
    })
}

fn validate_input(input: &Value) -> Result<()> {
    if input.is_null() {
        return Ok(());
    }
    let encoded = serde_json::to_vec(input)?;
    if encoded.len() > MAX_INPUT_BYTES {
        bail!("script input exceeds {} KiB", MAX_INPUT_BYTES / 1024);
    }
    if !input.is_object() && !input.is_array() {
        bail!("script input must be a JSON object or array");
    }
    Ok(())
}

/// Parse and statically check JavaScript. `oxc` is used only for syntax and
/// ambient-identifier checks; execution capability comes from the bridge.
pub fn validate_source(source: &str) -> Result<()> {
    if source.trim().is_empty() {
        bail!("script is empty");
    }
    if source.len() > MAX_SOURCE_BYTES {
        bail!("script exceeds {} KiB", MAX_SOURCE_BYTES / 1024);
    }
    if syntax_depth_exceeded(source, MAX_SYNTAX_DEPTH) {
        bail!("script nesting exceeds {MAX_SYNTAX_DEPTH} levels");
    }
    let allocator = Allocator::default();
    let options = ParseOptions {
        allow_return_outside_function: true,
        ..ParseOptions::default()
    };
    let parsed = Parser::new(&allocator, source, SourceType::script())
        .with_options(options)
        .parse();
    let errors = parsed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.to_string())
        .take(3)
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        bail!("JavaScript syntax error: {}", errors.join("; "));
    }
    let mut visitor = ForbiddenVisitor::default();
    visitor.visit_program(&parsed.program);
    if let Some(name) = visitor.forbidden.into_iter().next() {
        bail!(
            "forbidden ambient reference `{name}`; scripts may only call the frozen `tools` bindings"
        );
    }
    Ok(())
}

#[derive(Default)]
struct ForbiddenVisitor {
    forbidden: BTreeSet<String>,
}

impl<'a> Visit<'a> for ForbiddenVisitor {
    fn visit_identifier_reference(&mut self, reference: &IdentifierReference<'a>) {
        if FORBIDDEN_GLOBALS.contains(&reference.name.as_str()) {
            self.forbidden.insert(reference.name.to_string());
        }
        walk::walk_identifier_reference(self, reference);
    }

    fn visit_import_expression(&mut self, expression: &ImportExpression<'a>) {
        self.forbidden.insert("import()".to_string());
        walk::walk_import_expression(self, expression);
    }
}

/// Conservative lexical nesting check before handing source to a recursive
/// parser. It skips quoted strings and comments; template interpolation is
/// treated as string text, which can only under-count, never over-count.
fn syntax_depth_exceeded(source: &str, limit: usize) -> bool {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut index = 0usize;
    let mut quote: Option<u8> = None;
    let mut line_comment = false;
    let mut block_comment = false;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(active) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == active {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'/' if next == Some(b'/') => {
                line_comment = true;
                index += 2;
            }
            b'/' if next == Some(b'*') => {
                block_comment = true;
                index += 2;
            }
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                index += 1;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth > limit {
                    return true;
                }
                index += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            _ => index += 1,
        }
    }
    false
}

fn run_sync(
    source: &str,
    input: Value,
    bindings: BTreeMap<String, ScriptBinding>,
    limits: ScriptLimits,
    request_tx: mpsc::UnboundedSender<ScriptCallRequest>,
    cancel: Arc<AtomicBool>,
) -> Result<ScriptResult> {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(limits.wall_ms);
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_thread = Arc::clone(&timed_out);
    let cancel_thread = Arc::clone(&cancel);

    let runtime = JsRuntime::new().context("creating QuickJS runtime")?;
    runtime.set_memory_limit(HEAP_LIMIT_BYTES);
    runtime.set_max_stack_size(STACK_LIMIT_BYTES);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        if cancel_thread.load(Ordering::Relaxed) {
            return true;
        }
        if Instant::now() >= deadline {
            timed_out_thread.store(true, Ordering::Relaxed);
            return true;
        }
        false
    })));
    let context = JsContext::full(&runtime).context("creating QuickJS context")?;

    let stdout = Arc::new(Mutex::new(String::new()));
    let stdout_truncated = Arc::new(AtomicBool::new(false));
    let call_count = Arc::new(AtomicU32::new(0));
    let write_count = Arc::new(AtomicU32::new(0));
    let bindings_for_closure = bindings.clone();
    let input_json = serde_json::to_string(&input)?;
    let prelude = build_prelude(&bindings)?;
    let wrapped_source = format!("\"use strict\";\n{prelude}\n(function() {{\n{source}\n}})();\n");

    let value: std::result::Result<Option<Value>, String> = context.with(|ctx| {
        let outcome = (|| -> rquickjs::Result<Option<Value>> {
            let dispatch = {
                let request_tx = request_tx.clone();
                let bindings = bindings_for_closure.clone();
                let call_count = Arc::clone(&call_count);
                let write_count = Arc::clone(&write_count);
                let limits = limits.clone();
                JsFunction::new(
                    ctx.clone(),
                    move |js_ctx: rquickjs::Ctx<'_>,
                          tool: String,
                          arguments_json: String|
                          -> rquickjs::Result<String> {
                        let binding = bindings.get(&tool).ok_or_else(|| {
                            Exception::throw_message(
                                &js_ctx,
                                &format!("tool `{tool}` was not bound for this script"),
                            )
                        })?;
                        if arguments_json.len() > MAX_CALL_ARGUMENT_BYTES {
                            return Err(Exception::throw_message(
                                &js_ctx,
                                "script tool arguments exceed the 64 KiB limit",
                            ));
                        }
                        let arguments: Value =
                            serde_json::from_str(&arguments_json).map_err(|_| {
                                Exception::throw_message(
                                    &js_ctx,
                                    "script tool arguments must be JSON",
                                )
                            })?;
                        if !arguments.is_object() {
                            return Err(Exception::throw_message(
                                &js_ctx,
                                &format!("{tool} arguments must be a JSON object"),
                            ));
                        }
                        if let Err(error) = validate_arguments(&binding.schema, &arguments, 0) {
                            return Err(Exception::throw_message(
                                &js_ctx,
                                &format!("{tool} arguments are invalid: {error}"),
                            ));
                        }
                        let used = call_count.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                        if used > limits.max_calls {
                            return Err(Exception::throw_message(
                                &js_ctx,
                                &format!(
                                    "script tool-call budget exhausted ({} calls declared)",
                                    limits.max_calls
                                ),
                            ));
                        }
                        let write_used = if binding.read_only {
                            0
                        } else {
                            write_count
                                .fetch_add(1, Ordering::Relaxed)
                                .saturating_add(1)
                        };
                        if write_used > limits.max_write_calls {
                            return Err(Exception::throw_message(
                                &js_ctx,
                                &format!(
                                    "script write-call budget exhausted ({} writes declared)",
                                    limits.max_write_calls
                                ),
                            ));
                        }
                        let (reply_tx, reply_rx) = oneshot::channel();
                        request_tx
                            .send(ScriptCallRequest {
                                seq: used,
                                tool: tool.clone(),
                                arguments,
                                read_only: binding.read_only,
                                reply: reply_tx,
                            })
                            .map_err(|_| {
                                Exception::throw_message(
                                    &js_ctx,
                                    "script dispatcher is no longer running",
                                )
                            })?;
                        let reply = reply_rx.blocking_recv().map_err(|_| {
                            Exception::throw_message(&js_ctx, "script was cancelled")
                        })?;
                        if reply.aborted {
                            return Err(Exception::throw_message(&js_ctx, "script was cancelled"));
                        }
                        Ok(render_reply(&tool, &reply.output))
                    },
                )?
            };
            ctx.globals().set("__vp_dispatch__", dispatch)?;
            let print = {
                let stdout = Arc::clone(&stdout);
                let stdout_truncated = Arc::clone(&stdout_truncated);
                JsFunction::new(ctx.clone(), move |text: String| {
                    let mut buffer = match stdout.lock() {
                        Ok(buffer) => buffer,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    if buffer.len() + text.len() + 1 > MAX_STDOUT_BYTES {
                        let remaining = MAX_STDOUT_BYTES.saturating_sub(buffer.len());
                        if remaining > 0 {
                            buffer.push_str(&truncate_utf8(&text, remaining));
                        }
                        stdout_truncated.store(true, Ordering::Relaxed);
                    } else {
                        buffer.push_str(&text);
                        buffer.push('\n');
                    }
                })?
            };
            ctx.globals().set("__vp_print__", print)?;
            let input_value = ctx.json_parse(input_json.clone())?;
            ctx.globals().set("__vp_input__", input_value)?;
            let value: rquickjs::Value<'_> = ctx.eval(wrapped_source.as_str())?;
            let encoded = ctx.json_stringify(value)?;
            match encoded {
                Some(encoded) => {
                    let encoded = encoded.to_string()?;
                    if encoded.len() > MAX_RESULT_BYTES {
                        return Err(Exception::throw_message(
                            &ctx,
                            &format!(
                                "script return value exceeds {} KiB",
                                MAX_RESULT_BYTES / 1024
                            ),
                        ));
                    }
                    let value = serde_json::from_str(&encoded).map_err(|_| {
                        Exception::throw_message(&ctx, "script return value is not valid JSON")
                    })?;
                    Ok(Some(value))
                }
                None => Ok(None),
            }
        })();
        match outcome {
            Ok(value) => Ok(value),
            Err(error) => Err(format!("{} [{error}]", js_error_detail(&ctx))),
        }
    });

    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let stdout = stdout
        .lock()
        .map(|buffer| buffer.clone())
        .unwrap_or_default();
    let stdout_truncated = stdout_truncated.load(Ordering::Relaxed);
    match value {
        Ok(value) => Ok(ScriptResult {
            value,
            stdout,
            stdout_truncated,
            elapsed_ms,
            host_calls: call_count.load(Ordering::Relaxed),
            write_calls: write_count.load(Ordering::Relaxed),
        }),
        Err(message) => {
            if timed_out.load(Ordering::Relaxed) {
                bail!(
                    "script exceeded its {} ms CPU budget ({} tool calls completed)",
                    limits.wall_ms,
                    call_count.load(Ordering::Relaxed)
                );
            }
            if cancel.load(Ordering::Relaxed) {
                bail!("script was cancelled");
            }
            if message.to_ascii_lowercase().contains("out of memory") {
                bail!(
                    "script exceeded the {} MiB heap limit",
                    HEAP_LIMIT_BYTES / 1024 / 1024
                );
            }
            if message.contains("script tool-call budget exhausted")
                || message.contains("script write-call budget exhausted")
            {
                bail!("{message}");
            }
            bail!("script failed: {message}");
        }
    }
}

fn js_error_detail(ctx: &rquickjs::Ctx<'_>) -> String {
    let caught = ctx.catch();
    if caught.is_null() || caught.is_undefined() {
        return "QuickJS exception".to_string();
    }
    if let Some(object) = caught.as_object() {
        if let Ok(message) = object.get::<_, Option<String>>("message")
            && let Some(message) = message
            && !message.trim().is_empty()
        {
            return message.chars().take(2_000).collect();
        }
        if let Ok(stack) = object.get::<_, Option<String>>("stack")
            && let Some(stack) = stack
            && !stack.trim().is_empty()
        {
            return stack.chars().take(2_000).collect();
        }
    }
    caught
        .as_string()
        .map(|value| value.to_string().unwrap_or_default())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "QuickJS exception".to_string())
        .chars()
        .take(2_000)
        .collect()
}

const PRELUDE: &str = r#"
const __vp_call = globalThis.__vp_dispatch__;
const __vp_print = globalThis.__vp_print__;
delete globalThis.__vp_dispatch__;
delete globalThis.__vp_print__;
const __vp_input_raw = globalThis.__vp_input__;
delete globalThis.__vp_input__;
function __vp_stringify(value) {
  try { return JSON.stringify(value); } catch (_) { return String(value); }
}
function __vp_freeze(value, depth) {
  if (depth > 8 || value === null || typeof value !== "object") return value;
  Object.freeze(value);
  for (const key of Object.keys(value)) __vp_freeze(value[key], depth + 1);
  return value;
}
const input = __vp_freeze(__vp_input_raw, 0);
function __vp_println(...args) {
  __vp_print(args.map((value) => typeof value === "string" ? value : __vp_stringify(value)).join(" "));
}
globalThis.print = __vp_println;
globalThis.console = Object.freeze({ log: __vp_println, info: __vp_println, warn: __vp_println, error: __vp_println });
const __vp_names = __VP_BINDING_NAMES__;
const __vp_target = {};
function __vp_make(name) {
  return function(args) {
    let value = args === undefined ? {} : args;
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      throw new TypeError(name + " arguments must be an object");
    }
    const reply = JSON.parse(__vp_call(name, JSON.stringify(value)));
    if (!reply.ok) {
      const error = new Error(reply.message || reply.text || (name + " failed"));
      error.code = reply.error_code || "TOOL_FAILED";
      throw error;
    }
    return { text: reply.text, data: reply.data, truncated: reply.truncated === true };
  };
}
for (const name of __vp_names) {
  Object.defineProperty(__vp_target, name, {
    value: __vp_make(name), enumerable: true, writable: false, configurable: false
  });
}
Object.preventExtensions(__vp_target);
globalThis.tools = new Proxy(__vp_target, {
  get(target, name) {
    if (typeof name === "string" && Object.prototype.hasOwnProperty.call(target, name)) return target[name];
    throw new Error("tool `" + String(name) + "` is not bound for this script; available: " + __vp_names.join(", "));
  },
  set() { throw new TypeError("tools is read-only"); },
  defineProperty() { return false; },
  deleteProperty() { return false; }
});
delete globalThis.eval;
delete globalThis.Function;
"#;

fn build_prelude(bindings: &BTreeMap<String, ScriptBinding>) -> Result<String> {
    let names = bindings
        .keys()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    Ok(PRELUDE.replace("__VP_BINDING_NAMES__", &format!("[{names}]")))
}

fn render_reply(tool: &str, output: &ToolOutput) -> String {
    let text = truncate_utf8(&output.content, MAX_CALL_TEXT_BYTES);
    let truncated = output.truncated || text.len() < output.content.len();
    let data = output.structured.as_ref().and_then(|value| {
        let encoded = serde_json::to_vec(value).ok()?;
        if encoded.len() > MAX_CALL_VALUE_BYTES {
            None
        } else {
            Some(value.clone())
        }
    });
    let failure = output.failure.as_ref();
    let error_code = failure
        .map(|failure| failure.code.as_str())
        .unwrap_or(if output.is_error { "TOOL_FAILED" } else { "" });
    let message = failure
        .map(|failure| failure.message.clone())
        .unwrap_or_else(|| text.clone());
    json!({
        "ok": !output.is_error,
        "text": text,
        "message": message,
        "data": data,
        "truncated": truncated,
        "error_code": error_code,
        "tool": tool,
    })
    .to_string()
}

fn truncate_utf8(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// Minimal typed checking against the advertised JSON schema. This is a
/// usability guard, not authoritative validation: the tool still validates its
/// own arguments after dispatch.
fn validate_arguments(schema: &Value, value: &Value, depth: usize) -> Result<()> {
    if depth > 8 {
        bail!("argument nesting exceeds 8 levels");
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.iter().any(|option| option == value)
    {
        bail!("value is not one of the allowed enum values");
    }
    let Some(kind) = schema.get("type") else {
        return Ok(());
    };
    let allowed = match kind {
        Value::String(kind) => vec![kind.as_str()],
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !allowed.is_empty() && !allowed.iter().any(|kind| type_matches(kind, value)) {
        bail!(
            "expected {}, got {}",
            allowed.join(" or "),
            json_type(value)
        );
    }
    if value.is_object() {
        let object = value.as_object().expect("object");
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    bail!("missing required field `{name}`");
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&Value::Bool(false))
            && let Some(properties) = properties
        {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    bail!("unknown field `{key}`");
                }
            }
        }
        if let Some(properties) = properties {
            for (key, item) in object {
                if let Some(property) = properties.get(key) {
                    validate_arguments(property, item, depth + 1)
                        .with_context(|| format!("field `{key}`"))?;
                }
            }
        }
    }
    if value.is_array() {
        let items = value.as_array().expect("array");
        if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
            && items.len() as u64 > max
        {
            bail!("array exceeds maxItems {max}");
        }
        if let Some(item_schema) = schema.get("items") {
            for item in items.iter().take(64) {
                validate_arguments(item_schema, item, depth + 1)?;
            }
        }
    }
    if let Some(text) = value.as_str()
        && let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
        && text.chars().count() as u64 > max
    {
        bail!("string exceeds maxLength {max}");
    }
    if let Some(number) = value.as_f64() {
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && number > max
        {
            bail!("number exceeds maximum {max}");
        }
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && number < min
        {
            bail!("number is below minimum {min}");
        }
    }
    Ok(())
}

fn type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value.as_f64().is_some_and(|value| value.fract() == 0.0)
        }
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(name: &str, schema: Value, read_only: bool) -> ScriptBinding {
        ScriptBinding {
            name: name.to_string(),
            description: name.to_string(),
            schema,
            read_only,
        }
    }

    fn read_bindings() -> BTreeMap<String, ScriptBinding> {
        BTreeMap::from([(
            "read_file".to_string(),
            binding(
                "read_file",
                json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                    "additionalProperties": false
                }),
                true,
            ),
        )])
    }

    async fn run_with_reply(
        source: &str,
        input: Value,
        bindings: BTreeMap<String, ScriptBinding>,
        limits: ScriptLimits,
        reply: impl Fn(&ScriptCallRequest) -> ToolOutput + Send + Sync + 'static,
    ) -> Result<(ScriptResult, Vec<(String, Value)>)> {
        let mut run = spawn(source, input, bindings, limits)?;
        let reply = Arc::new(reply);
        let mut calls = Vec::new();
        loop {
            match run.next_event().await {
                Some(ScriptEvent::Finished(result)) => return result.map(|result| (result, calls)),
                Some(ScriptEvent::Call(request)) => {
                    calls.push((request.tool.clone(), request.arguments.clone()));
                    let output = reply(&request);
                    let _ = request.reply.send(ScriptReply {
                        output,
                        aborted: false,
                    });
                }
                None => bail!("script runner ended without a result"),
            }
        }
    }

    #[test]
    fn rejects_syntax_and_forbidden_globals() {
        assert!(validate_source("").is_err());
        assert!(validate_source("let x = (;").is_err());
        assert!(validate_source("eval('1')").is_err());
        assert!(validate_source("globalThis.x = 1").is_err());
        assert!(validate_source("import('x')").is_err());
        assert!(validate_source("require('fs')").is_err());
        assert!(validate_source("const x = 1; return x;").is_ok());
        assert!(validate_source("tools.read_file({path: 'a'})").is_ok());
    }

    #[test]
    fn rejects_deep_nesting_and_oversized_source() {
        let deep = format!(
            "{}1{}",
            "(".repeat(MAX_SYNTAX_DEPTH + 1),
            ")".repeat(MAX_SYNTAX_DEPTH + 1)
        );
        assert!(validate_source(&deep).is_err());
        assert!(validate_source(&"a".repeat(MAX_SOURCE_BYTES + 1)).is_err());
    }

    #[test]
    fn bindings_are_limited_to_the_frozen_bundle_and_write_flag() {
        let advertised = vec![
            ToolSchema {
                name: "read_file".into(),
                description: "read".into(),
                parameters: json!({"type": "object"}),
            },
            ToolSchema {
                name: "write_file".into(),
                description: "write".into(),
                parameters: json!({"type": "object"}),
            },
            ToolSchema {
                name: "shell".into(),
                description: "shell".into(),
                parameters: json!({"type": "object"}),
            },
            ToolSchema {
                name: "not_in_bundle".into(),
                description: "nope".into(),
                parameters: json!({"type": "object"}),
            },
        ];
        let read_only = bindings(&advertised, false);
        assert!(read_only.contains_key("read_file"));
        assert!(!read_only.contains_key("write_file"));
        assert!(!read_only.contains_key("shell"));
        assert!(!read_only.contains_key("not_in_bundle"));
        let writable = bindings(&advertised, true);
        assert!(writable.contains_key("write_file"));
        assert!(writable["read_file"].read_only);
        assert!(!writable["write_file"].read_only);
    }

    #[tokio::test]
    async fn unknown_tool_is_rejected_at_bind_time() {
        let error = run_with_reply(
            "tools.shell({command: 'rm -rf /'})",
            json!({}),
            read_bindings(),
            ScriptLimits::default(),
            |_| ToolOutput::ok("should not run"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not bound"), "{error:#}");
    }

    #[tokio::test]
    async fn ambient_capabilities_are_absent() {
        let (result, calls) = run_with_reply(
            r#"
            const F = ({}).constructor.constructor;
            const probe = (name) => { try { return F("return typeof " + name)(); } catch (_) { return "error"; } };
            const names = ["fetch", "require", "process", "os", "std", "WebAssembly", "XMLHttpRequest", "WebSocket", "eval", "Function"];
            const missing = names.filter((name) => probe(name) === "undefined");
            return { missing, toolsType: typeof tools, inputType: typeof input, dispatcher: probe("__vp_dispatch__") };
            "#,
            json!({"a": 1}),
            read_bindings(),
            ScriptLimits::default(),
            |_| ToolOutput::ok("unused"),
        )
        .await
        .unwrap();
        assert!(calls.is_empty());
        let value = result.value.unwrap();
        let missing = value["missing"].as_array().unwrap().clone();
        assert_eq!(missing.len(), 10, "ambient capability leaked: {missing:?}");
        assert_eq!(value["toolsType"], "object");
        assert_eq!(value["dispatcher"], "undefined");
    }

    #[tokio::test]
    async fn typed_arguments_are_checked_before_dispatch() {
        let error = run_with_reply(
            "return tools.read_file({path: 7});",
            json!({}),
            read_bindings(),
            ScriptLimits::default(),
            |_| ToolOutput::ok("should not run"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("invalid"), "{error:#}");
    }

    #[tokio::test]
    async fn call_and_write_budgets_are_enforced() {
        let limits = ScriptLimits {
            wall_ms: 5_000,
            max_calls: 2,
            max_write_calls: 1,
        };
        let error = run_with_reply(
            "for (let i = 0; i < 5; i++) tools.read_file({path: 'a'}); return 1;",
            json!({}),
            read_bindings(),
            limits,
            |_| ToolOutput::ok("x"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("tool-call budget"), "{error:#}");

        let write_bindings = BTreeMap::from([(
            "write_file".to_string(),
            binding(
                "write_file",
                json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}),
                false,
            ),
        )]);
        let error = run_with_reply(
            "for (let i = 0; i < 3; i++) tools.write_file({path: 'a', content: 'x'});",
            json!({}),
            write_bindings,
            ScriptLimits {
                wall_ms: 5_000,
                max_calls: 10,
                max_write_calls: 1,
            },
            |_| ToolOutput::ok("x"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("write-call budget"), "{error:#}");
    }

    #[tokio::test]
    async fn cpu_budget_stops_an_infinite_loop() {
        let error = run_with_reply(
            "while (true) {}",
            json!({}),
            read_bindings(),
            ScriptLimits {
                wall_ms: 150,
                max_calls: 4,
                max_write_calls: 0,
            },
            |_| ToolOutput::ok("x"),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("CPU budget"),
            "expected timeout, got {error:#}"
        );
    }

    #[tokio::test]
    async fn heap_budget_is_enforced() {
        let error = run_with_reply(
            r#"
            const chunks = [];
            for (;;) chunks.push("x".repeat(1024 * 1024));
            "#,
            json!({}),
            read_bindings(),
            ScriptLimits {
                wall_ms: 5_000,
                max_calls: 4,
                max_write_calls: 0,
            },
            |_| ToolOutput::ok("x"),
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("heap limit") || message.contains("CPU budget"),
            "expected a heap/CPU stop, got {message}"
        );
    }

    #[tokio::test]
    async fn write_tools_are_absent_without_the_write_flag() {
        let error = run_with_reply(
            "tools.write_file({path: 'a', content: 'b'});",
            json!({}),
            read_bindings(),
            ScriptLimits::default(),
            |_| ToolOutput::ok("should not run"),
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("not bound"), "{message}");
    }

    #[tokio::test]
    async fn stdout_and_calls_are_bounded_and_deterministic() {
        let source = r#"
            print("hello", 1);
            const first = tools.read_file({path: "a"}).text;
            const second = tools.read_file({path: "b"}).text;
            return { first, second };
        "#;
        let run = || {
            run_with_reply(
                source,
                json!({}),
                read_bindings(),
                ScriptLimits::default(),
                |request| {
                    ToolOutput::ok(format!(
                        "file:{}",
                        request.arguments["path"].as_str().unwrap_or("?")
                    ))
                },
            )
        };
        let (first, calls_one) = run().await.unwrap();
        let (second, calls_two) = run().await.unwrap();
        assert_eq!(calls_one, calls_two);
        assert_eq!(
            calls_one
                .iter()
                .map(|(name, args)| (name.as_str(), args["path"].as_str().unwrap_or("")))
                .collect::<Vec<_>>(),
            vec![("read_file", "a"), ("read_file", "b")]
        );
        assert_eq!(first.stdout, "hello 1\n");
        assert_eq!(first.value.unwrap()["first"], "file:a");
        assert_eq!(second.value.unwrap()["second"], "file:b");
    }

    #[tokio::test]
    async fn oversized_stdout_is_truncated() {
        let (result, _) = run_with_reply(
            "for (let i = 0; i < 20000; i++) print('abcdefgh');",
            json!({}),
            read_bindings(),
            ScriptLimits::default(),
            |_| ToolOutput::ok("x"),
        )
        .await
        .unwrap();
        assert!(result.stdout_truncated);
        assert!(result.stdout.len() <= MAX_STDOUT_BYTES);
    }

    #[tokio::test]
    async fn host_error_is_raised_as_a_catchable_tool_error() {
        let (result, _) = run_with_reply(
            r#"
            try {
              tools.read_file({path: "missing"});
              return "no error";
            } catch (error) {
              return { code: error.code, message: error.message };
            }
            "#,
            json!({}),
            read_bindings(),
            ScriptLimits::default(),
            |_| {
                ToolOutput::error_coded(
                    "read_file",
                    crate::tooling::errors::ToolErrorCode::FILE_NOT_FOUND,
                    "file not found",
                )
            },
        )
        .await
        .unwrap();
        let value = result.value.unwrap();
        assert_eq!(value["code"], "FILE_NOT_FOUND");
        assert_eq!(value["message"], "file not found");
    }

    #[test]
    fn runner_errors_map_to_stable_tool_codes() {
        assert_eq!(
            classify_error("script exceeded its 5000 ms CPU budget"),
            crate::tooling::errors::ToolErrorCode::SCRIPT_TIMEOUT
        );
        assert_eq!(
            classify_error("script exceeded the 64 MiB heap limit"),
            crate::tooling::errors::ToolErrorCode::SCRIPT_MEMORY_LIMIT
        );
        assert_eq!(
            classify_error("script tool-call budget exhausted"),
            crate::tooling::errors::ToolErrorCode::SCRIPT_CALL_BUDGET
        );
        assert_eq!(
            classify_error("script write-call budget exhausted"),
            crate::tooling::errors::ToolErrorCode::SCRIPT_WRITE_BUDGET
        );
        assert_eq!(
            classify_error("forbidden ambient reference `fetch`"),
            crate::tooling::errors::ToolErrorCode::SCRIPT_INVALID
        );
        assert_eq!(
            classify_error("some other failure"),
            crate::tooling::errors::ToolErrorCode::SCRIPT_FAILED
        );
    }

    #[test]
    fn argument_schema_honours_required_unknown_and_enum_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "maxLength": 3},
                "mode": {"type": "string", "enum": ["a", "b"]}
            },
            "required": ["path"],
            "additionalProperties": false
        });
        assert!(validate_arguments(&schema, &json!({"path": "abc"}), 0).is_ok());
        assert!(validate_arguments(&schema, &json!({"path": 1}), 0).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "abcd"}), 0).is_err());
        assert!(validate_arguments(&schema, &json!({}), 0).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "a", "x": 1}), 0).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "a", "mode": "c"}), 0).is_err());
    }
}
