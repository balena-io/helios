//! Mounts and environment requested through `io.balena.features.*` service labels

use std::path::Path;

use crate::common_types::Value;
use crate::service::{BindMount, BindPropagation, Mount, Service};

/// A host path bind mounted by a feature label
struct FeatureBind {
    source: &'static str,
    target: &'static str,
    read_only: bool,
    propagation: Option<BindPropagation>,
    create_host_path: bool,
}

/// Bind mounts and environment variables added by a feature label
///
/// Feature environment variables take priority over any variable with the same
/// name defined by the service.
struct Feature {
    label: &'static str,
    binds: &'static [FeatureBind],
    env: &'static [(&'static str, &'static str)],
}

/// Features supported through service labels
const FEATURES: &[Feature] = &[
    Feature {
        label: "io.balena.features.journal-logs",
        // we mount both logs locations so we don't want the mount to fail
        // if one of them doesn't exist
        binds: &[
            FeatureBind {
                source: "/var/log/journal",
                target: "/var/log/journal",
                read_only: true,
                propagation: None,
                create_host_path: true,
            },
            FeatureBind {
                source: "/run/log/journal",
                target: "/run/log/journal",
                read_only: true,
                propagation: None,
                create_host_path: true,
            },
            FeatureBind {
                source: "/etc/machine-id",
                target: "/etc/machine-id",
                read_only: true,
                propagation: None,
                create_host_path: false,
            },
        ],
        env: &[],
    },
    Feature {
        label: "io.balena.features.dbus",
        binds: &[FeatureBind {
            source: "/run/dbus",
            target: "/host/run/dbus",
            read_only: false,
            propagation: None,
            create_host_path: true,
        }],
        env: &[],
    },
    Feature {
        label: "io.balena.features.kernel-modules",
        binds: &[FeatureBind {
            source: "/lib/modules",
            target: "/lib/modules",
            read_only: false,
            propagation: None,
            create_host_path: true,
        }],
        env: &[],
    },
    Feature {
        label: "io.balena.features.firmware",
        binds: &[FeatureBind {
            source: "/lib/firmware",
            target: "/lib/firmware",
            read_only: false,
            propagation: None,
            create_host_path: true,
        }],
        env: &[],
    },
    // there is something wrong with the host if `/sys` or `/proc`
    // don't exist. Don't create them
    Feature {
        label: "io.balena.features.sysfs",
        binds: &[FeatureBind {
            source: "/sys",
            target: "/sys",
            read_only: false,
            propagation: None,
            create_host_path: false,
        }],
        env: &[],
    },
    Feature {
        label: "io.balena.features.procfs",
        binds: &[FeatureBind {
            source: "/proc",
            target: "/proc",
            read_only: false,
            propagation: None,
            create_host_path: false,
        }],
        env: &[],
    },
    // Give the service the host CA store.
    //
    // Mounts /etc/ssl/certs into the container /host/ssl/certs and sets SSL_CERT_FILE env var.
    // It also mounts /usr/share/ca-certificates and /etc/ca-certificates.conf to allow a
    // user to use them as source for re-building the cert dir. Directories use rslave propagation
    // so mounts on top of the host directory are propagated.
    Feature {
        label: "io.balena.features.ca-certificates",
        binds: &[
            FeatureBind {
                source: "/etc/ssl/certs",
                target: "/host/ssl/certs",
                read_only: true,
                propagation: Some(BindPropagation::Rslave),
                create_host_path: false,
            },
            FeatureBind {
                source: "/usr/share/ca-certificates",
                target: "/usr/share/ca-certificates",
                read_only: true,
                propagation: Some(BindPropagation::Rslave),
                create_host_path: false,
            },
            FeatureBind {
                source: "/etc/ca-certificates.conf",
                target: "/host/ca-certificates.conf",
                read_only: true,
                propagation: None,
                create_host_path: false,
            },
        ],
        env: &[
            ("SSL_CERT_FILE", "/host/ssl/certs/ca-certificates.crt"),
            ("SSL_CERT_DIR", "/host/ssl/certs"),
        ],
    },
];

/// Remove the feature `label` from the service and return whether the feature is enabled
///
/// A feature is enabled if its label is set to `1` or `true`. Top level service labels take
/// priority over the composition labels.
fn take_feature(svc: &mut Service, label: &str) -> bool {
    let composition = svc.composition.labels.remove(label);
    let value = svc.labels.remove(label).or(composition);
    matches!(value.as_deref(), Some("1" | "true"))
}

