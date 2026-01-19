use std::collections::HashMap;

use mahler::extract::{Args, Res, System, Target, View};
use mahler::job;
use mahler::state::Map;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};

use crate::common_types::{ImageUri, Uuid};
use crate::models::{
    App, AppMap, AppTarget, Device, ImageRef, RegistryAuthSet, Release, Service, ServiceTarget,
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
                    .filter_map(|(svc_name, svc)| {
                        if device
                            .apps
                            .get(app_uuid)
                            .and_then(|app| app.releases.get(commit))
                            .and_then(|rel| rel.services.get(svc_name))
                            .is_some()
                        {
                            // the service already exists, ignore the image
                            return None;
                        }

                        // only use the target image ref if it is an URI
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

    // only download at most 3 images at the time
    let mut tasks: Vec<Task> = Vec::new();
    for image in images_to_install.into_iter().take(3) {
        tasks.push(create_image.with_arg("image_name", image.clone()))
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
    if let ImageRef::Uri(tgt_img) = &tgt.image {
        enforce!(
            device.images.contains_key(tgt_img),
            "image {tgt_img} not found"
        );
    } else {
        // this should not happen
        return IO::abort("target image must be an URI");
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

fn update_service_image_metadata(
    mut img: View<ImageRef>,
    Target(tgt): Target<ImageUri>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
    store: Res<Store>,
) -> IO<ImageRef, StoreError> {
    *img = ImageRef::Uri(tgt);

    with_io(img, async move |img| {
        if let Some(local_store) = store.as_ref() {
            // store the image uri that corresponds to the current release service
            local_store
                .write(
                    format!("/apps/{app_uuid}/releases/{commit}/services/{svc_name}"),
                    "image",
                    &*img,
                )
                .await?;
        }
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
            job::create(prepare_app).with_description(|Args(uuid): Args<Uuid>| {
                format!("initialize app with uuid '{uuid}'")
            }),
        )
        .job(
            "/apps/{app_uuid}/name",
            job::any(set_app_name).with_description(|Args(uuid): Args<Uuid>| {
                format!("update name for app with uuid '{uuid}'")
            }),
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}",
            [job::create(prepare_release).with_description(
                |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                    format!("initialize release '{commit}' for app with uuid '{uuid}'")
                },
            )],
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}",
            [job::create(install_service).with_description(
                |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                    format!("initialize service '{service_name}' for release '{commit}'")
                },
            )],
        )
        .job(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}/image",
            job::update(update_service_image_metadata).with_description(
                |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                    format!(
                        "update image metadata for service '{service_name}' of release '{commit}'"
                    )
                },
            ),
        )
}
