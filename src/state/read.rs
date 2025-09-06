use std::collections::{BTreeMap, HashMap};

use bollard::{
    query_parameters::{InspectContainerOptions, ListContainersOptions, ListImagesOptions},
    Docker,
};
use thiserror::Error;
use tracing::instrument;

use crate::types::{OperatingSystem, Uuid};
use crate::util::docker::{ImageUri, InvalidImageUriError};

use super::models::{App, Device, Image, Release, Service, ServiceContainerName, UNKNOWN_APP_UUID};

#[derive(Debug, Error)]
pub enum ReadStateError {
    #[error(transparent)]
    DockerError(#[from] bollard::errors::Error),

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

    let installed_images = docker
        .list_images(Some(ListImagesOptions {
            all: true,
            ..Default::default()
        }))
        .await?;

    // Read the state of images
    for img_summary in installed_images {
        let repo_tags = img_summary.repo_tags;
        let img: Image = docker.inspect_image(&img_summary.id).await?.into();

        for img_tag in repo_tags {
            let mut img_name: ImageUri = img_tag.parse()?;

            // If the image name has a tag starting with 'sha256-' use that as the digest.
            //
            // This is needed because the digest doesn't survive when pulling with deltas
            // https://github.com/balena-os/balena-engine/issues/283
            if let Some(tag) = img_name.tag() {
                if tag.starts_with("sha256-") {
                    img_name = format!("{}@{}", img_name.repo(), tag.replace("sha256-", "sha256:"))
                        .parse()?;
                }
            }

            device.images.insert(img_name, img.clone());
        }
    }

    let apps = &mut device.apps;

    // read state of apps
    for (img_uri, img) in device.images.iter() {
        let mut filters = HashMap::new();
        filters.insert(
            "ancestor".to_string(),
            vec![img
                .engine_id
                .clone()
                .expect("image engine id should exist at this point")],
        );
        filters.insert(
            "label".to_string(),
            vec!["io.balena.supervised".to_string()],
        );

        // get all supervised containers for the images we already have
        let containers = docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await?;

        for container_summary in containers {
            if let Some(id) = container_summary.id {
                let container_info = docker
                    .inspect_container(&id, None as Option<InspectContainerOptions>)
                    .await?;

                let ServiceContainerName {
                    service_name,
                    release_uuid,
                } = container_info
                    .name
                    .and_then(|name| name.parse().ok())
                    // this should not happen
                    .expect("invalid container name");

                let labels = container_info
                    .config
                    .and_then(|c| c.labels)
                    .unwrap_or(HashMap::new());

                let app_uuid: Uuid = labels
                    .get("io.balena.app-uuid")
                    .map(|uuid| uuid.as_str().into())
                    .unwrap_or(UNKNOWN_APP_UUID.into());

                // Create the app if it doesn't exist yet
                let app = apps.entry(app_uuid).or_insert_with(|| {
                    let id: u32 = labels
                        .get("io.balena.app-id")
                        .and_then(|id| id.parse().ok())
                        .unwrap_or(0);

                    let name = labels.get("io.balena.app-name").cloned();
                    App {
                        id,
                        name,
                        releases: BTreeMap::new(),
                    }
                });

                // Create the release for the uuid if it doesn't exist
                let release = app.releases.entry(release_uuid).or_insert(Release {
                    services: BTreeMap::new(),
                });

                let service_id: u32 = labels
                    .get("io.balena.service-id")
                    .and_then(|id| id.parse().ok())
                    .unwrap_or(0);

                // Insert the service and link it to the image
                release.services.insert(
                    service_name,
                    Service {
                        id: service_id,
                        image: img_uri.clone(),
                    },
                );
            }
        }
    }

    Ok(device)
}
