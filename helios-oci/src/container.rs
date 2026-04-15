use std::collections::HashMap;

use bollard::{
    config::{
        ContainerInspectResponse, ContainerStateStatusEnum, EndpointIpamConfig, EndpointSettings,
        HealthStatusEnum, NetworkingConfig, RestartPolicyNameEnum,
    },
    models::ContainerCreateBody,
    query_parameters::{
        CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions,
        RemoveContainerOptions, RenameContainerOptions,
    },
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

use super::datetime::DateTime;
use super::util::types::{Environment, ImageUri};
use super::{Client, Error, LocalNamespace, Namespace, NoNamespace, Result, WithContext};

#[derive(Debug, Clone)]
pub struct Container<'a, N> {
    client: &'a Client,
    __: std::marker::PhantomData<N>,
}

impl<'a, N> Container<'a, N> {
    pub fn new(client: &'a Client) -> Self {
        Self {
            client,
            __: std::marker::PhantomData::<N>,
        }
    }
}

impl Container<'_, NoNamespace> {
    /// Create a temporary container from the given image
    ///
    /// This is only meant to get access to the container files and not to be started
    pub async fn create_tmp(&self, name: &str, image: &ImageUri) -> Result<String> {
        self.create(
            name,
            NoNamespace,
            image,
            ContainerConfig {
                command: Some(vec!["/bin/false".to_string()]),
                ..Default::default()
            },
        )
        .await
    }
}

impl<N: Namespace> Container<'_, N> {
    /// Returns the list of container ids on the server
    /// matching the given labels
    ///
    /// Use in combination with [`Container::inspect`] to get the container
    /// information
    pub async fn list_with_labels(&self, labels: Vec<&str>) -> Result<Vec<String>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            labels.into_iter().map(|s| s.to_owned()).collect(),
        );

        let opts = ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        };

        let res = self.client.inner().list_containers(Some(opts)).await;

        let container_list = res.map_err(Error::with_context("failed to list containers"))?;

        // find all
        Ok(container_list
            .into_iter()
            .flat_map(|c| c.names.into_iter().flatten())
            .map(|n| n.trim_start_matches('/').to_string())
            .collect())
    }

    /// Returns low-level information about a container.
    pub async fn inspect(&self, id: &str) -> Result<LocalContainer<N>> {
        let container_info = self
            .client
            .inner()
            .inspect_container(id, None)
            .await
            .map_err(|e| Error::from(e).context(format!("failed to inspect container '{id}'")))?;

        let container = container_info
            .try_into()
            .with_context(|| format!("failed to inspect container '{id}'"))?;

        Ok(container)
    }

    /// Create the container with the passed options
    ///
    /// Returns the identifier (name) of the newly created container
    pub async fn create(
        &self,
        service: &str,
        namespace: impl Into<N>,
        image: &str,
        config: ContainerConfig,
    ) -> Result<String> {
        let id = namespace.into().to_identifier(service);
        let options = Some(CreateContainerOptions {
            name: Some(id.clone()),
            platform: String::from(""),
        });

        let mut config: ContainerCreateBody = config.into();
        config.image = Some(image.to_string());

        // TODO: add networking and host config which should be passed as arguments to this
        // function

        match self.client.inner().create_container(options, config).await {
            Ok(_) => Ok(id),
            // container already exists, ignore
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => Ok(id),
            Err(e) => Err(Error::from(e).context(format!("failed to create container {service}"))),
        }
    }

    /// Start the container with the given name
    pub async fn start(&self, name: &str) -> Result<()> {
        match self.client.inner().start_container(name, None).await {
            Ok(_) => Ok(()),
            // service already running, ignore
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(e) => Err(Error::from(e).context(format!("failed to start container {name}"))),
        }
    }

    /// Stop the container with the given name
    pub async fn stop(&self, name: &str) -> Result<()> {
        match self.client.inner().stop_container(name, None).await {
            Ok(_) => Ok(()),
            // service already stopped, ignore
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(e) => Err(Error::from(e).context(format!("failed to stop container {name}"))),
        }
    }

    /// Rename the container with the given name
    async fn rename(&self, id: &str, new_name: &str) -> Result<()> {
        self.client
            .inner()
            .rename_container(
                id,
                RenameContainerOptions {
                    name: new_name.to_owned(),
                },
            )
            .await
            .map_err(Error::from)
            .with_context(|| format!("failed to rename container {id}"))?;

        Ok(())
    }

    /// Migrate a container to a new namespace
    pub async fn migrate(
        &self,
        id: &str,
        service: &str,
        namespace: impl Into<N>,
    ) -> Result<String> {
        let new_name = namespace.into().to_identifier(service);
        self.rename(id, &new_name).await?;
        Ok(new_name)
    }

    /// Remove a stopped container
    pub async fn remove(&self, id: &str) -> Result<()> {
        match self
            .client
            .inner()
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    v: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(Error::from(e).context(format!("failed to remove container {id}"))),
        }
    }

    /// Reads a container directory contents into an array of bytes using tar representation
    pub async fn read_from(&self, id: &str, container_path: &str) -> Result<Vec<u8>> {
        let mut stream = self.client.inner().download_from_container(
            id,
            Some(DownloadFromContainerOptions {
                path: container_path.to_owned(),
            }),
        );

        let mut archive = Vec::new();
        while let Some(res) = stream.next().await {
            match res {
                Ok(chunk) => archive.extend_from_slice(&chunk),
                Err(e) => {
                    return Err(Error::from(e).context(format!(
                        "failed to read {container_path} from container {id}"
                    )));
                }
            }
        }

        Ok(archive)
    }
}

