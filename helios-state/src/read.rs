use mahler::state::Map;
use thiserror::Error;
use tracing::{instrument, trace};

use crate::common_types::{InvalidImageUriError, OperatingSystem, Uuid};
use crate::labels::{LABEL_APP_UUID, LABEL_SERVICE_NAME, LABEL_SUPERVISED};
use crate::oci::{self, Client as Docker};
use crate::store::{self, DocumentStore};

use super::models::{
    App, Device, Network, Release, Service, UNKNOWN_APP_UUID, UNKNOWN_RELEASE_UUID, Volume,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Oci(#[from] oci::Error),

    #[error(transparent)]
    InvalidRegistryUri(#[from] InvalidImageUriError),

    #[error(transparent)]
    Store(#[from] store::Error),

    #[error(transparent)]
    IO(#[from] std::io::Error),
}

#[cfg(feature = "balenahup")]
impl From<crate::balenahup::read::Error> for Error {
    fn from(value: crate::balenahup::read::Error) -> Error {
        use crate::balenahup::read::Error::*;
        match value {
            Oci(e) => Error::Oci(e),
            Store(e) => Error::Store(e),
            IO(e) => Error::IO(e),
        }
    }
}

/// Find or create an app entry
async fn get_or_create_app<'a>(
    apps: &'a mut Map<Uuid, App>,
    app_uuid: &Uuid,
    local_store: &DocumentStore,
) -> Result<&'a mut App, Error> {
    if !apps.contains_key(app_uuid) {
        let name = local_store.get(format!("apps/{app_uuid}/name")).await?;

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
    local_store: &DocumentStore,
    uuid: Uuid,
    os: Option<OperatingSystem>,
) -> Result<Device, Error> {
    let mut device = Device::new(uuid, os);

    // read the device name from the local store
    device.name = local_store.get("device_name").await?;

    // Read the hostapp information from the local store
    #[cfg(feature = "balenahup")]
    if let Some(host) = &mut device.host {
        crate::balenahup::read::from_store(host, local_store).await?;
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
        let apps_view = local_store.as_view().at("apps")?;
        let app_uuids: Vec<Uuid> = apps_view
            .keys()
            .await?
            .into_iter()
            .map(Uuid::from)
            .collect();
        for app_uuid in app_uuids {
            if let Some(id) = apps_view.get(format!("{app_uuid}/id")).await? {
                let name = apps_view.get(format!("{app_uuid}/name")).await?;
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
                let installed = apps_view
                    .get(format!("{app_uuid}/releases/{release_uuid}/installed"))
                    .await?
                    .unwrap_or_default();

                app.releases.entry(release_uuid.clone()).or_insert(Release {
                    installed,
                    services: Map::new(),
                    networks: Map::new(),
                    volumes: Map::new(),
                })
            };

            // Create the service configuration from the container
            let mut svc = Service::from(local_container);

            // Link the service to the local image if there is state metadata about it
            svc.image = apps_view
                .get(format!(
                    "{app_uuid}/releases/{release_uuid}/services/{service_name}/image"
                ))
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

            // If there are no releases, create an unknown release for cleanup
            if app.releases.is_empty() {
                app.releases
                    .entry(UNKNOWN_RELEASE_UUID.into())
                    .or_insert(Release {
                        installed: false,
                        services: Map::new(),
                        networks: Map::new(),
                        volumes: Map::new(),
                    });
            }
            // Add network to all existing releases
            for release in app.releases.values_mut() {
                release.networks.insert(net_name.clone(), network.clone());
            }
        }

        // read the state of volumes
        let volumes = docker
            .volume()
            .list_with_labels(vec![LABEL_SUPERVISED])
            .await?;

        for volume_name in volumes {
            let local_volume = docker.volume().inspect(&volume_name).await?;

            let Some(app_uuid_str) = local_volume.labels.get(LABEL_APP_UUID) else {
                // Skip orphaned volumes with no app-uuid label
                trace!(volume = %local_volume.name, "skipping orphaned volume");
                continue;
            };
            let app_uuid: Uuid = app_uuid_str.as_str().into();

            // Extract the volume name by stripping the "{app_uuid}_" prefix
            let vol_name = local_volume
                .name
                .strip_prefix(&format!("{app_uuid}_"))
                .unwrap_or(&local_volume.name)
                .to_owned();

            let volume: Volume = local_volume.into();

            let app = get_or_create_app(apps, &app_uuid, local_store).await?;

            // If there are no releases, create an unknown release for cleanup
            if app.releases.is_empty() {
                app.releases
                    .entry(UNKNOWN_RELEASE_UUID.into())
                    .or_insert(Release {
                        installed: false,
                        services: Map::new(),
                        networks: Map::new(),
                        volumes: Map::new(),
                    });
            }
            // Add volume to all existing releases
            for release in app.releases.values_mut() {
                release.volumes.insert(vol_name.clone(), volume.clone());
            }
        }
    }

    Ok(device)
}
