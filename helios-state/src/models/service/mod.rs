use mahler::state::{Map, State};
use serde::{Deserialize, Serialize};

use crate::labels::LABEL_SERVICE_ID;
use crate::oci::{ContainerState, ContainerStatus, DateTime, LocalContainer};
use crate::remote_model::Service as RemoteServiceTarget;

use super::image::ImageRef;

mod config;

pub use config::*;

/// The container runtime status. This is a simplified state over what the container engine returns
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ServiceContainerStatus {
    #[default]
    Created,
    Running,
    Stopping,
    Stopped,
    Dead,
}

impl From<ContainerStatus> for ServiceContainerStatus {
    fn from(value: ContainerStatus) -> Self {
        use ContainerStatus::*;
        match value {
            Created => Self::Created,
            Running => Self::Running,
            Stopped => Self::Stopped,
            Dead => Self::Dead,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub struct ServiceContainerSummary {
    pub id: String,
    pub created: DateTime,
    pub status: ServiceContainerStatus,
}

impl ServiceContainerSummary {
    /// A mock container summary to use as part of planning tasks
    pub fn mock() -> Self {
        Self {
            id: String::default(),
            created: DateTime::default(),
            status: ServiceContainerStatus::Created,
        }
    }
}

impl From<(&str, ContainerState)> for ServiceContainerSummary {
    fn from((container_id, container_state): (&str, ContainerState)) -> Self {
        let container_id = container_id.to_owned();
        let ContainerState {
            status, created, ..
        } = container_state;

        ServiceContainerSummary {
            id: container_id,
            status: status.into(),
            created,
        }
    }
}

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Service {
    /// Service ID on the remote backend
    pub id: u32,

    /// Custom service container name
    pub container_name: Option<String>,

    /// Service container state
    #[mahler(internal)]
    pub container: Option<ServiceContainerSummary>,

    /// Flag to indicate that the service container is being
    /// created
    #[mahler(internal, default)]
    pub installing: bool,

    /// Flag to indicate that the service has been started.
    ///
    /// A service is considered started once the restart policy of
    /// the engine takes place, i.e. after the service has successfully started
    /// at least once
    #[mahler(default)]
    pub started: bool,

    /// Service image URI
    pub image: ImageRef,

    /// Service configuration
    #[mahler(default)]
    pub config: ServiceConfig,
}

impl From<Service> for ServiceTarget {
    fn from(svc: Service) -> Self {
        let Service {
            id,
            container_name,
            image,
            config,
            started,
            ..
        } = svc;
        ServiceTarget {
            id,
            container_name,
            image,
            config,
            started,
        }
    }
}

impl From<RemoteServiceTarget> for ServiceTarget {
    fn from(service: RemoteServiceTarget) -> Self {
        let RemoteServiceTarget {
            id,
            image,
            labels,
            composition,
            ..
        } = service;

        // merge the composition labels with the top level service labels
        // giving priority to the latter
        let labels: Map<String, String> = composition.labels.into_iter().chain(labels).collect();

        // convert the composition command to a List
        let command = composition.command.map(|cmd| cmd.into_iter().collect());

        ServiceTarget {
            id,
            // set the name to a random string by default, but a name will be set when transforming
            // from the target state
            container_name: None,
            image: image.into(),
            started: true,
            config: ServiceConfig { command, labels },
        }
    }
}

impl From<LocalContainer> for Service {
    fn from(mut container: LocalContainer) -> Self {
        // Parse the service id from the container labels, assume 0 if no id exists
        let id: u32 = container
            .config
            .labels
            .as_mut()
            .and_then(|value| value.remove(LABEL_SERVICE_ID))
            .and_then(|id| id.parse().ok())
            .unwrap_or(0);

        let image = ImageRef::Id(container.image_id.clone());
        let container_summary =
            ServiceContainerSummary::from((container.id.as_str(), container.state.clone()));

        // the service is considered started after the engine policy takes over
        // for now this just means that the container status is different than `Created`
        // FIXME: we probably want to handle the host/network manager race condition
        // like we do in https://github.com/balena-os/balena-supervisor/blob/5aa64126ab059505b6456cd9b170a3d609db4b75/src/compose/app.ts#L763-L776
        let started = container_summary.status != ServiceContainerStatus::Created;
        let config = ServiceConfig::from(container.config);

        Self {
            id,
            container_name: Some(container.name),
            container: Some(container_summary),
            image,
            installing: false,
            started,
            config,
        }
    }
}
