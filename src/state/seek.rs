use futures_lite::StreamExt;
use mahler::{
    worker::{SeekError as WorkerSeekError, SeekStatus},
    workflow::Interrupt,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    future::{self, Future},
    pin::Pin,
    sync::Arc,
};
use tokio::sync::{watch::Receiver, RwLock};
use tracing::{error, info, instrument, trace};

use crate::config::{Config, FallbackConfig};
use crate::fallback::{legacy_update, FallbackError, FallbackState};
use crate::remote::report::{
    get_report_client, send_report_if_managed, DeviceReport, LastReport, Report,
};

use super::models::{Device, TargetDevice, TargetStatus};
use super::worker::{create, CreateError as WorkerCreateError, LocalWorker};

impl From<Device> for DeviceReport {
    fn from(_: Device) -> Self {
        // TODO
        DeviceReport {}
    }
}

impl From<Device> for Report {
    fn from(device: Device) -> Self {
        Report::new(HashMap::from([(device.uuid.clone().into(), device.into())]))
    }
}

impl From<SeekStatus> for TargetStatus {
    fn from(status: SeekStatus) -> Self {
        match status {
            SeekStatus::Success => TargetStatus::Applied,
            SeekStatus::NotFound => TargetStatus::NotFound,
            SeekStatus::Interrupted => TargetStatus::Interrupted,
            SeekStatus::Aborted(errors) => {
                TargetStatus::Aborted(errors.iter().map(|e| e.to_string()).collect())
            }
        }
    }
}

struct InnerState {
    device: Device,
    status: TargetStatus,
}

#[derive(Clone)]
pub struct CurrentState(Arc<RwLock<InnerState>>);

impl CurrentState {
    fn new(device: Device) -> Self {
        Self(Arc::new(RwLock::new(InnerState {
            device,
            status: TargetStatus::default(),
        })))
    }

    pub async fn read(&self) -> Device {
        let state = self.0.read().await;
        state.device.clone()
    }

    async fn write(&self, device: Device) {
        let mut state = self.0.write().await;
        state.device = device;
    }

    pub async fn status(&self) -> TargetStatus {
        let state = self.0.read().await;
        state.status.clone()
    }

    async fn set_status(&self, status: TargetStatus) {
        let mut state = self.0.write().await;
        state.status = status;
    }
}

impl From<Device> for CurrentState {
    fn from(value: Device) -> Self {
        CurrentState::new(value)
    }
}

/// Options for controlling processing of a new target
/// by the main loop
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UpdateOpts {
    /// Ignore locks on the next apply
    /// defaults to false
    #[serde(default)]
    pub force: bool,

    /// Cancel the current update if any
    /// defaults to true, unless the value is coming
    /// from the API for backwards compatibility
    #[serde(default = "api_cancel_default")]
    pub cancel: bool,
}

fn api_cancel_default() -> bool {
    false
}

impl Default for UpdateOpts {
    fn default() -> Self {
        Self {
            force: false,
            cancel: true,
        }
    }
}

/// A request to reach a target state.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SeekRequest {
    pub target: TargetDevice,
    pub raw_target: Option<Value>,
    pub opts: UpdateOpts,
}

#[derive(Clone)]
enum SeekState {
    Reset,
    Local,
    Fallback,
}

