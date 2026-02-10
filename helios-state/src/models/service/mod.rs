use mahler::state::{Map, State};
use serde::{Deserialize, Serialize};

use crate::oci::{ContainerState, DateTime, ImageConfig, LocalContainer};
use crate::remote_model::Service as RemoteServiceTarget;

// re-export the container status
pub use crate::oci::ContainerStatus as ServiceContainerStatus;

use super::image::ImageRef;

mod config;
mod container_name;

pub use config::*;
pub use container_name::*;

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
            status: ServiceContainerStatus::Installed,
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
            status,
            created,
        }
    }
}

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Service {
    /// Service ID on the remote backend
    pub id: u32,

    /// Service container state
    #[mahler(internal)]
    pub container: Option<ServiceContainerSummary>,

    /// Flag to indicate that the service has been started.
    ///
    /// A service is considered started once the restart policy of
    /// the engine takes place, i.e. after the service has successfully started
    /// at least once
    pub started: bool,

    /// Service image URI
    pub image: ImageRef,

    /// Service configuration
    pub config: ServiceConfig,
}

impl From<Service> for ServiceTarget {
    fn from(svc: Service) -> Self {
        let Service {
            id,
            image,
            config,
            started,
            ..
        } = svc;
        ServiceTarget {
            id,
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
        let mut labels: Map<String, String> =
            composition.labels.into_iter().chain(labels).collect();

        // add the service id to the container labels
        labels.insert("io.balena.service-id".to_string(), id.to_string());

        // convert the composition command to a List
        let command = composition.command.map(|cmd| cmd.into_iter().collect());

        ServiceTarget {
            id,
            image: image.into(),
            started: true,
            config: ServiceConfig {
                // set the name to None by default, but a name will be set when transforming
                // from the target state
                container_name: None,
                command,
                labels,
            },
        }
    }
}

impl From<(&ImageConfig, LocalContainer)> for Service {
    fn from((img_config, container): (&ImageConfig, LocalContainer)) -> Self {
        // Parse the service id from the container labels, assume 0 if no id exists
        let id: u32 = container
            .config
            .labels
            .as_ref()
            .and_then(|value| value.get("io.balena.service-id"))
            .and_then(|id| id.parse().ok())
            .unwrap_or(0);

        let image = ImageRef::Id(container.image_id.clone());
        let container_summary =
            ServiceContainerSummary::from((container.id.as_str(), container.state.clone()));

        // the service is considered started after the engine policy takes over
        // for now this just means that the container status is different than `Installed`
        // FIXME: we probably want to handle the host/network manager race condition
        // like we do in https://github.com/balena-os/balena-supervisor/blob/5aa64126ab059505b6456cd9b170a3d609db4b75/src/compose/app.ts#L763-L776
        let started = container_summary.status != ServiceContainerStatus::Installed;
        let config = (img_config, container).into();

        Self {
            id,
            container: Some(container_summary),
            image,
            started,
            config,
        }
    }
}
