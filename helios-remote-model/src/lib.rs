//! Target types received from the remote backend
//!
//! All input validations should happen in this module
//!
//! the types are based  from https://github.com/balena-io/open-balena-api/blob/master/src/features/device-state/routes/state-get-v3.ts#L48

use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

mod byte_size;
mod duration;
mod labels;
mod network;
mod service;
mod volume;

pub use byte_size::ByteSize;
pub use duration::{DurationMicros, DurationNanos, DurationSecs};
pub use labels::*;
pub use network::*;
pub use service::*;
pub use volume::*;

use helios_util::types as common_types;

use common_types::{ImageUri, Uuid};

/// Target device as defined by the remote backend
#[derive(Debug)]
pub struct Device {
    pub name: String,
    pub apps: AppMap,
}

/// Internal struct for deriving Deserialize
#[derive(Deserialize)]
struct DeviceRaw {
    name: String,
    #[serde(default)]
    apps: AppMap,
}

impl<'de> Deserialize<'de> for Device {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde_path_to_error::deserialize(deserializer)
            .map(|raw: DeviceRaw| Device {
                name: raw.name,
                apps: raw.apps,
            })
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Default)]
pub struct AppMap(HashMap<Uuid, App>);

impl Deref for AppMap {
    type Target = HashMap<Uuid, App>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for AppMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for AppMap {
    type Item = (Uuid, App);
    type IntoIter = std::collections::hash_map::IntoIter<Uuid, App>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'de> Deserialize<'de> for AppMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Parse each app's JSON lazily so a per-app deserialization error
        // (bad service composition, bad release map, missing hostapp labels)
        // can be captured as a rejection rather than aborting the whole device.
        let raw: HashMap<Uuid, Value> = HashMap::deserialize(deserializer)?;

        // Duplicate hostapps are a device-level configuration problem. Peek
        // at the raw `is_host` flags before per-app parsing so the check
        // still fires if the apps would otherwise fail validation.
        if raw
            .values()
            .filter(|v| v.get("is_host").and_then(Value::as_bool).unwrap_or(false))
            .count()
            > 1
        {
            return Err(serde::de::Error::custom(
                "only one target hostapp is allowed",
            ));
        }

        let mut apps: HashMap<Uuid, App> = HashMap::new();
        for (app_uuid, value) in raw {
            match parse_app(value) {
                Ok(app) => {
                    apps.insert(app_uuid, app);
                }
                Err(ParseAppError::Reject(rejection)) => {
                    apps.insert(app_uuid, App::Rejected(rejection));
                }
                Err(ParseAppError::Fatal(msg)) => {
                    return Err(serde::de::Error::custom(msg));
                }
            }
        }

        Ok(AppMap(apps))
    }
}

/// An app with an invalid target release
#[derive(Debug, Clone)]
pub struct RejectedApp {
    pub id: u32,
    pub name: String,
    pub is_host: bool,
    pub release: Uuid,
    pub reason: String,
}

#[derive(Debug)]
pub enum App {
    User(UserApp),
    Host(HostApp),
    Rejected(RejectedApp),
}

/// Outcome of `parse_app` — either an accepted `App`, a per-app rejection
/// keyed by a specific release uuid, or a fatal device-level error that
/// should abort the whole `AppMap` deserialization.
enum ParseAppError {
    Reject(RejectedApp),
    Fatal(String),
}

