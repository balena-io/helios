//! This crate provides a proxy to OCI client, it should be used instead of that when podman
//! support is enabled as it will also perform podman specific tasks, like
//! creating/updating/removing quadlets

use std::ops::Deref;

use helios_oci as oci;
use helios_util as util;

// FIXME: this is only pub to avoid dead_code warnings
// remove once feature support is more complete
pub mod podlet;
mod quadlet;
pub use oci::{
    ContainerConfig, ContainerState, ContainerStatus, DateTime, Error, Image, ImageConfig,
    LocalContainer, LocalImage, LocalNamespace, LocalNetwork, LocalVolume, Namespace,
    NetworkConfig, NetworkDriver, NetworkIpamConfig, NetworkIpamDriver, NetworkIpamPoolConfig,
    RegistryAuth, RestartPolicy, Result, Volume, VolumeConfig, VolumeDriver, WithContext,
};

#[derive(Clone, Debug)]
pub struct Client {
    docker: oci::Client,
}

impl Client {
    /// Connect to the daemon based on the `DOCKER_HOST` environment variable.
    pub async fn connect() -> Result<Self> {
        let docker = oci::Client::connect().await?;
        Ok(Self { docker })
    }

    /// Exposes methods to work with container
    #[inline]
    pub fn container(&self) -> Container<'_, LocalNamespace> {
        Container(self.docker.container())
    }
}

impl Deref for Client {
    type Target = oci::Client;

    fn deref(&self) -> &Self::Target {
        &self.docker
    }
}

pub struct Container<'a, N>(oci::Container<'a, N>);

impl<'a, N: Namespace> Container<'a, N> {
    /// Create the container with the passed options
    ///
    /// Returns a reference to the container, either the newly created id or
    /// the container name if the container already exists
    pub async fn create(
        &self,
        service: &str,
        namespace: impl Into<N>,
        image: &str,
        config: ContainerConfig,
    ) -> Result<String> {
        // TODO: if the engine is really podman
        // - update configuration to be podman compatible: e.g. remove the restart policy as that
        //   will be handled by systemd
        // - create quadlet in /etc/containers/systemd
        // - etc

        self.0.create(service, namespace, image, config).await
    }
}

impl<'a, N> Deref for Container<'a, N> {
    type Target = oci::Container<'a, N>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
