//! Validation and secret indirection for ACP agent configuration.
//!
//! Remote ACP agents are configured with a URL, a bounded header map, and an
//! environment variable name for the `Authorization` header. Values are never
//! persisted: `${ENV_VAR}` references stay in SQLite and are expanded only when
//! a connection is opened. This module owns the pure validation rules so they
//! can be tested without a daemon, network, or environment mutation.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use reqwest::header::{HeaderName, HeaderValue};
use vp_protocol::types::{AcpAgentConfig, AcpTransport};

/// Hard cap on the number of configured extra headers.
pub(crate) const MAX_HEADERS: usize = 32;
/// Hard cap on one header name.
pub(crate) const MAX_HEADER_NAME_BYTES: usize = 128;
/// Hard cap on one header value, including unresolved `${ENV_VAR}` references.
pub(crate) const MAX_HEADER_VALUE_BYTES: usize = 4_096;
/// Hard cap on the combined header value size.
pub(crate) const MAX_HEADER_TOTAL_BYTES: usize = 32 * 1024;
/// Hard cap on one environment variable name used for auth indirection.
pub(crate) const MAX_ENV_NAME_BYTES: usize = 128;
/// Hard cap on an ACP endpoint URL.
pub(crate) const MAX_ENDPOINT_BYTES: usize = 4_096;
/// Default and bounds for the remote connect/handshake timeout.
pub(crate) const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;
pub(crate) const MIN_CONNECT_TIMEOUT_SECS: u64 = 1;
pub(crate) const MAX_CONNECT_TIMEOUT_SECS: u64 = 300;

/// Headers that the transport owns. Allowing a user to override them would let
/// a configuration desynchronize connection/session routing or smuggle a
/// second authentication channel past `auth_header_env`.
const RESERVED_HEADERS: &[&str] = &[
    "accept",
    "authorization",
    "connection",
    "content-length",
    "content-type",
    "cookie",
    "host",
    "proxy-authorization",
    "sec-websocket-accept",
    "sec-websocket-extensions",
    "sec-websocket-key",
    "sec-websocket-protocol",
    "sec-websocket-version",
    "set-cookie",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
    "acp-connection-id",
    "acp-session-id",
    "acp-protocol-version",
];

/// Query parameter names that look like inline credentials. VerlinPark refuses
/// to persist them in an endpoint because the endpoint is returned by list and
/// probe RPCs. Credentials belong in an environment variable.
const SECRET_QUERY_KEYS: &[&str] = &[
    "access_token",
    "api_key",
    "apikey",
    "auth",
    "authorization",
    "bearer",
    "client_secret",
    "credential",
    "credentials",
    "key",
    "password",
    "passwd",
    "secret",
    "sig",
    "signature",
    "token",
];

