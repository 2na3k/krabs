use anyhow::Result;
use std::path::{Path, PathBuf};
use tracing::warn;

/// An agent persona loaded from `./krabs/agents/<name>.md`.
///
/// Markdown body (after optional YAML frontmatter) is appended to the base
/// system prompt when the persona is activated. Frontmatter may optionally
/// override `model` and `provider`.
pub struct AgentPersona {
    pub name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Persona body — the system-prompt extension text.
    pub system_prompt: String,
    pub path: PathBuf,
}

impl AgentPersona {
    /// Parse a single `.md` file into an `AgentPersona`.
    pub fn parse(path: &Path) -> Result<Self> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid filename: {:?}", path))?
            .to_string();

        let content = std::fs::read_to_string(path)?;

        let (description, model, provider, system_prompt) =
            if let Some(stripped) = content.strip_prefix("---") {
                // Strip the leading "---\n"
                let after_open = stripped.trim_start_matches('\n');
                if let Some(end) = after_open.find("\n---") {
                    let yaml_str = &after_open[..end];
                    let body = after_open[end + 4..].trim_start_matches('\n').to_string();

                    let yaml: serde_yaml::Value =
                        serde_yaml::from_str(yaml_str).unwrap_or(serde_yaml::Value::Null);

                    let description = yaml["description"].as_str().map(String::from);
                    let model = yaml["model"].as_str().map(String::from);
                    let provider = yaml["provider"].as_str().map(String::from);

                    (description, model, provider, body)
                } else {
                    (None, None, None, content)
                }
            } else {
                (None, None, None, content)
            };

        Ok(Self {
            name,
            description,
            model,
            provider,
            system_prompt,
            path: path.to_path_buf(),
        })
    }

    /// Scan `./krabs/agents/` for `*.md` files, parse each one, skip bad
    /// files with a warning (never fatal). Returns personas sorted by name.
    pub fn discover() -> Vec<Self> {
        let cwd = std::env::current_dir().unwrap_or_default();
        let dir = cwd.join("krabs").join("agents");

        if !dir.exists() {
            return Vec::new();
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to read agents directory {:?}: {}", dir, e);
                return Vec::new();
            }
        };

        let mut personas: Vec<Self> = Vec::new();

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read directory entry: {}", e);
                    continue;
                }
            };

            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }

            match Self::parse(&path) {
                Ok(persona) => personas.push(persona),
                Err(e) => warn!("Skipping agent persona at {:?}: {}", path, e),
            }
        }

        personas.sort_by(|a, b| a.name.cmp(&b.name));
        personas
    }
}
