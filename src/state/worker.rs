use std::collections::HashMap;

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

use crate::remote::{RegistryAuth, RegistryAuthClient, RegistryAuthError};
use crate::types::Uuid;
use crate::util::docker::ImageUri;

use super::models::{
    App, Device, Image, RegistryAuthSet, Release, Service, TargetApp, TargetAppMap, TargetDevice,
    TargetService,
};

#[derive(Error, Debug)]
#[error("{context}: {source}")]
struct DockerError {
    context: String,
    source: bollard::errors::Error,
}

/// Make sure a cleanup happens after all tasks have been performed
///
/// This should be the first task for every workflow
fn ensure_cleanup(mut device: View<Device>) -> View<Device> {
    device.needs_cleanup = true;
    device
}

/// Clean up the device state after the target has been reached
///
/// This should be the final task for every workflow
fn perform_cleanup(
    device: View<Device>,
    Target(tgt_device): Target<TargetDevice>,
    auth_client_res: Res<RwLock<RegistryAuthClient>>,
) -> IO<Device> {
    // skip the task if we have not reached the target state
    // (outside the needs_cleanup property)
    if TargetDevice::from(Device {
        needs_cleanup: false,
        ..device.clone()
    }) != tgt_device
        || !device.needs_cleanup
    {
        return device.into();
    }

    with_io(device, |device| async move {
        // Clean up authorizations
        if let Some(auth_client_rwlock) = auth_client_res.as_ref() {
            debug!("clean up registry credentials");
            // Wait for write authorization
            let mut auth_client = auth_client_rwlock.write().await;
            auth_client.clear();
        }

        Ok(device)
    })
    .map(|mut device| {
        device.auths.clear();
        device.needs_cleanup = false;
        device
    })
}

/// Update the in-memory device name
fn set_device_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
) -> View<Option<String>> {
    *name = tgt;
    name
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

/// Find an installed service for a different commit
fn find_installed_service<'a>(
    device: &'a Device,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    service_name: &'a String,
) -> Option<&'a Service> {
    device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(_, r)| r.services.iter().find(|(k, _)| k == &service_name))
            .map(|(_, s)| s)
            .next()
    })
}

// Request registry tokens
fn request_registry_credentials(
    mut auths: View<RegistryAuthSet>,
    Target(tgt_auths): Target<RegistryAuthSet>,
    auth_client_res: Res<RwLock<RegistryAuthClient>>,
) -> IO<RegistryAuthSet, RegistryAuthError> {
    // Update the device authorization list
    for auth in tgt_auths.into_iter() {
        auths.insert(auth);
    }

    with_io(auths, |auths| async move {
        if let Some(auth_client_rwlock) = auth_client_res.as_ref() {
            // Wait for write authorization
            let mut auth_client = auth_client_rwlock.write().await;

            for auth in auths.iter() {
                auth_client.request(auth).await?;
            }
        }

        Ok(auths)
    })
}

/// Request authorization for new image installs
fn request_registry_token_for_new_images(
    System(device): System<Device>,
    Target(tgt_apps): Target<TargetAppMap>,
) -> Option<Task> {
    // Find all images for new services in the target state
    let images_to_install: Vec<&ImageUri> = tgt_apps
        .iter()
        .flat_map(|(app_uuid, app)| {
            app.releases.iter().flat_map(|(commit, release)| {
                release
                    .services
                    .iter()
                    .filter(|(service_name, svc)| {
                        // if the service image has not been downloaded
                        !device.images.contains_key(&svc.image) &&
                        // and there is no service from a different release
                        find_installed_service(
                            &device,
                            app_uuid,
                            commit,
                            service_name,
                        )
                        // or there is a service and the digest doesn't match the target digest
                        .is_none_or(|s| {
                            s.image.digest().is_none() || s.image.digest() != svc.image.digest()
                        })
                    })
                    // then select the image
                    .map(|(_, svc)| &svc.image)
            })
        })
        .collect();

    // Group images to install by registry
    let tgt_auths: RegistryAuthSet = images_to_install
        .into_iter()
        .fold(HashMap::<String, Vec<ImageUri>>::new(), |mut acc, img| {
            if let Some(service) = img.registry().clone() {
                if let Some(scope) = acc.get_mut(&service) {
                    scope.push((*img).clone());
                } else {
                    acc.insert(service, vec![(*img).clone()]);
                }
            }
            acc
        })
        .into_values()
        .map(|scope| RegistryAuth::try_from(scope).expect("auth creation should not fail"))
        // if the i
        .filter(|scope| device.auths.iter().all(|s| !s.is_super_scope(scope)))
        .collect();

    if tgt_auths.is_empty() {
        return None;
    }

    Some(request_registry_credentials.with_target(tgt_auths))
}

