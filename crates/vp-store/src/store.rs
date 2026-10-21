use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{Event, Project, Session};

use crate::paths;

/// SQLite-backed metadata store plus an append-only JSONL event mirror.
///
/// The event log is the long-term source of truth; SQLite is the fast query
/// projection. For M0 the mirror is best-effort and SQLite is authoritative.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    data_dir: PathBuf,
}

impl Store {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        paths::ensure_private_dir(&data_dir)?;
        paths::ensure_private_dir(&data_dir.join("run"))?;
        paths::ensure_private_dir(&data_dir.join("sessions"))?;
        paths::ensure_private_dir(&data_dir.join("logs"))?;

        let conn = Connection::open(data_dir.join("verlinpark.db"))
            .with_context(|| format!("opening database in {}", data_dir.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                id            TEXT PRIMARY KEY,
                name          TEXT NOT NULL,
                root          TEXT NOT NULL UNIQUE,
                default_branch TEXT NOT NULL,
                created_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id         TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                goal       TEXT NOT NULL,
                status     TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS events (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                type       TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (session_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_events_session_seq
                ON events(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_sessions_project
                ON sessions(project_id);
            "#,
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            data_dir,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("database connection mutex poisoned"))
    }

    pub fn add_project(&self, root: &str, name: Option<&str>) -> Result<Project> {
        let root_path = PathBuf::from(root);
        if !root_path.is_dir() {
            return Err(anyhow!("project root is not a directory: {root}"));
        }
        let root_path = root_path
            .canonicalize()
            .with_context(|| format!("canonicalizing {root}"))?;
        let root = root_path.to_string_lossy().to_string();

        let name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                root_path
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "Untitled project".to_string());

        let default_branch = detect_default_branch(&root_path);
        let project = Project {
            id: Uuid::now_v7().to_string(),
            name,
            root: root.clone(),
            default_branch,
            created_at: now_rfc3339(),
        };

        let conn = self.lock()?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM projects WHERE root = ?1",
                params![project.root],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Err(anyhow!("project already added: {id}"));
        }
        conn.execute(
            "INSERT INTO projects (id, name, root, default_branch, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.id,
                project.name,
                project.root,
                project.default_branch,
                project.created_at
            ],
        )?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, root, default_branch, created_at
             FROM projects ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                root: row.get(2)?,
                default_branch: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, root, default_branch, created_at FROM projects WHERE id = ?1",
            params![id],
            |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    root: row.get(2)?,
                    default_branch: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_session(&self, project_id: &str, goal: &str) -> Result<Session> {
        if self.get_project(project_id)?.is_none() {
            return Err(anyhow!("project not found: {project_id}"));
        }
        let goal = goal.trim();
        if goal.is_empty() {
            return Err(anyhow!("session goal must not be empty"));
        }
        let session = Session {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            goal: goal.to_string(),
            status: "draft".to_string(),
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sessions (id, project_id, goal, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.id,
                session.project_id,
                session.goal,
                session.status,
                session.created_at
            ],
        )?;
        Ok(session)
    }

    pub fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<Session>> {
        let conn = self.lock()?;
        let mut sessions = Vec::new();
        match project_id {
            Some(project_id) => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, created_at
                     FROM sessions WHERE project_id = ?1
                     ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map(params![project_id], |row| {
                    Ok(Session {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        goal: row.get(2)?,
                        status: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                })?;
                sessions.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, created_at
                     FROM sessions ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok(Session {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        goal: row.get(2)?,
                        status: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                })?;
                sessions.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
        }
        Ok(sessions)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, goal, status, created_at FROM sessions WHERE id = ?1",
            params![id],
            |row| {
                Ok(Session {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    goal: row.get(2)?,
                    status: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn append_event(
        &self,
        session_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Event> {
        if self.get_session(session_id)?.is_none() {
            return Err(anyhow!("session not found: {session_id}"));
        }

        let event = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let seq: u64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )?;
            let event = Event {
                session_id: session_id.to_string(),
                seq,
                kind: kind.to_string(),
                payload,
                created_at: now_rfc3339(),
            };
            tx.execute(
                "INSERT INTO events (session_id, seq, type, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    event.session_id,
                    event.seq as i64,
                    event.kind,
                    serde_json::to_string(&event.payload)?,
                    event.created_at
                ],
            )?;
            tx.commit()?;
            event
        };

        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    fn append_event_mirror(&self, event: &Event) -> Result<()> {
        let dir = self.data_dir.join("sessions").join(&event.session_id);
        paths::ensure_private_dir(&dir)?;
        let path = dir.join("events.jsonl");
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    pub fn list_events(&self, session_id: &str, since_seq: u64, limit: u32) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events
             WHERE session_id = ?1 AND seq >= ?2
             ORDER BY seq ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![session_id, since_seq as i64, limit.clamp(1, 1000) as i64],
            |row| {
                let payload: String = row.get(3)?;
                Ok(Event {
                    session_id: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn project_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn session_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok(count as usize)
    }
}

fn detect_default_branch(root: &Path) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if branch.is_empty() {
                "main".to_string()
            } else {
                branch
            }
        }
        _ => "main".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, Store, TempDir) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        (data, store, repo)
    }

    #[test]
    fn adds_and_lists_projects() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        assert_eq!(project.name, "demo");
        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, project.id);
        assert_eq!(store.project_count().unwrap(), 1);
    }

    #[test]
    fn rejects_duplicate_project() {
        let (_data, store, repo) = test_store();
        store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let error = store
            .add_project(repo.path().to_str().unwrap(), Some("again"))
            .unwrap_err();
        assert!(error.to_string().contains("already added"));
    }

    #[test]
    fn creates_sessions_and_events() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "ship a feature").unwrap();
        assert_eq!(session.status, "draft");

        let first = store
            .append_event(
                &session.id,
                "session.created",
                serde_json::json!({"goal": "ship a feature"}),
            )
            .unwrap();
        let second = store
            .append_event(&session.id, "note", serde_json::json!({"text": "hello"}))
            .unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);

        let events = store.list_events(&session.id, 1, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, "note");

        let path = store
            .data_dir()
            .join("sessions")
            .join(&session.id)
            .join("events.jsonl");
        let mirror = std::fs::read_to_string(path).unwrap();
        assert_eq!(mirror.lines().count(), 2);
    }

    #[test]
    fn lists_sessions_by_project() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        store.create_session(&project.id, "one").unwrap();
        store.create_session(&project.id, "two").unwrap();
        let sessions = store.list_sessions(Some(&project.id)).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(store.list_sessions(None).unwrap().len(), 2);
    }
}
