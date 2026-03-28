mod client;
mod config;
mod gateway;
mod platform;
mod queue;

use client::KrabsServerClient;
use config::GatewayConfig;
use gateway::Gateway;
use platform::{slack::SlackAdapter, telegram::TelegramAdapter, whatsapp::WhatsAppAdapter};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "krabs_gateway=info".parse().unwrap()),
        )
        .init();

    let cfg = GatewayConfig::from_env();

    let secret_key = std::env::var("KRABS_SERVER_SECRET_KEY").ok();
    let client = KrabsServerClient::new(&cfg.server_url, secret_key);
    let gateway: Arc<dyn platform::MessageHandler> = Gateway::new(client);

    info!("krabs-gateway starting — server: {}", cfg.server_url);

    if !cfg.validate() {
        // No platforms configured — stay alive so Docker doesn't restart-loop.
        // Add a token to .env and restart the container to activate a platform.
        std::future::pending::<()>().await;
        return Ok(());
    }

    let mut handles = Vec::new();

    if let Some(wa_cfg) = cfg.whatsapp {
        info!("starting WhatsApp adapter on port {}", wa_cfg.port);
        let adapter = Arc::new(WhatsAppAdapter::new(wa_cfg));
        handles.push(adapter.start(Arc::clone(&gateway)).await);
    }

    if let Some(tg_cfg) = cfg.telegram {
        info!("starting Telegram adapter");
        let adapter = Arc::new(TelegramAdapter::new(tg_cfg));
        handles.push(adapter.start(Arc::clone(&gateway)).await);
    }

    if let Some(sl_cfg) = cfg.slack {
        info!("starting Slack adapter on port {}", sl_cfg.port);
        let adapter = Arc::new(SlackAdapter::new(sl_cfg));
        handles.push(adapter.start(Arc::clone(&gateway)).await);
    }

    // Wait for all adapters — any one crashing surfaces as an error.
    for handle in handles {
        handle.await?;
    }

    Ok(())
}