// by ref in order to clone only what's necessary to build LocalImage.
impl<N> TryFrom<ContainerInspectResponse> for LocalContainer<N> {
    type Error = Error;

    fn try_from(value: ContainerInspectResponse) -> Result<Self> {
        let image = value.image.ok_or("container image ID should not be nil")?;
        let name = value
            .name
            .ok_or("container name should not be nil")?
            .trim_start_matches('/')
            .to_owned();

        let mut host_config = value.host_config;
        // Only recognize `none`/`host` from host_config: for user networks Docker also
        // populates `network_mode` with the first network name, which would otherwise
        // be misread as a NetworkMode::Other passthrough. `Other(..)` target-state modes
        // won't round-trip through inspect — the container will be recreated on mismatch.
        let network_mode = host_config
            .as_mut()
            .and_then(|hc| hc.network_mode.take())
            .and_then(|m| match m.as_str() {
                "none" => Some(NetworkMode::None),
                "host" => Some(NetworkMode::Host),
                _ => None,
            });
        let restart_policy = host_config
            .as_mut()
            .and_then(|hc| hc.restart_policy.take())
            .and_then(|rp| {
                use RestartPolicyNameEnum;
                rp.name.as_ref().map(|name| match name {
                    RestartPolicyNameEnum::NO | RestartPolicyNameEnum::EMPTY => RestartPolicy::No,
                    RestartPolicyNameEnum::ALWAYS => RestartPolicy::Always,
                    RestartPolicyNameEnum::ON_FAILURE => RestartPolicy::OnFailure {
                        max_retries: rp.maximum_retry_count.map(|n| n as u32),
                    },
                    RestartPolicyNameEnum::UNLESS_STOPPED => RestartPolicy::UnlessStopped,
                })
            })
            .unwrap_or_default();

        let mut config = value.config;
        let labels = config
            .as_mut()
            .and_then(|c| c.labels.take())
            .unwrap_or_default();
        let environment = config
            .as_mut()
            .and_then(|c| c.env.take())
            .map(Environment::from)
            .unwrap_or_default();
        let cmd = config.and_then(|c| c.cmd);

        // Read network endpoint configurations from the container's network settings.
        // When network_mode is `host` or `none`, networks are not user-managed — leave empty.
        let networks = if network_mode.is_some() {
            IndexMap::new()
        } else {
            value
                .network_settings
                .and_then(|ns| ns.networks)
                .unwrap_or_default()
                .into_iter()
                .map(|(name, endpoint)| {
                    let ipam = endpoint.ipam_config.unwrap_or_default();
                    let config = NetworkSettings {
                        aliases: endpoint.aliases.unwrap_or_default(),
                        ipv4_address: ipam.ipv4_address,
                        ipv6_address: ipam.ipv6_address,
                        link_local_ips: ipam.link_local_ips.unwrap_or_default(),
                        mac_address: endpoint.mac_address.filter(|a| !a.is_empty()),
                        driver_opts: endpoint.driver_opts.unwrap_or_default(),
                        gw_priority: endpoint.gw_priority.filter(|a| a != &0),
                    };
                    (name, config)
                })
                .collect()
        };

        let config = ContainerConfig {
            command: cmd,
            environment,
            labels,
            restart_policy,
            networks,
            network_mode,
        };

        let created: DateTime = value
            .created
            .ok_or("container creation date should not be nil")?
            .parse()
            .map_err(Error::from)
            .context("container creation date should be a valid date")?;

        let state = value.state.ok_or("container state should not be nil")?;
        let healthy = state
            .health
            .and_then(|health| health.status)
            .map(|status| status == HealthStatusEnum::HEALTHY)
            .unwrap_or_default();
        let status = state
            .status
            .ok_or("container status should not be nil")?
            .into();

        let state = ContainerState {
            created,
            error: state.error,
            healthy,
            status,
        };

        Ok(Self {
            name,
            image,
            config,
            state,
            __: std::marker::PhantomData::<N>,
        })
    }
}

