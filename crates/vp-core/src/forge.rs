//! Forge provider abstraction and Bitbucket Cloud pull-request creation.
//!
//! PR/MR creation used to be a GitHub/GitLab/Gitea CLI special case inside
//! `Engine::open_pull_request`. This module keeps those CLI providers behind one
//! small interface and adds an explicit Bitbucket Cloud REST provider.
//!
//! Design invariants:
//!
//! - Forge credentials come only from the daemon environment. They are never
//!   serialized, persisted, logged, or placed in an event/error payload.
//! - A pull request is reported as opened only after the provider returns a
//!   success status **and** a usable URL (or an API-issued id that lets us build
//!   the canonical URL).
//! - Bitbucket Server/Data Center is a typed non-goal. Only `bitbucket.org`
//!   Cloud origins are accepted.
//! - HTTP requests are bounded: explicit timeout, bounded body, same-origin
//!   redirects only, no automatic POST retry.
//! - Failures use the Q3 `FORGE_*` catalog codes and never contain secrets.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::StreamExt;
use reqwest::Method;
use reqwest::Url;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, RETRY_AFTER, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroize;

use crate::tooling::errors::{ToolErrorCode, ToolFailure, redact_secrets_for_display};

/// Default Bitbucket Cloud API root (includes the API version segment).
pub const DEFAULT_BITBUCKET_API_BASE: &str = "https://api.bitbucket.org/2.0/";
/// Default Bitbucket Cloud web root, used to build a canonical PR URL.
pub const DEFAULT_BITBUCKET_WEB_BASE: &str = "https://bitbucket.org/";
/// Hard default request timeout. The client is allowed to wait, not forever.
pub const DEFAULT_BITBUCKET_TIMEOUT: Duration = Duration::from_secs(30);
/// Default maximum response body. Bitbucket metadata is small; this is generous
/// while still bounding a hostile or broken response.
pub const DEFAULT_BITBUCKET_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Maximum number of same-origin redirects followed by one request.
pub const MAX_BITBUCKET_REDIRECTS: usize = 3;
const MAX_TITLE_CHARS: usize = 256;
const MAX_DESCRIPTION_CHARS: usize = 32_000;
const MAX_BRANCH_CHARS: usize = 255;
const MAX_ORIGIN_CHARS: usize = 2048;
const MAX_ERROR_MESSAGE_CHARS: usize = 600;
const MAX_UPSTREAM_MESSAGE_CHARS: usize = 300;
const MAX_EVIDENCE_CHARS: usize = 400;
const MAX_SLUG_SEGMENTS: usize = 8;

/// The forge providers VerlinPark can create a pull request or merge request
/// through. Gitea and Forgejo share the `tea` CLI and the same wire value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForgeProviderKind {
    GitHub,
    GitLab,
    Gitea,
    BitbucketCloud,
}

impl ForgeProviderKind {
    pub const ALL: [Self; 4] = [
        Self::GitHub,
        Self::GitLab,
        Self::Gitea,
        Self::BitbucketCloud,
    ];

    /// Stable lowercase id used by RPC, CLI, events, and status rows.
    pub fn id(self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
            Self::Gitea => "gitea",
            Self::BitbucketCloud => "bitbucket",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::Gitea => "Gitea/Forgejo",
            Self::BitbucketCloud => "Bitbucket Cloud",
        }
    }

    /// PR vs MR naming used in human output and details.
    pub fn change_name(self) -> &'static str {
        match self {
            Self::GitHub | Self::Gitea | Self::BitbucketCloud => "PR",
            Self::GitLab => "MR",
        }
    }

    pub fn transport(self) -> &'static str {
        match self {
            Self::BitbucketCloud => "rest",
            Self::GitHub | Self::GitLab | Self::Gitea => "cli",
        }
    }

    /// Default CLI executable for a CLI-backed provider.
    pub fn default_cli(self) -> Option<&'static str> {
        match self {
            Self::GitHub => Some("gh"),
            Self::GitLab => Some("glab"),
            Self::Gitea => Some("tea"),
            Self::BitbucketCloud => None,
        }
    }

    pub fn cli_env_var(self) -> Option<&'static str> {
        match self {
            Self::GitHub => Some("VERLINPARK_GH_BIN"),
            Self::GitLab => Some("VERLINPARK_GLAB_BIN"),
            Self::Gitea => Some("VERLINPARK_TEA_BIN"),
            Self::BitbucketCloud => None,
        }
    }

    /// Parse an explicit provider name. Aliases are intentionally narrow and
    /// documented; an unknown value fails closed instead of guessing.
    pub fn parse(raw: &str) -> Result<Self, ForgeError> {
        let normalized = raw.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "github" | "gh" => Ok(Self::GitHub),
            "gitlab" | "glab" | "gitlab-self-managed" => Ok(Self::GitLab),
            "gitea" | "tea" | "forgejo" => Ok(Self::Gitea),
            "bitbucket" | "bitbucket-cloud" | "bitbucketcloud" | "bitbucket_cloud" => {
                Ok(Self::BitbucketCloud)
            }
            "" => Err(ForgeError::new(
                ToolErrorCode::FORGE_UNSUPPORTED_PROVIDER,
                "the PR provider selection is empty",
            )),
            other => Err(ForgeError::new(
                ToolErrorCode::FORGE_UNSUPPORTED_PROVIDER,
                format!(
                    "unsupported forge provider '{other}'; use github, gitlab, gitea/forgejo, or bitbucket"
                ),
            )),
        }
    }

    /// CLI executable resolved from the environment override, if any.
    pub fn resolve_cli(self) -> Option<String> {
        let default = self.default_cli()?;
        let env_var = self.cli_env_var()?;
        Some(std::env::var(env_var).unwrap_or_else(|_| default.to_string()))
    }

    pub fn auth_args(self) -> &'static [&'static str] {
        match self {
            Self::GitHub | Self::GitLab => &["auth", "status"],
            Self::Gitea => &["login", "list"],
            Self::BitbucketCloud => &[],
        }
    }

    pub fn create_args(
        self,
        base_branch: &str,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Vec<String> {
        match self {
            Self::GitHub => vec![
                "pr".into(),
                "create".into(),
                "--base".into(),
                base_branch.into(),
                "--head".into(),
                branch.into(),
                "--title".into(),
                title.into(),
                "--body".into(),
                body.into(),
            ],
            Self::GitLab => vec![
                "mr".into(),
                "create".into(),
                "--source-branch".into(),
                branch.into(),
                "--target-branch".into(),
                base_branch.into(),
                "--title".into(),
                title.into(),
                "--description".into(),
                body.into(),
                "--yes".into(),
            ],
            Self::Gitea => vec![
                "pr".into(),
                "create".into(),
                "--head".into(),
                branch.into(),
                "--base".into(),
                base_branch.into(),
                "--title".into(),
                title.into(),
                "--description".into(),
                body.into(),
            ],
            Self::BitbucketCloud => Vec::new(),
        }
    }

    pub fn create_label(self) -> &'static str {
        match self {
            Self::GitHub => "gh pr create",
            Self::GitLab => "glab mr create",
            Self::Gitea => "tea pr create",
            Self::BitbucketCloud => "Bitbucket Cloud pull request API",
        }
    }
}

impl std::fmt::Display for ForgeProviderKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.id())
    }
}

/// Typed forge failure. Every constructor sanitizes its message; secrets are
/// never accepted as structured data in the first place.
#[derive(Debug, Clone)]
pub struct ForgeError {
    pub code: ToolErrorCode,
    pub message: String,
    pub retryable: bool,
    pub hint: String,
    pub evidence: Option<String>,
    pub provider: Option<ForgeProviderKind>,
}

impl ForgeError {
    pub fn new(code: ToolErrorCode, message: impl Into<String>) -> Self {
        let code = if code.is_known() {
            code
        } else {
            ToolErrorCode::TOOL_FAILED
        };
        Self {
            code,
            message: sanitize_message(&message.into(), MAX_ERROR_MESSAGE_CHARS),
            retryable: code.default_retryable(),
            hint: code.default_hint().to_string(),
            evidence: None,
            provider: None,
        }
    }

    pub fn with_provider(mut self, provider: ForgeProviderKind) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        let evidence = sanitize_message(&evidence.into(), MAX_EVIDENCE_CHARS);
        if !evidence.is_empty() {
            self.evidence = Some(evidence);
        }
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        let hint = sanitize_message(&hint.into(), 300);
        if !hint.is_empty() {
            self.hint = hint;
        }
        self
    }

    /// Prefix the message while keeping the code, hint, retryability, evidence,
    /// and provider. Used when a branch was pushed before PR creation failed.
    pub fn prefix_message(mut self, prefix: &str) -> Self {
        self.message = sanitize_message(
            &format!("{prefix}{}", self.message),
            MAX_ERROR_MESSAGE_CHARS,
        );
        self
    }

    /// Provider-facing Q3 contract. Safe to place in a tool result or event.
    pub fn to_tool_failure(&self) -> ToolFailure {
        let mut failure = ToolFailure::new(self.code, self.message.clone());
        failure.retryable = self.retryable;
        failure.hint = self.hint.clone();
        failure.evidence = self.evidence.clone();
        failure
    }

    /// Structured RPC error data. It contains stable fields only.
    pub fn data(&self) -> serde_json::Value {
        let mut object = json!({
            "code": self.code.as_str(),
            "retryable": self.retryable,
            "hint": self.hint,
        });
        if let Some(provider) = self.provider {
            object["provider"] = json!(provider.id());
        }
        if let Some(evidence) = &self.evidence {
            object["evidence"] = json!(evidence);
        }
        object
    }
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.to_tool_failure().render())
    }
}

impl std::error::Error for ForgeError {}

fn sanitize_message(text: &str, max_chars: usize) -> String {
    let cleaned = text
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>();
    let redacted = redact_secrets_for_display(&cleaned);
    let trimmed = redacted.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut bounded = trimmed
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    bounded.push('…');
    bounded
}

/// One parsed repository origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeOrigin {
    pub provider: ForgeProviderKind,
    pub host: String,
    /// Workspace/owner/group path. GitLab subgroups use `/` inside this value.
    pub owner: String,
    pub repo: String,
}

impl ForgeOrigin {
    pub fn repository_slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    pub fn web_url(&self) -> String {
        format!(
            "https://{}/{}/{}",
            self.host,
            self.owner.trim_matches('/'),
            self.repo
        )
    }
}

/// Detection result used by status surfaces. It never contains credentials or
/// a raw origin URL with userinfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginInspection {
    pub provider: Option<ForgeProviderKind>,
    pub host: Option<String>,
    pub repository: Option<String>,
    pub supported: bool,
    pub server_detected: bool,
    pub detail: String,
}

