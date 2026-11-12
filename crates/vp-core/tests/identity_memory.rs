//! S5 identity, department, and project memory integration tests.
//!
//! These use the same Store + Engine APIs as the daemon and never call a
//! provider. Memory is durable project state: the tests cover scope isolation,
//! evidence and content hashing, TTL/supersede/forget lifecycle, the bounded
//! prompt brief, archive import, replay/repair, and `db-rebuild`.

use tempfile::TempDir;
use time::OffsetDateTime;
use vp_core::Engine;
use vp_core::identity_memory::{DEFAULT_BRIEF_BYTES, content_hash};
use vp_core::rebuild::rebuild_database;
use vp_protocol::types::{
    AgentIdentity, AgentIdentityPolicy, AgentMemory, AgentMemoryError, AgentMemoryListParams,
    AgentMemoryParams, AgentMemoryReplayParams, AgentMemorySweepResult, CreateAgentIdentityParams,
    CreateAgentMemoryParams, ForgetAgentMemoryParams, MemoryEvidence, Project,
    RetireAgentIdentityParams, Session, SessionAssignParams, SupersedeAgentMemoryParams,
    SweepAgentMemoriesParams, UpdateAgentMemoryParams, VerifyAgentMemoryParams, agent_memory_codes,
};
use vp_store::Store;

struct Harness {
    data: TempDir,
    _repo: TempDir,
    engine: Engine,
    project: Project,
}

fn harness(name: &str) -> Harness {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some(name))
        .unwrap();
    Harness {
        data,
        _repo: repo,
        engine: Engine::new(store),
        project,
    }
}

async fn enable_policy(engine: &Engine, project_id: &str) {
    engine
        .set_agent_identity_policy(
            project_id,
            AgentIdentityPolicy {
                enabled: true,
                ..AgentIdentityPolicy::default()
            },
        )
        .await
        .unwrap();
}

async fn identity(
    engine: &Engine,
    project_id: &str,
    title: &str,
    department: &str,
    component: &str,
) -> AgentIdentity {
    engine
        .create_agent_identity(CreateAgentIdentityParams {
            project_id: project_id.to_string(),
            role: "worker".to_string(),
            title: Some(title.to_string()),
            department: Some(department.to_string()),
            component: Some(component.to_string()),
            profile: Some("coding".to_string()),
            ..CreateAgentIdentityParams::default()
        })
        .await
        .unwrap()
        .identity
}

fn session(engine: &Engine, project_id: &str, goal: &str) -> Session {
    let session = engine.store().create_session(project_id, goal).unwrap();
    engine
        .store()
        .append_event(
            &session.id,
            "session.created",
            serde_json::json!({
                "project_id": project_id,
                "goal": session.goal,
                "created_at": session.created_at,
            }),
        )
        .unwrap();
    session
}

async fn assign(engine: &Engine, session_id: &str, identity_id: &str) {
    engine
        .assign_session_identity(SessionAssignParams {
            session_id: session_id.to_string(),
            identity_id: identity_id.to_string(),
            ..SessionAssignParams::default()
        })
        .await
        .unwrap();
}

fn evidence(kind: &str, reference: &str) -> MemoryEvidence {
    MemoryEvidence {
        kind: kind.to_string(),
        reference: reference.to_string(),
        note: None,
    }
}

fn create_params(scope: &str, identity_id: Option<&str>, content: &str) -> CreateAgentMemoryParams {
    CreateAgentMemoryParams {
        project_id: "unset".to_string(),
        identity_id: identity_id.map(str::to_string),
        scope: scope.to_string(),
        content: content.to_string(),
        evidence: vec![evidence("task", "T1")],
        source: Some("operator".to_string()),
        ..CreateAgentMemoryParams::default()
    }
}

fn code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<AgentMemoryError>()
        .map(|typed| typed.code)
        .unwrap_or("UNTYPED")
}

async fn remember(
    engine: &Engine,
    project: &Project,
    scope: &str,
    identity_id: Option<&str>,
    content: &str,
) -> AgentMemory {
    let mut params = create_params(scope, identity_id, content);
    params.project_id = project.id.clone();
    if scope == "department" {
        params.department = Some("Platform".to_string());
    }
    engine.remember_agent_memory(params).await.unwrap().memory
}

async fn list_as(engine: &Engine, project_id: &str, viewer: Option<&str>) -> Vec<AgentMemory> {
    engine
        .list_agent_memories(AgentMemoryListParams {
            project_id: project_id.to_string(),
            identity_id: viewer.map(str::to_string),
            ..AgentMemoryListParams::default()
        })
        .await
        .unwrap()
        .memories
}

