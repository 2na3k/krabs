use async_trait::async_trait;

#[async_trait]
pub trait McpServerTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn call(&self, args: serde_json::Value) -> anyhow::Result<McpToolResult>;
}

#[derive(Debug)]
pub struct McpToolResult {
    pub content: Vec<McpContent>,
    pub is_error: bool,
}

#[derive(Debug)]
pub enum McpContent {
    Text { text: String },
}

impl McpContent {
    pub fn text(text: impl Into<String>) -> Self {
        McpContent::Text { text: text.into() }
    }
}
