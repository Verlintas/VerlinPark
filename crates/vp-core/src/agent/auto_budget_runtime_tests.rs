//! Deterministic runtime tests for the Auto Budget controller.
//!
//! These live in a dedicated module (not `agent/tests.rs`) because the auto
//! budget workstream owns this file. Every provider response is scripted; no
//! live network call, model, or clock-dependent assertion is used.

use super::*;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use vp_providers::{ChatMessage, EventStream, ProviderError};
use vp_store::Store;

struct ScriptedProvider {
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
    calls: AtomicUsize,
    delay: Option<Duration>,
    /// Conversation state observed by each provider call. Tests use this to
    /// prove a resumed run continues the same context instead of re-seeding it.
    requests: Mutex<Vec<Vec<ChatMessage>>>,
}

impl ScriptedProvider {
    fn new_with_delay(responses: Vec<Vec<StreamEvent>>, delay: Option<Duration>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::from(responses)),
            calls: AtomicUsize::new(0),
            delay,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn captured_requests(&self) -> Vec<Vec<ChatMessage>> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "auto-budget-scripted"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.messages.clone());
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                vec![
                    StreamEvent::TextDelta("scripted fallback".into()),
                    StreamEvent::Done,
                ]
            });
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

struct Harness {
    _data: TempDir,
    engine: Engine,
    service: AgentService,
    project: Project,
    provider: Arc<ScriptedProvider>,
}

fn init_repo(path: &Path) {
    std::fs::write(path.join("README.md"), "hello from auto budget\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@verlinpark.local"],
        vec!["config", "user.name", "VerlinPark Test"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "initial"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

async fn harness(responses: Vec<Vec<StreamEvent>>, max_turns: u32) -> Harness {
    harness_with_delay(responses, max_turns, None).await
}

async fn harness_with_delay(
    responses: Vec<Vec<StreamEvent>>,
    max_turns: u32,
    delay: Option<Duration>,
) -> Harness {
    let data = TempDir::new().unwrap();
    let repo = data.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("auto-budget-runtime"))
        .unwrap();
    let provider = ScriptedProvider::new_with_delay(responses, delay);
    let config = AgentConfig {
        provider: provider.clone(),
        provider_name: "auto-budget-scripted".into(),
        model: "test".into(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns,
        max_tokens: Some(1024),
        max_managers: 5,
        max_workers: 10,
        max_labour: 20,
        auto_expand_workers: false,
        integration_command: None,
        tools: ToolRegistry::default_tools(),
        max_active_sessions: 4,
        scope_locks: Arc::new(ScopeLocks::with_store(engine.store().clone())),
        capacity: Arc::new(CapacityController::new(
            global_worker_cap(),
            global_labour_cap(),
        )),
        provider_idle_timeout_secs: 300,
        question_timeout_secs: 1800,
        approval_mode: ApprovalMode::default(),
        approval_timeout_secs: 600,
        compact_after_tokens: 0,
        compact_keep_messages: 8,
        compact_keep_tokens: 1_000_000,
        compact_prepare_ratio: 0.6,
        context_window: 0,
    };
    let service = AgentService::configured(engine.clone(), config);
    Harness {
        _data: data,
        engine,
        service,
        project,
        provider,
    }
}

impl Harness {
    async fn create_auto(&self, policy: AutoBudgetPolicy) -> Session {
        self.engine
            .create_session_with_budget_options(
                self.project.id.clone(),
                "auto budget runtime".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(policy),
                0,
                false,
            )
            .await
            .unwrap()
    }

    async fn create_fixed(&self, token_budget: Option<u64>) -> Session {
        self.engine
            .create_session_with_budget_options(
                self.project.id.clone(),
                "fixed budget runtime".into(),
                token_budget,
                None,
                None,
                Some(BudgetMode::Fixed),
                None,
                0,
                false,
            )
            .await
            .unwrap()
    }

    async fn create_unlimited(&self) -> Session {
        self.engine
            .create_session_with_budget_options(
                self.project.id.clone(),
                "unlimited budget runtime".into(),
                None,
                None,
                None,
                Some(BudgetMode::Unlimited),
                None,
                0,
                true,
            )
            .await
            .unwrap()
    }
}

fn usage(input_tokens: u32, output_tokens: u32) -> StreamEvent {
    StreamEvent::Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}

fn tool_turn(
    call_id: &str,
    name: &str,
    arguments: serde_json::Value,
    input_tokens: u32,
    output_tokens: u32,
) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCall(ToolCall {
            id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }),
        usage(input_tokens, output_tokens),
        StreamEvent::Done,
    ]
}

fn final_turn(text: &str, input_tokens: u32, output_tokens: u32) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(text.to_string()),
        usage(input_tokens, output_tokens),
        StreamEvent::Done,
    ]
}

fn read_turn(call_id: &str, input_tokens: u32, output_tokens: u32) -> Vec<StreamEvent> {
    tool_turn(
        call_id,
        "read_file",
        serde_json::json!({"path": "README.md"}),
        input_tokens,
        output_tokens,
    )
}

fn write_turn(
    call_id: &str,
    path: &str,
    content: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Vec<StreamEvent> {
    tool_turn(
        call_id,
        "write_file",
        serde_json::json!({"path": path, "content": content}),
        input_tokens,
        output_tokens,
    )
}

fn multi_tool_turn(
    calls: Vec<(&str, &str, serde_json::Value)>,
    input_tokens: u32,
    output_tokens: u32,
) -> Vec<StreamEvent> {
    let mut events = calls
        .into_iter()
        .map(|(id, name, arguments)| {
            StreamEvent::ToolCall(ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
            })
        })
        .collect::<Vec<_>>();
    events.push(usage(input_tokens, output_tokens));
    events.push(StreamEvent::Done);
    events
}

async fn seed_session_tokens(
    harness: &Harness,
    session: &Session,
    entries: usize,
    input_per_entry: u64,
) {
    for _ in 0..entries {
        harness
            .engine
            .record_agent_usage(
                &session.id,
                "seed-usage",
                "Seed Usage",
                "leader",
                None,
                None,
                "test",
                input_per_entry,
                0,
                false,
                0.0,
            )
            .await
            .unwrap();
    }
}

async fn seed_session_cost(harness: &Harness, session: &Session, cost_usd: f64) {
    harness
        .engine
        .record_agent_usage(
            &session.id,
            "seed-cost",
            "Seed Cost",
            "worker",
            None,
            None,
            "test",
            0,
            0,
            false,
            cost_usd,
        )
        .await
        .unwrap();
}

