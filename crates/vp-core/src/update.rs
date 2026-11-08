//! Lightweight release update checks. This intentionally does not install
//! updates: the app is distributed as an ad-hoc DMG and signing/notarization is
//! deferred, so the safe action is to report the latest release and open its
//! page.

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;
use vp_protocol::APP_VERSION;
use vp_protocol::types::{UpdateCheckResult, UpdateDownloadResult};

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

pub async fn download_update(repo: Option<&str>) -> Result<UpdateDownloadResult> {
    let release = check_for_update(repo).await?;
    if !release.update_available {
        bail!("already up to date (latest {})", release.latest_version);
    }
    let dmg_url = release
        .dmg_url
        .clone()
        .ok_or_else(|| anyhow!("release has no .dmg asset"))?;
    let sha_url = release
        .sha256_url
        .clone()
        .ok_or_else(|| anyhow!("release has no .sha256 asset"))?;
    validate_release_asset_url(&release.repo, &dmg_url)?;
    validate_release_asset_url(&release.repo, &sha_url)?;
    let client = update_client()?;
    let checksum = fetch_checksum(&client, &sha_url).await?;
    let downloads = downloads_dir()?;
    std::fs::create_dir_all(&downloads)
        .with_context(|| format!("creating {}", downloads.display()))?;
    let file_name = sanitize_asset_name(
        release
            .dmg_name
            .as_deref()
            .unwrap_or("VerlinPark-update.dmg"),
    );
    let final_path = downloads.join(&file_name);
    let part_path = downloads.join(format!(".{file_name}.{}.part", Uuid::now_v7()));
    let result = download_to_path(&client, &dmg_url, &part_path).await?;
    if let Err(error) = verify_checksum(&checksum.sha256, &result.sha256) {
        let _ = std::fs::remove_file(&part_path);
        return Err(error);
    }
    std::fs::rename(&part_path, &final_path)
        .with_context(|| format!("moving download to {}", final_path.display()))?;
    Ok(UpdateDownloadResult {
        version: release.latest_version,
        path: final_path.to_string_lossy().to_string(),
        sha256: result.sha256,
        size: result.size,
        verified: true,
    })
}

fn update_client() -> Result<reqwest::Client> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let Some(host) = attempt.url().host_str() else {
            return attempt.error("redirect without host");
        };
        if host == "github.com" || host.ends_with(".githubusercontent.com") {
            attempt.follow()
        } else {
            attempt.error("redirect to an unexpected host")
        }
    });
    reqwest::Client::builder()
        .user_agent(format!("VerlinPark/{APP_VERSION}"))
        .timeout(Duration::from_secs(120))
        .redirect(policy)
        .build()
        .context("building update download client")
}

fn downloads_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(home.join("Downloads"))
}

fn sanitize_asset_name(name: &str) -> String {
    let cleaned = name
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '+')
        })
        .collect::<String>();
    let cleaned = cleaned.trim_matches('.');
    if cleaned.is_empty() || !cleaned.to_ascii_lowercase().ends_with(".dmg") {
        "VerlinPark-update.dmg".to_string()
    } else {
        cleaned.to_string()
    }
}

fn validate_release_asset_url(repo: &str, url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).context("parsing release asset URL")?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("github.com") {
        bail!("release asset must be an HTTPS github.com URL");
    }
    let prefix = format!("/{repo}/releases/download/");
    if !parsed.path().starts_with(&prefix) {
        bail!("release asset URL does not belong to {repo}");
    }
    Ok(())
}

fn verify_checksum(expected: &str, actual: &str) -> Result<()> {
    if expected.eq_ignore_ascii_case(actual) {
        Ok(())
    } else {
        bail!("download checksum mismatch: expected {expected}, got {actual}")
    }
}

struct Checksum {
    sha256: String,
}

