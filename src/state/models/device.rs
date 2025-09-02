use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

use crate::remote::RegistryAuth;
use crate::types::{OperatingSystem, Uuid};
use crate::util::docker::ImageUri;

use super::app::{App, TargetAppMap};
use super::image::Image;

pub type DeviceConfig = BTreeMap<String, String>;

pub type RegistryAuthSet = HashSet<RegistryAuth>;

/// The current state of a device that will be stored
/// by the worker
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Device {
    /// The device UUID
    pub uuid: Uuid,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<OperatingSystem>,

    #[serde(default)]
    pub auths: RegistryAuthSet,

    /// List of docker images on the device
    #[serde(default)]
    pub images: BTreeMap<ImageUri, Image>,

    /// Apps on the device
    #[serde(default)]
    pub apps: BTreeMap<Uuid, App>,

    /// Config vars
    #[serde(default)]
    pub config: DeviceConfig,

    #[serde(default)]
    pub needs_cleanup: bool,
}

impl Device {
    pub fn new(uuid: Uuid, os: Option<OperatingSystem>) -> Self {
        Self {
            uuid,
            name: None,
            os,
            auths: RegistryAuthSet::new(),
            images: BTreeMap::new(),
            apps: BTreeMap::new(),
            config: BTreeMap::new(),
            needs_cleanup: false,
        }
    }
}

/// Target state of a device
///
/// For Mahler, the target must be a structural subtype of the internal
/// state, meaning the state should be serializable into the target.
/// If they don't, planning will fail
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct TargetDevice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default)]
    pub apps: TargetAppMap,

    #[serde(default)]
    pub config: DeviceConfig,

    #[serde(default)]
    pub needs_cleanup: bool,
}

impl From<Device> for TargetDevice {
    fn from(device: Device) -> Self {
        let Device {
            name,
            apps,
            config,
            needs_cleanup,
            ..
        } = device;
        Self {
            name,
            apps: apps
                .into_iter()
                .map(|(uuid, app)| (uuid, app.into()))
                .collect(),
            config,
            needs_cleanup,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    #[test]
    fn device_state_should_be_serializable_into_target() {
        let json = json!({
            "uuid": "device-uuid",
            "os": {
                "name": "balenaOS",
                "version": "6.5.4",
            },
            "apps": {
                "aaa": {
                    "name": "my-app",
                    "id": 123
                },
                "bbb": {
                    "name": "other-app",
                    "id": 123
                }
            },

            "images": {
                "ubuntu": {
                    "engine_id": "ccc"
                }

            }
        });

        let device: Device = serde_json::from_value(json).unwrap();

        // this should not panic
        let _target: TargetDevice =
            serde_json::from_value(serde_json::to_value(device).unwrap()).unwrap();
    }
}
