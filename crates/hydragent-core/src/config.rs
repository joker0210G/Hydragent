use serde::Deserialize;
use config::{Config as ConfigBuilder, ConfigError, Environment};

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub primary_model: String,
    pub fallback_models: String,
    pub log_format: String,
    pub log_level: String,
    pub data_dir: String,
    pub max_react_steps: u8,
    pub bus_port: u16,
    pub openrouter_api_keys: String,
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        // Load .env file if it exists
        let _ = dotenvy::dotenv();

        let builder = ConfigBuilder::builder()
            // Set defaults
            .set_default("primary_model", "anthropic/claude-sonnet-4")?
            .set_default("fallback_models", "openai/gpt-4o,mistralai/mistral-7b-instruct")?
            .set_default("log_format", "terminal")?
            .set_default("log_level", "info")?
            .set_default("data_dir", "./data")?
            .set_default("max_react_steps", 10_u64)? // config crate expects integer types as u64/i64 for defaults
            .set_default("bus_port", 5000_u64)?
            .set_default("openrouter_api_keys", "")?


            // Add environment overrides
            .add_source(Environment::default())
            .build()?;

        builder.try_deserialize()
    }
}
