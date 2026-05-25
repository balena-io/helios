use mahler::state::{Map, State};

use crate::common_types::{HostRuntimeDir, ImageUri, OperatingSystem, Uuid};
use crate::oci::{BindPropagation, Mount};
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

impl DeviceTarget {
    /// Adds system runtime related context  to the target state
    ///
    /// Because this context is added in in the target state, if modified externally (e.g through a
    /// helios restart), they will also cause a service restart
    ///
    /// This information will also show on the state returned by helios API
    pub fn add_runtime_context(&mut self, device: &Device, host_runtime_dir: &HostRuntimeDir) {
        #[cfg(feature = "balenahup")]
        let (os_version, os_build) = if let Some(host) = &device.host {
            (Some(host.meta.to_string()), host.meta.build.clone())
        } else {
            (None, None)
        };

        for (app_uuid, app) in self.apps.iter_mut() {
            let bind_source = host_runtime_dir
                .join("locking")
                .join(app_uuid.as_str())
                .to_string_lossy()
                .into_owned();
            for rel in app.releases.values_mut() {
                for svc in rel.services.values_mut() {
                    svc.config.environment.insert(
                        "BALENA_DEVICE_UUID".to_string(),
                        Some(device.uuid.as_str().into()),
                    );

                    // Mount the per-app runtime directory at /tmp/balena so
                    // services can place files (e.g. update locks) that
                    // helios reads from the host side. Overrides any
                    // user-declared mount at the same target.
                    svc.config.volumes.retain(|m| m.target() != "/tmp/balena");
                    svc.config.volumes.push(Mount::Bind {
                        target: "/tmp/balena".to_string(),
                        source: bind_source.clone(),
                        read_only: false,
                        propagation: BindPropagation::Private,
                        create_host_path: true,
                    });

                    #[cfg(feature = "balenahup")]
                    if let Some(host_os_version) = &os_version {
                        svc.config.environment.insert(
                            "BALENA_HOST_OS_VERSION".to_string(),
                            Some(host_os_version.as_str().into()),
                        );

                        if let Some(host_os_build) = &os_build {
                            // NOTE: this replaces the old `BALENA_HOST_OS_BOARD_REV`, this should
                            // be updated on next supervisor major as well where we will remove the
                            // `io.balena.features.host-os.board-rev` and report
                            // `BALENA_HOST_OS_BUILD` which is a more correct name
                            svc.config.environment.insert(
                                "BALENA_HOST_OS_BUILD".to_string(),
                                Some(host_os_build.as_str().into()),
                            );
                        }
                    }
                }
            }
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
                #[cfg(feature = "userapps")]
                RemoteAppTarget::Rejected(app) if !app.is_host => {
                    userapps.insert(app_uuid, app.into());
                }
                _ => {}
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
                    "oci_id": "ccc",
                    "download_progress": 100,
                }

            },
        });

        let device: Device = serde_json::from_value(json).unwrap();

        // this should not panic
        let _target: DeviceTarget =
            serde_json::from_value(serde_json::to_value(device).unwrap()).unwrap();
    }

    #[cfg(feature = "userapps")]
    #[test]
    fn rejected_user_app_is_included_in_target_with_rejected_release_set() {
        // A rejected user app should land in `target.apps` with empty
        // releases and the rejected release uuid set, so the planner can
        // create the app locally and the report layer can surface it.
        let remote: crate::remote_model::Device = serde_json::from_value(json!({
            "name": "test-device",
            "apps": {
                "bad-app": {
                    "id": 7,
                    "name": "bad",
                    "releases": {
                        "bad-release": {
                            "id": 1,
                            "services": {
                                "main": {
                                    "id": 1,
                                    "image_id": 1,
                                    "image": "registry/img:1",
                                    "composition": {
                                        "command": "echo 'unterminated"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

        let target: DeviceTarget = remote.into();
        let app = target
            .apps
            .get(&Uuid::from("bad-app"))
            .expect("rejected user app should be in the target");
        assert!(app.releases.is_empty());
        assert_eq!(app.rejected_release, Some(Uuid::from("bad-release")));
    }

    #[cfg(all(feature = "userapps", feature = "balenahup"))]
    #[test]
    fn rejected_host_app_is_dropped_from_target() {
        // Rejected host apps are filtered out — the device keeps running
        // its current host release and there is nothing to report against.
        let remote: crate::remote_model::Device = serde_json::from_value(json!({
            "name": "test-device",
            "apps": {
                "bad-host": {
                    "id": 8,
                    "name": "host",
                    "is_host": true,
                    "releases": {
                        "bad-release": {
                            "id": 2,
                            "services": {
                                "hostapp": {
                                    "id": 1,
                                    "image_id": 1,
                                    "image": "registry/img:1",
                                }
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

        let target: DeviceTarget = remote.into();
        assert!(target.apps.is_empty());
        assert!(target.host.is_none());
    }
}
