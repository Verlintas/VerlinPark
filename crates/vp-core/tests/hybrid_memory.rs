//! End-to-end tests for the optional hybrid memory lane.
//!
//! These exercise the same `Store` + `Engine` APIs the daemon uses, against a
//! fake OpenAI-compatible `/embeddings` server. Vectors are synthetic and
//! deterministic; no provider is contacted.

use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use vp_core::Engine;
use vp_protocol::types::{MemoryEmbeddingPolicy, MemoryItem, MemoryPolicy, MemorySearchParams};
use vp_store::Store;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn harness(goal: &str) -> (TempDir, TempDir, Engine, String, String) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("hybrid-memory"))
        .unwrap();
    let session = store.create_session(&project.id, goal).unwrap();
    (data, repo, Engine::new(store), project.id, session.id)
}

fn embedding_policy(server_uri: String, env_name: &str, model: &str) -> MemoryEmbeddingPolicy {
    MemoryEmbeddingPolicy {
        enabled: true,
        endpoint: Some(server_uri),
        model: Some(model.to_string()),
        dimensions: 8,
        batch_size: 8,
        timeout_secs: 5,
        max_candidates: 32,
        min_similarity: 0.2,
        api_key_env: Some(env_name.to_string()),
        ..MemoryEmbeddingPolicy::default()
    }
}

fn policy_with(embedding: MemoryEmbeddingPolicy) -> MemoryPolicy {
    MemoryPolicy {
        enabled: true,
        max_items: Some(100),
        max_age_days: Some(180),
        task_result_verify_days: Some(90),
        default_verify_days: Some(365),
        auto_supersede: true,
        brief_max_chars: Some(4000),
        sweep_stale: false,
        embedding: Some(embedding),
    }
}

fn vector_for(text: &str) -> Vec<f64> {
    let lower = text.to_ascii_lowercase();
    let first = if lower.contains("rollout")
        || lower.contains("ship")
        || lower.contains("deployment")
        || lower.contains("production")
        || lower.contains("release handoff")
    {
        1.0
    } else {
        0.0
    };
    let second =
        if lower.contains("database") || lower.contains("migration") || lower.contains("schema") {
            1.0
        } else {
            0.0
        };
    let mut vector = vec![first, second, 0.05, 0.04, 0.03, 0.02, 0.01, 0.005];
    if first == 0.0 && second == 0.0 {
        vector[2] = 1.0;
    }
    vector
}

fn embedding_response(inputs: &[String], include_usage: bool) -> Value {
    let data = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            json!({
                "object": "embedding",
                "index": index,
                "embedding": vector_for(input),
            })
        })
        .collect::<Vec<_>>();
    let mut response = json!({
        "object": "list",
        "model": "embed-test",
        "data": data,
    });
    if include_usage {
        response["usage"] = json!({
            "prompt_tokens": inputs.len() as u64 * 5,
            "total_tokens": inputs.len() as u64 * 5,
        });
    }
    response
}

async fn mount_normal_embeddings(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(move |request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let inputs = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            ResponseTemplate::new(200).set_body_json(embedding_response(&inputs, true))
        })
        .mount(server)
        .await;
}

fn use_env(name: &str, value: &str) {
    // SAFETY: every test uses a unique environment-variable name.
    unsafe { std::env::set_var(name, value) };
}

fn clear_env(name: &str) {
    // SAFETY: every test uses a unique environment-variable name.
    unsafe { std::env::remove_var(name) };
}

async fn set_embedding_policy(engine: &Engine, project_id: &str, embedding: MemoryEmbeddingPolicy) {
    engine
        .set_memory_policy(project_id, policy_with(embedding))
        .await
        .unwrap();
}

async fn reconcile(
    engine: &Engine,
    project_id: &str,
    embedding: &MemoryEmbeddingPolicy,
) -> (usize, usize, usize) {
    engine
        .reconcile_project_memory_embeddings(project_id, embedding.clone())
        .await
        .unwrap()
}

fn search_params(project_id: &str, query: &str) -> MemorySearchParams {
    MemorySearchParams {
        project_id: project_id.to_string(),
        query: query.to_string(),
        kind: None,
        kinds: Vec::new(),
        task_id: None,
        department: None,
        paths: Vec::new(),
        limit: Some(20),
        include_stale: false,
        max_chars: None,
        lexical_only: false,
    }
}

