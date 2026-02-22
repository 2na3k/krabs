use crate::tools::tool::ToolDef;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Skill: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn tools(&self) -> Vec<ToolDef>;
    async fn system_prompt_section(&self) -> Result<String>;
}
