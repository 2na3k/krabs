use crate::sandbox::SandboxConfig;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsConfig {
    #[serde(default = "default_skill_paths")]
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub enabled: Vec<String>,
}

fn default_skill_paths() -> Vec<PathBuf> {
    vec![PathBuf::from("skills")]
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            paths: default_skill_paths(),
            enabled: Vec::new(),
        }
    }
}

/// A named custom model entry pointing at an OpenAI-compatible endpoint.
///
/// Example in `~/.krabs/config.json` or `.krabs.json`:
/// ```json
/// {
///   "custom_models": [
///     {
///       "name": "llama3.2-local",
///       "provider": "openai",
///       "base_url": "http://localhost:8080/v1",
///       "api_key": "",
///       "model": "llama3.2"
///     }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomModelEntry {
    /// Display name shown in `/models` output.
    pub name: String,
    /// Provider type: `"openai"` | `"anthropic"` | `"gemini"`. Defaults to `"openai"`.
    #[serde(default = "default_entry_provider")]
    pub provider: String,
    /// Base URL for the API endpoint (OpenAI-compatible servers, llama.cpp, vLLM, etc.).
    pub base_url: String,
    /// API key — may be empty for local servers that don't require auth.
    #[serde(default)]
    pub api_key: String,
    /// Model identifier sent in the request (e.g. `"llama3.2"`, `"mistral"`).
    pub model: String,
}

fn default_entry_provider() -> String {
    "openai".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KrabsConfig {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
    #[serde(default)]
    pub skills: SkillsConfig,
    /// User-defined custom model entries loaded from config.
    #[serde(default)]
    pub custom_models: Vec<CustomModelEntry>,
    /// How many times to retry a failed LLM API call before giving up.
    #[serde(default = "default_max_retries")]
    pub max_retries: usize,
    /// Base delay in milliseconds for exponential backoff between retries.
    #[serde(default = "default_retry_base_delay_ms")]
    pub retry_base_delay_ms: u64,
    /// Sandbox configuration for restricting agent capabilities.
    #[serde(default)]
    pub sandbox: SandboxConfig,
}

fn default_model() -> String {
    std::env::var("KRABS_MODEL").unwrap_or_else(|_| "gpt-4o".to_string())
}

fn default_base_url() -> String {
    std::env::var("KRABS_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string())
}

fn default_max_turns() -> usize {
    50
}

fn default_db_path() -> PathBuf {
    KrabsConfig::resolve_path("krabs.db")
}

fn default_max_context_tokens() -> usize {
    128_000
}

fn default_max_retries() -> usize {
    3
}

fn default_retry_base_delay_ms() -> u64 {
    500
}

impl Default for KrabsConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            base_url: default_base_url(),
            api_key: std::env::var("KRABS_API_KEY")
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .unwrap_or_default(),
            max_turns: default_max_turns(),
            db_path: default_db_path(),
            max_context_tokens: default_max_context_tokens(),
            skills: SkillsConfig::default(),
            custom_models: Vec::new(),
            max_retries: default_max_retries(),
            retry_base_delay_ms: default_retry_base_delay_ms(),
            sandbox: SandboxConfig::default(),
        }
    }
}

impl KrabsConfig {
    pub fn load() -> Result<Self> {
        let config_path = Self::resolve_path("config.json");

        let mut config = if config_path.exists() {
            let data = std::fs::read_to_string(&config_path)?;
            serde_json::from_str::<KrabsConfig>(&data)?
        } else {
            KrabsConfig::default()
        };

        if config.api_key.is_empty() {
            config.api_key = std::env::var("KRABS_API_KEY")
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .unwrap_or_default();
        }

        let local_path = std::env::current_dir()
            .ok()
            .map(|d| d.join(".krabs.json"))
            .filter(|p| p.exists());

        if let Some(local) = local_path {
            let data = std::fs::read_to_string(local)?;
            let override_val: serde_json::Value = serde_json::from_str(&data)?;
            let mut base = serde_json::to_value(&config)?;
            if let (Some(base_obj), Some(over_obj)) =
                (base.as_object_mut(), override_val.as_object())
            {
                for (k, v) in over_obj {
                    base_obj.insert(k.clone(), v.clone());
                }
            }
            config = serde_json::from_value(base)?;
        }

        Ok(config)
    }

    pub fn resolve_path(relative: &str) -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".krabs")
            .join(relative)
    }
}
