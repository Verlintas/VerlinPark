use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "ping" => RpcResponse::success(
            id,
            PingResult {
                pong: true,
                version: APP_VERSION.to_string(),
                protocol: PROTOCOL_VERSION,
                uptime_secs: state.started.elapsed().as_secs(),
            },
        ),
        "server.info" => match server_info(state).await {
            Ok(info) => RpcResponse::success(id, info),
            Err(error) => internal_error(id, error),
        },
        "daemon.metrics" => match collect_daemon_metrics(state).await {
            Ok(metrics) => {
                let prometheus = metrics::render_prometheus(&metrics);
                RpcResponse::success(
                    id,
                    DaemonMetricsResult {
                        metrics,
                        prometheus,
                    },
                )
            }
            Err(error) => internal_error(id, error),
        },
        "server.doctor" => match state.engine.doctor_checks().await {
            Ok(mut checks) => {
                let info = state.agent.info();
                checks.push(DoctorCheck {
                    name: "provider".into(),
                    status: if info.configured { "ok" } else { "warn" }.into(),
                    detail: if info.configured {
                        format!(
                            "{} / {}",
                            info.provider.as_deref().unwrap_or("?"),
                            info.model.as_deref().unwrap_or("?")
                        )
                    } else {
                        "no provider configured".to_string()
                    },
                });
                let ok = !checks.iter().any(|check| check.status == "error");
                RpcResponse::success(id, DoctorReport { checks, ok })
            }
            Err(error) => internal_error(id, error),
        },
        "event.list" => match parse_params::<ListEventsParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_events(params.session_id, params.since_seq, params.limit)
                .await
            {
                Ok(events) => RpcResponse::success(id, EventListResult { events }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "daemon.agent.status" => match parse_params::<DaemonAgentStatusParams>(request.params) {
            Ok(_) => {
                let data_dir = state.data_dir.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let paths =
                        vp_core::daemon_agent::DaemonAgentPaths::from_env(Some(data_dir), None)?;
                    let runner = vp_core::daemon_agent::SystemLaunchctl;
                    Ok::<_, anyhow::Error>(
                        vp_core::daemon_agent::DaemonAgent::new(paths, &runner).status(),
                    )
                })
                .await;
                match status {
                    Ok(Ok(status)) => {
                        RpcResponse::success(id, DaemonAgentStatusResult { ..status })
                    }
                    Ok(Err(error)) => internal_error(id, error),
                    Err(error) => internal_error(id, anyhow::anyhow!(error)),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "dashboard.snapshot" => match parse_params::<DashboardParams>(request.params) {
            Ok(params) => match state
                .engine
                .dashboard_snapshot(params.project_id.as_deref())
                .await
            {
                Ok(mut result) => {
                    result.summary.agent = Some(state.agent.info());
                    RpcResponse::success(id, result)
                }
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "event.subscribe" => RpcResponse::success(id, json!({"subscribed": true})),
        "auth" => RpcResponse::success(id, json!({"ok": true})),
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}
