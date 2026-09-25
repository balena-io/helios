use std::collections::BTreeMap;
use std::time::Duration;

use mahler::json::Operation;
use mahler::state::{Map, State};
use serde::{Deserializer, Serialize, Serializer};

use crate::logs::mask_sensitive_data;

pub fn deserialize_duration_from_ms<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let ms: u64 = serde::Deserialize::deserialize(deserializer)?;
    Ok(Duration::from_millis(ms))
}

pub fn serialize_duration_to_ms<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u64(duration.as_millis() as u64)
}

/// JSON patch describing the changes needed to go from `current` to `target`
///
/// Sensitive values are masked, as this is meant to be used for diagnostics.
/// Values that cannot be serialized are reported as part of the returned
/// string rather than propagated.
pub fn diff(current: &impl Serialize, target: &impl Serialize) -> String {
    let (Ok(current), Ok(target)) = (serde_json::to_value(current), serde_json::to_value(target))
    else {
        return "<unserializable>".to_string();
    };

    let changes: Vec<String> = json_patch::diff(&current, &target)
        .0
        .into_iter()
        .map(|op| mask_sensitive_data(Operation::from(op)).to_string())
        .collect();

    format!("[{}]", changes.join(", "))
}

/// JSON diff between the current and the target value of every entry in `target`
///
/// Current values are compared using their target representation, so internal
/// state is left out of the diff. Entries missing in `current` show up as
/// additions.
pub fn map_diff<S>(current: &Map<String, S>, target: &Map<String, S::Target>) -> String
where
    S: State + Clone,
    S::Target: From<S>,
{
    let current: BTreeMap<_, S::Target> = target
        .keys()
        .filter_map(|name| current.get(name).map(|s| (name, s.clone().into())))
        .collect();

    diff(&current, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Serialize)]
    struct Config {
        hostname: String,
        environment: BTreeMap<String, String>,
    }

    impl Config {
        fn new(hostname: &str, environment: [(&str, &str); 1]) -> Self {
            Self {
                hostname: hostname.to_string(),
                environment: environment
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }
        }
    }

    #[test]
    fn test_diff_reports_changed_values() {
        let current = Config::new("one", [("LOG_LEVEL", "info")]);
        let target = Config::new("two", [("LOG_LEVEL", "info")]);

        assert_eq!(
            diff(&current, &target),
            r#"[{"op": "update","path": /hostname,"value": "two"}]"#
        );
    }

    #[test]
    fn test_diff_of_equal_values_is_empty() {
        let value = Config::new("one", [("LOG_LEVEL", "info")]);

        assert_eq!(diff(&value, &value), "[]");
    }

    #[test]
    fn test_diff_masks_sensitive_values() {
        let current = json!({"environment": {}});
        let target = json!({"environment": {"API_TOKEN": "hunter2"}});

        let changes = diff(&current, &target);
        assert!(!changes.contains("hunter2"), "{changes}");
        assert!(changes.contains("API_TOKEN"), "{changes}");
    }
}
