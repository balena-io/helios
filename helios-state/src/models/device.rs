use std::collections::HashSet;

use mahler::state::{Map, State};

use crate::common_types::{ImageUri, OperatingSystem, Uuid};
use crate::oci::RegistryAuth;
use crate::remote_model::{App as RemoteAppTarget, Device as RemoteDeviceTarget};

use super::app::App;
use super::host::Host;
use super::image::Image;

pub type RegistryAuthSet = HashSet<RegistryAuth>;

/// The current state of a device that will be stored
/// by the worker
#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Device {
    /// The device UUID
    #[mahler(internal)]
    pub uuid: Uuid,

    pub name: Option<String>,

    #[mahler(internal)]
    pub auths: RegistryAuthSet,

    /// List of docker images on the device
    #[mahler(internal)]
    pub images: Map<ImageUri, Image>,

    /// Apps on the device
    pub apps: Map<Uuid, App>,

    /// The "hostapp" configuration
    pub host: Option<Host>,

    /// A cleanup sttep is needed
    pub needs_cleanup: bool,
}

impl Default for DeviceTarget {
    fn default() -> Self {
        DeviceTarget {
            name: None,
            apps: Map::new(),
            host: None,
            needs_cleanup: false,
        }
    }
}

impl Device {
    pub fn new(uuid: Uuid, os: Option<OperatingSystem>) -> Self {
        Self {
            uuid,
            name: None,
            auths: RegistryAuthSet::new(),
            images: Map::new(),
            apps: Map::new(),
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
            uuid: _,
            auths: _,
            images: _,
        } = device;
        Self {
            name,
            apps: apps
                .into_iter()
                .map(|(uuid, app)| (uuid, app.into()))
                .collect(),
            host: host.map(|r| r.into()),
            needs_cleanup,
        }
    }
}

impl From<RemoteDeviceTarget> for DeviceTarget {
    fn from(tgt: RemoteDeviceTarget) -> Self {
        let RemoteDeviceTarget { name, apps, .. } = tgt;

        let mut userapps = Map::new();
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
            "auths": [],
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
                    "engine_id": "ccc",
                    "download_progress": 100,
                }

            },
            "needs_cleanup": false
        });

        let device: Device = serde_json::from_value(json).unwrap();

        // this should not panic
        let _target: DeviceTarget =
            serde_json::from_value(serde_json::to_value(device).unwrap()).unwrap();
    }
}
