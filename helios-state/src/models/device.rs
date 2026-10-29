use std::collections::HashMap;
use std::path::Path;

use mahler::state::{List, Map, State};
use tracing::warn;

use crate::common_types::{HostRuntimeDir, ImageUri, OperatingSystem, Uuid, Value as EnvValue};
use crate::oci::{self, BindPropagation, Mount};
use crate::remote_model::{App as RemoteAppTarget, Device as RemoteDeviceTarget};
use crate::runtime::RemoteConfig;

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

    /// Whether a change only applies after a reboot. Always false in a target,
    /// so a derived `true` schedules the reboot.
    #[mahler(default)]
    pub needs_reboot: bool,
}

impl Default for DeviceTarget {
    fn default() -> Self {
        DeviceTarget {
            name: None,
            apps: Map::new(),
            #[cfg(feature = "balenahup")]
            host: None,
            needs_reboot: false,
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
            needs_reboot: false,
        }
    }
}

/// The target for the docker socket on the container. We don't use
/// the default `/var/run/docker.sock` to avoid clashing with a docker-in-docker
/// set-up.
const CONTAINER_DOCKER_SOCK: &str = "/host/run/balena-engine.sock";

impl DeviceTarget {
    /// Refuse the parts of the target this device cannot run, and say why.
    ///
    /// Runs before planning: a target with no runtime for it is bad input.
    /// Contract checks belong here once service targets carry a runtime.
    pub fn test_runtime_support(
        &mut self,
        device: &Device,
        runtimes: &[oci::Runtime],
    ) -> Vec<String> {
        #[cfg(feature = "balenahup")]
        if let (Some(host_tgt), Some(_)) = (self.host.as_mut(), device.host.as_ref()) {
            return crate::balenahup::reject_unsupported_releases(host_tgt, runtimes);
        }
        #[cfg(not(feature = "balenahup"))]
        let _ = (device, runtimes);
        Vec::new()
    }

