use std::ops::{Deref, DerefMut};

use mahler::state::State;
use serde::{Deserialize, Serialize};

use crate::labels::LABEL_SUPERVISED;
use crate::oci::{
    self, LocalNetwork, NetworkDriver, NetworkIpamConfig, NetworkIpamDriver, NetworkIpamPoolConfig,
};
use crate::remote_model::Network as RemoteNetwork;

const LABEL_IPAM_CONFIG: &str = "io.balena.private.ipam.config";
const LABEL_IPAM_OPTS: &str = "io.balena.private.ipam.options";

#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Network {
    #[serde(default)]
    pub network_name: String,
    #[serde(default)]
    pub config: NetworkConfig,
}

impl State for Network {
    type Target = Self;
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct NetworkConfig(oci::NetworkConfig);

impl Deref for NetworkConfig {
    type Target = oci::NetworkConfig;

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
        let mut driver_opts: std::collections::HashMap<String, String> =
            net.driver_opts.into_iter().collect();

        // Absorb `com.docker.network.enable_ipvX` driver opts into top-level
        // fields. The top-level field takes precedence when explicitly set;
        // otherwise the driver opt value is used as a fallback. Discard the
        // driver opts as we can use the top-level fields instead.
        driver_opts.remove("com.docker.network.enable_ipv4");
        let enable_ipv6_opt = driver_opts.remove("com.docker.network.enable_ipv6");
        let enable_ipv6 = net
            .enable_ipv6
            .or_else(|| enable_ipv6_opt.map(|v| v == "true"))
            .unwrap_or(false);

        let ipam = match net.ipam {
            Some(ipam) => NetworkIpamConfig {
                driver: ipam.driver.map(NetworkIpamDriver::from).unwrap_or_default(),
                config: ipam
                    .config
                    .into_iter()
                    .map(|cfg| NetworkIpamPoolConfig {
                        subnet: cfg.subnet,
                        gateway: cfg.gateway,
                        ip_range: cfg.ip_range,
                        aux_addresses: cfg.aux_addresses,
                    })
                    .collect(),
                options: ipam.options.into_iter().collect(),
            },
            None => NetworkIpamConfig::default(),
        };

        NetworkConfig(oci::NetworkConfig {
            driver: net.driver.map(NetworkDriver::from).unwrap_or_default(),
            driver_opts,
            enable_ipv6,
            internal: net.internal,
            labels: net.labels.into_iter().collect(),
            ipam,
        })
    }
}

impl From<RemoteNetwork> for Network {
    fn from(net: RemoteNetwork) -> Self {
        Network {
            // Filled in during normalization
            network_name: String::new(),
            config: net.into(),
        }
    }
}

impl From<LocalNetwork> for Network {
    fn from(net: LocalNetwork) -> Self {
        let network_name = net.name;
        let mut labels = net.labels;
        let mut driver_opts = net.driver_opts;
        let mut ipam_opts = net.ipam.options;

        // Remove labels injected during create that are not part of the
        // compose definition
        labels.remove(LABEL_SUPERVISED);

        // Always strip engine-injected driver_opts. These keys never appear
        // in the target state, so they should never appear in read-back.
        driver_opts.remove("com.docker.network.enable_ipv4");
        driver_opts.remove("com.docker.network.enable_ipv6");

        // Only preserve IPAM pools whose subnet was explicitly configured by the
        // user. Engine-assigned pools are discarded so they don't cause a
        // persistent mismatch against the target.
        let configured_subnets: Vec<String> = labels
            .remove(LABEL_IPAM_CONFIG)
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();

        // Some engine inject their own IPAM options. We write user defined options
        // into a label and check that all those options are present when reading
        // the state
        let ipam_opts_from_label: Vec<String> = labels
            .remove(LABEL_IPAM_OPTS)
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        ipam_opts.retain(|k, _| ipam_opts_from_label.contains(k));

        let ipam = NetworkIpamConfig {
            driver: net.ipam.driver,
            config: net
                .ipam
                .config
                .into_iter()
                .filter(|pool| {
                    pool.subnet
                        .as_ref()
                        .is_some_and(|s| configured_subnets.contains(s))
                })
                .collect(),
            options: ipam_opts,
        };

        Network {
            network_name,
            config: NetworkConfig(oci::NetworkConfig {
                driver: net.driver,
                driver_opts,
                enable_ipv6: net.enable_ipv6,
                internal: net.internal,
                labels,
                ipam,
            }),
        }
    }
}

