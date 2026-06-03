use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// Condition under which a `depends_on` dependency is considered satisfied.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DependsOnCondition {
    #[default]
    ServiceStarted, // service started
    ServiceHealthy,               // service healthy per healthcheck
    ServiceCompletedSuccessfully, // service exited with status 0
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct LongFormDependsOn {
    pub condition: DependsOnCondition,
    /// When true, the dependent service is restarted after dependency restarts.
    /// Only applies to restarts issued through Helios. Defaults false.
    pub restart: bool,
    /// When false, an unmet dependency only emits a warning instead of
    /// blocking the dependent service from starting. Defaults true.
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
/// - long form: `{ "svc1": { "condition": "service_healthy", ... } }`.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct DependsOn(HashMap<String, LongFormDependsOn>);

impl Deref for DependsOn {
    type Target = HashMap<String, LongFormDependsOn>;
    fn deref(&self) -> &Self::Target {
        &self.0
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
        // Long form requires `condition` explicitly while the
        // other fields default. A custom visitor preserves precise error
        // messages for malformed long-form entries.
        #[derive(Deserialize)]
        struct LongFormDependsOnRaw {
            condition: DependsOnCondition,
            #[serde(default)]
            restart: bool,
            #[serde(default)]
            required: Option<bool>,
        }

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
                while let Some((name, spec)) =
                    access.next_entry::<String, LongFormDependsOnRaw>()?
                {
                    map.insert(
                        name,
                        LongFormDependsOn {
                            condition: spec.condition,
                            restart: spec.restart,
                            required: spec.required.unwrap_or(true),
                        },
                    );
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
    fn long_form_requires_condition() {
        let err =
            serde_json::from_value::<DependsOn>(json!({"db": {"restart": true}})).unwrap_err();
        assert!(
            err.to_string().contains("condition"),
            "unexpected error: {err}"
        );
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