#[tokio::test]
async fn scope_isolation_agent_department_project_and_cross_project_rejection() {
    let harness = harness("scope");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let auth = identity(engine, &project.id, "Auth Tech Lead", "Platform", "auth").await;
    let billing = identity(
        engine,
        &project.id,
        "Billing Tech Lead",
        "Billing",
        "billing",
    )
    .await;
    let session = session(engine, &project.id, "scope isolation");
    assign(engine, &session.id, &auth.id).await;
    assign(engine, &session.id, &billing.id).await;

    let auth_memory = remember(
        engine,
        project,
        "agent",
        Some(&auth.id),
        "Auth owns JWT validation and key rotation.",
    )
    .await;
    assert_eq!(auth_memory.identity_id.as_deref(), Some(auth.id.as_str()));
    assert_eq!(auth_memory.department.as_deref(), Some("Platform"));
    assert_eq!(auth_memory.component.as_deref(), Some("auth"));
    let platform_memory = remember(
        engine,
        project,
        "department",
        None,
        "Platform interfaces use typed error codes.",
    )
    .await;
    assert!(platform_memory.identity_id.is_none());
    assert_eq!(platform_memory.department.as_deref(), Some("Platform"));
    let billing_memory = {
        let mut params = create_params("department", None, "Billing owns tax evidence.");
        params.project_id = project.id.clone();
        params.department = Some("Billing".to_string());
        engine.remember_agent_memory(params).await.unwrap().memory
    };
    let project_memory = remember(
        engine,
        project,
        "project",
        None,
        "The project uses deterministic merge integration.",
    )
    .await;
    assert!(project_memory.identity_id.is_none());
    assert!(project_memory.department.is_none());

    let auth_view = list_as(engine, &project.id, Some(&auth.id)).await;
    let auth_ids = auth_view
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    assert!(auth_ids.contains(&auth_memory.id.as_str()));
    assert!(auth_ids.contains(&platform_memory.id.as_str()));
    assert!(auth_ids.contains(&project_memory.id.as_str()));
    assert!(!auth_ids.contains(&billing_memory.id.as_str()));

    let billing_view = list_as(engine, &project.id, Some(&billing.id)).await;
    let billing_ids = billing_view
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    assert!(!billing_ids.contains(&auth_memory.id.as_str()));
    assert!(!billing_ids.contains(&platform_memory.id.as_str()));
    assert!(billing_ids.contains(&billing_memory.id.as_str()));
    assert!(billing_ids.contains(&project_memory.id.as_str()));

    let operator = list_as(engine, &project.id, None).await;
    assert_eq!(operator.len(), 4);

    let forbidden = engine
        .get_agent_memory(AgentMemoryParams {
            memory_id: auth_memory.id.clone(),
            project_id: None,
            identity_id: Some(billing.id.clone()),
        })
        .await
        .unwrap_err();
    assert_eq!(code(&forbidden), agent_memory_codes::MEMORY_SCOPE_FORBIDDEN);
    let wrong_project = engine
        .get_agent_memory(AgentMemoryParams {
            memory_id: auth_memory.id.clone(),
            project_id: Some("prj-other".to_string()),
            identity_id: None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        code(&wrong_project),
        agent_memory_codes::MEMORY_PROJECT_MISMATCH
    );

    // Another project cannot even use an identity as a viewer, and cannot
    // attach an identity from another project to a write.
    let other_repo = TempDir::new().unwrap();
    let other_project = engine
        .store()
        .add_project(other_repo.path().to_str().unwrap(), Some("other"))
        .unwrap();
    enable_policy(engine, &other_project.id).await;
    let other_worker = identity(engine, &other_project.id, "Other Lead", "Platform", "other").await;
    let cross_view = engine
        .list_agent_memories(AgentMemoryListParams {
            project_id: project.id.clone(),
            identity_id: Some(other_worker.id.clone()),
            ..AgentMemoryListParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        code(&cross_view),
        agent_memory_codes::MEMORY_PROJECT_MISMATCH
    );
    let mut cross_write = create_params("agent", Some(&other_worker.id), "cross project");
    cross_write.project_id = project.id.clone();
    let cross_write = engine.remember_agent_memory(cross_write).await.unwrap_err();
    assert_eq!(
        code(&cross_write),
        agent_memory_codes::MEMORY_PROJECT_MISMATCH
    );

    // Scope shape failures are typed, not silently coerced.
    let mut no_identity = create_params("agent", None, "no identity");
    no_identity.project_id = project.id.clone();
    let no_identity = engine.remember_agent_memory(no_identity).await.unwrap_err();
    assert_eq!(
        code(&no_identity),
        agent_memory_codes::MEMORY_IDENTITY_REQUIRED
    );
    let no_department = {
        let mut params = create_params("department", None, "no department");
        params.project_id = project.id.clone();
        params.department = None;
        engine.remember_agent_memory(params).await.unwrap_err()
    };
    assert_eq!(
        code(&no_department),
        agent_memory_codes::MEMORY_DEPARTMENT_REQUIRED
    );
}

#[tokio::test]
async fn evidence_content_hash_ttl_and_optimistic_update_are_bounded() {
    let harness = harness("evidence");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "Auth Lead", "Platform", "auth").await;

    let mut params = create_params("agent", Some(&worker.id), "  padded claim  ");
    params.project_id = project.id.clone();
    params.evidence = vec![
        evidence("task", "T1"),
        MemoryEvidence {
            kind: "commit".to_string(),
            reference: "abcdef1234567890".to_string(),
            note: Some("reviewed".to_string()),
        },
    ];
    let memory = engine
        .remember_agent_memory(params.clone())
        .await
        .unwrap()
        .memory;
    assert_eq!(memory.content, "padded claim");
    assert_eq!(memory.content_hash, content_hash("padded claim"));
    assert_eq!(memory.status, "active");
    assert_eq!(memory.evidence.len(), 2);

    params.content = "x".repeat(vp_protocol::types::AGENT_MEMORY_MAX_CONTENT_CHARS + 1);
    assert_eq!(
        code(
            &engine
                .remember_agent_memory(params.clone())
                .await
                .unwrap_err()
        ),
        agent_memory_codes::MEMORY_TOO_LARGE
    );
    params.content = "bounded".to_string();
    params.evidence = vec![evidence("prompt", "leak")];
    assert_eq!(
        code(
            &engine
                .remember_agent_memory(params.clone())
                .await
                .unwrap_err()
        ),
        agent_memory_codes::MEMORY_EVIDENCE_INVALID
    );
    params.evidence = Vec::new();
    params.source = Some("verifier".to_string());
    assert_eq!(
        code(
            &engine
                .remember_agent_memory(params.clone())
                .await
                .unwrap_err()
        ),
        agent_memory_codes::MEMORY_EVIDENCE_INVALID
    );
    params.source = Some("operator".to_string());
    params.expires_at = Some("2020-01-01T00:00:00Z".to_string());
    assert_eq!(
        code(
            &engine
                .remember_agent_memory(params.clone())
                .await
                .unwrap_err()
        ),
        agent_memory_codes::MEMORY_INVALID
    );
    params.expires_at = Some("2040-01-01T00:00:00Z".to_string());
    assert_eq!(
        code(&engine.remember_agent_memory(params).await.unwrap_err()),
        agent_memory_codes::MEMORY_INVALID
    );

    let conflict = engine
        .update_agent_memory(UpdateAgentMemoryParams {
            memory_id: memory.id.clone(),
            content: Some("changed".to_string()),
            expected_content_hash: Some("deadbeef".to_string()),
            ..UpdateAgentMemoryParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&conflict), agent_memory_codes::MEMORY_CONFLICT);
    let verified = engine
        .verify_agent_memory(VerifyAgentMemoryParams {
            memory_id: memory.id.clone(),
            extend_days: Some(7),
        })
        .await
        .unwrap()
        .memory;
    assert!(verified.verified_at.is_some());
    assert!(verified.expires_at.is_some());
    let not_shortened = engine
        .verify_agent_memory(VerifyAgentMemoryParams {
            memory_id: memory.id.clone(),
            extend_days: Some(1),
        })
        .await
        .unwrap()
        .memory;
    assert_eq!(
        not_shortened.expires_at, verified.expires_at,
        "verification extends but never shortens TTL"
    );
    let updated = engine
        .update_agent_memory(UpdateAgentMemoryParams {
            memory_id: memory.id.clone(),
            content: Some("changed".to_string()),
            expected_content_hash: Some(memory.content_hash.clone()),
            ..UpdateAgentMemoryParams::default()
        })
        .await
        .unwrap()
        .memory;
    assert_eq!(updated.content, "changed");
    assert!(
        updated.verified_at.is_none(),
        "content change invalidates verification"
    );
    assert_ne!(updated.content_hash, memory.content_hash);

    let unchanged = engine
        .update_agent_memory(UpdateAgentMemoryParams {
            memory_id: memory.id.clone(),
            ..UpdateAgentMemoryParams::default()
        })
        .await
        .unwrap();
    assert!(!unchanged.changed);
}

#[tokio::test]
async fn supersede_forget_prune_and_retired_identity_read_only() {
    let harness = harness("lifecycle");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "Auth Lead", "Platform", "auth").await;
    let original = remember(
        engine,
        project,
        "agent",
        Some(&worker.id),
        "Auth uses opaque session tokens.",
    )
    .await;

    let superseded = engine
        .supersede_agent_memory(SupersedeAgentMemoryParams {
            memory_id: original.id.clone(),
            content: Some("Auth uses rotating refresh tokens.".to_string()),
            source: Some("operator".to_string()),
            ..SupersedeAgentMemoryParams::default()
        })
        .await
        .unwrap();
    assert_eq!(superseded.previous.status, "superseded");
    assert_eq!(
        superseded.previous.superseded_by.as_deref(),
        Some(superseded.replacement.id.as_str())
    );
    assert_eq!(superseded.replacement.status, "active");
    assert_eq!(superseded.replacement.scope, "agent");
    assert_eq!(
        superseded.replacement.identity_id.as_deref(),
        Some(worker.id.as_str())
    );
    let edit_superseded = engine
        .update_agent_memory(UpdateAgentMemoryParams {
            memory_id: original.id.clone(),
            content: Some("late edit".to_string()),
            ..UpdateAgentMemoryParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&edit_superseded), agent_memory_codes::MEMORY_CONFLICT);

    // Retirement never deletes memory; content edits fail closed.
    engine
        .retire_agent_identity(RetireAgentIdentityParams {
            identity_id: worker.id.clone(),
            successor_id: None,
        })
        .await
        .unwrap();
    let retired_edit = engine
        .update_agent_memory(UpdateAgentMemoryParams {
            memory_id: superseded.replacement.id.clone(),
            content: Some("after retirement".to_string()),
            ..UpdateAgentMemoryParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        code(&retired_edit),
        agent_memory_codes::MEMORY_IDENTITY_RETIRED
    );
    let still_visible = engine
        .get_agent_memory(AgentMemoryParams {
            memory_id: superseded.replacement.id.clone(),
            project_id: None,
            identity_id: None,
        })
        .await
        .unwrap();
    assert!(still_visible.memory.status == "active");
    let retired_supersede = engine
        .supersede_agent_memory(SupersedeAgentMemoryParams {
            memory_id: superseded.replacement.id.clone(),
            content: Some("retired replacement note".to_string()),
            source: Some("operator".to_string()),
            ..SupersedeAgentMemoryParams::default()
        })
        .await
        .unwrap();
    assert_eq!(retired_supersede.previous.status, "superseded");

    // Soft forget keeps evidence; prune only after active state ends.
    let active_prune = engine
        .forget_agent_memory(ForgetAgentMemoryParams {
            memory_id: retired_supersede.replacement.id.clone(),
            prune: true,
        })
        .await
        .unwrap_err();
    assert_eq!(code(&active_prune), agent_memory_codes::MEMORY_CONFLICT);
    let soft = engine
        .forget_agent_memory(ForgetAgentMemoryParams {
            memory_id: retired_supersede.replacement.id.clone(),
            prune: false,
        })
        .await
        .unwrap();
    assert!(soft.changed);
    assert!(!soft.pruned);
    let stored = engine
        .get_agent_memory(AgentMemoryParams {
            memory_id: retired_supersede.replacement.id.clone(),
            project_id: None,
            identity_id: None,
        })
        .await
        .unwrap()
        .memory;
    assert_eq!(stored.status, "stale");
    assert_eq!(stored.stale_reason.as_deref(), Some("forgotten"));
    assert_eq!(stored.evidence.len(), 1, "soft forget keeps evidence");
    let again = engine
        .forget_agent_memory(ForgetAgentMemoryParams {
            memory_id: retired_supersede.replacement.id.clone(),
            prune: false,
        })
        .await
        .unwrap();
    assert!(!again.changed);
    let pruned = engine
        .forget_agent_memory(ForgetAgentMemoryParams {
            memory_id: retired_supersede.replacement.id.clone(),
            prune: true,
        })
        .await
        .unwrap();
    assert!(pruned.pruned);
    assert!(
        engine
            .get_agent_memory(AgentMemoryParams {
                memory_id: retired_supersede.replacement.id,
                project_id: None,
                identity_id: None,
            })
            .await
            .is_err()
    );

    // Imported rows are read-only evidence.
    let imported_content = "Imported evidence from another archive.";
    let imported = AgentMemory {
        id: "mem-imported-1".to_string(),
        project_id: project.id.clone(),
        scope: "project".to_string(),
        content: imported_content.to_string(),
        content_hash: content_hash(imported_content),
        source: "operator".to_string(),
        status: "active".to_string(),
        created_at: "2026-11-12T00:00:00Z".to_string(),
        updated_at: "2026-11-12T00:00:00Z".to_string(),
        origin: "imported".to_string(),
        ..AgentMemory::default()
    };
    engine
        .store()
        .insert_agent_memory_snapshot(&imported)
        .unwrap();
    let imported_edit = engine
        .update_agent_memory(UpdateAgentMemoryParams {
            memory_id: imported.id,
            content: Some("mutate archive".to_string()),
            ..UpdateAgentMemoryParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        code(&imported_edit),
        agent_memory_codes::MEMORY_IMPORTED_READONLY
    );
}

#[tokio::test]
async fn sweep_expires_due_rows_and_prunes_only_old_non_active_evidence() {
    let harness = harness("sweep");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let now = OffsetDateTime::now_utc();
    let past = (now - time::Duration::days(2))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let old = (now - time::Duration::days(60))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let future = (now + time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();

    let due_content = "Expired identity memory.";
    let due = AgentMemory {
        id: "mem-due-1".to_string(),
        project_id: project.id.clone(),
        scope: "project".to_string(),
        content: due_content.to_string(),
        content_hash: content_hash(due_content),
        source: "operator".to_string(),
        status: "active".to_string(),
        created_at: old.clone(),
        updated_at: old.clone(),
        expires_at: Some(past.clone()),
        ..AgentMemory::default()
    };
    engine.store().insert_agent_memory_snapshot(&due).unwrap();

    let stale_content = "Old forgotten memory.";
    let stale = AgentMemory {
        id: "mem-stale-1".to_string(),
        project_id: project.id.clone(),
        scope: "project".to_string(),
        content: stale_content.to_string(),
        content_hash: content_hash(stale_content),
        source: "operator".to_string(),
        status: "stale".to_string(),
        stale_reason: Some("forgotten".to_string()),
        created_at: old.clone(),
        updated_at: old.clone(),
        ..AgentMemory::default()
    };
    engine.store().insert_agent_memory_snapshot(&stale).unwrap();

    let live_content = "Live future memory.";
    let live = AgentMemory {
        id: "mem-live-1".to_string(),
        project_id: project.id.clone(),
        scope: "project".to_string(),
        content: live_content.to_string(),
        content_hash: content_hash(live_content),
        source: "operator".to_string(),
        status: "active".to_string(),
        created_at: now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        updated_at: now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        expires_at: Some(future),
        ..AgentMemory::default()
    };
    engine.store().insert_agent_memory_snapshot(&live).unwrap();

    let sweep = engine
        .sweep_agent_memories(SweepAgentMemoriesParams {
            project_id: project.id.clone(),
            older_than_days: Some(30),
            limit: Some(100),
            prune: true,
        })
        .await
        .unwrap();
    assert_eq!(
        sweep,
        AgentMemorySweepResult {
            expired: 1,
            pruned: 1,
            remaining: 2,
        }
    );
    let due = engine
        .get_agent_memory(AgentMemoryParams {
            memory_id: due.id,
            project_id: None,
            identity_id: None,
        })
        .await
        .unwrap()
        .memory;
    assert_eq!(due.status, "stale");
    assert_eq!(due.stale_reason.as_deref(), Some("expired"));
    assert!(
        engine
            .get_agent_memory(AgentMemoryParams {
                memory_id: stale.id,
                project_id: None,
                identity_id: None,
            })
            .await
            .is_err()
    );
    assert!(
        engine
            .get_agent_memory(AgentMemoryParams {
                memory_id: live.id,
                project_id: None,
                identity_id: None,
            })
            .await
            .is_ok()
    );

    let bounded = engine
        .sweep_agent_memories(SweepAgentMemoriesParams {
            project_id: project.id.clone(),
            older_than_days: Some(0),
            ..SweepAgentMemoriesParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&bounded), agent_memory_codes::MEMORY_INVALID);
}

#[tokio::test]
async fn bounded_brief_is_deterministic_scoped_and_never_unbounded() {
    let harness = harness("brief");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let auth = identity(engine, &project.id, "Auth Tech Lead", "Platform", "auth").await;
    let billing = identity(
        engine,
        &project.id,
        "Billing Tech Lead",
        "Billing",
        "billing",
    )
    .await;
    let session = session(engine, &project.id, "brief goal");
    assign(engine, &session.id, &auth.id).await;
    assign(engine, &session.id, &billing.id).await;

    let empty = engine
        .identity_memory_brief(&session.id, &auth)
        .await
        .unwrap();
    assert!(empty.is_empty());
    assert!(empty.telemetry().is_none());

    for index in 0..10 {
        remember(
            engine,
            project,
            "agent",
            Some(&auth.id),
            &format!("Agent memory {index}: {}", "a".repeat(350)),
        )
        .await;
    }
    let mut pinned = create_params(
        "department",
        None,
        &format!("Platform department memory: {}", "d".repeat(500)),
    );
    pinned.project_id = project.id.clone();
    pinned.department = Some("Platform".to_string());
    pinned.pinned = true;
    engine.remember_agent_memory(pinned).await.unwrap();
    for index in 0..8 {
        remember(
            engine,
            project,
            "project",
            None,
            &format!("Project memory {index}: {}", "p".repeat(500)),
        )
        .await;
    }
    // Billing memory must never cross into the auth brief.
    let mut billing_memory = create_params(
        "department",
        None,
        "Billing-only department memory that must not leak.",
    );
    billing_memory.project_id = project.id.clone();
    billing_memory.department = Some("Billing".to_string());
    engine.remember_agent_memory(billing_memory).await.unwrap();

    let first = engine
        .identity_memory_brief(&session.id, &auth)
        .await
        .unwrap();
    let second = engine
        .identity_memory_brief(&session.id, &auth)
        .await
        .unwrap();
    assert_eq!(first, second, "brief must be deterministic");
    assert!(!first.is_empty());
    assert!(first.brief_bytes <= DEFAULT_BRIEF_BYTES);
    assert!(first.brief_chars <= 5_500);
    assert!(
        first.memory_count < 19,
        "the brief must not include every candidate"
    );
    assert!(first.text.lines().count() == first.memory_count as usize);
    assert!(first.text.starts_with("[agent·"), "{}", first.text);
    assert!(!first.text.contains("Billing-only"));
    assert!(
        first.scope_counts.get("agent").copied().unwrap_or(0) > 0,
        "{:?}",
        first.scope_counts
    );
    assert_eq!(first.brief_hash, content_hash(&first.text));
    let telemetry = first.telemetry().unwrap();
    assert_eq!(telemetry.memory_count, first.memory_count);
    assert!(!telemetry.brief_hash.is_empty());
    assert!(telemetry.memory_hashes.iter().all(|hash| hash.len() == 64));
    let serialized = serde_json::to_string(&telemetry).unwrap();
    assert!(!serialized.contains("Agent memory 0"));
    assert!(!serialized.contains("Platform department memory"));

    let billing_brief = engine
        .identity_memory_brief(&session.id, &billing)
        .await
        .unwrap();
    assert!(billing_brief.text.contains("Billing-only"));
    assert!(!billing_brief.text.contains("Agent memory 0"));
}

#[tokio::test]
async fn replay_repair_bundle_import_and_db_rebuild_preserve_memory() {
    let harness = harness("replay");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "Auth Lead", "Platform", "auth").await;
    let session = session(engine, &project.id, "replay memory");
    assign(engine, &session.id, &worker.id).await;
    let created = remember(
        engine,
        project,
        "agent",
        Some(&worker.id),
        "Auth owns token validation.",
    )
    .await;
    let updated = engine
        .update_agent_memory(UpdateAgentMemoryParams {
            memory_id: created.id.clone(),
            content: Some("Auth owns token validation and key rotation.".to_string()),
            ..UpdateAgentMemoryParams::default()
        })
        .await
        .unwrap()
        .memory;
    let superseded = engine
        .supersede_agent_memory(SupersedeAgentMemoryParams {
            memory_id: updated.id.clone(),
            content: Some("Auth also owns refresh token rotation.".to_string()),
            source: Some("operator".to_string()),
            ..SupersedeAgentMemoryParams::default()
        })
        .await
        .unwrap();
    let forgotten = remember(
        engine,
        project,
        "project",
        None,
        "A forgotten project note.",
    )
    .await;
    engine
        .forget_agent_memory(ForgetAgentMemoryParams {
            memory_id: forgotten.id.clone(),
            prune: false,
        })
        .await
        .unwrap();

    let replay = engine
        .replay_agent_memories(AgentMemoryReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(replay.consistent, "{replay:?}");
    assert_eq!(replay.memories, 3);
    let kinds = engine
        .store()
        .list_agent_memory_events_for_project(&project.id, None)
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect::<Vec<_>>();
    assert_eq!(replay.events, 6, "{kinds:?}");

    engine
        .store()
        .replace_agent_memory_projection(&project.id, &[])
        .unwrap();
    let drift = engine
        .replay_agent_memories(AgentMemoryReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(!drift.consistent);
    let repaired = engine
        .replay_agent_memories(AgentMemoryReplayParams {
            project_id: project.id.clone(),
            repair: true,
        })
        .await
        .unwrap();
    assert!(repaired.repaired);
    assert!(repaired.consistent, "{repaired:?}");

    let events_before = engine
        .store()
        .list_agent_memory_events_for_project(&project.id, None)
        .unwrap()
        .len();
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.agent_memories.len(), 3);
    assert_eq!(bundle.agent_memory_events.len(), events_before);
    let imported_data = TempDir::new().unwrap();
    let imported_store = Store::open(imported_data.path()).unwrap();
    imported_store.import_session_bundle(&bundle).unwrap();
    let imported = imported_store
        .list_agent_memories_for_replay(&project.id)
        .unwrap();
    assert_eq!(imported.len(), 3);
    assert!(imported.iter().all(|memory| memory.origin == "imported"));
    assert_eq!(
        imported_store
            .list_agent_memory_events_for_project(&project.id, None)
            .unwrap()
            .len(),
        events_before
    );
    let imported_engine = Engine::new(imported_store);
    let imported_update = imported_engine
        .update_agent_memory(UpdateAgentMemoryParams {
            memory_id: superseded.replacement.id.clone(),
            content: Some("archive edit".to_string()),
            ..UpdateAgentMemoryParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        code(&imported_update),
        agent_memory_codes::MEMORY_IMPORTED_READONLY
    );

    engine.record_project_manifest().await.unwrap();
    let data_path = harness.data.path().to_path_buf();
    drop(harness.engine);
    let report = rebuild_database(&data_path, false).await.unwrap();
    assert_eq!(report.projects, 1);
    assert_eq!(report.sessions, 1, "{:?}", report.skipped);
    let store = Store::open(&data_path).unwrap();
    let memories = store.list_agent_memories_for_replay(&project.id).unwrap();
    assert_eq!(memories.len(), 3);
    assert_eq!(
        memories
            .iter()
            .find(|memory| memory.id == superseded.replacement.id)
            .unwrap()
            .status,
        "active"
    );
    assert!(
        memories.iter().any(|memory| memory.status == "stale"
            && memory.stale_reason.as_deref() == Some("forgotten"))
    );
    assert_eq!(
        store
            .list_agent_memory_events_for_project(&project.id, None)
            .unwrap()
            .len(),
        events_before
    );
}

#[tokio::test]
async fn bundle_validation_rejects_bad_scope_hash_and_duplicate_event_sequences() {
    let harness = harness("bundle-validation");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "Auth Lead", "Platform", "auth").await;
    let session = session(engine, &project.id, "bundle validation");
    assign(engine, &session.id, &worker.id).await;
    remember(
        engine,
        project,
        "agent",
        Some(&worker.id),
        "A valid memory.",
    )
    .await;
    let bundle = engine.export_session(&session.id).await.unwrap();

    let mut bad_hash = bundle.clone();
    bad_hash.agent_memories[0].content_hash = "deadbeef".to_string();
    assert!(bad_hash.agent_memories[0].project_id == project.id);
    let error = Store::open(TempDir::new().unwrap().path())
        .unwrap()
        .import_session_bundle(&bad_hash)
        .unwrap_err();
    assert!(error.to_string().contains("invalid"));

    let mut bad_scope = bundle.clone();
    bad_scope.agent_memories[0].identity_id = None;
    let error = Store::open(TempDir::new().unwrap().path())
        .unwrap()
        .import_session_bundle(&bad_scope)
        .unwrap_err();
    assert!(error.to_string().contains("identity"));

    let mut duplicate = bundle.clone();
    if let Some(first) = duplicate.agent_memory_events.first().cloned() {
        duplicate.agent_memory_events.push(first);
    }
    let error = Store::open(TempDir::new().unwrap().path())
        .unwrap()
        .import_session_bundle(&duplicate)
        .unwrap_err();
    assert!(error.to_string().contains("duplicated"));

    // A clean import still succeeds after the rejected variants.
    let imported_data = TempDir::new().unwrap();
    Store::open(imported_data.path())
        .unwrap()
        .import_session_bundle(&bundle)
        .unwrap();
}

#[tokio::test]
async fn pruned_memory_bundle_imports_and_replays_without_resurrecting() {
    let harness = harness("pruned-bundle");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "Auth Lead", "Platform", "auth").await;
    let session = session(engine, &project.id, "pruned bundle");
    assign(engine, &session.id, &worker.id).await;
    let memory = remember(
        engine,
        project,
        "agent",
        Some(&worker.id),
        "A memory that will be pruned.",
    )
    .await;
    engine
        .forget_agent_memory(ForgetAgentMemoryParams {
            memory_id: memory.id.clone(),
            prune: false,
        })
        .await
        .unwrap();
    let pruned = engine
        .forget_agent_memory(ForgetAgentMemoryParams {
            memory_id: memory.id.clone(),
            prune: true,
        })
        .await
        .unwrap();
    assert!(pruned.pruned);
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert!(
        bundle.agent_memories.iter().all(|row| row.id != memory.id),
        "a pruned row is not resurrected by export"
    );
    assert!(
        bundle
            .agent_memory_events
            .iter()
            .any(|event| event.kind == "agent.memory.deleted")
    );
    let imported_data = TempDir::new().unwrap();
    let imported_store = Store::open(imported_data.path()).unwrap();
    imported_store.import_session_bundle(&bundle).unwrap();
    let imported_engine = Engine::new(imported_store);
    let replay = imported_engine
        .replay_agent_memories(AgentMemoryReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(replay.consistent, "{replay:?}");
    assert_eq!(replay.memories, 0);
    assert!(
        imported_engine
            .get_agent_memory(AgentMemoryParams {
                memory_id: memory.id,
                project_id: None,
                identity_id: None,
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn memory_list_filters_and_status_visibility_are_bounded() {
    let harness = harness("filters");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "Auth Lead", "Platform", "auth").await;
    let first = remember(
        engine,
        project,
        "agent",
        Some(&worker.id),
        "Alpha auth memory.",
    )
    .await;
    let second = remember(engine, project, "department", None, "Beta platform memory.").await;
    let third = remember(engine, project, "project", None, "Gamma project memory.").await;

    let by_scope = engine
        .list_agent_memories(AgentMemoryListParams {
            project_id: project.id.clone(),
            scope: Some("department".to_string()),
            ..AgentMemoryListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(by_scope.memories.len(), 1);
    assert_eq!(by_scope.memories[0].id, second.id);
    assert_eq!(by_scope.total, 1);

    let by_query = engine
        .list_agent_memories(AgentMemoryListParams {
            project_id: project.id.clone(),
            query: Some("Gamma".to_string()),
            ..AgentMemoryListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(by_query.memories.len(), 1);
    assert_eq!(by_query.memories[0].id, third.id);

    let by_status = engine
        .list_agent_memories(AgentMemoryListParams {
            project_id: project.id.clone(),
            status: Some("stale".to_string()),
            ..AgentMemoryListParams::default()
        })
        .await
        .unwrap();
    assert!(by_status.memories.is_empty());

    let limited = engine
        .list_agent_memories(AgentMemoryListParams {
            project_id: project.id.clone(),
            limit: Some(1),
            ..AgentMemoryListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(limited.memories.len(), 1);
    assert_eq!(limited.total, 3);
    assert!(
        [first.id.as_str(), second.id.as_str(), third.id.as_str()]
            .contains(&limited.memories[0].id.as_str())
    );

    let invalid_scope = engine
        .list_agent_memories(AgentMemoryListParams {
            project_id: project.id.clone(),
            scope: Some("session".to_string()),
            ..AgentMemoryListParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        code(&invalid_scope),
        agent_memory_codes::MEMORY_SCOPE_INVALID
    );
    let invalid_status = engine
        .list_agent_memories(AgentMemoryListParams {
            project_id: project.id.clone(),
            status: Some("deleted".to_string()),
            ..AgentMemoryListParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&invalid_status), agent_memory_codes::MEMORY_INVALID);
}
