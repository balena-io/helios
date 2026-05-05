use std::collections::HashMap;

use serde::Deserialize;

use crate::common_types::{Environment, ImageUri, Value};

mod command;
mod network_mode;
mod networks;
mod restart_policy;
mod volumes;

pub use command::*;
pub use network_mode::*;
pub use networks::*;
pub use restart_policy::*;
pub use volumes::*;

use super::labels::Labels;

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
    pub command: Option<Command>,

    #[serde(default)]
    pub environment: Environment,

    /// `None`: daemon default applies
    /// `Some(_)`: container-level setting
    #[serde(default)]
    pub init: Option<bool>,

    #[serde(default)]
    pub labels: Labels,

    #[serde(default)]
    pub privileged: bool,

    #[serde(default)]
    pub read_only: bool,

    #[serde(default)]
    pub restart: RestartPolicy,

    #[serde(default)]
    pub tty: bool,

    #[serde(default)]
    pub networks: NetworkingConfig,

    #[serde(default)]
    pub network_mode: Option<NetworkMode>,

    #[serde(default)]
    pub volumes: VolumesConfig,
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
    fn composition_bool_fields_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!comp.privileged);
        assert!(!comp.read_only);
        assert!(!comp.tty);
        assert_eq!(comp.init, None);
    }

    #[test]
    fn composition_bool_fields_parse_explicit_values() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "privileged": true,
            "read_only": true,
            "tty": true,
            "init": true,
        }))
        .unwrap();
        assert!(comp.privileged);
        assert!(comp.read_only);
        assert!(comp.tty);
        assert_eq!(comp.init, Some(true));
    }

    #[test]
    fn composition_init_preserves_explicit_false() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"init": false})).unwrap();
        assert_eq!(comp.init, Some(false));
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
