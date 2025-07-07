use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::trace;
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

mod api;
mod cmd;
mod config;
mod fallback;
mod models;
mod report;
mod request;

use cmd::cli::{Cli, Commands};
use config::Config;

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
/// An update request coming from the API
struct UpdateRequest {
    #[serde(default)]
    /// Trigger an update ignoring locks
    force: bool,
    #[serde(default)]
    /// Cancel the current update if any
    cancel: bool,
}

fn initialize_tracing() {
    // Initialize tracing subscriber for human-readable logs
    tracing_subscriber::registry()
        .with(
            // Use some log defaults. These can be overriden using
            // RUST_LOG
            EnvFilter::try_from_default_env().unwrap_or(
                EnvFilter::default()
                    .add_directive("trace".parse().unwrap())
                    .add_directive("mahler::planner=warn".parse().unwrap())
                    .add_directive("mahler::worker=debug".parse().unwrap())
                    .add_directive("hyper=error".parse().unwrap())
                    .add_directive("bollard=error".parse().unwrap()),
            ),
        )
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_span_events(FmtSpan::CLOSE)
                .event_format(fmt::format().compact().with_target(false).without_time()),
        )
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    initialize_tracing();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Register {
            remote,
            provisioning_key,
        }) => cmd::register(remote, provisioning_key).await,
        None => {
            // Default command
            let config = Config::load(&cli)?;
            trace!(config = ?config, "configuration loaded");

            cmd::start_supervisor(config).await
        }
    }
}