struct SeekConfig {
    tgt_device: TargetDevice,
    update_opts: UpdateOpts,
    fallback: FallbackConfig,
    raw_target: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum SeekError {
    #[error("Failed to create worker: {0}")]
    CreateWorker(#[from] WorkerCreateError),

    #[error("Failed to reach target state: {0}")]
    SeekTargetState(#[from] WorkerSeekError),

    #[error("Failed to update state on legacy Supervisor: {0}")]
    LegacyUpdate(#[from] FallbackError),
}

type SeekResult = Result<SeekState, SeekError>;

async fn seek_target(
    worker: &mut LocalWorker,
    config: SeekConfig,
    interrupt: Interrupt,
    cur_state: &CurrentState,
    fallback_state: &FallbackState,
    prev_state: SeekState,
) -> SeekResult {
    if matches!(prev_state, SeekState::Local | SeekState::Reset) {
        // Reset the target state so a supervisor poll cannot
        // lead to a double apply
        fallback_state.clear_target_state().await;

        // Set the loop status
        cur_state.set_status(TargetStatus::Applying).await;

        // Apply the target
        let status = tokio::select! {
            status = worker
            .seek_target(config.tgt_device.clone()) => status?,
            _ = interrupt.wait() => SeekStatus::Interrupted
        };

        let interrupted = matches!(status, SeekStatus::Interrupted);
        // Update the status
        cur_state.set_status(status.into()).await;

        if interrupted {
            return Ok(SeekState::Local);
        }
    }

    // No matter the result of the seek_target call, we need to call the legacy
    // supervisor to re-apply the target
    if let (Some(uri), Some(target_state)) = (config.fallback.address.clone(), config.raw_target) {
        // Set as the target state the raw target accepted by
        // the fallback
        fallback_state.set_target_state(target_state).await;
        let interrupted = tokio::select! {
            res = legacy_update(uri, config.fallback.api_key.clone(), config.update_opts.force, config.update_opts.cancel) => {
                res?;
                false
            },
            _ = interrupt.wait() => true
        };

        if interrupted {
            return Ok(SeekState::Fallback);
        }

        // Tell the caller that they need to reset the worker
        return Ok(SeekState::Reset);
    }

    Ok(SeekState::Local)
}

#[instrument(name = "seek", skip_all, err)]
pub async fn start_seek(
    config: &Config,
    current_state: CurrentState,
    fallback_state: FallbackState,
    mut seek_rx: Receiver<SeekRequest>,
) -> Result<(), SeekError> {
    info!("waiting for target");

    let mut report_client = get_report_client(config);

    // Read the initial state
    let initial_state = current_state.read().await;

    // Create a mahler Worker instance
    // and start following changes
    let mut worker = create(initial_state.clone())?;
    let mut worker_stream = worker.follow();

    // Reporting variables
    let mut report_future: Pin<Box<dyn Future<Output = LastReport>>> = Box::pin(
        send_report_if_managed(&mut report_client, initial_state.into(), None),
    );
    let mut last_report: Option<Value> = None;

    // Seek target state
    let mut prev_seek_state = SeekState::Local;
    let mut seek_future: Pin<Box<dyn Future<Output = SeekResult>>> = Box::pin(future::pending());
    let mut is_apply_pending = false;
    let mut interrupt = Interrupt::new();

    // The next queued target state
    let mut next_target: (Option<TargetDevice>, UpdateOpts, Option<Value>) =
        (None, UpdateOpts::default(), None);

    // Main loop, polls state, applies changes and reports state
    // operations may be interrupted by an update request or a new target state
    // coming from the API
    loop {
        tokio::select! {
            // Wake on update request
            update_requested = seek_rx.changed() => {
                if update_requested.is_err() {
                    // Not really an error, it just means the API closed
                    trace!("request channel closed");
                    break;
                }

                let update_req = seek_rx.borrow_and_update().clone();

                if is_apply_pending {
                    // A new target came while applying.
                    // Interrupt the target if we are asked to cancel.
                    if update_req.opts.cancel {
                        // interrupt the existing target and wait for it to finish
                        interrupt.trigger();
                        prev_seek_state = seek_future.await?;

                        current_state.set_status(TargetStatus::Interrupted).await;

                        // Reset the future
                        seek_future = Box::pin(future::pending());
                    }
                    // Otherwise just store the target state for the next iteration
                    else {
                        next_target = (
                            Some(update_req.target.clone()),
                            update_req.opts.clone(),
                            update_req.raw_target.clone(),
                        );
                        continue;
                    }
                }

                // Trigger a new target state apply if we got here
                drop(seek_future);
                interrupt = Interrupt::new();
                let seek_config = SeekConfig {
                    tgt_device: update_req.target,
                    fallback: config.fallback.clone(),
                    update_opts: update_req.opts,
                    raw_target: update_req.raw_target,
                };
                seek_future = Box::pin(
                    seek_target(&mut worker, seek_config,
                                interrupt.clone(), &current_state,
                                &fallback_state, prev_seek_state.clone()
                    )
                );
                is_apply_pending = true;
            }

            // State changes should trigger a new patch
            stream_res = worker_stream.next() => {
                // The stream should not return None unless the worker is dropped
                // so this should not panic unless there is a bug
                let cur_state = stream_res.expect("worker stream should remain open");

                // Drop the previous patch if a new state comes before the previous one is finished
                // rate limiting is handled by the client
                drop(report_future);

                // Record the updated global state
                current_state.write(cur_state.clone()).await;

                // Report state changes to the API
                report_future = Box::pin(send_report_if_managed(&mut report_client, cur_state.into(), last_report.clone()));
            }

            // Update the last report on state patch
            report = &mut report_future => {
                last_report = report;
                report_future = Box::pin(future::pending());
            }

            // Wake up when apply returns
            seek_res = &mut seek_future => {
                // Reset the state
                is_apply_pending = false;
                seek_future = Box::pin(future::pending());

                // break the main loop if a fatal error happens with the seek state
                // call. If that happens there is either a loop in a type or task here or
                // within mahler.
                // See: https://github.com/balena-io-modules/mahler-rs/blob/main/src/worker/mod.rs#L42-L66
                prev_seek_state = seek_res?;

                // if the legacy apply went through
                if matches!(prev_seek_state, SeekState::Reset) {
                    // We need to create a new worker with the updated state as it
                    // may have been changed by the legacy supervisor
                    let initial_state = Device::initial_for(config.uuid.clone());

                    // Update the global state
                    current_state.write(initial_state.clone()).await;
                    worker = create(initial_state)?;
                    worker_stream = worker.follow();
                }

                // If there is a pending target
                if let (Some(target), opts, raw_target) = next_target {
                    // reset the next target
                    next_target = (None, UpdateOpts::default(), None);
                    interrupt = Interrupt::new();
                    let seek_config = SeekConfig {
                        tgt_device: target,
                        fallback: config.fallback.clone(),
                        update_opts: opts,
                        raw_target
                    };

                    // Trigger a new apply
                    seek_future = Box::pin(
                        seek_target(&mut worker, seek_config,
                                    interrupt.clone(), &current_state,
                                    &fallback_state, prev_seek_state.clone()
                        )
                    );
                    is_apply_pending = true;
                }
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
            uuid: "test-uuid".to_string().into(),
            images: HashMap::new(),
            apps: HashMap::new(),
            config: HashMap::new(),
        };

        let report: Report = device.into();

        let value: Value = report.into();
        assert_eq!(value, json!({"test-uuid": {}}))
    }
}