    /// Adds system runtime related context  to the target state
    ///
    /// Because this context is added in in the target state, if modified externally (e.g through a
    /// helios restart), they will also cause a service restart
    ///
    /// This information will also show on the state returned by helios API
    pub fn add_runtime_context(
        &mut self,
        device: &Device,
        host_runtime_dir: &HostRuntimeDir,
        host_docker_sock: &Path,
        remote_config: &Option<RemoteConfig>,
    ) {
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

                    // Insert the remote endpoint credentials if the `balena-api`
                    // feature is requested
                    if let Some("1" | "true") = svc
                        .config
                        .labels
                        .remove("io.balena.features.balena-api")
                        .as_deref()
                    {
                        if let Some(remote) = remote_config.as_ref() {
                            svc.config.environment.insert(
                                "BALENA_API_KEY".to_string(),
                                Some(EnvValue::String(remote.api_key.to_string())),
                            );
                            svc.config.environment.insert(
                                "BALENA_API_URL".to_string(),
                                Some(EnvValue::String(remote.api_endpoint.to_string())),
                            );
                        } else {
                            // FIXME: a service requesting API credentials on an unmanaged device
                            // should be rejected. That will be added later to test_runtime_support
                            warn!(
                                "no remote credentials are available to satisfy the feature `io.balena.features.balena-api`"
                            );
                        }
                    }

                    // Mount the host socket and set the BALENA_DOCKER_HOST env var in containers
                    // that define the `io.balena.features.balena-socket`. DOCKER_HOST is also set
                    // unless already defined by the service
                    if let Some("1" | "true") = svc
                        .config
                        .labels
                        .remove("io.balena.features.balena-socket")
                        .as_deref()
                    {
                        svc.config
                            .volumes
                            .retain(|m| m.target() != CONTAINER_DOCKER_SOCK);
                        svc.config.volumes.insert(Mount::Bind {
                            target: CONTAINER_DOCKER_SOCK.to_string(),
                            source: host_docker_sock.to_string_lossy().to_string(),
                            read_only: false,
                            propagation: BindPropagation::Rprivate,
                            // we don't want docker creating a directory for the bind
                            create_host_path: false,
                        });
                        let docker_host =
                            Some(EnvValue::String(format!("unix://{CONTAINER_DOCKER_SOCK}")));

                        // only add the env var if not defined
                        svc.config
                            .environment
                            .entry("DOCKER_HOST".to_string())
                            .or_insert(docker_host.clone());
                        svc.config
                            .environment
                            .insert("BALENA_DOCKER_HOST".to_string(), docker_host);
                    }

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
            needs_reboot: false,
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
                #[cfg(feature = "balenahup")]
                RemoteAppTarget::Rejected(app) if app.is_host => {
                    hostapps.push((app_uuid, app).into());
                }
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
            needs_reboot: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::common_types::ApiKey;
    use crate::models::ServiceTarget;
    use crate::util::http::Uri;

    const DEFAULT_HOST_DOCKER_SOCK: &str = "/var/run/docker.sock";

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
    fn rejected_host_app_is_included_in_target_with_rejected_release_set() {
        // Rejected host apps produce a host target with no releases and
        // `rejected_release` set — the device keeps running its current host
        // release.
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
        let host = target.host.expect("rejected host should be in the target");
        assert!(host.releases.is_empty());
        assert_eq!(host.app, Some(Uuid::from("bad-host")));
        assert_eq!(host.rejected_release, Some(Uuid::from("bad-release")));
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

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &None,
        );

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

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &None,
        );

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

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &None,
        );

        let volumes = &release_of(&target).volumes;
        assert!(volumes.contains_key("my-vol"));
        assert_eq!(mount_sources(&target), vec!["my-vol".to_string()]);
    }

    /// Target of a single app with one release, with `my-service` carrying `labels`.
    fn target_with_labels(labels: serde_json::Value) -> DeviceTarget {
        target_with_config(json!({"labels": labels}))
    }

    /// Target of a single app with one release, with `my-service` using `config`.
    fn target_with_config(config: serde_json::Value) -> DeviceTarget {
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

    fn remote_config() -> Option<RemoteConfig> {
        Some(RemoteConfig {
            api_endpoint: Uri::from_static("https://api.balena-cloud.com"),
            api_key: ApiKey::from("my-api-key".to_string()),
        })
    }

    fn service_of(target: &DeviceTarget) -> &ServiceTarget {
        release_of(target).services.get("my-service").unwrap()
    }

    #[test]
    fn balena_api_feature_injects_remote_credentials() {
        for value in ["1", "true"] {
            let mut target = target_with_labels(json!({"io.balena.features.balena-api": value}));
            let device = device_with_volumes(json!([]));

            target.add_runtime_context(
                &device,
                &HostRuntimeDir("/run/helios".into()),
                Path::new(DEFAULT_HOST_DOCKER_SOCK),
                &remote_config(),
            );

            let svc = service_of(&target);
            assert_eq!(
                svc.config.environment.get("BALENA_API_KEY"),
                Some(&Some(EnvValue::String("my-api-key".to_string())))
            );
            assert_eq!(
                svc.config.environment.get("BALENA_API_URL"),
                Some(&Some(EnvValue::String(
                    "https://api.balena-cloud.com".to_string()
                )))
            );
            assert!(
                !svc.config
                    .labels
                    .contains_key("io.balena.features.balena-api")
            );
        }
    }

    #[test]
    fn balena_api_feature_not_enabled_does_not_inject_credentials() {
        let mut target = target_with_labels(json!({"io.balena.features.balena-api": "0"}));
        let device = device_with_volumes(json!([]));

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &remote_config(),
        );

        let svc = service_of(&target);
        assert!(!svc.config.environment.contains_key("BALENA_API_KEY"));
        assert!(!svc.config.environment.contains_key("BALENA_API_URL"));
        assert!(
            !svc.config
                .labels
                .contains_key("io.balena.features.balena-api")
        );
    }

    #[test]
    fn missing_balena_api_label_does_not_inject_credentials() {
        let mut target = target_with_labels(json!({}));
        let device = device_with_volumes(json!([]));

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &remote_config(),
        );

        let svc = service_of(&target);
        assert!(!svc.config.environment.contains_key("BALENA_API_KEY"));
        assert!(!svc.config.environment.contains_key("BALENA_API_URL"));
    }

    #[test]
    fn balena_api_feature_without_remote_config_does_not_inject_credentials() {
        let mut target = target_with_labels(json!({"io.balena.features.balena-api": "1"}));
        let device = device_with_volumes(json!([]));

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &None,
        );

        let svc = service_of(&target);
        assert!(!svc.config.environment.contains_key("BALENA_API_KEY"));
        assert!(!svc.config.environment.contains_key("BALENA_API_URL"));
        assert!(
            !svc.config
                .labels
                .contains_key("io.balena.features.balena-api")
        );
    }

    /// Mounts of `my-service` targeting the engine socket path.
    fn socket_mounts(target: &DeviceTarget) -> Vec<&Mount> {
        service_of(target)
            .config
            .volumes
            .iter()
            .filter(|m| m.target() == "/host/run/balena-engine.sock")
            .collect()
    }

    #[test]
    fn balena_socket_feature_mounts_engine_socket() {
        for value in ["1", "true"] {
            let mut target = target_with_labels(json!({"io.balena.features.balena-socket": value}));
            let device = device_with_volumes(json!([]));

            target.add_runtime_context(
                &device,
                &HostRuntimeDir("/run/helios".into()),
                Path::new("/run/balena-engine.sock"),
                &None,
            );

            assert_eq!(
                socket_mounts(&target),
                vec![&Mount::Bind {
                    target: "/host/run/balena-engine.sock".to_string(),
                    source: "/run/balena-engine.sock".to_string(),
                    read_only: false,
                    propagation: BindPropagation::Rprivate,
                    create_host_path: false,
                }]
            );
            let svc = service_of(&target);
            let docker_host = Some(EnvValue::String(
                "unix:///host/run/balena-engine.sock".to_string(),
            ));
            assert_eq!(
                svc.config.environment.get("DOCKER_HOST"),
                Some(&docker_host)
            );
            assert_eq!(
                svc.config.environment.get("BALENA_DOCKER_HOST"),
                Some(&docker_host)
            );
            assert!(
                !svc.config
                    .labels
                    .contains_key("io.balena.features.balena-socket")
            );
        }
    }

    #[test]
    fn balena_socket_feature_overrides_user_mount_at_socket_path() {
        let mut target = target_with_config(json!({
            "labels": {"io.balena.features.balena-socket": "1"},
            "volumes": [
                {
                    "type": "bind",
                    "source": "/some/other.sock",
                    "target": "/host/run/balena-engine.sock"
                }
            ]
        }));
        let device = device_with_volumes(json!([]));

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &None,
        );

        let mounts = socket_mounts(&target);
        assert_eq!(mounts.len(), 1);
        assert!(matches!(
            mounts[0],
            Mount::Bind { source, .. } if source == DEFAULT_HOST_DOCKER_SOCK
        ));
    }

    #[test]
    fn balena_socket_feature_keeps_user_docker_host_but_overrides_balena_docker_host() {
        let mut target = target_with_config(json!({
            "labels": {"io.balena.features.balena-socket": "1"},
            "environment": {
                "DOCKER_HOST": "tcp://other:2375",
                "BALENA_DOCKER_HOST": "tcp://other:2375"
            }
        }));
        let device = device_with_volumes(json!([]));

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &None,
        );

        let svc = service_of(&target);
        assert_eq!(socket_mounts(&target).len(), 1);
        assert_eq!(
            svc.config.environment.get("DOCKER_HOST"),
            Some(&Some(EnvValue::String("tcp://other:2375".to_string())))
        );
        assert_eq!(
            svc.config.environment.get("BALENA_DOCKER_HOST"),
            Some(&Some(EnvValue::String(
                "unix:///host/run/balena-engine.sock".to_string()
            )))
        );
    }

    #[test]
    fn balena_socket_feature_not_enabled_does_not_mount_engine_socket() {
        let mut target = target_with_labels(json!({"io.balena.features.balena-socket": "0"}));
        let device = device_with_volumes(json!([]));

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &None,
        );

        let svc = service_of(&target);
        assert!(socket_mounts(&target).is_empty());
        assert!(!svc.config.environment.contains_key("DOCKER_HOST"));
        assert!(!svc.config.environment.contains_key("BALENA_DOCKER_HOST"));
        assert!(
            !svc.config
                .labels
                .contains_key("io.balena.features.balena-socket")
        );
    }

    #[test]
    fn missing_balena_socket_label_does_not_mount_engine_socket() {
        let mut target = target_with_labels(json!({}));
        let device = device_with_volumes(json!([]));

        target.add_runtime_context(
            &device,
            &HostRuntimeDir("/run/helios".into()),
            Path::new(DEFAULT_HOST_DOCKER_SOCK),
            &None,
        );

        let svc = service_of(&target);
        assert!(socket_mounts(&target).is_empty());
        assert!(!svc.config.environment.contains_key("DOCKER_HOST"));
        assert!(!svc.config.environment.contains_key("BALENA_DOCKER_HOST"));
    }
}
