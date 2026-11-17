use super::*;

impl Engine {
    pub(super) async fn project_for_sources(&self, project_id: &str) -> Result<Project> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&id))
            .await??
            .ok_or_else(|| anyhow!("project not found: {project_id}"))
    }
}

pub(super) fn validate_source_title(title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 || title.chars().any(char::is_control) {
        anyhow::bail!("source title must be 1-200 characters without control characters");
    }
    Ok(())
}

pub(super) fn validate_source_labels(labels: Vec<String>) -> Result<Vec<String>> {
    if labels.len() > 12 {
        anyhow::bail!("a source supports at most 12 labels");
    }
    let mut output = Vec::new();
    for label in labels {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 40 || label.chars().any(char::is_control) {
            anyhow::bail!("source labels must be 1-40 characters without control characters");
        }
        if !output.iter().any(|existing| existing == label) {
            output.push(label.to_string());
        }
    }
    Ok(output)
}

pub(super) fn build_source_chunks(source_id: &str, content: &str) -> Vec<SourceChunk> {
    crate::sources::chunk_text(content)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let digest = sha256_hex(
                format!(
                    "{}:{}:{}:{}",
                    source_id, index, chunk.start_offset, chunk.content
                )
                .as_bytes(),
            );
            SourceChunk {
                id: format!("{source_id}:{:04}:{}", index + 1, &digest[..12]),
                source_id: source_id.to_string(),
                chunk_no: (index + 1) as u32,
                heading: chunk.heading,
                content: chunk.content,
                start_offset: chunk.start_offset,
                end_offset: chunk.end_offset,
                token_count: chunk.token_count,
            }
        })
        .collect()
}

pub(super) fn normalize_source_collection(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > 80 || value.chars().any(char::is_control) {
        anyhow::bail!("source collection must be at most 80 characters without control characters");
    }
    Ok(Some(value.to_string()))
}

pub(super) fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}
