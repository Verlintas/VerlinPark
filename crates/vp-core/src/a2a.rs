//! Loopback-only A2A facade lifecycle.
//!
//! The wire/runtime implementation lives in `vp-a2a`. This module owns the
//! daemon policy, the one-time bearer token, and the explicit start/stop
//! surface. Inbound tasks are deliberately **card-only** in this version: the
//! handler rejects them with `rejected` because no project capability contract
//! has been configured. Starting the facade is host-state approval-classified
//! and binds only loopback.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use vp_a2a::{
    A2aHandler, A2aLimits, A2aPolicy as RuntimePolicy, A2aRequestContext, A2aResult, A2aServer,
    A2aStartHandle, A2aTaskUpdate, Message, TaskState, TaskStatus,
};
use vp_protocol::a2a::{A2aPolicy, A2aStartResult, A2aStatus, a2a_codes};

const DEFAULT_CARD_NAME: &str = "VerlinPark";
const DEFAULT_CARD_DESCRIPTION: &str = "Loopback-only VerlinPark A2A facade. Inbound tasks are disabled until a capability contract is configured.";
const DEFAULT_TOKEN_BYTES: usize = 32;

struct CardOnlyHandler {
    tasks: std::sync::Arc<Mutex<Vec<vp_protocol::a2a::A2aTaskSummary>>>,
    max_tasks: usize,
}

impl A2aHandler for CardOnlyHandler {
    async fn handle(&self, request: A2aRequestContext) -> A2aResult<A2aTaskUpdate> {
        if let Ok(mut tasks) = self.tasks.lock() {
            let max_tasks = self.max_tasks.max(1);
            while tasks.len() >= max_tasks {
                tasks.remove(0);
            }
            tasks.push(vp_protocol::a2a::A2aTaskSummary {
                id: request.task.id.clone(),
                context_id: request.task.context_id.clone(),
                state: TaskState::Rejected.as_str().to_string(),
                updated_at: vp_protocol::now_rfc3339(),
                message_count: request.task.history.len().min(u32::MAX as usize) as u32,
                artifact_count: request.task.artifacts.len().min(u32::MAX as usize) as u32,
            });
        }
        let message = Message::agent_text(
            format!("a2a-answer-{}", request.task_id),
            "Inbound A2A tasks are disabled: no capability contract is configured for this project. No local tool, file, shell, network, or model authority was granted.",
        );
        Ok(A2aTaskUpdate::new(TaskState::Rejected)
            .with_status(TaskStatus::new(TaskState::Rejected).with_message(message)))
    }
}

struct RunningFacade {
    handle: A2aStartHandle,
    token_present: bool,
    token_issued: bool,
    started_at: String,
}

/// Daemon-owned A2A lifecycle. The facade is disabled by default and is never
/// started by a scheduler or an agent.
pub struct A2aRuntime {
    policy: Mutex<A2aPolicy>,
    tasks: std::sync::Arc<Mutex<Vec<vp_protocol::a2a::A2aTaskSummary>>>,
    running: Mutex<Option<RunningFacade>>,
    last_error: Mutex<Option<String>>,
}

impl Default for A2aRuntime {
    fn default() -> Self {
        Self {
            policy: Mutex::new(A2aPolicy::default()),
            tasks: std::sync::Arc::new(Mutex::new(Vec::new())),
            running: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }
}

impl A2aRuntime {
    pub fn status(&self) -> A2aStatus {
        let policy = self
            .policy
            .lock()
            .map(|policy| policy.clone())
            .unwrap_or_default();
        let last_error = self
            .last_error
            .lock()
            .map(|error| error.clone())
            .unwrap_or_default();
        let running = self.running.lock().ok().and_then(|running| {
            running.as_ref().map(|facade| {
                let url = facade.handle.url().to_string();
                (
                    url.clone(),
                    format!("{url}/.well-known/agent-card.json"),
                    facade.token_present,
                    facade.token_issued,
                    facade.started_at.clone(),
                )
            })
        });
        let mut status = A2aStatus {
            enabled: policy.enabled,
            running: running.is_some(),
            inbound_enabled: false,
            url: running.as_ref().map(|(url, _, _, _, _)| url.clone()),
            card_url: running.as_ref().map(|(_, card, _, _, _)| card.clone()),
            task_count: self
                .tasks
                .lock()
                .map(|tasks| tasks.len().min(u32::MAX as usize) as u32)
                .unwrap_or(0),
            max_tasks: policy.max_tasks,
            token_present: running
                .as_ref()
                .is_some_and(|(_, _, present, _, _)| *present),
            token_issued: running.as_ref().is_some_and(|(_, _, _, issued, _)| *issued),
            last_error,
            policy,
        };
        if status.running {
            status.inbound_enabled = false;
        }
        status
    }