/// Run one Worker provider call directly through the shared run loop. The
/// scripted provider never touches the network; the helper keeps cost/time
/// request-fit tests focused on the budget path.
async fn run_child_worker(
    harness: &Harness,
    session: &Session,
    control: &RunControl,
    prompt: &str,
) -> LoopOutcome {
    let session_record = harness
        .engine
        .store()
        .get_session(&session.id)
        .unwrap()
        .unwrap();
    let project_record = harness
        .engine
        .store()
        .get_project(&harness.project.id)
        .unwrap()
        .unwrap();
    let config = harness
        .service
        .config
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    let worktree = PathBuf::from(
        session_record
            .worktree
            .clone()
            .expect("auto session has a worktree"),
    );
    let branch = session_record.branch.clone().unwrap_or_default();
    let spec = AgentSpec {
        id: "worker-request-fit".into(),
        role: AgentRole::Worker,
        name: "Worker".into(),
        depth: 1,
        parent_name: Some("Manager".into()),
        title: Some("Worker".into()),
        task_id: None,
        department: Some("engineering".into()),
        identity_id: None,
        identity_generation: None,
        profile: AgentProfile::Coding,
        allow_delegation: true,
        task_kind: Some("coding".into()),
        auto_approve: false,
        worktree: worktree.clone(),
        branch: branch.clone(),
        session_worktree: worktree.clone(),
        session_branch: branch,
        write_scope: Vec::new(),
        base_commit: session_record.base_commit.clone().unwrap_or_default(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let status = harness.engine.session_budget(&session.id).await.unwrap();
    let mut budget = SessionBudget::new(
        session_record.token_budget,
        session_record.tokens_in + session_record.tokens_out,
        AgentCaps {
            managers: config.max_managers,
            workers: config.max_workers,
            labour: config.max_labour,
        },
    )
    .with_cost(
        session_record.cost_budget_usd,
        session_record.cost_spent_usd,
    )
    .with_time(session_record.time_budget_secs)
    .with_unlimited_flag(
        harness
            .engine
            .unlimited_flag(&session_record.id, session_record.unlimited),
    );
    budget = budget.with_cumulative_elapsed(
        status
            .state
            .as_ref()
            .map(|state| state.elapsed_secs)
            .unwrap_or(0),
    );
    budget.apply_status(&status);
    budget.set_remaining_time(status.remaining_secs);
    let mut conversation = vec![ChatMessage::user(prompt)];
    run_loop(
        &harness.engine,
        &config,
        &session_record,
        &project_record,
        control,
        &budget,
        &spec,
        prompt,
        &mut conversation,
        None,
        false,
        None,
    )
    .await
    .unwrap()
}

fn messages_extend(previous: &[ChatMessage], next: &[ChatMessage]) -> bool {
    next.len() >= previous.len()
        && previous.iter().zip(next).all(|(left, right)| {
            left.role == right.role
                && left.content == right.content
                && left.tool_calls == right.tool_calls
                && left.tool_call_id == right.tool_call_id
        })
}

fn count_tool_finished(events: &[vp_protocol::types::Event], name: &str) -> usize {
    events
        .iter()
        .filter(|event| event.kind == "tool.finished" && event.payload["name"] == name)
        .count()
}

async fn wait_for_status_long(engine: &Engine, session_id: &str, statuses: &[&str]) -> Session {
    for _ in 0..6_000 {
        let session = engine.store().get_session(session_id).unwrap().unwrap();
        if statuses.contains(&session.status.as_str()) {
            return session;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let session = engine.store().get_session(session_id).unwrap().unwrap();
    panic!(
        "session {session_id} stayed in status `{}`; expected one of {statuses:?}",
        session.status
    );
}

async fn wait_for_status(engine: &Engine, session_id: &str, statuses: &[&str]) -> Session {
    for _ in 0..2_000 {
        let session = engine.store().get_session(session_id).unwrap().unwrap();
        if statuses.contains(&session.status.as_str()) {
            return session;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let session = engine.store().get_session(session_id).unwrap().unwrap();
    panic!(
        "session {session_id} stayed in status `{}`; expected one of {statuses:?}",
        session.status
    );
}

async fn session_events(engine: &Engine, session_id: &str) -> Vec<vp_protocol::types::Event> {
    // `list_events` clamps each call to 1,000 rows, so a long soak must page.
    // A single call silently hides every event after the first thousand and
    // makes tool/progress assertions look like missing events.
    let mut all = Vec::new();
    let mut since = 0u64;
    loop {
        let page = engine
            .list_events(session_id.to_string(), since, 1_000)
            .await
            .unwrap();
        let Some(last) = page.last() else {
            break;
        };
        let next = last.seq.saturating_add(1);
        all.extend(page);
        if next <= since || all.len() >= 50_000 {
            break;
        }
        since = next;
    }
    all
}

async fn wait_for_event_count(
    engine: &Engine,
    session_id: &str,
    kind: &str,
    expected: usize,
) -> Vec<vp_protocol::types::Event> {
    for _ in 0..2_000 {
        let events = session_events(engine, session_id).await;
        let found = events
            .iter()
            .filter(|event| event.kind == kind)
            .cloned()
            .collect::<Vec<_>>();
        if found.len() >= expected {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("session {session_id} never produced {expected} `{kind}` event(s)");
}

async fn cancel_paused_run(harness: &Harness, session_id: &str) {
    assert!(harness.service.cancel(session_id));
    wait_for_status(&harness.engine, session_id, &["cancelled"]).await;
}

fn auto_policy(initial_tokens: u64, hard_tokens: u64) -> AutoBudgetPolicy {
    AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(initial_tokens),
        hard_token_cap: Some(hard_tokens),
        min_runway_turns: 2,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    }
}

#[test]
fn budget_status_application_preserves_spend_and_uses_cumulative_time() {
    let budget = SessionBudget::new(
        Some(100),
        80,
        AgentCaps {
            managers: 1,
            workers: 1,
            labour: 1,
        },
    )
    .with_cost(Some(1.0), 0.25)
    .with_time(Some(60))
    .with_cumulative_elapsed(10);
    let status = SessionBudgetStatus {
        session_id: "ses-budget".into(),
        mode: BudgetMode::Auto,
        unlimited: false,
        effective_token_budget: Some(250),
        effective_cost_budget_usd: Some(2.0),
        effective_time_budget_secs: Some(60),
        hard_token_cap: Some(500),
        hard_cost_cap_usd: Some(4.0),
        hard_wall_clock_secs: Some(600),
        spent_tokens: 80,
        spent_cost_usd: 0.25,
        elapsed_secs: 10,
        remaining_tokens: Some(170),
        remaining_cost_usd: Some(1.75),
        remaining_secs: Some(50),
        policy: None,
        state: Some(SessionAutoBudgetState {
            elapsed_secs: 10,
            ..SessionAutoBudgetState::default()
        }),
        forecast: vp_protocol::types::BudgetForecast::default(),
        updated_at: None,
    };
    budget.apply_status(&status);
    assert_eq!(budget.token_limit(), Some(250));
    assert_eq!(budget.cost_limit(), Some(2.0));
    assert_eq!(budget.spent(), 80);
    assert_eq!(budget.cost_spent(), 0.25);
    assert_eq!(budget.elapsed_secs(), 10);
    budget.set_remaining_time(Some(40));
    assert_eq!(budget.time_limit(), Some(Duration::from_secs(50)));
    assert_eq!(budget.spent(), 80);
    assert_eq!(budget.cost_spent(), 0.25);
    assert!(
        matches!(budget.remaining_time_secs(), Some(39 | 40)),
        "remaining time can lose one second to the active clock: {:?}",
        budget.remaining_time_secs()
    );

    let before_pause = budget.elapsed();
    budget.pause_clock();
    assert!(budget.is_clock_paused());
    std::thread::sleep(Duration::from_millis(20));
    let after_pause = budget.elapsed();
    assert!(
        after_pause.saturating_sub(before_pause) < Duration::from_millis(5),
        "paused clock advanced: before={before_pause:?} after={after_pause:?}"
    );
    budget.resume_clock();
    assert!(!budget.is_clock_paused());
    assert!(budget.elapsed() >= before_pause);
}

#[tokio::test]
async fn auto_extension_records_event_and_updates_the_live_limit() {
    let harness = harness(
        vec![
            read_turn("call-read-1", 60, 20),
            final_turn("extension complete", 60, 20),
        ],
        20,
    )
    .await;
    // A real request is roughly 12k tokens once the system prompt and tool
    // schemas are counted. Use a production-scale allowance so the first
    // request fits only after one extension.
    let session = harness.create_auto(auto_policy(11_000, 100_000)).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 7,
                total_turns: 1,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();
    harness.service.start(&session.id, "work").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    assert_eq!(harness.provider.calls(), 2);
    let events = session_events(&harness.engine, &session.id).await;
    let extension = events
        .iter()
        .find(|event| event.kind == "session.budget.auto_extended")
        .expect("auto extension event");
    let new_budget = extension.payload["new"]["token_budget"]
        .as_u64()
        .expect("extended token budget");
    assert!(
        new_budget > 11_000 && new_budget <= 100_000,
        "extension must widen the live allowance within the hard cap: {new_budget}"
    );
    assert_eq!(extension.payload["extensions"], 1);

    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.extensions, 1);
    assert_eq!(record.state.total_turns, 3);
    assert!(
        record.state.elapsed_secs >= 7,
        "elapsed base was lost during extension: {}",
        record.state.elapsed_secs
    );
    let status = harness.engine.session_budget(&session.id).await.unwrap();
    assert_eq!(status.effective_token_budget, Some(new_budget));
    assert!(
        status.remaining_tokens.unwrap_or(0) > 0,
        "remaining tokens must stay positive after the two scripted calls"
    );
}

#[tokio::test]
async fn hard_cap_pauses_instead_of_failing_or_switching_to_unlimited() {
    let harness = harness(
        vec![
            read_turn("call-read-1", 60, 20),
            final_turn("must not be requested", 60, 20),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(auto_policy(1_000_000, 1_000_000)).await;
    // Spend the whole initial allowance before the run so there is no room to
    // extend. The runtime must pause before the first provider call.
    seed_session_tokens(&harness, &session, 10, 100_000).await;
    harness.service.start(&session.id, "work").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    let pauses = wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(pauses[0].payload["reason"], "budget_cap");
    assert_eq!(
        harness.provider.calls(),
        0,
        "no provider call may run after the hard cap is reached"
    );

    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.paused_reason.as_deref(), Some("budget_cap"));
    assert!(record.state.paused_at.is_some());
    let status = harness.engine.session_budget(&session.id).await.unwrap();
    assert!(!status.unlimited);
    assert_eq!(status.mode, BudgetMode::Auto);

    cancel_paused_run(&harness, &session.id).await;
}

#[tokio::test]
async fn operator_top_up_applies_at_the_next_turn_and_reaches_idle() {
    let harness = harness(
        vec![
            read_turn("call-read-1", 60, 20),
            final_turn("top-up complete", 60, 20),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(auto_policy(1_000_000, 1_000_000)).await;
    seed_session_tokens(&harness, &session, 10, 100_000).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 123,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();
    harness.service.start(&session.id, "work").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;

    // A top-up that leaves the hard cap at 1M is clamped, so the next resume
    // must pause again instead of failing or overshooting.
    let clamped = harness
        .engine
        .set_session_budget(vp_protocol::types::SetSessionBudgetParams {
            session_id: session.id.clone(),
            token_budget: Some(1_500_000),
            ..vp_protocol::types::SetSessionBudgetParams::default()
        })
        .await
        .unwrap();
    assert_eq!(clamped.budget.effective_token_budget, Some(1_000_000));
    assert!(harness.service.resume(&session.id).await.unwrap());
    wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 2).await;
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    assert_eq!(harness.provider.calls(), 0);

    // Raising the cap and the effective budget makes the next resume proceed.
    let raised = harness
        .engine
        .set_session_budget(vp_protocol::types::SetSessionBudgetParams {
            session_id: session.id.clone(),
            token_budget: Some(2_000_000),
            hard_token_cap: Some(2_000_000),
            ..vp_protocol::types::SetSessionBudgetParams::default()
        })
        .await
        .unwrap();
    assert_eq!(raised.budget.effective_token_budget, Some(2_000_000));
    assert!(harness.service.resume(&session.id).await.unwrap());
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    assert_eq!(harness.provider.calls(), 2);
    let resumed =
        wait_for_event_count(&harness.engine, &session.id, "session.auto_resumed", 2).await;
    assert!(
        resumed
            .iter()
            .all(|event| event.payload["operator_reset"] == true)
    );
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.paused_reason, None);
    assert_eq!(record.state.extensions, 0);
    assert!(
        record.state.elapsed_secs >= 123,
        "pause/resume reset cumulative elapsed time: {}",
        record.state.elapsed_secs
    );
}

#[tokio::test]
async fn no_progress_read_only_turns_pause_with_bounded_evidence() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(1_000_000),
        min_runway_turns: 1,
        max_turns_per_run: 10,
        no_progress_turns: 2,
        no_progress_action: vp_protocol::types::NoProgressAction::Pause,
        warn_at_ratio: 0.0,
        pause_at_ratio: 0.0,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(
        vec![
            read_turn("call-read-1", 10, 1),
            read_turn("call-read-2", 10, 1),
            final_turn("must not be requested", 10, 1),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(policy).await;
    harness
        .service
        .start(&session.id, "read only")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    let pauses = wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(pauses[0].payload["reason"], "no_progress");
    assert_eq!(harness.provider.calls(), 2);
    let evidence = &pauses[0].payload["details"];
    assert_eq!(evidence["schema"], "verlinpark.no_progress/v1");
    assert!(evidence["recent_evidence"].is_array());
    assert!(evidence["recent_evidence"].as_array().unwrap().len() <= 8);

    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.turns_without_progress, 2);
    assert_eq!(record.state.paused_reason.as_deref(), Some("no_progress"));
    cancel_paused_run(&harness, &session.id).await;
}

#[tokio::test]
async fn durable_progress_evidence_resets_the_no_progress_counter() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(1_000_000),
        min_runway_turns: 1,
        max_turns_per_run: 10,
        no_progress_turns: 3,
        no_progress_action: vp_protocol::types::NoProgressAction::Pause,
        warn_at_ratio: 0.0,
        pause_at_ratio: 0.0,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(
        vec![
            read_turn("call-read-1", 10, 1),
            tool_turn(
                "call-artifact",
                "publish_artifact",
                serde_json::json!({
                    "name": "progress.txt",
                    "content": "durable progress evidence\n"
                }),
                10,
                1,
            ),
            read_turn("call-read-2", 10, 1),
            final_turn("reset complete", 10, 1),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(policy).await;
    harness.service.start(&session.id, "mixed").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    assert_eq!(harness.provider.calls(), 4);
    let pauses = session_events(&harness.engine, &session.id)
        .await
        .into_iter()
        .filter(|event| event.kind == "session.auto_paused")
        .count();
    assert_eq!(pauses, 0);
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    // Turn 1 read-only, turn 2 published an artifact (reset), turn 3
    // read-only, turn 4 final text without durable evidence.
    assert_eq!(record.state.turns_without_progress, 2);
    assert_eq!(record.state.total_turns, 4);
    let progress = record.state.progress.expect("persisted tracker");
    assert!(progress["evidence_count"].as_u64().unwrap_or(0) >= 1);
}

#[tokio::test]
async fn no_progress_escalation_records_a_note_without_pausing() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(1_000_000),
        min_runway_turns: 1,
        max_turns_per_run: 10,
        no_progress_turns: 1,
        no_progress_action: vp_protocol::types::NoProgressAction::Escalate,
        warn_at_ratio: 0.0,
        pause_at_ratio: 0.0,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(
        vec![
            read_turn("call-read-1", 10, 1),
            final_turn("escalated", 10, 1),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(policy).await;
    harness
        .service
        .start(&session.id, "read only")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    let events = session_events(&harness.engine, &session.id).await;
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.auto_paused")
    );
    let note = events
        .iter()
        .find(|event| event.kind == "coordination.note" && event.payload["action"] == "escalate")
        .expect("escalation coordination note");
    assert_eq!(note.payload["severity"], "warning");
    assert!(
        note.payload["message"]
            .as_str()
            .unwrap()
            .contains("no-progress")
    );
    assert_eq!(harness.provider.calls(), 2);
}

#[tokio::test]
async fn cumulative_turns_and_elapsed_time_survive_a_second_run() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(100_000),
        hard_token_cap: Some(200_000),
        initial_wall_clock_secs: Some(600),
        hard_wall_clock_secs: Some(60_000),
        min_runway_turns: 1,
        max_turns_per_run: 2,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(
        vec![
            read_turn("call-read-1", 10, 1),
            read_turn("call-read-2", 10, 1),
            read_turn("call-read-3", 10, 1),
            read_turn("call-read-4", 10, 1),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(policy).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 123,
                total_turns: 7,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();

    harness.service.start(&session.id, "first").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["failed"]).await;
    assert_eq!(harness.provider.calls(), 2, "policy per-run turn cap");
    let first = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.state.total_turns, 9);
    assert!(
        first.state.elapsed_secs >= 123,
        "cumulative elapsed time was reset: {}",
        first.state.elapsed_secs
    );

    harness.service.start(&session.id, "second").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["failed"]).await;
    assert_eq!(harness.provider.calls(), 4);
    let second = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.state.total_turns, 11);
    assert!(
        second.state.elapsed_secs >= first.state.elapsed_secs,
        "cumulative elapsed time went backwards"
    );
    assert!(second.state.elapsed_secs >= 123);
}

#[tokio::test]
async fn max_total_turns_pauses_with_a_typed_reason() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(100_000),
        hard_token_cap: Some(200_000),
        min_runway_turns: 1,
        max_turns_per_run: 2,
        max_total_turns: Some(2),
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(
        vec![
            read_turn("call-read-1", 10, 1),
            final_turn("must not be requested", 10, 1),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(policy).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                total_turns: 1,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();
    harness.service.start(&session.id, "work").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    let pauses = wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(pauses[0].payload["reason"], "max_total_turns");
    assert_eq!(pauses[0].payload["details"]["limit"], 2);
    assert_eq!(harness.provider.calls(), 1);
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.total_turns, 2);
    cancel_paused_run(&harness, &session.id).await;
}

#[tokio::test]
async fn wall_clock_is_extended_when_time_is_the_limiting_dimension() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(2_000_000),
        initial_wall_clock_secs: Some(2),
        hard_wall_clock_secs: Some(32),
        min_runway_turns: 2,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(vec![final_turn("time extension complete", 10, 1)], 20).await;
    let session = harness.create_auto(policy).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 1,
                total_turns: 1,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();
    harness.service.start(&session.id, "time").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    let events = session_events(&harness.engine, &session.id).await;
    let extension = events
        .iter()
        .find(|event| event.kind == "session.budget.auto_extended")
        .expect("wall-clock auto extension");
    assert_eq!(extension.payload["new"]["time_budget_secs"], 3);
    assert!(
        extension.payload["reason"]
            .as_str()
            .unwrap()
            .contains("time")
    );
    assert_eq!(extension.payload["new"]["token_budget"], 1_000_000);
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.extensions, 1);
    assert!(record.state.elapsed_secs >= 1);
    let status = harness.engine.session_budget(&session.id).await.unwrap();
    assert_eq!(status.effective_time_budget_secs, Some(3));
}

#[tokio::test]
async fn policy_warn_ratio_emits_one_forecast_warning_per_crossing() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(120_000),
        hard_token_cap: Some(200_000),
        min_runway_turns: 1,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        warn_at_ratio: 0.20,
        pause_at_ratio: 0.05,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(
        vec![
            read_turn("call-read-1", 20_000, 5_000),
            final_turn("warning complete", 10, 1),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(policy).await;
    // Seed most of the initial allowance so the second turn crosses the warn
    // ratio while the first request still fits. A real request is ~12k tokens.
    seed_session_tokens(&harness, &session, 9, 10_000).await;
    harness.service.start(&session.id, "warn").await.unwrap();
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    let events = session_events(&harness.engine, &session.id).await;
    let warnings = events
        .iter()
        .filter(|event| event.kind == "session.budget.forecast_warning")
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let warning = warnings[0];
    assert_eq!(
        warning.payload["schema"],
        "verlinpark.auto_budget_warning/v1"
    );
    assert_eq!(warning.payload["warn_at_ratio"], 0.20);
    assert!(warning.payload["forecast"]["remaining_tokens"].is_number());
    assert!(warning.payload["dimensions"]["tokens"]["ratio"].is_number());
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.auto_paused")
    );
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.extensions, 1);
}

#[tokio::test]
async fn truncated_no_progress_scan_never_false_pauses_and_later_progress_is_seen() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(1_000_000),
        min_runway_turns: 1,
        max_turns_per_run: 10,
        no_progress_turns: 1,
        no_progress_action: vp_protocol::types::NoProgressAction::Pause,
        warn_at_ratio: 0.0,
        pause_at_ratio: 0.0,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(
        vec![
            read_turn("call-read-1", 10, 1),
            final_turn("scan complete", 10, 1),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(policy).await;
    // More than one bounded scan window of non-progress events, followed by a
    // progress event that the first truncated window cannot reach.
    for index in 0..5_100 {
        harness
            .engine
            .append_event(
                &session.id,
                "agent.message",
                serde_json::json!({"text": format!("filler {index}")}),
            )
            .await
            .unwrap();
    }
    harness
        .engine
        .append_event(
            &session.id,
            "artifact.published",
            serde_json::json!({
                "artifact_id": "artifact-progress",
                "version": 1,
                "sha256": "deadbeef"
            }),
        )
        .await
        .unwrap();

    harness
        .service
        .start(&session.id, "truncated")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    assert_eq!(harness.provider.calls(), 2);
    let events = session_events(&harness.engine, &session.id).await;
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.auto_paused")
    );
    let warning = events
        .iter()
        .find(|event| event.kind == "session.auto_state_warning")
        .expect("truncated scan warning");
    assert_eq!(warning.payload["code"], "NO_PROGRESS_SCAN_TRUNCATED");
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    let progress = record.state.progress.expect("persisted tracker");
    assert!(progress["evidence_count"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(record.state.turns_without_progress, 0);
}

#[test]
fn extension_detection_uses_cost_and_time_runways_not_only_token_forecast() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        hard_token_cap: Some(1_000_000),
        hard_cost_cap_usd: Some(10.0),
        hard_wall_clock_secs: Some(1_000),
        min_runway_turns: 2,
        max_turns_per_run: 10,
        ..AutoBudgetPolicy::default()
    };
    let mut status = SessionBudgetStatus {
        session_id: "ses-runway".into(),
        mode: BudgetMode::Auto,
        unlimited: false,
        effective_token_budget: None,
        effective_cost_budget_usd: Some(10.0),
        effective_time_budget_secs: Some(1_000),
        hard_token_cap: Some(1_000_000),
        hard_cost_cap_usd: Some(10.0),
        hard_wall_clock_secs: Some(1_000),
        spent_tokens: 0,
        spent_cost_usd: 9.0,
        elapsed_secs: 100,
        remaining_tokens: None,
        remaining_cost_usd: Some(1.0),
        remaining_secs: Some(900),
        policy: Some(policy.clone()),
        state: None,
        forecast: vp_protocol::types::BudgetForecast {
            avg_cost_usd_per_call: 2.0,
            ..vp_protocol::types::BudgetForecast::default()
        },
        updated_at: None,
    };
    // Cost runway is 0 turns while time is comfortable.
    assert!(auto_budget_needs_extension(&status, &policy, 100, 10));
    // Cost is comfortable but an observed 10s/turn leaves only one time turn.
    status.remaining_cost_usd = Some(9.0);
    status.forecast.avg_cost_usd_per_call = 0.01;
    status.remaining_secs = Some(10);
    assert!(auto_budget_needs_extension(&status, &policy, 100, 10));
    // Both runways are comfortable: no extension.
    status.remaining_secs = Some(900);
    assert!(!auto_budget_needs_extension(&status, &policy, 100, 10));

    // A low token forecast with no token hard cap is not extendable and must
    // not trigger a CapReached pause when the only hard cap (time) is fine.
    let mut uncapped_token = status.clone();
    uncapped_token.hard_token_cap = None;
    uncapped_token.hard_cost_cap_usd = None;
    uncapped_token.hard_wall_clock_secs = Some(1_000);
    uncapped_token.effective_token_budget = Some(100);
    uncapped_token.remaining_tokens = Some(10);
    uncapped_token.effective_cost_budget_usd = None;
    uncapped_token.remaining_cost_usd = None;
    uncapped_token.effective_time_budget_secs = Some(1_000);
    uncapped_token.remaining_secs = Some(900);
    uncapped_token.forecast.avg_input_tokens_per_call = 20;
    uncapped_token.forecast.avg_output_tokens_per_call = 0;
    uncapped_token.forecast.avg_cost_usd_per_call = 0.0;
    assert!(!auto_budget_needs_extension(
        &uncapped_token,
        &policy,
        100,
        10
    ));
}

#[tokio::test]
async fn paused_time_does_not_consume_the_auto_wall_clock() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(2_000_000),
        initial_wall_clock_secs: Some(2),
        hard_wall_clock_secs: Some(2),
        min_runway_turns: 2,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness_with_delay(
        vec![final_turn("resumed after pause", 10, 1)],
        20,
        Some(Duration::from_millis(1_100)),
    )
    .await;
    let session = harness.create_auto(policy).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 1,
                total_turns: 1,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();
    harness
        .service
        .start(&session.id, "time pause")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(harness.provider.calls(), 0);

    // Deliberately wait while paused, then raise the effective window. Paused
    // wall-clock time must not have been consumed by the session.
    tokio::time::sleep(Duration::from_millis(3_000)).await;
    harness
        .engine
        .set_session_budget(vp_protocol::types::SetSessionBudgetParams {
            session_id: session.id.clone(),
            time_budget_secs: Some(5),
            hard_wall_clock_secs: Some(5),
            ..vp_protocol::types::SetSessionBudgetParams::default()
        })
        .await
        .unwrap();
    assert!(harness.service.resume(&session.id).await.unwrap());
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;
    assert_eq!(harness.provider.calls(), 1);

    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        record.state.elapsed_secs >= 2,
        "active turn time was not counted: {}",
        record.state.elapsed_secs
    );
    assert!(
        record.state.elapsed_secs < 4,
        "paused wall-clock time leaked into the budget: {}",
        record.state.elapsed_secs
    );
    let pauses = wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(pauses.len(), 1);
    let status = harness.engine.session_budget(&session.id).await.unwrap();
    assert_eq!(status.effective_time_budget_secs, Some(5));
}

