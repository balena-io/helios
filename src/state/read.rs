use bollard::{query_parameters::ListImagesOptions, Docker};
use thiserror::Error;
use tracing::instrument;

use crate::util::docker::normalise_image_name;
use crate::{types::Uuid, util::docker::ImageNameError};

use super::models::{Device, Image};

#[derive(Debug, Error)]
pub enum ReadStateError {
    #[error(transparent)]
    DockerError(#[from] bollard::errors::Error),

    #[error(transparent)]
    ImageName(#[from] ImageNameError),
}

/// Read the state of system
#[instrument(name = "read_state", skip_all)]
pub async fn read(uuid: Uuid) -> Result<Device, ReadStateError> {
    let mut device = Device::new(uuid);

    let docker = Docker::connect_with_defaults()?;

    let installed_images = docker
        .list_images(Some(ListImagesOptions {
            all: true,
            ..Default::default()
        }))
        .await?;

    // Read the state of images
    for img_summary in installed_images {
        let img: Image = docker.inspect_image(&img_summary.id).await?.into();

        // we'll store the image digest as a label on the image
        let digest = img
            .labels
            .as_ref()
            .and_then(|labels| labels.get("io.balena.private.image.digest"));
        for tag in img_summary.repo_tags {
            let img_name = if let Some(dig) = digest {
                // if a digest is present, include the digest in the image name to match
                // the expected format for the target state
                normalise_image_name(&format!("{tag}@{dig}"))?
            } else {
                tag
            };
            device.images.insert(img_name, img.clone());
        }
    }

    // TODO: read state of apps

    Ok(device)
}
