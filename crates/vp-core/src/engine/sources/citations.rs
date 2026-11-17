use super::support::truncate_chars;
use super::*;

impl Engine {
    /// Derive citation evidence from persisted `agent.message` events. The
    /// event log remains the source of truth; no second citation projection can
    /// drift from it.
    pub async fn session_citations(
        &self,
        session_id: &str,
        message_id: Option<String>,
    ) -> Result<SessionCitationsResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut citations = Vec::new();
        for event in events {
            let Some(parsed) = parse_message_citation_event(&event) else {
                continue;
            };
            if message_id
                .as_deref()
                .is_some_and(|wanted| wanted != parsed.message_id)
            {
                continue;
            }
            for citation in parsed.citations {
                citations.push(MessageCitation {
                    message_id: parsed.message_id.clone(),
                    citation,
                });
            }
        }
        Ok(SessionCitationsResult { citations })
    }

    /// Deterministic citation coverage report. It exposes dangling `[cN]`
    /// markers and citation records the model never referenced so the UI can
    /// surface evidence problems instead of silently trusting prose.
    pub async fn session_citation_audit(
        &self,
        session_id: &str,
    ) -> Result<SessionCitationAuditResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut messages = Vec::new();
        let mut messages_with_citations = 0usize;
        let mut messages_without_citations = 0usize;
        let mut total_citations = 0usize;
        let mut unique_sources = std::collections::HashSet::new();
        let mut dangling_total = 0usize;
        let mut unused_total = 0usize;
        for event in events {
            let Some(parsed) = parse_message_citation_event(&event) else {
                continue;
            };
            if parsed.text.trim().is_empty() {
                continue;
            }
            let referenced = citation_keys_in_text(&parsed.text);
            let available = parsed
                .citations
                .iter()
                .map(|citation| citation.key.clone())
                .collect::<Vec<_>>();
            let dangling = referenced
                .iter()
                .filter(|key| !available.iter().any(|available| available == *key))
                .cloned()
                .collect::<Vec<_>>();
            let unused = available
                .iter()
                .filter(|key| !referenced.iter().any(|referenced| referenced == *key))
                .cloned()
                .collect::<Vec<_>>();
            if parsed.citations.is_empty() {
                messages_without_citations += 1;
            } else {
                messages_with_citations += 1;
            }
            total_citations += parsed.citations.len();
            for citation in &parsed.citations {
                unique_sources.insert(citation.source_id.clone());
            }
            dangling_total += dangling.len();
            unused_total += unused.len();
            messages.push(CitationAuditMessage {
                message_id: parsed.message_id,
                agent_name: parsed.agent_name,
                excerpt: truncate_chars(&parsed.text, 240),
                citation_count: parsed.citations.len(),
                referenced_keys: referenced,
                dangling_keys: dangling,
                unused_keys: unused,
                created_at: parsed.created_at,
            });
        }
        Ok(SessionCitationAuditResult {
            audit: CitationAudit {
                messages_with_citations,
                messages_without_citations,
                total_citations,
                unique_sources: unique_sources.len(),
                dangling_key_count: dangling_total,
                unused_citation_count: unused_total,
                messages,
            },
        })
    }
}

struct ParsedCitationMessage {
    message_id: String,
    agent_name: Option<String>,
    text: String,
    citations: Vec<SourceCitation>,
    created_at: String,
}

fn parse_message_citation_event(event: &Event) -> Option<ParsedCitationMessage> {
    if event.kind != "agent.message" {
        return None;
    }
    let payload = &event.payload;
    let message_id = payload
        .get("message_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}:{}", event.session_id, event.seq));
    let text = payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let citations = payload
        .get("citations")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| serde_json::from_value::<SourceCitation>(value.clone()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let agent_name = payload
        .get("agent_name")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            payload
                .get("agent_title")
                .and_then(serde_json::Value::as_str)
        })
        .map(ToOwned::to_owned);
    Some(ParsedCitationMessage {
        message_id,
        agent_name,
        text,
        citations,
        created_at: event.created_at.clone(),
    })
}

/// Extract `[cN]` markers without a regex. Markers are ASCII even when the
/// surrounding prose is not, so byte scanning is safe and deterministic.
fn citation_keys_in_text(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut keys = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'[' || index + 3 >= bytes.len() {
            index += 1;
            continue;
        }
        let prefix = bytes[index + 1];
        if prefix != b'c' && prefix != b'C' {
            index += 1;
            continue;
        }
        let mut end = index + 2;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > index + 2 && end < bytes.len() && bytes[end] == b']' {
            let key = format!("c{}", &text[index + 2..end]);
            if !keys.iter().any(|existing| existing == &key) {
                keys.push(key);
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    keys
}
