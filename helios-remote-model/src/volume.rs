use serde::{Deserialize, Deserializer};
use std::collections::HashMap;

use super::labels::Labels;

/// Target volume as defined by the remote backend
#[derive(Deserialize, Debug, Default)]
pub struct Volume {
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub driver_opts: HashMap<String, String>,
    #[serde(default)]
    pub labels: Labels,
    /// The volume exists outside the app lifecycle. External volumes
    /// are not allowed as input from the remote, but are created by feature labels.
    #[serde(default, deserialize_with = "reject_external")]
    pub external: bool,
}

fn reject_external<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    if bool::deserialize(deserializer)? {
        return Err(serde::de::Error::custom(
            "external volumes are not supported",
        ));
    }
    Ok(false)
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
        assert!(!vol.external);
    }

    #[test]
    fn test_volume_rejects_external_input() {
        let err = serde_json::from_value::<Volume>(json!({ "external": true })).unwrap_err();
        assert_eq!(err.to_string(), "external volumes are not supported");
    }

    #[test]
    fn test_volume_accepts_non_external_input() {
        let vol: Volume = serde_json::from_value(json!({ "external": false })).unwrap();
        assert!(!vol.external);
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
