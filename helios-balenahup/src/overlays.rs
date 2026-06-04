use mahler::extract::{Args, Res, Target, View};
use mahler::task::prelude::*;
use tracing::debug;

use crate::oci::{
    self, Client as Docker, ContainerConfig, ContainerStatus, LocalNamespace, Namespace,
    NetworkMode, RegistryAuth, WithContext,
};
use crate::util::proc;

use super::models::{Overlay, OverlayStatus, OverlayTarget, overlay_labels};

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
    docker: Res<Docker>,
    registry_auth: Res<RegistryAuth>,
) -> IO<Overlay, OverlayError> {
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
        let boot_id = proc::boot_id()?;

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

/// Decide whether a removal attempt withdrew the overlay.
///
/// A removal that leaves the container `Dead` is a success.
///
/// The container is inspected only when the removal failed. An inspect that
/// fails, or one that finds the container alive, says only that the removal did
/// not do its job, so the error propagates and the task is retried.
async fn interpret_withdrawal(
    container: &str,
    removal: Result<(), oci::Error>,
    inspect: impl AsyncFnOnce() -> Result<ContainerStatus, oci::Error>,
) -> Result<(), OverlayError> {
    let Err(e) = removal else {
        return Ok(());
    };

    match inspect().await {
        Ok(ContainerStatus::Dead) => {
            debug!("container '{container}' is dead, the withdrawal is complete");
            Ok(())
        }
        _ => Err(OverlayError::from(e)),
    }
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
    let removal = docker.container().remove(&container).await;
    interpret_withdrawal(&container, removal, async || {
        docker
            .container()
            .inspect(&container)
            .await
            .map(|c| c.state.status)
    })
    .await
}

/// Remove an overlay: withdraw its extension.
pub(crate) fn remove_overlay(
    overlay: View<Overlay>,
    Args((release_uuid, name)): Args<(String, String)>,
    docker: Res<Docker>,
) -> IO<Option<Overlay>, OverlayError> {
    let overlay = overlay.delete();

    with_io(overlay, async move |overlay| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        withdraw_overlay(docker, &release_uuid, &name).await?;

        Ok(overlay)
    })
}

/// Reconcile an overlay that already exists in the state but does not match
/// the target.
pub(crate) fn redeploy_overlay(
    overlay: View<Overlay>,
    Target(tgt): Target<Overlay>,
) -> Option<Task> {
    if overlay_diverged(&overlay, &tgt) {
        return Some(remove_overlay.into_task());
    }
    None
}

/// Whether an existing overlay differs from its target in a way only a
/// recreate can settle.
fn overlay_diverged(overlay: &Overlay, tgt: &OverlayTarget) -> bool {
    // The image changed, so what is deployed is the wrong extension.
    let wrong_image = overlay.image != tgt.image;
    // Or the right extension is deployed but the kernel it claims is not the one
    // that booted.
    let arm_did_not_take = overlay.status == OverlayStatus::Stale;
    // Or the runtime the composition asks for is not the one the container was
    // created against, which no in-place change can fix.
    let wrong_runtime = overlay.runtime != tgt.runtime;

    wrong_image || arm_did_not_take || wrong_runtime
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common_types::ImageUri;

    const CONTAINER: &str = "app_1_release_1_kernel-modules";

    const IMAGE: &str = "registry2.balena-cloud.com/v2/abc123:latest";

    /// What the engine returns for a removal it could not carry out.
    fn engine_error() -> oci::Error {
        oci::Error::other("failed to remove container: driver is busy")
    }

    #[tokio::test]
    async fn a_removal_the_engine_accepted_ends_the_withdrawal() {
        // The container is gone, so there is nothing left to inspect.
        assert!(
            interpret_withdrawal(CONTAINER, Ok(()), async || unreachable!(
                "a removal that succeeded must not inspect"
            ))
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_dead_container_ends_the_withdrawal() {
        // The engine could not release the layer the running root pins, which
        // is every removal a mounted extension can get. Retrying it until the
        // reboot would never converge.
        assert!(
            interpret_withdrawal(CONTAINER, Err(engine_error()), async || Ok(
                ContainerStatus::Dead
            ))
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_container_that_survived_keeps_the_task_retryable() {
        // The removal did not take and the extension is still composable, so
        // the error has to reach the seek loop.
        assert!(matches!(
            interpret_withdrawal(CONTAINER, Err(engine_error()), async || Ok(
                ContainerStatus::Stopped(0)
            ))
            .await,
            Err(OverlayError::Oci(_))
        ));
    }

    #[tokio::test]
    async fn an_unreadable_container_keeps_the_task_retryable() {
        // An inspect that fails says nothing about the removal, so the removal's
        // own failure stands.
        assert!(matches!(
            interpret_withdrawal(CONTAINER, Err(engine_error()), async || Err(
                oci::Error::other("engine socket refused")
            ))
            .await,
            Err(OverlayError::Oci(_))
        ));
    }

    fn overlay_state(image: &str, status: OverlayStatus, runtime: &str) -> Overlay {
        Overlay {
            image: ImageUri::from_static(image),
            status,
            runtime: runtime.to_string(),
        }
    }

    /// A target overlay the way `HostTarget` builds one: always asking to be
    /// carried by the running kernel.
    fn overlay_target(image: &str, runtime: &str) -> OverlayTarget {
        OverlayTarget {
            image: ImageUri::from_static(image),
            status: OverlayStatus::Active,
            runtime: runtime.to_string(),
        }
    }

    #[test]
    fn a_runtime_the_container_was_not_created_against_forces_a_redeploy() {
        // The image matches and the overlay is armed, so without this the
        // planner emits no task and the runtime the composition now names never
        // reaches the engine.
        let overlay = overlay_state(IMAGE, OverlayStatus::Active, "runc");
        let tgt = overlay_target(IMAGE, "extension");

        assert!(overlay_diverged(&overlay, &tgt));
    }

    #[test]
    fn an_overlay_created_against_the_runtime_the_target_names_stays_put() {
        let overlay = overlay_state(IMAGE, OverlayStatus::Active, "extension");
        let tgt = overlay_target(IMAGE, "extension");

        assert!(!overlay_diverged(&overlay, &tgt));
    }
}
