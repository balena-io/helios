use std::collections::{BTreeSet, HashMap};

use bollard::{
    config::{
        ContainerInspectResponse, ContainerStateStatusEnum, EndpointIpamConfig, EndpointSettings,
        HealthStatusEnum, NetworkingConfig, RestartPolicyNameEnum,
    },
    container::LogOutput,
    exec::{CreateExecOptions, StartExecResults},
    models::{
        ContainerCreateBody, MountBindOptions, MountBindOptionsPropagationEnum, MountTmpfsOptions,
        MountType, MountVolumeOptions,
    },
    query_parameters::{
        CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions,
        RemoveContainerOptions, RenameContainerOptions,
    },
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

use super::datetime::DateTime;
use super::ports::{PortMapping, from_oci_port_map, to_oci_port_maps};
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

    /// Run a command inside an existing container, returning its captured
    /// output and exit code.
    pub async fn exec(&self, id: &str, cmd: &[&str], env: &[&str]) -> Result<ExecOutput> {
        let exec = self
            .client
            .inner()
            .create_exec(
                id,
                CreateExecOptions {
                    cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                    env: Some(env.iter().map(|s| s.to_string()).collect()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                Error::from(e).context(format!("failed to create exec in container {id}"))
            })?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached { mut output, .. } = self
            .client
            .inner()
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| {
                Error::from(e).context(format!("failed to start exec in container {id}"))
            })?
        {
            while let Some(res) = output.next().await {
                let chunk = res.map_err(|e| {
                    Error::from(e)
                        .context(format!("failed to read exec output from container {id}"))
                })?;
                match chunk {
                    LogOutput::StdOut { message } => {
                        stdout.push_str(&String::from_utf8_lossy(&message))
                    }
                    LogOutput::StdErr { message } => {
                        stderr.push_str(&String::from_utf8_lossy(&message))
                    }
                    _ => {}
                }
            }
        }

        let exit_code = self
            .client
            .inner()
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| {
                Error::from(e).context(format!("failed to inspect exec in container {id}"))
            })?
            .exit_code
            .ok_or_else(|| format!("failed to get exit code for exec in container {id}"))?;

        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code,
        })
    }
}

/// Captured output of a command run via [`Container::exec`].
#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
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

        // Gather the host config fields from the engine
        // while filtering out the default values such as "" / 0.
        let mut host_config = value.host_config;
        let cgroup = host_config
            .as_mut()
            .and_then(|hc| hc.cgroupns_mode.take())
            .map(Cgroup::from)
            .unwrap_or_default();
        let cgroup_parent = host_config
            .as_mut()
            .and_then(|hc| hc.cgroup_parent.take())
            .filter(|s| !s.is_empty());
        let cpuset = host_config
            .as_mut()
            .and_then(|hc| hc.cpuset_cpus.take())
            .filter(|s| !s.is_empty());
        let cpu_rt_period = host_config
            .as_mut()
            .and_then(|hc| hc.cpu_realtime_period.take())
            .unwrap_or(0);
        let cpu_rt_runtime = host_config
            .as_mut()
            .and_then(|hc| hc.cpu_realtime_runtime.take())
            .unwrap_or(0);
        let cpu_shares = host_config
            .as_mut()
            .and_then(|hc| hc.cpu_shares.take())
            .unwrap_or(0);
        let dns = host_config
            .as_mut()
            .and_then(|hc| hc.dns.take())
            .unwrap_or_default();
        let dns_opt = host_config
            .as_mut()
            .and_then(|hc| hc.dns_options.take())
            .unwrap_or_default();
        let dns_search = host_config
            .as_mut()
            .and_then(|hc| hc.dns_search.take())
            .unwrap_or_default();
        // Engine reports each entry as `host:ip`; split on the first `:` so
        // IPv6 addresses remain untouched.
        let extra_hosts = host_config
            .as_mut()
            .and_then(|hc| hc.extra_hosts.take())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                entry
                    .split_once(':')
                    .map(|(host, ip)| (host.to_string(), ip.to_string()))
            })
            .collect::<HashMap<_, _>>();
        let init = host_config.as_mut().and_then(|hc| hc.init.take());
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
        let privileged = host_config
            .as_mut()
            .and_then(|hc| hc.privileged.take())
            .unwrap_or_default();
        let read_only = host_config
            .as_mut()
            .and_then(|hc| hc.readonly_rootfs.take())
            .unwrap_or_default();
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
        let mut volumes = host_config
            .as_mut()
            .and_then(|hc| hc.mounts.take())
            .unwrap_or_default()
            .into_iter()
            .map(Mount::try_from)
            .collect::<Result<Vec<_>>>()?;
        let mem_limit = host_config
            .as_mut()
            .and_then(|hc| hc.memory.take())
            .unwrap_or(0);
        let mem_reservation = host_config
            .as_mut()
            .and_then(|hc| hc.memory_reservation.take())
            .unwrap_or(0);
        let nano_cpus = host_config
            .as_mut()
            .and_then(|hc| hc.nano_cpus.take())
            .unwrap_or(0);
        let oom_score_adj = host_config
            .as_mut()
            .and_then(|hc| hc.oom_score_adj.take())
            .filter(|n| *n != 0);
        let pids_limit = host_config.as_mut().and_then(|hc| hc.pids_limit.take());
        let runtime = host_config.as_mut().and_then(|hc| hc.runtime.take());
        let shm_size = host_config.as_mut().and_then(|hc| hc.shm_size.take());
        let userns_mode = host_config
            .as_mut()
            .and_then(|hc| hc.userns_mode.take())
            .filter(|s| !s.is_empty());
        let uts = host_config
            .as_mut()
            .and_then(|hc| hc.uts_mode.take())
            .filter(|s| !s.is_empty());

        // Legacy supervisor bind mounts are reported via `HostConfig.Binds` as
        // `source:target[:options]` strings, not in `HostConfig.Mounts`. Parse
        // those into `Mount::Bind` so the same container config covers both
        // creation paths.
        let binds = host_config
            .as_mut()
            .and_then(|hc| hc.binds.take())
            .unwrap_or_default();
        for spec in binds {
            volumes.push(Mount::try_from_bind_spec(&spec)?);
        }

        // Sort by target so the serialized form is stable regardless of the
        // order the engine reports the mounts in — Mahler compares state via
        // serialized JSON, so reordering must not trigger reconfiguration.
        volumes.sort_by(|a, b| a.target().cmp(b.target()));

        // Published ports round-trip through `HostConfig.PortBindings`, which
        // holds exactly what the create request asked for. `Config.ExposedPorts`
        // is deliberately ignored (it includes image EXPOSE entries), as is
        // `NetworkSettings.Ports` (only populated while the container runs).
        let ports = host_config
            .as_mut()
            .and_then(|hc| hc.port_bindings.take())
            .map(from_oci_port_map)
            .transpose()?
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
        let cmd = config.as_mut().and_then(|c| c.cmd.take());
        let domainname = config
            .as_mut()
            .and_then(|c| c.domainname.take())
            .filter(|s| !s.is_empty());
        let healthcheck = config
            .as_mut()
            .and_then(|c| c.healthcheck.take())
            .map(Healthcheck::from);
        let hostname = config.as_mut().and_then(|c| c.hostname.take());
        let stop_grace_period = config.as_mut().and_then(|c| c.stop_timeout.take());
        let stop_signal = config.as_mut().and_then(|c| c.stop_signal.take());
        let tty = config
            .as_mut()
            .and_then(|c| c.tty.take())
            .unwrap_or_default();
        let user = config.as_mut().and_then(|c| c.user.take());
        let working_dir = config.as_mut().and_then(|c| c.working_dir.take());

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
            cgroup,
            cgroup_parent,
            command: cmd,
            cpuset,
            cpu_rt_period,
            cpu_rt_runtime,
            cpu_shares,
            dns,
            dns_opt,
            dns_search,
            domainname,
            environment,
            extra_hosts,
            healthcheck,
            hostname,
            init,
            labels,
            mem_limit,
            mem_reservation,
            nano_cpus,
            oom_score_adj,
            pids_limit,
            privileged,
            read_only,
            restart_policy,
            runtime,
            shm_size,
            stop_grace_period,
            stop_signal,
            tty,
            user,
            userns_mode,
            uts,
            working_dir,
            networks,
            network_mode,
            volumes,
            ports,
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
            STOPPING => ContainerStatus::Stopped,
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