async fn create_memory(engine: &Engine, session_id: &str, title: &str, body: &str) -> MemoryItem {
    engine
        .create_memory(session_id, "fact", title, body, Vec::new())
        .await
        .unwrap()
}

#[tokio::test]
async fn disabled_and_absent_policies_make_zero_http_calls() {
    let server = MockServer::start().await;
    let (_data, _repo, engine, project_id, session_id) = harness("lexical only");
    let memory = create_memory(
        &engine,
        &session_id,
        "Deployment release",
        "deploy release notes",
    )
    .await;
    assert_eq!(memory.title, "Deployment release");

    // No embedding object at all.
    let result = engine
        .memory_search(search_params(&project_id, "release"))
        .await
        .unwrap();
    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0].source.as_deref(), Some("lexical"));
    assert_eq!(
        result.semantic.as_ref().map(|summary| summary.enabled),
        Some(false)
    );
    assert!(server.received_requests().await.unwrap().is_empty());

    // Configured but disabled: writes still succeed, but no recall/index call
    // is allowed to happen.
    let mut embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_OFF", "embed-test");
    embedding.enabled = false;
    engine
        .set_memory_policy(&project_id, policy_with(embedding.clone()))
        .await
        .unwrap();
    let second = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    assert_eq!(second.title, "Rollout checklist");
    let _ = engine
        .memory_search(search_params(&project_id, "deployment release"))
        .await
        .unwrap();
    let _ = engine.reconcile_memory_embeddings().await.unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn discovery_indexes_memories_and_serves_a_semantic_paraphrase() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_DISCOVERY";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("deployment release");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;

    let lexical = create_memory(
        &engine,
        &session_id,
        "Deployment release",
        "deploy release notes",
    )
    .await;
    let paraphrase = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;

    let (indexed, failed, skipped) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed, skipped), (2, 0, 0));

    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert!(status.enabled);
    assert!(status.configured);
    assert_eq!(status.indexable, 2);
    assert_eq!(status.indexed, 2);
    assert_eq!(status.coverage, 1.0);
    assert_eq!(status.usage.calls, 1);
    assert_eq!(status.usage.input_tokens, 10);
    assert!(status.last_error.is_none());
    let serialized_status = serde_json::to_string(&status).unwrap();
    assert!(!serialized_status.contains("VPEM"));
    assert!(!serialized_status.contains("how we ship"));

    let result = engine
        .memory_search(search_params(&project_id, "deployment release"))
        .await
        .unwrap();
    let summary = result.semantic.as_ref().unwrap();
    assert!(summary.enabled);
    assert_eq!(summary.model.as_deref(), Some("embed-test"));
    assert!(summary.semantic_candidates >= 2);
    assert!(summary.fallback.is_none());
    let paraphrase_match = result
        .memories
        .iter()
        .find(|matched| matched.memory.id == paraphrase.id)
        .expect("semantic-only paraphrase must be merged into results");
    assert_eq!(paraphrase_match.source.as_deref(), Some("semantic"));
    assert!(paraphrase_match.semantic_score.unwrap() > 0.9);
    assert!(paraphrase_match.lexical_score.unwrap() == 0.0);
    assert!(
        paraphrase_match
            .reasons
            .iter()
            .any(|reason| reason == "semantic")
    );
    let lexical_match = result
        .memories
        .iter()
        .find(|matched| matched.memory.id == lexical.id)
        .unwrap();
    assert!(lexical_match.lexical_score.unwrap() > 0.0);
    assert!(lexical_match.semantic_score.is_some());

    // The brief path goes through the same retrieval and records usage.
    let brief = engine
        .memory_brief_with_department(&session_id, None, None)
        .await
        .unwrap();
    assert!(brief.text.contains("Rollout checklist"));
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert!(
        status.usage.calls >= 3,
        "index + two recall requests are counted"
    );
    clear_env(env);
}

