use crate::common_types::Uuid;
use crate::models::{
    ContainerStatus, DependsOn, DependsOnCondition, Device, DeviceTarget, Health, ImageRef,
    Network, NetworkTarget, Service, ServiceConfig, ServiceTarget, Volume, VolumeTarget,
};
use crate::oci::Mount;

/// Find an installed service for a different commit
pub fn find_installed_service<'a>(
    device: &'a Device,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    service_name: &'a String,
) -> Option<&'a Service> {
    device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(_, r)| r.services.iter().find(|(k, _)| k == &service_name))
            .map(|(_, s)| s)
            .next()
    })
}

/// Whether a dependency's current state satisfies a `depends_on` condition.
fn condition_met(dep: &Service, condition: DependsOnCondition) -> bool {
    match condition {
        // The dependency has been started at least once
        DependsOnCondition::ServiceStarted => dep.started,
        // The dependency's healthcheck reports healthy
        DependsOnCondition::ServiceHealthy => dep
            .oci
            .as_ref()
            .is_some_and(|c| c.health == Health::Healthy),
        // The dependency's container has exited with status 0
        DependsOnCondition::ServiceCompletedSuccessfully => dep
            .oci
            .as_ref()
            .is_some_and(|c| c.status == ContainerStatus::Stopped && c.exit_code == Some(0)),
    }
}

/// Whether every `depends_on` entry of a service is satisfied by the current
/// state of its dependencies in the same release. A `required` dependency must
/// have reached its condition; an optional (`required: false`) dependency never
/// blocks.
///
/// TODO: full Compose parity for optional dependencies (separate PR). Compose
/// also *waits* for an optional dependency to resolve and warns if it fails;
/// Helios does not wait for optional dependencies at all — the dependent starts
/// immediately regardless of their outcome.
pub fn dependencies_satisfied(
    device: &Device,
    app_uuid: &Uuid,
    commit: &Uuid,
    depends_on: &DependsOn,
) -> bool {
    let services = device
        .apps
        .get(app_uuid)
        .and_then(|app| app.releases.get(commit))
        .map(|release| &release.services);

    depends_on.iter().all(|(dep_name, spec)| {
        !spec.required
            || services
                .and_then(|services| services.get(dep_name))
                .is_some_and(|dep| condition_met(dep, spec.condition))
    })
}

/// Find a new network for a different commit
pub fn find_future_network<'a>(
    t_device: &'a DeviceTarget,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    network_name: &'a String,
) -> Option<(&'a Uuid, &'a NetworkTarget)> {
    t_device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(c, r)| {
                r.networks
                    .iter()
                    .find(|(k, _)| k == &network_name)
                    .map(|(_, n)| (c, n))
            })
            .next()
    })
}

/// Find a new network for a different commit
pub fn find_installed_network<'a>(
    device: &'a Device,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    network_name: &'a String,
) -> Option<&'a Network> {
    device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(_, r)| {
                r.networks
                    .iter()
                    .find(|(k, _)| k == &network_name)
                    .map(|(_, n)| n)
            })
            .next()
    })
}

/// Find a new volume for a different commit
pub fn find_future_volume<'a>(
    t_device: &'a DeviceTarget,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    volume_name: &'a String,
) -> Option<(&'a Uuid, &'a VolumeTarget)> {
    t_device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(c, r)| {
                r.volumes
                    .iter()
                    .find(|(k, _)| k == &volume_name)
                    .map(|(_, v)| (c, v))
            })
            .next()
    })
}

/// Find an installed volume for a different commit
pub fn find_installed_volume<'a>(
    device: &'a Device,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    volume_name: &'a String,
) -> Option<&'a Volume> {
    device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(_, r)| {
                r.volumes
                    .iter()
                    .find(|(k, _)| k == &volume_name)
                    .map(|(_, v)| v)
            })
            .next()
    })
}

