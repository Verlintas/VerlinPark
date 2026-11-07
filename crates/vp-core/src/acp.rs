//! Agent Client Protocol (ACP) client bridge.
//!
//! VerlinPark can delegate a focused task to a user-configured external agent
//! that speaks ACP over stdio. The external agent runs in a task worktree under
//! the same Seatbelt write sandbox as shell work, its permission requests are
//! routed through the VerlinPark approval center, and its committed changes go
//! through the normal Verifier and Integrator path.
//!
//! This is intentionally a *client* implementation: VerlinPark is the editor
//! side of ACP. It does not host an agent.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::{
    CancelNotification, ContentBlock, EnvVariable, InitializeRequest, McpServer, McpServerStdio,
    NewSessionRequest, PermissionOption, PermissionOptionKind, PromptRequest, ProtocolVersion,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::{Agent, Client, ConnectionTo};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::tools::ToolContext;

/// One permission option offered by the external agent.
#[derive(Debug, Clone)]
pub struct AcpPermissionOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// A normalized permission request. The runtime maps this to the ordinary
/// approval queue and answers with the closest matching ACP option.
#[derive(Debug, Clone)]
pub struct AcpPermissionRequest {
    pub tool_call_id: String,
    pub title: String,
    pub kind: String,
    pub raw_input: Option<Value>,
    pub options: Vec<AcpPermissionOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpPermissionDecision {
    Approve,
    Deny,
    Cancelled,
}

/// Normalized session updates emitted by the external agent.
#[derive(Debug, Clone)]
pub enum AcpUpdate {
    Message(String),
    Thought(String),
    ToolCall {
        id: String,
        title: String,
        kind: String,
        status: String,
        raw_input: Option<Value>,
        content: Option<Value>,
        raw_output: Option<Value>,
    },
    ToolCallUpdate {
        id: String,
        title: Option<String>,
        status: Option<String>,
        content: Option<Value>,
        raw_output: Option<Value>,
    },
}

/// Callbacks the runtime provides while an external agent runs.
#[async_trait]
pub trait AcpHost: Send + Sync {
    async fn update(&self, update: AcpUpdate);
    async fn permission(&self, request: AcpPermissionRequest) -> AcpPermissionDecision;
}

/// Everything needed to launch an external agent under the shell sandbox.
#[derive(Debug, Clone)]
pub struct AcpLaunch {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    pub sandbox_root: PathBuf,
    pub secret_roots: Vec<PathBuf>,
    pub network_allowed: bool,
    pub home: PathBuf,
    pub restrict_home: bool,
    pub read_jail: bool,
    pub mcp_servers: Vec<McpServer>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct AcpRunOutcome {
    pub session_id: String,
    pub stop_reason: String,
    pub cancelled: bool,
    pub agent_name: String,
    pub agent_version: String,
    pub protocol_version: u16,
}

#[derive(Debug, Clone)]
pub struct AcpProbeOutcome {
    pub agent_name: String,
    pub agent_version: String,
    pub protocol_version: u16,
    pub session_created: bool,
}

/// Run one prompt turn against an external ACP agent.
pub async fn run_prompt(
    launch: AcpLaunch,
    prompt: String,
    host: Arc<dyn AcpHost>,
    mut cancel: watch::Receiver<bool>,
) -> Result<AcpRunOutcome> {
    let timeout = launch.timeout;
    let cwd = launch.cwd.clone();
    let mcp_servers = launch.mcp_servers.clone();
    let outcome = connect(
        launch,
        host.clone(),
        move |connection, host, mut updates| {
            Box::pin(async move {
                let initialized = connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                if initialized.protocol_version != ProtocolVersion::V1 {
                    return Err(agent_client_protocol::util::internal_error(format!(
                        "unsupported ACP protocol version {}",
                        initialized.protocol_version
                    )));
                }
                let agent_name = initialized
                    .agent_info
                    .as_ref()
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| "external agent".to_string());
                let agent_version = initialized
                    .agent_info
                    .as_ref()
                    .map(|info| info.version.clone())
                    .unwrap_or_default();
                let protocol_version = initialized
                    .protocol_version
                    .to_string()
                    .parse()
                    .unwrap_or(1);
                let session = connection
                    .send_request(NewSessionRequest::new(cwd.clone()).mcp_servers(mcp_servers))
                    .block_task()
                    .await?;
                let session_id = session.session_id;
                if *cancel.borrow() {
                    return Ok(AcpRunOutcome {
                        session_id: session_id.to_string(),
                        stop_reason: "cancelled".to_string(),
                        cancelled: true,
                        agent_name,
                        agent_version,
                        protocol_version,
                    });
                }
                let prompt = connection
                    .send_request(PromptRequest::new(
                        session_id.clone(),
                        vec![ContentBlock::Text(TextContent::new(prompt))],
                    ))
                    .block_task();
                tokio::pin!(prompt);
                let mut deadline = tokio::time::Instant::now() + timeout;
                let mut cancelled = false;
                let mut cancel_closed = false;
                let response = loop {
                    tokio::select! {
                        result = &mut prompt => break result?,
                        Some(notification) = updates.recv() => {
                            if let Some(update) = normalize_update(notification.update) {
                                host.update(update).await;
                            }
                        }
                        changed = cancel.changed(), if !cancelled && !cancel_closed => {
                            if changed.is_err() {
                                cancel_closed = true;
                            } else if *cancel.borrow() {
                                cancelled = true;
                                let _ = connection
                                    .send_notification(CancelNotification::new(session_id.clone()));
                                deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                            }
                        }
                        _ = tokio::time::sleep_until(deadline) => {
                            if cancelled {
                                return Err(agent_client_protocol::util::internal_error(
                                    "ACP agent did not stop within 10s of cancellation",
                                ));
                            }
                            cancelled = true;
                            let _ = connection
                                .send_notification(CancelNotification::new(session_id.clone()));
                            deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                        }
                    }
                };
                while let Ok(notification) = updates.try_recv() {
                    if let Some(update) = normalize_update(notification.update) {
                        host.update(update).await;
                    }
                }
                let stop_reason = stop_reason_name(response.stop_reason);
                Ok(AcpRunOutcome {
                    session_id: session_id.to_string(),
                    stop_reason,
                    cancelled,
                    agent_name,
                    agent_version,
                    protocol_version,
                })
            })
        },
    )
    .await?;
    Ok(outcome)
}

/// Initialize an agent and create a session without sending a prompt. This is
/// the safe connectivity check used by `vpx acp-probe` / Settings.
pub async fn probe(launch: AcpLaunch) -> Result<AcpProbeOutcome> {
    let cwd = launch.cwd.clone();
    let mcp_servers = launch.mcp_servers.clone();
    connect(
        launch,
        Arc::new(NoopHost),
        move |connection, _host, _updates| {
            Box::pin(async move {
                let initialized = connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let session = connection
                    .send_request(NewSessionRequest::new(cwd.clone()).mcp_servers(mcp_servers))
                    .block_task()
                    .await?;
                let _ = session;
                Ok(AcpProbeOutcome {
                    agent_name: initialized
                        .agent_info
                        .as_ref()
                        .map(|info| info.name.clone())
                        .unwrap_or_else(|| "external agent".to_string()),
                    agent_version: initialized
                        .agent_info
                        .as_ref()
                        .map(|info| info.version.clone())
                        .unwrap_or_default(),
                    protocol_version: initialized
                        .protocol_version
                        .to_string()
                        .parse()
                        .unwrap_or(1),
                    session_created: true,
                })
            })
        },
    )
    .await
}

type BoxMain<T> =
    futures_util::future::BoxFuture<'static, std::result::Result<T, agent_client_protocol::Error>>;

async fn connect<T, F>(launch: AcpLaunch, host: Arc<dyn AcpHost>, main: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(
            ConnectionTo<Agent>,
            Arc<dyn AcpHost>,
            mpsc::UnboundedReceiver<SessionNotification>,
        ) -> BoxMain<T>
        + Send
        + 'static,
{
    let mut command = build_command(&launch).await?;
    let mut child = command
        .spawn()
        .with_context(|| format!("spawning ACP agent {} ({})", launch.name, launch.command))?;
    let _group = ProcessGroupGuard::new(child.id());
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("ACP agent stdin was not piped"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("ACP agent stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("ACP agent stderr was not piped"))?;
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_task = tokio::spawn(read_stderr(stderr, stderr_buffer.clone()));
    let transport = agent_client_protocol::ByteStreams::new(stdin.compat_write(), stdout.compat());
    let (update_tx, update_rx) = mpsc::unbounded_channel::<SessionNotification>();
    let permission_host = host.clone();
    let overall_timeout = launch.timeout + Duration::from_secs(15);
    let connected = Client
        .builder()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                let _ = update_tx.send(notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let decision = permission_host
                    .permission(normalize_permission(&request))
                    .await;
                let outcome = permission_outcome(&request.options, decision);
                responder.respond(RequestPermissionResponse::new(outcome))?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async move |connection| {
            main(connection, host, update_rx).await
        });
    let result = tokio::time::timeout(overall_timeout, connected)
        .await
        .unwrap_or_else(|_| {
            Err(agent_client_protocol::util::internal_error(format!(
                "ACP agent did not finish within {overall_timeout:?}"
            )))
        });
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), stderr_task).await;
    let stderr = stderr_buffer
        .lock()
        .map(|text| text.clone())
        .unwrap_or_default();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            let detail = if stderr.trim().is_empty() {
                error.to_string()
            } else {
                format!("{error}\nstderr: {}", truncate(&stderr, 4_000))
            };
            Err(anyhow!("ACP agent {} failed: {detail}", launch.name))
        }
    }
}

