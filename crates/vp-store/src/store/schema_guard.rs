use anyhow::Result;
use rusqlite::Connection;

/// On-disk schema generation. Bump only with an additive migration; refusal to
/// open a newer version is the downgrade guard.
pub const STORE_SCHEMA_VERSION: i64 = 1;

pub fn check_downgrade(conn: &Connection) -> Result<i64> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > STORE_SCHEMA_VERSION {
        anyhow::bail!(
            "database schema version {version} is newer than this build supports \
             ({STORE_SCHEMA_VERSION}); downgrade refused"
        );
    }
    Ok(version)
}

pub fn mark_current(conn: &Connection, current: i64) -> Result<()> {
    if current < STORE_SCHEMA_VERSION {
        conn.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_fail_closed() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", STORE_SCHEMA_VERSION + 1)
            .unwrap();
        let error = check_downgrade(&conn).unwrap_err();
        assert!(error.to_string().contains("downgrade refused"), "{error}");
    }
}
