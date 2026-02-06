use std::collections::HashMap;

use mahler::extract::{Args, Res, System, Target, View};
use mahler::job;
use mahler::state::Map;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};

use crate::common_types::{ImageUri, Uuid};
use crate::models::{
    App, AppMap, AppTarget, Device, ImageRef, RegistryAuthSet, Release, Service, ServiceStatus,
    ServiceTarget, mark_duplicate_service_config,
};
use crate::oci::{Client as Docker, Error as OciError, RegistryAuth, WithContext};
use crate::util::store::{Store, StoreError};

use super::image::{create_image, request_registry_credentials};
use super::utils::find_installed_service;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Oci(#[from] OciError),
}

/// Request authorization for new image installs
fn request_registry_token_for_new_images(
    System(device): System<Device>,
    Target(tgt_apps): Target<AppMap>,
) -> Option<Task> {
    // Find all images for new services in the target state
    let images_to_install: Vec<&ImageUri> = tgt_apps
        .iter()
        .flat_map(|(app_uuid, app)| {
            app.releases.iter().flat_map(|(commit, release)| {
                release
                    .services
                    .iter()
                    .filter_map(|(svc_name, svc)| {
                        if device
                            .apps
                            .get(app_uuid)
                            .and_then(|app| app.releases.get(commit))
                            .and_then(|rel| rel.services.get(svc_name))
                            .is_some()
                        {
                            // the service already exists, ignore it
                            return None;
                        }

                        if let ImageRef::Uri(img_uri) = &svc.image {
                            Some((svc_name, img_uri))
                        } else {
                            None
                        }
                    })
                    .filter(|(svc_name, svc_img)| {
                        // ignore the image if it already exists
                        if device.images.contains_key(svc_img) {
                            return false;
                        }

                        // if the image is for a new service or the existing service has a
                        // different digest (which means a new download), then add it to the
                        // authorization list
                        find_installed_service(&device, app_uuid, commit, svc_name).is_none_or(
                            |s| s.image.digest().is_none() || s.image.digest() != svc_img.digest(),
                        )
                    })
                    // then select the image
                    .map(|(_, svc_img)| svc_img)
            })
        })
        .collect();

    // Group images to install by registry
    let tgt_auths: RegistryAuthSet = images_to_install
        .iter()
        .fold(HashMap::<String, Vec<ImageUri>>::new(), |mut acc, img| {
            let img = (*img).clone();
            if let Some(service) = img.registry() {
                if let Some(scope) = acc.get_mut(service) {
                    scope.push(img);
                } else {
                    acc.insert(service.clone(), vec![img]);
                }
            }
            acc
        })
        .into_values()
        .map(|scope| RegistryAuth::try_from(scope).expect("auth creation should not fail"))
        .filter(|scope| device.auths.iter().all(|s| !s.is_super_scope(scope)))
        .collect();

    if tgt_auths.is_empty() {
        return None;
    }

    Some(request_registry_credentials.with_target(tgt_auths))
}

/// Initialize the app and store its local data
fn create_app(
    maybe_app: View<Option<App>>,
    Target(tgt_app): Target<App>,
    Args(app_uuid): Args<Uuid>,
    store: Res<Store>,
) -> IO<App, StoreError> {
    enforce!(maybe_app.is_none(), "app already exists");

    let AppTarget { id, name, .. } = tgt_app;
    let app = maybe_app.create(App {
        id,
        name,
        releases: Map::new(),
    });

    with_io(app, async move |app| {
        let local_store = store.as_ref().expect("store should be available");
        // store id and name as local state
        local_store
            .write(format!("/apps/{app_uuid}"), "id", &app.id)
            .await?;
        local_store
            .write(format!("/apps/{app_uuid}"), "name", &app.name)
            .await?;

        Ok(app)
    })
}

/// Update the local app id
fn store_app_id(
    mut id: View<u32>,
    Target(tgt): Target<u32>,
    Args(app_uuid): Args<String>,
    store: Res<Store>,
) -> IO<u32, StoreError> {
    *id = tgt;
    with_io(id, async move |id| {
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .write(format!("/apps/{app_uuid}"), "id", &*id)
            .await?;
        Ok(id)
    })
}

/// Update the local app name
fn store_app_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
    Args(app_uuid): Args<String>,
    store: Res<Store>,
) -> IO<Option<String>, StoreError> {
    *name = tgt;
    with_io(name, async move |name| {
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .write(format!("/apps/{app_uuid}"), "name", &*name)
            .await?;
        Ok(name)
    })
}

