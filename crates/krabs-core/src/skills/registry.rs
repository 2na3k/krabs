use crate::config::SkillsConfig;
use crate::skills::{fs_skill::FsSkill, loader::SkillLoader};
use anyhow::Result;
use tokio::sync::RwLock;
use tracing::info;

pub struct SkillRegistry {
    config: SkillsConfig,
    skills: RwLock<Vec<FsSkill>>,
}

impl SkillRegistry {
    pub fn load(config: &SkillsConfig) -> Self {
        let initial = SkillLoader::discover(config);
        Self {
            config: config.clone(),
            skills: RwLock::new(initial),
        }
    }

    /// Re-scan skill directories and update the loaded set.
    /// Called at the top of every agent turn. Never returns Err — bad skill
    /// files are logged and skipped so the agent loop is never interrupted.
    pub async fn sync(&self) {
        let fresh = SkillLoader::discover(&self.config);
        let mut guard = self.skills.write().await;

        for s in &fresh {
            if !guard.iter().any(|e| e.name == s.name) {
                info!(skill = %s.name, "skill loaded");
            }
        }
        for s in guard.iter() {
            if !fresh.iter().any(|e| e.name == s.name) {
                info!(skill = %s.name, "skill unloaded");
            }
        }

        *guard = fresh;
    }

    /// Level 1: metadata block for system prompt injection.
    /// Returns empty string when no skills are loaded.
    pub async fn metadata_prompt(&self) -> String {
        let guard = self.skills.read().await;
        if guard.is_empty() {
            return String::new();
        }
        let lines = guard
            .iter()
            .map(|s| format!("- **{}**: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "## Available Skills\n\nCall `read_skill(name)` to load full instructions before using a skill.\n\n{}",
            lines
        )
    }

    /// Level 2: load full SKILL.md body for a named skill.
    pub async fn load_body(&self, name: &str) -> Result<Option<String>> {
        let guard = self.skills.read().await;
        match guard.iter().find(|s| s.name == name) {
            Some(skill) => Ok(Some(skill.load_body().await?)),
            None => Ok(None),
        }
    }
}
