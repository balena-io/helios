use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::Uuid;

use super::app::{App, TargetAppMap};
use super::image::Image;

pub type DeviceConfig = HashMap<String, String>;

/// The current state of a device that will be stored
/// by the worker
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
    pub fn new(uuid: Uuid) -> Self {
        Self {
            uuid,
            images: HashMap::new(),
            apps: HashMap::new(),
            config: HashMap::new(),
        }
    }
}

/// Target state of a device
///
/// For Mahler, the target must be a structural subtype of the internal
/// state, meaning the state should be serializable into the target.
/// If they don't, planning will fail
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TargetDevice {
    pub apps: TargetAppMap,
    #[serde(default)]
    pub config: DeviceConfig,
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
    use serde_json::json;

    use super::*;
    #[test]
    fn device_state_should_be_serializable_into_target() {
        let json = json!({
            "uuid": "device-uuid",
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