impl OriginInspection {
    pub fn repository_label(&self) -> String {
        self.repository.clone().unwrap_or_else(|| "-".to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawScheme {
    Https,
    Http,
    Ssh,
}

#[derive(Debug, Clone)]
struct RawOrigin {
    scheme: RawScheme,
    host: String,
    port: Option<u16>,
    path: String,
}

fn parse_raw_origin(raw: &str) -> Result<RawOrigin, ForgeError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin is empty; a git origin remote is required",
        ));
    }
    if trimmed.chars().count() > MAX_ORIGIN_CHARS {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin is too long",
        ));
    }
    if trimmed.chars().any(|character| character.is_control()) {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin contains control characters",
        ));
    }

    if trimmed.contains("://") {
        let url = Url::parse(trimmed).map_err(|_| {
            ForgeError::new(
                ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
                "the repository origin is not a valid URL",
            )
        })?;
        let scheme = match url.scheme() {
            "https" => RawScheme::Https,
            "http" => RawScheme::Http,
            "ssh" => RawScheme::Ssh,
            _ => {
                return Err(ForgeError::new(
                    ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
                    format!(
                        "unsupported origin scheme '{}'; use HTTPS or SSH",
                        url.scheme()
                    ),
                ));
            }
        };
        let host = url.host_str().ok_or_else(|| {
            ForgeError::new(
                ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
                "the repository origin has no host",
            )
        })?;
        return Ok(RawOrigin {
            scheme,
            host: host.to_ascii_lowercase(),
            port: url.port(),
            path: url.path().trim_matches('/').to_string(),
        });
    }

    // SCP-like Git syntax: `[user@]host:path`.
    let Some((authority, path)) = split_scp_origin(trimmed) else {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin is not an HTTPS or SSH Git origin",
        ));
    };
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host_port.is_empty() {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin has no host",
        ));
    }
    if host_port.starts_with('[') || host_port.contains('@') {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "IPv6 and multi-user origins are not supported",
        ));
    }
    let (host, port) = split_host_port(host_port)?;
    if !path.is_empty() && path.starts_with('/') && path.trim_start_matches('/').is_empty() {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin has no repository path",
        ));
    }
    Ok(RawOrigin {
        scheme: RawScheme::Ssh,
        host: host.to_ascii_lowercase(),
        port,
        path: path.trim_matches('/').to_string(),
    })
}

fn split_scp_origin(raw: &str) -> Option<(&str, &str)> {
    // A colon in the first path component is the SCP separator. A bare local
    // path (or Windows drive) has no `user@host:` shape and is rejected.
    let (authority, path) = raw.split_once(':')?;
    if authority.is_empty() || authority.contains('/') || path.is_empty() {
        return None;
    }
    Some((authority, path))
}

fn split_host_port(raw: &str) -> Result<(String, Option<u16>), ForgeError> {
    if let Some((host, port)) = raw.rsplit_once(':') {
        if port.is_empty() || host.is_empty() {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
                "the repository origin has an invalid host or port",
            ));
        }
        if port.chars().all(|character| character.is_ascii_digit()) {
            let port = port.parse::<u16>().map_err(|_| {
                ForgeError::new(
                    ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
                    "the repository origin has an invalid port",
                )
            })?;
            return Ok((host.to_string(), Some(port)));
        }
    }
    Ok((raw.to_string(), None))
}

fn split_repository_path(
    path: &str,
    provider: ForgeProviderKind,
) -> Result<Vec<String>, ForgeError> {
    let cleaned = path.trim_matches('/');
    if cleaned.is_empty() {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin has no repository path",
        )
        .with_provider(provider));
    }
    let mut segments = cleaned.split('/').collect::<Vec<_>>();
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin contains an empty path segment",
        )
        .with_provider(provider));
    }
    for segment in &segments {
        validate_slug(segment, provider)?;
    }
    if let Some(last) = segments.last_mut()
        && let Some(stripped) = last.strip_suffix(".git")
    {
        if stripped.is_empty() {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
                "the repository origin has an empty repository name",
            )
            .with_provider(provider));
        }
        *last = stripped;
    }
    if segments.len() < 2 {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin must include an owner and a repository",
        )
        .with_provider(provider));
    }
    Ok(segments.into_iter().map(str::to_string).collect())
}

fn validate_slug(segment: &str, provider: ForgeProviderKind) -> Result<(), ForgeError> {
    if segment.len() > 100
        || segment == "."
        || segment == ".."
        || !segment.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the repository origin contains an unsupported path segment",
        )
        .with_provider(provider));
    }
    Ok(())
}

fn is_bitbucket_cloud_host(host: &str) -> bool {
    host == "bitbucket.org" || host == "www.bitbucket.org"
}

fn looks_like_bitbucket_server(raw: &RawOrigin, segments: &[String]) -> bool {
    let server_path = segments.first().is_some_and(|segment| {
        segment.eq_ignore_ascii_case("scm") || segment.eq_ignore_ascii_case("projects")
    });
    if is_bitbucket_cloud_host(&raw.host) {
        return raw.port == Some(7999) || server_path;
    }
    raw.port == Some(7999) || raw.host.contains("bitbucket") || server_path
}

fn parse_bitbucket_cloud(raw: &RawOrigin) -> Result<ForgeOrigin, ForgeError> {
    let provider = ForgeProviderKind::BitbucketCloud;
    if raw.scheme == RawScheme::Http {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "Bitbucket Cloud origins must use HTTPS or SSH, not HTTP",
        )
        .with_provider(provider));
    }
    let segments = split_repository_path(&raw.path, provider)?;
    let is_cloud_host = is_bitbucket_cloud_host(&raw.host);
    if !is_cloud_host {
        if looks_like_bitbucket_server(raw, &segments) {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_SERVER_UNSUPPORTED,
                "Bitbucket Server/Data Center is not supported; only Bitbucket Cloud (bitbucket.org) is implemented",
            )
            .with_provider(provider));
        }
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the origin is not a Bitbucket Cloud repository; only bitbucket.org is supported",
        )
        .with_provider(provider));
    }
    if looks_like_bitbucket_server(raw, &segments) {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_SERVER_UNSUPPORTED,
            "Bitbucket Server/Data Center is not supported; only Bitbucket Cloud (bitbucket.org) is implemented",
        )
        .with_provider(provider));
    }
    match raw.scheme {
        RawScheme::Https if raw.port.is_some_and(|port| port != 443) => {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_SERVER_UNSUPPORTED,
                "non-standard Bitbucket Cloud HTTPS ports are not supported",
            )
            .with_provider(provider));
        }
        RawScheme::Ssh if raw.port.is_some_and(|port| port != 22) => {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_SERVER_UNSUPPORTED,
                "non-standard Bitbucket Cloud SSH ports are not supported",
            )
            .with_provider(provider));
        }
        _ => {}
    }
    if segments.len() != 2 {
        if looks_like_bitbucket_server(raw, &segments) {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_SERVER_UNSUPPORTED,
                "Bitbucket Server/Data Center is not supported; only Bitbucket Cloud (bitbucket.org) is implemented",
            )
            .with_provider(provider));
        }
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "a Bitbucket Cloud origin must be bitbucket.org/<workspace>/<repository>",
        )
        .with_provider(provider));
    }
    Ok(ForgeOrigin {
        provider,
        host: "bitbucket.org".to_string(),
        owner: segments[0].clone(),
        repo: segments[1].clone(),
    })
}

fn parse_github_origin(raw: &RawOrigin) -> Result<ForgeOrigin, ForgeError> {
    let provider = ForgeProviderKind::GitHub;
    if raw.scheme == RawScheme::Http {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "GitHub origins must use HTTPS or SSH, not HTTP",
        )
        .with_provider(provider));
    }
    let segments = split_repository_path(&raw.path, provider)?;
    if segments.len() != 2 {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "a GitHub origin must include an owner and a repository",
        )
        .with_provider(provider));
    }
    Ok(ForgeOrigin {
        provider,
        host: raw.host.clone(),
        owner: segments[0].clone(),
        repo: segments[1].clone(),
    })
}

fn parse_gitlab_origin(raw: &RawOrigin) -> Result<ForgeOrigin, ForgeError> {
    let provider = ForgeProviderKind::GitLab;
    if raw.scheme == RawScheme::Http {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "GitLab origins must use HTTPS or SSH, not HTTP",
        )
        .with_provider(provider));
    }
    let segments = split_repository_path(&raw.path, provider)?;
    if segments.len() < 2 || segments.len() > MAX_SLUG_SEGMENTS {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "a GitLab origin must include a group path and a repository",
        )
        .with_provider(provider));
    }
    let repo = segments.last().cloned().unwrap_or_default();
    let owner = segments[..segments.len() - 1].join("/");
    Ok(ForgeOrigin {
        provider,
        host: raw.host.clone(),
        owner,
        repo,
    })
}

/// Parse a known forge origin. Unknown hosts fail closed; callers that only
/// need a human-readable inspection use [`inspect_origin`].
pub fn parse_origin(raw: &str) -> Result<ForgeOrigin, ForgeError> {
    let parsed = parse_raw_origin(raw)?;
    let segments =
        split_repository_path(&parsed.path, ForgeProviderKind::GitHub).unwrap_or_default();
    if parsed.host == "bitbucket.org" || parsed.host == "www.bitbucket.org" {
        return parse_bitbucket_cloud(&parsed);
    }
    if looks_like_bitbucket_server(&parsed, &segments) {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_SERVER_UNSUPPORTED,
            "Bitbucket Server/Data Center is not supported; only Bitbucket Cloud (bitbucket.org) is implemented",
        )
        .with_provider(ForgeProviderKind::BitbucketCloud));
    }
    if parsed.host == "github.com" || parsed.host == "www.github.com" {
        return parse_github_origin(&parsed);
    }
    if parsed.host.contains("github") {
        return parse_github_origin(&parsed);
    }
    if parsed.host == "gitlab.com"
        || parsed.host == "www.gitlab.com"
        || parsed.host.contains("gitlab")
    {
        return parse_gitlab_origin(&parsed);
    }
    Err(ForgeError::new(
        ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
        format!(
            "the origin host '{}' is not a recognized forge; pass an explicit provider",
            parsed.host
        ),
    ))
}

/// Non-failing inspection for status surfaces. It reports why an origin is not
/// usable without echoing userinfo or query data.
pub fn inspect_origin(raw: &str) -> OriginInspection {
    match parse_raw_origin(raw) {
        Ok(parsed) => {
            let host = parsed.host.clone();
            let segments =
                split_repository_path(&parsed.path, ForgeProviderKind::GitHub).unwrap_or_default();
            let server_detected = looks_like_bitbucket_server(&parsed, &segments);
            match parse_origin(raw) {
                Ok(origin) => {
                    let repository = origin.repository_slug();
                    OriginInspection {
                        provider: Some(origin.provider),
                        host: Some(origin.host),
                        repository: Some(repository),
                        supported: true,
                        server_detected: false,
                        detail: format!("{} origin detected", origin.provider.display_name()),
                    }
                }
                Err(error) => OriginInspection {
                    provider: None,
                    host: Some(host),
                    repository: None,
                    supported: false,
                    server_detected: server_detected
                        || error.code == ToolErrorCode::FORGE_SERVER_UNSUPPORTED,
                    detail: error.message,
                },
            }
        }
        Err(error) => OriginInspection {
            provider: None,
            host: None,
            repository: None,
            supported: false,
            server_detected: false,
            detail: error.message,
        },
    }
}

/// Legacy-compatible auto detection: Bitbucket Cloud when the origin names it,
/// GitLab when the host contains `gitlab`, GitHub otherwise. Gitea/Forgejo still
/// requires an explicit provider because self-hosted hosts are indistinguishable
/// from generic Git servers.
pub fn detect_provider(origin: &str) -> ForgeProviderKind {
    let lower = origin.to_ascii_lowercase();
    if lower.contains("bitbucket") {
        ForgeProviderKind::BitbucketCloud
    } else if lower.contains("gitlab") {
        ForgeProviderKind::GitLab
    } else {
        ForgeProviderKind::GitHub
    }
}

/// Provider selection for one PR operation.
#[derive(Debug, Clone)]
pub struct ForgeTarget {
    pub provider: ForgeProviderKind,
    /// Present when the origin was parsed for this provider. Bitbucket always
    /// requires it; CLI providers use it for status/result metadata only.
    pub origin: Option<ForgeOrigin>,
    pub selected_by: ForgeSelectionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeSelectionSource {
    Explicit,
    Environment,
    Detected,
}

impl ForgeTarget {
    pub fn repository(&self) -> Option<String> {
        self.origin.as_ref().map(ForgeOrigin::repository_slug)
    }