/// The container runtime status. This is a simplified state over what the container engine returns
#[derive(Debug, Clone, PartialEq, Eq, Default, PartialOrd, Ord)]
pub enum ContainerStatus {
    #[default]
    Created,
    Running,
    Stopped,
    Dead,
}

impl From<ContainerStateStatusEnum> for ContainerStatus {
    fn from(value: ContainerStateStatusEnum) -> Self {
        use ContainerStateStatusEnum::*;
        match value {
            EMPTY => ContainerStatus::Created,
            CREATED => ContainerStatus::Created,
            RUNNING => ContainerStatus::Running,
            PAUSED => ContainerStatus::Stopped,
            RESTARTING => ContainerStatus::Running,
            REMOVING => ContainerStatus::Stopped,
            EXITED => ContainerStatus::Stopped,
            DEAD => ContainerStatus::Dead,
        }
    }
}

/// Container state summary
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerState {
    /// The container runtime status
    pub status: ContainerStatus,
    /// Container health status, `true` means the container
    /// is healthy, `false` means the container health status is
    /// undetermined
    pub healthy: bool,
    /// Container creation date
    pub created: DateTime,
    /// Last error message from the container
    pub error: Option<String>,
}

/// Docker compose restart policy for a container.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(tag = "name", rename_all = "snake_case")]
pub enum RestartPolicy {
    // this is the OCI default, and should work for both reading and creating containers
    #[default]
    No,
    Always,
    OnFailure {
        max_retries: Option<u32>,
    },
    UnlessStopped,
}

/// Per-network endpoint configuration for a container
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct NetworkSettings {
    /// Alternative hostnames for the container on this network
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    /// Static IPv4 address for the container on this network
    pub ipv4_address: Option<String>,

    /// Static IPv6 address for the container on this network
    pub ipv6_address: Option<String>,

    /// Link-local IP addresses
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub link_local_ips: Vec<String>,

    /// MAC address for the container on this network
    pub mac_address: Option<String>,

    /// Driver-specific options
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub driver_opts: HashMap<String, String>,

    /// Gateway priority (highest wins default gateway)
    pub gw_priority: Option<i64>,
}

impl From<NetworkSettings> for EndpointSettings {
    fn from(config: NetworkSettings) -> Self {
        let ipam_config = if config.ipv4_address.is_some()
            || config.ipv6_address.is_some()
            || !config.link_local_ips.is_empty()
        {
            Some(EndpointIpamConfig {
                ipv4_address: config.ipv4_address,
                ipv6_address: config.ipv6_address,
                link_local_ips: if config.link_local_ips.is_empty() {
                    None
                } else {
                    Some(config.link_local_ips)
                },
            })
        } else {
            None
        };

        EndpointSettings {
            ipam_config,
            aliases: if config.aliases.is_empty() {
                None
            } else {
                Some(config.aliases)
            },
            mac_address: config.mac_address,
            driver_opts: if config.driver_opts.is_empty() {
                None
            } else {
                Some(config.driver_opts)
            },
            gw_priority: config.gw_priority,
            ..Default::default()
        }
    }
}

