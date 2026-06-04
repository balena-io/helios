use std::time::{Duration, SystemTime};

use thiserror::Error;

use crate::common_types::Uuid;
use crate::oci::{self, Client as Docker};
use crate::overlays::{self, CLASS_LABEL, CLASS_OVERLAY};
use crate::store::{self, DocumentStore};
use crate::util::dirs::runtime_dir;
use crate::util::proc;

use super::BALENAHUP;
use super::models::{Host, HostRelease, HostReleaseStatus};

const ROLLBACK_HEALTH_BREADCRUMB: &str = "rollback-health-breadcrumb";
const ROLLBACK_ALTBOOT_BREADCRUMB: &str = "rollback-altboot-breadcrumb";

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
pub async fn from_store(
    host: &mut Host,
    docker: &Docker,
    local_store: &DocumentStore,
    host_state_dir: &std::path::Path,
) -> Result<(), Error> {
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

                // Overlays are never bookkept; derive them fresh below.
                hostapp.overlays = mahler::state::Map::new();

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

    // Derive overlay extensions from engine reality and attach them to their
    // release. Overlay containers carry a private release label written at
    // deploy time (see overlays.rs).
    let boot_time = proc::boot_time()?;
    let overlay_ids = docker
        .container()
        .list_with_labels(vec![&format!("{CLASS_LABEL}={CLASS_OVERLAY}")])
        .await?;
    for id in overlay_ids {
        let container = match docker.container().inspect(&id).await {
            Ok(c) => c,
            // The OS reaper can remove an overlay between list and inspect.
            Err(e) if e.is_not_found() => continue,
            Err(e) => return Err(e)?,
        };
        let Some((release_uuid, name, overlay)) =
            overlays::overlay_from_container(&container, boot_time)
        else {
            continue; // not a helios-deployed overlay
        };
        if let Some(rel) = host.releases.get_mut(&Uuid::from(release_uuid)) {
            rel.overlays.insert(name, overlay);
        }
    }

    let hup_in_progress = tokio::fs::try_exists(host_state_dir.join(ROLLBACK_HEALTH_BREADCRUMB))
        .await
        .unwrap_or(false)
        || tokio::fs::try_exists(host_state_dir.join(ROLLBACK_ALTBOOT_BREADCRUMB))
            .await
            .unwrap_or(false);
    for rel in host.releases.values_mut() {
        rel.hup_in_progress = hup_in_progress;
    }

    Ok(())
}
