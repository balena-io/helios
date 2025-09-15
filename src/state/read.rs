use bollard::{query_parameters::ListImagesOptions, Docker};
use thiserror::Error;
use tracing::instrument;

use crate::types::{OperatingSystem, Uuid};
use crate::util::docker::{ImageUri, InvalidImageUriError};

use super::local::{self, LocalStateError};
use super::models::{App, Device, Image};

#[derive(Debug, Error)]
pub enum ReadStateError {
    #[error(transparent)]
    DockerError(#[from] bollard::errors::Error),

    #[error(transparent)]
    InvalidRegistryUri(#[from] InvalidImageUriError),

    #[error(transparent)]
    LocalStateError(#[from] LocalStateError),
}

/// Read the state of system
#[instrument(name = "read_state", skip_all)]
pub async fn read(
    docker: &Docker,
    uuid: Uuid,
    os: Option<OperatingSystem>,
) -> Result<Device, ReadStateError> {
    let mut device = Device::new(uuid, os);

    // read device name
    device.name = local::read("/name").await?;

    let installed_images = docker
        .list_images(Some(ListImagesOptions {
            all: true,
            ..Default::default()
        }))
        .await?;

    // Read the state of images
    for img_summary in installed_images {
        let repo_tags = img_summary.repo_tags;
        let img: Image = docker.inspect_image(&img_summary.id).await?.into();

        for img_tag in repo_tags {
            let mut img_name: ImageUri = img_tag.parse()?;

            // If the image name has a tag starting with 'sha256-' use that as the digest.
            //
            // This is needed because the digest doesn't survive when pulling with deltas
            // https://github.com/balena-os/balena-engine/issues/283
            if let Some(tag) = img_name.tag() {
                if tag.starts_with("sha256-") {
                    img_name = format!("{}@{}", img_name.repo(), tag.replace("sha256-", "sha256:"))
                        .parse()?;
                }
            }

            device.images.insert(img_name, img.clone());
        }
    }

    // TODO: read state of apps
    let app_uuids: Vec<Uuid> = local::read_all("/apps").await?;
    for app_uuid in app_uuids {
        let id: Option<u32> = local::read(&format!("/apps/{app_uuid}/id")).await?;
        if let Some(id) = id {
            let name: Option<String> = local::read(&format!("/apps/{app_uuid}/name")).await?;

            // FIXME: we probably don't want to read all apps in the local dir
            // but read containers and infer apps from there, but we'll do this for now
            device.apps.insert(
                app_uuid,
                App {
                    id,
                    name,
                    ..Default::default()
                },
            );
        }
    }

    Ok(device)
}
