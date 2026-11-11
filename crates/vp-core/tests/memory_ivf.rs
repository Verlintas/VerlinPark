//! End-to-end tests for the opt-in IVF memory index.
//!
//! These use the same `Store` + `Engine` APIs as the daemon and a fake
//! OpenAI-compatible `/embeddings` server. Vectors are synthetic and
//! deterministic: a text containing `cluster N` lands near axis `N`, so both
//! k-means and semantic recall have known structure. No provider is contacted.

use serde_json::{Value, json};
use tempfile::TempDir;
use vp_core::Engine;
use vp_core::memory::encode_embedding_vector;
use vp_core::memory_index::{audit_index, maintain_index, rebuild_index};
use vp_protocol::types::{
    MemoryEmbeddingPolicy, MemoryIndexMode, MemoryItem, MemoryPolicy, MemorySearchParams,
};
use vp_store::Store;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MODEL: &str = "embed-ivf";

struct Harness {
    _data: TempDir,
    _repo: TempDir,
    engine: Engine,
    store: Store,
    project_id: String,
    session_id: String,
}

fn harness(goal: &str) -> Harness {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("ivf-memory"))
        .unwrap();
    let session = store.create_session(&project.id, goal).unwrap();
    Harness {
        _data: data,
        _repo: repo,
        engine: Engine::new(store.clone()),
        store,
        project_id: project.id,
        session_id: session.id,
    }
}

fn embedding_policy(server_uri: String, env_name: &str) -> MemoryEmbeddingPolicy {
    MemoryEmbeddingPolicy {
        enabled: true,
        endpoint: Some(server_uri),
        model: Some(MODEL.to_string()),
        dimensions: 8,
        batch_size: 64,
        timeout_secs: 5,
        max_candidates: 32,
        min_similarity: 0.2,
        api_key_env: Some(env_name.to_string()),
        index: MemoryIndexMode::Ivf,
        ivf_nlist: 64,
        ivf_nprobe: 8,
        ivf_min_vectors: 256,
        ivf_rebuild_vectors: 4096,
    }
}

fn policy_with(embedding: MemoryEmbeddingPolicy) -> MemoryPolicy {
    MemoryPolicy {
        enabled: true,
        max_items: Some(10_000),
        max_age_days: Some(180),
        task_result_verify_days: Some(90),
        default_verify_days: Some(365),
        auto_supersede: true,
        brief_max_chars: Some(4000),
        sweep_stale: false,
        embedding: Some(embedding),
    }
}

fn first_number(text: &str) -> Option<usize> {
    let digits = text
        .chars()
        .skip_while(|character| !character.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
}

fn vector_for(text: &str) -> Vec<f64> {
    let cluster = first_number(text)
        .map(|value| value % 8)
        .unwrap_or_else(|| {
            text.bytes()
                .fold(0usize, |acc, byte| acc.wrapping_add(byte as usize))
                % 8
        });
    let mut vector = vec![0.004; 8];
    vector[cluster] = 1.0;
    vector[(cluster + 1) % 8] = 0.012;
    vector[(cluster + 2) % 8] = 0.007;
    vector
}

fn embedding_response(inputs: &[String]) -> Value {
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
    json!({
        "object": "list",
        "model": MODEL,
        "data": data,
        "usage": {"prompt_tokens": inputs.len() as u64 * 5},
    })
}

async fn mount_embeddings(server: &MockServer) {
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
            ResponseTemplate::new(200).set_body_json(embedding_response(&inputs))
        })
        .mount(server)
        .await;
}

fn use_env(name: &str, value: &str) {
    // SAFETY: every test uses a unique environment-variable name.
    unsafe { std::env::set_var(name, value) };
}

async fn set_policy(engine: &Engine, project_id: &str, embedding: MemoryEmbeddingPolicy) {
    engine
        .set_memory_policy(project_id, policy_with(embedding))
        .await
        .unwrap();
}

async fn create_cluster_memory(
    engine: &Engine,
    session_id: &str,
    cluster: usize,
    item: usize,
) -> MemoryItem {
    engine
        .create_memory(
            session_id,
            "fact",
            &format!("cluster {cluster} item {item}"),
            &format!("recorded note {item}"),
            Vec::new(),
        )
        .await
        .unwrap()
}

async fn reconcile_until_indexed(engine: &Engine, project_id: &str, expected: u32) {
    for _ in 0..20 {
        engine.reconcile_memory_embeddings().await.unwrap();
        let status = engine.memory_embedding_status(project_id).await.unwrap();
        if status.indexed >= expected {
            return;
        }
    }
    let status = engine.memory_embedding_status(project_id).await.unwrap();
    panic!("embedding reconciler did not reach {expected} indexed rows: {status:?}");
}