#[tokio::test]
async fn uncapped_token_runway_does_not_false_pause_a_time_only_policy() {
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        // No token hard cap: this effective token budget is finite but cannot
        // be auto-extended. Only the wall clock is extendable.
        hard_token_cap: None,
        initial_wall_clock_secs: Some(1_000),
        hard_wall_clock_secs: Some(4_000),
        min_runway_turns: 2,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let harness = harness(
        vec![
            read_turn("call-read-1", 60, 20),
            final_turn("token runway ignored", 10, 1),
        ],
        20,
    )
    .await;
    let session = harness.create_auto(policy).await;
    harness
        .service
        .start(&session.id, "uncapped token")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    assert_eq!(harness.provider.calls(), 2);
    let events = session_events(&harness.engine, &session.id).await;
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.auto_paused"),
        "an uncapped token runway must not trigger a CapReached pause"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.budget.auto_extended")
    );
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.extensions, 0);
    assert_eq!(record.state.total_turns, 2);
    let status = harness.engine.session_budget(&session.id).await.unwrap();
    assert_eq!(status.effective_token_budget, Some(1_000_000));
    assert_eq!(status.hard_token_cap, None);
    assert_eq!(status.hard_wall_clock_secs, Some(4_000));
}

#[tokio::test]
async fn fixed_and_unlimited_sessions_never_auto_extend() {
    let fixed = harness(
        vec![
            read_turn("call-read-1", 100_000, 0),
            final_turn("fixed done", 150_000, 0),
        ],
        20,
    )
    .await;
    let fixed_session = fixed.create_fixed(Some(200_000)).await;
    fixed
        .service
        .start(&fixed_session.id, "fixed")
        .await
        .unwrap();
    wait_for_status(&fixed.engine, &fixed_session.id, &["failed", "idle"]).await;
    assert_eq!(fixed.provider.calls(), 2);
    let fixed_events = session_events(&fixed.engine, &fixed_session.id).await;
    assert!(
        !fixed_events
            .iter()
            .any(|event| event.kind == "session.budget.auto_extended")
    );
    assert!(
        !fixed_events
            .iter()
            .any(|event| event.kind == "session.auto_paused")
    );
    assert!(
        fixed
            .engine
            .session_auto_budget(&fixed_session.id)
            .await
            .unwrap()
            .is_none()
    );

    let unlimited = harness(
        vec![
            read_turn("call-read-1", 250_000, 0),
            final_turn("unlimited done", 250_000, 0),
        ],
        20,
    )
    .await;
    let unlimited_session = unlimited.create_unlimited().await;
    unlimited
        .service
        .start(&unlimited_session.id, "unlimited")
        .await
        .unwrap();
    wait_for_status(&unlimited.engine, &unlimited_session.id, &["idle"]).await;
    assert_eq!(unlimited.provider.calls(), 2);
    let unlimited_events = session_events(&unlimited.engine, &unlimited_session.id).await;
    assert!(
        !unlimited_events
            .iter()
            .any(|event| event.kind == "session.budget.auto_extended")
    );
    assert!(
        unlimited
            .engine
            .session_auto_budget(&unlimited_session.id)
            .await
            .unwrap()
            .is_none()
    );
    let status = unlimited
        .engine
        .session_budget(&unlimited_session.id)
        .await
        .unwrap();
    assert_eq!(status.mode, BudgetMode::Unlimited);
    assert!(status.unlimited);
}

