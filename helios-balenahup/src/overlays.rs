use std::collections::HashMap;

use mahler::extract::{Args, Res, Target, View};
use mahler::task::prelude::*;
use tracing::debug;

use crate::oci::{
    self, Client as Docker, ContainerConfig, LocalNamespace, Mount, Namespace, NetworkMode,
    RegistryAuth, WithContext,
};
use crate::reboot::mark_pending_reboot;
use crate::util::breadcrumb;
use crate::util::systemd;

use super::models::{OVERLAY_REBOOT_BREADCRUMB, Overlay, OverlayStatus, overlay_labels};

/// Prefix of the image labels copied onto the `ext_*` volumes at deploy time.
///
/// A volume carries no labels of its own, and the OS sweep
/// (`balena-extension-manager cleanup --stale-os`) selects on them: it skips
/// any volume without `io.balena.image.class=overlay`, then decides staleness
/// from `kernel-version`, `kernel-abi-id` and `os-version`. An unlabelled
/// volume is therefore never collected, so copying these is what keeps the
/// `ext_*` volumes from accumulating one per overlay version.
const IMAGE_LABEL_PREFIX: &str = "io.balena.image.";
/// The Docker runtime that runs overlay activation hooks and exits 0.
const OVERLAY_RUNTIME: &str = "extension";

/// 12-hex-char short id of an image's content digest
fn short_digest(digest: &str) -> String {
    digest
        .trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect()
}

/// Name of the `ext_*` volume backing one image-declared VOLUME of an overlay.
fn volume_name_for(service: &str, digest: &str, dest: &str) -> String {
    let sanitized = dest.trim_start_matches('/').replace('/', "_");
    format!("ext_{service}_{}_{sanitized}", short_digest(digest))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OverlayError {
    #[error(transparent)]
    Oci(#[from] oci::Error),

    #[error("overlay '{name}' activation container exited with code {code}")]
    ActivationFailed { name: String, code: i64 },

    #[error(transparent)]
    Systemd(#[from] systemd::Error),

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
    });

    with_io(overlay, async move |overlay| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let image = overlay.image.clone();

        debug!("pull overlay '{name}' image from '{image}'");
        let credentials = registry_auth
            .as_ref()
            .and_then(|auth| auth.credentials(&image));
        docker.image().pull(&image, credentials).await?;

        // Back each image-declared VOLUME with a named `ext_*` volume so the
        // override survives container reap and OS volume-discovery finds it.
        let img = docker.image().inspect(image.as_str()).await?;

        // Resolve a content-stable id for the ext_* volume name
        let content_digest = image.digest().cloned().unwrap_or_else(|| img.id.clone());

        let image_labels: HashMap<String, String> = img
            .config
            .labels
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, _)| k.starts_with(IMAGE_LABEL_PREFIX))
            .collect();

        let volumes: Vec<Mount> = img
            .config
            .volumes
            .into_iter()
            .map(|dest| Mount::Volume {
                source: volume_name_for(&name, &content_digest, &dest),
                target: dest,
                read_only: false,
                nocopy: false,
                subpath: None,
                labels: image_labels.clone(),
            })
            .collect();

        let config = ContainerConfig {
            // The `none` placeholder cmd is required, not decorative
            command: Some(vec!["none".to_string()]),
            labels: overlay_labels(&name, &image),
            runtime: Some(OVERLAY_RUNTIME.to_string()),
            // overlays are stateless one-shots; they never need a network.
            network_mode: Some(NetworkMode::None),
            volumes,
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

/// Disarm an overlay extension through the OS extension manager, which also
/// drops its container.
///
/// The manager aborts without removing if the hook fails, so a failure here
/// leaves the extension armed and the calling task is retried, rather than
/// ever leaving a half-removed extension behind.
///
/// The `ext_*` volumes are deliberately left behind
async fn deactivate_overlay(release_uuid: &str, name: &str) -> Result<(), OverlayError> {
    // The manager addresses the container by its composed identifier, so it is
    // derived the same way the engine composes it for user services.
    let container = LocalNamespace::from(release_uuid).to_identifier(name);
    let cmd = systemd::Command::new("/usr/bin/balena-extension-manager")
        .args(&["deactivate", &container]);
    systemd::run(&format!("extension-remove-{name}-{release_uuid}"), &cmd).await?;

    Ok(())
}

/// Remove an overlay and record the reboot that applies the removal.
///
/// The breadcrumb is written before the disarm, and the order matters. Written
/// after, a disarm that succeeds followed by a failed breadcrumb write drops
/// the container and leaves no record: the next read sees the overlay gone and
/// the flag clear, calls the target reached, and the stale root composition
/// survives with nothing left in the state to schedule a reboot from. Written
/// first, the same failure costs one reboot that clears itself, and a disarm
/// that keeps failing aborts the workflow before the reboot task it feeds, so
/// no reboot lands while the extension is still armed.
pub(crate) fn remove_overlay(
    overlay: View<Overlay>,
    Args((release_uuid, name)): Args<(String, String)>,
) -> IO<Option<Overlay>, OverlayError> {
    let overlay = overlay.delete();

    with_io(overlay, async move |overlay| {
        breadcrumb::set(OVERLAY_REBOOT_BREADCRUMB).await?;
        deactivate_overlay(&release_uuid, &name).await?;

        Ok(overlay)
    })
}

/// Remove an overlay and schedule the reboot that applies the removal.
///
/// The removal writes the overlay subtree and the flag writes
/// `/host/pending_reboot`, and a task may only write its own subtree, so this
/// takes two tasks. Their listed order carries no meaning: the two paths are
/// disjoint, so the planner is free to branch them concurrently, and
/// `mark_pending_reboot` performs no IO to race with.
pub(crate) fn remove_overlay_and_mark_reboot() -> Vec<Task> {
    vec![remove_overlay.into_task(), mark_pending_reboot.into_task()]
}

/// Reconcile an overlay that already exists in the state but does not match
/// the target.
///
/// Two things can be wrong with an existing overlay, and both are repaired the
/// same way: drop the container so the create path pulls, re-runs the hooks and
/// re-arms. The container never survives to be re-armed in place, so the hooks'
/// idempotency is not relied on.
pub(crate) fn redeploy_overlay(
    overlay: View<Overlay>,
    Target(tgt): Target<Overlay>,
) -> Option<Task> {
    // The image changed, so what is deployed is the wrong extension.
    let wrong_image = overlay.image != tgt.image;
    // Or the right extension is deployed but the kernel it claims is not the one
    // that booted.
    let arm_did_not_take = overlay.status == OverlayStatus::Stale;

    if wrong_image || arm_did_not_take {
        return Some(remove_overlay.into_task());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_name_matches_legacy_convention() {
        // The resolved content digest (`sha256:` stripped) drives the name.
        assert_eq!(
            volume_name_for(
                "kernel-modules",
                "sha256:42befc76f4f8aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "/boot"
            ),
            "ext_kernel-modules_42befc76f4f8_boot"
        );
    }

    #[test]
    fn volume_name_distinguishes_image_content() {
        // Different content digests must yield different volumes so a new
        // overlay version never reuses a stale volume's contents.
        let v1 = volume_name_for("mods", "sha256:aaaaaaaaaaaa0000", "/boot");
        let v2 = volume_name_for("mods", "sha256:bbbbbbbbbbbb0000", "/boot");
        assert_ne!(v1, v2);
    }
}
