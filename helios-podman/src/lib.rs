//! This crate provides a proxy to OCI client, it should be used instead of that when podman
//! support is enabled as it will also perform podman specific tasks, like
//! creating/updating/removing quadlets

use std::{fs, ops::Deref, path::Path};
use tracing::{debug, info, warn};

use helios_oci as oci;
use helios_util as util;

// FIXME: this is only pub to avoid dead_code warnings
// remove once feature support is more complete
pub mod podlet;
mod quadlet;

// Re-export everything except the Client, Container, Network, and Volume accessors
pub use oci::{
    ContainerConfig, ContainerState, ContainerStatus, DateTime, Error, Image, ImageConfig,
    LocalContainer, LocalImage, LocalNamespace, LocalNetwork, LocalVolume, Namespace,
    NetworkConfig, NetworkDriver, NetworkIpamConfig, NetworkIpamDriver, NetworkIpamPoolConfig,
    RegistryAuth, RestartPolicy, Result, VolumeConfig, VolumeDriver, WithContext,
};

const PODMAN_ENGINE: &str = "Podman Engine";
const QUADLET_UNIT_DIR: &str = "/etc/containers/systemd";
const LABEL_RESTART_POLICY: &str = "io.balena.private.config.restart_policy";

#[derive(Clone, Debug)]
pub struct Client {
    docker: oci::Client,

    /// The podman service version. This is only available if the
    /// engine is podman and quadlet support is enabled
    podman_version: Option<podlet::quadlet::PodmanVersion>,
}

impl Client {
    /// Connect to the daemon based on the `DOCKER_HOST` environment variable.
    ///
    /// Detects whether the engine is Podman and resolves the quadlet-compatible version.
    /// If the engine is not Podman or the version does not support quadlets, `podman_version`
    /// is `None`.
    pub async fn connect() -> Result<Self> {
        let docker = oci::Client::connect().await?;
        let podman_version = Self::detect_podman_version(&docker).await?;

        Ok(Self {
            docker,
            podman_version,
        })
    }

    async fn detect_podman_version(
        client: &oci::Client,
    ) -> Result<Option<podlet::quadlet::PodmanVersion>> {
        let info = client.version().await?;

        let podman = info.components.iter().find(|c| c.name == PODMAN_ENGINE);
        let Some(podman) = podman else {
            debug!("engine is not Podman, quadlet support disabled");
            return Ok(None);
        };

        if !Path::new(QUADLET_UNIT_DIR).is_dir() {
            debug!("{QUADLET_UNIT_DIR} not found, quadlet support disabled");
            return Ok(None);
        }

        match podman.version.parse::<podlet::quadlet::PodmanVersion>() {
            Ok(version) => {
                debug!(%version, "detected Podman with quadlet support");
                Ok(Some(version))
            }
            Err(_) => {
                debug!(
                    version = %podman.version,
                    "Podman version does not support quadlets"
                );
                Ok(None)
            }
        }
    }

    /// Exposes methods to work with container
    #[inline]
    pub fn container(&self) -> Container<'_, LocalNamespace> {
        Container {
            client: &self.docker,
            inner: self.docker.container(),
            podman_version: self.podman_version,
        }
    }

    /// Exposes methods to work with networks
    #[inline]
    pub fn network(&self) -> Network<'_, LocalNamespace> {
        Network {
            inner: self.docker.network(),
            podman_version: self.podman_version,
        }
    }

    /// Exposes methods to work with volumes
    #[inline]
    pub fn volume(&self) -> Volume<'_, LocalNamespace> {
        Volume {
            inner: self.docker.volume(),
            podman_version: self.podman_version,
        }
    }
}

impl Deref for Client {
    type Target = oci::Client;

    fn deref(&self) -> &Self::Target {
        &self.docker
    }
}

/// Write a quadlet file to the systemd unit directory. And reload the daemon
async fn install_quadlet(quadlet_file: &podlet::quadlet::File) -> Result<()> {
    let contents = quadlet_file
        .serialize_to_quadlet(&podlet::quadlet::JoinOption::all_set())
        .map_err(Error::other)?;

    let quadlet_path = Path::new(QUADLET_UNIT_DIR).join(format!(
        "{}.{}",
        quadlet_file.name,
        quadlet_file.resource.extension()
    ));

    let path = quadlet_path.clone();
    util::fs::run_async(move || util::fs::safe_write_all(&path, contents))
        .await
        .map_err(Error::other)
        .with_context(|| format!("failed to write quadlet file {}", quadlet_path.display()))?;
    info!(path = %quadlet_path.display(), "wrote quadlet file");

    // reload the daemon so the unit is available when calling start
    util::systemd::daemon_reload().await.map_err(Error::other)?;

    Ok(())
}

