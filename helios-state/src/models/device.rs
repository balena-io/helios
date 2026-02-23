use mahler::state::{Map, State};

use crate::common_types::{ImageUri, OperatingSystem, Uuid};
use crate::labels::{LABEL_APP_UUID, LABEL_SERVICE_NAME};
use crate::remote_model::{App as RemoteAppTarget, Device as RemoteDeviceTarget};

use super::app::App;
use super::host::Host;
use super::image::Image;
use super::network::Network;

/// The current state of a device that will be stored
/// by the worker
#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Device {
    /// The device UUID
    #[mahler(internal)]
    pub uuid: Uuid,

    pub name: Option<String>,

    /// List of docker images on the device
    #[mahler(internal)]
    pub images: Map<ImageUri, Image>,

    /// Apps on the device
    pub apps: Map<Uuid, App>,

    /// The "hostapp" configuration
    pub host: Option<Host>,
}

impl Default for DeviceTarget {
    fn default() -> Self {
        DeviceTarget {
            name: None,
            apps: Map::new(),
            host: None,
        }
    }
}

impl Device {
    pub fn new(uuid: Uuid, os: Option<OperatingSystem>) -> Self {
        Self {
            uuid,
            name: None,
            images: Map::new(),
            apps: Map::new(),
            host: os.map(Host::new),
        }
    }
}

impl From<Device> for DeviceTarget {
    fn from(device: Device) -> Self {
        let Device {
            name,
            apps,
            host,
            uuid: _,
            images: _,
        } = device;
        Self {
            name,
            apps: apps
                .into_iter()
                .map(|(uuid, app)| (uuid, app.into()))
                .collect(),
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
                    svc.container_name = format!("{svc_name}_{rel_uuid}");
                    svc.config
                        .labels
                        .insert(LABEL_APP_UUID.to_string(), app_uuid.to_string());
                    svc.config
                        .labels
                        .insert(LABEL_SERVICE_NAME.to_string(), svc_name.clone());
                }

                // Ensure every release has an implicit "default" network
                rel.networks
                    .entry("default".to_string())
                    .or_insert_with(Network::default);

                for (net_key, net) in rel.networks.iter_mut() {
                    net.config
                        .labels
                        .insert(LABEL_APP_UUID.to_string(), app_uuid.to_string());
                    net.network_name = format!("{app_uuid}_{net_key}");
                }
            }
        }

        self
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
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalize_injects_default_network() {
        let target: DeviceTarget = serde_json::from_value(json!({
            "apps": {
                "app1": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "rel1": {
                            "installed": true,
                            "services": {},
                            "networks": {}
                        }
                    }
                }
            }
        }))
        .unwrap();

        let target = target.normalize();
        let app = target.apps.get(&"app1".into()).unwrap();
        let rel = app.releases.get(&"rel1".into()).unwrap();
        assert!(rel.networks.contains_key("default"));
        assert_eq!(
            rel.networks
                .get("default")
                .unwrap()
                .config
                .driver
                .to_string(),
            "bridge"
        );
        assert_eq!(
            rel.networks.get("default").unwrap().network_name,
            "app1_default"
        );
    }

    #[test]
    fn normalize_does_not_overwrite_explicit_default_network() {
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
                                "default": {
                                    "config": {
                                        "driver": "overlay",
                                        "driver_opts": {},
                                        "enable_ipv6": false,
                                        "internal": false,
                                        "labels": {},
                                        "ipam": {
                                            "driver": "default",
                                            "config": [],
                                            "options": {}
                                        }
                                    }
                                }
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
        assert_eq!(
            rel.networks
                .get("default")
                .unwrap()
                .config
                .driver
                .to_string(),
            "overlay"
        );
    }

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
                            "networks": {}
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
