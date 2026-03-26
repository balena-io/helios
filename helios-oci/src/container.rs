use std::collections::HashMap;

use bollard::{
    config::{
        ContainerInspectResponse, ContainerStateStatusEnum, HealthStatusEnum, RestartPolicyNameEnum,
    },
    models::ContainerCreateBody,
    query_parameters::{
        CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions,
        RemoveContainerOptions, RenameContainerOptions,
    },
};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

use super::datetime::DateTime;
use super::util::types::ImageUri;
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
        let cmd = config.and_then(|c| c.cmd);

        let config = ContainerConfig {
            command: cmd,
            labels,
            restart_policy,
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

/// Container configuration that is portable between hosts
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ContainerConfig {
    /// Command to run specified as an array of strings
    pub command: Option<Vec<String>>,

    /// User-defined key/value metadata
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,

    /// Restart policy for the container
    pub restart_policy: RestartPolicy,
}

impl From<ContainerConfig> for ContainerCreateBody {
    fn from(value: ContainerConfig) -> Self {
        let ContainerConfig {
            command: cmd,
            labels,
            restart_policy,
        } = value;

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

        let host_config = bollard::config::HostConfig {
            restart_policy: Some(restart_policy),
            ..Default::default()
        };

        ContainerCreateBody {
            cmd,
            labels: Some(labels),
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
