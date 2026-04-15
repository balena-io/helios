use serde::{Deserialize, Deserializer};

/// Service `network_mode` as defined by the Compose spec.
///
/// `none` and `host` are recognized explicitly. While not part of the spec, `bridge` is also
/// supported. Other platform specific modes are not supported as they might break.
/// The `service:{name}` mode will be supported once `depends_on` is implemented, and
/// `container:{name}` is not supported — both are rejected on deserialization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkMode {
    None,
    Host,
    Bridge,
    // Service(String), // TODO: requires depends_on support
}

impl<'de> Deserialize<'de> for NetworkMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "none" => Ok(NetworkMode::None),
            "host" => Ok(NetworkMode::Host),
            "bridge" => Ok(NetworkMode::Bridge),
            s if s.starts_with("service:") => Err(serde::de::Error::custom(
                "network_mode `service:{name}` is not yet supported",
            )),
            other => Err(serde::de::Error::custom(format!(
                "network_mode `{other}` is not supported"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_none() {
        let m: NetworkMode = serde_json::from_value(json!("none")).unwrap();
        assert_eq!(m, NetworkMode::None);
    }

    #[test]
    fn parses_host() {
        let m: NetworkMode = serde_json::from_value(json!("host")).unwrap();
        assert_eq!(m, NetworkMode::Host);
    }

    #[test]
    fn parses_bridge() {
        let m: NetworkMode = serde_json::from_value(json!("bridge")).unwrap();
        assert_eq!(m, NetworkMode::Bridge);
    }

    #[test]
    fn rejects_service_prefix() {
        let err = serde_json::from_value::<NetworkMode>(json!("service:db")).unwrap_err();
        assert!(err.to_string().contains("service:"));
    }

    #[test]
    fn rejects_container_prefix() {
        let err = serde_json::from_value::<NetworkMode>(json!("container:foo")).unwrap_err();
        assert!(err.to_string().contains("container:"));
    }

    #[test]
    fn rejects_other_network_modes() {
        let err = serde_json::from_value::<NetworkMode>(json!("my-mode")).unwrap_err();
        assert_eq!(err.to_string(), "network_mode `my-mode` is not supported");
    }
}
