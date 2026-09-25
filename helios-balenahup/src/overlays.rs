use mahler::extract::{Args, Res, System, Target, View};
use mahler::task::prelude::*;
use tracing::debug;

use crate::oci::{
    self, Client as Docker, ContainerConfig, ContainerStatus, LocalNamespace, Namespace,
    NetworkMode, RegistryAuth, WithContext,
};
use crate::reboot::mark_pending_reboot;
use crate::util::breadcrumb;
use crate::util::fs::run_async;
use crate::util::proc;

use super::models::{
    Device, HostRelease, HostReleaseTarget, OVERLAY_REBOOT_BREADCRUMB, Overlay, OverlayStatus,
    overlay_labels,
};
use super::tasks::host_is_validating;

#[derive(Debug, thiserror::Error)]
pub(crate) enum OverlayError {
    #[error(transparent)]
    Oci(#[from] oci::Error),

    #[error("overlay '{name}' activation container exited with code {code}")]
    ActivationFailed { name: String, code: i64 },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Deploy a single overlay extension
pub(crate) fn deploy_overlay(
    overlay: View<Option<Overlay>>,
    Args((release_uuid, name)): Args<(String, String)>,
    Target(tgt): Target<Overlay>,
    System(device): System<Device>,
    docker: Res<Docker>,
    registry_auth: Res<RegistryAuth>,
) -> IO<Overlay, OverlayError> {
    enforce!(!host_is_validating(&device), "host validation in progress");
    // Optimistic in-memory state: the planner treats the overlay as Deployed.
    let overlay = overlay.create(Overlay {
        image: tgt.image.clone(),
        status: OverlayStatus::Deployed,
        runtime: tgt.runtime.clone(),
    });

    with_io(overlay, async move |overlay| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let image = overlay.image.clone();

        // Stamp the boot this container is created in, so a later read can tell
        // an overlay staged since the root was composed from one the running
        // root already carries.
        let boot_id = run_async(proc::boot_id).await?;

        debug!("pull overlay '{name}' image from '{image}'");
        let credentials = registry_auth
            .as_ref()
            .and_then(|auth| auth.credentials(&image));
        docker.image().pull(&image, credentials).await?;

        // Neither mounts nor a command. The `ext_*` volumes are the data plane
        // between the extension runtime and the OS boot machinery, and helios
        // is not an endpoint of it.
        let config = ContainerConfig {
            labels: overlay_labels(&name, &image, &boot_id, &tgt.runtime),
            // The engine's field is optional; the overlay contract is not.
            runtime: Some(tgt.runtime.clone()),
            // overlays are stateless one-shots; they never need a network.
            network_mode: Some(NetworkMode::None),
            ..Default::default()
        };

        // An interrupted deploy can leave a container that was created but
        // never started, and the engine rejects the duplicate name with a 409.
        // Removing a container that is not there is a no-op.
        docker
            .container()
            .remove(&LocalNamespace::from(release_uuid.as_str()).to_identifier(&name))
            .await
            .with_context(|| format!("failed to remove stale overlay container '{name}'"))?;

        let id = docker
            .container()
            .create(&name, release_uuid.as_str(), image.as_str(), config)
            .await
            .with_context(|| format!("failed to create overlay container '{name}'"))?;
        docker
            .container()
            .start(&id)
            .await
            .with_context(|| format!("failed to start overlay container '{name}'"))?;

        // The extension runtime runs the container and it exits.
        // Wait for that exit and fail the deploy on a failure,
        // so the host update aborts here rather than committing to a release
        // whose required extension never applied. The failed container is left
        // in place.
        let code = docker.container().wait(&id).await?;
        if code != 0 {
            return Err(OverlayError::ActivationFailed { name, code });
        }

        Ok(overlay)
    })
}

/// Withdraw an overlay extension: remove its container.
///
/// The removal is the whole withdrawal. The container's `Dead` flag is the
/// mount-exclusion signal the boot reads, the pre-kexec check refuses a kernel
/// no live container claims, and `extension-rollback` sweeps the publications
/// at the next boot. Nothing runs a hook and nothing has to be told.
///
/// The `ext_*` volumes are deliberately left behind.
async fn withdraw_overlay(
    docker: &Docker,
    release_uuid: &str,
    name: &str,
) -> Result<(), OverlayError> {
    // The container is addressed by its composed identifier, derived the same
    // way the engine composes it for user services.
    let container = LocalNamespace::from(release_uuid).to_identifier(name);

    debug!("withdraw overlay '{name}' container '{container}'");
    let Err(e) = docker.container().remove(&container).await else {
        return Ok(());
    };

    // The engine cannot remove a mounted extension: `Dead` means withdrawn.
    match docker.container().inspect(&container).await {
        Ok(c) if c.state.status == ContainerStatus::Dead => {
            debug!("container '{container}' is dead, the withdrawal is complete");
            Ok(())
        }
        _ => Err(OverlayError::from(e)),
    }
}

/// Remove an overlay and, if it was carried by the running kernel, record the
/// reboot that applies the removal.
///
/// Only `Active` reached the live root. `Stale` and `Failed` never did, and a
/// `Deployed` overlay the target drops is withdrawn before the reboot that
/// would splice it in, so none of the three needs a reboot to undo.
///
/// The breadcrumb precedes the withdrawal: written after, a failed write would
/// leave the container gone with no record left to reboot from.
pub(crate) fn remove_overlay(
    overlay: View<Overlay>,
    Args((release_uuid, name)): Args<(String, String)>,
    docker: Res<Docker>,
) -> IO<Option<Overlay>, OverlayError> {
    let was_active = overlay.status == OverlayStatus::Active;
    let overlay = overlay.delete();

    with_io(overlay, async move |overlay| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        if was_active {
            breadcrumb::set(OVERLAY_REBOOT_BREADCRUMB).await?;
        }
        withdraw_overlay(docker, &release_uuid, &name).await?;

        Ok(overlay)
    })
}

/// Remove an overlay and, if it was carried by the running kernel, schedule
/// the reboot that applies the removal.
///
/// Two tasks because a task may only write its own subtree. The listed order
/// carries no meaning: the paths are disjoint.
pub(crate) fn remove_overlay_and_mark_reboot(
    overlay: View<Overlay>,
    System(device): System<Device>,
) -> Vec<Task> {
    if host_is_validating(&device) {
        return Vec::new();
    }

    let mut tasks = vec![remove_overlay.into_task()];
    if overlay.status == OverlayStatus::Active {
        tasks.push(mark_pending_reboot.into_task());
    }
    tasks
}

/// Reconcile an overlay that already exists in the state but does not match
/// the target.
///
/// The image, the armed kernel and the runtime are fixed at creation, so a
/// recreate is the only remedy.
pub(crate) fn redeploy_overlay(
    overlay: View<Overlay>,
    Target(tgt): Target<Overlay>,
    System(device): System<Device>,
) -> Option<Task> {
    if host_is_validating(&device) {
        return None;
    }

    let diverged = overlay.image != tgt.image
        || overlay.status == OverlayStatus::Stale
        || overlay.runtime != tgt.runtime;

    diverged.then(|| remove_overlay.into_task())
}

/// Whether every overlay named in the target has reached the release, either
/// staged for activation or already active.
pub(crate) fn overlays_ready(release: &HostRelease, tgt: &HostReleaseTarget) -> bool {
    tgt.overlays.keys().all(|name| {
        release
            .overlays
            .get(name)
            .is_some_and(|o| matches!(o.status, OverlayStatus::Deployed | OverlayStatus::Active))
    })
}
