use anyhow::{Context, Result};
use axum::http::uri::PathAndQuery;
use bollard::Docker;
use futures_lite::{future, StreamExt};
use hyper::Uri;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};
use tokio::sync::watch::Receiver;
use tokio::time::Instant;
use tracing::{error, field, info, instrument, trace, warn, Span};

use crate::config::Config;
use crate::fallback::{legacy_update, FallbackState};
use crate::request::{Get, Patch, RequestConfig};

use mahler::worker::{Ready, SeekError, SeekStatus, Worker};
use mahler::workflow::Interrupt;

type Uuid = String;

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
struct Image {
    docker_id: Option<String>,
}

/// Current state of a device
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
struct Device {
    /// The device UUID
    pub uuid: Uuid,

    /// List of docker images on the device
    pub images: HashMap<String, Image>,
}

/// Target state of a device
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
struct TargetDevice {}

/// An update request coming from the API
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct UpdateRequest {
    /// Trigger an update ignoring locks
    #[serde(default)]
    pub force: bool,

    /// Cancel the current update if any
    #[serde(default)]
    pub cancel: bool,
}

#[derive(Serialize, Debug, Clone)]
struct DeviceReport {}

impl From<Device> for DeviceReport {
    fn from(_: Device) -> Self {
        // TODO
        DeviceReport {}
    }
}

// The state for report
#[derive(Serialize, Debug, Clone)]
struct Report(HashMap<String, DeviceReport>);

