//! Lightweight release update checks. This intentionally does not install
//! updates: the app is distributed as an ad-hoc DMG and signing/notarization is
//! deferred, so the safe action is to report the latest release and open its
//! page.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::time::Duration;
use vp_protocol::APP_VERSION;
use vp_protocol::types::UpdateCheckResult;

const DEFAULT_REPO: &str = "Verlintas/VerlinPark";
const MAX_REPO_LEN: usize = 120;

pub async fn check_for_update(repo: Option<&str>) -> Result<UpdateCheckResult> {
    let repo = repo
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("VERLINPARK_UPDATE_REPO").ok())
        .unwrap_or_else(|| DEFAULT_REPO.to_string());
    validate_repo(&repo)?;

    let client = reqwest::Client::builder()
        .user_agent(format!("VerlinPark/{APP_VERSION}"))
        .timeout(Duration::from_secs(12))
        .build()
        .context("building update HTTP client")?;
    let response = client
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("checking the latest GitHub release")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "GitHub release check returned {status}: {}",
            body.chars().take(300).collect::<String>()
        );
    }
    let payload: Value = response
        .json()
        .await
        .context("decoding the latest GitHub release")?;
    parse_release(&payload, APP_VERSION, &repo)
}

fn validate_repo(repo: &str) -> Result<()> {
    if repo.len() > MAX_REPO_LEN {
        bail!("update repository is too long");
    }
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || owner.is_empty()
        || name.is_empty()
        || !repo
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._/".contains(character))
    {
        bail!("update repository must look like owner/repo");
    }
    Ok(())
}

fn parse_release(payload: &Value, current: &str, repo: &str) -> Result<UpdateCheckResult> {
    let tag = payload
        .get("tag_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .ok_or_else(|| anyhow!("release has no tag_name"))?;
    let latest = tag.trim_start_matches(['v', 'V']).to_string();
    let update_available = version_is_newer(&latest, current);
    Ok(UpdateCheckResult {
        current_version: current.to_string(),
        latest_version: latest,
        update_available,
        repo: repo.to_string(),
        release_name: payload
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .map(str::to_string),
        release_url: payload
            .get("html_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        published_at: payload
            .get("published_at")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    let latest = version_key(latest);
    let current = version_key(current);
    if latest.is_empty() || current.is_empty() {
        return false;
    }
    let len = latest.len().max(current.len());
    for index in 0..len {
        let left = latest.get(index).copied().unwrap_or(0);
        let right = current.get(index).copied().unwrap_or(0);
        if left != right {
            return left > right;
        }
    }
    false
}

fn version_key(value: &str) -> Vec<u64> {
    value
        .trim_start_matches(['v', 'V'])
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compares_versions_with_prefixes_and_suffixes() {
        assert!(version_is_newer("0.2.0", "0.1.0"));
        assert!(version_is_newer("v0.1.1", "0.1.0"));
        assert!(version_is_newer("1.0.0", "0.99.99"));
        assert!(!version_is_newer("0.1.0", "0.1.0"));
        assert!(!version_is_newer("0.1.0-dev", "0.1.0"));
        assert!(!version_is_newer("", "0.1.0"));
    }

    #[test]
    fn parses_a_github_release_payload() {
        let payload = json!({
            "tag_name": "v0.2.0",
            "name": "VerlinPark 0.2.0",
            "html_url": "https://example.com/releases/v0.2.0",
            "published_at": "2026-11-08T00:00:00Z"
        });
        let result = parse_release(&payload, "0.1.0-dev", "owner/repo").unwrap();
        assert_eq!(result.current_version, "0.1.0-dev");
        assert_eq!(result.latest_version, "0.2.0");
        assert!(result.update_available);
        assert_eq!(result.release_name.as_deref(), Some("VerlinPark 0.2.0"));
    }

    #[test]
    fn rejects_unsafe_repository_slugs() {
        assert!(validate_repo("owner/repo").is_ok());
        assert!(validate_repo("https://example.com/repo").is_err());
        assert!(validate_repo("owner/repo/extra").is_err());
        assert!(validate_repo("owner").is_err());
    }
}