/// Cgroup namespace mode for a container.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Cgroup {
    Host,
    #[default]
    Private,
}

impl From<Cgroup> for bollard::models::HostConfigCgroupnsModeEnum {
    fn from(value: Cgroup) -> Self {
        match value {
            Cgroup::Host => Self::HOST,
            Cgroup::Private => Self::PRIVATE,
        }
    }
}

impl From<bollard::models::HostConfigCgroupnsModeEnum> for Cgroup {
    fn from(value: bollard::models::HostConfigCgroupnsModeEnum) -> Self {
        use bollard::models::HostConfigCgroupnsModeEnum::*;
        match value {
            HOST => Cgroup::Host,
            PRIVATE | EMPTY => Cgroup::Private,
        }
    }
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

fn non_zero(n: i64) -> Option<i64> {
    (n != 0).then_some(n)
}

/// Container healthcheck configuration. Durations are in nanoseconds to match
/// the engine API.
/// `start_interval` requires Docker Engine 25.0+ (API 1.44) or
/// Podman 4.8+. Older daemons ignore unknown fields.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Healthcheck {
    /// Test command. `["NONE"]` disables an inherited healthcheck. With
    /// `["CMD", args...]` the args are exec'd directly. With
    /// `["CMD-SHELL", command]` the command runs through the default shell.
    /// An empty/`None` value means inherit from the image.
    pub test: Option<Vec<String>>,

    /// Interval between checks
    pub interval: Option<i64>,

    /// Time to wait for a probe to complete
    pub timeout: Option<i64>,

    /// Grace period after start before failures count
    pub start_period: Option<i64>,

    /// Interval between checks during the start period
    pub start_interval: Option<i64>,

    /// Consecutive failures needed to mark the container unhealthy
    pub retries: Option<i64>,
}

impl Healthcheck {
    /// True when every sub-field is unset — semantically equivalent to
    /// "no healthcheck specified" at this layer.
    pub fn is_empty(&self) -> bool {
        matches!(
            self,
            Self {
                test: None,
                interval: None,
                timeout: None,
                start_period: None,
                start_interval: None,
                retries: None,
            }
        )
    }
}

impl From<Healthcheck> for bollard::models::HealthConfig {
    fn from(value: Healthcheck) -> Self {
        let Healthcheck {
            test,
            interval,
            timeout,
            start_period,
            start_interval,
            retries,
        } = value;
        Self {
            test,
            interval,
            timeout,
            start_period,
            start_interval,
            retries,
        }
    }
}

impl From<bollard::models::HealthConfig> for Healthcheck {
    fn from(value: bollard::models::HealthConfig) -> Self {
        let bollard::models::HealthConfig {
            test,
            interval,
            timeout,
            start_period,
            start_interval,
            retries,
        } = value;
        Self {
            test,
            interval,
            timeout,
            start_period,
            start_interval,
            retries,
        }
    }
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

/// Bind mount propagation mode. Mirrors the compose `bind.propagation` setting.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BindPropagation {
    #[default]
    Private,
    Rprivate,
    Shared,
    Rshared,
    Slave,
    Rslave,
}

impl From<BindPropagation> for MountBindOptionsPropagationEnum {
    fn from(value: BindPropagation) -> Self {
        match value {
            BindPropagation::Private => Self::PRIVATE,
            BindPropagation::Rprivate => Self::RPRIVATE,
            BindPropagation::Shared => Self::SHARED,
            BindPropagation::Rshared => Self::RSHARED,
            BindPropagation::Slave => Self::SLAVE,
            BindPropagation::Rslave => Self::RSLAVE,
        }
    }
}

