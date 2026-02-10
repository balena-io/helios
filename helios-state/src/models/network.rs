use mahler::state::{List, Map, State};
use serde::{Deserialize, Serialize};

use crate::labels::LABEL_IPAM_CONFIG;
use crate::remote_model::IpamConfig as RemoteIpamConfig;
use crate::remote_model::Network as RemoteNetwork;
use crate::remote_model::NetworkIpam as RemoteNetworkIpam;
use crate::util::network::{DEFAULT_IPAM_DRIVER, DEFAULT_NETWORK_DRIVER};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub driver: String,
    pub driver_opts: Map<String, String>,
    pub enable_ipv6: bool,
    pub internal: bool,
    pub labels: Map<String, String>,
    pub config_only: bool,
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
            config_only: false,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

impl From<RemoteNetwork> for Network {
    fn from(net: RemoteNetwork) -> Self {
        let mut labels: Map<String, String> = net.labels.into_iter().collect();

        // Mark networks with IPAM config so changes trigger network recreation
        if !net.ipam.config.is_empty() {
            labels.insert(LABEL_IPAM_CONFIG.to_string(), "true".to_string());
        }

        Network {
            driver: net.driver,
            driver_opts: net.driver_opts.into_iter().collect(),
            enable_ipv6: net.enable_ipv6,
            internal: net.internal,
            labels,
            config_only: net.config_only,
            ipam: net.ipam.into(),
        }
    }
}

impl From<RemoteNetworkIpam> for NetworkIpam {
    fn from(ipam: RemoteNetworkIpam) -> Self {
        NetworkIpam {
            driver: ipam.driver,
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
    fn test_conversion_adds_ipam_label_when_config_present() {
        let remote = remote_model::Network {
            driver: "bridge".to_string(),
            driver_opts: Default::default(),
            enable_ipv6: false,
            internal: false,
            labels: Default::default(),
            config_only: false,
            ipam: remote_model::NetworkIpam {
                driver: "default".to_string(),
                config: vec![remote_model::IpamConfig {
                    subnet: Some("172.28.0.0/16".to_string()),
                    ..Default::default()
                }],
                options: Default::default(),
            },
        };

        let net: Network = remote.into();
        assert_eq!(
            net.labels.get("io.balena.private.ipam.config"),
            Some(&"true".to_string()),
        );
    }

    #[test]
    fn test_conversion_no_ipam_label_when_config_empty() {
        let remote = remote_model::Network {
            driver: "bridge".to_string(),
            driver_opts: Default::default(),
            enable_ipv6: false,
            internal: false,
            labels: Default::default(),
            config_only: false,
            ipam: Default::default(),
        };

        let net: Network = remote.into();
        assert_eq!(net.labels.get("io.balena.private.ipam.config"), None);
    }

    #[test]
    fn test_conversion_preserves_all_fields() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({
            "driver": "overlay",
            "driver_opts": {"foo": "bar"},
            "enable_ipv6": true,
            "internal": true,
            "labels": {"com.foo.bar": "app-label"},
            "config_only": true,
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
        assert!(net.config_only);
        assert_eq!(net.ipam.driver, "custom");
        assert_eq!(net.ipam.config.len(), 1);
        assert_eq!(net.ipam.config[0].subnet, Some("10.0.0.0/8".to_string()));
        assert_eq!(net.ipam.config[0].gateway, Some("10.0.0.1".to_string()));
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
