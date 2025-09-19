use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

use crate::util::types::Uuid;

use super::{ReleaseMap, TargetReleaseMap};

/// The internal state of the app
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct App {
    pub id: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default)]
    pub releases: ReleaseMap,
}

// Target app definition, used as input to the worker
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct TargetApp {
    /// App id on the remote backend. This only exists for legacy reasons
    /// and should be removed at some point.
    ///
    /// We use 0 as the default to not make the id required
    #[serde(default)]
    pub id: u32,

    /// The target app name
    ///
    /// while this value is technically never null, we need to be able to
    /// convert from App into TargetApp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Target app releases
    #[serde(default)]
    pub releases: TargetReleaseMap,
}

/// Target apps model
///
/// This type exists for controlling/validating deserialization of target apps
#[derive(Debug, Serialize, Default, Clone, PartialEq, Eq)]
pub struct TargetAppMap(BTreeMap<Uuid, TargetApp>);

impl Deref for TargetAppMap {
    type Target = BTreeMap<Uuid, TargetApp>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for TargetAppMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'de> Deserialize<'de> for TargetAppMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let apps_value: BTreeMap<Uuid, Value> = BTreeMap::deserialize(deserializer)?;

        let mut target_apps = BTreeMap::new();

        for (uuid, app_value) in apps_value {
            let is_host = app_value
                .get("is_host")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if !is_host {
                let target_app =
                    TargetApp::deserialize(app_value).map_err(serde::de::Error::custom)?;
                target_apps.insert(uuid, target_app);
            }
        }

        Ok(TargetAppMap(target_apps))
    }
}

impl FromIterator<(Uuid, TargetApp)> for TargetAppMap {
    fn from_iter<T: IntoIterator<Item = (Uuid, TargetApp)>>(iter: T) -> Self {
        Self(BTreeMap::from_iter(iter))
    }
}

impl From<App> for TargetApp {
    fn from(app: App) -> Self {
        let App { id, name, releases } = app;
        Self {
            id,
            name,
            releases: releases
                .into_iter()
                .map(|(commit, rel)| (commit, rel.into()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deserialize_target_apps_filters_host_apps() {
        let user_uuid = Uuid::from("user-uuid");
        let host_uuid = Uuid::from("hostapp-uuid");
        let another_user_uuid = Uuid::from("other-app-uuid");

        let json = json!({
                "user-uuid": {
                    "id": 123,
                    "name": "user-app"
                },
                "hostapp-uuid": {
                    "name": "host-app",
                    "is_host": true
                },
                "other-app-uuid": {
                    "id": 456,
                    "name": "another-user-app",
                    "is_host": false
                }
        });

        let target_apps: TargetAppMap = serde_json::from_value(json).unwrap();

        assert_eq!(target_apps.len(), 2);
        assert!(target_apps.contains_key(&user_uuid));
        assert!(target_apps.contains_key(&another_user_uuid));
        assert!(!target_apps.contains_key(&host_uuid));

        let user_app = target_apps.get(&user_uuid).unwrap();
        assert_eq!(user_app.name, Some("user-app".into()));

        let another_user_app = target_apps.get(&another_user_uuid).unwrap();
        assert_eq!(another_user_app.name, Some("another-user-app".into()));
    }

    #[test]
    fn test_deserialize_target_apps_no_host_apps() {
        let app1_uuid = Uuid::from("user-uuid".to_string());
        let app2_uuid = Uuid::from("hostapp-uuid".to_string());

        let json = json!({
                "user-uuid": {
                    "name": "app-1",
                },
                "hostapp-uuid": {
                    "name": "app-2"
                }
        });

        let target_apps: TargetAppMap = serde_json::from_value(json).unwrap();

        assert_eq!(target_apps.len(), 2);
        assert!(target_apps.contains_key(&app1_uuid));
        assert!(target_apps.contains_key(&app2_uuid));
    }

    #[test]
    fn test_deserialize_target_apps_all_host_apps() {
        let json = json!({
                "user-uuid": {
                    "name": "host-1",
                    "is_host": true
                },
                "hostapp-uuid": {
                    "name": "host-2",
                    "is_host": true
                }
        });

        let target_apps: TargetAppMap = serde_json::from_value(json).unwrap();

        assert_eq!(target_apps.len(), 0);
    }

    #[test]
    fn test_deserialize_target_apps_empty() {
        let json = json!({});

        let target_apps: TargetAppMap = serde_json::from_value(json).unwrap();

        assert_eq!(target_apps.len(), 0);
    }
}