#[tokio::test]
async fn soak_fifty_read_only_turns_then_progress_extends_without_false_pause() {
    const READ_TURNS: usize = 50;
    const EXPECTED_TURNS: u32 = READ_TURNS as u32 + 2;
    let mut responses = (0..READ_TURNS)
        .map(|index| read_turn(&format!("soak-read-{index}"), 60, 20))
        .collect::<Vec<_>>();
    responses.push(write_turn(
        "soak-write",
        "soak-progress.txt",
        "durable progress after fifty reads\n",
        60,
        20,
    ));
    responses.push(final_turn("soak complete", 60, 20));
    let harness = harness(responses, 20).await;
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(2_000_000),
        min_runway_turns: 2,
        max_turns_per_run: 70,
        no_progress_turns: 60,
        no_progress_action: vp_protocol::types::NoProgressAction::Pause,
        auto_tool_bundle: false,
        warn_at_ratio: 0.0,
        pause_at_ratio: 0.0,
        ..AutoBudgetPolicy::default()
    };
    let session = harness.create_auto(policy).await;
    // Pre-charge most of the initial budget so the first turn must extend.
    // Recording consumes no provider call and keeps the actual turn script
    // exactly fifty reads plus write and final.
    seed_session_tokens(&harness, &session, 25, 39_600).await;

    harness
        .service
        .start(&session.id, "fifty reads then progress")
        .await
        .unwrap();
    wait_for_status_long(&harness.engine, &session.id, &["idle"]).await;

    assert_eq!(
        harness.provider.calls(),
        EXPECTED_TURNS as usize,
        "each scripted read/write/final response is exactly one provider call"
    );
    let events = session_events(&harness.engine, &session.id).await;
    let pauses = events
        .iter()
        .filter(|event| event.kind == "session.auto_paused")
        .count();
    assert_eq!(
        pauses, 0,
        "read-only turns below the no-progress threshold must not pause the soak"
    );
    let extensions = events
        .iter()
        .filter(|event| event.kind == "session.budget.auto_extended")
        .collect::<Vec<_>>();
    assert!(
        !extensions.is_empty(),
        "low token runway must auto-extend at least once"
    );
    assert_eq!(extensions[0].payload["extensions"], 1);
    assert_eq!(count_tool_finished(&events, "read_file"), READ_TURNS);
    assert_eq!(count_tool_finished(&events, "write_file"), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.committed")
            .count(),
        0
    );

    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.total_turns, EXPECTED_TURNS);
    assert_eq!(record.state.extensions, 1);
    assert_eq!(record.state.paused_reason, None);
    assert_eq!(
        record.state.turns_without_progress, 1,
        "only the final text turn follows the durable write"
    );
    let progress = record.state.progress.expect("persisted progress tracker");
    assert_eq!(progress["last_progress_kind"], "file_changed");
    assert!(progress["evidence_count"].as_u64().unwrap_or(0) >= 1);

    let status = harness.engine.session_budget(&session.id).await.unwrap();
    assert_eq!(status.mode, BudgetMode::Auto);
    assert_eq!(status.effective_token_budget, Some(1_250_000));
    assert_eq!(status.hard_token_cap, Some(2_000_000));
    assert_eq!(status.remaining_tokens, Some(255_840));

    let worktree = session.worktree.expect("session worktree");
    assert_eq!(
        std::fs::read_to_string(Path::new(&worktree).join("soak-progress.txt")).unwrap(),
        "durable progress after fifty reads\n"
    );
}

