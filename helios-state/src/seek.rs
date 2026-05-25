use std::future::{self, Future};
use std::pin::Pin;
use std::time::{Duration, Instant};

use mahler::worker::{WorkerEvent, WorkflowStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{
    Notify,
    watch::{Receiver, Sender},
};
use tokio_stream::StreamExt;
use tracing::{error, info, instrument, trace, warn};

use crate::common_types::Uuid;
use crate::legacy::{self, LegacyConfig, ProxyState, StateUpdateError};
use crate::util::interrupt::Interrupt;
use crate::util::locking::ForceAcquireLocks;
use crate::util::logs;

use super::config::Resources;
use super::models::{Device, DeviceTarget};
use super::read::{self, read as read_state};
use super::worker::{LocalWorker, create};

/// Represents the service update status according to
/// https://docs.balena.io/learn/manage/device-statuses/#update-statuses
///
/// This is basically the status of the seek loop
#[derive(Clone, Serialize, Default, Debug)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum UpdateStatus {
    /// target state apply terminated successfully
    #[default]
    Done,
    /// target state apply is in progress
    #[serde(rename = "applying changes")]
    ApplyingChanges,
    /// invalid target state
    Rejected,
    /// target state apply has been aborted pending action from the user
    Aborted,
    /// target state apply has been interrupted by a new request
    Interrupted,
}