async fn fetch_checksum(client: &reqwest::Client, url: &str) -> Result<Checksum> {
    let response = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .send()
        .await
        .context("downloading SHA-256 asset")?;
    if !response.status().is_success() {
        bail!("SHA-256 asset returned {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|length| length > 1_048_576)
    {
        bail!("SHA-256 asset is unexpectedly large");
    }
    let bytes = response.bytes().await.context("reading SHA-256 asset")?;
    if bytes.len() > 1_048_576 {
        bail!("SHA-256 asset is unexpectedly large");
    }
    let text = String::from_utf8_lossy(&bytes);
    let sha256 = text
        .split_whitespace()
        .find(|part| {
            part.len() == 64 && part.chars().all(|character| character.is_ascii_hexdigit())
        })
        .map(|part| part.to_ascii_lowercase())
        .ok_or_else(|| anyhow!("SHA-256 asset contains no 64-character checksum"))?;
    Ok(Checksum { sha256 })
}

struct Downloaded {
    sha256: String,
    size: u64,
}

async fn download_to_path(client: &reqwest::Client, url: &str, path: &Path) -> Result<Downloaded> {
    const MAX_BYTES: u64 = 512 * 1024 * 1024;
    let response = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .send()
        .await
        .context("downloading update asset")?;
    if !response.status().is_success() {
        bail!("update asset returned {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES)
    {
        bail!(
            "update asset exceeds the {} MB limit",
            MAX_BYTES / 1024 / 1024
        );
    }
    let mut file = tokio::fs::File::create(path)
        .await
        .with_context(|| format!("creating {}", path.display()))?;
    let mut stream = response.bytes_stream();
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading update asset stream")?;
        size = size.saturating_add(chunk.len() as u64);
        if size > MAX_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(path).await;
            bail!(
                "update asset exceeds the {} MB limit",
                MAX_BYTES / 1024 / 1024
            );
        }
        hasher.update(&chunk);
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .context("writing update asset")?;
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .context("flushing update asset")?;
    Ok(Downloaded {
        sha256: hex(&hasher.finalize()),
        size,
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn release_assets(
    payload: &Value,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let Some(assets) = payload.get("assets").and_then(Value::as_array) else {
        return (None, None, None, None);
    };
    let mut dmg: Option<(String, String)> = None;
    let mut sha: Option<(String, String)> = None;
    for asset in assets {
        let Some(name) = asset.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(url) = asset.get("browser_download_url").and_then(Value::as_str) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".dmg") && dmg.is_none() {
            dmg = Some((name.to_string(), url.to_string()));
        } else if lower.ends_with(".sha256") && sha.is_none() {
            sha = Some((name.to_string(), url.to_string()));
        }
    }
    (
        dmg.as_ref().map(|(name, _)| name.clone()),
        dmg.map(|(_, url)| url),
        sha.as_ref().map(|(name, _)| name.clone()),
        sha.map(|(_, url)| url),
    )
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
    let (dmg_name, dmg_url, sha256_name, sha256_url) = release_assets(payload);
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
        dmg_name,
        dmg_url,
        sha256_name,
        sha256_url,
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
    fn parses_dmg_and_checksum_assets() {
        let payload = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "VerlinPark-0.2.0.dmg", "browser_download_url": "https://github.com/owner/repo/releases/download/v0.2.0/VerlinPark-0.2.0.dmg"},
                {"name": "VerlinPark-0.2.0.dmg.sha256", "browser_download_url": "https://github.com/owner/repo/releases/download/v0.2.0/VerlinPark-0.2.0.dmg.sha256"}
            ]
        });
        let (dmg_name, dmg_url, sha_name, sha_url) = release_assets(&payload);
        assert_eq!(dmg_name.as_deref(), Some("VerlinPark-0.2.0.dmg"));
        assert!(dmg_url.unwrap().ends_with(".dmg"));
        assert_eq!(sha_name.as_deref(), Some("VerlinPark-0.2.0.dmg.sha256"));
        assert!(sha_url.unwrap().ends_with(".sha256"));
    }

    #[test]
    fn sanitizes_download_names_and_rejects_unsafe_urls() {
        assert_eq!(sanitize_asset_name("../../evil.dmg"), "evil.dmg");
        assert_eq!(
            sanitize_asset_name("VerlinPark 1.0.dmg"),
            "VerlinPark1.0.dmg"
        );
        assert_eq!(
            sanitize_asset_name("not-a-dmg.zip"),
            "VerlinPark-update.dmg"
        );
        assert!(
            validate_release_asset_url(
                "owner/repo",
                "https://github.com/owner/repo/releases/download/v1/a.dmg"
            )
            .is_ok()
        );
        assert!(
            validate_release_asset_url(
                "owner/repo",
                "https://example.com/owner/repo/releases/download/v1/a.dmg"
            )
            .is_err()
        );
        assert!(
            validate_release_asset_url(
                "owner/repo",
                "https://github.com/other/repo/releases/download/v1/a.dmg"
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn downloads_an_asset_and_verifies_its_hash() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let body = b"VerlinPark DMG bytes".to_vec();
        Mock::given(method("GET"))
            .and(path("/asset.dmg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("asset.dmg");
        let client = reqwest::Client::new();
        let downloaded = download_to_path(&client, &format!("{}/asset.dmg", server.uri()), &target)
            .await
            .unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&body);
        assert_eq!(downloaded.sha256, hex(&hasher.finalize()));
        assert_eq!(downloaded.size, body.len() as u64);
        assert_eq!(std::fs::read(&target).unwrap(), body);
    }

    #[test]
    fn verifies_checksum_case_insensitively() {
        let checksum = "a".repeat(64);
        assert!(verify_checksum(&checksum, &checksum.to_ascii_uppercase()).is_ok());
        assert!(verify_checksum(&checksum, &"b".repeat(64)).is_err());
    }

    #[tokio::test]
    async fn parses_a_sha256_asset() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let checksum = "a".repeat(64);
        Mock::given(method("GET"))
            .and(path("/asset.sha256"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("{checksum}  VerlinPark-0.2.0.dmg\n")),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let parsed = fetch_checksum(&client, &format!("{}/asset.sha256", server.uri()))
            .await
            .unwrap();
        assert_eq!(parsed.sha256, checksum);
    }

    #[test]
    fn rejects_unsafe_repository_slugs() {
        assert!(validate_repo("owner/repo").is_ok());
        assert!(validate_repo("https://example.com/repo").is_err());
        assert!(validate_repo("owner/repo/extra").is_err());
        assert!(validate_repo("owner").is_err());
    }
}
