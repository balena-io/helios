#[cfg(feature = "userapps")]
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use mahler::extract::{Res, System, SystemTarget, Target, View};
use mahler::job;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};

use crate::common_types::HostRuntimeDir;
use crate::models::{Device, DeviceTarget};
use crate::store::{self, DocumentStore};
#[cfg(not(feature = "userapps"))]
use crate::util::dirs::runtime_dir;
use crate::util::fs::run_async;
use crate::util::locking::{ForceAcquireLocks, LockSet};
use crate::util::reboot::mark_reboot_required;
use crate::util::systemd;

/// Clean up the device state after the target has been reached
///
/// This should be the final task for every workflow
fn perform_cleanup() -> Vec<Task> {
    vec![
        #[cfg(feature = "balenahup")]
        crate::balenahup::cleanup_hostapp.into_task(),
        #[cfg(feature = "userapps")]
        cleanup_userapps.into_task(),
    ]
}

#[cfg(feature = "userapps")]
use cleanup::*;

#[cfg(feature = "userapps")]
mod cleanup {
    use super::*;
    use tracing::debug;

    use crate::common_types::{ImageUri, Uuid};
    use crate::models::{Device, ImageRef};
    use crate::oci::{self, Client as Docker};

    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        #[error(transparent)]
        Store(#[from] store::Error),
        #[error(transparent)]
        Oci(#[from] oci::Error),
    }

    pub fn cleanup_userapps(
        device: View<Device>,
        docker: Res<Docker>,
        store: Res<DocumentStore>,
    ) -> IO<Device, Error> {
        with_io(device, |mut device| async move {
            let docker = docker
                .as_ref()
                .expect("docker resource should be available");
            let local_store = store.as_ref().expect("store should be available");

            // clean up old app/release metadata and images
            let apps_view = local_store.as_view().at("apps")?;
            let app_uuids: Vec<Uuid> = apps_view
                .keys()
                .await?
                .into_iter()
                .map(Uuid::from)
                .collect();
            for app_uuid in app_uuids {
                let releases_view = apps_view.at(format!("{app_uuid}/releases"))?;
                let release_uuids: Vec<Uuid> = releases_view
                    .keys()
                    .await?
                    .into_iter()
                    .map(Uuid::from)
                    .collect();

                for release_uuid in release_uuids {
                    let services_view = releases_view.at(format!("{release_uuid}/services"))?;
                    let service_names: Vec<String> = services_view.keys().await?;
                    for service_name in service_names {
                        // the service does not exist in the end/target state
                        if !device
                            .apps
                            .get(&app_uuid)
                            .and_then(|app| app.releases.get(&release_uuid))
                            .map(|rel| rel.services.contains_key(&service_name))
                            .unwrap_or_default()
                        {
                            // if there is an image reference for the service
                            if let Some(img_uri) = services_view.get::<ImageUri>(format!("{service_name}/image")).await?
                        // and the image is not being used by any other service
                        && !device.apps.iter().any(|(a_uuid, app)| {
                            app.releases.iter().any(|(r_uuid, rel)| {
                                rel.services.iter().any(|(s_name, s)| {
                                    if a_uuid == &app_uuid && r_uuid == &release_uuid && s_name == &service_name {
                                        false
                                    }
                                    else if let ImageRef::Uri(s_img) = &s.image {
                                        s_img == &img_uri
                                    } else {
                                        false
                                    }
                                })
                            })
                        }) {
                                // then remove the image by tag
                                debug!("removing unused image {img_uri}");
                                docker.image().remove(&img_uri).await?;
                                device.images.remove(&img_uri);
                            }

                            // remove the service metadata
                            services_view
                                .delete_all(&format!("{service_name}/*"))
                                .await?;
                        }
                    }

                    // if the release does not exist on the end/target state
                    if !device
                        .apps
                        .get(&app_uuid)
                        .map(|app| app.releases.contains_key(&release_uuid))
                        .unwrap_or_default()
                    {
                        // remove the release metadata
                        releases_view
                            .delete_all(&format!("{release_uuid}/*"))
                            .await?;
                    }
                }

                // remove app metadata not in the end/target state
                if !device.apps.contains_key(&app_uuid) {
                    apps_view.delete_all(&format!("{app_uuid}/*")).await?;
                }
            }

            // clean up store
            local_store.gc().await?;

            Ok(device)
        })
    }
}

/// Whether the host is validating this boot; a reboot would roll it back.
#[cfg(feature = "balenahup")]
fn host_is_validating(device: &Device) -> bool {
    device
        .host
        .as_ref()
        .is_some_and(|host| host.host_validating)
}

#[cfg(not(feature = "balenahup"))]
fn host_is_validating(_: &Device) -> bool {
    false
}

/// Whether every target release is installed, or frozen by a planner exception.
///
/// Frozen releases count, or a pending reboot would make the whole target
/// unreachable.
fn apps_settled(device: &Device, target: &DeviceTarget) -> bool {
    userapps_settled(device, target) && host_settled(device, target)
}

#[cfg(feature = "userapps")]
fn userapps_settled(device: &Device, target: &DeviceTarget) -> bool {
    target.apps.iter().all(|(app_uuid, t_app)| {
        t_app
            .releases
            .keys()
            .all(|rel_uuid| super::app::target_release_settled(device, target, app_uuid, rel_uuid))
    })
}

#[cfg(not(feature = "userapps"))]
fn userapps_settled(_: &Device, _: &DeviceTarget) -> bool {
    true
}

/// A host release counts if a reboot is all it waits for.
#[cfg(feature = "balenahup")]
fn host_settled(device: &Device, target: &DeviceTarget) -> bool {
    target.host.as_ref().is_none_or(|t_host| {
        t_host.releases.iter().all(|(rel_uuid, t_rel)| {
            device
                .host
                .as_ref()
                .and_then(|host| host.releases.get(rel_uuid))
                .is_some_and(|rel| crate::balenahup::release_settled_on_reboot(rel, t_rel))
        })
    })
}

#[cfg(not(feature = "balenahup"))]
fn host_settled(_: &Device, _: &DeviceTarget) -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
enum RebootError {
    #[cfg(not(feature = "userapps"))]
    #[error("a user update-lock forbids rebooting: {}", .0.display())]
    Locked(PathBuf),
    #[error(transparent)]
    IO(#[from] io::Error),
    #[error(transparent)]
    Systemd(#[from] systemd::Error),
}

/// The update lock of every app service.
#[cfg(feature = "userapps")]
type LockScope = HashMap<PathBuf, Vec<String>>;

#[cfg(feature = "userapps")]
fn lock_scope(device: &Device, host_runtime_dir: &HostRuntimeDir) -> LockScope {
    super::app::service_lock_paths(device.apps.values(), host_runtime_dir)
}

/// Without app support, every update lock under the runtime dir.
#[cfg(not(feature = "userapps"))]
type LockScope = PathBuf;

/// Scan the container-visible runtime dir, not the host-side path
#[cfg(not(feature = "userapps"))]
fn lock_scope(_: &Device, _: &HostRuntimeDir) -> LockScope {
    runtime_dir()
}

/// Take every app's update lock, then reboot. The locks are released if the
/// reboot cannot be issued.
#[cfg(feature = "userapps")]
async fn guarded_reboot(
    service_locks: LockScope,
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
) -> Result<(), RebootError> {
    let force = force_acquire_locks
        .as_ref()
        .expect("force_acquire_locks should be available")
        .enabled();

    // return the lock set for the rollback, as a `Res` cannot be cloned
    let (locks, acquired) = run_async(move || {
        let acquired = super::app::acquire_locks(
            locks.as_ref().expect("locks resource should be available"),
            service_locks,
            force,
        );
        Ok((locks, acquired))
    })
    .await?;
    let acquired = acquired?;

    if let Err(e) = systemd::reboot().await {
        run_async(move || {
            let locks = locks.as_ref().expect("locks resource should be available");
            let _ = locks.unlock_many(acquired);
            Ok(())
        })
        .await?;
        return Err(e.into());
    }
    Ok(())
}

/// Reboot unless a user update lock forbids it. A forced update skips the scan.
#[cfg(not(feature = "userapps"))]
async fn guarded_reboot(
    runtime_dir: LockScope,
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
) -> Result<(), RebootError> {
    let force = force_acquire_locks
        .as_ref()
        .expect("force_acquire_locks should be available")
        .enabled();

    if !force {
        let held = run_async(move || {
            let locks = locks.as_ref().expect("locks resource should be available");
            locks.find_blocking(&runtime_dir)
        })
        .await?;
        if let Some(path) = held {
            return Err(RebootError::Locked(path));
        }
    }

    systemd::reboot().await?;
    Ok(())
}

/// Reboot once every app is installed. Reads the whole device, so it never runs
/// concurrently with another change.
fn reboot(
    mut needs_reboot: View<bool>,
    System(device): System<Device>,
    SystemTarget(target): SystemTarget<Device>,
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
    host_runtime_dir: Res<HostRuntimeDir>,
) -> IO<bool, RebootError> {
    enforce!(*needs_reboot, "no reboot required");
    enforce!(!host_is_validating(&device), "host validation in progress");
    enforce!(
        apps_settled(&device, &target),
        "apps are still being installed"
    );
    *needs_reboot = false;

    with_io(needs_reboot, async move |needs_reboot| {
        let host_runtime_dir = host_runtime_dir
            .as_ref()
            .expect("host runtime dir should be available");
        let lock_scope = lock_scope(&device, host_runtime_dir);
        guarded_reboot(lock_scope, locks, force_acquire_locks).await?;
        Ok(needs_reboot)
    })
}

/// Update the device name
fn set_device_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
    store: Res<DocumentStore>,
) -> IO<Option<String>, store::Error> {
    *name = tgt;
    with_io(name, |name| async move {
        if let (Some(local_store), Some(name)) = (store.as_ref(), name.as_ref()) {
            local_store.put("device_name", name).await?;
        }

        Ok(name)
    })
}

pub fn with_device_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    let mut worker = worker.job(
        "/name",
        job::any(set_device_name).with_description(|| "update device name"),
    );

    #[cfg(feature = "userapps")]
    {
        worker = worker.job(
            "",
            job::none(cleanup_userapps).with_description(|| "clean-up app metadata and images"),
        )
    }

    worker = worker.jobs(
        "/needs_reboot",
        [
            // Only reached by expansion from changes that need a reboot
            job::none(mark_reboot_required).with_description(|| "mark reboot as required"),
            job::update(reboot).with_description(|| "reboot the device"),
        ],
    );

    worker = worker.hook(perform_cleanup);
    worker
}
