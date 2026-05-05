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
    /// Cgroup namespace mode (`host` or `private`)
    #[serde(default)]
    pub cgroup: Option<String>,

    #[serde(default)]
    pub cgroup_parent: Option<String>,

    #[serde(default)]
    pub command: Option<Command>,

    #[serde(default)]
    pub cpuset: Option<String>,

    #[serde(default)]
    pub domainname: Option<String>,

    #[serde(default)]
    pub environment: Environment,

    #[serde(default)]
    pub hostname: Option<String>,

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
    pub runtime: Option<String>,

    #[serde(default)]
    pub stop_signal: Option<String>,

    #[serde(default)]
    pub tty: bool,

    #[serde(default)]
    pub user: Option<String>,

    #[serde(default)]
    pub userns_mode: Option<String>,

    /// UTS namespace mode (`host` or empty)
    #[serde(default)]
    pub uts: Option<String>,

    #[serde(default)]
    pub working_dir: Option<String>,

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
    fn composition_string_fields_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.cgroup, None);
        assert_eq!(comp.cgroup_parent, None);
        assert_eq!(comp.cpuset, None);
        assert_eq!(comp.domainname, None);
        assert_eq!(comp.hostname, None);
        assert_eq!(comp.runtime, None);
        assert_eq!(comp.stop_signal, None);
        assert_eq!(comp.user, None);
        assert_eq!(comp.userns_mode, None);
        assert_eq!(comp.uts, None);
        assert_eq!(comp.working_dir, None);
    }

    #[test]
    fn composition_string_fields_parse_explicit_values() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "cgroup": "host",
            "cgroup_parent": "/custom",
            "cpuset": "0-3",
            "domainname": "example.com",
            "hostname": "my-host",
            "runtime": "runc",
            "stop_signal": "SIGTERM",
            "user": "1000:1000",
            "userns_mode": "host",
            "uts": "host",
            "working_dir": "/app",
        }))
        .unwrap();
        assert_eq!(comp.cgroup.as_deref(), Some("host"));
        assert_eq!(comp.cgroup_parent.as_deref(), Some("/custom"));
        assert_eq!(comp.cpuset.as_deref(), Some("0-3"));
        assert_eq!(comp.domainname.as_deref(), Some("example.com"));
        assert_eq!(comp.hostname.as_deref(), Some("my-host"));
        assert_eq!(comp.runtime.as_deref(), Some("runc"));
        assert_eq!(comp.stop_signal.as_deref(), Some("SIGTERM"));
        assert_eq!(comp.user.as_deref(), Some("1000:1000"));
        assert_eq!(comp.userns_mode.as_deref(), Some("host"));
        assert_eq!(comp.uts.as_deref(), Some("host"));
        assert_eq!(comp.working_dir.as_deref(), Some("/app"));
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
