use mahler::state::State;
use serde::{Deserialize, Serialize};

use crate::labels::LABEL_SERVICE_ID;
use crate::oci::{
    self, ContainerConfig, DateTime, LocalContainer, NetworkMode, NetworkSettings, RestartPolicy,
};
use crate::remote_model::{
    NetworkMode as RemoteNetworkMode, RestartPolicy as RemoteRestartPolicy,
    Service as RemoteServiceTarget,
};

use super::image::ImageRef;

mod config;

pub use config::*;

/// The container runtime status. This is a simplified state over what the container engine returns
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ContainerStatus {
    #[default]
    Created,
    Running,
    Stopping,
    Stopped,
    Dead,
}

impl From<oci::ContainerStatus> for ContainerStatus {
    fn from(value: oci::ContainerStatus) -> Self {
        use oci::ContainerStatus::*;
        match value {
            Created => Self::Created,
            Running => Self::Running,
            Stopped => Self::Stopped,
            Dead => Self::Dead,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub struct Container {
    pub name: String,
    pub created: DateTime,
    pub status: ContainerStatus,
}

impl Container {
    /// A mock container summary to use as part of planning tasks
    pub fn mock() -> Self {
        Self {
            name: String::default(),
            created: DateTime::default(),
            status: ContainerStatus::Created,
        }
    }
}

impl From<(&str, oci::ContainerState)> for Container {
    fn from((container_name, container_state): (&str, oci::ContainerState)) -> Self {
        let container_id = container_name.to_owned();
        let oci::ContainerState {
            status, created, ..
        } = container_state;

        Container {
            name: container_id,
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

    /// Service container state
    #[mahler(internal)]
    pub oci: Option<Container>,

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
            environment,
            composition,
            ..
        } = service;

        // merge the composition labels with the top level service labels
        // giving priority to the latter
        let labels = composition.labels.into_iter().chain(labels).collect();

        // merge the composition environment with the top level service environment
        // giving priority to the latter
        let environment = composition
            .environment
            .into_iter()
            .chain(environment)
            .collect();

        // convert the composition command to a Vec
        let command = composition.command.map(|cmd| cmd.into_iter().collect());

        // convert the restart policy
        let restart_policy = match composition.restart {
            RemoteRestartPolicy::No => RestartPolicy::No,
            RemoteRestartPolicy::Always => RestartPolicy::Always,
            RemoteRestartPolicy::OnFailure { max_retries } => {
                RestartPolicy::OnFailure { max_retries }
            }
            RemoteRestartPolicy::UnlessStopped => RestartPolicy::UnlessStopped,
        };

        // convert the service networks (already priority-sorted from remote model)
        let networks = composition
            .networks
            .into_iter()
            .map(|(name, config)| {
                let endpoint = config
                    .map(|c| NetworkSettings {
                        aliases: c.aliases,
                        ipv4_address: c.ipv4_address,
                        ipv6_address: c.ipv6_address,
                        link_local_ips: c.link_local_ips,
                        mac_address: c.mac_address,
                        driver_opts: c.driver_opts,
                        gw_priority: c.gw_priority.map(|p| p as i64),
                    })
                    .unwrap_or_default();
                (name, endpoint)
            })
            .collect();

        let network_mode = composition.network_mode.map(|m| match m {
            RemoteNetworkMode::None => NetworkMode::None,
            RemoteNetworkMode::Host => NetworkMode::Host,
            RemoteNetworkMode::Bridge => NetworkMode::Other("bridge".to_string()),
        });

        ServiceTarget {
            id,
            image: image.into(),
            started: true,
            config: ServiceConfig(ContainerConfig {
                command,
                environment,
                labels,
                restart_policy,
                networks,
                network_mode,
            }),
        }
    }
}

impl<N> From<LocalContainer<N>> for Service {
    fn from(mut container: LocalContainer<N>) -> Self {
        // Parse the service id from the container labels, assume 0 if no id exists
        let id: u32 = container
            .config
            .labels
            .remove(LABEL_SERVICE_ID)
            .and_then(|id| id.parse().ok())
            .unwrap_or(0);

        let image = ImageRef::Id(container.image.clone());
        let container_summary = Container::from((container.name.as_str(), container.state.clone()));

        // the service is considered started after the engine policy takes over
        // for now this just means that the container status is different than `Created`
        // FIXME: we probably want to handle the host/network manager race condition
        // like we do in https://github.com/balena-os/balena-supervisor/blob/5aa64126ab059505b6456cd9b170a3d609db4b75/src/compose/app.ts#L763-L776
        let started = container_summary.status != ContainerStatus::Created;
        let config = ServiceConfig::from(container.config);

        Self {
            id,
            oci: Some(container_summary),
            image,
            installing: false,
            started,
            config,
        }
    }
}
