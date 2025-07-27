use std::error::Error;
use std::fs;
use std::future::{self, Future};
use std::pin::Pin;

use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch::{self};
use tracing::{debug, instrument, trace, warn};
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
mod util;

use api::{ApiConfig, Listener, LocalAddress};
use cli::Command;
use legacy::{LegacyConfig, ProxyConfig, ProxyState};
use remote::{register, ProvisioningConfig, RegisterRequest, RemoteConfig, RequestConfig};
use state::models::{Device, Uuid};
use state::SeekError;
use util::crypto::{pseudorandom_string, sha256_hex_digest, ALPHA_NUM};

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

    let command = cli::parse();

    // make sure our config directory exists
    _config::ensure_config_dir()?;

    let config = _config::load()?;

    match command {
        Command::Register(args) => {
            let args = *args;

            // If no device UUID was provided, generate a random one here.
            let uuid = config
                .as_ref()
                .map(|c| c.uuid.clone())
                .or(args.uuid)
                .unwrap_or_default();

            // Handle provisioning.
            //
            // See if we need to register with remote. This is an one-time operation that is
            // triggered by the presence of a `provisioning_key` argument in the CLI.
            // Before registration, anything related to a remote backend is unavailable --
            // we can't do much without an API key anyway.
            //
            // Provisioning state is tracked by whether `remote_config` below is nil or not.
            // If `remote_config` becomes non-nil, then we are registered with a remote.
            // If `remote_config` remains nil, then we'll run in "unmanaged" mode.
            let remote_config = match (
                config.as_ref().and_then(|c| c.remote.clone()),
                Some(args.provisioning_key),
            ) {
                (None, None) => {
                    // We haven't registered before and aren't being
                    // asked to; nothing to do then
                    None
                }

                (Some(remote), None) => {
                    // We have already registered and aren't being
                    // asked to register again; use the config we have
                    Some(remote)
                }

                (Some(remote), Some(_)) if remote.api_endpoint == args.remote_api_endpoint => {
                    // We have already registered on this remote but are
                    // asked to register again; ignore the request
                    warn!("already registered on {}", remote.api_endpoint);
                    Some(remote)
                }

                // We are asked to register and either...
                //  - we haven't registered at all before, or
                //  - we *may* have already registered but on a different remote
                // so proceed to register regardless.
                //
                // TODO: In the future we may want to backup previous remote configs
                // as part of a leave/join cycle. This is the place to do it.
                (_, Some(provisioning_key)) => {
                    // Get defaults
                    let request_config = RequestConfig::default();

                    // The remote expects us to send a UUID and API key during registration,
                    // and we comply by assigning random values if they aren't provided as
                    // arguments to the CLI.
                    //
                    // This however means that should an error or a crash happen after we
                    // register but before we stored the provisioning config (which completes
                    // provisioning from our perspective), we'd loose these values and try
                    // to register again on restart with new values and so on.
                    //
                    // Before attempting to call remote, save the registration request
                    // locally and try to restore it here.
                    let restore_path = _config::config_dir().join(format!(
                        ".provisioning.{}.json",
                        sha256_hex_digest(&args.remote_api_endpoint.to_string())
                    ));
                    fn generate_api_key() -> String {
                        pseudorandom_string(ALPHA_NUM, 32)
                    }
                    let register_request = RegisterRequest {
                        uuid: uuid.clone(),
                        application: args.provisioning_fleet,
                        device_type: args.provisioning_device_type.clone(),
                        api_key: args.remote_api_key.clone().unwrap_or_else(generate_api_key),
                        supervisor_version: args.provisioning_supervisor_version.clone(),
                        os_version: args.provisioning_os_version.clone(),
                        os_variant: args.provisioning_os_variant.clone(),
                        mac_address: args.provisioning_mac_address.clone(),
                    };
                    let register_request = match fs::read_to_string(restore_path) {
                        Ok(contents) => serde_json::from_str(&contents)?,
                        Err(_) => register_request,
                    };

                    let registration = register(
                        provisioning_key.clone(),
                        args.remote_api_endpoint.clone(),
                        args.remote_request_timeout
                            .unwrap_or(request_config.timeout),
                        register_request,
                    )
                    .await?;

                    let remote = RemoteConfig {
                        api_endpoint: args.remote_api_endpoint,
                        api_key: registration.api_key,
                        uuid: registration.uuid,
                        request: RequestConfig {
                            timeout: args
                                .remote_request_timeout
                                .unwrap_or(request_config.timeout),
                            poll_interval: args
                                .remote_poll_interval
                                .unwrap_or(request_config.poll_interval),
                            poll_min_interval: args
                                .remote_poll_min_interval
                                .unwrap_or(request_config.poll_min_interval),
                            poll_max_jitter: args
                                .remote_poll_max_jitter
                                .unwrap_or(request_config.poll_max_jitter),
                        },
                        provisioning: Some(ProvisioningConfig {
                            provisioning_key: provisioning_key,
                            fleet: args.provisioning_fleet,
                            device_type: args.provisioning_device_type,
                            supervisor_version: args.provisioning_supervisor_version,
                            os_name: args.provisioning_os_version,
                            os_variant: args.provisioning_os_variant,
                            mac_address: args.provisioning_mac_address,
                        }),
                    };

                    Some(remote)
                }
            };

            /*_config::save(_config::Config {
                uuid: uuid.clone(),
                api: api_config.clone(),
                legacy: legacy_config.clone(),
                remote: remote_config.clone(),
            })
            .await?;*/
        }
        Command::Start(args) => {
            let args = *args;

            let api_config = args
                .local_api_address
                .map(|local_address| ApiConfig { local_address });

            let legacy_config = args.legacy_api_endpoint.map(|api_endpoint| LegacyConfig {
                api_endpoint,
                api_key: args.legacy_api_key.unwrap(),
            });

            start_supervisor(uuid, api_config, remote_config, legacy_config).await?;
        }
    }

    Ok(())
}

