use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use axum::Router;
use axum::routing::{get, post};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use vp_core::Engine;
use vp_core::agent::AgentService;
use vp_protocol::discovery::Discovery;
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION, now_rfc3339};
use vp_store::Store;
use vp_store::paths;

mod backup_status;
mod dispatch;
mod errors;
mod http;
mod metrics;
mod params;
mod server;
mod sweepers;
#[cfg(test)]
mod tests;

use crate::http::{gate_resolve_http, health, metrics_http, rpc_http, ws_handler};
use crate::server::{
    acquire_instance_lock, generate_token, remove_discovery, shutdown_signal, write_discovery,
};
use crate::sweepers::{
    spawn_approval_sweeper, spawn_browser_sweeper, spawn_checkpoint_sweeper,
    spawn_computer_sweeper, spawn_coordination_sweeper, spawn_effect_sweeper, spawn_gate_sweeper,
    spawn_gui_grace_sweeper, spawn_gui_timeline_retention_sweeper, spawn_memory_conflict_scanner,
    spawn_memory_embedding_reconciler, spawn_memory_stale_sweeper, spawn_memory_sweeper,
    spawn_orchestration_controller, spawn_process_sweeper, spawn_source_embedding_reconciler,
    spawn_stall_recovery_reconciler, spawn_task_stall_sweeper, spawn_terminal_sweeper,
    spawn_tool_output_sweeper, spawn_worktree_sweeper,
};

#[derive(Debug, Parser)]
#[command(name = "verlinpadd", about = "VerlinPark local agent daemon")]
struct Args {
    /// Override the local data directory (useful for development and tests).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Bind a specific loopback port. Defaults to an ephemeral port.
    #[arg(long)]
    port: Option<u16>,
}

#[derive(Clone)]
struct AppState {
    engine: Engine,
    agent: AgentService,
    token: Arc<String>,
    data_dir: PathBuf,
    started_at: String,
    started: Instant,
    pid: u32,
}

/// Recursive Leader -> Manager -> Worker -> Labour delegation keeps a boxed
/// async future per level. Give daemon workers a deterministic stack budget so
/// a deep team graph cannot overflow the default 2 MiB worker stack.
const DAEMON_WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(DAEMON_WORKER_STACK_BYTES)
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "verlinpadd=info,vp_core=info".into()),
        )
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();
    let data_dir = args.data_dir.clone().unwrap_or_else(paths::data_dir);
    paths::ensure_private_dir(&data_dir)?;
    let _lock_file = acquire_instance_lock(&data_dir)?;

    let store = Store::open(&data_dir)?;
    let engine = Engine::new(store);
    let recovered = engine.recover_interrupted_sessions().await?;
    if recovered > 0 {
        tracing::warn!(
            sessions = recovered,
            "marked sessions interrupted by the previous daemon as failed"
        );
    }
    match engine.reconcile_session_agent_assignments().await {
        Ok(0) => {}
        Ok(expired) => tracing::info!(
            expired,
            "released expired identity leases from the previous daemon"
        ),
        Err(error) => tracing::warn!(%error, "failed to reconcile identity leases"),
    }
    match engine.sweep_agent_inbox_expiry().await {
        Ok(0) => {}
        Ok(expired) => tracing::info!(expired, "expired due project inbox rows"),
        Err(error) => tracing::warn!(%error, "failed to sweep project inbox expiry"),
    }
    match engine.sweep_agent_workspaces(Some(1_000)).await {
        Ok(0) => {}
        Ok(released) => tracing::info!(released, "reconciled Worker workspace leases"),
        Err(error) => tracing::warn!(%error, "failed to reconcile Worker workspace leases"),
    }
    if let Err(error) = engine.retire_legacy_gui_leases().await {
        tracing::warn!(%error, "failed to retire legacy GUI leases");
    }
    match engine.reconcile_supervised_processes().await {
        Ok(report)
            if report.adopted > 0
                || report.lost > 0
                || report.unknown > 0
                || report.skipped_imported > 0 =>
        {
            tracing::info!(
                adopted = report.adopted,
                lost = report.lost,
                unknown = report.unknown,
                restart_scheduled = report.restart_scheduled,
                restart_blocked = report.restart_blocked,
                skipped_imported = report.skipped_imported,
                "reconciled durable supervised processes"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "failed to reconcile supervised processes"),
    }
    if let Err(error) = engine.record_project_manifest().await {
        tracing::warn!(%error, "failed to write project manifest");
    }
    let agent = AgentService::from_env(engine.clone());
    match agent.resume_queued_sessions().await {
        Ok(0) => {}
        Ok(started) => tracing::info!(started, "re-enqueued persisted queued sessions"),
        Err(error) => tracing::warn!(%error, "failed to re-enqueue queued sessions"),
    }
    match agent.auto_resume_interrupted_runs().await {
        Ok(0) => {}
        Ok(started) => tracing::info!(started, "auto-resumed interrupted durable runs"),
        Err(error) => tracing::warn!(%error, "failed to auto-resume interrupted durable runs"),
    }
    let token = generate_token();
    let started_at = now_rfc3339();
    let state = AppState {
        engine,
        agent,
        token: Arc::new(token.clone()),
        data_dir: data_dir.clone(),
        started_at: started_at.clone(),
        started: Instant::now(),
        pid: std::process::id(),
    };
    state.agent.start_auto_resolver();
    state.agent.start_task_scheduler();
    state.agent.start_scaling_controller();
    spawn_worktree_sweeper(state.engine.clone());
    spawn_checkpoint_sweeper(state.engine.clone());
    spawn_tool_output_sweeper(state.engine.clone());
    spawn_memory_sweeper(state.engine.clone());
    spawn_memory_stale_sweeper(state.engine.clone());
    spawn_memory_conflict_scanner(state.engine.clone());
    spawn_memory_embedding_reconciler(state.engine.clone());
    spawn_source_embedding_reconciler(state.engine.clone());
    spawn_terminal_sweeper(state.engine.clone());
    spawn_browser_sweeper(state.engine.clone());
    spawn_computer_sweeper(state.engine.clone());
    spawn_gui_grace_sweeper(state.engine.clone());
    spawn_gui_timeline_retention_sweeper(state.engine.clone());
    spawn_task_stall_sweeper(state.engine.clone());
    spawn_effect_sweeper(state.engine.clone());
    spawn_gate_sweeper(state.engine.clone());
    spawn_coordination_sweeper(state.engine.clone());
    spawn_stall_recovery_reconciler(state.agent.clone());
    spawn_process_sweeper(state.engine.clone());
    spawn_approval_sweeper(state.engine.clone());
    spawn_orchestration_controller(state.engine.clone());

    let listener = TcpListener::bind(("127.0.0.1", args.port.unwrap_or(0))).await?;
    let addr = listener.local_addr()?;
    let discovery = Discovery {
        pid: state.pid,
        port: addr.port(),
        token,
        protocol: PROTOCOL_VERSION,
        version: APP_VERSION.to_string(),
        started_at: started_at.clone(),
    };
    write_discovery(&data_dir, &discovery)?;

    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_http))
        .route(
            "/rpc",
            post(rpc_http).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/ws", get(ws_handler))
        .route(
            "/gate/resolve",
            post(gate_resolve_http).layer(axum::extract::DefaultBodyLimit::max(16 * 1024)),
        )
        .with_state(state.clone());

    info!(
        addr = %addr,
        data_dir = %data_dir.display(),
        "verlinpadd listening"
    );

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    remove_discovery(&data_dir);
    result?;
    Ok(())
}
