use anyhow::bail;
use anyhow::Result;

pub fn run_setup() -> Result<()> {
    bail!(
        "No credentials configured.\n\
         Set the following environment variables (e.g. in a .env file at the project root):\n\
         \n\
         KRABS_PROVIDER=openai          # openai | anthropic | gemini | ollama | custom\n\
         KRABS_API_KEY=sk-...\n\
         KRABS_BASE_URL=https://api.openai.com/v1\n\
         KRABS_MODEL=gpt-4o\n\
         \n\
         See .env.example for the full list of supported variables."
    )
}
