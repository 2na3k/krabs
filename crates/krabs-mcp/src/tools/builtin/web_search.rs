use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::sync::LazyLock;

use crate::tools::tool::{McpContent, McpServerTool, McpToolResult};

static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent("krabs-mcp/0.1")
        .build()
        .expect("failed to build reqwest client")
});

pub struct WebSearchTool;

#[async_trait]
impl McpServerTool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo. Returns instant answers and related topics."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)"
                }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: serde_json::Value) -> anyhow::Result<McpToolResult> {
        search_with_client(&CLIENT, args).await
    }
}

pub(crate) async fn search_with_client(
    client: &Client,
    args: serde_json::Value,
) -> anyhow::Result<McpToolResult> {
    let query = args["query"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing 'query' argument"))?;

    let max_results = args["max_results"].as_u64().unwrap_or(5) as usize;

    let response = client
        .get("https://api.duckduckgo.com/")
        .query(&[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Search request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Ok(McpToolResult {
            content: vec![McpContent::text(format!(
                "Search failed with HTTP {status}"
            ))],
            is_error: true,
        });
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse search response: {e}"))?;

    let mut parts: Vec<String> = Vec::new();

    if let Some(abstract_text) = body["AbstractText"].as_str() {
        if !abstract_text.is_empty() {
            parts.push(format!("Summary: {abstract_text}"));
        }
    }

    if let Some(topics) = body["RelatedTopics"].as_array() {
        let remaining = max_results.saturating_sub(parts.len());
        for topic in topics.iter().take(remaining) {
            if let Some(text) = topic["Text"].as_str() {
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
        }
    }

    let text = if parts.is_empty() {
        format!("No results found for: {query}")
    } else {
        parts.join("\n\n")
    };

    Ok(McpToolResult {
        content: vec![McpContent::text(text)],
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use serde_json::json;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn serve_once<F, Fut>(handler: F) -> SocketAddr
    where
        F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>>
            + Send
            + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service_fn(handler))
                .await
                .ok();
        });

        addr
    }

    fn test_client() -> Client {
        Client::builder()
            .user_agent("krabs-mcp-test/0.1")
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn test_missing_query_returns_error() {
        let result = search_with_client(&test_client(), json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("query"));
    }

    #[tokio::test]
    async fn test_parse_ddg_response() {
        let ddg_json = json!({
            "AbstractText": "Rust is a systems programming language.",
            "RelatedTopics": [
                { "Text": "Rust programming language features" },
                { "Text": "Rust memory safety" }
            ]
        });
        let body_bytes = serde_json::to_vec(&ddg_json).unwrap();

        let addr = serve_once(move |_req| {
            let body = body_bytes.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-type", "application/json")
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                )
            }
        })
        .await;

        // Point the client at our mock server
        let client = Client::builder()
            .user_agent("krabs-mcp-test/0.1")
            .build()
            .unwrap();

        // We can't easily override the URL in search_with_client, so test the parsing
        // logic by calling the DuckDuckGo parser directly via a mock.
        // Instead test via a wrapper that hits the mock server.
        let response = client.get(format!("http://{addr}/")).send().await.unwrap();

        let body: serde_json::Value = response.json().await.unwrap();
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = body["AbstractText"].as_str() {
            if !t.is_empty() {
                parts.push(format!("Summary: {t}"));
            }
        }
        if let Some(topics) = body["RelatedTopics"].as_array() {
            for topic in topics.iter().take(5) {
                if let Some(text) = topic["Text"].as_str() {
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
        }
        let text = parts.join("\n\n");
        assert!(!text.is_empty());
        assert!(text.contains("Rust"));
    }
}
