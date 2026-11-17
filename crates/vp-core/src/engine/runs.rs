use super::*;

mod diagnostics;
mod export;
mod import;
mod repair;
mod replay;
mod support;

impl Engine {
    pub async fn set_session_run_policy(
        &self,
        project_id: &str,
        policy: vp_protocol::types::SessionRunPolicy,
    ) -> Result<vp_protocol::types::SessionRunPolicyResult> {
        let policy = crate::run_journal::set_policy(self, project_id, &policy).await?;
        Ok(vp_protocol::types::SessionRunPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn session_run_policy(
        &self,
        project_id: &str,
    ) -> Result<vp_protocol::types::SessionRunPolicyResult> {
        let policy = crate::run_journal::policy(self, project_id).await?;
        Ok(vp_protocol::types::SessionRunPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn session_run_detail(
        &self,
        run_id: &str,
    ) -> Result<vp_protocol::types::SessionRunDetailResult> {
        let (run, steps, tool_calls, checkpoints) =
            crate::run_journal::run_detail(self, run_id).await?;
        Ok(vp_protocol::types::SessionRunDetailResult {
            run,
            steps,
            tool_calls,
            checkpoints,
        })
    }

    pub async fn session_runs(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<vp_protocol::types::SessionRunsResult> {
        let runs = crate::run_journal::session_runs(self, session_id, limit).await?;
        Ok(vp_protocol::types::SessionRunsResult {
            session_id: session_id.to_string(),
            runs,
        })
    }

    pub async fn recent_events(&self, session_id: &str, limit: u32) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.recent_events(&id, limit)).await?
    }

    pub async fn recent_events_of_kind(
        &self,
        session_id: &str,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || store.recent_events_by_kind(&id, &kind, limit)).await?
    }

    /// Reconstruct the top-level conversation from the durable event stream,
    /// not from a live in-memory vector. Malformed message events fail closed.
    pub async fn conversation_from_events(
        &self,
        session_id: &str,
    ) -> Result<(Vec<vp_providers::ChatMessage>, Vec<Option<String>>)> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let events = tokio::task::spawn_blocking(move || store.list_all_events(&id)).await??;
        crate::agent::conversation_from_events(&events, session_id)
    }
}

struct ReplayData {
    session: Session,
    messages: Vec<StoredMessage>,
    plan: Option<String>,
    todos: Vec<TodoItem>,
    agents: Vec<AgentRecord>,
    approvals: Vec<Approval>,
    queue: Vec<MergeQueueEntry>,
    agent_messages: Vec<AgentMessage>,
    artifacts: Vec<Artifact>,
    usage: Vec<UsageEntry>,
    memories: Vec<MemoryItem>,
    memory_conflicts: Vec<MemoryConflict>,
    questions: Vec<Question>,
    progress: Vec<(u64, ProgressRound)>,
    tasks: Vec<ScheduledTask>,
    steering: Vec<SteeringMessage>,
    continuations: Vec<SessionContinuation>,
    gates: Vec<GateRecord>,
    recoveries: Vec<OrchestrationRecovery>,
    notes: Vec<OrchestrationNote>,
    decisions: Vec<OrchestrationDecision>,
    scaling_decisions: Vec<vp_protocol::scaling::ScalingDecision>,
    runs: Vec<vp_protocol::types::SessionRun>,
    run_steps: Vec<vp_protocol::types::RunStep>,
    run_checkpoints: Vec<vp_protocol::types::RunCheckpoint>,
    context_checkpoints: Vec<vp_protocol::types::ContextCheckpoint>,
    processes: Vec<vp_protocol::types::SupervisedProcess>,
    test_results: Vec<vp_protocol::test_cache::TestCacheEntry>,
    remote_test_evidence: Vec<RemoteTestEvidence>,
    instructions: Vec<vp_protocol::types::SessionInstructionState>,
    contracts: Vec<StoredContractRow>,
    contract_usage: Vec<ContractUsageRow>,
    effects: Vec<EffectReceipt>,
    policy_snapshots: Vec<vp_protocol::runtime::PolicySnapshot>,
    plan_runtime_snapshot: Option<String>,
    prompt_prefix_observations: Vec<PromptPrefixObservation>,
    prompt_prefix_aggregates: Vec<PromptPrefixAggregate>,
    prompt_prefix_inconsistent_epochs: Vec<String>,
    events: Vec<Event>,
}

struct ExpectedArtifact {
    artifact: Artifact,
    full: bool,
    has_storage: bool,
    has_size: bool,
}