#[tokio::test]
async fn soak_pause_raise_budget_resume_continues_context_without_duplicate_write() {
    let harness = harness(
        vec![
            write_turn(
                "soak-resume-write",
                "soak-resume.txt",
                "exactly one durable write\n",
                60_000,
                100,
            ),
            final_turn("resumed after operator top-up", 10, 1),
        ],
        20,
    )
    .await;
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(300_000),
        hard_token_cap: Some(300_000),
        forecast_calls: 1,
        min_runway_turns: 2,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        auto_tool_bundle: false,
        warn_at_ratio: 0.0,
        pause_at_ratio: 0.0,
        ..AutoBudgetPolicy::default()
    };
    let session = harness.create_auto(policy).await;
    // Fifty small ledger rows leave the newest sample cheap so the first call
    // is admitted, while the scripted 60k-token turn empties the runway.
    seed_session_tokens(&harness, &session, 50, 3_000).await;

    harness
        .service
        .start(&session.id, "soak pause and resume")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    let pauses = wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(pauses[0].payload["reason"], "budget_cap");
    assert_eq!(
        harness.provider.calls(),
        1,
        "a hard-cap pause must not request another provider turn"
    );

    let paused_record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(paused_record.state.total_turns, 1);
    let first_requests = harness.provider.captured_requests();
    assert_eq!(first_requests.len(), 1);

    let raised = harness
        .engine
        .set_session_budget(vp_protocol::types::SetSessionBudgetParams {
            session_id: session.id.clone(),
            token_budget: Some(500_000),
            hard_token_cap: Some(500_000),
            ..vp_protocol::types::SetSessionBudgetParams::default()
        })
        .await
        .unwrap();
    assert_eq!(raised.budget.effective_token_budget, Some(500_000));
    assert_eq!(raised.budget.hard_token_cap, Some(500_000));
    assert!(harness.service.resume(&session.id).await.unwrap());
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    assert_eq!(
        harness.provider.calls(),
        2,
        "one pre-pause and one resumed call"
    );
    let requests = harness.provider.captured_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        messages_extend(&requests[0], &requests[1]),
        "the resumed provider call must continue the pre-pause conversation"
    );
    assert!(
        requests[0].iter().any(|message| {
            message.role == "user" && message.content == "soak pause and resume"
        })
    );
    assert!(
        requests[1].iter().any(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("soak-resume-write")
        }),
        "the resumed call must see the completed write tool result"
    );

    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.state.total_turns, 2,
        "resume must accumulate turns instead of resetting the counter"
    );
    assert_eq!(record.state.paused_reason, None);
    let events = session_events(&harness.engine, &session.id).await;
    assert_eq!(
        count_tool_finished(&events, "write_file"),
        1,
        "resume must not replay the completed write"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.committed")
            .count(),
        0
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.auto_resumed")
            .count(),
        1
    );

    let worktree = session.worktree.expect("session worktree");
    assert_eq!(
        std::fs::read_to_string(Path::new(&worktree).join("soak-resume.txt")).unwrap(),
        "exactly one durable write\n"
    );
}