/// Container-level network mode. Mirrors the compose `network_mode` setting.
///
/// `None` and `Host` are recognized explicitly; `Other(..)` passes platform-specific
/// modes through to the engine.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    None,
    Host,
    #[serde(untagged)]
    Other(String),
}

impl std::fmt::Display for NetworkMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkMode::None => "none",
            NetworkMode::Host => "host",
            NetworkMode::Other(s) => s,
        }
        .fmt(f)
    }
}

/// Container configuration that is portable between hosts
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Eq, Default)]
#[serde(default)]
pub struct ContainerConfig {
    /// Command to run specified as an array of strings
    pub command: Option<Vec<String>>,

    /// Environment variables
    #[serde(skip_serializing_if = "Environment::is_empty")]
    pub environment: Environment,

    /// User-defined key/value metadata
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,

    /// Restart policy for the container
    pub restart_policy: RestartPolicy,

    /// Network endpoint configurations, ordered by connection priority.
    ///
    /// Mutually exclusive with `network_mode`.
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub networks: IndexMap<String, NetworkSettings>,

    /// Container network mode. When set, `networks` must be empty.
    pub network_mode: Option<NetworkMode>,
}

/// Compare configs treating networks as unordered (Docker inspect doesn't preserve priority order)
impl PartialEq for ContainerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.command == other.command
            && self.environment == other.environment
            && self.labels == other.labels
            && self.restart_policy == other.restart_policy
            && self.network_mode == other.network_mode
            && self.networks.len() == other.networks.len()
            && self
                .networks
                .iter()
                .all(|(k, v)| other.networks.get(k) == Some(v))
    }
}

impl From<ContainerConfig> for ContainerCreateBody {
    fn from(value: ContainerConfig) -> Self {
        let ContainerConfig {
            command: cmd,
            environment,
            labels,
            restart_policy,
            networks,
            network_mode,
        } = value;

        let env = if environment.is_empty() {
            None
        } else {
            Some(
                environment
                    .into_iter()
                    .map(|(k, v)| match v {
                        Some(val) => format!("{k}={val}"),
                        // a null value unsets an existing env var
                        None => k,
                    })
                    .collect(),
            )
        };

        let restart_policy = {
            let (name, maximum_retry_count) = match restart_policy {
                RestartPolicy::No => (RestartPolicyNameEnum::NO, None),
                RestartPolicy::Always => (RestartPolicyNameEnum::ALWAYS, None),
                RestartPolicy::OnFailure { max_retries } => (
                    RestartPolicyNameEnum::ON_FAILURE,
                    max_retries.map(|n| n as i64),
                ),
                RestartPolicy::UnlessStopped => (RestartPolicyNameEnum::UNLESS_STOPPED, None),
            };

            bollard::config::RestartPolicy {
                name: Some(name),
                maximum_retry_count,
            }
        };

        // When network_mode is set, `networks` must be empty (enforced upstream by the
        // remote-model validation). Use the explicit mode directly and skip endpoint
        // configuration.
        let (host_network_mode, networking_config) = if let Some(mode) = network_mode {
            (Some(mode.to_string()), None)
        } else {
            let host_network_mode = networks.first().map(|(net_name, _)| net_name.clone());
            let endpoints_config = networks
                .into_iter()
                .map(|(net_name, net)| {
                    let config: EndpointSettings = net.into();
                    (net_name, config)
                })
                .collect();
            let networking_config = NetworkingConfig {
                endpoints_config: Some(endpoints_config),
            };
            (host_network_mode, Some(networking_config))
        };

        let host_config = bollard::config::HostConfig {
            network_mode: host_network_mode,
            restart_policy: Some(restart_policy),
            ..Default::default()
        };

        ContainerCreateBody {
            cmd,
            env,
            labels: Some(labels),
            networking_config,
            host_config: Some(host_config),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalContainer<N = LocalNamespace> {
    /// The name of the container
    pub name: String,

    /// The content-addressable image id
    pub image: String,

    /// User-defined portable configuration
    pub config: ContainerConfig,

    /// The container runtime state
    pub state: ContainerState,

    __: std::marker::PhantomData<N>,
}

impl<N: Namespace> LocalContainer<N> {
    /// Get the namepace from the local container metadata
    pub fn namespace(&self, service: &str) -> Option<N> {
        N::from_identifier(&self.name, service)
    }
}
