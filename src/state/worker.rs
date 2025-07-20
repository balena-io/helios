use bollard::Docker;

use mahler::extract::{Pointer, Target, View};
use mahler::{
    extract::Args,
    task::{create as add, update},
    worker::{Ready, Worker},
};

use super::models::{App, Device, DeviceConfig, TargetApp, TargetDevice, Uuid};

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

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("Failed to connect to Docker daemon: {0}")]
    DockerConnection(#[from] bollard::errors::Error),

    #[error("Failed to serialize initial state: {0}")]
    StateSerialization(#[from] mahler::errors::SerializationError),
}

pub type LocalWorker = Worker<Device, Ready, TargetDevice>;

pub fn create(initial: Device) -> Result<LocalWorker, CreateError> {
    // Initialize the connection
    let docker = Docker::connect_with_defaults().map_err(CreateError::DockerConnection)?;

    Worker::new()
        .resource(docker)
        .job(
            "/apps/{app_uuid}",
            add(new_app).with_description(|Args(uuid): Args<Uuid>| {
                format!("initialize app with uuid '{uuid}'")
            }),
        )
        .job(
            "/config",
            update(store_config).with_description(|| "store device configuration"),
        )
        .initial_state(initial)
        .map_err(CreateError::StateSerialization)
}