/// Install all new images for all target apps
fn fetch_apps_images(
    System(device): System<Device>,
    Target(tgt_apps): Target<AppMap>,
) -> Vec<Task> {
    // Find all images for new services in the target state
    let images_to_install: Vec<&ImageUri> = tgt_apps
        .iter()
        .flat_map(|(app_uuid, app)| {
            app.releases.iter().flat_map(|(commit, release)| {
                release
                    .services
                    .iter()
                    // find all services that need downloading
                    .filter_map(|(svc_name, svc)| {
                        if device
                            .apps
                            .get(app_uuid)
                            .and_then(|app| app.releases.get(commit))
                            .and_then(|rel| rel.services.get(svc_name))
                            .map(|svc| &svc.status)
                            != Some(&ServiceStatus::Installing)
                        {
                            // the service has already been downloaded, ignore
                            // the image
                            return None;
                        }

                        // only use the target image ref if it is an URI
                        // (this should always be the case)
                        if let ImageRef::Uri(img_uri) = &svc.image {
                            Some((svc_name, img_uri))
                        } else {
                            None
                        }
                    })
                    .filter(|(svc_name, svc_img)| {
                        // ignore the image if it already exists
                        if device.images.contains_key(svc_img) {
                            return false;
                        }

                        // select the image if it is for a new service or the existing service image has the
                        // same digest (which means we are just re-tagging)
                        find_installed_service(&device, app_uuid, commit, svc_name).is_none_or(
                            |s| s.image.digest().is_some() && svc_img.digest() == s.image.digest(),
                        )
                    })
            })
        })
        // remove duplicate digests
        .fold(Vec::<&ImageUri>::new(), |mut acc, (_, svc_img)| {
            if acc
                .iter()
                .all(|img| img.digest().is_none() || img.digest() != svc_img.digest())
            {
                acc.push(svc_img);
            }
            acc
        });

    // download at most 3 images at the time
    let mut tasks: Vec<Task> = Vec::new();
    for image in images_to_install.into_iter().take(3) {
        tasks.push(create_image.with_arg("image_name", image.clone()))
    }
    tasks
}

/// Initialize an empty release
fn create_release(release: View<Option<Release>>) -> View<Release> {
    release.create(Release {
        services: Map::new(),
    })
}

/// Create the service in memory before initiating download
fn create_service(maybe_svc: View<Option<Service>>, Target(tgt): Target<Service>) -> View<Service> {
    let ServiceTarget {
        id, image, config, ..
    } = tgt;
    maybe_svc.create(Service {
        id,
        image,
        status: ServiceStatus::Installing,
        container_id: None,
        config,
    })
}

/// Install the service if the images exist
///
/// Implement this as a method to allow to make concurrent service installs
fn install_service_after_fetch(
    System(device): System<Device>,
    Target(tgt): Target<Service>,
) -> Option<Task> {
    // Skip the task if the image for the service doesn't exist yet
    if let ImageRef::Uri(tgt_img) = &tgt.image
        && device.images.contains_key(tgt_img)
    {
        return Some(install_service.with_target(tgt));
    }

    None
}

/// Install the service
fn install_service(
    mut svc: View<Service>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
    store: Res<Store>,
) -> IO<Service, AppError> {
    svc.status = ServiceStatus::Installed;

    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");
        let container_name = svc
            .config
            .container_name
            .clone()
            .expect("container name should be available");

        if let ImageRef::Uri(svc_img) = svc.image.clone() {
            let image = docker
                .image()
                .inspect(svc_img.as_str())
                .await
                .with_context(|| format!("failed to inspect image {svc_img}"))?;

            // convert the service configuration to a container configuration
            // and mark duplicates from the image
            let mut container_config = svc.config.clone().into();
            mark_duplicate_service_config(&mut container_config, &image.config);

            let container_id = docker
                .container()
                .create(&container_name, &svc_img, container_config)
                .await?;

            // check that the container was created and generate the Service configuration
            // from the image config and container info
            let local_container = docker
                .container()
                .inspect(&container_id)
                .await
                .context("failed to inspect container for service")?;
            *svc = Service::from((&image.config, local_container));
            svc.image = ImageRef::Uri(svc_img);

            // store the image uri that corresponds to the current release service
            local_store
                .write(
                    format!("/apps/{app_uuid}/releases/{commit}/services/{svc_name}"),
                    "image",
                    &svc.image,
                )
                .await?;
        }

        Ok(svc)
    })
}

fn store_service_image_uri(
    mut img: View<ImageRef>,
    Target(tgt): Target<ImageUri>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
    store: Res<Store>,
) -> IO<ImageRef, StoreError> {
    *img = ImageRef::Uri(tgt);

    with_io(img, async move |img| {
        let local_store = store.as_ref().expect("store should be available");
        // store the image uri that corresponds to the current release service
        local_store
            .write(
                format!("/apps/{app_uuid}/releases/{commit}/services/{svc_name}"),
                "image",
                &*img,
            )
            .await?;
        Ok(img)
    })
}

/// Update worker with user app tasks
pub fn with_userapp_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    worker
        .jobs(
            "/apps",
            [
                job::update(request_registry_token_for_new_images),
                job::update(fetch_apps_images),
            ],
        )
        .job(
            "/apps/{app_uuid}",
            job::create(create_app).with_description(|Args(uuid): Args<Uuid>| {
                format!("initialize app with uuid '{uuid}'")
            }),
        )
        .job(
            "/apps/{app_uuid}/name",
            job::any(store_app_name).with_description(|Args(uuid): Args<Uuid>| {
                format!("store name for app with uuid '{uuid}'")
            }),
        )
        .job(
            "/apps/{app_uuid}/id",
            job::update(store_app_id).with_description(|Args(uuid): Args<Uuid>| {
                format!("store id for app with uuid '{uuid}'")
            }),
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}",
            [job::create(create_release).with_description(
                |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                    format!("initialize release '{commit}' for app with uuid '{uuid}'")
                },
            )],
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}",
            [
                job::create(create_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("initialize service '{service_name}' for release '{commit}'")
                    },
                ),
                job::update(install_service_after_fetch),
                job::none(install_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("install service '{service_name}' for release '{commit}'")
                    },
                ),
            ],
        )
        .job(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}/image",
            job::update(store_service_image_uri).with_description(
                |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                    format!(
                        "store image metadata for service '{service_name}' of release '{commit}'"
                    )
                },
            ),
        )
}