#[tokio::test]
async fn soak_cumulative_wall_clock_excludes_pause_and_repauses_at_the_cap() {
    let harness = harness_with_delay(
        vec![
            read_turn("soak-clock-read", 10, 1),
            final_turn("clock soak complete", 10, 1),
        ],
        20,
        Some(Duration::from_millis(1_100)),
    )
    .await;
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(2_000_000),
        initial_wall_clock_secs: Some(2),
        hard_wall_clock_secs: Some(2),
        min_runway_turns: 2,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        auto_tool_bundle: false,
        warn_at_ratio: 0.0,
        pause_at_ratio: 0.0,
        ..AutoBudgetPolicy::default()
    };
    let session = harness.create_auto(policy).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 1,
                total_turns: 1,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();

    harness
        .service
        .start(&session.id, "soak cumulative clock")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    let first_pause =
        wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(first_pause[0].payload["reason"], "budget_cap");
    assert_eq!(
        harness.provider.calls(),
        0,
        "the cap pause happens before the first provider call"
    );

    let first_pause_record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    // Waiting for an operator decision is not active session time.
    tokio::time::sleep(Duration::from_millis(3_500)).await;
    let while_paused = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        while_paused.state.elapsed_secs, first_pause_record.state.elapsed_secs,
        "persisted elapsed time must not advance during a pause"
    );

    harness
        .engine
        .set_session_budget(vp_protocol::types::SetSessionBudgetParams {
            session_id: session.id.clone(),
            time_budget_secs: Some(3),
            hard_wall_clock_secs: Some(3),
            ..vp_protocol::types::SetSessionBudgetParams::default()
        })
        .await
        .unwrap();
    assert!(harness.service.resume(&session.id).await.unwrap());
    wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 2).await;
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    assert_eq!(
        harness.provider.calls(),
        1,
        "the resumed run makes one call, then the raised cap must stop it again"
    );

    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        record.state.elapsed_secs > first_pause_record.state.elapsed_secs,
        "active resumed turn time was not counted: {}",
        record.state.elapsed_secs
    );
    assert!(
        record.state.elapsed_secs < first_pause_record.state.elapsed_secs + 4,
        "paused wall-clock time leaked into the budget: {}",
        record.state.elapsed_secs
    );
    assert_eq!(record.state.paused_reason.as_deref(), Some("budget_cap"));
    assert!(record.state.total_turns >= 2);
    let status = harness.engine.session_budget(&session.id).await.unwrap();
    assert_eq!(status.effective_time_budget_secs, Some(3));

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        harness.provider.calls(),
        1,
        "no provider call may occur while paused at the raised hard cap"
    );
    cancel_paused_run(&harness, &session.id).await;
}