impl TryFrom<MountBindOptionsPropagationEnum> for BindPropagation {
    type Error = ();
    fn try_from(value: MountBindOptionsPropagationEnum) -> std::result::Result<Self, Self::Error> {
        Ok(match value {
            MountBindOptionsPropagationEnum::PRIVATE => Self::Private,
            MountBindOptionsPropagationEnum::RPRIVATE => Self::Rprivate,
            MountBindOptionsPropagationEnum::SHARED => Self::Shared,
            MountBindOptionsPropagationEnum::RSHARED => Self::Rshared,
            MountBindOptionsPropagationEnum::SLAVE => Self::Slave,
            MountBindOptionsPropagationEnum::RSLAVE => Self::Rslave,
            MountBindOptionsPropagationEnum::EMPTY => return Err(()),
        })
    }
}

/// A container mount declared by the composition.
///
/// Three mount types are supported at the composition level: named volumes,
/// host bind mounts, and tmpfs. A fourth variant, [`Mount::Other`], exists
/// only for round-tripping container state: if the engine reports a mount
/// type helios doesn't support (e.g. `image`, `npipe`, `cluster`), we
/// preserve the target so that inspecting the container doesn't fail.
/// Target-state flows never produce `Other`.
///
/// The `target` path inside the container is the unique identity of each
/// mount within a `ContainerConfig`.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Mount {
    Volume {
        /// Path inside the container where the volume is mounted
        target: String,
        /// Name of the volume (may be namespaced by the caller)
        source: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        read_only: bool,
        /// Disable copying of data from the target path when the volume is created
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        nocopy: bool,
        /// Subpath inside the volume to mount
        subpath: Option<String>,
    },
    Bind {
        target: String,
        /// Host path to mount into the container
        source: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        read_only: bool,
        #[serde(default)]
        propagation: BindPropagation,
        /// Create the host source path if it does not exist
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        create_host_path: bool,
    },
    Tmpfs {
        target: String,
        /// Size of the tmpfs in bytes
        size: Option<i64>,
        /// File mode for the tmpfs mount, encoded as Unix permission bits
        mode: Option<u32>,
    },
    /// Placeholder for a mount type helios doesn't model (image, npipe, cluster).
    /// Only produced when reading existing container state back from the engine.
    Other {
        target: String,
        /// Raw mount type string as reported by the engine, retained for
        /// diagnostics and so round-tripped state is stable.
        kind: String,
    },
}

impl Mount {
    /// Container path this mount is bound to. Unique within a `ContainerConfig`.
    pub fn target(&self) -> &str {
        match self {
            Self::Volume { target, .. } => target,
            Self::Bind { target, .. } => target,
            Self::Tmpfs { target, .. } => target,
            Self::Other { target, .. } => target,
        }
    }

    /// Parse a Docker bind spec (`source:target[:options]`) into a `Mount::Bind`.
    ///
    /// Used to read the legacy `HostConfig.Binds` list back from the engine.
    /// Unknown options (SELinux labels, `nocopy`, …) are ignored: the engine
    /// applies them at create time and they don't round-trip through this model.
    fn try_from_bind_spec(spec: &str) -> Result<Self> {
        let mut parts = spec.splitn(3, ':');
        let source = parts.next().unwrap_or_default();
        let target = parts
            .next()
            .ok_or_else(|| Error::from(format!("bind spec '{spec}' is missing a target")))?;
        if source.is_empty() || target.is_empty() {
            return Err(format!("bind spec '{spec}' has an empty source or target").into());
        }

        let mut read_only = false;
        let mut propagation = None;
        if let Some(opts) = parts.next() {
            for opt in opts.split(',').filter(|o| !o.is_empty()) {
                match opt {
                    "ro" => read_only = true,
                    "rw" => read_only = false,
                    "private" => propagation = Some(BindPropagation::Private),
                    "rprivate" => propagation = Some(BindPropagation::Rprivate),
                    "shared" => propagation = Some(BindPropagation::Shared),
                    "rshared" => propagation = Some(BindPropagation::Rshared),
                    "slave" => propagation = Some(BindPropagation::Slave),
                    "rslave" => propagation = Some(BindPropagation::Rslave),
                    _ => {}
                }
            }
        }

        Ok(Mount::Bind {
            target: target.to_owned(),
            source: source.to_owned(),
            read_only,
            propagation: propagation.unwrap_or_default(),
            // bind mounts will create the host path by default
            create_host_path: true,
        })
    }
}

impl From<Mount> for bollard::models::Mount {
    fn from(value: Mount) -> Self {
        match value {
            Mount::Volume {
                target,
                source,
                read_only,
                nocopy,
                subpath,
            } => {
                let volume_options = if nocopy || subpath.is_some() {
                    Some(MountVolumeOptions {
                        no_copy: nocopy.then_some(true),
                        subpath,
                        ..Default::default()
                    })
                } else {
                    None
                };
                bollard::models::Mount {
                    typ: Some(MountType::VOLUME),
                    target: Some(target),
                    source: Some(source),
                    read_only: Some(read_only),
                    volume_options,
                    ..Default::default()
                }
            }
            Mount::Bind {
                target,
                source,
                read_only,
                propagation,
                create_host_path,
            } => {
                let bind_options = if propagation != BindPropagation::default() || create_host_path
                {
                    Some(MountBindOptions {
                        propagation: Some(propagation.into()),
                        create_mountpoint: create_host_path.then_some(true),
                        ..Default::default()
                    })
                } else {
                    None
                };
                bollard::models::Mount {
                    typ: Some(MountType::BIND),
                    target: Some(target),
                    source: Some(source),
                    read_only: Some(read_only),
                    bind_options,
                    ..Default::default()
                }
            }
            Mount::Tmpfs { target, size, mode } => {
                let tmpfs_options = if size.is_some() || mode.is_some() {
                    Some(MountTmpfsOptions {
                        size_bytes: size,
                        mode: mode.map(|m| m as i64),
                        ..Default::default()
                    })
                } else {
                    None
                };
                bollard::models::Mount {
                    typ: Some(MountType::TMPFS),
                    target: Some(target),
                    tmpfs_options,
                    ..Default::default()
                }
            }
            // `Other` only comes from reading an existing container; it must
            // never reach a create request. Panic to surface the programmer
            // bug rather than silently sending a malformed Mount to the engine.
            Mount::Other { target, kind } => {
                unreachable!(
                    "Mount::Other (kind='{kind}', target='{target}') cannot be written to the engine"
                )
            }
        }
    }
}

