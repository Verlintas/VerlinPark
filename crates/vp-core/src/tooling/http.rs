//! Structured HTTP requests to local services.
//!
//! Agents use this instead of composing `curl` pipelines when testing a local
//! API. External hosts are rejected; redirects stay local and are revalidated
//! on every hop. Secret-bearing request headers are sent but never echoed.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::tools::{AgentTool, ToolContext, ToolOutput, truncate};

const MAX_BODY_BYTES: usize = 512 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const MAX_HEADERS: usize = 32;
const MAX_REDIRECTS: usize = 5;

pub struct LocalHttp;

#[async_trait]
impl AgentTool for LocalHttp {
    fn name(&self) -> &'static str {
        "local_http"
    }

    fn description(&self) -> &'static str {
        "Send a structured HTTP request to a localhost service. Use for local API and frontend testing instead of shell curl."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "method": {"type": "string", "enum": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"], "description": "Default GET"},
                "url": {"type": "string", "description": "localhost, 127.0.0.1, or [::1] http(s) URL"},
                "headers": {"type": "object", "description": "Explicit request headers; max 32"},
                "body": {"type": "string", "description": "Raw request body"},
                "json": {"description": "JSON request body; sets application/json"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 120, "description": "Default 30"},
                "follow_redirects": {"type": "boolean", "description": "Follow up to 5 local redirects; default false"}
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, _context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let request_id = format!("http_{}", &Uuid::now_v7().simple().to_string()[..12]);
        let method = input
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let method = match method.as_str() {
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" => {
                reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|error| anyhow!("invalid method: {error}"))?
            }
            other => {
                return Ok(ToolOutput::error_structured(
                    format!("unsupported HTTP method: {other}"),
                    json!({"code": "INVALID_ARGUMENT", "method": other}),
                ));
            }
        };
        let raw_url = input
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| anyhow!("url is required"))?;
        let timeout_secs = input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 120);
        let follow_redirects = input
            .get("follow_redirects")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut headers = BTreeMap::new();
        if let Some(values) = input.get("headers").and_then(Value::as_object) {
            if values.len() > MAX_HEADERS {
                return Ok(ToolOutput::error_structured(
                    format!("headers accept at most {MAX_HEADERS} entries"),
                    json!({"code": "INVALID_ARGUMENT", "headers": values.len()}),
                ));
            }
            for (name, value) in values {
                let Some(value) = value.as_str() else {
                    return Ok(ToolOutput::error_structured(
                        format!("header {name} must be a string"),
                        json!({"code": "INVALID_ARGUMENT", "header": name}),
                    ));
                };
                let lower = name.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "host" | "content-length" | "connection" | "transfer-encoding"
                ) {
                    return Ok(ToolOutput::error_structured(
                        format!("header {name} is managed by the runtime"),
                        json!({"code": "INVALID_ARGUMENT", "header": name}),
                    ));
                }
                if value.contains('\r') || value.contains('\n') || value.len() > 8_192 {
                    return Ok(ToolOutput::error_structured(
                        format!("header {name} contains invalid characters"),
                        json!({"code": "INVALID_ARGUMENT", "header": name}),
                    ));
                }
                headers.insert(lower, value.to_string());
            }
        }
        let mut body: Option<Vec<u8>> = None;
        if let Some(json_body) = input.get("json") {
            let serialized = serde_json::to_vec(json_body)?;
            if serialized.len() > MAX_REQUEST_BODY_BYTES {
                return Ok(ToolOutput::error_structured(
                    format!("JSON body exceeds {MAX_REQUEST_BODY_BYTES} bytes"),
                    json!({"code": "INVALID_ARGUMENT"}),
                ));
            }
            headers
                .entry("content-type".to_string())
                .or_insert_with(|| "application/json".to_string());
            body = Some(serialized);
        } else if let Some(text) = input.get("body").and_then(Value::as_str) {
            if text.len() > MAX_REQUEST_BODY_BYTES {
                return Ok(ToolOutput::error_structured(
                    format!("request body exceeds {MAX_REQUEST_BODY_BYTES} bytes"),
                    json!({"code": "INVALID_ARGUMENT"}),
                ));
            }
            body = Some(text.as_bytes().to_vec());
        }
        if method == reqwest::Method::HEAD || method == reqwest::Method::GET {
            body = None;
        }

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|error| anyhow!("building HTTP client: {error}"))?;
        let started = Instant::now();
        let mut current = raw_url.to_string();
        let mut redirects = Vec::new();
        let mut response = None;
        for hop in 0..=MAX_REDIRECTS {
            if !is_local_url(&current) {
                return Ok(ToolOutput::error_structured(
                    "local_http only allows localhost, 127.0.0.1, or [::1]",
                    json!({"code": "NETWORK_DENIED", "url": current, "request_id": request_id}),
                ));
            }
            let mut request = client.request(method.clone(), &current);
            for (name, value) in &headers {
                request = request.header(name, value);
            }
            if let Some(body) = &body {
                request = request.body(body.clone());
            }
            let sent = match request.send().await {
                Ok(sent) => sent,
                Err(error) => {
                    return Ok(ToolOutput::error_structured(
                        format!("{method} {current} failed: {error}"),
                        json!({
                            "code": "HTTP_ERROR",
                            "request_id": request_id,
                            "method": method.as_str(),
                            "url": current,
                            "elapsed_ms": started.elapsed().as_millis() as u64,
                        }),
                    ));
                }
            };
            if follow_redirects && sent.status().is_redirection() {
                let Some(location) = sent
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                else {
                    response = Some(sent);
                    break;
                };
                let next = reqwest::Url::parse(&current)
                    .and_then(|base| base.join(location))
                    .map(|url| url.to_string());
                let Ok(next) = next else {
                    return Ok(ToolOutput::error_structured(
                        "redirect location is not a valid URL",
                        json!({"code": "HTTP_ERROR", "request_id": request_id, "location": location}),
                    ));
                };
                if !is_local_url(&next) {
                    return Ok(ToolOutput::error_structured(
                        "redirect left localhost; local_http refuses to follow it",
                        json!({"code": "NETWORK_DENIED", "request_id": request_id, "url": next}),
                    ));
                }
                if hop >= MAX_REDIRECTS {
                    return Ok(ToolOutput::error_structured(
                        format!("too many redirects (max {MAX_REDIRECTS})"),
                        json!({"code": "HTTP_ERROR", "request_id": request_id}),
                    ));
                }
                redirects.push(json!({
                    "status": sent.status().as_u16(),
                    "from": current,
                    "to": next,
                }));
                current = next;
                continue;
            }
            response = Some(sent);
            break;
        }
        let Some(response) = response else {
            return Ok(ToolOutput::error_structured(
                "request produced no response",
                json!({"code": "HTTP_ERROR", "request_id": request_id}),
            ));
        };
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let selected_headers = selected_headers(&response, &headers);
        let mut bytes = Vec::new();
        let mut truncated = false;
        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() >= MAX_BODY_BYTES {
                truncated = true;
                break;
            }
        }
        let text = String::from_utf8_lossy(&bytes).to_string();
        let trimmed_body = text.trim_start();
        let parsed_json = if content_type.contains("json")
            || trimmed_body.starts_with('{')
            || trimmed_body.starts_with('[')
        {
            serde_json::from_slice::<Value>(&bytes).ok()
        } else {
            None
        };
        let body_text = truncate(text);
        let summary = format!(
            "{} {} -> {} ({}){}",
            method.as_str(),
            current,
            status.as_u16(),
            content_type,
            if truncated { " · truncated" } else { "" }
        );
        Ok(ToolOutput::ok_structured(
            truncate(format!("{summary}\n{body_text}")),
            json!({
                "ok": status.is_success() || status.is_redirection(),
                "request_id": request_id,
                "method": method.as_str(),
                "url": current,
                "status": status.as_u16(),
                "content_type": content_type,
                "headers": selected_headers,
                "body": body_text,
                "json": parsed_json,
                "truncated": truncated,
                "redirects": redirects,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "next_actions": if status.is_success() {
                    Vec::<String>::new()
                } else {
                    vec!["inspect the response body and server process output".to_string()]
                },
            }),
        ))
    }
}

