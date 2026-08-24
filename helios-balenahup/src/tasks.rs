use std::fs;
use std::io;

use mahler::extract::{Args, Res, System, Target, View};
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};
use mahler::{exception, job};
use tracing::debug;

use crate::common_types::{HostRuntimeDir, Uuid};
use crate::oci::{self, Client as Docker, RegistryAuth, WithContext};
use crate::store::{self as store, DocumentStore};
use crate::util::breadcrumb;
use crate::util::dirs::runtime_dir;
use crate::util::fs::run_async;
use crate::util::systemd;
use crate::util::tar;

use crate::overlays::{
    deploy_overlay, overlays_ready, redeploy_overlay, remove_overlay,
    remove_overlay_and_mark_reboot,
};
use crate::reboot::{mark_pending_reboot, reboot_to_activate, reboot_to_apply_overlays};

use super::BALENAHUP;
use super::models::{
    Device, DeviceTarget, Host, HostApp, HostRelease, HostReleaseStatus, HostReleaseTarget,
    HostTarget, OverlayStatus,
};

/// Whether the host is still validating this boot.
///
/// Every job under a release must refuse while this holds, or its work lands
/// inside the rollback window. Tasks use `enforce!`, methods expand to nothing.
pub(crate) fn host_is_validating(device: &Device) -> bool {
    device
        .host
        .as_ref()
        .is_some_and(|host| host.host_validating)
}

/// Wait for the host validation to finish before doing host work.
///
/// The task fails rather than blocking until the window closes, and the
/// failure is what makes the wait work.
/// Failing lands on the loop's recoverable path that re-reads
/// the device state, so `Host::host_validating` is re-derived from the units,
/// and re-plans. The device stays in `ApplyingChanges` until the window
/// closes, and the retry interval sets the polling cadence.
///
/// The state change is still declared: the planner picks the task because it
/// closes the gap on `/host/host_validating`, and only then does the IO run.
fn await_host_validation(mut validating: View<bool>) -> IO<bool, HostUpdateError> {
    enforce!(*validating, "the host is not validating anything");
    *validating = false;

    with_io(validating, async move |_| {
        Err(HostUpdateError::HostValidating)
    })
}

