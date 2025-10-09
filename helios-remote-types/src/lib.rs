//! Target types received from the remote backend
//!
//! All input validations should happen here
//!
//! the types are based  from https://github.com/balena-io/open-balena-api/blob/master/src/features/device-state/routes/state-get-v3.ts#L48

use serde::Deserialize;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use helios_util::types::{ImageUri, Uuid};

/// Target device as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct DeviceTarget {
    pub name: String,

    #[serde(default)]
    pub apps: AppTargetMap,
}

#[derive(Clone, Debug, Default)]
pub struct AppTargetMap(HashMap<Uuid, AppTarget>);

impl Deref for AppTargetMap {
    type Target = HashMap<Uuid, AppTarget>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for AppTargetMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for AppTargetMap {
    type Item = (Uuid, AppTarget);
    type IntoIter = std::collections::hash_map::IntoIter<Uuid, AppTarget>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
impl<'de> Deserialize<'de> for AppTargetMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize, Clone, Debug)]
        struct RemoteAppTarget {
            pub id: u32,
            pub name: String,
            #[serde(default)]
            pub is_host: bool,
            #[serde(default)]
            pub releases: ReleaseTargetMap,
        }

        let remote_apps: HashMap<Uuid, RemoteAppTarget> = HashMap::deserialize(deserializer)?;

        // validate that there is only one hostapp
        if remote_apps.iter().filter(|(_, app)| app.is_host).count() > 1 {
            return Err(serde::de::Error::custom(
                "only one target hostapp is allowed",
            ));
        }

        let mut apps = HashMap::new();
        for (app_uuid, app) in remote_apps {
            let RemoteAppTarget {
                id,
                name,
                releases,
                is_host,
            } = app;
            if !is_host {
                apps.insert(
                    app_uuid,
                    AppTarget::User(UserAppTarget { id, name, releases }),
                );
            // Only select the hostapp if it has the appropriate metadata
            } else if let Some((release_uuid, release)) = releases.into_iter().next() {
                let hostapp = release.services.into_values().find(|svc| {
                    svc.labels
                        .get("io.balena.image.class")
                        .map(|value| value == "hostapp")
                        .is_some()
                });

                // The target OS may be before v6.1.18 where the hostapp and board-rev
                // labels were added. If that's the case we won't be able to update to it so
                // we remove it from the target state
                if let Some(mut svc) = hostapp {
                    if let Some(board_rev) =
                        svc.labels.remove("io.balena.private.hostapp.board-rev")
                    {
                        apps.insert(
                            app_uuid,
                            AppTarget::Host(HostAppTarget {
                                release_uuid,
                                image: svc.image,
                                board_rev,
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

        Ok(AppTargetMap(apps))
    }
}

#[derive(Clone, Debug)]
pub enum AppTarget {
    User(UserAppTarget),
    Host(HostAppTarget),
}

#[derive(Clone, Debug)]
pub struct HostAppTarget {
    pub release_uuid: Uuid,
    pub image: ImageUri,
    pub board_rev: String,
}

/// Target app as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct UserAppTarget {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub releases: ReleaseTargetMap,
}

#[derive(Clone, Debug, Default)]
pub struct ReleaseTargetMap(HashMap<Uuid, ReleaseTarget>);

impl Deref for ReleaseTargetMap {
    type Target = HashMap<Uuid, ReleaseTarget>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ReleaseTargetMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for ReleaseTargetMap {
    type Item = (Uuid, ReleaseTarget);
    type IntoIter = std::collections::hash_map::IntoIter<Uuid, ReleaseTarget>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'de> Deserialize<'de> for ReleaseTargetMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let releases: HashMap<Uuid, ReleaseTarget> = HashMap::deserialize(deserializer)?;

        if releases.len() > 1 {
            return Err(serde::de::Error::custom(
                "target releases should only contain one release",
            ));
        }

        Ok(Self(releases))
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct ReleaseTarget {
    #[serde(default)]
    pub services: HashMap<String, ServiceTarget>,

    #[serde(default)]
    pub volumes: HashMap<String, VolumeTarget>,

    #[serde(default)]
    pub networks: HashMap<String, NetworkTarget>,
}

/// Target app as defined by the remote backend
// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug)]
pub struct ServiceTarget {
    pub id: u32,
    pub image: ImageUri,

    #[serde(default)]
    pub labels: HashMap<String, String>,
}

// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug)]
pub struct VolumeTarget;

// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug)]
pub struct NetworkTarget;
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_accepts_empty_releases() {
        let json = json!({});

        let releases: ReleaseTargetMap = serde_json::from_value(json).unwrap();
        assert_eq!(releases.len(), 0)
    }

    #[test]
    fn test_accepts_single_release() {
        let json = json!({
            "release-one": {}
        });

        let releases: ReleaseTargetMap = serde_json::from_value(json).unwrap();
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

        let release = serde_json::from_value::<ReleaseTargetMap>(json);
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

        let apps = serde_json::from_value::<AppTargetMap>(json);
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

        let apps = serde_json::from_value::<AppTargetMap>(json);
        assert!(
            apps.is_err_and(
                |e| e.to_string() == "the hostapp must have at least one target release"
            )
        );
    }
}