fn parse_app(value: Value) -> Result<App, ParseAppError> {
    #[derive(Deserialize, Debug)]
    struct AppRaw {
        id: u32,
        name: String,
        #[serde(default)]
        is_host: bool,
        #[serde(default)]
        releases: HashMap<Uuid, Value>,
    }

    // Stage 1: parse the app's top-level shape, leaving releases as raw JSON.
    // A failure here is an input error from the backend, not a per-release
    // content problem — abort the whole device deserialization.
    let app = AppRaw::deserialize(value).map_err(|e| ParseAppError::Fatal(e.to_string()))?;

    // Stage 2: enforce the single-release constraint and parse each release
    // independently so a per-release failure carries its uuid.
    if app.releases.len() > 1 {
        return Err(ParseAppError::Fatal(
            "target releases should only contain one release".to_string(),
        ));
    }

    let mut releases: HashMap<Uuid, Release> = HashMap::new();
    for (rel_uuid, rel_value) in app.releases {
        match serde_path_to_error::deserialize::<_, Release>(rel_value) {
            Ok(rel) => {
                releases.insert(rel_uuid, rel);
            }
            Err(e) => {
                // Invalid release content, the app is rejected
                return Err(ParseAppError::Reject(RejectedApp {
                    id: app.id,
                    name: app.name,
                    is_host: app.is_host,
                    release: rel_uuid,
                    reason: e.to_string(),
                }));
            }
        }
    }
    let releases = ReleaseMap(releases);

    if !app.is_host {
        return Ok(App::User(UserApp {
            id: app.id,
            name: app.name,
            releases,
        }));
    }

    // Stage 3: hostapp metadata validation. An empty release map at this
    // point is a shape problem (the backend sent a hostapp with no release).
    // Everything else is content of a specific release and
    // produces a per-app rejection tagged with that release's uuid.
    let Some((release_uuid, release)) = releases.into_iter().next() else {
        return Err(ParseAppError::Fatal(
            "hostapp should have at least one target release".to_string(),
        ));
    };

    let Some(svc) = release.services.into_values().find(|svc| {
        svc.composition
            .labels
            .get("io.balena.image.class")
            .map(|value| value == "hostapp")
            .is_some()
    }) else {
        return Err(ParseAppError::Reject(RejectedApp {
            id: app.id,
            name: app.name,
            is_host: app.is_host,
            release: release_uuid,
            reason: "hostapp should have a service with `io.balena.image.class=hostapp` label"
                .to_string(),
        }));
    };

    let mut labels: HashMap<String, String> = svc
        .composition
        .labels
        .into_iter()
        .chain(svc.labels)
        .collect();

    let updater = match labels.remove("io.balena.private.updater") {
        Some(updater) => match updater.parse::<ImageUri>() {
            Ok(u) => u,
            Err(e) => {
                return Err(ParseAppError::Reject(RejectedApp {
                    id: app.id,
                    name: app.name,
                    is_host: app.is_host,
                    release: release_uuid,
                    reason: format!("invalid hostapp updater: {e}"),
                }));
            }
        },
        None => {
            return Err(ParseAppError::Reject(RejectedApp {
                id: app.id,
                name: app.name,
                is_host: app.is_host,
                release: release_uuid,
                reason: "hostapp missing `updater` label".to_string(),
            }));
        }
    };

    let Some(board_rev) = labels.remove("io.balena.private.hostapp.board-rev") else {
        return Err(ParseAppError::Reject(RejectedApp {
            id: app.id,
            name: app.name,
            is_host: app.is_host,
            release: release_uuid,
            reason: "hostapp missing `board_rev` label".to_string(),
        }));
    };

    Ok(App::Host(HostApp {
        release_uuid,
        image: svc.image,
        board_rev,
        updater,
    }))
}

#[derive(Debug)]
pub struct HostApp {
    pub release_uuid: Uuid,
    pub image: ImageUri,
    pub board_rev: String,
    pub updater: ImageUri,
}

/// Target app as defined by the remote backend
#[derive(Deserialize, Debug)]
pub struct UserApp {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub releases: ReleaseMap,
}

#[derive(Debug, Default)]
pub struct ReleaseMap(HashMap<Uuid, Release>);

impl Deref for ReleaseMap {
    type Target = HashMap<Uuid, Release>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ReleaseMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for ReleaseMap {
    type Item = (Uuid, Release);
    type IntoIter = std::collections::hash_map::IntoIter<Uuid, Release>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'de> Deserialize<'de> for ReleaseMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let releases: HashMap<Uuid, Release> = HashMap::deserialize(deserializer)?;

        if releases.len() > 1 {
            return Err(serde::de::Error::custom(
                "target releases should only contain one release",
            ));
        }

        Ok(Self(releases))
    }
}

#[derive(Debug)]
pub struct Release {
    pub services: HashMap<String, Service>,
    pub volumes: HashMap<String, Volume>,
    pub networks: HashMap<String, Network>,
}

impl<'de> Deserialize<'de> for Release {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ReleaseRaw {
            #[serde(default)]
            services: HashMap<String, Service>,
            #[serde(default)]
            volumes: HashMap<String, Volume>,
            #[serde(default)]
            networks: HashMap<String, Network>,
        }

        let mut raw: ReleaseRaw = ReleaseRaw::deserialize(deserializer)?;
        let mut needs_default_net = false;

