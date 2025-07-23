use futures_lite::StreamExt;
use mahler::{
    worker::{SeekError as WorkerSeekError, SeekStatus},
    workflow::Interrupt,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fmt,
    future::{self, Future},
    pin::Pin,
};
use tokio::sync::{
    watch::{Receiver, Sender},
    Notify,
};
use tracing::{error, info, instrument, trace};

use crate::config::Config;
use crate::fallback::{legacy_update, wait_for_state_settle, FallbackError, FallbackState};

use super::models::{Device, TargetDevice};
use super::worker::{create, CreateError as WorkerCreateError};

/// Represents the service update status according to
/// https://docs.balena.io/learn/manage/device-statuses/#update-statuses
///
/// This is basically the status of the seek loop
///
/// TODO: discuss later if we want  to use the interrupted state, it might make
/// sense in case the device gets stuck on that state
#[derive(Clone, Serialize, Default, Debug)]
#[serde(tag = "status", content = "errors", rename_all = "snake_case")]
pub enum UpdateStatus {
    #[default]
    Done,
    ApplyingChanges,
    Aborted(Vec<String>),
    Interrupted,
}

impl From<SeekStatus> for UpdateStatus {
    fn from(status: SeekStatus) -> Self {
        match status {
            SeekStatus::Success => UpdateStatus::Done,
            SeekStatus::NotFound => UpdateStatus::Aborted(vec!["workflow not found".to_string()]),
            SeekStatus::Interrupted => UpdateStatus::Interrupted,
            SeekStatus::Aborted(errors) => {
                UpdateStatus::Aborted(errors.iter().map(|e| e.to_string()).collect())
            }
        }
    }
}

/// Helios' state and apply status to be reported used by the API
#[derive(Clone, Debug)]
pub struct LocalState {
    pub device: Device,
    pub status: UpdateStatus,
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
    Local(UpdateStatus),
    Fallback,
}

type SeekResult = Result<SeekState, SeekError>;

#[derive(Debug, Clone)]
enum SeekAction {
    Terminate,
    Apply(SeekRequest),
    Report(Device),
    Complete(SeekState),
}

impl fmt::Display for SeekAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let action = match self {
            Self::Terminate => "terminate",
            Self::Apply(_) => "apply",
            Self::Report(_) => "report",
            Self::Complete(_) => "complete",
        };
        f.write_str(action)
    }
}

// Helper struct to keep track of the next seek request
struct NextTarget {
    next: Option<SeekRequest>,
    notify: Notify,
}

impl NextTarget {
    fn new() -> Self {
        Self {
            next: None,
            notify: Notify::new(),
        }
    }

    fn set(&mut self, next: SeekRequest) {
        self.next = Some(next);
    }

    fn emit(&self) {
        if self.next.is_some() {
            self.notify.notify_one();
        }
    }

    async fn wait(&mut self) -> SeekRequest {
        let notify = &self.notify;
        // We loop here to apease the compiler, but no notification
        // will happen if self.next is None
        loop {
            notify.notified().await;
            if let Some(next) = self.next.take() {
                return next;
            }
        }
    }
}

fn report_state(tx: &Sender<LocalState>, device: &Device, status: &UpdateStatus) {
    let _ = tx.send(LocalState {
        device: device.clone(),
        status: status.clone(),
    });
}