    fn require_origin(&self) -> Result<&ForgeOrigin, ForgeError> {
        self.origin.as_ref().ok_or_else(|| {
            ForgeError::new(
                ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
                "this forge provider requires a parseable repository origin",
            )
            .with_provider(self.provider)
        })
    }
}

fn parse_bitbucket_target_origin(origin: &str) -> Result<ForgeOrigin, ForgeError> {
    let parsed = parse_origin(origin)?;
    if parsed.provider != ForgeProviderKind::BitbucketCloud {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            "the origin is not a Bitbucket Cloud repository; only bitbucket.org is supported",
        )
        .with_provider(ForgeProviderKind::BitbucketCloud));
    }
    Ok(parsed)
}

/// Resolve the effective provider from explicit input, environment, and origin.
/// Explicit input wins over `VERLINPARK_PR_PROVIDER`, which wins over detection.
pub fn resolve_target(
    explicit: Option<&str>,
    environment: Option<&str>,
    origin: &str,
) -> Result<ForgeTarget, ForgeError> {
    let (selection, source) = match explicit.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => (Some(value), ForgeSelectionSource::Explicit),
        None => match environment.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => (Some(value), ForgeSelectionSource::Environment),
            None => (None, ForgeSelectionSource::Detected),
        },
    };

    match selection {
        Some(raw) => {
            let provider = ForgeProviderKind::parse(raw)?;
            let origin = match provider {
                ForgeProviderKind::BitbucketCloud => Some(parse_bitbucket_target_origin(origin)?),
                _ => parse_origin(origin)
                    .ok()
                    .filter(|parsed| parsed.provider == provider),
            };
            Ok(ForgeTarget {
                provider,
                origin,
                selected_by: source,
            })
        }
        None => {
            let provider = detect_provider(origin);
            let origin = match provider {
                ForgeProviderKind::BitbucketCloud => Some(parse_bitbucket_target_origin(origin)?),
                _ => parse_origin(origin)
                    .ok()
                    .filter(|parsed| parsed.provider == provider),
            };
            Ok(ForgeTarget {
                provider,
                origin,
                selected_by: ForgeSelectionSource::Detected,
            })
        }
    }
}

/// One PR/MR creation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeChangeRequest {
    pub title: String,
    pub description: String,
    pub source_branch: String,
    pub destination_branch: String,
}

impl ForgeChangeRequest {
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        source_branch: impl Into<String>,
        destination_branch: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            source_branch: source_branch.into(),
            destination_branch: destination_branch.into(),
        }
    }

    pub fn validate(&self) -> Result<(), ForgeError> {
        validate_title(&self.title)?;
        validate_description(&self.description)?;
        validate_branch(&self.source_branch, "source branch")?;
        validate_branch(&self.destination_branch, "destination branch")?;
        if self.source_branch == self.destination_branch {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_REQUEST_INVALID,
                "the source branch and destination branch must differ",
            ));
        }
        Ok(())
    }
}

fn validate_title(title: &str) -> Result<(), ForgeError> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            "the pull request title is empty",
        ));
    }
    if trimmed.chars().count() > MAX_TITLE_CHARS {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            format!("the pull request title exceeds {MAX_TITLE_CHARS} characters"),
        ));
    }
    if trimmed.contains('\n') || trimmed.contains('\r') || trimmed.contains('\0') {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            "the pull request title must be a single line without control characters",
        ));
    }
    Ok(())
}

fn validate_description(description: &str) -> Result<(), ForgeError> {
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            format!("the pull request description exceeds {MAX_DESCRIPTION_CHARS} characters"),
        ));
    }
    if description.contains('\0') {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            "the pull request description contains a NUL character",
        ));
    }
    Ok(())
}

fn validate_branch(branch: &str, label: &str) -> Result<(), ForgeError> {
    if branch.trim().is_empty() {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            format!("the {label} is empty"),
        ));
    }
    if branch.chars().count() > MAX_BRANCH_CHARS {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            format!("the {label} exceeds {MAX_BRANCH_CHARS} characters"),
        ));
    }
    if branch
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            format!("the {label} contains whitespace or control characters"),
        ));
    }
    if branch.starts_with('-') || branch.starts_with('/') || branch.ends_with('/') {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_REQUEST_INVALID,
            format!("the {label} has an unsupported leading/trailing character"),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePreflight {
    pub provider: ForgeProviderKind,
    pub repository: Option<String>,
    pub auth: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeChange {
    pub provider: ForgeProviderKind,
    pub change_name: &'static str,
    pub repository: Option<String>,
    pub id: Option<u64>,
    pub url: String,
    pub state: Option<String>,
    pub title: String,
}

/// Unified PR/MR provider interface. Implementations are intentionally
/// operation-scoped: preflight must not create anything, and create must not
/// claim success without an API-issued id/URL.
#[async_trait]
pub trait ForgeProvider: Send + Sync {
    fn kind(&self) -> ForgeProviderKind;

    fn change_name(&self) -> &'static str {
        self.kind().change_name()
    }

    async fn preflight(&self, target: &ForgeTarget) -> Result<ForgePreflight, ForgeError>;

    async fn create(
        &self,
        target: &ForgeTarget,
        request: &ForgeChangeRequest,
    ) -> Result<ForgeChange, ForgeError>;
}

/// A secret value that zeroes its backing allocation on drop and never renders.
struct SecretValue(String);

impl SecretValue {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretValue([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitbucketAuthMode {
    Basic,
    Bearer,
}

impl BitbucketAuthMode {
    pub fn id(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Bearer => "bearer",
        }
    }
}

/// Bitbucket Cloud credentials. Environment-only by construction; there is no
/// SQLite/event/manifest serialization for this type.
pub struct BitbucketCredentials {
    username: Option<SecretValue>,
    app_password: Option<SecretValue>,
    token: Option<SecretValue>,
    mode: BitbucketAuthMode,
}

impl std::fmt::Debug for BitbucketCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BitbucketCredentials")
            .field("mode", &self.mode)
            .field("configured", &true)
            .finish()
    }
}

impl BitbucketCredentials {
    pub fn from_env() -> Result<Self, ForgeError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Testable constructor. The lookup returns raw environment values; only
    /// presence and sanitized metadata are ever exposed.
    pub fn from_lookup<F>(lookup: F) -> Result<Self, ForgeError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let username = clean_secret(lookup("BITBUCKET_USERNAME"));
        let app_password = clean_secret(lookup("BITBUCKET_APP_PASSWORD"));
        let token = clean_secret(lookup("BITBUCKET_TOKEN"));

        if token.is_some() && (username.is_some() || app_password.is_some()) {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_AUTH_CONFLICT,
                "BITBUCKET_TOKEN and BITBUCKET_USERNAME/BITBUCKET_APP_PASSWORD are both set; configure exactly one auth mode",
            )
            .with_provider(ForgeProviderKind::BitbucketCloud));
        }
        if token.is_none() && username.is_none() && app_password.is_none() {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_AUTH_MISSING,
                "Bitbucket credentials are not configured; set BITBUCKET_TOKEN or BITBUCKET_USERNAME + BITBUCKET_APP_PASSWORD in the daemon environment",
            )
            .with_provider(ForgeProviderKind::BitbucketCloud));
        }
        if let Some(token) = token {
            validate_secret("BITBUCKET_TOKEN", token.expose())?;
            return Ok(Self {
                username: None,
                app_password: None,
                token: Some(token),
                mode: BitbucketAuthMode::Bearer,
            });
        }
        let username = username.ok_or_else(|| {
            ForgeError::new(
                ToolErrorCode::FORGE_AUTH_MISSING,
                "BITBUCKET_APP_PASSWORD is set but BITBUCKET_USERNAME is missing",
            )
            .with_provider(ForgeProviderKind::BitbucketCloud)
        })?;
        let app_password = app_password.ok_or_else(|| {
            ForgeError::new(
                ToolErrorCode::FORGE_AUTH_MISSING,
                "BITBUCKET_USERNAME is set but BITBUCKET_APP_PASSWORD is missing",
            )
            .with_provider(ForgeProviderKind::BitbucketCloud)
        })?;
        validate_secret("BITBUCKET_USERNAME", username.expose())?;
        if username.expose().contains(':') {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_AUTH_MISSING,
                "BITBUCKET_USERNAME must not contain a colon in Basic authentication",
            )
            .with_provider(ForgeProviderKind::BitbucketCloud));
        }
        validate_secret("BITBUCKET_APP_PASSWORD", app_password.expose())?;
        Ok(Self {
            username: Some(username),
            app_password: Some(app_password),
            token: None,
            mode: BitbucketAuthMode::Basic,
        })
    }

    pub fn mode(&self) -> BitbucketAuthMode {
        self.mode
    }

    pub fn source_names(&self) -> &'static str {
        match self.mode {
            BitbucketAuthMode::Basic => "BITBUCKET_USERNAME + BITBUCKET_APP_PASSWORD",
            BitbucketAuthMode::Bearer => "BITBUCKET_TOKEN",
        }
    }

    fn authorization_header(&self) -> Result<String, ForgeError> {
        match self.mode {
            BitbucketAuthMode::Bearer => {
                let token = self.token.as_ref().ok_or_else(|| {
                    ForgeError::new(
                        ToolErrorCode::FORGE_AUTH_MISSING,
                        "Bitbucket bearer token is missing",
                    )
                })?;
                Ok(format!("Bearer {}", token.expose()))
            }
            BitbucketAuthMode::Basic => {
                let username = self.username.as_ref().ok_or_else(|| {
                    ForgeError::new(
                        ToolErrorCode::FORGE_AUTH_MISSING,
                        "Bitbucket username is missing",
                    )
                })?;
                let password = self.app_password.as_ref().ok_or_else(|| {
                    ForgeError::new(
                        ToolErrorCode::FORGE_AUTH_MISSING,
                        "Bitbucket app password is missing",
                    )
                })?;
                let encoded = BASE64.encode(format!("{}:{}", username.expose(), password.expose()));
                Ok(format!("Basic {encoded}"))
            }
        }
    }
}

fn clean_secret(value: Option<String>) -> Option<SecretValue> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(SecretValue)
}

fn validate_secret(name: &str, value: &str) -> Result<(), ForgeError> {
    if value.chars().count() > 4096 {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_AUTH_MISSING,
            format!("{name} is longer than the supported 4096 characters"),
        )
        .with_provider(ForgeProviderKind::BitbucketCloud));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_graphic() && character != ' ')
    {
        return Err(ForgeError::new(
            ToolErrorCode::FORGE_AUTH_MISSING,
            format!("{name} contains whitespace or non-ASCII characters"),
        )
        .with_provider(ForgeProviderKind::BitbucketCloud));
    }
    Ok(())
}

/// Bounded client configuration. Production uses the constants; tests may point
/// the same client at a local fake server.
#[derive(Debug, Clone)]
pub struct BitbucketCloudConfig {
    api_base: Url,
    web_base: Url,
    timeout: Duration,
    max_response_bytes: usize,
    require_https: bool,
}

impl Default for BitbucketCloudConfig {
    fn default() -> Self {
        Self {
            api_base: Url::parse(DEFAULT_BITBUCKET_API_BASE)
                .expect("constant Bitbucket API base is valid"),
            web_base: Url::parse(DEFAULT_BITBUCKET_WEB_BASE)
                .expect("constant Bitbucket web base is valid"),
            timeout: DEFAULT_BITBUCKET_TIMEOUT,
            max_response_bytes: DEFAULT_BITBUCKET_MAX_RESPONSE_BYTES,
            require_https: true,
        }
    }
}

