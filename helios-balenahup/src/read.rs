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

/// The systemd unit running the OS rollback validation after a HUP. Skipped on
/// a boot that installed no update.
const ROLLBACK_HEALTH_UNIT: &str = "rollback-health";

/// The systemd unit that reconciles the extension publications and puts an
/// independently armed override kernel on trial. Unconditional, so it runs on
/// every boot.
const EXTENSION_ROLLBACK_UNIT: &str = "extension-rollback";

/// The units whose activation holds host work back.
const HOST_VALIDATION_UNITS: [&str; 2] = [ROLLBACK_HEALTH_UNIT, EXTENSION_ROLLBACK_UNIT];

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Oci(#[from] oci::Error),

    #[error(transparent)]
    Store(#[from] store::Error),

    #[error(transparent)]
    IO(#[from] std::io::Error),
}

/// Whether the given validation unit is still holding host work back.
fn validation_in_progress(name: &str, unit: &systemd::UnitStatus) -> bool {
    // no validation machinery on this OS, or a unit systemd refuses to run
    // (masked, unloadable): nothing to wait for
    if !unit.exists() {
        return false;
    }
    // the validation script is running: the OS may still roll back, and the
    // publication sweep may still withdraw what a deploy is writing
    if unit.is_activating() {
        return true;
    }
    // Any other state means the condition was evaluated and the script is not
    // running: "active" is a validation that succeeded this boot
    // (RemainAfterExit), "failed" hands the outcome to the rollback machinery
    // (altboot on next reboot).
    let unevaluated = unit.is_inactive() && !unit.conditions_evaluated();
    if !unevaluated {
        return false;
    }
    // systemd has not evaluated the unit's condition yet this boot. The
    // condition is evaluated when the job runs, so a unit still waiting on its
    // ordering looks identical to one that will never run, and the job is what
    // separates them: defer for as long as systemd holds one, however long the
    // ordering takes to satisfy.
    if unit.job_queued() {
        return true;
    }
    // No job and no evaluation: the unit was not pulled into this boot's
    // transaction and is never going to run. Deferring on it would leave the
    // device taking no host update for the life of the boot, so say why and
    // proceed.
    tracing::warn!("{name}.service holds no start job this boot, proceeding with host work");
    false
}

/// Query the host validation state: whether either validation unit still owns
/// the host this boot.
///
/// A unit that cannot be queried is reported as not validating: refusing host
/// work on a failed query would strand the device on any OS where the unit is
/// unreachable.
async fn host_validating() -> bool {
    for name in HOST_VALIDATION_UNITS {
        match systemd::unit_status(name).await {
            Ok(unit) => {
                if validation_in_progress(name, &unit) {
                    return true;
                }
            }
            Err(e) => tracing::warn!(
                "could not query {name}.service, assuming it is not validating the host: {e}"
            ),
        }
    }
    false
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

    // An unreadable engine leaves the list unknown rather than empty: an empty
    // list is a host that registers nothing, and the two decline for different
    // reasons.
    host.engine_runtimes = match docker.runtimes().await {
        Ok(oci::Runtimes { names }) => Some(names),
        Err(e) => {
            tracing::warn!("could not read the runtimes the engine registers: {e}");
            None
        }
    };

    // Derive overlay extensions from engine reality and attach them to their
    // release. The release uuid is encoded in the container name at deploy
    // time (see overlays.rs).
    let boot_id = proc::boot_id()?;
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
            overlay_from_container(container, &boot_id, running_abi.as_deref())
        else {
            continue; // not a helios-deployed overlay
        };
        if let Some(rel) = host.releases.get_mut(&release_uuid) {
            rel.overlays.insert(name, overlay);
        }
    }

    // A helios-issued reboot during the rollback validation window would
    // trigger the rollback, and `extension-rollback` sweeps the publications a
    // concurrent redeploy would race, so host work defers while either unit
    // runs. The condition is device-global, so it lives on the host.
    host.host_validating = host_validating().await;

    host.pending_reboot = breadcrumb::exists(OVERLAY_REBOOT_BREADCRUMB).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::systemd::UnitStatus;

    /// A unit systemd holds a start job for.
    const QUEUED: bool = true;
    /// A unit systemd holds no job for.
    const NO_JOB: bool = false;

    fn unit(load: &str, active: &str, condition_ts: u64, job_queued: bool) -> UnitStatus {
        UnitStatus::new(load, active, condition_ts, job_queued)
    }

    fn in_progress(unit: &UnitStatus) -> bool {
        validation_in_progress(EXTENSION_ROLLBACK_UNIT, unit)
    }

    #[test]
    fn defers_while_the_validation_script_runs() {
        assert!(in_progress(&unit("loaded", "activating", 123, QUEUED)));
    }

    #[test]
    fn defers_while_the_unit_waits_on_its_ordering() {
        assert!(in_progress(&unit("loaded", "inactive", 0, QUEUED)));
    }

    #[test]
    fn proceeds_when_the_unit_holds_no_job_this_boot() {
        assert!(!in_progress(&unit("loaded", "inactive", 0, NO_JOB)));
    }

    #[test]
    fn proceeds_on_a_normal_boot_without_breadcrumb() {
        assert!(!in_progress(&unit("loaded", "inactive", 42, NO_JOB)));
    }

    #[test]
    fn proceeds_after_a_successful_validation() {
        assert!(!in_progress(&unit("loaded", "active", 42, NO_JOB)));
    }

    #[test]
    fn proceeds_when_the_script_failed_and_altboot_owns_the_outcome() {
        assert!(!in_progress(&unit("loaded", "failed", 42, NO_JOB)));
    }

    #[test]
    fn proceeds_when_the_rollback_machinery_does_not_exist() {
        assert!(!in_progress(&unit("not-found", "inactive", 0, NO_JOB)));
    }

    #[test]
    fn proceeds_when_the_unit_is_masked() {
        // systemd reports a masked unit as loaded-but-masked and never starts
        // it, so waiting on one waits for the life of the boot. It can still
        // hold a queued job, so the mask has to be caught before the job.
        assert!(!in_progress(&unit("masked", "inactive", 0, QUEUED)));
    }
}
