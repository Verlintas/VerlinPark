//! Human renderers for forges, pull requests, archives, and media.

use super::*;

pub(crate) fn forge_preflight_lines(value: &Value) -> Option<Vec<String>> {
    if value.get("auth").is_none()
        || value.get("change_name").is_none()
        || value.get("provider").is_none()
        || value.get("default_provider").is_some()
    {
        return None;
    }
    let mut lines = vec![format!(
        "{} preflight  {}  auth {}  repository {}",
        value["provider"].as_str().unwrap_or("?"),
        value["change_name"].as_str().unwrap_or("PR"),
        value["auth"].as_str().unwrap_or("?"),
        value["repository"].as_str().unwrap_or("-")
    )];
    if let Some(detail) = value["detail"].as_str().filter(|detail| !detail.is_empty()) {
        lines.push(format!("  {detail}"));
    }
    Some(lines)
}

pub(crate) fn forge_status_lines(value: &Value) -> Option<Vec<String>> {
    let providers = value.get("providers")?.as_array()?;
    value.get("default_provider")?;
    let mut lines = vec![format!(
        "forge  selected {}  override {}",
        value["default_provider"].as_str().unwrap_or("?"),
        value["provider_override"].as_str().unwrap_or("auto")
    )];
    if let Some(origin) = value.get("origin").filter(|origin| !origin.is_null()) {
        lines.push(format!(
            "  origin {}  provider {}  repository {}  supported {}",
            origin["host"].as_str().unwrap_or("-"),
            origin["provider"].as_str().unwrap_or("unknown"),
            origin["repository"].as_str().unwrap_or("-"),
            origin["supported"].as_bool().unwrap_or(false)
        ));
        if let Some(detail) = origin["detail"]
            .as_str()
            .filter(|detail| !detail.is_empty())
        {
            lines.push(format!("  {detail}"));
        }
    }
    for provider in providers {
        let available = provider["command_available"].as_bool().unwrap_or(false);
        let auth = match provider.get("auth_configured").and_then(Value::as_bool) {
            Some(true) => "credentials configured",
            Some(false) => "credentials missing",
            None => "auth checked at preflight",
        };
        lines.push(format!(
            "  {:<16} {:<4} {:<4} {:<24} {}",
            provider["provider"].as_str().unwrap_or("?"),
            provider["change_name"].as_str().unwrap_or("?"),
            provider["transport"].as_str().unwrap_or("?"),
            provider["command"].as_str().unwrap_or("REST"),
            if available { auth } else { "unavailable" }
        ));
        if let Some(detail) = provider["detail"].as_str().filter(|_| !available) {
            lines.push(format!("    {detail}"));
        }
    }
    if let Some(detail) = value.get("detail").and_then(Value::as_str) {
        lines.push(format!("  {detail}"));
    }
    Some(lines)
}

pub(crate) fn pull_request_lines(value: &Value) -> Option<Vec<String>> {
    let url = value.get("url")?.as_str()?;
    if value.get("pushed").is_none() || value.get("branch").is_none() {
        return None;
    }
    Some(vec![format!(
        "{} opened via {}: {url}  ({} → {})",
        value["change"].as_str().unwrap_or("PR"),
        value["provider"].as_str().unwrap_or("forge"),
        value["branch"].as_str().unwrap_or("?"),
        value["base_branch"].as_str().unwrap_or("?")
    )])
}

pub(crate) fn archive_list_lines(value: &Value) -> Option<Vec<String>> {
    let entries = value.get("entries")?.as_array()?;
    if value.get("format").and_then(Value::as_str).is_none()
        || value.get("entry_count").and_then(Value::as_u64).is_none()
    {
        return None;
    }
    let mut lines = vec![format!(
        "{} · {} · {} entries · {} bytes uncompressed{}",
        value["name"].as_str().unwrap_or("archive"),
        value["format"].as_str().unwrap_or("?"),
        value["entry_count"].as_u64().unwrap_or(0),
        value["total_uncompressed_bytes"].as_u64().unwrap_or(0),
        if value["truncated"].as_bool().unwrap_or(false) {
            " · truncated at cap"
        } else {
            ""
        }
    )];
    for entry in entries.iter().take(200) {
        let safe = entry["safe"].as_bool().unwrap_or(false);
        lines.push(format!(
            "{}  {:<60}  {:>10} bytes{}",
            if safe { "safe  " } else { "UNSAFE" },
            entry["path"].as_str().unwrap_or("?"),
            entry["size_bytes"].as_u64().unwrap_or(0),
            entry["unsafe_reason"]
                .as_str()
                .map(|reason| format!(" · {reason}"))
                .unwrap_or_default()
        ));
    }
    if entries.len() > 200 {
        lines.push(format!("… {} more entries omitted", entries.len() - 200));
    }
    for warning in value["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            lines.push(format!("warning: {warning}"));
        }
    }
    Some(lines)
}