    pub fn set_policy(&self, policy: A2aPolicy) -> Result<A2aStatus> {
        let policy = policy
            .normalized()
            .map_err(|error| anyhow!("{}: {error}", a2a_codes::A2A_INVALID_POLICY))?;
        if self
            .running
            .lock()
            .map_err(|_| anyhow!("A2A running lock poisoned"))?
            .is_some()
        {
            bail!(
                "{}: stop the A2A facade before changing its policy",
                a2a_codes::A2A_ALREADY_RUNNING
            );
        }
        let max_tasks = policy.max_tasks as usize;
        *self
            .policy
            .lock()
            .map_err(|_| anyhow!("A2A policy lock poisoned"))? = policy;
        if let Ok(mut tasks) = self.tasks.lock() {
            let excess = tasks.len().saturating_sub(max_tasks);
            if excess > 0 {
                tasks.drain(0..excess);
            }
        }
        Ok(self.status())
    }

    pub fn card(&self) -> Option<serde_json::Value> {
        self.running.lock().ok().and_then(|running| {
            running
                .as_ref()
                .and_then(|facade| serde_json::to_value(facade.handle.card()).ok())
        })
    }

    pub async fn start(&self, approved: bool, policy: Option<A2aPolicy>) -> Result<A2aStartResult> {
        if !approved {
            bail!(
                "{}: A2A start requires explicit operator approval",
                a2a_codes::A2A_APPROVAL_REQUIRED
            );
        }
        let mut policy = policy.unwrap_or_else(|| {
            self.policy
                .lock()
                .map(|policy| policy.clone())
                .unwrap_or_default()
        });
        policy = policy
            .normalized()
            .map_err(|error| anyhow!("{}: {error}", a2a_codes::A2A_INVALID_POLICY))?;
        if !policy.enabled {
            bail!(
                "{}: enable the A2A policy before starting",
                a2a_codes::A2A_DISABLED
            );
        }
        if policy.allow_inbound {
            bail!(
                "{}: inbound A2A requires a capability contract, which is not configured; keep allow_inbound=false",
                a2a_codes::A2A_INBOUND_DENIED
            );
        }
        if let Ok(running) = self.running.lock()
            && running.is_some()
        {
            bail!(
                "{}: A2A facade is already running",
                a2a_codes::A2A_ALREADY_RUNNING
            );
        }
        {
            let mut stored = self
                .policy
                .lock()
                .map_err(|_| anyhow!("A2A policy lock poisoned"))?;
            *stored = policy.clone();
        }
        let token = random_token(DEFAULT_TOKEN_BYTES)?;
        let mut runtime_policy = RuntimePolicy::new(
            DEFAULT_CARD_NAME,
            DEFAULT_CARD_DESCRIPTION,
            vp_protocol::APP_VERSION,
        )
        .with_listen(listen_endpoint(&policy.bind_host, policy.port))
        .map_err(|error| anyhow!("{}: {error}", a2a_codes::A2A_INVALID_POLICY))?
        .with_bearer_token(token.clone())
        .map_err(|error| anyhow!("{}: {error}", a2a_codes::A2A_INVALID_POLICY))?
        .with_limits(A2aLimits {
            max_stored_tasks: policy.max_tasks as usize,
            max_task_ttl: Duration::from_secs(policy.task_ttl_secs),
            ..Default::default()
        });
        runtime_policy = runtime_policy.enable();
        let handle = match A2aServer::start(
            runtime_policy,
            CardOnlyHandler {
                tasks: std::sync::Arc::clone(&self.tasks),
                max_tasks: policy.max_tasks as usize,
            },
        )
        .await
        {
            Ok(handle) => handle,
            Err(error) => {
                if let Ok(mut last) = self.last_error.lock() {
                    *last = Some(error.public_message());
                }
                bail!("{}: {error}", a2a_codes::A2A_INVALID_POLICY);
            }
        };
        let issued = handle.take_token();
        // The plaintext is handed to the operator once; the server keeps only
        // its hash, so the token remains configured for the facade lifetime.
        let token_present = issued.is_some();
        if issued.as_deref() != Some(token.as_str()) {
            let _ = handle.shutdown().await;
            bail!(
                "{}: A2A token handoff failed closed",
                a2a_codes::A2A_TOKEN_MISSING
            );
        }
        let started_at = vp_protocol::now_rfc3339();
        *self
            .running
            .lock()
            .map_err(|_| anyhow!("A2A running lock poisoned"))? = Some(RunningFacade {
            handle,
            token_present,
            token_issued: true,
            started_at,
        });
        if let Ok(mut last) = self.last_error.lock() {
            *last = None;
        }
        Ok(A2aStartResult {
            status: self.status(),
            token: Some(token),
        })
    }

