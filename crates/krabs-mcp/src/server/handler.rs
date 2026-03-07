use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::protocol::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use crate::protocol::types::{InitializeResult, ServerCapabilities, ServerInfo, ToolsCapability};
use crate::tools::registry::McpToolRegistry;
use crate::tools::tool::McpContent;

pub async fn dispatch(
    registry: &RwLock<McpToolRegistry>,
    server_name: &str,
    server_version: &str,
    req: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    let id = req.id;

    if req.method == "initialized" {
        return None;
    }

    let response = match req.method.as_str() {
        "initialize" => {
            let result = InitializeResult {
                protocol_version: "2024-11-05",
                server_info: ServerInfo {
                    name: server_name.to_string(),
                    version: server_version.to_string(),
                },
                capabilities: ServerCapabilities {
                    tools: Some(ToolsCapability { list_changed: true }),
                },
            };
            match serde_json::to_value(result) {
                Ok(v) => JsonRpcResponse::ok(id, v),
                Err(e) => JsonRpcResponse::err(id, -32603, format!("Internal error: {e}")),
            }
        }

        "tools/list" => {
            let tools = registry.read().await.tool_infos();
            match serde_json::to_value(&tools) {
                Ok(v) => JsonRpcResponse::ok(id, json!({ "tools": v })),
                Err(e) => JsonRpcResponse::err(id, -32603, format!("Internal error: {e}")),
            }
        }

        "tools/call" => {
            let params = req.params.unwrap_or(Value::Null);
            let tool_name = params["name"].as_str().unwrap_or("").to_string();
            let arguments = params["arguments"].clone();

            // Acquire read lock only to clone the Arc, then release before calling.
            let tool = registry.read().await.get(&tool_name);

            match tool {
                None => JsonRpcResponse::err(id, -32602, format!("unknown tool: {tool_name}")),
                Some(tool) => match tool.call(arguments).await {
                    Err(e) => JsonRpcResponse::err(id, -32603, format!("Tool error: {e}")),
                    Ok(result) => {
                        let content: Vec<Value> = result
                            .content
                            .into_iter()
                            .map(|c| match c {
                                McpContent::Text { text } => {
                                    json!({ "type": "text", "text": text })
                                }
                            })
                            .collect();
                        JsonRpcResponse::ok(
                            id,
                            json!({
                                "content": content,
                                "isError": result.is_error
                            }),
                        )
                    }
                },
            }
        }

        "ping" => JsonRpcResponse::ok(id, json!({})),

        _ => JsonRpcResponse::err(id, -32601, "Method not found"),
    };

    Some(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::tools::builtin::echo::EchoTool;
    use crate::tools::registry::McpToolRegistry;

    fn make_req(method: &str, id: Option<u64>, params: Option<Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }

    fn empty_registry() -> RwLock<McpToolRegistry> {
        RwLock::new(McpToolRegistry::new())
    }

    fn registry_with_echo() -> RwLock<McpToolRegistry> {
        let mut r = McpToolRegistry::new();
        r.register(Arc::new(EchoTool));
        RwLock::new(r)
    }

    #[tokio::test]
    async fn test_initialize_returns_protocol_version() {
        let registry = empty_registry();
        let req = make_req(
            "initialize",
            Some(1),
            Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            })),
        );
        let resp = dispatch(&registry, "test-server", "0.1.0", req)
            .await
            .unwrap();
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }

    #[tokio::test]
    async fn test_initialized_notification_returns_none() {
        let registry = empty_registry();
        let req = make_req("initialized", None, None);
        let resp = dispatch(&registry, "test-server", "0.1.0", req).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn test_tools_list_returns_registered_tools() {
        let registry = registry_with_echo();
        let req = make_req("tools/list", Some(2), None);
        let resp = dispatch(&registry, "test-server", "0.1.0", req)
            .await
            .unwrap();
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "echo"));
    }

    #[tokio::test]
    async fn test_tools_call_echo() {
        let registry = registry_with_echo();
        let req = make_req(
            "tools/call",
            Some(3),
            Some(json!({ "name": "echo", "arguments": { "x": 1 } })),
        );
        let resp = dispatch(&registry, "test-server", "0.1.0", req)
            .await
            .unwrap();
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let content = result["content"].as_array().unwrap();
        assert!(!content.is_empty());
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.contains("x"));
    }

    #[tokio::test]
    async fn test_tools_call_unknown_tool() {
        let registry = empty_registry();
        let req = make_req(
            "tools/call",
            Some(4),
            Some(json!({ "name": "does_not_exist", "arguments": {} })),
        );
        let resp = dispatch(&registry, "test-server", "0.1.0", req)
            .await
            .unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn test_unknown_method_returns_method_not_found() {
        let registry = empty_registry();
        let req = make_req("bogus/method", Some(5), None);
        let resp = dispatch(&registry, "test-server", "0.1.0", req)
            .await
            .unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_ping_returns_empty_object() {
        let registry = empty_registry();
        let req = make_req("ping", Some(6), None);
        let resp = dispatch(&registry, "test-server", "0.1.0", req)
            .await
            .unwrap();
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap(), json!({}));
    }
}
