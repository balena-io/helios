use mahler::state::{List, Map, State};
use serde::{Deserialize, Serialize};

use crate::labels::{LABEL_IPAM_CONFIG, LABEL_SUPERVISED};
use crate::oci::{
    LocalNetwork, NetworkConfig as OciNetworkConfig, NetworkIpamConfig, NetworkIpamPoolConfig,
};
use crate::remote_model::IpamConfig as RemoteIpamConfig;
use crate::remote_model::Network as RemoteNetwork;
use crate::remote_model::NetworkIpam as RemoteNetworkIpam;

const DEFAULT_NETWORK_DRIVER: &str = "bridge";
const DEFAULT_IPAM_DRIVER: &str = "default";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub driver: String,
    pub driver_opts: Map<String, String>,
    pub enable_ipv6: bool,
    pub internal: bool,
    pub labels: Map<String, String>,
    pub ipam: NetworkIpam,
}

impl Default for Network {
    fn default() -> Self {
        Network {
            driver: DEFAULT_NETWORK_DRIVER.to_string(),
            driver_opts: Map::new(),
            enable_ipv6: false,
            internal: false,
            labels: Map::new(),
            ipam: NetworkIpam {
                driver: DEFAULT_IPAM_DRIVER.to_string(),
                config: Default::default(),
                options: Map::new(),
            },
        }
    }
}

impl State for Network {
    type Target = Self;
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NetworkIpam {
    pub driver: String,
    pub config: List<IpamConfig>,
    pub options: Map<String, String>,
}

impl State for NetworkIpam {
    type Target = Self;
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IpamConfig {
    pub subnet: Option<String>,
    pub gateway: Option<String>,
    pub ip_range: Option<String>,
    pub aux_addresses: Option<Map<String, String>>,
}

impl State for IpamConfig {
    type Target = Self;
}

fn ipam_config_from(
    subnet: Option<String>,
    gateway: Option<String>,
    ip_range: Option<String>,
    aux_addresses: Option<impl IntoIterator<Item = (String, String)>>,
) -> IpamConfig {
    IpamConfig {
        subnet,
        gateway,
        ip_range,
        aux_addresses: aux_addresses.map(|a| a.into_iter().collect()),
    }
}

impl From<NetworkIpamPoolConfig> for IpamConfig {
    fn from(pool: NetworkIpamPoolConfig) -> Self {
        ipam_config_from(pool.subnet, pool.gateway, pool.ip_range, pool.aux_addresses)
    }
}

impl From<RemoteNetwork> for Network {
    fn from(net: RemoteNetwork) -> Self {
        Network {
            driver: net
                .driver
                .unwrap_or_else(|| DEFAULT_NETWORK_DRIVER.to_string()),
            driver_opts: net.driver_opts.into_iter().collect(),
            enable_ipv6: net.enable_ipv6,
            internal: net.internal,
            labels: net.labels.into_iter().collect(),
            ipam: net.ipam.into(),
        }
    }
}

impl From<LocalNetwork> for Network {
    fn from(net: LocalNetwork) -> Self {
        let mut labels: Map<String, String> = net.labels.into_iter().collect();

        // Remove labels injected during create that are not part of the
        // compose definition
        labels.remove(LABEL_SUPERVISED);
        labels.remove(LABEL_IPAM_CONFIG);

        Network {
            driver: net.driver.to_string(),
            driver_opts: net.driver_opts.into_iter().collect(),
            enable_ipv6: net.enable_ipv6,
            internal: net.internal,
            labels,
            ipam: net.ipam.into(),
        }
    }
}

impl From<NetworkIpamConfig> for NetworkIpam {
    fn from(ipam: NetworkIpamConfig) -> Self {
        NetworkIpam {
            driver: ipam.driver.to_string(),
            config: ipam.config.into_iter().map(Into::into).collect(),
            options: ipam.options.into_iter().collect(),
        }
    }
}

impl From<RemoteNetworkIpam> for NetworkIpam {
    fn from(ipam: RemoteNetworkIpam) -> Self {
        NetworkIpam {
            driver: ipam
                .driver
                .unwrap_or_else(|| DEFAULT_IPAM_DRIVER.to_string()),
            config: ipam.config.into_iter().map(Into::into).collect(),
            options: ipam.options.into_iter().collect(),
        }
    }
}

impl From<RemoteIpamConfig> for IpamConfig {
    fn from(cfg: RemoteIpamConfig) -> Self {
        ipam_config_from(cfg.subnet, cfg.gateway, cfg.ip_range, cfg.aux_addresses)
    }
}

impl From<&Network> for OciNetworkConfig {
    fn from(net: &Network) -> Self {
        let mut labels: std::collections::HashMap<String, String> =
            net.labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        // Mark the network as supervised
        labels.insert(LABEL_SUPERVISED.to_string(), "".to_string());

        // Mark networks with IPAM config so changes trigger network recreation
        if !net.ipam.config.is_empty() {
            labels.insert(LABEL_IPAM_CONFIG.to_string(), "true".to_string());
        }

        let ipam = NetworkIpamConfig {
            driver: net.ipam.driver.clone().into(),
            config: net
                .ipam
                .config
                .iter()
                .map(|cfg| NetworkIpamPoolConfig {
                    subnet: cfg.subnet.clone(),
                    gateway: cfg.gateway.clone(),
                    ip_range: cfg.ip_range.clone(),
                    aux_addresses: cfg
                        .aux_addresses
                        .as_ref()
                        .map(|a| a.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
                })
                .collect(),
            options: net.ipam.options.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        };

        OciNetworkConfig {
            driver: net.driver.clone().into(),
            driver_opts: net.driver_opts.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            enable_ipv6: net.enable_ipv6,
            internal: net.internal,
            labels,
            ipam,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_model;

    #[test]
    fn test_conversion_preserves_all_fields() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({
            "driver": "overlay",
            "driver_opts": {"foo": "bar"},
            "enable_ipv6": true,
            "internal": true,
            "labels": {"com.foo.bar": "app-label"},
            "ipam": {
                "driver": "custom",
                "config": [{"subnet": "10.0.0.0/8", "gateway": "10.0.0.1"}],
            }
        }))
        .unwrap();

        let net: Network = remote.into();
        assert_eq!(net.driver, "overlay");
        assert_eq!(net.driver_opts.get("foo"), Some(&"bar".to_string()));
        assert!(net.enable_ipv6);
        assert!(net.internal);
        assert_eq!(
            net.labels.get("com.foo.bar"),
            Some(&"app-label".to_string())
        );
        assert_eq!(net.ipam.driver, "custom");
        assert_eq!(net.ipam.config.len(), 1);
        assert_eq!(net.ipam.config[0].subnet, Some("10.0.0.0/8".to_string()));
        assert_eq!(net.ipam.config[0].gateway, Some("10.0.0.1".to_string()));
    }

    #[test]
    fn test_conversion_defaults_drivers_when_absent() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({})).unwrap();

