use crate::skills::skill::Skill;
use crate::tools::tool::ToolDef;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FsSkill {
    pub name: String,
    pub description: String,
    pub(crate) skill_dir: PathBuf,
}

#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}

impl FsSkill {
    pub fn parse(skill_dir: &Path) -> Result<Self> {
        let skill_md = skill_dir.join("SKILL.md");
        let content = std::fs::read_to_string(&skill_md)?;
        let (name, description) = parse_frontmatter(&content)?;
        validate_name(&name)?;
        validate_description(&description)?;
        Ok(Self {
            name,
            description,
            skill_dir: skill_dir.to_path_buf(),
        })
    }

    pub async fn load_body(&self) -> Result<String> {
        let content = tokio::fs::read_to_string(self.skill_dir.join("SKILL.md")).await?;
        Ok(strip_frontmatter(&content))
    }
}

fn parse_frontmatter(content: &str) -> Result<(String, String)> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return Err(anyhow!("SKILL.md missing YAML frontmatter"));
    }
    let rest = &content[3..];
    let end = rest
        .find("\n---")
        .ok_or_else(|| anyhow!("SKILL.md frontmatter not closed with ---"))?;
    let yaml = &rest[..end];
    let fm: Frontmatter =
        serde_yaml::from_str(yaml).map_err(|e| anyhow!("invalid SKILL.md frontmatter: {}", e))?;
    Ok((fm.name, fm.description))
}

fn strip_frontmatter(content: &str) -> String {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return content.to_string();
    }
    let rest = &content[3..];
    if let Some(end) = rest.find("\n---") {
        rest[end + 4..].trim_start_matches('\n').to_string()
    } else {
        content.to_string()
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(anyhow!("skill name must be 1–64 characters"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(anyhow!("skill name must match [a-z0-9-]"));
    }
    if name.contains("anthropic") || name.contains("claude") {
        return Err(anyhow!(
            "skill name must not contain reserved words (anthropic, claude)"
        ));
    }
    Ok(())
}

fn validate_description(desc: &str) -> Result<()> {
    if desc.is_empty() {
        return Err(anyhow!("skill description must not be empty"));
    }
    if desc.len() > 1024 {
        return Err(anyhow!("skill description must be <= 1024 characters"));
    }
    Ok(())
}

#[async_trait]
impl Skill for FsSkill {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![]
    }

    async fn system_prompt_section(&self) -> Result<String> {
        self.load_body().await
    }
}