impl BitbucketCloudConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn for_test(api_base: &str, web_base: &str) -> Result<Self, ForgeError> {
        let config = Self {
            api_base: Url::parse(api_base).map_err(config_error)?,
            web_base: Url::parse(web_base).map_err(config_error)?,
            require_https: false,
            ..Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub fn api_base(&self) -> &Url {
        &self.api_base
    }

    pub fn web_base(&self) -> &Url {
        &self.web_base
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.clamp(Duration::from_millis(1), Duration::from_secs(120));
        self
    }

    pub fn with_max_response_bytes(mut self, bytes: usize) -> Self {
        self.max_response_bytes = bytes.clamp(1, 8 * 1024 * 1024);
        self
    }

    fn validate(&self) -> Result<(), ForgeError> {
        if self.api_base.host_str().is_none() || self.web_base.host_str().is_none() {
            return Err(config_error("the forge base URL is missing a host"));
        }
        if self.require_https
            && (self.api_base.scheme() != "https" || self.web_base.scheme() != "https")
        {
            return Err(config_error(
                "Bitbucket Cloud base URLs must use HTTPS in production",
            ));
        }
        if !matches!(self.api_base.scheme(), "http" | "https")
            || !matches!(self.web_base.scheme(), "http" | "https")
        {
            return Err(config_error(
                "forge base URLs must use HTTP(S), never SSH or file schemes",
            ));
        }
        if self.timeout.is_zero() || self.max_response_bytes == 0 {
            return Err(config_error(
                "forge timeout and response bound must be non-zero",
            ));
        }
        Ok(())
    }
}

fn config_error(error: impl std::fmt::Display) -> ForgeError {
    ForgeError::new(
        ToolErrorCode::FORGE_CONFIG_INVALID,
        format!("invalid Bitbucket client configuration: {error}"),
    )
    .with_provider(ForgeProviderKind::BitbucketCloud)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

/// Bounded Bitbucket Cloud REST client. It never retries a create request and
/// never follows a redirect to another origin.
pub struct BitbucketCloudClient {
    config: BitbucketCloudConfig,
    http: reqwest::Client,
    credentials: BitbucketCredentials,
}

impl BitbucketCloudClient {
    pub fn new(
        config: BitbucketCloudConfig,
        credentials: BitbucketCredentials,
    ) -> Result<Self, ForgeError> {
        config.validate()?;
        let api_base = config.api_base.clone();
        let redirect = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_BITBUCKET_REDIRECTS {
                return attempt.stop();
            }
            if same_origin(attempt.url(), &api_base) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(redirect)
            .cookie_store(false)
            .https_only(config.require_https)
            .user_agent(format!("verlinpark/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| config_error(error.to_string()))?;
        Ok(Self {
            config,
            http,
            credentials,
        })
    }

    pub fn from_env(config: BitbucketCloudConfig) -> Result<Self, ForgeError> {
        let credentials = BitbucketCredentials::from_env()?;
        Self::new(config, credentials)
    }

    pub fn for_test(
        api_base: &str,
        web_base: &str,
        credentials: BitbucketCredentials,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, ForgeError> {
        let config = BitbucketCloudConfig::for_test(api_base, web_base)?
            .with_timeout(timeout)
            .with_max_response_bytes(max_response_bytes);
        Self::new(config, credentials)
    }

    pub fn config(&self) -> &BitbucketCloudConfig {
        &self.config
    }

    pub fn auth_mode(&self) -> BitbucketAuthMode {
        self.credentials.mode()
    }

    pub fn credential_source(&self) -> &'static str {
        self.credentials.source_names()
    }

    fn repository_url(&self, origin: &ForgeOrigin, suffix: &[&str]) -> Result<Url, ForgeError> {
        if origin.provider != ForgeProviderKind::BitbucketCloud {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
                "the Bitbucket client only accepts Bitbucket Cloud origins",
            )
            .with_provider(ForgeProviderKind::BitbucketCloud));
        }
        let mut path = format!(
            "{}/repositories/{}/{}",
            self.config.api_base.as_str().trim_end_matches('/'),
            origin.owner,
            origin.repo
        );
        for segment in suffix {
            path.push('/');
            path.push_str(segment);
        }
        Url::parse(&path).map_err(|error| {
            ForgeError::new(
                ToolErrorCode::FORGE_CONFIG_INVALID,
                format!("could not build the Bitbucket API URL: {error}"),
            )
            .with_provider(ForgeProviderKind::BitbucketCloud)
        })
    }

    fn authorization_header(&self) -> Result<String, ForgeError> {
        self.credentials
            .authorization_header()
            .map_err(|error| error.with_provider(ForgeProviderKind::BitbucketCloud))
    }

    fn request(&self, method: Method, url: Url) -> Result<reqwest::RequestBuilder, ForgeError> {
        let authorization = self.authorization_header()?;
        Ok(self
            .http
            .request(method, url)
            .header(ACCEPT, "application/json")
            .header(
                USER_AGENT,
                format!("verlinpark/{}", env!("CARGO_PKG_VERSION")),
            )
            .header(AUTHORIZATION, authorization)
            .timeout(self.config.timeout))
    }

    pub async fn preflight(&self, origin: &ForgeOrigin) -> Result<ForgePreflight, ForgeError> {
        let url = self.repository_url(origin, &[])?;
        let request = self.request(Method::GET, url)?;
        let response = request
            .send()
            .await
            .map_err(|error| transport_error(error, false, ForgeProviderKind::BitbucketCloud))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = self.read_bounded(response).await?;
        if !status.is_success() {
            return Err(map_http_error(status, &headers, &body, false, "preflight"));
        }
        let parsed: Option<BitbucketRepository> = serde_json::from_slice(&body).ok();
        let full_name = parsed
            .as_ref()
            .and_then(|repository| repository.full_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| origin.repository_slug());
        Ok(ForgePreflight {
            provider: ForgeProviderKind::BitbucketCloud,
            repository: Some(full_name.clone()),
            auth: self.credentials.mode().id().to_string(),
            detail: format!(
                "Bitbucket Cloud credentials accepted for {full_name} ({} auth)",
                self.credentials.mode().id()
            ),
        })
    }

    pub async fn create_pull_request(
        &self,
        origin: &ForgeOrigin,
        request: &ForgeChangeRequest,
    ) -> Result<ForgeChange, ForgeError> {
        request.validate()?;
        let url = self.repository_url(origin, &["pullrequests"])?;
        let payload = CreatePullRequestPayload {
            title: request.title.clone(),
            description: request.description.clone(),
            source: BranchTarget {
                branch: BranchName {
                    name: request.source_branch.clone(),
                },
            },
            destination: BranchTarget {
                branch: BranchName {
                    name: request.destination_branch.clone(),
                },
            },
            close_source_branch: false,
        };
        let response = self
            .request(Method::POST, url)?
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|error| transport_error(error, true, ForgeProviderKind::BitbucketCloud))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = self.read_bounded(response).await?;
        if !status.is_success() {
            return Err(map_http_error(
                status,
                &headers,
                &body,
                true,
                "pull request creation",
            ));
        }
        if status != reqwest::StatusCode::CREATED && status != reqwest::StatusCode::OK {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_RESPONSE_INVALID,
                format!(
                    "Bitbucket returned HTTP {} for pull request creation; expected 200 or 201",
                    status.as_u16()
                ),
            )
            .with_provider(ForgeProviderKind::BitbucketCloud)
            .with_evidence(format!("status={}", status.as_u16())));
        }
        let parsed: BitbucketPullRequest = serde_json::from_slice(&body).map_err(|_| {
            ForgeError::new(
                ToolErrorCode::FORGE_RESPONSE_INVALID,
                "Bitbucket returned a success status but the pull request response was not valid JSON",
            )
            .with_provider(ForgeProviderKind::BitbucketCloud)
            .with_evidence(format!("status={}", status.as_u16()))
        })?;
        let id = parsed.id;
        let html_url = parsed
            .links
            .as_ref()
            .and_then(|links| links.html.as_ref())
            .and_then(|html| html.href.as_deref())
            .and_then(|href| self.validated_web_url(href));
        let url = match (html_url, id) {
            (Some(url), _) => url,
            (None, Some(id)) => self.canonical_change_url(origin, id),
            (None, None) => {
                return Err(ForgeError::new(
                    ToolErrorCode::FORGE_RESPONSE_INVALID,
                    "Bitbucket created the pull request but returned neither a usable URL nor a pull request id; verify the provider before retrying",
                )
                .with_provider(ForgeProviderKind::BitbucketCloud)
                .with_evidence(format!("status={}", status.as_u16())));
            }
        };
        Ok(ForgeChange {
            provider: ForgeProviderKind::BitbucketCloud,
            change_name: ForgeProviderKind::BitbucketCloud.change_name(),
            repository: Some(origin.repository_slug()),
            id,
            url,
            state: parsed.state,
            title: parsed
                .title
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| request.title.clone()),
        })
    }

    fn validated_web_url(&self, raw: &str) -> Option<String> {
        let url = Url::parse(raw).ok()?;
        if self.config.require_https && url.scheme() != "https" {
            return None;
        }
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        if !same_origin(&url, &self.config.web_base) {
            return None;
        }
        Some(url.to_string())
    }

    fn canonical_change_url(&self, origin: &ForgeOrigin, id: u64) -> String {
        format!(
            "{}/{}/{}/pull-requests/{id}",
            self.config.web_base.as_str().trim_end_matches('/'),
            origin.owner,
            origin.repo
        )
    }

    async fn read_bounded(&self, response: reqwest::Response) -> Result<Vec<u8>, ForgeError> {
        if let Some(length) = response.content_length()
            && length as usize > self.config.max_response_bytes
        {
            return Err(ForgeError::new(
                ToolErrorCode::FORGE_RESPONSE_TOO_LARGE,
                format!(
                    "the forge response length ({length} bytes) exceeds the {} byte client limit",
                    self.config.max_response_bytes
                ),
            )
            .with_provider(ForgeProviderKind::BitbucketCloud));
        }
        let mut stream = response.bytes_stream();
        let mut output = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                transport_error(error, false, ForgeProviderKind::BitbucketCloud)
            })?;
            if output.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                return Err(ForgeError::new(
                    ToolErrorCode::FORGE_RESPONSE_TOO_LARGE,
                    format!(
                        "the forge response exceeded the {} byte client limit",
                        self.config.max_response_bytes
                    ),
                )
                .with_provider(ForgeProviderKind::BitbucketCloud));
            }
            output.extend_from_slice(&chunk);
        }
        Ok(output)
    }
}

fn transport_error(
    error: reqwest::Error,
    mutating: bool,
    provider: ForgeProviderKind,
) -> ForgeError {
    let timed_out = error.is_timeout();
    let connect = error.is_connect();
    let code = if timed_out {
        ToolErrorCode::FORGE_TIMEOUT
    } else {
        ToolErrorCode::FORGE_NETWORK_FAILED
    };
    let message = if timed_out {
        if mutating {
            "the Bitbucket request timed out; it may have reached the server, so verify whether the pull request exists before retrying".to_string()
        } else {
            "the Bitbucket preflight request timed out before a response".to_string()
        }
    } else {
        format!("the Bitbucket request failed before a usable response: {error}")
    };
    let retryable = if timed_out { !mutating } else { connect };
    ForgeError::new(code, message)
        .with_provider(provider)
        .with_retryable(retryable)
}