#[tokio::test]
async fn memory_write_succeeds_while_embedding_endpoint_is_failing() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_FAIL", "test-key");
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("embedding backend down"))
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, session_id) = harness("failure isolation");
    let embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_FAIL", "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;

    let memory = create_memory(
        &engine,
        &session_id,
        "Keep this memory",
        "the memory write must not depend on embeddings",
    )
    .await;
    assert_eq!(memory.title, "Keep this memory");
    assert!(engine.store().get_memory(&memory.id).unwrap().is_some());

    let (indexed, failed, skipped) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed, skipped), (0, 1, 0));
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.failed, 1);
    assert_eq!(status.indexable, 1);
    assert_eq!(status.indexed, 0);
    assert_eq!(status.coverage, 0.0);
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("500")
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3,
        "HTTP 500 is retried three times before the job fails"
    );

    // Lexical recall still works and reports the fallback reason.
    let result = engine
        .memory_search(search_params(&project_id, "memory write"))
        .await
        .unwrap();
    assert_eq!(result.memories.len(), 1);
    assert_eq!(
        result.memories[0].semantic_unavailable.as_deref(),
        Some("embedding endpoint returned HTTP 500: embedding backend down")
    );
    assert!(
        result.memories[0]
            .reasons
            .iter()
            .any(|reason| reason == "semantic_unavailable")
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        6,
        "the recall query makes its own three bounded attempts"
    );
    let serialized = serde_json::to_string(&result).unwrap();
    assert!(!serialized.contains("VPEM"));
    assert!(!serialized.contains("assertion"));
    clear_env("VP_TEST_HYBRID_FAIL");
}

#[tokio::test]
async fn stale_content_hash_reindexes_and_model_change_resets_coverage() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_REINDEX";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("reindex");
    let embedding = embedding_policy(server.uri(), env, "embed-model-a");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let (indexed, _, _) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!(indexed, 1);
    let requests_after_first = server.received_requests().await.unwrap().len();

    engine
        .memory_update(&memory.id, None, None, Some(vec!["release".to_string()]))
        .await
        .unwrap();
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.pending, 1);
    assert_eq!(
        status.indexed, 0,
        "pending re-index is not current coverage"
    );
    let (indexed, failed, _) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed), (1, 0));
    assert!(server.received_requests().await.unwrap().len() > requests_after_first);
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.pending, 0);
    assert_eq!(status.indexed, 1);

    let mut changed = embedding;
    changed.model = Some("embed-model-b".to_string());
    set_embedding_policy(&engine, &project_id, changed.clone()).await;
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("embed-model-b"));
    assert_eq!(status.indexed, 0, "old-model coverage is not current");
    assert_eq!(status.coverage, 0.0);
    let (indexed, failed, _) = reconcile(&engine, &project_id, &changed).await;
    assert_eq!((indexed, failed), (1, 0));
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.indexed, 1);
    assert_eq!(status.coverage, 1.0);
    clear_env(env);
}

#[tokio::test]
async fn rebuild_clears_and_requeues_and_delete_removes_coverage() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_REBUILD";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("rebuild");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let _ = reconcile(&engine, &project_id, &embedding).await;

    let rebuilt = engine.memory_embedding_rebuild(&project_id).await.unwrap();
    assert_eq!(rebuilt.enqueued, 1);
    assert!(rebuilt.cleared >= 2);
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.pending, 1);
    assert_eq!(status.indexed, 0);
    let _ = reconcile(&engine, &project_id, &embedding).await;

    let deleted = engine.delete_memory(&memory.id).await.unwrap();
    assert!(deleted.removed);
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.indexable, 0);
    assert_eq!(status.indexed, 0);
    assert_eq!(status.coverage, 0.0);
    assert!(status.recent_jobs.is_empty());
    clear_env(env);
}

#[tokio::test]
async fn partial_batches_and_dimension_mismatches_fail_individually() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_PARTIAL", "test-key");
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let inputs = body["input"].as_array().unwrap();
            if inputs.len() == 1 {
                // Query embeddings succeed.
                return ResponseTemplate::new(200)
                    .set_body_json(embedding_response(&["probe".to_string()], true));
            }
            // Two indexing inputs: one good vector, one wrong dimension.
            ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"index": 0, "embedding": [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]},
                    {"index": 1, "embedding": [1.0, 0.0, 0.0]},
                ],
                "usage": {"prompt_tokens": 7}
            }))
        })
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, session_id) = harness("partial");
    let embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_PARTIAL", "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _first = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let _second = create_memory(
        &engine,
        &session_id,
        "Database migration",
        "schema migration notes",
    )
    .await;
    let (indexed, failed, skipped) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed, skipped), (1, 1, 0));
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.indexed, 1);
    assert_eq!(status.failed, 1);
    assert_eq!(status.usage.calls, 1);
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("expected 8")
    );

    clear_env("VP_TEST_HYBRID_PARTIAL");
}

