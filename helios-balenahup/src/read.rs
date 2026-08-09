use std::time::{Duration, SystemTime};

use thiserror::Error;

use crate::common_types::Uuid;
use crate::oci::{self, Client as Docker};

use crate::store::{self, DocumentStore};
use crate::util::breadcrumb;
use crate::util::proc;
use crate::util::systemd;

use super::BALENAHUP;
use super::models::{
    CLASS_LABEL, CLASS_OVERLAY, Host, HostRelease, HostReleaseStatus, OVERLAY_REBOOT_BREADCRUMB,
    overlay_from_container,
};

/// The systemd unit running the OS rollback validation after a HUP.
const ROLLBACK_HEALTH_UNIT: &str = "rollback-health";

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Oci(#[from] oci::Error),

    #[error(transparent)]
    Store(#[from] store::Error),

    #[error(transparent)]
    IO(#[from] std::io::Error),
}

/// Whether the OS rollback validation is in progress
fn validation_in_progress(unit: &systemd::UnitStatus) -> bool {
    // no rollback machinery on this OS: nothing to wait for
    if !unit.exists() {
        return false;
    }
    // the validation script is running: the OS may still roll back
    if unit.is_activating() {
        return true;
    }
    // systemd has not evaluated the unit's condition yet this boot: too early
    // to know whether a validation is pending, defer conservatively (resolves
    // within seconds; polls re-trigger).
    //
    // Any other state means the condition was evaluated and the script is not
    // running: "active" is a validation that succeeded this boot
    // (RemainAfterExit), "failed" hands the outcome to the rollback machinery
    // (altboot on next reboot).
    unit.is_inactive() && !unit.conditions_evaluated()
}

/// Query the rollback validation state.
///
/// A unit that cannot be queried is reported as not validating: refusing host
/// work on a failed query would strand the device on any OS where the unit is
/// unreachable.
pub(crate) async fn os_validation_in_progress() -> bool {
    match systemd::unit_status(ROLLBACK_HEALTH_UNIT).await {
        Ok(unit) => validation_in_progress(&unit),
        Err(e) => {
            tracing::warn!(
                "could not query {ROLLBACK_HEALTH_UNIT}.service, assuming no rollback validation in progress: {e}"
            );
            false
        }
    }
}

/// Read the hostapp data from the store
pub async fn from_store(
    host: &mut Host,
    docker: &Docker,
    local_store: &DocumentStore,
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
                let mut release: HostRelease = hostapp_doc.into_value().await?;

                // Overlays are never bookkept; derive them fresh below.
                release.overlays = mahler::state::Map::new();

                // ignore the status on the store and deduce it instead
                release.status = if host.meta.build.as_ref() == Some(&release.hostapp.build) {
                    // if the hostapp build is the current OS build then the release is running
                    HostReleaseStatus::Running
                } else if breadcrumb::exists(&format!("{BALENAHUP}-{release_uuid}-breadcrumb"))
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
                    release.hostapp.install_attempts = 0;
                }

                host.releases.insert(release_uuid, release);
            }
            Err(store::Error::NotFound { .. }) => {}
            Err(e) => return Err(e)?,
        }
    }

    // Derive overlay extensions from engine reality and attach them to their
    // release. The release uuid is encoded in the container name at deploy
    // time (see overlays.rs).
    let boot_time = proc::boot_time()?;
    // The ABI id of the kernel that booted.
    let running_abi = proc::kernel_abi()?;
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
            overlay_from_container(container, boot_time, running_abi.as_deref())
        else {
            continue; // not a helios-deployed overlay
        };
        if let Some(rel) = host.releases.get_mut(&release_uuid) {
            rel.overlays.insert(name, overlay);
        }
    }

    // A helios-issued reboot during the validation window would trigger the
    // OS rollback, so host work defers while the validation runs. The condition
    // is device-global (one rollback-health unit), so it lives on the host.
    host.os_validating = os_validation_in_progress().await;

    host.pending_reboot = breadcrumb::exists(OVERLAY_REBOOT_BREADCRUMB).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::systemd::UnitStatus;

    fn unit(load: &str, active: &str, condition_ts: u64) -> UnitStatus {
        UnitStatus::new(load, active, condition_ts)
    }

    #[test]
    fn defers_while_the_validation_script_runs() {
        assert!(validation_in_progress(&unit("loaded", "activating", 123)));
    }

    #[test]
    fn defers_before_the_condition_is_evaluated_this_boot() {
        assert!(validation_in_progress(&unit("loaded", "inactive", 0)));
    }

    #[test]
    fn proceeds_on_a_normal_boot_without_breadcrumb() {
        assert!(!validation_in_progress(&unit("loaded", "inactive", 42)));
    }

    #[test]
    fn proceeds_after_a_successful_validation() {
        assert!(!validation_in_progress(&unit("loaded", "active", 42)));
    }

    #[test]
    fn proceeds_when_the_script_failed_and_altboot_owns_the_outcome() {
        assert!(!validation_in_progress(&unit("loaded", "failed", 42)));
    }

    #[test]
    fn proceeds_when_the_rollback_machinery_does_not_exist() {
        assert!(!validation_in_progress(&unit("not-found", "inactive", 0)));
    }
}
