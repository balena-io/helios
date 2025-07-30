use std::error::Error;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch::{self};
use tracing::{debug, instrument, trace};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

mod api;
mod cli;
mod legacy;
mod remote;
mod state;
mod types;
mod util;

use api::Listener;
use cli::{Command, LocalAddress};
use legacy::{LegacyConfig, ProxyConfig, ProxyState};
use remote::{register, RemoteConfig};
use state::models::Device;
use types::Uuid;

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

    let (command, _ /* global_args */) = cli::load();

    match command {
        Command::Register(args) => {
            let args = *args;

            register(args.provisioning_key).await
        }
        Command::Start(args) => {
            let args = *args;

            start_supervisor(
                // If no device UUID was provided, generate a random one here.
                args.uuid.unwrap_or_default(),
                args.local_address,
                args.remote_api_endpoint.map(|api_endpoint| RemoteConfig {
                    api_endpoint,
                    api_key: args.remote_api_key,
                    request_timeout: args.remote_request_timeout_ms,
                    poll_interval: args.remote_poll_interval_ms,
                    min_interval: args.remote_min_interval_ms,
                    max_poll_jitter: args.remote_max_poll_jitter_ms,
                }),
                args.legacy_address.map(|address| LegacyConfig {
                    address,
                    api_key: args.legacy_api_key,
                }),
            )
            .await?
        }
    }

    Ok(())
}

#[instrument(name = "helios", skip_all, err)]
async fn start_supervisor(
    uuid: Uuid,
    local_address: LocalAddress,
    remote_config: Option<RemoteConfig>,
    legacy_config: Option<LegacyConfig>,
) -> Result<(), Box<dyn Error>> {
    trace!(
        uuid = ?uuid,
        local_address = ?local_address,
        remote = ?remote_config,
        legacy = ?legacy_config,
        "using config:"
    );

    // Load the initial state
    let initial_state = Device::initial_for(uuid.clone());

    // Set-up channels to trigger state poll, updates and reporting
    let (seek_request_tx, seek_request_rx) = watch::channel(state::SeekRequest::default());
    let (poll_request_tx, poll_request_rx) = watch::channel(remote::PollRequest::default());
    let (local_state_tx, local_state_rx) = watch::channel(state::LocalState {
        device: initial_state.clone(),
        status: state::UpdateStatus::default(),
    });

    // Try to bind to the API port first, this will avoid doing an extra poll
    // if the local port is taken
    let listener = match local_address {
        LocalAddress::Tcp(socket_addr) => Listener::Tcp(TcpListener::bind(socket_addr).await?),
        LocalAddress::Unix(ref path) => Listener::Unix(UnixListener::bind(path)?),
    };
    debug!("bound to local address {}", local_address);

    // Setup legacy proxy
    let proxy_config = ProxyConfig::new(
        uuid.clone().into(),
        remote_config.as_ref().map(|c| c.api_endpoint.clone()),
        legacy_config.as_ref().map(|c| c.address.clone()),
    );
    let proxy_state = ProxyState::new(None);

    // Start the API and the main loop and terminate on any error
    tokio::select! {
        _ = api::start(
            listener,
            seek_request_tx.clone(),
            poll_request_tx.clone(),
            local_state_rx.clone(),
            proxy_config,
            proxy_state.clone(),
        ) => Ok(()),

        _ = remote::start_poll(
            &uuid,
            &remote_config,
            poll_request_rx.clone(),
            seek_request_tx,
        ) => Ok(()),

        _ = remote::start_report(
            &remote_config,
            local_state_rx,
        ) => Ok(()),

        res = state::start_seek(
            &uuid,
            initial_state,
            proxy_state,
            &legacy_config,
            seek_request_rx,
            local_state_tx,
        ) => res.map_err(|err| err.into()),
    }
}
