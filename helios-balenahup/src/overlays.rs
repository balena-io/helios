use std::collections::HashMap;
use std::time::SystemTime;

use mahler::extract::{Args, Res, Target, View};
use mahler::task::prelude::*;
use tracing::debug;

use crate::common_types::ImageUri;
use crate::oci::{
    self, Client as Docker, ContainerConfig, ContainerState, ContainerStatus, LocalContainer,
    Mount, NetworkMode, WithContext,
};

use super::models::{Overlay, OverlayStatus};

/// `io.balena.image.class` — marks an overlay service.
pub(crate) const CLASS_LABEL: &str = "io.balena.image.class";
/// Value of `CLASS_LABEL` for overlays handled in Phase 1. The `class` field is
/// retained verbatim for forward-compat with future non-overlay classes
/// (firmware, etc.) even though only `overlay` is accepted in Phase 1.
pub(crate) const CLASS_OVERLAY: &str = "overlay";
/// `io.balena.update.requires-reboot` — activation needs a reboot.
pub(crate) const REQUIRES_REBOOT_LABEL: &str = "io.balena.update.requires-reboot";
/// `io.balena.service-name` — the overlay's service name.
pub(crate) const SERVICE_NAME_LABEL: &str = "io.balena.service-name";
/// helios-private: links an overlay container to its host release uuid.
pub(crate) const RELEASE_LABEL: &str = "io.balena.private.hostapp.release";
/// helios-private: carries the overlay's target image uri for faithful diffing.
pub(crate) const IMAGE_LABEL: &str = "io.balena.private.image";
/// The Docker runtime that runs overlay activation hooks and exits 0.
pub(crate) const OVERLAY_RUNTIME: &str = "extension";

/// Encode `requires_reboot` for the `io.balena.update.requires-reboot` label.
fn reboot_flag(requires_reboot: bool) -> &'static str {
    if requires_reboot { "1" } else { "0" }
}

/// Decode the `io.balena.update.requires-reboot` label value.
fn parse_reboot_flag(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

/// 12-hex-char short digest of an image reference, mirroring the legacy
/// `shortDigest`. Falls back to a sanitized form when no digest is present.
fn short_digest(image: &ImageUri) -> String {
    match image.digest() {
        Some(d) => d.trim_start_matches("sha256:").chars().take(12).collect(),
        None => image
            .as_str()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(12)
            .collect(),
    }
}

/// Name of the `ext_*` volume backing one image-declared VOLUME of an overlay,
/// mirroring the legacy `volumeNameFor` so OS-side activation finds it.
pub(crate) fn volume_name_for(service: &str, image: &ImageUri, dest: &str) -> String {
    let sanitized = dest.trim_start_matches('/').replace('/', "_");
    format!("ext_{service}_{}_{sanitized}", short_digest(image))
}

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error(transparent)]
    Oci(#[from] oci::Error),
}

/// Deploy a single overlay extension: pull the image and run a one-shot
/// `extension`-runtime container that writes its kernel override into the
/// `ext_*` named volume(s). Mirrors the legacy `deployExtensionContainer`.
pub(crate) fn deploy_overlay(
    overlay: View<Option<Overlay>>,
    Args((release_uuid, name)): Args<(String, String)>,
    Target(tgt): Target<Overlay>,
    docker: Res<Docker>,
) -> IO<Overlay, OverlayError> {
    // Optimistic in-memory state: the planner treats the overlay as Deployed.
    let overlay = overlay.create(Overlay {
        image: tgt.image.clone(),
        class: tgt.class.clone(),
        requires_reboot: tgt.requires_reboot,
        status: OverlayStatus::Deployed,
    });

    with_io(overlay, async move |overlay| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let image = overlay.image.clone();

        debug!("pull overlay '{name}' image from '{image}'");
        docker.image().pull(&image, None).await?;

        // Back each image-declared VOLUME with a named `ext_*` volume so the
        // override survives container reap and OS volume-discovery finds it.
        let img = docker.image().inspect(image.as_str()).await?;
        let volumes: Vec<Mount> = img
            .config
            .volumes
            .into_iter()
            .map(|dest| Mount::Volume {
                source: volume_name_for(&name, &image, &dest),
                target: dest,
                read_only: false,
                nocopy: false,
                subpath: None,
            })
            .collect();

        let labels = HashMap::from([
            (CLASS_LABEL.to_string(), CLASS_OVERLAY.to_string()),
            (
                REQUIRES_REBOOT_LABEL.to_string(),
                reboot_flag(overlay.requires_reboot).to_string(),
            ),
            (SERVICE_NAME_LABEL.to_string(), name.clone()),
            (RELEASE_LABEL.to_string(), release_uuid.clone()),
            (IMAGE_LABEL.to_string(), image.as_str().to_string()),
        ]);

        let config = ContainerConfig {
            // `none` cmd: the extension runtime runs hooks, not the image cmd.
            command: Some(vec!["none".to_string()]),
            labels,
            runtime: Some(OVERLAY_RUNTIME.to_string()),
            // overlays are stateless one-shots; they never need a network.
            network_mode: Some(NetworkMode::None),
            volumes,
            ..Default::default()
        };

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

        Ok(overlay)
    })
}

