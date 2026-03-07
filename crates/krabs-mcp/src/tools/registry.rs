use std::collections::HashMap;
use std::sync::Arc;

use crate::protocol::types::ToolInfo;
use crate::tools::tool::McpServerTool;

pub struct McpToolRegistry {
    tools: HashMap<String, Arc<dyn McpServerTool>>,
}

impl McpToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn McpServerTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn McpServerTool>> {
        self.tools.get(name).cloned()
    }

    pub fn tool_infos(&self) -> Vec<ToolInfo> {
        let mut infos: Vec<ToolInfo> = self
            .tools
            .values()
            .map(|t| ToolInfo {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        infos
    }
}

impl Default for McpToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
