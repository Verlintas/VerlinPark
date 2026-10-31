use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use vp_protocol::discovery::Discovery;
use vp_protocol::rpc::RpcResponse;
use vp_store::paths;

#[derive(Debug, Parser)]
#[command(name = "vp", about = "VerlinPark CLI", version)]
struct Cli {
    /// Print raw JSON results instead of human-readable output.
    #[arg(long, global = true)]
    json: bool,
    /// Override the daemon data directory (must match the daemon's data dir).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check that the daemon is reachable.
    Ping,
    /// Show daemon information.
    Info,
    /// Run local diagnostics.
    Doctor,
    /// Send a tiny live request to the configured provider.
    ProviderTest,
    /// List projects.
    Projects,
    /// Add a local repository as a project.
    ProjectAdd {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Show the project-wide shared budget and spend.
    ProjectBudget { project: String },
    /// Show token/cost usage grouped by session for a project.
    ProjectUsage { project: String },
    /// Set or clear the project-wide shared budget.
    ProjectBudgetSet {
        project: String,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
    },
    /// Remove old terminal session worktrees while keeping branches by default.
    CleanupWorktrees {
        project: String,
        #[arg(long, default_value_t = 7)]
        older_than_days: u64,
        /// Report what would be removed without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
        /// Remove worktrees with uncommitted changes.
        #[arg(long)]
        force: bool,
        /// Also delete the session branches.
        #[arg(long)]
        delete_branches: bool,
        /// Keep worktrees for failed/cancelled sessions.
        #[arg(long)]
        keep_failed: bool,
    },
    /// Show the project's shell permission policy.
    ShellPolicy { project: String },
    /// Update the project's shell network/read-deny policy.
    ShellPolicySet {
        project: String,
        /// Network access for shell commands: allow or deny.
        #[arg(long, default_value = "allow")]
        network: String,
        /// Additional absolute (or ~/...) path to deny for reads.
        #[arg(long = "read-deny")]
        read_deny: Vec<String>,
    },
    /// Show the project's automatic worktree retention policy.
    WorktreePolicy { project: String },
    /// Update the project's automatic worktree retention policy.
    WorktreePolicySet {
        project: String,
        /// Enable the background sweep (omitting this disables it).
        #[arg(long)]
        enabled: bool,
        #[arg(long, default_value_t = 7)]
        older_than_days: u64,
        #[arg(long)]
        delete_branches: bool,
        /// Also remove failed/cancelled worktrees (default keeps them).
        #[arg(long)]
        drop_failed: bool,
    },
    /// Show the project's merge/integration policy.
    IntegrationPolicy { project: String },
    /// Update automatic conflict resolution and its caps.
    IntegrationPolicySet {
        project: String,
        /// Enable automatic conflict resolution for this project.
        #[arg(long)]
        auto_resolve: bool,
        /// Maximum automatic attempts per conflict entry (0-5).
        #[arg(long, default_value_t = 1)]
        max_auto_attempts: u32,
        /// Token cap for each automatic resolution (resolver + verifier).
        #[arg(long)]
        max_resolve_tokens: Option<u64>,
    },
    /// List sessions, optionally scoped to a project.
    Sessions {
        #[arg(long)]
        project: Option<String>,
    },
    /// Create a session for a project.
    SessionNew {
        project: String,
        goal: String,
        #[arg(long)]
        token_budget: Option<u64>,
        #[arg(long)]
        cost_budget_usd: Option<f64>,
        #[arg(long)]
        time_budget_secs: Option<u64>,
    },
    /// Search messages and event payloads across sessions.
    Search {
        query: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: u32,
    },
    /// List events for a session.
    Events {
        session: String,
        #[arg(long, default_value_t = 0)]
        since: u64,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Send a follow-up prompt to the session Leader.
    Prompt { session: String, content: String },
    /// Ask the running Leader to stop.
    Cancel { session: String },
    /// Pause the running Leader between turns/tools.
    Pause { session: String },
    /// Resume a paused Leader.
    Resume { session: String },
    /// Show the session plan/todo list.
    Todos { session: String },
    /// Show the per-call usage/cost ledger.
    Usage {
        session: String,
        /// Emit CSV instead of the grouped human-readable ledger.
        #[arg(long)]
        csv: bool,
    },
    /// Show the validated team plan.
    TeamPlan { session: String },
    /// Print a Markdown transcript of the session's plan and messages.
    Transcript { session: String },
    /// List the agents spawned for a session.
    Agents { session: String },
    /// List typed agent messages.
    Mailbox {
        session: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        unread: bool,
    },
    /// Mark a typed agent message as read.
    ReadMessage { message: String },
    /// List published artifacts.
    Artifacts { session: String },
    /// Backfill full publish events for legacy artifacts in a session.
    BackfillArtifacts { session: String },
    /// Show the project artifact retention policy.
    ArtifactPolicy { project: String },
    /// Update artifact retention and optional automatic GC.
    ArtifactPolicySet {
        project: String,
        #[arg(long)]
        auto_gc: bool,
        #[arg(long)]
        max_bytes: Option<u64>,
        #[arg(long)]
        max_age_secs: Option<u64>,
        #[arg(long)]
        max_count: Option<u64>,
    },
    /// Run artifact retention GC for a project.
    Gc { project: String },
    /// Show the project mailbox retention policy.
    MailboxPolicy { project: String },
    /// Update mailbox retention limits.
    MailboxPolicySet {
        project: String,
        #[arg(long)]
        max_age_secs: Option<u64>,
        #[arg(long)]
        max_count: Option<u64>,
        /// Also delete unread messages (default: read messages only).
        #[arg(long)]
        delete_unread: bool,
        /// Run mailbox GC after sending or marking a message read.
        #[arg(long)]
        auto_gc: bool,
    },
    /// Run mailbox retention GC for a project.
    MailboxGc { project: String },
    /// Read an artifact by id.
    Artifact { artifact: String },
    /// Show the session merge queue and Integrator results.
    Queue { session: String },
    /// Retry a conflicted or failed merge queue entry after rebasing it.
    Retry { entry: String },
    /// Resolve a merge conflict with an internal agent, verify, and merge.
    Resolve { entry: String },
    /// Land the session branch into the project default branch.
    Land { session: String },
    /// Push the session branch and open a GitHub pull request with gh.
    Pr { session: String },
    /// Export a portable session bundle (events + projections).
    Export {
        session: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Import an archived session bundle into the local daemon.
    Import { bundle: PathBuf },
    /// Recreate a worktree for an imported archive session.
    Rehydrate {
        session: String,
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Replay the event stream and check projection consistency.
    Replay {
        session: String,
        #[arg(long)]
        repair: bool,
        #[arg(long)]
        rebuild: bool,
    },
    /// Compare the JSONL event mirror with the database.
    LogStatus { session: String },
    /// Restore the event table from the JSONL mirror and rebuild projections.
    RestoreLog { session: String },
    /// Discard uncommitted changes in the session worktree.
    Discard { session: String },
    /// Commit the session worktree.
    Commit {
        session: String,
        #[arg(short, long)]
        message: String,
    },
    /// Show the session worktree diff.
    Diff { session: String },
    /// List approvals, optionally scoped to a session.
    Approvals {
        #[arg(long)]
        session: Option<String>,
    },
    /// Approve a pending tool call.
    Approve { approval: String },
    /// Deny a pending tool call.
    Deny { approval: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let discovery = read_discovery(cli.data_dir.as_deref())?;
    let value = match cli.command {
        Command::Ping => {
            let result = rpc(&discovery, "ping", json!({})).await?;
            if cli.json {
                print_json(&result)?;
            } else {
                println!(
                    "pong · v{} · protocol {} · up {}s",
                    result["version"].as_str().unwrap_or("?"),
                    result["protocol"].as_u64().unwrap_or(0),
                    result["uptime_secs"].as_u64().unwrap_or(0)
                );
            }
            return Ok(());
        }
        Command::Info => rpc(&discovery, "server.info", json!({})).await?,
        Command::Doctor => rpc(&discovery, "server.doctor", json!({})).await?,
        Command::ProviderTest => rpc(&discovery, "agent.probe", json!({})).await?,
        Command::Projects => rpc(&discovery, "project.list", json!({})).await?,
        Command::ProjectAdd { path, name } => {
            let root = path
                .canonicalize()
                .with_context(|| format!("resolving {}", path.display()))?;
            rpc(
                &discovery,
                "project.add",
                json!({"root": root, "name": name}),
            )
            .await?
        }
        Command::ProjectBudget { project } => {
            rpc(
                &discovery,
                "project.budget.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ProjectUsage { project } => {
            rpc(&discovery, "project.usage", json!({"project_id": project})).await?
        }
        Command::ProjectBudgetSet {
            project,
            token_budget,
            cost_budget_usd,
        } => {
            rpc(
                &discovery,
                "project.budget.set",
                json!({
                    "project_id": project,
                    "budget": {
                        "token_budget": token_budget,
                        "cost_budget_usd": cost_budget_usd,
                    }
                }),
            )
            .await?
        }
        Command::CleanupWorktrees {
            project,
            older_than_days,
            dry_run,
            force,
            delete_branches,
            keep_failed,
        } => {
            rpc(
                &discovery,
                "project.cleanup_worktrees",
                json!({
                    "project_id": project,
                    "older_than_secs": older_than_days.saturating_mul(86_400),
                    "dry_run": dry_run,
                    "force": force,
                    "delete_branches": delete_branches,
                    "keep_failed": keep_failed,
                }),
            )
            .await?
        }
        Command::ShellPolicy { project } => {
            rpc(
                &discovery,
                "project.shell_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ShellPolicySet {
            project,
            network,
            read_deny,
        } => {
            rpc(
                &discovery,
                "project.shell_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "network": network,
                        "read_deny": read_deny,
                    }
                }),
            )
            .await?
        }
        Command::WorktreePolicy { project } => {
            rpc(
                &discovery,
                "project.worktree_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::WorktreePolicySet {
            project,
            enabled,
            older_than_days,
            delete_branches,
            drop_failed,
        } => {
            let policy = json!({
                "enabled": enabled,
                "older_than_secs": older_than_days.saturating_mul(86_400),
                "delete_branches": delete_branches,
                "keep_failed": !drop_failed
            });
            rpc(
                &discovery,
                "project.worktree_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::IntegrationPolicy { project } => {
            rpc(
                &discovery,
                "project.integration_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::IntegrationPolicySet {
            project,
            auto_resolve,
            max_auto_attempts,
            max_resolve_tokens,
        } => {
            rpc(
                &discovery,
                "project.integration_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "auto_resolve": auto_resolve,
                        "max_auto_attempts": max_auto_attempts,
                        "max_resolve_tokens": max_resolve_tokens,
                    }
                }),
            )
            .await?
        }
        Command::Sessions { project } => {
            rpc(&discovery, "session.list", json!({"project_id": project})).await?
        }
        Command::SessionNew {
            project,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
        } => {
            rpc(
                &discovery,
                "session.create",
                json!({
                    "project_id": project,
                    "goal": goal,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                }),
            )
            .await?
        }
        Command::Search {
            query,
            project,
            limit,
        } => {
            rpc(
                &discovery,
                "session.search",
                json!({"query": query, "project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::Events {
            session,
            since,
            limit,
        } => {
            rpc(
                &discovery,
                "event.list",
                json!({"session_id": session, "since_seq": since, "limit": limit}),
            )
            .await?
        }
        Command::Prompt { session, content } => {
            rpc(
                &discovery,
                "session.prompt",
                json!({"session_id": session, "content": content}),
            )
            .await?
        }
        Command::Cancel { session } => {
            rpc(&discovery, "session.cancel", json!({"session_id": session})).await?
        }
        Command::Pause { session } => {
            rpc(&discovery, "session.pause", json!({"session_id": session})).await?
        }
        Command::Resume { session } => {
            rpc(&discovery, "session.resume", json!({"session_id": session})).await?
        }
        Command::Todos { session } => {
            rpc(&discovery, "session.todos", json!({"session_id": session})).await?
        }
        Command::TeamPlan { session } => {
            rpc(&discovery, "team_plan.get", json!({"session_id": session})).await?
        }
        Command::Transcript { session } => {
            rpc(
                &discovery,
                "session.transcript",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Usage { session, csv } => {
            let method = if csv {
                "session.usage_csv"
            } else {
                "session.usage"
            };
            rpc(&discovery, method, json!({"session_id": session})).await?
        }
        Command::Agents { session } => {
            rpc(&discovery, "agent.list", json!({"session_id": session})).await?
        }
        Command::Mailbox {
            session,
            agent,
            unread,
        } => {
            rpc(
                &discovery,
                "mailbox.list",
                json!({
                    "session_id": session,
                    "agent_id": agent,
                    "unread_only": unread,
                }),
            )
            .await?
        }
        Command::ReadMessage { message } => {
            rpc(&discovery, "mailbox.read", json!({"message_id": message})).await?
        }
        Command::Artifacts { session } => {
            rpc(&discovery, "artifact.list", json!({"session_id": session})).await?
        }
        Command::Artifact { artifact } => {
            let value = rpc(&discovery, "artifact.get", json!({"artifact_id": artifact})).await?;
            if value["artifact"]["storage"].as_str() == Some("file") {
                rpc(
                    &discovery,
                    "artifact.materialize",
                    json!({"artifact_id": artifact}),
                )
                .await?
            } else {
                value
            }
        }
        Command::BackfillArtifacts { session } => {
            rpc(
                &discovery,
                "artifact.backfill",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ArtifactPolicy { project } => {
            rpc(
                &discovery,
                "artifact.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ArtifactPolicySet {
            project,
            auto_gc,
            max_bytes,
            max_age_secs,
            max_count,
        } => {
            rpc(
                &discovery,
                "artifact.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "auto_gc": auto_gc,
                        "max_bytes": max_bytes,
                        "max_age_secs": max_age_secs,
                        "max_count": max_count,
                    }
                }),
            )
            .await?
        }
        Command::Gc { project } => {
            rpc(&discovery, "artifact.gc", json!({"project_id": project})).await?
        }
        Command::MailboxPolicy { project } => {
            rpc(
                &discovery,
                "mailbox.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MailboxPolicySet {
            project,
            max_age_secs,
            max_count,
            delete_unread,
            auto_gc,
        } => {
            rpc(
                &discovery,
                "mailbox.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "max_age_secs": max_age_secs,
                        "max_count": max_count,
                        "delete_unread": delete_unread,
                        "auto_gc": auto_gc,
                    }
                }),
            )
            .await?
        }
        Command::MailboxGc { project } => {
            rpc(&discovery, "mailbox.gc", json!({"project_id": project})).await?
        }
        Command::Queue { session } => {
            rpc(
                &discovery,
                "merge_queue.list",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Retry { entry } => {
            rpc(&discovery, "merge_queue.retry", json!({"entry_id": entry})).await?
        }
        Command::Resolve { entry } => {
            rpc(
                &discovery,
                "merge_queue.resolve",
                json!({"entry_id": entry}),
            )
            .await?
        }
        Command::Land { session } => {
            rpc(&discovery, "session.land", json!({"session_id": session})).await?
        }
        Command::Pr { session } => {
            rpc(&discovery, "session.pr", json!({"session_id": session})).await?
        }
        Command::Export { session, output } => {
            let value = rpc(&discovery, "session.export", json!({"session_id": session})).await?;
            if let Some(path) = output {
                std::fs::write(&path, serde_json::to_string_pretty(&value)?)?;
                println!("exported {}", path.display());
                return Ok(());
            }
            value
        }
        Command::Import { bundle } => {
            let data =
                std::fs::read(&bundle).with_context(|| format!("reading {}", bundle.display()))?;
            let parsed: Value = serde_json::from_slice(&data).context("parsing session bundle")?;
            rpc(&discovery, "session.import", json!({"bundle": parsed})).await?
        }
        Command::Rehydrate { session, worktree } => {
            rpc(
                &discovery,
                "session.rehydrate",
                json!({"session_id": session, "worktree": worktree}),
            )
            .await?
        }
        Command::LogStatus { session } => {
            rpc(
                &discovery,
                "session.log_status",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::RestoreLog { session } => {
            rpc(
                &discovery,
                "session.restore_log",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Replay {
            session,
            repair,
            rebuild,
        } => {
            rpc(
                &discovery,
                "session.replay",
                json!({"session_id": session, "repair": repair, "rebuild": rebuild}),
            )
            .await?
        }
        Command::Discard { session } => {
            rpc(
                &discovery,
                "session.discard",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Commit { session, message } => {
            rpc(
                &discovery,
                "session.commit",
                json!({"session_id": session, "message": message}),
            )
            .await?
        }
        Command::Diff { session } => {
            rpc(&discovery, "session.diff", json!({"session_id": session})).await?
        }
        Command::Approvals { session } => {
            rpc(&discovery, "approval.list", json!({"session_id": session})).await?
        }
        Command::Approve { approval } => {
            rpc(
                &discovery,
                "approval.resolve",
                json!({"approval_id": approval, "approved": true}),
            )
            .await?
        }
        Command::Deny { approval } => {
            rpc(
                &discovery,
                "approval.resolve",
                json!({"approval_id": approval, "approved": false}),
            )
            .await?
        }
    };

    if cli.json {
        print_json(&value)?;
    } else {
        print_human(&value);
    }
    Ok(())
}

fn read_discovery(data_dir: Option<&Path>) -> Result<Discovery> {
    let path = match data_dir {
        Some(dir) => dir.join("run/daemon.json"),
        None => paths::discovery_path(),
    };
    let data = std::fs::read(&path).with_context(|| {
        format!(
            "daemon discovery file not found at {}. Start verlinpadd first (just daemon).",
            path.display()
        )
    })?;
    let discovery: Discovery =
        serde_json::from_slice(&data).context("parsing daemon discovery file")?;
    Ok(discovery)
}

async fn rpc(discovery: &Discovery, method: &str, params: Value) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": "cli",
        "method": method,
        "params": params,
    });
    let response = reqwest::Client::new()
        .post(discovery.rpc_url())
        .bearer_auth(&discovery.token)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("connecting to {}", discovery.rpc_url()))?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        bail!("daemon returned HTTP {status}: {text}");
    }
    let response: RpcResponse = serde_json::from_str(&text)?;
    if let Some(error) = response.error {
        bail!("{} (code {})", error.message, error.code);
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_human(value: &Value) {
    if let Some(csv) = value.get("csv").and_then(Value::as_str) {
        print!("{csv}");
        return;
    }
    if let Some(markdown) = value.get("markdown").and_then(Value::as_str) {
        print!("{markdown}");
        return;
    }
    if let Some(hits) = value.get("hits").and_then(Value::as_array)
        && value.get("query").is_some()
    {
        if hits.is_empty() {
            println!("No matches.");
            return;
        }
        for hit in hits {
            let source = hit["source"].as_str().unwrap_or("?");
            let detail = hit["role"]
                .as_str()
                .or_else(|| hit["kind"].as_str())
                .unwrap_or("");
            println!(
                "{} {:<18} {} · {}",
                source,
                detail,
                hit["session_goal"].as_str().unwrap_or("?"),
                hit["snippet"].as_str().unwrap_or("")
            );
            println!(
                "  {} · session {}",
                hit["created_at"].as_str().unwrap_or("?"),
                hit["session_id"].as_str().unwrap_or("?")
            );
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            println!("… more matches exist; increase --limit or narrow the query.");
        }
        return;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array)
        && value.get("project_id").is_some()
        && let Some(totals) = value.get("totals")
    {
        println!(
            "project {}  sessions={}  calls={}  in={}  out={}  cost=${:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            sessions.len(),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        );
        for session in sessions {
            println!(
                "  {:<10} calls={:<4} in={:<8} out={:<8} ${:.4}  {}",
                session["status"].as_str().unwrap_or("?"),
                session["calls"].as_u64().unwrap_or(0),
                session["input_tokens"].as_u64().unwrap_or(0),
                session["output_tokens"].as_u64().unwrap_or(0),
                session["cost_usd"].as_f64().unwrap_or(0.0),
                session["goal"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(totals) = value.get("totals")
        && value.get("entries").is_some()
    {
        println!(
            "session {}  calls={}  in={}  out={}  cost=${:.4}",
            value["session_id"].as_str().unwrap_or("?"),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        );
        for (label, field) in [
            ("role", "by_role"),
            ("dept", "by_department"),
            ("task", "by_task"),
            ("agent", "by_agent"),
            ("model", "by_model"),
        ] {
            for bucket in totals[field].as_array().into_iter().flatten() {
                println!(
                    "  {label:5} {:<20} calls={:<3} in={:<7} out={:<7} ${:.4}",
                    bucket["key"].as_str().unwrap_or("?"),
                    bucket["calls"].as_u64().unwrap_or(0),
                    bucket["input_tokens"].as_u64().unwrap_or(0),
                    bucket["output_tokens"].as_u64().unwrap_or(0),
                    bucket["cost_usd"].as_f64().unwrap_or(0.0)
                );
            }
        }
        return;
    }
    if value.get("session_id").is_some()
        && value.get("worktree").is_some()
        && value.get("commit").is_some()
    {
        println!(
            "rehydrated {} at {} · branch {} · {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["worktree"].as_str().unwrap_or("?"),
            value["branch"].as_str().unwrap_or("?"),
            value["commit"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("session_id").is_some() && value.get("events").is_some() {
        println!(
            "imported session {} into project {} · events={} · status={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            value["events"].as_u64().unwrap_or(0),
            value["status"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("version").is_some() && value.get("exported_at").is_some() {
        println!(
            "session {}  events={}  agents={}  messages={}  queue={}",
            value["session"]["id"].as_str().unwrap_or("?"),
            value["events"].as_array().map_or(0, Vec::len),
            value["agents"].as_array().map_or(0, Vec::len),
            value["messages"].as_array().map_or(0, Vec::len),
            value["merge_queue"].as_array().map_or(0, Vec::len)
        );
        return;
    }
    if value.get("latency_ms").is_some() && value.get("provider").is_some() {
        println!(
            "provider {} / {}  {} ms  in={} out={}  {}",
            value["provider"].as_str().unwrap_or("?"),
            value["model"].as_str().unwrap_or("?"),
            value["latency_ms"].as_u64().unwrap_or(0),
            value["input_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0),
            value["text"].as_str().unwrap_or("")
        );
        return;
    }
    if let Some(checks) = value.get("checks").and_then(Value::as_array) {
        println!(
            "{}",
            if value["ok"].as_bool().unwrap_or(false) {
                "doctor: ok"
            } else {
                "doctor: errors"
            }
        );
        for check in checks {
            println!(
                "  {:<8} {:<18} {}",
                check["status"].as_str().unwrap_or("?"),
                check["name"].as_str().unwrap_or("?"),
                check["detail"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if value.get("budget").is_some() && value.get("spent_tokens").is_some() {
        println!(
            "project {}  token-budget={}  cost-budget={}  spent-tokens={}  spent-usd={:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            value["budget"]["token_budget"].as_u64().unwrap_or(0),
            value["budget"]["cost_budget_usd"].as_f64().unwrap_or(0.0),
            value["spent_tokens"].as_u64().unwrap_or(0),
            value["spent_cost_usd"].as_f64().unwrap_or(0.0)
        );
        return;
    }
    if value.get("scanned").is_some() && value.get("entries").is_some() {
        println!(
            "project {}  scanned={}  removed={}  skipped={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["scanned"].as_u64().unwrap_or(0),
            value["removed"].as_u64().unwrap_or(0),
            value["skipped"].as_u64().unwrap_or(0)
        );
        for entry in value["entries"].as_array().into_iter().flatten() {
            println!(
                "{}  {}  {}  {}",
                if entry["removed"].as_bool().unwrap_or(false) {
                    "removed"
                } else {
                    "skipped"
                },
                entry["session_id"].as_str().unwrap_or("?"),
                entry["branch"].as_str().unwrap_or("?"),
                entry["reason"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if value.get("artifact_id").is_some() && value.get("path").is_some() {
        println!(
            "materialized {} at {}",
            value["artifact_id"].as_str().unwrap_or("?"),
            value["path"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("session_id").is_some() && value.get("backfilled").is_some() {
        println!(
            "session {}  scanned={}  backfilled={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["scanned"].as_u64().unwrap_or(0),
            value["backfilled"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(policy) = value.get("policy") {
        if policy.get("delete_unread").is_some() {
            println!(
                "project {}  mailbox-max-age={}  mailbox-max-count={}  delete-unread={}  auto-gc={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["max_age_secs"].as_u64().unwrap_or(0),
                policy["max_count"].as_u64().unwrap_or(0),
                policy["delete_unread"].as_bool().unwrap_or(false),
                policy["auto_gc"].as_bool().unwrap_or(false)
            );
            return;
        }
        if policy.get("auto_gc").is_some() {
            println!(
                "project {}  auto-gc={}  max-bytes={}  max-age={}  max-count={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["auto_gc"].as_bool().unwrap_or(false),
                policy["max_bytes"].as_u64().unwrap_or(0),
                policy["max_age_secs"].as_u64().unwrap_or(0),
                policy["max_count"].as_u64().unwrap_or(0)
            );
            return;
        }
        if policy.get("enabled").is_some() && policy.get("older_than_secs").is_some() {
            println!(
                "project {}  auto-cleanup={}  older-than={}d  delete-branches={}  keep-failed={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["enabled"].as_bool().unwrap_or(false),
                policy["older_than_secs"].as_u64().unwrap_or(0) / 86_400,
                policy["delete_branches"].as_bool().unwrap_or(false),
                policy["keep_failed"].as_bool().unwrap_or(true)
            );
            return;
        }
        if policy.get("auto_resolve").is_some() {
            println!(
                "project {}  auto-resolve={}  max-attempts={}  max-tokens={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["auto_resolve"].as_bool().unwrap_or(false),
                policy["max_auto_attempts"].as_u64().unwrap_or(0),
                policy["max_resolve_tokens"]
                    .as_u64()
                    .map_or_else(|| "session budget".to_string(), |value| value.to_string())
            );
            return;
        }
        println!(
            "project {}  network={}",
            value["project_id"].as_str().unwrap_or("?"),
            policy["network"].as_str().unwrap_or("?")
        );
        let paths = policy["read_deny"].as_array();
        if paths.is_none_or(|paths| paths.is_empty()) {
            println!("read-deny: none");
        } else {
            for path in paths.into_iter().flatten() {
                println!("read-deny: {}", path.as_str().unwrap_or("?"));
            }
        }
        return;
    }
    if let Some(projects) = value.get("projects").and_then(Value::as_array) {
        if projects.is_empty() {
            println!("No projects. Add one with: vp project-add <path>");
        }
        for project in projects {
            println!(
                "{}  {}  [{}]",
                project["id"].as_str().unwrap_or("?"),
                project["name"].as_str().unwrap_or("?"),
                project["default_branch"].as_str().unwrap_or("?")
            );
            println!("  {}", project["root"].as_str().unwrap_or("?"));
        }
        return;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array) {
        if sessions.is_empty() {
            println!("No sessions.");
        }
        for session in sessions {
            println!(
                "{}  {}  [{}]",
                session["id"].as_str().unwrap_or("?"),
                session["goal"].as_str().unwrap_or("?"),
                session["status"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(plan) = value.get("plan").filter(|plan| !plan.is_null()) {
        println!("GOAL  {}", plan["goal"].as_str().unwrap_or("?"));
        for department in plan["departments"].as_array().into_iter().flatten() {
            println!(
                "  {:<16} manager {:<16} profile {}",
                department["name"].as_str().unwrap_or("?"),
                department["manager_title"].as_str().unwrap_or("?"),
                department["profile"].as_str().unwrap_or("?")
            );
        }
        for task in plan["tasks"].as_array().into_iter().flatten() {
            println!(
                "  task {:<12} {:<16} kind {}",
                task["id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["kind"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(entry) = value.get("entry").filter(|entry| !entry.is_null()) {
        println!(
            "{}  {}  {}",
            entry["status"].as_str().unwrap_or("?"),
            entry["agent_name"].as_str().unwrap_or("?"),
            entry["commit"].as_str().unwrap_or("?")
        );
        if let Some(detail) = entry["detail"].as_str() {
            println!("{detail}");
        }
        return;
    }
    if value.get("mirror_events").is_some() && value.get("database_events").is_some() {
        println!(
            "{}  mirror={} database={}  {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["mirror_events"].as_u64().unwrap_or(0),
            value["database_events"].as_u64().unwrap_or(0),
            value["detail"].as_str().unwrap_or("")
        );
        return;
    }
    if let Some(consistent) = value.get("consistent").and_then(Value::as_bool) {
        println!(
            "{}  events={}  status={} usage={} ledger={} mailbox={} artifacts={} approvals={} plan={} todos={} messages={} agents={} queue={}",
            if consistent { "consistent" } else { "MISMATCH" },
            value["event_count"].as_u64().unwrap_or(0),
            value["status_matches"].as_bool().unwrap_or(false),
            value["usage_matches"].as_bool().unwrap_or(false),
            value["ledger_matches"].as_bool().unwrap_or(false),
            value["mailbox_matches"].as_bool().unwrap_or(false),
            value["artifacts_matches"].as_bool().unwrap_or(false),
            value["approvals_matches"].as_bool().unwrap_or(false),
            value["plan_matches"].as_bool().unwrap_or(false),
            value["todos_matches"].as_bool().unwrap_or(false),
            value["messages_matches"].as_bool().unwrap_or(false),
            value["agents_matches"].as_bool().unwrap_or(false),
            value["queue_matches"].as_bool().unwrap_or(false)
        );
        if value["rebuilt"].as_bool().unwrap_or(false) {
            println!("rebuilt projections from the event stream.");
        } else if value["repaired"].as_bool().unwrap_or(false) {
            println!("repaired projections from the event stream.");
        }
        for mismatch in value["mismatches"].as_array().into_iter().flatten() {
            if let Some(mismatch) = mismatch.as_str() {
                println!("  - {mismatch}");
            }
        }
        return;
    }
    if let Some(url) = value.get("url").and_then(Value::as_str) {
        println!(
            "PR opened: {url}  ({} → {})",
            value["branch"].as_str().unwrap_or("?"),
            value["base_branch"].as_str().unwrap_or("?")
        );
        return;
    }
    if let Some(target) = value.get("target_branch").and_then(Value::as_str) {
        println!(
            "landed {} → {} at {}",
            value["branch"].as_str().unwrap_or("?"),
            target,
            value["commit"].as_str().unwrap_or("?")
        );
        if let Some(detail) = value["detail"].as_str() {
            println!("{detail}");
        }
        return;
    }
    if value.get("removed").is_some() && value.get("bytes_freed").is_none() {
        println!(
            "mailbox GC removed {} message(s)",
            value["removed"].as_u64().unwrap_or(0)
        );
        return;
    }
    if value.get("removed").is_some() && value.get("bytes_freed").is_some() {
        println!(
            "artifact GC removed {} artifact(s), freed {} bytes",
            value["removed"].as_u64().unwrap_or(0),
            value["bytes_freed"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        if messages.is_empty() {
            println!("Mailbox is empty.");
        }
        for message in messages {
            println!(
                "{}  {} → {}  {}  [{}]",
                message["id"].as_str().unwrap_or("?"),
                message["from_name"].as_str().unwrap_or("?"),
                message["to_name"].as_str().unwrap_or("?"),
                message["kind"].as_str().unwrap_or("?"),
                message["status"].as_str().unwrap_or("?")
            );
            if let Some(body) = message["body"].as_str() {
                println!("  {body}");
            }
        }
        return;
    }
    if let Some(message) = value.get("message") {
        println!(
            "{} → {}  {}  [{}]",
            message["from_name"].as_str().unwrap_or("?"),
            message["to_name"].as_str().unwrap_or("?"),
            message["kind"].as_str().unwrap_or("?"),
            message["status"].as_str().unwrap_or("?")
        );
        if let Some(body) = message["body"].as_str() {
            println!("{body}");
        }
        return;
    }
    if let Some(artifacts) = value.get("artifacts").and_then(Value::as_array) {
        if artifacts.is_empty() {
            println!("No artifacts.");
        }
        for artifact in artifacts {
            println!(
                "{}  {:<24} {:<10} {}",
                artifact["id"].as_str().unwrap_or("?"),
                artifact["name"].as_str().unwrap_or("?"),
                artifact["kind"].as_str().unwrap_or("?"),
                artifact["agent_name"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(artifact) = value.get("artifact") {
        println!(
            "{}  {}  [{}]  {}  {} bytes",
            artifact["name"].as_str().unwrap_or("?"),
            artifact["kind"].as_str().unwrap_or("?"),
            artifact["mime_type"].as_str().unwrap_or("?"),
            artifact["storage"].as_str().unwrap_or("?"),
            artifact["size_bytes"].as_u64().unwrap_or(0)
        );
        if artifact["storage"].as_str() == Some("file") {
            println!(
                "stored at {}",
                artifact["path"].as_str().unwrap_or("(path unavailable)")
            );
        } else {
            println!("{}", artifact["content"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        if entries.is_empty() {
            println!("Merge queue is empty.");
        }
        for entry in entries {
            println!(
                "{:<10} {:<10} {:<16} {}",
                entry["status"].as_str().unwrap_or("?"),
                entry["task_id"].as_str().unwrap_or("—"),
                entry["agent_name"].as_str().unwrap_or("?"),
                entry["commit"].as_str().unwrap_or("?")
            );
            if let Some(detail) = entry["detail"].as_str() {
                println!("  {detail}");
            }
        }
        return;
    }
    if let Some(agents) = value.get("agents").and_then(Value::as_array) {
        if agents.is_empty() {
            println!("No agents.");
        }
        for agent in agents {
            println!(
                "{:<8} {:<16} {:<10} {}",
                agent["role"].as_str().unwrap_or("?"),
                agent["name"].as_str().unwrap_or("?"),
                agent["status"].as_str().unwrap_or("?"),
                agent["id"].as_str().unwrap_or("?")
            );
            if let Some(title) = agent["title"].as_str() {
                println!("         title {title}");
            }
            if let Some(profile) = agent["profile"].as_str() {
                println!("         profile {profile}");
            }
            if let Some(branch) = agent["branch"].as_str() {
                println!("         branch {branch}");
            }
        }
        return;
    }
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        if items.is_empty() {
            println!("No plan yet.");
        }
        for (index, item) in items.iter().enumerate() {
            let status = item["status"].as_str().unwrap_or("?");
            let marker = match status {
                "completed" => "✓",
                "in_progress" => "•",
                _ => "○",
            };
            println!(
                "{marker} {}. {}  [{}]",
                index + 1,
                item["title"].as_str().unwrap_or("?"),
                status
            );
        }
        return;
    }
    if let Some(paused) = value.get("paused").and_then(Value::as_bool) {
        println!(
            "{}",
            if paused {
                "paused the active run."
            } else {
                "no active run to pause."
            }
        );
        return;
    }
    if let Some(resumed) = value.get("resumed").and_then(Value::as_bool) {
        println!(
            "{}",
            if resumed {
                "resumed the active run."
            } else {
                "no paused run to resume."
            }
        );
        return;
    }
    if let Some(cancelled) = value.get("cancelled").and_then(Value::as_bool) {
        println!(
            "{}",
            if cancelled {
                "cancellation requested."
            } else {
                "no active run for that session."
            }
        );
        return;
    }
    if value.get("discarded").and_then(Value::as_bool) == Some(true) {
        println!("discarded uncommitted changes.");
        return;
    }
    if let Some(commit) = value.get("commit").and_then(Value::as_str) {
        println!("committed {commit}");
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            println!("  {message}");
        }
        if let Some(summary) = value.get("summary").and_then(Value::as_str) {
            println!("{summary}");
        }
        return;
    }
    if value.get("patch").is_some() {
        if let Some(status) = value.get("status").and_then(Value::as_str)
            && !status.is_empty()
        {
            println!("{status}\n");
        }
        if let Some(patch) = value.get("patch").and_then(Value::as_str) {
            if patch.is_empty() {
                println!("No changes.");
            } else {
                println!("{patch}");
            }
        }
        return;
    }
    if let Some(approvals) = value.get("approvals").and_then(Value::as_array) {
        if approvals.is_empty() {
            println!("No approvals.");
        }
        for approval in approvals {
            println!(
                "{}  {}  [{}]",
                approval["id"].as_str().unwrap_or("?"),
                approval["tool"].as_str().unwrap_or("?"),
                approval["status"].as_str().unwrap_or("?")
            );
            println!("  {}", approval["input"]);
        }
        return;
    }
    if let Some(events) = value.get("events").and_then(Value::as_array) {
        if events.is_empty() {
            println!("No events.");
        }
        for event in events {
            println!(
                "#{:04}  {}  {}",
                event["seq"].as_u64().unwrap_or(0),
                event["created_at"].as_str().unwrap_or("?"),
                event["type"].as_str().unwrap_or("?")
            );
            if !event["payload"].is_null() {
                println!("      {}", event["payload"]);
            }
        }
        return;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}
