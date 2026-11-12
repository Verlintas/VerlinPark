//! Project-scoped agent identities: validation and event replay.
//!
//! S1 deliberately keeps this module small and deterministic. It validates the
//! append-only identity stream and derives the projection; it does not assign
//! identities to sessions, create inboxes or workspaces, route models, or
//! change the role/profile execution gate.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use vp_protocol::types::{
    AgentIdentity, AgentIdentityError, AgentIdentityEvent, AgentIdentityPolicy,
    AgentIdentityReplayReport, agent_identity_codes,
};

pub use vp_protocol::types::agent_identity_codes as codes;

/// Validate one lifecycle event and return the bounded identity snapshot it
/// carries. Malformed or cross-project payloads fail closed.
pub fn identity_from_event(event: &AgentIdentityEvent) -> Result<AgentIdentity> {
    if !vp_protocol::types::AGENT_IDENTITY_EVENT_KINDS.contains(&event.kind.as_str()) {
        return Err(AgentIdentityError::new(
            agent_identity_codes::IDENTITY_INVALID,
            format!(
                "identity event {}:{} has an unknown kind",
                event.identity_id, event.seq
            ),
        )
        .into());
    }
    let identity: AgentIdentity =
        serde_json::from_value(event.payload.get("identity").cloned().ok_or_else(|| {
            AgentIdentityError::new(
                agent_identity_codes::IDENTITY_INVALID,
                format!(
                    "identity event {}:{} has no identity snapshot",
                    event.identity_id, event.seq
                ),
            )
        })?)?;
    if identity.id != event.identity_id {
        return Err(AgentIdentityError::new(
            agent_identity_codes::IDENTITY_INVALID,
            format!(
                "identity event {}:{} snapshot id does not match",
                event.identity_id, event.seq
            ),
        )
        .into());
    }
    if identity.project_id != event.project_id {
        return Err(AgentIdentityError::new(
            agent_identity_codes::IDENTITY_PROJECT_MISMATCH,
            format!(
                "identity event {}:{} snapshot belongs to another project",
                event.identity_id, event.seq
            ),
        )
        .into());
    }
    if identity.id.is_empty() || identity.project_id.is_empty() {
        return Err(AgentIdentityError::new(
            agent_identity_codes::IDENTITY_INVALID,
            "identity snapshot has an empty id or project id",
        )
        .into());
    }
    if vp_protocol::types::agent_identity_role(&identity.role).is_none() {
        return Err(AgentIdentityError::new(
            agent_identity_codes::IDENTITY_ROLE_INVALID,
            format!("identity {} has an invalid role", identity.id),
        )
        .into());
    }
    if !vp_protocol::types::AGENT_IDENTITY_STATUSES.contains(&identity.status.as_str()) {
        return Err(AgentIdentityError::new(
            agent_identity_codes::IDENTITY_INVALID,
            format!("identity {} has an invalid status", identity.id),
        )
        .into());
    }
    if !matches!(identity.origin.as_str(), "local" | "imported") {
        return Err(AgentIdentityError::new(
            agent_identity_codes::IDENTITY_INVALID,
            format!("identity {} has an invalid origin", identity.id),
        )
        .into());
    }
    if identity.generation == 0 {
        return Err(AgentIdentityError::new(
            agent_identity_codes::IDENTITY_INVALID,
            format!("identity {} has generation 0", identity.id),
        )
        .into());
    }
    Ok(identity)
}

/// Fold the append-only stream into the latest projection row per identity.
/// Events must already be in `(identity_id, seq)` order; the caller supplies
/// the store's ordered list.
pub fn identities_from_events(events: &[AgentIdentityEvent]) -> Result<Vec<AgentIdentity>> {
    let mut latest: BTreeMap<String, (u64, AgentIdentity)> = BTreeMap::new();
    for event in events {
        let identity = identity_from_event(event)?;
        match latest.get(&identity.id) {
            Some((seq, _)) if *seq > event.seq => {
                return Err(AgentIdentityError::new(
                    agent_identity_codes::IDENTITY_INVALID,
                    format!(
                        "identity event stream for {} is out of order at seq {}",
                        identity.id, event.seq
                    ),
                )
                .into());
            }
            Some((seq, _)) if *seq == event.seq => {
                return Err(AgentIdentityError::new(
                    agent_identity_codes::IDENTITY_CONFLICT,
                    format!(
                        "identity event stream for {} has duplicate seq {}",
                        identity.id, event.seq
                    ),
                )
                .into());
            }
            _ => {}
        }
        latest.insert(identity.id.clone(), (event.seq, identity));
    }
    Ok(latest.into_values().map(|(_, identity)| identity).collect())
}