#[instrument(name = "helios", skip_all, err)]
async fn start_supervisor(
    uuid: Uuid,
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

    // Load the initial state
    let initial_state = Device::initial_for(uuid.clone());

    // Setup channels to trigger state poll, updates and reporting
    let (seek_request_tx, seek_request_rx) = watch::channel(state::SeekRequest::default());
    let (poll_request_tx, poll_request_rx) = watch::channel(remote::PollRequest::default());
    let (local_state_tx, local_state_rx) = watch::channel(state::LocalState {
        device: initial_state.clone(),
        status: state::UpdateStatus::default(),
    });

    // Setup legacy proxy
    let (proxy_config, proxy_state) = if let Some(legacy_config) = &legacy_config {
        (
            Some(ProxyConfig::new(
                uuid.clone().into(),
                remote_config.as_ref().map(|c| c.api_endpoint.clone()),
                Some(legacy_config.api_endpoint.clone()),
            )),
            Some(ProxyState::new(None)),
        )
    } else {
        (None, None)
    };

    // Start local API server
    type ApiFuture = Pin<Box<dyn Future<Output = ()>>>;

    let api_future: ApiFuture = if let Some(api_config) = &api_config {
        // Try to bind to the API port first, this will avoid doing an extra poll
        // if the local port is taken
        let listener = match api_config.local_address {
            LocalAddress::Tcp(socket_addr) => Listener::Tcp(TcpListener::bind(socket_addr).await?),
            LocalAddress::Unix(ref path) => Listener::Unix(UnixListener::bind(path)?),
        };
        debug!("bound to local address {}", api_config.local_address);

        Box::pin(api::start(
            listener,
            seek_request_tx.clone(),
            poll_request_tx.clone(),
            local_state_rx.clone(),
            proxy_config.clone(),
            proxy_state.clone(),
        ))
    } else {
        Box::pin(future::pending())
    };

    if remote_config.is_none() {
        warn!("running in unmanaged mode");
    }

    // Start remote polling
    type PollFuture = Pin<Box<dyn Future<Output = ()>>>;

    let poll_future: PollFuture = if let Some(remote_config) = &remote_config {
        Box::pin(remote::start_poll(
            uuid.clone(),
            remote_config.clone(),
            poll_request_rx.clone(),
            seek_request_tx,
        ))
    } else {
        Box::pin(future::pending())
    };

    // Start remote reporting
    type ReportFuture = Pin<Box<dyn Future<Output = ()>>>;

    let report_future: ReportFuture = if let Some(remote_config) = remote_config {
        Box::pin(remote::start_report(remote_config, local_state_rx))
    } else {
        Box::pin(future::pending())
    };

    // Start state seeking
    type SeekFuture = Pin<Box<dyn Future<Output = Result<(), SeekError>>>>;

    let seek_future: SeekFuture = Box::pin(state::start_seek(
        uuid,
        initial_state,
        proxy_state,
        legacy_config,
        seek_request_rx,
        local_state_tx,
    ));

    // Start main loop and terminate on error
    tokio::select! {
        _ = api_future => Ok(()),
        _ = poll_future => Ok(()),
        _ = report_future => Ok(()),
        res = seek_future => res.map_err(|err| err.into()),
    }
}

