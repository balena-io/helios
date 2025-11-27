use std::collections::{BTreeMap, HashSet};

use mahler::State;
use serde::{Deserialize, Serialize};

use crate::common_types::{ImageUri, OperatingSystem, Uuid};
use crate::oci::RegistryAuth;
use crate::remote_types::{AppTarget as RemoteAppTarget, DeviceTarget as RemoteDeviceTarget};

use super::app::App;
use super::host::Host;
use super::image::Image;

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

    /// The "hostapp" configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub host: Option<Host>,

    #[serde(default)]
    pub needs_cleanup: bool,
}

impl Device {
    pub fn new(uuid: Uuid, os: Option<OperatingSystem>) -> Self {
        Self {
            uuid,
            name: None,
            auths: RegistryAuthSet::new(),
            images: BTreeMap::new(),
            apps: BTreeMap::new(),
            host: os.map(Host::new),
            needs_cleanup: false,
        }
    }
}

impl From<Device> for DeviceTarget {
    fn from(device: Device) -> Self {
        let Device {
            name,
            apps,
            host,
            needs_cleanup,
            ..
        } = device;
        Self {
            name,
            apps,
            host: host.map(|r| r.into()),
            needs_cleanup,
        }
    }
}

impl From<RemoteDeviceTarget> for DeviceTarget {
    fn from(tgt: RemoteDeviceTarget) -> Self {
        let RemoteDeviceTarget { name, apps, .. } = tgt;

        let mut userapps = BTreeMap::new();
        let mut hostapps = Vec::new();
        for (app_uuid, app) in apps {
            match app {
                // Read the hostapp info if it exists and the feature is enabled
                RemoteAppTarget::Host(hostapp) => {
                    if cfg!(feature = "balenahup") {
                        hostapps.push((app_uuid, hostapp).into());
                    }
                }
                // Read the userapp info if it exists and the feature is enabled
                RemoteAppTarget::User(userapp) => {
                    if cfg!(feature = "userapps") {
                        userapps.insert(app_uuid, userapp.into());
                    }
                }
            };
        }

        // Get only the first hostapp if any
        let hostapp = hostapps.pop();

        Self {
            name: Some(name),
            apps: userapps,
            host: hostapp,
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
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "6.5.4",
                }
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