fn search_params(project_id: &str, query: &str, limit: u32) -> MemorySearchParams {
    MemorySearchParams {
        project_id: project_id.to_string(),
        query: query.to_string(),
        kind: None,
        kinds: Vec::new(),
        task_id: None,
        department: None,
        paths: Vec::new(),
        limit: Some(limit),
        include_stale: false,
        max_chars: None,
        lexical_only: false,
    }
}

#[tokio::test]
async fn ivf_recall_beats_lexical_baseline_and_reports_generation() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_RECALL";
    use_env(env, "test-key");
    let harness = harness("ivf semantic retrieval");
    let embedding = embedding_policy(server.uri(), env);
    set_policy(&harness.engine, &harness.project_id, embedding.clone()).await;

    for index in 0..300usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 8, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 300).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(rebuild.rebuilt, "{rebuild:?}");
    assert_eq!(rebuild.vector_count, 300);
    assert_eq!(rebuild.assigned_vectors, 300);
    assert_eq!(rebuild.nlist, 64);
    assert!(rebuild.cluster_count > 1);
    let generation = rebuild.generation.expect("rebuilt index has a generation");

    let embedding_status = harness
        .engine
        .memory_embedding_status(&harness.project_id)
        .await
        .unwrap();
    let embedded_index = embedding_status
        .index
        .as_ref()
        .expect("embedding status carries derived index health");
    assert_eq!(embedded_index.mode, "ivf");
    assert_eq!(embedded_index.generation, Some(generation));
    assert_eq!(embedded_index.assigned_vectors, 300);

    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.project_id, harness.project_id);
    assert_eq!(status.index.mode, "ivf");
    assert!(status.index.ready, "{:?}", status.index);
    assert_eq!(status.index.generation, Some(generation));
    assert_eq!(status.index.vector_count, 300);
    assert_eq!(status.index.assigned_vectors, 300);
    assert_eq!(status.index.coverage, 1.0);
    assert_eq!(status.index.nlist, 64);
    assert_eq!(status.index.nprobe, 8);
    assert_eq!(status.index.fallback_count, 0);
    assert!(status.index.last_rebuilt_at.is_some());
    assert!(status.index.bytes > 0);
    let status_json = serde_json::to_string(&status).unwrap();
    assert!(!status_json.contains("VPIV"));
    assert!(!status_json.contains("centroid"));
    assert!(!status_json.contains("0.012"));

    // The query shares no lexical tokens with the memory titles, so a result
    // is evidence that the semantic lane (not LIKE/FTS) supplied it.
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 3", 10))
        .await
        .unwrap();
    assert!(!result.memories.is_empty());
    assert!(
        result
            .memories
            .iter()
            .all(|matched| matched.memory.title.starts_with("cluster 3")),
        "unexpected titles: {:?}",
        result
            .memories
            .iter()
            .map(|matched| matched.memory.title.clone())
            .collect::<Vec<_>>()
    );
    let summary = result.semantic.expect("semantic summary");
    assert!(summary.enabled);
    assert_eq!(summary.index_source.as_deref(), Some("ivf"));
    assert_eq!(summary.index_generation, Some(generation));
    assert!(summary.index_fallback.is_none());
    assert_eq!(summary.probed_clusters, 8);
    assert!(summary.candidate_vectors > 0);
    assert!(summary.semantic_candidates >= result.memories.len() as u32);
    assert!(summary.semantic_candidates <= 32);

    // Exact scan remains available with the same stored vectors. It is the
    // fallback and the comparison lane, not a separate provider.
    let mut scan_embedding = embedding.clone();
    scan_embedding.index = MemoryIndexMode::Scan;
    set_policy(&harness.engine, &harness.project_id, scan_embedding).await;
    let scan = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 3", 10))
        .await
        .unwrap();
    let scan_summary = scan.semantic.expect("scan summary");
    assert_eq!(scan_summary.index_source.as_deref(), Some("scan"));
    assert!(scan_summary.index_generation.is_none());
    assert!(scan_summary.index_fallback.is_none());
    assert!(!scan.memories.is_empty());
    assert!(
        scan.memories
            .iter()
            .all(|matched| matched.memory.title.starts_with("cluster 3"))
    );
}

