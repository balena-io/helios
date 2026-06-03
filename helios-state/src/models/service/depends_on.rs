use std::collections::HashMap;
use std::ops::Deref;

use mahler::state::State;
use serde::{Deserialize, Serialize};

use crate::remote_model::{
    DependsOn as RemoteDependsOn, DependsOnCondition as RemoteDependsOnCondition,
    LongFormDependsOn as RemoteLongFormDependsOn,
};

/// Condition under which a `depends_on` dependency is considered satisfied.
///
/// `Ord` gives a stable tiebreak when emitting await tasks for a dependency
/// referenced under more than one condition.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DependsOnCondition {
    ServiceStarted,
    ServiceHealthy,
    ServiceCompletedSuccessfully,
}

impl From<RemoteDependsOnCondition> for DependsOnCondition {
    fn from(value: RemoteDependsOnCondition) -> Self {
        match value {
            RemoteDependsOnCondition::ServiceStarted => Self::ServiceStarted,
            RemoteDependsOnCondition::ServiceHealthy => Self::ServiceHealthy,
            RemoteDependsOnCondition::ServiceCompletedSuccessfully => {
                Self::ServiceCompletedSuccessfully
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DependencySpec {
    pub condition: DependsOnCondition,
    pub restart: bool,
    pub required: bool,
}

impl From<RemoteLongFormDependsOn> for DependencySpec {
    fn from(value: RemoteLongFormDependsOn) -> Self {
        Self {
            condition: value.condition.into(),
            restart: value.restart,
            required: value.required,
        }
    }
}

/// Per-service `depends_on` map carried on `Service`. Serialized as JSON into
/// the engine container's `LABEL_DEPENDS_ON` so that the value round-trips on
/// container observation and does not register as state drift.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(transparent)]
pub struct DependsOn(HashMap<String, DependencySpec>);

impl State for DependsOn {
    type Target = Self;
}

impl Deref for DependsOn {
    type Target = HashMap<String, DependencySpec>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<HashMap<String, DependencySpec>> for DependsOn {
    fn from(value: HashMap<String, DependencySpec>) -> Self {
        Self(value)
    }
}

impl From<RemoteDependsOn> for DependsOn {
    fn from(value: RemoteDependsOn) -> Self {
        Self(
            value
                .iter()
                .map(|(name, spec)| (name.clone(), spec.clone().into()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_label() {
        let mut map = HashMap::new();
        map.insert(
            "db".to_string(),
            DependencySpec {
                condition: DependsOnCondition::ServiceHealthy,
                restart: true,
                required: false,
            },
        );
        map.insert(
            "redis".to_string(),
            DependencySpec {
                condition: DependsOnCondition::ServiceStarted,
                restart: false,
                required: true,
            },
        );
        let deps: DependsOn = map.into();

        let encoded = serde_json::to_string(&deps).unwrap();
        let decoded: DependsOn = serde_json::from_str(&encoded).unwrap();
        assert_eq!(deps, decoded);
    }
}
