mod api;
mod config;
pub mod control;
pub mod link;

use anyhow::Result;
use api::Api;
use config::Config;
use link::UplinkService;
use tokio::sync::mpsc;
use tracing::{debug, info};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for human-readable logs
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_span_events(FmtSpan::CLOSE)
                .event_format(fmt::format().compact().with_target(false)),
        )
        .init();

    info!("Supervisor started");

    let config = Config::from_env()?;
    info!("Configuration loaded successfully");
    debug!("{:#?}", config);

    let api = Api::new(config.clone());

    if config.balena.api_key.is_some() {
        info!("Starting target state poll");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _ = UplinkService::new(config, tx).await?;
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                debug!("received target state request");
            }
        });
    }

    api.start().await?;

    Ok(())
}