#[tokio::test]
async fn incremental_assignments_cover_new_memories_without_rebuilding() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_INCREMENTAL";
    use_env(env, "test-key");
    let harness = harness("ivf incremental assignment");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    embedding.ivf_rebuild_vectors = 1_000;
    set_policy(&harness.engine, &harness.project_id, embedding).await;

    for index in 0..8usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 8).await;
    let first = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(first.rebuilt);
    let generation = first.generation.unwrap();
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.assigned_vectors, 8);

    let newcomer = harness
        .engine
        .create_memory(
            &harness.session_id,
            "fact",
            "cluster 2 newcomer",
            "fresh claim",
            Vec::new(),
        )
        .await
        .unwrap();
    // The daemon reconciler embeds the new row and runs bounded assignment
    // maintenance in the same tick. No explicit index rebuild is requested.
    reconcile_until_indexed(&harness.engine, &harness.project_id, 9).await;
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.generation, Some(generation));
    assert_eq!(status.index.vector_count, 9);
    assert_eq!(status.index.assigned_vectors, 9);
    assert_eq!(status.index.coverage, 1.0);

    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 2", 10))
        .await
        .unwrap();
    assert!(
        result
            .memories
            .iter()
            .any(|matched| matched.memory.id == newcomer.id),
        "the newly written vector must join recall without a rebuild"
    );
    assert_eq!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_generation),
        Some(generation)
    );
}

#[tokio::test]
async fn below_minimum_uses_exact_scan_and_scan_mode_clears_the_index() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_MIN";
    use_env(env, "test-key");
    let harness = harness("ivf minimum fallback");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 10;
    set_policy(&harness.engine, &harness.project_id, embedding.clone()).await;

    for index in 0..4usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 2, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 4).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(!rebuild.rebuilt);
    assert_eq!(rebuild.reason.as_deref(), Some("below_min_vectors"));
    assert_eq!(rebuild.vector_count, 4);

    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(!status.index.ready);
    assert_eq!(
        status.index.fallback_reason.as_deref(),
        Some("below_min_vectors")
    );
    assert_eq!(status.index.mode, "ivf");

    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 1", 10))
        .await
        .unwrap();
    let summary = result.semantic.expect("semantic summary");
    assert_eq!(summary.index_source.as_deref(), Some("scan"));
    assert_eq!(summary.index_fallback.as_deref(), Some("below_min_vectors"));
    assert!(!result.memories.is_empty());

    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.fallback_count, 1);
    assert_eq!(
        status.index.last_fallback_reason.as_deref(),
        Some("below_min_vectors")
    );

    let mut scan_policy = embedding;
    scan_policy.index = MemoryIndexMode::Scan;
    set_policy(&harness.engine, &harness.project_id, scan_policy).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(!rebuild.rebuilt);
    assert!(
        rebuild
            .reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("index_mode_scan"))
    );
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.mode, "scan");
    assert_eq!(status.index.fallback_reason.as_deref(), Some("scan_mode"));
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 1", 10))
        .await
        .unwrap();
    assert_eq!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_source.as_deref()),
        Some("scan")
    );
    assert!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_fallback.as_deref())
            .is_none()
    );
}

#[tokio::test]
async fn corrupt_index_falls_back_with_a_reason_and_explicit_rebuild_repairs_it() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_CORRUPT";
    use_env(env, "test-key");
    let harness = harness("ivf corruption");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding).await;

    for index in 0..12usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 12).await;
    let first = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(first.rebuilt);
    let first_generation = first.generation.unwrap();

    // Persist a structurally invalid centroid blob through the same public
    // write path a buggy builder would use. Generation/assignment fencing is
    // bypassed, so recall must detect the corrupt payload itself.
    harness
        .store
        .save_memory_embedding_index(
            &harness.project_id,
            MODEL,
            8,
            0,
            4,
            4,
            12,
            b"corrupt-centroids",
        )
        .unwrap();

    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(!status.index.ready);
    assert!(
        status
            .index
            .fallback_reason
            .as_deref()
            .unwrap()
            .starts_with("index_corrupt")
    );

    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 2", 10))
        .await
        .unwrap();
    let summary = result.semantic.expect("semantic summary");
    assert_eq!(summary.index_source.as_deref(), Some("scan"));
    assert!(
        summary
            .index_fallback
            .as_deref()
            .unwrap()
            .starts_with("index_corrupt")
    );
    assert!(!result.memories.is_empty());

    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.fallback_count, 1);
    let _ = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 3", 10))
        .await
        .unwrap();
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.fallback_count, 2);

    let repaired = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(repaired.rebuilt);
    let repaired_generation = repaired.generation.unwrap();
    assert!(repaired_generation > first_generation);
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(status.index.ready);
    assert_eq!(status.index.generation, Some(repaired_generation));
    assert_eq!(
        status.index.fallback_count, 2,
        "diagnostic counters survive a successful generation change"
    );
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 2", 10))
        .await
        .unwrap();
    assert_eq!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_source.as_deref()),
        Some("ivf")
    );
}

