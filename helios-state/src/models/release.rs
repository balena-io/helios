use mahler::state::{Map, State};

use crate::remote_model::Release as RemoteReleaseTarget;

use super::network::Network;
use super::service::Service;
use super::volume::Volume;

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Release {
    /// Indicates if the release has been fully installed
    pub installed: bool,
    pub services: Map<String, Service>,
    pub networks: Map<String, Network>,
    pub volumes: Map<String, Volume>,
}

impl From<Release> for ReleaseTarget {
    fn from(rel: Release) -> Self {
        let Release {
            installed,
            services,
            networks,
            volumes,
        } = rel;
        ReleaseTarget {
            installed,
            services: services
                .into_iter()
                .map(|(svc_name, svc)| (svc_name, svc.into()))
                .collect(),
            networks: networks.into_iter().collect(),
            volumes: volumes.into_iter().collect(),
        }
    }
}

impl From<RemoteReleaseTarget> for ReleaseTarget {
    fn from(tgt: RemoteReleaseTarget) -> Self {
        let RemoteReleaseTarget {
            services,
            networks,
            volumes,
            ..
        } = tgt;
        ReleaseTarget {
            installed: true,
            services: services
                .into_iter()
                .map(|(svc_name, svc)| (svc_name, svc.into()))
                .collect(),
            networks: networks
                .into_iter()
                .map(|(name, net)| (name, net.into()))
                .collect(),
            volumes: volumes
                .into_iter()
                .map(|(name, vol)| (name, vol.into()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_model;

    #[test]
    fn test_remote_release_conversion_includes_volumes() {
        let remote: remote_model::Release = serde_json::from_value(serde_json::json!({
            "volumes": {
                "my-volume": {
                    "driver": "local",
                    "driver_opts": {
                        "o": "bind",
                        "type": "none",
                        "device": "/tmp/helios"
                    }
                }
            }
        }))
        .unwrap();

        let target: ReleaseTarget = remote.into();
        assert!(target.installed);
        assert!(target.volumes.contains_key("my-volume"));
        let vol = target.volumes.get("my-volume").unwrap();
        assert_eq!(vol.config.driver.to_string(), "local");
        assert_eq!(vol.config.driver_opts.get("o"), Some(&"bind".to_string()));
    }

    #[test]
    fn test_remote_release_conversion_includes_networks() {
        let remote: remote_model::Release = serde_json::from_value(serde_json::json!({
            "networks": {
                "my-network": {
                    "driver": "overlay",
                    "ipam": {
                        "config": [{"subnet": "10.0.0.0/8"}]
                    }
                }
            }
        }))
        .unwrap();

        let target: ReleaseTarget = remote.into();
        assert!(target.installed);
        assert!(target.networks.contains_key("my-network"));
        let net = target.networks.get("my-network").unwrap();
        assert_eq!(net.config.driver.to_string(), "overlay");
        assert_eq!(net.config.ipam.config.len(), 1);
        assert_eq!(
            net.config.ipam.config[0].subnet,
            Some("10.0.0.0/8".to_string())
        );
    }
}