#[tokio::test]
async fn missing_api_key_reports_status_and_does_not_burn_retries() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_MISSING_KEY";
    clear_env(env);
    let (_data, _repo, engine, project_id, session_id) = harness("missing key");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;

    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains(env)
    );
    assert!(server.received_requests().await.unwrap().is_empty());

    let error = engine
        .reconcile_project_memory_embeddings(&project_id, embedding.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains(env));

    let result = engine
        .memory_search(search_params(&project_id, "rollout"))
        .await
        .unwrap();
    assert_eq!(result.memories.len(), 1);
    assert!(result.memories[0].semantic_unavailable.is_some());
    assert!(server.received_requests().await.unwrap().is_empty());
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.failed, 0);
    assert_eq!(status.pending, 1);
}

#[tokio::test]
async fn malformed_policy_is_rejected_before_storage() {
    let (_data, _repo, engine, project_id, _session_id) = harness("validation");
    let mut policy = policy_with(MemoryEmbeddingPolicy {
        enabled: true,
        endpoint: Some("https://example.com/v1".to_string()),
        model: Some("embed-test".to_string()),
        dimensions: 8,
        batch_size: 4,
        timeout_secs: 5,
        max_candidates: 8,
        min_similarity: 0.2,
        api_key_env: None,
        ..MemoryEmbeddingPolicy::default()
    });
    policy.embedding.as_mut().unwrap().dimensions = 1;
    let error = engine
        .set_memory_policy(&project_id, policy)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("dimensions"));

    let policy = policy_with(MemoryEmbeddingPolicy {
        enabled: true,
        endpoint: Some("https://user:pass@example.com/v1".to_string()),
        model: Some("embed-test".to_string()),
        ..MemoryEmbeddingPolicy::default()
    });
    let error = engine
        .set_memory_policy(&project_id, policy)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("userinfo"));

    let policy = policy_with(MemoryEmbeddingPolicy {
        enabled: true,
        endpoint: Some("https://example.com/v1".to_string()),
        model: Some("embed-test".to_string()),
        api_key_env: Some("sk-secret-value".to_string()),
        ..MemoryEmbeddingPolicy::default()
    });
    let error = engine
        .set_memory_policy(&project_id, policy)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("api_key_env"));

    // Stored policy stayed at the default (no embedding).
    let stored = engine.get_memory_policy(&project_id).await.unwrap().policy;
    assert!(stored.embedding.is_none());
}

#[tokio::test]
async fn test_rpc_probe_is_explicit_and_can_run_while_disabled() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_PROBE", "test-key");
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let inputs = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            ResponseTemplate::new(200).set_body_json(embedding_response(&inputs, true))
        })
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, _session_id) = harness("probe");
    let mut embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_PROBE", "embed-test");
    embedding.enabled = false;
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let probe = engine
        .memory_embedding_test(&project_id, Some("release planning"))
        .await
        .unwrap();
    assert!(probe.ok);
    assert_eq!(probe.model.as_deref(), Some("embed-test"));
    assert_eq!(probe.dimensions, 8);
    assert!(probe.provider_reported);
    assert!(probe.error.is_none());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    let rebuild = engine.memory_embedding_rebuild(&project_id).await.unwrap();
    assert_eq!(rebuild.enqueued, 0);
    assert_eq!(rebuild.cleared, 0);
    clear_env("VP_TEST_HYBRID_PROBE");
}

