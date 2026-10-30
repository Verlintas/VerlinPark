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
    /// List projects.
    Projects,
    /// Add a local repository as a project.
    ProjectAdd {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
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
    /// Show the validated team plan.
    TeamPlan { session: String },
    /// List the agents spawned for a session.
    Agents { session: String },
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
        } => {
            rpc(
                &discovery,
                "session.create",
                json!({
                    "project_id": project,
                    "goal": goal,
                    "token_budget": token_budget,
                }),
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
        Command::Agents { session } => {
            rpc(&discovery, "agent.list", json!({"session_id": session})).await?
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
    if let Some(policy) = value.get("policy") {
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
    if let Some(consistent) = value.get("consistent").and_then(Value::as_bool) {
        println!(
            "{}  events={}  status={} usage={} plan={} todos={} messages={} agents={} queue={}",
            if consistent { "consistent" } else { "MISMATCH" },
            value["event_count"].as_u64().unwrap_or(0),
            value["status_matches"].as_bool().unwrap_or(false),
            value["usage_matches"].as_bool().unwrap_or(false),
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
