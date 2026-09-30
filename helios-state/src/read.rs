use std::collections::HashSet;

use mahler::state::Map;
use thiserror::Error;
use tracing::instrument;

use crate::common_types::{InvalidImageUriError, OperatingSystem, Uuid};
use crate::labels::{
    LABEL_APP_UUID, LABEL_NETWORK_NAME, LABEL_SERVICE_NAME, LABEL_SUPERVISED, LABEL_VOLUME_NAME,
};
use crate::oci::{self, Client as Docker};
use crate::store::{self, DocumentStore};

use super::models::{
    App, Device, LocalVolume, Network, Release, Service, UNKNOWN_APP_UUID, UNKNOWN_RELEASE_UUID,
    Volume,
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
                locked: false,
                lockfiles: Vec::new(),
                releases: Map::new(),
                rejected_release: None,
            },
        );
    }

    Ok(apps.get_mut(app_uuid).expect("app was just inserted"))
}

/// What [`read`] needs to rebuild the device state from the engine and the
/// store, bundled so callers can re-read without threading the arguments
/// through.
#[derive(Clone)]
pub(crate) struct StateReader {
    pub docker: Docker,
    pub local_store: DocumentStore,
    pub uuid: Uuid,
    pub os: Option<OperatingSystem>,
}

impl StateReader {
    pub(crate) async fn read(&self) -> Result<Device, Error> {
        read(
            &self.docker,
            &self.local_store,
            self.uuid.clone(),
            self.os.clone(),
        )
        .await
    }
}

