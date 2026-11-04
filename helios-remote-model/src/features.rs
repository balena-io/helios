//! Mounts requested through `io.balena.features.*` service labels

use std::collections::HashMap;
use std::path::Path;

use crate::service::{BindMount, Mount, Service, VolumeMount};
use crate::volume::Volume;

/// A host path bind mounted by a feature label
struct FeatureBind {
    source: &'static str,
    target: &'static str,
    read_only: bool,
    create_host_path: bool,
}

/// Bind mounts added for each feature label
const FEATURE_BINDS: &[(&str, &[FeatureBind])] = &[
    (
        "io.balena.features.journal-logs",
        // we mount both logs locations so we don't want the mount to fail
        // if one of them doesn't exist
        &[
            FeatureBind {
                source: "/var/log/journal",
                target: "/var/log/journal",
                read_only: true,
                create_host_path: true,
            },
            FeatureBind {
                source: "/run/log/journal",
                target: "/run/log/journal",
                read_only: true,
                create_host_path: true,
            },
            FeatureBind {
                source: "/etc/machine-id",
                target: "/etc/machine-id",
                read_only: true,
                create_host_path: false,
            },
        ],
    ),
    (
        "io.balena.features.dbus",
        &[FeatureBind {
            source: "/run/dbus",
            target: "/host/run/dbus",
            read_only: false,
            create_host_path: true,
        }],
    ),
    (
        "io.balena.features.kernel-modules",
        &[FeatureBind {
            source: "/lib/modules",
            target: "/lib/modules",
            read_only: false,
            create_host_path: true,
        }],
    ),
    (
        "io.balena.features.firmware",
        &[FeatureBind {
            source: "/lib/firmware",
            target: "/lib/firmware",
            read_only: false,
            create_host_path: true,
        }],
    ),
    // there is something wrong with the host if `/sys` or `/proc`
    // don't exist. Don't create them
    (
        "io.balena.features.sysfs",
        &[FeatureBind {
            source: "/sys",
            target: "/sys",
            read_only: false,
            create_host_path: false,
        }],
    ),
    (
        "io.balena.features.procfs",
        &[FeatureBind {
            source: "/proc",
            target: "/proc",
            read_only: false,
            create_host_path: false,
        }],
    ),
];

const EXTRA_FIRMWARE_LABEL: &str = "io.balena.features.extra-firmware";

/// Name of the external volume mounted by `io.balena.features.extra-firmware`
const EXTRA_FIRMWARE_VOLUME: &str = "extra-firmware";

/// Remove the feature `label` from the service and return whether the feature is enabled
///
/// A feature is enabled if its label is set to `1` or `true`. Top level service labels take
/// priority over the composition labels.
fn take_feature(svc: &mut Service, label: &str) -> bool {
    let composition = svc.composition.labels.remove(label);
    let value = svc.labels.remove(label).or(composition);
    matches!(value.as_deref(), Some("1" | "true"))
}

/// Add `mount` to the service, rejecting it if the service already has a mount
/// or tmpfs with the same target
fn add_mount(svc_name: &str, svc: &mut Service, label: &str, mount: Mount) -> Result<(), String> {
    let target = Path::new(mount.target());
    if svc
        .composition
        .volumes
        .iter()
        .any(|m| Path::new(m.target()) == target)
    {
        return Err(format!(
            "service '{svc_name}' declares a mount on {}, which conflicts with feature {label}",
            mount.target()
        ));
    }

    if svc
        .composition
        .tmpfs
        .iter()
        .flatten()
        .any(|(tgt, _)| tgt == target)
    {
        return Err(format!(
            "service '{svc_name}' declares a tmpfs on {}, which conflicts with feature {label}",
            mount.target()
        ));
    }
    svc.composition.volumes.push(mount);
    Ok(())
}

