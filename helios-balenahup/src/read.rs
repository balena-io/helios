use std::time::{Duration, SystemTime};

use thiserror::Error;

use crate::common_types::Uuid;
use crate::oci;
use crate::store::{self, DocumentStore};
use crate::util::dirs::runtime_dir;

use super::BALENAHUP;
use super::models::{Host, HostRelease, HostReleaseStatus};

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Oci(#[from] oci::Error),

    #[error(transparent)]
    Store(#[from] store::Error),

    #[error(transparent)]
    IO(#[from] std::io::Error),
}

/// Read the hostapp data from the store
pub async fn from_store(host: &mut Host, local_store: &DocumentStore) -> Result<(), Error> {
    // Read the hostapp information from the local store
    let host_releases_view = local_store.as_view().at("host/releases")?;
    let host_releases: Vec<Uuid> = host_releases_view
        .keys()
        .await?
        .into_iter()
        .map(Uuid::from)
        .collect();
    for release_uuid in host_releases {
        match local_store
            .open(format!("host/releases/{release_uuid}/hostapp"))
            .await
        {
            Ok(hostapp_doc) => {
                let last_modified = hostapp_doc.modified().unwrap_or_else(SystemTime::now);
                let mut hostapp: HostRelease = hostapp_doc.into_value().await?;

                // ignore the status on the store and deduce it instead
                hostapp.status = if host.meta.build.as_ref() == Some(&hostapp.build) {
                    // if the hostapp build is the current OS build then the release is running
                    HostReleaseStatus::Running
                } else if tokio::fs::try_exists(
                    runtime_dir().join(format!("{BALENAHUP}-{release_uuid}-breadcrumb")),
                )
                .await?
                {
                    // if there is a balenahup breadcrumb, then we are still waiting for a
                    // reboot
                    HostReleaseStatus::Installed
                } else {
                    // otherwise the release has only been created
                    HostReleaseStatus::Created
                };

                if SystemTime::now() - Duration::from_secs(3600 * 24) > last_modified {
                    // reset the install attempts after 24 hours
                    hostapp.install_attempts = 0;
                }

                host.releases.insert(release_uuid, hostapp);
            }
            Err(store::Error::NotFound { .. }) => {}
            Err(e) => return Err(e)?,
        }
    }

    Ok(())
}
