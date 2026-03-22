use anyhow::Result;
use clap::Parser;
use krabs_server::{AppState, ServerConfig};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "krabs-server", about = "Krabs HTTP API server")]
struct Cli {
    /// Bind address (overrides KRABS_SERVER_BIND)
    #[arg(long)]
    bind: Option<String>,

    /// Secret key for X-Secret-Key auth (overrides KRABS_SERVER_SECRET_KEY)
    #[arg(long, env = "KRABS_SERVER_SECRET_KEY")]
    secret_key: Option<String>,

    /// Max concurrent agents (overrides KRABS_SERVER_MAX_AGENTS)
    #[arg(long)]
    max_agents: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let mut config = ServerConfig::from_env()?;

    // CLI flags override env vars
    if let Some(bind) = cli.bind {
        config.bind = bind;
    }
    if let Some(secret_key) = cli.secret_key {
        config.secret_key = Some(secret_key);
    }
    if let Some(max_agents) = cli.max_agents {
        config.max_agents = max_agents;
    }

    let bind_addr = config.bind.clone();
    let state: Arc<AppState> = AppState::new(config);
    let app = krabs_server::routes::router(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("krabs-server listening on {bind_addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
