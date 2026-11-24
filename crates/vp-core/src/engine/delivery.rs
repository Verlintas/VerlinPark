//! Explicit, event-only partial delivery.
//!
//! A partial-delivery record states what an operator is handing over: the
//! bounded readiness level, the commit the work stopped at, changed paths, and
//! the remaining work/blockers. It never merges, verifies, approves, extends a
//! budget, or changes capacity; the normal Verifier/Integrator gates still
//! apply to any commit it names.

use super::*;

use vp_protocol::types::{
    PARTIAL_DELIVERY_SCHEMA, PartialDeliveryParams, PartialDeliveryRecord, PartialDeliveryResult,
    SESSION_PARTIAL_DELIVERY_EVENT, SessionDeliveryParams, SessionDeliveryResult,
};

/// Stable typed codes for the partial-delivery operation. The daemon maps them
/// to `INVALID_PARAMS` without parsing message text.
pub mod partial_delivery_codes {
    pub const DELIVERY_CONFIRM_REQUIRED: &str = "DELIVERY_CONFIRM_REQUIRED";
    pub const DELIVERY_IMPORTED: &str = "DELIVERY_IMPORTED";
    pub const DELIVERY_SESSION_ACTIVE: &str = "DELIVERY_SESSION_ACTIVE";
    pub const DELIVERY_EMPTY: &str = "DELIVERY_EMPTY";
    pub const DELIVERY_ALREADY_COMPLETE: &str = "DELIVERY_ALREADY_COMPLETE";
}

/// Typed failure for partial delivery. It carries only a stable code and a
/// bounded message; no file content, prompt text, or secret enters it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialDeliveryError {
    pub code: &'static str,
    pub message: String,
}

impl PartialDeliveryError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PartialDeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PartialDeliveryError {}

const DEFAULT_PARTIAL_DELIVERY_SUMMARY: &str = "Partial delivery recorded by operator.";
const MAX_PARTIAL_DELIVERY_SUMMARY_CHARS: usize = 2_000;
const MAX_PARTIAL_DELIVERY_REMAINING_ITEMS: usize = 50;
const MAX_PARTIAL_DELIVERY_REMAINING_CHARS: usize = 500;
const MAX_PARTIAL_DELIVERY_RECORDED_BY_CHARS: usize = 120;