fn map_http_error(
    status: reqwest::StatusCode,
    headers: &HeaderMap,
    body: &[u8],
    mutating: bool,
    operation: &str,
) -> ForgeError {
    let provider = ForgeProviderKind::BitbucketCloud;
    let upstream = upstream_error_message(body);
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(|value| sanitize_message(value, 120))
        .filter(|value| !value.is_empty());
    let retry_after = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| seconds.min(86_400));

    let code = match status.as_u16() {
        401 => ToolErrorCode::FORGE_AUTH_FAILED,
        403 => ToolErrorCode::FORGE_PERMISSION_DENIED,
        404 => ToolErrorCode::FORGE_REPO_NOT_FOUND,
        409 => ToolErrorCode::FORGE_CONFLICT,
        429 => ToolErrorCode::FORGE_RATE_LIMITED,
        300..=399 => ToolErrorCode::FORGE_REDIRECT_REFUSED,
        500..=599 => ToolErrorCode::FORGE_SERVICE_UNAVAILABLE,
        _ => ToolErrorCode::FORGE_REQUEST_REJECTED,
    };
    let base = match status.as_u16() {
        401 => "Bitbucket rejected the credentials",
        403 => "the Bitbucket credentials cannot write to this repository",
        404 => {
            "the Bitbucket workspace or repository was not found, or the credentials cannot see it"
        }
        409 => "Bitbucket reported a conflict; a pull request may already exist for this branch",
        429 => "Bitbucket rate-limited the request",
        300..=399 => {
            "Bitbucket attempted a redirect to another origin; credentials are never forwarded there"
        }
        500..=599 => "Bitbucket is temporarily unavailable",
        _ => "Bitbucket rejected the request",
    };
    let mut message = format!("{base} during {operation} (HTTP {})", status.as_u16());
    if let Some(upstream) = &upstream {
        message.push_str(": ");
        message.push_str(upstream);
    }
    if matches!(status.as_u16(), 500..=599) && mutating {
        message.push_str("; the create request may have succeeded before the failure, so verify the provider first");
    }
    let retryable = match status.as_u16() {
        429 => true,
        500..=599 => !mutating,
        _ => false,
    };
    let mut evidence = format!("status={}", status.as_u16());
    if let Some(request_id) = request_id {
        evidence.push_str(&format!("; request_id={request_id}"));
    }
    if let Some(retry_after) = retry_after {
        evidence.push_str(&format!("; retry_after_secs={retry_after}"));
    }
    let error = ForgeError::new(code, message)
        .with_provider(provider)
        .with_retryable(retryable)
        .with_evidence(evidence);
    match (status.as_u16(), retry_after) {
        (429, Some(seconds)) => error.with_hint(format!(
            "Bitbucket asked to retry after {seconds} second(s); wait for that window before repeating the operation. No automatic retry was performed."
        )),
        _ => error,
    }
}

#[derive(Deserialize)]
struct BitbucketErrorEnvelope {
    #[serde(default)]
    error: Option<BitbucketErrorBody>,
}

#[derive(Deserialize)]
struct BitbucketErrorBody {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    detail: Option<String>,
}

fn upstream_error_message(body: &[u8]) -> Option<String> {
    if body.is_empty() || body.len() > 64 * 1024 {
        return None;
    }
    let parsed: BitbucketErrorEnvelope = serde_json::from_slice(body).ok()?;
    let error = parsed.error?;
    let mut parts = Vec::new();
    if let Some(message) = error.message.filter(|value| !value.trim().is_empty()) {
        parts.push(message);
    }
    if let Some(detail) = error.detail.filter(|value| !value.trim().is_empty()) {
        parts.push(detail);
    }
    if parts.is_empty() {
        return None;
    }
    let combined = parts.join(" — ");
    Some(sanitize_message(&combined, MAX_UPSTREAM_MESSAGE_CHARS))
}

#[derive(Deserialize)]
struct BitbucketRepository {
    #[serde(default)]
    full_name: Option<String>,
}

#[derive(Serialize)]
struct CreatePullRequestPayload {
    title: String,
    description: String,
    source: BranchTarget,
    destination: BranchTarget,
    close_source_branch: bool,
}

#[derive(Serialize)]
struct BranchTarget {
    branch: BranchName,
}

#[derive(Serialize)]
struct BranchName {
    name: String,
}

#[derive(Deserialize, Default)]
struct BitbucketPullRequest {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    links: Option<BitbucketLinks>,
}

#[derive(Deserialize, Default)]
struct BitbucketLinks {
    #[serde(default)]
    html: Option<BitbucketHref>,
}

#[derive(Deserialize, Default)]
struct BitbucketHref {
    #[serde(default)]
    href: Option<String>,
}

/// REST-backed Bitbucket Cloud provider.
pub struct BitbucketCloudProvider {
    client: BitbucketCloudClient,
}

impl BitbucketCloudProvider {
    pub fn new(client: BitbucketCloudClient) -> Self {
        Self { client }
    }

    pub fn from_env(config: BitbucketCloudConfig) -> Result<Self, ForgeError> {
        Ok(Self {
            client: BitbucketCloudClient::from_env(config)?,
        })
    }

    pub fn client(&self) -> &BitbucketCloudClient {
        &self.client
    }
}

#[async_trait]
impl ForgeProvider for BitbucketCloudProvider {
    fn kind(&self) -> ForgeProviderKind {
        ForgeProviderKind::BitbucketCloud
    }

    async fn preflight(&self, target: &ForgeTarget) -> Result<ForgePreflight, ForgeError> {
        let origin = target.require_origin()?;
        self.client.preflight(origin).await
    }

    async fn create(
        &self,
        target: &ForgeTarget,
        request: &ForgeChangeRequest,
    ) -> Result<ForgeChange, ForgeError> {
        let origin = target.require_origin()?;
        self.client.create_pull_request(origin, request).await
    }
}

/// CLI-backed provider. Preflight and creation run the existing gh/glab/tea
/// binaries; no credentials are read, stored, or logged by VerlinPark.
#[derive(Debug, Clone)]
pub struct CliForgeProvider {
    kind: ForgeProviderKind,
    binary: String,
    preflight_cwd: PathBuf,
    create_cwd: PathBuf,
}

impl CliForgeProvider {
    pub fn new(
        kind: ForgeProviderKind,
        binary: impl Into<String>,
        preflight_cwd: PathBuf,
        create_cwd: PathBuf,
    ) -> Self {
        Self {
            kind,
            binary: binary.into(),
            preflight_cwd,
            create_cwd,
        }
    }

    pub fn binary(&self) -> &str {
        &self.binary
    }

    fn auth_failure_detail(output: &std::process::Output) -> String {
        let stderr = String::from_utf8_lossy(&output.stderr);
        sanitize_message(stderr.trim(), 400)
    }

    fn run_preflight(&self) -> Result<ForgePreflight, ForgeError> {
        let output = Command::new(&self.binary)
            .args(self.kind.auth_args())
            .current_dir(&self.preflight_cwd)
            .output()
            .map_err(|error| {
                ForgeError::new(
                    ToolErrorCode::FORGE_CLI_UNAVAILABLE,
                    format!(
                        "running {} auth status: {error}",
                        self.kind.default_cli().unwrap_or("forge CLI")
                    ),
                )
                .with_provider(self.kind)
            })?;
        if !output.status.success() {
            let detail = Self::auth_failure_detail(&output);
            let message = if detail.is_empty() {
                format!(
                    "{} is not authenticated",
                    self.kind.default_cli().unwrap_or("forge CLI")
                )
            } else {
                format!(
                    "{} is not authenticated: {detail}",
                    self.kind.default_cli().unwrap_or("forge CLI")
                )
            };
            return Err(ForgeError::new(ToolErrorCode::FORGE_AUTH_FAILED, message)
                .with_provider(self.kind)
                .with_hint(format!(
                    "Authenticate {} in a terminal, then retry the pull request.",
                    self.kind.default_cli().unwrap_or("the forge CLI")
                )));
        }
        Ok(ForgePreflight {
            provider: self.kind,
            repository: None,
            auth: "cli".to_string(),
            detail: format!(
                "{} CLI authentication check passed",
                self.kind.default_cli().unwrap_or("forge")
            ),
        })
    }

    fn run_create(&self, request: &ForgeChangeRequest) -> Result<ForgeChange, ForgeError> {
        request.validate()?;
        let output = Command::new(&self.binary)
            .args(self.kind.create_args(
                &request.destination_branch,
                &request.source_branch,
                &request.title,
                &request.description,
            ))
            .current_dir(&self.create_cwd)
            .output()
            .map_err(|error| {
                ForgeError::new(
                    ToolErrorCode::FORGE_CLI_UNAVAILABLE,
                    format!("running {}: {error}", self.kind.create_label()),
                )
                .with_provider(self.kind)
            })?;
        if !output.status.success() {
            let detail = Self::auth_failure_detail(&output);
            let message = if detail.is_empty() {
                format!(
                    "branch pushed, but {} failed without an error message",
                    self.kind.create_label()
                )
            } else {
                format!(
                    "branch pushed, but {} failed: {detail}",
                    self.kind.create_label()
                )
            };
            return Err(
                ForgeError::new(ToolErrorCode::FORGE_REQUEST_REJECTED, message)
                    .with_provider(self.kind),
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let url = stdout
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("http"))
            .ok_or_else(|| {
                ForgeError::new(
                    ToolErrorCode::FORGE_RESPONSE_INVALID,
                    format!(
                        "{} succeeded but did not return a URL; verify the provider before retrying",
                        self.kind.create_label()
                    ),
                )
                .with_provider(self.kind)
            })?
            .to_string();
        Ok(ForgeChange {
            provider: self.kind,
            change_name: self.kind.change_name(),
            repository: None,
            id: None,
            url,
            state: None,
            title: request.title.clone(),
        })
    }
}

#[async_trait]
impl ForgeProvider for CliForgeProvider {
    fn kind(&self) -> ForgeProviderKind {
        self.kind
    }

    async fn preflight(&self, _target: &ForgeTarget) -> Result<ForgePreflight, ForgeError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.run_preflight())
            .await
            .map_err(|error| {
                ForgeError::new(
                    ToolErrorCode::FORGE_CLI_UNAVAILABLE,
                    format!("the forge CLI task failed: {error}"),
                )
                .with_provider(self.kind)
            })?
    }

    async fn create(
        &self,
        _target: &ForgeTarget,
        request: &ForgeChangeRequest,
    ) -> Result<ForgeChange, ForgeError> {
        let this = self.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || this.run_create(&request))
            .await
            .map_err(|error| {
                ForgeError::new(
                    ToolErrorCode::FORGE_CLI_UNAVAILABLE,
                    format!("the forge CLI task failed: {error}"),
                )
                .with_provider(self.kind)
            })?
    }
}

/// Provider row used by the daemon status surface. No executable run, no
/// authentication probe, and no credential value is included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeProviderSnapshot {
    pub provider: ForgeProviderKind,
    pub command: Option<String>,
    pub command_available: bool,
    pub auth_configured: Option<bool>,
    pub credential_source: Option<String>,
    pub supported: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitbucketCredentialSnapshot {
    pub configured: bool,
    pub mode: Option<BitbucketAuthMode>,
    pub source: Option<&'static str>,
    pub detail: String,
}

/// Inspect Bitbucket credentials without a network call. The returned detail is
/// safe: it names environment variables, never values.
pub fn bitbucket_credential_snapshot() -> BitbucketCredentialSnapshot {
    match BitbucketCredentials::from_env() {
        Ok(credentials) => BitbucketCredentialSnapshot {
            configured: true,
            mode: Some(credentials.mode()),
            source: Some(credentials.source_names()),
            detail: format!(
                "credentials configured from {} ({} auth)",
                credentials.source_names(),
                credentials.mode().id()
            ),
        },
        Err(error) => BitbucketCredentialSnapshot {
            configured: false,
            mode: None,
            source: None,
            detail: error.message,
        },
    }
}

