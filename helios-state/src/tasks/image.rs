use tokio::sync::RwLock;

use mahler::extract::{Args, RawTarget, Res, System, Target, View};
use mahler::job;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};

use crate::common_types::ImageUri;
use crate::models::{Device, Image, ImageRef, RegistryAuthSet};
use crate::oci::{Client as Docker, Error as DockerError};
use crate::oci::{Credentials, RegistryAuth, RegistryAuthClient, RegistryAuthError};

/// Request registry tokens
pub(super) fn request_registry_credentials(
    mut auths: View<RegistryAuthSet>,
    RawTarget(tgt_auths): RawTarget<RegistryAuthSet>,
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

/// Tag an existing image with a new name
fn tag_image(
    image: View<Option<Image>>,
    Target(tgt): Target<Image>,
    Args(image_name): Args<ImageUri>,
    docker: Res<Docker>,
) -> IO<Image, DockerError> {
    // probably unnecessary, just some defensive programming
    enforce!(image.is_none(), "image already exists");

    let engine_id = tgt.engine_id.clone();
    let image = image.create(tgt.into());

    with_io(image, |image| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        docker.image().tag(&engine_id, &image_name).await?;

        Ok(image)
    })
}

/// Pull an image from the registry, this task is applicable to
/// the creation of a new image
///
/// Condition: the image is not already present in the device
/// Effect: add the image to the list of images
/// Action: pull the image from the registry and add it to the images local registry
pub(super) fn pull_image(
    mut image: View<Option<Image>>,
    Args(image_name): Args<ImageUri>,
    docker: Res<Docker>,
    auth_client: Res<RwLock<RegistryAuthClient>>,
) -> IO<Option<Image>, DockerError> {
    // Initialize the image if it doesn't exist
    if image.is_none() {
        image.replace(Image {
            engine_id: image_name.to_string(),
            labels: Default::default(),
        });
    }

    with_io(image, |mut image| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available")
            .clone();

        // FIXME: make registry auth easier to work with
        let credentials = if let Some(auth_client_rwlock) = auth_client.as_ref() {
            auth_client_rwlock
                .read()
                .await
                .token(&image_name)
                .map(|token| Credentials {
                    registrytoken: Some(token.to_string()),
                    ..Default::default()
                })
        } else {
            None
        };

        // FIXME: progress reporting
        docker.image().pull(&image_name, credentials).await?;
        let new_img: Image = docker.image().inspect(&image_name).await?.into();

        image.replace(new_img);

        Ok(image)
    })
}

/// Creates a new image with the given name.
///
/// Depending on whether the image has a digest or there is another matching image, a different
/// operation will be chosen
pub(super) fn create_image(
    Args(image_name): Args<ImageUri>,
    System(device): System<Device>,
) -> Option<Task> {
    if let Some(digest) = image_name.digest() {
        let existing = device
            .images
            .iter()
            .find(|(name, _)| name.digest().is_some_and(|d| d == digest));

        if let Some((_, image)) = existing {
            // If an image with the same digest exists, we just need to tag it
            return Some(tag_image.with_target(image.clone()));
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

/// Remove an image
///
/// Condition: the image exists and there are no services referencing it
/// Effect: remove the image from the state
/// Action: remove the image from the engine
fn remove_image(
    img_ptr: View<Image>,
    Args(image_name): Args<ImageUri>,
    System(device): System<Device>,
    docker: Res<Docker>,
) -> IO<Option<Image>, DockerError> {
    // only remove the image if it not being used by any service
    if device.apps.values().any(|app| {
        app.releases.values().any(|release| {
            release.services.values().any(|s| {
                if let ImageRef::Uri(s_img) = &s.image {
                    s_img == &image_name
                } else {
                    false
                }
            })
        })
    }) {
        return IO::abort("image is still in use");
    }

    // delete the image
    let img_ptr = img_ptr.delete();

    with_io(img_ptr, |img_ptr| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        docker.image().remove(&image_name).await?;

        Ok(img_ptr)
    })
}

/// Update worker with image tasks
pub fn with_image_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    worker
        .job(
            "/auths",
            job::none(request_registry_credentials)
                .with_description(|| "request registry credentials"),
        )
        .jobs(
            "/images/{image_name}",
            [
                job::none(pull_image).with_description(|Args(image_name): Args<String>| {
                    format!("pull image '{image_name}'")
                }),
                job::none(remove_image).with_description(|Args(image_name): Args<String>| {
                    format!("delete image '{image_name}'")
                }),
                job::none(tag_image).with_description(
                    |Args(image_name): Args<String>, tgt: Target<Image>| {
                        format!("tag image '{}' as '{image_name}'", tgt.engine_id)
                    },
                ),
                job::none(create_image),
            ],
        )
}