impl Engine {
    async fn partial_delivery_session(&self, session_id: &str) -> Result<Session> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))
    }

    /// Newest explicit partial-delivery record for a session. A malformed
    /// payload is treated as absent rather than crashing readiness/delivery
    /// reads; the raw event remains in the audit stream.
    pub(crate) async fn latest_partial_delivery(
        &self,
        session_id: &str,
    ) -> Result<Option<PartialDeliveryRecord>> {
        let Some(event) = self
            .latest_event_of_kind(session_id, SESSION_PARTIAL_DELIVERY_EVENT)
            .await?
        else {
            return Ok(None);
        };
        let Ok(record) = serde_json::from_value::<PartialDeliveryRecord>(event.payload) else {
            return Ok(None);
        };
        // Every record field is serde-defaulted for forward compatibility, so a
        // structurally valid but meaningless payload can still decode. Require
        // the known schema and a non-empty id before treating it as usable.
        Ok(
            (record.schema == PARTIAL_DELIVERY_SCHEMA && !record.id.trim().is_empty())
                .then_some(record),
        )
    }

    /// Record an explicit partial delivery for an inactive session. The record
    /// is appended as one `session.partial_delivery` event and never mutates the
    /// worktree, merge queue, approvals, effects, budgets, or session status.
    pub async fn record_partial_delivery(
        &self,
        params: PartialDeliveryParams,
    ) -> Result<PartialDeliveryResult> {
        if !params.confirm {
            return Err(partial_delivery_error(
                partial_delivery_codes::DELIVERY_CONFIRM_REQUIRED,
                "recording a partial delivery requires explicit confirmation (confirm=true / --yes); it does not merge or complete work",
            ));
        }

        let session = self.partial_delivery_session(&params.session_id).await?;
        if session.status == "imported" {
            return Err(partial_delivery_error(
                partial_delivery_codes::DELIVERY_IMPORTED,
                format!(
                    "session {} is an imported read-only archive; partial delivery cannot be recorded on it",
                    session.id
                ),
            ));
        }
        if !matches!(
            session.status.as_str(),
            "draft" | "idle" | "failed" | "cancelled" | "paused" | "completed"
        ) {
            return Err(partial_delivery_error(
                partial_delivery_codes::DELIVERY_SESSION_ACTIVE,
                format!(
                    "session {} is {}; stop or pause it before recording a partial delivery",
                    session.id, session.status
                ),
            ));
        }

        let readiness = self.session_readiness(&session.id).await?;
        if readiness.level == "none" {
            return Err(partial_delivery_error(
                partial_delivery_codes::DELIVERY_EMPTY,
                format!(
                    "session {} readiness is none: there is no commit, dirty path, task, verification, or blocker to deliver",
                    session.id
                ),
            ));
        }

        let summary = params
            .summary
            .as_deref()
            .map(|value| bounded_partial_delivery_text(value, MAX_PARTIAL_DELIVERY_SUMMARY_CHARS))
            .unwrap_or_default();
        let remaining_work = params
            .remaining_work
            .iter()
            .filter_map(|item| {
                let item =
                    bounded_partial_delivery_text(item, MAX_PARTIAL_DELIVERY_REMAINING_CHARS);
                (!item.is_empty()).then_some(item)
            })
            .take(MAX_PARTIAL_DELIVERY_REMAINING_ITEMS)
            .collect::<Vec<_>>();

        if readiness.level == "merged" && remaining_work.is_empty() && readiness.blockers.is_empty()
        {
            return Err(partial_delivery_error(
                partial_delivery_codes::DELIVERY_ALREADY_COMPLETE,
                format!(
                    "session {} is already merged with no remaining work or blockers; a partial delivery would misstate completion",
                    session.id
                ),
            ));
        }
        if summary.is_empty() && remaining_work.is_empty() {
            return Err(partial_delivery_error(
                partial_delivery_codes::DELIVERY_EMPTY,
                format!(
                    "partial delivery for session {} needs a non-empty summary or at least one remaining work item",
                    session.id
                ),
            ));
        }
        let summary = if summary.is_empty() {
            DEFAULT_PARTIAL_DELIVERY_SUMMARY.to_string()
        } else {
            summary
        };

        let recorded_by = params
            .recorded_by
            .as_deref()
            .map(|value| {
                bounded_partial_delivery_text(value, MAX_PARTIAL_DELIVERY_RECORDED_BY_CHARS)
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "operator".to_string());

        let latest_commit = readiness.latest_commit.clone();
        let changed_paths = readiness.changed_paths.clone();
        let blockers = readiness.blockers.clone();
        let uncommitted = readiness.level == "dirty" || !changed_paths.is_empty();
        let record = PartialDeliveryRecord {
            schema: PARTIAL_DELIVERY_SCHEMA.to_string(),
            id: format!("pd-{}", Uuid::now_v7()),
            session_id: session.id.clone(),
            project_id: session.project_id.clone(),
            recorded_at: now_rfc3339(),
            recorded_by,
            summary,
            remaining_work,
            readiness_level: readiness.level.clone(),
            latest_commit,
            changed_paths,
            blockers,
            uncommitted,
        };

        self.append_event(
            &session.id,
            SESSION_PARTIAL_DELIVERY_EVENT,
            serde_json::to_value(&record)?,
        )
        .await?;

        let mut readiness = readiness;
        readiness.partial_delivery = Some(record.clone());
        Ok(PartialDeliveryResult {
            session_id: session.id,
            record,
            readiness,
        })
    }

    /// Latest delivery status for a session. Imported archives are readable;
    /// only recording a new partial delivery is refused.
    pub async fn session_delivery(
        &self,
        params: SessionDeliveryParams,
    ) -> Result<SessionDeliveryResult> {
        let readiness = self.session_readiness(&params.session_id).await?;
        Ok(SessionDeliveryResult {
            session_id: params.session_id,
            partial: readiness.partial_delivery.clone(),
            readiness,
        })
    }
}

fn partial_delivery_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    PartialDeliveryError::new(code, message).into()
}

