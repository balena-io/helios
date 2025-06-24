mod api;
mod config;
pub mod control;
pub mod link;
mod overrides;

use anyhow::Result;
use api::Api;
use config::Config;
use link::UplinkService;
use overrides::Overrides;
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
        let overrides = Overrides::new(config.uuid.clone());

        info!("Starting target state poll");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _ = UplinkService::new(config, tx).await?;

        // Get a copy of the API state to pass on the target
        let api_state = api.get_state();
        tokio::spawn(async move {
            while let Some(directive) = rx.recv().await {
                debug!("received target state request");
                // Override the target state from `/mnt/temp/apps`
                let target_with_overrides = overrides.apply(directive.target.clone()).await;

                // Update the API target state, this will be returned by the API in
                // polling requests from the legacy supervisor
                api_state.set_target_state(target_with_overrides).await;
            }
        });
    }

    api.start().await?;

    Ok(())
}
