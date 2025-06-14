mod api;
mod config;

use anyhow::Result;
use api::Api;
use config::Config;
use std::env;
use std::net::SocketAddr;
use tracing::info;

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

    let balena_api_endpoint = env::var("BALENA_API_ENDPOINT")
        .unwrap_or_else(|_| "https://api.balena-cloud.com".to_string());
    let listen_port = env::var("BALENA_SUPERVISOR_PORT")
        .unwrap_or_else(|_| "48484".to_string())
        .parse::<u16>()?;

    let listen_addr: SocketAddr = format!("0.0.0.0:{}", listen_port).parse()?;
    let legacy_supervisor_url = env::var("BALENA_SUPERVISOR_ADDRESS")
        .unwrap_or_else(|_| "http://127.0.0.1:48480".to_string());

    info!("Configuration loaded successfully");
    info!("Balena API endpoint: {}", balena_api_endpoint);
    info!("Legacy Supervisor URL: {}", legacy_supervisor_url);

    let config = Config {
        listen_addr,
        balena_api_endpoint,
        legacy_supervisor_url,
    };

    let api = Api::new(config);

    info!("Starting supervisor API");
    api.start().await?;

    Ok(())
}