        let net: Network = remote.into();
        assert_eq!(net.driver, "bridge");
        assert_eq!(net.ipam.driver, "default");
    }

    #[test]
    fn test_to_oci_config_maps_all_fields() {
        let net = Network {
            driver: "overlay".to_string(),
            driver_opts: [(
                "com.docker.network.driver.mtu".to_string(),
                "1450".to_string(),
            )]
            .into_iter()
            .collect(),
            enable_ipv6: true,
            internal: true,
            labels: [
                ("com.example.label".to_string(), "value".to_string()),
                ("io.balena.app-uuid".to_string(), "abc123".to_string()),
            ]
            .into_iter()
            .collect(),
            ipam: NetworkIpam {
                driver: "custom".to_string(),
                config: vec![IpamConfig {
                    subnet: Some("10.0.0.0/8".to_string()),
                    gateway: Some("10.0.0.1".to_string()),
                    ip_range: Some("10.0.1.0/24".to_string()),
                    aux_addresses: Some(
                        [("host1".to_string(), "10.0.0.2".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                }]
                .into_iter()
                .collect(),
                options: [("opt1".to_string(), "val1".to_string())]
                    .into_iter()
                    .collect(),
            },
        };

        let config: OciNetworkConfig = (&net).into();

        // Basic fields
        assert_eq!(config.driver.to_string(), "overlay");
        assert_eq!(
            config.driver_opts.get("com.docker.network.driver.mtu"),
            Some(&"1450".to_string())
        );
        assert!(config.enable_ipv6);
        assert!(config.internal);

        // Labels: user labels pass through
        assert_eq!(
            config.labels.get("com.example.label"),
            Some(&"value".to_string())
        );
        // Labels: app-uuid passes through
        assert_eq!(
            config.labels.get("io.balena.app-uuid"),
            Some(&"abc123".to_string())
        );
        // Labels: supervised label is injected
        assert_eq!(
            config.labels.get("io.balena.supervised"),
            Some(&"".to_string())
        );
        // Labels: IPAM config label is injected when config is non-empty
        assert_eq!(
            config.labels.get("io.balena.private.ipam.config"),
            Some(&"true".to_string())
        );

        // IPAM
        assert_eq!(config.ipam.driver.to_string(), "custom");
        assert_eq!(config.ipam.options.get("opt1"), Some(&"val1".to_string()));
        assert_eq!(config.ipam.config.len(), 1);

        let pool = &config.ipam.config[0];
        assert_eq!(pool.subnet, Some("10.0.0.0/8".to_string()));
        assert_eq!(pool.gateway, Some("10.0.0.1".to_string()));
        assert_eq!(pool.ip_range, Some("10.0.1.0/24".to_string()));
        let aux = pool
            .aux_addresses
            .as_ref()
            .expect("aux_addresses should be set");
        assert_eq!(aux.get("host1"), Some(&"10.0.0.2".to_string()));
    }

    #[test]
    fn test_conversion_with_multiple_ipam_configs() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({
            "ipam": {
                "config": [
                    {"subnet": "172.28.0.0/16", "gateway": "172.28.0.1"},
                    {"subnet": "10.0.0.0/8", "gateway": "10.0.0.1"}
                ]
            }
        }))
        .unwrap();

        let net: Network = remote.into();
        assert_eq!(net.ipam.config.len(), 2);
        assert_eq!(net.ipam.config[0].subnet, Some("172.28.0.0/16".to_string()));
        assert_eq!(net.ipam.config[0].gateway, Some("172.28.0.1".to_string()));
        assert_eq!(net.ipam.config[1].subnet, Some("10.0.0.0/8".to_string()));
        assert_eq!(net.ipam.config[1].gateway, Some("10.0.0.1".to_string()));
    }
}