/// Remove an overlay's container. Volumes are left in place (the OS reaper
/// finalizes them on reboot), mirroring the legacy `removeExtensionContainer`.
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

        let ids = docker
            .container()
            .list_with_labels(vec![
                &format!("{CLASS_LABEL}={CLASS_OVERLAY}"),
                &format!("{SERVICE_NAME_LABEL}={name}"),
                &format!("{RELEASE_LABEL}={release_uuid}"),
            ])
            .await?;
        for id in ids {
            docker
                .container()
                .remove(&id)
                .await
                .with_context(|| format!("failed to remove overlay container '{name}'"))?;
        }

        Ok(overlay)
    })
}

/// Derive an overlay's status from its container's runtime state and the host
/// boot time. The helios form of the legacy `requiresActivationReboot`:
/// `Active` unless the overlay requires a reboot and was staged after boot.
pub(crate) fn derive_overlay_status(
    state: &ContainerState,
    requires_reboot: bool,
    boot_time: SystemTime,
) -> OverlayStatus {
    // Deployed requires a clean one-shot exit (legacy: Exited && code 0 && no error).
    let deployed = state.status == ContainerStatus::Stopped
        && state.exit_code == Some(0)
        && state.error.as_deref().unwrap_or("").is_empty();
    if !deployed {
        return OverlayStatus::Absent;
    }

    let created = state.created.as_system_time();
    // Strict `>` is deliberate: `/proc/stat` btime has 1-second granularity, so
    // an overlay staged in the same second as boot is treated as pre-boot
    // (Active). Do NOT relax to `>=`.
    let needs_activation_reboot = requires_reboot && created > boot_time;
    if needs_activation_reboot {
        OverlayStatus::Deployed
    } else {
        OverlayStatus::Active
    }
}

/// Build the derived `Overlay` (and its release uuid + service name) from an
/// overlay container, reading identity off the labels written at deploy time.
/// Returns `None` for containers missing the helios-private labels (e.g. manual
/// deploys).
pub(crate) fn overlay_from_container<N>(
    container: &LocalContainer<N>,
    boot_time: SystemTime,
) -> Option<(String, String, Overlay)> {
    let labels = &container.config.labels;
    let name = labels.get(SERVICE_NAME_LABEL)?.clone();
    let release_uuid = labels.get(RELEASE_LABEL)?.clone();
    let image: ImageUri = labels.get(IMAGE_LABEL)?.parse().ok()?;
    let requires_reboot = parse_reboot_flag(labels.get(REQUIRES_REBOOT_LABEL).map(String::as_str));
    let class = labels
        .get(CLASS_LABEL)
        .cloned()
        .unwrap_or_else(|| CLASS_OVERLAY.to_string());

    let overlay = Overlay {
        image,
        class,
        requires_reboot,
        status: derive_overlay_status(&container.state, requires_reboot, boot_time),
    };
    Some((release_uuid, name, overlay))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn volume_name_matches_legacy_convention() {
        let image: ImageUri =
            "registry2.balena-cloud.com/v2/ko@sha256:42befc76f4f8aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()
                .unwrap();
        assert_eq!(
            volume_name_for("kernel-modules", &image, "/boot"),
            "ext_kernel-modules_42befc76f4f8_boot"
        );
    }

    fn state(status: ContainerStatus, exit: Option<i64>, created: SystemTime) -> ContainerState {
        ContainerState {
            status,
            healthy: true,
            created: created.into(),
            error: None,
            exit_code: exit,
        }
    }

    #[test]
    fn derives_active_when_staged_before_boot() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500); // before boot
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(derive_overlay_status(&st, true, boot), OverlayStatus::Active);
    }

    #[test]
    fn derives_deployed_when_staged_after_boot_and_requires_reboot() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500); // after boot
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, true, boot),
            OverlayStatus::Deployed
        );
    }

    #[test]
    fn derives_absent_on_nonzero_exit() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, Some(1), created);
        assert_eq!(derive_overlay_status(&st, true, boot), OverlayStatus::Absent);
    }
}