impl TryFrom<bollard::models::Mount> for Mount {
    type Error = Error;

    fn try_from(value: bollard::models::Mount) -> Result<Self> {
        let target = value.target.ok_or("mount target is required")?;
        let read_only = value.read_only.unwrap_or_default();
        match value.typ {
            Some(MountType::VOLUME) => {
                let source = value.source.unwrap_or_default();
                let (nocopy, subpath) = value
                    .volume_options
                    .map(|o| (o.no_copy.unwrap_or_default(), o.subpath))
                    .unwrap_or((false, None));
                Ok(Mount::Volume {
                    target,
                    source,
                    read_only,
                    nocopy,
                    subpath,
                })
            }
            Some(MountType::BIND) => {
                let source = value.source.unwrap_or_default();
                let (propagation, create_host_path) = value
                    .bind_options
                    .map(|o| {
                        (
                            o.propagation
                                .and_then(|p| BindPropagation::try_from(p).ok())
                                .unwrap_or_default(),
                            o.create_mountpoint.unwrap_or_default(),
                        )
                    })
                    .unwrap_or((BindPropagation::default(), false));
                Ok(Mount::Bind {
                    target,
                    source,
                    read_only,
                    propagation,
                    create_host_path,
                })
            }
            Some(MountType::TMPFS) => {
                let (size, mode) = value
                    .tmpfs_options
                    .map(|o| (o.size_bytes, o.mode.map(|m| m as u32)))
                    .unwrap_or((None, None));
                Ok(Mount::Tmpfs { target, size, mode })
            }
            // Preserve unsupported mount types so that inspecting an existing
            // container doesn't fail. Target-state flows never produce these.
            other => Ok(Mount::Other {
                target,
                kind: other.map(|t| t.to_string()).unwrap_or_default(),
            }),
        }
    }
}

/// Container configuration that is portable between hosts
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ContainerConfig {
    /// Cgroup namespace mode (`host` or `private`)
    pub cgroup: Cgroup,

    /// Path to cgroups under which the container's cgroup is created
    pub cgroup_parent: Option<String>,

    /// Command to run specified as an array of strings
    pub command: Option<Vec<String>>,

    /// CPUs the container is allowed to run on (`0-3`, `1,3`, etc)
    pub cpuset: Option<String>,

    /// Real-time CPU period in microseconds.
    #[serde(skip_serializing_if = "is_zero")]
    pub cpu_rt_period: i64,

    /// Real-time CPU runtime in microseconds (must be <= `cpu_rt_period`).
    #[serde(skip_serializing_if = "is_zero")]
    pub cpu_rt_runtime: i64,

    /// CPU shares (relative weight vs other containers; default 1024)
    #[serde(skip_serializing_if = "is_zero")]
    pub cpu_shares: i64,

    /// Custom DNS servers for the container.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dns: Vec<String>,

    /// Custom DNS resolver options (`/etc/resolv.conf` options).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dns_opt: Vec<String>,

    /// Custom DNS search domains.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dns_search: Vec<String>,

    /// NIS domain name to use for the container.
    pub domainname: Option<String>,

    /// Environment variables
    #[serde(skip_serializing_if = "Environment::is_empty")]
    pub environment: Environment,

    /// Hostname to use for the container, as a valid RFC 1123 hostname
    pub hostname: Option<String>,

    /// Run an init process inside the container. `None` defers to the daemon
    /// default, `Some(_)` overrides it.
    pub init: Option<bool>,

    /// User-defined key/value metadata
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,

    /// Memory limit in bytes
    #[serde(skip_serializing_if = "is_zero")]
    pub mem_limit: i64,

    /// Memory reservation in bytes
    #[serde(skip_serializing_if = "is_zero")]
    pub mem_reservation: i64,

    /// CPU quota in nanoseconds-of-CPU-per-second (1 CPU = 1_000_000_000),
    /// Compose's fractional `cpus` is stored here after the `* 1e9` conversion
    #[serde(skip_serializing_if = "is_zero")]
    pub nano_cpus: i64,

    /// Run container with extended privileges.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub privileged: bool,

    /// Mount container's root filesystem as read-only.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,

    /// Allocate a pseudo-TTY for the container
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub tty: bool,

    /// Restart policy for the container
    pub restart_policy: RestartPolicy,

    /// Runtime to use with this container, such as `runc` or `nvidia`
    pub runtime: Option<String>,

    /// Size of `/dev/shm` in bytes
    pub shm_size: Option<i64>,

    /// Time in seconds the engine waits for a stopping container before
    /// killing it
    pub stop_grace_period: Option<i64>,

    /// Signal sent to stop the container
    pub stop_signal: Option<String>,

    /// User the container runs commands as, format `<name|uid>[:<group|gid>]`
    pub user: Option<String>,

    /// User namespace mode, only `host` is supported
    pub userns_mode: Option<String>,

    /// UTS namespace mode (`host` or empty)
    pub uts: Option<String>,

    /// Working directory for commands run inside the container
    pub working_dir: Option<String>,

    /// Tune the host's OOM-killer score adjustment for the container
    /// process (-1000..=1000)
    pub oom_score_adj: Option<i64>,

    /// Maximum number of process IDs allowed in the container
    pub pids_limit: Option<i64>,

    /// Extra entries added to the container's `/etc/hosts`, each in the
    /// engine's `host:ip` form.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub extra_hosts: HashMap<String, String>,

    /// Network endpoint configurations, ordered by connection priority.
    ///
    /// Mutually exclusive with `network_mode`.
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub networks: IndexMap<String, NetworkSettings>,

    /// Container network mode. When set, `networks` must be empty.
    pub network_mode: Option<NetworkMode>,

    /// Filesystem mounts (volume, bind, tmpfs) indexed by target path.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<Mount>,

    /// Published container ports. Serialized as compose short-syntax strings;
    /// the set keeps the serialized form deterministic and deduplicated.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub ports: BTreeSet<PortMapping>,

    /// Container healthcheck. `None` means defer to the image's HEALTHCHECK
    pub healthcheck: Option<Healthcheck>,
}

