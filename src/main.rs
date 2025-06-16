mod api;
mod config;

use anyhow::Result;
use api::Api;
use config::Config;
use tracing::{debug, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for human-readable logs
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .without_time()
        .init();

    info!("Supervisor started");

    let config = Config::from_env()?;
    info!("Configuration loaded successfully");
    debug!("{:#?}", config);

    let api = Api::new(config);

    api.start().await?;

    Ok(())
}