/// Static provider inventory for status. CLI auth remains unknown until a real
/// preflight; status never spawns `gh`, `glab`, or `tea`.
pub fn provider_snapshots() -> Vec<ForgeProviderSnapshot> {
    let bitbucket = bitbucket_credential_snapshot();
    ForgeProviderKind::ALL
        .into_iter()
        .map(|provider| match provider {
            ForgeProviderKind::BitbucketCloud => ForgeProviderSnapshot {
                provider,
                command: None,
                command_available: true,
                auth_configured: Some(bitbucket.configured),
                credential_source: bitbucket.source.map(str::to_string),
                supported: true,
                detail: format!(
                    "Bitbucket Cloud REST only; {}. Server/Data Center is not supported.",
                    bitbucket.detail
                ),
            },
            _ => {
                let command = provider.resolve_cli();
                let available = command.as_deref().is_some_and(command_available);
                ForgeProviderSnapshot {
                    provider,
                    command: command.clone(),
                    command_available: available,
                    auth_configured: None,
                    credential_source: None,
                    supported: available,
                    detail: if available {
                        format!(
                            "{} CLI found; authentication is checked by preflight before any push.",
                            provider.default_cli().unwrap_or("forge")
                        )
                    } else {
                        format!(
                            "{} CLI not found; install {} or set {}.",
                            provider.default_cli().unwrap_or("forge"),
                            provider.default_cli().unwrap_or("the CLI"),
                            provider.cli_env_var().unwrap_or("the CLI path")
                        )
                    },
                }
            }
        })
        .collect()
}

