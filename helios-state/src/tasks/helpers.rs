use mahler::state::Map;

use crate::common_types::Uuid;
use crate::models::{
    Container, ContainerStatus, DependsOn, DependsOnCondition, Device, DeviceTarget, Health,
    ImageRef, Network, NetworkTarget, Service, ServiceConfig, ServiceTarget, Volume, VolumeTarget,
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

/// Outcome of evaluating a `depends_on` condition.
#[derive(Debug, PartialEq)]
pub enum DependsOnConditionOutcome {
    Satisfied,
    // terminal failure with reason
    Failed(String),
    Pending,
}

/// Judges a `depends_on` condition from an observed container.
pub type ConditionEvaluator = fn(&Container) -> DependsOnConditionOutcome;

/// The evaluator for a condition observed on the container, or `None` for
/// `service_started`, which is observed on the service.
pub fn container_evaluator(condition: DependsOnCondition) -> Option<ConditionEvaluator> {
    match condition {
        DependsOnCondition::ServiceStarted => None,
        DependsOnCondition::ServiceHealthy => Some(evaluate_health),
        DependsOnCondition::ServiceCompletedSuccessfully => Some(evaluate_completion),
    }
}

/// Evaluate a dependency against a `depends_on` condition from its observed state.
fn evaluate_condition(dep: &Service, condition: DependsOnCondition) -> DependsOnConditionOutcome {
    use DependsOnConditionOutcome::*;
    match container_evaluator(condition) {
        // a container condition cannot be met before the container exists
        Some(evaluate) => dep.oci.as_ref().map_or(Pending, evaluate),
        // `service_started`: once the container leaves `created`, it stays true
        None if dep.started => Satisfied,
        None => Pending,
    }
}

/// Evaluate an observed container against the `service_healthy` condition.
pub fn evaluate_health(container: &Container) -> DependsOnConditionOutcome {
    use DependsOnConditionOutcome::*;
    match &container.health {
        Health::Healthy => Satisfied,
        Health::Unhealthy => Failed("unhealthy".to_owned()),
        // a running container reporting no health has no healthcheck
        // configured, which should fail fast in line with compose behavior
        Health::None if container.status == ContainerStatus::Running => {
            Failed("no healthcheck configured".to_owned())
        }
        // still starting, or not running yet
        Health::Starting | Health::None => Pending,
    }
}

/// Evaluate an observed container against the `service_completed_successfully`
/// condition.
pub fn evaluate_completion(container: &Container) -> DependsOnConditionOutcome {
    use DependsOnConditionOutcome::*;
    match &container.status {
        ContainerStatus::Stopped(0) => Satisfied,
        ContainerStatus::Stopped(code) => Failed(format!("exited with code {code}")),
        // still running
        _ => Pending,
    }
}

/// Whether a dependency may still reach a `depends_on` condition, i.e. the
/// condition has neither been met nor terminally failed, so it is worth waiting
/// for it.
pub fn depends_on_condition_pending(dep: &Service, condition: DependsOnCondition) -> bool {
    matches!(
        evaluate_condition(dep, condition),
        DependsOnConditionOutcome::Pending
    )
}

/// The services of a release, if the release exists.
pub fn release_services<'a>(
    device: &'a Device,
    app_uuid: &Uuid,
    commit: &Uuid,
) -> Option<&'a Map<String, Service>> {
    device
        .apps
        .get(app_uuid)
        .and_then(|app| app.releases.get(commit))
        .map(|release| &release.services)
}

/// The target services of a release, if the release exists in the target.
pub fn target_release_services<'a>(
    t_device: &'a DeviceTarget,
    app_uuid: &Uuid,
    commit: &Uuid,
) -> Option<&'a Map<String, ServiceTarget>> {
    t_device
        .apps
        .get(app_uuid)
        .and_then(|app| app.releases.get(commit))
        .map(|release| &release.services)
}

/// Evaluate every `depends_on` entry of a service against its dependencies in
/// the same release. The set is satisfied once all of them have resolved.
///
/// As in compose, required and optional entries are both waited on while
/// pending. They only differ on a terminal failure: a required one fails the
/// set, an optional one is merely warned about at start.
fn dependencies_outcome(
    device: &Device,
    app_uuid: &Uuid,
    commit: &Uuid,
    depends_on: &DependsOn,
) -> DependsOnConditionOutcome {
    use DependsOnConditionOutcome::*;

    let services = release_services(device, app_uuid, commit);

    let mut outcome = Satisfied;
    for (dep_name, spec) in depends_on.iter() {
        let dep_outcome = services
            .and_then(|services| services.get(dep_name))
            .map_or_else(
                // a dependency not in the release cannot be observed. Treating
                // an optional one as resolved keeps an entry naming a service
                // that never appears from wedging the release
                || if spec.required { Pending } else { Satisfied },
                |dep| evaluate_condition(dep, spec.condition),
            );

        match dep_outcome {
            Satisfied => {}
            // a terminal failure cannot be recovered from, so no need to look
            // at the remaining dependencies
            Failed(reason) if spec.required => return Failed(reason),
            // an optional failure never blocks, it is warned about at start
            Failed(_) => {}
            Pending => outcome = Pending,
        }
    }

    outcome
}

