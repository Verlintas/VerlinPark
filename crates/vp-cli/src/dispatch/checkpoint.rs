//! Checkpoint, context-checkpoint, and session-fork commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::CheckpointPolicy { project } => {
            rpc(
                &discovery,
                "project.checkpoint_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::CheckpointPolicySet {
            project,
            enabled,
            older_than_days,
            keep_latest,
        } => {
            let policy = json!({
                "enabled": enabled,
                "older_than_secs": older_than_days.saturating_mul(86_400),
                "keep_latest": keep_latest
            });
            rpc(
                &discovery,
                "project.checkpoint_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Checkpoint { session, label } => {
            rpc(
                &discovery,
                "session.checkpoint",
                json!({"session_id": session, "label": label}),
            )
            .await?
        }
        Command::Checkpoints { session } => {
            rpc(
                &discovery,
                "session.checkpoints",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CheckpointStats { session } => {
            rpc(
                &discovery,
                "session.checkpoint.stats",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CheckpointRestore {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.restore",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::CheckpointDelete {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.delete",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::CheckpointPrune {
            session,
            older_than_days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.prune",
                json!({
                    "session_id": session,
                    "older_than_days": older_than_days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::ContextCheckpoints { session, limit } => {
            rpc(
                &discovery,
                "session.context.checkpoints",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::ContextCheckpoint { session, label } => {
            rpc(
                &discovery,
                "session.context.checkpoint",
                json!({"session_id": session, "label": label}),
            )
            .await?
        }
        Command::ContextCheckpointDetail {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.detail",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::ContextRestore {
            session,
            checkpoint,
            force,
        } => {
            rpc(
                &discovery,
                "session.context.restore",
                json!({
                    "session_id": session,
                    "checkpoint_id": checkpoint,
                    "force": force
                }),
            )
            .await?
        }
        Command::ContextCheckpointDelete {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.delete",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::ContextCheckpointPrune {
            session,
            older_than_days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.prune",
                json!({
                    "session_id": session,
                    "older_than_days": older_than_days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::ContextCheckpointPolicy { project } => {
            rpc(
                &discovery,
                "session.context.policy",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ContextCheckpointPolicySet {
            project,
            enabled,
            max_checkpoints,
            max_age_days,
        } => {
            rpc(
                &discovery,
                "session.context.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled,
                        "max_checkpoints": max_checkpoints,
                        "max_age_days": max_age_days
                    }
                }),
            )
            .await?
        }
        Command::Fork {
            session,
            checkpoint,
            goal,
        } => {
            rpc(
                &discovery,
                "session.fork",
                json!({
                    "session_id": session,
                    "checkpoint_id": checkpoint,
                    "goal": goal
                }),
            )
            .await?
        }
        Command::Lineage { session } => {
            rpc(
                &discovery,
                "session.lineage",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ForkTree { session } => {
            rpc(
                &discovery,
                "session.fork.tree",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ForkForest { project } => {
            rpc(
                &discovery,
                "project.fork.forest",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ForkAudit { project } => {
            rpc(
                &discovery,
                "project.fork.audit",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ForkRepair { project } => {
            rpc(
                &discovery,
                "project.fork.repair",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Compare { session, other } => {
            rpc(
                &discovery,
                "session.compare",
                json!({"session_id": session, "other_session_id": other}),
            )
            .await?
        }
        Command::ForkMerge { session, fork } => {
            rpc(
                &discovery,
                "session.fork.merge",
                json!({"source_session_id": session, "fork_session_id": fork}),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
