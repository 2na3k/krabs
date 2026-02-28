pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod provider;

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub use openai::OpenAiProvider;
pub use provider::{LlmProvider, LlmResponse, Message, Role, TokenUsage, ToolCall};

/// Infer a human-readable provider name from the API base URL.
pub fn provider_name_from_url(base_url: &str) -> String {
    if base_url.contains("anthropic.com") {
        "anthropic".to_string()
    } else if base_url.contains("generativelanguage.googleapis.com")
        || base_url.contains("aiplatform.googleapis.com")
    {
        "gemini".to_string()
    } else if base_url.contains("openai.com") {
        "openai".to_string()
    } else {
        "custom".to_string()
    }
}
