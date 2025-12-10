use mahler::state::Map;
use thiserror::Error;
use tokio::fs;
use tracing::instrument;

use crate::common_types::{ImageUri, InvalidImageUriError, OperatingSystem, Uuid};
use crate::models::{HostRelease, HostReleaseStatus};
use crate::oci::{Client as Docker, Error as DockerError, WithContext};
use crate::util::dirs::runtime_dir;
use crate::util::store::{Store, StoreError};

use super::models::{App, Device, Release, Service, ServiceContainerName, UNKNOWN_APP_UUID};

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
        let apps = &mut device.apps;

        // read apps from local state
        let app_uuids: Vec<Uuid> = local_store.list("/apps").await?;
        for app_uuid in app_uuids {
            if let Some(id) = local_store.read(format!("/apps/{app_uuid}"), "id").await? {
                let name = local_store
                    .read(format!("/apps/{app_uuid}"), "name")
                    .await?;
                apps.insert(
                    app_uuid,
                    App {
                        id,
                        name,
                        releases: Map::new(),
                    },
                );
            }
        }

        // read the state of containers
        let containers = docker
            .container()
            .list_with_labels(vec!["io.balena.supervised"])
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
                    releases: Map::new(),
                }
            });

            // Read the app name from the local state
            if app.name.is_none() {
                app.name = local_store
                    .read(format!("/apps/{app_uuid}"), "name")
                    .await?;
            }

            // Create the release for the uuid if it doesn't exist
            let release = app.releases.entry(release_uuid.clone()).or_insert(Release {
                services: Map::new(),
            });

            // Get the image uri for the service from the local store
            let svc_img: Option<ImageUri> = local_store
                .read(
                    format!("/apps/{app_uuid}/releases/{release_uuid}/services/{service_name}"),
                    "image",
                )
                .await?;

            // Insert the service and link it to the image if there is state
            // metadata about the image
            // TODO: we might need to create an in-memory service referencing an image by id
            // rather than uri if the image does not exist in the local store
            if let Some(image) = svc_img {
                let svc_id: u32 = labels
                    .get("io.balena.service-id")
                    .and_then(|id| id.parse().ok())
                    .unwrap_or(0);

                release
                    .services
                    .insert(service_name, Service { id: svc_id, image });
            }
        }
    }

    Ok(device)
}
