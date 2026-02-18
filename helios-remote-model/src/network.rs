use serde::Deserialize;
use std::collections::HashMap;

use super::labels::Labels;

/// Target network as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct Network {
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub driver_opts: HashMap<String, String>,
    #[serde(default)]
    pub enable_ipv6: bool,
    #[serde(default)]
    pub internal: bool,
    #[serde(default)]
    pub labels: Labels,
    #[serde(default)]
    pub ipam: NetworkIpam,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct NetworkIpam {
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub config: Vec<IpamConfig>,
    #[serde(default)]
    pub options: HashMap<String, String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct IpamConfig {
    pub subnet: Option<String>,
    pub gateway: Option<String>,
    pub ip_range: Option<String>,
    pub aux_addresses: Option<HashMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_network_defaults_from_empty_object() {
        let net: Network = serde_json::from_value(json!({})).unwrap();
        assert_eq!(net.driver, None);
        assert!(net.driver_opts.is_empty());
        assert!(!net.enable_ipv6);
        assert!(!net.internal);
        assert!(net.labels.is_empty());
        assert_eq!(net.ipam.driver, None);
        assert!(net.ipam.config.is_empty());
        assert!(net.ipam.options.is_empty());
    }

    #[test]
    fn test_network_with_all_fields() {
        let net: Network = serde_json::from_value(json!({
            "driver": "overlay",
            "driver_opts": {"foo": "bar"},
            "enable_ipv6": true,
            "internal": true,
            "labels": {"com.foo.bar": "app-label"},
            "ipam": {
                "driver": "custom",
                "config": [
                    {
                        "subnet": "172.28.0.0/16",
                        "gateway": "172.28.0.1",
                        "ip_range": "172.28.5.0/24",
                        "aux_addresses": {"host1": "172.28.1.5"}
                    }
                ],
                "options": {"baz": "qux"}
            }
        }))
        .unwrap();

        assert_eq!(net.driver, Some("overlay".to_string()));
        assert_eq!(net.driver_opts.get("foo").unwrap(), "bar");
        assert!(net.enable_ipv6);
        assert!(net.internal);
        assert_eq!(net.labels.get("com.foo.bar").unwrap(), "app-label");
        assert_eq!(net.ipam.driver, Some("custom".to_string()));
        assert_eq!(net.ipam.config.len(), 1);
        assert_eq!(net.ipam.config[0].subnet.as_deref(), Some("172.28.0.0/16"));
        assert_eq!(net.ipam.config[0].gateway.as_deref(), Some("172.28.0.1"));
        assert_eq!(
            net.ipam.config[0].ip_range.as_deref(),
            Some("172.28.5.0/24")
        );
        assert_eq!(
            net.ipam.config[0]
                .aux_addresses
                .as_ref()
                .unwrap()
                .get("host1")
                .unwrap(),
            "172.28.1.5"
        );
        assert_eq!(net.ipam.options.get("baz").unwrap(), "qux");
    }

    #[test]
    fn test_network_with_partial_ipam_config() {
        let net: Network = serde_json::from_value(json!({
            "ipam": {
                "config": [{"subnet": "10.0.0.0/8"}]
            }
        }))
        .unwrap();

        assert_eq!(net.ipam.driver, None);
        assert_eq!(net.ipam.config.len(), 1);
        assert_eq!(net.ipam.config[0].subnet.as_deref(), Some("10.0.0.0/8"));
        assert!(net.ipam.config[0].gateway.is_none());
        assert!(net.ipam.config[0].ip_range.is_none());
        assert!(net.ipam.config[0].aux_addresses.is_none());
    }

    #[test]
    fn test_network_labels_from_list_format() {
        let net: Network = serde_json::from_value(json!({
            "labels": ["com.foo.bar=foo", "com.foo.baz=qux"]
        }))
        .unwrap();

        assert_eq!(net.labels.get("com.foo.bar").unwrap(), "foo");
        assert_eq!(net.labels.get("com.foo.baz").unwrap(), "qux");
    }
}
