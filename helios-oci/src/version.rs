use bollard::models::{SystemVersion, SystemVersionComponents};

use crate::{Error, Result};

/// Convenience alias to describe an OCI runtime
pub type Runtime = String;

/// The container engine implementation behind the daemon API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineKind {
    BalenaEngine,
    Docker,
    Podman,
    /// An engine that could not be identified
    Other,
}

/// Description of the connected container engine.
#[derive(Debug, Clone)]
pub struct Version {
    pub kind: EngineKind,
    /// The engine version
    pub version: String,
    /// The Docker API version served by the engine
    pub api_version: String,
    /// The OCI runtimes configured on the engine
    pub runtimes: Vec<Runtime>,
}

impl Version {
    /// Build the version description from the daemon's `/version` response.
    ///
    /// Fails if the response does not include the engine or API version.
    pub(crate) fn new(system_version: SystemVersion, runtimes: Vec<Runtime>) -> Result<Self> {
        let SystemVersion {
            components,
            version,
            api_version,
            ..
        } = system_version;
        Ok(Self {
            kind: detect_kind(components.unwrap_or_default()),
            version: version
                .ok_or_else(|| Error::from("missing engine version in /version response"))?,
            api_version: api_version
                .ok_or_else(|| Error::from("missing API version in /version response"))?,
            runtimes,
        })
    }
}

/// Identify the engine from the component names in the `/version` response.
///
/// All three engines name their main component differently or ship a
/// differently named init binary:
///
/// - Podman: `Podman Engine`
/// - balenaEngine: `Engine`, `balena-engine-init`
/// - Docker: `Engine`, `containerd`, `runc`, `docker-init`
///
/// `Engine` alone is shared by balenaEngine and Docker, so the init component
/// is what tells them apart.
fn detect_kind(components: Vec<SystemVersionComponents>) -> EngineKind {
    let has = |name: &str| components.iter().any(|c| c.name == name);

    if has("Podman Engine") {
        EngineKind::Podman
    } else if has("balena-engine-init") {
        EngineKind::BalenaEngine
    } else if has("docker-init") {
        EngineKind::Docker
    } else {
        EngineKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn kind(components: serde_json::Value) -> EngineKind {
        let version: SystemVersion = serde_json::from_value(json!({
            "Version": "1.0.0",
            "ApiVersion": "1.41",
            "Components": components,
        }))
        .unwrap();
        Version::new(version, vec![]).unwrap().kind
    }

    #[test]
    fn detects_balena_engine() {
        let components = json!([
            {"Name": "Engine", "Version": "20.10.43"},
            {"Name": "containerd", "Version": "1.6.0"},
            {"Name": "runc", "Version": "1.1.0"},
            {"Name": "balena-engine-init", "Version": "0.13.0"},
        ]);
        assert_eq!(kind(components), EngineKind::BalenaEngine);
    }

    #[test]
    fn detects_docker() {
        let components = json!([
            {"Name": "Engine", "Version": "27.3.1"},
            {"Name": "containerd", "Version": "1.7.22"},
            {"Name": "runc", "Version": "1.1.14"},
            {"Name": "docker-init", "Version": "0.19.0"},
        ]);
        assert_eq!(kind(components), EngineKind::Docker);
    }

    #[test]
    fn detects_podman() {
        let components = json!([
            {"Name": "Podman Engine", "Version": "5.2.0"},
            {"Name": "Conmon", "Version": "conmon 2.1.12"},
            {"Name": "OCI Runtime (crun)", "Version": "crun 1.15"},
        ]);
        assert_eq!(kind(components), EngineKind::Podman);
    }

    #[test]
    fn unrecognized_engine_is_other() {
        let components = json!([{"Name": "Mystery Engine", "Version": "1"}]);
        assert_eq!(kind(components), EngineKind::Other);
    }

    #[test]
    fn missing_components_is_other() {
        let version: SystemVersion =
            serde_json::from_value(json!({"Version": "1", "ApiVersion": "1.41"})).unwrap();
        assert_eq!(
            Version::new(version, vec![]).unwrap().kind,
            EngineKind::Other
        );
    }

    #[test]
    fn fails_without_engine_version() {
        let version: SystemVersion = serde_json::from_value(json!({"ApiVersion": "1.41"})).unwrap();
        assert!(Version::new(version, vec![]).is_err());
    }

    #[test]
    fn fails_without_api_version() {
        let version: SystemVersion = serde_json::from_value(json!({"Version": "1"})).unwrap();
        assert!(Version::new(version, vec![]).is_err());
    }

    #[test]
    fn populates_versions_and_runtimes() {
        let version: SystemVersion = serde_json::from_value(json!({
            "Version": "27.3.1",
            "ApiVersion": "1.47",
        }))
        .unwrap();
        let v = Version::new(version, vec!["runc".into()]).unwrap();
        assert_eq!(v.version, "27.3.1");
        assert_eq!(v.api_version, "1.47");
        assert_eq!(v.runtimes, vec!["runc".to_string()]);
    }
}
