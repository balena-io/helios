use mahler::state::Map;
use thiserror::Error;
use tokio::fs;
use tracing::instrument;

use crate::common_types::{InvalidImageUriError, OperatingSystem, Uuid};
use crate::labels::{LABEL_APP_UUID, LABEL_SERVICE_NAME, LABEL_SUPERVISED};
use crate::models::{HostRelease, HostReleaseStatus};
use crate::oci::{Client as Docker, Error as DockerError};
use crate::util::dirs::runtime_dir;
use crate::util::store::{Store, StoreError};

use super::models::{
    App, Device, Network, Release, Service, UNKNOWN_APP_UUID, UNKNOWN_RELEASE_UUID,
};

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

/// Find or create an app entry
async fn get_or_create_app<'a>(
    apps: &'a mut Map<Uuid, App>,
    app_uuid: &Uuid,
    local_store: &Store,
) -> Result<&'a mut App, ReadStateError> {
    if !apps.contains_key(app_uuid) {
        let name = local_store
            .read(format!("/apps/{app_uuid}"), "name")
            .await?;

        apps.insert(
            app_uuid.clone(),
            App {
                id: 0,
                name,
                releases: Map::new(),
            },
        );
    }

    Ok(apps.get_mut(app_uuid).expect("app was just inserted"))
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
    let images = docker.image().list().await?;
    for img_uri in images {
        let image = docker.image().inspect(img_uri.as_str()).await?;
        device.images.insert(img_uri, image.into());
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
            .list_with_labels(vec![LABEL_SUPERVISED])
            .await?;

        for container_id in containers {
            let local_container = docker.container().inspect(&container_id).await?;
            let labels = local_container.config.labels.as_ref();
            let app_uuid: Uuid = labels
                .and_then(|l| l.get(LABEL_APP_UUID))
                .map(|uuid| uuid.as_str())
                .unwrap_or(UNKNOWN_APP_UUID)
                .into();
            let service_name: String = labels
                .and_then(|l| l.get(LABEL_SERVICE_NAME))
                .map(|name| name.as_str())
                .unwrap_or(local_container.name.as_str())
                .into();
            let release_uuid: Uuid = local_container
                .name
                .strip_prefix(&format!("{service_name}_"))
                // if the remainder has underscores, assume the last
                // component to be the release uuid
                .and_then(|suffix| suffix.rsplit('_').next())
                // ignore the value if empty
                .filter(|r_uuid| !r_uuid.is_empty())
                .unwrap_or(UNKNOWN_RELEASE_UUID)
                .into();

            let app = get_or_create_app(apps, &app_uuid, local_store).await?;

            // Create the release for the uuid if it doesn't exist
            let release = if let Some(rel) = app.releases.get_mut(&release_uuid) {
                rel
            } else {
                // only read the install state when creating the release
                let installed = local_store
                    .read(
                        format!("/apps/{app_uuid}/releases/{release_uuid}"),
                        "installed",
                    )
                    .await?
                    .unwrap_or_default();

                app.releases.entry(release_uuid.clone()).or_insert(Release {
                    installed,
                    services: Map::new(),
                    networks: Map::new(),
                })
            };

            // Create the service configuration from the container and image config
            let image = docker.image().inspect(&local_container.image_id).await?;
            let mut svc = Service::from_local_container(local_container, &image.config);

            // Link the service to the local image if there is state metadata about it
            svc.image = local_store
                .read(
                    format!("/apps/{app_uuid}/releases/{release_uuid}/services/{service_name}"),
                    "image",
                )
                .await?
                // use the image id if no store metadata is available
                .unwrap_or(svc.image);

            release.services.insert(service_name, svc);
        }

        // read the state of networks
        let networks = docker
            .network()
            .list_with_labels(vec![LABEL_SUPERVISED])
            .await?;

        for network_name in networks {
            let local_network = docker.network().inspect(&network_name).await?;

            let app_uuid: Uuid = local_network
                .labels
                .get(LABEL_APP_UUID)
                .map(|uuid| uuid.as_str())
                .unwrap_or(UNKNOWN_APP_UUID)
                .into();

            // Extract the network name by stripping the "{app_uuid}_" prefix
            let net_name = local_network
                .name
                .strip_prefix(&format!("{app_uuid}_"))
                .unwrap_or(&local_network.name)
                .to_owned();

            let network: Network = local_network.into();

            let app = get_or_create_app(apps, &app_uuid, local_store).await?;

            // If app_uuid is unknown, insert orphaned network under
            // unknown release to prepare for cleanup
            let release = if let Some(release) = app.releases.values_mut().next() {
                release
            } else {
                app.releases
                    .entry(UNKNOWN_RELEASE_UUID.into())
                    .or_insert(Release {
                        installed: false,
                        services: Map::new(),
                        networks: Map::new(),
                    })
            };
            release.networks.insert(net_name, network);
        }
    }

    Ok(device)
}