/// Helios' state and apply status to be reported used by the API
#[derive(Clone, Debug)]
pub struct LocalState {
    // Authorized apps
    pub authorized_apps: Vec<Uuid>,
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

/// This encodes possible target configurations coming from
/// the remote API or the local API.
///
/// - A Remote target optionally has a DeviceTarget if the target is supported
///   (and hence it can be serialized), otherwise it always includes a raw_target
///   which is the exact request. The raw_target only serves to pass to the legacy supervisor.
/// - A Local API target is validated by the request so it can only have an already validated
///   device target
///
/// FIXME: this type will no longer be necessary once the legacy supervisor no longer handles
/// state updates
#[derive(Debug, Clone)]
pub enum TargetState {
    Remote {
        target: Option<DeviceTarget>,
        raw_target: Value,
    },
    Local {
        target: DeviceTarget,
    },
}

impl TargetState {
    fn value(&self) -> Option<&DeviceTarget> {
        match self {
            TargetState::Remote { target, .. } => target.as_ref(),
            TargetState::Local { target } => Some(target),
        }
    }
}

/// A request to reach a target state.
#[derive(Debug, Clone)]
pub struct SeekRequest {
    pub target: TargetState,
    pub opts: UpdateOpts,
}

impl Default for SeekRequest {
    fn default() -> Self {
        Self {
            target: TargetState::Local {
                target: DeviceTarget::default(),
            },
            opts: UpdateOpts::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SeekError {
    #[error("failed to read state: {0}")]
    ReadState(#[from] read::Error),

    #[error("failed to reach target state: {0}")]
    SeekTargetState(#[from] mahler::error::Error),

    #[error("failed to update state on legacy Supervisor: {0}")]
    LegacyUpdate(#[from] StateUpdateError),
}

#[derive(Debug, Clone)]
enum SeekState {
    Reset,
    Local(UpdateStatus),
    Legacy,
}

type SeekResult = Result<SeekState, SeekError>;

#[derive(Debug, Clone)]
enum SeekAction {
    Terminate,
    Apply(SeekRequest),
    Complete(SeekState),
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
    // FIXME: re-enable reporting once user app support is more advanced.
    // At that point helios should be the only one reporting
    if cfg!(feature = "userapps") {
        tx.send_modify(|state| {
            state.device = device.clone();
            state.status = status.clone();
        });
    }
}

fn report_authorized_apps(tx: &Sender<LocalState>, target_state: &DeviceTarget) {
    // FIXME: re-enable reporting once user app support is more advanced.
    // At that point helios should be the only one reporting
    if cfg!(feature = "userapps") {
        tx.send_modify(|state| {
            let app_keys = target_state.apps.keys();

            #[cfg(feature = "balenahup")]
            let host_keys: Vec<_> = target_state
                .host
                .iter()
                .flat_map(|host| host.releases.values().map(|r| &r.app))
                .collect();
            #[cfg(not(feature = "balenahup"))]
            let host_keys: Vec<&Uuid> = Vec::new();

            state.authorized_apps = app_keys.chain(host_keys).cloned().collect()
        });
    }
}

async fn seek_target(
    worker: &LocalWorker,
    current_state: &mut Device,
    target: &DeviceTarget,
    interrupt: &Interrupt,
    state_tx: &Sender<LocalState>,
    retry_interval: Duration,
) -> Result<UpdateStatus, SeekError> {
    info!("applying target state");

    while !interrupt.is_set() {
        let worker = worker.clone().initial_state(current_state.clone())?;

        // Report the list of pending changes to the logs
        if tracing::enabled!(tracing::Level::INFO) {
            let changes = worker.distance(target)?;
            if !changes.is_empty() {
                info!("searching workflow for pending state changes:");
                for change in changes {
                    info!("- {}", logs::mask_sensitive_data(change));
                }
            } else {
                info!("no pending state changes");
            }
        }

        let now = Instant::now();
        if let Some(workflow) = worker.find_workflow(target)? {
            let has_exceptions = !workflow.exceptions().is_empty();
            if tracing::enabled!(tracing::Level::WARN) && has_exceptions {
                warn!("the following operations were ignored during planning");
                for ignored in workflow.exceptions() {
                    warn!("- {}", logs::mask_sensitive_data(ignored.operation.clone()));
                    if let Some(reason) = ignored.reason.as_ref() {
                        warn!("    reason: {reason}");
                    }
                }
            }

            if workflow.is_empty() {
                info!("target state applied");
                if has_exceptions {
                    // no changes to make but there are exceptions so we return aborted
                    return Ok(UpdateStatus::Aborted);
                }
                return Ok(UpdateStatus::Done);
            } else {
                info!(time = ?now.elapsed(), "workflow found");
                if tracing::enabled!(tracing::Level::INFO) {
                    info!("will execute the following tasks:");
                    for line in workflow.to_string().lines() {
                        info!("{line}");
                    }
                }

                let now = Instant::now();
                info!("executing workflow");

                let mut stream = std::pin::pin!(worker.run_workflow(workflow));

                loop {
                    let event = tokio::select! {
                        _ = interrupt.wait() => {
                            info!(time = ?now.elapsed(), "interrupted by new target");
                            return Ok(UpdateStatus::Interrupted)
                        },
                        Some(evt) = stream.next() => evt,
                        else => unreachable!("stream should not close before workflow terminates")
                    };

                    match event? {
                        WorkerEvent::StateUpdated(new_state) => {
                            *current_state = new_state;
                            report_state(state_tx, current_state, &UpdateStatus::ApplyingChanges);
                        }
                        WorkerEvent::WorkflowFinished(WorkflowStatus::Success) => {
                            info!(time = ?now.elapsed(), "workflow executed successfully");
                            info!("target state applied");
                            if has_exceptions {
                                // workflow completed but there were skipped changes so we
                                // return aborted
                                return Ok(UpdateStatus::Aborted);
                            }
                            return Ok(UpdateStatus::Done);
                        }
                        // if a recoverable error happened, we try-again
                        WorkerEvent::WorkflowFinished(WorkflowStatus::Aborted(_)) => {
                            warn!(time = ?now.elapsed(), "workflow terminated with errors, re-trying in {retry_interval:#?}");

                            tokio::select! {
                                _ = interrupt.wait() => {
                                    info!(time = ?now.elapsed(), "interrupted by new target");
                                    return Ok(UpdateStatus::Interrupted)
                                },
                                _ = tokio::time::sleep(retry_interval) => {},
                            }

                            // break-the inner loop after the back-off
                            break;
                        }
                    }
                }
            }
        } else {
            warn!(time = ?now.elapsed(), "workflow not found");
            return Ok(UpdateStatus::Aborted);
        }
    }
    Ok(UpdateStatus::Interrupted)
}

#[instrument(name = "seek", skip_all, err)]
pub async fn start_seek(
    runtime: Resources,
    initial_state: Device,
    proxy_state: Option<ProxyState>,
    legacy_config: Option<LegacyConfig>,
    mut seek_rx: Receiver<SeekRequest>,
    state_tx: Sender<LocalState>,
    retry_interval: Duration,
) -> Result<(), SeekError> {
    info!("waiting for target");

    // Keep track of the current state and update status
    let uuid = initial_state.uuid.clone();
    #[cfg(feature = "balenahup")]
    let os = initial_state.host.as_ref().map(|host| host.meta.clone());
    #[cfg(not(feature = "balenahup"))]
    let os: Option<crate::common_types::OperatingSystem> = None;
    let mut current_state = initial_state.clone();
    let mut update_status = UpdateStatus::default();

    let Resources {
        docker,
        local_store,
        registry_auth_client,
        host_runtime_dir,
    } = runtime;

    // Create an uninitialized local worker
    let worker = create(
        docker.clone(),
        local_store.clone(),
        host_runtime_dir.clone(),
        registry_auth_client.clone(),
    );

    // Seek target state
    let mut prev_seek_state = SeekState::Local(UpdateStatus::default());
    let mut apply_future: Pin<Box<dyn Future<Output = SeekResult>>> = Box::pin(future::pending());
    let mut interrupt = Interrupt::new();
    if legacy_config.is_some() {
        // If there is a fallback, we just assume it is applying changes, so the first apply will
        // go to the legacy supervisor instead of the local worker
        prev_seek_state = SeekState::Legacy;
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

        match action {
            SeekAction::Terminate => {
                break;
            }

            SeekAction::Apply(update_req) => {
                // Update authorization list
                if let Some(target) = &update_req.target.value() {
                    report_authorized_apps(&state_tx, target);
                }

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

                // We re-initialize the worker each time as the state of the system may have changed
                // outside of what is monitored by the worker
                current_state = read_state(&docker, &local_store, uuid.clone(), os.clone()).await?;

                // Set the update status immediately
                update_status = UpdateStatus::ApplyingChanges;
                report_state(&state_tx, &current_state, &update_status);

                // Create the apply future
                apply_future = {
                    let interrupt = interrupt.clone();
                    let proxy_state = &proxy_state;
                    let prev_seek_state = prev_seek_state.clone();
                    let legacy_config = legacy_config.clone();
                    let host_runtime_dir = host_runtime_dir.clone();

                    // Allow reporting from inside the future
                    let state_tx = &state_tx;
                    let worker = worker
                        .clone()
                        .resource(ForceAcquireLocks::from(update_req.opts.force));
                    let current_state = &mut current_state;

                    // If we are continuing a legacy apply we also set the next target
                    // if it has valid target so any pending steps can be handled by this loop
                    if update_req.target.value().is_some()
                        && matches!(prev_seek_state, SeekState::Legacy)
                    {
                        next_target.set(update_req.clone());
                    }

                    Box::pin(async move {
                        let mut update_status = UpdateStatus::ApplyingChanges;
                        let (mut maybe_device_target, raw_target) = match update_req.target {
                            TargetState::Remote { target, raw_target } => {
                                (target, Some(raw_target))
                            }
                            TargetState::Local { target } => (Some(target), None),
                        };

                        // If there is a target and the last apply was not an apply to
                        // the legacy state
                        if let Some(device_target) = maybe_device_target.as_mut()
                            && !matches!(prev_seek_state, SeekState::Legacy)
                        {
                            // Reset the target state so a supervisor poll cannot
                            // lead to a double apply
                            if let Some(proxy_state) = &proxy_state {
                                proxy_state.clear().await;
                            }

                            device_target.add_runtime_context(current_state, &host_runtime_dir);

                            // Look for a plan to the target
                            update_status = seek_target(
                                &worker,
                                current_state,
                                device_target,
                                &interrupt,
                                state_tx,
                                retry_interval,
                            )
                            .await?;
                        }

                        // If there is a legacy supervisor and the target state is coming from
                        // the remote API
                        if let (Some(config), Some(maybe_raw_target)) =
                            (legacy_config.clone(), raw_target)
                        {
                            // Set as the target state the raw target accepted by
                            // the fallback
                            if let Some(proxy_state) = &proxy_state {
                                proxy_state.set(maybe_raw_target).await;
                            }
                            return match legacy::trigger_update(
                                config.api_endpoint,
                                config.api_key,
                                update_req.opts.force,
                                update_req.opts.cancel,
                                interrupt,
                            )
                            .await
                            {
                                // Tell the caller that they need to reset the worker
                                Ok(_) => Ok(SeekState::Reset),
                                Err(StateUpdateError::Interrupted) => Ok(SeekState::Legacy),
                                Err(e) => return Err(e)?,
                            };
                        }

                        // If there is a legacy supervisor but the target is coming from the
                        // local API (so no raw target)
                        if let (Some(config), SeekState::Legacy) =
                            (legacy_config.clone(), prev_seek_state)
                        {
                            // keep waiting
                            return match legacy::wait_for_state_settle(
                                config.api_endpoint,
                                config.api_key,
                                interrupt,
                            )
                            .await
                            {
                                Ok(_) => Ok(SeekState::Reset),
                                Err(StateUpdateError::Interrupted) => Ok(SeekState::Legacy),
                                Err(e) => Err(e)?,
                            };
                        }

                        // If the target could not be processed just return the updated status
                        if maybe_device_target.is_none() {
                            // if we get here, then there was no target state from the backend
                            // and there is no legacy config so we just reject
                            error!("cannot process target");
                            return Ok(SeekState::Local(UpdateStatus::Rejected));
                        }

                        Ok(SeekState::Local(update_status))
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
                    // reload the current state
                    current_state =
                        read_state(&docker, &local_store, uuid.clone(), os.clone()).await?;

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
