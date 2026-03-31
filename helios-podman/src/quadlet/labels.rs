//! Docker Compose label parser for converting container labels to quadlet dependencies.

use compose_spec::service::{Condition, Dependency as DependencyConfig};
use std::{collections::HashMap, ops::Deref};

use thiserror::Error;

#[derive(Debug, PartialEq)]
pub struct Dependency {
    pub service: String,
    pub config: DependencyConfig,
}

/// Parsed Docker Compose labels from a container.
#[derive(Debug, Default)]
pub struct DependsOn(pub Vec<Dependency>);

impl Deref for DependsOn {
    type Target = Vec<Dependency>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl IntoIterator for DependsOn {
    type Item = Dependency;
    type IntoIter = std::vec::IntoIter<Dependency>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Error returned when parsing compose labels.
#[derive(Error, Debug)]
#[error(
    "unknown condition `{0}`, expected `service_started`, `service_healthy`, or `service_completed_successfully`"
)]
pub struct UnknownDependsOnCondition(String);

const LABEL_DEPENDS_ON: &str = "com.docker.compose.depends_on";

impl DependsOn {
    /// Parse Docker Compose labels from a label map.
    ///
    /// # Errors
    ///
    /// Returns an error if `com.docker.compose.depends_on` contains an invalid entry.
    pub fn parse(labels: &HashMap<String, String>) -> Result<Self, UnknownDependsOnCondition> {
        let depends_on = match labels.get(LABEL_DEPENDS_ON) {
            Some(value) if !value.is_empty() => parse_depends_on(value)?,
            _ => Vec::new(),
        };

        Ok(Self(depends_on))
    }
}

/// Parse the `com.docker.compose.depends_on` label value.
///
/// Format: `service1:condition:restart:required,service2:condition:restart:required`
/// where condition is `service_started`, `service_healthy`, or `service_completed_successfully`
/// and restart is `true` or `false`.
fn parse_depends_on(value: &str) -> Result<Vec<Dependency>, UnknownDependsOnCondition> {
    value
        .split(',')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let parts: Vec<&str> = entry.split(':').collect();

            let service = parts[0].to_owned();
            let mut condition = Condition::ServiceStarted;
            let mut restart = true;
            let mut required = true;

            if parts.len() > 1 {
                condition = parse_condition(parts[1])?;
                if parts.len() > 2 {
                    restart = parts[2] == "true";

                    if parts.len() > 3 {
                        required = parts[3] == "true";
                    }
                }
            }

            Ok(Dependency {
                service,
                config: DependencyConfig {
                    condition,
                    restart,
                    required,
                },
            })
        })
        .collect()
}

fn parse_condition(s: &str) -> Result<Condition, UnknownDependsOnCondition> {
    match s {
        "service_started" => Ok(Condition::ServiceStarted),
        "service_healthy" => Ok(Condition::ServiceHealthy),
        "service_completed_successfully" => Ok(Condition::ServiceCompletedSuccessfully),
        other => Err(UnknownDependsOnCondition(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_labels() {
        let labels = HashMap::new();
        let result = DependsOn::parse(&labels).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_single_dependency() {
        let labels = HashMap::from([(
            LABEL_DEPENDS_ON.to_owned(),
            "db:service_started:true".to_owned(),
        )]);
        let result = DependsOn::parse(&labels).unwrap();
        assert_eq!(
            *result,
            vec![Dependency {
                service: "db".to_owned(),
                config: DependencyConfig {
                    condition: Condition::ServiceStarted,
                    restart: true,
                    required: true,
                }
            }]
        );
    }

    #[test]
    fn parse_multiple_dependencies() {
        let labels = HashMap::from([(
            LABEL_DEPENDS_ON.to_owned(),
            "db:service_started:true,cache:service_healthy:false".to_owned(),
        )]);
        let result = DependsOn::parse(&labels).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].service, "db");
        assert_eq!(result[0].config.condition, Condition::ServiceStarted);
        assert!(result[0].config.restart);
        assert_eq!(result[1].service, "cache");
        assert_eq!(result[1].config.condition, Condition::ServiceHealthy);
        assert!(!result[1].config.restart);
    }

    #[test]
    fn parse_unknown_condition() {
        let labels = HashMap::from([(
            LABEL_DEPENDS_ON.to_owned(),
            "db:unknown_condition:true".to_owned(),
        )]);
        assert!(DependsOn::parse(&labels).is_err());
    }

    #[test]
    fn parse_empty_depends_on() {
        let labels = HashMap::from([(LABEL_DEPENDS_ON.to_owned(), String::new())]);
        let result = DependsOn::parse(&labels).unwrap();
        assert!(result.is_empty());
    }
}
