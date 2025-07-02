use anyhow::{Context, Result};
use axum::http::uri::PathAndQuery;
use bollard::Docker;
use futures_lite::{future, StreamExt};
use hyper::Uri;
use serde_json::Value;
use std::net::SocketAddr;
use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};
use tokio::net::TcpListener;
use tokio::sync::watch::{self, Receiver};
use tokio::time::Instant;
use tracing::{debug, error, field, info, instrument, warn, Span};

use crate::models::{Device, TargetDevice};
use crate::overrides::Overrides;
use crate::report::Report;
use crate::request::{Get, Patch, RequestConfig};
use crate::{api, Config};
use crate::{
    fallback::{update_legacy, FallbackState},
    UpdateRequest,
};

use mahler::{
    worker::{Ready, SeekStatus, Worker},
    workflow::Interrupt,
};

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

fn get_report_client(config: &Config) -> Result<Option<Patch>> {
    if let Some(uri) = config.remote.api_endpoint.clone() {
        let mut parts = uri.into_parts();
        parts.path_and_query = Some(PathAndQuery::from_maybe_shared("/device/v3/state")?);
        let endpoint = Uri::from_parts(parts)?.to_string();

        let client_config = RequestConfig {
            timeout: config.remote.request_timeout,
            min_interval: config.remote.min_interval,
            max_backoff: config.remote.poll_interval,
            api_token: config.remote.api_key.clone(),
        };

        let client = Patch::new(endpoint, client_config);

        Ok(Some(client))
    } else {
        Ok(None)
    }
}

fn calculate_jitter(max_jitter: &Duration) -> Duration {
    let jitter_ms = rand::random_range(0..=max_jitter.as_millis() as u64);
    Duration::from_millis(jitter_ms)
}

#[instrument(skip_all, fields(success_rate=field::Empty))]
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
        let res = match result {
            Ok(response) if response.modified => (response.value, next_poll_time),
            Ok(_) => (None, next_poll_time),
            Err(e) => {
                warn!("poll failed: {e}");
                (None, next_poll_time)
            }
        };

        let metrics = client.metrics();
        Span::current().record("success_rate", metrics.success_rate());

        res
    } else {
        (None, next_poll_time)
    }
}

#[instrument(skip_all, fields(success_rate=field::Empty))]
async fn send_report(
    report_client: &mut Option<Patch>,
    current_state: Device,
    last_report: Option<Value>,
) -> Option<Value> {
    if let Some(client) = report_client {
        let report: Report = current_state.clone().into();
        let value = match serde_json::to_value(report) {
            Ok(v) => v,
            Err(e) => {
                // This is probably a bug in the types, it shouldn't really happen
                error!("state report serialization failed {e}");
                return last_report;
            }
        };

        // TODO: calculate differences with the last report and just send that
        let res = match client.patch(value.clone()).await {
            Ok(_) => Some(value),
            Err(e) => {
                warn!("patch failed: {e}");
                last_report
            }
        };

        let metrics = client.metrics();
        Span::current().record("success_rate", metrics.success_rate());

        return res;
    }
    last_report
}

async fn load_initial_state(uuid: String) -> Result<Device> {
    // TODO: read initial state from the engine
    Ok(Device {
        uuid,
        images: HashMap::new(),
    })
}

fn create_worker(initial: Device) -> Result<Worker<Device, Ready, TargetDevice>> {
    // Initialize the connection
    let docker = Docker::connect_with_defaults()?;

    Worker::new()
        // TODO: .jobs(...)
        .resource(docker)
        .initial_state(initial)
        .with_context(|| "failed to create initial worker")
}

#[instrument(skip_all, err)]
pub async fn helios(config: Config) -> Result<()> {
    let fallback_state = FallbackState::new(
        config.uuid.clone(),
        config.remote.api_endpoint.clone(),
        config.fallback.address.clone(),
    );
    let api_fallback_state = fallback_state.clone();

    // Set-up a channel to receive update requests coming from the API
    let (update_request_tx, update_request_rx) = watch::channel(UpdateRequest::default());

    // Try to bind to the API port first, this will avoid doing an extra poll
    // if the local port is taken
    let address = SocketAddr::new(config.local.address, config.local.port);
    let addr_str = address.to_string();
    let listener = TcpListener::bind(address).await?;
    debug!("bound to local address {addr_str}");

    // Start the API and the main loop and terminate on any error
    tokio::select! {
        res = api::start(listener, update_request_tx, api_fallback_state) => res,
        res = start(config, fallback_state, update_request_rx) => res
    }
}

