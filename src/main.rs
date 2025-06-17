mod api;
mod config;

use anyhow::Result;
use api::Api;
use config::Config;
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

    let api = Api::new(config);

    api.start().await?;

    Ok(())
}
