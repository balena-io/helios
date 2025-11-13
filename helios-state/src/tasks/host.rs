use std::io::Cursor;
use std::time::Duration;

use mahler::extract::{Args, Res, System, Target, View};
use mahler::task::prelude::*;
use mahler::{
    task,
    worker::{Uninitialized, Worker},
};
use tar::Archive;
use tokio::fs;
use tracing::{debug, warn};

use crate::models::{Device, Host, HostRelease, HostReleaseTarget, HostTarget};
use crate::oci::{Client as Docker, Error as DockerError};
use crate::util::dirs::runtime_dir;
use crate::util::store::{Store, StoreError};
use crate::util::systemd;

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

    #[error("too many update attempts")]
    TooManyAttempts,
}

/// Initialize the hostapp and store the app uuid
///
/// Applies to `create(/host)` operation
fn init_hostapp(
    mut maybe_host: View<Option<Host>>,
    Target(tgt): Target<HostTarget>,
    store: Res<Store>,
) -> IO<Option<Host>, StoreError> {
    let HostTarget { app_uuid, .. } = tgt;
    maybe_host.replace(Host {
        app_uuid,
        releases: Default::default(),
    });

    with_io(maybe_host, async move |maybe_host| {
        if let (Some(local_store), Some(host)) = (store.as_ref(), maybe_host.as_ref()) {
            local_store
                .write("/host", "app_uuid", &host.app_uuid)
                .await?;
        }
        Ok(maybe_host)
    })
}

/// Install the hostapp release
///
/// Applies to `create(/host/releases/<commit>)`
fn install_hostapp_release(
    mut maybe_rel: View<Option<HostRelease>>,
    Target(tgt): Target<HostReleaseTarget>,
    System(device): System<Device>,
    Args(release_uuid): Args<String>,
    docker: Res<Docker>,
    store: Res<Store>,
) -> IO<Option<HostRelease>, HostUpdateError> {
    // set the host release with the details from the target
    maybe_rel.replace(tgt.into());

    with_io(maybe_rel, async move |mut maybe_rel| {
        if let (Some(os), Some(host), Some(rel)) = (device.os, device.host, maybe_rel.as_mut()) {
            // abort the update after too many tries
            if rel.install_attempts >= 3 {
                warn!("skipping update after too many attempts");
                // wait 60 seconds to let other tasks to finish and error
                tokio::time::sleep(Duration::from_secs(60)).await;
                return Err(HostUpdateError::TooManyAttempts);
            }

            let docker = docker
                .as_ref()
                .expect("docker resource should be available")
                .clone();

            // Pull the docker image for the updater
            debug!("pull hostapp updater script from '{}'", rel.updater);
            docker.image().pull(&rel.updater, None).await?;

            let mut requires_reboot = false;

            // only install the target OS if the builds match
            if os.build.is_some_and(|build| build != rel.build) {
                let container_helper = docker.container();

                // remove any existing `balenahup` container
                // XXX: use a more generic container name?
                container_helper.remove("balenahup").await?;

                // create a `balenahup` container from the update image
                let id = container_helper
                    .create_tmp("balenahup", &rel.updater)
                    .await?;

                // configure the target dir in $RUNTIME_DIR/balenahup
                let target_dir = runtime_dir().join("balenahup");

                // remove the target dir if it exists
                fs::remove_dir_all(&target_dir).await?;

                // read scripts from the container
                let cursor = Cursor::new(container_helper.read_from(&id, "/app").await?);
                let mut archive = Archive::new(cursor);

                // extract the scripts into the target directory
                archive.unpack(target_dir)?;

                // call systemd run using `/tmp/balena-supervisor/balenahup` as the workdir, wait for
                // the script to finish
                debug!("running the updater script");
                let hup_cmd = systemd::Command::new("update-2.x.sh")
                    .args(&[
                        "--app-uuid",
                        host.app_uuid.as_str(),
                        "--release-commit",
                        release_uuid.as_str(),
                        "--target-image-uri",
                        rel.image.as_str(),
                        "--no-reboot",
                    ])
                    // FIXME: this is the host equivalent of $RUNTIME_DIR/balenahup, we
                    // need to declare this mapping somewhere
                    .workdir("/tmp/balena-supervisor/balenahup");
                systemd::run("hostos-update", &hup_cmd).await?;

                // remove the balenahup container
                container_helper.remove("balenahup").await?;

                // everything worked, we should reboot
                requires_reboot = true;

                // update the install attempt count for use in a future workflow
                rel.install_attempts += 1;
            }

            // write the release data into the store
            if let Some(local_store) = store.as_ref() {
                local_store
                    .write(format!("/host/{release_uuid}"), "hostapp", &rel)
                    .await?;
            }

            // reboot if HUP succeeded
            if requires_reboot {
                // set the reboot breadcrumb to tell the supervisor to reboot
                fs::File::create(runtime_dir().join("reboot-after-apply")).await?;
            }
        }
        Ok(maybe_rel)
    })
}

/// handle an `update(/host/releases/<commit>)`
///
/// this only updates the local storage metadata about the hostapp if the
/// target build is the same as the current build
fn update_hostapp_metadata(
    mut rel: View<HostRelease>,
    Target(tgt): Target<HostReleaseTarget>,
    Args(release_uuid): Args<String>,
    store: Res<Store>,
) -> IO<HostRelease, StoreError> {
    // do nothing if the build is different, this should never happen
    // as a new hostapp should come in a different release
    if tgt.build != rel.build {
        return rel.into();
    }

    // update the release info
    *rel = tgt.into();

    with_io(rel, async move |rel| {
        // write the release data into the store
        if let Some(local_store) = store.as_ref() {
            local_store
                .write(format!("/host/{release_uuid}"), "hostapp", &*rel)
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
                .delete_all(format!("/host/{release_uuid}"))
                .await?;
        }

        Ok(rel)
    })
}

pub fn with_hostapp_tasks<O, I>(
    worker: Worker<O, Uninitialized, I>,
) -> Worker<O, Uninitialized, I> {
    worker
        .job(
            "/host",
            task::create(init_hostapp).with_description(|| "set-up host metadata"),
        )
        .jobs(
            "/host/releases/{release_uuid}",
            [
                task::create(install_hostapp_release).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("install hostOS release '{release_uuid}'")
                    },
                ),
                task::update(update_hostapp_metadata).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("update metadata for hostOS release '{release_uuid}'")
                    },
                ),
                task::delete(remove_old_metadata).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("clean up metadata for hostOS release '{release_uuid}'",)
                    },
                ),
            ],
        )
}
