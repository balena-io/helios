use tokio::sync::RwLock;

use mahler::extract::{Res, Target, View};
use mahler::job;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};

use crate::common_types::Uuid;
use crate::models::{Device, DeviceTarget};
use crate::oci::RegistryAuthClient;
use crate::util::store::{Store, StoreError};

/// Make sure a cleanup happens after all tasks have been performed
///
/// This should be the first task for every workflow
fn ensure_cleanup(mut device: View<Device>) -> View<Device> {
    device.needs_cleanup = true;
    device
}

/// Clean up the device state after the target has been reached
///
/// This should be the final task for every workflow
fn perform_cleanup(
    device: View<Device>,
    Target(tgt_device): Target<Device>,
    auth_client_res: Res<RwLock<RegistryAuthClient>>,
    store: Res<Store>,
) -> IO<Device, StoreError> {
    // skip the task if we have not reached the target state
    // (outside the needs_cleanup property)
    if DeviceTarget::from(Device {
        needs_cleanup: false,
        ..device.clone()
    }) != tgt_device
        || !device.needs_cleanup
    {
        return IO::abort("target state not reached yet");
    }

    with_io(device, |device| async move {
        // Clean up authorizations
        if let Some(auth_client_rwlock) = auth_client_res.as_ref() {
            // Wait for write authorization
            let mut auth_client = auth_client_rwlock.write().await;
            auth_client.clear();
        }

        // clean up old app/release metadata
        if let Some(local_store) = store.as_ref() {
            let app_uuids: Vec<Uuid> = local_store.list("/apps").await?;
            for app_uuid in app_uuids {
                // remove app metadata not in the current/target state
                if !device.apps.contains_key(&app_uuid) {
                    local_store.delete_all(format!("/apps/{app_uuid}")).await?;
                } else {
                    let release_uuids: Vec<Uuid> = local_store
                        .list(format!("/apps/{app_uuid}/releases"))
                        .await?;

                    // remove release metadata if not in the target state
                    for release_uuid in release_uuids {
                        if !device
                            .apps
                            .get(&app_uuid)
                            .map(|app| app.releases.contains_key(&release_uuid))
                            .unwrap_or_default()
                        {
                            local_store
                                .delete_all(format!("/apps/{app_uuid}/releases/{release_uuid}"))
                                .await?;
                        }
                    }
                }
            }
        }

        Ok(device)
    })
    .map(|mut device| {
        device.auths.clear();
        device.needs_cleanup = false;
        device
    })
}

/// Update the device name
fn set_device_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
    store: Res<Store>,
) -> IO<Option<String>, StoreError> {
    *name = tgt;
    with_io(name, |name| async move {
        if let (Some(local_store), Some(name)) = (store.as_ref(), name.as_ref()) {
            local_store.write("/", "device_name", name).await?;
        }

        Ok(name)
    })
}

pub fn with_device_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    worker
        .job(
            "/name",
            job::any(set_device_name).with_description(|| "update device name"),
        )
        // XXX: this is not added first because of
        // https://github.com/balena-io-modules/mahler-rs/pull/50
        .jobs(
            "",
            [
                job::update(ensure_cleanup).with_description(|| "ensure clean-up"),
                job::update(perform_cleanup).with_description(|| "perform clean-up"),
            ],
        )
}
