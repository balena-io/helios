use serde::Deserialize;
use std::collections::HashMap;

use super::labels::Labels;

/// Target volume as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct Volume {
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub driver_opts: HashMap<String, String>,
    #[serde(default)]
    pub labels: Labels,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_volume_defaults_from_empty_object() {
        let vol: Volume = serde_json::from_value(json!({})).unwrap();
        assert_eq!(vol.driver, None);
        assert!(vol.driver_opts.is_empty());
        assert!(vol.labels.is_empty());
    }

    #[test]
    fn test_volume_with_all_fields() {
        let vol: Volume = serde_json::from_value(json!({
            "driver": "local",
            "driver_opts": {
                "o": "bind",
                "type": "none",
                "device": "/tmp/helios"
            },
            "labels": {"com.foo.bar": "app-label"}
        }))
        .unwrap();

        assert_eq!(vol.driver, Some("local".to_string()));
        assert_eq!(vol.driver_opts.get("o").unwrap(), "bind");
        assert_eq!(vol.driver_opts.get("type").unwrap(), "none");
        assert_eq!(vol.driver_opts.get("device").unwrap(), "/tmp/helios");
        assert_eq!(vol.labels.get("com.foo.bar").unwrap(), "app-label");
    }

    #[test]
    fn test_volume_with_driver_only() {
        let vol: Volume = serde_json::from_value(json!({
            "driver": "local"
        }))
        .unwrap();

        assert_eq!(vol.driver, Some("local".to_string()));
        assert!(vol.driver_opts.is_empty());
        assert!(vol.labels.is_empty());
    }

    #[test]
    fn test_volume_labels_from_list_format() {
        let vol: Volume = serde_json::from_value(json!({
            "labels": ["com.foo.bar=foo", "com.foo.baz=qux"]
        }))
        .unwrap();

        assert_eq!(vol.labels.get("com.foo.bar").unwrap(), "foo");
        assert_eq!(vol.labels.get("com.foo.baz").unwrap(), "qux");
    }
}
