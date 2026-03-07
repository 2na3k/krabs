use async_trait::async_trait;
use reqwest::{Client, Method};
use serde_json::json;
use std::sync::LazyLock;

use crate::tools::tool::{McpContent, McpServerTool, McpToolResult};

static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent("krabs-mcp/0.1")
        .build()
        .expect("failed to build reqwest client")
});

pub struct WebFetchTool;

#[async_trait]
impl McpServerTool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL. Supports GET and POST. Returns the response body as text."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "method": {
                    "type": "string",
                    "description": "HTTP method: GET or POST (default: GET)",
                    "enum": ["GET", "POST"]
                },
                "body": {
                    "type": "string",
                    "description": "Request body for POST requests"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30)"
                }
            },
            "required": ["url"]
        })
    }

    async fn call(&self, args: serde_json::Value) -> anyhow::Result<McpToolResult> {
        fetch_with_client(&CLIENT, args).await
    }
}

pub(crate) async fn fetch_with_client(
    client: &Client,
    args: serde_json::Value,
) -> anyhow::Result<McpToolResult> {
    let url = args["url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing 'url' argument"))?;

    let method = match args["method"].as_str().unwrap_or("GET") {
        "POST" => Method::POST,
        _ => Method::GET,
    };

    let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(30);

    let mut req = client
        .request(method, url)
        .timeout(std::time::Duration::from_secs(timeout_secs));

    if let Some(headers) = args["headers"].as_object() {
        for (k, v) in headers {
            if let Some(v) = v.as_str() {
                req = req.header(k.as_str(), v);
            }
        }
    }

    if let Some(body) = args["body"].as_str() {
        req = req.body(body.to_string());
    }

    let response = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Request failed: {e}"))?;

    let status = response.status();
    let is_error = status.is_client_error() || status.is_server_error();

    let body = response
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read response body: {e}"))?;

    let content = format!("HTTP {}\n\n{}", status.as_u16(), body);

    Ok(McpToolResult {
        content: vec![McpContent::text(content)],
        is_error,
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

    async fn call(args: serde_json::Value) -> anyhow::Result<McpToolResult> {
        fetch_with_client(&test_client(), args).await
    }

    #[tokio::test]
    async fn test_missing_url_returns_error() {
        let result = call(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("url"));
    }

    #[tokio::test]
    async fn test_get_200_returns_body() {
        let addr = serve_once(|_req| async {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("hello krabs-mcp"))))
        })
        .await;

        let result = call(json!({ "url": format!("http://{addr}/") }))
            .await
            .unwrap();

        assert!(!result.is_error);
        let McpContent::Text { text } = &result.content[0];
        assert!(text.contains("200"));
        assert!(text.contains("hello krabs-mcp"));
    }

    #[tokio::test]
    async fn test_get_404_sets_is_error() {
        let addr = serve_once(|_req| async {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(404)
                    .body(Full::new(Bytes::from("not found")))
                    .unwrap(),
            )
        })
        .await;

        let result = call(json!({ "url": format!("http://{addr}/missing") }))
            .await
            .unwrap();

        assert!(result.is_error);
        let McpContent::Text { text } = &result.content[0];
        assert!(text.contains("404"));
    }

    #[tokio::test]
    async fn test_post_sends_body() {
        use http_body_util::BodyExt;

        let addr = serve_once(|req| async move {
            assert_eq!(req.method(), hyper::Method::POST);
            let body_bytes = req.collect().await.unwrap().to_bytes();
            let echo = format!("echo: {}", String::from_utf8_lossy(&body_bytes));
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(echo))))
        })
        .await;

        let result = call(json!({
            "url": format!("http://{addr}/"),
            "method": "POST",
            "body": "ping"
        }))
        .await
        .unwrap();

        assert!(!result.is_error);
        let McpContent::Text { text } = &result.content[0];
        assert!(text.contains("echo: ping"));
    }

    #[tokio::test]
    async fn test_custom_headers_forwarded() {
        let addr = serve_once(|req| async move {
            let val = req
                .headers()
                .get("x-krabs-test")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
        })
        .await;

        let result = call(json!({
            "url": format!("http://{addr}/"),
            "headers": { "x-krabs-test": "sentinel" }
        }))
        .await
        .unwrap();

        assert!(!result.is_error);
        let McpContent::Text { text } = &result.content[0];
        assert!(text.contains("sentinel"));
    }

    #[tokio::test]
    async fn test_timeout_returns_err() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });

        let result = call(json!({
            "url": format!("http://{addr}/"),
            "timeout_secs": 1
        }))
        .await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Request failed"), "unexpected: {msg}");
    }
}
