use futures_lite::StreamExt;
use mahler::{
    worker::{SeekError as WorkerSeekError, SeekStatus},
    workflow::Interrupt,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
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
    /// Ignore locks on the next apply.
    ///
    /// Defaults to false
    #[serde(default)]
    pub force: bool,

    /// Cancel the current update if any.
    ///
    /// Defaults to true, unless the value is coming
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

#[derive(Debug, thiserror::Error)]
pub enum SeekError {
    #[error("Failed to create worker: {0}")]
    CreateWorker(#[from] WorkerCreateError),

    #[error("Failed to reach target state: {0}")]
    SeekTargetState(#[from] WorkerSeekError),

    #[error("Failed to update state on legacy Supervisor: {0}")]
    LegacyUpdate(#[from] FallbackError),
}

#[derive(Debug, Clone)]
enum SeekState {
    Reset,
    Local,
    Fallback,
}

type SeekResult = Result<SeekState, SeekError>;

// input to this function must be trimmed down.
// ignore the lint warning until then.
#[allow(clippy::too_many_arguments)]
async fn apply_target(
    worker: &mut LocalWorker,
    target: TargetDevice,
    raw_target: Option<Value>,
    update_opts: UpdateOpts,
    fallback: &FallbackConfig,
    interrupt: Interrupt,
    cur_state: &CurrentState,
    fallback_state: &FallbackState,
    prev_seek_state: SeekState,
) -> SeekResult {
    if matches!(prev_seek_state, SeekState::Local | SeekState::Reset) {
        // Reset the target state so a supervisor poll cannot
        // lead to a double apply
        fallback_state.clear_target_state().await;

        // Set the loop status
        cur_state.set_status(TargetStatus::Applying).await;

        // Apply the target
        let status = tokio::select! {
            status = worker.seek_target(target.clone()) => status?,
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
    if let (Some(uri), Some(target_state)) = (fallback.address.clone(), raw_target) {
        // Set as the target state the raw target accepted by
        // the fallback
        fallback_state.set_target_state(target_state).await;
        let interrupted = tokio::select! {
            res = legacy_update(uri, fallback.api_key.clone(), update_opts.force, update_opts.cancel) => {
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

#[derive(Debug, Clone)]
enum SeekAction {
    Terminate,
    Idle(LastReport),
    Apply(SeekRequest),
    Report(Device),
    Complete(SeekState),
}

impl fmt::Display for SeekAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let action = match self {
            Self::Terminate => "terminate",
            Self::Idle(_) => "idle",
            Self::Apply(_) => "apply",
            Self::Report(_) => "report",
            Self::Complete(_) => "complete",
        };
        f.write_str(action)
    }
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

    // Create a mahler worker and start following changes
    let mut worker = create(initial_state.clone())?;
    let mut worker_stream = worker.follow();

    // Reporting variables
    let mut report_future: Pin<Box<dyn Future<Output = LastReport>>> = Box::pin(
        send_report_if_managed(&mut report_client, initial_state.into(), None),
    );
    let mut last_report = LastReport::None;

    // Seek target state
    let mut prev_seek_state = SeekState::Local;
    let mut apply_future: Pin<Box<dyn Future<Output = SeekResult>>> = Box::pin(future::pending());
    let mut is_apply_pending = false;
    let mut interrupt = Interrupt::new();

    // The next queued target state
    let mut next_target: Option<SeekRequest> = None;

    // Main loop, applies changes and reports state.
    // Operations may be interrupted by a new seek request.
    loop {
        let action: SeekAction = tokio::select! {
            // Wake on update request
            update_requested = seek_rx.changed() => {
                if update_requested.is_err() {
                    // Not really an error, it just means the API closed
                    trace!("request channel closed");
                    SeekAction::Terminate
                } else {
                    let update_req = seek_rx.borrow_and_update().clone();
                    SeekAction::Apply(update_req)
                }
            }

            // State changes should trigger a new patch
            new_state = worker_stream.next() => {
                // The stream should not return None unless the worker is dropped
                // so this should not panic unless there is a bug
                SeekAction::Report(new_state.expect("worker stream should remain open"))
            }

            // Update the last report on state patch
            report = &mut report_future => SeekAction::Idle(report),

            // Wake up when apply returns
            seek_res = &mut apply_future => {
                // break the main loop if a fatal error happens with the seek state call.
                // If that happens there is either a loop in a type or task here or
                // within mahler.
                // See: https://github.com/balena-io-modules/mahler-rs/blob/main/src/worker/mod.rs#L42-L66
                let seek_state = seek_res?;

                SeekAction::Complete(seek_state)
            }
        };

        trace!("performing action: {action}");

        match action {
            SeekAction::Terminate => {
                break;
            }

            SeekAction::Idle(report) => {
                last_report = report;
                report_future = Box::pin(future::pending());
            }

            SeekAction::Report(state) => {
                // Drop the previous patch if a new state comes before the previous one
                // is finished. rate limiting is handled by the client
                drop(report_future);

                // Record the updated global state
                current_state.write(state.clone()).await;

                // Report state changes to the API
                report_future = Box::pin(send_report_if_managed(
                    &mut report_client,
                    state.into(),
                    last_report.clone(),
                ));
            }

            SeekAction::Apply(update_req) => {
                if is_apply_pending {
                    // A new target came while applying.
                    // Interrupt the target if we are asked to cancel.
                    if update_req.opts.cancel {
                        // interrupt the existing target and wait for it to finish
                        interrupt.trigger();
                        prev_seek_state = apply_future.await?;

                        current_state.set_status(TargetStatus::Interrupted).await;

                        // Reset the future
                        apply_future = Box::pin(future::pending());
                    }
                    // Otherwise just store the target state for the next iteration
                    else {
                        next_target = Some(update_req);
                        continue;
                    }
                }

                // Trigger a new target state apply if we got here
                drop(apply_future);
                interrupt = Interrupt::new();
                apply_future = Box::pin(apply_target(
                    &mut worker,
                    update_req.target,
                    update_req.raw_target,
                    update_req.opts,
                    &config.fallback,
                    interrupt.clone(),
                    &current_state,
                    &fallback_state,
                    prev_seek_state.clone(),
                ));
                is_apply_pending = true;
            }

            SeekAction::Complete(state) => {
                prev_seek_state = state.clone();

                // Reset the state
                is_apply_pending = false;
                apply_future = Box::pin(future::pending());

                // if the legacy apply went through
                if matches!(state, SeekState::Reset) {
                    // We need to create a new worker with the updated state as it
                    // may have been changed by the legacy supervisor
                    let initial_state = Device::initial_for(config.uuid.clone());

                    // Update the global state
                    current_state.write(initial_state.clone()).await;
                    worker = create(initial_state)?;
                    worker_stream = worker.follow();
                }

                // If there is a pending target
                if let Some(update_req) = next_target {
                    // reset the next target
                    next_target = None;
                    interrupt = Interrupt::new();

                    // Trigger a new apply
                    apply_future = Box::pin(apply_target(
                        &mut worker,
                        update_req.target,
                        update_req.raw_target,
                        update_req.opts,
                        &config.fallback,
                        interrupt.clone(),
                        &current_state,
                        &fallback_state,
                        prev_seek_state.clone(),
                    ));
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
