use crate::platform::{slack::SlackConfig, telegram::TelegramConfig, whatsapp::WhatsAppConfig};
use anyhow::Result;

pub struct GatewayConfig {
    pub server_url: String,
    pub whatsapp: Option<WhatsAppConfig>,
    pub telegram: Option<TelegramConfig>,
    pub slack: Option<SlackConfig>,
}

impl GatewayConfig {
    /// Load from environment variables. Each platform is optional — if its
    /// required token is absent, that adapter is simply not started.
    pub fn from_env() -> Self {
        let server_url = std::env::var("KRABS_SERVER_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".into());

        Self {
            server_url,
            whatsapp: WhatsAppConfig::from_env().ok(),
            telegram: TelegramConfig::from_env().ok(),
            slack: SlackConfig::from_env().ok(),
        }
    }

    pub fn validate(&self) -> bool {
        let ok = self.whatsapp.is_some() || self.telegram.is_some() || self.slack.is_some();
        if !ok {
            tracing::warn!(
                "no platform configured — set at least one of: \
                 WHATSAPP_TOKEN / TELEGRAM_BOT_TOKEN / SLACK_BOT_TOKEN. \
                 Gateway is idle."
            );
        }
        ok
    }
}
