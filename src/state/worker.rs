use bollard::query_parameters::RemoveImageOptions;
use bollard::{query_parameters::CreateImageOptions, Docker};
use futures_lite::StreamExt;
use thiserror::Error;

use mahler::extract::{Pointer, Res, System, Target, View};
use mahler::task::{with_io, Create, Delete};
use mahler::worker::Uninitialized;
use mahler::{
    extract::Args,
    task,
    worker::{Ready, Worker},
};

use crate::types::Uuid;
use crate::util::docker::ImageUri;

use super::models::{App, Device, DeviceConfig, Image, Release, TargetApp, TargetDevice};

#[derive(Error, Debug)]
#[error("{context}: {source}")]
pub struct DockerError {
    context: String,
    source: bollard::errors::Error,
}

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
fn prepare_app(mut app: Pointer<App>, Target(tgt_app): Target<TargetApp>) -> Pointer<App> {
    let TargetApp { id, name, .. } = tgt_app;
    app.replace(App {
        id,
        name,
        ..Default::default()
    });
    app
}

/// Update the in-memory app name
fn set_app_name(mut name: Pointer<String>, Target(tgt): Target<Option<String>>) -> Pointer<String> {
    *name = tgt;
    name
}

/// Initialize an empty release
fn prepare_release(mut release: Pointer<Release>) -> Pointer<Release> {
    release.replace(Release::default());
    release
}

/// Pull an image from the registry, this task is applicable to
/// the creation of a new image
///
/// Condition: the image is not already present in the device
/// Effect: add the image to the list of images
/// Action: pull the image from the registry and add it to the images local registry
fn pull_image(
    mut image: Pointer<Image>,
    Args(image_name): Args<ImageUri>,
    docker: Res<Docker>,
) -> Create<Image, DockerError> {
    // Initialize the image if it doesn't exist
    if image.is_none() {
        image.get_or_insert_default();
    }

    with_io(image, |mut image| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        // Check if the image exists first, we do this because
        // we don't know if the initial state is correct
        match docker.inspect_image(image_name.as_str()).await {
            Ok(img_info) => {
                if img_info.id.is_some() {
                    // If the image exists and has an id, skip
                    // download
                    image.replace(img_info.into());
                    return Ok(image);
                }
            }
            Err(e) => {
                if let bollard::errors::Error::DockerResponseServerError { status_code, .. } = e {
                    if status_code != 404 {
                        return Err(DockerError {
                            context: format!("failed to read information for image {image_name}"),
                            source: e,
                        });
                    }
                } else {
                    return Err(DockerError {
                        context: format!("failed to read information for image {image_name}"),
                        source: e,
                    });
                }
            }
        }

        // Otherwise try to download the image
        let options = Some(CreateImageOptions {
            from_image: Some(image_name.clone().into()),
            ..Default::default()
        });

        // Try to create the image
        let mut stream = docker.create_image(options, None, None);
        while let Some(progress) = stream.next().await {
            // TODO: report progress. This requires https://github.com/balena-io-modules/mahler-rs/issues/43
            let _ = progress.map_err(|e| DockerError {
                context: format!("failed to download image {image_name}"),
                source: e,
            })?;
        }

        // Check that the image
        let img_info = docker
            .inspect_image(image_name.as_str())
            .await
            .map_err(|e| DockerError {
                context: format!("failed to read information for image {image_name}"),
                source: e,
            })?;

        image.replace(img_info.into());

        Ok(image)
    })
}

/// Remove an image
///
/// Condition: the image exists (and there are no services referencing it?)
/// Effect: remove the image from the state
/// Action: remove the image from the engine
fn remove_image(
    img_ptr: Pointer<Image>,
    Args(image_name): Args<ImageUri>,
    System(device): System<Device>,
    docker: Res<Docker>,
) -> Delete<Image, DockerError> {
    // only remove the image if it not being used by any service
    if device.apps.values().any(|app| {
        app.releases.values().any(|release| {
            release
                .services
                .values()
                .any(|s| s.image == image_name.clone())
        })
    }) {
        return img_ptr.into();
    }

    with_io(img_ptr, |img_ptr| async move {
        docker
            .as_ref()
            .expect("docker resource should be available")
            .remove_image(
                image_name.as_str(),
                Option::<RemoveImageOptions>::None,
                None,
            )
            .await
            .map_err(|source| DockerError {
                context: format!("failed to remove image {image_name}"),
                source,
            })?;

        Ok(img_ptr)
    })
    .map(|mut img| {
        // delete the image pointer
        img.take();
        img
    })
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
        .jobs(
            "/images/{image_name}",
            [
                task::create(pull_image).with_description(|Args(image_name): Args<String>| {
                    format!("pull image '{image_name}'")
                }),
                task::delete(remove_image).with_description(|Args(image_name): Args<String>| {
                    format!("delete image '{image_name}'")
                }),
            ],
        )
        .job(
            "/apps/{app_uuid}",
            task::create(prepare_app).with_description(|Args(uuid): Args<Uuid>| {
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
            "/apps/{app_uuid}/releases/{commit}",
            task::create(prepare_release).with_description(
                |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                    format!("initialize release '{commit}' for app with uuid '{uuid}")
                },
            ),
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