#[instrument(name = "main", skip_all, err)]
pub async fn start(
    config: Config,
    fallback_state: FallbackState,
    mut update_request_rx: Receiver<UpdateRequest>,
) -> Result<()> {
    info!("starting");

    let mut poll_client = get_poll_client(&config)?;
    let mut report_client = get_report_client(&config)?;
    let mut next_poll_time = Instant::now();

    if poll_client.is_none() {
        warn!("running in unmanaged mode");
    }

    // NOTE: still uncertain if we need the overrides at this point
    let overrides = Overrides::new(config.uuid.clone());
    let initial_state = load_initial_state(config.uuid.clone()).await?;

    // Report initial state
    let mut last_report = send_report(&mut report_client, initial_state.clone(), None).await;

    // Create a mahler Worker instance
    let mut worker = create_worker(initial_state)?;

    // Start following changes
    let mut worker_stream = worker.follow();

    let mut update_req = UpdateRequest::default();
    let mut skip_worker_apply = false;

    // Main loop, polls state, applies changes and reports state
    // operations may be interrupted by an update request or a new target state
    // coming from the API
    loop {
        let (target_device, fallback_target) = tokio::select! {
            // By default, the loop waits for the next poll
            _ = tokio::time::sleep_until(next_poll_time) => {
                let (response, next_poll) = poll_target(&mut poll_client, &config).await;
                next_poll_time = next_poll;
                // reset the update request
                update_req = UpdateRequest::default();

                if let Some(fallback_tgt) = response {
                    // Apply overrides
                    let fallback_tgt = overrides.apply(fallback_tgt.clone()).await;

                    // TODO: we'll need to reject the target if it cannot be deserialized
                    let tgt_device = serde_json::from_value::<TargetDevice>(fallback_tgt.clone()).ok();
                    (tgt_device, Some(fallback_tgt))
                }
                else {
                    continue;
                }
            }
            // An update request coming from the API can short circuit the timer
            update_requested = update_request_rx.changed() => {
                update_requested?;

                // Get the target state and reset the timer
                let (response, next_poll) = poll_target(&mut poll_client, &config).await;
                next_poll_time = next_poll;
                update_req = update_request_rx.borrow_and_update().clone();

                if let Some(fallback_tgt) = response {
                    // Apply overrides
                    let fallback_tgt = overrides.apply(fallback_tgt.clone()).await;
                    let tgt_device = serde_json::from_value::<TargetDevice>(fallback_tgt.clone()).ok();
                    (tgt_device, Some(fallback_tgt))
                }
                else {
                    continue;
                }
            }
        };

        // try to apply changes if any, we skip this step if the last iteration got cancelled while
        // waiting for the legacy supervisor to apply changes
        if !skip_worker_apply {
            if let Some(device) = target_device {
                let interrupt = Interrupt::new();
                let mut seek_future =
                    Box::pin(worker.seek_with_interrupt(device, interrupt.clone()));

                // Create dummy future that will never return
                let mut patch: Pin<Box<dyn Future<Output = Option<Value>> + Send>> =
                    Box::pin(future::pending::<Option<Value>>());
                let mut has_pending_patch = false;

                worker = loop {
                    tokio::select! {
                        // Wait for the seek operation to finish
                        seek_res = &mut seek_future => {
                            // break the main loop if a fatal error happens with the seek state
                            // call. If that happens there is either a loop in a type or task here or
                            // withih mahler.
                            // See: https://github.com/balena-io-modules/mahler-rs/blob/main/src/worker/mod.rs#L42-L66
                            // TODO: depending on the resulting status we may want to retry
                            break seek_res?
                        }

                        // Follow state changes
                        stream_res = worker_stream.next() => {
                            if let Some(current_state) = stream_res {
                                // Drop the previous patch if a new state comes before the previous one is finished
                                drop(patch);

                                // Report state changes to the API
                                patch = Box::pin(send_report(&mut report_client, current_state, last_report.clone()));
                                has_pending_patch = true;
                            }
                        }

                        // If the patch completes then update the last report
                        report = &mut patch, if has_pending_patch => {
                            last_report = report;

                            // Create dummy future that will never return
                            patch = Box::pin(future::pending::<Option<Value>>());
                            has_pending_patch = false;
                        }

                        // a new update request may interrupt the seek_target call
                        update_requested = update_request_rx.changed() => {
                            if update_requested.is_err() {
                                // channel is closed but we want to terminate the state apply
                                continue;
                            }

                            // Interrupt the seek call if indicated in the request
                            update_req = update_request_rx.borrow_and_update().clone();
                            if let UpdateRequest {cancel: true, ..} = update_req {
                                interrupt.trigger();
                            }
                        }
                    }
                };

                // Wait for the patch_future to finish
                if has_pending_patch {
                    last_report = patch.await;
                }
            }
        }

        // Update the global state and trigger an update on the legacy supervisor
        // NOTE: this fallback block should dissapear entirely once we can remove the
        // legacy supervisor
        if let Some(fallback) = fallback_target {
            // Skip supervisor state apply if a cancellation happened
            if matches!(worker.status(), SeekStatus::Interrupted) {
                continue;
            }

            // Trigger a /v1/update on the fallback and wait for the state to
            // be applied
            if let Some(fallback_uri) = config.fallback.address.clone() {
                fallback_state.set_target_state(fallback).await;

                let mut legacy_update = Box::pin(update_legacy(
                    fallback_uri.clone(),
                    config.fallback.api_key.clone(),
                    &update_req,
                ));

                // Wait for the legacy update to finish or an new update request
                tokio::select! {
                    _ = &mut legacy_update => {
                        skip_worker_apply = false;
                    },

                    // A new call to /v1/update will need to trigger a new poll
                    // and interrupt the legacy update
                    update_requested = update_request_rx.changed() => {
                        // If the channel closes we can terminate the main loop
                        update_requested?;

                        // Drop the legacy update future
                        drop(legacy_update);

                        // The next iteration of the loop should come straight
                        // to the legacy update
                        skip_worker_apply = false;

                        // Go straight to the next loop iteration
                        continue;
                    }
                }

                // Since the state changed outside of the worker we need to re-load
                // it and re-initialize the worker
                let initial_state = load_initial_state(config.uuid.clone()).await?;

                worker = create_worker(initial_state)?;
                worker_stream = worker.follow();
            }
        }
    }
}
