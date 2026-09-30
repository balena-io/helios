use std::collections::HashMap;

use mahler::state::{List, Map, State};

use crate::common_types::{HostRuntimeDir, ImageUri, OperatingSystem, Uuid};
use crate::oci::{BindPropagation, Mount};
use crate::remote_model::{App as RemoteAppTarget, Device as RemoteDeviceTarget};

use super::app::App;
use super::image::Image;
use super::release::ReleaseTarget;
use super::volume::{LocalVolume, VolumeTarget};

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

    /// List of unsupervised volumes on the device
    #[mahler(internal, default)]
    pub volumes: List<LocalVolume>,
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
            volumes: List::new(),
        }
    }
}

impl DeviceTarget {
    /// Refuse the parts of the target this device cannot run, and say why.
    ///
    /// Runs before planning: a target with no runtime for it is bad input.
    /// Contract checks belong here once service targets carry a runtime.
    pub fn test_runtime_support(&mut self, device: &Device) -> Vec<String> {
        #[cfg(feature = "balenahup")]
        if let (Some(host_tgt), Some(host)) = (self.host.as_mut(), device.host.as_ref()) {
            return crate::balenahup::reject_unsupported_releases(host_tgt, host);
        }
        #[cfg(not(feature = "balenahup"))]
        let _ = device;
        Vec::new()
    }

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
            let app_id = app.id;
            let bind_source = host_runtime_dir
                .join("update-locks")
                .join(app_uuid.as_str())
                .to_string_lossy()
                .into_owned();
            for rel in app.releases.values_mut() {
                reuse_legacy_volumes(app_id, rel, &device.volumes);

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
                    svc.config.volumes.insert(Mount::Bind {
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

/// Reuse the volumes left behind by the legacy supervisor
///
/// Volume metadata and namespacing in helios is different from previous versions of the supervisor,
/// which means helios by default will ignore legacy volumes and will create containers with fresh state.
///
/// Legacy volumes (idenfied by the name `<appId>_<name>`) if their configuration matches the
/// target, are adopted as external volumes under their engine name, keeping its data in place.
/// Mounts referencing the legacy volume are also renamed,
///
/// A legacy volume with a different configuration is left alone and helios creates its
/// own volume for the release.
fn reuse_legacy_volumes(app_id: u32, rel: &mut ReleaseTarget, device_volumes: &[LocalVolume]) {
    // the common case is a device with nothing left behind by the legacy supervisor
    if device_volumes.is_empty() {
        return;
    }

    let legacy: HashMap<String, String> = rel
        .volumes
        .iter()
        .filter_map(|(vol_name, tgt)| {
            let VolumeTarget::Internal(config) = tgt else {
                return None;
            };
            let legacy_name = format!("{app_id}_{vol_name}");
            device_volumes
                .iter()
                .any(|vol| vol.oci_name == legacy_name && &vol.config == config)
                .then(|| (vol_name.clone(), legacy_name))
        })
        .collect();

    if legacy.is_empty() {
        return;
    }

    for (vol_name, legacy_name) in &legacy {
        rel.volumes.remove(vol_name);
        rel.volumes
            .insert(legacy_name.clone(), VolumeTarget::External);
    }

    for svc in rel.services.values_mut() {
        svc.config.volumes = std::mem::take(&mut svc.config.volumes)
            .into_iter()
            .map(|mut mount| {
                if let Mount::Volume { source, .. } = &mut mount
                    && let Some(legacy_name) = legacy.get(source)
                {
                    *source = legacy_name.clone();
                }
                mount
            })
            .collect();
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

    /// Target of a single app with one release, mounting `my-vol` from `my-service`.
    fn target_with_volume(volume: serde_json::Value) -> DeviceTarget {
        serde_json::from_value(json!({
            "apps": {
                "my-app-uuid": {
                    "id": 1234,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "my-service": {
                                    "id": 1,
                                    "image": "alpine:latest",
                                    "config": {
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-vol",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "volumes": {"my-vol": volume},
                        }
                    }
                }
            }
        }))
        .unwrap()
    }

    /// Device carrying `volumes` as unsupervised volumes.
    fn device_with_volumes(volumes: serde_json::Value) -> Device {
        serde_json::from_value(json!({
            "uuid": "device-uuid",
            "volumes": volumes,
        }))
        .unwrap()
    }

    fn release_of(target: &DeviceTarget) -> &ReleaseTarget {
        target
            .apps
            .get(&Uuid::from("my-app-uuid"))
            .unwrap()
            .releases
            .get(&Uuid::from("my-release-uuid"))
            .unwrap()
    }

    /// Sources of the volume mounts of `my-service`.
    fn mount_sources(target: &DeviceTarget) -> Vec<String> {
        release_of(target)
            .services
            .get("my-service")
            .unwrap()
            .config
            .volumes
            .iter()
            .filter_map(|mount| match mount {
                Mount::Volume { source, .. } => Some(source.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn legacy_volume_with_matching_config_is_adopted_as_external() {
        let mut target = target_with_volume(json!({"config": {"driver": "local"}}));
        let device = device_with_volumes(json!([
            {"oci_name": "1234_my-vol", "config": {"driver": "local"}}
        ]));

        target.add_runtime_context(&device, &HostRuntimeDir("/run/helios".into()));

        let volumes = &release_of(&target).volumes;
        assert!(!volumes.contains_key("my-vol"));
        assert_eq!(volumes.get("1234_my-vol"), Some(&VolumeTarget::External));
        // the mount has to name the legacy volume, as that is how the engine
        // identifies an external volume
        assert_eq!(mount_sources(&target), vec!["1234_my-vol".to_string()]);
    }

    #[test]
    fn legacy_volume_with_a_different_config_is_left_alone() {
        let mut target = target_with_volume(json!({
            "config": {"driver": "local", "driver_opts": {"type": "tmpfs"}}
        }));
        let device = device_with_volumes(json!([
            {"oci_name": "1234_my-vol", "config": {"driver": "local"}}
        ]));

        target.add_runtime_context(&device, &HostRuntimeDir("/run/helios".into()));

        let volumes = &release_of(&target).volumes;
        assert!(volumes.contains_key("my-vol"));
        assert!(!volumes.contains_key("1234_my-vol"));
        assert_eq!(mount_sources(&target), vec!["my-vol".to_string()]);
    }

    #[test]
    fn volumes_of_other_apps_are_not_adopted() {
        let mut target = target_with_volume(json!({"config": {"driver": "local"}}));
        let device = device_with_volumes(json!([
            {"oci_name": "4321_my-vol", "config": {"driver": "local"}}
        ]));

        target.add_runtime_context(&device, &HostRuntimeDir("/run/helios".into()));

        let volumes = &release_of(&target).volumes;
        assert!(volumes.contains_key("my-vol"));
        assert_eq!(mount_sources(&target), vec!["my-vol".to_string()]);
    }
}