#[tokio::test]
async fn model_change_invalidates_then_reconcile_rebuilds_a_new_generation() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_MODEL";
    use_env(env, "test-key");
    let harness = harness("ivf model change");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding.clone()).await;

    for index in 0..8usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 8).await;
    let first = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    let first_generation = first.generation.unwrap();

    let changed = embedding.with_model("embed-ivf-v2");
    set_policy(&harness.engine, &harness.project_id, changed.clone()).await;
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(!status.index.ready);
    assert_eq!(
        status.index.fallback_reason.as_deref(),
        Some("model_mismatch")
    );
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 1", 10))
        .await
        .unwrap();
    let summary = result.semantic.expect("semantic summary");
    assert_eq!(summary.index_source.as_deref(), Some("scan"));
    assert_eq!(summary.index_fallback.as_deref(), Some("model_mismatch"));

    reconcile_until_indexed(&harness.engine, &harness.project_id, 8).await;
    // Model-scoped jobs are re-queued; the maintenance pass sees the metadata
    // mismatch and creates a new generation.
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(status.index.ready, "{:?}", status.index);
    assert_eq!(status.index.model.as_deref(), Some("embed-ivf-v2"));
    let second_generation = status.index.generation.unwrap();
    assert!(second_generation > first_generation);
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 1", 10))
        .await
        .unwrap();
    assert_eq!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_source.as_deref()),
        Some("ivf")
    );
}

#[tokio::test]
async fn delete_and_prune_update_assignment_coverage_and_minimum_state() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_DELETE";
    use_env(env, "test-key");
    let harness = harness("ivf delete");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 6;
    set_policy(&harness.engine, &harness.project_id, embedding).await;

    let mut memories = Vec::new();
    for index in 0..12usize {
        memories.push(
            create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await,
        );
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 12).await;
    let first = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(first.rebuilt);
    let generation = first.generation.unwrap();

    let deleted = harness.engine.delete_memory(&memories[0].id).await.unwrap();
    assert!(deleted.removed);
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.vector_count, 11);
    assert_eq!(status.index.assigned_vectors, 11);
    assert_eq!(status.index.generation, Some(generation));
    assert!(status.index.ready);

    let pruned = harness
        .engine
        .prune_memories(&harness.project_id, None, Some(5), false)
        .await
        .unwrap();
    assert_eq!(pruned.removed, 6);
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.vector_count, 5);
    assert_eq!(status.index.assigned_vectors, 0);
    assert_eq!(
        status.index.fallback_reason.as_deref(),
        Some("below_min_vectors")
    );
    assert!(status.index.generation.is_none());
}

#[tokio::test]
async fn growth_crossing_the_rebuild_threshold_starts_a_new_generation() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_THRESHOLD";
    use_env(env, "test-key");
    let harness = harness("ivf rebuild threshold");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    embedding.ivf_rebuild_vectors = 8;
    set_policy(&harness.engine, &harness.project_id, embedding).await;

    for index in 0..8usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 8).await;
    let first = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(first.rebuilt);
    let first_generation = first.generation.unwrap();

    // Growing by less than one threshold keeps the centroid set.
    for index in 8..15usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 15).await;
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.generation, Some(first_generation));
    assert_eq!(status.index.vector_count, 15);
    assert_eq!(status.index.assigned_vectors, 15);

    // Crossing the growth threshold rebuilds with the same policy.
    create_cluster_memory(&harness.engine, &harness.session_id, 3, 15).await;
    reconcile_until_indexed(&harness.engine, &harness.project_id, 16).await;
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(status.index.generation.unwrap() > first_generation);
    assert_eq!(status.index.vector_count, 16);
    assert_eq!(status.index.assigned_vectors, 16);
    assert_eq!(status.index.cluster_count, 4);
}

#[tokio::test]
async fn memory_brief_uses_the_ivf_semantic_lane() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_BRIEF";
    use_env(env, "test-key");
    let harness = harness("semantic probe 2");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding).await;

    for index in 0..12usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 12).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(rebuild.rebuilt);
    let brief = harness
        .engine
        .memory_brief(&harness.session_id, None)
        .await
        .unwrap();
    assert!(!brief.text.is_empty());
    assert!(!brief.memories.is_empty());
    assert!(
        brief
            .memories
            .iter()
            .all(|matched| matched.memory.title.starts_with("cluster 2"))
    );
    assert!(
        brief
            .memories
            .iter()
            .all(|matched| matched.semantic_score.is_some())
    );
    let encoded = serde_json::to_string(&brief).unwrap();
    assert!(!encoded.contains("VPIV"));
}

#[tokio::test]
async fn disabled_ivf_policy_does_no_index_work_and_preserves_existing_state() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_DISABLED";
    use_env(env, "test-key");
    let harness = harness("ivf disabled");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding.clone()).await;
    for index in 0..8usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 8).await;
    let rebuilt = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(rebuilt.rebuilt);
    let generation = rebuilt.generation.unwrap();

    let mut disabled = embedding;
    disabled.enabled = false;
    set_policy(&harness.engine, &harness.project_id, disabled).await;
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(!status.index.ready);
    assert_eq!(
        status.index.fallback_reason.as_deref(),
        Some("embedding_disabled")
    );
    assert_eq!(status.index.generation, Some(generation));

    // Explicit rebuild is a no-op while disabled: it neither calls the
    // endpoint nor deletes existing derived state.
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(!rebuild.rebuilt);
    assert_eq!(rebuild.reason.as_deref(), Some("embedding_disabled"));
    let row = harness
        .store
        .memory_embedding_index_row(&harness.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, generation);
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 1", 5))
        .await
        .unwrap();
    assert_eq!(
        result.semantic.as_ref().map(|summary| summary.enabled),
        Some(false)
    );
}

