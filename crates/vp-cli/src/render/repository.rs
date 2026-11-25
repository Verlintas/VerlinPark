//! Backup repository, keyring, and portable-anchor renderers.

use super::support::*;
use super::*;

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
        "  validation valid={}  blobs present={} missing={} verified={} corrupt={} unverified={}  mode={}",
        validation["valid"].as_bool().unwrap_or(false),
        validation["blobs_present"].as_u64().unwrap_or(0),
        validation["blobs_missing"].as_u64().unwrap_or(0),
        validation["blobs_verified"].as_u64().unwrap_or(0),
        validation["blobs_corrupt"].as_u64().unwrap_or(0),
        validation["blobs_unverified"].as_u64().unwrap_or(0),
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
        "  chain_valid={}  tombstones_valid={}  entries_valid={}  blobs present={} missing={} verified={} corrupt={} unverified={}",
        value["chain_valid"].as_bool().unwrap_or(false),
        value["tombstones_valid"].as_bool().unwrap_or(false),
        value["entries_valid"].as_bool().unwrap_or(false),
        value["blobs_present"].as_u64().unwrap_or(0),
        value["blobs_missing"].as_u64().unwrap_or(0),
        value["blobs_verified"].as_u64().unwrap_or(0),
        value["blobs_corrupt"].as_u64().unwrap_or(0),
        value["blobs_unverified"].as_u64().unwrap_or(0)
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
