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
mod legacy;
mod remote;
mod state;
mod util;

use api::Listener;
use cli::Command;
use config::{Config, LocalAddress};
use legacy::ProxyContext;
use remote::register;
use state::models::Device;

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
    let proxy_ctx = ProxyContext::new(
        config.uuid.clone().into(),
        config.remote.api_endpoint.clone(),
        config.legacy.address.clone(),
    );

    // Load the initial state
    let initial_state = Device::initial_for(config.uuid.clone());

    // Set-up channels to trigger state poll, updates and reporting
    let (seek_request_tx, seek_request_rx) = watch::channel(state::SeekRequest::default());
    let (poll_request_tx, poll_request_rx) = watch::channel(remote::PollRequest::default());
    let (local_state_tx, local_state_rx) = watch::channel(state::LocalState {
        device: initial_state.clone(),
        status: state::UpdateStatus::default(),
    });

    // Try to bind to the API port first, this will avoid doing an extra poll
    // if the local port is taken
    let listener = match config.local_address {
        LocalAddress::Tcp(socket_addr) => Listener::Tcp(TcpListener::bind(socket_addr).await?),
        LocalAddress::Unix(ref path) => Listener::Unix(UnixListener::bind(path)?),
    };
    debug!("bound to local address {}", config.local_address);

    // Start the API and the main loop and terminate on any error
    tokio::select! {
        _ = api::start(listener, seek_request_tx.clone(), poll_request_tx.clone(), local_state_rx.clone(), proxy_ctx.clone()) => Ok(()),
        _ = remote::start_poll(&config.uuid, &config.remote, poll_request_rx.clone(), seek_request_tx) => Ok(()),
        _ = remote::start_report(&config.remote, local_state_rx) => Ok(()),
        res = state::start_seek(&config.uuid, initial_state, proxy_ctx, &config.legacy, seek_request_rx, local_state_tx) => res.map_err(|err| err.into()),
    }
}