#[tokio::test]
async fn assignment_audit_repairs_out_of_range_rows_and_restores_coverage() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_AUDIT";
    use_env(env, "test-key");
    let harness = harness("ivf audit");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding.clone()).await;

    let mut memories = Vec::new();
    for index in 0..12usize {
        memories.push(
            create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await,
        );
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 12).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(rebuild.rebuilt);
    let generation = rebuild.generation.unwrap();

    // Overwrite one valid assignment with an out-of-range cluster. Recall can
    // never probe it, so coverage would silently lose that vector without an
    // audit.
    harness
        .store
        .save_memory_embedding_assignments(
            &harness.project_id,
            generation,
            &[(memories[0].id.clone(), 99u32)],
        )
        .unwrap();
    let broken = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(broken.index.invalid_assignments, 1);
    assert_eq!(broken.index.assigned_vectors, 11);
    assert_eq!(broken.index.vector_count, 12);

    let audit = audit_index(&harness.store, &harness.project_id, &embedding).unwrap();
    assert_eq!(audit.total, 12);
    assert_eq!(audit.invalid, 1);
    assert_eq!(audit.repaired, 1);
    assert_eq!(audit.assigned, 11);
    let maintained = maintain_index(&harness.store, &harness.project_id, &embedding).unwrap();
    assert_eq!(maintained.generation, Some(generation));
    assert_eq!(maintained.assigned_vectors, 1);
    assert_eq!(maintained.repaired_assignments, 0);

    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(status.index.ready);
    assert_eq!(status.index.assigned_vectors, 12);
    assert_eq!(status.index.invalid_assignments, 0);
    assert_eq!(status.index.coverage, 1.0);
}

#[test]
fn concurrent_rebuild_and_writes_leave_every_vector_assigned() {
    let harness = harness("ivf concurrency");
    // A real embedding endpoint is unnecessary: this test writes validated
    // vectors directly through the same store path the reconciler uses.
    let embedding = MemoryEmbeddingPolicy {
        enabled: true,
        endpoint: Some("http://127.0.0.1:9/v1".to_string()),
        model: Some(MODEL.to_string()),
        dimensions: 8,
        batch_size: 64,
        timeout_secs: 5,
        max_candidates: 32,
        min_similarity: 0.2,
        api_key_env: None,
        index: MemoryIndexMode::Ivf,
        ivf_nlist: 8,
        ivf_nprobe: 2,
        ivf_min_vectors: 4,
        ivf_rebuild_vectors: 4096,
    };
    let store = harness.store.clone();
    let session = harness.session_id.clone();
    let project = harness.project_id.clone();
    for index in 0..60usize {
        let memory = store
            .create_memory_with_event(
                &session,
                &project,
                "agent-1",
                "Leader",
                "fact",
                &format!("cluster {} item {index}", index % 8),
                "seed",
                &[],
            )
            .unwrap()
            .0;
        store
            .mark_memory_embedding_attempted(&memory.id, MODEL, false)
            .unwrap();
        let values = vector_for(&memory.title)
            .iter()
            .map(|value| *value as f32)
            .collect::<Vec<_>>();
        let bytes = encode_embedding_vector(&values).unwrap();
        store
            .record_memory_embedding_success(&memory.id, MODEL, 8, &format!("hash-{index}"), &bytes)
            .unwrap();
    }
    let built = rebuild_index(&store, &project, &embedding).unwrap();
    assert!(built.rebuilt);
    assert_eq!(built.vector_count, 60);

    let rebuild_store = store.clone();
    let rebuild_project = project.clone();
    let rebuild_policy = embedding.clone();
    let rebuild_thread = std::thread::spawn(move || {
        rebuild_index(&rebuild_store, &rebuild_project, &rebuild_policy)
    });
    let writer_store = store.clone();
    let writer_session = session.clone();
    let writer_project = project.clone();
    let writer_thread = std::thread::spawn(move || {
        for index in 0..20usize {
            let memory = writer_store
                .create_memory_with_event(
                    &writer_session,
                    &writer_project,
                    "agent-1",
                    "Leader",
                    "fact",
                    &format!("cluster {} concurrent {index}", index % 8),
                    "written during rebuild",
                    &[],
                )
                .unwrap()
                .0;
            writer_store
                .mark_memory_embedding_attempted(&memory.id, MODEL, false)
                .unwrap();
            let values = vector_for(&memory.title)
                .iter()
                .map(|value| *value as f32)
                .collect::<Vec<_>>();
            let bytes = encode_embedding_vector(&values).unwrap();
            writer_store
                .record_memory_embedding_success(
                    &memory.id,
                    MODEL,
                    8,
                    &format!("hash-concurrent-{index}"),
                    &bytes,
                )
                .unwrap();
        }
    });
    rebuild_thread.join().unwrap().unwrap();
    writer_thread.join().unwrap();

    // Whichever generation the rebuild committed, a later bounded maintenance
    // pass must assign every vector, including writes that landed after the
    // sample. No vector is lost and the stored generation never mixes.
    let maintained = maintain_index(&store, &project, &embedding).unwrap();
    assert!(maintained.generation.is_some());
    assert_eq!(maintained.vector_count, 80);
    let generation = maintained.generation.unwrap();
    let counts = store
        .memory_embedding_index_counts(&project, MODEL, 8, generation, {
            store
                .memory_embedding_index_row(&project)
                .unwrap()
                .unwrap()
                .cluster_count
        })
        .unwrap();
    assert_eq!(counts.vector_count, 80);
    assert_eq!(counts.assigned_vectors, 80);
    assert!(
        store
            .memory_embedding_unassigned_vectors(&project, MODEL, 8, generation, 1_000)
            .unwrap()
            .is_empty()
    );
    let audit = audit_index(&store, &project, &embedding).unwrap();
    assert_eq!(audit.invalid, 0);
    let row = store.memory_embedding_index_row(&project).unwrap().unwrap();
    assert_eq!(row.generation, generation);
    assert!(row.cluster_count > 0);
}

