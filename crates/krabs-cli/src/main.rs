mod chat;
mod setup;

use anyhow::Result;
use krabs_core::Credentials;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_banner(creds: &Credentials) {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    println!();
    println!("  ██╗  ██╗██████╗  █████╗ ██████╗ ███████╗");
    println!("  ██║ ██╔╝██╔══██╗██╔══██╗██╔══██╗██╔════╝");
    println!("  █████╔╝ ██████╔╝███████║██████╔╝███████╗");
    println!("  ██╔═██╗ ██╔══██╗██╔══██║██╔══██╗╚════██║");
    println!("  ██║  ██╗██║  ██║██║  ██║██████╔╝███████║");
    println!("  ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝╚═════╝ ╚══════╝");
    println!();
    println!("  version   {VERSION}");
    println!("  provider  {}", creds.provider);
    println!("  model     {}", creds.model);
    println!("  dir       {cwd}");
    println!();
    println!("  type /quit to exit");
    println!();
}

#[tokio::main]
async fn main() -> Result<()> {
    let creds = match Credentials::load()? {
        Some(c) if c.is_configured() => c,
        _ => setup::run_setup()?,
    };
    print_banner(&creds);
    chat::run(creds).await
}