/// Add the mounts requested by feature labels to the service
///
/// Feature labels are always removed from the service. Volumes requested by a feature are
/// added to the release `volumes` as external. A feature mount whose target is already used
/// by another mount of the service, or a feature volume with the name of a release volume,
/// is rejected as the definition would be ambiguous.
pub(crate) fn add_feature_mounts(
    svc_name: &str,
    svc: &mut Service,
    volumes: &mut HashMap<String, Volume>,
) -> Result<(), String> {
    for (label, binds) in FEATURE_BINDS {
        if !take_feature(svc, label) {
            continue;
        }
        for b in binds.iter() {
            let mount = Mount::Bind(BindMount {
                source: b.source.to_string(),
                target: b.target.to_string(),
                read_only: b.read_only,
                propagation: None,
                create_host_path: b.create_host_path,
            });
            add_mount(svc_name, svc, label, mount)?;
        }
    }

    if take_feature(svc, EXTRA_FIRMWARE_LABEL) {
        // the volume is already external if added by another service using the feature
        if volumes
            .get(EXTRA_FIRMWARE_VOLUME)
            .is_some_and(|vol| !vol.external)
        {
            return Err(format!(
                "service '{svc_name}' uses feature {EXTRA_FIRMWARE_LABEL}, which conflicts with release volume {EXTRA_FIRMWARE_VOLUME}"
            ));
        }
        volumes.insert(
            EXTRA_FIRMWARE_VOLUME.to_string(),
            Volume {
                external: true,
                ..Default::default()
            },
        );
        let mount = Mount::Volume(VolumeMount {
            source: EXTRA_FIRMWARE_VOLUME.to_string(),
            target: "/extra-firmware".to_string(),
            read_only: false,
            nocopy: false,
            subpath: None,
        });
        add_mount(svc_name, svc, EXTRA_FIRMWARE_LABEL, mount)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::Release;
    use crate::service::is_allowed_bind_source;

    use super::*;

    /// Release with a single service `my-service` using the given labels and mounts
    fn release_with(
        labels: Value,
        composition_labels: Value,
        volumes: Value,
    ) -> Result<Release, serde_json::Error> {
        serde_json::from_value(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "alpine:latest",
                    "labels": labels,
                    "composition": {"labels": composition_labels, "volumes": volumes}
                }
            },
            "volumes": {"my-vol": {}}
        }))
    }

    fn service(release: &Release) -> &Service {
        &release.services["my-service"]
    }

    fn mounts(release: &Release) -> &[Mount] {
        service(release).composition.volumes.as_slice()
    }

    fn bind_mount(source: &str, target: &str, read_only: bool, create_host_path: bool) -> Mount {
        Mount::Bind(BindMount {
            source: source.to_string(),
            target: target.to_string(),
            read_only,
            propagation: None,
            create_host_path,
        })
    }

    fn has_label(release: &Release, label: &str) -> bool {
        let svc = service(release);
        svc.labels.contains_key(label) || svc.composition.labels.contains_key(label)
    }

    #[test]
    fn feature_bind_sources_are_allowed_bind_sources() {
        for (_, binds) in FEATURE_BINDS {
            for b in binds.iter() {
                assert!(
                    is_allowed_bind_source(b.source),
                    "{} is not allowed",
                    b.source
                );
            }
        }
    }

    #[test]
    fn feature_labels_add_bind_mounts() {
        let cases = [
            (
                "io.balena.features.journal-logs",
                vec![
                    bind_mount("/var/log/journal", "/var/log/journal", true, true),
                    bind_mount("/run/log/journal", "/run/log/journal", true, true),
                    bind_mount("/etc/machine-id", "/etc/machine-id", true, false),
                ],
            ),
            (
                "io.balena.features.dbus",
                vec![bind_mount("/run/dbus", "/host/run/dbus", false, true)],
            ),
            (
                "io.balena.features.kernel-modules",
                vec![bind_mount("/lib/modules", "/lib/modules", false, true)],
            ),
            (
                "io.balena.features.firmware",
                vec![bind_mount("/lib/firmware", "/lib/firmware", false, true)],
            ),
            (
                "io.balena.features.sysfs",
                vec![bind_mount("/sys", "/sys", false, false)],
            ),
            (
                "io.balena.features.procfs",
                vec![bind_mount("/proc", "/proc", false, false)],
            ),
        ];

        for (label, expected) in cases {
            // the label may come from the top level service labels or the composition
            for (labels, composition_labels) in [
                (json!({ label: "1" }), json!({})),
                (json!({}), json!({ label: "1" })),
            ] {
                let release = release_with(labels, composition_labels, json!([])).unwrap();
                assert_eq!(mounts(&release), expected, "mounts for {label}");
                assert!(!has_label(&release, label));
            }
        }
    }

    #[test]
    fn feature_label_is_enabled_only_by_truthy_values() {
        for (value, enabled) in [
            ("1", true),
            ("true", true),
            ("0", false),
            ("false", false),
            ("", false),
            ("yes", false),
            ("TRUE", false),
        ] {
            let release = release_with(
                json!({}),
                json!({"io.balena.features.dbus": value}),
                json!([]),
            )
            .unwrap();
            assert_eq!(
                !mounts(&release).is_empty(),
                enabled,
                "mounts for {value:?}"
            );
            assert!(!has_label(&release, "io.balena.features.dbus"));
        }
    }

    #[test]
    fn top_level_feature_label_takes_priority_over_composition() {
        let release = release_with(
            json!({"io.balena.features.dbus": "0"}),
            json!({"io.balena.features.dbus": "1"}),
            json!([]),
        )
        .unwrap();
        assert!(mounts(&release).is_empty());
        assert!(!has_label(&release, "io.balena.features.dbus"));
    }

    #[test]
    fn rejects_feature_mount_on_a_declared_mount_target() {
        for volumes in [
            json!(["/sys:/sys:ro"]),
            json!(["my-vol:/sys/"]),
            json!([{"type": "tmpfs", "target": "/sys"}]),
        ] {
            let err = release_with(json!({}), json!({"io.balena.features.sysfs": "1"}), volumes)
                .unwrap_err();
            assert!(
                err.to_string().contains(
                    "service 'my-service' declares a mount on /sys, which conflicts with feature io.balena.features.sysfs"
                ),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn rejects_feature_mount_on_a_declared_tmpfs_target() {
        for tmpfs in [json!("/sys"), json!(["/run", "/sys/"])] {
            let err = serde_json::from_value::<Release>(json!({
                "services": {
                    "my-service": {
                        "id": 1,
                        "image": "alpine:latest",
                        "composition": {
                            "labels": {"io.balena.features.sysfs": "1"},
                            "tmpfs": tmpfs
                        }
                    }
                }
            }))
            .unwrap_err();
            assert!(
                err.to_string().contains(
                    "service 'my-service' declares a tmpfs on /sys, which conflicts with feature io.balena.features.sysfs"
                ),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn feature_mounts_are_added_next_to_declared_mounts() {
        let release = release_with(
            json!({}),
            json!({"io.balena.features.sysfs": "1"}),
            json!([{"type": "tmpfs", "target": "/tmp"}]),
        )
        .unwrap();
        assert_eq!(mounts(&release).len(), 2);
        assert!(mounts(&release).contains(&bind_mount("/sys", "/sys", false, false)));
    }

    fn extra_firmware_mount() -> Mount {
        Mount::Volume(VolumeMount {
            source: "extra-firmware".to_string(),
            target: "/extra-firmware".to_string(),
            read_only: false,
            nocopy: false,
            subpath: None,
        })
    }

    #[test]
    fn extra_firmware_label_mounts_external_volume() {
        let release = release_with(
            json!({}),
            json!({"io.balena.features.extra-firmware": "1"}),
            json!([]),
        )
        .unwrap();
        assert_eq!(mounts(&release), [extra_firmware_mount()]);
        assert!(release.volumes["extra-firmware"].external);
        assert!(!release.volumes["my-vol"].external);
        assert!(!has_label(&release, "io.balena.features.extra-firmware"));
    }

    #[test]
    fn extra_firmware_volume_is_shared_by_services_using_the_feature() {
        let release: Release = serde_json::from_value(json!({
            "services": {
                "one": {
                    "id": 1,
                    "image": "alpine:latest",
                    "composition": {"labels": {"io.balena.features.extra-firmware": "1"}}
                },
                "two": {
                    "id": 2,
                    "image": "alpine:latest",
                    "composition": {"labels": {"io.balena.features.extra-firmware": "true"}}
                }
            }
        }))
        .unwrap();
        for svc in ["one", "two"] {
            assert_eq!(
                release.services[svc].composition.volumes.as_slice(),
                [extra_firmware_mount()],
                "mounts for {svc}"
            );
        }
        assert_eq!(release.volumes.len(), 1);
        assert!(release.volumes["extra-firmware"].external);
    }

    #[test]
    fn extra_firmware_volume_is_not_added_without_label() {
        let release = release_with(
            json!({}),
            json!({"io.balena.features.extra-firmware": "0"}),
            json!([]),
        )
        .unwrap();
        assert!(mounts(&release).is_empty());
        assert!(!release.volumes.contains_key("extra-firmware"));
    }

    #[test]
    fn rejects_extra_firmware_label_with_extra_firmware_release_volume() {
        let err = serde_json::from_value::<Release>(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "alpine:latest",
                    "composition": {"labels": {"io.balena.features.extra-firmware": "1"}}
                }
            },
            "volumes": {"extra-firmware": {}}
        }))
        .unwrap_err();
        assert!(
            err.to_string().contains(
                "service 'my-service' uses feature io.balena.features.extra-firmware, which conflicts with release volume extra-firmware"
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_extra_firmware_label_with_a_declared_mount_target() {
        let err = release_with(
            json!({}),
            json!({"io.balena.features.extra-firmware": "1"}),
            json!(["my-vol:/extra-firmware"]),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains(
                "service 'my-service' declares a mount on /extra-firmware, which conflicts with feature io.balena.features.extra-firmware"
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unrelated_labels_are_left_untouched() {
        let release = release_with(
            json!({"com.example": "a"}),
            json!({"io.balena.features.foo": "1"}),
            json!([]),
        )
        .unwrap();
        assert!(mounts(&release).is_empty());
        let svc = service(&release);
        assert_eq!(svc.labels.get("com.example"), Some(&"a".to_string()));
        assert_eq!(
            svc.composition.labels.get("io.balena.features.foo"),
            Some(&"1".to_string())
        );
    }
}