#[tokio::test]
async fn soak_late_progress_before_the_decision_prevents_a_no_progress_pause() {
    let harness = harness(
        vec![
            read_turn("soak-late-read-1", 10, 1),
            multi_tool_turn(
                vec![
                    (
                        "soak-late-read-2",
                        "read_file",
                        serde_json::json!({"path": "README.md"}),
                    ),
                    (
                        "soak-late-artifact",
                        "publish_artifact",
                        serde_json::json!({
                            "name": "late-progress.txt",
                            "content": "late durable progress\n"
                        }),
                    ),
                ],
                10,
                1,
            ),
            final_turn("late progress complete", 10, 1),
        ],
        20,
    )
    .await;
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_token_budget: Some(1_000_000),
        hard_token_cap: Some(2_000_000),
        min_runway_turns: 1,
        max_turns_per_run: 10,
        no_progress_turns: 2,
        no_progress_action: vp_protocol::types::NoProgressAction::Pause,
        auto_tool_bundle: false,
        warn_at_ratio: 0.0,
        pause_at_ratio: 0.0,
        ..AutoBudgetPolicy::default()
    };
    let session = harness.create_auto(policy).await;

    harness
        .service
        .start(&session.id, "late progress")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;

    assert_eq!(harness.provider.calls(), 3);
    let events = session_events(&harness.engine, &session.id).await;
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.auto_paused"),
        "a progress event in the crossing turn must be considered before the pause decision"
    );
    let last_read_seq = events
        .iter()
        .filter(|event| {
            event.kind == "tool.finished"
                && event.payload["name"] == "read_file"
                && event.payload["tool_call_id"] == "soak-late-read-2"
        })
        .map(|event| event.seq)
        .max()
        .expect("second read tool.finished");
    let artifact = events
        .iter()
        .find(|event| event.kind == "artifact.published")
        .expect("artifact.published progress event");
    assert!(
        artifact.seq > last_read_seq,
        "the progress event must arrive after the crossing read and before the decision"
    );

    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.total_turns, 3);
    assert_eq!(
        record.state.turns_without_progress, 1,
        "only the final text turn remains without progress"
    );
    assert_eq!(record.state.last_progress_seq, artifact.seq);
    let progress = record.state.progress.expect("persisted progress tracker");
    assert_eq!(progress["last_progress_kind"], "artifact_published");
    assert!(progress["evidence_count"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn resource_governor_decisions_map_to_bounded_turn_pause_reasons() {
    let decision =
        |action: &str, reason: &str| crate::engine::resources::ResourceGovernorDecision {
            session_id: "ses-resource".into(),
            phase: "turn".into(),
            action: action.into(),
            reason: reason.into(),
            bottleneck: Some("attention".into()),
            details: serde_json::json!({"states": []}),
            generated_at: "2026-11-24T00:00:00Z".into(),
        };
    assert!(
        resource_turn_pause_reason(&decision("continue", ""), false).is_none(),
        "continue never pauses"
    );
    assert!(
        resource_turn_pause_reason(&decision("warn", "soft_resource_limit"), false).is_none(),
        "warn is advisory and never pauses"
    );
    assert_eq!(
        resource_turn_pause_reason(&decision("defer", "attention_cap"), false).as_deref(),
        Some("attention_cap")
    );
    assert_eq!(
        resource_turn_pause_reason(&decision("pause", ""), false).as_deref(),
        Some("resource_pause")
    );
    assert_eq!(
        resource_turn_pause_reason(&decision("stop", "risk_ceiling"), false).as_deref(),
        Some("risk_ceiling")
    );
    // Readiness is the R2a new-work gate; an active run is already under the
    // approval/effect/merge/Verifier gates and must be able to finish a turn.
    assert!(resource_turn_pause_reason(&decision("defer", "readiness_blocked"), false).is_none());
    // Auto budgets extend on demand, so a reserve measured against the hard
    // cap must not pause a healthy fresh Auto session.
    assert!(
        resource_turn_pause_reason(&decision("defer", "completion_reserve_tokens"), true).is_none()
    );
    assert_eq!(
        resource_turn_pause_reason(&decision("defer", "completion_reserve_tokens"), false)
            .as_deref(),
        Some("completion_reserve_tokens")
    );
    for role in [
        AgentRole::Leader,
        AgentRole::Manager,
        AgentRole::Worker,
        AgentRole::Labour,
        AgentRole::Scout,
    ] {
        assert!(
            resource_turn_pause_applies(role, false),
            "{role:?} must respect a turn-boundary resource block"
        );
    }
    assert!(!resource_turn_pause_applies(AgentRole::Verifier, false));
    assert!(!resource_turn_pause_applies(AgentRole::Manager, true));
}

#[test]
fn critical_path_gate_does_not_defer_verification_or_scout() {
    assert!(critical_path_gate_applies(AgentRole::Leader));
    assert!(critical_path_gate_applies(AgentRole::Manager));
    assert!(critical_path_gate_applies(AgentRole::Worker));
    assert!(critical_path_gate_applies(AgentRole::Labour));
    assert!(!critical_path_gate_applies(AgentRole::Verifier));
    assert!(!critical_path_gate_applies(AgentRole::Scout));
}

#[tokio::test]
async fn worker_request_fit_extends_the_auto_budget_without_owning_turns() {
    let harness = harness(vec![final_turn("worker complete", 40, 8)], 10).await;
    let session = harness.create_auto(auto_policy(11_000, 100_000)).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 5,
                total_turns: 7,
                turns_without_progress: 3,
                last_progress_seq: 11,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();
    let control = RunControl::new();
    let outcome = run_child_worker(&harness, &session, &control, "worker request fit").await;
    assert!(matches!(outcome, LoopOutcome::Final(_)));
    assert_eq!(harness.provider.calls(), 1);

    let events = session_events(&harness.engine, &session.id).await;
    let extension = events
        .iter()
        .find(|event| {
            event.kind == "session.budget.auto_extended" && event.payload["reason"] == "request_fit"
        })
        .expect("worker request-fit auto extension");
    assert!(
        extension.payload["new"]["token_budget"]
            .as_u64()
            .is_some_and(|budget| budget > 11_000),
        "the worker request must extend the shared effective budget: {extension:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.auto_paused"),
        "a successful request-fit extension must not pause the session"
    );
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.state.total_turns, 7,
        "a child run must not advance Leader-owned turn accounting"
    );
    assert_eq!(record.state.turns_without_progress, 3);
    assert_eq!(record.state.last_progress_seq, 11);
    assert_eq!(record.state.extensions, 1);
}

#[tokio::test]
async fn cost_only_auto_child_request_fit_extends_the_cost_budget() {
    let harness = harness(vec![final_turn("cost child complete", 40, 8)], 10).await;
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_cost_budget_usd: Some(1.0),
        hard_cost_cap_usd: Some(10.0),
        min_runway_turns: 4,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let session = harness.create_auto(policy).await;
    // A child that starts with five cents left and a $0.95 forecast per call
    // must extend before the provider request instead of crossing the cap.
    seed_session_cost(&harness, &session, 0.95).await;
    let control = RunControl::new();
    let outcome = run_child_worker(&harness, &session, &control, "cost child").await;
    assert!(matches!(outcome, LoopOutcome::Final(_)));
    assert_eq!(harness.provider.calls(), 1);

    let events = session_events(&harness.engine, &session.id).await;
    let extension = events
        .iter()
        .find(|event| {
            event.kind == "session.budget.auto_extended" && event.payload["reason"] == "request_fit"
        })
        .expect("cost request-fit auto extension");
    assert!(
        extension.payload["new"]["cost_budget_usd"]
            .as_f64()
            .is_some_and(|budget| budget > 1.0),
        "the child request must extend the shared cost budget: {extension:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.auto_paused"),
        "a successful cost extension must not pause the session"
    );
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.extensions, 1);
    assert_eq!(
        record.state.total_turns, 0,
        "a child run must not advance Leader-owned turn accounting"
    );
}

