use std::fs;
use std::io;

use mahler::extract::{Args, Res, System, SystemTarget, Target, View};
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
use crate::util::reboot::mark_reboot_required;
use crate::util::systemd;
use crate::util::tar;

use crate::overlays::{
    deploy_overlay, deploy_overlay_and_mark_reboot, overlays_ready, redeploy_overlay,
    remove_overlay, remove_overlay_and_mark_reboot,
};

use super::BALENAHUP;
use super::models::{
    Device, Host, HostApp, HostRelease, HostReleaseStatus, HostReleaseTarget, HostTarget, Overlay,
    OverlayStatus,
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

/// The task marking a reboot as required, unless one already is: the mark
/// refuses to run then, failing the whole method.
pub(crate) fn request_reboot(device: &Device) -> Option<Task> {
    (!device.needs_reboot).then(|| mark_reboot_required.into_task())
}

/// The task followed by a request for the reboot that activates its change.
///
/// Two tasks because a task may only write its own subtree.
pub(crate) fn with_reboot(task: Task, device: &Device) -> Vec<Task> {
    let mut tasks = vec![task];
    tasks.extend(request_reboot(device));
    tasks
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

/// Install the hostapp release and request the reboot that activates it.
///
/// Handles `update(/host/releases/<commit>)`
fn install_and_mark_reboot(System(device): System<Device>) -> Vec<Task> {
    with_reboot(install_hostapp_release.into_task(), &device)
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

/// Whether the release has changes only a reboot activates: an installed
/// hostapp, or staged overlays the target keeps on the running release.
///
/// Waits for every target overlay, so one reboot activates them all.
fn awaits_activation(rel: &HostRelease, tgt: &HostReleaseTarget) -> bool {
    overlays_ready(rel, tgt)
        && match rel.status {
            HostReleaseStatus::Installed => true,
            HostReleaseStatus::Running => rel.overlays.iter().any(|(name, overlay)| {
                overlay.status == OverlayStatus::Deployed && tgt.overlays.contains_key(name)
            }),
            HostReleaseStatus::Created => false,
        }
}

/// Bring up the installed hostapp and the staged overlays.
fn activate(rel: &mut HostRelease) {
    rel.status = HostReleaseStatus::Running;
    for overlay in rel.overlays.values_mut() {
        if overlay.status == OverlayStatus::Deployed {
            overlay.status = OverlayStatus::Active;
        }
    }
}

/// Activate the release once the reboot that brings it up has landed.
///
/// Pure state: the reboot is what activates it. The install and the deploys
/// request that reboot, so a release awaiting activation with no reboot pending
/// has been through it.
///
/// Handles `update(/host/releases/<commit>)`
fn activate_release(
    mut rel: View<HostRelease>,
    Target(tgt): Target<HostRelease>,
    System(device): System<Device>,
) -> Option<View<HostRelease>> {
    if device.needs_reboot || host_is_validating(&device) || !awaits_activation(&rel, &tgt) {
        return None;
    }
    activate(&mut rel);
    Some(rel)
}

/// Whether the release is settled, or will be once a reboot activates it.
pub fn release_settled_on_reboot(rel: &HostRelease, tgt: &HostReleaseTarget) -> bool {
    let mut rel = rel.clone();
    if awaits_activation(&rel, tgt) {
        activate(&mut rel);
    }
    release_settled(&rel, tgt)
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
        tasks.extend(request_reboot(&device));
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
///
/// Booted on it, and no target release runs the same build: the update has not
/// been activated yet, or has rolled back.
fn release_still_accounted_for(
    rel: View<HostRelease>,
    SystemTarget(target): SystemTarget<Device>,
) -> bool {
    rel.status == HostReleaseStatus::Running
        && !target.host.as_ref().is_some_and(|host| {
            host.releases
                .values()
                .any(|tgt| tgt.hostapp.build == rel.hostapp.build)
        })
}

/// The release exceeded its install budget and is left alone until an
/// operator looks at the device.
fn too_many_failed_installs(rel: &HostRelease) -> bool {
    rel.status == HostReleaseStatus::Created && rel.hostapp.install_attempts >= 3
}

/// An overlay failed to activate at the image the target still asks for, so
/// the release is left alone rather than retried.
fn overlay_activation_failed(rel: &HostRelease, tgt: &HostReleaseTarget) -> bool {
    tgt.overlays.iter().any(|(name, tgt_overlay)| {
        rel.overlays
            .get(name)
            .is_some_and(|ov| ov.status == OverlayStatus::Failed && ov.image == tgt_overlay.image)
    })
}

/// The target asks again for a withdrawn overlay. Its container keeps the name
/// until the OS removes it on boot, so the deploy waits for the reboot.
fn overlay_awaits_reboot(rel: &HostRelease, tgt: &HostReleaseTarget) -> bool {
    tgt.overlays.keys().any(|name| {
        rel.overlays
            .get(name)
            .is_some_and(|ov| ov.status == OverlayStatus::Withdrawn)
    })
}

/// Whether the target release is installed, or frozen by an exception on
/// `/host/releases/{release_uuid}`. Keep the two in step.
///
/// Installed means running with exactly the target overlays active. A release
/// awaiting its reboot gets there through `activate_release`.
fn release_settled(rel: &HostRelease, tgt: &HostReleaseTarget) -> bool {
    let installed = rel.status == HostReleaseStatus::Running
        && rel
            .overlays
            .values()
            .filter(|o| o.status != OverlayStatus::Withdrawn)
            .count()
            == tgt.overlays.len()
        && tgt.overlays.iter().all(|(name, tgt_overlay)| {
            rel.overlays.get(name).is_some_and(|o| {
                o.status == OverlayStatus::Active
                    && o.image == tgt_overlay.image
                    && o.runtime == tgt_overlay.runtime
            })
        });
    installed
        || too_many_failed_installs(rel)
        || overlay_activation_failed(rel, tgt)
        || overlay_awaits_reboot(rel, tgt)
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
fn overlay_runtime_unavailable(registered: &[oci::Runtime], tgt: &HostReleaseTarget) -> bool {
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
pub fn reject_unsupported_releases(tgt: &mut HostTarget, runtimes: &[oci::Runtime]) -> Vec<String> {
    let mut reasons = Vec::new();
    tgt.releases.retain(|_, rel| {
        if overlay_runtime_unavailable(runtimes, rel) {
            reasons.push(unavailable_runtime_reason(rel));
            return false;
        }
        true
    });
    reasons
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
                job::update(install_and_mark_reboot),
                // Reachable only through `install_and_mark_reboot`
                job::none(install_hostapp_release).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("install host OS release '{release_uuid}'")
                    },
                ),
                job::update(activate_release).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("activate host OS release '{release_uuid}'")
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
                job::create(deploy_overlay_and_mark_reboot),
                // Reachable only through `deploy_overlay_and_mark_reboot`
                job::none(deploy_overlay).with_description(
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
        // redeploy a withdrawn overlay after the reboot
        .exception(
            "/host/releases/{release_uuid}",
            exception::update(|rel: View<HostRelease>, Target(tgt): Target<HostRelease>| {
                overlay_awaits_reboot(&rel, &tgt)
            })
            .with_description(|Args(release_uuid): Args<String>| {
                format!("host OS release '{release_uuid}' is waiting for a reboot")
            }),
        )
        // a withdrawn overlay is already removed
        .exception(
            "/host/releases/{release_uuid}/overlays/{name}",
            exception::delete(|ov: View<Overlay>| ov.status == OverlayStatus::Withdrawn)
                .with_description(|Args((release_uuid, name)): Args<(String, String)>| {
                    format!(
                        "overlay '{name}' of host OS release '{release_uuid}' is waiting for a reboot"
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

    use crate::common_types::ImageUri;
    use crate::models::{HostAppTarget, OverlayTarget};
    use mahler::state::Map;

    /// The runtimes an engine registers.
    fn runtimes(names: &[&str]) -> Vec<oci::Runtime> {
        names.iter().map(|r| r.to_string()).collect()
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
        overlay_runtime_unavailable(&runtimes(engine), &target(overlays))
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
            host_validating: false,
        }
    }

    #[test]
    fn a_release_the_engine_can_run_is_kept() {
        let mut tgt = host_target(&[("ebpf", "extension")]);
        let reasons = reject_unsupported_releases(&mut tgt, &runtimes(&["runc", "extension"]));
        assert!(reasons.is_empty());
        assert_eq!(tgt.releases.len(), 1);
    }

    #[test]
    fn a_release_naming_an_unregistered_runtime_is_dropped() {
        // X-04: a profile activated on a pre-extension OS.
        let mut tgt = host_target(&[("ebpf", "extension")]);
        let reasons = reject_unsupported_releases(&mut tgt, &runtimes(&[]));
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
