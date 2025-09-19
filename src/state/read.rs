use thiserror::Error;
use tracing::instrument;

use crate::oci::{Client as Docker, Error as DockerError, InvalidImageUriError, WithContext};
use crate::types::{OperatingSystem, Uuid};

use super::models::Device;

#[derive(Debug, Error)]
pub enum ReadStateError {
    #[error(transparent)]
    DockerError(#[from] DockerError),

    #[error(transparent)]
    InvalidRegistryUri(#[from] InvalidImageUriError),
}

/// Read the state of system
#[instrument(name = "read_state", skip_all)]
pub async fn read(
    docker: &Docker,
    uuid: Uuid,
    os: Option<OperatingSystem>,
) -> Result<Device, ReadStateError> {
    let mut device = Device::new(uuid, os);

    // Read the state of images
    let res = docker.image().list().await;
    let images = res.context("failed to read state of images")?;
    for res in images.iter() {
        let (uri, image) = res?;
        device.images.insert(uri, image.into());
    }

    // TODO: read state of apps

    Ok(device)
}