#[tokio::test]
async fn usage_is_recorded_separately_from_session_budgets() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_USAGE", "test-key");
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let inputs = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            // No usage field: the runtime must record an honest estimate.
            ResponseTemplate::new(200).set_body_json(embedding_response(&inputs, false))
        })
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, session_id) = harness("usage");
    let embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_USAGE", "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let _ = reconcile(&engine, &project_id, &embedding).await;
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.usage.calls, 1);
    assert_eq!(status.usage.provider_reported_calls, 0);
    assert!(status.usage.estimated_tokens > 0);
    assert_eq!(status.usage.input_tokens, status.usage.estimated_tokens);
    assert!(status.usage.bytes > 0);

    let ledger = engine.usage_ledger(&session_id).await.unwrap();
    assert!(
        ledger.entries.is_empty(),
        "embedding calls must not appear in a session usage ledger"
    );
    clear_env("VP_TEST_HYBRID_USAGE");
}

#[tokio::test]
async fn reconcile_all_processes_enabled_projects_and_skips_disabled_ones() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_ALL_PROJECTS";
    use_env(env, "test-key");
    let data = TempDir::new().unwrap();
    let repo_a = TempDir::new().unwrap();
    let repo_b = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project_a = store
        .add_project(repo_a.path().to_str().unwrap(), Some("enabled"))
        .unwrap();
    let project_b = store
        .add_project(repo_b.path().to_str().unwrap(), Some("disabled"))
        .unwrap();
    let session_a = store.create_session(&project_a.id, "enabled").unwrap();
    let session_b = store.create_session(&project_b.id, "disabled").unwrap();
    let engine = Engine::new(store);

    let embedding_a = embedding_policy(server.uri(), env, "embed-test");
    engine
        .set_memory_policy(&project_a.id, policy_with(embedding_a))
        .await
        .unwrap();
    let mut embedding_b = embedding_policy(server.uri(), env, "embed-test");
    embedding_b.enabled = false;
    engine
        .set_memory_policy(&project_b.id, policy_with(embedding_b))
        .await
        .unwrap();
    create_memory(
        &engine,
        &session_a.id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    create_memory(
        &engine,
        &session_b.id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;

    let (projects, indexed, failed, skipped) = engine.reconcile_memory_embeddings().await.unwrap();
    assert_eq!(projects, 1);
    assert_eq!(indexed, 1);
    assert_eq!(failed, 0);
    assert_eq!(skipped, 0);
    let status_a = engine.memory_embedding_status(&project_a.id).await.unwrap();
    assert_eq!(status_a.indexed, 1);
    let status_b = engine.memory_embedding_status(&project_b.id).await.unwrap();
    assert!(!status_b.enabled);
    assert!(status_b.configured);
    assert_eq!(status_b.indexed, 0);
    assert_eq!(status_b.pending, 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    clear_env(env);
}

#[tokio::test]
async fn rate_limited_indexing_retries_then_records_one_successful_call() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_RATE_LIMIT", "test-key");
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attempts_clone = attempts.clone();
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(move |request: &Request| {
            let attempt = attempts_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if attempt == 0 {
                return ResponseTemplate::new(429)
                    .insert_header("retry-after", "0")
                    .set_body_string("slow down");
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let inputs = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            ResponseTemplate::new(200).set_body_json(embedding_response(&inputs, true))
        })
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, session_id) = harness("retry");
    let embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_RATE_LIMIT", "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let (indexed, failed, _) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed), (1, 0));
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.usage.calls, 1, "only the successful batch is billed");
    assert_eq!(status.failed, 0);
    clear_env("VP_TEST_HYBRID_RATE_LIMIT");
}

#[tokio::test]
async fn model_change_hides_old_semantics_until_reindex() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_MODEL_SWITCH";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("model switch");
    let embedding_a = embedding_policy(server.uri(), env, "embed-model-a");
    set_embedding_policy(&engine, &project_id, embedding_a.clone()).await;
    let paraphrase = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let _ = reconcile(&engine, &project_id, &embedding_a).await;

    let mut embedding_b = embedding_a;
    embedding_b.model = Some("embed-model-b".to_string());
    set_embedding_policy(&engine, &project_id, embedding_b.clone()).await;
    let before = engine
        .memory_search(search_params(&project_id, "deployment release"))
        .await
        .unwrap();
    assert!(
        before
            .memories
            .iter()
            .all(|matched| matched.memory.id != paraphrase.id),
        "old-model vectors are ignored before re-index"
    );
    assert_eq!(
        before.semantic.as_ref().unwrap().semantic_candidates,
        0,
        "query embedding still runs, but no current vectors contribute"
    );
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.indexed, 0);

    let (indexed, failed, _) = reconcile(&engine, &project_id, &embedding_b).await;
    assert_eq!((indexed, failed), (1, 0));
    let after = engine
        .memory_search(search_params(&project_id, "deployment release"))
        .await
        .unwrap();
    let matched = after
        .memories
        .iter()
        .find(|matched| matched.memory.id == paraphrase.id)
        .expect("re-indexed semantics restore paraphrase recall");
    assert_eq!(matched.source.as_deref(), Some("semantic"));
    clear_env(env);
}

