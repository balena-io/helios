use mahler::state::{List, Map, State};
use serde::{Deserialize, Serialize};

use crate::labels::LABEL_SUPERVISED;

const LABEL_IPAM_CONFIG: &str = "io.balena.private.ipam.config";
use crate::oci::{
    NetworkConfig as OciNetworkConfig, NetworkDriver, NetworkIpamConfig, NetworkIpamDriver,
    NetworkIpamPoolConfig,
};
use crate::remote_model::IpamConfig as RemoteIpamConfig;
use crate::remote_model::Network as RemoteNetwork;
use crate::remote_model::NetworkIpam as RemoteNetworkIpam;

#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Network {
    #[serde(default)]
    pub network_name: String,
    pub config: NetworkConfig,
}

impl State for Network {
    type Target = Self;
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NetworkConfig {
    pub driver: NetworkDriver,
    pub driver_opts: Map<String, String>,
    pub enable_ipv6: bool,
    pub internal: bool,
    pub labels: Map<String, String>,
    pub ipam: NetworkIpam,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
            driver: NetworkDriver::default(),
            driver_opts: Map::new(),
            enable_ipv6: false,
            internal: false,
            labels: Map::new(),
            ipam: NetworkIpam {
                driver: NetworkIpamDriver::default(),
                config: Default::default(),
                options: Map::new(),
            },
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NetworkIpam {
    pub driver: NetworkIpamDriver,
    pub config: List<IpamConfig>,
    pub options: Map<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IpamConfig {
    pub subnet: Option<String>,
    pub gateway: Option<String>,
    pub ip_range: Option<String>,
    pub aux_addresses: Option<Map<String, String>>,
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

impl From<RemoteNetwork> for NetworkConfig {
    fn from(net: RemoteNetwork) -> Self {
        NetworkConfig {
            driver: net.driver.map(NetworkDriver::from).unwrap_or_default(),
            driver_opts: net.driver_opts.into_iter().collect(),
            enable_ipv6: net.enable_ipv6,
            internal: net.internal,
            labels: net.labels.into_iter().collect(),
            ipam: net.ipam.into(),
        }
    }
}

impl From<RemoteNetwork> for Network {
    fn from(net: RemoteNetwork) -> Self {
        Network {
            // Placeholder name, filled in during normalization
            network_name: String::new(),
            config: net.into(),
        }
    }
}

impl From<RemoteNetworkIpam> for NetworkIpam {
    fn from(ipam: RemoteNetworkIpam) -> Self {
        NetworkIpam {
            driver: ipam.driver.map(NetworkIpamDriver::from).unwrap_or_default(),
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

impl From<NetworkConfig> for OciNetworkConfig {
    fn from(net: NetworkConfig) -> Self {
        let mut labels: std::collections::HashMap<String, String> =
            net.labels.into_iter().collect();

        // Mark the network as supervised
        labels.insert(LABEL_SUPERVISED.to_string(), "".to_string());

        // Mark networks with IPAM config so changes trigger network recreation
        if !net.ipam.config.is_empty() {
            labels.insert(LABEL_IPAM_CONFIG.to_string(), "true".to_string());
        }

        let ipam = NetworkIpamConfig {
            driver: net.ipam.driver,
            config: net
                .ipam
                .config
                .into_iter()
                .map(|cfg| NetworkIpamPoolConfig {
                    subnet: cfg.subnet,
                    gateway: cfg.gateway,
                    ip_range: cfg.ip_range,
                    aux_addresses: cfg
                        .aux_addresses
                        .map(|a| a.into_iter().collect()),
                })
                .collect(),
            options: net.ipam.options.into_iter().collect(),
        };

        OciNetworkConfig {
            driver: net.driver,
            driver_opts: net.driver_opts.into_iter().collect(),
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
                "config": [{
                    "subnet": "10.0.0.0/8",
                    "gateway": "10.0.0.1",
                    "ip_range": "10.0.1.0/24",
                    "aux_addresses": {"host1": "10.0.0.2"}
                }],
            }
        }))
        .unwrap();

        let config: NetworkConfig = remote.into();
        assert_eq!(config.driver.to_string(), "overlay");
        assert_eq!(config.driver_opts.get("foo"), Some(&"bar".to_string()));
        assert!(config.enable_ipv6);
        assert!(config.internal);
        assert_eq!(
            config.labels.get("com.foo.bar"),
            Some(&"app-label".to_string())
        );
        assert_eq!(config.ipam.driver.to_string(), "custom");
        assert_eq!(config.ipam.config.len(), 1);
        assert_eq!(config.ipam.config[0].subnet, Some("10.0.0.0/8".to_string()));
        assert_eq!(config.ipam.config[0].gateway, Some("10.0.0.1".to_string()));
        assert_eq!(
            config.ipam.config[0].ip_range,
            Some("10.0.1.0/24".to_string())
        );
        assert_eq!(
            config.ipam.config[0]
                .aux_addresses
                .as_ref()
                .unwrap()
                .get("host1"),
            Some(&"10.0.0.2".to_string())
        );
    }

    #[test]
    fn test_network_config_default() {
        let config = NetworkConfig::default();
        assert_eq!(config.driver, NetworkDriver::default());
        assert!(config.driver_opts.is_empty());
        assert!(!config.enable_ipv6);
        assert!(!config.internal);
        assert!(config.labels.is_empty());
        assert_eq!(config.ipam.driver, NetworkIpamDriver::default());
        assert!(config.ipam.config.is_empty());
        assert!(config.ipam.options.is_empty());
    }

    #[test]
    fn test_conversion_defaults_drivers_when_absent() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({})).unwrap();

        let config: NetworkConfig = remote.into();
        assert_eq!(config.driver.to_string(), "bridge");
        assert_eq!(config.ipam.driver.to_string(), "default");
    }

    #[test]
    fn test_to_oci_config_maps_all_fields() {
        let config = NetworkConfig {
            driver: NetworkDriver::from("overlay".to_string()),
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
                driver: NetworkIpamDriver::from("custom".to_string()),
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

        let oci_config: OciNetworkConfig = config.into();

        // Basic fields
        assert_eq!(oci_config.driver.to_string(), "overlay");
        assert_eq!(
            oci_config.driver_opts.get("com.docker.network.driver.mtu"),
            Some(&"1450".to_string())
        );
        assert!(oci_config.enable_ipv6);
        assert!(oci_config.internal);

        // Labels: user labels pass through
        assert_eq!(
            oci_config.labels.get("com.example.label"),
            Some(&"value".to_string())
        );
        // Labels: app-uuid passes through
        assert_eq!(
            oci_config.labels.get("io.balena.app-uuid"),
            Some(&"abc123".to_string())
        );
        // Labels: supervised label is injected
        assert_eq!(
            oci_config.labels.get("io.balena.supervised"),
            Some(&"".to_string())
        );
        // Labels: IPAM config label is injected when config is non-empty
        assert_eq!(
            oci_config.labels.get("io.balena.private.ipam.config"),
            Some(&"true".to_string())
        );

        // IPAM
        assert_eq!(oci_config.ipam.driver.to_string(), "custom");
        assert_eq!(
            oci_config.ipam.options.get("opt1"),
            Some(&"val1".to_string())
        );
        assert_eq!(oci_config.ipam.config.len(), 1);

        let pool = &oci_config.ipam.config[0];
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
    fn test_to_oci_config_omits_ipam_label_when_config_empty() {
        let config = NetworkConfig::default();
        let oci_config: OciNetworkConfig = config.into();
        assert!(
            !oci_config
                .labels
                .contains_key("io.balena.private.ipam.config")
        );
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

        let config: NetworkConfig = remote.into();
        assert_eq!(config.ipam.config.len(), 2);
        assert_eq!(
            config.ipam.config[0].subnet,
            Some("172.28.0.0/16".to_string())
        );
        assert_eq!(
            config.ipam.config[0].gateway,
            Some("172.28.0.1".to_string())
        );
        assert_eq!(config.ipam.config[1].subnet, Some("10.0.0.0/8".to_string()));
        assert_eq!(config.ipam.config[1].gateway, Some("10.0.0.1".to_string()));
    }
}
