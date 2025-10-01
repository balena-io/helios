use std::collections::BTreeMap;

use thiserror::Error;
use tracing::instrument;

use crate::oci::{
    Client as Docker, Error as DockerError, ImageRef, ImageUri, InvalidImageUriError, WithContext,
};
use crate::util::state;
use crate::util::types::{OperatingSystem, Uuid};

use super::models::{App, Device, Release, Service, ServiceContainerName, UNKNOWN_APP_UUID};

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

    let apps = &mut device.apps;

    // Read the state of apps from containers
    let containers = docker
        .container()
        .list_with_labels(vec!["io.baleena.supervised"])
        .await
        .context("failed to read state of containers")?;

    for (container_name, local_container) in containers.iter() {
        let ServiceContainerName {
            service_name,
            release_uuid,
        } = container_name
            .parse()
            // this should not happen
            .expect("invalid container name");

        let labels = local_container.labels;

        let app_uuid: Uuid = labels
            .get("io.balena.app-uuid")
            .map(|uuid| uuid.as_str().into())
            .unwrap_or(UNKNOWN_APP_UUID.into());

        // Create the app if it doesn't exist yet
        let app = apps.entry(app_uuid.clone()).or_insert_with(|| {
            let id: u32 = labels
                .get("io.balena.app-id")
                .and_then(|id| id.parse().ok())
                .unwrap_or(0);

            App {
                id,
                name: None,
                releases: BTreeMap::new(),
            }
        });

        // Read the app name from the local state
        if app.name.is_none() {
            app.name = state::read(&format!("/apps/{app_uuid}/name")).await?;
        }

        // Create the release for the uuid if it doesn't exist
        let release = app.releases.entry(release_uuid.clone()).or_insert(Release {
            services: BTreeMap::new(),
        });

        let svc_img: Option<ImageUri> = state::read(&format!(
            "/apps/{app_uuid}/releases/{release_uuid}/services/{service_name}/image"
        ))
        .await?;

        // Insert the service and link it to the image if there is state
        // metadata about the image
        let image = if let Some(svc_img) = svc_img {
            ImageRef::Uri(svc_img)
        } else {
            ImageRef::Id(local_container.image_id)
        };

        let svc_id: u32 = labels
            .get("io.balena.service-id")
            .and_then(|id| id.parse().ok())
            .unwrap_or(0);
        release
            .services
            .insert(service_name, Service { id: svc_id, image });
    }

    Ok(device)
}
