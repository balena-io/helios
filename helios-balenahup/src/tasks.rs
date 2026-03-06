use std::fs;
use std::io;

use mahler::extract::{Args, Res, System, Target, View};
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};
use mahler::{exception, job};
use tracing::debug;

use crate::common_types::Uuid;
use crate::oci::{self, Client as Docker};
use crate::store::{self as store, DocumentStore};
use crate::util::dirs::runtime_dir;
use crate::util::fs::run_async;
use crate::util::systemd;
use crate::util::tar;

use super::models::{Device, Host, HostRelease, HostReleaseStatus, HostReleaseTarget};
use super::{BALENAHUP, HostRuntimeDir};

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
    let HostReleaseTarget {
        app,
        image,
        build,
        updater,
        ..
    } = tgt;

    // Get the running status by comparing to the current os build
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
fn install_hostapp_release(
    mut release: View<HostRelease>,
    Args(release_uuid): Args<String>,
    docker: Res<Docker>,
    store: Res<DocumentStore>,
    host_runtime_dir: Res<HostRuntimeDir>,
) -> IO<HostRelease, HostUpdateError> {
    // this task is only applicable if the release is not already running
    enforce!(
        release.status == HostReleaseStatus::Created,
        "OS release already installed"
    );

    // increase the install counter
    release.install_attempts += 1;
    with_io(release, async move |release| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");

        let container_helper = docker.container();

        // remove any existing `balenahup` container
        container_helper.remove(BALENAHUP).await?;

        // write the release data into the store to update install_attempts
        local_store
            .put(format!("host/releases/{release_uuid}/hostapp"), &*release)
            .await?;

        // Pull the docker image for the updater
        debug!("pull hostapp updater script from '{}'", release.updater);
        docker.image().pull(&release.updater, None).await?;

        // create a `balenahup` container from the update image
        let id = container_helper
            .create_tmp(BALENAHUP, &release.updater)
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
                release.image.as_str(),
                // FIXME: this needs to be re-added after helios handles update-locks
                // "--no-reboot"
            ])
            .workdir(host_target_dir);
        systemd::run("os-update", &hup_cmd).await?;

        // leave a breadcrumb in the runtime-dir to indicate that the os release was installed.
        // The breadcrumb will be removed after a reboot, so the worker will be able to re-try
        // HUP after a rollback. Since the hup script may reboot immediately after finishing, this
        // step may be skipped, but that is fine since the breadcrumb is not longer needed at that
        // point
        run_async(move || {
            fs::File::create(runtime_dir().join(format!("{BALENAHUP}-{release_uuid}-breadcrumb")))
        })
        .await?;

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
    store: Res<DocumentStore>,
) -> IO<HostRelease, store::Error> {
    // do nothing if the release is not currently running
    enforce!(
        rel.status == HostReleaseStatus::Running,
        "OS release is not running yet"
    );

    // the only change that this applies is to the updater script
    rel.updater = tgt.updater;

    with_io(rel, async move |rel| {
        // write the release data into the store
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .put(format!("host/releases/{release_uuid}/hostapp"), &*rel)
            .await?;

        Ok(rel)
    })
}

/// handle an `delete(/host/releases/<commit>)`
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
                docker.image().remove(&rel.updater).await?;
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
                job::update(update_script_uri).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("update metadata for host OS release '{release_uuid}'")
                    },
                ),
                job::delete(remove_old_metadata).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("remove metadata for host OS release '{release_uuid}'",)
                    },
                ),
            ],
        )
        .job(
            "/host",
            job::none(cleanup_hostapp).with_description(|| "clean-up host metadata and images"),
        )
        // ignore requests to delete the host field if the target OS is set to null
        .exception("/host", exception::delete(|| true))
        // ignore requests to update the host if it has already been installed (we are waiting for
        // a reboot) or we reached the number of install attempts
        .exception(
            "/host/releases/{release_uuid}",
            exception::update(|rel: View<HostRelease>| {
                rel.status == HostReleaseStatus::Installed || rel.install_attempts > 3
            }),
        )
}