#[tokio::test]
async fn nlist_change_invalidates_then_maintenance_rebuilds() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_NLIST";
    use_env(env, "test-key");
    let harness = harness("ivf nlist change");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding.clone()).await;

    for index in 0..12usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 12).await;
    let first = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(first.rebuilt);
    assert_eq!(first.cluster_count, 4);
    let first_generation = first.generation.unwrap();

    let mut wider = embedding;
    wider.ivf_nlist = 8;
    wider.ivf_nprobe = 3;
    set_policy(&harness.engine, &harness.project_id, wider).await;
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(
        status.index.fallback_reason.as_deref(),
        Some("nlist_mismatch")
    );
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 2", 5))
        .await
        .unwrap();
    assert_eq!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_fallback.as_deref()),
        Some("nlist_mismatch")
    );

    // No new embedding text is needed: maintenance only changes centroids.
    harness.engine.reconcile_memory_embeddings().await.unwrap();
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(status.index.ready, "{:?}", status.index);
    assert_eq!(status.index.nlist, 8);
    assert_eq!(status.index.nprobe, 3);
    assert_eq!(status.index.cluster_count, 8);
    let second_generation = status.index.generation.unwrap();
    assert!(second_generation > first_generation);
    assert_eq!(status.index.assigned_vectors, 12);
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 2", 5))
        .await
        .unwrap();
    assert_eq!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_source.as_deref()),
        Some("ivf")
    );
}

#[tokio::test]
async fn stale_memories_require_include_stale_in_the_ivf_lane() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_STALE";
    use_env(env, "test-key");
    let harness = harness("ivf stale");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding).await;

    let target = create_cluster_memory(&harness.engine, &harness.session_id, 2, 2).await;
    for index in 0..8usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, 100 + index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 9).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(rebuild.rebuilt);
    harness
        .engine
        .memory_update(&target.id, Some("stale"), None, None)
        .await
        .unwrap();

    let active_only = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 2", 10))
        .await
        .unwrap();
    assert_eq!(
        active_only
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_source.as_deref()),
        Some("ivf")
    );
    assert!(
        active_only
            .memories
            .iter()
            .all(|matched| matched.memory.id != target.id)
    );
    let mut include_stale = search_params(&harness.project_id, "semantic probe 2", 10);
    include_stale.include_stale = true;
    let with_stale = harness.engine.memory_search(include_stale).await.unwrap();
    assert!(
        with_stale
            .memories
            .iter()
            .any(|matched| matched.memory.id == target.id),
        "include_stale must let the IVF candidate participate"
    );
}

