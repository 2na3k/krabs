use super::tool::{Tool, ToolDef};
use crate::config::KrabsConfig;
use crate::permissions::PermissionGuard;
use crate::providers::provider::LlmProvider;
use std::collections::HashMap;
use std::sync::Arc;

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn tool_defs(&self) -> Vec<ToolDef> {
        let mut defs: Vec<ToolDef> = self
            .tools
            .values()
            .map(|t| ToolDef {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    /// Standard Krabs tool set: bash, read, write, glob, grep, web_fetch.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(crate::tools::bash::BashTool));
        r.register(Arc::new(crate::tools::read::ReadTool));
        r.register(Arc::new(crate::tools::write::WriteTool));
        r.register(Arc::new(crate::tools::glob::GlobTool));
        r.register(Arc::new(crate::tools::glob::GrepTool));
        r.register(Arc::new(crate::tools::web_fetch::WebFetchTool));
        r
    }

    /// Add the delegate + dispatch orchestration tools.
    ///
    /// These require config, provider, and a clone of the current registry,
    /// so they must be added after the base tools are registered.
    pub fn with_orchestration(&mut self, config: &KrabsConfig, provider: &Arc<dyn LlmProvider>) {
        self.register(Arc::new(crate::tools::delegate::DelegateTool::new(
            config.clone(),
            Arc::clone(provider),
            self.clone(),
            PermissionGuard::new(),
        )));
        self.register(Arc::new(crate::tools::dispatch::DispatchTool::new(
            config.clone(),
            Arc::clone(provider),
            self.clone(),
            PermissionGuard::new(),
        )));
    }
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        Self {
            tools: self.tools.clone(),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
