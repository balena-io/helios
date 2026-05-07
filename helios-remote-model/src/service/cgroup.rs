use serde::Deserialize;

/// Service `cgroup` namespace mode as defined by the Compose spec.
///
/// Compose accepts `host` or `private`
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cgroup {
    Host,
    Private,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_host() {
        let c: Cgroup = serde_json::from_value(json!("host")).unwrap();
        assert_eq!(c, Cgroup::Host);
    }

    #[test]
    fn parses_private() {
        let c: Cgroup = serde_json::from_value(json!("private")).unwrap();
        assert_eq!(c, Cgroup::Private);
    }

    #[test]
    fn rejects_unknown_value() {
        let err = serde_json::from_value::<Cgroup>(json!("nonsense")).unwrap_err();
        assert!(err.to_string().contains("unknown variant `nonsense`"));
    }

    #[test]
    fn rejects_empty() {
        let err = serde_json::from_value::<Cgroup>(json!("")).unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }
}
