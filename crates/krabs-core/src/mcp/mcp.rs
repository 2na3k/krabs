pub struct McpClient {
    _server_url: String,
}

impl McpClient {
    pub fn new(server_url: impl Into<String>) -> Self {
        Self {
            _server_url: server_url.into(),
        }
    }
}