#[tokio::test]
async fn time_only_auto_child_request_fit_extends_the_wall_clock() {
    let harness = harness(vec![final_turn("time child complete", 40, 8)], 10).await;
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_wall_clock_secs: Some(1_000),
        hard_wall_clock_secs: Some(4_000),
        min_runway_turns: 4,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let session = harness.create_auto(policy).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 980,
                total_turns: 5,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();
    let control = RunControl::new();
    let outcome = run_child_worker(&harness, &session, &control, "time child").await;
    assert!(matches!(outcome, LoopOutcome::Final(_)));
    assert_eq!(harness.provider.calls(), 1);

    let events = session_events(&harness.engine, &session.id).await;
    let extension = events
        .iter()
        .find(|event| {
            event.kind == "session.budget.auto_extended" && event.payload["reason"] == "request_fit"
        })
        .expect("time request-fit auto extension");
    assert!(
        extension.payload["new"]["time_budget_secs"]
            .as_u64()
            .is_some_and(|budget| budget > 1_000),
        "the child request must extend the shared wall clock: {extension:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.auto_paused"),
        "a successful time extension must not pause the session"
    );
    let record = harness
        .engine
        .session_auto_budget(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state.extensions, 1);
    assert_eq!(record.state.total_turns, 5);
}

#[tokio::test]
async fn cost_only_auto_child_request_fit_pauses_at_the_hard_cost_cap() {
    let harness = harness(vec![final_turn("must not be requested", 10, 1)], 10).await;
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_cost_budget_usd: Some(1.0),
        hard_cost_cap_usd: Some(1.0),
        min_runway_turns: 4,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let session = harness.create_auto(policy).await;
    seed_session_cost(&harness, &session, 1.0).await;
    let control = RunControl::new();
    let run = run_child_worker(&harness, &session, &control, "cost pause");
    tokio::select! {
        _ = run => panic!("child run returned instead of pausing at the cost cap"),
        _ = wait_for_status(&harness.engine, &session.id, &["paused"]) => {}
    }
    assert_eq!(
        harness.provider.calls(),
        0,
        "no provider call may run while the hard cost cap is reached"
    );
    let pauses = wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(pauses[0].payload["reason"], "budget_cap");
    assert!(
        pauses[0].payload["details"]["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dimension| dimension == "cost"),
        "the pause must name the cost dimension: {}",
        pauses[0].payload
    );
}

#[tokio::test]
async fn time_only_auto_child_request_fit_pauses_at_the_hard_wall_clock_cap() {
    let harness = harness(vec![final_turn("must not be requested", 10, 1)], 10).await;
    let policy = AutoBudgetPolicy {
        enabled: true,
        initial_wall_clock_secs: Some(1_000),
        hard_wall_clock_secs: Some(1_000),
        min_runway_turns: 4,
        max_turns_per_run: 10,
        no_progress_turns: 100,
        ..AutoBudgetPolicy::default()
    };
    let session = harness.create_auto(policy).await;
    harness
        .engine
        .update_session_auto_budget_state(
            &session.id,
            SessionAutoBudgetState {
                elapsed_secs: 1_000,
                total_turns: 5,
                ..SessionAutoBudgetState::default()
            },
        )
        .await
        .unwrap();
    let control = RunControl::new();
    let run = run_child_worker(&harness, &session, &control, "time pause");
    tokio::select! {
        _ = run => panic!("child run returned instead of pausing at the time cap"),
        _ = wait_for_status(&harness.engine, &session.id, &["paused"]) => {}
    }
    assert_eq!(
        harness.provider.calls(),
        0,
        "no provider call may run while the hard wall-clock cap is reached"
    );
    let pauses = wait_for_event_count(&harness.engine, &session.id, "session.auto_paused", 1).await;
    assert_eq!(pauses[0].payload["reason"], "budget_cap");
    assert!(
        pauses[0].payload["details"]["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dimension| dimension == "time"),
        "the pause must name the time dimension: {}",
        pauses[0].payload
    );
}

#[tokio::test]
async fn turn_boundary_resource_block_pauses_and_resumes_the_same_run() {
    let harness = harness(vec![final_turn("resource pause complete", 10, 2)], 10).await;
    let session = harness.create_fixed(Some(1_000_000)).await;
    harness
        .engine
        .set_run_contract(vp_protocol::types::SetRunContractParams {
            session_id: session.id.clone(),
            profile: vp_protocol::types::RunProfile::Balanced,
            completion_reserve_ratio: None,
            attention_cap: Some(1),
            stop_policy: None,
            updated_by: None,
        })
        .await
        .unwrap();
    let (_approval, _receiver) = harness
        .engine
        .request_approval_with_risk(
            &session.id,
            "call-resource-pause",
            "shell",
            serde_json::json!({"command": "git status"}),
            None,
            Some("host_state"),
            Some("resource pause test"),
        )
        .await
        .unwrap();

    harness
        .service
        .start(&session.id, "resource pause")
        .await
        .unwrap();
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    let pauses = wait_for_event_count(
        &harness.engine,
        &session.id,
        SESSION_RESOURCE_PAUSED_EVENT,
        1,
    )
    .await;
    assert_eq!(pauses[0].payload["phase"], "turn");
    assert_eq!(pauses[0].payload["action"], "defer");
    assert_eq!(pauses[0].payload["reason"], "attention_cap");
    assert_eq!(
        harness.provider.calls(),
        0,
        "no provider call may run after a turn-boundary resource block"
    );

    // A bare resume does not bypass an attention budget that is still at its
    // cap; the same run pauses again instead of silently spending more.
    assert!(harness.service.resume(&session.id).await.unwrap());
    let pauses = wait_for_event_count(
        &harness.engine,
        &session.id,
        SESSION_RESOURCE_PAUSED_EVENT,
        2,
    )
    .await;
    wait_for_status(&harness.engine, &session.id, &["paused"]).await;
    assert_eq!(pauses[1].payload["reason"], "attention_cap");
    assert_eq!(harness.provider.calls(), 0);

    // Raising the operator attention cap releases the block. The same run
    // continues without a new prompt or a restarted worktree.
    harness
        .engine
        .set_run_contract(vp_protocol::types::SetRunContractParams {
            session_id: session.id.clone(),
            profile: vp_protocol::types::RunProfile::Balanced,
            completion_reserve_ratio: None,
            attention_cap: Some(100),
            stop_policy: None,
            updated_by: None,
        })
        .await
        .unwrap();
    assert!(harness.service.resume(&session.id).await.unwrap());
    wait_for_status(&harness.engine, &session.id, &["idle"]).await;
    assert_eq!(harness.provider.calls(), 1);
    let events = session_events(&harness.engine, &session.id).await;
    assert!(
        events.iter().any(|event| event.kind == "agent.resumed"),
        "the same run must visibly resume"
    );
    assert!(
        !events.iter().any(|event| event.kind == "agent.cancelled"),
        "a resource pause must never cancel or discard the run"
    );
}
