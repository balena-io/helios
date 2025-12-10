use mahler::state::Map;
use thiserror::Error;
use tokio::fs;
use tracing::instrument;

use crate::common_types::{InvalidImageUriError, OperatingSystem, Uuid};
use crate::models::{HostRelease, HostReleaseStatus};
use crate::oci::{Client as Docker, Error as DockerError, WithContext};
use crate::util::dirs::runtime_dir;
use crate::util::store::{Store, StoreError};

use super::models::{App, Device};

#[derive(Debug, Error)]
pub enum ReadStateError {
    #[error(transparent)]
    DockerError(#[from] DockerError),

    #[error(transparent)]
    InvalidRegistryUri(#[from] InvalidImageUriError),

    #[error(transparent)]
    StoreReadFailed(#[from] StoreError),

    #[error(transparent)]
    IO(#[from] std::io::Error),
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

    // Read the hostapp information from the local store
    if let Some(host) = &mut device.host {
        let host_releases: Vec<Uuid> = local_store.list("/host/releases").await?;
        for release_uuid in host_releases {
            if let Some(mut hostapp) = local_store
                .read::<_, HostRelease>(format!("/host/releases/{release_uuid}"), "hostapp")
                .await?
            {
                // ignore the status on the store and deduce it instead
                hostapp.status = if host.meta.build.as_ref() == Some(&hostapp.build) {
                    HostReleaseStatus::Running
                } else if fs::try_exists(
                    runtime_dir().join(format!("balenahup-{release_uuid}-breadcrumb")),
                )
                .await?
                {
                    HostReleaseStatus::Installed
                } else {
                    HostReleaseStatus::Created
                };

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

    // read state of apps if the `userapps` feature is enabled
    if cfg!(feature = "userapps") {
        let app_uuids: Vec<Uuid> = local_store.list("/apps").await?;
        for app_uuid in app_uuids {
            if let Some(id) = local_store.read(&format!("/apps/{app_uuid}"), "id").await? {
                let name = local_store
                    .read(format!("/apps/{app_uuid}"), "name")
                    .await?;
                device.apps.insert(
                    app_uuid,
                    App {
                        id,
                        name,
                        releases: Map::new(),
                    },
                );
            }
        }
    }

    Ok(device)
}