/// Find an new service for a different commit
pub fn find_future_service<'a>(
    t_device: &'a DeviceTarget,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    service_name: &'a String,
) -> Option<(&'a Uuid, &'a ServiceTarget)> {
    t_device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(c, r)| {
                r.services
                    .iter()
                    .find(|(k, _)| k == &service_name)
                    .map(|(_, s)| (c, s))
            })
            .next()
    })
}

/// Check that every volume and network referenced by the service has matching
/// configuration in the target release. If a linked resource changes config
/// across releases the service cannot be migrated state-only — its container
/// must be recreated against the new resource.
fn linked_resources_can_migrate(
    device: &Device,
    t_device: &DeviceTarget,
    app_uuid: &Uuid,
    rel_uuid: &Uuid,
    t_rel_uuid: &Uuid,
    cfg: &ServiceConfig,
) -> bool {
    let release = device
        .apps
        .get(app_uuid)
        .and_then(|app| app.releases.get(rel_uuid));
    let t_release = t_device
        .apps
        .get(app_uuid)
        .and_then(|app| app.releases.get(t_rel_uuid));

    let volumes_ok = cfg.volumes.iter().all(|mount| match mount {
        Mount::Volume { source, .. } => {
            let cur = release.and_then(|r| r.volumes.get(source));
            let tgt = t_release.and_then(|r| r.volumes.get(source));
            match (cur, tgt) {
                (Some(c), Some(t)) => c.config == t.config,
                _ => true,
            }
        }
        _ => true,
    });

    let networks_ok = cfg.networks.keys().all(|name| {
        let cur = release.and_then(|r| r.networks.get(name));
        let tgt = t_release.and_then(|r| r.networks.get(name));
        match (cur, tgt) {
            (Some(c), Some(t)) => c.config == t.config,
            _ => true,
        }
    });

    volumes_ok && networks_ok
}

/// Check whether the current service can be migrated to the given target
/// service without recreating its container. Requires matching image,
/// configuration and started state, and that all linked volumes and networks
/// have the same configuration across releases.
pub fn service_matches_target(
    device: &Device,
    t_device: &DeviceTarget,
    app_uuid: &Uuid,
    rel_uuid: &Uuid,
    svc: &Service,
    t_rel_uuid: &Uuid,
    t_svc: &ServiceTarget,
) -> bool {
    svc.image.is_same_artifact(&t_svc.image)
        && svc.config == t_svc.config
        && svc.started == t_svc.started
        && svc.depends_on == t_svc.depends_on
        && linked_resources_can_migrate(
            device,
            t_device,
            app_uuid,
            rel_uuid,
            t_rel_uuid,
            &svc.config,
        )
}

/// Check whether a running service needs to be stopped to converge towards
/// the target. A service needs stopping if it is running and any of:
/// - it does not exist in any target release, or
/// - it does not match the target service (image, config, started state, or
///   linked volumes/networks change across releases).
///
/// A non-running service never needs stopping.
pub fn service_needs_stopping(
    device: &Device,
    t_device: &DeviceTarget,
    app_uuid: &Uuid,
    rel_uuid: &Uuid,
    svc_name: &str,
    svc: &Service,
) -> bool {
    // only running services can be stopped
    if svc
        .oci
        .as_ref()
        .is_none_or(|c| c.status != ContainerStatus::Running)
    {
        return false;
    }

    // look for the same-named service in the target: same release first,
    // then any other release
    let target = t_device.apps.get(app_uuid).and_then(|t_app| {
        t_app
            .releases
            .get(rel_uuid)
            .and_then(|t_rel| t_rel.services.get(svc_name))
            .map(|t_svc| (rel_uuid, t_svc))
            .or_else(|| {
                t_app.releases.iter().find_map(|(t_rel_uuid, t_rel)| {
                    t_rel
                        .services
                        .get(svc_name)
                        .map(|t_svc| (t_rel_uuid, t_svc))
                })
            })
    });

    match target {
        None => true,
        Some((t_rel_uuid, t_svc)) => {
            !service_matches_target(device, t_device, app_uuid, rel_uuid, svc, t_rel_uuid, t_svc)
        }
    }
}

