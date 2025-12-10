use std::collections::HashMap;

use mahler::extract::{Args, Res, System, Target, View};
use mahler::state::Map;
use mahler::task::prelude::*;
use mahler::{
    task,
    worker::{Uninitialized, Worker},
};

use crate::common_types::{ImageUri, Uuid};
use crate::models::{
    App, AppMap, AppTarget, Device, RegistryAuthSet, Release, Service, ServiceTarget,
};
use crate::oci::RegistryAuth;
use crate::util::store::{Store, StoreError};

use super::image::{create_image, request_registry_credentials};
use super::utils::find_installed_service;

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
                    .filter(|(service_name, svc)| {
                        // if the service image has not been downloaded
                        !device.images.contains_key(&svc.image) &&
                        // and there is no service from a different release
                        find_installed_service(
                            &device,
                            app_uuid.clone(),
                            commit.clone(),
                            (*service_name).clone(),
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
        .iter()
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
        .filter(|scope| device.auths.iter().all(|s| !s.is_super_scope(scope)))
        .collect();

    if tgt_auths.is_empty() {
        return None;
    }

    Some(request_registry_credentials.with_target(tgt_auths))
}

/// Initialize the app and store its local data
fn prepare_app(
    mut maybe_app: View<Option<App>>,
    Target(tgt_app): Target<App>,
    Args(app_uuid): Args<Uuid>,
    store: Res<Store>,
) -> IO<Option<App>, StoreError> {
    let AppTarget { id, name, .. } = tgt_app;
    maybe_app.replace(App {
        id,
        name,
        releases: Map::new(),
    });

    with_io(maybe_app, async move |maybe_app| {
        // store id and name as local state
        if let (Some(app), Some(local_store)) = (maybe_app.as_ref(), store.as_ref()) {
            local_store
                .write(format!("/apps/{app_uuid}"), "id", &app.id)
                .await?;
            local_store
                .write(format!("/apps/{app_uuid}"), "name", &app.name)
                .await?;
        }

        Ok(maybe_app)
    })
}

/// Update the local app name
fn set_app_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
    Args(app_uuid): Args<String>,
    store: Res<Store>,
) -> IO<Option<String>, StoreError> {
    *name = tgt;
    with_io(name, async move |name| {
        if let Some(local_store) = store.as_ref() {
            local_store
                .write(format!("/apps/{app_uuid}"), "name", &*name)
                .await?;
        }
        Ok(name)
    })
}

/// Install all new images for a release
fn fetch_release_images(
    System(device): System<Device>,
    Target(tgt_release): Target<Release>,
    Args((app_uuid, commit)): Args<(Uuid, Uuid)>,
) -> Vec<Task> {
    // Find all images for new services in the target state
    let images_to_install: Vec<ImageUri> = tgt_release
        .services
        .into_iter()
        .filter(|(service_name, svc)| {
            // if the service image has not been downloaded
            !device.images.contains_key(&svc.image) &&
            // and there is no service from a different release
            find_installed_service(
                    &device,
                    app_uuid.clone(),
                    commit.clone(),
                    (*service_name).clone(),
                )
                // or if there is a service, then only chose it if it has the same digest
                // as the target service
                .is_none_or(|s| {
                    s.image.digest().is_some() && svc.image.digest() == s.image.digest()
                })
        })
        // remove duplicate digests
        .fold(Vec::<ImageUri>::new(), |mut acc, (_, svc)| {
            if acc
                .iter()
                .all(|img| img.digest().is_none() || img.digest() != svc.image.digest())
            {
                acc.push(svc.image);
            }
            acc
        });

    // only download at most 3 images at the time
    let mut tasks: Vec<Task> = Vec::new();
    for image in images_to_install.into_iter().take(3) {
        tasks.push(create_image.with_arg("image_name", image))
    }
    tasks
}

/// Initialize an empty release
fn prepare_release(mut release: View<Option<Release>>) -> View<Option<Release>> {
    release.replace(Release {
        services: Map::new(),
    });
    release
}

/// Do the initial pull for a new service
fn fetch_service_image(
    System(device): System<Device>,
    Target(tgt): Target<Service>,
    Args((app_uuid, commit, service_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    // tell the planner to skip this task if the image already exists
    if device.images.contains_key(&tgt.image) {
        return None;
    }

    // If there is a service with the same name in any other releases of the service
    // and the service image has as different digest then we skip the fetch as
    // that pull will need deltas and needs to be handled by another task
    if find_installed_service(&device, app_uuid, commit, service_name)
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
    mut maybe_svc: View<Option<Service>>,
    System(device): System<Device>,
    Target(tgt): Target<Service>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
    store: Res<Store>,
) -> IO<Option<Service>, StoreError> {
    // Skip the task if the image for the service doesn't exist yet
    if device.images.keys().all(|img| img != &tgt.image) {
        return maybe_svc.into();
    }

    let ServiceTarget { id, image, .. } = tgt;
    maybe_svc.replace(Service { id, image });

    with_io(maybe_svc, async move |maybe_svc| {
        // FIXME: create/manage container

        if let (Some(svc), Some(local_store)) = (maybe_svc.as_ref(), store.as_ref()) {
            // store the image uri that corresponds to the current release service
            local_store
                .write(
                    format!("/apps/{app_uuid}/releases/{commit}/services/{svc_name}"),
                    "image",
                    &svc.image,
                )
                .await?;
        }

        Ok(maybe_svc)
    })
}

/// Update worker with user app tasks
pub fn with_userapp_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    worker
        .job("/apps", task::update(request_registry_token_for_new_images))
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
            [
                task::create(fetch_release_images),
                task::create(prepare_release).with_description(
                    |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                        format!("initialize release '{commit}' for app with uuid '{uuid}'")
                    },
                ),
            ],
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