        for (svc_name, svc) in raw.services.iter_mut() {
            // network_mode and networks are mutually exclusive per the Compose spec
            if svc.composition.network_mode.is_some() && !svc.composition.networks.is_empty() {
                return Err(serde::de::Error::custom(format!(
                    "service {svc_name} declares mutually exclusive `network_mode` and `networks`"
                )));
            }

            for net_name in svc.composition.networks.keys() {
                if !raw.networks.contains_key(net_name) {
                    return Err(serde::de::Error::custom(format!(
                        "service '{svc_name}' refers to undefined network {net_name}"
                    )));
                }
            }

            // Verify each volume-type mount references a volume declared at the release level.
            for mount in svc.composition.volumes.iter() {
                if let Mount::Volume(v) = mount
                    && !raw.volumes.contains_key(&v.source)
                {
                    return Err(serde::de::Error::custom(format!(
                        "service '{svc_name}' refers to undefined volume {}",
                        v.source
                    )));
                }
            }

            // Skip default-network injection when network_mode is set (host/none don't
            // participate in user-defined networks).
            if svc.composition.networks.is_empty() && svc.composition.network_mode.is_none() {
                needs_default_net = true;
                svc.composition
                    .networks
                    .entry("default".to_string())
                    .or_insert(None);
            }
        }

        // Add a default network to isolate app services
        if needs_default_net {
            raw.networks.entry("default".to_string()).or_default();
        }

