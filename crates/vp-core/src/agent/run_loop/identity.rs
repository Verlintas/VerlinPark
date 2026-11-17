use super::*;

/// Resolve the project identity for a top-level run once, before the first
/// event is emitted. The resolved id/generation are observational provenance;
/// they do not grant authority or change any runtime gate.
pub(in crate::agent) async fn resolve_run_identity(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
) -> Result<(Option<String>, Option<u64>)> {
    Ok(
        match engine
            .prompt_identity_for_run(
                &session.id,
                spec.role.as_str(),
                spec.title.as_deref(),
                spec.department.as_deref(),
            )
            .await?
        {
            Some(identity) => (Some(identity.id), Some(identity.generation)),
            None => (spec.identity_id.clone(), spec.identity_generation),
        },
    )
}