/// Resolve whether a command is executable without spawning it. An explicit
/// path is checked directly; a bare name is searched on `PATH`.
pub fn command_available(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    let path = Path::new(command);
    if command.contains('/') || command.contains('\\') {
        return is_executable_file(path);
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|directory| {
        if directory.as_os_str().is_empty() {
            return false;
        }
        is_executable_file(&directory.join(command))
    })
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Sanitize a value for a human-readable status line. Kept public for the
/// daemon and tests that want the same redaction rules.
pub fn sanitize_status_text(value: &str, max_chars: usize) -> String {
    sanitize_message(value, max_chars)
}

/// Environment variable names read by the Bitbucket provider. Public so tests
/// and docs can assert the exact surface without duplicating strings.
pub const BITBUCKET_ENV_VARS: [&str; 3] = [
    "BITBUCKET_TOKEN",
    "BITBUCKET_USERNAME",
    "BITBUCKET_APP_PASSWORD",
];

/// True when a value is present and non-empty, without returning it.
pub fn env_configured(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// A borrowed view used by tests to prove no secret reached a JSON value.
pub fn value_contains_secret(value: &serde_json::Value, secret: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    match value {
        serde_json::Value::String(text) => text.contains(secret),
        serde_json::Value::Array(items) => {
            items.iter().any(|item| value_contains_secret(item, secret))
        }
        serde_json::Value::Object(object) => object
            .values()
            .any(|value| value_contains_secret(value, secret)),
        _ => false,
    }
}

/// Build a command path for diagnostics without exposing the argument list.
pub fn display_command(binary: &str) -> String {
    let name = Path::new(binary)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(binary);
    sanitize_message(name, 120)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn credentials(pairs: &[(&str, &str)]) -> Result<BitbucketCredentials, ForgeError> {
        let values = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();
        BitbucketCredentials::from_lookup(|name| values.get(name).cloned())
    }

    fn basic_credentials() -> BitbucketCredentials {
        credentials(&[
            ("BITBUCKET_USERNAME", "acme"),
            ("BITBUCKET_APP_PASSWORD", "app-password"),
        ])
        .unwrap()
    }

    fn bearer_credentials(token: &str) -> BitbucketCredentials {
        credentials(&[("BITBUCKET_TOKEN", token)]).unwrap()
    }

    fn cloud_origin() -> ForgeOrigin {
        parse_origin("https://bitbucket.org/acme/widgets.git").unwrap()
    }

    async fn test_client(
        credentials: BitbucketCredentials,
        max_response_bytes: usize,
        timeout: Duration,
    ) -> (MockServer, BitbucketCloudClient) {
        let server = MockServer::start().await;
        let api_base = format!("{}/2.0/", server.uri());
        let web_base = format!("{}/", server.uri());
        let client = BitbucketCloudClient::for_test(
            &api_base,
            &web_base,
            credentials,
            timeout,
            max_response_bytes,
        )
        .unwrap();
        (server, client)
    }

    fn request() -> ForgeChangeRequest {
        ForgeChangeRequest::new("Ship it", "Opened by a test", "feature/bitbucket", "main")
    }

    fn pull_request_response(server: &MockServer, id: u64) -> serde_json::Value {
        json!({
            "id": id,
            "title": "Ship it",
            "state": "OPEN",
            "links": {
                "html": {
                    "href": format!("{}/acme/widgets/pull-requests/{id}", server.uri())
                }
            }
        })
    }

    #[test]
    fn parses_bitbucket_cloud_https_and_ssh_origins() {
        let cases = [
            ("https://bitbucket.org/acme/widgets.git", "acme", "widgets"),
            ("https://bitbucket.org/acme/widgets", "acme", "widgets"),
            (
                "https://www.bitbucket.org/acme/widgets.git/",
                "acme",
                "widgets",
            ),
            (
                "https://git@bitbucket.org/acme/widgets.git",
                "acme",
                "widgets",
            ),
            ("git@bitbucket.org:acme/widgets.git", "acme", "widgets"),
            (
                "ssh://git@bitbucket.org/acme/widgets.git",
                "acme",
                "widgets",
            ),
            (
                "ssh://git@bitbucket.org:22/acme/widgets.git",
                "acme",
                "widgets",
            ),
            ("git@BitBucket.org:acme/widgets.git", "acme", "widgets"),
        ];
        for (raw, owner, repo) in cases {
            let origin = parse_origin(raw).unwrap_or_else(|error| {
                panic!("expected {raw} to parse: {error}");
            });
            assert_eq!(origin.provider, ForgeProviderKind::BitbucketCloud);
            assert_eq!(origin.host, "bitbucket.org");
            assert_eq!(origin.owner, owner);
            assert_eq!(origin.repo, repo);
            assert_eq!(origin.repository_slug(), format!("{owner}/{repo}"));
        }
    }

    #[test]
    fn bitbucket_origin_rejects_http_ports_and_extra_segments() {
        for raw in [
            "http://bitbucket.org/acme/widgets.git",
            "https://bitbucket.org:8443/acme/widgets.git",
            "ssh://git@bitbucket.org:7999/acme/widgets.git",
            "https://bitbucket.org/acme/widgets/src/main",
            "https://bitbucket.org/acme",
            "git@bitbucket.org:acme",
            "https://bitbucket.org/acme/widgets%2Fevil.git",
            "https://bitbucket.org/../widgets.git",
        ] {
            let error = parse_origin(raw).unwrap_err();
            assert!(
                matches!(
                    error.code,
                    ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED
                        | ToolErrorCode::FORGE_SERVER_UNSUPPORTED
                ),
                "{raw}: {error}"
            );
        }
    }

    #[test]
    fn bitbucket_server_and_unknown_origins_fail_closed() {
        let server = parse_origin("https://bitbucket.example.com/scm/PROJ/repo.git").unwrap_err();
        assert_eq!(server.code, ToolErrorCode::FORGE_SERVER_UNSUPPORTED);
        assert!(server.message.contains("Server/Data Center"), "{server}");

        let server =
            parse_origin("ssh://git@bitbucket.example.com:7999/proj/repo.git").unwrap_err();
        assert_eq!(server.code, ToolErrorCode::FORGE_SERVER_UNSUPPORTED);

        let server = parse_origin("https://bitbucket.org/scm/acme/widgets.git").unwrap_err();
        assert_eq!(server.code, ToolErrorCode::FORGE_SERVER_UNSUPPORTED);

        assert_eq!(
            parse_origin("https://gitlab.com/acme/widgets.git")
                .unwrap()
                .provider,
            ForgeProviderKind::GitLab
        );

        let unsupported = parse_origin("https://example.test/acme/widgets").unwrap_err();
        assert_eq!(unsupported.code, ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED);

        let empty = parse_origin("  ").unwrap_err();
        assert_eq!(empty.code, ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED);

        let local = parse_origin("/tmp/bare.git").unwrap_err();
        assert_eq!(local.code, ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED);
    }

    #[test]
    fn inspect_origin_reports_server_without_echoing_userinfo() {
        let inspection =
            inspect_origin("https://user:secret@bitbucket.example.com/scm/PROJ/repo.git");
        assert!(!inspection.supported);
        assert!(inspection.server_detected);
        assert!(inspection.detail.contains("Server/Data Center"));
        assert!(!inspection.detail.contains("secret"));

        let inspection = inspect_origin("https://bitbucket.org/acme/widgets.git");
        assert!(inspection.supported);
        assert_eq!(inspection.provider, Some(ForgeProviderKind::BitbucketCloud));
        assert_eq!(inspection.repository.as_deref(), Some("acme/widgets"));

        let inspection = inspect_origin("not a remote");
        assert!(!inspection.supported);
        assert!(inspection.host.is_none());
    }

    #[test]
    fn detects_and_resolves_providers_with_explicit_priority() {
        assert_eq!(
            detect_provider("https://bitbucket.org/acme/widgets.git"),
            ForgeProviderKind::BitbucketCloud
        );
        assert_eq!(
            detect_provider("git@gitlab.example.com:group/repo.git"),
            ForgeProviderKind::GitLab
        );
        assert_eq!(
            detect_provider("https://github.com/acme/widgets.git"),
            ForgeProviderKind::GitHub
        );

        let target = resolve_target(
            Some("bitbucket"),
            None,
            "https://bitbucket.org/acme/widgets.git",
        )
        .unwrap();
        assert_eq!(target.provider, ForgeProviderKind::BitbucketCloud);
        assert_eq!(target.selected_by, ForgeSelectionSource::Explicit);
        assert_eq!(target.repository().as_deref(), Some("acme/widgets"));

        let target = resolve_target(
            None,
            Some("forgejo"),
            "https://git.example.com/team/repo.git",
        )
        .unwrap();
        assert_eq!(target.provider, ForgeProviderKind::Gitea);
        assert_eq!(target.selected_by, ForgeSelectionSource::Environment);

        let target = resolve_target(
            Some("gitea"),
            Some("github"),
            "https://example.com/team/repo.git",
        )
        .unwrap();
        assert_eq!(target.provider, ForgeProviderKind::Gitea);
        assert_eq!(target.selected_by, ForgeSelectionSource::Explicit);

        let target = resolve_target(None, None, "https://github.com/acme/widgets.git").unwrap();
        assert_eq!(target.provider, ForgeProviderKind::GitHub);
        assert_eq!(target.selected_by, ForgeSelectionSource::Detected);
        assert_eq!(target.repository().as_deref(), Some("acme/widgets"));

        let error = resolve_target(Some("sourcehut"), None, "https://example.com/a/b").unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_UNSUPPORTED_PROVIDER);
        assert_eq!(error.provider, None);

        let error = resolve_target(Some("bitbucket"), None, "https://gitlab.com/group/repo.git")
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED);
        assert_eq!(error.provider, Some(ForgeProviderKind::BitbucketCloud));
    }

    #[test]
    fn provider_aliases_have_stable_metadata() {
        for (raw, kind) in [
            ("github", ForgeProviderKind::GitHub),
            ("gh", ForgeProviderKind::GitHub),
            ("gitlab", ForgeProviderKind::GitLab),
            ("glab", ForgeProviderKind::GitLab),
            ("gitea", ForgeProviderKind::Gitea),
            ("tea", ForgeProviderKind::Gitea),
            ("forgejo", ForgeProviderKind::Gitea),
            ("bitbucket", ForgeProviderKind::BitbucketCloud),
            ("bitbucket-cloud", ForgeProviderKind::BitbucketCloud),
            ("bitbucket_cloud", ForgeProviderKind::BitbucketCloud),
            ("  BITBUCKET ", ForgeProviderKind::BitbucketCloud),
        ] {
            assert_eq!(ForgeProviderKind::parse(raw).unwrap(), kind, "{raw}");
        }
        assert_eq!(ForgeProviderKind::GitLab.change_name(), "MR");
        assert_eq!(ForgeProviderKind::BitbucketCloud.change_name(), "PR");
        assert_eq!(ForgeProviderKind::GitHub.default_cli(), Some("gh"));
        assert_eq!(ForgeProviderKind::BitbucketCloud.default_cli(), None);
        assert_eq!(
            ForgeProviderKind::parse("").unwrap_err().code,
            ToolErrorCode::FORGE_UNSUPPORTED_PROVIDER
        );
    }

    #[test]
    fn bitbucket_credentials_support_basic_and_bearer_auth() {
        let basic = basic_credentials();
        assert_eq!(basic.mode(), BitbucketAuthMode::Basic);
        assert_eq!(
            basic.source_names(),
            "BITBUCKET_USERNAME + BITBUCKET_APP_PASSWORD"
        );
        assert_eq!(
            basic.authorization_header().unwrap(),
            format!("Basic {}", BASE64.encode("acme:app-password"))
        );
        let debug = format!("{basic:?}");
        assert!(!debug.contains("app-password"), "{debug}");
        assert!(!debug.contains("acme:"), "{debug}");

        let bearer = bearer_credentials("ATATT3xFfGF0testtoken1234567890");
        assert_eq!(bearer.mode(), BitbucketAuthMode::Bearer);
        assert_eq!(bearer.source_names(), "BITBUCKET_TOKEN");
        assert_eq!(
            bearer.authorization_header().unwrap(),
            "Bearer ATATT3xFfGF0testtoken1234567890"
        );
        let debug = format!("{bearer:?}");
        assert!(!debug.contains("ATATT"), "{debug}");
    }

    #[test]
    fn bitbucket_credentials_fail_closed_without_secret_leaks() {
        let missing = credentials(&[]).unwrap_err();
        assert_eq!(missing.code, ToolErrorCode::FORGE_AUTH_MISSING);
        assert!(missing.message.contains("BITBUCKET_TOKEN"), "{missing}");

        let missing_username =
            credentials(&[("BITBUCKET_APP_PASSWORD", "super-secret-value")]).unwrap_err();
        assert_eq!(missing_username.code, ToolErrorCode::FORGE_AUTH_MISSING);
        assert!(!missing_username.to_string().contains("super-secret-value"));

        let missing_password = credentials(&[("BITBUCKET_USERNAME", "acme")]).unwrap_err();
        assert_eq!(missing_password.code, ToolErrorCode::FORGE_AUTH_MISSING);

        let conflict = credentials(&[
            ("BITBUCKET_TOKEN", "token-value"),
            ("BITBUCKET_USERNAME", "acme"),
            ("BITBUCKET_APP_PASSWORD", "password-value"),
        ])
        .unwrap_err();
        assert_eq!(conflict.code, ToolErrorCode::FORGE_AUTH_CONFLICT);
        assert!(!conflict.to_string().contains("token-value"));
        assert!(!conflict.to_string().contains("password-value"));

        let invalid = credentials(&[("BITBUCKET_TOKEN", "token with space")]).unwrap_err();
        assert_eq!(invalid.code, ToolErrorCode::FORGE_AUTH_MISSING);
        assert!(!invalid.to_string().contains("token with space"));

        let colon = credentials(&[
            ("BITBUCKET_USERNAME", "acme:admin"),
            ("BITBUCKET_APP_PASSWORD", "password"),
        ])
        .unwrap_err();
        assert_eq!(colon.code, ToolErrorCode::FORGE_AUTH_MISSING);
    }

    #[tokio::test]
    async fn create_pull_request_posts_expected_payload_with_basic_auth() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        let expected_auth = format!("Basic {}", BASE64.encode("acme:app-password"));
        let response = pull_request_response(&server, 7);
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .and(header("authorization", expected_auth.as_str()))
            .and(header("accept", "application/json"))
            .and(body_json(json!({
                "title": "Ship it",
                "description": "Opened by a test",
                "source": {"branch": {"name": "feature/bitbucket"}},
                "destination": {"branch": {"name": "main"}},
                "close_source_branch": false
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;

        let change = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap();
        assert_eq!(change.provider, ForgeProviderKind::BitbucketCloud);
        assert_eq!(change.change_name, "PR");
        assert_eq!(change.id, Some(7));
        assert_eq!(change.repository.as_deref(), Some("acme/widgets"));
        assert_eq!(
            change.url,
            format!("{}/acme/widgets/pull-requests/7", server.uri())
        );
        assert_eq!(change.state.as_deref(), Some("OPEN"));
        assert_eq!(change.title, "Ship it");
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn create_pull_request_uses_bearer_auth() {
        let token = "ATATT3xFfGF0testtoken1234567890";
        let (server, client) =
            test_client(bearer_credentials(token), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(pull_request_response(&server, 8)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let change = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap();
        assert_eq!(change.id, Some(8));
        assert_eq!(client.auth_mode(), BitbucketAuthMode::Bearer);
    }

    #[tokio::test]
    async fn create_pull_request_falls_back_to_canonical_url() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": 9,
                "title": "Ship it",
                "state": "OPEN"
            })))
            .mount(&server)
            .await;

        let change = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap();
        assert_eq!(change.id, Some(9));
        assert_eq!(
            change.url,
            format!("{}/acme/widgets/pull-requests/9", server.uri())
        );
    }

    #[tokio::test]
    async fn create_pull_request_refuses_cross_origin_html_link() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": 10,
                "links": {"html": {"href": "https://evil.example/acme/widgets/pull-requests/10"}}
            })))
            .mount(&server)
            .await;

        let change = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap();
        assert_eq!(
            change.url,
            format!("{}/acme/widgets/pull-requests/10", server.uri())
        );
        assert!(!change.url.contains("evil.example"));
    }

    #[tokio::test]
    async fn create_pull_request_never_fakes_success_without_url_or_id() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"state": "OPEN"})))
            .mount(&server)
            .await;

        let error = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_RESPONSE_INVALID);
        assert!(error.message.contains("verify the provider"), "{error}");

        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(ResponseTemplate::new(201).set_body_string("not json"))
            .mount(&server)
            .await;
        let error = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_RESPONSE_INVALID);
    }

    #[tokio::test]
    async fn create_pull_request_validates_input_before_any_request() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        let cases = [
            ForgeChangeRequest::new("", "body", "feature", "main"),
            ForgeChangeRequest::new("line\nbreak", "body", "feature", "main"),
            ForgeChangeRequest::new("x".repeat(300), "body", "feature", "main"),
            ForgeChangeRequest::new("title", "body", "feature", "feature"),
            ForgeChangeRequest::new("title", "body", "feature branch", "main"),
            ForgeChangeRequest::new("title", "body", "-feature", "main"),
            ForgeChangeRequest::new("title", "x".repeat(40_000), "feature", "main"),
        ];
        for request in cases {
            let error = client
                .create_pull_request(&cloud_origin(), &request)
                .await
                .unwrap_err();
            assert_eq!(error.code, ToolErrorCode::FORGE_REQUEST_INVALID);
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn preflight_checks_repository_access_before_push() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        let expected_auth = format!("Basic {}", BASE64.encode("acme:app-password"));
        Mock::given(method("GET"))
            .and(path("/2.0/repositories/acme/widgets"))
            .and(header("authorization", expected_auth.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "full_name": "acme/widgets"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let preflight = client.preflight(&cloud_origin()).await.unwrap();
        assert_eq!(preflight.provider, ForgeProviderKind::BitbucketCloud);
        assert_eq!(preflight.repository.as_deref(), Some("acme/widgets"));
        assert_eq!(preflight.auth, "basic");
        assert!(preflight.detail.contains("credentials accepted"));
    }

    #[tokio::test]
    async fn preflight_maps_missing_auth_and_repository_access() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("GET"))
            .and(path("/2.0/repositories/acme/widgets"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "Invalid credentials"}
            })))
            .mount(&server)
            .await;
        let error = client.preflight(&cloud_origin()).await.unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_AUTH_FAILED);
        assert!(!error.retryable);

        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("GET"))
            .and(path("/2.0/repositories/acme/widgets"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": {"message": "Repository not found"}
            })))
            .mount(&server)
            .await;
        let error = client.preflight(&cloud_origin()).await.unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_REPO_NOT_FOUND);
    }

    #[tokio::test]
    async fn maps_http_statuses_to_typed_forge_errors() {
        let cases = [
            (400, ToolErrorCode::FORGE_REQUEST_REJECTED, false),
            (401, ToolErrorCode::FORGE_AUTH_FAILED, false),
            (403, ToolErrorCode::FORGE_PERMISSION_DENIED, false),
            (404, ToolErrorCode::FORGE_REPO_NOT_FOUND, false),
            (409, ToolErrorCode::FORGE_CONFLICT, false),
            (422, ToolErrorCode::FORGE_REQUEST_REJECTED, false),
            (429, ToolErrorCode::FORGE_RATE_LIMITED, true),
            (500, ToolErrorCode::FORGE_SERVICE_UNAVAILABLE, false),
            (503, ToolErrorCode::FORGE_SERVICE_UNAVAILABLE, false),
        ];
        for (status, code, retryable) in cases {
            let (server, client) =
                test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
            let mut response = ResponseTemplate::new(status).set_body_json(json!({
                "type": "error",
                "error": {"message": format!("provider detail for {status}")}
            }));
            if status == 429 {
                response = response.insert_header("Retry-After", "30");
            }
            Mock::given(method("POST"))
                .and(path("/2.0/repositories/acme/widgets/pullrequests"))
                .respond_with(response)
                .mount(&server)
                .await;

            let error = client
                .create_pull_request(&cloud_origin(), &request())
                .await
                .unwrap_err();
            assert_eq!(error.code, code, "HTTP {status}: {error}");
            assert_eq!(error.retryable, retryable, "HTTP {status}: {error}");
            assert!(
                error.message.contains("HTTP"),
                "HTTP {status} message: {error}"
            );
            if status == 429 {
                assert!(
                    error
                        .evidence
                        .as_deref()
                        .is_some_and(|evidence| evidence.contains("retry_after_secs=30")),
                    "{error:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn mutating_server_error_is_not_auto_retryable() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                "error": {"message": "service down"}
            })))
            .mount(&server)
            .await;
        let error = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_SERVICE_UNAVAILABLE);
        assert!(!error.retryable);
        assert!(error.message.contains("may have succeeded"), "{error}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn preflight_server_error_is_retryable() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("GET"))
            .and(path("/2.0/repositories/acme/widgets"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let error = client.preflight(&cloud_origin()).await.unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_SERVICE_UNAVAILABLE);
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn follows_same_origin_redirects() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("GET"))
            .and(path("/2.0/repositories/acme/widgets"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/redirected", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/redirected"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"full_name": "acme/widgets"})),
            )
            .mount(&server)
            .await;
        let preflight = client.preflight(&cloud_origin()).await.unwrap();
        assert_eq!(preflight.repository.as_deref(), Some("acme/widgets"));
    }

    #[tokio::test]
    async fn refuses_cross_origin_redirects() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("GET"))
            .and(path("/2.0/repositories/acme/widgets"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", "https://evil.example/steal-credentials"),
            )
            .mount(&server)
            .await;
        let error = client.preflight(&cloud_origin()).await.unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_REDIRECT_REFUSED);
        assert!(error.message.contains("another origin"), "{error}");
    }

    #[tokio::test]
    async fn bounds_response_size() {
        let (server, client) = test_client(basic_credentials(), 64, Duration::from_secs(5)).await;
        Mock::given(method("GET"))
            .and(path("/2.0/repositories/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(1024)))
            .mount(&server)
            .await;
        let error = client.preflight(&cloud_origin()).await.unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_RESPONSE_TOO_LARGE);
    }

    #[tokio::test]
    async fn maps_timeout_without_claiming_the_request_was_not_sent() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_millis(40)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_delay(Duration::from_millis(300))
                    .set_body_json(pull_request_response(&server, 11)),
            )
            .mount(&server)
            .await;
        let error = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_TIMEOUT);
        assert!(!error.retryable);
        assert!(
            error.message.contains("may have reached the server"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn upstream_error_text_is_bounded_and_redacted() {
        let secret = "ATATT3xFfGF0superSecretToken123456";
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "type": "error",
                "error": {
                    "message": format!("bad token {secret}"),
                    "detail": "authorization: Bearer another-secret-value"
                }
            })))
            .mount(&server)
            .await;
        let error = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_REQUEST_REJECTED);
        let rendered = error.to_string();
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(!rendered.contains("another-secret-value"), "{rendered}");
        assert!(rendered.contains("[redacted"), "{rendered}");
        assert!(error.message.chars().count() <= MAX_ERROR_MESSAGE_CHARS);
    }

    #[tokio::test]
    async fn invalid_error_body_still_maps_the_status() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("GET"))
            .and(path("/2.0/repositories/acme/widgets"))
            .respond_with(ResponseTemplate::new(403).set_body_string("<html>forbidden</html>"))
            .mount(&server)
            .await;
        let error = client.preflight(&cloud_origin()).await.unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_PERMISSION_DENIED);
        assert!(!error.message.contains("<html>"), "{error}");
    }

    #[test]
    fn invalid_client_configuration_fails_closed() {
        let error =
            BitbucketCloudConfig::for_test("file:///tmp/api", "https://bitbucket.org").unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_CONFIG_INVALID);

        let error =
            BitbucketCloudConfig::for_test("https://api.bitbucket.org/2.0", "ftp://bitbucket.org")
                .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_CONFIG_INVALID);

        let config = BitbucketCloudConfig::for_test(
            "https://api.bitbucket.org/2.0",
            "https://bitbucket.org",
        )
        .unwrap();
        let config = config
            .with_timeout(Duration::from_millis(1))
            .with_max_response_bytes(0);
        assert_eq!(config.timeout(), Duration::from_millis(1));
        assert_eq!(config.max_response_bytes(), 1);
    }

    #[test]
    fn command_availability_does_not_spawn_a_process() {
        assert!(command_available("/bin/sh"));
        assert!(!command_available("/definitely/not/a/command"));
        assert!(!command_available(""));
        assert_eq!(display_command("/tmp/fake-gh"), "fake-gh");
    }

    #[test]
    fn provider_snapshots_cover_all_transports() {
        let snapshots = provider_snapshots();
        assert_eq!(snapshots.len(), 4);
        let bitbucket = snapshots
            .iter()
            .find(|snapshot| snapshot.provider == ForgeProviderKind::BitbucketCloud)
            .unwrap();
        assert!(bitbucket.supported);
        assert!(bitbucket.command.is_none());
        assert_eq!(bitbucket.provider.transport(), "rest");
        let github = snapshots
            .iter()
            .find(|snapshot| snapshot.provider == ForgeProviderKind::GitHub)
            .unwrap();
        assert_eq!(github.provider.transport(), "cli");
        assert!(github.auth_configured.is_none());
    }

    #[test]
    fn every_forge_error_code_is_cataloged_with_a_hint() {
        let codes = [
            ToolErrorCode::FORGE_UNSUPPORTED_PROVIDER,
            ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED,
            ToolErrorCode::FORGE_SERVER_UNSUPPORTED,
            ToolErrorCode::FORGE_AUTH_MISSING,
            ToolErrorCode::FORGE_AUTH_CONFLICT,
            ToolErrorCode::FORGE_AUTH_FAILED,
            ToolErrorCode::FORGE_PERMISSION_DENIED,
            ToolErrorCode::FORGE_REPO_NOT_FOUND,
            ToolErrorCode::FORGE_CONFLICT,
            ToolErrorCode::FORGE_RATE_LIMITED,
            ToolErrorCode::FORGE_SERVICE_UNAVAILABLE,
            ToolErrorCode::FORGE_TIMEOUT,
            ToolErrorCode::FORGE_NETWORK_FAILED,
            ToolErrorCode::FORGE_RESPONSE_INVALID,
            ToolErrorCode::FORGE_RESPONSE_TOO_LARGE,
            ToolErrorCode::FORGE_REDIRECT_REFUSED,
            ToolErrorCode::FORGE_REQUEST_REJECTED,
            ToolErrorCode::FORGE_REQUEST_INVALID,
            ToolErrorCode::FORGE_CLI_UNAVAILABLE,
            ToolErrorCode::FORGE_CONFIG_INVALID,
        ];
        for code in codes {
            assert!(code.is_known(), "{code} is missing");
            assert_eq!(code.category(), "forge");
            assert!(code.default_hint().len() >= 20);
        }
    }

    #[test]
    fn forge_failure_contract_and_data_are_redacted() {
        let secret = "ghp_abcdefghijklmnop";
        let error = ForgeError::new(
            ToolErrorCode::FORGE_AUTH_FAILED,
            format!("token {secret} rejected"),
        )
        .with_provider(ForgeProviderKind::BitbucketCloud)
        .with_evidence(format!("token={secret}"));
        let failure = error.to_tool_failure();
        assert_eq!(failure.code, ToolErrorCode::FORGE_AUTH_FAILED);
        assert!(!failure.message.contains(secret));
        assert!(!failure.render().contains(secret));
        let data = error.data();
        assert_eq!(data["code"], "FORGE_AUTH_FAILED");
        assert_eq!(data["provider"], "bitbucket");
        assert!(!value_contains_secret(&data, secret));
    }

    #[test]
    fn parses_github_enterprise_and_nested_gitlab_origins() {
        let origin = parse_origin("git@github.example.com:acme/widgets.git").unwrap();
        assert_eq!(origin.provider, ForgeProviderKind::GitHub);
        assert_eq!(origin.host, "github.example.com");
        assert_eq!(origin.repository_slug(), "acme/widgets");

        let origin = parse_origin("https://gitlab.example.com/group/subgroup/repo.git").unwrap();
        assert_eq!(origin.provider, ForgeProviderKind::GitLab);
        assert_eq!(origin.owner, "group/subgroup");
        assert_eq!(origin.repo, "repo");
        assert_eq!(origin.repository_slug(), "group/subgroup/repo");

        let origin = parse_origin("ssh://git@gitlab.example.com:22/team/repo.git").unwrap();
        assert_eq!(origin.provider, ForgeProviderKind::GitLab);

        let error = parse_origin("https://gitlab.example.com/group").unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED);
    }

    #[test]
    fn inspect_origin_reports_github_enterprise() {
        let inspection = inspect_origin("https://github.example.com/acme/widgets.git");
        assert!(inspection.supported);
        assert_eq!(inspection.provider, Some(ForgeProviderKind::GitHub));
        assert_eq!(inspection.host.as_deref(), Some("github.example.com"));
        assert_eq!(inspection.repository.as_deref(), Some("acme/widgets"));
    }

    #[test]
    fn parse_origin_trims_surrounding_whitespace() {
        let origin = parse_origin(
            "  https://bitbucket.org/acme/widgets.git
",
        )
        .unwrap();
        assert_eq!(origin.repository_slug(), "acme/widgets");
        let origin = parse_origin("  git@bitbucket.org:acme/widgets.git  ").unwrap();
        assert_eq!(origin.repository_slug(), "acme/widgets");
    }

    #[test]
    fn request_validation_accepts_boundary_values() {
        let title = "t".repeat(MAX_TITLE_CHARS);
        let description = "d".repeat(MAX_DESCRIPTION_CHARS);
        let branch = "b".repeat(MAX_BRANCH_CHARS);
        let request = ForgeChangeRequest::new(title, description, branch, "main");
        request.validate().unwrap();

        let error = ForgeChangeRequest::new("t".repeat(MAX_TITLE_CHARS + 1), "", "feature", "main")
            .validate()
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_REQUEST_INVALID);

        let error = ForgeChangeRequest::new(
            "title",
            "d".repeat(MAX_DESCRIPTION_CHARS + 1),
            "feature",
            "main",
        )
        .validate()
        .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_REQUEST_INVALID);

        let error = ForgeChangeRequest::new("title", "", "b".repeat(MAX_BRANCH_CHARS + 1), "main")
            .validate()
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_REQUEST_INVALID);

        let error = ForgeChangeRequest::new("title", "nul\0inside", "feature", "main")
            .validate()
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_REQUEST_INVALID);
    }

    #[test]
    fn credentials_reject_an_oversized_token() {
        let token = "a".repeat(4097);
        let error = credentials(&[("BITBUCKET_TOKEN", token.as_str())]).unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_AUTH_MISSING);
        assert!(error.message.contains("4096"), "{error}");
    }

    #[test]
    fn bitbucket_target_requires_a_parsed_origin() {
        let target = ForgeTarget {
            provider: ForgeProviderKind::BitbucketCloud,
            origin: None,
            selected_by: ForgeSelectionSource::Explicit,
        };
        let error = target.require_origin().unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_ORIGIN_UNSUPPORTED);
        assert_eq!(error.provider, Some(ForgeProviderKind::BitbucketCloud));
    }

    #[test]
    fn status_text_sanitizer_redacts_and_truncates() {
        let secret = "ghp_abcdefghijklmnop";
        let text = sanitize_status_text(&format!("token={secret} {}", "x".repeat(100)), 40);
        assert!(!text.contains(secret));
        assert!(text.chars().count() <= 40);
        assert_eq!(sanitize_status_text("line\nbreak", 40), "line\nbreak");
    }

    #[test]
    fn value_contains_secret_walks_nested_structures() {
        let value = json!({
            "nested": {"array": ["safe", {"token": "sentinel-value"}]},
            "count": 2
        });
        assert!(value_contains_secret(&value, "sentinel-value"));
        assert!(!value_contains_secret(&value, "not-present"));
        assert!(!value_contains_secret(&value, ""));
    }

    #[tokio::test]
    async fn rate_limit_hint_names_the_retry_window() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "12")
                    .set_body_json(json!({"error": {"message": "slow down"}})),
            )
            .mount(&server)
            .await;
        let error = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::FORGE_RATE_LIMITED);
        assert!(error.retryable);
        assert!(error.hint.contains("12 second"), "{error:?}");
        assert!(error.message.contains("slow down"), "{error}");
    }

    #[tokio::test]
    async fn create_pull_request_accepts_a_200_response() {
        let (server, client) =
            test_client(basic_credentials(), 16 * 1024, Duration::from_secs(5)).await;
        Mock::given(method("POST"))
            .and(path("/2.0/repositories/acme/widgets/pullrequests"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(pull_request_response(&server, 12)),
            )
            .mount(&server)
            .await;
        let change = client
            .create_pull_request(&cloud_origin(), &request())
            .await
            .unwrap();
        assert_eq!(change.id, Some(12));
    }

    #[test]
    fn forged_origin_labels_do_not_expose_userinfo() {
        let origin = parse_origin("https://user:password@bitbucket.org/acme/widgets.git").unwrap();
        assert_eq!(origin.repository_slug(), "acme/widgets");
        assert_eq!(origin.web_url(), "https://bitbucket.org/acme/widgets");
        assert!(!origin.web_url().contains("password"));
    }
}