#[tokio::test]
async fn memory_brief_falls_back_to_lexical_when_semantic_query_fails() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_BRIEF_FAIL", "test-key");
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, session_id) = harness("rollout checklist");
    let embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_BRIEF_FAIL", "embed-test");
    set_embedding_policy(&engine, &project_id, embedding).await;
    let memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let brief = engine.memory_brief(&session_id, None).await.unwrap();
    assert!(brief.text.contains("Rollout checklist"));
    let matched = brief
        .memories
        .iter()
        .find(|matched| matched.memory.id == memory.id)
        .unwrap();
    assert!(matched.semantic_unavailable.is_some());
    assert!(matched.semantic_score.is_none());
    clear_env("VP_TEST_HYBRID_BRIEF_FAIL");
}

#[tokio::test]
async fn prune_removes_derived_rows_and_keeps_usage_history() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_PRUNE";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("prune");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _first = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let second = create_memory(
        &engine,
        &session_id,
        "Database migration",
        "schema migration notes",
    )
    .await;
    let _ = reconcile(&engine, &project_id, &embedding).await;
    let before = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(before.indexed, 2);
    assert_eq!(before.usage.calls, 1);

    let pruned = engine
        .prune_memories(&project_id, None, Some(1), false)
        .await
        .unwrap();
    assert_eq!(pruned.removed, 1);
    let after = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(after.indexable, 1);
    assert_eq!(after.indexed, 1);
    assert_eq!(
        after.usage.calls, 1,
        "usage is accounting history and survives pruning"
    );
    assert!(engine.store().get_memory(&second.id).unwrap().is_some());
    clear_env(env);
}

#[tokio::test]
async fn lexical_only_requests_skip_the_embedding_endpoint() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_LEXICAL_ONLY";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("lexical only request");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _lexical = create_memory(
        &engine,
        &session_id,
        "Deployment release",
        "deploy release notes",
    )
    .await;
    let paraphrase = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let _ = reconcile(&engine, &project_id, &embedding).await;
    let requests_before = server.received_requests().await.unwrap().len();

    let mut params = search_params(&project_id, "deployment release");
    params.lexical_only = true;
    let result = engine.memory_search(params).await.unwrap();
    assert_eq!(
        result.semantic.as_ref().map(|summary| summary.enabled),
        Some(false)
    );
    assert_eq!(result.semantic.as_ref().unwrap().semantic_candidates, 0);
    assert!(
        result
            .memories
            .iter()
            .all(|matched| matched.memory.id != paraphrase.id),
        "lexical-only must not merge semantic-only candidates"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        requests_before,
        "lexical-only makes zero embedding requests"
    );
    clear_env(env);
}

#[tokio::test]
async fn auth_failure_is_terminal_and_not_retried_automatically() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_AUTH", "bad-key");
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid key"))
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, session_id) = harness("auth");
    let embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_AUTH", "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let (indexed, failed, _) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed), (0, 1));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.failed, 1);
    assert_eq!(status.recent_jobs[0].next_attempt_at, None);
    assert_eq!(status.recent_jobs[0].attempts, 1);

    let (indexed, failed, skipped) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed, skipped), (0, 0, 0));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "terminal auth failures are not retried"
    );
    clear_env("VP_TEST_HYBRID_AUTH");
}

