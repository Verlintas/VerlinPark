//! Agent-scoped event-index reads for the V1 Collab Agent Inspector.
//!
//! `events` is ordered by `(session_id, seq)`; the inspector needs "events for
//! this one agent" without scanning and decoding every session event. The
//! `agent_event_index` projection is maintained by SQLite triggers on append
//! and is backfilled once per store (see `schema.rs`). These reads join the
//! index back to the authoritative event row, so a stale derived row can never
//! fabricate an event.

use super::*;

/// Create the derived index rows for events written before the trigger
/// existed. The marker is written only after the idempotent `INSERT OR
/// IGNORE`, so an interrupted upgrade is safe to rerun. If the marker exists
/// but the table is empty, an offline repair/import removed derived state;
/// rebuild defensively instead of serving an empty inspector timeline.
pub(super) fn backfill_agent_event_index(conn: &Connection) -> Result<()> {
    let migrated = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = 'agent_event_index_v1'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let has_rows: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_event_index)",
        [],
        |row| row.get(0),
    )?;
    if migrated.is_none() || has_rows == 0 {
        conn.execute(
            "INSERT OR IGNORE INTO agent_event_index
                (session_id, agent_id, seq, kind, created_at)
             SELECT session_id, json_extract(payload, '$.agent_id'), seq, type, created_at
             FROM events
             WHERE json_valid(payload) AND json_type(payload, '$.agent_id') = 'text'",
            [],
        )?;
        conn.execute(
            "INSERT INTO store_meta (key, value) VALUES ('agent_event_index_v1', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
    }
    Ok(())
}

impl Store {
    /// Bounded newest window for one agent, returned oldest-first so a UI can
    /// append rows without re-sorting. The hard ceiling is 200 events; larger
    /// requests are clamped rather than rejected.
    pub fn list_agent_events(
        &self,
        session_id: &str,
        agent_id: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let limit = limit.clamp(1, 200) as i64;
        let rows = {
            let conn = self.lock()?;
            let mut statement = conn.prepare(
                "SELECT e.session_id, e.seq, e.type, e.payload, e.created_at
                 FROM agent_event_index i
                 JOIN events e
                   ON e.session_id = i.session_id AND e.seq = i.seq
                 WHERE i.session_id = ?1 AND i.agent_id = ?2
                 ORDER BY i.seq DESC
                 LIMIT ?3",
            )?;
            statement
                .query_map(params![session_id, agent_id, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = rows
            .into_iter()
            .map(|(session_id, seq, kind, payload, created_at)| Event {
                session_id,
                seq: seq.max(0) as u64,
                kind,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at,
            })
            .collect::<Vec<_>>();
        events.reverse();
        Ok(events)
    }

    /// Exact count of indexed events that still have an authoritative event
    /// row. The inspector uses this only as a truncation probe at the 200-row
    /// ceiling; it never loads payloads.
    pub fn agent_event_count(&self, session_id: &str, agent_id: &str) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*)
             FROM agent_event_index i
             JOIN events e
               ON e.session_id = i.session_id AND e.seq = i.seq
             WHERE i.session_id = ?1 AND i.agent_id = ?2",
            params![session_id, agent_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn latest_agent_event_of_kind(
        &self,
        session_id: &str,
        agent_id: &str,
        kind: &str,
    ) -> Result<Option<Event>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT e.session_id, e.seq, e.type, e.payload, e.created_at
             FROM agent_event_index i
             JOIN events e
               ON e.session_id = i.session_id AND e.seq = i.seq
             WHERE i.session_id = ?1 AND i.agent_id = ?2 AND i.kind = ?3
             ORDER BY i.seq DESC
             LIMIT 1",
            params![session_id, agent_id, kind],
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
        )
        .optional()
        .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn store_and_session(data: &TempDir) -> (Store, String) {
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("inspect"))
            .unwrap();
        let session = store
            .create_session(&project.id, "inspect an agent")
            .unwrap();
        (store, session.id)
    }

    fn append(store: &Store, session_id: &str, agent_id: Option<&str>, text: &str) -> Event {
        let payload = match agent_id {
            Some(agent_id) => json!({"agent_id": agent_id, "text": text}),
            None => json!({"text": text}),
        };
        store
            .append_event(session_id, "agent.message", payload)
            .unwrap()
    }

    #[test]
    fn agent_event_index_populates_for_agent_events_and_skips_other_events() {
        let data = TempDir::new().unwrap();
        let (store, session_id) = store_and_session(&data);

        let indexed = append(&store, &session_id, Some("agent-a"), "hello");
        let unindexed = append(&store, &session_id, None, "session-wide");
        append(&store, &session_id, Some("agent-b"), "other agent");

        let events = store.list_agent_events(&session_id, "agent-a", 20).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, indexed.seq);

        let other = store.list_agent_events(&session_id, "agent-b", 20).unwrap();
        assert_eq!(other.len(), 1);
        assert!(other.iter().all(|event| event.seq != unindexed.seq));
        assert_eq!(store.agent_event_count(&session_id, "agent-a").unwrap(), 1);
    }

