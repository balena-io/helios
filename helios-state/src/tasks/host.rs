use std::time::Duration;

use mahler::extract::{Args, Res, System, Target, View};
use mahler::task::prelude::*;
use mahler::{
    task,
    worker::{Uninitialized, Worker},
};
use tokio::fs;
use tracing::{debug, warn};

use crate::config::HostRuntimeDir;
use crate::models::{Device, HostRelease, HostReleaseStatus, HostReleaseTarget};
use crate::oci::{Client as Docker, Error as DockerError};
use crate::util::dirs::runtime_dir;
use crate::util::store::{Store, StoreError};
use crate::util::systemd;
use crate::util::tar;

#[derive(Debug, thiserror::Error)]
enum HostUpdateError {
    #[error(transparent)]
    Docker(#[from] DockerError),

    #[error(transparent)]
    LocalStore(#[from] StoreError),

    #[error(transparent)]
    IO(#[from] std::io::Error),

    #[error(transparent)]
    Systemd(#[from] systemd::Error),

    #[error("operation needs reboot before it can complete")]
    NeedsReboot,

    #[error("too many update attempts")]
    TooManyAttempts,
}

/// Initialize the release
///
/// Applies to `create(/host/releases/<rel_uuid>)`
fn init_hostapp_release(
    mut maybe_rel: View<Option<HostRelease>>,
    Args(release_uuid): Args<String>,
    Target(tgt): Target<HostReleaseTarget>,
    System(device): System<Device>,
    store: Res<Store>,
) -> IO<Option<HostRelease>, StoreError> {
    let HostReleaseTarget {
        app,
        image,
        build,
        updater,
        ..
    } = tgt;

    // Get the running status by comparing
    // to the current os build
    let is_running = device
        .host
        .and_then(|host| host.meta.build)
        .is_some_and(|os_build| os_build == build);

    let status = if is_running {
        HostReleaseStatus::Running
    } else {
        HostReleaseStatus::Created
    };

    // Create a release using the target metadata
    let rel = HostRelease {
        app,
        image,
        build,
        updater,
        status,
        install_attempts: 0,
    };

    // set the host release with the details from the target
    maybe_rel.replace(rel);

    with_io(maybe_rel, async move |maybe_rel| {
        // write the release data into the store
        if let (Some(rel), Some(local_store)) = (maybe_rel.as_ref(), store.as_ref()) {
            local_store
                .write(format!("/host/releases/{release_uuid}"), "hostapp", rel)
                .await?;
        }
        Ok(maybe_rel)
    })
}

/// Install the hostapp release
///
/// Applies to `create(/host/releases/<commit>)`
fn install_hostapp_release(
    mut release: View<HostRelease>,
    Args(release_uuid): Args<String>,
    docker: Res<Docker>,
    store: Res<Store>,
    host_runtime_dir: Res<HostRuntimeDir>,
) -> IO<HostRelease, HostUpdateError> {
    // this task is only applicable if the release is not already running
    if release.status != HostReleaseStatus::Created {
        return release.into();
    }

    // increase the install counter and set the status after
    // the successful run of the task
    release.install_attempts += 1;
    release.status = HostReleaseStatus::Installed;

    with_io(release, async move |release| {
        // abort the update after too many tries
        if release.install_attempts > 3 {
            warn!("skipping update after too many attempts");
            // wait 60 seconds to let other tasks to finish and error
            tokio::time::sleep(Duration::from_secs(60)).await;
            return Err(HostUpdateError::TooManyAttempts);
        }

        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        // Pull the docker image for the updater
        debug!("pull hostapp updater script from '{}'", release.updater);
        docker.image().pull(&release.updater, None).await?;

        // only install the target OS if the builds don't match
        let container_helper = docker.container();

        // remove any existing `balenahup` container
        // XXX: use a more generic container name?
        container_helper.remove("balenahup").await?;

        // create a `balenahup` container from the update image
        let id = container_helper
            .create_tmp("balenahup", &release.updater)
            .await?;

        // configure the target dir in $RUNTIME_DIR/balenahup
        let target_dir = runtime_dir().join("balenahup");
        let host_target_dir = host_runtime_dir
            .as_ref()
            .expect("should not be nil")
            .join("balenahup");

        // remove the target dir if it exists
        if let Err(e) = fs::remove_dir_all(&target_dir).await {
            // ignore the error if the directory does not exist
            if !matches!(e.kind(), std::io::ErrorKind::NotFound) {
                return Err(e.into());
            }
        }
        fs::create_dir_all(&target_dir).await?;

        // read scripts from the container
        let bytes = container_helper.read_from(&id, "/app").await?;

        // extract the scripts into the target directory
        tar::unpack_from(&bytes, "/app", target_dir)?;

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
                release.image.as_str(),
                "--no-reboot",
            ])
            // FIXME: this is the host equivalent of $RUNTIME_DIR/balenahup, we
            // need to declare this mapping somewhere
            .workdir(host_target_dir);
        systemd::run("os-update", &hup_cmd).await?;

        // remove the balenahup container
        container_helper.remove("balenahup").await?;

        // write the release data into the store
        if let Some(local_store) = store.as_ref() {
            local_store
                .write(
                    format!("/host/releases/{release_uuid}"),
                    "hostapp",
                    &*release,
                )
                .await?;
        }

        // leave a breadcrumb in the runtime-dir to indicate that the os release was installed.
        // The breadcrumb will be removed after a reboot, so the worker will be able to re-try
        // HUP after a rollback
        fs::File::create(runtime_dir().join(format!("balenahup-{release_uuid}-breadcrumb")))
            .await?;

        Ok(release)
    })
}

/// Mark the release as running in memory
///
/// Applies to `update(/host/releases/<commit>)`
fn complete_hostapp_install(
    rel: View<HostRelease>,
    System(device): System<Device>,
) -> IO<HostRelease, HostUpdateError> {
    // This task is only  applicable if the release is already installed
    // which will only happen if install_hostapp_release finishes without an
    // error
    if rel.status != HostReleaseStatus::Installed {
        return rel.into();
    }

    with_io(rel, async move |rel| {
        // Get the running status by comparing
        // to the current os build
        let is_running = device
            .host
            .and_then(|host| host.meta.build)
            .is_some_and(|os_build| os_build == rel.build);

        if !is_running {
            // set the reboot breadcrumb to tell the legacy supervisor to reboot
            fs::File::create(runtime_dir().join("reboot-after-apply")).await?;

            // return an error to terminate the worker operation
            return Err(HostUpdateError::NeedsReboot);
        }

        Ok(rel)
    })
    .map(|mut rel| {
        rel.status = HostReleaseStatus::Running;
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
    Target(tgt): Target<HostReleaseTarget>,
    Args(release_uuid): Args<String>,
    store: Res<Store>,
) -> IO<HostRelease, StoreError> {
    // do nothing if the release is not currently running
    if rel.status != HostReleaseStatus::Running {
        return rel.into();
    }

    // the only change that this applies is to the updater script
    rel.updater = tgt.updater;

    with_io(rel, async move |rel| {
        // write the release data into the store
        if let Some(local_store) = store.as_ref() {
            local_store
                .write(format!("/host/releases/{release_uuid}"), "hostapp", &*rel)
                .await?;
        }

        Ok(rel)
    })
}

/// handle an `delete(/host/releases/<commit>)`
fn remove_old_metadata(
    mut rel: View<Option<HostRelease>>,
    Args(release_uuid): Args<String>,
    store: Res<Store>,
) -> IO<Option<HostRelease>, StoreError> {
    // remove the old release
    rel.take();

    with_io(rel, async move |rel| {
        // remove the old release metadata
        if let Some(local_store) = store.as_ref() {
            local_store
                .delete_all(format!("/host/releases/{release_uuid}"))
                .await?;
        }

        Ok(rel)
    })
}

pub fn with_hostapp_tasks<O, I>(
    worker: Worker<O, Uninitialized, I>,
) -> Worker<O, Uninitialized, I> {
    worker.jobs(
        "/host/releases/{release_uuid}",
        [
            task::create(init_hostapp_release).with_description(
                |Args(release_uuid): Args<String>| {
                    format!("initialize hostOS release '{release_uuid}'")
                },
            ),
            task::update(install_hostapp_release).with_description(
                |Args(release_uuid): Args<String>| {
                    format!("install hostOS release '{release_uuid}'")
                },
            ),
            task::update(complete_hostapp_install).with_description(
                |Args(release_uuid): Args<String>| {
                    format!("complete hostOS release install for '{release_uuid}'")
                },
            ),
            task::update(update_script_uri).with_description(|Args(release_uuid): Args<String>| {
                format!("update metadata for release '{release_uuid}'")
            }),
            task::delete(remove_old_metadata).with_description(
                |Args(release_uuid): Args<String>| {
                    format!("clean up metadata for previous hostOS release '{release_uuid}'",)
                },
            ),
        ],
    )
}
