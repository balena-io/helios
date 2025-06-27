mod api;
mod config;
pub mod control;
pub mod link;
mod overrides;

use anyhow::Result;
use api::Api;
use clap::Parser;
use config::Config;
use link::UplinkService;
use overrides::Overrides;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
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
        .with(
            EnvFilter::try_from_default_env().unwrap_or(
                EnvFilter::default()
                    .add_directive("debug".parse()?)
                    .add_directive("hyper=error".parse()?)
                    .add_directive("bollard=error".parse()?),
            ),
        )
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_span_events(FmtSpan::CLOSE)
                .event_format(fmt::format().compact().with_target(false).without_time()),
        )
        .init();

    info!("Service started");

    let config = Config::parse();
    info!("Configuration loaded successfully");
    debug!("{:#?}", config);

    // Only start poll if there is an API endpoint
    let api = if let Some(api_endpoint) = config.remote.api_endpoint.clone() {
        let overrides = Overrides::new(config.uuid.clone());

        info!("Starting target state poll to {api_endpoint}");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let uplink = UplinkService::new(api_endpoint, config.clone(), tx).await?;

        let api = Arc::new(Api::new(config).with_uplink(uplink));

        let api_ref = Arc::clone(&api);
        tokio::spawn(async move {
            while let Some(directive) = rx.recv().await {
                // Override the target state from `/mnt/temp/apps`
                let target_with_overrides = overrides.apply(directive.target.clone()).await;

                // Update the API target state, this will be returned by the API in
                // polling requests from the legacy supervisor
                api_ref.set_target_state(target_with_overrides).await;
            }
        });
        api
    } else {
        warn!("No remote API endpoint provided, running in unmanaged mode");
        Arc::new(Api::new(config.clone()))
    };

    api.start().await?;

    Ok(())
}
