use std::collections::HashMap;

use mahler::state::{List, Map, State};

use crate::common_types::{HostRuntimeDir, ImageUri, OperatingSystem, Uuid};
use crate::oci::{self, BindPropagation, Mount};
use crate::remote_model::{App as RemoteAppTarget, Device as RemoteDeviceTarget};

use super::app::App;
use super::image::Image;
use super::release::ReleaseTarget;
use super::volume::{LocalVolume, VolumeTarget};

#[cfg(any(feature = "userapps", feature = "balenahup"))]
use super::app::AppTarget;
#[cfg(feature = "balenahup")]
use crate::balenahup::Host;
#[cfg(any(feature = "userapps", feature = "balenahup"))]
use crate::remote_model::rejected_release_message;

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
    /// Reject the target releases this device cannot meet because of unmet requirements
    ///
    /// For now this function only validates OCI runtime support but in the future it will check for
    /// different types of requirements (including those defined in contracts)
    ///
    /// Runs before planning: A rejected release is replaced by an app with the release set as
    /// `rejected_release`, the same as a release that failed to parse.
    pub fn reject_unsupported_releases(
        &mut self,
        device: &Device,
        runtimes: &[oci::Runtime],
    ) -> Vec<String> {
        #[cfg_attr(
            not(any(feature = "userapps", feature = "balenahup")),
            allow(unused_mut)
        )]
        let mut reasons = Vec::new();

        #[cfg(feature = "userapps")]
        for (app_uuid, app) in self.apps.iter_mut() {
            let unsupported = app.releases.iter().find_map(|(rel_uuid, rel)| {
                Some((rel_uuid.clone(), unsupported_runtime(rel, runtimes)?))
            });

            if let Some((rel_uuid, reason)) = unsupported {
                reasons.push(rejected_release_message(app_uuid, &rel_uuid, &reason));
                *app = AppTarget::rejected(app.id, app.name.take(), rel_uuid);
            }
        }

        #[cfg(feature = "balenahup")]
        if let (Some(host_tgt), Some(_)) = (self.host.as_mut(), device.host.as_ref()) {
            for rejected in crate::balenahup::reject_unsupported_releases(host_tgt, runtimes) {
                reasons.push(rejected_release_message(
                    &rejected.app,
                    &rejected.release,
                    &rejected.reason,
                ));
                // The host target does not carry the app id and name. Neither is
                // read for a host app, the same as for apps restored from the
                // local store with an unknown id
                self.apps
                    .insert(rejected.app, AppTarget::rejected(0, None, rejected.release));
            }
        }
        #[cfg(not(feature = "balenahup"))]
        let _ = device;
        #[cfg(not(any(feature = "userapps", feature = "balenahup")))]
        let _ = runtimes;

        reasons
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
                        propagation: BindPropagation::Rprivate,
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

