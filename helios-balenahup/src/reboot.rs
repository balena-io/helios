use std::io;
use std::path::PathBuf;

use mahler::extract::{Args, Res, System, SystemTarget, Target, View};
use mahler::task::prelude::*;

use crate::util::dirs::runtime_dir;
use crate::util::fs::run_async;
use crate::util::locking::{ForceAcquireLocks, LockSet};
use crate::util::systemd;

use super::models::{Device, HostRelease, HostReleaseStatus, OverlayStatus};
use super::overlays::overlays_ready;
use super::tasks::{host_is_validating, overlay_removal_pending};

/// Issue the coordinated host-OS reboot, honoring user update locks.
///
/// A forced update overrides those locks, so the gate is skipped entirely
/// rather than consulted and ignored.
///
/// TODO: the lock scan is point-in-time. Holding every app lock across the
/// reboot closes that.
async fn guarded_reboot(
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
) -> Result<(), RebootError> {
    let force = force_acquire_locks
        .as_ref()
        .expect("force_acquire_locks should be available")
        .enabled();

    if !force {
        // Scan the container-visible runtime dir, not the host-side path
        let runtime_dir = runtime_dir();
        let held = run_async(move || {
            let locks = locks.as_ref().expect("locks resource should be available");
            locks.find_blocking(&runtime_dir)
        })
        .await?;
        if let Some(path) = held {
            return Err(RebootError::Locked(path));
        }
    }

    systemd::reboot().await?;
    Ok(())
}

/// Issue the single coordinated host-OS reboot to activate a release's
/// reboot-requiring overlays.
// TODO: Replace with a device level `requires_reboot` flag and a top level task
// that takes and holds all app locks before rebooting,
pub(crate) fn reboot_to_activate(
    mut release: View<HostRelease>,
    Args(_release_uuid): Args<String>,
    Target(tgt): Target<HostRelease>,
    System(device): System<Device>,
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
) -> IO<HostRelease, RebootError> {
    enforce!(!host_is_validating(&device), "host validation in progress");
    enforce!(
        release.status == HostReleaseStatus::Installed
            || (release.status == HostReleaseStatus::Running
                && release
                    .overlays
                    .values()
                    .any(|o| o.status == OverlayStatus::Deployed)),
        "release is not staged for reboot and no overlay needs activation"
    );
    enforce!(overlays_ready(&release, &tgt), "overlays not yet deployed");
    // The reboot splices whatever is staged when it lands, so an overlay the
    // target dropped has to be withdrawn before it, not after.
    enforce!(
        release
            .overlays
            .iter()
            .all(|(name, o)| o.status != OverlayStatus::Deployed
                || tgt.overlays.contains_key(name)),
        "a dropped overlay is still staged"
    );

    release.status = HostReleaseStatus::Running;
    for overlay in release.overlays.values_mut() {
        if overlay.status == OverlayStatus::Deployed {
            overlay.status = OverlayStatus::Active;
        }
    }

    with_io(release, async move |release| {
        guarded_reboot(locks, force_acquire_locks).await?;
        Ok(release)
    })
}

/// Reboot to apply overlay removals, once every removal the planner can still
/// make has been made.
///
pub(crate) fn reboot_to_apply_overlays(
    mut pending: View<bool>,
    System(device): System<Device>,
    SystemTarget(target): SystemTarget<Device>,
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
) -> IO<bool, RebootError> {
    enforce!(*pending, "no overlay change awaiting a reboot");
    enforce!(!host_is_validating(&device), "host validation in progress");
    enforce!(
        !overlay_removal_pending(&device, &target),
        "overlay removals still pending"
    );
    *pending = false;

    with_io(pending, async move |pending| {
        guarded_reboot(locks, force_acquire_locks).await?;
        Ok(pending)
    })
}

/// Record that an overlay change is waiting for the reboot that applies it.
///
/// Pure state, no IO: the breadcrumb backing the flag is written by
/// `remove_overlay`. A task may only write its own subtree, so the removal
/// cannot raise this flag itself. Expanding both from one method is what lets
/// the planner see the flag rise during simulation and sequence
/// `reboot_to_apply_overlays` into the same workflow, instead of waiting for
/// the next state read to surface the breadcrumb.
pub(crate) fn mark_pending_reboot(mut pending: View<bool>) -> View<bool> {
    *pending = true;
    pending
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RebootError {
    #[error("a user update-lock forbids rebooting: {}", .0.display())]
    Locked(PathBuf),
    #[error(transparent)]
    IO(#[from] io::Error),
    #[error(transparent)]
    Systemd(#[from] systemd::Error),
}
