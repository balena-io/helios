use std::ops::Deref;
use std::{collections::HashMap, ops::DerefMut};

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};

/// Per-network endpoint configuration for a service (without priority, which is consumed during
/// deserialization to determine insertion order)
#[derive(Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct NetworkSettings {
    #[serde(default)]
    pub aliases: Vec<String>,

    #[serde(default)]
    pub ipv4_address: Option<String>,

    #[serde(default)]
    pub ipv6_address: Option<String>,

    #[serde(default)]
    pub link_local_ips: Vec<String>,

    #[serde(default)]
    pub mac_address: Option<String>,

    #[serde(default)]
    pub driver_opts: HashMap<String, String>,

    #[serde(default)]
    pub gw_priority: Option<i32>,
}

/// Intermediate type that includes `priority` for sorting during deserialization
#[derive(Deserialize)]
struct NetworkSettingsWithPriority {
    #[serde(default)]
    aliases: Vec<String>,

    #[serde(default)]
    ipv4_address: Option<String>,

    #[serde(default)]
    ipv6_address: Option<String>,

    #[serde(default)]
    link_local_ips: Vec<String>,

    #[serde(default)]
    mac_address: Option<String>,

    #[serde(default)]
    driver_opts: HashMap<String, String>,

    #[serde(default)]
    gw_priority: Option<i32>,

    #[serde(default)]
    priority: i32,
}

impl NetworkSettingsWithPriority {
    fn into_config(self) -> NetworkSettings {
        NetworkSettings {
            aliases: self.aliases,
            ipv4_address: self.ipv4_address,
            ipv6_address: self.ipv6_address,
            link_local_ips: self.link_local_ips,
            mac_address: self.mac_address,
            driver_opts: self.driver_opts,
            gw_priority: self.gw_priority,
        }
    }
}

/// Ordered map of network name to optional endpoint configuration.
///
/// `None` means "connect with default settings". Entries are ordered by descending `priority`
/// (highest priority first), which determines the order in which the container connects to
/// networks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkingConfig(IndexMap<String, Option<NetworkSettings>>);