/// Remove a quadlet file from the systemd unit directory.
async fn uninstall_quadlet(name: &str, kind: podlet::quadlet::ResourceKind) -> Result<()> {
    let quadlet_path = Path::new(QUADLET_UNIT_DIR).join(format!("{name}.{kind}"));

    let path = quadlet_path.clone();
    util::fs::run_async(move || match fs::remove_file(&path) {
        Ok(()) => {
            util::fs::sync_dir(QUADLET_UNIT_DIR)?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    })
    .await
    .map_err(Error::other)
    .with_context(|| format!("failed to remove quadlet file {}", quadlet_path.display()))?;
    info!(path = %quadlet_path.display(), "removed quadlet file");

    // reload the daemon to apply the change
    util::systemd::daemon_reload().await.map_err(Error::other)?;

    Ok(())
}

pub struct Container<'a, N> {
    client: &'a oci::Client,
    inner: oci::Container<'a, N>,
    podman_version: Option<podlet::quadlet::PodmanVersion>,
}

impl<'a, N: Namespace> Container<'a, N> {
    /// Returns low-level information about a container.
    pub async fn inspect(&self, name: &str) -> Result<LocalContainer<N>> {
        let mut info = self.inner.inspect(name).await?;
        if self.podman_version.is_some() {
            let labels = &mut info.config.labels;

            // restore configuration from added labels
            info.config.restart_policy = labels
                .remove(LABEL_RESTART_POLICY)
                .and_then(|restart| serde_json::from_str(&restart).ok())
                .unwrap_or(info.config.restart_policy);
        }
        Ok(info)
    }

    /// Create the container with the passed options
    ///
    /// Returns a reference to the container, either the newly created id or
    /// the container name if the container already exists
    ///
    /// If quadlet support is enabled, it created the .container quadlet on
    /// /etc/containers/systemd
    pub async fn create(
        &self,
        service: &str,
        namespace: impl Into<N>,
        image: &str,
        mut config: ContainerConfig,
    ) -> Result<String> {
        let namespace = namespace.into();
        if let Some(version) = self.podman_version {
            let id = namespace.to_identifier(service);
            // use image id instead of name so we can rename the unit
            let img_info = self.client.image().inspect(image).await?;
            let quadlet_file =
                quadlet::from_container_config(&id, &img_info.id, &mut config, version)
                    .map_err(Error::other)?;

            // create the container first, avoid creating the quadlet if a failure
            // happens
            let container_id = self.inner.create(service, namespace, image, config).await?;

            // store the quadlet
            install_quadlet(&quadlet_file).await?;

            Ok(container_id)
        } else {
            self.inner.create(service, namespace, image, config).await
        }
    }

    /// Migrate a container to a new namespace
    pub async fn migrate(
        &self,
        old_name: &str,
        service: &str,
        namespace: impl Into<N>,
    ) -> Result<String> {
        let namespace = namespace.into();
        if self.podman_version.is_some() {
            let new_name = namespace.to_identifier(service);
            let old_quadlet_path = Path::new(QUADLET_UNIT_DIR).join(format!(
                "{old_name}.{}",
                podlet::quadlet::ResourceKind::Container
            ));
            let new_quadlet_path = Path::new(QUADLET_UNIT_DIR).join(format!(
                "{new_name}.{}",
                podlet::quadlet::ResourceKind::Container
            ));

            let old_path = old_quadlet_path.clone();
            let new_path = new_quadlet_path.clone();

            // copy (instead of moving) the existing quadlet to the new location
            // this way if the operation fails after this step this can be re-tried
            util::fs::run_async(move || {
                std::fs::copy(old_path, new_path)?;
                util::fs::sync_dir(QUADLET_UNIT_DIR)
            })
            .await
            .map_err(Error::other)
            .with_context(|| format!("failed to create quadlet  {}", new_quadlet_path.display()))?;

            // migrate the container
            let new_id = self.inner.migrate(old_name, service, namespace).await?;

            // uninstall the old quadlet and reload daemon
            uninstall_quadlet(old_name, podlet::quadlet::ResourceKind::Container).await?;

            Ok(new_id)
        } else {
            self.inner.migrate(old_name, service, namespace).await
        }
    }