async fn build_command(launch: &AcpLaunch) -> Result<tokio::process::Command> {
    let context = ToolContext {
        root: launch.cwd.clone(),
        project_root: launch.project_root.clone(),
        sandbox_root: launch.sandbox_root.clone(),
        secret_roots: launch.secret_roots.clone(),
        network_allowed: launch.network_allowed,
        home: launch.home.clone(),
        restrict_home: launch.restrict_home,
        read_jail: launch.read_jail,
    };
    let mut command = crate::tools::sandboxed_command(&context).await?;
    let scratch = launch
        .sandbox_root
        .canonicalize()
        .unwrap_or_else(|_| launch.sandbox_root.clone());
    let home = scratch.join("home");
    let tmp = scratch.join("tmp");
    for directory in [
        &home,
        &tmp,
        &scratch.join("config"),
        &scratch.join("data"),
        &scratch.join("cache"),
    ] {
        tokio::fs::create_dir_all(directory).await?;
    }
    command
        .arg(&launch.command)
        .args(&launch.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Credentials and other extras are configured explicitly. They are
    // applied before the fixed scratch paths so an env entry cannot redirect
    // HOME/caches back into the real user state.
    for (key, value) in &launch.env {
        command.env(key, value);
    }
    command
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env("XDG_DATA_HOME", scratch.join("data"))
        .env("XDG_CACHE_HOME", scratch.join("cache"))
        .env("TMPDIR", &tmp)
        .env("TMP", &tmp)
        .env("TEMP", &tmp);
    Ok(command)
}

async fn read_stderr(mut stderr: tokio::process::ChildStderr, buffer: Arc<Mutex<String>>) {
    let mut collected = Vec::new();
    let mut chunk = [0_u8; 4_096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if collected.len() < 64 * 1024 {
                    collected.extend_from_slice(&chunk[..read]);
                }
            }
        }
    }
    if let Ok(mut text) = buffer.lock() {
        *text = String::from_utf8_lossy(&collected).into_owned();
    }
}

