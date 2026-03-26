use std::collections::HashMap;

use serde::Deserialize;

use crate::common_types::{Environment, ImageUri, Value};

use super::command::Command;
use super::labels::Labels;
use super::restart_policy::RestartPolicy;

/// Target service as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct Service {
    pub id: u32,
    pub image: ImageUri,

    #[serde(default)]
    pub labels: HashMap<String, String>,

    #[serde(default)]
    pub environment: HashMap<String, Option<Value>>,

    #[serde(default)]
    pub composition: ServiceComposition,
}

// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug, Default)]
pub struct ServiceComposition {
    #[serde(default)]
    pub restart: RestartPolicy,

    #[serde(default)]
    pub command: Option<Command>,

    #[serde(default)]
    pub labels: Labels,

    #[serde(default)]
    pub environment: Environment,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_defaults_restart_to_always() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.restart, RestartPolicy::Always);
    }

    #[test]
    fn composition_with_restart_policy() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"restart": "on-failure:5"})).unwrap();
        assert_eq!(
            comp.restart,
            RestartPolicy::OnFailure {
                max_retries: Some(5)
            }
        );
    }

    #[test]
    fn service_environment_parses_string_bool() {
        let svc: Service = serde_json::from_value(serde_json::json!({
            "id": 1,
            "image": "registry.example.com/repo/image@sha256:1234567890abcdef1234567890abcdef12345678",
            "environment": { "DEBUG": "true", "VERBOSE": "false" }
        }))
        .unwrap();
        assert_eq!(svc.environment.get("DEBUG"), Some(&Some(Value::Bool(true))));
        assert_eq!(
            svc.environment.get("VERBOSE"),
            Some(&Some(Value::Bool(false)))
        );
    }

    #[test]
    fn service_environment_parses_string_numbers() {
        let svc: Service = serde_json::from_value(serde_json::json!({
            "id": 1,
            "image": "registry.example.com/repo/image@sha256:1234567890abcdef1234567890abcdef12345678",
            "environment": { "PORT": "8080", "OFFSET": "-5", "RATIO": "1.5" }
        }))
        .unwrap();
        assert_eq!(
            svc.environment.get("PORT"),
            Some(&Some(Value::Unsigned(8080)))
        );
        assert_eq!(
            svc.environment.get("OFFSET"),
            Some(&Some(Value::Signed(-5)))
        );
        assert_eq!(svc.environment.get("RATIO"), Some(&Some(Value::Float(1.5))));
    }

    #[test]
    fn service_environment_preserves_native_types() {
        let svc: Service = serde_json::from_value(serde_json::json!({
            "id": 1,
            "image": "registry.example.com/repo/image@sha256:1234567890abcdef1234567890abcdef12345678",
            "environment": { "FLAG": true, "COUNT": 42, "RATIO": 5.14 }
        }))
        .unwrap();
        assert_eq!(svc.environment.get("FLAG"), Some(&Some(Value::Bool(true))));
        assert_eq!(
            svc.environment.get("COUNT"),
            Some(&Some(Value::Unsigned(42)))
        );
        assert_eq!(
            svc.environment.get("RATIO"),
            Some(&Some(Value::Float(5.14)))
        );
    }

    #[test]
    fn service_environment_handles_null_and_string() {
        let svc: Service = serde_json::from_value(serde_json::json!({
            "id": 1,
            "image": "registry.example.com/repo/image@sha256:1234567890abcdef1234567890abcdef12345678",
            "environment": { "UNSET": null, "HOST": "localhost" }
        }))
        .unwrap();
        assert_eq!(svc.environment.get("UNSET"), Some(&None));
        assert_eq!(
            svc.environment.get("HOST"),
            Some(&Some(Value::String("localhost".to_string())))
        );
    }
}