    /// Start the container with the given name
    ///
    /// If quadlet support is enabled it starts the service via systemd.
    pub async fn start(&self, name: &str) -> Result<()> {
        if self.podman_version.is_some() {
            match util::systemd::start(name).await {
                Ok(()) => return Ok(()),
                Err(util::systemd::Error::NoSuchUnit(unit)) => {
                    warn!("unit not found {unit}, reverting to regular start")
                }
                Err(e) => {
                    return Err(Error::other(e))
                        .with_context(|| format!("failed to start systemd unit {name}.service"));
                }
            }
        }
        self.inner.start(name).await
    }

    /// Stop the container with the given name
    ///
    /// If quadlet support is enabled it stops the service via systemd.
    pub async fn stop(&self, name: &str) -> Result<()> {
        if self.podman_version.is_some() {
            // NOTE: stopping the unit also removes the container which means the
            // behavior of this method with and without quadlets is different
            match util::systemd::stop(name).await {
                Ok(()) => return Ok(()),
                Err(util::systemd::Error::NoSuchUnit(unit)) => {
                    warn!("unit not found {unit}, reverting to regular stop")
                }
                Err(e) => {
                    return Err(Error::other(e))
                        .with_context(|| format!("failed to stop systemd unit {name}.service"));
                }
            }
        }
        self.inner.stop(name).await
    }

    /// Remove a stopped container
    ///
    /// If quadlet support is enabled, it removes the unit
    pub async fn remove(&self, name: &str) -> Result<()> {
        if self.podman_version.is_some() {
            uninstall_quadlet(name, podlet::quadlet::ResourceKind::Container).await?;
        }
        self.client.container().remove(name).await
    }
}

impl<'a, N> Deref for Container<'a, N> {
    type Target = oci::Container<'a, N>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub struct Network<'a, N> {
    inner: oci::Network<'a, N>,
    podman_version: Option<podlet::quadlet::PodmanVersion>,
}

impl<'a, N: Namespace> Network<'a, N> {
    /// Create a network with the given name and configuration
    ///
    /// If quadlet support is enabled, it creates the .network quadlet on
    /// /etc/containers/systemd
    pub async fn create(
        &self,
        network: &str,
        namespace: impl Into<N>,
        config: NetworkConfig,
    ) -> Result<String> {
        let namespace = namespace.into();
        if let Some(version) = self.podman_version {
            let id = namespace.to_identifier(network);
            let quadlet_file =
                quadlet::from_network_config(&id, &config, version).map_err(Error::other)?;

            // create the network via the engine
            let net = self.inner.create(network, namespace, config).await?;

            install_quadlet(&quadlet_file).await?;

            Ok(net)
        } else {
            self.inner.create(network, namespace, config).await
        }
    }

    /// Remove a network
    ///
    /// If quadlet support is enabled, it removes the quadlet file
    pub async fn remove(&self, name: &str) -> Result<()> {
        if self.podman_version.is_some() {
            uninstall_quadlet(name, podlet::quadlet::ResourceKind::Network).await?;
        }
        self.inner.remove(name).await
    }
}

impl<'a, N> Deref for Network<'a, N> {
    type Target = oci::Network<'a, N>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub struct Volume<'a, N> {
    inner: oci::Volume<'a, N>,
    podman_version: Option<podlet::quadlet::PodmanVersion>,
}

impl<'a, N: Namespace> Volume<'a, N> {
    /// Create a volume with the given name and configuration
    ///
    /// If quadlet support is enabled, it creates the .volume quadlet on
    /// /etc/containers/systemd
    pub async fn create(
        &self,
        volume: &str,
        namespace: impl Into<N>,
        config: VolumeConfig,
    ) -> Result<String> {
        let namespace = namespace.into();
        if let Some(version) = self.podman_version {
            let id = namespace.to_identifier(volume);
            let quadlet_file =
                quadlet::from_volume_config(&id, &config, version).map_err(Error::other)?;

            // create the volume via the engine
            let vol = self.inner.create(volume, namespace, config).await?;

            // write the quadlet
            install_quadlet(&quadlet_file).await?;

            Ok(vol)
        } else {
            self.inner.create(volume, namespace, config).await
        }
    }

    /// Remove a volume
    ///
    /// If quadlet support is enabled, it removes the quadlet file.
    pub async fn remove(&self, name: &str) -> Result<()> {
        if self.podman_version.is_some() {
            uninstall_quadlet(name, podlet::quadlet::ResourceKind::Volume).await?;
        }
        self.inner.remove(name).await
    }
}

impl<'a, N> Deref for Volume<'a, N> {
    type Target = oci::Volume<'a, N>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
