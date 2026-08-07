use std::collections::HashMap;
use std::time::SystemTime;

use mahler::state::{Map, State};
use serde::{Deserialize, Serialize};

use crate::common_types::{ImageUri, OperatingSystem, Uuid};
use crate::oci::{ContainerState, ContainerStatus, LocalContainer};
use crate::remote_model::HostRelease as RemoteHostReleaseTarget;

/// `io.balena.image.class` — marks an overlay service.
pub(crate) const CLASS_LABEL: &str = "io.balena.image.class";
/// Value of `CLASS_LABEL` for overlay services, the only class supported so
/// far. Other classes (firmware, etc.) will need a discriminator enum at the
/// remote-model parse boundary when they arrive.
pub(crate) const CLASS_OVERLAY: &str = "overlay";
/// `io.balena.service-name` — the overlay's service name.
const SERVICE_NAME_LABEL: &str = "io.balena.service-name";
/// helios-private: carries the overlay's target image uri for faithful diffing.
const IMAGE_LABEL: &str = "io.balena.private.image";

/// Alternative Device definition to avoid cicular dependencies
/// DO NOT use this outside the `System` extractor
#[derive(State, Debug, Clone)]
pub(crate) struct Device {
    /// The "hostapp" configuration
    pub host: Option<Host>,
}

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Host {
    /// Internal host metadata obtained from the hostOS
    #[mahler(internal)]
    pub meta: OperatingSystem,

    /// The hostapp releases. While only one release is expected on the target state, the
    /// device may be in-between releases, in which case there may still be clean-up steps to
    /// perform.
    pub releases: Map<Uuid, HostRelease>,

    /// Whether the running OS release is still on trial: the rollback
    /// validation has not finished, so the OS may yet roll back. This is a
    /// device-global condition (a single `rollback-health` unit), derived fresh
    /// on every read; a helios-issued reboot during the window would trigger
    /// the rollback, so all host work defers while it is set.
    #[mahler(internal, default)]
    pub os_validating: bool,
}

impl Host {
    pub fn new(meta: OperatingSystem) -> Self {
        Host {
            meta,
            releases: Map::new(),
            os_validating: false,
        }
    }
}

impl From<Host> for HostTarget {
    fn from(app: Host) -> Self {
        let Host { releases, .. } = app;
        HostTarget {
            releases: releases.into_iter().map(|(u, r)| (u, r.into())).collect(),
        }
    }
}

impl From<(Uuid, RemoteHostReleaseTarget)> for HostTarget {
    fn from((app_uuid, rel): (Uuid, RemoteHostReleaseTarget)) -> Self {
        let RemoteHostReleaseTarget {
            release_uuid,
            hostapp,
            overlays,
        } = rel;

        let overlays = overlays
            .into_iter()
            .map(|ov| {
                (
                    ov.name,
                    OverlayTarget {
                        image: ov.image,
                        // target: the overlay should be carried by the running kernel
                        status: OverlayStatus::Active,
                    },
                )
            })
            .collect();

        let mut releases = Map::new();
        releases.insert(
            release_uuid,
            HostReleaseTarget {
                app: app_uuid,
                hostapp: HostAppTarget {
                    image: hostapp.image,
                    build: hostapp.board_rev,
                    updater: hostapp.updater,
                },
                // the release should be running (target)
                status: HostReleaseStatus::Running,
                overlays,
            },
        );

        HostTarget { releases }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HostReleaseStatus {
    /// the release metadata has been written to disk and it should be installed next
    Created,
    /// the release has been installed and we are waiting for a reboot
    Installed,
    /// the release is currently running
    Running,
}

impl State for HostReleaseStatus {
    type Target = Self;
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OverlayStatus {
    /// The activation container ran and did not exit cleanly. An overlay that
    /// was never deployed has no entry at all, so this only means failure.
    Failed,
    /// Staged into its `ext_*` volume, waiting for the reboot that activates it
    Deployed,
    /// Carried by the running kernel
    Active,
}

impl State for OverlayStatus {
    type Target = Self;
}

/// A hostapp overlay extension, versioned together with its host release.
#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Overlay {
    pub image: ImageUri,

    pub status: OverlayStatus,
}

impl From<Overlay> for OverlayTarget {
    fn from(ov: Overlay) -> Self {
        let Overlay { image, status } = ov;
        OverlayTarget { image, status }
    }
}

/// The labels written on an overlay container at deploy time
pub(crate) fn overlay_labels(name: &str, image: &ImageUri) -> HashMap<String, String> {
    HashMap::from([
        (CLASS_LABEL.to_string(), CLASS_OVERLAY.to_string()),
        (SERVICE_NAME_LABEL.to_string(), name.to_string()),
        (IMAGE_LABEL.to_string(), image.as_str().to_string()),
    ])
}

/// Derive an overlay's status from its container's runtime state and the host
/// boot time.
/// Overlays are always currently reboot-activated, the `io.balena.update.requires-reboot`
/// label is reserved in the extension contract for future runtime activated
/// extensions, but no such mechanism exists yet.
///
/// Returns `None` for a container that was created but never started. That is
/// an interrupted deploy, not a failed activation: reporting no overlay lets
/// the create job run again, where `Failed` would trip the abort exception and
/// leave the release stuck on a deploy that never ran.
fn derive_overlay_status(state: &ContainerState, boot_time: SystemTime) -> Option<OverlayStatus> {
    if state.status == ContainerStatus::Created {
        return None;
    }

    // Deployed requires a clean one-shot exit (legacy: Exited && code 0 && no error).
    let deployed = state.status == ContainerStatus::Stopped
        && state.exit_code == Some(0)
        && state.error.as_deref().unwrap_or("").is_empty();
    if !deployed {
        return Some(OverlayStatus::Failed);
    }

    let created = state.created.as_system_time();
    // TODO: Derive overlay activation state from a deploy-time marker (a
    // per-overlay marker file written in tmpfs) instead of a timestamp.
    //
    // Strict `>` is deliberate: `/proc/stat` btime has 1-second granularity, so
    // an overlay staged in the same second as boot is treated as pre-boot
    // (Active). Do NOT relax to `>=`.
    let needs_activation_reboot = created > boot_time;
    if needs_activation_reboot {
        Some(OverlayStatus::Deployed)
    } else {
        Some(OverlayStatus::Active)
    }
}

/// Build the derived `Overlay` from an overlay container.
///
/// Returns `None` for anything that is not a helios-deployed overlay: a missing
/// service-name or image label, a name that resolves to no release, or a
/// container left behind by an interrupted deploy.
pub(crate) fn overlay_from_container(
    container: LocalContainer,
    boot_time: SystemTime,
) -> Option<(Uuid, String, Overlay)> {
    let labels = &container.config.labels;
    let name = labels.get(SERVICE_NAME_LABEL)?.clone();
    let image: ImageUri = labels.get(IMAGE_LABEL)?.parse().ok()?;
    // The release uuid is the container's namespace, composed into the name at
    // deploy time. Resolving it through the namespace rather than by splitting
    // the name also rejects a container whose name does not match its service
    // label, which is not one of ours.
    let release_uuid = Uuid::from(container.namespace(&name)?.as_str());

    let overlay = Overlay {
        image,
        status: derive_overlay_status(&container.state, boot_time)?,
    };
    Some((release_uuid, name, overlay))
}

/// The rootfs component of a host OS release
#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct HostApp {
    /// The fileset image
    /// This is needed for reporting and will be stored on local storage
    pub image: ImageUri,

    /// Build identifier.
    ///
    /// Used to compare the current/target core instances to avoid unnecessary downloads
    pub build: String,

    /// The updater artifact
    pub updater: ImageUri,

    /// How many installs have been attempted for this release
    #[mahler(internal)]
    pub install_attempts: usize,
}

impl From<HostApp> for HostAppTarget {
    fn from(app: HostApp) -> Self {
        let HostApp {
            image,
            build,
            updater,
            ..
        } = app;
        HostAppTarget {
            image,
            build,
            updater,
        }
    }
}

/// A host OS release: the rootfs component plus release-level state.
#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct HostRelease {
    /// The host app uuid
    ///
    /// There can only be one hostOS app runnning at a time, but the uuid
    /// may change when moving between compatible device types or between
    /// non-esr and esr
    pub app: Uuid,