/// Compare the event-derived projection with the stored rows. The comparison
/// uses the complete bounded identity record, not free-form prose.
pub fn compare_identity_projection(
    expected: &[AgentIdentity],
    actual: &[AgentIdentity],
) -> Vec<String> {
    // `origin` is archive provenance, not lifecycle state. Imported rows stay
    // read-only but their event snapshots still say `local`; replay compares
    // the lifecycle record and ignores only that storage marker.
    let comparable = |identity: &AgentIdentity| {
        let mut identity = identity.clone();
        identity.origin = "local".to_string();
        identity
    };
    let expected = expected.iter().map(comparable).collect::<Vec<_>>();
    let actual = actual.iter().map(comparable).collect::<Vec<_>>();
    let expected_by_id = expected
        .iter()
        .map(|identity| (identity.id.as_str(), identity))
        .collect::<BTreeMap<_, _>>();
    let actual_by_id = actual
        .iter()
        .map(|identity| (identity.id.as_str(), identity))
        .collect::<BTreeMap<_, _>>();
    let mut mismatches = Vec::new();
    for id in expected_by_id
        .keys()
        .chain(actual_by_id.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        match (expected_by_id.get(id), actual_by_id.get(id)) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(expected), Some(actual)) => mismatches.push(format!(
                "identity {id}: projection generation {} does not match replay generation {}",
                actual.generation, expected.generation
            )),
            (Some(_), None) => {
                mismatches.push(format!("identity {id}: event stream exists without a row"))
            }
            (None, Some(_)) => mismatches.push(format!(
                "identity {id}: row exists without a lifecycle event"
            )),
            (None, None) => {}
        }
    }
    mismatches
}

/// Read-only replay report. `repair` is only honored by the caller after the
/// project is confirmed quiescent; this function never appends events.
pub fn replay_report(
    project_id: &str,
    expected: &[AgentIdentity],
    events: usize,
    repaired: bool,
    mismatches: Vec<String>,
) -> AgentIdentityReplayReport {
    AgentIdentityReplayReport {
        project_id: project_id.to_string(),
        identities: expected.len() as u64,
        events: events as u64,
        consistent: mismatches.is_empty(),
        repaired,
        mismatches,
    }
}

/// Validate a policy without writing it. Core and the store share this
/// normalization so an invalid cap can never be enabled implicitly.
pub fn normalize_policy(policy: &AgentIdentityPolicy) -> Result<AgentIdentityPolicy> {
    policy.normalized().map_err(|error| {
        AgentIdentityError::new(agent_identity_codes::IDENTITY_POLICY_INVALID, error).into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity(id: &str, project_id: &str, generation: u64) -> AgentIdentity {
        AgentIdentity {
            id: id.into(),
            project_id: project_id.into(),
            role: "worker".into(),
            department: None,
            component: None,
            title: None,
            profile: Some("coding".into()),
            model: None,
            parent_identity_id: None,
            status: "active".into(),
            generation,
            created_at: "2026-11-12T00:00:00Z".into(),
            updated_at: "2026-11-12T00:00:00Z".into(),
            retired_at: None,
            successor_id: None,
            policy: json!({}),
            origin: "local".into(),
        }
    }

    fn event(identity: &AgentIdentity, seq: u64) -> AgentIdentityEvent {
        AgentIdentityEvent {
            identity_id: identity.id.clone(),
            project_id: identity.project_id.clone(),
            seq,
            kind: "agent.identity.updated".into(),
            payload: json!({"identity": identity}),
            created_at: identity.updated_at.clone(),
        }
    }

    #[test]
    fn folds_latest_snapshot_and_rejects_cross_project_payloads() {
        let first = identity("id-1", "prj-1", 1);
        let mut second = first.clone();
        second.generation = 2;
        let rows = identities_from_events(&[event(&first, 1), event(&second, 2)]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].generation, 2);

        let mut wrong = second;
        wrong.project_id = "prj-2".into();
        let mut wrong_event = event(&wrong, 3);
        wrong_event.project_id = "prj-1".into();
        let error = identities_from_events(&[wrong_event]).unwrap_err();
        let typed = error
            .downcast_ref::<AgentIdentityError>()
            .expect("typed identity error");
        assert_eq!(typed.code, agent_identity_codes::IDENTITY_PROJECT_MISMATCH);
    }

    #[test]
    fn rejects_duplicate_sequences() {
        let row = identity("id-1", "prj-1", 1);
        let error = identities_from_events(&[event(&row, 1), event(&row, 1)]).unwrap_err();
        let typed = error
            .downcast_ref::<AgentIdentityError>()
            .expect("typed identity error");
        assert_eq!(typed.code, agent_identity_codes::IDENTITY_CONFLICT);
    }

    #[test]
    fn compare_reports_missing_and_extra_rows_without_inventing_a_row() {
        let row = identity("id-1", "prj-1", 1);
        let mut changed = row.clone();
        changed.generation = 3;
        let mismatches = compare_identity_projection(&[row], &[changed]);
        assert_eq!(mismatches.len(), 1);
        assert!(mismatches[0].contains("generation"));
        assert_eq!(compare_identity_projection(&[], &[]).len(), 0);
    }
}