#[instrument(name = "seek", skip_all, err)]
pub async fn start_seek(
    config: &Config,
    initial_state: Device,
    fallback_state: FallbackState,
    mut seek_rx: Receiver<SeekRequest>,
    state_tx: Sender<LocalState>,
) -> Result<(), SeekError> {
    info!("waiting for target");

    // Keep track of the current state and update status
    let mut current_state = initial_state.clone();
    let mut update_status = UpdateStatus::default();

    // Create a mahler worker and start following changes
    let mut worker = create(initial_state.clone())?;
    let mut worker_stream = worker.follow();

    // Seek target state
    let mut prev_seek_state = SeekState::Local(UpdateStatus::default());
    let mut apply_future: Pin<Box<dyn Future<Output = SeekResult>>> = Box::pin(future::pending());
    let mut interrupt = Interrupt::new();
    if config.fallback.address.is_some() {
        // If there is a fallback, we just assume it is applying changes, so the first apply will
        // go to the legacy supervisor instead of the local worker
        prev_seek_state = SeekState::Fallback;
    }

    // The next queued target state
    let mut next_target = NextTarget::new();

    // Main loop, applies changes and reports state.
    // Operations may be interrupted by a new seek request.
    loop {
        let action: SeekAction = tokio::select! {
            // Prioritize new requests over a pending target
            biased;

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

            // Wake up on pending target
            req = next_target.wait() => {
               SeekAction::Apply(req)
            }

            // State changes should trigger a new patch
            new_state = worker_stream.next() => {
                // The stream should not return None unless the worker is dropped
                // so this should not panic unless there is a bug
                SeekAction::Report(new_state.expect("worker stream should remain open"))
            }

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

            SeekAction::Report(device) => {
                current_state = device;
                report_state(&state_tx, &current_state, &update_status);
            }

            SeekAction::Apply(update_req) => {
                if matches!(update_status, UpdateStatus::ApplyingChanges) {
                    // A new target came while applying.
                    // Interrupt the target if we are asked to cancel.
                    if update_req.opts.cancel {
                        // interrupt the existing target and wait for it to finish
                        interrupt.trigger();
                        prev_seek_state = apply_future.await?;

                        // Update the status
                        update_status = UpdateStatus::Interrupted;
                        report_state(&state_tx, &current_state, &update_status);

                        // Reset the future
                        apply_future = Box::pin(future::pending());
                    }
                    // Otherwise just store the target state for the next iteration
                    else {
                        next_target.set(update_req);
                        continue;
                    }
                }

                // Trigger a new target state apply if we got here
                drop(apply_future);
                interrupt = Interrupt::new();

                // Set the update status immediately
                update_status = UpdateStatus::ApplyingChanges;
                report_state(&state_tx, &current_state, &update_status);

                // Create the apply future
                apply_future = {
                    let interrupt = interrupt.clone();
                    let fallback_state = &fallback_state;
                    let worker = &mut worker;
                    let prev_seek_state = prev_seek_state.clone();

                    // If this is the case we already know the target won't be processed by the
                    // apply block so, we just put it back on the pending queue.
                    if update_req.raw_target.is_none()
                        && matches!(prev_seek_state, SeekState::Fallback)
                    {
                        next_target.set(update_req.clone());
                    }

                    Box::pin(async move {
                        let mut status = UpdateStatus::ApplyingChanges;
                        if !matches!(prev_seek_state, SeekState::Fallback) {
                            // Reset the target state so a supervisor poll cannot
                            // lead to a double apply
                            fallback_state.clear_target_state().await;

                            // Apply the target
                            status = tokio::select! {
                                status = worker.seek_target(update_req.target.clone()) => status?.into(),
                                _ = interrupt.wait() => {
                                    return Ok(SeekState::Local(UpdateStatus::Interrupted))
                                }
                            };
                        }

                        if let (Some(uri), Some(target_state)) =
                            (config.fallback.address.clone(), update_req.raw_target)
                        {
                            // Set as the target state the raw target accepted by
                            // the fallback
                            fallback_state.set_target_state(target_state).await;
                            return match legacy_update(
                                uri,
                                config.fallback.api_key.clone(),
                                update_req.opts.force,
                                update_req.opts.cancel,
                                interrupt,
                            )
                            .await
                            {
                                // Tell the caller that they need to reset the worker
                                Ok(_) => Ok(SeekState::Reset),
                                Err(FallbackError::Interrupted) => Ok(SeekState::Fallback),
                                Err(e) => return Err(e)?,
                            };
                        }

                        if let (Some(uri), SeekState::Fallback) =
                            (config.fallback.address.clone(), prev_seek_state)
                        {
                            // if we get here it means there is no raw target, so we need to keep waiting
                            return match wait_for_state_settle(
                                uri,
                                config.fallback.api_key.clone(),
                                interrupt,
                            )
                            .await
                            {
                                Ok(_) => Ok(SeekState::Reset),
                                Err(FallbackError::Interrupted) => Ok(SeekState::Fallback),
                                Err(e) => Err(e)?,
                            };
                        }

                        Ok(SeekState::Local(status))
                    })
                };
            }

            SeekAction::Complete(state) => {
                prev_seek_state = state.clone();

                // Reset the state
                apply_future = Box::pin(future::pending());

                // If only the local apply was performed
                update_status = if let SeekState::Local(status) = state {
                    status
                }
                // if the legacy apply went through
                else if matches!(state, SeekState::Reset) {
                    // We need to create a new worker with the updated state as it
                    // may have been changed by the legacy supervisor
                    let initial_state = Device::initial_for(config.uuid.clone());

                    worker = create(initial_state)?;
                    worker_stream = worker.follow();

                    UpdateStatus::Done
                } else {
                    unreachable!()
                };
                // Update the global state
                report_state(&state_tx, &current_state, &update_status);

                // Emit the next target if any
                next_target.emit();
            }
        }
    }

    info!("terminating");
    Ok(())
}