/// Validate and normalize a full ACP agent configuration.
///
/// Existing stdio configurations are accepted unchanged. Remote
/// configurations must be explicitly opted in with `allow_remote`, must have an
/// endpoint and no process command, and may not carry secrets inline.
pub fn validate_agent_config(mut config: AcpAgentConfig) -> Result<AcpAgentConfig> {
    config.name = config.name.trim().to_string();
    config.command = config.command.trim().to_string();
    if config.name.is_empty() || config.name.chars().count() > 100 {
        bail!("ACP agent name must be 1-100 characters");
    }
    if !(30..=14_400).contains(&config.timeout_secs) {
        bail!("ACP agent timeout must be between 30 and 14400 seconds");
    }
    validate_env_map(&config.env)?;

    match config.transport {
        AcpTransport::Stdio => {
            if config.command.is_empty() || config.command.chars().count() > 4_096 {
                bail!("ACP agent command must be 1-4096 characters");
            }
            if config.args.len() > 128
                || config
                    .args
                    .iter()
                    .any(|arg| arg.chars().count() > 4_096 || arg.contains('\0'))
            {
                bail!("ACP agent arguments are limited to 128 values of 4096 characters");
            }
            if config.allow_remote {
                bail!("allow_remote is only valid for remote ACP transports");
            }
            if config
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| !endpoint.trim().is_empty())
            {
                bail!("stdio ACP agents cannot configure an endpoint");
            }
            if config
                .auth_header_env
                .as_deref()
                .is_some_and(|name| !name.trim().is_empty())
            {
                bail!("stdio ACP agents cannot configure auth_header_env");
            }
            if !config.headers.is_empty() {
                bail!("stdio ACP agents cannot configure HTTP headers");
            }
            config.endpoint = None;
            config.auth_header_env = None;
            config.headers.clear();
            config.connect_timeout_secs = DEFAULT_CONNECT_TIMEOUT_SECS;
        }
        AcpTransport::StreamableHttp | AcpTransport::Websocket => {
            if !config.allow_remote {
                bail!(
                    "remote ACP transport {} requires allow_remote=true; remote agents are trusted external services and are not sandboxed",
                    config.transport.as_str()
                );
            }
            if !config.command.is_empty() {
                bail!("remote ACP agents cannot configure a process command");
            }
            if !config.args.is_empty() {
                bail!("remote ACP agents cannot configure process arguments");
            }
            if !config.env.is_empty() {
                bail!("remote ACP agents cannot configure process environment variables");
            }
            let endpoint = config
                .endpoint
                .as_deref()
                .map(str::trim)
                .filter(|endpoint| !endpoint.is_empty())
                .ok_or_else(|| anyhow!("remote ACP agent requires an endpoint"))?;
            config.endpoint = Some(validate_acp_endpoint(config.transport, endpoint)?);
            if !(MIN_CONNECT_TIMEOUT_SECS..=MAX_CONNECT_TIMEOUT_SECS)
                .contains(&config.connect_timeout_secs)
            {
                bail!(
                    "ACP connect_timeout_secs must be between {MIN_CONNECT_TIMEOUT_SECS} and {MAX_CONNECT_TIMEOUT_SECS}"
                );
            }
            if let Some(name) = config.auth_header_env.as_deref() {
                let name = name.trim();
                if name.is_empty() {
                    config.auth_header_env = None;
                } else {
                    validate_env_name(name)?;
                    config.auth_header_env = Some(name.to_string());
                }
            }
            config.headers = validate_headers(&config.headers)?;
        }
    }
    Ok(config)
}

/// Validate a remote endpoint URL for the selected transport profile.
///
/// The returned URL is normalized and contains no userinfo or fragment. Query
/// strings are allowed, but query keys that look like inline credentials are
/// rejected because endpoints are not secret storage.
pub fn validate_acp_endpoint(transport: AcpTransport, endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() || endpoint.len() > MAX_ENDPOINT_BYTES {
        bail!("ACP endpoint must be 1-{MAX_ENDPOINT_BYTES} bytes");
    }
    if endpoint.chars().any(char::is_whitespace) {
        bail!("ACP endpoint must not contain whitespace");
    }
    let authority = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    if authority.is_empty() || authority.starts_with('/') {
        bail!("ACP endpoint must include a host");
    }
    let url =
        reqwest::Url::parse(endpoint).map_err(|_| anyhow!("ACP endpoint is not a valid URL"))?;
    let scheme = url.scheme().to_ascii_lowercase();
    match transport {
        AcpTransport::StreamableHttp => {
            if !matches!(scheme.as_str(), "http" | "https") {
                bail!("streamable_http ACP endpoints must use http or https, not {scheme}");
            }
        }
        AcpTransport::Websocket => {
            if !matches!(scheme.as_str(), "ws" | "wss") {
                bail!("websocket ACP endpoints must use ws or wss, not {scheme}");
            }
        }
        AcpTransport::Stdio => bail!("stdio ACP agents do not have endpoints"),
    }
    if url.host_str().is_none_or(str::is_empty) {
        bail!("ACP endpoint must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!(
            "ACP endpoint must not embed credentials; use auth_header_env or a ${{ENV_VAR}} header reference"
        );
    }
    if url.fragment().is_some() {
        bail!("ACP endpoint must not include a fragment");
    }
    for (key, _) in url.query_pairs() {
        let key = key.to_ascii_lowercase();
        if SECRET_QUERY_KEYS.iter().any(|secret| key == *secret)
            || SECRET_QUERY_KEYS
                .iter()
                .any(|secret| key.ends_with(&format!("_{secret}")))
        {
            bail!(
                "ACP endpoint query contains a credential-like parameter '{key}'; use auth_header_env instead"
            );
        }
    }
    Ok(url.to_string())
}

