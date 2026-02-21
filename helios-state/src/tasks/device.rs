use mahler::extract::{Res, Target, View};
use mahler::job;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};
use tracing::debug;

use crate::common_types::{ImageUri, Uuid};
use crate::models::{Device, ImageRef};
use crate::oci::{Client as Docker, Error as OciError};
use crate::util::store::{Store, StoreError};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Oci(#[from] OciError),
}

/// Clean up the device state after the target has been reached
///
/// This should be the final task for every workflow
fn perform_cleanup(
    device: View<Device>,
    docker: Res<Docker>,
    store: Res<Store>,
) -> IO<Device, Error> {
    with_io(device, |mut device| async move {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");

        // clean up old app/release metadata and images
        let app_uuids: Vec<Uuid> = local_store.list("/apps").await?;
        for app_uuid in app_uuids {
            let release_uuids: Vec<Uuid> = local_store
                .list(format!("/apps/{app_uuid}/releases"))
                .await?;

            for release_uuid in release_uuids {
                let service_names: Vec<String> = local_store
                    .list(format!("/apps/{app_uuid}/releases/{release_uuid}/services"))
                    .await?;
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
                        if let Some(img_uri) = local_store
                            .read::<_, ImageUri>(
                                format!("/apps/{app_uuid}/releases/{release_uuid}/services/{service_name}"),
                                "image",
                            )
                        .await?
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
                        })
                        {
                            // then remove the image by tag
                            debug!("removing unused image {img_uri}");
                            docker.image().remove(&img_uri).await?;
                            device.images.remove(&img_uri);
                        }

                        // remove the service metadata
                        local_store
                            .delete_all(format!(
                                "/apps/{app_uuid}/releases/{release_uuid}/services/{service_name}"
                            ))
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
                    local_store
                        .delete_all(format!("/apps/{app_uuid}/releases/{release_uuid}"))
                        .await?;
                }
            }

            // remove app metadata not in the end/target state
            if !device.apps.contains_key(&app_uuid) {
                local_store.delete_all(format!("/apps/{app_uuid}")).await?;
            }
        }

        Ok(device)
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
        .with_cleanup(perform_cleanup)
}
