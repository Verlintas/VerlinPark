//! Human renderer chunk: sources.

use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(collections) = value.get("collections").and_then(Value::as_array)
        && value.get("total_sources").is_some()
    {
        println!("{} sources", value["total_sources"].as_u64().unwrap_or(0));
        for collection in collections {
            println!(
                "{:<24} {:>4} sources {:>5} chunks {:>8} bytes {:>8} tokens  labels={}",
                collection["name"].as_str().unwrap_or("(uncollected)"),
                collection["sources"].as_u64().unwrap_or(0),
                collection["chunks"].as_u64().unwrap_or(0),
                collection["bytes"].as_u64().unwrap_or(0),
                collection["tokens"].as_u64().unwrap_or(0),
                collection["labels"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        return true;
    }
    if let Some(sources) = value.get("sources").and_then(Value::as_array)
        && value.get("stats").is_some()
    {
        let stats = &value["stats"];
        println!(
            "{} sources · {} indexed · {} failed · {} chunks · {} bytes · {} tokens",
            stats["sources"].as_u64().unwrap_or(0),
            stats["indexed"].as_u64().unwrap_or(0),
            stats["failed"].as_u64().unwrap_or(0),
            stats["chunks"].as_u64().unwrap_or(0),
            stats["bytes"].as_u64().unwrap_or(0),
            stats["tokens"].as_u64().unwrap_or(0),
        );
        for source in sources {
            println!(
                "{:<10} {:<8} {:>5} chunks  {}  ({})",
                source["status"].as_str().unwrap_or("?"),
                source["kind"].as_str().unwrap_or("?"),
                source["chunk_count"].as_u64().unwrap_or(0),
                source["title"].as_str().unwrap_or(""),
                source["id"].as_str().unwrap_or("?"),
            );
        }
        return true;
    }
    if let Some(source) = value.get("source")
        && let Some(chunks) = value.get("chunks").and_then(Value::as_array)
    {
        println!(
            "{} · {} · {} chunks · {}",
            source["kind"].as_str().unwrap_or("?"),
            source["status"].as_str().unwrap_or("?"),
            source["chunk_count"].as_u64().unwrap_or(0),
            source["title"].as_str().unwrap_or(""),
        );
        println!("id={}", source["id"].as_str().unwrap_or("?"));
        if let Some(uri) = source["uri"].as_str() {
            println!("uri={uri}");
        }
        for chunk in chunks {
            println!(
                "\n[{}] {}",
                chunk["chunk_no"].as_u64().unwrap_or(0),
                chunk["heading"].as_str().unwrap_or("untitled section")
            );
            println!(
                "{}",
                chunk["content"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1200)
                    .collect::<String>()
            );
        }
        return true;
    }
    if let Some(hits) = value.get("hits").and_then(Value::as_array)
        && value.get("degraded").is_some()
    {
        if hits.is_empty() {
            println!("No source matches.");
            return true;
        }
        for hit in hits {
            println!(
                "{} chunk {} · {:.2} · {}",
                hit["source_title"].as_str().unwrap_or("?"),
                hit["chunk_no"].as_u64().unwrap_or(0),
                hit["score"].as_f64().unwrap_or(0.0),
                hit["heading"].as_str().unwrap_or("")
            );
            println!(
                "  {}",
                hit["content"]
                    .as_str()
                    .unwrap_or("")
                    .replace('\n', " ")
                    .chars()
                    .take(240)
                    .collect::<String>()
            );
        }
        if value["degraded"].as_bool() == Some(true) {
            println!("(lexical fallback)");
        }
        if let Some(mode) = value["mode"].as_str().filter(|mode| !mode.is_empty()) {
            println!("mode={mode}");
        }
        if let Some(reason) = value["semantic_unavailable"].as_str() {
            println!("(semantic unavailable: {reason})");
        }
        return true;
    }
    if value.get("indexable_chunks").is_some() && value.get("pending_jobs").is_some() {
        println!(
            "source embeddings · enabled={} configured={} · model={} · {} dims · coverage {:.0}%",
            value["enabled"].as_bool().unwrap_or(false),
            value["configured"].as_bool().unwrap_or(false),
            value["model"].as_str().unwrap_or("-"),
            value["dimensions"].as_u64().unwrap_or(0),
            value["coverage"].as_f64().unwrap_or(0.0) * 100.0,
        );
        println!(
            "chunks {} indexed · {} stale · {} invalid · cap {} · retention {}d",
            value["indexable_chunks"].as_u64().unwrap_or(0),
            value["stale_chunks"].as_u64().unwrap_or(0),
            value["invalid_chunks"].as_u64().unwrap_or(0),
            value["max_chunks_per_source"].as_u64().unwrap_or(0),
            value["retention_days"].as_u64().unwrap_or(0),
        );
        println!(
            "jobs {} pending · {} running · {} failed · {} dead · {} done",
            value["pending_jobs"].as_u64().unwrap_or(0),
            value["running_jobs"].as_u64().unwrap_or(0),
            value["failed_jobs"].as_u64().unwrap_or(0),
            value["dead_jobs"].as_u64().unwrap_or(0),
            value["done_jobs"].as_u64().unwrap_or(0),
        );
        let usage = &value["usage"];
        println!(
            "usage {} calls · {} input tokens ({} estimated) · {} bytes",
            usage["calls"].as_u64().unwrap_or(0),
            usage["input_tokens"].as_u64().unwrap_or(0),
            usage["estimated_tokens"].as_u64().unwrap_or(0),
            usage["bytes"].as_u64().unwrap_or(0),
        );
        if let Some(env) = value["endpoint_env"].as_str() {
            println!(
                "endpoint_env={env} present={}",
                value["endpoint_env_present"].as_bool().unwrap_or(false)
            );
        }
        if let Some(reason) = value["semantic_unavailable"].as_str() {
            println!("semantic unavailable: {reason}");
        }
        if let Some(error) = value["last_error"].as_str() {
            println!("last error: {error}");
        }
        for job in value["recent_jobs"].as_array().into_iter().flatten() {
            println!(
                "  {:<7} {}/{} chunks · attempts {} · {}",
                job["status"].as_str().unwrap_or("?"),
                job["chunks_done"].as_u64().unwrap_or(0),
                job["chunks_total"].as_u64().unwrap_or(0),
                job["attempts"].as_u64().unwrap_or(0),
                job["source_title"].as_str().unwrap_or("?"),
            );
        }
        return true;
    }
    if value.get("enqueued_sources").is_some() && value.get("cleared_vectors").is_some() {
        println!(
            "source embedding reindex · enabled={} · cleared {} vectors / {} jobs · queued {} sources",
            value["enabled"].as_bool().unwrap_or(false),
            value["cleared_vectors"].as_u64().unwrap_or(0),
            value["cleared_jobs"].as_u64().unwrap_or(0),
            value["enqueued_sources"].as_u64().unwrap_or(0),
        );
        return true;
    }
    if let Some(policy) = value.get("policy")
        && policy.get("max_chunks_per_source").is_some()
    {
        println!(
            "source embedding policy · enabled={} · model={} · endpoint_env={} · {} dims",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["model"].as_str().unwrap_or("-"),
            policy["endpoint_env"].as_str().unwrap_or("-"),
            policy["dimensions"].as_u64().unwrap_or(0),
        );
        println!(
            "batch {} · max chunks {} · retention {}d · timeout {}s · api_key_env={}",
            policy["batch_size"].as_u64().unwrap_or(0),
            policy["max_chunks_per_source"].as_u64().unwrap_or(0),
            policy["retention_days"].as_u64().unwrap_or(0),
            policy["timeout_secs"].as_u64().unwrap_or(0),
            policy["api_key_env"].as_str().unwrap_or("-"),
        );
        return true;
    }
    if let Some(imported) = value.get("imported").and_then(Value::as_array)
        && value.get("skipped").is_some()
    {
        println!(
            "imported {} · skipped {} · errors {}",
            imported.len(),
            value["skipped"].as_u64().unwrap_or(0),
            value["errors"].as_array().map(Vec::len).unwrap_or(0)
        );
        for source in imported {
            println!(
                "  {} ({})",
                source["title"].as_str().unwrap_or("?"),
                source["id"].as_str().unwrap_or("?")
            );
        }
        for error in value["errors"].as_array().into_iter().flatten() {
            println!(
                "  error {}: {}",
                error["path"].as_str().unwrap_or("?"),
                error["error"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if let Some(source) = value.get("title").and_then(Value::as_str)
        && value.get("chunk_count").is_some()
        && value.get("sha256").is_some()
    {
        println!(
            "{} · {} · {} chunks · {} · id={}",
            value["status"].as_str().unwrap_or("?"),
            source,
            value["chunk_count"].as_u64().unwrap_or(0),
            value["kind"].as_str().unwrap_or("?"),
            value["id"].as_str().unwrap_or("?")
        );
        return true;
    }
    if let Some(matches) = value.get("matches").and_then(Value::as_array)
        && value.get("files_scanned").is_some()
    {
        if matches.is_empty() {
            println!("No matches.");
            return true;
        }
        for item in matches {
            println!(
                "{}:{}:{}  {}",
                item["path"].as_str().unwrap_or("?"),
                item["line"].as_u64().unwrap_or(0),
                item["column"].as_u64().unwrap_or(0),
                item["preview"].as_str().unwrap_or("")
            );
        }
        println!(
            "{} matches across {} files{}",
            matches.len(),
            value["files_scanned"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool() == Some(true) {
                " (truncated)"
            } else {
                ""
            }
        );
        return true;
    }
    if let Some(deleted) = value.get("deleted").and_then(Value::as_bool) {
        println!("{}", if deleted { "deleted." } else { "not deleted." });
        return true;
    }
    false
}
