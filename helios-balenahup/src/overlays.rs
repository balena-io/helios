use std::collections::HashMap;

use mahler::extract::{Args, Res, Target, View};
use mahler::task::prelude::*;
use tracing::debug;

use crate::oci::{
    self, Client as Docker, ContainerConfig, Mount, NetworkMode, RegistryAuth, WithContext,
};

use super::models::{Overlay, OverlayStatus, overlay_labels};

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
            .remove(&format!("{name}_{release_uuid}"))
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

/// Remove an overlay's container. The `ext_*` volumes are deliberately left
/// behind: the OS reaps them at the HUP commit boundary, once the new release
/// has passed validation (`balena-extension-manager cleanup --stale-os`).
/// Removing them here would take the previous kernel's overlays with them,
/// which is exactly what a rollback needs to find still on disk.
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

        // The container is addressed by its composed identifier: the release
        // uuid is encoded in the name (LocalNamespace), same as user services.
        docker
            .container()
            .remove(&format!("{name}_{release_uuid}"))
            .await
            .with_context(|| format!("failed to remove overlay container '{name}'"))?;

        Ok(overlay)
    })
}

/// Reconcile an overlay that already exists in the state but does not match
/// the target.
///
pub(crate) fn redeploy_overlay(
    overlay: View<Overlay>,
    Target(tgt): Target<Overlay>,
) -> Option<Task> {
    // The image is the only trigger.
    if overlay.image != tgt.image {
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