/// Describe the first service of the release asking for a runtime missing from
/// `runtimes`, if any
#[cfg(feature = "userapps")]
fn unsupported_runtime(rel: &ReleaseTarget, runtimes: &[oci::Runtime]) -> Option<String> {
    rel.services.iter().find_map(|(svc_name, svc)| {
        let runtime = svc.config.runtime.as_ref()?;
        (!runtimes.contains(runtime)).then(|| {
            format!(
                "service '{svc_name}' requires runtime '{runtime}', which is not available on the device"
            )
        })
    })
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

        #[cfg(any(feature = "userapps", feature = "balenahup"))]
        let mut target_apps = Map::new();
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
                    target_apps.insert(app_uuid, userapp.into());
                }
                #[cfg(not(feature = "userapps"))]
                RemoteAppTarget::User(_) => {}
                // Rejected apps are kept with the rejected release set, so the
                // current release is preserved and the rejection is reported
                #[cfg(feature = "userapps")]
                RemoteAppTarget::Rejected(app) if !app.is_host => {
                    target_apps.insert(app_uuid, app.into());
                }
                #[cfg(feature = "balenahup")]
                RemoteAppTarget::Rejected(app) if app.is_host => {
                    target_apps.insert(app_uuid, app.into());
                }
                _ => {}
            };
        }

        Self {
            name: Some(name),
            #[cfg(any(feature = "userapps", feature = "balenahup"))]
            apps: target_apps,
            #[cfg(not(any(feature = "userapps", feature = "balenahup")))]
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

    #[cfg(feature = "balenahup")]
    #[test]
    fn rejected_host_app_is_included_in_target_with_rejected_release_set() {
        // A rejected host app leaves no host target, so the device keeps
        // running its current host release, and is kept as an app with the
        // rejected release set so the rejection can be reported.
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
        assert!(target.host.is_none());
        let app = target
            .apps
            .get(&Uuid::from("bad-host"))
            .expect("rejected host app should be in the target");
        assert_eq!(app.id, 8);
        assert_eq!(app.name.as_deref(), Some("host"));
        assert!(app.releases.is_empty());
        assert_eq!(app.rejected_release, Some(Uuid::from("bad-release")));
    }

    /// Target of a single app with one release, running `my-service` with the
    /// given service config.
    #[cfg(feature = "userapps")]
    fn target_with_service_config(config: serde_json::Value) -> DeviceTarget {
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
                                    "config": config,
                                },
                            },
                        }
                    }
                }
            }
        }))
        .unwrap()
    }

    #[cfg(any(feature = "userapps", feature = "balenahup"))]
    fn runtimes(names: &[&str]) -> Vec<oci::Runtime> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[cfg(feature = "userapps")]
    #[test]
    fn a_service_asking_for_an_unregistered_runtime_rejects_its_release() {
        let mut target = target_with_service_config(json!({"runtime": "nvidia"}));
        let device = Device::new(Uuid::from("device-uuid"), None);

        let reasons = target.reject_unsupported_releases(&device, &runtimes(&["runc"]));

        assert_eq!(
            reasons,
            vec![
                "invalid target release (my-rele) for app my-app-uuid: service 'my-service' \
                 requires runtime 'nvidia', which is not available on the device"
                    .to_string()
            ]
        );
        let app = target.apps.get(&Uuid::from("my-app-uuid")).unwrap();
        assert_eq!(app.id, 1234);
        assert_eq!(app.name.as_deref(), Some("my-app"));
        assert!(app.releases.is_empty());
        assert_eq!(app.rejected_release, Some(Uuid::from("my-release-uuid")));
    }

    #[cfg(feature = "userapps")]
    #[test]
    fn a_service_asking_for_a_registered_runtime_keeps_its_release() {
        let mut target = target_with_service_config(json!({"runtime": "runc"}));
        let device = Device::new(Uuid::from("device-uuid"), None);

        let reasons = target.reject_unsupported_releases(&device, &runtimes(&["runc"]));

        assert!(reasons.is_empty());
        let app = target.apps.get(&Uuid::from("my-app-uuid")).unwrap();
        assert_eq!(app.releases.len(), 1);
        assert_eq!(app.rejected_release, None);
    }

    #[cfg(feature = "userapps")]
    #[test]
    fn a_service_naming_no_runtime_keeps_its_release() {
        let mut target = target_with_service_config(json!({}));
        let device = Device::new(Uuid::from("device-uuid"), None);

        let reasons = target.reject_unsupported_releases(&device, &runtimes(&[]));

        assert!(reasons.is_empty());
        let app = target.apps.get(&Uuid::from("my-app-uuid")).unwrap();
        assert_eq!(app.releases.len(), 1);
        assert_eq!(app.rejected_release, None);
    }

    #[cfg(feature = "balenahup")]
    #[test]
    fn a_host_release_asking_for_an_unregistered_runtime_is_rejected_as_an_app() {
        let mut target: DeviceTarget = serde_json::from_value(json!({
            "host": {
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();
        let device = Device::new(
            Uuid::from("device-uuid"),
            Some(serde_json::from_value(json!({"name": "balenaOS", "version": "5.7.3"})).unwrap()),
        );

        let reasons = target.reject_unsupported_releases(&device, &runtimes(&["runc"]));

        assert_eq!(reasons.len(), 1);
        assert!(target.host.as_ref().unwrap().releases.is_empty());
        let app = target.apps.get(&Uuid::from("hostapp-uuid")).unwrap();
        assert!(app.releases.is_empty());
        assert_eq!(app.rejected_release, Some(Uuid::from("target-release")));
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