#[tokio::test]
async fn failed_job_retries_after_backoff_and_then_indexes() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_BACKOFF";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("backoff");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    engine
        .store()
        .mark_memory_embedding_attempted(&memory.id, "embed-test", false)
        .unwrap();
    engine
        .store()
        .record_memory_embedding_failure(
            &memory.id,
            "embed-test",
            1,
            "simulated transient failure",
            Some("2000-01-01T00:00:00Z"),
        )
        .unwrap();
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.failed, 1);

    let (indexed, failed, skipped) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed, skipped), (1, 0, 0));
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.indexed, 1);
    assert_eq!(status.failed, 0);
    assert_eq!(status.pending, 0);
    clear_env(env);
}

#[tokio::test]
async fn rebuild_while_disabled_clears_vectors_but_keeps_usage() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_CLEAR_DISABLED";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("disabled rebuild");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let _ = reconcile(&engine, &project_id, &embedding).await;
    let before = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(before.indexed, 1);
    assert_eq!(before.usage.calls, 1);

    let mut disabled = embedding;
    disabled.enabled = false;
    engine
        .set_memory_policy(&project_id, policy_with(disabled.clone()))
        .await
        .unwrap();
    let requests_before = server.received_requests().await.unwrap().len();
    let rebuilt = engine.memory_embedding_rebuild(&project_id).await.unwrap();
    assert!(!rebuilt.enabled);
    assert_eq!(rebuilt.enqueued, 0);
    assert!(rebuilt.cleared >= 2);
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.indexed, 0);
    assert_eq!(status.pending, 0);
    assert_eq!(status.usage.calls, 1, "usage history is not derived state");
    let _ = engine
        .memory_search(search_params(&project_id, "rollout"))
        .await
        .unwrap();
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        requests_before,
        "disabled search performs no embedding call"
    );
    clear_env(env);
}

#[tokio::test]
async fn superseding_a_memory_deletes_old_vectors_and_indexes_the_new_claim() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_SUPERSEDE";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("supersede");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let first = create_memory(&engine, &session_id, "Release policy", "ship every Friday").await;
    let _ = reconcile(&engine, &project_id, &embedding).await;
    let second = create_memory(
        &engine,
        &session_id,
        "Release policy",
        "ship only after a green integration run",
    )
    .await;
    assert_ne!(first.id, second.id);
    let stored = engine.store().get_memory(&first.id).unwrap().unwrap();
    assert_eq!(stored.status, "superseded");
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.indexable, 1, "superseded claims are not indexable");
    let (indexed, failed, _) = reconcile(&engine, &project_id, &embedding).await;
    assert_eq!((indexed, failed), (1, 0));
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.indexable, 1);
    assert_eq!(status.indexed, 1);
    let result = engine
        .memory_search(search_params(&project_id, "release policy"))
        .await
        .unwrap();
    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0].memory.id, second.id);
    clear_env(env);
}

#[tokio::test]
async fn stale_semantic_matches_require_include_stale() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_STALE";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("stale");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let _ = reconcile(&engine, &project_id, &embedding).await;
    engine
        .memory_update(&memory.id, Some("stale"), None, None)
        .await
        .unwrap();
    let hidden = engine
        .memory_search(search_params(&project_id, "deployment release"))
        .await
        .unwrap();
    assert!(
        hidden
            .memories
            .iter()
            .all(|matched| matched.memory.id != memory.id)
    );
    let mut params = search_params(&project_id, "deployment release");
    params.include_stale = true;
    let visible = engine.memory_search(params).await.unwrap();
    let matched = visible
        .memories
        .iter()
        .find(|matched| matched.memory.id == memory.id)
        .expect("stale semantic match is visible only when requested");
    assert!(matched.semantic_score.unwrap() > 0.9);
    assert!(matched.reasons.iter().any(|reason| reason == "stale"));
    clear_env(env);
}

