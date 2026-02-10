use mahler::state::{List, Map, State};
use serde::{Deserialize, Serialize};

use crate::remote_model::IpamConfig as RemoteIpamConfig;
use crate::remote_model::Network as RemoteNetwork;
use crate::remote_model::NetworkIpam as RemoteNetworkIpam;

const DEFAULT_NETWORK_DRIVER: &str = "bridge";
const DEFAULT_IPAM_DRIVER: &str = "default";

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
    pub driver: String,
    pub driver_opts: Map<String, String>,
    pub enable_ipv6: bool,
    pub internal: bool,
    pub labels: Map<String, String>,
    pub ipam: NetworkIpam,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NetworkIpam {
    pub driver: String,
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

impl From<RemoteNetwork> for NetworkConfig {
    fn from(net: RemoteNetwork) -> Self {
        NetworkConfig {
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
        assert_eq!(config.driver, "overlay");
        assert_eq!(config.driver_opts.get("foo"), Some(&"bar".to_string()));
        assert!(config.enable_ipv6);
        assert!(config.internal);
        assert_eq!(
            config.labels.get("com.foo.bar"),
            Some(&"app-label".to_string())
        );
        assert_eq!(config.ipam.driver, "custom");
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
        assert_eq!(config.driver, "bridge");
        assert!(config.driver_opts.is_empty());
        assert!(!config.enable_ipv6);
        assert!(!config.internal);
        assert!(config.labels.is_empty());
        assert_eq!(config.ipam.driver, "default");
        assert!(config.ipam.config.is_empty());
        assert!(config.ipam.options.is_empty());
    }

    #[test]
    fn test_conversion_defaults_drivers_when_absent() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({})).unwrap();

        let config: NetworkConfig = remote.into();
        assert_eq!(config.driver, "bridge");
        assert_eq!(config.ipam.driver, "default");
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
