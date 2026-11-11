//! Provider-backed web search.
//!
//! Search is never hidden model routing: the user configures a provider and
//! credentials through the daemon environment. The tool returns normalized
//! results; the agent then fetches chosen URLs with `fetch_url`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::{AgentTool, ToolContext, ToolOutput};

const CACHE_TTL: Duration = Duration::from_secs(300);
const MAX_CACHE_ENTRIES: usize = 64;
const MAX_RESULTS: usize = 20;

pub struct WebSearch;

#[derive(Debug, Clone)]
struct SearchConfig {
    provider: String,
    endpoint: Option<String>,
    api_key: Option<String>,
    language: String,
    safe: bool,
    timeout: Duration,
}

impl SearchConfig {
    fn from_env() -> Result<Self> {
        let provider = std::env::var("VERLINPARK_SEARCH_PROVIDER")
            .unwrap_or_else(|_| "searxng".to_string())
            .trim()
            .to_ascii_lowercase();
        if !matches!(provider.as_str(), "searxng" | "brave" | "tavily") {
            bail!(
                "unsupported VERLINPARK_SEARCH_PROVIDER '{provider}'; use searxng, brave, or tavily"
            );
        }
        let endpoint = std::env::var("VERLINPARK_SEARCH_ENDPOINT")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());
        let api_key = std::env::var("VERLINPARK_SEARCH_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if provider == "searxng" && endpoint.is_none() {
            bail!("SearXNG search requires VERLINPARK_SEARCH_ENDPOINT");
        }
        if matches!(provider.as_str(), "brave" | "tavily") && api_key.is_none() {
            bail!("{provider} search requires VERLINPARK_SEARCH_API_KEY");
        }
        let language =
            std::env::var("VERLINPARK_SEARCH_LANGUAGE").unwrap_or_else(|_| "en".to_string());
        let safe = std::env::var("VERLINPARK_SEARCH_SAFE")
            .map(|value| {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off"
                )
            })
            .unwrap_or(true);
        let timeout = std::env::var("VERLINPARK_SEARCH_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(20)
            .clamp(1, 120);
        Ok(Self {
            provider,
            endpoint,
            api_key,
            language,
            safe,
            timeout: Duration::from_secs(timeout),
        })
    }
}

#[derive(Clone)]
struct CachedSearch {
    at: Instant,
    results: Vec<Value>,
}

static SEARCH_CACHE: OnceLock<Mutex<HashMap<String, CachedSearch>>> = OnceLock::new();

fn cache_key(
    config: &SearchConfig,
    query: &str,
    site: Option<&str>,
    recency: &str,
    limit: usize,
) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        config.provider,
        query,
        site.unwrap_or_default(),
        recency,
        limit
    )
}

#[async_trait]
impl AgentTool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web through the configured search provider. Returns titles, URLs, snippets, sources, and ranks; use fetch_url to read a result."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query, max 500 characters"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20, "description": "Default 8"},
                "site": {"type": "string", "description": "Optional site restriction such as docs.rs"},
                "recency": {"type": "string", "enum": ["any", "day", "week", "month", "year"], "description": "Default any"}
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        if !context.network_allowed {
            return Ok(ToolOutput::error_structured(
                "network access is denied by the project shell policy",
                json!({"code": "NETWORK_DENIED"}),
            ));
        }
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| anyhow!("query is required"))?;
        if query.len() > 500 {
            return Ok(ToolOutput::error_structured(
                "query exceeds 500 characters",
                json!({"code": "INVALID_ARGUMENT"}),
            ));
        }
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(8)
            .clamp(1, MAX_RESULTS as u64) as usize;
        let site = input
            .get("site")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|site| !site.is_empty());
        let recency = input
            .get("recency")
            .and_then(Value::as_str)
            .unwrap_or("any");
        if !matches!(recency, "any" | "day" | "week" | "month" | "year") {
            return Ok(ToolOutput::error_structured(
                format!("unsupported recency: {recency}"),
                json!({"code": "INVALID_ARGUMENT", "recency": recency}),
            ));
        }
        let config = match SearchConfig::from_env() {
            Ok(config) => config,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    error.to_string(),
                    json!({"code": "WEB_SEARCH_UNAVAILABLE", "provider": "unconfigured"}),
                ));
            }
        };
        let full_query = if let Some(site) = site {
            format!("site:{site} {query}")
        } else {
            query.to_string()
        };
        let key = cache_key(&config, &full_query, site, recency, limit);
        if let Some(cached) = SEARCH_CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).cloned())
            && cached.at.elapsed() < CACHE_TTL
        {
            return Ok(ToolOutput::ok_structured(
                render_results(&cached.results, &config.provider, true),
                json!({
                    "provider": config.provider,
                    "query": full_query,
                    "results": cached.results,
                    "count": cached.results.len(),
                    "cached": true,
                }),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent("VerlinPark/0.1 web search")
            .build()
            .map_err(|error| anyhow!("building search client: {error}"))?;
        let results = match config.provider.as_str() {
            "brave" => search_brave(&client, &config, &full_query, limit, recency).await,
            "tavily" => search_tavily(&client, &config, &full_query, limit, recency).await,
            "searxng" => search_searxng(&client, &config, &full_query, limit, recency).await,
            other => Err(anyhow!("unsupported search provider: {other}")),
        };
        let results = match results {
            Ok(results) => results,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    format!("web search failed: {error}"),
                    json!({"code": "WEB_SEARCH_FAILED", "provider": config.provider}),
                ));
            }
        };
        if let Ok(mut cache) = SEARCH_CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
        {
            if cache.len() >= MAX_CACHE_ENTRIES {
                cache.clear();
            }
            cache.insert(
                key,
                CachedSearch {
                    at: Instant::now(),
                    results: results.clone(),
                },
            );
        }
        Ok(ToolOutput::ok_structured(
            render_results(&results, &config.provider, false),
            json!({
                "provider": config.provider,
                "query": full_query,
                "results": results,
                "count": results.len(),
                "cached": false,
            }),
        ))
    }
}