#[tokio::test]
async fn empty_query_reports_semantic_fallback_without_a_request() {
    let server = MockServer::start().await;
    mount_normal_embeddings(&server).await;
    let env = "VP_TEST_HYBRID_EMPTY_QUERY";
    use_env(env, "test-key");
    let (_data, _repo, engine, project_id, session_id) = harness("empty query");
    let embedding = embedding_policy(server.uri(), env, "embed-test");
    set_embedding_policy(&engine, &project_id, embedding.clone()).await;
    let _memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let _ = reconcile(&engine, &project_id, &embedding).await;
    let requests_before = server.received_requests().await.unwrap().len();
    let result = engine
        .memory_search(search_params(&project_id, "   "))
        .await
        .unwrap();
    let summary = result.semantic.as_ref().unwrap();
    assert_eq!(summary.fallback.as_deref(), Some("query text is empty"));
    assert_eq!(summary.semantic_candidates, 0);
    assert!(!result.memories.is_empty());
    assert!(
        result.memories[0]
            .semantic_unavailable
            .as_deref()
            .unwrap()
            .contains("empty")
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        requests_before
    );
    clear_env(env);
}

#[tokio::test]
async fn probe_timeout_is_reported_without_exposing_vectors_or_keys() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_PROBE_TIMEOUT", "secret-key-value");
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": []}))
                .set_delay(Duration::from_millis(1_500)),
        )
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, _session_id) = harness("probe timeout");
    let mut embedding =
        embedding_policy(server.uri(), "VP_TEST_HYBRID_PROBE_TIMEOUT", "embed-test");
    embedding.timeout_secs = 1;
    set_embedding_policy(&engine, &project_id, embedding).await;
    let probe = engine
        .memory_embedding_test(&project_id, None)
        .await
        .unwrap();
    assert!(!probe.ok);
    let error = probe.error.as_deref().unwrap();
    assert!(error.contains("timed out"), "unexpected error: {error}");
    assert!(!error.contains("secret-key-value"));
    assert!(!error.contains("VPEM"));
    clear_env("VP_TEST_HYBRID_PROBE_TIMEOUT");
}

#[tokio::test]
async fn embedding_policy_round_trips_through_the_rpc_surface() {
    let (_data, _repo, engine, project_id, _session_id) = harness("policy roundtrip");
    let embedding = MemoryEmbeddingPolicy {
        enabled: false,
        endpoint: Some("http://127.0.0.1:9/v1".to_string()),
        model: Some("embed-roundtrip".to_string()),
        dimensions: 64,
        batch_size: 2,
        timeout_secs: 9,
        max_candidates: 12,
        min_similarity: 0.42,
        api_key_env: Some("EMBED_ROUNDTRIP_KEY".to_string()),
        ..MemoryEmbeddingPolicy::default()
    };
    let policy = policy_with(embedding.clone());
    engine
        .set_memory_policy(&project_id, policy.clone())
        .await
        .unwrap();
    let stored = engine.get_memory_policy(&project_id).await.unwrap().policy;
    assert_eq!(stored, policy);
    assert_eq!(stored.embedding, Some(embedding));
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert!(!status.enabled);
    assert!(status.configured);
    assert_eq!(status.model.as_deref(), Some("embed-roundtrip"));
    assert_eq!(status.dimensions, 64);
    assert_eq!(status.indexable, 0);
    assert!(status.usage.calls == 0);
}

#[tokio::test]
async fn tag_update_commits_and_queues_even_when_the_endpoint_is_down() {
    let server = MockServer::start().await;
    use_env("VP_TEST_HYBRID_UPDATE_FAIL", "test-key");
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .mount(&server)
        .await;
    let (_data, _repo, engine, project_id, session_id) = harness("update failure");
    let embedding = embedding_policy(server.uri(), "VP_TEST_HYBRID_UPDATE_FAIL", "embed-test");
    set_embedding_policy(&engine, &project_id, embedding).await;
    let memory = create_memory(
        &engine,
        &session_id,
        "Rollout checklist",
        "how we ship a build to customers",
    )
    .await;
    let updated = engine
        .memory_update(
            &memory.id,
            None,
            None,
            Some(vec!["release".to_string(), "production".to_string()]),
        )
        .await
        .unwrap();
    assert_eq!(
        updated.memory.tags,
        vec!["release".to_string(), "production".to_string()]
    );
    let status = engine.memory_embedding_status(&project_id).await.unwrap();
    assert_eq!(status.pending, 1);
    assert_eq!(status.failed, 0);
    assert!(server.received_requests().await.unwrap().is_empty());
    clear_env("VP_TEST_HYBRID_UPDATE_FAIL");
}