        Ok(Release {
            services: raw.services,
            volumes: raw.volumes,
            networks: raw.networks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_accepts_empty_releases() {
        let json = json!({});

        let releases: ReleaseMap = serde_json::from_value(json).unwrap();
        assert_eq!(releases.len(), 0)
    }

    #[test]
    fn test_accepts_single_release() {
        let json = json!({
            "release-one": {}
        });

        let releases: ReleaseMap = serde_json::from_value(json).unwrap();
        assert_eq!(releases.len(), 1);
        assert!(releases.contains_key(&"release-one".into()));
    }

    #[test]
    fn test_rejects_target_releases_with_more_than_one_release() {
        let json = json!({
                "relase-one": {
                },
                "release-two": {
                },
        });

        let release = serde_json::from_value::<ReleaseMap>(json);
        assert!(release.is_err());
    }

    #[test]
    fn test_rejects_target_apps_with_more_than_one_hostapp() {
        let json = json!({
            "app-one": {
                "id": 1,
                "name": "ubuntu",
                "is_host": true,
            },
            "app-two": {
                "id": 2,
                "name": "fedora",
                "is_host": true,
            }
        });

        let apps = serde_json::from_value::<AppMap>(json);
        assert!(apps.is_err_and(|e| e.to_string() == "only one target hostapp is allowed"));
    }

    #[test]
    fn test_rejects_target_apps_with_no_releases() {
        let json = json!({
            "app-one": {
                "id": 1,
                "name": "ubuntu",
                "is_host": true,
                "releases": {}
            },

        });

        let err = serde_json::from_value::<AppMap>(json).unwrap_err();
        assert_eq!(
            err.to_string(),
            "hostapp should have at least one target release",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_command() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "command": "echo 'hello world",
                                    }
                                }
                            }
                        }
                    }
                },
            }

        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.command: missing closing quote",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_entrypoint() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "entrypoint": "/bin/sh -c 'echo hello",
                                    }
                                }
                            }
                        }
                    }
                },
            }

        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.entrypoint: missing closing quote",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_security_opt() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "security_opt": ["label:user:USER"]
                                    }
                                }
                            }
                        }
                    }
                },
            }

        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.security_opt: only `no-new-privileges`, `apparmor=unconfined` and `seccomp=unconfined` are allowed, got `label:user:USER`",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_networks() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "networks": {
                                            "my-net": {
                                                "aliases": "my-alias"
                                            }

                                        },
                                    }
                                }
                            },
                            "networks": {
                                "my-net": {}
                            }
                        }
                    }
                },
            }

        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.networks.my-net.aliases: invalid type: string \"my-alias\", expected a sequence",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_volumes() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "data",
                                                "target": "/data",
                                                "read_only": "yes"
                                            }
                                        ]
                                    }
                                }
                            },
                            "volumes": {
                                "data": {}
                            }
                        }
                    }
                },
            }
        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.volumes[0].read_only: invalid type: string \"yes\", expected a boolean",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_environment() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "environment": {
                                            "MY_VAR": ["value"]
                                        },
                                    }
                                }
                            }
                        }
                    }
                },
            }

        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.environment.MY_VAR: invalid type: sequence, expected a boolean, number, or string",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_extra_hosts() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "extra_hosts": ["noseparator"]
                                    }
                                }
                            }
                        }
                    }
                },
            }
        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.extra_hosts: entry `noseparator` must be in `host:ip` or `host=ip` form",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_labels() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "labels": {
                                            "my-label": 123
                                        },
                                    }
                                }
                            }
                        }
                    }
                },
            }

        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.labels.my-label: invalid type: integer `123`, expected a string",
        );
    }

    #[test]
    fn test_rejects_target_service_with_invalid_ports() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "app-one": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "c8b48659434e80a8b3adc0c5ad1e347a": {
                            "id": 7,
                            "services": {
                                "main": {
                                    "id": 3,
                                    "image_id": 4,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "ports": [
                                            "abc",
                                        ]
                                    }
                                }
                            }
                        }
                    }
                },
            }

        });

        let device: Device = serde_json::from_value(json).unwrap();
        let rejection = match device.apps.get(&Uuid::from("app-one")) {
            Some(App::Rejected(r)) => r,
            other => panic!("app-one should be rejected, got {other:?}"),
        };
        assert_eq!(
            rejection.release,
            Uuid::from("c8b48659434e80a8b3adc0c5ad1e347a"),
        );
        assert_eq!(
            rejection.reason,
            "services.main.composition.ports: invalid port number `abc`",
        );
    }

    #[test]
    fn test_rejects_only_invalid_app_keeping_valid_ones() {
        let json = json!({
            "name": "my-device",
            "apps": {
                "good-app": {
                    "id": 1,
                    "name": "good",
                    "releases": {
                        "rel-good": {
                            "id": 10,
                            "services": {
                                "main": {
                                    "id": 100,
                                    "image_id": 200,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                }
                            }
                        }
                    }
                },
                "bad-app": {
                    "id": 2,
                    "name": "bad",
                    "releases": {
                        "rel-bad": {
                            "id": 11,
                            "services": {
                                "main": {
                                    "id": 101,
                                    "image_id": 201,
                                    "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                    "composition": {
                                        "command": "echo 'unterminated"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let device: Device = serde_json::from_value(json).unwrap();
        assert_eq!(device.apps.len(), 2);
        assert!(matches!(
            device.apps.get(&Uuid::from("good-app")),
            Some(App::User(_)),
        ));
        let rejection = match device.apps.get(&Uuid::from("bad-app")) {
            Some(App::Rejected(r)) => r,
            other => panic!("bad-app should be rejected, got {other:?}"),
        };
        assert_eq!(rejection.release, Uuid::from("rel-bad"));
        assert!(
            rejection.reason.contains("missing closing quote"),
            "unexpected reason: {}",
            rejection.reason,
        );
    }

    #[test]
    fn test_accepts_single_release_with_volumes_and_networks() {
        let json = json!({
            "release-one": {
                "services": {},
                "networks": {
                    "my-net": {},
                },
                "volumes": {
                    "cache": {},
                    "runtime": {
                        "driver": "local",
                        "driver_opts": {
                            "o": "bind",
                            "type": "none",
                            "device": "/tmp/helios"
                        }
                    }
                }

            }
        });

        let releases: ReleaseMap = serde_json::from_value(json).unwrap();
        assert_eq!(releases.len(), 1);
        assert!(releases.contains_key(&"release-one".into()));
    }

    #[test]
    fn test_accepts_hostapp_target() {
        // use a target from a real hostapp to test
        let json = json!({
            "ea8013b1a82540b59bc8b109b45739ab": {
                "id": 3,
                "name": "generic-aarch64",
                "is_host": true,
                "class": "app",
                "releases": {
                    "c8b48659434e80a8b3adc0c5ad1e347a": {
                        "id": 7,
                        "services": {
                            "hostapp": {
                                "id": 3,
                                "image_id": 4,
                                "image": "registry2.balena-cloud.com/v2/8a961e0325a37441f33091743fa40a4c@sha256:0f3169ee8672222eb775b032cb3b2d06ef8eafa23a970643052bb67ac1fc5cd9",
                                "environment": {},
                                "labels": {
                                    "io.balena.image.store": "root",
                                    "io.balena.private.updater": "registry2.balena-cloud.com/v2/1ccec8773ae44f99ffd90e037820cb3f@sha256:18ed4befff5fe0267bfa7cce5823b80fb00f6ab6a1f476c899ed32b1ac40f110"
                                },
                                "composition": {
                                    "image": "sha256:f7746a3c289a1ba5818ec6dab298ea7a399f15f9459fac9a89b371bec46ad2ac",
                                    "labels": {
                                        "io.balena.image.class": "hostapp",
                                        "io.balena.image.store": "root",
                                        "io.balena.update.requires-reboot": "1",
                                        "io.balena.private.hostapp.board-rev": "7de0f0f"
                                    }
                                }
                            }
                        }
                    }
                }
        }
        });

        let apps: AppMap = serde_json::from_value(json).unwrap();
        assert_eq!(apps.len(), 1);

        let app = apps
            .get(&"ea8013b1a82540b59bc8b109b45739ab".into())
            .unwrap();

        if let App::Host(hostapp) = app {
            assert_eq!(
                hostapp.release_uuid,
                "c8b48659434e80a8b3adc0c5ad1e347a".into()
            );
        } else {
            panic!("expected hostapp");
        }
    }

    #[test]
    fn test_release_injects_default_network_for_services_without_net_config() {
        let release: Release = serde_json::from_value(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "ubuntu:latest",
                },
                "my-other-service": {
                    "id": 2,
                    "image": "ubuntu:latest",
                    "composition": {
                        "networks": {"my-net": {}}
                    }
                }
            },
            "volumes": {},
            "networks": {
                "my-net": {}
            }
        }))
        .unwrap();

        assert!(release.networks.contains_key("default"));
        assert!(
            release
                .services
                .get("my-service")
                .is_some_and(|svc| svc.composition.networks.contains_key("default"))
        );
        assert!(
            release
                .services
                .get("my-other-service")
                .is_some_and(|svc| !svc.composition.networks.contains_key("default"))
        );
        let default_net = &release.networks["default"];
        assert_eq!(default_net.driver, None);
        assert!(default_net.ipam.is_none());
    }

    #[test]
    fn test_release_rejects_network_mode_and_networks_together() {
        let err = serde_json::from_value::<Release>(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "ubuntu:latest",
                    "composition": {
                        "network_mode": "host",
                        "networks": {"my-net": {}}
                    }
                }
            },
            "volumes": {},
            "networks": {"my-net": {}}
        }))
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "service my-service declares mutually exclusive `network_mode` and `networks`"
        );
    }

    #[test]
    fn test_release_network_mode_skips_default_network() {
        let release: Release = serde_json::from_value(json!({
            "services": {
                "host-svc": {
                    "id": 1,
                    "image": "ubuntu:latest",
                    "composition": {"network_mode": "host"}
                }
            },
            "volumes": {},
            "networks": {}
        }))
        .unwrap();

        assert!(!release.networks.contains_key("default"));
        let svc = release.services.get("host-svc").unwrap();
        assert!(svc.composition.networks.is_empty());
        assert_eq!(svc.composition.network_mode, Some(NetworkMode::Host));
    }

    #[test]
    fn test_release_preserves_explicit_default_network() {
        let release: Release = serde_json::from_value(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "ubuntu:latest",
                },
            },
            "volumes": {},
            "networks": {
                "default": {
                    "driver": "overlay",
                    "internal": true
                }
            }
        }))
        .unwrap();

        let default_net = &release.networks["default"];
        assert_eq!(default_net.driver, Some("overlay".to_string()));
        assert!(default_net.internal);
    }

    #[test]
    fn test_release_accepts_service_with_volume_mount() {
        let release: Release = serde_json::from_value(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "ubuntu:latest",
                    "composition": {
                        "volumes": ["data:/var/data"]
                    }
                }
            },
            "volumes": {"data": {}},
            "networks": {}
        }))
        .unwrap();

        let svc = release.services.get("my-service").unwrap();
        assert_eq!(svc.composition.volumes.iter().count(), 1);
    }

    #[test]
    fn test_release_rejects_undefined_volume_reference() {
        let err = serde_json::from_value::<Release>(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "ubuntu:latest",
                    "composition": {
                        "volumes": ["missing:/data"]
                    }
                }
            },
            "volumes": {},
            "networks": {}
        }))
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "service 'my-service' refers to undefined volume missing"
        );
    }

    #[test]
    fn test_release_accepts_service_with_bind_and_tmpfs_mounts() {
        let release: Release = serde_json::from_value(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "ubuntu:latest",
                    "composition": {
                        "volumes": [
                            "/etc/machine-id:/etc/machine-id",
                            {"type": "tmpfs", "target": "/tmp"}
                        ]
                    }
                }
            },
            "volumes": {},
            "networks": {}
        }))
        .unwrap();

        let svc = release.services.get("my-service").unwrap();
        assert_eq!(svc.composition.volumes.iter().count(), 2);
    }
}
