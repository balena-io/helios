use anyhow::Result;
use axum::http::uri::PathAndQuery;
use clap::Parser;
use hyper::Uri;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, time::Duration};
use tokio::{net::TcpListener, sync::watch, time::Instant};
use tracing::{debug, info, info_span, instrument, trace, warn, Instrument};
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
mod overrides;
pub mod request;

use cli::{Cli, Commands};
use config::Config;
use fallback::{trigger_legacy_update, FallbackState};
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
            trace!(config = ?config, "configuration loaded");

            start_helios(config).await?;
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

#[instrument(skip_all)]
async fn poll_target(
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
                warn!("poll failed: {e}");
                (None, next_poll_time)
            }
        }
    } else {
        (None, next_poll_time)
    }
}

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

#[instrument(name = "helios", skip_all, err)]
async fn start_helios(config: Config) -> Result<()> {
    info!("starting");

    let mut poll_client = get_poll_client(&config)?;
    let mut next_poll_time = Instant::now();

    if poll_client.is_none() {
        warn!("running in unmanaged mode");
    }

    let fallback_state = FallbackState::new(
        config.uuid.clone(),
        config.remote.api_endpoint.clone(),
        config.fallback.address.clone(),
    );
    let api_fallback_state = fallback_state.clone();
    let overrides = Overrides::new(config.uuid.clone());

    // Set-up a channel to receive update requests coming from the API
    let (update_request_tx, mut update_request_rx) = watch::channel(UpdateRequest::default());

    // Try to bind to the API port first, this will avoid doing an extra poll
    // if the local port is taken
    let address = SocketAddr::new(config.local.address, config.local.port);
    let addr_str = address.to_string();
    let listener = TcpListener::bind(address).await?;
    debug!("bound to local address {addr_str}");

    tokio::spawn(async move {
        info!("starting");
        loop {
            let mut update_req = UpdateRequest::default();
            let target_state = tokio::select! {
                // By default, the loop waits for the next poll
                _ = tokio::time::sleep_until(next_poll_time) => {
                    let (response, next_poll) = poll_target(&mut poll_client, &config).await;
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
                    let (response, next_poll) = poll_target(&mut poll_client, &config).await;
                    next_poll_time = next_poll;
                    update_req = update_request_rx.borrow_and_update().clone();

                    if let Some(tgt) = response {
                        tgt
                    }
                    else {
                        continue;
                    }
                }
            };

            // Override the target state from `/mnt/temp/apps`
            let target_with_overrides = overrides.apply(target_state.clone()).await;

            // Update the global state
            fallback_state.set_target_state(target_with_overrides).await;

            // TODO: try to apply changes

            // trigger an update on the fallback
            let _ = trigger_legacy_update(
                config.fallback.address.clone(),
                config.fallback.api_key.clone(),
                &update_req,
            )
            .await;

            // TODO: report state to API
        }
    }.instrument(info_span!("loop")));

    // Start the API
    api::start(listener, update_request_tx, api_fallback_state).await?;

    Ok(())
}
