use anyhow::Result;
use krabs_core::Credentials;
use std::io::{self, BufRead, Write};

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

pub fn run_setup() -> Result<Credentials> {
    println!("\nWelcome to Krabs CLI");
    println!("No provider configured. Let's set one up.\n");
    println!("Choose a provider:");
    println!("  [1] OpenAI          (api.openai.com)");
    println!("  [2] Anthropic       (api.anthropic.com)");
    println!("  [3] Gemini          (generativelanguage.googleapis.com)");
    println!("  [4] Ollama          (localhost — no key needed)");
    println!("  [5] Custom");

    let choice = read_line("\n> ")?;

    let (provider, base_url, default_model) = match choice.as_str() {
        "1" => (
            "openai",
            "https://api.openai.com/v1".to_string(),
            "gpt-4o".to_string(),
        ),
        "2" => (
            "anthropic",
            "https://api.anthropic.com".to_string(),
            "claude-opus-4-6".to_string(),
        ),
        "3" => (
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            "gemini-2.0-flash".to_string(),
        ),
        "4" => (
            "ollama",
            "http://localhost:11434/v1".to_string(),
            "llama3.2".to_string(),
        ),
        "5" => {
            let url = read_line("Enter base URL: ")?;
            let model = read_line("Enter model name: ")?;
            ("custom", url, model)
        }
        _ => {
            println!("Invalid choice, defaulting to OpenAI.");
            (
                "openai",
                "https://api.openai.com/v1".to_string(),
                "gpt-4o".to_string(),
            )
        }
    };

    let api_key = if provider == "ollama" {
        String::new()
    } else {
        print!("Enter API key: ");
        io::stdout().flush()?;
        let key = rpassword::read_password()?;
        key.trim().to_string()
    };

    let model = if provider == "ollama" || provider == "custom" {
        default_model
    } else {
        let m = read_line(&format!("Model [{}]: ", default_model))?;
        if m.is_empty() {
            default_model
        } else {
            m
        }
    };

    let creds = Credentials {
        provider: provider.to_string(),
        api_key,
        base_url,
        model,
        is_default: true,
    };
    creds.save()?;
    println!("Saved to ~/.krabs/credentials.json\n");

    Ok(creds)
}
