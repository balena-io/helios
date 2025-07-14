use bollard::Docker;
use futures_lite::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    future::{self, Future},
    pin::Pin,
    sync::Arc,
};
use tokio::sync::{watch::Receiver, RwLock};
use tokio::time::Instant;
use tracing::{error, info, instrument, trace, warn};

use crate::config::Config;
use crate::fallback::{legacy_update, FallbackError, FallbackState};
use crate::remote::{
    get_poll_client, get_report_client, poll_remote_if_managed, send_report_if_managed,
    DeviceReport, LastReport, PollResult, Report,
};

use mahler::{
    extract::{Args, Pointer, Target, View},
    task::create,
    worker::{Ready, SeekError, SeekStatus, Worker},
};
use mahler::{task::update, workflow::Interrupt};

#[derive(Clone, Serialize, Default, Debug)]
#[serde(tag = "status", content = "errors", rename_all = "snake_case")]
pub enum TargetStatus {
    #[default]
    NoTargetYet,
    Applying,
    NotFound,
    Interrupted,
    Aborted(Vec<String>),
    Applied,
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
    // Read the host and apps state from the underlying system
    pub async fn load_initial(uuid: Uuid) -> Self {
        let device = load_initial_state(uuid).await;
        Self::new(device)
    }

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

pub type Uuid = String;

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Image {
    pub docker_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct App {
    pub name: String,
}

type DeviceConfig = HashMap<String, String>;

/// Current state of a device
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Device {
    /// The device UUID
    pub uuid: Uuid,

    /// List of docker images on the device
    pub images: HashMap<String, Image>,

    /// Apps on the device
    pub apps: HashMap<Uuid, App>,

    /// Config vars
    pub config: DeviceConfig,
}

impl From<Device> for DeviceReport {
    fn from(_: Device) -> Self {
        // TODO
        DeviceReport {}
    }
}

impl From<Device> for Report {
    fn from(device: Device) -> Self {
        Report::new(HashMap::from([(device.uuid.clone(), device.into())]))
    }
}

async fn load_initial_state(uuid: Uuid) -> Device {
    // TODO: read initial state from the engine
    Device {
        uuid,
        images: HashMap::new(),
        apps: HashMap::new(),
        config: HashMap::new(),
    }
}

// Alias the App for now, the target app will have
// its own structure eventually
pub type TargetApp = App;

/// Target state of a device
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TargetDevice {
    pub apps: HashMap<Uuid, TargetApp>,
    pub config: DeviceConfig,
}

impl From<Device> for TargetDevice {
    fn from(device: Device) -> Self {
        let Device { apps, config, .. } = device;
        Self { apps, config }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct TargetState(HashMap<String, TargetDevice>);

impl TargetState {
    fn new(uuid: Uuid, device: TargetDevice) -> Self {
        Self(HashMap::from([(uuid, device)]))
    }
}

/// An update request coming from the API
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct UpdateRequest {
    /// Optional target state
    pub target: Option<TargetDevice>,

    /// Trigger an update ignoring locks
    #[serde(default)]
    pub force: bool,

    /// Cancel the current update if any
    #[serde(default)]
    pub cancel: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateWorkerError {
    #[error("Failed to connect to Docker daemon: {0}")]
    DockerConnection(#[from] bollard::errors::Error),

    #[error("Failed to serialize initial state: {0}")]
    StateSerialization(#[from] mahler::errors::SerializationError),
}

fn serialize_state(state: &Device) -> Value {
    let report: Report = state.clone().into();
    serde_json::to_value(report)
        // This is probably a bug in the types, it shouldn't really happen
        .expect("state report serialization failed")
}

/// Store configuration in memory
fn store_config(
    mut config: View<DeviceConfig>,
    Target(tgt_config): Target<DeviceConfig>,
) -> View<DeviceConfig> {
    // If a new config received, just update the in-memory state, the config will be handled
    // by the legacy supervisor
    *config = tgt_config;
    config
}

/// Initialize the app in memory
fn new_app(mut app: Pointer<App>, Target(tgt_app): Target<TargetApp>) -> Pointer<App> {
    let TargetApp { name, .. } = tgt_app;
    app.assign(App { name });
    app
}

fn create_worker(
    /* jobs: ..., */
    initial: Device,
) -> Result<Worker<Device, Ready, TargetDevice>, CreateWorkerError> {
    // Initialize the connection
    let docker = Docker::connect_with_defaults().map_err(CreateWorkerError::DockerConnection)?;

    Worker::new()
        .resource(docker)
        .job(
            "/apps/{app_uuid}",
            create(new_app).with_description(|Args(uuid): Args<Uuid>| {
                format!("initialize app with uuid '{uuid}'")
            }),
        )
        .job(
            "/config",
            update(store_config).with_description(|| "store device configuration"),
        )
        .initial_state(initial)
        .map_err(CreateWorkerError::StateSerialization)
}

#[derive(Debug, thiserror::Error)]
pub enum StartSupervisorError {
    #[error("Failed to create worker: {0}")]
    CreateWorker(#[from] CreateWorkerError),

    #[error("Failed to reach target state: {0}")]
    SeekTargetState(#[from] SeekError),

    #[error("Failed to update state on legacy Supervisor: {0}")]
    LegacyUpdate(#[from] FallbackError),
}

// Helper types to make the code a bit more readable
type LegacyFuture = Pin<Box<dyn Future<Output = Result<(), FallbackError>>>>;
type SeekResult = Result<SeekStatus, SeekError>;

#[instrument(name = "main", skip_all, err)]
pub async fn start(
    config: Config,
    current_state: CurrentState,
    fallback_state: FallbackState,
    mut update_request_rx: Receiver<UpdateRequest>,
) -> Result<(), StartSupervisorError> {
    info!("starting");
    let mut next_poll_time = Instant::now();

    let mut poll_client = get_poll_client(&config);
    let mut report_client = get_report_client(&config);

    if poll_client.is_none() {
        warn!("running in unmanaged mode");
    }

    // Read the initial state
    let initial_state = current_state.read().await;

    // Create a mahler Worker instance
    // and start following changes
    let mut worker = create_worker(initial_state.clone())?;
    let mut worker_stream = worker.follow();

    // Store the last update request
    let mut update_req = UpdateRequest::default();

    // Reporting variables
    let mut report_future: Pin<Box<dyn Future<Output = LastReport>>> = Box::pin(
        send_report_if_managed(&mut report_client, serialize_state(&initial_state), None),
    );
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
        Box::pin(poll_remote_if_managed(&mut poll_client, &config));
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
                poll_future = Box::pin(poll_remote_if_managed(&mut poll_client, &config));
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

                    let tgt_device_opt = match serde_json::from_value::<TargetState>(target_state) {
                        Ok(TargetState(mut map)) => map.remove(&config.uuid),
                        Err(e)  => {
                            // FIXME: we'll need to reject the target if it cannot be deserialized
                            warn!("failed to deserialize target state: {e}");
                            None
                        }
                    };

                    // Apply the state unless waiting for a legacy apply
                    if let (Some(target_device), false) = (tgt_device_opt, is_legacy_pending) {
                        drop(seek_future);
                        interrupt = Interrupt::new();
                        seek_future = Box::pin(worker.seek_with_interrupt(target_device, interrupt.clone()));
                        is_apply_pending = true;
                        current_state.set_status(TargetStatus::Applying).await;
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
                        current_state.set_status(TargetStatus::Interrupted).await;

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


                update_req = update_request_rx.borrow_and_update().clone();
                drop(poll_future);
                if let Some(target_device) = update_req.target.clone() {
                    let target_state = TargetState::new(config.uuid.clone(), target_device);
                    // If the target is not empty, set it as the result of the new future
                    poll_future = Box::pin(async move {
                        // Serializing should not fail here since this input was already validated
                        // at the API
                        (Some(serde_json::to_value(target_state).unwrap()), next_poll_time)
                    });
                }
                else {
                    // Otherwise trigger a new poll
                    poll_future = Box::pin(poll_remote_if_managed(&mut poll_client, &config));
                    is_poll_pending = true;
                }
            }

            // State changes should trigger a new patch
            stream_res = worker_stream.next() => {
                if let Some(cur_state) = stream_res {
                    // Drop the previous patch if a new state comes before the previous one is finished
                    // rate limiting is handled by the client
                    drop(report_future);

                    // Record the updated global state
                    current_state.write(cur_state.clone()).await;

                    // Report state changes to the API
                    report_future = Box::pin(send_report_if_managed(&mut report_client, serialize_state(&cur_state), last_report.clone()));
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
                current_state.set_status(seek_status?.into()).await;

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
                let initial_state = load_initial_state(config.uuid.clone()).await;

                // Update the global state
                current_state.write(initial_state.clone()).await;
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
            apps: HashMap::new(),
            config: HashMap::new(),
        };

        let report: Report = device.into();

        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value, json!({"test-uuid": {}}))
    }
}