mod _config {
    // this module is meant to be private to `main.rs`
    // even though it is visible in submodules due to
    // rust's rules around code visibility.
    //
    // DO NOT feel tempted to import it into your submodule.
    // if you need to read configuration, ask instead your
    // caller to get it for you, asking its caller if needed,
    // all the way up to main.
    //
    // you should also avoid importing and carrying around
    // large structs of config (eg. `RemoteConfig`, etc.).
    // just ask for what you need to be provided as arguments
    // to your function.

    use std::fs;
    use std::io;
    use std::path::PathBuf;

    use serde::{Deserialize, Serialize};
    use thiserror::Error;
    use tracing::debug;

    use super::api::ApiConfig;
    use super::legacy::LegacyConfig;
    use super::remote::RemoteConfig;
    use super::state::models::Uuid;
    use super::util::fs::safe_write_all;

    #[derive(Debug, Error)]
    pub enum LoadConfigError {
        #[error(transparent)]
        Io(#[from] io::Error),

        #[error(transparent)]
        Derialization(#[from] serde_json::Error),
    }

    pub fn load() -> Result<Option<Config>, LoadConfigError> {
        let path = config_dir().join("config.json");

        match fs::read_to_string(&path) {
            Ok(contents) => {
                // We have a previously saved config; use it
                let config = serde_json::from_str::<Config>(&contents)?;
                debug!("Loaded config from {}", path.display());
                Ok(Some(config))
            }
            Err(err) => match err.kind() {
                // We don't have a saved config; we'll create one in this run
                io::ErrorKind::NotFound => Ok(None),

                // We have a config but failed to load it; error out
                _ => Err(err.into()),
            },
        }
    }

    #[derive(Debug, Error)]
    pub enum SaveConfigError {
        #[error(transparent)]
        Io(#[from] io::Error),

        #[error(transparent)]
        Serialization(#[from] serde_json::Error),
    }

    pub async fn save(config: Config) -> Result<(), SaveConfigError> {
        let path = config_dir().join("config.json");
        let buf = serde_json::to_vec(&config)?;

        debug!("Writing config to {}", path.display());
        safe_write_all(path, &buf)?;

        Ok(())
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct Config {
        pub uuid: Uuid,
        pub api: Option<ApiConfig>,
        pub legacy: Option<LegacyConfig>,
        pub remote: Option<RemoteConfig>,
    }

    pub fn config_dir() -> PathBuf {
        if let Some(config_dir) = dirs::config_dir() {
            config_dir.join(env!("CARGO_PKG_NAME"))
        } else {
            // Fallback to home directory if config dir is not available
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
                .join(env!("CARGO_PKG_NAME"))
        }
    }

    pub fn ensure_config_dir() -> io::Result<()> {
        let dir = config_dir();
        let res = fs::create_dir(dir.as_path());
        // create_dir will error if the directory already exists.
        // check if that is the reason it failed.
        if res.is_err() && fs::exists(dir.as_path()).unwrap_or(false) {
            Ok(())
        } else {
            res
        }
    }
}