#[tokio::test]
async fn archived_memory_removes_its_assignment_and_candidate() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_ARCHIVE";
    use_env(env, "test-key");
    let harness = harness("ivf archive");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding).await;

    let target = create_cluster_memory(&harness.engine, &harness.session_id, 1, 1).await;
    for index in 0..11usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, 10 + index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 12).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(rebuild.rebuilt);
    let generation = rebuild.generation.unwrap();

    harness
        .engine
        .memory_update(&target.id, Some("archived"), None, None)
        .await
        .unwrap();
    let counts = harness
        .store
        .memory_embedding_index_counts(&harness.project_id, MODEL, 8, generation, 4)
        .unwrap();
    assert_eq!(counts.vector_count, 11);
    assert_eq!(counts.assigned_vectors, 11);
    let audit = harness
        .store
        .memory_embedding_assignment_audit(&harness.project_id, MODEL, 8, generation, 4)
        .unwrap();
    assert_eq!(audit.invalid(), 0);
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 1", 10))
        .await
        .unwrap();
    assert_eq!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_source.as_deref()),
        Some("ivf")
    );
    assert!(
        result
            .memories
            .iter()
            .all(|matched| matched.memory.id != target.id)
    );
}

#[tokio::test]
async fn lexical_only_requests_skip_index_reads_even_when_the_index_is_corrupt() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_LEXICAL_ONLY";
    use_env(env, "test-key");
    let harness = harness("ivf lexical only");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding).await;

    let target = create_cluster_memory(&harness.engine, &harness.session_id, 2, 2).await;
    for index in 0..7usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, 20 + index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 8).await;
    harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    harness
        .store
        .save_memory_embedding_index(
            &harness.project_id,
            MODEL,
            8,
            0,
            4,
            4,
            8,
            b"lexical-only-corrupt",
        )
        .unwrap();

    let mut params = search_params(&harness.project_id, &target.title, 10);
    params.lexical_only = true;
    let result = harness.engine.memory_search(params).await.unwrap();
    assert!(!result.memories.is_empty());
    let summary = result.semantic.expect("lexical-only summary");
    assert!(!summary.enabled);
    assert!(summary.index_source.is_none());
    assert!(summary.index_fallback.is_none());
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(
        status.index.fallback_count, 0,
        "lexical-only requests must not touch ANN fallback accounting"
    );
}

#[tokio::test]
async fn projects_have_independent_indexes_and_generations() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_PROJECTS";
    use_env(env, "test-key");
    let harness = harness("ivf project one");
    let second_repo = TempDir::new().unwrap();
    let second = harness
        .store
        .add_project(second_repo.path().to_str().unwrap(), Some("ivf-two"))
        .unwrap();
    let second_session = harness
        .store
        .create_session(&second.id, "ivf project two")
        .unwrap();
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding.clone()).await;
    set_policy(&harness.engine, &second.id, embedding).await;

    for index in 0..8usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
        harness
            .engine
            .create_memory(
                &second_session.id,
                "fact",
                &format!("cluster {} item {index}", (index + 1) % 4),
                "second project",
                Vec::new(),
            )
            .await
            .unwrap();
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 8).await;
    reconcile_until_indexed(&harness.engine, &second.id, 8).await;
    let first_build = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    let second_build = harness
        .engine
        .memory_embedding_index_rebuild(&second.id)
        .await
        .unwrap();
    assert!(first_build.rebuilt);
    assert!(second_build.rebuilt);
    let first_generation = first_build.generation.unwrap();
    assert!(first_generation >= 1);
    assert!(second_build.generation.unwrap() >= 1);

    let first_status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    let second_status = harness
        .engine
        .memory_embedding_index_status(&second.id)
        .await
        .unwrap();
    assert_eq!(first_status.index.vector_count, 8);
    assert_eq!(second_status.index.vector_count, 8);
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 0", 10))
        .await
        .unwrap();
    assert!(
        result
            .memories
            .iter()
            .all(|matched| matched.memory.project_id == harness.project_id)
    );
    // Rebuilding one project cannot delete the other project's assignments.
    harness
        .engine
        .memory_embedding_index_rebuild(&second.id)
        .await
        .unwrap();
    let first_status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(first_status.index.assigned_vectors, 8);
    assert_eq!(first_status.index.generation, Some(first_generation));
}

#[tokio::test]
async fn absent_policy_reports_not_configured_and_rebuild_is_a_noop() {
    let harness = harness("ivf absent");
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.mode, "scan");
    assert!(!status.index.ready);
    assert_eq!(
        status.index.fallback_reason.as_deref(),
        Some("not_configured")
    );
    assert_eq!(status.index.generation, None);
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(!rebuild.rebuilt);
    assert_eq!(rebuild.reason.as_deref(), Some("not_configured"));
    assert!(
        harness
            .store
            .memory_embedding_index_row(&harness.project_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn embedding_failure_does_no_index_work_and_search_stays_lexical() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
        .mount(&server)
        .await;
    let env = "VP_TEST_IVF_DOWN";
    use_env(env, "test-key");
    let harness = harness("ivf endpoint down");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding).await;
    for index in 0..6usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    let _ = harness.engine.reconcile_memory_embeddings().await.unwrap();
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(!status.index.ready);
    assert_eq!(status.index.vector_count, 0);
    assert_eq!(status.index.assigned_vectors, 0);
    assert!(status.index.generation.is_none());
    assert!(status.index.fallback_reason.is_some());

    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "cluster 1", 10))
        .await
        .unwrap();
    let summary = result.semantic.expect("semantic summary");
    assert!(summary.enabled);
    assert!(
        summary.fallback.is_some(),
        "the query embedding call failed"
    );
    assert!(summary.index_source.is_none());
    assert!(summary.index_fallback.is_none());
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(
        status.index.fallback_count, 0,
        "no ANN probe ran, so the ANN fallback counter must stay zero"
    );
}

