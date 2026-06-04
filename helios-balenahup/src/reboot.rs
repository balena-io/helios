use std::io;
use std::path::{Path, PathBuf};

use mahler::extract::{Args, Res, View};
use mahler::task::prelude::*;

use crate::common_types::HostRuntimeDir;
use crate::util::fs::run_async;
use crate::util::locking::{self, find_update_locks, ForceAcquireLocks, LockSet};
use crate::util::systemd;

use super::models::{HostRelease, HostReleaseStatus, OverlayStatus};

/// Returns the path of a user-held update lock that forbids rebooting, or
/// `None` if it is safe to reboot. With `force`, externally-held locks are
/// overridden (mirrors `UpdateOpts.force`), so this always returns `None`.
fn blocking_lock(
    locks: &LockSet,
    runtime_dir: &Path,
    force: bool,
) -> io::Result<Option<PathBuf>> {
    for path in find_update_locks(runtime_dir)? {
        // A lock helios itself holds (taken to update its own service) does not
        // forbid helios's own reboot, and must NOT be released by this read-only
        // gate — leave it untouched.
        if locks.holds(&path) {
            continue;
        }
        match locks.try_lock(path.clone(), force) {
            // The lock was free (or force-overridden): we only took it to probe,
            // so release the transient lock immediately.
            Ok(()) => {
                let _ = locks.unlock(path);
            }
            // Held by another party (a user service): defer the reboot.
            Err(locking::Error::WouldBlock) => return Ok(Some(path)),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}

/// Issue the single coordinated host-OS reboot once the rootfs is staged and
/// every reboot-requiring overlay is deployed.
///
/// This is a `job::update` on `/host/releases/{release_uuid}`, sharing the
/// route with `install_hostapp_release`. The two are mutually exclusive by
/// state: install is guarded `status == Created`, this is guarded
/// `status == Installed`.
pub(crate) fn reboot_to_activate(
    mut release: View<HostRelease>,
    Args(_release_uuid): Args<String>,
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
    host_runtime_dir: Res<HostRuntimeDir>,
) -> IO<HostRelease, RebootError> {
    // Only relevant once the rootfs is staged and waiting for a reboot...
    enforce!(
        release.status == HostReleaseStatus::Installed,
        "release is not staged for reboot"
    );
    // ...and every reboot-requiring overlay has been deployed (staged on its
    // ext_* volume). Non-reboot overlays are already Active.
    enforce!(
        release.overlays.values().all(|o| !o.requires_reboot
            || matches!(o.status, OverlayStatus::Deployed | OverlayStatus::Active)),
        "overlays not yet deployed"
    );

    // Planner view: the single coordinated reboot activates the rootfs and the
    // overlays together. The release becomes Running and every deployed overlay
    // becomes Active (mirroring the post-reboot re-derivation, where the staged
    // override is carried by the now-running kernel: createdAt < bootTime).
    release.status = HostReleaseStatus::Running;
    for overlay in release.overlays.values_mut() {
        if overlay.status == OverlayStatus::Deployed {
            overlay.status = OverlayStatus::Active;
        }
    }

    with_io(release, async move |release| {
        let force = force_acquire_locks
            .as_ref()
            .expect("force_acquire_locks should be available")
            .enabled();
        let runtime_dir = host_runtime_dir
            .as_ref()
            .expect("host_runtime_dir resource should be available")
            .as_path()
            .to_path_buf();

        // Honor the user update-lock: a reboot tears down every container, so
        // refuse while a user service holds a lock (unless forced). We acquire
        // nothing long-lived; this is a gate only. The lock probing is sync, so
        // run it off the async executor (mirrors `take_locks`).
        if let Some(held) = run_async(move || {
            let locks = locks.as_ref().expect("locks resource should be available");
            blocking_lock(locks, &runtime_dir, force)
        })
        .await?
        {
            return Err(RebootError::Locked(held));
        }

        // Issue the single coordinated reboot. The process is torn down as part
        // of shutdown; correctness is by re-derivation on next boot.
        systemd::reboot().await?;
        Ok(release)
    })
}

#[derive(Debug, thiserror::Error)]
pub enum RebootError {
    #[error("a user update-lock forbids rebooting: {}", .0.display())]
    Locked(PathBuf),
    #[error(transparent)]
    IO(#[from] io::Error),
    #[error(transparent)]
    Systemd(#[from] systemd::Error),
}

