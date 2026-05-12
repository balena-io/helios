use mahler::state::State;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::labels::LABEL_SERVICE_ID;
use crate::oci::{
    self, BindPropagation, Cgroup, ContainerConfig, DateTime, LocalContainer, Mount, NetworkMode,
    NetworkSettings, RestartPolicy,
};
use crate::remote_model::{
    BindPropagation as RemoteBindPropagation, Cgroup as RemoteCgroup, Mount as RemoteMount,
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

/// Convert a fractional CPU count (compose `cpus`) to nano-CPUs (engine
/// `nano_cpus`). Returns `None` and logs a warning when the value isn't a
/// finite non-negative number, or when the converted value would overflow
/// `i64`. Round to nearest to avoid drift on values like `0.3` whose binary
/// f64 representation is slightly below the rational.
fn cpus_to_nano_cpus(svc_id: u32, cpus: f64) -> Option<i64> {
    if !cpus.is_finite() || cpus < 0.0 {
        warn!(
            svc_id,
            cpus, "ignoring invalid `cpus` value (must be finite and non-negative)"
        );
        return None;
    }
    let nanos = (cpus * 1_000_000_000.0).round();
    if nanos > i64::MAX as f64 {
        warn!(
            svc_id,
            cpus, "ignoring `cpus` value that overflows i64 nano_cpus"
        );
        return None;
    }
    Some(nanos as i64)
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

        // Convert the service mounts. The composition `volumes` arrive already
        // canonicalized (sorted by target) from the remote-model deserializer,
        // so no further sorting is required here.
        let volumes: Vec<Mount> = composition
            .volumes
            .into_iter()
            .map(|m| match m {
                RemoteMount::Volume(v) => Mount::Volume {
                    target: v.target,
                    source: v.source,
                    read_only: v.read_only,
                    nocopy: v.nocopy,
                    subpath: v.subpath,
                },
                RemoteMount::Bind(b) => Mount::Bind {
                    target: b.target,
                    source: b.source,
                    read_only: b.read_only,
                    propagation: b.propagation.map(|p| match p {
                        RemoteBindPropagation::Private => BindPropagation::Private,
                        RemoteBindPropagation::Rprivate => BindPropagation::Rprivate,
                        RemoteBindPropagation::Shared => BindPropagation::Shared,
                        RemoteBindPropagation::Rshared => BindPropagation::Rshared,
                        RemoteBindPropagation::Slave => BindPropagation::Slave,
                        RemoteBindPropagation::Rslave => BindPropagation::Rslave,
                    }),
                },
                RemoteMount::Tmpfs(t) => Mount::Tmpfs {
                    target: t.target,
                    size: t.size,
                    mode: t.mode,
                },
            })
            .collect();

        ServiceTarget {
            id,
            image: image.into(),
            started: true,
            config: ServiceConfig(ContainerConfig {
                cgroup: composition.cgroup.map(|c| match c {
                    RemoteCgroup::Host => Cgroup::Host,
                    RemoteCgroup::Private => Cgroup::Private,
                }),
                cgroup_parent: composition.cgroup_parent,
                command,
                cpuset: composition.cpuset,
                cpu_rt_period: composition.cpu_rt_period,
                cpu_rt_runtime: composition.cpu_rt_runtime,
                cpu_shares: composition.cpu_shares,
                domainname: composition.domainname,
                environment,
                hostname: composition.hostname,
                init: composition.init,
                labels,
                mem_limit: composition.mem_limit,
                mem_reservation: composition.mem_reservation,
                // Compose ships fractional CPU; engine takes nanoseconds-of-CPU-per-second.
                // Drop NaN/Inf/negative/overflow with a warning rather than producing a
                // saturated i64 that the engine would silently accept.
                nano_cpus: composition.cpus.and_then(|c| cpus_to_nano_cpus(id, c)),
                oom_score_adj: composition.oom_score_adj,
                pids_limit: composition.pids_limit,
                privileged: composition.privileged,
                read_only: composition.read_only,
                restart_policy,
                runtime: composition.runtime,
                shm_size: composition.shm_size,
                stop_grace_period: composition.stop_grace_period,
                stop_signal: composition.stop_signal,
                tty: composition.tty,
                user: composition.user,
                userns_mode: composition.userns_mode,
                uts: composition.uts,
                working_dir: composition.working_dir,
                networks,
                network_mode,
                volumes,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpus_to_nano_cpus_rounds_to_nearest() {
        // 0.3 has no exact f64 representation; truncation would yield
        // 299_999_999, rounding gives the user-intended 300_000_000.
        assert_eq!(cpus_to_nano_cpus(1, 0.3), Some(300_000_000));
        assert_eq!(cpus_to_nano_cpus(1, 1.5), Some(1_500_000_000));
        assert_eq!(cpus_to_nano_cpus(1, 2.0), Some(2_000_000_000));
        assert_eq!(cpus_to_nano_cpus(1, 0.0), Some(0));
    }

    #[test]
    fn cpus_to_nano_cpus_rejects_invalid_values() {
        assert_eq!(cpus_to_nano_cpus(1, f64::NAN), None);
        assert_eq!(cpus_to_nano_cpus(1, f64::INFINITY), None);
        assert_eq!(cpus_to_nano_cpus(1, f64::NEG_INFINITY), None);
        assert_eq!(cpus_to_nano_cpus(1, -0.5), None);
    }

    #[test]
    fn cpus_to_nano_cpus_rejects_overflow() {
        // 1e10 cpus * 1e9 ns/cpu = 1e19, which exceeds i64::MAX (~9.2e18).
        assert_eq!(cpus_to_nano_cpus(1, 1e10), None);
    }
}
