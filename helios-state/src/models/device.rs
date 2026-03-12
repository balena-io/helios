use mahler::state::{Map, State};

use crate::common_types::{ImageUri, OperatingSystem, Uuid};
use crate::labels::LABEL_APP_UUID;
use crate::remote_model::{App as RemoteAppTarget, Device as RemoteDeviceTarget};

use super::app::App;
use super::image::Image;

#[cfg(feature = "balenahup")]
use crate::balenahup::Host;

/// The current state of a device that will be stored
/// by the worker
#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Device {
    /// The device UUID
    #[mahler(internal)]
    pub uuid: Uuid,

    /// The device name on the remote
    pub name: Option<String>,

    /// List of docker images on the device
    #[mahler(internal)]
    pub images: Map<ImageUri, Image>,

    /// Apps on the device
    pub apps: Map<Uuid, App>,

    /// The "hostapp" configuration
    #[cfg(feature = "balenahup")]
    pub host: Option<Host>,
}

impl Default for DeviceTarget {
    fn default() -> Self {
        DeviceTarget {
            name: None,
            apps: Map::new(),
            #[cfg(feature = "balenahup")]
            host: None,
        }
    }
}

impl Device {
    pub fn new(uuid: Uuid, os: Option<OperatingSystem>) -> Self {
        #[cfg(not(feature = "balenahup"))]
        let _ = os;
        Self {
            uuid,
            name: None,
            images: Map::new(),
            apps: Map::new(),
            #[cfg(feature = "balenahup")]
            host: os.map(Host::new),
        }
    }
}

impl From<Device> for DeviceTarget {
    fn from(device: Device) -> Self {
        let Device {
            name,
            apps,
            #[cfg(feature = "balenahup")]
            host,
            uuid: _,
            images: _,
            ..
        } = device;
        Self {
            name,
            apps: apps
                .into_iter()
                .map(|(uuid, app)| (uuid, app.into()))
                .collect(),
            #[cfg(feature = "balenahup")]
            host: host.map(|r| r.into()),
        }
    }
}

impl DeviceTarget {
    pub fn normalize(mut self) -> DeviceTarget {
        // Insert app metadata for every service of every app
        for (app_uuid, app) in self.apps.iter_mut() {
            for (rel_uuid, rel) in app.releases.iter_mut() {
                for (svc_name, svc) in rel.services.iter_mut() {
                    svc.container_name = Some(format!("{svc_name}_{rel_uuid}"));
                }

                for (net_key, net) in rel.networks.iter_mut() {
                    net.config
                        .labels
                        .insert(LABEL_APP_UUID.to_string(), app_uuid.to_string());
                    net.network_name = format!("{app_uuid}_{net_key}");
                }

                for (vol_key, vol) in rel.volumes.iter_mut() {
                    vol.config
                        .labels
                        .insert(LABEL_APP_UUID.to_string(), app_uuid.to_string());
                    vol.volume_name = format!("{app_uuid}_{vol_key}");
                }
            }
        }

        self
    }
}

impl From<RemoteDeviceTarget> for DeviceTarget {
    fn from(tgt: RemoteDeviceTarget) -> Self {
        let RemoteDeviceTarget { name, apps, .. } = tgt;

        #[cfg(feature = "userapps")]
        let mut userapps = Map::new();
        #[cfg(feature = "balenahup")]
        let mut hostapps = Vec::new();
        for (app_uuid, app) in apps {
            match app {
                // Read the hostapp info if it exists and the feature is enabled
                #[cfg(feature = "balenahup")]
                RemoteAppTarget::Host(hostapp) => {
                    hostapps.push((app_uuid, hostapp).into());
                }
                #[cfg(not(feature = "balenahup"))]
                RemoteAppTarget::Host(_) => {}
                // Read the userapp info if it exists and the feature is enabled
                #[cfg(feature = "userapps")]
                RemoteAppTarget::User(userapp) => {
                    userapps.insert(app_uuid, userapp.into());
                }
                #[cfg(not(feature = "userapps"))]
                RemoteAppTarget::User(_) => {}
            };
        }

        Self {
            name: Some(name),
            #[cfg(feature = "userapps")]
            apps: userapps,
            #[cfg(not(feature = "userapps"))]
            apps: Map::new(),
            // Get only the first hostapp if any
            #[cfg(feature = "balenahup")]
            host: hostapps.pop(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalize_applies_app_uuid_label_to_default_network() {
        let target: DeviceTarget = serde_json::from_value(json!({
            "apps": {
                "app1": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "rel1": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "default": {}
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

        let target = target.normalize();
        let app = target.apps.get(&"app1".into()).unwrap();
        let rel = app.releases.get(&"rel1".into()).unwrap();
        let default_net = rel.networks.get("default").unwrap();
        assert_eq!(
            default_net.config.labels.get("io.balena.app-uuid"),
            Some(&"app1".to_string())
        );
    }

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
                    "engine_id": "ccc",
                    "download_progress": 100,
                }

            },
        });

        let device: Device = serde_json::from_value(json).unwrap();

        // this should not panic
        let _target: DeviceTarget =
            serde_json::from_value(serde_json::to_value(device).unwrap()).unwrap();
    }
}