    /// The rootfs component of the release
    pub hostapp: HostApp,

    /// The release is running/should be running
    pub status: HostReleaseStatus,

    pub overlays: Map<String, Overlay>,
}

impl From<HostRelease> for HostReleaseTarget {
    fn from(rel: HostRelease) -> Self {
        let HostRelease {
            app,
            hostapp,
            status,
            overlays,
            ..
        } = rel;
        HostReleaseTarget {
            app,
            hostapp: hostapp.into(),
            status,
            overlays: overlays.into_iter().map(|(k, v)| (k, v.into())).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

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
        assert_eq!(
            derive_overlay_status(&st, boot),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn derives_deployed_when_staged_after_boot() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500); // after boot
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, boot),
            Some(OverlayStatus::Deployed)
        );
    }

    #[test]
    fn derives_failed_on_nonzero_exit() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, Some(1), created);
        assert_eq!(
            derive_overlay_status(&st, boot),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn derives_active_when_staged_in_the_same_second_as_boot() {
        // /proc/stat btime has 1-second granularity, so an overlay staged in the
        // same second as boot cannot be distinguished from one staged just
        // before it. Treating equality as pre-boot (Active) is the safe answer.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let st = state(ContainerStatus::Stopped, Some(0), boot);
        assert_eq!(
            derive_overlay_status(&st, boot),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn derives_failed_when_the_container_is_still_running() {
        // An overlay is a one-shot. Anything still running never reached a clean
        // exit, so it has not been staged.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Running, None, created);
        assert_eq!(
            derive_overlay_status(&st, boot),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn derives_failed_without_an_exit_code() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, None, created);
        assert_eq!(
            derive_overlay_status(&st, boot),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn reports_no_status_for_a_container_that_never_started() {
        // helios dying between the create and start calls leaves the container
        // sitting in Created. Nothing failed to activate, so the overlay must
        // read as absent and be deployed again rather than aborting the release.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500);
        let st = state(ContainerStatus::Created, None, created);
        assert_eq!(derive_overlay_status(&st, boot), None);
    }

    #[test]
    fn derives_failed_when_the_engine_recorded_an_error() {
        // Exit 0 with an engine-recorded error is not a clean activation.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let mut st = state(ContainerStatus::Stopped, Some(0), created);
        st.error = Some("oci runtime error".to_string());
        assert_eq!(
            derive_overlay_status(&st, boot),
            Some(OverlayStatus::Failed)
        );
    }
}