/// Whether every `depends_on` entry of a service has been satisfied by its
/// dependencies in the same release.
pub fn dependencies_satisfied(
    device: &Device,
    app_uuid: &Uuid,
    commit: &Uuid,
    depends_on: &DependsOn,
) -> bool {
    matches!(
        dependencies_outcome(device, app_uuid, commit, depends_on),
        DependsOnConditionOutcome::Satisfied
    )
}

/// Whether any required `depends_on` entry of a service has terminally failed its
/// condition, e.g. a dependency that is unhealthy, running with no healthcheck
/// configured, or has exited with an error.
pub fn any_dependency_failed(
    device: &Device,
    app_uuid: &Uuid,
    commit: &Uuid,
    depends_on: &DependsOn,
) -> bool {
    matches!(
        dependencies_outcome(device, app_uuid, commit, depends_on),
        DependsOnConditionOutcome::Failed(_)
    )
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
    use crate::models::Dependency;
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

    fn svc(value: serde_json::Value) -> Service {
        serde_json::from_value(value).unwrap()
    }

    fn svc_with_oci(mut oci: serde_json::Value) -> Service {
        let fields = oci.as_object_mut().unwrap();
        fields.insert("name".into(), json!("c"));
        fields.insert("created".into(), json!("2026-02-11T15:03:43Z"));
        svc(json!({"id": 1, "image": "alpine:latest", "config": {}, "oci": oci}))
    }

    /// Build a `depends_on` of `condition` entries with the given
    /// `(name, required)` pairs.
    fn deps(condition: DependsOnCondition, entries: &[(&str, bool)]) -> DependsOn {
        entries
            .iter()
            .map(|(name, required)| {
                (
                    name.to_string(),
                    Dependency {
                        condition,
                        restart: false,
                        required: *required,
                    },
                )
            })
            .collect::<HashMap<_, _>>()
            .into()
    }

    fn add_started_deps(entries: &[(&str, bool)]) -> DependsOn {
        deps(DependsOnCondition::ServiceStarted, entries)
    }

    fn dependencies_satisfied_for(device: &Device, deps: &DependsOn) -> bool {
        dependencies_satisfied(device, &"app-uuid".into(), &"rel-uuid".into(), deps)
    }

    fn any_dependency_failed_for(device: &Device, deps: &DependsOn) -> bool {
        any_dependency_failed(device, &"app-uuid".into(), &"rel-uuid".into(), deps)
    }

    #[test]
    fn evaluate_condition_started() {
        let started = DependsOnCondition::ServiceStarted;
        let mut s = svc(json!({"id": 1, "image": "alpine:latest", "started": true, "config": {}}));
        assert_eq!(
            evaluate_condition(&s, started),
            DependsOnConditionOutcome::Satisfied
        );
        s.started = false;
        assert_eq!(
            evaluate_condition(&s, started),
            DependsOnConditionOutcome::Pending
        );
    }

    #[test]
    fn evaluate_condition_health() {
        let healthy = DependsOnCondition::ServiceHealthy;

        let s = svc_with_oci(json!({"status": "running", "health": "healthy"}));
        assert_eq!(
            evaluate_condition(&s, healthy),
            DependsOnConditionOutcome::Satisfied
        );

        let s = svc_with_oci(json!({"status": "running", "health": "unhealthy"}));
        assert_eq!(
            evaluate_condition(&s, healthy),
            DependsOnConditionOutcome::Failed("unhealthy".into())
        );

        // still waiting on healthcheck outcome
        let s = svc_with_oci(json!({"status": "running", "health": "starting"}));
        assert_eq!(
            evaluate_condition(&s, healthy),
            DependsOnConditionOutcome::Pending
        );

        // no configured healthcheck fails fast at runtime
        let s = svc_with_oci(json!({"status": "running", "health": "none"}));
        assert_eq!(
            evaluate_condition(&s, healthy),
            DependsOnConditionOutcome::Failed("no healthcheck configured".into())
        );

        // container still starting
        let s = svc_with_oci(json!({"status": "created", "health": "none"}));
        assert_eq!(
            evaluate_condition(&s, healthy),
            DependsOnConditionOutcome::Pending
        );

        // no container observed yet
        let s = svc(json!({"id": 1, "image": "alpine:latest", "config": {}}));
        assert_eq!(
            evaluate_condition(&s, healthy),
            DependsOnConditionOutcome::Pending
        );
    }

    #[test]
    fn evaluate_condition_completion() {
        let completed = DependsOnCondition::ServiceCompletedSuccessfully;

        let s = svc_with_oci(json!({"status": "stopped", "exit_code": 0}));
        assert_eq!(
            evaluate_condition(&s, completed),
            DependsOnConditionOutcome::Satisfied
        );

        // failed due to non-zero exit
        let s = svc_with_oci(json!({"status": "stopped", "exit_code": 137}));
        assert_eq!(
            evaluate_condition(&s, completed),
            DependsOnConditionOutcome::Failed("exited with code 137".into())
        );

        // a running container carries no exit code
        let s = svc_with_oci(json!({"status": "running"}));
        assert_eq!(
            evaluate_condition(&s, completed),
            DependsOnConditionOutcome::Pending
        );

        // no container observed yet
        let s = svc(json!({"id": 1, "image": "alpine:latest", "config": {}}));
        assert_eq!(
            evaluate_condition(&s, completed),
            DependsOnConditionOutcome::Pending
        );
    }

    #[test]
    fn empty_depends_on_is_satisfied() {
        let device = device_with(json!({}));
        assert!(dependencies_satisfied_for(&device, &DependsOn::default()));
    }

    #[test]
    fn satisfied_when_required_dependency_is_met() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {}},
        }));
        assert!(dependencies_satisfied_for(
            &device,
            &add_started_deps(&[("db", true)])
        ));
    }

    #[test]
    fn blocks_on_unmet_required_dependency() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": false, "config": {}},
        }));
        assert!(!dependencies_satisfied_for(
            &device,
            &add_started_deps(&[("db", true)])
        ));
    }

    #[test]
    fn blocks_on_pending_optional_dependency() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": false, "config": {}},
        }));
        assert!(!dependencies_satisfied_for(
            &device,
            &add_started_deps(&[("db", false)])
        ));
    }

    #[test]
    fn proceeds_on_failed_optional_dependency() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {},
                   "oci": {"name": "db", "created": "2026-02-11T15:03:43Z", "status": "running", "health": "unhealthy"}},
        }));
        let deps = deps(DependsOnCondition::ServiceHealthy, &[("db", false)]);
        assert!(dependencies_satisfied_for(&device, &deps));
        // the failure is reported by `start_service`, it never blocks planning
        assert!(!any_dependency_failed_for(&device, &deps));
    }

    #[test]
    fn blocks_on_failed_required_dependency() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {},
                   "oci": {"name": "db", "created": "2026-02-11T15:03:43Z", "status": "running", "health": "unhealthy"}},
        }));
        let deps = deps(DependsOnCondition::ServiceHealthy, &[("db", true)]);
        assert!(!dependencies_satisfied_for(&device, &deps));
        assert!(any_dependency_failed_for(&device, &deps));
    }

    #[test]
    fn blocks_when_a_required_dependency_is_unmet_even_if_an_optional_one_is_met() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {}},
            "cache": {"id": 2, "image": "alpine:latest", "started": false, "config": {}},
        }));
        assert!(!dependencies_satisfied_for(
            &device,
            &add_started_deps(&[("db", false), ("cache", true)])
        ));
    }

    #[test]
    fn missing_required_dependency_blocks() {
        let device = device_with(json!({}));
        assert!(!dependencies_satisfied_for(
            &device,
            &add_started_deps(&[("db", true)])
        ));
    }

    #[test]
    fn missing_optional_dependency_proceeds() {
        let device = device_with(json!({}));
        assert!(dependencies_satisfied_for(
            &device,
            &add_started_deps(&[("db", false)])
        ));
    }

    #[test]
    fn satisfied_when_required_healthy_dependency_is_confirmed() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {},
                   "oci": {"name": "db", "created": "2026-02-11T15:03:43Z", "status": "running", "health": "healthy"}},
        }));
        assert!(dependencies_satisfied_for(
            &device,
            &deps(DependsOnCondition::ServiceHealthy, &[("db", true)])
        ));
    }

    #[test]
    fn blocks_on_unconfirmed_required_healthy_dependency() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {}},
        }));
        assert!(!dependencies_satisfied_for(
            &device,
            &deps(DependsOnCondition::ServiceHealthy, &[("db", true)])
        ));
    }

    #[test]
    fn blocks_on_unconfirmed_optional_healthy_dependency() {
        let device = device_with(json!({
            "db": {"id": 1, "image": "alpine:latest", "started": true, "config": {}},
        }));
        assert!(!dependencies_satisfied_for(
            &device,
            &deps(DependsOnCondition::ServiceHealthy, &[("db", false)])
        ));
    }

    #[test]
    fn satisfied_when_required_completed_dependency_is_confirmed() {
        let device = device_with(json!({
            "migrate": {"id": 1, "image": "alpine:latest", "started": true, "config": {},
                        "oci": {"name": "migrate", "created": "2026-02-11T15:03:43Z", "status": "stopped", "exit_code": 0}},
        }));
        assert!(dependencies_satisfied_for(
            &device,
            &deps(
                DependsOnCondition::ServiceCompletedSuccessfully,
                &[("migrate", true)]
            )
        ));
    }

    #[test]
    fn blocks_on_unconfirmed_required_completed_dependency() {
        let device = device_with(json!({
            "migrate": {"id": 1, "image": "alpine:latest", "started": true, "config": {}},
        }));
        assert!(!dependencies_satisfied_for(
            &device,
            &deps(
                DependsOnCondition::ServiceCompletedSuccessfully,
                &[("migrate", true)]
            )
        ));
    }
}