    #[test]
    fn agent_event_index_backfills_when_marker_or_rows_are_missing() {
        let data = TempDir::new().unwrap();
        let session_id;
        {
            let (store, id) = store_and_session(&data);
            session_id = id;
            append(&store, &session_id, Some("agent-a"), "before backfill");
        }
        {
            let conn = Connection::open(data.path().join("verlinpark.db")).unwrap();
            conn.execute("DELETE FROM agent_event_index", []).unwrap();
            conn.execute(
                "DELETE FROM store_meta WHERE key = 'agent_event_index_v1'",
                [],
            )
            .unwrap();
        }
        let reopened = Store::open(data.path()).unwrap();
        let events = reopened
            .list_agent_events(&session_id, "agent-a", 20)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["text"], "before backfill");
        {
            let conn = Connection::open(data.path().join("verlinpark.db")).unwrap();
            let marker: String = conn
                .query_row(
                    "SELECT value FROM store_meta WHERE key = 'agent_event_index_v1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(marker, "1");
            conn.execute("DELETE FROM agent_event_index", []).unwrap();
        }
        drop(reopened);
        // A marker without rows means derived state was lost (offline repair or
        // import); opening must rebuild instead of serving an empty timeline.
        let rebuilt = Store::open(data.path()).unwrap();
        let events = rebuilt
            .list_agent_events(&session_id, "agent-a", 20)
            .unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn agent_event_window_is_bounded_newest_first_and_agent_scoped() {
        let data = TempDir::new().unwrap();
        let (store, session_id) = store_and_session(&data);
        let mut agent_a = Vec::new();
        for index in 0..5 {
            agent_a.push(append(
                &store,
                &session_id,
                Some("agent-a"),
                &format!("a-{index}"),
            ));
        }
        for index in 0..2 {
            append(&store, &session_id, Some("agent-b"), &format!("b-{index}"));
        }

        let newest = store.list_agent_events(&session_id, "agent-a", 2).unwrap();
        assert_eq!(newest.len(), 2);
        assert_eq!(newest[0].seq, agent_a[3].seq);
        assert_eq!(newest[1].seq, agent_a[4].seq);
        assert!(newest.iter().all(|event| event.seq >= agent_a[3].seq));

        let clamped = store
            .list_agent_events(&session_id, "agent-a", 10_000)
            .unwrap();
        assert_eq!(clamped.len(), 5);
        assert!(clamped.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        assert_eq!(store.agent_event_count(&session_id, "agent-b").unwrap(), 2);

        let latest = store
            .latest_agent_event_of_kind(&session_id, "agent-a", "agent.message")
            .unwrap()
            .unwrap();
        assert_eq!(latest.seq, agent_a[4].seq);
        assert!(
            store
                .latest_agent_event_of_kind(&session_id, "agent-a", "tool.finished")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn agent_usage_totals_are_aggregated_in_sql() {
        let data = TempDir::new().unwrap();
        let (store, session_id) = store_and_session(&data);
        store
            .add_usage_entry_with_events(
                &session_id,
                "agent-a",
                "Worker",
                "worker",
                None,
                Some("impl"),
                "model",
                100,
                20,
                40,
                5,
                false,
                0.25,
            )
            .unwrap();
        store
            .add_usage_entry_with_events(
                &session_id,
                "agent-a",
                "Worker",
                "worker",
                None,
                Some("impl"),
                "model",
                200,
                30,
                60,
                7,
                true,
                0.5,
            )
            .unwrap();
        store
            .add_usage_entry_with_events(
                &session_id,
                "agent-b",
                "Other",
                "worker",
                None,
                Some("impl"),
                "model",
                999,
                99,
                0,
                0,
                false,
                9.0,
            )
            .unwrap();

        let totals = store.agent_usage_totals(&session_id, "agent-a").unwrap();
        assert_eq!(totals.calls, 2);
        assert_eq!(totals.input_tokens, 300);
        assert_eq!(totals.output_tokens, 50);
        assert_eq!(totals.cache_read_tokens, 100);
        assert_eq!(totals.cache_write_tokens, 12);
        assert!((totals.cost_usd - 0.75).abs() < 1e-9);

        let empty = store
            .agent_usage_totals(&session_id, "agent-missing")
            .unwrap();
        assert_eq!(empty.calls, 0);
        assert_eq!(empty.input_tokens, 0);
        assert_eq!(empty.cost_usd, 0.0);
    }
}
