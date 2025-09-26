use thiserror::Error;
use tracing::instrument;

use crate::oci::{Client as Docker, Error as DockerError, InvalidImageUriError, WithContext};
use crate::util::state;
use crate::util::types::{OperatingSystem, Uuid};

use super::models::{App, Device};

#[derive(Debug, Error)]
pub enum ReadStateError {
    #[error(transparent)]
    DockerError(#[from] DockerError),

    #[error(transparent)]
    InvalidRegistryUri(#[from] InvalidImageUriError),

    #[error(transparent)]
    PersistentStateError(#[from] state::ReadWriteError),
}

/// Read the state of system
#[instrument(name = "read_state", skip_all)]
pub async fn read(
    docker: &Docker,
    uuid: Uuid,
    os: Option<OperatingSystem>,
) -> Result<Device, ReadStateError> {
    let mut device = Device::new(uuid, os);

    // Read device name
    device.name = state::read("/name").await?;

    // Read the state of images
    let res = docker.image().list().await;
    let images = res.context("failed to read state of images")?;
    for res in images.iter() {
        let (uri, image) = res?;
        device.images.insert(uri, image.into());
    }

    // Read the state of apps
    let app_uuids: Vec<Uuid> = state::read_all("/apps").await?;
    for app_uuid in app_uuids {
        let id: Option<u32> = state::read(&format!("/apps/{app_uuid}/id")).await?;
        if let Some(id) = id {
            let name: Option<String> = state::read(&format!("/apps/{app_uuid}/name")).await?;

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