fn render_results(results: &[Value], provider: &str, cached: bool) -> String {
    let mut text = format!(
        "{} result(s) via {}{}\\n",
        results.len(),
        provider,
        if cached { " (cached)" } else { "" }
    );
    for (index, result) in results.iter().enumerate() {
        text.push_str(&format!(
            "{}. {} — {}\n   {}\n",
            index + 1,
            result["title"].as_str().unwrap_or("untitled"),
            result["url"].as_str().unwrap_or(""),
            result["snippet"].as_str().unwrap_or("")
        ));
    }
    text
}

async fn search_searxng(
    client: &reqwest::Client,
    config: &SearchConfig,
    query: &str,
    limit: usize,
    recency: &str,
) -> Result<Vec<Value>> {
    let endpoint = config
        .endpoint
        .as_deref()
        .ok_or_else(|| anyhow!("SearXNG endpoint is not configured"))?;
    let mut url = reqwest::Url::parse(endpoint).context("invalid SearXNG endpoint")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("SearXNG endpoint must use http or https");
    }
    if url.username() != "" || url.password().is_some() {
        bail!("SearXNG endpoint must not contain credentials");
    }
    url.path_segments_mut()
        .map_err(|_| anyhow!("SearXNG endpoint cannot accept a path"))?
        .pop_if_empty()
        .push("search");
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query);
        pairs.append_pair("format", "json");
        pairs.append_pair("language", &config.language);
        pairs.append_pair("safesearch", if config.safe { "1" } else { "0" });
        if let Some(time_range) = searxng_time_range(recency) {
            pairs.append_pair("time_range", time_range);
        }
    }
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| anyhow!("SearXNG request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("SearXNG returned HTTP {}", status.as_u16());
    }
    let value: Value = response
        .json()
        .await
        .context("SearXNG returned invalid JSON")?;
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .take(limit)
                .enumerate()
                .filter_map(|(index, result)| {
                    normalize_result(
                        index,
                        result.get("title").and_then(Value::as_str),
                        result.get("url").and_then(Value::as_str),
                        result.get("content").and_then(Value::as_str),
                        result.get("engine").and_then(Value::as_str),
                        result.get("publishedDate").and_then(Value::as_str),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(results)
}

async fn search_brave(
    client: &reqwest::Client,
    config: &SearchConfig,
    query: &str,
    limit: usize,
    recency: &str,
) -> Result<Vec<Value>> {
    let endpoint = config
        .endpoint
        .as_deref()
        .unwrap_or("https://api.search.brave.com/res/v1/web/search");
    let mut url = reqwest::Url::parse(endpoint).context("invalid Brave endpoint")?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query);
        pairs.append_pair("count", &limit.to_string());
        pairs.append_pair("search_lang", &config.language);
        pairs.append_pair("safesearch", if config.safe { "strict" } else { "off" });
        if let Some(freshness) = brave_freshness(recency) {
            pairs.append_pair("freshness", freshness);
        }
    }
    let response = client
        .get(url)
        .header(
            "X-Subscription-Token",
            config.api_key.as_deref().unwrap_or_default(),
        )
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|error| anyhow!("Brave request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("Brave returned HTTP {}", status.as_u16());
    }
    let value: Value = response
        .json()
        .await
        .context("Brave returned invalid JSON")?;
    let results = value
        .get("web")
        .and_then(|web| web.get("results"))
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .take(limit)
                .enumerate()
                .filter_map(|(index, result)| {
                    normalize_result(
                        index,
                        result.get("title").and_then(Value::as_str),
                        result.get("url").and_then(Value::as_str),
                        result.get("description").and_then(Value::as_str),
                        Some("brave"),
                        result.get("age").and_then(Value::as_str),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(results)
}

async fn search_tavily(
    client: &reqwest::Client,
    config: &SearchConfig,
    query: &str,
    limit: usize,
    recency: &str,
) -> Result<Vec<Value>> {
    let endpoint = config
        .endpoint
        .as_deref()
        .unwrap_or("https://api.tavily.com/search");
    let mut body = json!({
        "api_key": config.api_key.as_deref().unwrap_or_default(),
        "query": query,
        "max_results": limit,
        "search_depth": "basic",
        "include_answer": false,
        "include_raw_content": false,
    });
    if let Some(days) = tavily_days(recency) {
        body["days"] = json!(days);
    }
    let response = client
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|error| anyhow!("Tavily request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("Tavily returned HTTP {}", status.as_u16());
    }
    let value: Value = response
        .json()
        .await
        .context("Tavily returned invalid JSON")?;
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .take(limit)
                .enumerate()
                .filter_map(|(index, result)| {
                    normalize_result(
                        index,
                        result.get("title").and_then(Value::as_str),
                        result.get("url").and_then(Value::as_str),
                        result.get("content").and_then(Value::as_str),
                        Some("tavily"),
                        result.get("published_date").and_then(Value::as_str),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(results)
}

fn normalize_result(
    index: usize,
    title: Option<&str>,
    url: Option<&str>,
    snippet: Option<&str>,
    source: Option<&str>,
    published: Option<&str>,
) -> Option<Value> {
    let url = url?.trim();
    let parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let title = crate::sources::html_to_text(title.unwrap_or(url));
    let snippet = crate::sources::html_to_text(snippet.unwrap_or(""));
    Some(json!({
        "rank": index + 1,
        "title": title.chars().take(300).collect::<String>(),
        "url": url,
        "snippet": snippet.chars().take(800).collect::<String>(),
        "source": source.unwrap_or("search"),
        "published": published,
        "citation_id": format!("web{}", index + 1),
    }))
}

fn searxng_time_range(recency: &str) -> Option<&'static str> {
    match recency {
        "day" => Some("day"),
        "week" => Some("week"),
        "month" => Some("month"),
        "year" => Some("year"),
        _ => None,
    }
}

fn brave_freshness(recency: &str) -> Option<&'static str> {
    match recency {
        "day" => Some("pd"),
        "week" => Some("pw"),
        "month" => Some("pm"),
        "year" => Some("py"),
        _ => None,
    }
}

fn tavily_days(recency: &str) -> Option<u32> {
    match recency {
        "day" => Some(1),
        "week" => Some(7),
        "month" => Some(30),
        "year" => Some(365),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;
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
        }
    }

    #[tokio::test]
    async fn parses_searxng_results() {
        let app = Router::new().route(
            "/search",
            get(|| async {
                axum::Json(json!({
                    "results": [
                        {"title": "Rust docs", "url": "https://doc.rust-lang.org/", "content": "Rust language docs", "engine": "duckduckgo"},
                        {"title": "Bad", "url": "javascript:alert(1)", "content": "drop me"}
                    ]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let config = SearchConfig {
            provider: "searxng".into(),
            endpoint: Some(format!("http://{address}")),
            api_key: None,
            language: "en".into(),
            safe: true,
            timeout: Duration::from_secs(5),
        };
        let results = search_searxng(&client, &config, "rust", 5, "any")
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "Rust docs");
        assert_eq!(results[0]["citation_id"], "web1");
        server.abort();
    }

    #[test]
    fn normalizes_result_fields() {
        let result = normalize_result(
            0,
            Some("Example"),
            Some("https://example.com/a"),
            Some("Snippet"),
            Some("test"),
            None,
        )
        .unwrap();
        assert_eq!(result["rank"], 1);
        assert_eq!(result["url"], "https://example.com/a");
        assert!(normalize_result(0, None, Some("file:///etc/passwd"), None, None, None).is_none());
    }

    #[tokio::test]
    async fn rejects_unconfigured_or_disabled_network() {
        let dir = TempDir::new().unwrap();
        let mut context = context(&dir);
        context.network_allowed = false;
        let output = WebSearch
            .execute(&context, json!({"query": "rust"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("network access"));
    }
    #[tokio::test]
    async fn parses_brave_and_tavily_results() {
        use axum::routing::{get, post};
        let app = axum::Router::new()
            .route(
                "/brave",
                get(|| async {
                    axum::Json(json!({"web": {"results": [
                        {"title": "Brave result", "url": "https://example.com/brave", "description": "from brave", "age": "1 day"}
                    ]}}))
                }),
            )
            .route(
                "/tavily",
                post(|| async {
                    axum::Json(json!({"results": [
                        {"title": "Tavily result", "url": "https://example.com/tavily", "content": "from tavily", "published_date": "2026-11-10"}
                    ]}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let brave = search_brave(
            &client,
            &SearchConfig {
                provider: "brave".into(),
                endpoint: Some(format!("http://{address}/brave")),
                api_key: Some("test".into()),
                language: "en".into(),
                safe: true,
                timeout: Duration::from_secs(5),
            },
            "rust",
            5,
            "day",
        )
        .await
        .unwrap();
        assert_eq!(brave[0]["title"], "Brave result");
        let tavily = search_tavily(
            &client,
            &SearchConfig {
                provider: "tavily".into(),
                endpoint: Some(format!("http://{address}/tavily")),
                api_key: Some("test".into()),
                language: "en".into(),
                safe: true,
                timeout: Duration::from_secs(5),
            },
            "rust",
            5,
            "month",
        )
        .await
        .unwrap();
        assert_eq!(tavily[0]["title"], "Tavily result");
        server.abort();
    }
}
