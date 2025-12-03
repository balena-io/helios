use std::error::Error;
use std::future::{self, Future};

use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch::{self};
use tracing::{debug, instrument, trace, warn};
use tracing_subscriber::{
    EnvFilter,
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

mod cli;

use helios_api as api;
use helios_legacy as legacy;
use helios_oci as oci;
use helios_remote as remote;
use helios_state as state;
use helios_util as util;
use helios_util::types::{OperatingSystem, Uuid};

use crate::api::{ApiConfig, Listener, LocalAddress};
use crate::cli::Cli;
use crate::legacy::{LegacyConfig, ProxyConfig, ProxyState};
use crate::oci::RegistryAuthClient;
use crate::remote::{
    ProvisioningConfig, ProvisioningError, RemoteConfig, RequestConfig, provision,
};
use crate::util::config::StoredConfig;
use crate::util::dirs::config_dir;
use crate::util::store::Store;

fn initialize_tracing() {
    // Initialize tracing subscriber for human-readable logs
    tracing_subscriber::registry()
        .with(
            // Use some log defaults. These can be overriden using
            // RUST_LOG
            EnvFilter::try_from_default_env().unwrap_or(
                EnvFilter::default()
                    .add_directive("trace".parse().unwrap())
                    .add_directive("mahler_core::planner=warn".parse().unwrap())
                    .add_directive("mahler_core::worker=debug".parse().unwrap())
                    .add_directive("hyper=error".parse().unwrap())
                    .add_directive("reqwest=debug".parse().unwrap())
                    .add_directive("zbus=error".parse().unwrap())
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

    // make sure the runtime dir exists
    util::dirs::ensure_runtime_dir()?;

    let cli = cli::parse();

    // Create a new configuration store instance
    let config_store = Store::new(config_dir());

    let api_config = cli
        .local_api_address
        .as_ref()
        .map(|local_address| ApiConfig {
            local_address: local_address.clone(),
        });

    let legacy_config = cli
        .legacy_api_endpoint
        .as_ref()
        .map(|api_endpoint| LegacyConfig {
            api_endpoint: api_endpoint.clone(),
            api_key: cli
                .legacy_api_key
                .clone()
                .expect("not nil because legacy_api_endpoint isn't nil"),
        });

    let (uuid, remote_config) = maybe_provision(&cli, &config_store).await?;
    let os = cli.os.clone();

    start_supervisor(uuid, os, api_config, remote_config, legacy_config).await?;

    Ok(())
}

#[instrument(name = "helios", skip_all, err)]
async fn start_supervisor(
    uuid: Uuid,
    os: Option<OperatingSystem>,
    api_config: Option<ApiConfig>,
    remote_config: Option<RemoteConfig>,
    legacy_config: Option<LegacyConfig>,
) -> Result<(), Box<dyn Error>> {
    trace!(
        uuid = ?uuid,
        api = ?api_config,
        remote = ?remote_config,
        legacy = ?legacy_config,
        "using config:"
    );

    // Create a registry auth client using the remote credentials
    let registry_auth = remote_config
        .clone()
        .map(|c| RegistryAuthClient::new(c.api_endpoint.clone(), c.into()));

    let (state_runtime, initial_state) = state::prepare(uuid.clone(), os, registry_auth).await?;

    // Set-up channels to trigger state poll, updates and reporting
    let (seek_request_tx, seek_request_rx) = watch::channel(state::SeekRequest::default());
    let (poll_request_tx, poll_request_rx) = watch::channel(remote::PollRequest::default());
    let (local_state_tx, local_state_rx) = watch::channel(state::LocalState {
        device: initial_state.clone(),
        status: state::UpdateStatus::default(),
    });

    // Setup local API server
    let listener = if let Some(api_config) = &api_config {
        // Try to bind to the API port first, this will avoid doing an extra poll
        // if the local port is taken
        let listener = match api_config.local_address {
            LocalAddress::Tcp(socket_addr) => Listener::Tcp(TcpListener::bind(socket_addr).await?),
            LocalAddress::Unix(ref path) => Listener::Unix(UnixListener::bind(path)?),
        };
        debug!("bound to local address {}", api_config.local_address);
        Some(listener)
    } else {
        None
    };

    // Setup legacy proxy
    let (proxy_config, proxy_state) = if let Some(legacy_config) = &legacy_config {
        let proxy_config = ProxyConfig::new(
            uuid.clone(),
            legacy_config.api_endpoint.clone(),
            remote_config.as_ref().map(|c| c.api_endpoint.clone()),
        );
        let proxy_state = ProxyState::new(None);

        (Some(proxy_config), Some(proxy_state))
    } else {
        (None, None)
    };

    if remote_config.is_none() {
        warn!("running in unmanaged mode");
    }

    // Start main loop and terminate on error
    tokio::select! {
        // Start local API server
        _ = maybe_start(listener, |listener| {
            api::start(
                listener,
                seek_request_tx.clone(),
                poll_request_tx.clone(),
                local_state_rx.clone(),
                proxy_config.clone(),
                proxy_state.clone(),
            )
        }) => Ok(()),

        // Start remote polling
        _ = maybe_start(remote_config.clone(), |remote_config| {
            remote::start_poll(
                uuid.clone(),
                remote_config,
                poll_request_rx.clone(),
                seek_request_tx.clone(),
            )
        }) => Ok(()),

        // Start remote reporting
        _ = maybe_start(remote_config, |remote_config| {
            remote::start_report(
                remote_config,
                local_state_rx.clone(),
            )
        }) => Ok(()),

        // Start state seeking
        res = state::start_seek(
            state_runtime,
            initial_state,
            proxy_state.clone(),
            legacy_config,
            seek_request_rx,
            local_state_tx,
        ) => res.map_err(|err| err.into()),
    }
}

/// Given an optional value `pred`, invoke closure `f` if it is `Some(P)`
/// and return the result, otherwise return a pending future in its place.
///
/// This is useful to reduce boilerplate in our main loop `select!`.
async fn maybe_start<P, F, Fut, T>(pred: Option<P>, f: F) -> T
where
    Fut: Future<Output = T>,
    F: FnOnce(P) -> Fut,
{
    match pred {
        Some(p) => f(p).await,
        None => future::pending().await,
    }
}

/// Handle provisioning.
///
/// See if we need to register with remote. This is an one-time operation that
/// is triggered by the presence of a `provisioning_key` argument in the CLI.
/// Before registration, anything related to a remote backend is unavailable --
/// we can't do much without an API key anyway.
///
/// Instead of registration, the CLI also allows the user to pass a
/// (uuid, api-endpoint, api-key) triplet that is enough for us to work with a
/// remote, in which case we won't try provisioning even if a provisioning key
/// is present.
///
/// Provisioning state is tracked by whether `remote_config` is nil or not.
/// If `remote_config` is not nil, then we are registered with a remote.
/// If `remote_config` is nil, then we aren't registered and need to provision.
/// If `remote_config` is still nil after provisioning, then we'll run in "unmanaged" mode.
async fn maybe_provision(
    cli: &Cli,
    config_store: &Store,
) -> Result<(Uuid, Option<RemoteConfig>), ProvisioningError> {
    // Load our provisioning config, if one exists
    let provisioning_config: Option<ProvisioningConfig> = config_store
        .read("/", ProvisioningConfig::default_name())
        .await?;

    // See if the triplet (uuid, remote_api_endpoint, remote_api_key) is provided.
    // If so, we have everything we need to assume an identity.
    if let (Some(uuid), Some(api_endpoint), Some(api_key)) =
        (&cli.uuid, &cli.remote_api_endpoint, &cli.remote_api_key)
    {
        let request_defaults = RequestConfig::default();
        let request = RequestConfig {
            timeout: cli
                .remote_request_timeout
                .unwrap_or(request_defaults.timeout),
            poll_interval: cli
                .remote_poll_interval
                .unwrap_or(request_defaults.poll_interval),
            poll_min_interval: cli
                .remote_poll_min_interval
                .unwrap_or(request_defaults.poll_min_interval),
            poll_max_jitter: cli
                .remote_poll_max_jitter
                .unwrap_or(request_defaults.poll_max_jitter),
        };
        let remote = RemoteConfig {
            api_endpoint: api_endpoint.clone(),
            api_key: api_key.clone(),
            request,
        };

        if let Some(provisioning_config) = &provisioning_config {
            warn!(
                "ignoring existing identity {0} at {1}",
                provisioning_config.uuid, provisioning_config.remote.api_endpoint
            );
        }

        Ok((uuid.clone(), Some(remote)))
    }
    // Otherwise use an existing provisioning config, if available
    else if let Some(provisioning_config) = &provisioning_config {
        if cli.uuid.is_some() && !cli.uuid.as_ref().eq(&Some(&provisioning_config.uuid)) {
            warn!("ignoring --uuid argument that is different to registered remote");
        }
        if cli.remote_api_key.is_some()
            && !cli
                .remote_api_key
                .as_ref()
                .eq(&Some(&provisioning_config.remote.api_key))
        {
            warn!("ignoring --remote-api-key argument that is different to registered remote");
        }
        if cli.remote_api_endpoint.is_some()
            && !cli
                .remote_api_endpoint
                .as_ref()
                .eq(&Some(&provisioning_config.remote.api_endpoint))
        {
            warn!("ignoring --remote-api-endpoint argument that is different to registered remote");
        }

        let uuid = &provisioning_config.uuid;
        let request_defaults = &provisioning_config.remote.request;
        let remote = RemoteConfig {
            request: RequestConfig {
                timeout: cli
                    .remote_request_timeout
                    .unwrap_or(request_defaults.timeout),
                poll_interval: cli
                    .remote_poll_interval
                    .unwrap_or(request_defaults.poll_interval),
                poll_min_interval: cli
                    .remote_poll_min_interval
                    .unwrap_or(request_defaults.poll_min_interval),
                poll_max_jitter: cli
                    .remote_poll_max_jitter
                    .unwrap_or(request_defaults.poll_max_jitter),
            },
            ..provisioning_config.remote.clone()
        };

        Ok((uuid.clone(), Some(remote)))
    }
    // We have a provisioning key
    else if let Some(provisioning_key) = &cli.provisioning_key {
        // Get some defaults
        let request_defaults = RequestConfig::default();

        // Gather all necessary input for provisioning
        let provisioning_config = ProvisioningConfig {
            // Auto-generate a uuid if none provided
            uuid: cli.uuid.clone().unwrap_or_default(),

            fleet: cli
                .provisioning_fleet
                .expect("not nil because provisioning_key isn't nil"),
            device_type: cli
                .provisioning_device_type
                .clone()
                .expect("not nil because provisioning_key isn't nil"),

            remote: RemoteConfig {
                // Auto-generate an api key if none provided
                api_key: cli.remote_api_key.clone().unwrap_or_default(),
                api_endpoint: cli
                    .remote_api_endpoint
                    .clone()
                    .expect("not nil because provisioning_key isn't nil"),
                request: RequestConfig {
                    timeout: cli
                        .remote_request_timeout
                        .unwrap_or(request_defaults.timeout),
                    poll_interval: cli
                        .remote_poll_interval
                        .unwrap_or(request_defaults.poll_interval),
                    poll_min_interval: cli
                        .remote_poll_min_interval
                        .unwrap_or(request_defaults.poll_min_interval),
                    poll_max_jitter: cli
                        .remote_poll_max_jitter
                        .unwrap_or(request_defaults.poll_max_jitter),
                },
            },
        };

        let (uuid, remote, _) =
            provision(provisioning_key, &provisioning_config, config_store).await?;

        Ok((uuid, Some(remote)))
    }
    // We don't have a remote at all; run in "unmanaged" mode
    else {
        // Generate a UUID if none provided
        Ok((cli.uuid.clone().unwrap_or_default(), None))
    }
}
