use crate::config::SkillsConfig;
use crate::skills::fs_skill::FsSkill;
use std::path::Path;
use tracing::warn;

pub struct SkillLoader;

impl SkillLoader {
    /// Discover all valid skills across all configured paths.
    /// Invalid skill directories are logged and skipped — never fatal.
    pub fn discover(config: &SkillsConfig) -> Vec<FsSkill> {
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut skills = Vec::new();

        for path in &config.paths {
            let dir = if path.is_absolute() {
                path.clone()
            } else {
                cwd.join(path)
            };

            match Self::scan_dir(&dir, config) {
                Ok(found) => skills.extend(found),
                Err(e) => warn!("Failed to scan skill directory {:?}: {}", dir, e),
            }
        }

        skills
    }

    fn scan_dir(dir: &Path, config: &SkillsConfig) -> std::io::Result<Vec<FsSkill>> {
        if !dir.exists() {
            return Ok(vec![]);
        }

        let mut skills = Vec::new();

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            if !path.join("SKILL.md").exists() {
                continue;
            }

            match FsSkill::parse(&path) {
                Ok(skill) => {
                    let allowed = config.enabled.is_empty() || config.enabled.contains(&skill.name);
                    if allowed {
                        skills.push(skill);
                    }
                }
                Err(e) => warn!("Skipping skill at {:?}: {}", path, e),
            }
        }

        Ok(skills)
    }
}