pub(crate) fn archive_extract_lines(value: &Value) -> Option<Vec<String>> {
    let entries = value.get("entries")?.as_array()?;
    value.get("extraction_id")?.as_str()?;
    let artifacts = value["published_artifacts"].as_array().map_or(0, Vec::len);
    let mut lines = vec![format!(
        "extracted {} entries ({} bytes) from {} · {} artifact(s) · scratch retained: {}",
        entries.len(),
        value["total_bytes"].as_u64().unwrap_or(0),
        value["format"].as_str().unwrap_or("?"),
        artifacts,
        value["scratch_retained"].as_bool().unwrap_or(false)
    )];
    for entry in entries.iter().take(200) {
        lines.push(format!(
            "{}  {} bytes{}",
            entry["path"].as_str().unwrap_or("?"),
            entry["size_bytes"].as_u64().unwrap_or(0),
            entry["published_artifact_id"]
                .as_str()
                .map(|id| format!(" · artifact {id}"))
                .or_else(|| {
                    entry["skipped_reason"]
                        .as_str()
                        .map(|reason| format!(" · skipped: {reason}"))
                })
                .unwrap_or_default()
        ));
        if let Some(preview) = entry["preview"].as_str() {
            let preview = preview
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>();
            lines.push(format!("    preview: {preview}"));
        }
    }
    Some(lines)
}

pub(crate) fn media_info_lines(value: &Value) -> Option<Vec<String>> {
    let info = value.get("info")?;
    let source = info.get("source")?.as_str()?;
    let mut lines = vec![format!(
        "{} · {} · {} · source {}",
        value["name"].as_str().unwrap_or("media"),
        info["container"].as_str().unwrap_or("unknown container"),
        value["size_bytes"].as_u64().unwrap_or(0),
        source
    )];
    if let Some(duration) = info["duration_secs"].as_f64() {
        lines.push(format!("duration: {duration:.2}s"));
    }
    if let (Some(width), Some(height)) = (info["width"].as_u64(), info["height"].as_u64()) {
        lines.push(format!("dimensions: {width}x{height}"));
    }
    for stream in info["streams"].as_array().into_iter().flatten() {
        lines.push(format!(
            "stream {}: {} {}{}{}",
            stream["index"].as_u64().unwrap_or(0),
            stream["codec_type"].as_str().unwrap_or("?"),
            stream["codec_name"].as_str().unwrap_or("?"),
            stream["width"]
                .as_u64()
                .zip(stream["height"].as_u64())
                .map(|(width, height)| format!(" {width}x{height}"))
                .unwrap_or_default(),
            stream["duration_secs"]
                .as_f64()
                .map(|duration| format!(" {duration:.2}s"))
                .unwrap_or_default()
        ));
    }
    for warning in info["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            lines.push(format!("warning: {warning}"));
        }
    }
    Some(lines)
}

pub(crate) fn media_transcode_lines(value: &Value) -> Option<Vec<String>> {
    let artifact = value.get("artifact")?;
    value.get("source_artifact_id")?.as_str()?;
    value.get("command_summary")?.as_str()?;
    let mut lines = vec![format!(
        "transcoded {} -> {} ({}) · artifact {} · {} bytes · {} ms",
        value["source_artifact_id"].as_str().unwrap_or("?"),
        value["format"].as_str().unwrap_or("?"),
        artifact["mime_type"].as_str().unwrap_or("?"),
        artifact["id"].as_str().unwrap_or("?"),
        value["size_bytes"].as_u64().unwrap_or(0),
        value["elapsed_ms"].as_u64().unwrap_or(0)
    )];
    lines.push(format!(
        "bounds: max {}s · max {} bytes · command {}",
        value["max_duration_secs"].as_u64().unwrap_or(0),
        value["max_bytes"].as_u64().unwrap_or(0),
        value["command_summary"].as_str().unwrap_or("?")
    ));
    if let Some(provenance) = artifact.get("provenance") {
        lines.push(format!(
            "provenance: {} · source {} · output {}",
            provenance["operation"].as_str().unwrap_or("?"),
            provenance["source_artifact_id"].as_str().unwrap_or("?"),
            provenance["output_format"].as_str().unwrap_or("?")
        ));
    }
    for warning in value["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            lines.push(format!("warning: {warning}"));
        }
    }
    Some(lines)
}
