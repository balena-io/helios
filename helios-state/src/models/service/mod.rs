use mahler::state::{Map, State};

use crate::oci::{ImageConfig, LocalContainer};
use crate::remote_model::Service as RemoteServiceTarget;

use super::image::ImageRef;

mod config;
mod container_name;

pub use config::*;
pub use container_name::*;

/// The worker service status
///
/// Note: there is no Downloading/Downloaded status because
/// that is not necessary for planning and complicates the task
/// definitions.
///
/// The `Downloading` state can be determined by the state
/// report module by checking if status == Installing and
/// image download progress < 100
#[derive(State, Debug, Clone, Default, Eq, PartialEq)]
pub enum ServiceStatus {
    #[default]
    Installing,
    Installed,
}

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Service {
    /// Service ID on the remote backend
    pub id: u32,

    /// Service container ID on the engine
    #[mahler(internal)]
    pub container_id: Option<String>,

    /// The service lifecycle status
    pub status: ServiceStatus,

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
            status,
            ..
        } = svc;
        ServiceTarget {
            id,
            image,
            config,
            status,
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
            // FIXME: this should depend on the running state
            status: ServiceStatus::Installed,
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

        let container_id = container.id.clone();
        let image = ImageRef::Id(container.image_id.clone());
        let config = (img_config, container).into();

        Self {
            id,
            image,
            status: ServiceStatus::Installed,
            container_id: Some(container_id),
            config,
        }
    }
}