/// Read and remove the `io.balena.features.optional` label, marking the service contract
/// as optional if the feature is enabled
///
/// The label is always removed from the service.
pub(crate) fn take_optional(svc: &mut Service) {
    let optional = take_feature(svc, "io.balena.features.optional");
    if let Some(contract) = svc.contract.as_mut() {
        contract.optional = optional;
    }
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

/// Add the mounts and environment variables requested by feature labels to the service
///
/// Feature labels are always removed from the service. A feature mount whose target is
/// already used by another mount of the service is rejected as the definition would be
/// ambiguous. Feature environment variables replace any service variable with the same name.
pub(crate) fn add_features(svc_name: &str, svc: &mut Service) -> Result<(), String> {
    for feature in FEATURES {
        if !take_feature(svc, feature.label) {
            continue;
        }
        for b in feature.binds.iter() {
            let mount = Mount::Bind(BindMount {
                source: b.source.to_string(),
                target: b.target.to_string(),
                read_only: b.read_only,
                propagation: b.propagation,
                create_host_path: b.create_host_path,
            });
            add_mount(svc_name, svc, feature.label, mount)?;
        }
        for (key, value) in feature.env.iter() {
            // the top level service environment takes priority over the composition
            // environment, so setting the variable there overrides both
            svc.environment
                .insert(key.to_string(), Some(Value::String(value.to_string())));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::Release;
    use crate::common_types::Value as EnvValue;
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

    fn rslave_bind_mount(source: &str, target: &str) -> Mount {
        Mount::Bind(BindMount {
            source: source.to_string(),
            target: target.to_string(),
            read_only: true,
            propagation: Some(BindPropagation::Rslave),
            create_host_path: false,
        })
    }

    fn has_label(release: &Release, label: &str) -> bool {
        let svc = service(release);
        svc.labels.contains_key(label) || svc.composition.labels.contains_key(label)
    }

    #[test]
    fn feature_bind_sources_are_allowed_bind_sources() {
        for feature in FEATURES
            .iter()
            .filter(|f| f.label != "io.balena.features.ca-certificates")
        {
            for b in feature.binds.iter() {
                assert!(
                    is_allowed_bind_source(b.source),
                    "{} is not allowed",
                    b.source
                );
            }
        }
    }

    #[test]
    fn ca_certificates_bind_sources_are_not_allowed_bind_sources() {
        let feature = FEATURES
            .iter()
            .find(|f| f.label == "io.balena.features.ca-certificates")
            .unwrap();
        for b in feature.binds.iter() {
            assert!(!is_allowed_bind_source(b.source), "{} is allowed", b.source);
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
            (
                "io.balena.features.ca-certificates",
                vec![
                    rslave_bind_mount("/etc/ssl/certs", "/host/ssl/certs"),
                    rslave_bind_mount("/usr/share/ca-certificates", "/usr/share/ca-certificates"),
                    bind_mount(
                        "/etc/ca-certificates.conf",
                        "/host/ca-certificates.conf",
                        true,
                        false,
                    ),
                ],
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

    fn ssl_release(environment: Value, composition_environment: Value) -> Release {
        serde_json::from_value(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "alpine:latest",
                    "labels": {"io.balena.features.ca-certificates": "1"},
                    "environment": environment,
                    "composition": {"environment": composition_environment}
                }
            }
        }))
        .unwrap()
    }

    fn env(release: &Release, key: &str) -> Option<EnvValue> {
        service(release).environment.get(key).cloned().flatten()
    }

    #[test]
    fn ca_certificates_feature_sets_ssl_environment() {
        let release = ssl_release(json!({"OTHER": "a"}), json!({}));
        assert_eq!(
            env(&release, "SSL_CERT_FILE"),
            Some(EnvValue::String(
                "/host/ssl/certs/ca-certificates.crt".to_string()
            ))
        );
        assert_eq!(
            env(&release, "SSL_CERT_DIR"),
            Some(EnvValue::String("/host/ssl/certs".to_string()))
        );
        assert_eq!(
            env(&release, "OTHER"),
            Some(EnvValue::String("a".to_string()))
        );
    }

    #[test]
    fn ca_certificates_feature_overrides_service_ssl_environment() {
        let release = ssl_release(
            json!({"SSL_CERT_FILE": "/etc/my-ca.crt"}),
            json!({"SSL_CERT_DIR": "/etc/my-certs"}),
        );
        assert_eq!(
            env(&release, "SSL_CERT_FILE"),
            Some(EnvValue::String(
                "/host/ssl/certs/ca-certificates.crt".to_string()
            ))
        );
        assert_eq!(
            env(&release, "SSL_CERT_DIR"),
            Some(EnvValue::String("/host/ssl/certs".to_string()))
        );
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

    fn optional_release(label: &str, composition_label: &str) -> Release {
        serde_json::from_value(json!({
            "services": {
                "my-service": {
                    "id": 1,
                    "image": "alpine:latest",
                    "labels": {"io.balena.features.optional": label},
                    "composition": {"labels": {"io.balena.features.optional": composition_label}},
                    "contract": {"type": "sw.container", "slug": "my-service"}
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn optional_label_marks_the_contract_optional_and_is_removed() {
        for value in ["1", "true"] {
            let release = optional_release(value, "0");
            assert!(service(&release).contract.as_ref().unwrap().optional);
            assert!(!has_label(&release, "io.balena.features.optional"));
        }
    }

    #[test]
    fn contract_is_not_optional_unless_the_label_is_set() {
        let release = optional_release("0", "1");
        assert!(!service(&release).contract.as_ref().unwrap().optional);
        assert!(!has_label(&release, "io.balena.features.optional"));
    }
}
