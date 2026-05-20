use serde::Deserialize;
use serde::de;

/// Docker compose restart policy.
///
/// Supports `no`, `always`, `on-failure[:max-retries]`, and `unless-stopped`.
#[derive(Debug, PartialEq, Eq, Default)]
pub enum RestartPolicy {
    No,
    #[default]
    Always,
    OnFailure {
        max_retries: Option<u32>,
    },
    UnlessStopped,
}

impl<'de> Deserialize<'de> for RestartPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

impl std::str::FromStr for RestartPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "no" => Ok(Self::No),
            "always" => Ok(Self::Always),
            "unless-stopped" => Ok(Self::UnlessStopped),
            _ if s.starts_with("on-failure") => {
                let max_retries = if let Some(rest) = s.strip_prefix("on-failure:") {
                    Some(
                        rest.parse::<u32>()
                            .map_err(|e| format!("invalid max retries in '{s}': {e}"))?,
                    )
                } else if s == "on-failure" {
                    None
                } else {
                    return Err(format!("invalid restart policy: '{s}'"));
                };
                Ok(Self::OnFailure { max_retries })
            }
            _ => Err(format!("invalid restart policy: '{s}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deser_restart(value: serde_json::Value) -> Result<RestartPolicy, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn restart_policy_default_is_always() {
        assert_eq!(RestartPolicy::default(), RestartPolicy::Always);
    }

    #[test]
    fn restart_policy_deserialize_no() {
        let rp = deser_restart(serde_json::json!("no")).unwrap();
        assert_eq!(rp, RestartPolicy::No);
    }

    #[test]
    fn restart_policy_deserialize_always() {
        let rp = deser_restart(serde_json::json!("always")).unwrap();
        assert_eq!(rp, RestartPolicy::Always);
    }

    #[test]
    fn restart_policy_deserialize_on_failure() {
        let rp = deser_restart(serde_json::json!("on-failure")).unwrap();
        assert_eq!(rp, RestartPolicy::OnFailure { max_retries: None });
    }

    #[test]
    fn restart_policy_deserialize_on_failure_with_retries() {
        let rp = deser_restart(serde_json::json!("on-failure:3")).unwrap();
        assert_eq!(
            rp,
            RestartPolicy::OnFailure {
                max_retries: Some(3)
            }
        );
    }

    #[test]
    fn restart_policy_deserialize_unless_stopped() {
        let rp = deser_restart(serde_json::json!("unless-stopped")).unwrap();
        assert_eq!(rp, RestartPolicy::UnlessStopped);
    }

    #[test]
    fn restart_policy_deserialize_invalid() {
        assert!(deser_restart(serde_json::json!("invalid")).is_err());
    }
}
