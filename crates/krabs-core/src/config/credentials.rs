use crate::providers::provider::LlmProvider;
use crate::providers::{AnthropicProvider, GeminiProvider, OpenAiProvider};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub is_default: bool,
}

impl Credentials {
    /// Build credentials from environment variables.
    /// Requires `KRABS_PROVIDER` to be set; all other fields fall back to
    /// sensible defaults for the chosen provider.
    pub fn from_env() -> Option<Self> {
        let provider = std::env::var("KRABS_PROVIDER").ok()?;

        let api_key = std::env::var("KRABS_API_KEY")
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .unwrap_or_default();

        let base_url = std::env::var("KRABS_BASE_URL")
            .unwrap_or_else(|_| Self::default_base_url_for(&provider));

        let model =
            std::env::var("KRABS_MODEL").unwrap_or_else(|_| Self::default_model_for(&provider));

        Some(Self {
            provider,
            api_key,
            base_url,
            model,
            is_default: true,
        })
    }

    fn default_base_url_for(provider: &str) -> String {
        match provider {
            "anthropic" => "https://api.anthropic.com".to_string(),
            "gemini" | "google" => {
                "https://generativelanguage.googleapis.com/v1beta/openai".to_string()
            }
            "ollama" => "http://localhost:11434/v1".to_string(),
            _ => "https://api.openai.com/v1".to_string(),
        }
    }

    fn default_model_for(provider: &str) -> String {
        match provider {
            "anthropic" => "claude-opus-4-6".to_string(),
            "gemini" | "google" => "gemini-2.5-flash-preview".to_string(),
            "ollama" => "llama3.2".to_string(),
            _ => "gpt-4o".to_string(),
        }
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
