use bollard::secret::SystemInfo;
use bollard::{query_parameters::ListImagesOptions, Docker};
use thiserror::Error;
use tracing::instrument;

use crate::types::Uuid;
use crate::util::docker::{ImageUri, InvalidImageUriError};

use super::models::{Device, Host, Image};

#[derive(Debug, Error)]
pub enum ReadStateError {
    #[error(transparent)]
    DockerError(#[from] bollard::errors::Error),

    #[error(transparent)]
    InvalidRegistryUri(#[from] InvalidImageUriError),
}

// Convert an architecture from the string returned by he engine
// to a balenaCloud accepted engine
fn parse_engine_arch(arch: String) -> Option<String> {
    // In theory, the list of possible architectures is limited to
    // https://go.dev/doc/install/source#environment
    // however in practice, some systems use more specific architecture strings
    // such as armv6l and armv7l
    let arch = match arch.as_ref() {
        "amd64" => "amd64",
        "arm64" => "aarch644",
        "386" => "i386",
        "arm" => "armv7hf",
        "armv6l" => "rpi",
        "armv7l" => "armv7hf",
        _ => return None,
    };

    Some(arch.into())
}

/// Read the state of system
#[instrument(name = "read_state", skip_all)]
pub async fn read(
    docker: &Docker,
    uuid: Uuid,
    device_type: Option<String>,
) -> Result<Device, ReadStateError> {
    let SystemInfo {
        operating_system,
        architecture,
        ..
    } = docker.info().await?;

    // XXX: I would like to get the engine name and version but the results
    // of the /version endpoint are not consistent accross engines
    let default_host = Host::default();
    let host = Host {
        os: operating_system.unwrap_or(default_host.os),
        arch: architecture
            .and_then(parse_engine_arch)
            .unwrap_or(default_host.arch),
    };

    let mut device = Device::new(uuid, device_type, host);

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

    // TODO: read state of apps

    Ok(device)
}
