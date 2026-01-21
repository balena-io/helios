//! Target types received from the remote backend
//!
//! All input validations should happen in this module
//!
//! the types are based  from https://github.com/balena-io/open-balena-api/blob/master/src/features/device-state/routes/state-get-v3.ts#L48

use serde::Deserialize;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use helios_util::types::{ImageUri, Uuid};

mod network;
mod service;
mod volume;

pub use network::*;
pub use service::*;
pub use volume::*;

/// Target device as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct Device {
    pub name: String,

    #[serde(default)]
    pub apps: AppMap,
}

#[derive(Clone, Debug, Default)]
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
        #[derive(Deserialize, Clone, Debug)]
        struct AppTarget {
            pub id: u32,
            pub name: String,
            #[serde(default)]
            pub is_host: bool,
            #[serde(default)]
            pub releases: ReleaseMap,
        }

        let remote_apps: HashMap<Uuid, AppTarget> = HashMap::deserialize(deserializer)?;

        // validate that there is only one hostapp
        if remote_apps.iter().filter(|(_, app)| app.is_host).count() > 1 {
            return Err(serde::de::Error::custom(
                "only one target hostapp is allowed",
            ));
        }

        let mut apps = HashMap::new();
        for (app_uuid, app) in remote_apps {
            let AppTarget {
                id,
                name,
                releases,
                is_host,
            } = app;
            if !is_host {
                apps.insert(app_uuid, App::User(UserApp { id, name, releases }));
            // Only select the hostapp if it has the appropriate metadata
            } else if let Some((release_uuid, release)) = releases.into_iter().next() {
                let hostapp = release.services.into_values().find(|svc| {
                    svc.composition
                        .labels
                        .get("io.balena.image.class")
                        .map(|value| value == "hostapp")
                        .is_some()
                });

                // The target OS may be before v6.1.18 where the hostapp and board-rev
                // labels were added. If that's the case we won't be able to update to it so
                // we remove it from the target state
                if let Some(svc) = hostapp {
                    // merge top level labels with those in the composition
                    let mut labels: HashMap<String, String> = svc
                        .composition
                        .labels
                        .into_iter()
                        .chain(svc.labels)
                        .collect();

                    // The hostapp must provide an updater artifact
                    let updater = if let Some(updater) = labels.remove("io.balena.private.updater")
                    {
                        updater.parse().map_err(serde::de::Error::custom)?
                    } else {
                        return Err(serde::de::Error::custom(
                            "the hostapp must provide an updater artifact reference",
                        ));
                    };

                    if let Some(board_rev) = labels.remove("io.balena.private.hostapp.board-rev") {
                        apps.insert(
                            app_uuid,
                            App::Host(HostApp {
                                release_uuid,
                                image: svc.image,
                                board_rev,
                                updater,
                            }),
                        );
                    }
                }
            } else {
                return Err(serde::de::Error::custom(
                    "the hostapp must have at least one target release",
                ));
            }
        }

        Ok(AppMap(apps))
    }
}

#[derive(Clone, Debug)]
pub enum App {
    User(UserApp),
    Host(HostApp),
}

#[derive(Clone, Debug)]
pub struct HostApp {
    pub release_uuid: Uuid,
    pub image: ImageUri,
    pub board_rev: String,
    pub updater: ImageUri,
}

/// Target app as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct UserApp {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub releases: ReleaseMap,
}

#[derive(Clone, Debug, Default)]
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

#[derive(Deserialize, Clone, Debug)]
pub struct Release {
    #[serde(default)]
    pub services: HashMap<String, Service>,

    #[serde(default)]
    pub volumes: HashMap<String, Volume>,

    #[serde(default)]
    pub networks: HashMap<String, Network>,
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

        let apps = serde_json::from_value::<AppMap>(json);
        assert!(
            apps.is_err_and(
                |e| e.to_string() == "the hostapp must have at least one target release"
            )
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
}
