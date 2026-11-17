//! Human-readable, JSON, and CSV renderers for CLI command results.

use anyhow::Result;
use serde_json::{Value, json};

pub(crate) fn print_repository_backup(value: &Value) {
    println!(
        "snapshot {}  sequence {}  parent {}",
        value["snapshot_id"].as_str().unwrap_or("?"),
        value["sequence"].as_u64().unwrap_or(0),
        value["parent"].as_str().unwrap_or("-")
    );
    println!(
        "  {}  encrypted={}  incremental={}  artifacts={}  compression={}",
        value["repository"].as_str().unwrap_or("?"),
        value["encrypted"].as_bool().unwrap_or(false),
        value["incremental"].as_bool().unwrap_or(false),
        value["includes_artifacts"].as_bool().unwrap_or(false),
        value["compression"].as_str().unwrap_or("?")
    );
    println!(
        "  entries {} ({} sessions, {} artifacts)  logical {} bytes",
        value["entries"].as_u64().unwrap_or(0),
        value["sessions"].as_u64().unwrap_or(0),
        value["artifacts"].as_u64().unwrap_or(0),
        value["bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  written {} bytes  deduplicated {} bytes ({} parent, {} store)  stored blob bytes {}",
        value["written_bytes"].as_u64().unwrap_or(0),
        value["deduplicated_bytes"].as_u64().unwrap_or(0),
        value["deduplicated_from_parent_bytes"]
            .as_u64()
            .unwrap_or(0),
        value["deduplicated_from_store_bytes"].as_u64().unwrap_or(0),
        value["stored_bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  blobs written {}  reused {}  manifest {} bytes  sha256 {}",
        value["blobs_written"].as_u64().unwrap_or(0),
        value["blobs_reused"].as_u64().unwrap_or(0),
        value["manifest_bytes"].as_u64().unwrap_or(0),
        value["manifest_sha256"].as_str().unwrap_or("?")
    );
    println!(
        "  signed={}  key={}  algorithm={}  anchor generation {}",
        value["signed"].as_bool().unwrap_or(false),
        value["signing_key_id"].as_str().unwrap_or("-"),
        value["signature_algorithm"].as_str().unwrap_or("-"),
        value["anchor_update"]["highest_generation"]
            .as_u64()
            .unwrap_or(0)
    );
}

pub(crate) fn print_repository_restore(value: &Value) {
    println!(
        "restored {}  sequence {}  -> {}",
        value["snapshot_id"].as_str().unwrap_or("?"),
        value["sequence"].as_u64().unwrap_or(0),
        value["output"].as_str().unwrap_or("?")
    );
    println!(
        "  entries {} ({} sessions, {} artifacts)  {} bytes  encrypted={}  replaced_existing={}",
        value["entries"].as_u64().unwrap_or(0),
        value["sessions"].as_u64().unwrap_or(0),
        value["artifacts"].as_u64().unwrap_or(0),
        value["bytes"].as_u64().unwrap_or(0),
        value["encrypted"].as_bool().unwrap_or(false),
        value["replaced_existing"].as_bool().unwrap_or(false)
    );
    println!(
        "  validated blobs {}  stored blob bytes {}",
        value["validated_blobs"].as_u64().unwrap_or(0),
        value["stored_blob_bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  signed={}  key={}  signature={}  anchor highest={}",
        value["signed"].as_bool().unwrap_or(false),
        value["signing_key_id"].as_str().unwrap_or("-"),
        value["signature_status"].as_str().unwrap_or("-"),
        value["anchor_update"]["highest_generation"]
            .as_u64()
            .unwrap_or(0)
    );
    if value["rollback"]["rollback"].as_bool().unwrap_or(false) {
        println!(
            "  rollback accepted explicitly: restored generation {} below anchor {}",
            value["rollback"]["generation"].as_u64().unwrap_or(0),
            value["rollback"]["anchor_generation"].as_u64().unwrap_or(0)
        );
    }
    if let Some(issues) = value["head_issues"].as_array() {
        for issue in issues {
            println!("  signed-head issue: {}", issue.as_str().unwrap_or("?"));
        }
    }
    if let Some(warnings) = value["revoked_signature_warnings"].as_array() {
        for warning in warnings {
            println!("  revoked-key warning: {}", warning.as_str().unwrap_or("?"));
        }
    }
}

pub(crate) fn print_repository_list(value: &Value) {
    println!(
        "{}  {} snapshots  encrypted={}  blobs={}  logical {} bytes",
        value["repository"].as_str().unwrap_or("?"),
        value["snapshot_count"].as_u64().unwrap_or(0),
        value["encrypted"].as_bool().unwrap_or(false),
        value["unique_blobs"].as_u64().unwrap_or(0),
        value["logical_bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  signed={}  status={}  format v{}  active key={}",
        value["signed"].as_bool().unwrap_or(false),
        value["signing_status"].as_str().unwrap_or("unsigned"),
        value["format_version"].as_u64().unwrap_or(0),
        value["keyring"]["active_key_id"].as_str().unwrap_or("-")
    );
    let validation = &value["validation"];
    println!(
        "  validation valid={}  blobs present={} missing={} verified={} corrupt={}  mode={}",
        validation["valid"].as_bool().unwrap_or(false),
        validation["blobs_present"].as_u64().unwrap_or(0),
        validation["blobs_missing"].as_u64().unwrap_or(0),
        validation["blobs_verified"].as_u64().unwrap_or(0),
        validation["blobs_corrupt"].as_u64().unwrap_or(0),
        validation["blob_verification"].as_str().unwrap_or("?")
    );
    if validation["revoked_signatures"].as_u64().unwrap_or(0) > 0 {
        println!(
            "  revoked-key signatures accepted: {}  keys {}",
            validation["revoked_signatures"].as_u64().unwrap_or(0),
            validation["revoked_key_ids"]
                .as_array()
                .map(|keys| {
                    keys.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        );
    }
    if let Some(issues) = validation["issues"].as_array()
        && !issues.is_empty()
    {
        for issue in issues {
            println!("  issue: {}", issue.as_str().unwrap_or("?"));
        }
    }
    if let Some(snapshots) = value["snapshots"].as_array() {
        for snapshot in snapshots {
            println!(
                "{}  seq {:<4} parent {:<36} {:<8} {} bytes  {} entries  signed={} key={}  sha256 {}",
                snapshot["snapshot_id"].as_str().unwrap_or("?"),
                snapshot["sequence"].as_u64().unwrap_or(0),
                snapshot["parent"].as_str().unwrap_or("-"),
                if snapshot["incremental"].as_bool().unwrap_or(false) {
                    "incr"
                } else {
                    "full"
                },
                snapshot["bytes"].as_u64().unwrap_or(0),
                snapshot["entries"].as_u64().unwrap_or(0),
                snapshot["signed"].as_bool().unwrap_or(false),
                snapshot["signing_key_id"].as_str().unwrap_or("-"),
                snapshot["manifest_sha256"].as_str().unwrap_or("?")
            );
        }
    }
}

pub(crate) fn print_repository_prune(value: &Value) {
    println!(
        "{}  keep {}  dry_run={}  applied={}",
        value["repository"].as_str().unwrap_or("?"),
        value["keep"].as_u64().unwrap_or(0),
        value["dry_run"].as_bool().unwrap_or(false),
        value["applied"].as_bool().unwrap_or(false)
    );
    println!(
        "  snapshots total {}  kept {}  pruned {}  blobs kept {}  pruned {}",
        value["snapshots_total"].as_u64().unwrap_or(0),
        value["snapshots_kept"].as_u64().unwrap_or(0),
        value["snapshots_pruned"]
            .as_array()
            .map(|entries| entries.len())
            .unwrap_or(0),
        value["blobs_kept"].as_u64().unwrap_or(0),
        value["blobs_pruned"].as_u64().unwrap_or(0)
    );
    println!(
        "  reclaimed {} bytes  protected {} bytes",
        value["reclaimed_bytes"].as_u64().unwrap_or(0),
        value["protected_bytes"].as_u64().unwrap_or(0)
    );
    println!(
        "  signed={}  key={}",
        value["signed"].as_bool().unwrap_or(false),
        value["signing_key_id"].as_str().unwrap_or("-")
    );
    if let Some(snapshots) = value["snapshots_pruned"].as_array() {
        for snapshot in snapshots {
            println!(
                "  prune {}  seq {}  {} bytes",
                snapshot["snapshot_id"].as_str().unwrap_or("?"),
                snapshot["sequence"].as_u64().unwrap_or(0),
                snapshot["bytes"].as_u64().unwrap_or(0)
            );
        }
    }
}

pub(crate) fn print_repository_verify(value: &Value) {
    println!(
        "{}  format v{}  generation {}  snapshots {}",
        value["repository"].as_str().unwrap_or("?"),
        value["format_version"].as_u64().unwrap_or(0),
        value["generation"].as_u64().unwrap_or(0),
        value["snapshot_count"].as_u64().unwrap_or(0)
    );
    println!(
        "  valid={}  authenticity={}  signed={}  verified snapshots {}  unsigned {}",
        value["valid"].as_bool().unwrap_or(false),
        value["authenticity"].as_str().unwrap_or("?"),
        value["signed"].as_bool().unwrap_or(false),
        value["signed_snapshots"].as_u64().unwrap_or(0),
        value["unsigned_snapshots"].as_u64().unwrap_or(0)
    );
    println!(
        "  metadata signature={} key={}  head_consistent={}",
        value["metadata_signature"]["status"]
            .as_str()
            .unwrap_or("?"),
        value["metadata_signature"]["key_id"]
            .as_str()
            .unwrap_or("-"),
        value["signed_head_consistent"].as_bool().unwrap_or(false)
    );
    println!(
        "  chain_valid={}  tombstones_valid={}  entries_valid={}  blobs present={} missing={} verified={} corrupt={}",
        value["chain_valid"].as_bool().unwrap_or(false),
        value["tombstones_valid"].as_bool().unwrap_or(false),
        value["entries_valid"].as_bool().unwrap_or(false),
        value["blobs_present"].as_u64().unwrap_or(0),
        value["blobs_missing"].as_u64().unwrap_or(0),
        value["blobs_verified"].as_u64().unwrap_or(0),
        value["blobs_corrupt"].as_u64().unwrap_or(0)
    );
    if value["rollback"].is_object() {
        println!(
            "  anchor generation={}  accepted={}  rollback={}  allow_rollback={}",
            value["rollback"]["anchor_generation"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            value["rollback"]["accepted"].as_bool().unwrap_or(false),
            value["rollback"]["rollback"].as_bool().unwrap_or(false),
            value["rollback"]["allow_rollback"]
                .as_bool()
                .unwrap_or(false)
        );
    }
    if let Some(issues) = value["issues"].as_array() {
        for issue in issues {
            println!("  issue: {}", issue.as_str().unwrap_or("?"));
        }
    }
    if let Some(issues) = value["head_issues"].as_array() {
        for issue in issues {
            println!("  signed-head issue: {}", issue.as_str().unwrap_or("?"));
        }
    }
    if let Some(warnings) = value["revoked_signature_warnings"].as_array() {
        for warning in warnings {
            println!("  revoked-key warning: {}", warning.as_str().unwrap_or("?"));
        }
    }
}

pub(crate) fn print_backup_key_generation(value: &Value) {
    println!(
        "key {}  active={}  algorithm={}  fingerprint {}",
        value["key_id"].as_str().unwrap_or("?"),
        value["active"].as_bool().unwrap_or(false),
        value["algorithm"].as_str().unwrap_or("?"),
        value["fingerprint"].as_str().unwrap_or("?")
    );
    println!(
        "  public key {}",
        value["public_key_b64"].as_str().unwrap_or("?")
    );
    println!(
        "  keyring {}  source {}",
        value["keyring"].as_str().unwrap_or("?"),
        value["source"].as_str().unwrap_or("?")
    );
}

pub(crate) fn print_backup_key_rotation(value: &Value) {
    println!(
        "active key {}  previous {}  changed={}",
        value["key_id"].as_str().unwrap_or("?"),
        value["previous_key_id"].as_str().unwrap_or("-"),
        value["changed"].as_bool().unwrap_or(false)
    );
    let retired = value["retired_public_keys"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    println!(
        "  retired public keys retained for verification: {}",
        if retired.is_empty() { "-" } else { &retired }
    );
    println!("  keyring {}", value["keyring"].as_str().unwrap_or("?"));
}

pub(crate) fn print_backup_key_revocation(value: &Value) {
    println!(
        "revoked key {}  at {}  changed={}  was_active={}",
        value["key_id"].as_str().unwrap_or("?"),
        value["revoked_at"].as_str().unwrap_or("?"),
        value["changed"].as_bool().unwrap_or(false),
        value["was_active"].as_bool().unwrap_or(false)
    );
    println!(
        "  active key now: {}",
        value["active_key_id"].as_str().unwrap_or("(none)")
    );
    if let Some(warning) = value["warning"].as_str() {
        println!("  WARNING: {warning}");
    }
    println!(
        "  public key retained for historical verification; keyring {}",
        value["keyring"].as_str().unwrap_or("?")
    );
}

pub(crate) fn print_repository_rekey(value: &Value) {
    println!(
        "{}  repo {}  format v{}  dry_run={}  applied={}  changed={}",
        value["repository"].as_str().unwrap_or("?"),
        value["repository_id"].as_str().unwrap_or("?"),
        value["format_version"].as_u64().unwrap_or(0),
        value["dry_run"].as_bool().unwrap_or(false),
        value["applied"].as_bool().unwrap_or(false),
        value["changed"].as_bool().unwrap_or(false)
    );
    println!(
        "  key {} -> {}  metadata_resigned={}",
        value["previous_key_id"].as_str().unwrap_or("?"),
        value["key_id"].as_str().unwrap_or("?"),
        value["metadata_resigned"].as_bool().unwrap_or(false)
    );
    println!(
        "  snapshots {}  resigned {}  unchanged {}  unsigned preserved {}",
        value["snapshots_total"].as_u64().unwrap_or(0),
        value["snapshots_resigned"].as_u64().unwrap_or(0),
        value["snapshots_unchanged"].as_u64().unwrap_or(0),
        value["snapshots_unsigned_preserved"].as_u64().unwrap_or(0)
    );
    println!(
        "  generation {}  latest {}  chain_preserved={}  entries_preserved={}  tombstones_preserved={}",
        value["generation"].as_u64().unwrap_or(0),
        value["latest_snapshot_id"].as_str().unwrap_or("-"),
        value["parent_chain_preserved"].as_bool().unwrap_or(false),
        value["entries_preserved"].as_bool().unwrap_or(false),
        value["tombstones_preserved"].as_bool().unwrap_or(false)
    );
    println!(
        "  blobs {}  {} bytes  ciphertext_unchanged={}  ciphertext_sha256 {}",
        value["blobs_total"].as_u64().unwrap_or(0),
        value["blob_bytes"].as_u64().unwrap_or(0),
        value["blob_ciphertext_unchanged"]
            .as_bool()
            .unwrap_or(false),
        value["blob_ciphertext_sha256"].as_str().unwrap_or("?")
    );
    if let Some(revoked) = value["revoked_source_key_ids"].as_array()
        && !revoked.is_empty()
    {
        println!(
            "  replaced signatures from revoked key(s): {}",
            revoked
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

pub(crate) fn print_portable_anchor_export(value: &Value) {
    println!(
        "exported portable anchor  key {}  {} repositories  {}",
        value["key_id"].as_str().unwrap_or("?"),
        value["repository_count"].as_u64().unwrap_or(0),
        value["path"].as_str().unwrap_or("(stdout)")
    );
    println!(
        "  exported_at {}  document_sha256 {}",
        value["exported_at"].as_str().unwrap_or("?"),
        value["document_sha256"].as_str().unwrap_or("?")
    );
}

pub(crate) fn print_portable_anchor_verify(value: &Value) {
    println!(
        "{}  key {}  exported {}  signature {}",
        value["anchor_path"].as_str().unwrap_or("?"),
        value["key_id"].as_str().unwrap_or("?"),
        value["exported_at"].as_str().unwrap_or("?"),
        value["signature"].as_str().unwrap_or("?")
    );
    println!(
        "  repositories {}  would raise {}  unchanged {}  stale_export={}  signer_revoked={}",
        value["repositories_total"].as_u64().unwrap_or(0),
        value["would_raise"].as_u64().unwrap_or(0),
        value["unchanged"].as_u64().unwrap_or(0),
        value["stale_export"].as_bool().unwrap_or(false),
        value["signer_revoked"].as_bool().unwrap_or(false)
    );
    if let Some(raises) = value["raises"].as_array() {
        for raise in raises {
            println!(
                "  {}  highest {} -> {}  last verified {} -> {}  would_raise={}",
                raise["repository_id"].as_str().unwrap_or("?"),
                raise["previous_highest"]
                    .as_u64()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                raise["highest_generation"].as_u64().unwrap_or(0),
                raise["previous_last_verified"]
                    .as_u64()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                raise["last_verified_generation"].as_u64().unwrap_or(0),
                raise["raised"].as_bool().unwrap_or(false)
            );
        }
    }
    println!(
        "  document_sha256 {}",
        value["document_sha256"].as_str().unwrap_or("?")
    );
}

pub(crate) fn print_portable_anchor_import(value: &Value) {
    println!(
        "{}  key {}  exported {}  stale_export={}",
        value["anchor_path"].as_str().unwrap_or("?"),
        value["key_id"].as_str().unwrap_or("?"),
        value["exported_at"].as_str().unwrap_or("?"),
        value["stale_export"].as_bool().unwrap_or(false)
    );
    println!(
        "  repositories {}  raised {}  unchanged {}",
        value["repositories_total"].as_u64().unwrap_or(0),
        value["repositories_raised"].as_u64().unwrap_or(0),
        value["repositories_unchanged"].as_u64().unwrap_or(0)
    );
    if let Some(raises) = value["raises"].as_array() {
        for raise in raises {
            println!(
                "  {}  highest {} -> {}  last verified {} -> {}  raised={}",
                raise["repository_id"].as_str().unwrap_or("?"),
                raise["previous_highest"]
                    .as_u64()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                raise["highest_generation"].as_u64().unwrap_or(0),
                raise["previous_last_verified"]
                    .as_u64()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                raise["last_verified_generation"].as_u64().unwrap_or(0),
                raise["raised"].as_bool().unwrap_or(false)
            );
        }
    }
    println!(
        "  document_sha256 {}",
        value["document_sha256"].as_str().unwrap_or("?")
    );
}

pub(crate) fn print_backup_key_events(value: &Value) {
    println!(
        "{}  present={}  {} events{}",
        value["path"].as_str().unwrap_or("?"),
        value["present"].as_bool().unwrap_or(false),
        value["event_count"].as_u64().unwrap_or(0),
        if value["truncated"].as_bool().unwrap_or(false) {
            " (truncated to the requested limit)"
        } else {
            ""
        }
    );
    if let Some(events) = value["events"].as_array() {
        for event in events {
            println!(
                "  {}  {}  {}",
                event["created_at"].as_str().unwrap_or("?"),
                event["event"].as_str().unwrap_or("?"),
                compact_json(&event["details"])
            );
        }
    }
}

pub(crate) fn print_backup_keyring(value: &Value) {
    println!(
        "{}  {} keys  active={}",
        value["path"].as_str().unwrap_or("?"),
        value["key_count"].as_u64().unwrap_or(0),
        value["active_key_id"].as_str().unwrap_or("-")
    );
    if let Some(keys) = value["keys"].as_array() {
        for key in keys {
            println!(
                "  {}  {}  active={}  retired={}  revoked={}  public {}",
                key["key_id"].as_str().unwrap_or("?"),
                key["algorithm"].as_str().unwrap_or("?"),
                key["active"].as_bool().unwrap_or(false),
                key["retired_at"].as_str().unwrap_or("-"),
                key["revoked_at"].as_str().unwrap_or("-"),
                key["public_key_b64"].as_str().unwrap_or("?")
            );
        }
    }
    if value["key_count"].as_u64().unwrap_or(0) == 0 {
        println!("  no signing keys yet; run `vp backup-key-generate <key-id>`");
    }
}

pub(crate) fn print_backup_anchor(value: &Value) {
    println!(
        "{}  present={}  updated={}",
        value["path"].as_str().unwrap_or("?"),
        value["present"].as_bool().unwrap_or(false),
        value["updated_at"].as_str().unwrap_or("-")
    );
    if let Some(repositories) = value["repositories"].as_array() {
        for repository in repositories {
            println!(
                "  {}  highest generation {}  last verified {}  operation {}  origin {}  at {}",
                repository["repository_id"].as_str().unwrap_or("?"),
                repository["highest_generation"].as_u64().unwrap_or(0),
                repository["last_verified_generation"].as_u64().unwrap_or(0),
                repository["last_operation"].as_str().unwrap_or("?"),
                repository["origin"].as_str().unwrap_or("local"),
                repository["last_verified_at"].as_str().unwrap_or("?")
            );
        }
    }
}

pub(crate) fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Short component-hash display for cache status/plan output. Full hashes stay
/// available in `--json`; the human view only needs a stable visual prefix.
fn short_hash(value: &Value) -> String {
    value
        .as_str()
        .map(|hash| hash.chars().take(12).collect::<String>() + "…")
        .unwrap_or_else(|| "—".to_string())
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn print_program_line(program: &Value) {
    let id = program["id"].as_str().unwrap_or("?");
    let status = program["status"].as_str().unwrap_or("?");
    let goal = program["goal"].as_str().unwrap_or("");
    println!("{id}  {status:<10}  {goal}");
    let mut budget = Vec::new();
    if let Some(tokens) = program["token_budget"].as_u64() {
        budget.push(format!("{tokens} tokens"));
    }
    if let Some(cost) = program["cost_budget_usd"].as_f64() {
        budget.push(format!("${cost:.2}"));
    }
    if !budget.is_empty() {
        println!("  budget {}", budget.join(" / "));
    }
    if let Some(reason) = program["reason"].as_str() {
        println!("  {reason}");
    }
}

/// Human rendering for the durable context checkpoint surface. Returns
/// `None` for every other RPC shape so the generic printer owns them.
pub(crate) fn tool_activity_csv(value: &Value) -> String {
    const COLUMNS: &[&str] = &[
        "seq",
        "created_at",
        "tool_call_id",
        "name",
        "status",
        "error_code",
        "retryable",
        "duration_ms",
        "output_bytes",
        "truncated",
        "approval_risk",
        "approval_mode",
        "approval_wait_ms",
        "hint",
    ];
    let mut output = COLUMNS.join(",");
    output.push('\n');
    let Some(entries) = value.get("entries").and_then(Value::as_array) else {
        return output;
    };
    for entry in entries {
        let status = if entry["is_error"].as_bool().unwrap_or(false) {
            entry["error_code"].as_str().unwrap_or("TOOL_FAILED")
        } else {
            "ok"
        };
        let row = [
            entry["seq"].as_u64().unwrap_or(0).to_string(),
            entry["created_at"].as_str().unwrap_or("").to_string(),
            entry["tool_call_id"].as_str().unwrap_or("").to_string(),
            entry["name"].as_str().unwrap_or("").to_string(),
            status.to_string(),
            entry["error_code"].as_str().unwrap_or("").to_string(),
            entry["retryable"].as_bool().unwrap_or(false).to_string(),
            entry["duration_ms"].as_u64().unwrap_or(0).to_string(),
            entry["output_bytes"].as_u64().unwrap_or(0).to_string(),
            entry["truncated"].as_bool().unwrap_or(false).to_string(),
            entry["approval_risk"].as_str().unwrap_or("").to_string(),
            entry["approval_mode"].as_str().unwrap_or("").to_string(),
            entry["approval_wait_ms"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_default(),
            entry["hint"].as_str().unwrap_or("").to_string(),
        ];
        output.push_str(
            &row.iter()
                .map(|cell| csv_cell(cell))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push('\n');
    }
    output
}

fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub(crate) fn filtered_tool_codes(
    category: Option<&str>,
    retryable: Option<bool>,
) -> Vec<vp_core::tooling::errors::ToolErrorCatalogEntry> {
    let mut entries = vp_core::tooling::errors::catalog();
    if let Some(category) = category {
        let category = category.trim().to_ascii_lowercase();
        entries.retain(|entry| entry.category == category);
    }
    if let Some(retryable) = retryable {
        entries.retain(|entry| entry.retryable == retryable);
    }
    entries.sort_by(|left, right| {
        left.category
            .cmp(right.category)
            .then_with(|| left.code.as_str().cmp(right.code.as_str()))
    });
    entries
}

pub(crate) fn tool_activity_lines(value: &Value) -> Option<Vec<String>> {
    let entries = value.get("entries")?.as_array()?;
    let summary = value.get("summary")?;
    let calls = summary.get("calls")?.as_u64()?;
    value.get("total").and_then(Value::as_u64)?;
    if !entries
        .iter()
        .all(|entry| entry.get("duration_ms").is_some())
    {
        return None;
    }
    let mut lines = vec![format!(
        "{} tool call(s) · {} error(s) · {} retryable · avg {} ms · max {} ms · {} output bytes{}",
        calls,
        summary["errors"].as_u64().unwrap_or(0),
        summary["retryable_errors"].as_u64().unwrap_or(0),
        summary["average_duration_ms"].as_u64().unwrap_or(0),
        summary["max_duration_ms"].as_u64().unwrap_or(0),
        summary["output_bytes"].as_u64().unwrap_or(0),
        if value["truncated"].as_bool().unwrap_or(false) {
            " · older events omitted"
        } else {
            ""
        }
    )];
    if entries.is_empty() {
        lines.push("No tool activity recorded for this session.".to_string());
        return Some(lines);
    }
    for entry in entries {
        let status = if entry["is_error"].as_bool().unwrap_or(false) {
            entry["error_code"].as_str().unwrap_or("ERROR")
        } else {
            "ok"
        };
        let retry = if entry["is_error"].as_bool().unwrap_or(false) {
            if entry["retryable"].as_bool().unwrap_or(false) {
                " · retryable"
            } else {
                ""
            }
        } else {
            ""
        };
        let truncated = if entry["truncated"].as_bool().unwrap_or(false) {
            " · truncated"
        } else {
            ""
        };
        lines.push(format!(
            "{}  {:<18} {:<9} {:>6} ms  {:>8} bytes{}{}",
            entry["created_at"].as_str().unwrap_or("?"),
            entry["name"].as_str().unwrap_or("unknown"),
            status,
            entry["duration_ms"].as_u64().unwrap_or(0),
            entry["output_bytes"].as_u64().unwrap_or(0),
            truncated,
            retry
        ));
        if let Some(hint) = entry["hint"].as_str().filter(|hint| !hint.is_empty()) {
            lines.push(format!("    hint: {hint}"));
        }
    }
    Some(lines)
}

pub(crate) fn context_checkpoint_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(checkpoints) = value.get("checkpoints").and_then(Value::as_array)
        && value.get("latest_id").is_some()
    {
        let mut lines = Vec::new();
        lines.push(format!(
            "context checkpoints for {} ({} shown, latest {})",
            value["session_id"].as_str().unwrap_or("?"),
            checkpoints.len(),
            value["latest_id"].as_str().unwrap_or("none")
        ));
        let policy = &value["policy"];
        lines.push(format!(
            "retention: sweep {} · max {} · age {}d",
            if policy["enabled"].as_bool().unwrap_or(false) {
                "on"
            } else {
                "off"
            },
            policy["max_checkpoints"].as_u64().unwrap_or(20),
            policy["max_age_days"].as_u64().unwrap_or(30)
        ));
        for checkpoint in checkpoints {
            let before = checkpoint["tokens_before"].as_u64().unwrap_or(0);
            let after = checkpoint["tokens_after"].as_u64().unwrap_or(0);
            let delta = before.saturating_sub(after);
            let summary = checkpoint["summary"]
                .as_str()
                .or_else(|| checkpoint["label"].as_str())
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let preview = if summary.chars().count() > 80 {
                format!("{}…", summary.chars().take(80).collect::<String>())
            } else {
                summary
            };
            lines.push(format!(
                "{}  {:<8} rev {:<3} {:>7}→{:<7} −{:<7} {}  {}",
                checkpoint["id"].as_str().unwrap_or("?"),
                checkpoint["mode"].as_str().unwrap_or("?"),
                checkpoint["plan_revision"].as_u64().unwrap_or(0),
                before,
                after,
                delta,
                checkpoint["created_at"].as_str().unwrap_or(""),
                preview
            ));
        }
        return Some(lines);
    }
    if let Some(checkpoint) = value.get("checkpoint").filter(|checkpoint| {
        checkpoint.get("mode").is_some() && checkpoint.get("tokens_after").is_some()
    }) && let Some(preflight) = value.get("preflight")
    {
        let mut lines = vec![format!(
            "{}  {}  {}  rev {}  {} → {} tokens",
            checkpoint["id"].as_str().unwrap_or("?"),
            checkpoint["mode"].as_str().unwrap_or("?"),
            checkpoint["label"].as_str().unwrap_or(""),
            checkpoint["plan_revision"].as_u64().unwrap_or(0),
            checkpoint["tokens_before"].as_u64().unwrap_or(0),
            checkpoint["tokens_after"].as_u64().unwrap_or(0)
        )];
        lines.push(format!(
            "active boundary: {} · messages {} · estimate {} / hard {}",
            if value["is_active"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            },
            preflight["active_messages"].as_u64().unwrap_or(0),
            preflight["estimated_tokens"].as_u64().unwrap_or(0),
            preflight["hard_limit_tokens"].as_u64().unwrap_or(0)
        ));
        for error in preflight["errors"].as_array().into_iter().flatten() {
            if let Some(error) = error.as_str() {
                lines.push(format!("error: {error}"));
            }
        }
        for warning in preflight["warnings"].as_array().into_iter().flatten() {
            if let Some(warning) = warning.as_str() {
                lines.push(format!("warning: {warning}"));
            }
        }
        return Some(lines);
    }
    if let Some(restored) = value.get("restored_event_seq") {
        return Some(vec![format!(
            "restored {} ({}) · {} active messages · {} tokens · events {}/{}",
            value["checkpoint_id"].as_str().unwrap_or("?"),
            value["mode"].as_str().unwrap_or("?"),
            value["active_messages"].as_u64().unwrap_or(0),
            value["estimated_tokens"].as_u64().unwrap_or(0),
            value["compaction_event_seq"].as_u64().unwrap_or(0),
            restored.as_u64().unwrap_or(0)
        )]);
    }
    if value.get("removed").is_some()
        && let Some(checkpoint_id) = value.get("checkpoint_id").and_then(Value::as_str)
        && value.get("session_id").is_some()
    {
        return Some(vec![format!(
            "{} {}",
            checkpoint_id,
            if value["removed"].as_bool().unwrap_or(false) {
                "deleted"
            } else {
                "not found"
            }
        )]);
    }
    if let Some(candidates) = value.get("candidates").and_then(Value::as_u64)
        && value.get("session_id").is_some()
        && value
            .get("checkpoints")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| row.get("tokens_before").is_some()))
    {
        return Some(vec![format!(
            "{}pruned {} of {} candidate context checkpoint(s)",
            if value["dry_run"].as_bool().unwrap_or(false) {
                "dry run: "
            } else {
                ""
            },
            value["removed"].as_u64().unwrap_or(0),
            candidates
        )]);
    }
    if let Some(project_id) = value.get("project_id").and_then(Value::as_str)
        && let Some(policy) = value
            .get("policy")
            .filter(|policy| policy.get("max_checkpoints").is_some())
    {
        return Some(vec![format!(
            "{}: context checkpoint sweep {} · max {} · age {}d",
            project_id,
            if policy["enabled"].as_bool().unwrap_or(false) {
                "on"
            } else {
                "off"
            },
            policy["max_checkpoints"].as_u64().unwrap_or(20),
            policy["max_age_days"].as_u64().unwrap_or(30)
        )]);
    }
    None
}

pub(crate) fn governor_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(policy) = value.get("policy")
        && value.get("session_id").is_none()
        && policy.get("reserve_ratio").is_some()
    {
        let enabled = policy["enabled"].as_bool().unwrap_or(false);
        let ratio = policy["reserve_ratio"].as_f64().unwrap_or(0.0) * 100.0;
        let throttle = policy["throttle_at_ratio"].as_f64().unwrap_or(0.0) * 100.0;
        let mode = policy["mode"].as_str().unwrap_or("single");
        let mut lines = vec![format!(
            "Critical-path governor: {} · {} · reserve {:.0}% · throttle {:.0}% · boost {} · min {} tokens",
            if enabled { "enabled" } else { "disabled" },
            mode,
            ratio.max(0.0),
            throttle.max(0.0),
            policy["critical_boost"].as_i64().unwrap_or(0),
            policy["min_task_budget"].as_u64().unwrap_or(0)
        )];
        if mode == "multi" {
            lines.push(format!(
                "Fairness: max {} paths · department floor {:.0}% · observed {:.0}% · starvation {} s",
                policy["max_paths"].as_u64().unwrap_or(4),
                policy["department_min_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                policy["observed_weight_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                policy["starvation_after_secs"].as_u64().unwrap_or(0)
            ));
        }
        return Some(lines);
    }
    let session = value.get("session_id")?.as_str()?;
    let reserve = value.get("reserve")?;
    let mut lines = Vec::new();
    lines.push(format!(
        "Governor {}{} · session {session} · plan r{} · {}{}",
        if value["enabled"].as_bool().unwrap_or(false) {
            "enabled"
        } else {
            "disabled"
        },
        reserve["state"]
            .as_str()
            .map(|state| format!(" ({state})"))
            .unwrap_or_default(),
        value["plan_revision"].as_u64().unwrap_or(0),
        value["session_status"].as_str().unwrap_or("unknown"),
        if value["unlimited"].as_bool().unwrap_or(false) {
            " · unlimited"
        } else {
            ""
        }
    ));
    if let Some(path) = value
        .get("critical_path")
        .filter(|path| path["task_ids"].is_array())
    {
        let tasks = path["task_ids"]
            .as_array()
            .map(|tasks| {
                tasks
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            })
            .unwrap_or_default();
        if tasks.is_empty() {
            lines.push(format!(
                "Path: complete · weight {} tokens",
                path["weight_tokens"].as_u64().unwrap_or(0)
            ));
        } else {
            lines.push(format!(
                "Path (weight {} tokens): {tasks}",
                path["weight_tokens"].as_u64().unwrap_or(0)
            ));
        }
    }
    lines.push(format!(
        "Reserve: protected {} of remaining {} · ratio {:.0}% · throttle {:.0}%{}",
        reserve["protected_tokens"].as_u64().unwrap_or(0),
        reserve["remaining_tokens"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unbounded".to_string()),
        reserve["reserve_ratio"].as_f64().unwrap_or(0.0) * 100.0,
        reserve["throttle_at_ratio"].as_f64().unwrap_or(0.0) * 100.0,
        if reserve["throttled"].as_bool().unwrap_or(false) {
            " · throttled"
        } else {
            ""
        }
    ));
    let mode = reserve["mode"]
        .as_str()
        .or_else(|| value["policy"]["mode"].as_str())
        .unwrap_or("single");
    if mode == "multi" {
        lines.push(format!(
            "Fairness: {} path(s) · allocated {} · floors {} · observed {}%{}{}",
            reserve["path_count"].as_u64().unwrap_or(0),
            reserve["allocated_tokens"].as_u64().unwrap_or(0),
            reserve["floor_tokens"].as_u64().unwrap_or(0),
            value["policy"]["observed_weight_ratio"]
                .as_f64()
                .unwrap_or(0.0)
                * 100.0,
            reserve["enumerated_paths"]
                .as_u64()
                .map(|value| format!(" · {value} enumerated"))
                .unwrap_or_default(),
            if reserve["paths_truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        ));
        if let Some(paths) = value["paths"].as_array().filter(|paths| !paths.is_empty()) {
            lines.push("Path table:".to_string());
            for path in paths.iter().take(4) {
                let tasks = path["task_ids"]
                    .as_array()
                    .map(|tasks| {
                        tasks
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" -> ")
                    })
                    .unwrap_or_default();
                let departments = path["departments"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "  #{} weight {} · floor {} · alloc {} · deficit {} · [{}] {}",
                    path["path_id"].as_u64().unwrap_or(0),
                    path["weight_tokens"].as_u64().unwrap_or(0),
                    path["floor_tokens"].as_u64().unwrap_or(0),
                    path["allocation_tokens"].as_u64().unwrap_or(0),
                    path["deficit_tokens"].as_u64().unwrap_or(0),
                    departments,
                    tasks
                ));
            }
            if paths.len() > 4 {
                lines.push(format!("  +{} more protected paths", paths.len() - 4));
            }
        }
        if let Some(departments) = value["departments"]
            .as_array()
            .filter(|departments| !departments.is_empty())
        {
            lines.push("Department floors:".to_string());
            for department in departments.iter().take(6) {
                let path_ids = department["path_ids"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(Value::as_u64)
                            .map(|id| format!("#{id}"))
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "  {} weight {} · floor {} · paths {}",
                    department["department"].as_str().unwrap_or("—"),
                    department["weight_tokens"].as_u64().unwrap_or(0),
                    department["floor_tokens"].as_u64().unwrap_or(0),
                    path_ids
                ));
            }
            if departments.len() > 6 {
                lines.push(format!("  +{} more departments", departments.len() - 6));
            }
        }
    }
    if let Some(admitted) = value["admitted"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        lines.push(format!(
            "Admit: {}",
            admitted
                .iter()
                .filter_map(Value::as_str)
                .take(8)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(deferred) = value["deferred"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        for deferral in deferred.iter().take(5) {
            lines.push(format!(
                "Defer: {} ({}, {})",
                deferral["task_id"].as_str().unwrap_or("?"),
                deferral["reason"].as_str().unwrap_or("?"),
                deferral["code"].as_str().unwrap_or("-")
            ));
        }
        if deferred.len() > 5 {
            lines.push(format!("  +{} more deferred tasks", deferred.len() - 5));
        }
        if let Some(order) = value["deferred_order"]
            .as_array()
            .filter(|order| !order.is_empty())
        {
            lines.push(format!(
                "Deferred order: {}",
                order
                    .iter()
                    .filter_map(Value::as_str)
                    .take(8)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Some(error) = value.get("error").filter(|error| error["code"].is_string()) {
        lines.push(format!(
            "Error: {} · {}",
            error["code"].as_str().unwrap_or("?"),
            error["message"].as_str().unwrap_or("")
        ));
        if let Some(hint) = error["hint"].as_str() {
            lines.push(format!("  hint: {hint}"));
        }
    }
    if let Some(last) = value["last_decisions"]
        .as_array()
        .and_then(|decisions| decisions.last())
    {
        lines.push(format!(
            "Last decision: {} {} at {} · {}",
            last["action"].as_str().unwrap_or("?"),
            last["reason"].as_str().unwrap_or("?"),
            last["created_at"].as_str().unwrap_or("?"),
            last["detail"].as_str().unwrap_or("")
        ));
    }
    Some(lines)
}

pub(crate) fn lsp_status_lines(value: &Value) -> Option<Vec<String>> {
    let languages = value.get("languages")?.as_array()?;
    value.get("supported_languages")?;
    let mut lines = vec![format!(
        "code intelligence · {} · {} running · {} installed",
        value["root"].as_str().unwrap_or("?"),
        value["running"].as_u64().unwrap_or(0),
        value["installed"].as_u64().unwrap_or(0)
    )];
    for language in languages {
        let state = if language["running"].as_bool().unwrap_or(false) {
            "running"
        } else if language["installed"].as_bool().unwrap_or(false) {
            "installed"
        } else {
            "missing"
        };
        let server = language["server"].as_str().unwrap_or("—");
        let capabilities = language["capabilities"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "—".to_string());
        lines.push(format!(
            "  {:<12} {:<10} {:<20} {}",
            language["display"].as_str().unwrap_or("?"),
            state,
            server,
            capabilities
        ));
        if state == "missing"
            && let Some(hint) = language["install_hint"].as_str()
            && !hint.is_empty()
        {
            lines.push(format!("    {hint}"));
        }
    }
    Some(lines)
}

pub(crate) fn process_console_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_processes").is_some())
    {
        let project = value["project_id"].as_str().unwrap_or("?");
        return Some(vec![
            format!("project {project} process policy"),
            format!(
                "  idle sweep {} · max {} · idle TTL {}s",
                if policy["idle_sweep_enabled"].as_bool().unwrap_or(false) {
                    "on"
                } else {
                    "off"
                },
                policy["max_processes"].as_u64().unwrap_or(0),
                policy["idle_ttl_secs"].as_u64().unwrap_or(0),
            ),
            format!(
                "  retained log {} bytes / {} lines",
                policy["max_output_bytes"].as_u64().unwrap_or(0),
                policy["max_output_lines"].as_u64().unwrap_or(0),
            ),
        ]);
    }
    if let Some(processes) = value.get("processes").and_then(Value::as_array)
        && value.get("active").is_some()
    {
        if processes.is_empty() {
            return Some(vec!["No supervised processes.".to_string()]);
        }
        let mut lines = Vec::new();
        for process in processes {
            let id = process["id"].as_str().unwrap_or("?");
            let state = process["state"].as_str().unwrap_or("?");
            let name = process["name"].as_str().unwrap_or("");
            let pid = process["pid"].as_i64().unwrap_or(0);
            let elapsed = process["started_at"].as_str().unwrap_or("");
            let exit = process["exit_code"]
                .as_i64()
                .map(|code| format!("  exit {code}"))
                .unwrap_or_default();
            lines.push(format!(
                "{id}  {state:<8}  pid {pid:<7}  {name}{exit}  started {elapsed}  {} bytes",
                process["output_bytes"].as_u64().unwrap_or(0)
            ));
            if let Some(detail) = process["detail"].as_str() {
                lines.push(format!("  {detail}"));
            }
        }
        return Some(lines);
    }
    if let Some(process_id) = value.get("process_id").and_then(Value::as_str)
        && let Some(content) = value.get("content").and_then(Value::as_str)
        && value.get("next_offset").is_some()
    {
        let mut lines = vec![format!(
            "{process_id}  {}  bytes {}/{}  offset {} -> {}{}",
            value["state"].as_str().unwrap_or("?"),
            value["content"].as_str().map(str::len).unwrap_or(0),
            value["total_bytes"].as_u64().unwrap_or(0),
            value["offset"].as_u64().unwrap_or(0),
            value["next_offset"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                "  (truncated)"
            } else {
                ""
            }
        )];
        if !content.is_empty() {
            lines.push(content.to_string());
        }
        return Some(lines);
    }
    if let Some(process) = value.get("process")
        && value.get("forced").is_some()
    {
        return Some(vec![format!(
            "{}  {}  exit {}{}  waited {}ms{}",
            process["id"].as_str().unwrap_or("?"),
            process["state"].as_str().unwrap_or("?"),
            process["exit_code"].as_i64().unwrap_or(-1),
            process["signal"]
                .as_i64()
                .map(|signal| format!("  signal {signal}"))
                .unwrap_or_default(),
            value["waited_ms"].as_u64().unwrap_or(0),
            if value["forced"].as_bool().unwrap_or(false) {
                "  (forced)"
            } else {
                ""
            }
        )]);
    }
    None
}

pub(crate) fn scaling_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(decision) = value
        .get("decision")
        .filter(|decision| decision.is_object())
    {
        let mut lines = Vec::new();
        let session = value
            .get("session_id")
            .and_then(Value::as_str)
            .or_else(|| decision.get("session_id").and_then(Value::as_str))
            .unwrap_or("?");
        lines.push(format!(
            "scaling preview  {session}  action={}  Workers {}→{}  Labour {}→{}",
            decision
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("hold"),
            decision
                .get("current_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("target_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("current_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("target_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ));
        lines.push(format!(
            "  ready {} · running {} · active Workers {} · active Labour {} · revision {} · applied {}",
            decision
                .get("ready_tasks")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("running_tasks")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("active_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("active_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("plan_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            decision
                .get("applied")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ));
        if let Some(reasons) = decision.get("reasons").and_then(Value::as_array) {
            lines.push(format!(
                "  reasons: {}",
                reasons
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        if let Some(hash) = decision.get("inputs_hash").and_then(Value::as_str) {
            let prefix = hash.get(..12).unwrap_or(hash);
            lines.push(format!("  inputs {prefix}…"));
        }
        if let Some(evidence) = decision.get("manager_evidence").and_then(Value::as_str) {
            lines.push(format!("  manager evidence: {evidence}"));
        }
        return Some(lines);
    }
    if value.get("global_worker_cap").is_some()
        && let Some(projects) = value.get("projects").and_then(Value::as_array)
    {
        let mut lines = vec![format!(
            "scaling  global Workers {}/{} available · Labour {}/{} available",
            value
                .get("global_worker_available")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_worker_cap")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_labour_available")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("global_labour_cap")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )];
        if projects.is_empty() {
            lines.push("  no projects".to_string());
            return Some(lines);
        }
        for project in projects {
            let name = project
                .get("project_name")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let policy = project.get("policy").unwrap_or(&Value::Null);
            lines.push(format!(
                "{name}  scaling={}  Workers {}-{}  Labour {}-{}  threshold {}  cooldown {}s  step {}  budget {}",
                if policy
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "on"
                } else {
                    "off"
                },
                policy
                    .get("min_workers")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_workers")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("min_labour")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_labour")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("expand_ready_threshold")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("cooldown_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("max_step")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                policy
                    .get("budget_headroom_ratio")
                    .and_then(Value::as_f64)
                    .map(|ratio| format!("≥{:.0}%", ratio * 100.0))
                    .unwrap_or_else(|| "off".to_string())
            ));
            for session in project
                .get("sessions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let latest = session.get("latest").unwrap_or(&Value::Null);
                let action = latest
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("none");
                lines.push(format!(
                    "  {}  {:<10}  rev {}  tasks {}/{} ready  slots {}W/{}L  active {}W/{}L  latest {}  {}",
                    session
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    session
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    session
                        .get("plan_revision")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("ready_tasks")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("plan_tasks")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("current_workers")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("current_labour")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("active_workers")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    session
                        .get("active_labour")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    action,
                    session
                        .get("goal")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                ));
                if latest.is_object()
                    && let Some(reasons) = latest.get("reasons").and_then(Value::as_array)
                    && !reasons.is_empty()
                {
                    lines.push(format!(
                        "    {}",
                        reasons
                            .iter()
                            .filter_map(Value::as_str)
                            .take(6)
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ));
                }
            }
        }
        return Some(lines);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_workers").is_some() && policy.get("max_labour").is_some())
    {
        let mut lines = vec![format!(
            "scaling policy  {}  enabled={}",
            value
                .get("project_id")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            policy
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )];
        lines.push(format!(
            "  Workers {}-{}  Labour {}-{}  threshold {}  critical delay {}s  idle {}s  cooldown {}s  step {}",
            policy
                .get("min_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_workers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("min_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_labour")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("expand_ready_threshold")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("expand_critical_delay_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("scale_down_idle_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("cooldown_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            policy
                .get("max_step")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
        lines.push(format!(
            "  budget headroom: {}",
            policy
                .get("budget_headroom_ratio")
                .and_then(Value::as_f64)
                .map(|ratio| format!("≥{:.0}%", ratio * 100.0))
                .unwrap_or_else(|| "check disabled".to_string())
        ));
        return Some(lines);
    }
    None
}

pub(crate) fn remote_bootstrap_lines(value: &Value) -> Option<Vec<String>> {
    if value.get("used_sudo").is_some()
        && value.get("sha256").is_some()
        && value.get("host").is_some()
    {
        let mut lines = vec![format!(
            "remote bootstrap {}  {}:{}  {}",
            if value["replaced"].as_bool().unwrap_or(false) {
                "replaced"
            } else {
                "installed"
            },
            value["host"].as_str().unwrap_or("?"),
            value["install_path"].as_str().unwrap_or("?"),
            value["helper_version"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "  sha256 {}  bytes {}  protocol {}  target {}/{}  sudo {}",
            value["sha256"].as_str().unwrap_or("?"),
            value["bytes"].as_u64().unwrap_or(0),
            value["protocol_version"].as_u64().unwrap_or(0),
            value["target_os"].as_str().unwrap_or("?"),
            value["target_arch"].as_str().unwrap_or("?"),
            value["used_sudo"].as_bool().unwrap_or(true)
        ));
        lines.push(format!(
            "  risk {}  confirmation required  chunks {}  duration {}ms",
            value["risk"].as_str().unwrap_or("host_state"),
            value["chunks"].as_u64().unwrap_or(0),
            value["duration_ms"].as_u64().unwrap_or(0)
        ));
        if value["host_path_mismatch"].as_bool().unwrap_or(false) {
            lines.push(
                "  note: ExecutionHost.helper_path does not name the installed file; update the host config explicitly"
                    .to_string(),
            );
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat: {caveat}"));
        }
        return Some(lines);
    }
    if value.get("configured").is_some() && value.get("configured_helper_path").is_some() {
        let mut lines = vec![format!(
            "remote bootstrap status for {}: configured={} path={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["configured"].as_bool().unwrap_or(false),
            value["install_path"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "  helper config {}  path matches install {}  risk {}",
            value["configured_helper_path"].as_str().unwrap_or("?"),
            value["path_matches_config"].as_bool().unwrap_or(false),
            value["risk"].as_str().unwrap_or("host_state")
        ));
        if let Some(record) = value.get("record").filter(|record| !record.is_null()) {
            lines.push(format!(
                "  installed {}  version {}  protocol {}  sha256 {}  target {}/{}",
                record["installed_at"].as_str().unwrap_or("?"),
                record["helper_version"].as_str().unwrap_or("?"),
                record["protocol_version"].as_u64().unwrap_or(0),
                record["sha256"].as_str().unwrap_or("?"),
                record["target_os"].as_str().unwrap_or("?"),
                record["target_arch"].as_str().unwrap_or("?")
            ));
        } else {
            lines.push("  no bootstrap record for this host".to_string());
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat: {caveat}"));
        }
        return Some(lines);
    }
    None
}

pub(crate) fn remote_test_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(evidence) = value
        .get("evidence")
        .filter(|evidence| evidence.get("digest").is_some())
    {
        let mut lines = vec![format!(
            "remote test {}  {}  {}:{}  {}",
            evidence["status"].as_str().unwrap_or("?"),
            evidence["profile"].as_str().unwrap_or("?"),
            evidence["host"].as_str().unwrap_or("?"),
            evidence["commit"].as_str().unwrap_or("?"),
            evidence["command"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "  exit {}  duration {}ms  digest {}",
            evidence["exit_code"]
                .as_i64()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_string()),
            evidence["duration_ms"].as_u64().unwrap_or(0),
            evidence["digest"].as_str().unwrap_or("?")
        ));
        if let Some(output) = evidence["output_ref"].as_str() {
            lines.push(format!(
                "  output {}  sha256 {}  bytes {}  truncated {}",
                output,
                evidence["output_sha256"].as_str().unwrap_or("?"),
                evidence["output_bytes"].as_u64().unwrap_or(0),
                evidence["output_truncated"].as_bool().unwrap_or(false)
            ));
        }
        if value
            .get("supplemental")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            lines.push(
                "  supplemental: local Verifier and deterministic Integrator remain mandatory; the merge gate is unchanged"
                    .to_string(),
            );
        }
        if let Some(entry) = value.get("entry").filter(|entry| !entry.is_null()) {
            lines.push(format!(
                "  attached to queue entry {}  verifier {}  status {}",
                entry["id"].as_str().unwrap_or("?"),
                entry["verifier_approved"]
                    .as_bool()
                    .map(|approved| if approved { "approved" } else { "rejected" })
                    .unwrap_or("not yet run"),
                entry["status"].as_str().unwrap_or("?")
            ));
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat: {caveat}"));
        }
        return Some(lines);
    }
    if value.get("passed").is_some()
        && value.get("timed_out").is_some()
        && value.get("evidence").is_some()
    {
        let mut lines = vec![format!(
            "remote test evidence for {}: {} total · {} passed · {} failed · {} timeout",
            value["project_id"].as_str().unwrap_or("?"),
            value["total"].as_u64().unwrap_or(0),
            value["passed"].as_u64().unwrap_or(0),
            value["failed"].as_u64().unwrap_or(0),
            value["timed_out"].as_u64().unwrap_or(0)
        )];
        if let Some(policy) = value.get("policy") {
            lines.push(format!(
                "  policy enabled={} default={} profiles={}",
                policy["enabled"].as_bool().unwrap_or(false),
                policy["default_profile"].as_str().unwrap_or("-"),
                policy["profiles"].as_array().map(Vec::len).unwrap_or(0)
            ));
        }
        for row in value["evidence"].as_array().cloned().unwrap_or_default() {
            lines.push(format!(
                "  {}  {}  {}  {}  digest {}",
                row["status"].as_str().unwrap_or("?"),
                row["profile"].as_str().unwrap_or("?"),
                row["commit"].as_str().unwrap_or("?"),
                row["command"].as_str().unwrap_or("?"),
                row["digest"]
                    .as_str()
                    .map(|digest| &digest[..digest.len().min(12)])
                    .unwrap_or("?")
            ));
        }
        lines.push(
            "  supplemental only: remote evidence never bypasses the local Verifier or deterministic Integrator"
                .to_string(),
        );
        return Some(lines);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("profiles").is_some())
    {
        let mut lines = vec![format!(
            "remote test policy for {}: enabled={} default={} timeout={}s output_cap={}B",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(false),
            policy["default_profile"].as_str().unwrap_or("-"),
            policy["timeout_secs"].as_u64().unwrap_or(0),
            policy["output_cap_bytes"].as_u64().unwrap_or(0)
        )];
        for profile in policy["profiles"].as_array().cloned().unwrap_or_default() {
            lines.push(format!(
                "  {}  {}  {}",
                profile["name"].as_str().unwrap_or("?"),
                profile["kind"].as_str().unwrap_or("?"),
                profile["command"].as_str().unwrap_or("?")
            ));
        }
        return Some(lines);
    }
    None
}

pub(crate) fn remote_integration_lines(value: &Value) -> Option<Vec<String>> {
    if value.get("removed_refs").is_some() && value.get("skipped").is_some() {
        let refs = value["removed_refs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let worktrees = value["removed_worktrees"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let skipped = value["skipped"].as_array().cloned().unwrap_or_default();
        let mut lines = vec![format!(
            "remote integration cleanup for {}: examined {} · removed {} ref(s) · {} worktree(s) · {} skipped",
            value["project_id"].as_str().unwrap_or("?"),
            value["entries_examined"].as_u64().unwrap_or(0),
            refs.len(),
            worktrees.len(),
            skipped.len()
        )];
        for reference in refs {
            if let Some(reference) = reference.as_str() {
                lines.push(format!("  removed {reference}"));
            }
        }
        for worktree in worktrees {
            if let Some(worktree) = worktree.as_str() {
                lines.push(format!("  removed worktree {worktree}"));
            }
        }
        for reason in skipped {
            if let Some(reason) = reason.as_str() {
                lines.push(format!("  skipped {reason}"));
            }
        }
        return Some(lines);
    }
    if let (Some(entry), Some(bundle), Some(verification)) = (
        value.get("entry"),
        value.get("bundle"),
        value.get("verification"),
    ) {
        let mut lines = Vec::new();
        lines.push(format!(
            "remote integration {}  {}  {}:{}",
            entry["status"].as_str().unwrap_or("?"),
            entry["id"].as_str().unwrap_or("?"),
            bundle["host"].as_str().unwrap_or("?"),
            bundle["branch"].as_str().unwrap_or("?")
        ));
        lines.push(format!(
            "  commit {}  bundle sha256 {}  bytes {}",
            entry["commit"].as_str().unwrap_or("?"),
            bundle["sha256"].as_str().unwrap_or("?"),
            bundle["bytes"].as_u64().unwrap_or(0)
        ));
        lines.push(format!(
            "  verifier required={} performed={} approved={}",
            verification["required"].as_bool().unwrap_or(false),
            verification["performed"].as_bool().unwrap_or(false),
            verification["approved"]
                .as_bool()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "—".into())
        ));
        if let Some(summary) = verification["summary"].as_str() {
            lines.push(format!("  verdict {summary}"));
        }
        lines.push(format!(
            "  imported_ref {}  refs_pruned {}",
            value["imported_ref"].as_str().unwrap_or("—"),
            value["refs_pruned"].as_bool().unwrap_or(false)
        ));
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat {caveat}"));
        }
        return Some(lines);
    }
    if let Some(policy) = value.get("policy")
        && let Some(entries) = value.get("entries").and_then(Value::as_array)
    {
        let mut lines = vec![format!(
            "remote integration policy enabled={} verifier_required={} max_bundle_bytes={} keep_refs={}",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["require_verifier"].as_bool().unwrap_or(true),
            policy["max_bundle_bytes"].as_u64().unwrap_or(0),
            policy["keep_import_refs"].as_bool().unwrap_or(false)
        )];
        let hosts = policy["allowed_hosts"]
            .as_array()
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        lines.push(format!(
            "  allowed hosts: {}",
            if hosts.is_empty() {
                "(none)".to_string()
            } else {
                hosts
            }
        ));
        lines.push(format!(
            "  queue total {} · pending {} · merged {} · conflicts {} · failed {} · refs retained {}",
            value["total"].as_u64().unwrap_or(0),
            value["pending"].as_u64().unwrap_or(0),
            value["merged"].as_u64().unwrap_or(0),
            value["conflicts"].as_u64().unwrap_or(0),
            value["failed"].as_u64().unwrap_or(0),
            value["retained_refs"].as_u64().unwrap_or(0)
        ));
        if entries.is_empty() {
            lines.push("  no remote queue entries".to_string());
        }
        for entry in entries {
            lines.push(format!(
                "  {}  {:<10}  {}  {}:{}  attempt {}  {}",
                entry["entry_id"].as_str().unwrap_or("?"),
                entry["status"].as_str().unwrap_or("?"),
                entry["commit"]
                    .as_str()
                    .unwrap_or("?")
                    .chars()
                    .take(12)
                    .collect::<String>(),
                entry["remote_host"].as_str().unwrap_or("?"),
                entry["remote_branch"].as_str().unwrap_or("?"),
                entry["attempt"].as_u64().unwrap_or(0),
                entry["imported_ref"].as_str().unwrap_or("—")
            ));
            if let Some(approved) = entry["verifier_approved"].as_bool() {
                lines.push(format!(
                    "    verifier {}: {}",
                    if approved { "approved" } else { "rejected" },
                    entry["verifier_summary"].as_str().unwrap_or("no summary")
                ));
            }
            if let Some(detail) = entry["detail"].as_str()
                && !detail.is_empty()
            {
                lines.push(format!("    {detail}"));
            }
        }
        if let Some(caveat) = value["environment_caveat"].as_str() {
            lines.push(format!("  caveat {caveat}"));
        }
        return Some(lines);
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && value.get("entries").is_none()
    {
        let hosts = policy["allowed_hosts"]
            .as_array()
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        return Some(vec![
            format!(
                "remote integration policy for {} enabled={} verifier_required={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["enabled"].as_bool().unwrap_or(false),
                policy["require_verifier"].as_bool().unwrap_or(true)
            ),
            format!(
                "  max_bundle_bytes {} · keep_import_refs {} · allowed hosts {}",
                policy["max_bundle_bytes"].as_u64().unwrap_or(0),
                policy["keep_import_refs"].as_bool().unwrap_or(false),
                if hosts.is_empty() {
                    "(none)".to_string()
                } else {
                    hosts
                }
            ),
        ]);
    }
    None
}

pub(crate) fn test_cache_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(map) = value.get("map") {
        let mut lines = vec![format!(
            "test impact  session {}  project {}  {} target(s)  generation {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            map["targets"].as_array().map(Vec::len).unwrap_or(0),
            map["generation"].as_str().unwrap_or("?")
        )];
        if let Some(command) = value.get("test_command").and_then(Value::as_str) {
            lines.push(format!("  command: {command}"));
        }
        lines.push(format!(
            "  lsp_available={} lexical_only={} truncated={} changed={}",
            map["lsp_available"].as_bool().unwrap_or(false),
            map["lexical_only"].as_bool().unwrap_or(false),
            map["truncated"].as_bool().unwrap_or(false),
            map["changed_paths"].as_array().map(Vec::len).unwrap_or(0)
        ));
        for target in map["targets"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  {:<42} {:<8} {:>5.0}%  {}",
                target["path"].as_str().unwrap_or("?"),
                target["source"].as_str().unwrap_or("?"),
                target["confidence"].as_f64().unwrap_or(0.0) * 100.0,
                target["reason"].as_str().unwrap_or("")
            ));
        }
        for note in map["notes"].as_array().into_iter().flatten() {
            lines.push(format!("  note: {}", note.as_str().unwrap_or("?")));
        }
        return Some(lines);
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && value.get("total").is_some()
        && value.get("project_id").is_some()
    {
        let mut lines = vec![format!(
            "test cache entries  {}  {} of {} shown",
            value["project_id"].as_str().unwrap_or("?"),
            entries.len(),
            value["total"].as_u64().unwrap_or(0)
        )];
        for entry in entries {
            lines.push(format!(
                "  {}  {:<5} {:<6}  exit {:<3}  {:>7}ms  hits {:<4}  {}",
                entry["id"].as_str().unwrap_or("?"),
                entry["kind"].as_str().unwrap_or("?"),
                entry["status"].as_str().unwrap_or("?"),
                entry["exit_code"].as_i64().unwrap_or(0),
                entry["duration_ms"].as_u64().unwrap_or(0),
                entry["hit_count"].as_u64().unwrap_or(0),
                entry["command"].as_str().unwrap_or("")
            ));
        }
        if entries.is_empty() {
            lines.push("  no cached results".to_string());
        }
        return Some(lines);
    }
    if value.get("pruned").is_some() && value.get("affected_sessions").is_some() {
        let mut lines = vec![format!(
            "test cache prune  {}  {} removed · {} remaining · {} session(s) · dry_run={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["pruned"].as_u64().unwrap_or(0),
            value["remaining"].as_u64().unwrap_or(0),
            value["affected_sessions"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        )];
        if let Some(ids) = value.get("removed_ids").and_then(Value::as_array) {
            for id in ids.iter().take(20).filter_map(Value::as_str) {
                lines.push(format!("  removed {id}"));
            }
        }
        return Some(lines);
    }
    if value.get("cleared").is_some() && value.get("affected_sessions").is_some() {
        return Some(vec![format!(
            "test cache clear  {}  {} cleared · {} remaining · {} session(s)",
            value["project_id"].as_str().unwrap_or("?"),
            value["cleared"].as_u64().unwrap_or(0),
            value["remaining"].as_u64().unwrap_or(0),
            value["affected_sessions"].as_u64().unwrap_or(0)
        )]);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_output_bytes").is_some())
    {
        let mut lines = vec![format!(
            "test cache policy  {}  enabled={}  max-entries={}  retention={}d  output≤{}B  reuse-lint={}  integrator={}  failed-ttl={}s",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(true),
            policy["max_entries"].as_u64().unwrap_or(0),
            policy["retention_days"].as_u64().unwrap_or(0),
            policy["max_output_bytes"].as_u64().unwrap_or(0),
            policy["reuse_lint"].as_bool().unwrap_or(true),
            policy["include_integration"].as_bool().unwrap_or(false),
            policy["failed_retention_secs"].as_u64().unwrap_or(0)
        )];
        if value.get("entries").is_some_and(Value::is_u64) {
            lines.push(format!(
                "  entries={} passed={} failed={} error={} expired={} hits={} output={}B",
                value["entries"].as_u64().unwrap_or(0),
                value["passed"].as_u64().unwrap_or(0),
                value["failed"].as_u64().unwrap_or(0),
                value["error"].as_u64().unwrap_or(0),
                value["expired"].as_u64().unwrap_or(0),
                value["total_hits"].as_u64().unwrap_or(0),
                value["output_bytes"].as_u64().unwrap_or(0)
            ));
            if let Some(kinds) = value.get("by_kind").and_then(Value::as_object) {
                let summary = kinds
                    .iter()
                    .map(|(kind, count)| format!("{kind}={}", count.as_u64().unwrap_or(0)))
                    .collect::<Vec<_>>()
                    .join(" ");
                if !summary.is_empty() {
                    lines.push(format!("  kinds: {summary}"));
                }
            }
        }
        return Some(lines);
    }
    None
}

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

pub(crate) fn instruction_lines(value: &Value) -> Option<Vec<String>> {
    value.get("prompt_layout")?;
    let states = value.get("states")?.as_array()?;
    let session = value
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("session");
    let layout = value
        .get("prompt_layout")
        .and_then(Value::as_u64)
        .unwrap_or(2);
    let prefix = value
        .get("latest_prefix_hash")
        .and_then(Value::as_str)
        .map(|hash| hash.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "none".to_string());
    let identity = value
        .get("latest_identity_id")
        .and_then(Value::as_str)
        .filter(|identity| !identity.is_empty())
        .map(|identity| {
            let generation = value
                .get("latest_identity_generation")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            format!(
                " · identity {}… gen {generation}",
                identity.chars().take(12).collect::<String>()
            )
        })
        .unwrap_or_default();
    let mut lines = vec![format!(
        "instructions · {session} · layout v{layout} · prefix {prefix}…{identity} · {} state(s)",
        states.len()
    )];
    if value
        .get("last_rebuilt")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        lines.push(format!(
            "rebuilt diagnostics in window: {}",
            value
                .get("last_rebuilt")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
    }
    for state in states {
        let state_id = state
            .get("state_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let role = state
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("task-scoped");
        lines.push(format!("  {state_id} · {role}"));
        let Some(sections) = state.get("sections").and_then(Value::as_array) else {
            continue;
        };
        for section in sections {
            let ordinal = section.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
            let tail = section
                .get("tail")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let key = section.get("key").and_then(Value::as_str).unwrap_or("?");
            let hash = section
                .get("hash")
                .and_then(Value::as_str)
                .map(|hash| hash.chars().take(12).collect::<String>())
                .unwrap_or_default();
            let chars = section.get("chars").and_then(Value::as_u64).unwrap_or(0);
            let available = section
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            lines.push(format!(
                "    {ordinal:>3} {:<4} {key:<18} {chars:>7} chars  {hash}…{}",
                if tail { "tail" } else { "head" },
                if available { "" } else { "  MISSING" }
            ));
        }
    }
    Some(lines)
}

pub(crate) fn tool_bundle_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && entries.iter().all(|entry| entry.get("bundle").is_some())
    {
        let mut lines = vec![format!("tool bundle history  {session_id}")];
        if entries.is_empty() {
            lines.push("  no persisted bundle observations".to_string());
        }
        for entry in entries {
            lines.push(format!(
                "  seq {:<6} {:<10} {:<13} {:>3} tools · {} mcp · {} · {}{}",
                entry["seq"].as_u64().unwrap_or(0),
                entry["role"].as_str().unwrap_or("?"),
                entry["bundle"].as_str().unwrap_or("?"),
                entry["tool_count"].as_u64().unwrap_or(0),
                entry["mcp_tool_count"].as_u64().unwrap_or(0),
                entry["created_at"].as_str().unwrap_or(""),
                short_hash(&entry["bundle_hash"]),
                if entry["fallback"].as_bool().unwrap_or(false) {
                    " fallback"
                } else {
                    ""
                }
            ));
        }
        return Some(lines);
    }
    if value.get("from_bundle").is_some()
        && value.get("to_bundle").is_some()
        && value.get("added").is_some()
    {
        let mut lines = vec![format!(
            "tool bundle diff  {} → {}",
            value["from_bundle"].as_str().unwrap_or("?"),
            value["to_bundle"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "  role {} · profile {} · task kind {}",
            value["role"].as_str().unwrap_or("?"),
            value["profile"].as_str().unwrap_or("?"),
            value["task_kind"].as_str().unwrap_or("—")
        ));
        lines.push(format!(
            "  {} → {} tools · changed {}",
            value["from_count"].as_u64().unwrap_or(0),
            value["to_count"].as_u64().unwrap_or(0),
            value["changed"].as_bool().unwrap_or(false)
        ));
        for (label, key) in [
            ("added", "added"),
            ("removed", "removed"),
            ("shared", "shared"),
        ] {
            if let Some(items) = value[key].as_array().filter(|items| !items.is_empty()) {
                lines.push(format!(
                    "  {label:<8} {}",
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        for warning in value["warnings"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  [{}] {}",
                warning["code"].as_str().unwrap_or("warning"),
                warning["message"].as_str().unwrap_or("")
            ));
        }
        return Some(lines);
    }
    if let Some(rows) = value.get("rows").and_then(Value::as_array)
        && value.get("task_kinds").and_then(Value::as_array).is_some()
        && rows.iter().all(|row| row.get("bundle_hash").is_some())
    {
        let mut lines = vec!["tool bundle matrix".to_string()];
        lines.push(format!(
            "  {:<10} {:<13} {:<18} {:<13} {:>7} {:>8} {:>7} {}",
            "role", "profile", "task kind", "bundle", "tools", "target", "omitted", "status"
        ));
        for row in rows {
            let task_kind = row["task_kind"].as_str().unwrap_or("—");
            let target = row["target_limit"]
                .as_u64()
                .map(|limit| format!("{limit}"))
                .unwrap_or_else(|| "—".to_string());
            let status = match row["within_target"].as_bool() {
                Some(true) => "ok",
                Some(false) => "OVER",
                None => {
                    if row["fallback"].as_bool().unwrap_or(false) {
                        "fallback"
                    } else {
                        "ok"
                    }
                }
            };
            lines.push(format!(
                "  {:<10} {:<13} {:<18} {:<13} {:>7} {:>8} {:>7} {}",
                row["role"].as_str().unwrap_or("?"),
                row["profile"].as_str().unwrap_or("?"),
                task_kind,
                row["bundle"].as_str().unwrap_or("?"),
                row["builtin_count"].as_u64().unwrap_or(0),
                target,
                row["omitted_count"].as_u64().unwrap_or(0),
                status
            ));
        }
        for warning in value["warnings"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  [{}] {}",
                warning["code"].as_str().unwrap_or("warning"),
                warning["message"].as_str().unwrap_or("")
            ));
        }
        return Some(lines);
    }
    if let Some(bundles) = value.get("bundles").and_then(Value::as_array)
        && bundles
            .iter()
            .all(|row| row.get("requested_count").is_some())
    {
        let mut lines = vec!["tool bundles".to_string()];
        for row in bundles {
            let target = row["target_limit"]
                .as_u64()
                .map(|limit| format!("/{limit}"))
                .unwrap_or_default();
            lines.push(format!(
                "  {:<12} requested {}{target} · {} mcp · {}/{}",
                row["bundle"].as_str().unwrap_or("?"),
                row["requested_count"].as_u64().unwrap_or(0),
                if row["allows_dynamic_mcp"].as_bool().unwrap_or(false) {
                    "appends"
                } else {
                    "no"
                },
                row["representative_role"].as_str().unwrap_or("?"),
                row["representative_profile"].as_str().unwrap_or("?")
            ));
            if let Some(summary) = row["summary"].as_str() {
                lines.push(format!("               {summary}"));
            }
        }
        for warning in value["warnings"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  [{}] {}",
                warning["code"].as_str().unwrap_or("warning"),
                warning["message"].as_str().unwrap_or("")
            ));
        }
        return Some(lines);
    }
    let session_id = value.get("session_id")?.as_str()?;
    let roles = value.get("roles")?.as_array()?;
    if !value.get("catalog")?.as_array().is_some() {
        return None;
    }
    if !roles
        .iter()
        .any(|role| role.get("bundle").is_some() && role.get("bundle_hash").is_some())
    {
        return None;
    }
    let mut lines = vec![format!("tool bundle  {session_id}")];
    for role in roles {
        let role_name = role["role"].as_str().unwrap_or("?");
        let bundle = role["bundle"].as_str().unwrap_or("?");
        let target = role["target_limit"]
            .as_u64()
            .map(|limit| format!("/{limit}"))
            .unwrap_or_default();
        let within = match role["within_target"].as_bool() {
            Some(true) => "ok",
            Some(false) => "OVER TARGET",
            None => "no target",
        };
        lines.push(format!(
            "  {role_name:<9} {bundle:<12} {}{target} tools · {within} · {} mcp",
            role["builtin_count"].as_u64().unwrap_or(0),
            role["mcp_count"].as_u64().unwrap_or(0)
        ));
        lines.push(format!(
            "            profile {} · task kind {} · {}",
            role["profile"].as_str().unwrap_or("?"),
            role["task_kind"].as_str().unwrap_or("—"),
            role["reason"].as_str().unwrap_or("?")
        ));
        lines.push(format!(
            "            bundle {} · tools {} · {} schema bytes · ~{} tokens",
            short_hash(&role["bundle_hash"]),
            short_hash(&role["tools_hash"]),
            role["schema_bytes"].as_u64().unwrap_or(0),
            role["estimated_tool_tokens"].as_u64().unwrap_or(0)
        ));
        if let Some(last) = role.get("last_run").filter(|last| !last.is_null()) {
            lines.push(format!(
                "            last run {} · {} tools · {} mcp · {}",
                last["bundle"].as_str().unwrap_or("?"),
                last["tool_count"].as_u64().unwrap_or(0),
                last["mcp_tool_count"].as_u64().unwrap_or(0),
                last["created_at"].as_str().unwrap_or("")
            ));
        }
        if let Some(omitted) = role["omitted"].as_array().filter(|items| !items.is_empty()) {
            lines.push(format!(
                "            omitted {}",
                omitted
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for warning in role["warnings"].as_array().into_iter().flatten() {
            lines.push(format!(
                "            [{}] {}",
                warning["code"].as_str().unwrap_or("warning"),
                warning["message"].as_str().unwrap_or("")
            ));
        }
        if let Some(categories) = role["categories"]
            .as_array()
            .filter(|categories| !categories.is_empty())
        {
            let rendered = categories
                .iter()
                .map(|category| {
                    format!(
                        "{} {}",
                        category["category"].as_str().unwrap_or("?"),
                        category["tools"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            lines.push(format!("            categories {rendered}"));
        }
        if let Some(tools) = role["tools"].as_array().filter(|tools| !tools.is_empty()) {
            let mut line = String::from("            tools ");
            let mut first = true;
            for tool in tools {
                if !first {
                    line.push_str(", ");
                }
                first = false;
                line.push_str(tool["name"].as_str().unwrap_or("?"));
            }
            lines.push(line);
        }
    }
    if let Some(catalog) = value.get("catalog").and_then(Value::as_array) {
        lines.push("  catalog".to_string());
        for row in catalog {
            let target = row["target_limit"]
                .as_u64()
                .map(|limit| format!("/{limit}"))
                .unwrap_or_default();
            lines.push(format!(
                "    {:<12} requested {}{target} · {} mcp",
                row["bundle"].as_str().unwrap_or("?"),
                row["requested_count"].as_u64().unwrap_or(0),
                if row["allows_dynamic_mcp"].as_bool().unwrap_or(false) {
                    "appends"
                } else {
                    "no"
                }
            ));
        }
    }
    Some(lines)
}

/// Bounded human rendering for effect receipts and policy. It prints only
/// target labels, states, attempts, and probe evidence — never file content,
/// command text, or provider secrets.
pub(crate) fn effect_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(receipts) = value.get("receipts").and_then(Value::as_array) {
        let policy = value.get("policy").cloned().unwrap_or_else(|| json!({}));
        let counts = value.get("counts").cloned().unwrap_or_else(|| json!({}));
        let blocked = value
            .get("blocked")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut lines = vec![format!(
            "effects: {} total · {} intent · {} done · {} failed · {} unknown · {} blocking{}",
            counts.get("total").and_then(Value::as_u64).unwrap_or(0),
            counts.get("intent").and_then(Value::as_u64).unwrap_or(0),
            counts.get("done").and_then(Value::as_u64).unwrap_or(0),
            counts.get("failed").and_then(Value::as_u64).unwrap_or(0),
            counts.get("unknown").and_then(Value::as_u64).unwrap_or(0),
            counts.get("blocking").and_then(Value::as_u64).unwrap_or(0),
            if blocked { " (merge blocked)" } else { "" },
        )];
        lines.push(format!(
            "policy: enabled={} unknown_blocks_merge={} stale_grace={}s shell_mode={}",
            policy
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("unknown_blocks_merge")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("stale_intent_grace_secs")
                .and_then(Value::as_u64)
                .unwrap_or(120),
            policy
                .get("shell_mode")
                .and_then(Value::as_str)
                .unwrap_or("external_only"),
        ));
        if let Some(reason) = value.get("block_reason").and_then(Value::as_str) {
            lines.push(reason.to_string());
        }
        for receipt in receipts {
            let target = receipt
                .get("target")
                .map(|target| {
                    format!(
                        "{}:{}",
                        target.get("kind").and_then(Value::as_str).unwrap_or("?"),
                        target.get("key").and_then(Value::as_str).unwrap_or("")
                    )
                })
                .unwrap_or_else(|| "?".to_string());
            let evidence = receipt
                .get("evidence")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let anchor = evidence
                .get("sha256")
                .and_then(Value::as_str)
                .or_else(|| evidence.get("commit").and_then(Value::as_str))
                .or_else(|| receipt.get("external_id").and_then(Value::as_str))
                .map(|value| format!(" @ {}", value.chars().take(16).collect::<String>()))
                .unwrap_or_default();
            lines.push(format!(
                "{}  {}  attempt {}{}  {} → {}",
                receipt.get("id").and_then(Value::as_str).unwrap_or("?"),
                receipt.get("state").and_then(Value::as_str).unwrap_or("?"),
                receipt.get("attempt").and_then(Value::as_u64).unwrap_or(0),
                anchor,
                receipt.get("tool").and_then(Value::as_str).unwrap_or("?"),
                target,
            ));
            if let Some(reason) = evidence.get("reason").and_then(Value::as_str) {
                lines.push(format!("    reason: {reason}"));
            }
        }
        return Some(lines);
    }
    if let Some(receipt) = value.get("receipt")
        && let Some(state) = receipt.get("state").and_then(Value::as_str)
    {
        return Some(vec![format!(
            "effect {}: {} attempt {} · {}",
            receipt.get("id").and_then(Value::as_str).unwrap_or("?"),
            state,
            receipt.get("attempt").and_then(Value::as_u64).unwrap_or(0),
            receipt
                .get("target")
                .map(|target| format!(
                    "{}:{}",
                    target.get("kind").and_then(Value::as_str).unwrap_or("?"),
                    target.get("key").and_then(Value::as_str).unwrap_or("")
                ))
                .unwrap_or_else(|| "?".to_string()),
        )]);
    }
    if let Some(policy) = value.get("policy")
        && let Some(project) = value.get("project_id").and_then(Value::as_str)
    {
        return Some(vec![format!(
            "project {project}  effects enabled={} unknown_blocks_merge={} stale_grace={}s shell_mode={} http_mutations={} http_reads={} mcp_mutations={} workspace_writes={}",
            policy
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("unknown_blocks_merge")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("stale_intent_grace_secs")
                .and_then(Value::as_u64)
                .unwrap_or(120),
            policy
                .get("shell_mode")
                .and_then(Value::as_str)
                .unwrap_or("external_only"),
            policy
                .get("track_mutating_http")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("track_read_http")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            policy
                .get("track_mcp_mutations")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            policy
                .get("track_workspace_writes")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        )]);
    }
    if let Some(swept) = value.get("swept").and_then(Value::as_u64) {
        return Some(vec![format!(
            "effect sweep: {swept} stale intent(s) moved to unknown"
        )]);
    }
    None
}

pub(crate) fn diagnosis_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(rows) = value.get("attributions").and_then(Value::as_array) {
        let mut lines = vec![format!(
            "attributions: {} recorded  total={}  truncated={}",
            rows.len(),
            value.get("total").and_then(Value::as_u64).unwrap_or(0),
            value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )];
        for row in rows.iter().take(32) {
            let attribution = row.get("attribution").unwrap_or(row);
            let target = attribution.get("target").unwrap_or(&Value::Null);
            let earliest = attribution
                .get("earliest_node_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            lines.push(format!(
                "  seq {} · {} · target={}:{} · earliest={} @{}",
                row.get("event_seq").and_then(Value::as_u64).unwrap_or(0),
                attribution
                    .get("reason_code")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                target.get("kind").and_then(Value::as_str).unwrap_or("?"),
                target.get("id").and_then(Value::as_str).unwrap_or("-"),
                earliest,
                attribution
                    .get("earliest_seq")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ));
        }
        lines.push(
            "advisory only: recorded evidence never retries, cancels, merges, reassigns, or changes budgets"
                .to_string(),
        );
        return Some(lines);
    }
    value.get("graph_summary")?;
    let target = value.get("target").unwrap_or(&Value::Null);
    let mut lines = vec![format!(
        "diagnosis: {}  target={}:{}  status={}  truncated={}",
        value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_uppercase(),
        target.get("kind").and_then(Value::as_str).unwrap_or("-"),
        target.get("id").and_then(Value::as_str).unwrap_or("-"),
        target.get("status").and_then(Value::as_str).unwrap_or("-"),
        value
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    )];
    if let Some(recorded) = value.get("recorded_event_seq").and_then(Value::as_u64) {
        lines.push(format!("recorded: failure.attribution seq {recorded}"));
    }
    if let Some(earliest) = value.get("earliest") {
        lines.push(format!(
            "earliest unsupported claim: {} · {} @{} · task={} · node={}",
            earliest
                .get("reason_code")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            earliest
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            earliest.get("at_seq").and_then(Value::as_u64).unwrap_or(0),
            earliest
                .get("task_id")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            earliest
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or("-")
        ));
        if let Some(summary) = earliest.get("summary").and_then(Value::as_str) {
            lines.push(format!("  {}", bounded_line(summary, 220)));
        }
        if let Some(agents) = earliest.get("agents").and_then(Value::as_array) {
            for agent in agents.iter().take(4) {
                lines.push(format!(
                    "  responsible: {} [{}] {}",
                    agent
                        .get("agent_name")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    agent.get("role").and_then(Value::as_str).unwrap_or("-"),
                    agent.get("agent_id").and_then(Value::as_str).unwrap_or("-")
                ));
            }
        }
        if let Some(evidence) = earliest.get("evidence").and_then(Value::as_array) {
            for item in evidence.iter().take(6) {
                let hash = item
                    .get("hash")
                    .and_then(Value::as_str)
                    .map(short_hash_str)
                    .unwrap_or_else(|| "-".to_string());
                lines.push(format!(
                    "  evidence: {}:{} @{} hash={}",
                    item.get("kind").and_then(Value::as_str).unwrap_or("?"),
                    item.get("reference").and_then(Value::as_str).unwrap_or("-"),
                    item.get("event_seq").and_then(Value::as_u64).unwrap_or(0),
                    hash
                ));
            }
        }
    }
    if let Some(cascade) = value.get("cascade").and_then(Value::as_array)
        && !cascade.is_empty()
    {
        let path = cascade
            .iter()
            .take(24)
            .map(|step| {
                format!(
                    "{}[{}]",
                    step.get("task_id").and_then(Value::as_str).unwrap_or("?"),
                    step.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                )
            })
            .collect::<Vec<_>>()
            .join(" -> ");
        lines.push(format!("cascade: {path}"));
    }
    if let Some(findings) = value.get("findings").and_then(Value::as_array) {
        for finding in findings.iter().take(12) {
            lines.push(format!(
                "  finding: {} [{}] task={} node={}",
                finding
                    .get("reason_code")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                finding
                    .get("severity")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                finding
                    .get("task_id")
                    .and_then(Value::as_str)
                    .unwrap_or("-"),
                finding
                    .get("node_id")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
            ));
        }
    }
    if let Some(summary) = value.get("graph_summary") {
        lines.push(format!(
            "provenance: nodes={} edges={} tasks={} agents={} artifacts={} commits={} checks={} verdicts={} merges={} depth={} depth_limited={} digest={}",
            summary.get("nodes").and_then(Value::as_u64).unwrap_or(0),
            summary.get("edges").and_then(Value::as_u64).unwrap_or(0),
            summary.get("tasks").and_then(Value::as_u64).unwrap_or(0),
            summary.get("agents").and_then(Value::as_u64).unwrap_or(0),
            summary
                .get("artifacts")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary.get("commits").and_then(Value::as_u64).unwrap_or(0),
            summary
                .get("test_evidence")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("verifications")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary.get("merges").and_then(Value::as_u64).unwrap_or(0),
            summary
                .get("max_depth")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("depth_limited")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            summary
                .get("digest")
                .and_then(Value::as_str)
                .map(short_hash_str)
                .unwrap_or_else(|| "-".to_string())
        ));
        lines.push(format!(
            "window: {} of {} events (seq {}..{})",
            summary
                .get("scanned_events")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("total_events")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("window_start_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            summary
                .get("window_end_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
    }
    if value
        .get("other_failures")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        lines.push(format!(
            "other failures in window: {} (not attributed by this report)",
            value
                .get("other_failures")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
    }
    if let Some(assignments) = value.get("assignments").and_then(Value::as_array)
        && !assignments.is_empty()
    {
        let identity = assignments
            .iter()
            .take(4)
            .map(|assignment| {
                format!(
                    "{} [{}] {}",
                    assignment
                        .get("identity_id")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    assignment
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    assignment
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "assignment context (not proof of a historical run identity): {identity}"
        ));
    }
    if let Some(warnings) = value.get("warnings").and_then(Value::as_array) {
        for warning in warnings.iter().take(8).filter_map(Value::as_str) {
            lines.push(format!("  warning: {}", bounded_line(warning, 220)));
        }
    }
    lines.push(
        "advisory only: this report never retries, cancels, merges, reassigns, or changes budgets, capacity, approvals, or plans"
            .to_string(),
    );
    Some(lines)
}

fn bounded_line(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect::<String>() + "..."
    }
}

fn short_hash_str(value: &str) -> String {
    value.chars().take(12).collect()
}

pub(crate) fn runtime_pin_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(status) = value.get("status").and_then(Value::as_str)
        && value.get("snapshot_age_secs").is_some()
        && value.get("checked_at").is_some()
    {
        let mut lines = vec![format!(
            "compatibility: {}  target={}  plan_revision={}  snapshot_age={}s",
            status.to_uppercase(),
            value.get("target").and_then(Value::as_str).unwrap_or("?"),
            value
                .get("plan_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("snapshot_age_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        )];
        if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
            lines.push(format!("run: {run_id}"));
        }
        if let Some(changed) = value.get("changed").and_then(Value::as_array) {
            for change in changed.iter().take(16) {
                lines.push(format!(
                    "  {}: {} -> {} ({})",
                    change.get("field").and_then(Value::as_str).unwrap_or("?"),
                    change.get("stored").and_then(Value::as_str).unwrap_or(""),
                    change.get("current").and_then(Value::as_str).unwrap_or(""),
                    change
                        .get("severity")
                        .and_then(Value::as_str)
                        .unwrap_or("drift")
                ));
            }
        }
        if let Some(warnings) = value.get("warnings").and_then(Value::as_array) {
            for warning in warnings.iter().take(16) {
                if let Some(warning) = warning.as_str() {
                    lines.push(format!("  warning: {warning}"));
                }
            }
        }
        if value
            .get("migration_required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            lines.push(
                "migration required: review the diff, then run `vp plan-migrate <session> --yes`"
                    .to_string(),
            );
        }
        return Some(lines);
    }
    if value.get("migrated").is_some() {
        let mut lines = vec![format!(
            "plan migration: migrated={}  revision={}  event_seq={}",
            value
                .get("migrated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            value
                .get("plan_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value.get("event_seq").and_then(Value::as_u64).unwrap_or(0)
        )];
        if let Some(snapshot) = value.get("snapshot") {
            lines.push(format!(
                "snapshot: runtime={} build={} policy={} bundle={} layout={} contract={}",
                snapshot
                    .get("runtime_version")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                snapshot
                    .get("runtime_build")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                short_optional_hash(snapshot.get("policy_hash").and_then(Value::as_str)),
                short_optional_hash(snapshot.get("tool_bundle_hash").and_then(Value::as_str)),
                snapshot
                    .get("prompt_layout_version")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                short_optional_hash(snapshot.get("contract_hash").and_then(Value::as_str)),
            ));
        }
        if let Some(warnings) = value.get("warnings").and_then(Value::as_array) {
            for warning in warnings.iter().take(16) {
                if let Some(warning) = warning.as_str() {
                    lines.push(format!("  note: {warning}"));
                }
            }
        }
        return Some(lines);
    }
    None
}

fn short_optional_hash(value: Option<&str>) -> String {
    value
        .map(|value| value.chars().take(12).collect::<String>())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

pub(crate) fn controller_replay_lines(value: &Value) -> Option<Vec<String>> {
    if !value
        .get("controller_equivalent_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let mut lines = vec![format!(
        "controller replay: matched={} mismatched={} skipped={} examined={} total={} truncated={}",
        value.get("matched").and_then(Value::as_u64).unwrap_or(0),
        value.get("mismatched").and_then(Value::as_u64).unwrap_or(0),
        value.get("skipped").and_then(Value::as_u64).unwrap_or(0),
        value.get("examined").and_then(Value::as_u64).unwrap_or(0),
        value
            .get("total_recorded")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        value
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    )];
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        for entry in entries.iter().take(200) {
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if status == "matched" {
                continue;
            }
            lines.push(format!(
                "  #{} {} {} recorded={} recomputed={}",
                entry.get("seq").and_then(Value::as_u64).unwrap_or(0),
                entry.get("kind").and_then(Value::as_str).unwrap_or("?"),
                status.to_uppercase(),
                entry
                    .get("recorded_action")
                    .and_then(Value::as_str)
                    .unwrap_or("-"),
                entry
                    .get("recomputed_action")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
            ));
            if let Some(changes) = entry.get("changes").and_then(Value::as_array) {
                for change in changes.iter().take(6) {
                    lines.push(format!(
                        "      {}: {} -> {}",
                        change.get("field").and_then(Value::as_str).unwrap_or("?"),
                        change.get("recorded").and_then(Value::as_str).unwrap_or(""),
                        change
                            .get("recomputed")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    ));
                }
            }
            if let Some(reason) = entry.get("reason").and_then(Value::as_str) {
                lines.push(format!("      reason: {reason}"));
            }
        }
    }
    lines.push(
        "controller-equivalent only; replay never proves model behavior and is not verification or merge evidence"
            .to_string(),
    );
    Some(lines)
}

pub(crate) fn gate_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(gate) = value.get("gate")
        && gate.is_object()
    {
        let mut lines = vec![format!(
            "gate {}  {}  {}  {}",
            gate.get("id").and_then(Value::as_str).unwrap_or("?"),
            gate.get("kind").and_then(Value::as_str).unwrap_or("?"),
            gate.get("state").and_then(Value::as_str).unwrap_or("?"),
            gate.get("label").and_then(Value::as_str).unwrap_or("")
        )];
        if let Some(outcome) = gate.get("outcome").and_then(Value::as_str) {
            lines.push(format!("outcome: {outcome}"));
        }
        if let Some(task) = gate.get("task_id").and_then(Value::as_str) {
            lines.push(format!("task: {task}"));
        }
        if let Some(expires) = gate.get("expires_at").and_then(Value::as_str) {
            lines.push(format!("expires: {expires}"));
        }
        if let Some(note) = gate.get("resolution_note").and_then(Value::as_str) {
            lines.push(format!("note: {note}"));
        }
        if let Some(changed) = value.get("changed").and_then(Value::as_bool) {
            lines.push(format!("changed: {changed}"));
        }
        if let Some(continuation) = value.get("continuation_id").and_then(Value::as_str) {
            lines.push(format!("continuation: {continuation}"));
        }
        if let Some(status) = value.get("continuation_status").and_then(Value::as_str) {
            lines.push(format!("continuation status: {status}"));
        }
        if let Some(token) = value.get("token").and_then(Value::as_str) {
            lines.push(String::new());
            lines.push("ONE-TIME GATE TOKEN (shown once; never stored in plaintext):".to_string());
            lines.push(token.to_string());
            if let Some(hint) = value.get("resolve_hint").and_then(Value::as_str) {
                lines.push(format!("resolve: {hint}"));
            }
            if value
                .get("loopback_resolve")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                lines.push(
                    "loopback resolve is enabled for this project's same-host CI (scoped token still required)"
                        .to_string(),
                );
            }
        }
        return Some(lines);
    }
    if let Some(gates) = value.get("gates").and_then(Value::as_array) {
        let counts = value.get("counts").cloned().unwrap_or_else(|| json!({}));
        let mut lines = vec![format!(
            "gates: {} total · {} open · {} resolved · {} expired · {} cancelled{}",
            counts.get("total").and_then(Value::as_u64).unwrap_or(0),
            counts.get("open").and_then(Value::as_u64).unwrap_or(0),
            counts.get("resolved").and_then(Value::as_u64).unwrap_or(0),
            counts.get("expired").and_then(Value::as_u64).unwrap_or(0),
            counts.get("cancelled").and_then(Value::as_u64).unwrap_or(0),
            if value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                " (truncated)"
            } else {
                ""
            },
        )];
        for gate in gates {
            lines.push(format!(
                "{}  {}  {}  {}{}",
                gate.get("id").and_then(Value::as_str).unwrap_or("?"),
                gate.get("state").and_then(Value::as_str).unwrap_or("?"),
                gate.get("kind").and_then(Value::as_str).unwrap_or("?"),
                gate.get("label").and_then(Value::as_str).unwrap_or(""),
                gate.get("outcome")
                    .and_then(Value::as_str)
                    .map(|outcome| format!(" → {outcome}"))
                    .unwrap_or_default()
            ));
        }
        return Some(lines);
    }
    if let Some(expired) = value.get("expired_gates").and_then(Value::as_u64) {
        return Some(vec![format!(
            "gate sweep: {expired} open gate(s) expired and their waits woken"
        )]);
    }
    if let Some(policy) = value.get("policy")
        && policy.get("loopback_resolve").is_some()
        && let Some(project) = value.get("project_id").and_then(Value::as_str)
    {
        return Some(vec![format!(
            "project {project}  gates loopback_resolve={}",
            policy
                .get("loopback_resolve")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )]);
    }
    None
}

pub(crate) fn agent_memory_lines(value: &Value) -> Option<Vec<String>> {
    let line = |memory: &Value| {
        let id = memory["id"].as_str().unwrap_or("?");
        let scope = memory["scope"].as_str().unwrap_or("?");
        let status = if memory["expired"].as_bool().unwrap_or(false) {
            "expired"
        } else {
            memory["status"].as_str().unwrap_or("?")
        };
        let source = memory["source"].as_str().unwrap_or("?");
        let content = memory["content"].as_str().unwrap_or("");
        let evidence = memory["evidence"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let kind = row["kind"].as_str()?;
                        let reference = row["reference"].as_str()?;
                        Some(format!("{kind} {reference}"))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let provenance = if evidence.is_empty() {
            String::new()
        } else {
            format!(" · {}", evidence.join(", "))
        };
        format!("{id}  {scope:<10}  {status:<10}  {source:<12}  {content}{provenance}")
    };
    if let Some(memories) = value.get("memories").and_then(Value::as_array)
        && memories.iter().all(|memory| memory.get("scope").is_some())
    {
        if memories.is_empty() {
            return Some(vec!["No identity memories.".to_string()]);
        }
        let mut lines = Vec::with_capacity(memories.len() + 1);
        lines.push(format!(
            "{} identity memories (total {})",
            memories.len(),
            value["total"].as_u64().unwrap_or(memories.len() as u64)
        ));
        lines.extend(memories.iter().map(line));
        return Some(lines);
    }
    if let Some(memory) = value
        .get("memory")
        .filter(|memory| memory.get("scope").is_some())
    {
        let suffix = if value["changed"].as_bool().unwrap_or(false) {
            " (changed)"
        } else {
            " (unchanged)"
        };
        return Some(vec![format!("{}{}", line(memory), suffix)]);
    }
    if let (Some(previous), Some(replacement)) = (
        value
            .get("previous")
            .filter(|memory| memory.get("scope").is_some()),
        value
            .get("replacement")
            .filter(|memory| memory.get("scope").is_some()),
    ) {
        return Some(vec![
            format!("superseded {}", line(previous)),
            format!("replacement {}", line(replacement)),
        ]);
    }
    if value.get("memory_id").is_some()
        && value.get("pruned").is_some()
        && value.get("changed").is_some()
    {
        return Some(vec![format!(
            "{}  {}",
            value["memory_id"].as_str().unwrap_or("?"),
            if value["pruned"].as_bool().unwrap_or(false) {
                "pruned"
            } else if value["changed"].as_bool().unwrap_or(false) {
                "forgotten (stale)"
            } else {
                "unchanged"
            }
        )]);
    }
    if value.get("expired").is_some()
        && value.get("pruned").is_some()
        && value.get("remaining").is_some()
    {
        return Some(vec![format!(
            "expired {} · pruned {} · remaining {}",
            value["expired"].as_u64().unwrap_or(0),
            value["pruned"].as_u64().unwrap_or(0),
            value["remaining"].as_u64().unwrap_or(0)
        )]);
    }
    None
}

fn team_template_lines(value: &Value) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    if let Some(markdown) = value.get("markdown").and_then(Value::as_str)
        && value.get("entry").is_some()
    {
        lines.extend(markdown.lines().map(str::to_string));
        return Some(lines);
    }
    if let Some(templates) = value.get("templates").and_then(Value::as_array) {
        if templates.is_empty() {
            lines.push("No team templates.".to_string());
            return Some(lines);
        }
        for entry in templates {
            lines.push(format!(
                "{:<28} {:<10} {:<6} {:<10} {}",
                entry["template_id"].as_str().unwrap_or("?"),
                entry["version"].as_str().unwrap_or("?"),
                if entry["applied"].as_bool().unwrap_or(false) {
                    "applied"
                } else {
                    "library"
                },
                short_hash(&entry["content_hash"]),
                entry["name"].as_str().unwrap_or("")
            ));
            if entry["read_only"].as_bool().unwrap_or(false) {
                lines.push("  read-only until explicit apply".to_string());
            }
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            lines.push("… truncated".to_string());
        }
        return Some(lines);
    }
    if let Some(preview) = value.get("preview") {
        let name = preview["name"].as_str().unwrap_or("?");
        let version = preview["version"].as_str().unwrap_or("?");
        lines.push(format!(
            "{name} v{version}  hash {}  apply {}",
            short_hash(&preview["content_hash"]),
            if preview["apply_allowed"].as_bool().unwrap_or(false) {
                "allowed"
            } else {
                "blocked"
            }
        ));
        if preview["leader_overlay"].as_bool().unwrap_or(false) {
            lines.push("  Leader: existing overlay (never overwritten)".to_string());
        }
        for line in preview["proposed_org"].as_array().into_iter().flatten() {
            if let Some(line) = line.as_str() {
                lines.push(format!("  {line}"));
            }
        }
        for conflict in preview["conflicts"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  conflict {}: {}",
                conflict["slot"].as_str().unwrap_or("?"),
                conflict["message"].as_str().unwrap_or("")
            ));
        }
        for gap in preview["model_gaps"].as_array().into_iter().flatten() {
            lines.push(format!("  model gap: {}", gap.as_str().unwrap_or("")));
        }
        for gap in preview["tool_gaps"].as_array().into_iter().flatten() {
            lines.push(format!("  tool gap: {}", gap.as_str().unwrap_or("")));
        }
        for conflict in preview["policy_conflicts"].as_array().into_iter().flatten() {
            lines.push(format!("  policy: {}", conflict.as_str().unwrap_or("")));
        }
        for warning in preview["warnings"].as_array().into_iter().flatten() {
            lines.push(format!("  warning: {}", warning.as_str().unwrap_or("")));
        }
        if value["applied"].as_bool().unwrap_or(false) {
            lines.push(format!(
                "  applied {} pending hire request(s)",
                value["pending_hire_request_ids"]
                    .as_array()
                    .map_or(0, Vec::len)
            ));
        }
        return Some(lines);
    }
    if let Some(rows) = value.get("rows").and_then(Value::as_array) {
        lines.push(format!(
            "diff {}  template {}  hash {}",
            value["project_id"].as_str().unwrap_or("?"),
            value["template_id"].as_str().unwrap_or("?"),
            short_hash(&value["content_hash"])
        ));
        if rows.is_empty() {
            lines.push("  no differences".to_string());
        }
        for row in rows {
            lines.push(format!(
                "  {:<14} {:<24} {}",
                row["kind"].as_str().unwrap_or("?"),
                row["slot"].as_str().unwrap_or("?"),
                row["detail"].as_str().unwrap_or("")
            ));
        }
        return Some(lines);
    }
    if let Some(template) = value.get("template")
        && value.get("json").is_some()
    {
        lines.push(format!(
            "exported {} v{}  hash {}",
            template["template_id"].as_str().unwrap_or("?"),
            template["version"].as_str().unwrap_or("?"),
            short_hash(&template["content_hash"])
        ));
        if let Some(path) = value.get("json_path").and_then(Value::as_str) {
            lines.push(format!("  json     {path}"));
        }
        if let Some(path) = value.get("markdown_path").and_then(Value::as_str) {
            lines.push(format!("  markdown {path}"));
        }
        for warning in value["warnings"].as_array().into_iter().flatten() {
            lines.push(format!("  warning: {}", warning.as_str().unwrap_or("")));
        }
        return Some(lines);
    }
    if value.get("template_id").is_some()
        && value.get("json").is_some()
        && value.get("out").is_none()
    {
        lines.push(format!(
            "exported {} v{}  hash {}",
            value["template_id"].as_str().unwrap_or("?"),
            value["version"].as_str().unwrap_or("?"),
            short_hash(&value["content_hash"])
        ));
        if let Some(path) = value.get("json_path").and_then(Value::as_str) {
            lines.push(format!("  json     {path}"));
        }
        return Some(lines);
    }
    if value.get("repaired_links").is_some() && value.get("mismatches").is_some() {
        lines.push(format!(
            "template replay {}  events {} · templates {} · links {} · repaired {}",
            value["project_id"].as_str().unwrap_or("?"),
            value["events"].as_u64().unwrap_or(0),
            value["templates"].as_u64().unwrap_or(0),
            value["links"].as_u64().unwrap_or(0),
            value["repaired_links"].as_u64().unwrap_or(0)
        ));
        for mismatch in value["mismatches"].as_array().into_iter().flatten() {
            lines.push(format!("  mismatch: {}", mismatch.as_str().unwrap_or("")));
        }
        return Some(lines);
    }
    None
}

pub(crate) fn agent_org_lines(value: &Value) -> Option<Vec<String>> {
    let counts = value.get("counts")?;
    counts.get("identities")?;
    let mut lines = Vec::new();
    let project_id = value["project_id"].as_str().unwrap_or("?");
    let enabled = value["enabled"].as_bool().unwrap_or(false);
    lines.push(format!(
        "Project org {project_id} · {} identities · {} leaders · {} managers · {} workers · policy {}",
        counts["identities"].as_u64().unwrap_or(0),
        counts["leaders"].as_u64().unwrap_or(0),
        counts["managers"].as_u64().unwrap_or(0),
        counts["workers"].as_u64().unwrap_or(0),
        if enabled { "enabled" } else { "disabled" }
    ));
    lines.push(format!(
        "status active {} · idle {} · paused {} · retired {} · imported {} · assignments {} · workspaces {} · unread {} · memory {}",
        counts["active"].as_u64().unwrap_or(0),
        counts["idle"].as_u64().unwrap_or(0),
        counts["paused"].as_u64().unwrap_or(0),
        counts["retired"].as_u64().unwrap_or(0),
        counts["imported"].as_u64().unwrap_or(0),
        counts["active_assignments"].as_u64().unwrap_or(0),
        counts["active_workspaces"].as_u64().unwrap_or(0),
        counts["unread_inbox"].as_u64().unwrap_or(0),
        counts["memory_count"].as_u64().unwrap_or(0)
    ));
    lines.push(format!(
        "pending hires {} · pending retirement proposals {} · advisory candidates {}",
        counts["pending_hire_requests"].as_u64().unwrap_or(0),
        counts["pending_retirement_proposals"].as_u64().unwrap_or(0),
        counts["retirement_candidates"].as_u64().unwrap_or(0)
    ));
    if let Some(leader) = value.get("leader").filter(|row| !row.is_null()) {
        lines.push("Leader".to_string());
        agent_org_node_lines(leader, 1, &mut lines);
    } else {
        lines.push("Leader: none".to_string());
    }
    if let Some(unassigned) = value["unassigned"].as_array()
        && !unassigned.is_empty()
    {
        lines.push(format!(
            "Unassigned {} (no parent in the active hierarchy)",
            unassigned.len()
        ));
        for node in unassigned {
            agent_org_node_lines(node, 1, &mut lines);
        }
    }
    if let Some(departments) = value["departments"].as_array() {
        for department in departments {
            lines.push(format!(
                "Department {} · {} managers · {} workers · {} active · unread {}",
                department["name"].as_str().unwrap_or("?"),
                department["manager_count"].as_u64().unwrap_or(0),
                department["worker_count"].as_u64().unwrap_or(0),
                department["active_count"].as_u64().unwrap_or(0),
                department["unread_inbox"].as_u64().unwrap_or(0)
            ));
        }
    }
    if let Some(requests) = value["pending_hire_requests"].as_array()
        && !requests.is_empty()
    {
        lines.push(format!("Pending hire requests {}", requests.len()));
        for request in requests {
            lines.push(format!(
                "  {} · {} · {} · requested {}",
                request["id"].as_str().unwrap_or("?"),
                request["role"].as_str().unwrap_or("?"),
                request["title"]
                    .as_str()
                    .or_else(|| request["component"].as_str())
                    .unwrap_or(""),
                request["requested_at"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(proposals) = value["pending_retirement_proposals"].as_array()
        && !proposals.is_empty()
    {
        lines.push(format!("Pending retirement proposals {}", proposals.len()));
        for proposal in proposals {
            lines.push(format!(
                "  {} · identity {} · {} · proposed {}",
                proposal["id"].as_str().unwrap_or("?"),
                proposal["identity_id"].as_str().unwrap_or("?"),
                proposal["reason_code"].as_str().unwrap_or("?"),
                proposal["proposed_at"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(assignments) = value["active_assignments"].as_array()
        && !assignments.is_empty()
    {
        lines.push(format!("Active assignments {}", assignments.len()));
        for assignment in assignments {
            lines.push(format!(
                "session {} · identity {} · {} · lease {}",
                assignment["session_id"].as_str().unwrap_or("?"),
                assignment["identity_id"].as_str().unwrap_or("?"),
                assignment["status"].as_str().unwrap_or("?"),
                if assignment["lease_active"].as_bool().unwrap_or(false) {
                    "active"
                } else {
                    "expired"
                }
            ));
        }
    }
    for warning in value["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            lines.push(format!("warning: {warning}"));
        }
    }
    Some(lines)
}

fn agent_org_node_lines(node: &Value, depth: usize, lines: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    let identity = &node["identity"];
    let mut detail = vec![format!(
        "{}{} · {} · {} · gen {}",
        indent,
        identity["title"]
            .as_str()
            .or_else(|| identity["component"].as_str())
            .unwrap_or_else(|| identity["role"].as_str().unwrap_or("?")),
        identity["role"].as_str().unwrap_or("?"),
        identity["status"].as_str().unwrap_or("?"),
        identity["generation"].as_u64().unwrap_or(0)
    )];
    if let Some(department) = identity["department"].as_str() {
        detail.push(format!("department {department}"));
    }
    if let Some(component) = identity["component"].as_str() {
        detail.push(format!("component {component}"));
    }
    if identity["origin"].as_str() == Some("imported") {
        detail.push("imported/read-only".to_string());
    }
    if let Some(assignment) = node.get("active_assignment").filter(|row| !row.is_null()) {
        detail.push(format!(
            "session {} (lease {})",
            assignment["session_id"].as_str().unwrap_or("?"),
            if assignment["lease_active"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "expired"
            }
        ));
    }
    if let Some(workspace) = node.get("workspace").filter(|row| !row.is_null()) {
        detail.push(format!(
            "workspace {} · {}",
            workspace["component"].as_str().unwrap_or("?"),
            workspace["status"].as_str().unwrap_or("?")
        ));
    }
    let unread = node["unread_inbox"].as_u64().unwrap_or(0);
    let memory = node["memory_count"].as_u64().unwrap_or(0);
    if unread > 0 || memory > 0 {
        detail.push(format!("inbox {unread} · memory {memory}"));
    }
    let performance = &node["performance"];
    let completed = performance["completed_tasks"].as_u64().unwrap_or(0);
    let failed = performance["failed_tasks"].as_u64().unwrap_or(0);
    let approvals = performance["verifier_approvals"].as_u64().unwrap_or(0);
    let rejections = performance["verifier_rejections"].as_u64().unwrap_or(0);
    if completed > 0 || failed > 0 || approvals > 0 || rejections > 0 {
        detail.push(format!(
            "tasks {completed}/{failed} · verifier {approvals}/{rejections}"
        ));
    }
    let retirement = &node["retirement"];
    if retirement["candidate"].as_bool().unwrap_or(false) {
        detail.push(format!(
            "retirement candidate severity {}",
            retirement["severity"].as_u64().unwrap_or(0)
        ));
    }
    if retirement["pending_proposals"].as_u64().unwrap_or(0) > 0 {
        detail.push("retirement proposal pending".to_string());
    }
    lines.push(detail.join(" · "));
    for report in node["reports"].as_array().into_iter().flatten() {
        agent_org_node_lines(report, depth + 1, lines);
    }
}

pub(crate) fn agent_detail_lines(value: &Value) -> Option<Vec<String>> {
    let identity = value.get("identity")?;
    if identity.get("project_id").is_none() || value.get("node").is_none() {
        return None;
    }
    let mut lines = Vec::new();
    let mut header = vec![format!(
        "{} · {} · {} · gen {}",
        identity["title"]
            .as_str()
            .or_else(|| identity["component"].as_str())
            .unwrap_or_else(|| identity["role"].as_str().unwrap_or("?")),
        identity["role"].as_str().unwrap_or("?"),
        identity["status"].as_str().unwrap_or("?"),
        identity["generation"].as_u64().unwrap_or(0)
    )];
    header.push(format!("id {}", identity["id"].as_str().unwrap_or("?")));
    if let Some(department) = identity["department"].as_str() {
        header.push(format!("department {department}"));
    }
    if let Some(component) = identity["component"].as_str() {
        header.push(format!("component {component}"));
    }
    if let Some(parent) = identity["parent_identity_id"].as_str() {
        header.push(format!("parent {parent}"));
    }
    if let Some(successor) = identity["successor_id"].as_str() {
        header.push(format!("successor {successor}"));
    }
    if value["read_only"].as_bool().unwrap_or(false) {
        header.push("read-only".to_string());
    }
    lines.push(header.join(" · "));
    if let Some(reason) = value["read_only_reason"].as_str() {
        lines.push(format!("read-only: {reason}"));
    }
    let node = &value["node"];
    if let Some(assignment) = node.get("active_assignment").filter(|row| !row.is_null()) {
        lines.push(format!(
            "active assignment session {} · lease {} · {} assignments / {} sessions",
            assignment["session_id"].as_str().unwrap_or("?"),
            if assignment["lease_active"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "expired"
            },
            assignment["assignment_count"].as_u64().unwrap_or(0),
            assignment["session_count"].as_u64().unwrap_or(0)
        ));
    } else {
        lines.push("active assignment: none".to_string());
    }
    if let Some(workspace) = node.get("workspace").filter(|row| !row.is_null()) {
        lines.push(format!(
            "workspace {} · {} · branch {} · lease {}",
            workspace["component"].as_str().unwrap_or("?"),
            workspace["status"].as_str().unwrap_or("?"),
            workspace["branch"].as_str().unwrap_or("?"),
            if workspace["lease_active"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "idle"
            }
        ));
    }
    lines.push(format!(
        "inbox unread {} / read {} / done {} · memory {}/{}",
        value["inbox_counts"]["unread"].as_u64().unwrap_or(0),
        value["inbox_counts"]["read"].as_u64().unwrap_or(0),
        value["inbox_counts"]["done"].as_u64().unwrap_or(0),
        value["memories"].as_array().map(Vec::len).unwrap_or(0),
        value["memory_total"].as_u64().unwrap_or(0)
    ));
    let performance = &value["performance"];
    lines.push(format!(
        "performance completed {} · failed {} · consecutive {} · verifier {}/{} · tokens {}/{} · cost ${:.3}",
        performance["completed_tasks"].as_u64().unwrap_or(0),
        performance["failed_tasks"].as_u64().unwrap_or(0),
        performance["consecutive_failures"].as_u64().unwrap_or(0),
        performance["verifier_approvals"].as_u64().unwrap_or(0),
        performance["verifier_rejections"].as_u64().unwrap_or(0),
        performance["input_tokens"].as_u64().unwrap_or(0),
        performance["output_tokens"].as_u64().unwrap_or(0),
        performance["cost_usd"].as_f64().unwrap_or(0.0)
    ));
    if let Some(assignments) = value["assignments"].as_array()
        && !assignments.is_empty()
    {
        lines.push(format!("Assignments {}", assignments.len()));
        for assignment in assignments {
            lines.push(format!(
                "  session {} · {} · assigned {} · lease {}",
                assignment["session_id"].as_str().unwrap_or("?"),
                assignment["status"].as_str().unwrap_or("?"),
                assignment["assigned_at"].as_str().unwrap_or("?"),
                if assignment["lease_active"].as_bool().unwrap_or(false) {
                    "active"
                } else {
                    "expired"
                }
            ));
        }
    }
    if let Some(inbox) = value["inbox"].as_array()
        && !inbox.is_empty()
    {
        lines.push(format!("Inbox {}", inbox.len()));
        for message in inbox {
            let mut row = vec![format!(
                "  {} · {} · {} · from {}",
                message["kind"].as_str().unwrap_or("?"),
                message["status"].as_str().unwrap_or("?"),
                message["created_at"].as_str().unwrap_or("?"),
                message["from_label"].as_str().unwrap_or("?")
            )];
            if let Some(summary) = message["summary"].as_str() {
                row.push(summary.to_string());
            }
            lines.push(row.join(" · "));
        }
    }
    if let Some(memories) = value["memories"].as_array()
        && !memories.is_empty()
    {
        lines.push(format!("Memory {}", memories.len()));
        for memory in memories {
            lines.push(format!(
                "  {} · {} · {} · {}",
                memory["scope"].as_str().unwrap_or("?"),
                memory["status"].as_str().unwrap_or("?"),
                memory["created_at"].as_str().unwrap_or("?"),
                memory["content"].as_str().unwrap_or("")
            ));
        }
    }
    if let Some(candidates) = value["retirement_candidates"].as_array()
        && !candidates.is_empty()
    {
        lines.push("Retirement candidates (advisory, explicit approval required)".to_string());
        for candidate in candidates {
            lines.push(format!(
                "  severity {} · {}",
                candidate["severity"].as_u64().unwrap_or(0),
                candidate["reasons"]
                    .as_array()
                    .map(|reasons| reasons
                        .iter()
                        .map(|reason| format!(
                            "{} {}/{}",
                            reason["code"].as_str().unwrap_or("?"),
                            reason["count"].as_u64().unwrap_or(0),
                            reason["threshold"].as_u64().unwrap_or(0)
                        ))
                        .collect::<Vec<_>>()
                        .join(" · "))
                    .unwrap_or_default()
            ));
        }
    }
    if let Some(proposals) = value["retirement_proposals"].as_array()
        && !proposals.is_empty()
    {
        lines.push("Retirement proposals".to_string());
        for proposal in proposals {
            lines.push(format!(
                "  {} · {} · {} · reason {}",
                proposal["id"].as_str().unwrap_or("?"),
                proposal["status"].as_str().unwrap_or("?"),
                proposal["proposed_at"].as_str().unwrap_or("?"),
                proposal["reason_code"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(requests) = value["hire_requests"].as_array()
        && !requests.is_empty()
    {
        lines.push("Hire requests".to_string());
        for request in requests {
            lines.push(format!(
                "  {} · {} · {} · role {}",
                request["id"].as_str().unwrap_or("?"),
                request["status"].as_str().unwrap_or("?"),
                request["requested_at"].as_str().unwrap_or("?"),
                request["role"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(events) = value["employment_events"].as_array()
        && !events.is_empty()
    {
        lines.push("Employment events".to_string());
        for event in events {
            lines.push(format!(
                "  {} · {} · {}",
                event["kind"].as_str().unwrap_or("?"),
                event["created_at"].as_str().unwrap_or("?"),
                event["id"].as_str().unwrap_or("?")
            ));
        }
    }
    for warning in value["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            lines.push(format!("warning: {warning}"));
        }
    }
    Some(lines)
}

pub(crate) fn agent_identity_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(identities) = value.get("identities").and_then(Value::as_array)
        && identities
            .iter()
            .all(|identity| identity.get("project_id").is_some())
    {
        if identities.is_empty() {
            return Some(vec!["No project identities.".to_string()]);
        }
        let mut lines = Vec::with_capacity(identities.len() + 1);
        lines.push(format!("{} project identities", identities.len()));
        for identity in identities {
            let id = identity["id"].as_str().unwrap_or("?");
            let role = identity["role"].as_str().unwrap_or("?");
            let status = identity["status"].as_str().unwrap_or("?");
            let generation = identity["generation"].as_u64().unwrap_or(0);
            let mut detail = vec![format!("{id}  {role:<7}  {status:<7}  gen {generation}")];
            if let Some(component) = identity["component"].as_str() {
                detail.push(format!("component {component}"));
            }
            if let Some(department) = identity["department"].as_str() {
                detail.push(format!("department {department}"));
            }
            if let Some(title) = identity["title"].as_str() {
                detail.push(format!("title {title}"));
            }
            if let Some(parent) = identity["parent_identity_id"].as_str() {
                detail.push(format!("parent {parent}"));
            }
            if let Some(successor) = identity["successor_id"].as_str() {
                detail.push(format!("successor {successor}"));
            }
            if identity["origin"].as_str() == Some("imported") {
                detail.push("imported/read-only".to_string());
            }
            lines.push(detail.join(" · "));
        }
        return Some(lines);
    }
    if let Some(identity) = value
        .get("identity")
        .filter(|identity| identity.get("project_id").is_some())
    {
        let changed = if value["changed"].as_bool().unwrap_or(false) {
            " (changed)"
        } else {
            " (unchanged)"
        };
        let mut lines = vec![format!(
            "{}  {}{}",
            identity["id"].as_str().unwrap_or("?"),
            identity["status"].as_str().unwrap_or("?"),
            changed
        )];
        lines.push(
            agent_identity_lines(&json!({"identities": [identity]}))
                .and_then(|lines| lines.into_iter().nth(1))
                .unwrap_or_default(),
        );
        return Some(lines);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_managers").is_some())
    {
        return Some(vec![format!(
            "identities {} · managers ≤ {} · workers ≤ {} · hire approval {} · retirement approval {}",
            if policy["enabled"].as_bool().unwrap_or(false) {
                "enabled"
            } else {
                "disabled"
            },
            policy["max_managers"].as_u64().unwrap_or(0),
            policy["max_workers"].as_u64().unwrap_or(0),
            policy["hire_requires_approval"].as_bool().unwrap_or(true),
            policy["retirement_requires_approval"]
                .as_bool()
                .unwrap_or(true),
        )]);
    }
    None
}

pub(crate) fn coordination_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(assignments) = value.get("assignments").and_then(Value::as_array) {
        if assignments.is_empty() {
            return Some(vec!["No session assignments.".to_string()]);
        }
        let mut lines = Vec::with_capacity(assignments.len() + 1);
        lines.push(format!("{} session assignments", assignments.len()));
        for assignment in assignments {
            let latest = assignment["status"].as_str().unwrap_or("?");
            let lease = assignment["lease_expires_at"]
                .as_str()
                .map(|expires| format!(" · lease {expires}"))
                .unwrap_or_default();
            let active = if assignment["lease_active"].as_bool().unwrap_or(false) {
                " · live"
            } else {
                ""
            };
            lines.push(format!(
                "{}  {}  {}  {}{}{}",
                assignment["session_id"].as_str().unwrap_or("?"),
                assignment["identity_id"].as_str().unwrap_or("?"),
                assignment["role"].as_str().unwrap_or("?"),
                latest,
                active,
                lease
            ));
        }
        return Some(lines);
    }
    if let Some(assignment) = value.get("assignment")
        && (value.get("created").is_some()
            || value.get("released").is_some()
            || value.get("renewed").is_some())
    {
        let changed = if value["created"].as_bool().unwrap_or(false) {
            "created"
        } else if value["released"].as_bool().unwrap_or(false) {
            "released"
        } else if value["renewed"].as_bool().unwrap_or(false) {
            "renewed"
        } else {
            "unchanged"
        };
        let mut lines = vec![format!(
            "{}  {}  {}  {changed}",
            assignment["session_id"].as_str().unwrap_or("?"),
            assignment["identity_id"].as_str().unwrap_or("?"),
            assignment["status"].as_str().unwrap_or("?")
        )];
        if let Some(wake) = value.get("wake")
            && wake["requested"].as_bool().unwrap_or(false)
        {
            lines.push(format!(
                "wake: {} ({})",
                if wake["created"].as_bool().unwrap_or(false) {
                    "deferred continuation created"
                } else {
                    "not created"
                },
                wake["reason"].as_str().unwrap_or("unknown")
            ));
        }
        return Some(lines);
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array)
        && value.get("counts").is_some()
    {
        let counts = &value["counts"];
        let mut lines = vec![format!(
            "{} inbox rows · unread {} · read {} · done {} · expired {}",
            messages.len(),
            counts["unread"].as_u64().unwrap_or(0),
            counts["read"].as_u64().unwrap_or(0),
            counts["done"].as_u64().unwrap_or(0),
            counts["expired"].as_u64().unwrap_or(0)
        )];
        for message in messages {
            let summary = message["payload"]
                .get("summary")
                .or_else(|| message["payload"].get("body"))
                .or_else(|| message["payload"].get("title"))
                .and_then(Value::as_str)
                .unwrap_or("");
            lines.push(format!(
                "{}  {}  {}  {}  {}{}",
                message["id"].as_str().unwrap_or("?"),
                message["status"].as_str().unwrap_or("?"),
                message["kind"].as_str().unwrap_or("?"),
                message["from_label"].as_str().unwrap_or("operator"),
                summary,
                message["expires_at"]
                    .as_str()
                    .map(|expires| format!(" · expires {expires}"))
                    .unwrap_or_default()
            ));
        }
        return Some(lines);
    }
    if let Some(message) = value.get("message")
        && value.get("changed").is_some()
        && message.get("identity_id").is_some()
    {
        return Some(vec![format!(
            "{}  {}  {}{}",
            message["id"].as_str().unwrap_or("?"),
            message["status"].as_str().unwrap_or("?"),
            message["kind"].as_str().unwrap_or("?"),
            if value["changed"].as_bool().unwrap_or(false) {
                " (changed)"
            } else {
                " (unchanged)"
            }
        )]);
    }
    if value.get("consistent").is_some()
        && value.get("assignments").is_some()
        && value.get("inbox").is_some()
        && value.get("events").is_some()
    {
        return Some(vec![format!(
            "coordination replay: assignments {} · inbox {} · events {} · {}",
            value["assignments"].as_u64().unwrap_or(0),
            value["inbox"].as_u64().unwrap_or(0),
            value["events"].as_u64().unwrap_or(0),
            if value["consistent"].as_bool().unwrap_or(false) {
                "consistent"
            } else {
                "drift detected"
            }
        )]);
    }
    None
}

fn workspace_line(workspace: &Value) -> String {
    let lease = if workspace["lease_active"].as_bool().unwrap_or(false) {
        " · live lease"
    } else {
        ""
    };
    format!(
        "{}  {}  {}  {}  {}{}",
        workspace["id"].as_str().unwrap_or("?"),
        workspace["identity_id"].as_str().unwrap_or("?"),
        workspace["component"].as_str().unwrap_or("?"),
        workspace["status"].as_str().unwrap_or("?"),
        workspace["branch"].as_str().unwrap_or("?"),
        lease
    )
}

fn workspace_session_line(session: &Value) -> String {
    format!(
        "{}  {}  {}  {}  {}",
        session["session_id"].as_str().unwrap_or("?"),
        session["identity_id"].as_str().unwrap_or("?"),
        session["workspace_id"].as_str().unwrap_or("?"),
        session["status"].as_str().unwrap_or("?"),
        session["branch"].as_str().unwrap_or("?")
    )
}

pub(crate) fn workspace_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(bindings) = value.get("bindings").and_then(Value::as_array) {
        if bindings.is_empty() {
            return Some(vec![format!(
                "session {} has no Worker workspaces.",
                value["session_id"].as_str().unwrap_or("?")
            )]);
        }
        let mut lines = vec![format!(
            "session {} · {} Worker workspace(s)",
            value["session_id"].as_str().unwrap_or("?"),
            bindings.len()
        )];
        lines.extend(bindings.iter().map(workspace_session_line));
        return Some(lines);
    }
    if value.get("landed_commit").is_some() {
        let status = if value["already_up_to_date"].as_bool().unwrap_or(false) {
            "already up to date"
        } else {
            "landed"
        };
        let mut lines = vec![format!(
            "workspace {} · {} · session {}",
            value["workspace"]["id"].as_str().unwrap_or("?"),
            status,
            value["session_id"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "component {} -> {} ({} commit(s))",
            value["previous_component_commit"].as_str().unwrap_or("?"),
            value["landed_commit"].as_str().unwrap_or("?"),
            value["commits"].as_array().map(Vec::len).unwrap_or(0)
        ));
        lines.push(format!(
            "branch {} · worktree {}",
            value["workspace"]["branch"].as_str().unwrap_or("?"),
            value["workspace"]["worktree_path"].as_str().unwrap_or("?")
        ));
        return Some(lines);
    }
    if value.get("workspaces").is_some_and(Value::is_array) {
        let workspaces = value["workspaces"].as_array()?;
        let sessions = value["sessions"].as_array();
        if workspaces.is_empty() && sessions.is_none_or(Vec::is_empty) {
            return Some(vec!["No worker workspaces.".to_string()]);
        }
        let mut lines = vec![format!(
            "{} worker workspace(s){}",
            workspaces.len(),
            sessions
                .map(|rows| format!(" · {} session binding(s)", rows.len()))
                .unwrap_or_default()
        )];
        lines.extend(workspaces.iter().map(workspace_line));
        if let Some(sessions) = sessions {
            lines.extend(sessions.iter().map(workspace_session_line));
        }
        return Some(lines);
    }
    if let Some(workspace) = value.get("workspace")
        && workspace.is_object()
    {
        let mut lines = vec![workspace_line(workspace)];
        if let Some(rows) = value.get("sessions").and_then(Value::as_array) {
            lines.extend(rows.iter().map(workspace_session_line));
        }
        if value.get("created").is_some() || value.get("attached").is_some() {
            lines.push(format!(
                "created {} · attached {}",
                value["created"].as_bool().unwrap_or(false),
                value["attached"].as_bool().unwrap_or(false)
            ));
        }
        if value.get("changed").is_some() {
            lines.push(format!(
                "changed {}",
                value["changed"].as_bool().unwrap_or(false)
            ));
        }
        if let Some(binding) = value.get("session")
            && binding.is_object()
        {
            lines.push(workspace_session_line(binding));
        }
        return Some(lines);
    }
    if value.get("project_id").is_some()
        && value.get("workspaces").is_some()
        && value.get("events").is_some()
    {
        return Some(vec![format!(
            "workspace replay: workspaces {} · sessions {} · events {} · {}",
            value["workspaces"].as_u64().unwrap_or(0),
            value["sessions"].as_u64().unwrap_or(0),
            value["events"].as_u64().unwrap_or(0),
            if value["consistent"].as_bool().unwrap_or(false) {
                "consistent"
            } else {
                "drift detected"
            }
        )]);
    }
    None
}

pub(crate) fn agent_employment_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(requests) = value.get("requests").and_then(Value::as_array) {
        if requests.is_empty() {
            return Some(vec!["No hire requests.".to_string()]);
        }
        let mut lines = vec![format!("{} hire requests", requests.len())];
        for request in requests {
            lines.push(format!(
                "{}  {}  {}  {}  parent {}",
                request["id"].as_str().unwrap_or("?"),
                request["role"].as_str().unwrap_or("?"),
                request["status"].as_str().unwrap_or("?"),
                request["title"]
                    .as_str()
                    .or_else(|| request["component"].as_str())
                    .unwrap_or("-"),
                request["parent_identity_id"].as_str().unwrap_or("-"),
            ));
        }
        return Some(lines);
    }
    if let Some(request) = value
        .get("request")
        .filter(|request| request.get("role").is_some())
    {
        return Some(vec![format!(
            "hire {} · {} · {}{}",
            request["id"].as_str().unwrap_or("?"),
            request["role"].as_str().unwrap_or("?"),
            value["state"].as_str().unwrap_or("pending"),
            value["identity"]["id"]
                .as_str()
                .map(|id| format!(" · identity {id}"))
                .unwrap_or_default(),
        )]);
    }
    if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
        if candidates.is_empty() {
            let state = if value["policy_enabled"].as_bool().unwrap_or(false) {
                "No retirement candidates crossed a configured threshold."
            } else {
                "Retirement policy is disabled; no candidates."
            };
            return Some(vec![state.to_string()]);
        }
        let mut lines = vec![format!("{} retirement candidates", candidates.len())];
        for candidate in candidates {
            let reasons = candidate["reasons"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|reason| {
                    format!(
                        "{} {}/{}",
                        reason["code"].as_str().unwrap_or("?"),
                        reason["count"].as_u64().unwrap_or(0),
                        reason["threshold"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "{}  {}  gen {}  severity {}  {}",
                candidate["identity_id"].as_str().unwrap_or("?"),
                candidate["role"].as_str().unwrap_or("?"),
                candidate["generation"].as_u64().unwrap_or(0),
                candidate["severity"].as_u64().unwrap_or(0),
                reasons
            ));
        }
        return Some(lines);
    }
    if let Some(proposals) = value.get("proposals").and_then(Value::as_array) {
        if proposals.is_empty() {
            return Some(vec!["No retirement proposals.".to_string()]);
        }
        let mut lines = vec![format!("{} retirement proposals", proposals.len())];
        for proposal in proposals {
            lines.push(format!(
                "{}  {}  {}  {}",
                proposal["id"].as_str().unwrap_or("?"),
                proposal["identity_id"].as_str().unwrap_or("?"),
                proposal["status"].as_str().unwrap_or("?"),
                proposal["reason_code"].as_str().unwrap_or("-")
            ));
        }
        return Some(lines);
    }
    if value.get("proposal").is_some() && value.get("reason_code").is_some() {
        return Some(vec![format!(
            "{}  {}  {}",
            value["proposal"]["id"].as_str().unwrap_or("?"),
            value["proposal"]["status"].as_str().unwrap_or("?"),
            value["changed"].as_bool().unwrap_or(false)
        )]);
    }
    if value.get("candidate").is_some() && value.get("proposal").is_some() {
        return Some(vec![format!(
            "proposal {} · {} · {}",
            value["proposal"]["id"].as_str().unwrap_or("?"),
            value["proposal"]["status"].as_str().unwrap_or("?"),
            if value["already_pending"].as_bool().unwrap_or(false) {
                "already pending"
            } else {
                "created"
            }
        )]);
    }
    if value.get("totals").is_some() && value.get("identities").is_some() {
        let totals = &value["totals"];
        let mut lines = vec![format!(
            "{} identities · completed {} · failed {} · verifier {}/{} · tokens {}/{} · ${:.4}",
            totals["identities"].as_u64().unwrap_or(0),
            totals["completed_tasks"].as_u64().unwrap_or(0),
            totals["failed_tasks"].as_u64().unwrap_or(0),
            totals["verifier_approvals"].as_u64().unwrap_or(0),
            totals["verifier_rejections"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        )];
        if value["identities"].as_array().is_some_and(Vec::is_empty) {
            lines.push("No performance rows.".to_string());
        }
        for row in value["identities"].as_array().into_iter().flatten() {
            lines.push(performance_line(row));
        }
        return Some(lines);
    }
    if value.get("completed_tasks").is_some() && value.get("identity_id").is_some() {
        return Some(vec![performance_line(value)]);
    }
    if value.get("previous_role").is_some() && value.get("identity").is_some() {
        return Some(vec![format!(
            "promoted {} → {} · gen {} · released {} assignment(s), archived {} workspace(s)",
            value["previous_role"].as_str().unwrap_or("?"),
            value["identity"]["role"].as_str().unwrap_or("?"),
            value["identity"]["generation"].as_u64().unwrap_or(0),
            value["released_assignments"].as_u64().unwrap_or(0),
            value["archived_workspaces"].as_u64().unwrap_or(0)
        )]);
    }
    if value.get("identity").is_some() && value.get("changed").is_some() {
        return Some(vec![format!(
            "{}  {}  changed {}  released {} assignment(s), archived {} workspace(s)",
            value["identity"]["id"].as_str().unwrap_or("?"),
            value["identity"]["status"].as_str().unwrap_or("?"),
            value["changed"].as_bool().unwrap_or(false),
            value["released_assignments"].as_u64().unwrap_or(0),
            value["archived_workspaces"].as_u64().unwrap_or(0)
        )]);
    }
    None
}

fn performance_line(row: &Value) -> String {
    format!(
        "{}  {}  {}  completed {} · failed {} · consecutive {} · retries {} · verifier {}/{} · evidence {}",
        row["identity_id"].as_str().unwrap_or("?"),
        row["role"].as_str().unwrap_or("?"),
        row["status"].as_str().unwrap_or("?"),
        row["completed_tasks"].as_u64().unwrap_or(0),
        row["failed_tasks"].as_u64().unwrap_or(0),
        row["consecutive_failures"].as_u64().unwrap_or(0),
        row["retries"].as_u64().unwrap_or(0),
        row["verifier_approvals"].as_u64().unwrap_or(0),
        row["verifier_rejections"].as_u64().unwrap_or(0),
        row["evidence"].as_array().map(Vec::len).unwrap_or(0)
    )
}

pub(crate) fn print_human(value: &Value) {
    if let Some(lines) = agent_org_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = agent_detail_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = agent_employment_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = workspace_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = coordination_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = agent_memory_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = agent_identity_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = team_template_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = diagnosis_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = runtime_pin_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = controller_replay_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = gate_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = effect_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = instruction_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = forge_preflight_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = forge_status_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = pull_request_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = [
        archive_list_lines(value),
        archive_extract_lines(value),
        media_info_lines(value),
        media_transcode_lines(value),
    ]
    .into_iter()
    .flatten()
    .next()
    {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = test_cache_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = process_console_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = governor_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = remote_bootstrap_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = remote_test_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = remote_integration_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = lsp_status_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = scaling_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = tool_activity_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(lines) = context_checkpoint_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(programs) = value.get("programs").and_then(Value::as_array) {
        if programs.is_empty() {
            println!("No programs.");
            return;
        }
        for entry in programs {
            let program = entry.get("program").unwrap_or(entry);
            let id = program["id"].as_str().unwrap_or("?");
            let status = program["status"].as_str().unwrap_or("?");
            let goal = program["goal"].as_str().unwrap_or("");
            let counts = &entry["counts"];
            println!(
                "{id}  {status:<10}  {goal}  milestones {}/{} done · {} running · {} blocked",
                counts["completed"].as_u64().unwrap_or(0),
                counts["pending"].as_u64().unwrap_or(0)
                    + counts["ready"].as_u64().unwrap_or(0)
                    + counts["running"].as_u64().unwrap_or(0)
                    + counts["verifying"].as_u64().unwrap_or(0)
                    + counts["completed"].as_u64().unwrap_or(0)
                    + counts["failed"].as_u64().unwrap_or(0)
                    + counts["blocked"].as_u64().unwrap_or(0)
                    + counts["cancelled"].as_u64().unwrap_or(0),
                counts["running"].as_u64().unwrap_or(0) + counts["verifying"].as_u64().unwrap_or(0),
                counts["blocked"].as_u64().unwrap_or(0),
            );
            print_program_line(program);
        }
        return;
    }
    if let Some(program) = value
        .get("program")
        .filter(|program| program["id"].is_string())
    {
        print_program_line(program);
        if let Some(milestones) = value.get("milestones").and_then(Value::as_array) {
            for milestone in milestones {
                let key = milestone["key"].as_str().unwrap_or("?");
                let id = milestone["id"].as_str().unwrap_or("?");
                let status = milestone["status"].as_str().unwrap_or("?");
                let title = milestone["title"].as_str().unwrap_or("");
                println!(
                    "  {key:<8} {status:<10} {title}  attempts {}/{}{}",
                    milestone["attempts"].as_u64().unwrap_or(0),
                    milestone["max_attempts"].as_u64().unwrap_or(0),
                    milestone["session_id"]
                        .as_str()
                        .map(|session| format!("  session {session}"))
                        .unwrap_or_default()
                );
                if let Some(deps) = milestone["deps"].as_array().filter(|deps| !deps.is_empty()) {
                    println!(
                        "    deps {}",
                        deps.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if let Some(error) = milestone["last_error"].as_str() {
                    println!("    {error}");
                }
                if let Some(next) = milestone["next_attempt_at"].as_str() {
                    println!("    retry after {next}");
                }
                let _ = id;
            }
        }
        if let Some(usage) = value.get("usage") {
            println!(
                "  usage {} tokens / ${:.4}",
                usage["token_spent"].as_u64().unwrap_or(0),
                usage["cost_spent_usd"].as_f64().unwrap_or(0.0)
            );
        }
        return;
    }
    if value.as_object().is_some_and(|object| {
        object.len() == 2
            && object.contains_key("session_id")
            && object.contains_key("continuations")
    }) && let Some(continuations) = value.get("continuations").and_then(Value::as_array)
    {
        if continuations.is_empty() {
            println!("No continuations.");
            return;
        }
        for continuation in continuations {
            let id = continuation["id"].as_str().unwrap_or("?");
            let series = continuation["series_id"]
                .as_str()
                .filter(|series| !series.is_empty())
                .unwrap_or(id);
            let run_count = continuation["run_count"].as_u64().unwrap_or(0);
            let max_runs = continuation["recurrence_max_runs"].as_u64().unwrap_or(1);
            let mut header = format!(
                "{id}  {}  run {run_count}/{max_runs}  series {series}",
                continuation["status"].as_str().unwrap_or("?")
            );
            if let Some(every_secs) = continuation["recurrence_every_secs"].as_u64() {
                header.push_str(&format!("  every {every_secs}s"));
            }
            println!("{header}");
            if let Some(deadline) = continuation["deadline_at"].as_str() {
                println!("  deadline {deadline}");
            }
            let watch = continuation["watch_task_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if !watch.is_empty() {
                println!(
                    "  watch {}: {watch}",
                    continuation["watch_mode"].as_str().unwrap_or("all")
                );
            }
            if let Some(error) = continuation["last_error"].as_str() {
                println!("  {error}");
            }
            println!("  {}", continuation["body"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("goal").is_none()
        && let Some(unlimited) = value.get("unlimited").and_then(Value::as_bool)
    {
        println!(
            "session {} budget mode: {}",
            session_id,
            if unlimited { "unlimited" } else { "limited" }
        );
        return;
    }
    if value.get("console_streamed").and_then(Value::as_bool) == Some(true)
        || value.get("terminal_streamed").and_then(Value::as_bool) == Some(true)
    {
        return;
    }
    if let Some(project_id) = value.get("project_id").and_then(Value::as_str)
        && value
            .get("sessions_observed")
            .and_then(Value::as_u64)
            .is_some()
        && value.get("scan_limit").and_then(Value::as_u64).is_some()
    {
        println!(
            "project cache  {project_id}  {} session(s) · {} call(s) · scan limit {}{}",
            value["sessions_observed"].as_u64().unwrap_or(0),
            value["calls"].as_u64().unwrap_or(0),
            value["scan_limit"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        );
        println!(
            "  hit ratio        {:.2}% · read {} · miss {} · write {}",
            value["hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_miss_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0)
        );
        if let (Some(p50), Some(p95)) = (
            value["hit_ratio_p50"].as_f64(),
            value["hit_ratio_p95"].as_f64(),
        ) {
            println!(
                "  percentiles      p50 {:.2}% · p95 {:.2}%",
                p50 * 100.0,
                p95 * 100.0
            );
        }
        println!(
            "  continuity       append-only {} · breaks {} · epochs {}",
            value["append_only_calls"].as_u64().unwrap_or(0),
            value["append_only_breaks"].as_u64().unwrap_or(0),
            value["cache_epochs"].as_u64().unwrap_or(0)
        );
        if let Some(reasons) = value["top_miss_reasons"].as_array()
            && !reasons.is_empty()
        {
            let rendered = reasons
                .iter()
                .map(|reason| {
                    format!(
                        "{}×{}",
                        reason["key"].as_str().unwrap_or("?"),
                        reason["calls"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            println!("  top misses       {rendered}");
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("returned").and_then(Value::as_u64).is_some()
        && value.get("calls").and_then(Value::as_array).is_some()
    {
        println!(
            "cache calls  {session_id}  {} returned · {} matching · limit {}{}",
            value["returned"].as_u64().unwrap_or(0),
            value["observed"].as_u64().unwrap_or(0),
            value["limit"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        );
        if let Some(role) = value["role"].as_str() {
            println!("  role filter     {role}");
        }
        if let Some(reason) = value["miss_reason"].as_str() {
            println!("  reason filter   {reason}");
        }
        println!(
            "  {:<19} {:<14} {:>8} {:>8} {:>8} {:>7} {:>7} {:<16} EPOCH",
            "AT", "ROLE", "INPUT", "READ", "MISS", "HIT", "APPEND", "MISS REASON"
        );
        for call in value["calls"].as_array().into_iter().flatten() {
            let append = match call["append_only"].as_bool() {
                Some(true) => "yes",
                Some(false) => "break",
                None => "n/a",
            };
            println!(
                "  {:<19} {:<14} {:>8} {:>8} {:>8} {:>7.2}% {:>7} {:<16} {}",
                call["at"].as_str().unwrap_or("-"),
                call["role"].as_str().unwrap_or("-"),
                call["input_tokens"].as_u64().unwrap_or(0),
                call["cache_read_tokens"].as_u64().unwrap_or(0),
                call["cache_miss_tokens"].as_u64().unwrap_or(0),
                call["cache_hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                append,
                call["miss_reason"].as_str().unwrap_or("-"),
                short_hash(&call["cache_epoch"])
            );
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("calls_checked").and_then(Value::as_u64).is_some()
        && value.get("findings").and_then(Value::as_array).is_some()
    {
        println!("cache audit  {session_id}");
        println!(
            "  calls            {} agent · {} warm · {} compaction resets",
            value["calls_checked"].as_u64().unwrap_or(0),
            value["warm_calls_checked"].as_u64().unwrap_or(0),
            value["compactions_checked"].as_u64().unwrap_or(0)
        );
        println!(
            "  hit ratio        {:.2}% · read {} · miss {} · write {}",
            value["hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_miss_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0)
        );
        if let Some(theoretical) = value["theoretical_max_hit_ratio"].as_f64() {
            println!(
                "  theoretical max  {:.2}% · append-only {} · breaks {} · epochs {}",
                theoretical * 100.0,
                value["append_only_calls"].as_u64().unwrap_or(0),
                value["append_only_breaks"].as_u64().unwrap_or(0),
                value["cache_epochs"].as_u64().unwrap_or(0)
            );
        }
        if value["clean"].as_bool().unwrap_or(false) {
            println!("  result           clean (no warning or critical findings)");
        }
        for finding in value["findings"].as_array().into_iter().flatten() {
            println!(
                "  [{:<8}] {:<32} {}",
                finding["severity"].as_str().unwrap_or("info"),
                finding["code"].as_str().unwrap_or("?"),
                finding["message"].as_str().unwrap_or("")
            );
            if let Some(evidence) = finding["evidence"].as_object()
                && !evidence.is_empty()
            {
                let rendered = evidence
                    .iter()
                    .map(|(key, value)| format!("{key}={}", compact_json(value)))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("             evidence: {rendered}");
            }
        }
        return;
    }
    if let Some(lines) = tool_bundle_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("policy").is_some()
        && value
            .get("roles")
            .and_then(Value::as_array)
            .is_some_and(|roles| roles.iter().any(|role| role.get("prefix_hash").is_some()))
    {
        println!("cache plan  {session_id}");
        if let Some(provider) = value["provider"].as_str() {
            println!("  provider         {provider}");
        }
        if let Some(policy) = value.get("policy") {
            println!(
                "  policy           enabled {} · ttl {} · automatic {} · role overrides {}",
                policy["enabled"].as_bool().unwrap_or(true),
                policy["ttl"].as_str().unwrap_or("5m"),
                policy["automatic_caching"].as_bool().unwrap_or(true),
                policy["role_ttl"]
                    .as_object()
                    .map_or(0, serde_json::Map::len)
            );
        }
        for role in value["roles"].as_array().into_iter().flatten() {
            println!(
                "  {:<9} {} · {} breakpoints · ttl {} · tail {}",
                role["role"].as_str().unwrap_or("?"),
                role["model"].as_str().unwrap_or("?"),
                role["cache_breakpoints"].as_u64().unwrap_or(0),
                role["cache_ttl"].as_str().unwrap_or("5m"),
                role["tail_message"].as_bool().unwrap_or(false)
            );
            println!(
                "            core {}… project {}… tools {}… prefix {}…",
                short_hash(&role["core_hash"]),
                short_hash(&role["project_hash"]),
                short_hash(&role["tools_hash"]),
                short_hash(&role["prefix_hash"])
            );
            println!(
                "            sections {} core / {} project chars · {} tools · ~{} + {} tokens",
                role["core_chars"].as_u64().unwrap_or(0),
                role["project_chars"].as_u64().unwrap_or(0),
                role["tool_count"].as_u64().unwrap_or(0),
                role["system_tokens_estimate"].as_u64().unwrap_or(0),
                role["tool_tokens_estimate"].as_u64().unwrap_or(0)
            );
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("warmed").and_then(Value::as_bool).is_some()
        && value.get("source").and_then(Value::as_str).is_some()
    {
        println!(
            "cache warm  {session_id}  [{}]  {}",
            value["source"].as_str().unwrap_or("manual"),
            if value["warmed"].as_bool().unwrap_or(false) {
                "warmed"
            } else {
                "not warmed"
            }
        );
        println!(
            "  provider         {} · {}",
            value["provider"].as_str().unwrap_or("?"),
            value["model"].as_str().unwrap_or("?")
        );
        println!(
            "  breakpoints      {} · ttl {} · automatic {}",
            value["cache_breakpoints"].as_u64().unwrap_or(0),
            value["cache_ttl"].as_str().unwrap_or("?"),
            value["automatic"].as_bool().unwrap_or(false)
        );
        println!(
            "  tokens           input {} · cache read {} · cache write {} · output {}",
            value["input_tokens"].as_u64().unwrap_or(0),
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0)
        );
        println!(
            "  cost             ${:.6}{}",
            value["cost_usd"].as_f64().unwrap_or(0.0),
            if value["estimated"].as_bool().unwrap_or(false) {
                " (estimated usage)"
            } else {
                ""
            }
        );
        if let Some(message) = value["message"]
            .as_str()
            .filter(|message| !message.is_empty())
        {
            println!("  message          {message}");
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value
            .get("cache_hit_ratio")
            .and_then(Value::as_f64)
            .is_some()
        && value.get("input_tokens").is_some()
    {
        let ratio = value["cache_hit_ratio"].as_f64().unwrap_or(0.0);
        println!("cache  {session_id}");
        println!(
            "  hit ratio        {:.2}%  (cache read / ledger input buckets)",
            ratio * 100.0
        );
        println!(
            "  tokens           input {} · cache read {} · cache miss {} · cache write {} · output {}",
            value["input_tokens"].as_u64().unwrap_or(0),
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_miss_tokens"].as_u64().unwrap_or_else(|| {
                value["input_tokens"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_sub(value["cache_read_tokens"].as_u64().unwrap_or(0))
            }),
            value["cache_write_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0)
        );
        if let Some(theoretical) = value["theoretical_max_hit_ratio"].as_f64() {
            println!(
                "  theoretical max  {:.2}%  (previous prompt / current input; append-only headroom)",
                theoretical * 100.0
            );
        }
        if let (Some(p50), Some(p95)) = (
            value["hit_ratio_p50"].as_f64(),
            value["hit_ratio_p95"].as_f64(),
        ) {
            println!(
                "  percentiles      p50 {:.2}% · p95 {:.2}%",
                p50 * 100.0,
                p95 * 100.0
            );
        }
        println!(
            "  continuity       append-only {} · breaks {} · last {}",
            value["append_only_calls"].as_u64().unwrap_or(0),
            value["append_only_breaks"].as_u64().unwrap_or(0),
            match value["last_append_only"].as_bool() {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            }
        );
        if value["cache_epoch"].as_str().is_some() {
            println!(
                "  cache epoch      {} ({})",
                short_hash(&value["cache_epoch"]),
                value["cache_epoch_source"].as_str().unwrap_or("unknown")
            );
        }
        if let Some(reasons) = value["top_miss_reasons"].as_array()
            && !reasons.is_empty()
        {
            let rendered = reasons
                .iter()
                .map(|reason| {
                    format!(
                        "{}×{}",
                        reason["key"].as_str().unwrap_or("?"),
                        reason["calls"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            println!("  top misses       {rendered}");
        }
        if let Some(roles) = value["per_role"].as_array()
            && !roles.is_empty()
        {
            let rendered = roles
                .iter()
                .map(|role| {
                    format!(
                        "{} {:.1}% ({} miss, ${:.4})",
                        role["role"].as_str().unwrap_or("?"),
                        role["hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                        role["cache_miss_tokens"].as_u64().unwrap_or(0),
                        role["cost_usd"].as_f64().unwrap_or(0.0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            println!("  per role         {rendered}");
        }
        println!(
            "  cross-session    {} session(s) · {} hit token(s) observed (bounded scan, not a guarantee)",
            value["cross_session_reuse_sessions"].as_u64().unwrap_or(0),
            value["cross_session_hit_tokens"].as_u64().unwrap_or(0)
        );
        println!(
            "  lost to compact  ${:.6} (estimated cache-read savings invalidated by compaction)",
            value["compaction_lost_cache_usd"].as_f64().unwrap_or(0.0)
        );
        println!(
            "  savings          ${:.6} (estimated from configured prices; negative means cache writes cost more than reads saved)",
            value["estimated_savings_usd"].as_f64().unwrap_or(0.0)
        );
        println!(
            "  calls            {} cache-observed provider calls",
            value["cache_calls"].as_u64().unwrap_or(0)
        );
        println!(
            "  plan             {} breakpoints · ttl {}",
            value["last_breakpoints"].as_u64().unwrap_or(0),
            value["last_cache_ttl"].as_str().unwrap_or("unknown")
        );
        println!(
            "  last miss        {}",
            value["last_miss_reason"]
                .as_str()
                .unwrap_or("none recorded")
        );
        match value["preamble_tokens_estimate"].as_u64() {
            Some(tokens) => println!("  preamble         ~{tokens} tokens (estimate)"),
            None => println!("  preamble         unknown"),
        }
        match value["prompt_prefix"].as_str() {
            Some(prefix) => println!("  prompt prefix    {prefix}"),
            None => println!("  prompt prefix    unknown"),
        }
        match value["last_call_at"].as_str() {
            Some(last_call) => println!("  last call        {last_call}"),
            None => println!("  last call        never"),
        }
        if value.get("append_only_calls").is_some() {
            println!(
                "  append-only      {} ({} append · {} break{})",
                value["last_append_only"].as_bool().unwrap_or(false),
                value["append_only_calls"].as_u64().unwrap_or(0),
                value["non_append_only_calls"].as_u64().unwrap_or(0),
                value["last_tail_memory_mode"]
                    .as_str()
                    .map(|mode| format!(" · tail {mode}"))
                    .unwrap_or_default()
            );
            println!(
                "  message chain    {} · previous {} · {} messages",
                short_hash(&value["last_message_prefix_hash"]),
                short_hash(&value["last_previous_message_prefix_hash"]),
                value["last_message_count"].as_u64().unwrap_or(0)
            );
            if let Some(capability) = value["last_tail_system_capability"].as_str() {
                println!(
                    "  provider tail    system {} · capability {capability}",
                    value["provider_tail_system"].as_bool().unwrap_or(false)
                );
            }
            let breaks = value["recent_breaks"].as_array();
            if let Some(breaks) = breaks.filter(|breaks| !breaks.is_empty()) {
                println!("  breaks           {} recent", breaks.len());
                for entry in breaks.iter().take(5) {
                    let index = entry["first_differing_index"]
                        .as_u64()
                        .map(|index| format!(" at #{index}"))
                        .unwrap_or_default();
                    let role = entry["first_differing_role"]
                        .as_str()
                        .map(|role| format!(" · {role}"))
                        .unwrap_or_default();
                    println!(
                        "    {:<20} {}{}{}",
                        entry["reason"].as_str().unwrap_or("unknown"),
                        entry["at"].as_str().unwrap_or(""),
                        index,
                        role
                    );
                }
            }
        }
        if let Some(policy) = value.get("policy").filter(|policy| policy.is_object()) {
            println!(
                "  policy           enabled {} · ttl {} · automatic {} · warm-on-resume {} · compact-aware {}",
                policy["enabled"].as_bool().unwrap_or(true),
                policy["ttl"].as_str().unwrap_or("5m"),
                policy["automatic_caching"].as_bool().unwrap_or(true),
                policy["warm_on_resume"].as_bool().unwrap_or(false),
                policy["compact_aware"].as_bool().unwrap_or(true)
            );
        }
        return;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("total_breaks").is_some()
        && value.get("breaks").is_some()
    {
        println!(
            "cache breaks  {session_id}  ({} append · {} break · {} recorded{})",
            value["append_only_calls"].as_u64().unwrap_or(0),
            value["non_append_only_calls"].as_u64().unwrap_or(0),
            value["total_breaks"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        );
        for entry in value["breaks"].as_array().into_iter().flatten() {
            let index = entry["first_differing_index"]
                .as_u64()
                .map(|index| format!(" at #{index}"))
                .unwrap_or_default();
            let role = entry["first_differing_role"]
                .as_str()
                .map(|role| format!(" · {role}"))
                .unwrap_or_default();
            let tool = entry["first_differing_tool_call_id"]
                .as_str()
                .map(|tool| format!(" · {tool}"))
                .unwrap_or_default();
            println!(
                "  {:<20} {} · {}→{} msgs{}{}{}",
                entry["reason"].as_str().unwrap_or("unknown"),
                entry["at"].as_str().unwrap_or(""),
                entry["previous_message_count"].as_u64().unwrap_or(0),
                entry["message_count"].as_u64().unwrap_or(0),
                index,
                role,
                tool
            );
        }
        return;
    }
    if let Some(csv) = value.get("csv").and_then(Value::as_str) {
        print!("{csv}");
        return;
    }
    if let Some(markdown) = value.get("markdown").and_then(Value::as_str) {
        print!("{markdown}");
        return;
    }
    if let Some(steering) = value.get("steering").and_then(Value::as_object) {
        println!(
            "{}  {}  agent={}  task={}",
            steering["id"].as_str().unwrap_or("?"),
            steering["status"].as_str().unwrap_or("?"),
            steering["agent_id"].as_str().unwrap_or("?"),
            steering["task_id"].as_str().unwrap_or("-")
        );
        println!("  {}", steering["body"].as_str().unwrap_or(""));
        if let Some(delivered_at) = steering["delivered_at"].as_str() {
            println!("  delivered at {delivered_at}");
        }
        return;
    }
    if let Some(messages) = value.get("steering").and_then(Value::as_array) {
        if messages.is_empty() {
            println!("No steering messages.");
            return;
        }
        for steering in messages {
            println!(
                "{}  {:<9}  agent={}  task={}  {}",
                steering["id"].as_str().unwrap_or("?"),
                steering["status"].as_str().unwrap_or("?"),
                steering["agent_id"].as_str().unwrap_or("?"),
                steering["task_id"].as_str().unwrap_or("-"),
                steering["created_at"].as_str().unwrap_or("?")
            );
            println!("  {}", steering["body"].as_str().unwrap_or(""));
        }
        return;
    }
    if value.get("session_id").is_some()
        && let Some(recoveries) = value.get("recoveries").and_then(Value::as_array)
    {
        println!(
            "session {}  recoveries={}",
            value["session_id"].as_str().unwrap_or("?"),
            recoveries.len()
        );
        if recoveries.is_empty() {
            println!("No stall recovery records.");
            return;
        }
        for recovery in recoveries {
            println!(
                "{}  {:<14}  action={}  attempts={}  last_stall={}",
                recovery["task_id"].as_str().unwrap_or("?"),
                recovery["status"].as_str().unwrap_or("?"),
                recovery["action"].as_str().unwrap_or("?"),
                recovery["attempts"].as_u64().unwrap_or(0),
                recovery["last_stall_count"].as_u64().unwrap_or(0)
            );
            println!(
                "  requested={}  updated={}  next={}",
                recovery["requested_at"].as_str().unwrap_or("-"),
                recovery["updated_at"].as_str().unwrap_or("-"),
                recovery["next_action_at"].as_str().unwrap_or("-")
            );
            if let Some(error) = recovery["last_error"].as_str() {
                println!("  {error}");
            }
        }
        return;
    }
    if value.get("stalls").is_some() && value.get("progress").is_some() {
        println!(
            "session {}  stall threshold={}s",
            value["session_id"].as_str().unwrap_or("?"),
            value["threshold_secs"].as_u64().unwrap_or(0)
        );
        let progress = value["progress"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if progress.is_empty() {
            println!("No task progress recorded.");
        }
        for task in progress {
            println!(
                "  {}  stalls={}  last={}  turn={}  tool={}",
                task["task_id"].as_str().unwrap_or("?"),
                task["stall_count"].as_u64().unwrap_or(0),
                task["last_progress_at"].as_str().unwrap_or("-"),
                task["last_turn"].as_u64().unwrap_or(0),
                task["last_tool"].as_str().unwrap_or("-")
            );
        }
        let stalls = value["stalls"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if stalls.is_empty() {
            println!("No stall observations.");
        }
        for stall in stalls {
            println!(
                "  stall {}  age={}s  attempt={}  status={}  observed={}",
                stall["task_id"].as_str().unwrap_or("?"),
                stall["age_secs"].as_u64().unwrap_or(0),
                stall["attempts"].as_u64().unwrap_or(0),
                stall["status"].as_str().unwrap_or("?"),
                stall["observed_at"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(actions) = value.get("actions").and_then(Value::as_array)
        && value.get("session_id").is_some()
        && value.get("snapshot").is_none()
    {
        if actions.is_empty() {
            println!("No orchestration action was needed.");
        } else {
            for action in actions {
                println!(
                    "{}  {}  {}  {}",
                    action["action"].as_str().unwrap_or("?"),
                    action["task_id"].as_str().unwrap_or("-"),
                    action["attempt"].as_u64().unwrap_or(0),
                    action["detail"].as_str().unwrap_or("")
                );
            }
        }
        return;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array)
        && let Some(totals) = value.get("totals")
    {
        println!(
            "{} sessions · {} running · {} attention · {} blocked tasks · {} failed · {} retrying · {} stalled · {} conflicts",
            totals["sessions"].as_u64().unwrap_or(0),
            totals["running"].as_u64().unwrap_or(0),
            totals["attention"].as_u64().unwrap_or(0),
            totals["blocked_tasks"].as_u64().unwrap_or(0),
            totals["failed_tasks"].as_u64().unwrap_or(0),
            totals["retrying_tasks"].as_u64().unwrap_or(0),
            totals["stalled_tasks"].as_u64().unwrap_or(0),
            totals["merge_conflicts"].as_u64().unwrap_or(0)
        );
        for session in sessions {
            println!(
                "  {}  [{}]  {}  r{}  tasks {}/{}  ready {}  blocked {}  failed {}",
                session["project_name"].as_str().unwrap_or("?"),
                session["status"].as_str().unwrap_or("?"),
                session["goal"].as_str().unwrap_or(""),
                session["plan_revision"].as_u64().unwrap_or(0),
                session["metrics"]["completed_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["total_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["ready_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["blocked_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["failed_tasks"].as_u64().unwrap_or(0)
            );
            if let Some(blocker) = session["blockers"]
                .as_array()
                .and_then(|values| values.first())
            {
                println!("      blocker: {}", blocker.as_str().unwrap_or(""));
            }
            if let Some(decision) = session["last_decision"].as_str() {
                println!("      last: {}", decision);
            }
        }
        return;
    }
    if let Some(snapshot) = value.get("snapshot")
        && snapshot.get("metrics").is_some()
    {
        let metrics = &snapshot["metrics"];
        println!(
            "session {}  status={}  plan={} rev={}",
            snapshot["session_id"].as_str().unwrap_or("?"),
            snapshot["status"].as_str().unwrap_or("?"),
            if snapshot["plan"].is_null() {
                "none"
            } else {
                "present"
            },
            snapshot["plan_revision"].as_u64().unwrap_or(0)
        );
        println!(
            "agents={} running={} queued={} failed={}",
            metrics["total_agents"].as_u64().unwrap_or(0),
            metrics["running_agents"].as_u64().unwrap_or(0),
            metrics["queued_agents"].as_u64().unwrap_or(0),
            metrics["failed_agents"].as_u64().unwrap_or(0)
        );
        println!(
            "tasks={} completed={} active={} ready={} blocked={} failed={} retrying={} stalled={}",
            metrics["total_tasks"].as_u64().unwrap_or(0),
            metrics["completed_tasks"].as_u64().unwrap_or(0),
            metrics["active_tasks"].as_u64().unwrap_or(0),
            metrics["ready_tasks"].as_u64().unwrap_or(0),
            metrics["blocked_tasks"].as_u64().unwrap_or(0),
            metrics["failed_tasks"].as_u64().unwrap_or(0),
            metrics["retrying_tasks"].as_u64().unwrap_or(0),
            metrics["stalled_tasks"].as_u64().unwrap_or(0)
        );
        for task in snapshot["tasks"].as_array().into_iter().flatten() {
            println!(
                "  {} [{}/{}] {} · attempts={} {}",
                task["task_id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["status"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or(""),
                task["attempts"].as_u64().unwrap_or(0),
                task["blocked_by"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        for blocker in snapshot["blockers"].as_array().into_iter().flatten() {
            println!(
                "  blocker {} · {}",
                blocker["kind"].as_str().unwrap_or("?"),
                blocker["message"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if let Some(revision) = value.get("revision")
        && let Some(plan) = value.get("plan")
    {
        println!(
            "r{}  {}  {}  tasks={}  {}",
            revision["revision"].as_u64().unwrap_or(0),
            revision["kind"].as_str().unwrap_or("?"),
            revision["author"].as_str().unwrap_or("?"),
            revision["task_count"].as_u64().unwrap_or(0),
            revision["reason"].as_str().unwrap_or("")
        );
        println!(
            "diff: +{}  -{}  ~{}",
            revision["added"].as_array().map(Vec::len).unwrap_or(0),
            revision["removed"].as_array().map(Vec::len).unwrap_or(0),
            revision["changed"].as_array().map(Vec::len).unwrap_or(0)
        );
        for task in plan["tasks"].as_array().into_iter().flatten() {
            println!(
                "  {} [{}] {}  deps={}",
                task["id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or(""),
                task["deps"]
                    .as_array()
                    .map(|deps| deps
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        return;
    }
    if let Some(revisions) = value.get("revisions").and_then(Value::as_array) {
        if revisions.is_empty() {
            println!("No plan revisions.");
        }
        for revision in revisions {
            println!(
                "r{}  {}  {}  tasks={}  +{} -{} ~{}  {}",
                revision["revision"].as_u64().unwrap_or(0),
                revision["kind"].as_str().unwrap_or("?"),
                revision["author"].as_str().unwrap_or("?"),
                revision["task_count"].as_u64().unwrap_or(0),
                revision["added"].as_array().map(Vec::len).unwrap_or(0),
                revision["removed"].as_array().map(Vec::len).unwrap_or(0),
                revision["changed"].as_array().map(Vec::len).unwrap_or(0),
                revision["reason"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
    {
        println!(
            "{}",
            serde_json::to_string_pretty(policy).unwrap_or_default()
        );
        return;
    }
    if let Some(leases) = value.get("leases").and_then(Value::as_array) {
        println!(
            "runtime {} · {} lease(s)",
            value
                .get("runtime_id")
                .and_then(|value| value.as_str())
                .unwrap_or("?"),
            leases.len()
        );
        if let Some(instance) = value.get("daemon_instance") {
            println!(
                "  instance {}  pid {}  started {}",
                instance
                    .get("instance_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?"),
                instance
                    .get("pid")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                instance
                    .get("started_at")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?")
            );
        }
        for lease in leases {
            println!(
                "  {}  {}  {}  generation {}  {}  expires {}  owner {} ({})",
                lease["kind"].as_str().unwrap_or("?"),
                lease["lease_key"].as_str().unwrap_or("?"),
                lease["session_id"].as_str().unwrap_or("-"),
                lease["fencing_token"].as_u64().unwrap_or(0),
                lease["state"].as_str().unwrap_or("?"),
                lease["expires_at"].as_str().unwrap_or("?"),
                lease["owner_id"].as_str().unwrap_or("?"),
                lease["owner_instance"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && entries
            .iter()
            .all(|entry| entry.get("action").is_some() && entry.get("lease_key").is_some())
    {
        if entries.is_empty() {
            println!("No runtime lease history.");
        }
        for entry in entries {
            println!(
                "  {}  {}  {}  generation {}  {} ({})  {}",
                entry["occurred_at"].as_str().unwrap_or("?"),
                entry["action"].as_str().unwrap_or("?"),
                entry["lease_key"].as_str().unwrap_or("?"),
                entry["fencing_token"].as_u64().unwrap_or(0),
                entry["owner_id"].as_str().unwrap_or("?"),
                entry["owner_instance"].as_str().unwrap_or("?"),
                entry["detail"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if value.get("removed").is_some() {
        println!(
            "Removed {} expired runtime lease(s).",
            value["removed"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(hits) = value.get("hits").and_then(Value::as_array)
        && value.get("query").is_some()
    {
        if hits.is_empty() {
            println!("No matches.");
            return;
        }
        for hit in hits {
            let source = hit["source"].as_str().unwrap_or("?");
            let detail = hit["role"]
                .as_str()
                .or_else(|| hit["kind"].as_str())
                .unwrap_or("");
            println!(
                "{} {:<18} {} · {}",
                source,
                detail,
                hit["session_goal"].as_str().unwrap_or("?"),
                hit["snippet"].as_str().unwrap_or("")
            );
            println!(
                "  {} · session {}",
                hit["created_at"].as_str().unwrap_or("?"),
                hit["session_id"].as_str().unwrap_or("?")
            );
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            println!("… more matches exist; increase --limit or narrow the query.");
        }
        return;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array)
        && value.get("project_id").is_some()
        && let Some(totals) = value.get("totals")
    {
        println!(
            "project {}  sessions={}  calls={}  in={}  out={}  cache_read={}  cache_write={}  cost=${:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            sessions.len(),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cache_read_tokens"].as_u64().unwrap_or(0),
            totals["cache_write_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        );
        for session in sessions {
            println!(
                "  {:<10} calls={:<4} in={:<8} out={:<8} ${:.4}  {}",
                session["status"].as_str().unwrap_or("?"),
                session["calls"].as_u64().unwrap_or(0),
                session["input_tokens"].as_u64().unwrap_or(0),
                session["output_tokens"].as_u64().unwrap_or(0),
                session["cost_usd"].as_f64().unwrap_or(0.0),
                session["goal"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(tasks) = value.get("tasks").and_then(Value::as_array)
        && tasks
            .first()
            .is_none_or(|task| task.get("attempts").is_some() && task.get("instructions").is_some())
        && value.get("leases").is_none()
        && tasks
            .iter()
            .all(|task| task.get("attempts").is_some() && task.get("instructions").is_some())
    {
        if tasks.is_empty() {
            println!("No intake tasks.");
            return;
        }
        for task in tasks {
            println!(
                "{:<12} p{:<4} {:<10} {:<9} {}/{}  {}",
                task["status"].as_str().unwrap_or("?"),
                task["priority"].as_i64().unwrap_or(0),
                task["profile"].as_str().unwrap_or("?"),
                task["kind"].as_str().unwrap_or("?"),
                task["attempts"].as_u64().unwrap_or(0),
                task["max_attempts"].as_u64().unwrap_or(0),
                task["title"].as_str().unwrap_or("")
            );
            println!("  id={}", task["id"].as_str().unwrap_or("?"));
            if let Some(session) = task["session_id"].as_str() {
                println!("  session={session}");
            }
            if let Some(deps) = task["deps"].as_array()
                && !deps.is_empty()
            {
                println!(
                    "  depends on: {}",
                    deps.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(error) = task["last_error"].as_str() {
                println!("  last error: {error}");
            } else if let Some(summary) = task["result_summary"].as_str() {
                println!("  result: {summary}");
            }
        }
        return;
    }
    if let Some(task) = value.get("task")
        && task.get("attempts").is_some()
        && task.get("instructions").is_some()
    {
        println!(
            "{}  p{}  {}  {}/{}  {}",
            task["status"].as_str().unwrap_or("?"),
            task["priority"].as_i64().unwrap_or(0),
            task["profile"].as_str().unwrap_or("?"),
            task["attempts"].as_u64().unwrap_or(0),
            task["max_attempts"].as_u64().unwrap_or(0),
            task["title"].as_str().unwrap_or("")
        );
        println!("id={}", task["id"].as_str().unwrap_or("?"));
        if let Some(session) = task["session_id"].as_str() {
            println!("session={session}");
        }
        if let Some(error) = task["last_error"].as_str() {
            println!("last error: {error}");
        }
        return;
    }
    if let Some(activations) = value.get("activations").and_then(Value::as_array) {
        if activations.is_empty() {
            println!("No skill activations.");
            return;
        }
        for activation in activations {
            println!(
                "{} v{}  role={} profile={}  {}",
                activation["name"].as_str().unwrap_or("?"),
                activation["version"].as_str().unwrap_or(""),
                activation["role"].as_str().unwrap_or(""),
                activation["profile"].as_str().unwrap_or(""),
                activation["activated_at"].as_str().unwrap_or("")
            );
            if let Some(allow) = activation["tool_allow"].as_array()
                && !allow.is_empty()
            {
                println!(
                    "  allow: {}",
                    allow
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(deny) = activation["tool_deny"].as_array()
                && !deny.is_empty()
            {
                println!(
                    "  deny: {}",
                    deny.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        return;
    }
    if let Some(skills) = value.get("skills").and_then(Value::as_array) {
        if skills.is_empty() {
            println!("No skills.");
            return;
        }
        for skill in skills {
            println!(
                "{}  {} {}  [{}]  roles={} profiles={}",
                skill["enabled"].as_bool().unwrap_or(false),
                skill["name"].as_str().unwrap_or("?"),
                skill["version"].as_str().unwrap_or(""),
                skill["source"].as_str().unwrap_or("?"),
                skill["roles"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
                skill["profiles"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
            );
            println!("  id={}", skill["id"].as_str().unwrap_or("?"));
            if let Some(allow) = skill["tool_allow"].as_array()
                && !allow.is_empty()
            {
                println!(
                    "  allow: {}",
                    allow
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(deny) = skill["tool_deny"].as_array()
                && !deny.is_empty()
            {
                println!(
                    "  deny: {}",
                    deny.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        return;
    }
    if let Some(skill) = value.get("name").and_then(Value::as_str)
        && value.get("instructions").is_some()
        && value.get("slug").is_some()
    {
        println!(
            "{} {}  enabled={}  id={}",
            skill,
            value["version"].as_str().unwrap_or(""),
            value["enabled"].as_bool().unwrap_or(false),
            value["id"].as_str().unwrap_or("?")
        );
        return;
    }
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
        return;
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
        return;
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
        return;
    }
    if let Some(hits) = value.get("hits").and_then(Value::as_array)
        && value.get("degraded").is_some()
    {
        if hits.is_empty() {
            println!("No source matches.");
            return;
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
        return;
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
        return;
    }
    if value.get("enqueued_sources").is_some() && value.get("cleared_vectors").is_some() {
        println!(
            "source embedding reindex · enabled={} · cleared {} vectors / {} jobs · queued {} sources",
            value["enabled"].as_bool().unwrap_or(false),
            value["cleared_vectors"].as_u64().unwrap_or(0),
            value["cleared_jobs"].as_u64().unwrap_or(0),
            value["enqueued_sources"].as_u64().unwrap_or(0),
        );
        return;
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
        return;
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
        return;
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
        return;
    }
    if let Some(matches) = value.get("matches").and_then(Value::as_array)
        && value.get("files_scanned").is_some()
    {
        if matches.is_empty() {
            println!("No matches.");
            return;
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
        return;
    }
    if let Some(deleted) = value.get("deleted").and_then(Value::as_bool) {
        println!("{}", if deleted { "deleted." } else { "not deleted." });
        return;
    }
    if let Some(task) = value.get("task")
        && task.get("session_id").is_some()
        && task.get("task_id").is_some()
    {
        println!(
            "{} {} -> {}",
            task["session_id"].as_str().unwrap_or("?"),
            task["task_id"].as_str().unwrap_or("?"),
            task["status"].as_str().unwrap_or("?")
        );
        return;
    }
    if let Some(projects) = value.get("projects").and_then(Value::as_array)
        && value.get("global_session_cap").is_some()
    {
        println!(
            "sessions: active={} queued={} paused={} cap={}",
            value["active_sessions"].as_u64().unwrap_or(0),
            value["queued_sessions"].as_u64().unwrap_or(0),
            value["paused_sessions"].as_u64().unwrap_or(0),
            value["global_session_cap"].as_u64().unwrap_or(0)
        );
        println!(
            "workers: {}  labour: {}  plan tasks: pending={} running={}",
            value["global_worker_cap"].as_u64().unwrap_or(0),
            value["global_labour_cap"].as_u64().unwrap_or(0),
            value["pending_plan_tasks"].as_u64().unwrap_or(0),
            value["running_plan_tasks"].as_u64().unwrap_or(0)
        );
        println!(
            "intake: queued={} active={} failed={}",
            value["queued_intake"].as_u64().unwrap_or(0),
            value["active_intake"].as_u64().unwrap_or(0),
            value["failed_intake"].as_u64().unwrap_or(0)
        );
        for project in projects {
            println!(
                "{}  enabled={} intake={} cap={} cooldown={}s  sessions={} plan={}/{}  intake={}/{}/{}",
                project["project_name"].as_str().unwrap_or("?"),
                project["enabled"].as_bool().unwrap_or(false),
                project["include_intake"].as_bool().unwrap_or(false),
                project["max_intake_parallel"].as_u64().unwrap_or(0),
                project["cooldown_secs"].as_u64().unwrap_or(0),
                project["pending_sessions"].as_u64().unwrap_or(0),
                project["running_plan_tasks"].as_u64().unwrap_or(0),
                project["pending_plan_tasks"].as_u64().unwrap_or(0),
                project["active_intake"].as_u64().unwrap_or(0),
                project["queued_intake"].as_u64().unwrap_or(0),
                project["failed_intake"].as_u64().unwrap_or(0)
            );
        }
        return;
    }
    if let Some(tasks) = value.get("tasks").and_then(Value::as_array)
        && value.get("leases").is_some()
    {
        if tasks.is_empty() && value["leases"].as_array().is_none_or(Vec::is_empty) {
            println!("No scheduled tasks or write leases.");
            return;
        }
        for task in tasks {
            println!(
                "{:<10} {:<8} p{:<4} {:<12} {:<24} {}",
                task["status"].as_str().unwrap_or("?"),
                task["task_id"].as_str().unwrap_or("?"),
                task["priority"].as_i64().unwrap_or(0),
                task["department"].as_str().unwrap_or("?"),
                task["session_id"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or("")
            );
            if let Some(error) = task["last_error"].as_str() {
                println!("  last error: {error}");
            }
        }
        for lease in value["leases"].as_array().into_iter().flatten() {
            println!(
                "lease {:<14} {:<20} scopes={}",
                lease["agent_id"].as_str().unwrap_or("?"),
                lease["session_id"].as_str().unwrap_or("?"),
                lease["scopes"]
                    .as_array()
                    .map(|scopes| scopes
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        return;
    }
    if let Some(totals) = value.get("totals")
        && value.get("entries").is_some()
    {
        println!(
            "session {}  calls={}  in={}  out={}  cache_read={}  cache_write={}  cost=${:.4}",
            value["session_id"].as_str().unwrap_or("?"),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cache_read_tokens"].as_u64().unwrap_or(0),
            totals["cache_write_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        );
        for (label, field) in [
            ("role", "by_role"),
            ("dept", "by_department"),
            ("task", "by_task"),
            ("agent", "by_agent"),
            ("model", "by_model"),
        ] {
            for bucket in totals[field].as_array().into_iter().flatten() {
                println!(
                    "  {label:5} {:<20} calls={:<3} in={:<7} out={:<7} cache={}/{} ${:.4}",
                    bucket["key"].as_str().unwrap_or("?"),
                    bucket["calls"].as_u64().unwrap_or(0),
                    bucket["input_tokens"].as_u64().unwrap_or(0),
                    bucket["output_tokens"].as_u64().unwrap_or(0),
                    bucket["cache_read_tokens"].as_u64().unwrap_or(0),
                    bucket["cache_write_tokens"].as_u64().unwrap_or(0),
                    bucket["cost_usd"].as_f64().unwrap_or(0.0)
                );
            }
        }
        return;
    }
    if value.get("session_id").is_some()
        && value.get("worktree").is_some()
        && value.get("commit").is_some()
    {
        println!(
            "rehydrated {} at {} · branch {} · {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["worktree"].as_str().unwrap_or("?"),
            value["branch"].as_str().unwrap_or("?"),
            value["commit"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("session_id").is_some() && value.get("events").is_some() {
        println!(
            "imported session {} into project {} · events={} · status={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            value["events"].as_u64().unwrap_or(0),
            value["status"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("version").is_some() && value.get("exported_at").is_some() {
        println!(
            "session {}  events={}  agents={}  messages={}  queue={}",
            value["session"]["id"].as_str().unwrap_or("?"),
            value["events"].as_array().map_or(0, Vec::len),
            value["agents"].as_array().map_or(0, Vec::len),
            value["messages"].as_array().map_or(0, Vec::len),
            value["merge_queue"].as_array().map_or(0, Vec::len)
        );
        return;
    }
    if value.get("latency_ms").is_some() && value.get("provider").is_some() {
        println!(
            "provider {} / {}  {} ms  in={} out={}  {}",
            value["provider"].as_str().unwrap_or("?"),
            value["model"].as_str().unwrap_or("?"),
            value["latency_ms"].as_u64().unwrap_or(0),
            value["input_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0),
            value["text"].as_str().unwrap_or("")
        );
        return;
    }
    if let Some(checks) = value.get("checks").and_then(Value::as_array)
        && value.get("ok").is_some()
    {
        println!(
            "{}",
            if value["ok"].as_bool().unwrap_or(false) {
                "doctor: ok"
            } else {
                "doctor: errors"
            }
        );
        for check in checks {
            println!(
                "  {:<8} {:<18} {}",
                check["status"].as_str().unwrap_or("?"),
                check["name"].as_str().unwrap_or("?"),
                check["detail"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if value.get("budget").is_some() && value.get("spent_tokens").is_some() {
        println!(
            "project {}  token-budget={}  cost-budget={}  spent-tokens={}  spent-usd={:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            value["budget"]["token_budget"].as_u64().unwrap_or(0),
            value["budget"]["cost_budget_usd"].as_f64().unwrap_or(0.0),
            value["spent_tokens"].as_u64().unwrap_or(0),
            value["spent_cost_usd"].as_f64().unwrap_or(0.0)
        );
        return;
    }
    if value.get("scanned").is_some() && value.get("entries").is_some() {
        println!(
            "project {}  scanned={}  removed={}  skipped={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["scanned"].as_u64().unwrap_or(0),
            value["removed"].as_u64().unwrap_or(0),
            value["skipped"].as_u64().unwrap_or(0)
        );
        for entry in value["entries"].as_array().into_iter().flatten() {
            println!(
                "{}  {}  {}  {}",
                if entry["removed"].as_bool().unwrap_or(false) {
                    "removed"
                } else {
                    "skipped"
                },
                entry["session_id"].as_str().unwrap_or("?"),
                entry["branch"].as_str().unwrap_or("?"),
                entry["reason"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if value.get("artifact_id").is_some() && value.get("path").is_some() {
        println!(
            "materialized {} at {}",
            value["artifact_id"].as_str().unwrap_or("?"),
            value["path"].as_str().unwrap_or("?")
        );
        return;
    }
    if value.get("recent_decisions").is_some() && value.get("contract").is_some() {
        let contract = &value["contract"];
        println!(
            "session {}  project {}  policy={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            value["policy"]["mode"].as_str().unwrap_or("enforce")
        );
        if contract.is_null() {
            println!("no task contract derived yet");
        } else {
            println!(
                "contract {}  role={}  task={}  revision={}",
                contract["hash"].as_str().unwrap_or("?"),
                contract["role"].as_str().unwrap_or("?"),
                contract["task_id"].as_str().unwrap_or("session"),
                contract["plan_revision"].as_u64().unwrap_or(0)
            );
            println!(
                "paths={} forbidden={} acceptance={} egress={} tools={} scope={} mode={}",
                contract["allowed_paths_count"].as_u64().unwrap_or(0),
                contract["forbidden_paths_count"].as_u64().unwrap_or(0),
                contract["acceptance_count"].as_u64().unwrap_or(0),
                contract["egress"].as_str().unwrap_or("?"),
                contract["allowed_tools_count"].as_u64().unwrap_or(0),
                if contract["unrestricted_scope"].as_bool().unwrap_or(false) {
                    "unrestricted"
                } else {
                    "declared"
                },
                contract["enforcement_mode"].as_str().unwrap_or("?")
            );
            if let Some(budget) = contract.get("max_writes").filter(|v| !v.is_null()) {
                println!(
                    "mutation used writes={}/{} bytes={}/{} commits={}/{}",
                    contract["writes_used"].as_u64().unwrap_or(0),
                    budget.as_u64().unwrap_or(0),
                    contract["bytes_used"].as_u64().unwrap_or(0),
                    contract["max_bytes"].as_u64().unwrap_or(0),
                    contract["commits_used"].as_u64().unwrap_or(0),
                    contract["max_commits"].as_u64().unwrap_or(0)
                );
            }
        }
        for decision in value["recent_decisions"].as_array().into_iter().flatten() {
            println!(
                "  #{} {}  {}  {}  {}  risk={}",
                decision["seq"].as_u64().unwrap_or(0),
                decision["decision"].as_str().unwrap_or("?"),
                decision["tool"].as_str().unwrap_or("?"),
                decision["rule"].as_str().unwrap_or("?"),
                decision["reason"].as_str().unwrap_or(""),
                decision["risk"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if value.get("policy").is_some() && value["policy"].get("enforce_shell").is_some() {
        let policy = &value["policy"];
        println!(
            "project {}  contract-policy enabled={} mode={} enforce_shell={} max_writes={:?} max_bytes={:?} max_commits={:?}",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(true),
            policy["mode"].as_str().unwrap_or("enforce"),
            policy["enforce_shell"].as_bool().unwrap_or(false),
            policy["max_writes"].as_u64(),
            policy["max_bytes"].as_u64(),
            policy["max_commits"].as_u64()
        );
        for path in policy["forbidden_paths"].as_array().into_iter().flatten() {
            println!("  forbidden {}", path.as_str().unwrap_or("?"));
        }
        for host in policy["egress_allowlist"].as_array().into_iter().flatten() {
            println!("  egress {}", host.as_str().unwrap_or("?"));
        }
        return;
    }
    if value.get("pins").is_some() && value.get("pinned").is_some() {
        println!(
            "project {}  pins={} drift={} unverified={} removed={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["pinned"].as_u64().unwrap_or(0),
            value["drift"].as_u64().unwrap_or(0),
            value["unverified"].as_u64().unwrap_or(0),
            value["removed"].as_u64().unwrap_or(0)
        );
        for pin in value["pins"].as_array().into_iter().flatten() {
            println!(
                "  {:<11} {} / {}  schema={} description={}",
                pin["status"].as_str().unwrap_or("?"),
                pin["server_name"].as_str().unwrap_or("?"),
                pin["tool_name"].as_str().unwrap_or("?"),
                pin["observed_schema_hash"]
                    .as_str()
                    .or_else(|| pin["schema_hash"].as_str())
                    .unwrap_or("?"),
                pin["observed_description_hash"]
                    .as_str()
                    .or_else(|| pin["description_hash"].as_str())
                    .unwrap_or("?")
            );
        }
        return;
    }
    if value.get("pin").is_some() && value.get("message").is_some() {
        println!(
            "{}  status={}",
            value["message"].as_str().unwrap_or("MCP pin action"),
            value["status"].as_str().unwrap_or("?")
        );
        if let Some(pin) = value.get("pin").filter(|pin| !pin.is_null()) {
            println!(
                "  {} / {}  schema={} description={} approved_by={}",
                pin["server_name"].as_str().unwrap_or("?"),
                pin["tool_name"].as_str().unwrap_or("?"),
                pin["schema_hash"].as_str().unwrap_or("?"),
                pin["description_hash"].as_str().unwrap_or("?"),
                pin["approved_by"].as_str().unwrap_or("-")
            );
        }
        return;
    }
    if let Some(servers) = value.get("servers").and_then(Value::as_array) {
        for server in servers {
            let transport = server["transport"].as_str().unwrap_or("stdio");
            let target = server["url"]
                .as_str()
                .or_else(|| server["command"].as_str())
                .unwrap_or("?");
            println!(
                "{}  {}  {}  {}  enabled={}",
                server["id"].as_str().unwrap_or("?"),
                server["name"].as_str().unwrap_or("?"),
                transport,
                target,
                server["enabled"].as_bool().unwrap_or(false)
            );
        }
        return;
    }
    if let Some(server_name) = value.get("server_name").and_then(Value::as_str)
        && value.get("tools").is_some()
    {
        let name = value["name"].as_str().unwrap_or("?");
        let transport = value["transport"].as_str().unwrap_or("?");
        let version = value["server_version"].as_str().unwrap_or("");
        let protocol = value["protocol_version"].as_str().unwrap_or("?");
        let resources = value["resources"].as_u64().unwrap_or(0);
        let prompts = value["prompts"].as_u64().unwrap_or(0);
        let resource_note = if value["resources_supported"].as_bool().unwrap_or(false) {
            format!("{resources} resources")
        } else {
            "resources unsupported".to_string()
        };
        let prompt_note = if value["prompts_supported"].as_bool().unwrap_or(false) {
            format!("{prompts} prompts")
        } else {
            "prompts unsupported".to_string()
        };
        println!(
            "{name}  {transport}  {server_name} {version}  protocol={protocol}  {} tools  {resource_note}  {prompt_note}",
            value["tools"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(server) = value.get("server") {
        println!(
            "{}  {}  {}  {}",
            server["id"].as_str().unwrap_or("?"),
            server["name"].as_str().unwrap_or("?"),
            server["transport"].as_str().unwrap_or("stdio"),
            server["url"]
                .as_str()
                .or_else(|| server["command"].as_str())
                .unwrap_or("?")
        );
        return;
    }
    if let Some(resources) = value.get("resources").and_then(Value::as_array) {
        for resource in resources {
            println!(
                "{}  <{}>  {}",
                resource["name"].as_str().unwrap_or("?"),
                resource["uri"].as_str().unwrap_or("?"),
                resource["mime_type"].as_str().unwrap_or("")
            );
            if let Some(description) = resource["description"].as_str()
                && !description.is_empty()
            {
                println!("  {description}");
            }
        }
        if let Some(templates) = value.get("templates").and_then(Value::as_array) {
            for template in templates {
                println!(
                    "template {}  <{}>",
                    template["name"].as_str().unwrap_or("?"),
                    template["uri_template"].as_str().unwrap_or("?")
                );
            }
        }
        return;
    }
    if let Some(contents) = value.get("contents").and_then(Value::as_array) {
        for content in contents {
            if let Some(text) = content["text"].as_str() {
                println!("{}", text);
            } else if let Some(blob) = content["blob"].as_str() {
                println!(
                    "[binary {}: {} base64 chars]",
                    content["uri"].as_str().unwrap_or("?"),
                    blob.len()
                );
            }
        }
        return;
    }
    if let Some(prompts) = value.get("prompts").and_then(Value::as_array) {
        for prompt in prompts {
            println!(
                "{}  {}",
                prompt["name"].as_str().unwrap_or("?"),
                prompt["description"].as_str().unwrap_or("")
            );
            if let Some(arguments) = prompt["arguments"].as_array() {
                for argument in arguments {
                    println!(
                        "  {}{}",
                        argument["name"].as_str().unwrap_or("?"),
                        if argument["required"].as_bool().unwrap_or(false) {
                            " (required)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        return;
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array)
        && value.get("server_id").is_some()
    {
        if let Some(description) = value["description"].as_str()
            && !description.is_empty()
        {
            println!("{description}\n");
        }
        for message in messages {
            println!(
                "[{}] {}",
                message["role"].as_str().unwrap_or("user"),
                serde_json::to_string(&message["content"])
                    .unwrap_or_else(|_| message["content"].to_string())
            );
        }
        return;
    }
    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for tool in tools {
            println!(
                "{:<30} {}",
                tool["name"].as_str().unwrap_or("?"),
                tool["description"].as_str().unwrap_or("")
            );
        }
        return;
    }
    if let Some(result) = value.get("result")
        && value.get("tool").is_some()
    {
        println!(
            "{}",
            serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
        );
        return;
    }
    if let Some(memory_id) = value.get("memory_id").and_then(Value::as_str) {
        println!(
            "memory {} removed={}",
            memory_id,
            value["removed"].as_bool().unwrap_or(false)
        );
        return;
    }
    if let Some(removed) = value.get("removed").and_then(Value::as_bool) {
        println!("MCP server removed={removed}");
        return;
    }
    if let Some(questions) = value.get("questions").and_then(Value::as_array) {
        for question in questions {
            println!(
                "[{}] {} · {} · {}",
                question["status"].as_str().unwrap_or("?"),
                question["question"].as_str().unwrap_or("?"),
                question["agent_name"].as_str().unwrap_or("?"),
                question["id"].as_str().unwrap_or("?")
            );
            if let Some(options) = question["options"].as_array()
                && !options.is_empty()
            {
                println!(
                    "  options: {}",
                    options
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
            }
            if let Some(answer) = question["answer"].as_str() {
                println!("  answer: {answer}");
            }
        }
        return;
    }
    if value.get("answered").is_some() && value.get("started").is_none() {
        println!(
            "question answered={}",
            value["answered"].as_bool().unwrap_or(false)
        );
        return;
    }
    if let Some(sha256) = value.get("sha256").and_then(Value::as_str)
        && value.get("size").is_some()
        && value.get("path").is_some()
        && value.get("content").is_none()
    {
        println!(
            "wrote {} ({} bytes, sha256 {})",
            value["path"].as_str().unwrap_or("?"),
            value["size"].as_u64().unwrap_or(0),
            &sha256[..sha256.len().min(12)]
        );
        return;
    }
    if value.get("attempts").is_some() && value.get("instructions").is_some() {
        println!(
            "{}  p{}  {}  {}/{}  {}",
            value["status"].as_str().unwrap_or("?"),
            value["priority"].as_i64().unwrap_or(0),
            value["profile"].as_str().unwrap_or("?"),
            value["attempts"].as_u64().unwrap_or(0),
            value["max_attempts"].as_u64().unwrap_or(0),
            value["title"].as_str().unwrap_or("")
        );
        println!("id={}", value["id"].as_str().unwrap_or("?"));
        if let Some(session) = value["session_id"].as_str() {
            println!("session={session}");
        }
        if let Some(error) = value["last_error"].as_str() {
            println!("last error: {error}");
        }
        return;
    }
    if let Some(priority) = value.get("priority").and_then(Value::as_i64)
        && value.get("session_id").is_some()
        && value.get("commits").is_none()
    {
        println!(
            "session {} priority={}",
            value["session_id"].as_str().unwrap_or("?"),
            priority
        );
        return;
    }
    if let Some(commits) = value.get("commits").and_then(Value::as_array) {
        for commit in commits {
            println!(
                "{}  {}  {}  {}",
                commit["short_hash"].as_str().unwrap_or("?"),
                commit["author_date"].as_str().unwrap_or("?"),
                commit["author"].as_str().unwrap_or("?"),
                commit["subject"].as_str().unwrap_or("?")
            );
            if let Some(refs) = commit["refs"].as_str()
                && !refs.is_empty()
            {
                println!("         {}", refs);
            }
        }
        return;
    }
    if let Some(output) = value.get("output").and_then(Value::as_str)
        && value.get("commit").is_some()
    {
        println!("{output}");
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        for entry in entries {
            println!(
                "{:<4} {:>10}  {}",
                entry["kind"].as_str().unwrap_or("?"),
                entry["size"].as_u64().unwrap_or(0),
                entry["path"].as_str().unwrap_or("?")
            );
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            println!("(truncated)");
        }
        return;
    }
    if let Some(content) = value.get("content").and_then(Value::as_str)
        && value.get("size").is_some()
        && value.get("binary").is_some()
    {
        print!("{content}");
        return;
    }
    if let Some(text) = value.get("text").and_then(Value::as_str)
        && let Some(memories) = value.get("memories").and_then(Value::as_array)
    {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        if !memories.is_empty() {
            let ids = memories
                .iter()
                .filter_map(|memory| memory.get("memory"))
                .filter_map(|memory| memory["id"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("selected: {ids}");
        }
        return;
    }
    if let Some(memories) = value.get("memories").and_then(Value::as_array)
        && memories.iter().any(|memory| memory.get("score").is_some())
    {
        for matched in memories {
            let memory = &matched["memory"];
            let reasons = matched["reasons"]
                .as_array()
                .map(|reasons| {
                    reasons
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            println!(
                "{:>8.3} [{}·{}] {} · {} source={}{}{}",
                matched["score"].as_f64().unwrap_or(0.0),
                memory["kind"].as_str().unwrap_or("note"),
                memory["source"].as_str().unwrap_or("agent"),
                memory["title"].as_str().unwrap_or("?"),
                memory["id"].as_str().unwrap_or("?"),
                matched["source"].as_str().unwrap_or("lexical"),
                matched["lexical_score"]
                    .as_f64()
                    .map(|score| format!(" lexical={score:.3}"))
                    .unwrap_or_default(),
                matched["semantic_score"]
                    .as_f64()
                    .map(|score| format!(" semantic={score:.3}"))
                    .unwrap_or_default()
            );
            println!("  reasons: {reasons}");
            if let Some(reason) = matched["semantic_unavailable"].as_str() {
                println!("  semantic_unavailable: {reason}");
            }
            println!("  {}", memory["body"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(checks) = value.get("checks").and_then(Value::as_array)
        && let Some(memory) = value.get("memory")
    {
        for check in checks {
            println!(
                "{} {} · {} · {}",
                check["status"].as_str().unwrap_or("unchecked"),
                check["evidence"]["kind"].as_str().unwrap_or("?"),
                check["evidence"]["reference"].as_str().unwrap_or("?"),
                check["detail"].as_str().unwrap_or("")
            );
        }
        println!(
            "memory {} status={} verified_at={} stale_reason={}",
            memory["id"].as_str().unwrap_or("?"),
            memory["status"].as_str().unwrap_or("active"),
            memory["last_verified_at"].as_str().unwrap_or("never"),
            memory["stale_reason"].as_str().unwrap_or("none")
        );
        return;
    }
    if let Some(conflicts) = value.get("conflicts").and_then(Value::as_array) {
        if let Some(detected) = value.get("detected").and_then(Value::as_u64) {
            println!(
                "memory conflict scan: detected={} open={}",
                detected,
                value["open"].as_u64().unwrap_or(0)
            );
        }
        if conflicts.is_empty() {
            println!("no memory conflicts");
        }
        for conflict in conflicts {
            println!(
                "[{}·{}] {} · {} · {}",
                conflict["memory_a_kind"].as_str().unwrap_or("?"),
                conflict["memory_b_kind"].as_str().unwrap_or("?"),
                conflict["memory_a_title"].as_str().unwrap_or("?"),
                conflict["memory_b_title"].as_str().unwrap_or("?"),
                conflict["status"].as_str().unwrap_or("open")
            );
            println!(
                "  {} {} · id {}",
                conflict["scope_kind"].as_str().unwrap_or("?"),
                conflict["scope_value"].as_str().unwrap_or("?"),
                conflict["id"].as_str().unwrap_or("?")
            );
            println!("  {}", conflict["reason"].as_str().unwrap_or(""));
            for side in ["a", "b"] {
                let key = format!("memory_{side}_excerpt");
                if let Some(excerpt) = conflict[&key].as_str()
                    && !excerpt.is_empty()
                {
                    println!("  {}: {}", side.to_uppercase(), excerpt);
                }
            }
        }
        return;
    }
    if let Some(conflict) = value.get("conflict") {
        println!(
            "memory conflict {} resolved={} action={} at={}",
            conflict["id"].as_str().unwrap_or("?"),
            conflict["status"].as_str().unwrap_or("?"),
            conflict["resolution"].as_str().unwrap_or("?"),
            conflict["resolved_at"].as_str().unwrap_or("?")
        );
        if let Some(note) = conflict["note"].as_str() {
            println!("  note: {note}");
        }
        if let Some(memories) = value.get("memories").and_then(Value::as_array) {
            for memory in memories {
                println!(
                    "  [{}·{}] {} · {} · {}",
                    memory["kind"].as_str().unwrap_or("?"),
                    memory["source"].as_str().unwrap_or("agent"),
                    memory["title"].as_str().unwrap_or("?"),
                    memory["status"].as_str().unwrap_or("active"),
                    memory["id"].as_str().unwrap_or("?")
                );
            }
        }
        return;
    }
    if value.get("indexable").is_some() && value.get("coverage").is_some() {
        println!(
            "memory embeddings: enabled={} configured={} model={} dimensions={}",
            value["enabled"].as_bool().unwrap_or(false),
            value["configured"].as_bool().unwrap_or(false),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0)
        );
        println!(
            "  coverage={:.1}% indexed={}/{} pending={} failed={} skipped={}",
            value["coverage"].as_f64().unwrap_or(0.0) * 100.0,
            value["indexed"].as_u64().unwrap_or(0),
            value["indexable"].as_u64().unwrap_or(0),
            value["pending"].as_u64().unwrap_or(0),
            value["failed"].as_u64().unwrap_or(0),
            value["skipped"].as_u64().unwrap_or(0)
        );
        if let Some(error) = value["last_error"].as_str() {
            println!("  last_error: {error}");
        }
        if let Some(usage) = value.get("usage") {
            println!(
                "  embedding_calls={} input_tokens={} estimated_tokens={} bytes={} provider_reported_calls={}",
                usage["calls"].as_u64().unwrap_or(0),
                usage["input_tokens"].as_u64().unwrap_or(0),
                usage["estimated_tokens"].as_u64().unwrap_or(0),
                usage["bytes"].as_u64().unwrap_or(0),
                usage["provider_reported_calls"].as_u64().unwrap_or(0)
            );
        }
        if let Some(jobs) = value.get("recent_jobs").and_then(Value::as_array) {
            for job in jobs.iter().take(10) {
                println!(
                    "  {} {} attempts={} updated={}{}",
                    job["status"].as_str().unwrap_or("?"),
                    job["title"].as_str().unwrap_or("?"),
                    job["attempts"].as_u64().unwrap_or(0),
                    job["updated_at"].as_str().unwrap_or("?"),
                    job["last_error"]
                        .as_str()
                        .map(|error| format!(" · {error}"))
                        .unwrap_or_default()
                );
            }
        }
        return;
    }
    if value.get("cleared").is_some() && value.get("enqueued").is_some() {
        println!(
            "memory embeddings rebuild: cleared={} enqueued={} model={} dimensions={} enabled={}",
            value["cleared"].as_u64().unwrap_or(0),
            value["enqueued"].as_u64().unwrap_or(0),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0),
            value["enabled"].as_bool().unwrap_or(false)
        );
        return;
    }
    if value.get("ok").is_some() && value.get("latency_ms").is_some() {
        println!(
            "memory embeddings test: ok={} model={} dimensions={} latency_ms={} input_tokens={} estimated_tokens={} provider_reported={}",
            value["ok"].as_bool().unwrap_or(false),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0),
            value["latency_ms"].as_u64().unwrap_or(0),
            value["input_tokens"].as_u64().unwrap_or(0),
            value["estimated_tokens"].as_u64().unwrap_or(0),
            value["provider_reported"].as_bool().unwrap_or(false)
        );
        if let Some(error) = value["error"].as_str() {
            println!("  error: {error}");
        }
        return;
    }
    if value.get("verification_due").is_some() && value.get("evidence_covered").is_some() {
        println!(
            "memory health: total={} active={} stale={} superseded={} archived={} pinned={}",
            value["total"].as_u64().unwrap_or(0),
            value["active"].as_u64().unwrap_or(0),
            value["stale"].as_u64().unwrap_or(0),
            value["superseded"].as_u64().unwrap_or(0),
            value["archived"].as_u64().unwrap_or(0),
            value["pinned"].as_u64().unwrap_or(0)
        );
        println!(
            "  evidence_covered={} verified={} verification_due={} open_conflicts={}",
            value["evidence_covered"].as_u64().unwrap_or(0),
            value["verified"].as_u64().unwrap_or(0),
            value["verification_due"].as_u64().unwrap_or(0),
            value["open_conflicts"].as_u64().unwrap_or(0)
        );
        if let Some(kinds) = value.get("by_kind").and_then(Value::as_array) {
            let counts = kinds
                .iter()
                .map(|count| {
                    format!(
                        "{}={}",
                        count["key"].as_str().unwrap_or("?"),
                        count["count"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("  by_kind: {counts}");
        }
        return;
    }
    if value.get("by_status").and_then(Value::as_array).is_some() && value.get("total").is_some() {
        println!(
            "memory stats: total={}",
            value["total"].as_u64().unwrap_or(0)
        );
        for key in ["by_status", "by_source", "by_kind"] {
            let counts = value[key]
                .as_array()
                .map(|counts| {
                    counts
                        .iter()
                        .map(|count| {
                            format!(
                                "{}={}",
                                count["key"].as_str().unwrap_or("?"),
                                count["count"].as_u64().unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            println!("  {key}: {counts}");
        }
        return;
    }
    if value.get("marked_stale").is_some() && value.get("candidates").is_some() {
        println!(
            "memory sweep: candidates={} marked_stale={} dry_run={}",
            value["candidates"].as_u64().unwrap_or(0),
            value["marked_stale"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        );
        if let Some(ids) = value.get("memory_ids").and_then(Value::as_array)
            && !ids.is_empty()
        {
            println!(
                "  ids: {}",
                ids.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        return;
    }
    if let Some(memories) = value.get("memories").and_then(Value::as_array) {
        for memory in memories {
            println!(
                "[{}·{}] {} · {} · {}",
                memory["kind"].as_str().unwrap_or("note"),
                memory["source"].as_str().unwrap_or("agent"),
                memory["title"].as_str().unwrap_or("?"),
                memory["status"].as_str().unwrap_or("active"),
                memory["id"].as_str().unwrap_or("?")
            );
            println!("  {}", memory["body"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && policy.get("cooldown_secs").is_some()
    {
        println!(
            "project {}  task-scheduler enabled={} cooldown={}s",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(false),
            policy["cooldown_secs"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && policy.get("enabled").is_some()
    {
        println!(
            "memory policy enabled={} max_items={} max_age_days={} task_result_verify_days={} default_verify_days={} auto_supersede={} brief_max_chars={} sweep_stale={}",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["max_items"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["max_age_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["task_result_verify_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["default_verify_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["auto_supersede"].as_bool().unwrap_or(true),
            policy["brief_max_chars"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["sweep_stale"].as_bool().unwrap_or(false)
        );
        return;
    }
    if let Some(candidates) = value.get("candidates").and_then(Value::as_u64) {
        println!(
            "memory prune: candidates={} removed={} dry_run={}",
            candidates,
            value["removed"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        );
        return;
    }
    if let Some(memory) = value.get("memory") {
        println!(
            "[{}·{}] {} · {} · status={} source={}",
            memory["kind"].as_str().unwrap_or("note"),
            memory["source"].as_str().unwrap_or("agent"),
            memory["title"].as_str().unwrap_or("?"),
            memory["id"].as_str().unwrap_or("?"),
            memory["status"].as_str().unwrap_or("active"),
            memory["source"].as_str().unwrap_or("agent")
        );
        return;
    }
    if value.get("dry_run").is_some() && value.get("projects").is_some() {
        println!(
            "db rebuild{}: projects={} sessions={} events={} backup={}",
            if value["dry_run"].as_bool().unwrap_or(false) {
                " (dry run)"
            } else {
                ""
            },
            value["projects"].as_u64().unwrap_or(0),
            value["sessions"].as_u64().unwrap_or(0),
            value["events"].as_u64().unwrap_or(0),
            value["backup"].as_str().unwrap_or("none")
        );
        for warning in value["skipped"].as_array().into_iter().flatten() {
            println!("  skipped: {}", warning.as_str().unwrap_or("?"));
        }
        return;
    }
    if let Some(started) = value.get("started").and_then(Value::as_bool) {
        println!(
            "session {}  plan-resume-started={}",
            value["session_id"].as_str().unwrap_or("?"),
            started
        );
        return;
    }
    if value.get("session_id").is_some() && value.get("backfilled").is_some() {
        println!(
            "session {}  scanned={}  backfilled={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["scanned"].as_u64().unwrap_or(0),
            value["backfilled"].as_u64().unwrap_or(0)
        );
        return;
    }
    if value.get("hash_mismatches").is_some() && value.get("orphan_blobs").is_some() {
        println!(
            "tool-output audit: {} row(s), {} blob(s), {} missing, {} mismatched, {} unsafe, {} orphan(s) ({} bytes, cleaned {})",
            value["rows"].as_u64().unwrap_or(0),
            value["blob_files"].as_u64().unwrap_or(0),
            value["missing_blobs"].as_u64().unwrap_or(0),
            value["hash_mismatches"].as_u64().unwrap_or(0),
            value["unsafe_blob_files"].as_u64().unwrap_or(0),
            value["orphan_blobs"].as_u64().unwrap_or(0),
            value["orphan_bytes"].as_u64().unwrap_or(0),
            value["cleaned_orphans"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && value.get("total").is_some()
    {
        if entries.is_empty() {
            println!("No spilled tool outputs.");
        } else {
            for entry in entries {
                println!(
                    "{}  {:<12}  {:>8}B  {} lines  {}",
                    entry["id"].as_str().unwrap_or("?"),
                    entry["tool_name"].as_str().unwrap_or("?"),
                    entry["bytes"].as_u64().unwrap_or(0),
                    entry["lines"].as_u64().unwrap_or(0),
                    entry["created_at"].as_str().unwrap_or("?")
                );
            }
            println!(
                "{} of {} handle(s) shown.",
                entries.len(),
                value["total"].as_u64().unwrap_or(0)
            );
        }
        return;
    }
    if value.get("entries").is_some_and(Value::is_u64)
        && value
            .get("policy")
            .is_some_and(|policy| policy.get("spill_threshold_bytes").is_some())
    {
        println!(
            "tool outputs: {} handle(s), {} bytes stored, {} active, {} expired",
            value["entries"].as_u64().unwrap_or(0),
            value["total_bytes"].as_u64().unwrap_or(0),
            value["active_entries"].as_u64().unwrap_or(0),
            value["expired_entries"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(policy) = value.get("policy") {
        if policy.get("spill_threshold_bytes").is_some() {
            println!(
                "project {}  tool-output={}  spill>{}  preview={}  turn-budget={}  retention={}s  max-entries={}",
                value["project_id"].as_str().unwrap_or("?"),
                if policy["enabled"].as_bool().unwrap_or(true) {
                    "enabled"
                } else {
                    "disabled"
                },
                policy["spill_threshold_bytes"].as_u64().unwrap_or(0),
                policy["preview_bytes"].as_u64().unwrap_or(0),
                policy["max_total_bytes_per_turn"].as_u64().unwrap_or(0),
                policy["retention_secs"].as_u64().unwrap_or(0),
                policy["max_entries"].as_u64().unwrap_or(0)
            );
            return;
        }
        if policy.get("delete_unread").is_some() {
            println!(
                "project {}  mailbox-max-age={}  mailbox-max-count={}  delete-unread={}  auto-gc={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["max_age_secs"].as_u64().unwrap_or(0),
                policy["max_count"].as_u64().unwrap_or(0),
                policy["delete_unread"].as_bool().unwrap_or(false),
                policy["auto_gc"].as_bool().unwrap_or(false)
            );
            return;
        }
        if policy.get("auto_gc").is_some() {
            println!(
                "project {}  auto-gc={}  max-bytes={}  max-age={}  max-count={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["auto_gc"].as_bool().unwrap_or(false),
                policy["max_bytes"].as_u64().unwrap_or(0),
                policy["max_age_secs"].as_u64().unwrap_or(0),
                policy["max_count"].as_u64().unwrap_or(0)
            );
            return;
        }
        if policy.get("enabled").is_some() && policy.get("older_than_secs").is_some() {
            println!(
                "project {}  auto-cleanup={}  older-than={}d  delete-branches={}  keep-failed={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["enabled"].as_bool().unwrap_or(false),
                policy["older_than_secs"].as_u64().unwrap_or(0) / 86_400,
                policy["delete_branches"].as_bool().unwrap_or(false),
                policy["keep_failed"].as_bool().unwrap_or(true)
            );
            return;
        }
        if policy.get("auto_resolve").is_some() {
            println!(
                "project {}  auto-resolve={}  max-attempts={}  max-tokens={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["auto_resolve"].as_bool().unwrap_or(false),
                policy["max_auto_attempts"].as_u64().unwrap_or(0),
                policy["max_resolve_tokens"]
                    .as_u64()
                    .map_or_else(|| "session budget".to_string(), |value| value.to_string())
            );
            return;
        }
        println!(
            "project {}  network={}  restrict-home={}  read-jail={}",
            value["project_id"].as_str().unwrap_or("?"),
            policy["network"].as_str().unwrap_or("?"),
            policy["restrict_home"].as_bool().unwrap_or(false),
            policy["read_jail"].as_bool().unwrap_or(false)
        );
        let paths = policy["read_deny"].as_array();
        if paths.is_none_or(|paths| paths.is_empty()) {
            println!("read-deny: none");
        } else {
            for path in paths.into_iter().flatten() {
                println!("read-deny: {}", path.as_str().unwrap_or("?"));
            }
        }
        return;
    }
    if let Some(projects) = value.get("projects").and_then(Value::as_array) {
        if projects.is_empty() {
            println!("No projects. Add one with: vp project-add <path>");
        }
        for project in projects {
            println!(
                "{}  {}  [{}]",
                project["id"].as_str().unwrap_or("?"),
                project["name"].as_str().unwrap_or("?"),
                project["default_branch"].as_str().unwrap_or("?")
            );
            println!("  {}", project["root"].as_str().unwrap_or("?"));
        }
        return;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array) {
        if sessions.is_empty() {
            println!("No sessions.");
        }
        for session in sessions {
            println!(
                "{}  {}  [{}]",
                session["id"].as_str().unwrap_or("?"),
                session["goal"].as_str().unwrap_or("?"),
                session["status"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(plan) = value.get("plan").filter(|plan| !plan.is_null()) {
        println!("GOAL  {}", plan["goal"].as_str().unwrap_or("?"));
        for department in plan["departments"].as_array().into_iter().flatten() {
            println!(
                "  {:<16} manager {:<16} profile {}",
                department["name"].as_str().unwrap_or("?"),
                department["manager_title"].as_str().unwrap_or("?"),
                department["profile"].as_str().unwrap_or("?")
            );
        }
        for task in plan["tasks"].as_array().into_iter().flatten() {
            println!(
                "  task {:<12} {:<16} kind {}",
                task["id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["kind"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(entry) = value.get("entry").filter(|entry| !entry.is_null()) {
        println!(
            "{}  {}  {}",
            entry["status"].as_str().unwrap_or("?"),
            entry["agent_name"].as_str().unwrap_or("?"),
            entry["commit"].as_str().unwrap_or("?")
        );
        if let Some(detail) = entry["detail"].as_str() {
            println!("{detail}");
        }
        return;
    }
    if value.get("mirror_events").is_some() && value.get("database_events").is_some() {
        println!(
            "{}  mirror={} database={}  {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["mirror_events"].as_u64().unwrap_or(0),
            value["database_events"].as_u64().unwrap_or(0),
            value["detail"].as_str().unwrap_or("")
        );
        return;
    }
    if let Some(consistent) = value.get("consistent").and_then(Value::as_bool) {
        println!(
            "{}  events={}  status={} usage={} ledger={} mailbox={} artifacts={} approvals={} plan={} todos={} messages={} agents={} queue={}",
            if consistent { "consistent" } else { "MISMATCH" },
            value["event_count"].as_u64().unwrap_or(0),
            value["status_matches"].as_bool().unwrap_or(false),
            value["usage_matches"].as_bool().unwrap_or(false),
            value["ledger_matches"].as_bool().unwrap_or(false),
            value["mailbox_matches"].as_bool().unwrap_or(false),
            value["artifacts_matches"].as_bool().unwrap_or(false),
            value["approvals_matches"].as_bool().unwrap_or(false),
            value["plan_matches"].as_bool().unwrap_or(false),
            value["todos_matches"].as_bool().unwrap_or(false),
            value["messages_matches"].as_bool().unwrap_or(false),
            value["agents_matches"].as_bool().unwrap_or(false),
            value["queue_matches"].as_bool().unwrap_or(false)
        );
        if value["rebuilt"].as_bool().unwrap_or(false) {
            println!("rebuilt projections from the event stream.");
        } else if value["repaired"].as_bool().unwrap_or(false) {
            println!("repaired projections from the event stream.");
        }
        for mismatch in value["mismatches"].as_array().into_iter().flatten() {
            if let Some(mismatch) = mismatch.as_str() {
                println!("  - {mismatch}");
            }
        }
        return;
    }
    if let Some(target) = value.get("target_branch").and_then(Value::as_str) {
        println!(
            "landed {} → {} at {}",
            value["branch"].as_str().unwrap_or("?"),
            target,
            value["commit"].as_str().unwrap_or("?")
        );
        if let Some(detail) = value["detail"].as_str() {
            println!("{detail}");
        }
        return;
    }
    if value.get("removed").is_some() && value.get("bytes_freed").is_none() {
        println!(
            "mailbox GC removed {} message(s)",
            value["removed"].as_u64().unwrap_or(0)
        );
        return;
    }
    if value.get("removed").is_some() && value.get("bytes_freed").is_some() {
        println!(
            "artifact GC removed {} artifact(s), freed {} bytes",
            value["removed"].as_u64().unwrap_or(0),
            value["bytes_freed"].as_u64().unwrap_or(0)
        );
        return;
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        if messages.is_empty() {
            println!("Mailbox is empty.");
        }
        for message in messages {
            println!(
                "{}  {} → {}  {}  [{}]",
                message["id"].as_str().unwrap_or("?"),
                message["from_name"].as_str().unwrap_or("?"),
                message["to_name"].as_str().unwrap_or("?"),
                message["kind"].as_str().unwrap_or("?"),
                message["status"].as_str().unwrap_or("?")
            );
            if let Some(body) = message["body"].as_str() {
                println!("  {body}");
            }
        }
        return;
    }
    if let Some(message) = value.get("message") {
        println!(
            "{} → {}  {}  [{}]",
            message["from_name"].as_str().unwrap_or("?"),
            message["to_name"].as_str().unwrap_or("?"),
            message["kind"].as_str().unwrap_or("?"),
            message["status"].as_str().unwrap_or("?")
        );
        if let Some(body) = message["body"].as_str() {
            println!("{body}");
        }
        return;
    }
    if let Some(artifacts) = value.get("artifacts").and_then(Value::as_array) {
        if artifacts.is_empty() {
            println!("No artifacts.");
        }
        for artifact in artifacts {
            println!(
                "{}  {:<24} {:<10} {}",
                artifact["id"].as_str().unwrap_or("?"),
                artifact["name"].as_str().unwrap_or("?"),
                artifact["kind"].as_str().unwrap_or("?"),
                artifact["agent_name"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(audit) = value.get("audit")
        && audit.get("messages_with_citations").is_some()
    {
        println!(
            "{} messages with citations · {} without · {} citations · {} sources · dangling {} · unused {}",
            audit["messages_with_citations"].as_u64().unwrap_or(0),
            audit["messages_without_citations"].as_u64().unwrap_or(0),
            audit["total_citations"].as_u64().unwrap_or(0),
            audit["unique_sources"].as_u64().unwrap_or(0),
            audit["dangling_key_count"].as_u64().unwrap_or(0),
            audit["unused_citation_count"].as_u64().unwrap_or(0),
        );
        for message in audit["messages"].as_array().into_iter().flatten() {
            let dangling = message["dangling_keys"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            let unused = message["unused_keys"].as_array().map(Vec::len).unwrap_or(0);
            if dangling == 0 && unused == 0 {
                continue;
            }
            println!(
                "  {} · citations {} · dangling {} · unused {}",
                message["message_id"].as_str().unwrap_or("?"),
                message["citation_count"].as_u64().unwrap_or(0),
                message["dangling_keys"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
                message["unused_keys"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
            println!(
                "    {}",
                message["excerpt"]
                    .as_str()
                    .unwrap_or("")
                    .replace('\n', " ")
                    .chars()
                    .take(180)
                    .collect::<String>()
            );
        }
        return;
    }
    if let Some(citations) = value.get("citations").and_then(Value::as_array)
        && citations.iter().all(|item| item.get("citation").is_some())
    {
        if citations.is_empty() {
            println!("No citations.");
            return;
        }
        for item in citations {
            let citation = &item["citation"];
            println!(
                "[{}] {} · {} · chunk {} · {}",
                citation["key"].as_str().unwrap_or("?"),
                citation["source_title"].as_str().unwrap_or("?"),
                citation["heading"].as_str().unwrap_or(""),
                citation["chunk_no"].as_u64().unwrap_or(0),
                item["message_id"].as_str().unwrap_or("?"),
            );
            println!("  {}", citation["excerpt"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(diff) = value.get("diff") {
        println!(
            "{}  v{} -> v{}  +{} -{} unchanged {}",
            value["name"].as_str().unwrap_or("?"),
            value["from_version"].as_u64().unwrap_or(0),
            value["to_version"].as_u64().unwrap_or(0),
            diff["additions"].as_u64().unwrap_or(0),
            diff["deletions"].as_u64().unwrap_or(0),
            diff["unchanged"].as_u64().unwrap_or(0),
        );
        if diff["identical"].as_bool() == Some(true) {
            println!("identical");
            return;
        }
        for hunk in diff["hunks"].as_array().into_iter().flatten() {
            println!(
                "@@ -{},{} +{},{} @@",
                hunk["old_start"].as_u64().unwrap_or(0),
                hunk["old_lines"].as_u64().unwrap_or(0),
                hunk["new_start"].as_u64().unwrap_or(0),
                hunk["new_lines"].as_u64().unwrap_or(0)
            );
            for line in hunk["lines"].as_array().into_iter().flatten() {
                let prefix = match line["kind"].as_str() {
                    Some("add") => '+',
                    Some("delete") => '-',
                    _ => ' ',
                };
                let old = line["old_line"]
                    .as_u64()
                    .map(|value| format!("{value:>4}"))
                    .unwrap_or_else(|| "    ".to_string());
                let new = line["new_line"]
                    .as_u64()
                    .map(|value| format!("{value:>4}"))
                    .unwrap_or_else(|| "    ".to_string());
                println!(
                    "{old} {new} {prefix} {}",
                    line["text"].as_str().unwrap_or("")
                );
            }
        }
        if diff["truncated"].as_bool() == Some(true) {
            println!("(diff truncated by the line/edit budget)");
        }
        return;
    }
    if let Some(versions) = value.get("versions").and_then(Value::as_array) {
        if versions.is_empty() {
            println!("No saved versions.");
            return;
        }
        for version in versions {
            println!(
                "v{}  {}  {} bytes  {}",
                version["version"].as_u64().unwrap_or(0),
                version["name"].as_str().unwrap_or("?"),
                version["size_bytes"].as_u64().unwrap_or(0),
                version["created_at"].as_str().unwrap_or("?")
            );
        }
        return;
    }
    if let Some(artifact) = value.get("artifact") {
        println!(
            "{}  {}  [{}]  {}  {} bytes",
            artifact["name"].as_str().unwrap_or("?"),
            artifact["kind"].as_str().unwrap_or("?"),
            artifact["mime_type"].as_str().unwrap_or("?"),
            artifact["storage"].as_str().unwrap_or("?"),
            artifact["size_bytes"].as_u64().unwrap_or(0)
        );
        if artifact["storage"].as_str() == Some("file") {
            println!(
                "stored at {}",
                artifact["path"].as_str().unwrap_or("(path unavailable)")
            );
        } else {
            println!("{}", artifact["content"].as_str().unwrap_or(""));
        }
        return;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        if entries.is_empty() {
            println!("Merge queue is empty.");
        }
        for entry in entries {
            println!(
                "{:<10} {:<10} {:<16} {}",
                entry["status"].as_str().unwrap_or("?"),
                entry["task_id"].as_str().unwrap_or("—"),
                entry["agent_name"].as_str().unwrap_or("?"),
                entry["commit"].as_str().unwrap_or("?")
            );
            if let Some(detail) = entry["detail"].as_str() {
                println!("  {detail}");
            }
        }
        return;
    }
    if let Some(agents) = value.get("agents").and_then(Value::as_array) {
        if agents.is_empty() {
            println!("No agents.");
        }
        for agent in agents {
            println!(
                "{:<8} {:<16} {:<10} {}",
                agent["role"].as_str().unwrap_or("?"),
                agent["name"].as_str().unwrap_or("?"),
                agent["status"].as_str().unwrap_or("?"),
                agent["id"].as_str().unwrap_or("?")
            );
            if let Some(title) = agent["title"].as_str() {
                println!("         title {title}");
            }
            if let Some(profile) = agent["profile"].as_str() {
                println!("         profile {profile}");
            }
            if let Some(branch) = agent["branch"].as_str() {
                println!("         branch {branch}");
            }
        }
        return;
    }
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        if items.is_empty() {
            println!("No plan yet.");
        }
        for (index, item) in items.iter().enumerate() {
            let status = item["status"].as_str().unwrap_or("?");
            let marker = match status {
                "completed" => "✓",
                "in_progress" => "•",
                _ => "○",
            };
            println!(
                "{marker} {}. {}  [{}]",
                index + 1,
                item["title"].as_str().unwrap_or("?"),
                status
            );
        }
        return;
    }
    if let Some(paused) = value.get("paused").and_then(Value::as_bool) {
        println!(
            "{}",
            if paused {
                "paused the active run."
            } else {
                "no active run to pause."
            }
        );
        return;
    }
    if let Some(resumed) = value.get("resumed").and_then(Value::as_bool) {
        println!(
            "{}",
            if resumed {
                "resumed the active run."
            } else {
                "no paused run to resume."
            }
        );
        return;
    }
    if let Some(preempted) = value.get("preempted").and_then(Value::as_bool)
        && value.get("records").is_none()
        && value.get("detail").is_some()
    {
        let detail = value
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if preempted {
            println!("hard preemption signalled.");
        } else {
            println!("no active run was preempted.");
        }
        if !detail.is_empty() {
            println!("  {detail}");
        }
        return;
    }
    if let Some(cancelled) = value.get("cancelled").and_then(Value::as_bool) {
        println!(
            "{}",
            if cancelled {
                "cancellation requested."
            } else {
                "no active run for that session."
            }
        );
        return;
    }
    if value.get("discarded").and_then(Value::as_bool) == Some(true) {
        println!("discarded uncommitted changes.");
        return;
    }
    if let Some(commit) = value.get("commit").and_then(Value::as_str) {
        println!("committed {commit}");
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            println!("  {message}");
        }
        if let Some(summary) = value.get("summary").and_then(Value::as_str) {
            println!("{summary}");
        }
        return;
    }
    if value.get("patch").is_some() {
        if let Some(status) = value.get("status").and_then(Value::as_str)
            && !status.is_empty()
        {
            println!("{status}\n");
        }
        if let Some(patch) = value.get("patch").and_then(Value::as_str) {
            if patch.is_empty() {
                println!("No changes.");
            } else {
                println!("{patch}");
            }
        }
        return;
    }
    if let Some(approvals) = value.get("approvals").and_then(Value::as_array) {
        if approvals.is_empty() {
            println!("No approvals.");
        }
        for approval in approvals {
            println!(
                "{}  {}  [{}]  risk={}  waiters={}  priority={}",
                approval["id"].as_str().unwrap_or("?"),
                approval["tool"].as_str().unwrap_or("?"),
                approval["status"].as_str().unwrap_or("?"),
                approval["approval_risk"].as_str().unwrap_or("-"),
                approval["waiter_count"].as_u64().unwrap_or(0),
                approval["priority"].as_i64().unwrap_or(0)
            );
            if let Some(template) = approval["template_id"].as_str() {
                println!("  template {template}");
            }
            if let Some(expires) = approval["expires_at"].as_str() {
                println!("  expires {expires}");
            }
            if let Some(reason) = approval["approval_reason"].as_str() {
                println!("  reason {reason}");
            }
            println!("  {}", approval["input"]);
        }
        return;
    }
    if let Some(events) = value.get("events").and_then(Value::as_array) {
        if events.is_empty() {
            println!("No events.");
        }
        for event in events {
            println!(
                "#{:04}  {}  {}",
                event["seq"].as_u64().unwrap_or(0),
                event["created_at"].as_str().unwrap_or("?"),
                event["type"].as_str().unwrap_or("?")
            );
            if !event["payload"].is_null() {
                println!("      {}", event["payload"]);
            }
        }
        return;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}