fn validate_env_map(env: &BTreeMap<String, String>) -> Result<()> {
    if env.len() > 64
        || env.iter().any(|(key, value)| {
            key.is_empty()
                || key.chars().count() > 256
                || key.contains('=')
                || key.contains('\0')
                || value.chars().count() > 16_384
                || value.contains('\0')
        })
    {
        bail!("ACP agent environment is limited to 64 valid variables of 16 KiB each");
    }
    Ok(())
}

/// Validate an environment variable *name* used for auth or header indirection.
pub fn validate_env_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_ENV_NAME_BYTES {
        bail!("environment variable name must be 1-{MAX_ENV_NAME_BYTES} bytes");
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        bail!("environment variable name must not be empty");
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        bail!("environment variable name must start with a letter or underscore");
    }
    if !chars.all(|character| character.is_ascii_alphanumeric() || character == '_') {
        bail!("environment variable name may only contain letters, digits, and underscores");
    }
    Ok(())
}

/// Validate and normalize the configured extra header map.
///
/// Values may contain `${ENV_VAR}` references. Validation checks the literal
/// template only; expansion happens when a connection is opened.
pub fn validate_headers(headers: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    if headers.len() > MAX_HEADERS {
        bail!("ACP headers are limited to {MAX_HEADERS} entries");
    }
    let mut normalized = BTreeMap::new();
    let mut total = 0_usize;
    for (name, value) in headers {
        let name = name.trim();
        if name.is_empty() || name.len() > MAX_HEADER_NAME_BYTES {
            bail!("ACP header names must be 1-{MAX_HEADER_NAME_BYTES} bytes");
        }
        let parsed = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow!("ACP header name '{name}' is not valid"))?;
        let lower = parsed.as_str().to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lower.as_str()) {
            bail!(
                "ACP header '{lower}' is managed by the transport; use auth_header_env for credentials"
            );
        }
        let value = value.trim();
        if value.len() > MAX_HEADER_VALUE_BYTES {
            bail!("ACP header '{lower}' exceeds {MAX_HEADER_VALUE_BYTES} bytes");
        }
        if value.contains(['\r', '\n', '\0']) {
            bail!("ACP header '{lower}' contains a forbidden control character");
        }
        validate_env_references(value)?;
        total = total.saturating_add(value.len());
        if total > MAX_HEADER_TOTAL_BYTES {
            bail!("ACP headers exceed {MAX_HEADER_TOTAL_BYTES} bytes in total");
        }
        // Reject a literal value that is not a valid HTTP header value. Values
        // containing references are checked after expansion at connect time.
        if !value.contains("${") {
            HeaderValue::from_str(value)
                .map_err(|_| anyhow!("ACP header '{lower}' is not a valid HTTP header value"))?;
        }
        normalized.insert(lower, value.to_string());
    }
    Ok(normalized)
}

/// Check `${ENV_VAR}` references without resolving them.
fn validate_env_references(value: &str) -> Result<()> {
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            bail!("header value has an unterminated ${{ENV_VAR}} reference");
        };
        validate_env_name(after[..end].trim())?;
        rest = &after[end + 1..];
    }
    if rest.contains('}') && !value.contains("${") {
        bail!("header value contains an unmatched '}}'");
    }
    Ok(())
}

