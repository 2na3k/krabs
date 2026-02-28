use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn hooks_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".krabs")
        .join("hooks.json")
}

/// A single persisted hook entry stored in `~/.krabs/hooks.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    /// Unique identifier for this hook.
    pub name: String,
    /// Lifecycle event this hook listens to.
    /// One of: AgentStart, AgentStop, TurnStart, TurnEnd,
    ///         PreToolUse, PostToolUse, PostToolUseFailure
    pub event: String,
    /// Optional regex matched against the tool name (tool events only).
    pub matcher: Option<String>,
    /// What to do when the event fires.
    /// One of: deny, stop, log
    pub action: String,
    /// Reason string (used for `deny` action).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookConfig {
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

impl HookConfig {
    pub fn load() -> Self {
        let path = hooks_path();
        if !path.exists() {
            return Self::default();
        }
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = hooks_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    pub fn add(&mut self, entry: HookEntry) {
        // replace if same name exists
        self.hooks.retain(|h| h.name != entry.name);
        self.hooks.push(entry);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.hooks.len();
        self.hooks.retain(|h| h.name != name);
        self.hooks.len() < before
    }
}
