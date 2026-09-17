use serde::{Deserialize, Deserializer};

/// Service `network_mode` as defined by the Compose spec.
///
/// `none` and `host` are recognized explicitly. While not part of the spec, `bridge` is also
/// supported. `service:{name}` joins the network namespace of another service in the
/// release, which the release validation checks and turns into an implicit
/// `depends_on` entry. Other platform specific modes are not supported as they might
/// break, and `container:{name}` is rejected, as helios names the containers it manages.
#[derive(Debug, PartialEq)]
pub enum NetworkMode {
    None,
    Host,
    Bridge,
    Service(String),
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
            s => match s.split_once(':') {
                Some(("service", name)) if !name.is_empty() => {
                    Ok(NetworkMode::Service(name.to_owned()))
                }
                Some(("service", _)) => Err(serde::de::Error::custom(
                    "network_mode `service:` is missing a service name",
                )),
                _ => Err(serde::de::Error::custom(format!(
                    "network_mode `{s}` is not supported"
                ))),
            },
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
    fn parses_service() {
        let m: NetworkMode = serde_json::from_value(json!("service:db")).unwrap();
        assert_eq!(m, NetworkMode::Service("db".to_string()));
    }

    #[test]
    fn rejects_service_without_a_name() {
        let err = serde_json::from_value::<NetworkMode>(json!("service:")).unwrap_err();
        assert!(err.to_string().contains("missing a service name"));
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
