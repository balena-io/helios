use std::error::Error;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch::{self};
use tracing::trace;
use tracing::{debug, instrument};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

mod api;
mod cli;
mod config;
mod fallback;
mod register;
mod remote;
mod target;
mod util;

use api::Listener;
use cli::Command;
use config::{Config, LocalAddress};
use register::register;
use target::Device;

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
async fn main() -> Result<(), Box<dyn Error>> {
    initialize_tracing();

    let (command, config) = cli::load()?;
    trace!(config = ?config, "configuration loaded");

    match command {
        Some(Command::Register { provisioning_key }) => {
            register(config.remote, provisioning_key).await
        }
        None => {
            // Default command
            run_supervisor(config).await?
        }
    }

    Ok(())
}

#[instrument(name = "helios", skip_all, err)]
pub async fn run_supervisor(config: Config) -> Result<(), Box<dyn Error>> {
    let fallback_state = fallback::FallbackState::new(
        config.uuid.clone().into(),
        config.remote.api_endpoint.clone(),
        config.fallback.address.clone(),
    );

    // Set-up a channel to receive update requests coming from the API
    let (update_request_tx, update_request_rx) = watch::channel(target::UpdateRequest::default());

    // Try to bind to the API port first, this will avoid doing an extra poll
    // if the local port is taken
    let listener = match config.local_address {
        LocalAddress::Tcp(socket_addr) => Listener::Tcp(TcpListener::bind(socket_addr).await?),
        LocalAddress::Unix(ref path) => Listener::Unix(UnixListener::bind(path)?),
    };
    debug!("bound to local address {}", config.local_address);

    // Load the initial state
    let current_state = Device::initial_for(config.uuid.clone()).into_current_state();

    // Start the API and the main loop and terminate on any error
    tokio::select! {
        _ = api::start(listener, update_request_tx, current_state.clone(), fallback_state.clone()) => Ok(()),
        res = target::start(config, current_state, fallback_state, update_request_rx) => res.map_err(|err| err.into()),
    }
}
