use mahler::state::{Map, State};

use crate::remote_model::Release as RemoteReleaseTarget;

use super::network::Network;
use super::service::Service;

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Release {
    /// Indicates if the release has been fully installed
    pub installed: bool,
    pub services: Map<String, Service>,
    pub networks: Map<String, Network>,
}

impl From<Release> for ReleaseTarget {
    fn from(rel: Release) -> Self {
        let Release {
            installed,
            services,
            networks,
        } = rel;
        ReleaseTarget {
            installed,
            services: services
                .into_iter()
                .map(|(svc_name, svc)| (svc_name, svc.into()))
                .collect(),
            networks: networks.into_iter().collect(),
        }
    }
}

impl From<RemoteReleaseTarget> for ReleaseTarget {
    fn from(tgt: RemoteReleaseTarget) -> Self {
        let RemoteReleaseTarget {
            services, networks, ..
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_model;

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
