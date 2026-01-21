use futures_lite::StreamExt;
use tokio::sync::RwLock;
use tracing::trace;

use mahler::extract::{Args, RawTarget, Res, System, View};
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
    // because this function is only called from a method (composite task), we can use RawTarget
    // and receive any serializable data as the target
    RawTarget((src_img_name, src_img)): RawTarget<(String, Image)>,
    Args(image_name): Args<ImageUri>,
    docker: Res<Docker>,
) -> IO<Image, DockerError> {
    // probably unnecessary, just some defensive programming
    enforce!(image.is_none(), "image already exists");

    // if both source image and the tagged image are being
    // created as part of the same plan, the src_image engine id will be null,
    // because the target is created at planning and is not updated at runtime
    // for this reason, we use the src_img_name if that's the case. It's also the
    // same reason why the image is inspected again at the end of the IO block
    let image_ref = if let Some(id) = &src_img.engine_id {
        id.clone()
    } else {
        src_img_name
    };
    let image = image.create(src_img);

    with_io(image, |mut image| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        // tag the image
        docker.image().tag(&image_ref, &image_name).await?;

        // get the updated image information
        *image = docker.image().inspect(image_name.as_str()).await?.into();

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
    image: View<Option<Image>>,
    Args(image_name): Args<ImageUri>,
    docker: Res<Docker>,
    auth_client: Res<RwLock<RegistryAuthClient>>,
) -> IO<Image, DockerError> {
    // probably unnecessary, just some defensive programming
    enforce!(image.is_none(), "image already exists");

    // Initialize the image if it doesn't exist
    let image = image.create(Image {
        engine_id: None,
        config: Default::default(),
        download_progress: 0,
    });

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

        // Report the state
        let _ = image.flush().await;

        // Report progress
        let mut stream = docker.image().pull_with_progress(&image_name, credentials);
        let mut last_logged = -1;
        while let Some(result) = stream.next().await {
            let (current, total) = result?;
            let percent = 100 * current / total;
            let bucket = percent / 20;

            // limit the number of reports per pull to avoid spamming
            // the backend
            if bucket > last_logged {
                trace!("progress={percent}%");
                last_logged = bucket;

                image.download_progress = percent;

                // report download progress back to the worker
                let _ = image.flush().await;
            }
        }
        trace!("progress=100%");

        let new_img: Image = docker.image().inspect(image_name.as_str()).await?.into();

        *image = new_img;

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
            .into_iter()
            .find(|(name, _)| name.digest().is_some_and(|d| d == digest));

        if let Some((src_img_name, src_img)) = existing {
            // If an image with the same digest exists, we just need to tag it
            return Some(tag_image.with_target((src_img_name, src_img)));
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
                    |Args(image_name): Args<String>,
                     RawTarget((src_img_name, _)): RawTarget<(String, Image)>| {
                        format!("tag image '{src_img_name}' as '{image_name}'")
                    },
                ),
                job::none(create_image),
            ],
        )
}
