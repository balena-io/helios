use crate::duration::DurationNanos;
use serde::{Deserialize, Deserializer};

/// Service healthcheck as delivered by the remote backend. Durations are in
/// nanoseconds to match the engine API.
///
/// The `test` field accepts Compose-style shapes:
/// - a bare string (interpreted as `CMD-SHELL`),
/// - a list starting with `CMD` / `CMD-SHELL` / `NONE`,
///   - if a list not starting with `CMD` / `CMD-SHELL` / `NONE`,  
///     it's valid but is treated as exec args by the Engine
/// - or the special `disable: true` flag, which produces `["NONE"]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Healthcheck {
    pub test: Option<Vec<String>>,
    pub interval: Option<DurationNanos>,
    pub timeout: Option<DurationNanos>,
    pub start_period: Option<DurationNanos>,
    pub start_interval: Option<DurationNanos>,
    pub retries: Option<i64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Test {
    String(String),
    List(Vec<String>),
}

#[derive(Deserialize, Default)]
struct RawHealthcheck {
    test: Option<Test>,
    #[serde(default)]
    interval: Option<DurationNanos>,
    #[serde(default)]
    timeout: Option<DurationNanos>,
    #[serde(default)]
    start_period: Option<DurationNanos>,
    #[serde(default)]
    start_interval: Option<DurationNanos>,
    retries: Option<i64>,
    #[serde(default)]
    disable: bool,
}

impl<'de> Deserialize<'de> for Healthcheck {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawHealthcheck::deserialize(deserializer)?;

        // `disable: true` is the Compose shorthand for "explicitly turn off
        // the image's HEALTHCHECK". It maps to the engine's `["NONE"]` test
        // and takes precedence over any `test` value also provided.
        let test = if raw.disable {
            Some(vec!["NONE".to_string()])
        } else {
            match raw.test {
                None => None,
                Some(Test::String(s)) => Some(vec!["CMD-SHELL".to_string(), s]),
                Some(Test::List(list)) => (!list.is_empty()).then_some(list),
            }
        };

        Ok(Healthcheck {
            test,
            interval: raw.interval,
            timeout: raw.timeout,
            start_period: raw.start_period,
            start_interval: raw.start_interval,
            retries: raw.retries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_as_string_becomes_cmd_shell() {
        let hc: Healthcheck =
            serde_json::from_value(json!({"test": "curl -f http://localhost"})).unwrap();
        assert_eq!(
            hc.test.as_deref(),
            Some(
                &[
                    "CMD-SHELL".to_string(),
                    "curl -f http://localhost".to_string()
                ][..]
            )
        );
    }

    #[test]
    fn test_with_cmd_verb_preserved() {
        let hc: Healthcheck =
            serde_json::from_value(json!({"test": ["CMD", "curl", "-f", "http://x"]})).unwrap();
        assert_eq!(
            hc.test.as_deref(),
            Some(
                &[
                    "CMD".to_string(),
                    "curl".to_string(),
                    "-f".to_string(),
                    "http://x".to_string()
                ][..]
            )
        );
    }

    #[test]
    fn test_with_cmd_shell_verb_preserved() {
        let hc: Healthcheck =
            serde_json::from_value(json!({"test": ["CMD-SHELL", "exit 0"]})).unwrap();
        assert_eq!(
            hc.test.as_deref(),
            Some(&["CMD-SHELL".to_string(), "exit 0".to_string()][..])
        );
    }

    #[test]
    fn test_none_list_preserved() {
        let hc: Healthcheck = serde_json::from_value(json!({"test": ["NONE"]})).unwrap();
        assert_eq!(hc.test.as_deref(), Some(&["NONE".to_string()][..]));
    }

    #[test]
    fn test_disable_true_overrides_test() {
        let hc: Healthcheck =
            serde_json::from_value(json!({"disable": true, "test": ["CMD", "ignored"]})).unwrap();
        assert_eq!(hc.test.as_deref(), Some(&["NONE".to_string()][..]));
    }

    #[test]
    fn test_bare_list_passes_through_unchanged() {
        // Docker treats this as exec args, not a shell command,
        // but it's valid and the legacy Supervisor implements it this way.
        let hc: Healthcheck =
            serde_json::from_value(json!({"test": ["curl", "-f", "http://x"]})).unwrap();
        assert_eq!(
            hc.test.as_deref(),
            Some(&["curl".to_string(), "-f".to_string(), "http://x".to_string()][..])
        );
    }

    #[test]
    fn test_empty_list_means_inherit() {
        // Unlike the legacy Supervisor, helios does not merge image and
        // service healthchecks — partial overrides aren't supported. An
        // empty `test` drops the field, letting the engine inherit the
        // image's HEALTHCHECK.
        let hc: Healthcheck =
            serde_json::from_value(json!({"test": [], "interval": 30000000000i64})).unwrap();
        assert_eq!(hc.test, None);
        assert_eq!(hc.interval.map(DurationNanos::to_i64), Some(30_000_000_000));
    }

    #[test]
    fn durations_pass_through() {
        let hc: Healthcheck = serde_json::from_value(json!({
            "test": "true",
            "interval": 30000000000i64,
            "timeout": 5000000000i64,
            "start_period": 10000000000i64,
            "start_interval": 1000000000i64,
            "retries": 3,
        }))
        .unwrap();
        assert_eq!(hc.interval.map(DurationNanos::to_i64), Some(30_000_000_000));
        assert_eq!(hc.timeout.map(DurationNanos::to_i64), Some(5_000_000_000));
        assert_eq!(
            hc.start_period.map(DurationNanos::to_i64),
            Some(10_000_000_000)
        );
        assert_eq!(
            hc.start_interval.map(DurationNanos::to_i64),
            Some(1_000_000_000)
        );
        assert_eq!(hc.retries, Some(3));
    }

    #[test]
    fn durations_parse_compose_strings() {
        let hc: Healthcheck = serde_json::from_value(json!({
            "test": "true",
            "interval": "30s",
            "timeout": "5000ms",
            "start_period": "10000000us",
            "start_interval": "1000000000ns",
            "retries": 3,
        }))
        .unwrap();
        assert_eq!(hc.interval.map(DurationNanos::to_i64), Some(30_000_000_000));
        assert_eq!(hc.timeout.map(DurationNanos::to_i64), Some(5_000_000_000));
        assert_eq!(
            hc.start_period.map(DurationNanos::to_i64),
            Some(10_000_000_000)
        );
        assert_eq!(
            hc.start_interval.map(DurationNanos::to_i64),
            Some(1_000_000_000)
        );
        assert_eq!(hc.retries, Some(3));
    }

    #[test]
    fn empty_object_yields_all_none() {
        let hc: Healthcheck = serde_json::from_value(json!({})).unwrap();
        assert_eq!(hc.test, None);
        assert_eq!(hc.interval, None);
        assert_eq!(hc.retries, None);
    }
}