fn normalize_update(update: SessionUpdate) -> Option<AcpUpdate> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            let ContentBlock::Text(text) = chunk.content else {
                return None;
            };
            Some(AcpUpdate::Message(text.text))
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            let ContentBlock::Text(text) = chunk.content else {
                return None;
            };
            Some(AcpUpdate::Thought(text.text))
        }
        SessionUpdate::ToolCall(call) => Some(AcpUpdate::ToolCall {
            id: call.tool_call_id.to_string(),
            title: call.title,
            kind: json_name(&call.kind),
            status: json_name(&call.status),
            raw_input: call.raw_input,
            content: if call.content.is_empty() {
                None
            } else {
                serde_json::to_value(call.content).ok()
            },
            raw_output: call.raw_output,
        }),
        SessionUpdate::ToolCallUpdate(update) => Some(AcpUpdate::ToolCallUpdate {
            id: update.tool_call_id.to_string(),
            title: update.fields.title,
            status: update.fields.status.map(|status| json_name(&status)),
            content: update
                .fields
                .content
                .and_then(|content| serde_json::to_value(content).ok()),
            raw_output: update.fields.raw_output,
        }),
        _ => None,
    }
}

fn normalize_permission(request: &RequestPermissionRequest) -> AcpPermissionRequest {
    AcpPermissionRequest {
        tool_call_id: request.tool_call.tool_call_id.to_string(),
        title: request
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "External agent permission request".to_string()),
        kind: request
            .tool_call
            .fields
            .kind
            .map(|kind| json_name(&kind))
            .unwrap_or_else(|| "other".to_string()),
        raw_input: request.tool_call.fields.raw_input.clone(),
        options: request
            .options
            .iter()
            .map(|option| AcpPermissionOption {
                id: option.option_id.to_string(),
                name: option.name.clone(),
                kind: json_name(&option.kind),
            })
            .collect(),
    }
}