impl From<ContainerConfig> for ContainerCreateBody {
    fn from(value: ContainerConfig) -> Self {
        let ContainerConfig {
            cgroup,
            cgroup_parent,
            command: cmd,
            cpuset,
            cpu_rt_period,
            cpu_rt_runtime,
            cpu_shares,
            dns,
            dns_opt,
            dns_search,
            domainname,
            environment,
            extra_hosts,
            healthcheck,
            hostname,
            init,
            labels,
            mem_limit,
            mem_reservation,
            nano_cpus,
            oom_score_adj,
            pids_limit,
            privileged,
            read_only,
            restart_policy,
            runtime,
            shm_size,
            stop_grace_period,
            stop_signal,
            tty,
            user,
            userns_mode,
            uts,
            working_dir,
            networks,
            network_mode,
            volumes,
            ports,
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

        let engine_mounts = if volumes.is_empty() {
            None
        } else {
            Some(volumes.into_iter().map(Into::into).collect())
        };

        let (exposed_ports, port_bindings) = if ports.is_empty() {
            (None, None)
        } else {
            let (exposed, bindings) = to_oci_port_maps(ports);
            (Some(exposed), Some(bindings))
        };

        let cgroupns_mode = Some(cgroup.into());

        let host_config = bollard::config::HostConfig {
            cgroup_parent,
            cgroupns_mode,
            cpuset_cpus: cpuset,
            cpu_realtime_period: non_zero(cpu_rt_period),
            cpu_realtime_runtime: non_zero(cpu_rt_runtime),
            cpu_shares: non_zero(cpu_shares),
            dns: (!dns.is_empty()).then_some(dns),
            dns_options: (!dns_opt.is_empty()).then_some(dns_opt),
            dns_search: (!dns_search.is_empty()).then_some(dns_search),
            extra_hosts: (!extra_hosts.is_empty()).then(|| {
                extra_hosts
                    .into_iter()
                    .map(|(host, ip)| format!("{host}:{ip}"))
                    .collect()
            }),
            init,
            memory: non_zero(mem_limit),
            memory_reservation: non_zero(mem_reservation),
            mounts: engine_mounts,
            nano_cpus: non_zero(nano_cpus),
            network_mode: host_network_mode,
            oom_score_adj,
            pids_limit,
            port_bindings,
            privileged: Some(privileged),
            readonly_rootfs: Some(read_only),
            restart_policy: Some(restart_policy),
            runtime,
            shm_size,
            userns_mode,
            uts_mode: uts,
            ..Default::default()
        };

        ContainerCreateBody {
            cmd,
            domainname,
            env,
            exposed_ports,
            healthcheck: healthcheck.map(Into::into),
            hostname,
            labels: Some(labels),
            networking_config,
            host_config: Some(host_config),
            stop_signal,
            stop_timeout: stop_grace_period,
            tty: Some(tty),
            user,
            working_dir,
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

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{HostConfig, Mount as EngineMount};

    fn vol_mount(target: &str, source: &str) -> EngineMount {
        EngineMount {
            typ: Some(MountType::VOLUME),
            target: Some(target.to_string()),
            source: Some(source.to_string()),
            read_only: Some(false),
            ..Default::default()
        }
    }

    fn inspect_with_mounts(mounts: Vec<EngineMount>) -> ContainerInspectResponse {
        inspect_with(Some(mounts), None)
    }

    fn inspect_with_binds(binds: Vec<&str>) -> ContainerInspectResponse {
        inspect_with(None, Some(binds.into_iter().map(String::from).collect()))
    }

    fn inspect_with(
        mounts: Option<Vec<EngineMount>>,
        binds: Option<Vec<String>>,
    ) -> ContainerInspectResponse {
        ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                mounts,
                binds,
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn inspect_sorts_volumes_by_target() {
        let c: LocalContainer = inspect_with_mounts(vec![
            vol_mount("/c", "vol-c"),
            vol_mount("/a", "vol-a"),
            vol_mount("/b", "vol-b"),
        ])
        .try_into()
        .unwrap();
        let targets: Vec<&str> = c.config.volumes.iter().map(|m| m.target()).collect();
        assert_eq!(targets, vec!["/a", "/b", "/c"]);
    }

    #[test]
    fn inspect_preserves_unsupported_mount_types_as_other() {
        // An `image` mount on an existing container must not cause the inspect
        // conversion to fail — it round-trips as `Mount::Other`.
        let image_mount = EngineMount {
            typ: Some(MountType::IMAGE),
            target: Some("/data".to_string()),
            source: Some("some/image".to_string()),
            ..Default::default()
        };
        let c: LocalContainer = inspect_with_mounts(vec![image_mount]).try_into().unwrap();
        assert_eq!(c.config.volumes.len(), 1);
        match &c.config.volumes[0] {
            Mount::Other { target, kind } => {
                assert_eq!(target, "/data");
                assert_eq!(kind, "image");
            }
            other => panic!("expected Mount::Other, got {other:?}"),
        }
    }

    #[test]
    fn inspect_reads_bool_host_flags() {
        let resp = ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                init: Some(true),
                privileged: Some(true),
                readonly_rootfs: Some(true),
                ..Default::default()
            }),
            config: Some(bollard::models::ContainerConfig {
                tty: Some(true),
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c: LocalContainer = resp.try_into().unwrap();
        assert_eq!(c.config.init, Some(true));
        assert!(c.config.privileged);
        assert!(c.config.read_only);
        assert!(c.config.tty);
    }

    #[test]
    fn inspect_defaults_missing_bool_flags_to_false_and_init_to_none() {
        let c: LocalContainer = inspect_with_mounts(vec![]).try_into().unwrap();
        assert_eq!(c.config.init, None);
        assert!(!c.config.privileged);
        assert!(!c.config.read_only);
        assert!(!c.config.tty);
    }

    #[test]
    fn container_create_body_emits_bool_host_flags() {
        let cfg = ContainerConfig {
            init: Some(false),
            privileged: true,
            read_only: true,
            tty: true,
            ..Default::default()
        };
        let body: ContainerCreateBody = cfg.into();
        assert_eq!(body.tty, Some(true));
        let hc = body.host_config.unwrap();
        assert_eq!(hc.init, Some(false));
        assert_eq!(hc.privileged, Some(true));
        assert_eq!(hc.readonly_rootfs, Some(true));
    }

    #[test]
    fn container_create_body_preserves_init_none() {
        let cfg = ContainerConfig::default();
        let body: ContainerCreateBody = cfg.into();
        assert_eq!(body.tty, Some(false));
        let hc = body.host_config.unwrap();
        assert_eq!(hc.init, None);
        assert_eq!(hc.privileged, Some(false));
        assert_eq!(hc.readonly_rootfs, Some(false));
    }

    #[test]
    fn container_create_body_emits_ports() {
        let cfg = ContainerConfig {
            ports: BTreeSet::from([
                "8080:80".parse().unwrap(),
                "127.0.0.1:8081:80".parse().unwrap(),
                "53:53/udp".parse().unwrap(),
            ]),
            ..Default::default()
        };
        let body: ContainerCreateBody = cfg.into();
        assert_eq!(
            body.exposed_ports,
            Some(vec!["53/udp".into(), "80/tcp".into()])
        );
        let bindings = body.host_config.unwrap().port_bindings.unwrap();
        assert_eq!(
            bindings["80/tcp"],
            Some(vec![
                bollard::models::PortBinding {
                    host_ip: None,
                    host_port: Some("8080".to_string()),
                },
                bollard::models::PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some("8081".to_string()),
                },
            ])
        );
        assert_eq!(
            bindings["53/udp"],
            Some(vec![bollard::models::PortBinding {
                host_ip: None,
                host_port: Some("53".to_string()),
            }])
        );
    }

    #[test]
    fn container_create_body_omits_empty_ports() {
        let body: ContainerCreateBody = ContainerConfig::default().into();
        assert_eq!(body.exposed_ports, None);
        assert_eq!(body.host_config.unwrap().port_bindings, None);
    }

    #[test]
    fn inspect_reads_port_bindings() {
        let resp = ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                port_bindings: Some(bollard::models::PortMap::from([(
                    "80/tcp".to_string(),
                    Some(vec![bollard::models::PortBinding {
                        // the engine reports unset values as empty strings
                        host_ip: Some("".to_string()),
                        host_port: Some("8080".to_string()),
                    }]),
                )])),
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c: LocalContainer = resp.try_into().unwrap();
        assert_eq!(c.config.ports, BTreeSet::from(["8080:80".parse().unwrap()]));
    }

    #[test]
    fn ports_round_trip_through_create_and_inspect() {
        let ports: BTreeSet<PortMapping> = BTreeSet::from([
            "8080:80".parse().unwrap(),
            "127.0.0.1:8081:80".parse().unwrap(),
            "53:53/udp".parse().unwrap(),
            "443".parse().unwrap(),
            "8000-9000:3000".parse().unwrap(),
        ]);
        let cfg = ContainerConfig {
            ports: ports.clone(),
            ..Default::default()
        };
        let body: ContainerCreateBody = cfg.into();

        let resp = ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                port_bindings: body.host_config.unwrap().port_bindings,
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c: LocalContainer = resp.try_into().unwrap();
        assert_eq!(c.config.ports, ports);
    }

    #[test]
    fn inspect_reads_string_fields() {
        let resp = ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                cgroup_parent: Some("/custom".to_string()),
                cgroupns_mode: Some(bollard::models::HostConfigCgroupnsModeEnum::HOST),
                cpuset_cpus: Some("0-3".to_string()),
                runtime: Some("runc".to_string()),
                userns_mode: Some("host".to_string()),
                uts_mode: Some("host".to_string()),
                ..Default::default()
            }),
            config: Some(bollard::models::ContainerConfig {
                hostname: Some("my-host".to_string()),
                domainname: Some("example.com".to_string()),
                user: Some("1000:1000".to_string()),
                stop_signal: Some("SIGTERM".to_string()),
                working_dir: Some("/app".to_string()),
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c: LocalContainer = resp.try_into().unwrap();
        assert_eq!(c.config.cgroup, Cgroup::Host);
        assert_eq!(c.config.cgroup_parent.as_deref(), Some("/custom"));
        assert_eq!(c.config.cpuset.as_deref(), Some("0-3"));
        assert_eq!(c.config.runtime.as_deref(), Some("runc"));
        assert_eq!(c.config.userns_mode.as_deref(), Some("host"));
        assert_eq!(c.config.uts.as_deref(), Some("host"));
        assert_eq!(c.config.hostname.as_deref(), Some("my-host"));
        assert_eq!(c.config.domainname.as_deref(), Some("example.com"));
        assert_eq!(c.config.user.as_deref(), Some("1000:1000"));
        assert_eq!(c.config.stop_signal.as_deref(), Some("SIGTERM"));
        assert_eq!(c.config.working_dir.as_deref(), Some("/app"));
    }

    #[test]
    fn container_create_body_emits_string_fields() {
        let cfg = ContainerConfig {
            cgroup: Cgroup::Host,
            cgroup_parent: Some("/custom".to_string()),
            cpuset: Some("0-3".to_string()),
            domainname: Some("example.com".to_string()),
            hostname: Some("my-host".to_string()),
            runtime: Some("runc".to_string()),
            stop_signal: Some("SIGTERM".to_string()),
            user: Some("1000:1000".to_string()),
            userns_mode: Some("host".to_string()),
            uts: Some("host".to_string()),
            working_dir: Some("/app".to_string()),
            ..Default::default()
        };
        let body: ContainerCreateBody = cfg.into();
        assert_eq!(body.hostname.as_deref(), Some("my-host"));
        assert_eq!(body.domainname.as_deref(), Some("example.com"));
        assert_eq!(body.user.as_deref(), Some("1000:1000"));
        assert_eq!(body.stop_signal.as_deref(), Some("SIGTERM"));
        assert_eq!(body.working_dir.as_deref(), Some("/app"));
        let hc = body.host_config.unwrap();
        assert_eq!(
            hc.cgroupns_mode,
            Some(bollard::models::HostConfigCgroupnsModeEnum::HOST)
        );
        assert_eq!(hc.cgroup_parent.as_deref(), Some("/custom"));
        assert_eq!(hc.cpuset_cpus.as_deref(), Some("0-3"));
        assert_eq!(hc.runtime.as_deref(), Some("runc"));
        assert_eq!(hc.userns_mode.as_deref(), Some("host"));
        assert_eq!(hc.uts_mode.as_deref(), Some("host"));
    }

    #[test]
    fn inspect_reads_number_host_fields() {
        let resp = ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                cpu_realtime_period: Some(1_000_000),
                cpu_realtime_runtime: Some(950_000),
                cpu_shares: Some(2048),
                memory: Some(1073741824),
                memory_reservation: Some(536870912),
                nano_cpus: Some(1_500_000_000),
                oom_score_adj: Some(-500),
                pids_limit: Some(100),
                shm_size: Some(67108864),
                ..Default::default()
            }),
            config: Some(bollard::models::ContainerConfig {
                stop_timeout: Some(30),
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c: LocalContainer = resp.try_into().unwrap();
        assert_eq!(c.config.cpu_rt_period, 1_000_000);
        assert_eq!(c.config.cpu_rt_runtime, 950_000);
        assert_eq!(c.config.cpu_shares, 2048);
        assert_eq!(c.config.mem_limit, 1073741824);
        assert_eq!(c.config.mem_reservation, 536870912);
        assert_eq!(c.config.nano_cpus, 1_500_000_000);
        assert_eq!(c.config.oom_score_adj, Some(-500));
        assert_eq!(c.config.pids_limit, Some(100));
        assert_eq!(c.config.shm_size, Some(67108864));
        assert_eq!(c.config.stop_grace_period, Some(30));
    }

    #[test]
    fn container_create_body_emits_number_host_fields() {
        let cfg = ContainerConfig {
            cpu_rt_period: 1_000_000,
            cpu_rt_runtime: 950_000,
            cpu_shares: 2048,
            mem_limit: 1073741824,
            mem_reservation: 536870912,
            nano_cpus: 1_500_000_000,
            oom_score_adj: Some(-500),
            pids_limit: Some(100),
            shm_size: Some(67108864),
            stop_grace_period: Some(30),
            ..Default::default()
        };
        let body: ContainerCreateBody = cfg.into();
        assert_eq!(body.stop_timeout, Some(30));
        let hc = body.host_config.unwrap();
        assert_eq!(hc.cpu_realtime_period, Some(1_000_000));
        assert_eq!(hc.cpu_realtime_runtime, Some(950_000));
        assert_eq!(hc.cpu_shares, Some(2048));
        assert_eq!(hc.memory, Some(1073741824));
        assert_eq!(hc.memory_reservation, Some(536870912));
        assert_eq!(hc.nano_cpus, Some(1_500_000_000));
        assert_eq!(hc.oom_score_adj, Some(-500));
        assert_eq!(hc.pids_limit, Some(100));
        assert_eq!(hc.shm_size, Some(67108864));
    }

    #[test]
    fn inspect_reads_extra_hosts() {
        let resp = ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                extra_hosts: Some(vec![
                    "foo:127.0.0.1".to_string(),
                    "bar:8.8.8.8".to_string(),
                    "v6:2001:db8::1".to_string(),
                    "other-v6:::1".to_string(),
                ]),
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c: LocalContainer = resp.try_into().unwrap();
        assert_eq!(
            c.config.extra_hosts,
            HashMap::from([
                ("foo".to_string(), "127.0.0.1".to_string()),
                ("bar".to_string(), "8.8.8.8".to_string()),
                ("v6".to_string(), "2001:db8::1".to_string()),
                ("other-v6".to_string(), "::1".to_string()),
            ])
        );
    }

    #[test]
    fn container_create_body_emits_extra_hosts() {
        let cfg = ContainerConfig {
            extra_hosts: HashMap::from([
                ("foo".to_string(), "127.0.0.1".to_string()),
                ("bar".to_string(), "8.8.8.8".to_string()),
            ]),
            ..Default::default()
        };
        let body: ContainerCreateBody = cfg.into();
        let hc = body.host_config.unwrap();
        let mut hosts = hc.extra_hosts.unwrap();
        hosts.sort();
        assert_eq!(
            hosts,
            vec!["bar:8.8.8.8".to_string(), "foo:127.0.0.1".to_string()]
        );

        // An empty map should not emit an ExtraHosts entry on the engine request
        let empty: ContainerCreateBody = ContainerConfig::default().into();
        assert_eq!(empty.host_config.unwrap().extra_hosts, None);
    }

    #[test]
    fn inspect_reads_dns_config() {
        let resp = ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                dns: Some(vec!["8.8.8.8".to_string(), "9.9.9.9".to_string()]),
                dns_options: Some(vec!["use-vc".to_string()]),
                dns_search: Some(vec!["example.com".to_string()]),
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c: LocalContainer = resp.try_into().unwrap();
        assert_eq!(
            c.config.dns,
            vec!["8.8.8.8".to_string(), "9.9.9.9".to_string()]
        );
        assert_eq!(c.config.dns_opt, vec!["use-vc".to_string()]);
        assert_eq!(c.config.dns_search, vec!["example.com".to_string()]);
    }

    #[test]
    fn container_create_body_emits_dns_config() {
        let cfg = ContainerConfig {
            dns: vec!["8.8.8.8".to_string(), "9.9.9.9".to_string()],
            dns_opt: vec!["use-vc".to_string(), "no-tld-query".to_string()],
            dns_search: vec!["dc1.example.com".to_string()],
            ..Default::default()
        };
        let body: ContainerCreateBody = cfg.into();
        let hc = body.host_config.unwrap();
        assert_eq!(
            hc.dns,
            Some(vec!["8.8.8.8".to_string(), "9.9.9.9".to_string()])
        );
        assert_eq!(
            hc.dns_options,
            Some(vec!["use-vc".to_string(), "no-tld-query".to_string()])
        );
        assert_eq!(hc.dns_search, Some(vec!["dc1.example.com".to_string()]));

        // Empty lists should not emit Dns/DnsOptions/DnsSearch on the engine request
        let empty: ContainerCreateBody = ContainerConfig::default().into();
        let hc = empty.host_config.unwrap();
        assert_eq!(hc.dns, None);
        assert_eq!(hc.dns_options, None);
        assert_eq!(hc.dns_search, None);
    }

    #[test]
    fn inspect_collapses_default_sentinels() {
        // Engine reports `""` and `0` for fields the user didn't set; the
        // inspect path collapses those sentinels to the field's default
        // (`None` for string options, `0` for numeric fields) so target
        // round-trip is stable without LABEL_CONFIG_FIELDS tracking.
        let resp = ContainerInspectResponse {
            id: Some("cid".to_string()),
            name: Some("/svc".to_string()),
            image: Some("img".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            host_config: Some(HostConfig {
                cgroup_parent: Some(String::new()),
                cpuset_cpus: Some(String::new()),
                cpu_realtime_period: Some(0),
                cpu_realtime_runtime: Some(0),
                cpu_shares: Some(0),
                memory: Some(0),
                memory_reservation: Some(0),
                nano_cpus: Some(0),
                oom_score_adj: Some(0),
                userns_mode: Some(String::new()),
                uts_mode: Some(String::new()),
                ..Default::default()
            }),
            config: Some(bollard::models::ContainerConfig {
                domainname: Some(String::new()),
                ..Default::default()
            }),
            state: Some(bollard::models::ContainerState {
                status: Some(ContainerStateStatusEnum::RUNNING),
                ..Default::default()
            }),
            ..Default::default()
        };
        let c: LocalContainer = resp.try_into().unwrap();
        assert_eq!(c.config.cgroup_parent, None);
        assert_eq!(c.config.cpuset, None);
        assert_eq!(c.config.cpu_rt_period, 0);
        assert_eq!(c.config.cpu_rt_runtime, 0);
        assert_eq!(c.config.cpu_shares, 0);
        assert_eq!(c.config.domainname, None);
        assert_eq!(c.config.mem_limit, 0);
        assert_eq!(c.config.mem_reservation, 0);
        assert_eq!(c.config.nano_cpus, 0);
        assert_eq!(c.config.oom_score_adj, None);
        assert_eq!(c.config.userns_mode, None);
        assert_eq!(c.config.uts, None);
    }

    #[test]
    fn inspect_fails_on_missing_mount_target() {
        let no_target = EngineMount {
            typ: Some(MountType::VOLUME),
            target: None,
            source: Some("vol".to_string()),
            ..Default::default()
        };
        let result: Result<LocalContainer> = inspect_with_mounts(vec![no_target]).try_into();
        assert!(result.is_err());
    }

    #[test]
    fn inspect_reads_binds_as_bind_mounts() {
        let c: LocalContainer = inspect_with_binds(vec![
            "/host/a:/container/a",
            "/host/b:/container/b:ro",
            "/host/c:/container/c:ro,rshared",
        ])
        .try_into()
        .unwrap();
        assert_eq!(c.config.volumes.len(), 3);
        assert_eq!(
            c.config.volumes[0],
            Mount::Bind {
                target: "/container/a".to_string(),
                source: "/host/a".to_string(),
                read_only: false,
                propagation: BindPropagation::Private,
                create_host_path: true,
            }
        );
        assert_eq!(
            c.config.volumes[1],
            Mount::Bind {
                target: "/container/b".to_string(),
                source: "/host/b".to_string(),
                read_only: true,
                propagation: BindPropagation::Private,
                create_host_path: true,
            }
        );
        assert_eq!(
            c.config.volumes[2],
            Mount::Bind {
                target: "/container/c".to_string(),
                source: "/host/c".to_string(),
                read_only: true,
                propagation: BindPropagation::Rshared,
                create_host_path: true,
            }
        );
    }

    #[test]
    fn inspect_merges_mounts_and_binds_sorted_by_target() {
        let response = inspect_with(
            Some(vec![vol_mount("/c", "vol-c")]),
            Some(vec!["/host/a:/a".to_string(), "/host/b:/b:ro".to_string()]),
        );
        let c: LocalContainer = response.try_into().unwrap();
        let targets: Vec<&str> = c.config.volumes.iter().map(|m| m.target()).collect();
        assert_eq!(targets, vec!["/a", "/b", "/c"]);
    }

    #[test]
    fn inspect_ignores_unknown_bind_options() {
        // SELinux labels and other engine-applied options are not modeled and
        // must not cause the parse to fail.
        let c: LocalContainer = inspect_with_binds(vec!["/h:/t:z,Z,nocopy,rw"])
            .try_into()
            .unwrap();
        assert_eq!(
            c.config.volumes[0],
            Mount::Bind {
                target: "/t".to_string(),
                source: "/h".to_string(),
                read_only: false,
                propagation: BindPropagation::Private,
                create_host_path: true,
            }
        );
    }

    #[test]
    fn inspect_fails_on_malformed_bind_spec() {
        let result: Result<LocalContainer> = inspect_with_binds(vec!["/just-a-source"]).try_into();
        assert!(result.is_err());

        let result: Result<LocalContainer> = inspect_with_binds(vec![":/target"]).try_into();
        assert!(result.is_err());

        let result: Result<LocalContainer> = inspect_with_binds(vec!["/source:"]).try_into();
        assert!(result.is_err());
    }
}
