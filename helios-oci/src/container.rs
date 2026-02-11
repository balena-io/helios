use std::collections::HashMap;

use bollard::{
    models::ContainerCreateBody,
    query_parameters::{
        CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions,
        RemoveContainerOptions,
    },
    secret::{ContainerInspectResponse, ContainerStateStatusEnum, HealthStatusEnum},
};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

use super::datetime::DateTime;
use super::image::ImageConfig;
use super::util::types::ImageUri;
use super::{Client, Error, Result, WithContext};

#[derive(Debug, Clone)]
pub struct Container<'a>(&'a Client);

impl<'a> Container<'a> {
    pub fn new(client: &'a Client) -> Self {
        Self(client)
    }
}

impl Container<'_> {
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

        let res = self.0.inner().list_containers(Some(opts)).await;

        let container_list = res.map_err(Error::with_context("failed to list containers"))?;

        // find all
        Ok(container_list
            .into_iter()
            .flat_map(|c| c.id.into_iter())
            .collect())
    }

    /// Returns low-level information about a container.
    pub async fn inspect(&self, id: &str) -> Result<LocalContainer> {
        let container_info = self
            .0
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
    pub async fn create(
        &self,
        name: &str,
        image: &ImageUri,
        config: ContainerConfig,
    ) -> Result<String> {
        let options = Some(CreateContainerOptions {
            name: Some(name.to_owned()),
            platform: String::from(""),
        });

        let mut config: ContainerCreateBody = config.into();
        config.image = Some(image.to_string());
        // TODO: add networking and host config which should be passed as arguments to this
        // function

        let res = self
            .0
            .inner()
            .create_container(options, config)
            .await
            .map_err(Error::from)
            .with_context(|| format!("failed to create container {name}"))?;

        Ok(res.id)
    }

    /// Start the container with the given name
    pub async fn start(&self, name: &str) -> Result<()> {
        match self.0.inner().start_container(name, None).await {
            Ok(_) => Ok(()),
            // service already running, ignore
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(e) => {
                Err(Error::from(e)).with_context(|| format!("failed to start container {name}"))
            }
        }
    }

    /// Stop the container with the given name
    pub async fn stop(&self, name: &str) -> Result<()> {
        match self.0.inner().stop_container(name, None).await {
            Ok(_) => Ok(()),
            // service already stopped, ignore
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(e) => {
                Err(Error::from(e)).with_context(|| format!("failed to stop container {name}"))
            }
        }
    }

    /// Create a temporary container from the given image
    ///
    /// This is only meant to get access to the container files and not to be started
    pub async fn create_tmp(&self, name: &str, image: &ImageUri) -> Result<String> {
        self.create(
            name,
            image,
            ContainerConfig {
                cmd: Some(vec!["/bin/false".to_string()]),
                ..Default::default()
            },
        )
        .await
    }

    /// Remove a stopped container
    pub async fn remove(&self, container_name: &str) -> Result<()> {
        if let Err(e) = self
            .0
            .inner()
            .remove_container(container_name, None::<RemoveContainerOptions>)
            .await
        {
            if let bollard::errors::Error::DockerResponseServerError { status_code, .. } = e {
                // do not fail if the container doesn't exist
                if status_code != 404 {
                    return Err(Error::from(e)
                        .context(format!("failed to remove container {container_name}")));
                }
            } else {
                return Err(
                    Error::from(e).context(format!("failed to remove container {container_name}"))
                );
            }
        }
        Ok(())
    }

    /// Reads a container directory contents into an array of bytes using tar representation
    pub async fn read_from(&self, container_name: &str, container_path: &str) -> Result<Vec<u8>> {
        let mut stream = self.0.inner().download_from_container(
            container_name,
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
                        "failed to read {container_path} from container {container_name}"
                    )));
                }
            }
        }

        Ok(archive)
    }
}

// by ref in order to clone only what's necessary to build LocalImage.
impl TryFrom<ContainerInspectResponse> for LocalContainer {
    type Error = Error;

    fn try_from(value: ContainerInspectResponse) -> Result<Self> {
        let id = value.id.ok_or("container ID should not be nil")?;
        let image_id = value.image.ok_or("container image ID should not be nil")?;
        let name = value
            .name
            .ok_or("container name should not be nil")?
            .trim_start_matches('/')
            .to_owned();
        let config = value.config.map(|c| c.into()).unwrap_or_default();

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
            id,
            name,
            image_id,
            config,
            state,
        })
    }
}

/// The container runtime status. This is a simplified state over what the container engine returns
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ContainerStatus {
    #[default]
    Installed,
    Running,
    Stopped,
    Dead,
}

impl From<ContainerStateStatusEnum> for ContainerStatus {
    fn from(value: ContainerStateStatusEnum) -> Self {
        use ContainerStateStatusEnum::*;
        match value {
            EMPTY => ContainerStatus::Installed,
            CREATED => ContainerStatus::Installed,
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
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Container configuration that is portable between hosts
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ContainerConfig {
    /// Command to run specified as an array of strings
    pub cmd: Option<Vec<String>>,

    /// User-defined key/value metadata
    pub labels: Option<HashMap<String, String>>,
}

impl From<ImageConfig> for ContainerConfig {
    fn from(value: ImageConfig) -> Self {
        let ImageConfig { cmd, labels, .. } = value;
        ContainerConfig { cmd, labels }
    }
}

impl From<bollard::config::ContainerConfig> for ContainerConfig {
    fn from(value: bollard::config::ContainerConfig) -> Self {
        let bollard::config::ContainerConfig { cmd, labels, .. } = value;
        ContainerConfig { cmd, labels }
    }
}

impl From<ContainerConfig> for ContainerCreateBody {
    fn from(value: ContainerConfig) -> Self {
        let ContainerConfig { cmd, labels } = value;
        ContainerCreateBody {
            cmd,
            labels,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalContainer {
    /// The engine id of the container
    pub id: String,

    /// The name of the container
    pub name: String,

    /// The content-addressable image id
    pub image_id: String,

    /// User-defined portable configuration
    pub config: ContainerConfig,

    /// The container runtime state
    pub state: ContainerState,
}