/// Install all new images for target apps
///
/// NOTE: this and [`request_registry_token_for_new_images`] are expensive operations
/// (`O(num_of_services^2)` in the worst case) and they are assigned to `update` operations on `/apps`
/// on the worker configuration.
/// This means they will be executed pretty much for every search iteration of the planner.
/// `num_of_services` is usually small and the search will be closer to `O(num_of_services)` if
/// there are no images to install so this might not have a ton of impact but there might
/// be some future optimization to consider.
fn fetch_apps_images(
    System(device): System<Device>,
    Target(tgt_apps): Target<TargetAppMap>,
) -> Vec<Task> {
    // Find all images for new services in the target state
    let images_to_install: Vec<&ImageUri> = tgt_apps
        .iter()
        .flat_map(|(app_uuid, app)| {
            app.releases.iter().flat_map(|(commit, release)| {
                release.services.iter().filter(|(service_name, svc)| {
                    // if the service image has not been downloaded
                    !device.images.contains_key(&svc.image) &&
                    // and there is no service from a different release
                    find_installed_service(
                            &device,
                            app_uuid,
                            commit,
                            service_name,
                        )
                        // or if there is a service, then only chose it if it has the same digest
                        // as the target service
                        .is_none_or(|s| {
                            s.image.digest().is_some() && svc.image.digest() == s.image.digest()
                        })
                })
            })
        })
        // remove duplicate digests
        .fold(Vec::<&ImageUri>::new(), |mut acc, (_, svc)| {
            if acc
                .iter()
                .all(|img| img.digest().is_none() || img.digest() != svc.image.digest())
            {
                acc.push(&svc.image);
            }
            acc
        });

    // only download at most 3 images at the time
    let mut tasks: Vec<Task> = Vec::new();
    for image in images_to_install.into_iter().take(3) {
        tasks.push(create_image.with_arg("image_name", image.clone()))
    }
    tasks
}

/// Initialize an empty release
fn prepare_release(mut release: View<Option<Release>>) -> View<Option<Release>> {
    release.replace(Release::default());
    release
}