/// Check whether any running service in the app needs to be stopped to
/// converge towards the target.
pub fn services_need_stopping(app_uuid: &Uuid, device: &Device, t_device: &DeviceTarget) -> bool {
    device.apps.get(app_uuid).is_some_and(|app| {
        app.releases.iter().any(|(rel_uuid, rel)| {
            rel.services.iter().any(|(svc_name, svc)| {
                service_needs_stopping(device, t_device, app_uuid, rel_uuid, svc_name, svc)
            })
        })
    })
}

/// True if any target release of `app_uuid` other than `exclude_rel` contains
/// a service whose image URI has not yet been pulled. Used by uninstall paths
/// to defer disturbing current state until the future release's images are
/// ready to take over.
pub fn any_images_are_pending_download(
    device: &Device,
    t_device: &DeviceTarget,
    app_uuid: &Uuid,
    exclude_rel: &Uuid,
) -> bool {
    t_device.apps.get(app_uuid).is_some_and(|t_app| {
        t_app.releases.iter().any(|(t_rel_uuid, t_rel)| {
            t_rel_uuid != exclude_rel
                && t_rel.services.values().any(|t_svc| {
                    !matches!(&t_svc.image, ImageRef::Uri(uri) if device.images.contains_key(uri))
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DependencySpec;
    use serde_json::json;
    use std::collections::HashMap;

    fn device_with(services: serde_json::Value) -> Device {
        serde_json::from_value(json!({
            "uuid": "device-uuid",
            "apps": {
                "app-uuid": {
                    "id": 1,
                    "name": "app",
                    "releases": {
                        "rel-uuid": {
                            "installed": true,
                            "services": services,
                        }
                    }
                }
            }
        }))
        .unwrap()
    }

    /// Build a `depends_on` of `service_started` entries with the given
    /// `(name, required)` pairs.
    fn started_deps(entries: &[(&str, bool)]) -> DependsOn {
        entries
            .iter()
            .map(|(name, required)| {
                (
                    name.to_string(),
                    DependencySpec {
                        condition: DependsOnCondition::ServiceStarted,
                        restart: false,
                        required: *required,
                    },
                )
            })
            .collect::<HashMap<_, _>>()
            .into()
    }

    fn check(device: &Device, deps: &DependsOn) -> bool {
        dependencies_satisfied(device, &"app-uuid".into(), &"rel-uuid".into(), deps)
    }

    #[test]
    fn empty_depends_on_is_satisfied() {
        let device = device_with(json!({}));
        assert!(check(&device, &DependsOn::default()));
    }

    #[test]
    fn satisfied_when_required_dependency_is_met() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {}},
        }));
        assert!(check(&device, &started_deps(&[("db", true)])));
    }

    #[test]
    fn blocks_on_unmet_required_dependency() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": false, "config": {}},
        }));
        assert!(!check(&device, &started_deps(&[("db", true)])));
    }

    #[test]
    fn proceeds_on_unmet_optional_dependency() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": false, "config": {}},
        }));
        assert!(check(&device, &started_deps(&[("db", false)])));
    }

    #[test]
    fn blocks_when_a_required_dependency_is_unmet_even_if_an_optional_one_is_met() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {}},
            "cache": {"id": 2, "image": "alpine:latest", "started": false, "config": {}},
        }));
        assert!(!check(
            &device,
            &started_deps(&[("db", false), ("cache", true)])
        ));
    }

    #[test]
    fn missing_required_dependency_blocks() {
        let device = device_with(json!({}));
        assert!(!check(&device, &started_deps(&[("db", true)])));
    }

    #[test]
    fn missing_optional_dependency_proceeds() {
        let device = device_with(json!({}));
        assert!(check(&device, &started_deps(&[("db", false)])));
    }
}
