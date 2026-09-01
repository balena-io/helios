use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// Condition under which a `depends_on` dependency is considered satisfied.
#[derive(Deserialize, Debug, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DependsOnCondition {
    #[default]
    ServiceStarted, // service started
    ServiceHealthy,               // service healthy per healthcheck
    ServiceCompletedSuccessfully, // service exited with status 0
}

/// Long form entry. Every field falls back to the `Default` impl below.
#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct LongFormDependsOn {
    pub condition: DependsOnCondition,
    /// When true, the dependent service is restarted after dependency restarts.
    /// Only applies to restarts issued through Helios. Defaults false.
    pub restart: bool,
    /// The dependent always waits while the condition is pending. When false, a
    /// condition that then fails only warns instead of blocking. Defaults true.
    pub required: bool,
}

impl Default for LongFormDependsOn {
    fn default() -> Self {
        Self {
            condition: DependsOnCondition::ServiceStarted,
            restart: false,
            required: true,
        }
    }
}

/// `depends_on` block on a service composition.
///
/// Accepts both Compose syntaxes at deserialization time:
/// - short form: `["svc1", "svc2"]`: equivalent to each entry with
///   `condition: service_started`, `restart: false`, `required: true`.
/// - long form: `{ "svc1": { "condition": "service_healthy", ... } }`, where every
///   field is optional and falls back to the short-form defaults.
#[derive(Debug, Default)]
pub struct DependsOn(HashMap<String, LongFormDependsOn>);

impl Deref for DependsOn {
    type Target = HashMap<String, LongFormDependsOn>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl IntoIterator for DependsOn {
    type Item = (String, LongFormDependsOn);
    type IntoIter = std::collections::hash_map::IntoIter<String, LongFormDependsOn>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl From<HashMap<String, LongFormDependsOn>> for DependsOn {
    fn from(value: HashMap<String, LongFormDependsOn>) -> Self {
        Self(value)
    }
}

/// Parse `depends_on` from either a list of service names or a map of per-service specs.
impl<'de> Deserialize<'de> for DependsOn {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DependsOnVisitor;

        impl<'de> Visitor<'de> for DependsOnVisitor {
            type Value = DependsOn;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a list of service names or a map of service names to dependency specs")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut map = HashMap::new();
                while let Some(name) = seq.next_element::<String>()? {
                    map.insert(name, LongFormDependsOn::default());
                }
                Ok(DependsOn(map))
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut map = HashMap::new();
                while let Some((name, spec)) = access.next_entry::<String, LongFormDependsOn>()? {
                    map.insert(name, spec);
                }
                Ok(DependsOn(map))
            }
        }

        deserializer.deserialize_any(DependsOnVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_short_form_as_service_started_with_defaults() {
        let deps: DependsOn = serde_json::from_value(json!(["db", "redis"])).unwrap();
        assert_eq!(deps.len(), 2);
        let db = deps.get("db").unwrap();
        assert_eq!(db.condition, DependsOnCondition::ServiceStarted);
        assert!(!db.restart);
        assert!(db.required);
    }

    #[test]
    fn parses_long_form_with_all_conditions() {
        let deps: DependsOn = serde_json::from_value(json!({
            "db": {"condition": "service_healthy", "restart": true},
            "redis": {"condition": "service_started", "required": false},
            "migrate": {"condition": "service_completed_successfully"},
        }))
        .unwrap();
        assert_eq!(
            deps.get("db").unwrap().condition,
            DependsOnCondition::ServiceHealthy
        );
        assert!(deps.get("db").unwrap().restart);
        assert!(deps.get("db").unwrap().required);
        assert_eq!(
            deps.get("redis").unwrap().condition,
            DependsOnCondition::ServiceStarted
        );
        assert!(!deps.get("redis").unwrap().required);
        assert_eq!(
            deps.get("migrate").unwrap().condition,
            DependsOnCondition::ServiceCompletedSuccessfully
        );
        assert!(!deps.get("migrate").unwrap().restart);
        assert!(deps.get("migrate").unwrap().required);
    }

    #[test]
    fn long_form_defaults_a_missing_condition_to_service_started() {
        let deps: DependsOn = serde_json::from_value(json!({"db": {"restart": true}})).unwrap();
        let db = deps.get("db").unwrap();
        assert_eq!(db.condition, DependsOnCondition::ServiceStarted);
        assert!(db.restart);
        assert!(db.required);
    }

    #[test]
    fn rejects_unknown_condition() {
        let err =
            serde_json::from_value::<DependsOn>(json!({"db": {"condition": "foo"}})).unwrap_err();
        assert!(
            err.to_string().contains("foo") || err.to_string().contains("variant"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_short_form_yields_empty_map() {
        let deps: DependsOn = serde_json::from_value(json!([])).unwrap();
        assert!(deps.is_empty());
    }
}
