use serde::{Deserialize, Deserializer};

/// Service `ipc` mode as defined by the Compose spec.
///
/// `shareable` and `service:{name}` are the modes the spec defines. While not part of the
/// spec, `none` and `host` are also supported. `service:{name}` joins the IPC namespace of another
/// service in the release, which the release validation turns into an implicit `depends_on`.
#[derive(Debug, PartialEq)]
pub enum IpcMode {
    None,
    Host,
    Shareable,
    Service(String),
}

impl<'de> Deserialize<'de> for IpcMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "none" => Ok(IpcMode::None),
            "host" => Ok(IpcMode::Host),
            "shareable" => Ok(IpcMode::Shareable),
            s => match s.split_once(':') {
                Some(("service", name)) if !name.is_empty() => {
                    Ok(IpcMode::Service(name.to_owned()))
                }
                _ => Err(serde::de::Error::custom(format!(
                    "unsupported ipc mode `{s}`"
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
    fn parses_engine_modes() {
        for (raw, mode) in [
            ("none", IpcMode::None),
            ("host", IpcMode::Host),
            ("shareable", IpcMode::Shareable),
            ("service:db", IpcMode::Service("db".to_string())),
        ] {
            let m: IpcMode = serde_json::from_value(json!(raw)).unwrap();
            assert_eq!(m, mode);
        }
    }

    #[test]
    fn rejects_service_without_a_name() {
        let err = serde_json::from_value::<IpcMode>(json!("service:")).unwrap_err();
        assert_eq!(err.to_string(), "unsupported ipc mode `service:`");
    }

    #[test]
    fn rejects_container_prefix() {
        let err = serde_json::from_value::<IpcMode>(json!("container:foo")).unwrap_err();
        assert!(err.to_string().contains("container:"));
    }

    #[test]
    fn rejects_other_ipc_modes() {
        for input in ["private", "my-mode", "xxx:"] {
            let err = serde_json::from_value::<IpcMode>(json!(input)).unwrap_err();
            assert_eq!(err.to_string(), format!("unsupported ipc mode `{input}`"));
        }
    }
}
