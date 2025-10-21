use mahler::extract::{Args, Res, System, Target, View};
use mahler::task::prelude::*;
use mahler::{
    task,
    worker::{Uninitialized, Worker},
};

use crate::models::{Device, Host, HostRelease, HostReleaseTarget, HostTarget};
use crate::tasks::image::pull_image;
use crate::util::store::{Store, StoreError};

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

    with_io(maybe_host, async move |opt_host| {
        if let (Some(local_store), Some(host)) = (store.as_ref(), opt_host.as_ref()) {
            local_store
                .write("/host", "app_uuid", &host.app_uuid)
                .await?;
        }
        Ok(opt_host)
    })
}

/// Fetch the updater artifact from the registry and install the hostapp release
///
/// Applies to `create(/host/releases/<commit>)`
fn fetch_updater_and_install_hostapp(
    System(device): System<Device>,
    Target(tgt): Target<HostReleaseTarget>,
) -> Vec<Task> {
    let mut tasks = Vec::new();

    // we can only update if we have current OS information, if we don't there is a
    // problem somewhere
    if let Some(os) = device.os.as_ref() {
        // fetch the updater if not available yet
        if !device.images.contains_key(&tgt.updater)
        // and if the OS has a different build than the target
        && os.build.as_ref().is_some_and(|build| build != &tgt.build)
        {
            tasks.push(pull_image.with_arg("image_name", tgt.updater));
        }
        tasks.push(install_hostapp_release.into_task());
    }

    tasks
}

/// Install the hostapp release
///
/// Applies to `none(/host/releases/<commit>)` as this is only called in the context of
/// fetch_updater
fn install_hostapp_release(
    mut maybe_rel: View<Option<HostRelease>>,
    Target(tgt): Target<HostReleaseTarget>,
    System(device): System<Device>,
    store: Res<Store>,
) -> IO<Option<HostRelease>> {
    // set the host release with the details from the target
    maybe_rel.replace(tgt.into());

    with_io(maybe_rel, async move |rel| {
        if let (Some(os), Some(rel)) = (device.os, rel.as_ref()) {
            // only install the target OS if the builds match
            if os.build.is_some_and(|build| build != rel.build) {
                // TODO:
                // - remove any existing `balenahup` container
                // - create a `balenahup` container from the update image
                // - use [`Docker::download_from_container`] copy the script into /tmp/run/balenahup
                // - call systemd runn using `/tmp/balena-supervisor/balenahup` as the workdir, wait for
                //   the script to finish (use --no-reboot)
                // - remove the `balenahup` container
            }

            // TODO: write the release data into the store

            // TODO: reboot if HUP succeeded
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
            task::create(init_hostapp).with_description(|| "initialize host app"),
        )
        .jobs(
            "/host/{release_uuid}",
            [
                task::create(fetch_updater_and_install_hostapp),
                task::none(install_hostapp_release).with_description(
                    |Args(release_uuid): Args<String>| {
                        format!("install hostOS release '{release_uuid}'")
                    },
                ),
            ],
        )
}
