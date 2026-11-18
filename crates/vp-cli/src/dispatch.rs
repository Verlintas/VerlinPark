//! Command dispatch for the `vp` binary.
//!
//! `run_command` routes each parsed command to a command-family module; the
//! arms inside those modules are kept verbatim from the historical
//! single-file entrypoint.
//!
//! `None` means the arm already printed its own output and the caller must not
//! print the returned value again.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use std::path::Path;
use vp_protocol::discovery::Discovery;
use vp_protocol::remote::{
    RemoteIntegrationPolicy, RemoteTestKind, RemoteTestPolicy, RemoteTestProfile,
};

use crate::cli::*;
use crate::render::*;
use crate::rpc::*;
use crate::*;

mod acp;
mod agent;
mod approval;
mod artifact;
mod backup;
mod cache;
mod checkpoint;
mod gate;
mod git;
mod mailbox;
mod mcp;
mod memory;
mod preview;
mod project;
mod remote;
mod runtime;
mod session;
mod skill;
mod source;
mod system;
mod task;
mod team;
mod testing;

#[cfg(test)]
pub(crate) use preview::streamed_exit_error;

pub(crate) async fn run_command(
    command: Command,
    discovery: Discovery,
    json: bool,
) -> Result<Option<Value>> {
    match &command {
        Command::DbRebuild { .. }
        | Command::Backup { .. }
        | Command::Restore { .. }
        | Command::BackupList { .. }
        | Command::BackupPrune { .. }
        | Command::BackupVerify { .. }
        | Command::BackupKeyGenerate { .. }
        | Command::BackupKeyList { .. }
        | Command::BackupKeyEvents { .. }
        | Command::BackupKeyRotate { .. }
        | Command::BackupKeyRevoke { .. }
        | Command::BackupRekey { .. }
        | Command::BackupKeyExportPublic { .. }
        | Command::BackupKeyImport { .. }
        | Command::BackupKeyring { .. }
        | Command::BackupAnchor { .. }
        | Command::BackupAnchorExport { .. }
        | Command::BackupAnchorVerify { .. }
        | Command::BackupAnchorImport { .. }
        | Command::Bench { .. }
        | Command::CacheBench { .. }
        | Command::DaemonAgent { .. }
        | Command::ToolCodes { .. } => backup::dispatch(command, discovery, json).await,
        Command::Ping
        | Command::OtelStatus { .. }
        | Command::OtelPolicy { .. }
        | Command::OtelPolicySet { .. }
        | Command::OtelExport { .. }
        | Command::OtelTest { .. }
        | Command::Metrics { .. }
        | Command::Info
        | Command::Doctor
        | Command::ProviderTest
        | Command::A2aStatus
        | Command::A2aCard
        | Command::A2aPolicySet { .. }
        | Command::A2aTasks { .. }
        | Command::A2aStart { .. }
        | Command::A2aStop { .. } => system::dispatch(command, discovery, json).await,
        Command::AcpAgents
        | Command::AcpAdd { .. }
        | Command::AcpUpdate { .. }
        | Command::AcpRemove { .. }
        | Command::AcpProbe { .. } => acp::dispatch(command, discovery, json).await,
        Command::McpServers
        | Command::McpAdd { .. }
        | Command::McpUpdate { .. }
        | Command::McpProbe { .. }
        | Command::McpRemove { .. }
        | Command::McpTools { .. }
        | Command::McpCall { .. }
        | Command::McpTasks { .. }
        | Command::McpTask { .. }
        | Command::McpTaskResult { .. }
        | Command::McpTaskCancel { .. }
        | Command::McpResources { .. }
        | Command::McpReadResource { .. }
        | Command::McpPrompts { .. }
        | Command::McpPrompt { .. }
        | Command::McpPins { .. }
        | Command::McpPinApprove { .. }
        | Command::McpPinReject { .. } => mcp::dispatch(command, discovery, json).await,
        Command::Projects
        | Command::ProjectAdd { .. }
        | Command::ProjectBudget { .. }
        | Command::ProjectUsage { .. }
        | Command::ProjectCache { .. }
        | Command::ProjectBudgetSet { .. }
        | Command::Governor { .. }
        | Command::CriticalPath { .. }
        | Command::GovernorPreview { .. }
        | Command::GovernorPolicy { .. }
        | Command::GovernorPolicySet { .. }
        | Command::CleanupWorktrees { .. }
        | Command::TaskScheduler { .. }
        | Command::TaskSchedulerSet { .. }
        | Command::Scaling { .. }
        | Command::ScalingPreview { .. }
        | Command::ScalingPolicy { .. }
        | Command::ScalingPolicySet { .. }
        | Command::Dashboard { .. }
        | Command::ModelPresets
        | Command::ModelPresetCreate { .. }
        | Command::ModelPresetUpdate { .. }
        | Command::ModelPresetDelete { .. }
        | Command::UpdateCheck { .. }
        | Command::WorktreePolicy { .. }
        | Command::WorktreePolicySet { .. } => project::dispatch(command, discovery, json).await,
        Command::HostList
        | Command::HostSet { .. }
        | Command::HostRemove { .. }
        | Command::HostProbe { .. }
        | Command::RemoteStatus { .. }
        | Command::Lsp { .. }
        | Command::RemoteWorktrees { .. }
        | Command::RemoteLedger { .. }
        | Command::RemoteIntegrate { .. }
        | Command::RemoteBootstrap { .. }
        | Command::RemoteBootstrapStatus { .. }
        | Command::RemoteTest { .. }
        | Command::RemoteTestStatus { .. }
        | Command::RemoteTestOutput { .. }
        | Command::RemoteTestPolicy { .. }
        | Command::RemoteTestPolicySet { .. }
        | Command::RemoteIntegration { .. }
        | Command::RemoteIntegrationCleanup { .. }
        | Command::RemoteIntegrationPolicy { .. }
        | Command::RemoteIntegrationPolicySet { .. } => {
            remote::dispatch(command, discovery, json).await
        }
        Command::Skills { .. }
        | Command::SkillCreate { .. }
        | Command::SkillActivations { .. }
        | Command::SkillImport { .. }
        | Command::SkillEnabled { .. }
        | Command::SkillDelete { .. } => skill::dispatch(command, discovery, json).await,
        Command::Sources { .. }
        | Command::SourceGet { .. }
        | Command::SourceNote { .. }
        | Command::SourceAddFile { .. }
        | Command::SourceAddUrl { .. }
        | Command::SourceImport { .. }
        | Command::SourceRefresh { .. }
        | Command::SourceReindex { .. }
        | Command::SourceDelete { .. }
        | Command::SourceUpdate { .. }
        | Command::SourceCollections { .. }
        | Command::SourceMove { .. }
        | Command::SourceLabels { .. }
        | Command::SourceSearch { .. }
        | Command::SourceEmbedding { .. }
        | Command::SourceEmbeddingReindex { .. }
        | Command::SourceEmbeddingPolicy { .. }
        | Command::SourceEmbeddingPolicySet { .. }
        | Command::SourceRetrievalPolicy { .. }
        | Command::SourceRetrievalPolicySet { .. }
        | Command::SourceIndex { .. }
        | Command::SourceIndexRebuild { .. } => source::dispatch(command, discovery, json).await,
        Command::OrchestrationPolicy { .. }
        | Command::OrchestrationPolicySet { .. }
        | Command::Orchestration { .. }
        | Command::PlanRevisions { .. }
        | Command::OrchestrationOverview { .. }
        | Command::Orchestrate { .. }
        | Command::Coordination { .. }
        | Command::Unblock { .. }
        | Command::PlanRevision { .. }
        | Command::PlanRestore { .. }
        | Command::TaskEdit { .. }
        | Command::PlanFanout { .. }
        | Command::TeamPlanRevise { .. }
        | Command::Leases
        | Command::LeasesSweep
        | Command::LeaseHistory { .. }
        | Command::PlanMigrate { .. }
        | Command::OrchestrateReplay { .. }
        | Command::Progress { .. }
        | Command::ProgressReport { .. }
        | Command::TeamPlan { .. } => team::dispatch(command, discovery, json).await,
        Command::Computer { .. }
        | Command::Gui { .. }
        | Command::GuiPolicy { .. }
        | Command::GuiPolicySet { .. }
        | Command::GuiTimelineAudit { .. }
        | Command::GuiTimelineStatus { .. }
        | Command::GuiTimelinePrune { .. }
        | Command::ShellPolicy { .. }
        | Command::ShellPolicySet { .. }
        | Command::ProcessPolicy { .. }
        | Command::ProcessPolicySet { .. }
        | Command::Processes { .. }
        | Command::ProcessLogs { .. }
        | Command::ProcessRestart { .. }
        | Command::ProcessStop { .. }
        | Command::Preempt { .. }
        | Command::Preemption { .. }
        | Command::PreemptionPolicy { .. }
        | Command::Preemptions { .. }
        | Command::PreemptionPolicySet { .. } => runtime::dispatch(command, discovery, json).await,
        Command::CheckpointPolicy { .. }
        | Command::CheckpointPolicySet { .. }
        | Command::Checkpoint { .. }
        | Command::Checkpoints { .. }
        | Command::CheckpointStats { .. }
        | Command::CheckpointRestore { .. }
        | Command::CheckpointDelete { .. }
        | Command::CheckpointPrune { .. }
        | Command::ContextCheckpoints { .. }
        | Command::ContextCheckpoint { .. }
        | Command::ContextCheckpointDetail { .. }
        | Command::ContextRestore { .. }
        | Command::ContextCheckpointDelete { .. }
        | Command::ContextCheckpointPrune { .. }
        | Command::ContextCheckpointPolicy { .. }
        | Command::ContextCheckpointPolicySet { .. }
        | Command::Fork { .. }
        | Command::Lineage { .. }
        | Command::ForkTree { .. }
        | Command::ForkForest { .. }
        | Command::ForkAudit { .. }
        | Command::ForkRepair { .. }
        | Command::Compare { .. }
        | Command::ForkMerge { .. } => checkpoint::dispatch(command, discovery, json).await,
        Command::IntegrationPolicy { .. }
        | Command::IntegrationPolicySet { .. }
        | Command::Queue { .. }
        | Command::Retry { .. }
        | Command::Resolve { .. }
        | Command::Land { .. }
        | Command::Pr { .. }
        | Command::Forge { .. }
        | Command::Export { .. }
        | Command::Import { .. }
        | Command::Rehydrate { .. }
        | Command::LogStatus { .. }
        | Command::RestoreLog { .. }
        | Command::Replay { .. }
        | Command::Discard { .. }
        | Command::Commit { .. }
        | Command::Diff { .. } => git::dispatch(command, discovery, json).await,
        Command::Sessions { .. }
        | Command::SessionNew { .. }
        | Command::SessionUnlimited { .. }
        | Command::SessionPriority { .. }
        | Command::Search { .. }
        | Command::Events { .. }
        | Command::Prompt { .. }
        | Command::Cancel { .. }
        | Command::Pause { .. }
        | Command::Resume { .. }
        | Command::ResumePlan { .. }
        | Command::Compatibility { .. }
        | Command::Diagnose { .. }
        | Command::Context { .. }
        | Command::Compact { .. }
        | Command::Todos { .. }
        | Command::CitationAudit { .. }
        | Command::Citations { .. }
        | Command::Transcript { .. }
        | Command::Usage { .. } => session::dispatch(command, discovery, json).await,
        Command::Scheduler { .. }
        | Command::Programs { .. }
        | Command::ProgramStatus { .. }
        | Command::ProgramCreate { .. }
        | Command::ProgramUpdate { .. }
        | Command::ProgramPause { .. }
        | Command::ProgramResume { .. }
        | Command::ProgramCancel { .. }
        | Command::ProgramDelete { .. }
        | Command::ProgramMilestoneAdd { .. }
        | Command::ProgramMilestoneRetry { .. }
        | Command::ProgramMilestoneSkip { .. }
        | Command::ProgramRun { .. }
        | Command::ProgramTick { .. }
        | Command::ProgramReplay { .. }
        | Command::Tasks { .. }
        | Command::Runs { .. }
        | Command::RunDetail { .. }
        | Command::RunResume { .. }
        | Command::RunCancel { .. }
        | Command::RunPolicy { .. }
        | Command::RunPolicySet { .. }
        | Command::Intake { .. }
        | Command::IntakeAdd { .. }
        | Command::IntakeEdit { .. }
        | Command::IntakeRun { .. }
        | Command::IntakeCancel { .. }
        | Command::IntakeRetry { .. }
        | Command::IntakeDelete { .. }
        | Command::TaskDispatch { .. }
        | Command::TaskRetry { .. }
        | Command::TaskSkip { .. }
        | Command::TaskCancel { .. }
        | Command::TaskPause { .. }
        | Command::TaskResume { .. }
        | Command::Steer { .. }
        | Command::Steering { .. }
        | Command::SteerCancel { .. }
        | Command::Defer { .. }
        | Command::Continuations { .. }
        | Command::ContinuationCancel { .. }
        | Command::ContinuationStatus { .. }
        | Command::ContinuationReclaim { .. }
        | Command::TaskStalls { .. }
        | Command::ToolActivity { .. }
        | Command::Recoveries { .. } => task::dispatch(command, discovery, json).await,
        Command::Console { .. }
        | Command::Files { .. }
        | Command::File { .. }
        | Command::Grep { .. }
        | Command::WriteFile { .. }
        | Command::History { .. }
        | Command::Show { .. }
        | Command::Terminal { .. } => preview::dispatch(command, discovery, json).await,
        Command::Cache { .. }
        | Command::CacheEpoch { .. }
        | Command::CacheEpochs { .. }
        | Command::CacheEpochsPrune { .. }
        | Command::CacheCalls { .. }
        | Command::Instructions { .. }
        | Command::PromptLayout { .. }
        | Command::CacheAudit { .. }
        | Command::CacheBreaks { .. }
        | Command::CachePlan { .. }
        | Command::CacheWarm { .. }
        | Command::ToolBundles { .. }
        | Command::ToolBundle { .. }
        | Command::CachePolicy { .. }
        | Command::CachePolicySet { .. } => cache::dispatch(command, discovery, json).await,
        Command::Contract { .. }
        | Command::ContractPolicy { .. }
        | Command::ContractPolicySet { .. }
        | Command::Effects { .. }
        | Command::EffectReconcile { .. }
        | Command::EffectPolicy { .. }
        | Command::EffectPolicySet { .. }
        | Command::EffectSweep { .. }
        | Command::Gate { .. } => gate::dispatch(command, discovery, json).await,
        Command::Agents { .. }
        | Command::AgentOrg { .. }
        | Command::AgentDetail { .. }
        | Command::TeamExport { .. }
        | Command::TeamImport { .. }
        | Command::TeamTemplates { .. }
        | Command::TeamDiff { .. }
        | Command::TeamTemplateShow { .. }
        | Command::TeamReplay { .. }
        | Command::AgentCreate { .. }
        | Command::AgentUpdate { .. }
        | Command::AgentPause { .. }
        | Command::AgentResume { .. }
        | Command::AgentRetire { .. }
        | Command::AgentHire { .. }
        | Command::AgentHires { .. }
        | Command::AgentHireApprove { .. }
        | Command::AgentHireReject { .. }
        | Command::AgentPromote { .. }
        | Command::AgentTransfer { .. }
        | Command::AgentPerformance { .. }
        | Command::AgentRetirement { .. }
        | Command::RetirementPolicy { .. }
        | Command::RetirementPolicySet { .. }
        | Command::AgentPolicy { .. }
        | Command::AgentPolicySet { .. }
        | Command::Assignments { .. }
        | Command::Assign { .. }
        | Command::AssignmentRenew { .. }
        | Command::Release { .. }
        | Command::Workspaces { .. }
        | Command::Workspace { .. }
        | Command::WorkspaceLand { .. }
        | Command::WorkspaceArchive { .. }
        | Command::Inbox { .. }
        | Command::InboxSend { .. }
        | Command::InboxRead { .. }
        | Command::InboxAck { .. }
        | Command::CoordinationReplay { .. } => agent::dispatch(command, discovery, json).await,
        Command::AgentMemory { .. }
        | Command::AgentMemoryRemember { .. }
        | Command::AgentMemorySupersede { .. }
        | Command::AgentMemoryForget { .. }
        | Command::AgentMemoryErase { .. }
        | Command::AgentMemorySweep { .. }
        | Command::Memory { .. }
        | Command::Remember { .. }
        | Command::MemoryDelete { .. }
        | Command::MemorySearch { .. }
        | Command::MemoryBrief { .. }
        | Command::MemoryUpdate { .. }
        | Command::MemoryVerify { .. }
        | Command::MemorySweep { .. }
        | Command::MemoryStats { .. }
        | Command::MemoryConflicts { .. }
        | Command::MemoryConflictScan { .. }
        | Command::MemoryConflictResolve { .. }
        | Command::MemoryHealth { .. }
        | Command::MemoryPrune { .. }
        | Command::MemoryPolicy { .. }
        | Command::MemoryPolicySet { .. }
        | Command::MemoryEmbeddings { .. }
        | Command::ToolOutputPolicy { .. }
        | Command::MemoryEmbeddingsRebuild { .. }
        | Command::MemoryEmbeddingsTest { .. }
        | Command::MemoryEmbeddingsIndex { .. }
        | Command::MemoryEmbeddingsIndexRebuild { .. }
        | Command::ToolOutputPolicySet { .. }
        | Command::ToolOutputs { .. }
        | Command::ToolOutputStatus { .. }
        | Command::ToolOutputPrune { .. }
        | Command::ToolOutputAudit { .. } => memory::dispatch(command, discovery, json).await,
        Command::Mailbox { .. }
        | Command::ReadMessage { .. }
        | Command::MailboxPolicy { .. }
        | Command::MailboxPolicySet { .. }
        | Command::MailboxGc { .. } => mailbox::dispatch(command, discovery, json).await,
        Command::Artifacts { .. }
        | Command::ArtifactVersions { .. }
        | Command::ArtifactDiff { .. }
        | Command::ArtifactRestore { .. }
        | Command::Artifact { .. }
        | Command::ArtifactArchiveList { .. }
        | Command::ArtifactArchiveExtract { .. }
        | Command::MediaInfo { .. }
        | Command::MediaTranscode { .. }
        | Command::BackfillArtifacts { .. }
        | Command::ArtifactPolicy { .. }
        | Command::ArtifactPolicySet { .. }
        | Command::Gc { .. } => artifact::dispatch(command, discovery, json).await,
        Command::TestCache { .. }
        | Command::TestCacheEntries { .. }
        | Command::TestCachePolicySet { .. }
        | Command::TestCacheScopes { .. }
        | Command::TestCachePrune { .. }
        | Command::TestCacheClear { .. }
        | Command::TestImpact { .. } => testing::dispatch(command, discovery, json).await,
        Command::Approvals { .. }
        | Command::ApprovalBatch { .. }
        | Command::ApprovalTemplate { .. }
        | Command::ApprovalTemplateSet { .. }
        | Command::ApprovalTemplateDelete { .. }
        | Command::ApprovalPolicy { .. }
        | Command::ApprovalPolicySet { .. }
        | Command::ApprovalAudit { .. }
        | Command::Questions { .. }
        | Command::Answer { .. }
        | Command::Approve { .. }
        | Command::Deny { .. } => approval::dispatch(command, discovery, json).await,
    }
}