impl From<NetworkConfig> for oci::NetworkConfig {
    fn from(net: NetworkConfig) -> Self {
        let mut inner = net.0;

        // Mark the network as supervised
        inner
            .labels
            .insert(LABEL_SUPERVISED.to_string(), "".to_string());

        // Serialize user-defined subnets into a label so engine-assigned pools
        // can be filtered out on read-back.
        let subnets_label = inner
            .ipam
            .config
            .iter()
            .filter_map(|p| p.subnet.as_deref())
            .map(|s| serde_json::Value::String(s.to_string()))
            .collect::<serde_json::Value>();
        inner
            .labels
            .insert(LABEL_IPAM_CONFIG.to_string(), subnets_label.to_string());

        // Serialize user defined ipam options into a label. When reading from the network
        // we can make sure to remove options not in the target before comparing
        let opts_label = inner
            .ipam
            .options
            .keys()
            .map(|s| serde_json::Value::String(s.to_string()))
            .collect::<serde_json::Value>();
        inner
            .labels
            .insert(LABEL_IPAM_OPTS.to_string(), opts_label.to_string());

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
    fn test_enable_ipvx_only_driver_opt_absorbed_into_top_level() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({
            "driver_opts": {
                "com.docker.network.enable_ipv4": "false",
                "com.docker.network.enable_ipv6": "true"
            }
        }))
        .unwrap();

