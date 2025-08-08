use bollard::auth::DockerCredentials;
use bollard::query_parameters::{CreateImageOptions, RemoveImageOptions, TagImageOptions};
use bollard::Docker;
use futures_lite::StreamExt;
use thiserror::Error;

use mahler::extract::{Res, System, Target, View};
use mahler::task::prelude::*;
use mahler::worker::Uninitialized;
use mahler::{
    extract::Args,
    task,
    worker::{Ready, Worker},
};
use tokio::sync::RwLock;
use tracing::debug;

use crate::remote::RegistryAuthClient;
use crate::types::Uuid;
use crate::util::docker::{ImageUri, InvalidImageUriError};

use super::models::{
    App, Device, DeviceConfig, Image, Release, Service, TargetApp, TargetDevice, TargetService,
};

#[derive(Error, Debug)]
#[error("{context}: {source}")]
struct DockerError {
    context: String,
    source: bollard::errors::Error,
}

#[derive(Error, Debug)]
enum TaskError {
    #[error(transparent)]
    Docker(#[from] DockerError),

    #[error(transparent)]
    InvalidImageUri(#[from] InvalidImageUriError),

    #[error("tar operation failed: {0}")]
    Tar(#[from] std::io::Error),
}

/// Update the in-memory device name
fn set_device_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
) -> View<Option<String>> {
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
fn prepare_app(
    mut app: View<Option<App>>,
    Target(tgt_app): Target<TargetApp>,
) -> View<Option<App>> {
    let TargetApp { id, name, .. } = tgt_app;
    app.replace(App {
        id,
        name,
        ..Default::default()
    });
    app
}

/// Update the in-memory app name
fn set_app_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
) -> View<Option<String>> {
    *name = tgt;
    name
}

/// Initialize an empty release
fn prepare_release(mut release: View<Option<Release>>) -> View<Option<Release>> {
    release.replace(Release::default());
    release
}

/// Tag an existing image with a new name
fn tag_image(
    image: View<Option<Image>>,
    Target(tgt): Target<Image>,
    Args(image_name): Args<ImageUri>,
    docker: Res<Docker>,
) -> IO<Option<Image>, DockerError> {
    let tgt_engine_id = tgt.engine_id.clone();

    with_io(image, |image| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        let repo = image_name.repo();
        let tag = image_name.tag().clone();

        // catch any inconsistencies when using the task within a method. If this
        // happens there is a bug
        let engine_id = tgt_engine_id.expect("target image should include an engine id");
        docker
            .tag_image(
                &engine_id,
                Some(TagImageOptions {
                    repo: Some(repo),
                    tag,
                }),
            )
            .await
            .map_err(|source| DockerError {
                context: format!("failed to tag image {engine_id} with {image_name}"),
                source,
            })?;

        Ok(image)
    })
    // set the to the target
    .map(|mut image| {
        image.replace(tgt);
        image
    })
}

/// Pull an image from the registry, this task is applicable to
/// the creation of a new image
///
/// Condition: the image is not already present in the device
/// Effect: add the image to the list of images
/// Action: pull the image from the registry and add it to the images local registry
fn pull_image(
    mut image: View<Option<Image>>,
    Args(image_name): Args<ImageUri>,
    docker: Res<Docker>,
    docker_credentials: Res<DockerCredentials>,
) -> IO<Option<Image>, DockerError> {
    // Initialize the image if it doesn't exist
    if image.is_none() {
        image.get_or_insert_default();
    }

    with_io(image, |mut image| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        // Check if the image exists first, we do this because
        // the state may have changed from under the worker
        match docker.inspect_image(image_name.as_str()).await {
            Ok(img_info) => {
                if img_info.id.is_some() {
                    // If the image exists and has an id, skip
                    // download
                    debug!("image already exists, skipping");
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

        let credentials = docker_credentials
            .as_ref()
            .cloned()
            // Only use the credentials if they match the image registry
            .take_if(|c| c.serveraddress.is_some() && &c.serveraddress == image_name.registry());

        // Try to create the image
        let mut stream = docker.create_image(options, None, credentials);
        while let Some(progress) = stream.next().await {
            // TODO: report progress. This requires https://github.com/balena-io-modules/mahler-rs/issues/43
            let _ = progress.map_err(|e| DockerError {
                context: format!("failed to download image {image_name}"),
                source: e,
            })?;
        }

        // Check that the image exists
        let new_img: Image = docker
            .inspect_image(image_name.as_str())
            .await
            .map_err(|e| DockerError {
                context: format!("failed to read information for image {image_name}"),
                source: e,
            })?
            .into();

        image.replace(new_img);

        Ok(image)
    })
}

/// Creates a new image with the given name.
///
/// Depending on whether the image has a digest or there is another matching image, a different
/// operation will be chosen
fn create_image(Args(image_name): Args<ImageUri>, System(device): System<Device>) -> Task {
    if let Some(digest) = image_name.digest() {
        let existing = device
            .images
            .iter()
            .find(|(name, _)| name.digest().as_ref().is_some_and(|d| d == digest));

        if let Some((_, image)) = existing {
            // If an image with the same digest exists, we just need to tag it
            return tag_image.with_target(image.clone());
        }
    }

    // If the target image doesn't have a digest, just pull it
    pull_image.into_task()
}

/// Do the initial pull for a new service
fn fetch_service_image(
    System(device): System<Device>,
    Target(tgt): Target<TargetService>,
    Args((uuid, commit, service_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    if device.images.keys().any(|img| img == &tgt.image) {
        // tell the planner to skip this task if the image already exists
        return None;
    }

    // If there is a service with the same name in any other releases of the service
    // then skip the direct fetch as that pull will need deltas and needs to
    // be handled by another task
    if device
        .apps
        .get(&uuid)
        .map(|app| {
            app.releases
                .iter()
                .filter(|(c, _)| c != &&commit)
                .any(|(_, r)| r.services.keys().any(|k| k == &service_name))
        })
        .unwrap_or_default()
    {
        return None;
    }

    // Create the image if it doesn't exist already
    Some(create_image.with_arg("image_name", tgt.image))
}

/// Install the service
///
/// FIXME: this only creates the service in memory for now, as we add features, this will also
/// create the service container
fn install_service(
    mut svc: View<Option<Service>>,
    System(device): System<Device>,
    Target(tgt): Target<TargetService>,
) -> View<Option<Service>> {
    // Skip the task if the image for the service doesn't exist yet
    if device.images.keys().all(|img| img != &tgt.image) {
        return svc;
    }

    let TargetService { id, image, .. } = tgt;

    svc.replace(Service { id, image });
    svc
}

/// Remove an image
///
/// Condition: the image exists (and there are no services referencing it?)
/// Effect: remove the image from the state
/// Action: remove the image from the engine
fn remove_image(
    img_ptr: View<Option<Image>>,
    Args(image_name): Args<ImageUri>,
    System(device): System<Device>,
    docker: Res<Docker>,
) -> IO<Option<Image>, DockerError> {
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
        .job(
            "/config",
            task::update(store_config).with_description(|| "store device configuration"),
        )
        .jobs(
            "/images/{image_name}",
            [
                task::none(pull_image).with_description(|Args(image_name): Args<String>| {
                    format!("pull image '{image_name}'")
                }),
                task::none(remove_image).with_description(|Args(image_name): Args<String>| {
                    format!("delete image '{image_name}'")
                }),
                task::none(tag_image).with_description(
                    |Args(image_name): Args<String>, tgt: Target<Image>| {
                        if let Some(engine_id) = &tgt.engine_id {
                            format!("tag image {engine_id} with '{image_name}'")
                        } else {
                            format!("tag image '{image_name}'")
                        }
                    },
                ),
                task::none(create_image),
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
                    format!("initialize release '{commit}' for app with uuid '{uuid}'")
                },
            ),
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}",
            [
                task::create(fetch_service_image),
                task::create(install_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("initialize service '{service_name}' for release '{commit}'")
                    },
                ),
            ],
        )
}

type LocalWorker = Worker<Device, Ready, TargetDevice>;

/// Create and initialize the worker
pub fn create(
    docker: Docker,
    registry_auth_client: Option<RegistryAuthClient>,
    initial: Device,
) -> Result<LocalWorker, CreateError> {
    // Create the worker and set-up resources
    let mut worker = worker().resource(docker).initial_state(initial)?;

    if let Some(auth_client) = registry_auth_client {
        // Add a registry auth client behind a RwLock to allow
        // tasks to modify it
        worker.use_resource(RwLock::new(auth_client));
    }

    Ok(worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    use mahler::{dag, par, seq, Dag};
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
            "store device configuration",
            "update device name",
            "initialize app with uuid 'my-app-uuid'",
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

    #[tokio::test]
    async fn it_finds_a_workflow_to_fetch_and_install_services() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                }
            }
        }))
        .unwrap();
        let target = serde_json::from_value::<TargetDevice>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "services": {
                                "service-one": {
                                    "id": 1,
                                    "image": "ubuntu:latest"
                                },
                                "service-two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/0e3f629e6eb2905b75aae244eb316291@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080"
                                }
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = dag!(
            seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
                + par!(
                    "pull image 'ubuntu:latest'", 
                    "pull image 'registry2.balena-cloud.com/v2/0e3f629e6eb2905b75aae244eb316291@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080'"
                )
                + seq!(
                    "initialize service 'service-one' for release 'my-release-uuid'",
                    "initialize service 'service-two' for release 'my-release-uuid'"
                )
        );
        assert_eq!(workflow.to_string(), expected.to_string());
    }
}