#[tokio::test]
async fn empty_query_never_probes_the_index() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_EMPTY_QUERY";
    use_env(env, "test-key");
    let harness = harness("ivf empty query");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding).await;
    for index in 0..6usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 6).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(rebuild.rebuilt);
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "", 10))
        .await
        .unwrap();
    let summary = result.semantic.expect("semantic summary");
    assert_eq!(summary.fallback.as_deref(), Some("query text is empty"));
    assert!(summary.index_source.is_none());
    assert!(summary.index_fallback.is_none());
    assert_eq!(summary.probed_clusters, 0);
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert_eq!(status.index.fallback_count, 0);
}

#[tokio::test]
async fn a_single_vector_project_can_opt_into_a_one_cluster_index() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_SINGLE";
    use_env(env, "test-key");
    let harness = harness("ivf single vector");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 1;
    embedding.ivf_min_vectors = 1;
    embedding.ivf_rebuild_vectors = 1;
    set_policy(&harness.engine, &harness.project_id, embedding).await;
    create_cluster_memory(&harness.engine, &harness.session_id, 3, 1).await;
    reconcile_until_indexed(&harness.engine, &harness.project_id, 1).await;
    let rebuild = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(rebuild.rebuilt, "{rebuild:?}");
    assert_eq!(rebuild.vector_count, 1);
    assert_eq!(rebuild.assigned_vectors, 1);
    assert_eq!(rebuild.cluster_count, 1);
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(status.index.ready, "{:?}", status.index);
    assert_eq!(status.index.coverage, 1.0);
    let result = harness
        .engine
        .memory_search(search_params(&harness.project_id, "semantic probe 3", 5))
        .await
        .unwrap();
    assert_eq!(
        result
            .semantic
            .as_ref()
            .and_then(|summary| summary.index_source.as_deref()),
        Some("ivf")
    );
    assert_eq!(result.memories.len(), 1);
}

#[tokio::test]
async fn index_repair_does_not_depend_on_the_embedding_endpoint() {
    let server = MockServer::start().await;
    mount_embeddings(&server).await;
    let env = "VP_TEST_IVF_REPAIR_OFFLINE";
    use_env(env, "test-key");
    let harness = harness("ivf repair offline");
    let mut embedding = embedding_policy(server.uri(), env);
    embedding.ivf_nlist = 4;
    embedding.ivf_nprobe = 2;
    embedding.ivf_min_vectors = 4;
    set_policy(&harness.engine, &harness.project_id, embedding.clone()).await;
    for index in 0..8usize {
        create_cluster_memory(&harness.engine, &harness.session_id, index % 4, index).await;
    }
    reconcile_until_indexed(&harness.engine, &harness.project_id, 8).await;
    let first = harness
        .engine
        .memory_embedding_index_rebuild(&harness.project_id)
        .await
        .unwrap();
    assert!(first.rebuilt);
    harness
        .store
        .save_memory_embedding_index(
            &harness.project_id,
            MODEL,
            8,
            0,
            4,
            4,
            8,
            b"offline-corrupt",
        )
        .unwrap();

    // Missing key blocks the embedding client, but maintenance runs before the
    // client is built and repairs the derived index from stored vectors.
    let mut offline = embedding;
    offline.api_key_env = Some("VP_TEST_IVF_MISSING_KEY_9Z".to_string());
    set_policy(&harness.engine, &harness.project_id, offline).await;
    let _ = harness.engine.reconcile_memory_embeddings().await.unwrap();
    let status = harness
        .engine
        .memory_embedding_index_status(&harness.project_id)
        .await
        .unwrap();
    assert!(status.index.ready, "{:?}", status.index);
    assert_eq!(status.index.vector_count, 8);
    assert_eq!(status.index.assigned_vectors, 8);
    assert!(status.index.generation.unwrap() > first.generation.unwrap());
}

trait PolicyModelOverride {
    fn with_model(&self, model: &str) -> MemoryEmbeddingPolicy;
}

impl PolicyModelOverride for MemoryEmbeddingPolicy {
    fn with_model(&self, model: &str) -> MemoryEmbeddingPolicy {
        let mut next = self.clone();
        next.model = Some(model.to_string());
        next
    }
}
