use crate::skills::registry::SkillRegistry;
use crate::tools::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub struct ReadSkillTool {
    registry: Arc<SkillRegistry>,
}

impl ReadSkillTool {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn description(&self) -> &str {
        "Load the full instructions for an available skill by name. \
         Call this before using a skill to get its complete guidance."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "The name of the skill to load"
                }
            },
            "required": ["skill_name"]
        })
    }

    async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
        let name = match args["skill_name"].as_str() {
            Some(n) => n,
            None => return Ok(ToolResult::err("missing required argument: skill_name")),
        };

        match self.registry.load_body(name).await {
            Ok(Some(body)) => Ok(ToolResult::ok(body)),
            Ok(None) => Ok(ToolResult::err(format!("skill '{}' not found", name))),
            Err(e) => Ok(ToolResult::err(format!(
                "failed to load skill '{}': {}",
                name, e
            ))),
        }
    }
}
