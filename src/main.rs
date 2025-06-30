use anyhow::Result;
use axum::http::uri::PathAndQuery;
use clap::Parser;
use hyper::Uri;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{watch, RwLock},
    time::Instant,
};
use tracing::{debug, field, info, instrument, warn, Span};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

mod api;
mod cli;
mod config;
pub mod control;
mod overrides;
pub mod request;

use api::Api;
use cli::{Cli, Commands};
use config::Config;
use overrides::Overrides;
use request::{Get, RequestConfig};

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

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Register { .. }) => {
            // TODO: Implement register command
            todo!();
        }
        None => {
            // Main service mode
            let config = Config::load(&cli)?;
            info!("Configuration loaded successfully");
            debug!("{:#?}", config);

            start_supervising(config).await?;
        }
    }

    Ok(())
}

fn get_poll_client(config: &Config) -> Result<Option<Get>> {
    if let Some(uri) = config.remote.api_endpoint.clone() {
        let mut parts = uri.into_parts();
        parts.path_and_query = Some(PathAndQuery::from_maybe_shared(format!(
            "/device/v3/{}/state",
            config.uuid
        ))?);
        let endpoint = Uri::from_parts(parts)?.to_string();

        let client_config = RequestConfig {
            timeout: config.remote.request_timeout,
            min_interval: config.remote.min_interval,
            max_backoff: config.remote.poll_interval,
            api_token: config.remote.api_key.clone(),
        };

        let client = Get::new(endpoint, client_config);

        Ok(Some(client))
    } else {
        Ok(None)
    }
}

fn calculate_jitter(max_jitter: &Duration) -> Duration {
    let jitter_ms = rand::random_range(0..=max_jitter.as_millis() as u64);
    Duration::from_millis(jitter_ms)
}

async fn fetch_target_state(
    poll_client: &mut Option<Get>,
    config: &Config,
) -> (Option<serde_json::Value>, Instant) {
    let jitter = calculate_jitter(&config.remote.max_poll_jitter);
    let mut next_poll_time = Instant::now() + config.remote.poll_interval + jitter;

    // poll if we have a client
    if let Some(ref mut client) = poll_client {
        let result = client.get().await;

        // Reset the poll timer after we get the response
        next_poll_time = Instant::now() + config.remote.poll_interval + jitter;
        match result {
            Ok(response) if response.modified => (response.value, next_poll_time),
            Ok(_) => (None, next_poll_time),
            Err(e) => {
                warn!("Target state poll failed: {e}");
                (None, next_poll_time)
            }
        }
    } else {
        (None, next_poll_time)
    }
}

/// Notify the fallback service of a state update via POST to /v1/update
#[instrument(skip_all, fields(force = request.force, cancel = request.cancel, response = field::Empty), err)]
async fn notify_fallback(config: &Config, request: &UpdateRequest) -> Result<()> {
    let client = reqwest::Client::new();
    let fallback_address = if let Some(addr) = config.fallback.address.clone() {
        addr
    } else {
        Span::current().record("response", 404);
        return Ok(());
    };

    // Build the URI from the address parts
    let mut addr_parts = fallback_address.into_parts();
    addr_parts.path_and_query = if let Some(apikey) = config.fallback.api_key.clone() {
        Some(PathAndQuery::from_maybe_shared(format!(
            "/v1/update?apikey={apikey}",
        ))?)
    } else {
        Some(PathAndQuery::from_maybe_shared("/v1/update")?)
    };
    let url = Uri::from_parts(addr_parts)?.to_string();

    let payload = json!({
        "force": request.force,
        "cancel": request.cancel
    });

    let response = client.post(&url).json(&payload).send().await?;

    Span::current().record("response", response.status().as_u16());

    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[allow(unused)]
/// An update request coming from the API
struct UpdateRequest {
    #[serde(default)]
    /// Trigger an update ignoring locks
    force: bool,
    #[serde(default)]
    /// Cancel the current update if any
    cancel: bool,
}

// This will become a proper type
pub type TargetState = serde_json::Value;

/// The supervisor state
struct InnerState {
    // TODO: add status
    // TODO: add current_state

    // The current target state
    target_state: Option<TargetState>,
}

#[derive(Clone)]
// Global service state.
//
// This is shared between the main loop and the API and is used for keeping state synchronized
// between those two threads.
//
// Only the main loop can update the global state
pub struct GlobalState(Arc<RwLock<InnerState>>);

impl GlobalState {
    fn new() -> Self {
        Self(Arc::new(RwLock::new(InnerState { target_state: None })))
    }

    pub async fn target_state(&self) -> Option<TargetState> {
        let inner = self.0.read().await;
        inner.target_state.clone()
    }

    async fn set_target_state(&self, target_state: TargetState) {
        let mut inner = self.0.write().await;
        inner.target_state = Some(target_state)
    }
}

async fn start_supervising(config: Config) -> Result<()> {
    info!("Service started");

    let mut poll_client = get_poll_client(&config)?;
    let mut next_poll_time = Instant::now();

    if poll_client.is_none() {
        warn!("No remote API endpoint provided, running in unmanaged mode");
    }

    let global_state = GlobalState::new();
    let overrides = Overrides::new(config.uuid.clone());

    // Set-up a channel to receive target state requests coming from the API
    // TODO: we still need to implement this endpoint
    let (target_state_tx, mut target_state_rx) = watch::channel::<Option<TargetState>>(None);

    // Set-up a channel to receive update requests coming from the API
    let (update_request_tx, mut update_request_rx) =
        watch::channel::<UpdateRequest>(UpdateRequest::default());

    let api = Api::new(
        &config,
        global_state.clone(),
        target_state_tx,
        update_request_tx,
    );

    // Try to bind to the API port first, this will avoid doing an extra poll
    // if the local port is taken
    let address = SocketAddr::new(config.local.address, config.local.port);
    let addr_str = address.to_string();
    let listener = TcpListener::bind(address).await?;
    debug!("Bound to local address {addr_str}");

    tokio::spawn(async move {
        loop {
            let mut update_req = UpdateRequest::default();
            let target_state = tokio::select! {
                // By default, the loop waits for the next poll
                _ = tokio::time::sleep_until(next_poll_time) => {
                    let (response, next_poll) = fetch_target_state(&mut poll_client, &config).await;
                    next_poll_time = next_poll;

                    if let Some(tgt) = response {
                        tgt
                    }
                    else {
                        continue;
                    }
                }
                // An update request coming from the API can short circuit the timer
                changed = update_request_rx.changed() => {
                    if changed.is_err() {
                        // channel closed
                        break;
                    }

                    // Get the target state and reset the timer
                    let (response, next_poll) = fetch_target_state(&mut poll_client, &config).await;
                    next_poll_time = next_poll;
                    update_req = update_request_rx.borrow_and_update().clone();

                    if let Some(tgt) = response {
                        tgt
                    }
                    else {
                        continue;
                    }
                }
                // A new target state may also come from the API
                changed = target_state_rx.changed() => {
                    if changed.is_err() {
                        // channel closed
                        break;
                    }

                    if let Some(tgt) = target_state_rx.borrow_and_update().clone() {
                        tgt
                    }
                    else {
                        continue
                    }
                }
            };

            // Override the target state from `/mnt/temp/apps`
            let target_with_overrides = overrides.apply(target_state.clone()).await;

            // Update the global state
            global_state.set_target_state(target_with_overrides).await;

            // TODO: try to apply changes

            // trigger an update on the fallback
            let _ = notify_fallback(&config, &update_req).await;

            // TODO: report state to API
        }
    });

    // Start the API
    api.start(listener).await?;

    Ok(())
}
