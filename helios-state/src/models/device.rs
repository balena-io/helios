use std::collections::{BTreeMap, HashSet};

use mahler::State;
use serde::{Deserialize, Serialize};

use crate::oci::RegistryAuth;
use crate::remote_types::DeviceTarget as RemoteDeviceTarget;
use crate::util::types::{OperatingSystem, Uuid};

use super::app::App;
use super::image::{Image, ImageUri};

pub type RegistryAuthSet = HashSet<RegistryAuth>;

/// The current state of a device that will be stored
/// by the worker
#[derive(State, Serialize, Deserialize, Debug, Clone)]
#[mahler(derive(PartialEq, Eq, Default))]
pub struct Device {
    /// The device UUID
    #[mahler(internal)]
    pub uuid: Uuid,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[mahler(internal)]
    pub os: Option<OperatingSystem>,

    #[serde(default)]
    #[mahler(internal)]
    pub auths: RegistryAuthSet,

    /// List of docker images on the device
    #[serde(default)]
    #[mahler(internal)]
    pub images: BTreeMap<ImageUri, Image>,

    /// Apps on the device
    #[serde(default)]
    pub apps: BTreeMap<Uuid, App>,

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
            needs_cleanup: false,
        }
    }
}

impl From<Device> for DeviceTarget {
    fn from(device: Device) -> Self {
        let Device {
            name,
            apps,
            needs_cleanup,
            ..
        } = device;
        Self {
            name,
            apps,
            needs_cleanup,
        }
    }
}

impl From<RemoteDeviceTarget> for DeviceTarget {
    fn from(tgt: RemoteDeviceTarget) -> Self {
        let RemoteDeviceTarget { name, apps, .. } = tgt;

        Self {
            name: Some(name),
            apps: apps
                .into_iter()
                // filter host apps for now
                // FIXME: implement hostapp support
                .filter(|(_, app)| !app.is_host)
                .map(|(uuid, app)| (uuid, app.into()))
                .collect(),
            needs_cleanup: false,
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
        let _target: DeviceTarget =
            serde_json::from_value(serde_json::to_value(device).unwrap()).unwrap();
    }
}
