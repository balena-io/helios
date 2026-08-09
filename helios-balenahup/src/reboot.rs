use std::io;
use std::path::{Path, PathBuf};

use mahler::extract::{Args, Res, Target, View};
use mahler::task::prelude::*;

use crate::util::dirs::runtime_dir;
use crate::util::fs::run_async;
use crate::util::locking::{self, ForceAcquireLocks, LockSet, find_update_locks};
use crate::util::systemd;

use super::models::{HostRelease, HostReleaseStatus, OverlayStatus};

/// Returns the path of a user-held update lock that forbids disrupting its
/// service, or `None` if every lock under `runtime_dir` is free.
///
/// The scan is read-only: a free lock is taken only to prove it was free and
/// released again, and a lock helios itself holds is left untouched, since its
/// own update lock does not forbid its own reboot.
///
/// TODO: this is a point-in-time answer. A service that takes its update lock
/// after the scan and before the reboot lands has its critical section
/// violated. Closing that needs the reboot rework (public id 4536): a top level
/// task that takes and holds every app lock across the reboot.
fn find_blocking_lock(locks: &LockSet, runtime_dir: &Path) -> io::Result<Option<PathBuf>> {
    for path in find_update_locks(runtime_dir)? {
        // Taken by helios to update its own service: releasing it here would
        // drop a lock the caller still relies on.
        if locks.contains(&path) {
            continue;
        }
        match locks.try_lock(path.clone(), false) {
            // Free (a stale helios lockfile): we only took it to probe, so
            // release the transient lock immediately.
            Ok(()) => {
                let _ = locks.unlock(path);
            }
            // Held by another party (a user service).
            Err(locking::Error::WouldBlock) => return Ok(Some(path)),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}

/// Issue the coordinated host-OS reboot, honoring user update locks.
///
/// A forced update overrides those locks, so the gate is skipped entirely
/// rather than consulted and ignored.
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
            find_blocking_lock(locks, &runtime_dir)
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
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
) -> IO<HostRelease, RebootError> {
    enforce!(
        release.status == HostReleaseStatus::Installed
            || (release.status == HostReleaseStatus::Running
                && release
                    .overlays
                    .values()
                    .any(|o| o.status == OverlayStatus::Deployed)),
        "release is not staged for reboot and no overlay needs activation"
    );
    let overlays_ready = tgt.overlays.keys().all(|name| {
        release
            .overlays
            .get(name)
            .is_some_and(|o| matches!(o.status, OverlayStatus::Deployed | OverlayStatus::Active))
    });
    enforce!(overlays_ready, "overlays not yet deployed");

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

/// Reboot to apply an overlay removal.
///
/// The only condition is the flag itself, and that is deliberate.
pub(crate) fn reboot_to_apply_overlays(
    mut pending: View<bool>,
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
) -> IO<bool, RebootError> {
    enforce!(*pending, "no overlay change awaiting a reboot");
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reports_a_user_lock_and_leaves_it_intact() {
        // An external (user) lock: a plain file with no helios tag. The scan
        // must report it and leave it on disk, since destroying it would defeat
        // the lock for the service that took it.
        let dir = tempdir().unwrap();
        let svc = dir.path().join("app-uuid").join("svc");
        std::fs::create_dir_all(&svc).unwrap();
        let path = svc.join("updates.lock");
        std::fs::write(&path, b"user-held").unwrap();

        let locks = LockSet::new();
        let held = find_blocking_lock(&locks, dir.path()).unwrap();
        assert_eq!(held, Some(path.clone()));
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"user-held");
    }
}