/// Trim and bound a single operator-provided line by character count. The
/// ellipsis is included in the bound.
fn bounded_partial_delivery_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || max_chars == 0 {
        return String::new();
    }
    let mut chars = trimmed.chars();
    let mut bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        bounded.pop();
        bounded.push('…');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::{PartialDeliveryParams, SessionDeliveryParams};

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    async fn fixture() -> (TempDir, TempDir, Engine, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("delivery"))
            .unwrap();
        let session = store
            .create_session(&project.id, "partial delivery")
            .unwrap();
        let engine = Engine::new(store);
        (data, repo, engine, session)
    }

    async fn dirty_session(engine: &Engine, session: &Session) -> Session {
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        let session = engine.ensure_session_worktree(&session.id).await.unwrap();
        let worktree = session.worktree.as_deref().expect("session worktree");
        std::fs::create_dir_all(Path::new(worktree).join("src")).unwrap();
        std::fs::write(Path::new(worktree).join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        session
    }

    fn code(error: &anyhow::Error) -> Option<&'static str> {
        error
            .downcast_ref::<PartialDeliveryError>()
            .map(|typed| typed.code)
    }

    fn params(session: &Session) -> PartialDeliveryParams {
        PartialDeliveryParams {
            session_id: session.id.clone(),
            ..PartialDeliveryParams::default()
        }
    }

    #[tokio::test]
    async fn partial_delivery_requires_confirmation_before_any_event() {
        let (_data, _repo, engine, session) = fixture().await;
        let error = engine
            .record_partial_delivery(PartialDeliveryParams {
                summary: Some("hand over".into()),
                ..params(&session)
            })
            .await
            .expect_err("missing confirmation must fail");
        assert_eq!(
            code(&error),
            Some(partial_delivery_codes::DELIVERY_CONFIRM_REQUIRED)
        );
        assert_eq!(
            engine
                .count_events_of_kind(&session.id, SESSION_PARTIAL_DELIVERY_EVENT)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn partial_delivery_rejects_active_imported_and_complete_sessions() {
        let (_data, _repo, engine, session) = fixture().await;

        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        let active = engine
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                summary: Some("too early".into()),
                ..params(&session)
            })
            .await
            .expect_err("a running session must be stopped or paused first");
        assert_eq!(
            code(&active),
            Some(partial_delivery_codes::DELIVERY_SESSION_ACTIVE)
        );

        engine
            .store()
            .update_session_status(&session.id, "imported")
            .unwrap();
        let imported = engine
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                summary: Some("archive".into()),
                ..params(&session)
            })
            .await
            .expect_err("imported archives are read-only");
        assert_eq!(
            code(&imported),
            Some(partial_delivery_codes::DELIVERY_IMPORTED)
        );

        // Imported sessions are still readable, including a missing record.
        let delivery = engine
            .session_delivery(SessionDeliveryParams {
                session_id: session.id.clone(),
            })
            .await
            .expect("imported delivery read");
        assert!(delivery.partial.is_none());
        assert_eq!(delivery.session_id, session.id);

        // A merged session with no remaining work/blockers is complete.
        let (_data2, _repo2, engine2, session2) = fixture().await;
        engine2
            .store()
            .update_session_status(&session2.id, "idle")
            .unwrap();
        let entry = engine2
            .store()
            .enqueue_merge(
                &session2.id,
                "agent-1",
                "Worker",
                None,
                "feature",
                "0123456789abcdef0123456789abcdef01234567",
                "",
                None,
            )
            .unwrap();
        engine2
            .store()
            .update_merge_queue_entry(&entry.id, "merged", None)
            .unwrap();
        let complete = engine2
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                ..params(&session2)
            })
            .await
            .expect_err("a merged session with nothing left is complete");
        assert_eq!(
            code(&complete),
            Some(partial_delivery_codes::DELIVERY_ALREADY_COMPLETE)
        );

        // A merged session with explicit remaining work is still deliverable.
        let recorded = engine2
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                remaining_work: vec!["polish docs".into()],
                ..params(&session2)
            })
            .await
            .expect("remaining work keeps a merged delivery explicit");
        assert_eq!(recorded.record.readiness_level, "merged");
        assert_eq!(
            recorded.record.summary,
            DEFAULT_PARTIAL_DELIVERY_SUMMARY.to_string()
        );
        assert!(!recorded.record.uncommitted);
    }

    #[tokio::test]
    async fn partial_delivery_rejects_none_readiness_and_empty_operator_text() {
        let (_data, _repo, engine, session) = fixture().await;
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        let none = engine
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                ..params(&session)
            })
            .await
            .expect_err("nothing to deliver");
        assert_eq!(code(&none), Some(partial_delivery_codes::DELIVERY_EMPTY));

        let session = dirty_session(&engine, &session).await;
        let empty = engine
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                summary: Some("   ".into()),
                remaining_work: vec!["  ".into()],
                ..params(&session)
            })
            .await
            .expect_err("blank summary and no remaining items");
        assert_eq!(code(&empty), Some(partial_delivery_codes::DELIVERY_EMPTY));
    }

    #[tokio::test]
    async fn partial_delivery_records_bounded_event_backed_state() {
        let (_data, _repo, engine, session) = fixture().await;
        let session = dirty_session(&engine, &session).await;

        let long_summary = "s".repeat(3_000);
        let long_remaining = (0..60)
            .map(|index| format!("{index}-{}", "r".repeat(600)))
            .collect::<Vec<_>>();
        let result = engine
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                summary: Some(long_summary),
                remaining_work: long_remaining,
                recorded_by: Some("operator-but-long".repeat(20)),
                ..params(&session)
            })
            .await
            .expect("record partial delivery");

        assert_eq!(result.session_id, session.id);
        assert_eq!(result.record.schema, PARTIAL_DELIVERY_SCHEMA);
        assert!(result.record.id.starts_with("pd-"));
        assert_eq!(result.record.project_id, session.project_id);
        assert_eq!(result.record.readiness_level, "dirty");
        assert!(result.record.uncommitted);
        assert!(
            result
                .record
                .changed_paths
                .iter()
                .any(|path| path.contains("src/lib.rs"))
        );
        assert!(result.record.summary.chars().count() <= 2_000);
        assert_eq!(result.record.remaining_work.len(), 50);
        assert!(
            result
                .record
                .remaining_work
                .iter()
                .all(|item| item.chars().count() <= 500)
        );
        assert!(
            result.record.recorded_by.chars().count() <= MAX_PARTIAL_DELIVERY_RECORDED_BY_CHARS
        );
        assert!(result.readiness.partial_delivery.as_ref() == Some(&result.record));

        let event = engine
            .latest_event_of_kind(&session.id, SESSION_PARTIAL_DELIVERY_EVENT)
            .await
            .unwrap()
            .expect("partial delivery event");
        assert_eq!(event.kind, SESSION_PARTIAL_DELIVERY_EVENT);
        assert_eq!(event.payload["schema"], PARTIAL_DELIVERY_SCHEMA);
        assert_eq!(event.payload["id"], result.record.id);
        assert_eq!(event.payload["readiness_level"], "dirty");
        assert_eq!(event.payload["uncommitted"], true);
        assert_eq!(
            event.payload["remaining_work"].as_array().unwrap().len(),
            50
        );
        assert!(event.payload.get("summary").is_some());

        let readiness = engine.session_readiness(&session.id).await.unwrap();
        assert_eq!(readiness.partial_delivery.as_ref(), Some(&result.record));

        let delivery = engine
            .session_delivery(SessionDeliveryParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(delivery.partial.as_ref(), Some(&result.record));
        assert_eq!(
            delivery.readiness.partial_delivery.as_ref(),
            Some(&result.record)
        );

        // A newer malformed payload is treated as absent, not a crash.
        engine
            .append_event(
                &session.id,
                SESSION_PARTIAL_DELIVERY_EVENT,
                json!({"bad": true}),
            )
            .await
            .unwrap();
        let delivery = engine
            .session_delivery(SessionDeliveryParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert!(delivery.partial.is_none());

        engine
            .append_event(
                &session.id,
                SESSION_PARTIAL_DELIVERY_EVENT,
                json!({
                    "schema": "other/v1",
                    "id": "pd-untrusted",
                    "session_id": session.id,
                }),
            )
            .await
            .unwrap();
        let delivery = engine
            .session_delivery(SessionDeliveryParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert!(delivery.partial.is_none());
    }

    #[tokio::test]
    async fn partial_delivery_survives_export_and_import() {
        let (_data, _repo, engine, session) = fixture().await;
        let session = dirty_session(&engine, &session).await;
        let result = engine
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                summary: Some("archive this handover".into()),
                remaining_work: vec!["finish the parser".into()],
                ..params(&session)
            })
            .await
            .expect("record partial delivery");
        let bundle = engine.export_session(&session.id).await.unwrap();

        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let imported_engine = Engine::new(store);
        imported_engine.import_session(bundle).await.unwrap();
        let delivery = imported_engine
            .session_delivery(SessionDeliveryParams {
                session_id: session.id.clone(),
            })
            .await
            .expect("imported delivery read");
        assert_eq!(delivery.partial.as_ref(), Some(&result.record));
    }

    #[tokio::test]
    async fn partial_delivery_defaults_recorded_by_and_rejects_other_active_statuses() {
        let (_data, _repo, engine, session) = fixture().await;
        let session = dirty_session(&engine, &session).await;
        for status in ["queued", "waiting_input", "landed"] {
            engine
                .store()
                .update_session_status(&session.id, status)
                .unwrap();
            let error = engine
                .record_partial_delivery(PartialDeliveryParams {
                    confirm: true,
                    summary: Some("hand over".into()),
                    ..params(&session)
                })
                .await
                .expect_err("only inactive statuses may record partial delivery");
            assert_eq!(
                code(&error),
                Some(partial_delivery_codes::DELIVERY_SESSION_ACTIVE),
                "status {status}"
            );
        }

        engine
            .store()
            .update_session_status(&session.id, "paused")
            .unwrap();
        let result = engine
            .record_partial_delivery(PartialDeliveryParams {
                confirm: true,
                summary: Some("paused after the parser".into()),
                ..params(&session)
            })
            .await
            .expect("paused session records");
        assert_eq!(result.record.recorded_by, "operator");
        assert_eq!(result.record.summary, "paused after the parser");
    }
}
