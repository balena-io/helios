use thiserror::Error;
use tracing::instrument;

use crate::common_types::{InvalidImageUriError, OperatingSystem, Uuid};
use crate::oci::{Client as Docker, Error as DockerError, WithContext};
use crate::util::store::{Store, StoreError};

use super::models::{Device, Host};

#[derive(Debug, Error)]
pub enum ReadStateError {
    #[error(transparent)]
    DockerError(#[from] DockerError),

    #[error(transparent)]
    InvalidRegistryUri(#[from] InvalidImageUriError),

    #[error(transparent)]
    StoreReadFailed(#[from] StoreError),
}

/// Read the state of system
#[instrument(name = "read_state", skip_all)]
pub async fn read(
    docker: &Docker,
    local_store: &Store,
    uuid: Uuid,
    os: Option<OperatingSystem>,
) -> Result<Device, ReadStateError> {
    let mut device = Device::new(uuid, os);

    // read the device name from the local store
    device.name = local_store.read("/", "device_name").await?;

    // Read the host app information from the local store
    device.host = local_store
        .read("/host", "app_uuid")
        .await?
        .map(|app_uuid| Host {
            app_uuid,
            releases: Default::default(),
        });

    if let Some(host) = &mut device.host {
        let host_releases: Vec<Uuid> = local_store.list("/host/releases").await?;
        for release_uuid in host_releases {
            if let Some(hostapp) = local_store
                .read(format!("/host/releases/{release_uuid}"), "hostapp")
                .await?
            {
                host.releases.insert(release_uuid, hostapp);
            }
        }
    }

    // Read the state of images
    let res = docker.image().list().await;
    let images = res.context("failed to read state of images")?;
    for res in images.iter() {
        let (uri, image) = res?;
        device.images.insert(uri, image.into());
    }

    // TODO: read state of apps if the `userapps` feature is enabled

    Ok(device)
}