    pub fn tasks(&self, limit: u32, state: Option<&str>) -> vp_protocol::a2a::A2aTaskListResult {
        let max_tasks = self
            .policy
            .lock()
            .map(|policy| policy.max_tasks as usize)
            .unwrap_or(1)
            .max(1);
        let limit = limit.clamp(1, vp_protocol::a2a::MAX_A2A_TASK_PAGE) as usize;
        let limit = limit.min(max_tasks);
        let tasks = self
            .tasks
            .lock()
            .map(|tasks| {
                tasks
                    .iter()
                    .rev()
                    .filter(|task| state.is_none_or(|state| task.state == state))
                    .take(limit + 1)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let truncated = tasks.len() > limit;
        vp_protocol::a2a::A2aTaskListResult {
            tasks: tasks.into_iter().take(limit).collect(),
            truncated,
        }
    }

    pub async fn stop(&self, reason: Option<&str>) -> Result<A2aStatus> {
        // `reason` is an operator note for the caller/audit trail. It is not a
        // failure, so it must never populate `last_error`.
        if let Some(reason) = reason.map(str::trim).filter(|reason| !reason.is_empty()) {
            tracing::debug!(%reason, "A2A facade stopped");
        }
        let facade = self
            .running
            .lock()
            .map_err(|_| anyhow!("A2A running lock poisoned"))?
            .take();
        if let Some(facade) = facade {
            facade
                .handle
                .shutdown()
                .await
                .map_err(|error| anyhow!("{}: {error}", a2a_codes::A2A_NOT_RUNNING))?;
        }
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.clear();
        }
        Ok(self.status())
    }
}

/// Format a validated loopback host/port pair. IPv6 literals require brackets
/// before they can be parsed as a socket address.
fn listen_endpoint(bind_host: &str, port: u16) -> String {
    if bind_host.contains(':') {
        format!("[{bind_host}]:{port}")
    } else {
        format!("{bind_host}:{port}")
    }
}

fn random_token(bytes: usize) -> Result<String> {
    use rand::RngCore;
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    let mut token = String::with_capacity(bytes * 2);
    for byte in buffer {
        use std::fmt::Write as _;
        let _ = write!(&mut token, "{byte:02x}");
    }
    if token.len() != bytes * 2 {
        bail!("A2A token generation failed");
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_status_is_disabled_and_has_no_token() {
        let runtime = A2aRuntime::default();
        let status = runtime.status();
        assert!(!status.enabled);
        assert!(!status.running);
        assert!(!status.inbound_enabled);
        assert!(!status.token_present);
        assert!(status.url.is_none());
        let json = serde_json::to_value(&status).unwrap();
        assert!(json.get("token").is_none());
    }

    #[test]
    fn public_policy_rejects_public_bind() {
        let policy = A2aPolicy {
            enabled: true,
            bind_host: "0.0.0.0".into(),
            ..A2aPolicy::default()
        };
        assert!(policy.normalized().is_err());
    }

    #[test]
    fn random_tokens_are_unique_and_bounded() {
        let first = random_token(32).unwrap();
        let second = random_token(32).unwrap();
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn status_never_exposes_a_token_and_starts_disabled() {
        let runtime = A2aRuntime::default();
        let status = runtime.status();
        assert!(!status.enabled);
        assert!(!status.running);
        assert!(!status.token_present);
        let json = serde_json::to_value(&status).unwrap();
        assert!(json.get("token").is_none());
        assert!(runtime.card().is_none());
        assert!(runtime.tasks(10, None).tasks.is_empty());
    }

    #[test]
    fn policy_set_is_rejected_while_running_shape_is_validated() {
        let runtime = A2aRuntime::default();
        let policy = A2aPolicy {
            enabled: true,
            bind_host: "192.168.1.1".into(),
            ..A2aPolicy::default()
        };
        assert!(runtime.set_policy(policy).is_err());
    }

    #[test]
    fn listen_endpoint_brackets_ipv6_and_leaves_ipv4_plain() {
        assert_eq!(listen_endpoint("127.0.0.1", 0), "127.0.0.1:0");
        assert_eq!(listen_endpoint("127.0.0.1", 7777), "127.0.0.1:7777");
        assert_eq!(listen_endpoint("::1", 0), "[::1]:0");
        assert_eq!(listen_endpoint("::1", 7777), "[::1]:7777");
    }

    #[test]
    fn random_token_shape_is_fixed() {
        for _ in 0..8 {
            let token = random_token(16).unwrap();
            assert_eq!(token.len(), 32);
            assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
        }
    }
}
