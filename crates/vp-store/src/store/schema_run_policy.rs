//! Additive project Run Contract policy schema.
//!
//! Kept separate from the ordered main schema batch so `schema.rs` stays
//! within the module-governance line budget. A missing row means derived
//! defaults; an explicit row is reported as `project_policy`.

use super::*;

pub(super) fn ensure(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_run_policies (
            project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
            policy     TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )?;
    Ok(())
}