/// Read the state of system
#[instrument(name = "read_state", level = "trace", skip_all, err)]
pub async fn read(
    docker: &Docker,
    local_store: &DocumentStore,
    uuid: Uuid,
    os: Option<OperatingSystem>,
) -> Result<Device, Error> {
    let mut device = Device::new(uuid, os);

    // read the device name from the local store
    device.name = local_store.get("device_name").await?;

    // Read the hostapp information from the local store and the engine
    #[cfg(feature = "balenahup")]
    if let Some(host) = &mut device.host {
        crate::balenahup::read::derive_host(host, docker, local_store).await?;
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
                        locked: false,
                        lockfiles: Vec::new(),
                        releases: Map::new(),
                        rejected_release: None,
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
            let labels = &local_container.config.labels;
            let app_uuid: Uuid = labels
                .get(LABEL_APP_UUID)
                .map(|uuid| uuid.as_str())
                .unwrap_or(UNKNOWN_APP_UUID)
                .into();
            let service_name: String = labels
                .get(LABEL_SERVICE_NAME)
                .map(|name| name.as_str())
                .unwrap_or(local_container.name.as_str())
                .into();
            let release_uuid: Uuid = local_container
                .namespace(&service_name)
                .as_ref()
                .map(|n| n.as_str())
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

            // get the network name from the label
            let net_name: String = local_network
                .labels
                .get(LABEL_NETWORK_NAME)
                .map(|name| name.as_str())
                .unwrap_or(&network_name)
                .into();

            let app_uuid: Uuid = local_network
                .namespace(&net_name)
                .as_ref()
                .map(|uuid| uuid.as_str())
                .unwrap_or(UNKNOWN_APP_UUID)
                .into();

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
        let volumes = docker.volume().list_all().await?;
        let mut unsupervised = Vec::new();

        for volume_name in volumes {
            let mut local_volume = docker.volume().inspect(&volume_name).await?;

            // get the volume name from the label
            // if the label is not present, then the volume is not supervised by helios
            let Some(vol_name) = local_volume.labels.remove(LABEL_VOLUME_NAME) else {
                unsupervised.push(local_volume);
                continue;
            };

            let app_uuid: Uuid = local_volume
                .namespace(&vol_name)
                .as_ref()
                .map(|n| n.as_str())
                .unwrap_or(UNKNOWN_APP_UUID)
                .into();

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

        // Add unsupervised volumes to the device state
        for local_volume in unsupervised {
            device.volumes.push(local_volume.into());
        }

        // Look for any references to external volumes and add them to the app
        link_external_volumes(&mut device.apps, &device.volumes);
    }

    Ok(device)
}

/// Look for unsupervised volume references in app services and record them as an external volume of
/// the app release.
///
/// Volumes the release already knows about are kept.
fn link_external_volumes(apps: &mut Map<Uuid, App>, device_volumes: &[LocalVolume]) {
    let unsupervised: HashSet<&str> = device_volumes
        .iter()
        .map(|vol| vol.oci_name.as_str())
        .collect();

    for app in apps.values_mut() {
        for release in app.releases.values_mut() {
            let external: Vec<String> = release
                .services
                .values()
                .flat_map(|svc| svc.config.volumes.iter())
                .filter_map(|mount| match mount {
                    oci::Mount::Volume { source, .. } => Some(source.as_str()),
                    _ => None,
                })
                .filter(|source| unsupervised.contains(source))
                .map(String::from)
                .collect();

            for vol_name in external {
                release.volumes.entry(vol_name).or_insert(Volume::External);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Apps of a device with a single release whose service mounts `mount_source`.
    fn apps_mounting(mount_source: &str) -> Map<Uuid, App> {
        serde_json::from_value(json!({
            "my-app-uuid": {
                "id": 1234,
                "name": "my-app",
                "releases": {
                    "my-release-uuid": {
                        "installed": true,
                        "services": {
                            "my-service": {
                                "id": 1,
                                "image": "alpine:latest",
                                "config": {
                                    "volumes": [
                                        {
                                            "type": "volume",
                                            "source": mount_source,
                                            "target": "/data"
                                        },
                                        {
                                            "type": "bind",
                                            "source": "/proc",
                                            "target": "/host/proc"
                                        }
                                    ]
                                },
                            },
                        },
                    }
                }
            }
        }))
        .unwrap()
    }

    fn release_volumes(apps: &Map<Uuid, App>) -> &Map<String, Volume> {
        &apps
            .get(&Uuid::from("my-app-uuid"))
            .unwrap()
            .releases
            .get(&Uuid::from("my-release-uuid"))
            .unwrap()
            .volumes
    }

    fn unsupervised(oci_name: &str) -> Vec<LocalVolume> {
        serde_json::from_value(json!([{"oci_name": oci_name, "config": {"driver": "local"}}]))
            .unwrap()
    }

    #[test]
    fn links_an_unsupervised_volume_mounted_by_a_service() {
        let mut apps = apps_mounting("1234_my-vol");

        link_external_volumes(&mut apps, &unsupervised("1234_my-vol"));

        assert_eq!(
            release_volumes(&apps).get("1234_my-vol"),
            Some(&Volume::External)
        );
    }

    #[test]
    fn leaves_a_release_without_unsupervised_mounts_alone() {
        // the mount names a volume helios manages, which the volume read already
        // recorded on the release
        let mut apps = apps_mounting("my-vol");

        link_external_volumes(&mut apps, &unsupervised("1234_other-vol"));

        assert!(release_volumes(&apps).is_empty());
    }

    #[test]
    fn does_not_replace_a_supervised_volume_of_the_same_name() {
        // an unsupervised volume happens to be named like the app volume once the
        // mount source is de-namespaced. The volume helios manages wins
        let mut apps = apps_mounting("my-vol");
        let volume: Volume =
            serde_json::from_value(json!({"oci_name": "my-vol_my-app-uuid", "config": {}}))
                .unwrap();
        apps.get_mut(&Uuid::from("my-app-uuid"))
            .unwrap()
            .releases
            .get_mut(&Uuid::from("my-release-uuid"))
            .unwrap()
            .volumes
            .insert("my-vol".to_string(), volume.clone());

        link_external_volumes(&mut apps, &unsupervised("my-vol"));

        assert_eq!(release_volumes(&apps).get("my-vol"), Some(&volume));
    }
}
