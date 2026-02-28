#[allow(clippy::module_inception)]
pub mod config;
pub mod credentials;
pub use config::{KrabsConfig, SkillsConfig};
pub use credentials::Credentials;