        let config: NetworkConfig = remote.into();
        // Driver opts from remote are absorbed into top-level enable_ipvX fields
        assert!(config.enable_ipv6);
        assert!(
            !config
                .driver_opts
                .contains_key("com.docker.network.enable_ipv4")
        );
        assert!(
            !config
                .driver_opts
                .contains_key("com.docker.network.enable_ipv6")
        );
    }

    #[test]
    fn test_enable_ipvx_only_top_level_used_directly() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({
            "enable_ipv6": true,
        }))
        .unwrap();

        let config: NetworkConfig = remote.into();
        assert!(config.enable_ipv6);
    }

    #[test]
    fn test_enable_ipvx_top_level_supersedes_driver_opt() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({
            "enable_ipv6": false,
            "driver_opts": {
                "com.docker.network.enable_ipv4": "false",
                "com.docker.network.enable_ipv6": "true"
            }
        }))
        .unwrap();

        let config: NetworkConfig = remote.into();
        // Top-level enable_ipvX fields take precedence over driver opts
        assert!(!config.enable_ipv6);
        assert!(
            !config
                .driver_opts
                .contains_key("com.docker.network.enable_ipv4")
        );
        assert!(
            !config
                .driver_opts
                .contains_key("com.docker.network.enable_ipv6")
        );
    }

    #[test]
    fn test_enable_ipvx_neither_specified_uses_defaults() {
        let remote: remote_model::Network = serde_json::from_value(serde_json::json!({})).unwrap();

        let config: NetworkConfig = remote.into();
        // Engine defaults to true for enable_ipv4, false for enable_ipv6
        assert!(!config.enable_ipv6);
    }

    #[test]
    fn test_to_oci_config_maps_all_fields() {
        let config = NetworkConfig(oci::NetworkConfig {
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

        let oci_config: oci::NetworkConfig = config.into();

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
        // Labels: IPAM config label is injected with the user-defined subnets
        assert_eq!(
            oci_config.labels.get(LABEL_IPAM_CONFIG),
            Some(&r#"["10.0.0.0/8"]"#.to_string())
        );
        // Labels: IPAM opts label is injected with the option keys
        assert_eq!(
            oci_config.labels.get(LABEL_IPAM_OPTS),
            Some(&r#"["opt1"]"#.to_string())
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
    fn test_from_local_network_strips_injected_labels() {
        let local = LocalNetwork {
            name: "app1_my-net".to_string(),
            driver: NetworkDriver::from("overlay".to_string()),
            driver_opts: [("mtu".to_string(), "1450".to_string())]
                .into_iter()
                .collect(),
            enable_ipv4: true,
            enable_ipv6: true,
            internal: true,
            labels: [
                // injected labels that should be stripped
                (LABEL_SUPERVISED.to_string(), "".to_string()),
                (
                    LABEL_IPAM_CONFIG.to_string(),
                    r#"["10.0.0.0/8"]"#.to_string(),
                ),
                // user label that should survive
                ("com.example.label".to_string(), "value".to_string()),
                (LABEL_IPAM_OPTS.to_string(), r#"["opt1"]"#.to_string()),
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
        assert!(!network.config.labels.contains_key(LABEL_IPAM_OPTS));

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
    fn test_from_local_network_discards_engine_injected_enable_ipv4() {
        let local = LocalNetwork {
            name: "app1_default".to_string(),
            labels: [(LABEL_SUPERVISED.to_string(), "".to_string())]
                .into_iter()
                .collect(),
            driver_opts: [(
                "com.docker.network.enable_ipv4".to_string(),
                "true".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };

        let network: Network = local.into();

        // Engine-injected enable_ipv4 is always stripped as top-level field reflects
        // state accurately
        assert!(network.config.driver_opts.is_empty());
    }

    #[test]
    fn test_from_local_network_preserves_driver_opts_and_filters_engine_enable_ipv4() {
        let local = LocalNetwork {
            name: "app1_my-net".to_string(),
            labels: [(LABEL_SUPERVISED.to_string(), "".to_string())]
                .into_iter()
                .collect(),
            driver_opts: [
                ("mtu".to_string(), "1450".to_string()),
                (
                    "com.docker.network.enable_ipv4".to_string(),
                    "true".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };

        let network: Network = local.into();

        // Non-engine driver_opt is preserved
        assert_eq!(
            network.config.driver_opts.get("mtu"),
            Some(&"1450".to_string())
        );
        // Engine-injected enable_ipv4 is always stripped
        assert!(
            !network
                .config
                .driver_opts
                .contains_key("com.docker.network.enable_ipv4")
        );
        assert_eq!(network.config.driver_opts.len(), 1);
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

    #[test]
    fn test_from_local_network_ipv4_only_strips_engine_ipv6() {
        // User defines IPv4 IPAM, engine appends an IPv6 pool.
        // Only the IPv4 subnet is in the label → IPv6 pool is stripped.
        let local = LocalNetwork {
            name: "app1_net".to_string(),
            enable_ipv6: true,
            labels: [
                (LABEL_SUPERVISED.to_string(), "".to_string()),
                (
                    LABEL_IPAM_CONFIG.to_string(),
                    r#"["172.28.0.0/16"]"#.to_string(),
                ),
            ]
            .into_iter()
            .collect(),
            ipam: NetworkIpamConfig {
                driver: NetworkIpamDriver::default(),
                config: vec![
                    NetworkIpamPoolConfig {
                        subnet: Some("172.28.0.0/16".to_string()),
                        gateway: Some("172.28.0.1".to_string()),
                        ip_range: None,
                        aux_addresses: None,
                    },
                    NetworkIpamPoolConfig {
                        subnet: Some("fd00::/80".to_string()),
                        gateway: Some("fd00::1".to_string()),
                        ip_range: None,
                        aux_addresses: None,
                    },
                ],
                options: Default::default(),
            },
            ..Default::default()
        };

        let network: Network = local.into();
        assert_eq!(network.config.ipam.config.len(), 1);
        assert_eq!(
            network.config.ipam.config[0].subnet,
            Some("172.28.0.0/16".to_string())
        );
    }

    #[test]
    fn test_from_local_network_both_families_keeps_all() {
        // User defines both IPv4 and IPv6 IPAM -> both subnets in label -> both kept
        let local = LocalNetwork {
            name: "app1_net".to_string(),
            enable_ipv6: true,
            labels: [
                (LABEL_SUPERVISED.to_string(), "".to_string()),
                (
                    LABEL_IPAM_CONFIG.to_string(),
                    r#"["172.28.0.0/16","fd00::/80"]"#.to_string(),
                ),
            ]
            .into_iter()
            .collect(),
            ipam: NetworkIpamConfig {
                driver: NetworkIpamDriver::default(),
                config: vec![
                    NetworkIpamPoolConfig {
                        subnet: Some("172.28.0.0/16".to_string()),
                        gateway: Some("172.28.0.1".to_string()),
                        ip_range: None,
                        aux_addresses: None,
                    },
                    NetworkIpamPoolConfig {
                        subnet: Some("fd00::/80".to_string()),
                        gateway: Some("fd00::1".to_string()),
                        ip_range: None,
                        aux_addresses: None,
                    },
                ],
                options: Default::default(),
            },
            ..Default::default()
        };

        let network: Network = local.into();
        assert_eq!(network.config.ipam.config.len(), 2);
        assert_eq!(
            network.config.ipam.config[0].subnet,
            Some("172.28.0.0/16".to_string())
        );
        assert_eq!(
            network.config.ipam.config[1].subnet,
            Some("fd00::/80".to_string())
        );
    }

    #[test]
    fn test_from_local_network_ipv6_only_strips_engine_ipv4() {
        // User defines IPv6 IPAM only, engine prepends an IPv4 pool.
        // Only the IPv6 subnet is in the label → IPv4 pool is stripped.
        let local = LocalNetwork {
            name: "app1_net".to_string(),
            enable_ipv6: true,
            labels: [
                (LABEL_SUPERVISED.to_string(), "".to_string()),
                (
                    LABEL_IPAM_CONFIG.to_string(),
                    r#"["2001:db8::/64"]"#.to_string(),
                ),
            ]
            .into_iter()
            .collect(),
            ipam: NetworkIpamConfig {
                driver: NetworkIpamDriver::default(),
                config: vec![
                    NetworkIpamPoolConfig {
                        subnet: Some("172.18.0.0/16".to_string()),
                        gateway: Some("172.18.0.1".to_string()),
                        ip_range: None,
                        aux_addresses: None,
                    },
                    NetworkIpamPoolConfig {
                        subnet: Some("2001:db8::/64".to_string()),
                        gateway: Some("2001:db8::1".to_string()),
                        ip_range: None,
                        aux_addresses: None,
                    },
                ],
                options: Default::default(),
            },
            ..Default::default()
        };

        let network: Network = local.into();
        assert_eq!(network.config.ipam.config.len(), 1);
        assert_eq!(
            network.config.ipam.config[0].subnet,
            Some("2001:db8::/64".to_string())
        );
    }

    #[test]
    fn test_from_local_network_no_ipam_labels_discards_all_pools() {
        // No user IPAM -> no labels -> all pool families discarded
        let local = LocalNetwork {
            name: "app1_net".to_string(),
            labels: [(LABEL_SUPERVISED.to_string(), "".to_string())]
                .into_iter()
                .collect(),
            ipam: NetworkIpamConfig {
                driver: NetworkIpamDriver::default(),
                config: vec![
                    NetworkIpamPoolConfig {
                        subnet: Some("172.18.0.0/16".to_string()),
                        gateway: Some("172.18.0.1".to_string()),
                        ip_range: None,
                        aux_addresses: None,
                    },
                    NetworkIpamPoolConfig {
                        subnet: Some("fd00::/80".to_string()),
                        gateway: Some("fd00::1".to_string()),
                        ip_range: None,
                        aux_addresses: None,
                    },
                ],
                options: Default::default(),
            },
            ..Default::default()
        };

        let network: Network = local.into();
        assert!(network.config.ipam.config.is_empty());
    }
}
