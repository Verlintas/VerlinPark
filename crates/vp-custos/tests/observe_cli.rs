use std::path::Path;
use std::time::Duration;

use serde_json::json;
use tokio::process::{Child, Command};

fn seed_database(data_dir: &Path) {
    let path = vp_custos::database_path(data_dir);
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(
            "
            PRAGMA user_version = 1;
            CREATE TABLE sessions(status TEXT);
            CREATE TABLE events(type TEXT, created_at TEXT);
            CREATE TABLE runtime_leases(released_at TEXT, expires_at TEXT);
            CREATE TABLE session_runs(status TEXT);
            CREATE TABLE plan_tasks(status TEXT);
            CREATE TABLE merge_queue(status TEXT);
            CREATE TABLE effect_receipts(state TEXT);
            CREATE TABLE approvals(status TEXT);
            CREATE TABLE supervised_processes(state TEXT);
            INSERT INTO sessions VALUES ('running');
            INSERT INTO events VALUES ('agent.message', datetime('now'));
            INSERT INTO session_runs VALUES ('running');
            INSERT INTO plan_tasks VALUES ('pending');
            INSERT INTO merge_queue VALUES ('queued');
            INSERT INTO effect_receipts VALUES ('done');
            INSERT INTO approvals VALUES ('approved');
            INSERT INTO supervised_processes VALUES ('running');
            INSERT INTO runtime_leases VALUES (NULL, datetime('now', '+10 minutes'));
            ",
        )
        .unwrap();
}

async fn spawn_daemon(data_dir: &Path, socket: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_custosd"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--socket")
        .arg(socket)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("RUST_LOG", "error")
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn wait_for_socket(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("custos socket did not appear at {}", path.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observe_only_status_health_charter_and_ping() {
    let dir = tempfile::TempDir::new().unwrap();
    seed_database(dir.path());
    let socket = dir.path().join("custos").join("custos.sock");
    let mut child = spawn_daemon(dir.path(), &socket).await;
    wait_for_socket(&socket).await;

    let status = vp_custos::client::status(&socket).await.unwrap();
    assert_eq!(status["schema"], "verlinpark.custos_status/v1");
    assert_eq!(status["observe_only"], true);
    assert_eq!(status["role"], "core");
    assert_eq!(status["counts"]["sessions_total"], 1);
    assert!(
        status["dimensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dimension| dimension["key"] == "data" && dimension["state"] == "ok")
    );

    let health = vp_custos::client::health(&socket).await.unwrap();
    assert_eq!(health["probe"]["db_open"], true);
    assert_eq!(health["probe"]["user_version"], 1);

    let charter = vp_custos::client::charter(&socket).await.unwrap();
    assert_eq!(charter["observe_only"], true);
    assert!(
        charter["actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| action["enabled"] == false || action["class"] == "observe")
    );

    let ping = vp_custos::client::ping(&socket).await.unwrap();
    assert_eq!(ping["role"], "core");
    assert_eq!(ping["protocol"], 1);

    let error = vp_custos::client::call(&socket, "act", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("METHOD_NOT_FOUND"));

    child.kill().await.unwrap();
    let _ = child.wait().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_database_is_reported_not_created() {
    let dir = tempfile::TempDir::new().unwrap();
    let socket = dir.path().join("custos").join("custos.sock");
    let mut child = spawn_daemon(dir.path(), &socket).await;
    wait_for_socket(&socket).await;
    let status = vp_custos::client::status(&socket).await.unwrap();
    assert_eq!(status["counts"]["sessions_total"], 0);
    assert!(
        status["dimensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dimension| dimension["key"] == "data" && dimension["state"] == "watch")
    );
    assert!(!vp_custos::database_path(dir.path()).exists());
    child.kill().await.unwrap();
    let _ = child.wait().await;
}
