use serde::{Deserialize, Deserializer};

/// Service `pid` mode.
///
/// The Compose spec leaves the supported values platform specific. `host` and
/// `service:{name}` are supported. `service:{name}` joins the PID namespace of another
/// service in the release, which the release validation turns into an implicit `depends_on`.
#[derive(Debug, PartialEq)]
pub enum PidMode {
    Host,
    Service(String),
}

impl<'de> Deserialize<'de> for PidMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "host" => Ok(PidMode::Host),
            s => match s.split_once(':') {
                Some(("service", name)) if !name.is_empty() => {
                    Ok(PidMode::Service(name.to_owned()))
                }
                _ => Err(serde::de::Error::custom(format!(
                    "unsupported pid mode `{s}`"
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
    fn parses_supported_modes() {
        for (raw, mode) in [
            ("host", PidMode::Host),
            ("service:db", PidMode::Service("db".to_string())),
        ] {
            let m: PidMode = serde_json::from_value(json!(raw)).unwrap();
            assert_eq!(m, mode);
        }
    }

    #[test]
    fn rejects_service_without_a_name() {
        let err = serde_json::from_value::<PidMode>(json!("service:")).unwrap_err();
        assert_eq!(err.to_string(), "unsupported pid mode `service:`");
    }

    #[test]
    fn rejects_container_prefix() {
        let err = serde_json::from_value::<PidMode>(json!("container:foo")).unwrap_err();
        assert!(err.to_string().contains("container:"));
    }

    #[test]
    fn rejects_other_pid_modes() {
        for input in ["private", "my-mode"] {
            let err = serde_json::from_value::<PidMode>(json!(input)).unwrap_err();
            assert_eq!(err.to_string(), format!("unsupported pid mode `{input}`"));
        }
    }
}