impl Deref for NetworkingConfig {
    type Target = IndexMap<String, Option<NetworkSettings>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for NetworkingConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for NetworkingConfig {
    type Item = (String, Option<NetworkSettings>);
    type IntoIter = indexmap::map::IntoIter<String, Option<NetworkSettings>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'de> Deserialize<'de> for NetworkingConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NetworksVisitor;

        impl<'de> serde::de::Visitor<'de> for NetworksVisitor {
            type Value = Vec<(String, Option<NetworkSettings>, i32)>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a list of network names or a map of network name to optional settings",
                )
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut entries = Vec::new();
                while let Some(name) = seq.next_element::<String>()? {
                    entries.push((name, None, 0));
                }
                Ok(entries)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut entries = Vec::new();
                while let Some(name) = map.next_key::<String>()? {
                    match map.next_value::<Option<NetworkSettingsWithPriority>>()? {
                        Some(r) => {
                            let priority = r.priority;
                            entries.push((name, Some(r.into_config()), priority));
                        }
                        None => entries.push((name, None, 0)),
                    }
                }
                Ok(entries)
            }
        }

        let mut entries = deserializer.deserialize_any(NetworksVisitor)?;

        // Sort by priority descending (highest first); stable sort preserves original
        // order for entries with equal priority
        entries.sort_by(|a, b| b.2.cmp(&a.2));

        let map = entries
            .into_iter()
            .map(|(name, config, _)| (name, config))
            .collect();

        Ok(NetworkingConfig(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn networks_from_list() {
        let networks: NetworkingConfig = serde_json::from_value(json!(["net1", "net2"])).unwrap();

        assert_eq!(networks.len(), 2);
        assert_eq!(networks.get_index(0).unwrap().0, "net1");
        assert_eq!(networks.get_index(0).unwrap().1, &None);
        assert_eq!(networks.get_index(1).unwrap().0, "net2");
        assert_eq!(networks.get_index(1).unwrap().1, &None);
    }

    #[test]
    fn networks_from_map_with_settings() {
        let networks: NetworkingConfig = serde_json::from_value(json!({
            "net1": {
                "aliases": ["alias1"],
                "ipv4_address": "172.16.238.10"
            },
            "net2": null
        }))
        .unwrap();

        assert_eq!(networks.len(), 2);

        let (_, net1_config) = networks.get_index(0).unwrap();
        let config = net1_config.as_ref().unwrap();
        assert_eq!(config.aliases, vec!["alias1".to_string()]);
        assert_eq!(config.ipv4_address, Some("172.16.238.10".to_string()));

        let (_, net2_config) = networks.get_index(1).unwrap();
        assert!(net2_config.is_none());
    }

    #[test]
    fn networks_from_map_with_null_values() {
        let networks: NetworkingConfig = serde_json::from_value(json!({
            "net1": null,
            "net2": null
        }))
        .unwrap();

        assert_eq!(networks.len(), 2);
        assert!(networks["net1"].is_none());
        assert!(networks["net2"].is_none());
    }

    #[test]
    fn networks_empty_default() {
        let networks: NetworkingConfig = serde_json::from_value(json!({})).unwrap();
        assert!(networks.is_empty());
    }

    #[test]
    fn networks_sorted_by_priority_descending() {
        let networks: NetworkingConfig = serde_json::from_value(json!({
            "low": { "priority": 0 },
            "high": { "priority": 1000 },
            "mid": { "priority": 100 }
        }))
        .unwrap();

        let names: Vec<&String> = networks.keys().collect();
        assert_eq!(names, vec!["high", "mid", "low"]);
    }

    #[test]
    fn networks_priority_not_stored() {
        let networks: NetworkingConfig = serde_json::from_value(json!({
            "net1": { "priority": 100, "aliases": ["a"] }
        }))
        .unwrap();

        // ServiceNetworkConfig has no priority field - it was consumed during deserialization
        let config = networks["net1"].as_ref().unwrap();
        assert_eq!(config.aliases, vec!["a".to_string()]);
    }

    #[test]
    fn networks_preserves_order_for_equal_priority() {
        let networks: NetworkingConfig = serde_json::from_value(json!({
            "alpha": {},
            "beta": {},
            "gamma": {}
        }))
        .unwrap();

        // All have default priority 0, original insertion order preserved
        let names: Vec<&String> = networks.keys().collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn networks_with_all_endpoint_fields() {
        let networks: NetworkingConfig = serde_json::from_value(json!({
            "mynet": {
                "aliases": ["db", "database"],
                "ipv4_address": "172.16.238.10",
                "ipv6_address": "2001:3984:3989::10",
                "link_local_ips": ["57.123.22.11"],
                "mac_address": "02:42:ac:11:65:43",
                "driver_opts": { "foo": "bar" },
                "gw_priority": 100
            }
        }))
        .unwrap();

        let config = networks["mynet"].as_ref().unwrap();
        assert_eq!(config.aliases, vec!["db", "database"]);
        assert_eq!(config.ipv4_address, Some("172.16.238.10".to_string()));
        assert_eq!(config.ipv6_address, Some("2001:3984:3989::10".to_string()));
        assert_eq!(config.link_local_ips, vec!["57.123.22.11"]);
        assert_eq!(config.mac_address, Some("02:42:ac:11:65:43".to_string()));
        assert_eq!(config.driver_opts.get("foo"), Some(&"bar".to_string()));
        assert_eq!(config.gw_priority, Some(100));
    }

    #[test]
    fn networks_list_format_defaults() {
        let networks: NetworkingConfig = serde_json::from_value(json!(["default"])).unwrap();

        assert_eq!(networks.len(), 1);
        assert!(networks["default"].is_none());
    }

    #[test]
    fn networks_map_with_empty_object() {
        let networks: NetworkingConfig = serde_json::from_value(json!({
            "net1": {}
        }))
        .unwrap();

        let config = networks["net1"].as_ref().unwrap();
        assert_eq!(*config, NetworkSettings::default());
    }
}