#[derive(Debug, thiserror::Error)]
enum HostUpdateError {
    #[error(transparent)]
    Docker(#[from] oci::Error),

    #[error(transparent)]
    Store(#[from] store::Error),

    #[error(transparent)]
    IO(#[from] io::Error),

    #[error(transparent)]
    Systemd(#[from] systemd::Error),

    #[error("host validation in progress")]
    HostValidating,
}

/// Initialize the release
///
/// Applies to `create(/host/releases/<rel_uuid>)`
fn init_hostapp_release(
    maybe_rel: View<Option<HostRelease>>,
    Args(release_uuid): Args<String>,
    Target(tgt): Target<HostRelease>,
    System(device): System<Device>,
    store: Res<DocumentStore>,
) -> IO<HostRelease, store::Error> {
    let HostReleaseTarget { app, hostapp, .. } = tgt;

    // Get the running status by comparing to the current os build
    let is_running = device
        .host
        .and_then(|host| host.meta.build)
        .is_some_and(|os_build| os_build == hostapp.build);

    let status = if is_running {
        HostReleaseStatus::Running
    } else {
        HostReleaseStatus::Created
    };

    // Create a release using the target metadata
    let rel = HostRelease {
        app,
        hostapp: HostApp {
            image: hostapp.image,
            build: hostapp.build,
            updater: hostapp.updater,
            install_attempts: 0,
        },
        status,
        overlays: mahler::state::Map::new(),
    };

    // set the host release with the details from the target
    let host_release = maybe_rel.create(rel);

    with_io(host_release, async move |host_release| {
        // write the release data into the store
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .put(
                format!("host/releases/{release_uuid}/hostapp"),
                &*host_release,
            )
            .await?;
        Ok(host_release)
    })
}

/// Install the hostapp release
///
/// Applies to `create(/host/releases/<commit>)`
// mahler extractors, not a wide interface
#[allow(clippy::too_many_arguments)]
fn install_hostapp_release(
    mut release: View<HostRelease>,
    Args(release_uuid): Args<String>,
    Target(tgt): Target<HostRelease>,
    System(device): System<Device>,
    docker: Res<Docker>,
    store: Res<DocumentStore>,
    registry_auth: Res<RegistryAuth>,
    host_runtime_dir: Res<HostRuntimeDir>,
) -> IO<HostRelease, HostUpdateError> {
    enforce!(!host_is_validating(&device), "host validation in progress");
    // this task is only applicable if the release is not already running
    enforce!(
        release.status == HostReleaseStatus::Created,
        "OS release already installed"
    );

    // Stage every overlay before committing to the install. The reboot has its
    // own check, so this is not about ordering the activation; it is about not
    // installing a hostapp that a failing overlay will then abort. The install
    // cannot be undone and this task only runs from `Created`, so a release
    // installed alongside a failed overlay would sit at `Installed` with no way
    // forward short of a reboot.
    enforce!(overlays_ready(&release, &tgt), "overlays not yet deployed");

    // increase the install counter
    release.hostapp.install_attempts += 1;
    with_io(release, async move |mut release| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");

        let container_helper = docker.non_namepaced_container();

        // remove any existing `balenahup` container
        container_helper.remove(BALENAHUP).await?;

        // write the release data into the store to update install_attempts
        local_store
            .put(format!("host/releases/{release_uuid}/hostapp"), &*release)
            .await?;

        // commit the install attemps to the local state
        let _ = release.commit().await;

        // Pull the docker image for the updater
        debug!(
            "pull hostapp updater script from '{}'",
            release.hostapp.updater
        );
        let credentials = registry_auth
            .as_ref()
            .and_then(|auth| auth.credentials(&release.hostapp.updater));
        docker
            .image()
            .pull(&release.hostapp.updater, credentials)
            .await
            .with_context(|| {
                format!(
                    "failed to pull hostapp updater script from '{}",
                    release.hostapp.updater
                )
            })?;

        // create a `balenahup` container from the update image
        let id = container_helper
            .create_tmp(BALENAHUP, &release.hostapp.updater)
            .await?;

        // configure the target dir in $RUNTIME_DIR/balenahup
        let target_dir = runtime_dir().join(BALENAHUP);
        let host_target_dir = host_runtime_dir
            .as_ref()
            .expect("should not be nil")
            .join(BALENAHUP);

        // read scripts from the container
        let bytes = container_helper.read_from(&id, "/app").await?;

        run_async(move || {
            // remove the target dir if it exists
            if let Err(e) = fs::remove_dir_all(&target_dir)
            // ignore the error if the directory does not exist
            && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(e);
            }
            fs::create_dir_all(&target_dir)?;

            // extract the scripts into the target directory
            tar::unpack_from(&bytes, "/app", target_dir)?;

            Ok(())
        })
        .await?;

        // call systemd run using `/tmp/balena-supervisor/balenahup` as the workdir, wait for
        // the script to finish
        debug!("running the updater script");
        let hup_script = host_target_dir.join("entry.sh");
        let hup_script = hup_script.to_str().expect("should be valid unicode");
        let hup_cmd = systemd::Command::new(hup_script)
            .args(&[
                "--app-uuid",
                release.app.as_str(),
                "--release-commit",
                release_uuid.as_str(),
                "--target-image-uri",
                release.hostapp.image.as_str(),
                "--no-reboot",
            ])
            .workdir(host_target_dir);
        systemd::run("os-update", &hup_cmd).await?;

        // leave a breadcrumb in the runtime-dir to indicate that the os release was installed.
        // The breadcrumb will be removed after a reboot, so the worker will be able to re-try
        // HUP after a rollback. Since the hup script may reboot immediately after finishing, this
        // step may be skipped, but that is fine since the breadcrumb is not longer needed at that
        // point
        breadcrumb::set(&format!("{BALENAHUP}-{release_uuid}-breadcrumb")).await?;

        Ok(release)
    })
    .map(|mut rel| {
        // set the status after the successful run of the task
        rel.status = HostReleaseStatus::Installed;
        rel
    })
}

/// update the local storage metadata about the hostapp if the
/// release is already the current release.
///
/// This is only used if a new version of the updater script is
/// released, in which case we want to update the internal reference.
///
/// handle an `update(/host/releases/<commit>)`
fn update_script_uri(
    mut rel: View<HostRelease>,
    Target(tgt): Target<HostRelease>,
    Args(release_uuid): Args<String>,
    System(device): System<Device>,
    store: Res<DocumentStore>,
) -> IO<HostRelease, store::Error> {
    enforce!(!host_is_validating(&device), "host validation in progress");
    // do nothing if the release is not currently running
    enforce!(
        rel.status == HostReleaseStatus::Running,
        "OS release is not running yet"
    );

    // the only change that this applies is to the updater script
    rel.hostapp.updater = tgt.hostapp.updater;

    with_io(rel, async move |rel| {
        // write the release data into the store
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .put(format!("host/releases/{release_uuid}/hostapp"), &*rel)
            .await?;

        Ok(rel)
    })
}

/// Forget a release: remove the artifacts it owns, then its metadata.
///
/// Applies to `delete(/host/releases/<commit>)`.
fn remove_release(rel: View<HostRelease>, System(device): System<Device>) -> Vec<Task> {
    if host_is_validating(&device) {
        return Vec::new();
    }

    let mut tasks: Vec<Task> = rel
        .overlays
        .keys()
        .map(|name| remove_overlay.with_arg("name", name.clone()))
        .collect();

    // Only a mounted overlay needs the reboot: removing a container that never
    // made it into the root changes nothing about the running system.
    if rel
        .overlays
        .values()
        .any(|overlay| overlay.status == OverlayStatus::Active)
    {
        tasks.push(mark_pending_reboot.into_task());
    }

    tasks.push(remove_old_metadata.into_task());
    tasks
}

/// Reached only through `remove_release`, which removes the release's overlays
/// first.
fn remove_old_metadata(
    rel: View<HostRelease>,
    Args(release_uuid): Args<String>,
    store: Res<DocumentStore>,
) -> IO<Option<HostRelease>, store::Error> {
    // remove the old release
    let rel = rel.delete();

    with_io(rel, async move |rel| {
        // remove the old release metadata
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .delete(format!("host/releases/{release_uuid}/hostapp"))
            .await?;

        Ok(rel)
    })
}

/// Whether this release's record is still the device's account of what it is
/// running.
fn release_still_accounted_for(rel: View<HostRelease>) -> bool {
    release_is_accounted_for(&rel)
}

/// Booted on it: the update has not been activated yet, or has rolled back.
pub(crate) fn release_is_accounted_for(rel: &HostRelease) -> bool {
    rel.status == HostReleaseStatus::Running
}

/// The release exceeded its install budget and is left alone until an
/// operator looks at the device.
pub(crate) fn too_many_failed_installs(rel: &HostRelease) -> bool {
    rel.status == HostReleaseStatus::Created && rel.hostapp.install_attempts >= 3
}

/// An overlay failed to activate at the image the target still asks for, so
/// the release is left alone rather than retried.
pub(crate) fn overlay_activation_failed(rel: &HostRelease, tgt: &HostReleaseTarget) -> bool {
    tgt.overlays.iter().any(|(name, tgt_overlay)| {
        rel.overlays
            .get(name)
            .is_some_and(|ov| ov.status == OverlayStatus::Failed && ov.image == tgt_overlay.image)
    })
}

/// The runtimes the target's overlays ask for, deduplicated.
fn requested_runtimes(tgt: &HostReleaseTarget) -> Vec<&str> {
    let mut wanted: Vec<&str> = tgt
        .overlays
        .values()
        .map(|ov| ov.runtime.as_str())
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    wanted
}

/// An overlay the target brings asks for a runtime this engine does not
/// register, so the release cannot be deployed on this host at all.
fn overlay_runtime_unavailable(registered: &[String], tgt: &HostReleaseTarget) -> bool {
    requested_runtimes(tgt)
        .iter()
        .any(|wanted| !registered.iter().any(|name| name == wanted))
}

/// The reason an operator reads when a release's overlays name a runtime the
/// engine does not register. It lists every runtime the release asks for: a
/// description sees the target, not the device, so it cannot single out the
/// missing one.
fn unavailable_runtime_reason(tgt: &HostReleaseTarget) -> String {
    format!(
        "host OS engine does not register a runtime the overlays ask for ('{}'), update the OS first",
        requested_runtimes(tgt).join("', '")
    )
}

/// Drop the target releases this host cannot run, and say why for each.
///
/// The release the device runs is kept: `release_still_accounted_for` declines
/// to forget one the target no longer names.
pub fn reject_unsupported_releases(tgt: &mut HostTarget, host: &Host) -> Vec<String> {
    let mut reasons = Vec::new();
    tgt.releases.retain(|_, rel| {
        if overlay_runtime_unavailable(&host.engine_runtimes, rel) {
            reasons.push(unavailable_runtime_reason(rel));
            return false;
        }
        true
    });
    reasons
}

/// True while some overlay the target dropped is still present and the planner
/// is free to remove it.
///
/// This mirrors the exceptions on `/host/releases/{release_uuid}` below. A
/// removal they hold back must not hold back the reboot. Keep the two in step.
pub(crate) fn overlay_removal_pending(device: &Device, target: &DeviceTarget) -> bool {
    let Some(host) = device.host.as_ref() else {
        return false;
    };
    let target_releases = target.host.as_ref().map(|h| &h.releases);
    host.releases.iter().any(|(uuid, rel)| {
        match target_releases.and_then(|releases| releases.get(uuid)) {
            // The target keeps the release: its dropped overlays are removed one
            // by one, unless the release is frozen.
            Some(tgt) => {
                !too_many_failed_installs(rel)
                    && !overlay_activation_failed(rel, tgt)
                    && rel
                        .overlays
                        .keys()
                        .any(|name| !tgt.overlays.contains_key(name))
            }
            // The target forgets the release: its overlays go with it, once the
            // device no longer depends on it.
            None => !release_is_accounted_for(rel) && !rel.overlays.is_empty(),
        }
    })
}

#[derive(Debug, thiserror::Error)]
pub enum HostCleanupError {
    #[error(transparent)]
    Oci(#[from] oci::Error),

    #[error(transparent)]
    Store(#[from] store::Error),
}

/// Clean up balenahup container and host release metadata/images.
///
/// Called from the main device cleanup task when the balenahup feature is active.
pub fn cleanup_hostapp(
    host: View<Option<Host>>,
    docker: Res<Docker>,
    store: Res<DocumentStore>,
) -> IO<Option<Host>, HostCleanupError> {
    with_io(host, async move |host| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");

        // clean up balenahup container if it exists
        docker.container().remove(BALENAHUP).await?;

        // clean up old host release metadata and images
        let host_releases_view = local_store.as_view().at("host/releases")?;
        let host_release_uuids: Vec<Uuid> = host_releases_view
            .keys()
            .await?
            .into_iter()
            .map(Uuid::from)
            .collect();

        for release_uuid in host_release_uuids {
            if let Some(rel) = host_releases_view
                .get::<HostRelease>(format!("{release_uuid}/hostapp"))
                .await?
            {
                // remove the updater image if it exists
                docker.image().remove(&rel.hostapp.updater).await?;
            }

            // if the release does not exist in the target state
            if !host
                .as_ref()
                .map(|host| host.releases.contains_key(&release_uuid))
                .unwrap_or_default()
            {
                // remove the release metadata
                host_releases_view
                    .delete(format!("{release_uuid}/*"))
                    .await?;
            }
        }
        Ok(host)
    })
}

pub fn with_hostapp_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    worker
        .jobs(
            "/host/releases/{release_uuid}",
            [
                job::create(init_hostapp_release).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("initialize host OS release '{release_uuid}'")
                    },
                ),
                job::update(install_hostapp_release).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("install host OS release '{release_uuid}'")
                    },
                ),
                job::update(reboot_to_activate).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("reboot to activate host OS release '{release_uuid}'")
                    },
                ),
                job::update(update_script_uri).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("update metadata for host OS release '{release_uuid}'")
                    },
                ),
                job::delete(remove_release).with_description(|Args(release_uuid): Args<String>| {
                    format!("remove host OS release '{release_uuid}'")
                }),
                // Reachable only through `remove_release`, which removes the
                // release's overlays before forgetting it.
                job::none(remove_old_metadata).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("remove metadata for host OS release '{release_uuid}'",)
                    },
                ),
            ],
        )
        .jobs(
            "/host/releases/{release_uuid}/overlays/{name}",
            [
                job::create(deploy_overlay).with_description(
                    |Args((release_uuid, name)): Args<(String, String)>| {
                        format!("deploy overlay '{name}' for host OS release '{release_uuid}'")
                    },
                ),
                job::update(redeploy_overlay),
                job::delete(remove_overlay_and_mark_reboot),
                // Reachable only through a method: `remove_overlay_and_mark_reboot`
                // on delete, `redeploy_overlay` on an image change.
                job::none(remove_overlay).with_description(
                    |Args((release_uuid, name)): Args<(String, String)>| {
                        format!("remove overlay '{name}' for host OS release '{release_uuid}'")
                    },
                ),
            ],
        )
        .jobs(
            "/host/pending_reboot",
            [
                job::update(reboot_to_apply_overlays)
                    .with_description(|| "reboot to apply overlay changes"),
                // The target never asks for the flag, so this is only ever
                // reached by expansion from the overlay removal method.
                job::none(mark_pending_reboot)
                    .with_description(|| "mark overlay change as awaiting a reboot"),
            ],
        )
        .job(
            "/host/host_validating",
            job::update(await_host_validation)
                .with_description(|| "wait for the host validation to finish"),
        )
        .job(
            "/host",
            job::none(cleanup_hostapp).with_description(|| "clean-up host metadata and images"),
        )
        // ignore requests to delete the host field if the target OS is set to null
        .exception(
            "/host",
            exception::delete(|| true)
                .with_description(|| "target host release is invalid or missing"),
        )
        // ignore requests to update the host if we reached the number of install attempts
        .exception(
            "/host/releases/{release_uuid}",
            exception::update(|rel: View<HostRelease>| too_many_failed_installs(&rel))
                .with_description(|| "too many failed installs, check device"),
        )
        // abort the release if an overlay failed to activate at the target image
        .exception(
            "/host/releases/{release_uuid}",
            exception::update(|rel: View<HostRelease>, Target(tgt): Target<HostRelease>| {
                overlay_activation_failed(&rel, &tgt)
            })
            .with_description(|Args(release_uuid): Args<String>| {
                format!(
                    "overlay activation failed for host OS release '{release_uuid}', check device"
                )
            }),
        )
        // keep the record of the release the device is running; forgetting it
        // strands that release's overlay containers
        .exception(
            "/host/releases/{release_uuid}",
            exception::delete(release_still_accounted_for).with_description(
                |Args(release_uuid): Args<String>| {
                    format!("host OS release '{release_uuid}' is still running")
                },
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::common_types::{ImageUri, OperatingSystem};
    use crate::models::{HostAppTarget, OverlayTarget};
    use mahler::state::Map;

    /// A host whose engine registers the given runtimes.
    fn host(engine_runtimes: &[&str]) -> Host {
        let mut host = Host::new(OperatingSystem {
            name: "balenaOS".to_string(),
            version: Some("6.5.0".to_string()),
            build: None,
        });
        host.engine_runtimes = engine_runtimes.iter().map(|r| r.to_string()).collect();
        host
    }

    /// A device whose engine registers the given runtimes.
    fn device(engine_runtimes: &[&str]) -> Device {
        Device {
            host: Some(host(engine_runtimes)),
        }
    }

    /// A target release carrying one overlay per entry, each asking for the
    /// runtime named beside it.
    fn target(overlays: &[(&str, &str)]) -> HostReleaseTarget {
        HostReleaseTarget {
            app: Uuid::from("1b2c3d4e5f60718293a4b5c6d7e8f900"),
            hostapp: HostAppTarget {
                image: ImageUri::from_static("registry2.balena-cloud.com/v2/hostapp:latest"),
                build: "abc1234".to_string(),
                updater: ImageUri::from_static("registry2.balena-cloud.com/v2/updater:latest"),
            },
            status: HostReleaseStatus::Running,
            overlays: overlays
                .iter()
                .map(|(name, runtime)| {
                    (
                        name.to_string(),
                        OverlayTarget {
                            image: ImageUri::from_static(
                                "registry2.balena-cloud.com/v2/overlay:latest",
                            ),
                            status: OverlayStatus::Active,
                            runtime: runtime.to_string(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn unavailable(engine: &[&str], overlays: &[(&str, &str)]) -> bool {
        let d = device(engine);
        let registered = &d.host.as_ref().unwrap().engine_runtimes;
        overlay_runtime_unavailable(registered, &target(overlays))
    }

    #[test]
    fn a_runtime_the_engine_does_not_register_is_unavailable() {
        assert!(unavailable(&["runc"], &[("ebpf", "extension")]));
    }

    #[test]
    fn an_engine_that_registers_nothing_satisfies_no_request() {
        // A host that predates extensions: the list was read, and it is empty.
        assert!(unavailable(&[], &[("ebpf", "extension")]));
    }

    #[test]
    fn a_release_without_overlays_asks_for_nothing() {
        // An ordinary host OS update reaches a host that predates extensions
        // untouched by either guard.
        assert!(!unavailable(&[], &[]));
    }

    #[test]
    fn every_runtime_the_overlays_name_is_registered() {
        let overlays = [("ebpf", "extension"), ("tracing", "extension")];
        assert!(!unavailable(&["runc", "extension"], &overlays));
    }

    /// A target carrying one release with the given overlays.
    fn host_target(overlays: &[(&str, &str)]) -> HostTarget {
        let mut releases = Map::new();
        releases.insert(Uuid::from("target-release"), target(overlays));
        HostTarget {
            releases,
            pending_reboot: false,
            host_validating: false,
        }
    }

    #[test]
    fn a_release_the_engine_can_run_is_kept() {
        let mut tgt = host_target(&[("ebpf", "extension")]);
        let reasons = reject_unsupported_releases(&mut tgt, &host(&["runc", "extension"]));
        assert!(reasons.is_empty());
        assert_eq!(tgt.releases.len(), 1);
    }

    #[test]
    fn a_release_naming_an_unregistered_runtime_is_dropped() {
        // X-04: a profile activated on a pre-extension OS.
        let mut tgt = host_target(&[("ebpf", "extension")]);
        let reasons = reject_unsupported_releases(&mut tgt, &host(&[]));
        assert!(tgt.releases.is_empty());
        assert_eq!(
            reasons,
            vec![
                "host OS engine does not register a runtime the overlays ask for ('extension'), update the OS first"
                    .to_string()
            ]
        );
    }

    #[test]
    fn pruning_leaves_the_reboot_request_alone() {
        // The pending_reboot diff schedules a reboot already due.
        let mut tgt = host_target(&[("ebpf", "extension")]);
        reject_unsupported_releases(&mut tgt, &host(&[]));
        assert!(!tgt.pending_reboot);
    }

    #[test]
    fn the_reason_names_every_runtime_the_release_asked_for() {
        // The description cannot see the device, so it cannot single out the
        // missing one; it must not claim any of them is missing either.
        let tgt = target(&[("ebpf", "extension"), ("tracing", "runc")]);
        assert_eq!(
            unavailable_runtime_reason(&tgt),
            "host OS engine does not register a runtime the overlays ask for \
             ('extension', 'runc'), update the OS first"
        );
    }
}
