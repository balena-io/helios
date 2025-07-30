use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use crate::types::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Image {
    pub docker_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct App {
    pub name: String,
}

pub type DeviceConfig = HashMap<String, String>;

/// Current state of a device
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Device {
    /// The device UUID
    pub uuid: Uuid,

    /// List of docker images on the device
    #[serde(default)]
    pub images: HashMap<String, Image>,

    /// Apps on the device
    #[serde(default)]
    pub apps: HashMap<Uuid, App>,

    /// Config vars
    #[serde(default)]
    pub config: DeviceConfig,
}

impl Device {
    /// Read the host and apps state from the underlying system
    pub fn initial_for(uuid: Uuid) -> Self {
        // TODO: read initial state from the engine
        Self {
            uuid,
            images: HashMap::new(),
            apps: HashMap::new(),
            config: HashMap::new(),
        }
    }
}

// Alias the App for now, the target app will have
// its own structure eventually
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TargetApp {
    pub name: String,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct TargetApps(HashMap<Uuid, TargetApp>);

impl Deref for TargetApps {
    type Target = HashMap<Uuid, TargetApp>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for TargetApps {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'de> Deserialize<'de> for TargetApps {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let apps_value: HashMap<Uuid, Value> = HashMap::deserialize(deserializer)?;

        let mut target_apps = HashMap::new();

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

        Ok(TargetApps(target_apps))
    }
}

impl FromIterator<(Uuid, TargetApp)> for TargetApps {
    fn from_iter<T: IntoIterator<Item = (Uuid, TargetApp)>>(iter: T) -> Self {
        Self(HashMap::from_iter(iter))
    }
}

/// Target state of a device
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TargetDevice {
    pub apps: TargetApps,
    #[serde(default)]
    pub config: DeviceConfig,
}

impl From<App> for TargetApp {
    fn from(app: App) -> Self {
        let App { name } = app;
        Self { name }
    }
}

impl From<Device> for TargetDevice {
    fn from(device: Device) -> Self {
        let Device { apps, config, .. } = device;
        Self {
            apps: apps
                .into_iter()
                .map(|(uuid, app)| (uuid, app.into()))
                .collect(),
            config,
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
            "apps": {
                "user-uuid": {
                    "name": "user-app"
                },
                "hostapp-uuid": {
                    "name": "host-app",
                    "is_host": true
                },
                "other-app-uuid": {
                    "name": "another-user-app",
                    "is_host": false
                }
            }
        });

        let target_device: TargetDevice = serde_json::from_value(json).unwrap();

        assert_eq!(target_device.apps.len(), 2);
        assert!(target_device.apps.contains_key(&user_uuid));
        assert!(target_device.apps.contains_key(&another_user_uuid));
        assert!(!target_device.apps.contains_key(&host_uuid));

        let user_app = target_device.apps.get(&user_uuid).unwrap();
        assert_eq!(user_app.name, "user-app");

        let another_user_app = target_device.apps.get(&another_user_uuid).unwrap();
        assert_eq!(another_user_app.name, "another-user-app");
    }

    #[test]
    fn test_deserialize_target_apps_no_host_apps() {
        let app1_uuid = Uuid::from("user-uuid".to_string());
        let app2_uuid = Uuid::from("hostapp-uuid".to_string());

        let json = json!({
            "apps": {
                "user-uuid": {
                    "name": "app-1"
                },
                "hostapp-uuid": {
                    "name": "app-2"
                }
            }
        });

        let target_device: TargetDevice = serde_json::from_value(json).unwrap();

        assert_eq!(target_device.apps.len(), 2);
        assert!(target_device.apps.contains_key(&app1_uuid));
        assert!(target_device.apps.contains_key(&app2_uuid));
    }

    #[test]
    fn test_deserialize_target_apps_all_host_apps() {
        let json = json!({
            "apps": {
                "user-uuid": {
                    "name": "host-1",
                    "is_host": true
                },
                "hostapp-uuid": {
                    "name": "host-2",
                    "is_host": true
                }
            }
        });

        let target_device: TargetDevice = serde_json::from_value(json).unwrap();

        assert_eq!(target_device.apps.len(), 0);
    }

    #[test]
    fn test_deserialize_target_apps_empty() {
        let json = json!({
            "apps": {}
        });

        let target_device: TargetDevice = serde_json::from_value(json).unwrap();

        assert_eq!(target_device.apps.len(), 0);
    }
}
