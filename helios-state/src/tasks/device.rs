use mahler::extract::{Res, Target, View};
use mahler::job;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};
use tracing::debug;

use crate::common_types::{ImageUri, Uuid};
use crate::models::{Device, ImageRef};
use crate::oci::{Client as Docker, Error as OciError};
use crate::store::{self, DocumentStore};

#[cfg(feature = "balenahup")]
use crate::balenahup;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Store(#[from] store::Error),
    #[error(transparent)]
    Oci(#[from] OciError),
}

#[cfg(feature = "balenahup")]
impl From<balenahup::HostCleanupError> for Error {
    fn from(value: balenahup::HostCleanupError) -> Self {
        use balenahup::HostCleanupError::*;
        match value {
            Oci(e) => Error::Oci(e),
            Store(e) => Error::Store(e),
        }
    }
}

fn perform_cleanup() -> Vec<Task> {
    vec![
        #[cfg(feature = "balenahup")]
        balenahup::cleanup_hostapp.into_task(),
        cleanup_userapps.into_task(),
    ]
}

/// Clean up the device state after the target has been reached
///
/// This should be the final task for every workflow
fn cleanup_userapps(
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
    worker
        .job(
            "/name",
            job::any(set_device_name).with_description(|| "update device name"),
        )
        .job(
            "",
            job::none(cleanup_userapps).with_description(|| "clean-up app metadata and images"),
        )
        .with_cleanup(perform_cleanup)
}
