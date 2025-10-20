use mahler::extract::{Args, System, Target, View};
use mahler::task::prelude::*;

use crate::common_types::{ImageUri, Uuid};
use crate::models::{App, AppTarget, Device, Release, ReleaseTarget, Service, ServiceTarget};

use super::image::create_image;
use super::utils::find_installed_service;

/// Initialize the app in memory
pub fn prepare_app(
    mut app: View<Option<App>>,
    Target(tgt_app): Target<AppTarget>,
) -> View<Option<App>> {
    let AppTarget { id, name, .. } = tgt_app;
    app.replace(App {
        id,
        name,
        ..Default::default()
    });
    app
}

/// Update the in-memory app name
pub fn set_app_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
) -> View<Option<String>> {
    *name = tgt;
    name
}

/// Install all new images for a release
pub fn fetch_release_images(
    System(device): System<Device>,
    Target(tgt_release): Target<ReleaseTarget>,
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
pub fn prepare_release(mut release: View<Option<Release>>) -> View<Option<Release>> {
    release.replace(Release::default());
    release
}

/// Do the initial pull for a new service
pub fn fetch_service_image(
    System(device): System<Device>,
    Target(tgt): Target<ServiceTarget>,
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
pub fn install_service(
    mut svc: View<Option<Service>>,
    System(device): System<Device>,
    Target(tgt): Target<ServiceTarget>,
) -> View<Option<Service>> {
    // Skip the task if the image for the service doesn't exist yet
    if device.images.keys().all(|img| img != &tgt.image) {
        return svc;
    }

    let ServiceTarget { id, image, .. } = tgt;

    svc.replace(Service { id, image });
    svc
}
