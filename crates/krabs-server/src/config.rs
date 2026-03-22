use krabs_core::KrabsConfig;
use serde::{Deserialize, Serialize};

fn default_bind() -> String {
    "127.0.0.1:3001".to_string()
}

fn default_max_agents() -> usize {
    16
}

fn default_heartbeat_ms() -> u64 {
    500
}

fn default_replay_capacity() -> usize {
    512
}

/// Server configuration.
///
/// Every field is injectable via environment variable with the `KRABS_SERVER_` prefix:
///
/// | Field              | Env Var                        | Default            |
/// |--------------------|--------------------------------|--------------------|
/// | `bind`             | `KRABS_SERVER_BIND`            | `127.0.0.1:3001`   |
/// | `secret_key`       | `KRABS_SERVER_SECRET_KEY`      | None (no auth)     |
/// | `cors_origins`     | `KRABS_SERVER_CORS_ORIGINS`    | permissive         |
/// | `max_agents`       | `KRABS_SERVER_MAX_AGENTS`      | `16`               |
/// | `heartbeat_ms`     | `KRABS_SERVER_HEARTBEAT_MS`    | `500`              |
/// | `replay_capacity`  | `KRABS_SERVER_REPLAY_CAPACITY` | `512`              |
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,

    #[serde(default)]
    pub secret_key: Option<String>,

    #[serde(default)]
    pub cors_origins: Vec<String>,

    #[serde(default = "default_max_agents")]
    pub max_agents: usize,

    #[serde(default = "default_heartbeat_ms")]
    pub heartbeat_ms: u64,

    #[serde(default = "default_replay_capacity")]
    pub replay_capacity: usize,

    #[serde(flatten)]
    pub krabs: KrabsConfig,
}

impl ServerConfig {
    /// Load configuration from environment variables, falling back to defaults.
    ///
    /// Loading order: defaults < env vars < CLI overrides (applied by caller).
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = std::env::var("KRABS_SERVER_BIND").unwrap_or_else(|_| default_bind());

        let secret_key = std::env::var("KRABS_SERVER_SECRET_KEY").ok();

        let cors_origins: Vec<String> = std::env::var("KRABS_SERVER_CORS_ORIGINS")
            .map(|s| s.split(',').map(|o| o.trim().to_string()).collect())
            .unwrap_or_default();

        let max_agents = std::env::var("KRABS_SERVER_MAX_AGENTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_max_agents);

        let heartbeat_ms = std::env::var("KRABS_SERVER_HEARTBEAT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_heartbeat_ms);

        let replay_capacity = std::env::var("KRABS_SERVER_REPLAY_CAPACITY")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_replay_capacity);

        let krabs = KrabsConfig::default();

        Ok(Self {
            bind,
            secret_key,
            cors_origins,
            max_agents,
            heartbeat_ms,
            replay_capacity,
            krabs,
        })
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            secret_key: None,
            cors_origins: Vec::new(),
            max_agents: default_max_agents(),
            heartbeat_ms: default_heartbeat_ms(),
            replay_capacity: default_replay_capacity(),
            krabs: KrabsConfig::default(),
        }
    }
}
