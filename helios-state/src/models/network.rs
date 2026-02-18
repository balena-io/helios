use std::ops::{Deref, DerefMut};

use mahler::state::State;
use serde::{Deserialize, Serialize};

use crate::labels::LABEL_SUPERVISED;

const LABEL_IPAM_CONFIG: &str = "io.balena.private.ipam.config";
use crate::oci::{
    LocalNetwork, NetworkConfig as OciNetworkConfig, NetworkDriver, NetworkIpamConfig,
    NetworkIpamDriver, NetworkIpamPoolConfig,
};
use crate::remote_model::Network as RemoteNetwork;

#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Network {
    #[serde(default)]
    pub network_name: String,
    pub config: NetworkConfig,
}

impl State for Network {
    type Target = Self;
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct NetworkConfig(OciNetworkConfig);

impl Deref for NetworkConfig {
    type Target = OciNetworkConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for NetworkConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<RemoteNetwork> for NetworkConfig {
    fn from(net: RemoteNetwork) -> Self {
        NetworkConfig(OciNetworkConfig {
            driver: net.driver.map(NetworkDriver::from).unwrap_or_default(),
            driver_opts: net.driver_opts.into_iter().collect(),
            enable_ipv6: net.enable_ipv6,
            internal: net.internal,
            labels: net.labels.into_iter().collect(),
            ipam: NetworkIpamConfig {
                driver: net
                    .ipam
                    .driver
                    .map(NetworkIpamDriver::from)
                    .unwrap_or_default(),
                config: net
                    .ipam
                    .config
                    .into_iter()
                    .map(|cfg| NetworkIpamPoolConfig {
                        subnet: cfg.subnet,
                        gateway: cfg.gateway,
                        ip_range: cfg.ip_range,
                        aux_addresses: cfg.aux_addresses,
                    })
                    .collect(),
                options: net.ipam.options.into_iter().collect(),
            },
        })
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

impl From<LocalNetwork> for Network {
    fn from(net: LocalNetwork) -> Self {
        let network_name = net.name;
        let mut labels = net.labels;

        // Remove labels injected during create that are not part of the
        // compose definition
        labels.remove(LABEL_SUPERVISED);
        let has_ipam_config = labels.remove(LABEL_IPAM_CONFIG).is_some();

        // Only preserve IPAM config if it was explicitly set by the user.
        // Engine-assigned IPAM (subnet, gateway) would cause a mismatch
        // against the target, which has no IPAM config.
        let ipam = if has_ipam_config {
            net.ipam
        } else {
            NetworkIpamConfig {
                driver: net.ipam.driver,
                config: Vec::new(),
                options: net.ipam.options,
            }
        };

        Network {
            network_name,
            config: NetworkConfig(OciNetworkConfig {
                driver: net.driver,
                driver_opts: net.driver_opts,
                enable_ipv6: net.enable_ipv6,
                internal: net.internal,
                labels,
                ipam,
            }),
        }
    }
}

impl From<NetworkConfig> for OciNetworkConfig {
    fn from(net: NetworkConfig) -> Self {
        let mut inner = net.0;

        // Mark the network as supervised
        inner
            .labels
            .insert(LABEL_SUPERVISED.to_string(), "".to_string());

        // Mark networks with IPAM config so changes trigger network recreation
        if !inner.ipam.config.is_empty() {
            inner
                .labels
                .insert(LABEL_IPAM_CONFIG.to_string(), "true".to_string());
        }

        inner
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
        let config = NetworkConfig(OciNetworkConfig {
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
            ipam: NetworkIpamConfig {
                driver: NetworkIpamDriver::from("custom".to_string()),
                config: vec![NetworkIpamPoolConfig {
                    subnet: Some("10.0.0.0/8".to_string()),
                    gateway: Some("10.0.0.1".to_string()),
                    ip_range: Some("10.0.1.0/24".to_string()),
                    aux_addresses: Some(
                        [("host1".to_string(), "10.0.0.2".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                }],
                options: [("opt1".to_string(), "val1".to_string())]
                    .into_iter()
                    .collect(),
            },
        });

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
    fn test_from_local_network_strips_injected_labels() {
        let local = LocalNetwork {
            name: "app1_my-net".to_string(),
            driver: NetworkDriver::from("overlay".to_string()),
            driver_opts: [("mtu".to_string(), "1450".to_string())]
                .into_iter()
                .collect(),
            enable_ipv6: true,
            internal: true,
            labels: [
                // injected labels that should be stripped
                (LABEL_SUPERVISED.to_string(), "".to_string()),
                (LABEL_IPAM_CONFIG.to_string(), "true".to_string()),
                // user label that should survive
                ("com.example.label".to_string(), "value".to_string()),
            ]
            .into_iter()
            .collect(),
            ipam: NetworkIpamConfig {
                driver: NetworkIpamDriver::from("custom".to_string()),
                config: vec![NetworkIpamPoolConfig {
                    subnet: Some("10.0.0.0/8".to_string()),
                    gateway: Some("10.0.0.1".to_string()),
                    ip_range: None,
                    aux_addresses: None,
                }],
                options: [("opt1".to_string(), "val1".to_string())]
                    .into_iter()
                    .collect(),
            },
        };

        let network: Network = local.into();

        // network_name comes from LocalNetwork.name
        assert_eq!(network.network_name, "app1_my-net");

        // Injected labels are stripped
        assert!(!network.config.labels.contains_key(LABEL_SUPERVISED));
        assert!(!network.config.labels.contains_key(LABEL_IPAM_CONFIG));

        // User label survives
        assert_eq!(
            network.config.labels.get("com.example.label"),
            Some(&"value".to_string())
        );

        // All other fields pass through
        assert_eq!(network.config.driver.to_string(), "overlay");
        assert_eq!(
            network.config.driver_opts.get("mtu"),
            Some(&"1450".to_string())
        );
        assert!(network.config.enable_ipv6);
        assert!(network.config.internal);
        assert_eq!(network.config.ipam.driver.to_string(), "custom");
        assert_eq!(network.config.ipam.config.len(), 1);
        assert_eq!(
            network.config.ipam.config[0].subnet,
            Some("10.0.0.0/8".to_string())
        );
        assert_eq!(
            network.config.ipam.options.get("opt1"),
            Some(&"val1".to_string())
        );
    }

    #[test]
    fn test_from_local_network_discards_engine_assigned_ipam() {
        let local = LocalNetwork {
            name: "app1_default".to_string(),
            labels: [(LABEL_SUPERVISED.to_string(), "".to_string())]
                .into_iter()
                .collect(),
            ipam: NetworkIpamConfig {
                driver: NetworkIpamDriver::default(),
                config: vec![NetworkIpamPoolConfig {
                    subnet: Some("172.18.0.0/16".to_string()),
                    gateway: Some("172.18.0.1".to_string()),
                    ip_range: None,
                    aux_addresses: None,
                }],
                options: Default::default(),
            },
            ..Default::default()
        };

        let network: Network = local.into();

        // Without LABEL_IPAM_CONFIG, engine-assigned IPAM config is discarded
        assert!(network.config.ipam.config.is_empty());
        // IPAM driver is still preserved
        assert_eq!(network.config.ipam.driver, NetworkIpamDriver::default());
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