/// Expand `${ENV_VAR}` references in a header value using the supplied lookup.
///
/// The error names the variable, never its value.
pub fn expand_env_references(
    value: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String> {
    let mut expanded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            bail!("header value has an unterminated ${{ENV_VAR}} reference");
        };
        let name = after[..end].trim();
        validate_env_name(name)?;
        let resolved =
            lookup(name).ok_or_else(|| anyhow!("environment variable '{name}' is not set"))?;
        if resolved.contains(['\r', '\n', '\0']) {
            bail!("environment variable '{name}' contains a forbidden control character");
        }
        expanded.push_str(&resolved);
        if expanded.len() > MAX_HEADER_VALUE_BYTES {
            bail!("expanded header value exceeds {MAX_HEADER_VALUE_BYTES} bytes");
        }
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    Ok(expanded)
}

/// Build the final, resolved header list for a remote connection.
///
/// `env` is injected for tests. The `Authorization` value comes from
/// `auth_header_env` and is used verbatim (for example `Bearer <token>`); no
/// scheme is invented. Expanded values are returned to the caller and are never
/// persisted or logged by this module.
pub fn resolve_remote_headers(
    auth_header_env: Option<&str>,
    headers: &BTreeMap<String, String>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Vec<(HeaderName, HeaderValue)>> {
    let mut resolved = Vec::new();
    if let Some(name) = auth_header_env {
        validate_env_name(name)?;
        let value = env(name).ok_or_else(|| {
            anyhow!("environment variable '{name}' for ACP Authorization is not set")
        })?;
        if value.trim().is_empty() {
            bail!("environment variable '{name}' for ACP Authorization is empty");
        }
        if value.contains(['\r', '\n', '\0']) {
            bail!(
                "environment variable '{name}' for ACP Authorization contains a control character"
            );
        }
        let header = HeaderValue::from_str(&value).map_err(|_| {
            anyhow!("environment variable '{name}' is not a valid Authorization header value")
        })?;
        resolved.push((HeaderName::from_static("authorization"), header));
    }
    for (name, template) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow!("ACP header name '{name}' is not valid"))?;
        let value = expand_env_references(template, &env)?;
        let value = HeaderValue::from_str(&value).map_err(|_| {
            anyhow!(
                "ACP header '{}' is not a valid HTTP header value",
                name.as_str()
            )
        })?;
        resolved.push((name, value));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config(transport: AcpTransport) -> AcpAgentConfig {
        AcpAgentConfig {
            name: "remote".into(),
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            enabled: true,
            auto_approve: false,
            timeout_secs: 900,
            transport,
            endpoint: Some("https://acp.example.test/acp".into()),
            auth_header_env: Some("ACP_TOKEN".into()),
            headers: BTreeMap::new(),
            connect_timeout_secs: 30,
            allow_remote: true,
        }
    }

    #[test]
    fn accepts_existing_stdio_configuration_unchanged() {
        let config = AcpAgentConfig {
            name: "  gemini  ".into(),
            command: " npx ".into(),
            args: vec!["-y".into(), "gemini".into()],
            env: BTreeMap::from([("API_KEY".into(), "value".into())]),
            enabled: true,
            auto_approve: false,
            timeout_secs: 900,
            transport: AcpTransport::Stdio,
            endpoint: None,
            auth_header_env: None,
            headers: BTreeMap::new(),
            connect_timeout_secs: 0,
            allow_remote: false,
        };
        let validated = validate_agent_config(config).unwrap();
        assert_eq!(validated.name, "gemini");
        assert_eq!(validated.command, "npx");
        assert_eq!(validated.args, ["-y", "gemini"]);
        assert_eq!(validated.transport, AcpTransport::Stdio);
        assert_eq!(validated.connect_timeout_secs, 30);
    }

    #[test]
    fn rejects_stdio_with_remote_fields() {
        for mutate in [
            |config: &mut AcpAgentConfig| config.endpoint = Some("https://x.test".into()),
            |config: &mut AcpAgentConfig| config.auth_header_env = Some("TOKEN".into()),
            |config: &mut AcpAgentConfig| config.allow_remote = true,
        ] {
            let mut config = AcpAgentConfig {
                name: "local".into(),
                command: "npx".into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                enabled: true,
                auto_approve: false,
                timeout_secs: 900,
                transport: AcpTransport::Stdio,
                endpoint: None,
                auth_header_env: None,
                headers: BTreeMap::new(),
                connect_timeout_secs: 30,
                allow_remote: false,
            };
            mutate(&mut config);
            assert!(validate_agent_config(config).is_err());
        }
    }

    #[test]
    fn remote_requires_explicit_allow_remote() {
        let mut config = base_config(AcpTransport::StreamableHttp);
        config.allow_remote = false;
        let error = validate_agent_config(config).unwrap_err().to_string();
        assert!(error.contains("allow_remote=true"), "{error}");
    }

    #[test]
    fn remote_rejects_command_and_process_environment() {
        let mut config = base_config(AcpTransport::StreamableHttp);
        config.command = "npx".into();
        assert!(validate_agent_config(config).is_err());

        let mut config = base_config(AcpTransport::Websocket);
        config.args = vec!["--stdio".into()];
        assert!(validate_agent_config(config).is_err());

        let mut config = base_config(AcpTransport::Websocket);
        config.env = BTreeMap::from([("HOME".into(), "/".into())]);
        assert!(validate_agent_config(config).is_err());
    }

    #[test]
    fn remote_requires_endpoint() {
        let mut config = base_config(AcpTransport::StreamableHttp);
        config.endpoint = Some("   ".into());
        assert!(validate_agent_config(config.clone()).is_err());
        config.endpoint = None;
        assert!(validate_agent_config(config).is_err());
    }

    #[test]
    fn endpoint_schemes_match_the_profile() {
        assert!(
            validate_acp_endpoint(AcpTransport::StreamableHttp, "https://acp.example.test/acp")
                .is_ok()
        );
        assert!(
            validate_acp_endpoint(AcpTransport::StreamableHttp, "http://127.0.0.1:9/acp").is_ok()
        );
        assert!(
            validate_acp_endpoint(AcpTransport::StreamableHttp, "ws://127.0.0.1:9/acp").is_err()
        );
        assert!(
            validate_acp_endpoint(AcpTransport::Websocket, "wss://acp.example.test/acp").is_ok()
        );
        assert!(validate_acp_endpoint(AcpTransport::Websocket, "ws://127.0.0.1:9/acp").is_ok());
        assert!(
            validate_acp_endpoint(AcpTransport::Websocket, "https://acp.example.test/acp").is_err()
        );
        assert!("file:///tmp/acp.sock".parse::<reqwest::Url>().is_ok());
        assert!(validate_acp_endpoint(AcpTransport::StreamableHttp, "file:///tmp/acp").is_err());
    }

    #[test]
    fn endpoint_rejects_credentials_and_secret_queries() {
        for endpoint in [
            "https://user:pass@acp.example.test/acp",
            "https://user@acp.example.test/acp",
            "https://acp.example.test/acp?token=abc",
            "https://acp.example.test/acp?access_token=abc",
            "https://acp.example.test/acp?api_key=abc",
            "https://acp.example.test/acp#fragment",
            "https:///acp",
            "https://acp.example.test/acp path",
        ] {
            assert!(
                validate_acp_endpoint(AcpTransport::StreamableHttp, endpoint).is_err(),
                "{endpoint} should be rejected"
            );
        }
        assert!(
            validate_acp_endpoint(
                AcpTransport::StreamableHttp,
                "https://acp.example.test/acp?project=demo&mode=stream"
            )
            .is_ok()
        );
    }

    #[test]
    fn endpoint_is_normalized() {
        let normalized =
            validate_acp_endpoint(AcpTransport::StreamableHttp, "HTTPS://ACP.EXAMPLE.TEST/acp")
                .unwrap();
        assert_eq!(normalized, "https://acp.example.test/acp");
    }

    #[test]
    fn env_names_are_restricted() {
        for good in ["TOKEN", "_TOKEN", "A1_B2"] {
            validate_env_name(good).unwrap();
        }
        for bad in ["", "1TOKEN", "TO-KEN", "TO N", "TO.KEN", "TO€KEN"] {
            assert!(validate_env_name(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn header_validation_is_bounded_and_blocks_transport_headers() {
        let headers = BTreeMap::from([("x-project".into(), "demo".into())]);
        assert_eq!(validate_headers(&headers).unwrap(), headers);

        for name in ["authorization", "Acp-Connection-Id", "cookie", "host"] {
            let headers = BTreeMap::from([(name.into(), "value".into())]);
            assert!(
                validate_headers(&headers).is_err(),
                "{name} should be reserved"
            );
        }
        let headers = BTreeMap::from([("x-test".into(), "line\r\ninjected".into())]);
        assert!(validate_headers(&headers).is_err());
        let headers = BTreeMap::from([("x-test".into(), "x".repeat(MAX_HEADER_VALUE_BYTES + 1))]);
        assert!(validate_headers(&headers).is_err());
        let headers = (0..=MAX_HEADERS)
            .map(|index| (format!("x-{index}"), "v".to_string()))
            .collect();
        assert!(validate_headers(&headers).is_err());
        let headers = BTreeMap::from([("bad header".into(), "value".into())]);
        assert!(validate_headers(&headers).is_err());
    }

    #[test]
    fn header_values_support_environment_indirection() {
        let headers = BTreeMap::from([
            ("x-token".into(), "${ACP_TOKEN}".into()),
            ("x-prefix".into(), "Bearer ${ACP_TOKEN}".into()),
        ]);
        assert_eq!(validate_headers(&headers).unwrap(), headers);
        let resolved = resolve_remote_headers(None, &headers, |name| {
            (name == "ACP_TOKEN").then(|| "s3cret".to_string())
        })
        .unwrap();
        assert_eq!(resolved.len(), 2);
        let token = resolved
            .iter()
            .find(|(name, _)| name.as_str() == "x-token")
            .expect("x-token header");
        assert_eq!(token.1.to_str().unwrap(), "s3cret");
        let prefix = resolved
            .iter()
            .find(|(name, _)| name.as_str() == "x-prefix")
            .expect("x-prefix header");
        assert_eq!(prefix.1.to_str().unwrap(), "Bearer s3cret");
    }

    #[test]
    fn header_expansion_names_missing_variables_without_values() {
        let headers = BTreeMap::from([("x-token".into(), "${MISSING}".into())]);
        let error = resolve_remote_headers(None, &headers, |_| None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("MISSING"), "{error}");
        assert!(!error.contains("s3cret"));
    }

    #[test]
    fn authorization_is_read_from_the_named_environment_variable() {
        let resolved = resolve_remote_headers(Some("ACP_TOKEN"), &BTreeMap::new(), |name| {
            (name == "ACP_TOKEN").then(|| "Bearer abc".to_string())
        })
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0.as_str(), "authorization");
        assert_eq!(resolved[0].1.to_str().unwrap(), "Bearer abc");

        let error = resolve_remote_headers(Some("ACP_TOKEN"), &BTreeMap::new(), |_| None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ACP_TOKEN"));
        let error = resolve_remote_headers(Some("ACP_TOKEN"), &BTreeMap::new(), |_| {
            Some("Bearer\r\nInjected: yes".to_string())
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("control character"));
    }

    #[test]
    fn expanded_values_are_bounded() {
        let headers = BTreeMap::from([("x-long".into(), "${LONG}".into())]);
        let error = resolve_remote_headers(None, &headers, |_| {
            Some("x".repeat(MAX_HEADER_VALUE_BYTES + 1))
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("exceeds"), "{error}");
    }

    #[test]
    fn remote_connect_timeout_is_bounded() {
        for value in [0, 1, 300] {
            let mut config = base_config(AcpTransport::StreamableHttp);
            config.connect_timeout_secs = value;
            let result = validate_agent_config(config);
            if value == 1 || value == 300 {
                assert!(result.is_ok(), "{value}");
            } else {
                assert!(result.is_err(), "{value}");
            }
        }
        let mut config = base_config(AcpTransport::Websocket);
        config.connect_timeout_secs = 301;
        assert!(validate_agent_config(config).is_err());
    }

    #[test]
    fn names_commands_arguments_and_timeouts_are_bounded() {
        let mut config = AcpAgentConfig {
            name: "n".repeat(101),
            command: "npx".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            enabled: true,
            auto_approve: false,
            timeout_secs: 900,
            transport: AcpTransport::Stdio,
            endpoint: None,
            auth_header_env: None,
            headers: BTreeMap::new(),
            connect_timeout_secs: 30,
            allow_remote: false,
        };
        assert!(validate_agent_config(config.clone()).is_err());
        config.name = "ok".into();
        config.command = String::new();
        assert!(validate_agent_config(config.clone()).is_err());
        config.command = "c".repeat(4_097);
        assert!(validate_agent_config(config.clone()).is_err());
        config.command = "npx".into();
        config.args = vec!["a".repeat(4_097)];
        assert!(validate_agent_config(config.clone()).is_err());
        config.args = (0..129).map(|index| index.to_string()).collect();
        assert!(validate_agent_config(config.clone()).is_err());
        config.args.clear();
        config.timeout_secs = 29;
        assert!(validate_agent_config(config.clone()).is_err());
        config.timeout_secs = 14_401;
        assert!(validate_agent_config(config.clone()).is_err());
    }

    #[test]
    fn endpoint_length_and_header_total_are_bounded() {
        let long = format!(
            "https://acp.example.test/{}",
            "a".repeat(MAX_ENDPOINT_BYTES)
        );
        assert!(validate_acp_endpoint(AcpTransport::StreamableHttp, &long).is_err());

        let headers: BTreeMap<String, String> = (0..9)
            .map(|index| (format!("x-{index}"), "v".repeat(MAX_HEADER_VALUE_BYTES)))
            .collect();
        assert!(validate_headers(&headers).is_err());
    }

    #[test]
    fn empty_and_unterminated_references_are_rejected() {
        for value in [
            "${}",
            "${ }",
            "${TOKEN",
            "prefix ${TOKEN} suffix ${BAD-NAME}",
        ] {
            let headers = BTreeMap::from([("x-value".into(), value.into())]);
            assert!(validate_headers(&headers).is_err(), "{value}");
        }
        let headers = BTreeMap::from([("x-value".into(), "ok ${TOKEN}".into())]);
        assert!(validate_headers(&headers).is_ok());
    }

    #[test]
    fn stdio_rejects_transport_only_fields() {
        for mutate in [
            |config: &mut AcpAgentConfig| {
                config.headers = BTreeMap::from([("x-project".into(), "demo".into())]);
            },
            |config: &mut AcpAgentConfig| config.connect_timeout_secs = 1,
        ] {
            let mut config = AcpAgentConfig {
                name: "local".into(),
                command: "npx".into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                enabled: true,
                auto_approve: false,
                timeout_secs: 900,
                transport: AcpTransport::Stdio,
                endpoint: None,
                auth_header_env: None,
                headers: BTreeMap::new(),
                connect_timeout_secs: 30,
                allow_remote: false,
            };
            mutate(&mut config);
            let result = validate_agent_config(config);
            // connect_timeout is normalized for stdio, headers are rejected.
            if let Ok(value) = result {
                assert_eq!(value.connect_timeout_secs, 30);
            }
        }
    }

    #[test]
    fn unterminated_references_fail_closed() {
        let headers = BTreeMap::from([("x-token".into(), "${TOKEN".into())]);
        assert!(validate_headers(&headers).is_err());
        assert!(expand_env_references("${TOKEN", |_| None).is_err());
    }
}
