use bollard::Docker;

use mahler::extract::{Pointer, Target, View};
use mahler::worker::Uninitialized;
use mahler::{
    extract::Args,
    task,
    worker::{Ready, Worker},
};

use crate::types::Uuid;

use super::models::{App, Device, DeviceConfig, TargetApp, TargetDevice};

/// Update the in-memory device name
fn set_device_name(
    mut name: Pointer<String>,
    Target(tgt): Target<Option<String>>,
) -> Pointer<String> {
    *name = tgt;
    name
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
    let TargetApp { id, name, .. } = tgt_app;
    app.assign(App { id, name });
    app
}

/// Update the in-memory app name
fn set_app_name(mut name: Pointer<String>, Target(tgt): Target<Option<String>>) -> Pointer<String> {
    *name = tgt;
    name
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("Failed to connect to Docker daemon: {0}")]
    DockerConnection(#[from] bollard::errors::Error),

    #[error("Failed to serialize initial state: {0}")]
    StateSerialization(#[from] mahler::errors::SerializationError),
}

/// Configure the worker jobs
///
/// This is mostly used for tests
fn worker() -> Worker<Device, Uninitialized, TargetDevice> {
    Worker::new()
        .job(
            "/name",
            task::any(set_device_name).with_description(|| "update device name"),
        )
        .job(
            "/apps/{app_uuid}",
            task::create(new_app).with_description(|Args(uuid): Args<Uuid>| {
                format!("initialize app with uuid '{uuid}'")
            }),
        )
        .job(
            "/apps/{app_uuid}/name",
            task::any(set_app_name).with_description(|Args(uuid): Args<Uuid>| {
                format!("update name for app with uuid '{uuid}'")
            }),
        )
        .job(
            "/config",
            task::update(store_config).with_description(|| "store device configuration"),
        )
}

type LocalWorker = Worker<Device, Ready, TargetDevice>;

/// Create and initialize the worker
pub fn create(docker: Docker, initial: Device) -> Result<LocalWorker, CreateError> {
    // Create the worker and set-up resources
    let worker = worker().resource(docker).initial_state(initial)?;

    Ok(worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    use mahler::{par, seq, Dag};
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tracing_subscriber::fmt::{self, format::FmtSpan};
    use tracing_subscriber::{prelude::*, EnvFilter};

    fn before() {
        // Initialize tracing subscriber with custom formatting
        tracing_subscriber::registry()
            .with(EnvFilter::from_default_env())
            .with(
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_span_events(FmtSpan::CLOSE)
                    .event_format(fmt::format().pretty().with_target(false)),
            )
            .try_init()
            .unwrap_or(());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_create_a_single_app() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
        }))
        .unwrap();
        let target = serde_json::from_value::<TargetDevice>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "name": "my-app"
                }
            }
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!("initialize app with uuid 'my-app-uuid'",);
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_create_an_app_and_configs() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
        }))
        .unwrap();
        let target = serde_json::from_value::<TargetDevice>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "name": "my-app"
                }
            },
            "config": {
                "SOME_VAR": "one",
                "OTHER_VAR": "two"
            }
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = par!(
            "initialize app with uuid 'my-app-uuid'",
            "store device configuration",
            "update device name"
        );
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_change_an_app_name() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "name": "device-name",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app"
                }
            }
        }))
        .unwrap();
        let target = serde_json::from_value::<TargetDevice>(json!({
            "uuid": "my-device-uuid",
            "name": "device-name",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name"
                }
            }
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!("update name for app with uuid 'my-app-uuid'",);
        assert_eq!(workflow.to_string(), expected.to_string());
    }
}
