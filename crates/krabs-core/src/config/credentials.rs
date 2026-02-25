use crate::providers::provider::LlmProvider;
use crate::providers::{AnthropicProvider, GeminiProvider, OpenAiProvider};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub is_default: bool,
}

impl Credentials {
    pub fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".krabs")
            .join("credentials.json")
    }

    pub fn load() -> Result<Option<Self>> {
        let path = Self::path();
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)?;
        let creds: Credentials = serde_json::from_str(&data)?;
        Ok(Some(creds))
    }

    pub async fn load_async() -> Result<Option<Self>> {
        let path = Self::path();
        if !path.exists() {
            return Ok(None);
        }
        let data = tokio::fs::read_to_string(&path).await?;
        let creds: Credentials = serde_json::from_str(&data)?;
        Ok(Some(creds))
    }

    pub async fn save_async(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let data = serde_json::to_string_pretty(self)?;
        tokio::fs::write(&path, data).await?;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data)?;
        Ok(())
    }

    pub fn is_configured(&self) -> bool {
        self.provider == "ollama" || !self.api_key.is_empty()
    }

    pub fn build_provider(&self) -> Box<dyn LlmProvider> {
        match self.provider.as_str() {
            "anthropic" => Box::new(AnthropicProvider::new(
                &self.base_url,
                &self.api_key,
                &self.model,
            )),
            "gemini" | "google" => Box::new(GeminiProvider::new(&self.api_key, &self.model)),
            _ => Box::new(OpenAiProvider::new(
                &self.base_url,
                &self.api_key,
                &self.model,
            )),
        }
    }
}