/// Tag an existing image with a new name
fn tag_image(
    image: View<Option<Image>>,
    Target((tgt_uri, tgt_img)): Target<(ImageUri, Image)>,
    Args(image_name): Args<ImageUri>,
    docker: Res<Docker>,
) -> IO<Option<Image>, DockerError> {
    with_io(image, |image| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        let repo = image_name.repo();
        let tag = image_name.tag().clone();

        docker
            .tag_image(
                tgt_uri.as_str(),
                Some(TagImageOptions {
                    repo: Some(repo),
                    tag,
                }),
            )
            .await
            .map_err(|source| DockerError {
                context: format!("failed to tag image {tgt_uri} with {image_name}"),
                source,
            })?;

        Ok(image)
    })
    // set the to the target
    .map(|mut image| {
        image.replace(tgt_img);
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
    auth_client: Res<RwLock<RegistryAuthClient>>,
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

        let credentials = if let Some(auth_client_rwlock) = auth_client.as_ref() {
            auth_client_rwlock
                .read()
                .await
                .token(&image_name)
                .map(|token| DockerCredentials {
                    registrytoken: Some(token.to_string()),
                    ..Default::default()
                })
        } else {
            None
        };

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
fn create_image(Args(image_name): Args<ImageUri>, System(device): System<Device>) -> Option<Task> {
    if let Some(digest) = image_name.digest() {
        let existing = device
            .images
            .into_iter()
            .find(|(name, _)| name.digest().as_ref().is_some_and(|d| d == digest));

        if let Some((img_uri, img)) = existing {
            // If an image with the same digest exists, we just need to tag it
            return Some(tag_image.with_target((img_uri, img)));
        }
    }

    // If the image requires auth, make sure there is an auth with this image
    // in scope before pulling the image
    if RegistryAuth::needs_auth(&image_name)
        && device.auths.iter().all(|auth| !auth.in_scope(&image_name))
    {
        return None;
    }

    // If the target image doesn't have a digest, just pull it
    Some(pull_image.into_task())
}

/// Do the initial pull for a new service
fn fetch_service_image(
    System(device): System<Device>,
    Target(tgt): Target<TargetService>,
    Args((app_uuid, commit, service_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    // tell the planner to skip this task if the image already exists
    if device.images.contains_key(&tgt.image) {
        return None;
    }

    // If there is a service with the same name in any other releases of the service
    // and the service image has as different digest then we skip the fetch as
    // that pull will need deltas and needs to be handled by another task
    if find_installed_service(&device, &app_uuid, &commit, &service_name)
        .is_none_or(|svc| svc.image.digest().is_some() && tgt.image.digest() == svc.image.digest())
    {
        return Some(create_image.with_arg("image_name", tgt.image));
    }

    None
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

/// Normalize the in-memory service image info
///
/// If all that changes from a service is the image, it means the docker image may be missing the
/// digest expected on the target. In that case we just update the service to allow
/// the planner to get to the target.
fn normalize_service_image(
    mut svc: View<Service>,
    Target(tgt): Target<TargetService>,
) -> View<Service> {
    let TargetService { id, image, .. } = tgt;

    if svc.id != id {
        return svc;
    }

    svc.image = image;
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
        // XXX: this is not added first because of
        // https://github.com/balena-io-modules/mahler-rs/pull/50
        .jobs(
            "",
            [
                task::update(ensure_cleanup).with_description(|| "ensure clean-up"),
                task::update(perform_cleanup).with_description(|| "perform clean-up"),
            ],
        )
        .job(
            "/auths",
            task::none(request_registry_credentials)
                .with_description(|| "request registry credentials"),
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
                    |Args(image_name): Args<ImageUri>,
                     Target((src_uri, _)): Target<(ImageUri, Image)>| {
                        format!("tag image '{src_uri}' with '{}'", image_name.repo())
                    },
                ),
                task::none(create_image),
            ],
        )
        .jobs(
            "/apps",
            [
                task::update(request_registry_token_for_new_images),
                task::update(fetch_apps_images),
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
        .jobs(
            "/apps/{app_uuid}/releases/{commit}",
            [task::create(prepare_release).with_description(
                |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                    format!("initialize release '{commit}' for app with uuid '{uuid}'")
                },
            )],
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
                task::update(normalize_service_image).with_description(
                    |Args((_, _, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("normalize image info for service {service_name}")
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
        let expected: Dag<&str> = seq!(
            "ensure clean-up",
            "initialize app with uuid 'my-app-uuid'",
            "perform clean-up"
        );
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
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!("ensure clean-up")
            + par!(
                "update device name",
                "initialize app with uuid 'my-app-uuid'",
            )
            + seq!("perform clean-up");
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_update_an_app_and_configs() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "name": "my-device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-old-name"
                }
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<TargetDevice>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app"
                }
            },
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!("ensure clean-up")
            + par!(
                "update device name",
                "update name for app with uuid 'my-app-uuid'",
            )
            + seq!("perform clean-up");
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
        let expected: Dag<&str> = seq!(
            "ensure clean-up",
            "update name for app with uuid 'my-app-uuid'",
            "perform clean-up"
        );
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
                                "service1": {
                                    "id": 1,
                                    "image": "ubuntu:latest"
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080"
                                },
                                "service3": {
                                    "id": 3,
                                    // different image same digest
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080"
                                },
                                // additional images to test download batching
                                "service4": {
                                    "id": 4,
                                    "image": "alpine:latest"
                                },
                                "service5": {
                                    "id": 5,
                                    "image": "alpine:3.20"
                                },
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!("ensure clean-up", "request registry credentials")
            + par!(
                "pull image 'ubuntu:latest'",
                "pull image 'registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080'",
                "pull image 'alpine:latest'",
            )
            + par!(
                "tag image 'registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080' with 'registry2.balena-cloud.com/v2/deafc41f'",
                "pull image 'alpine:3.20'",
            )
            + seq!(
                "initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'",
                "initialize service 'service1' for release 'my-release-uuid'",
                "initialize service 'service2' for release 'my-release-uuid'",
                "initialize service 'service3' for release 'my-release-uuid'",
                "initialize service 'service4' for release 'my-release-uuid'",
                "initialize service 'service5' for release 'my-release-uuid'",
                "perform clean-up"
            );

        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    // The worker doesn't have any tasks to update services or delete releases
    // so this plan should fail
    #[tokio::test]
    async fn it_fails_to_find_a_workflow_for_updating_services() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "old-release": {
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc1@sha256:a111111111111111111111111111111111111111111111111111111111111111"
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222"
                                },

                            }
                        }
                    }
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
                        "new-release": {
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc1@sha256:b111111111111111111111111111111111111111111111111111111111111111"
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222"
                                },

                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

        // this should return Err(NotFound) and not panic
        let workflow = worker().find_workflow(initial_state, target);
        assert!(workflow.is_err(), "unexpected plan:\n{}", workflow.unwrap());
    }
}