fn selected_headers(
    response: &reqwest::Response,
    request_headers: &BTreeMap<String, String>,
) -> Value {
    let names = [
        reqwest::header::CONTENT_TYPE,
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::LOCATION,
        reqwest::header::CACHE_CONTROL,
        reqwest::header::ETAG,
        reqwest::header::LAST_MODIFIED,
    ];
    let mut headers = serde_json::Map::new();
    for name in names {
        if let Some(value) = response.headers().get(&name)
            && let Ok(value) = value.to_str()
        {
            headers.insert(
                name.as_str().to_string(),
                Value::String(value.chars().take(1_000).collect()),
            );
        }
    }
    if !request_headers.is_empty() {
        let names = request_headers
            .keys()
            .filter(|name| !is_secret_header(name))
            .cloned()
            .collect::<Vec<_>>();
        headers.insert(
            "x-request-headers".to_string(),
            Value::Array(names.into_iter().map(Value::String).collect()),
        );
    }
    Value::Object(headers)
}

fn is_secret_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("authorization")
        || name.contains("cookie")
        || name.contains("token")
        || name.contains("secret")
        || name.contains("api-key")
        || name.contains("apikey")
}

fn is_local_url(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    if url.username() != "" || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    matches!(
        host.trim_matches(['[', ']']).to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use axum::Router;
    use axum::routing::{get, post};
    use tempfile::TempDir;

    fn context(root: &TempDir) -> ToolContext {
        ToolContext {
            root: root.path().to_path_buf(),
            project_root: root.path().to_path_buf(),
            sandbox_root: root.path().join(".sandbox"),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: root.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: None,
            process_supervisor: None,
            test_cache: None,
        }
    }

    #[tokio::test]
    async fn performs_local_json_requests() {
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/echo", post(|body: String| async move { body }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let get_response = LocalHttp
            .execute(
                &context,
                json!({"method": "GET", "url": format!("http://{address}/health")}),
            )
            .await
            .unwrap();
        assert!(!get_response.is_error, "{}", get_response.content);
        assert_eq!(get_response.structured.unwrap()["status"], 200);

        let post_response = LocalHttp
            .execute(
                &context,
                json!({
                    "method": "POST",
                    "url": format!("http://{address}/echo"),
                    "json": {"hello": "world"},
                    "headers": {"authorization": "Bearer secret"}
                }),
            )
            .await
            .unwrap();
        assert!(!post_response.is_error, "{}", post_response.content);
        let structured = post_response.structured.unwrap();
        assert_eq!(structured["json"]["hello"], "world");
        assert!(
            !structured["headers"]
                .to_string()
                .to_ascii_lowercase()
                .contains("secret")
        );
        server.abort();
    }

    #[tokio::test]
    async fn rejects_external_hosts() {
        let dir = TempDir::new().unwrap();
        let output = LocalHttp
            .execute(&context(&dir), json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("localhost"));
    }

    #[test]
    fn validates_local_urls() {
        assert!(is_local_url("http://127.0.0.1:3000"));
        assert!(is_local_url("http://localhost:8080/path"));
        assert!(is_local_url("http://[::1]:8080"));
        assert!(!is_local_url("https://example.com"));
        assert!(!is_local_url("file:///etc/passwd"));
        assert!(!is_local_url("http://user:pass@localhost:8080"));
    }
}
