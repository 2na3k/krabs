mod chat;
mod setup;

use anyhow::Result;
use krabs_core::Credentials;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    let resume_id = args
        .windows(2)
        .find(|w| w[0] == "--resume")
        .map(|w| w[1].clone());

    let creds = match Credentials::from_env() {
        Some(c) if c.is_configured() => c,
        _ => {
            setup::run_setup()?;
            unreachable!()
        }
    };
    chat::run(creds, resume_id).await
}
