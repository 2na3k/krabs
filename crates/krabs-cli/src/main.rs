mod chat;
mod setup;

use anyhow::Result;
use krabs_core::Credentials;

#[tokio::main]
async fn main() -> Result<()> {
    let creds = match Credentials::load()? {
        Some(c) if c.is_configured() => c,
        _ => setup::run_setup()?,
    };
    chat::run(creds).await
}