impl From<Device> for Report {
    fn from(device: Device) -> Self {
        Report(HashMap::from([(device.uuid.clone(), device.into())]))
    }
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

// Return type from poll_remote
type PollResult = (Option<serde_json::Value>, Instant);

#[instrument(skip_all, fields(success_rate=field::Empty))]
async fn poll_remote(poll_client: &mut Option<Get>, config: &Config) -> PollResult {
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

// Return type from send_report
type LastReport = Option<Value>;

#[instrument(skip_all, fields(success_rate=field::Empty))]
async fn send_report(
    report_client: &mut Option<Patch>,
    current_state: Device,
    last_report: LastReport,
) -> LastReport {
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

// Helper types to make the code a bit more readable
type LegacyFuture = Pin<Box<dyn Future<Output = Result<()>>>>;
type SeekResult = Result<SeekStatus, SeekError>;

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

    // Load the current state at start time
    let initial_state = load_initial_state(config.uuid.clone()).await?;

    // Create a mahler Worker instance
    // and start following changes
    let mut worker = create_worker(initial_state.clone())?;
    let mut worker_stream = worker.follow();

    // Store the last update request
    let mut update_req = UpdateRequest::default();

    // Reporting variables
    let mut report_future: Pin<Box<dyn Future<Output = LastReport>>> =
        Box::pin(send_report(&mut report_client, initial_state, None));
    let mut last_report: Option<Value> = None;

    // Seek target state
    let mut seek_future: Pin<Box<dyn Future<Output = SeekResult>>> = Box::pin(future::pending());
    let mut is_apply_pending = false;
    let mut interrupt = Interrupt::new();

    // Legacy update variables
    let mut legacy_update_future: LegacyFuture = Box::pin(future::pending());
    let mut is_legacy_pending = false;

    // Poll trigger variables
    let mut poll_future: Pin<Box<dyn Future<Output = PollResult>>> =
        Box::pin(poll_remote(&mut poll_client, &config));
    let mut is_poll_pending = true;

    // Main loop, polls state, applies changes and reports state
    // operations may be interrupted by an update request or a new target state
    // coming from the API
    loop {
        tokio::select! {
            // Wake up on poll if not applying changes
            _ = tokio::time::sleep_until(next_poll_time), if !is_apply_pending && !is_legacy_pending && !is_poll_pending => {
                // Start the poll future
                drop(poll_future);
                poll_future = Box::pin(poll_remote(&mut poll_client, &config));
                is_poll_pending = true;

                // Reset the update request
                update_req = UpdateRequest::default();
            }

            // Handle poll completion
            (response, next_poll) = &mut poll_future => {
                // Reset the polling state
                is_poll_pending = false;
                poll_future = Box::pin(future::pending());
                next_poll_time = next_poll;

                if let Some(target_state) = response {
                    // Set the fallback target for legacy supervisor requests
                    fallback_state.set_target_state(target_state.clone()).await;

                    // TODO: we'll need to reject the target if it cannot be deserialized
                    let tgt_device_opt = serde_json::from_value::<TargetDevice>(target_state).ok();

                    // Apply the state unless waiting for a legacy apply
                    if let (Some(target_device), false) = (tgt_device_opt, is_legacy_pending) {
                        drop(seek_future);
                        interrupt = Interrupt::new();
                        seek_future = Box::pin(worker.seek_with_interrupt(target_device, interrupt.clone()));
                        is_apply_pending = true;
                    }
                    else if let Some(fallback_uri) = config.fallback.address.clone() {
                        // If we get here we are coming from a cancellation, so trigger
                        // the legacy supervisor immediately
                        legacy_update_future = Box::pin(legacy_update(
                            fallback_uri.clone(),
                            config.fallback.api_key.clone(),
                            update_req.clone(),
                        ));
                        is_legacy_pending = true;
                    }
                }
            }

            // Wake on update request
            update_requested = update_request_rx.changed() => {
                if update_requested.is_err() {
                    // Not really an error, it just means the API closed
                    trace!("request channel closed");
                    break;
                }

                // Read the request value without marking it as updated
                // If a cancellation request is received and there is an apply pending
                // then interrupt the previous apply
                match (&*update_request_rx.borrow(), is_apply_pending, is_legacy_pending) {
                    (UpdateRequest {cancel: true, ..}, true, _) => {
                        // interrupt the existing target and wait for it to finish
                        interrupt.trigger();

                        // Wait for the interrupt to be processed
                        seek_future.await?;

                        // Reset the future
                        seek_future = Box::pin(future::pending());
                        is_apply_pending = false;
                    }
                    (UpdateRequest {cancel: true, ..}, _, true) => {
                        // Drop the legacy future but don't reset the
                        // flag so a new legacy update can be triggered from the new poll
                        legacy_update_future = Box::pin(future::pending());
                    }
                    // If there is no apply in progress then proceed
                    (_, false, false) => {}
                    // If no cancellation was requested then wait for any apply to finish
                    // before triggering a new poll
                    _ => continue
                }

                // Trigger a new poll
                drop(poll_future);
                poll_future = Box::pin(poll_remote(&mut poll_client, &config));
                is_poll_pending = true;
                update_req = update_request_rx.borrow_and_update().clone();
            }

            // State changes should trigger a new patch
            stream_res = worker_stream.next() => {
                if let Some(current_state) = stream_res {
                    // Drop the previous patch if a new state comes before the previous one is finished
                    // rate limiting is handled by the client
                    drop(report_future);

                    // Report state changes to the API
                    report_future = Box::pin(send_report(&mut report_client, current_state, last_report.clone()));
                }
            }

            // Update the last report on state patch
            report = &mut report_future => {
                last_report = report;
                report_future = Box::pin(future::pending());
            }

            // Wake up when apply returns
            seek_status = &mut seek_future => {
                // Reset the state
                is_apply_pending = false;
                seek_future = Box::pin(future::pending());

                // break the main loop if a fatal error happens with the seek state
                // call. If that happens there is either a loop in a type or task here or
                // within mahler.
                // See: https://github.com/balena-io-modules/mahler-rs/blob/main/src/worker/mod.rs#L42-L66
                // TODO: depending on the resulting status we will want to retry, e.g. on Aborted
                // status that indicates an error happened when executing a task
                seek_status?;

                // No matter the result of the seek_target call, we need to call the legacy
                // supervisor to re-apply the target
                if let Some(fallback_uri) = config.fallback.address.clone() {
                    drop(legacy_update_future);
                    legacy_update_future = Box::pin(legacy_update(
                        fallback_uri.clone(),
                        config.fallback.api_key.clone(),
                        update_req.clone(),
                    ));
                    is_legacy_pending = true;
                }
            }

            // Wake up when the supervisor apply is settled
            _ = &mut legacy_update_future => {
                // Reset the state
                is_legacy_pending = false;
                legacy_update_future = Box::pin(future::pending());

                // Legacy future and seek future are exclusive so we can drop this safely
                drop(seek_future);
                seek_future = Box::pin(future::pending());

                // We need to create a new worker with the updated state as it
                // may have been changed by the legacy supervisor
                let initial_state = load_initial_state(config.uuid.clone()).await?;
                worker = create_worker(initial_state)?
            }


        }
    }

    info!("terminating");
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn it_creates_a_device_report_from_a_device() {
        let device = Device {
            uuid: "test-uuid".to_string(),
            images: HashMap::new(),
        };

        let report: Report = device.into();

        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value, json!({"test-uuid": {}}))
    }
}