fn permission_outcome(
    options: &[PermissionOption],
    decision: AcpPermissionDecision,
) -> RequestPermissionOutcome {
    if decision == AcpPermissionDecision::Cancelled {
        return RequestPermissionOutcome::Cancelled;
    }
    let allowed = matches!(decision, AcpPermissionDecision::Approve);
    let preferred = if allowed {
        [
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::AllowAlways,
        ]
    } else {
        [
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        ]
    };
    let option = options
        .iter()
        .find(|option| option.kind == preferred[0])
        .or_else(|| options.iter().find(|option| option.kind == preferred[1]))
        .or_else(|| {
            options.iter().find(|option| {
                let allow = matches!(
                    option.kind,
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                );
                allow == allowed
            })
        });
    match option {
        Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            option.option_id.clone(),
        )),
        None => RequestPermissionOutcome::Cancelled,
    }
}

fn json_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "other".to_string())
}

fn stop_reason_name(reason: agent_client_protocol::schema::StopReason) -> String {
    json_name(&reason)
}

fn truncate(content: &str, max: usize) -> String {
    if content.len() <= max {
        return content.to_string();
    }
    let mut end = max.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &content[..end])
}

pub fn mcp_stdio_server(
    name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> McpServer {
    McpServer::Stdio(
        McpServerStdio::new(name, command).args(args.to_vec()).env(
            env.iter()
                .map(|(name, value)| EnvVariable::new(name, value))
                .collect(),
        ),
    )
}

struct NoopHost;

#[async_trait]
impl AcpHost for NoopHost {
    async fn update(&self, _update: AcpUpdate) {}

    async fn permission(&self, _request: AcpPermissionRequest) -> AcpPermissionDecision {
        AcpPermissionDecision::Deny
    }
}

struct ProcessGroupGuard(Option<i32>);

impl ProcessGroupGuard {
    fn new(pid: Option<u32>) -> Self {
        Self(pid.and_then(|pid| i32::try_from(pid).ok()))
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: kill(2) with a negative pid targets the process group we
            // asked sandboxed_command to create. ESRCH is a harmless no-op when
            // the group already exited.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    #[derive(Default)]
    struct RecordingHost {
        messages: Mutex<String>,
        tool_calls: AtomicUsize,
        permissions: AtomicUsize,
    }

    #[async_trait]
    impl AcpHost for RecordingHost {
        async fn update(&self, update: AcpUpdate) {
            if let AcpUpdate::Message(text) = &update
                && let Ok(mut messages) = self.messages.lock()
            {
                messages.push_str(text);
            }
            if matches!(update, AcpUpdate::ToolCall { .. }) {
                self.tool_calls.fetch_add(1, Ordering::SeqCst);
            }
        }

        async fn permission(&self, _request: AcpPermissionRequest) -> AcpPermissionDecision {
            self.permissions.fetch_add(1, Ordering::SeqCst);
            AcpPermissionDecision::Approve
        }
    }

    fn fake_agent(dir: &Path) -> PathBuf {
        let path = dir.join("fake-acp-agent.py");
        std::fs::write(
            &path,
            r#"#!/usr/bin/env python3
import json, os, sys

def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

def reply(req, result):
    send({"jsonrpc": "2.0", "id": req.get("id"), "result": result})

for line in sys.stdin:
    if not line.strip():
        continue
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        reply(req, {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "authMethods": [],
            "agentInfo": {"name": "fake-acp", "version": "0.0.1"},
        })
    elif method == "session/new":
        reply(req, {"sessionId": "fake-session"})
    elif method == "session/prompt":
        send({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "fake-session",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "working "}},
        }})
        send({"jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission", "params": {
            "sessionId": "fake-session",
            "toolCall": {"toolCallId": "tool-1", "title": "Write a file", "kind": "edit", "rawInput": {"path": "acp.txt"}},
            "options": [
                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                {"optionId": "deny", "name": "Reject", "kind": "reject_once"},
            ],
        }})
        # Wait for the permission response before continuing.
        while True:
            response = json.loads(sys.stdin.readline())
            if response.get("id") == "perm-1":
                option = (response.get("result") or {}).get("outcome") or {}
                if option.get("outcome") != "selected" or option.get("optionId") != "allow":
                    send({"jsonrpc": "2.0", "method": "session/update", "params": {
                        "sessionId": "fake-session",
                        "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "denied"}},
                    }})
                break
        with open(os.path.join(os.getcwd(), "acp.txt"), "w") as handle:
            handle.write("from acp\n")
        send({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "fake-session",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "done"}},
        }})
        send({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "fake-session",
            "update": {"sessionUpdate": "tool_call", "toolCallId": "tool-1", "title": "Write a file", "kind": "edit", "status": "completed"},
        }})
        reply(req, {"stopReason": "end_turn"})
    elif method == "session/cancel":
        pass
    else:
        send({"jsonrpc": "2.0", "id": req.get("id"), "error": {"code": -32601, "message": "not found"}})
"#,
        )
        .unwrap();
        path
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn runs_a_prompt_and_streams_updates() {
        let dir = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "test\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(project.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let script = fake_agent(dir.path());
        let host = Arc::new(RecordingHost::default());
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let launch = AcpLaunch {
            name: "fake".into(),
            command: "/usr/bin/python3".into(),
            args: vec![script.to_string_lossy().to_string()],
            env: BTreeMap::new(),
            cwd: project.path().to_path_buf(),
            project_root: project.path().to_path_buf(),
            sandbox_root: dir.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: false,
            read_jail: false,
            mcp_servers: Vec::new(),
            timeout: Duration::from_secs(20),
        };
        let outcome = run_prompt(launch, "do the task".into(), host.clone(), cancel_rx)
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, "end_turn");
        assert_eq!(outcome.agent_name, "fake-acp");
        assert_eq!(
            std::fs::read_to_string(project.path().join("acp.txt")).unwrap(),
            "from acp\n"
        );
        assert_eq!(host.messages.lock().unwrap().as_str(), "working done");
        assert_eq!(host.permissions.load(Ordering::SeqCst), 1);
        assert_eq!(host.tool_calls.load(Ordering::SeqCst), 1);
    }

    struct SignalHost {
        started: tokio::sync::mpsc::UnboundedSender<()>,
    }

    #[async_trait]
    impl AcpHost for SignalHost {
        async fn update(&self, update: AcpUpdate) {
            if let AcpUpdate::Message(text) = update
                && text.contains("started")
            {
                let _ = self.started.send(());
            }
        }

        async fn permission(&self, _request: AcpPermissionRequest) -> AcpPermissionDecision {
            AcpPermissionDecision::Approve
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn cancels_a_running_prompt() {
        let dir = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let script = dir.path().join("cancel-acp-agent.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, sys

def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

def reply(req, result):
    send({"jsonrpc": "2.0", "id": req.get("id"), "result": result})

for line in sys.stdin:
    if not line.strip():
        continue
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        reply(req, {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": [], "agentInfo": {"name": "cancel-acp", "version": "1"}})
    elif method == "session/new":
        reply(req, {"sessionId": "cancel-session"})
    elif method == "session/prompt":
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "cancel-session", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "started"}}}})
        for next_line in sys.stdin:
            next_request = json.loads(next_line)
            if next_request.get("method") == "session/cancel":
                reply(req, {"stopReason": "cancelled"})
                break
"#,
        )
        .unwrap();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let host = Arc::new(SignalHost {
            started: started_tx,
        });
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let launch = AcpLaunch {
            name: "cancel".into(),
            command: "/usr/bin/python3".into(),
            args: vec![script.to_string_lossy().to_string()],
            env: BTreeMap::new(),
            cwd: project.path().to_path_buf(),
            project_root: project.path().to_path_buf(),
            sandbox_root: dir.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: false,
            read_jail: false,
            mcp_servers: Vec::new(),
            timeout: Duration::from_secs(20),
        };
        let run =
            tokio::spawn(
                async move { run_prompt(launch, "cancel me".into(), host, cancel_rx).await },
            );
        tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
            .await
            .expect("agent did not start")
            .expect("agent start signal was dropped");
        cancel_tx.send_replace(true);
        let outcome = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .expect("cancel did not finish")
            .unwrap()
            .unwrap();
        assert!(outcome.cancelled);
        assert_eq!(outcome.stop_reason, "cancelled");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn probes_an_agent_without_prompting() {
        let dir = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let script = fake_agent(dir.path());
        let launch = AcpLaunch {
            name: "fake".into(),
            command: "/usr/bin/python3".into(),
            args: vec![script.to_string_lossy().to_string()],
            env: BTreeMap::new(),
            cwd: project.path().to_path_buf(),
            project_root: project.path().to_path_buf(),
            sandbox_root: dir.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: false,
            read_jail: false,
            mcp_servers: Vec::new(),
            timeout: Duration::from_secs(20),
        };
        let outcome = probe(launch).await.unwrap();
        assert_eq!(outcome.agent_name, "fake-acp");
        assert_eq!(outcome.agent_version, "0.0.1");
        assert_eq!(outcome.protocol_version, 1);
        assert!(outcome.session_created);
        assert!(!project.path().join("acp.txt").exists());
    }

    #[test]
    fn truncate_does_not_split_utf8_characters() {
        let value = "界".repeat(2_000);
        let truncated = truncate(&value, 4_000);
        assert!(truncated.ends_with("… (truncated)"));
        assert!(truncated.chars().count() < value.chars().count());
        assert_eq!(truncate("short", 4_000), "short");
    }
}
