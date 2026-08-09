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
/// `io.balena.image.kernel-abi-id` — the ABI id of the kernel an extension
/// ships.
const KERNEL_ABI_ID_LABEL: &str = "io.balena.image.kernel-abi-id";

/// breadcrumb marking that an overlay was removed and the root overlay
/// composition is stale until a reboot. Written by `remove_overlay`, read
/// into `Host::pending_reboot`, cleared by the tmpfs on boot.
pub(crate) const OVERLAY_REBOOT_BREADCRUMB: &str = "overlay-reboot-breadcrumb";

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

    /// True while an overlay removal awaits the reboot that applies it.
    #[mahler(default)]
    pub pending_reboot: bool,

    /// Whether the running OS release is still on trial: the rollback
    /// validation has not finished, so the OS may yet roll back. This is a
    /// device-global condition (a single `rollback-health` unit), derived fresh
    /// on every read; a helios-issued reboot during the window would trigger
    /// the rollback, so all host work defers while it is set.
    #[mahler(default)]
    pub os_validating: bool,
}

impl Host {
    pub fn new(meta: OperatingSystem) -> Self {
        Host {
            meta,
            releases: Map::new(),
            pending_reboot: false,
            os_validating: false,
        }
    }
}

impl From<Host> for HostTarget {
    fn from(app: Host) -> Self {
        let Host { releases, .. } = app;
        HostTarget {
            releases: releases.into_iter().map(|(u, r)| (u, r.into())).collect(),
            // A target never asks for a reboot; the diff against a derived
            // `true` is what schedules one.
            pending_reboot: false,
            // Nor for a validation in flight
            os_validating: false,
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

        HostTarget {
            releases,
            // A remote target never asks for a reboot; the diff against a
            // derived `true` is what schedules one.
            pending_reboot: false,
            // Nor for a validation in flight
            os_validating: false,
        }
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
    /// Staged before the running boot, which then did not pick up the kernel it
    /// claims.
    Stale,
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

/// The ABI id of the kernel an overlay ships, if it ships one.
///
/// An empty label is treated as no claim: it must never compare equal to an
/// absent boot token, which would read a stock-kernel boot as having honoured
/// the claim.
fn kernel_claim(labels: &HashMap<String, String>) -> Option<&str> {
    labels
        .get(KERNEL_ABI_ID_LABEL)
        .map(String::as_str)
        .filter(|abi| !abi.is_empty())
}

/// Derive an overlay's status from its container's runtime state, the host boot
/// time, the kernel ABI the overlay claims and the one the running kernel was
/// published under.
/// Overlays are always currently reboot-activated, the `io.balena.update.requires-reboot`
/// label is reserved in the extension contract for future runtime activated
/// extensions, but no such mechanism exists yet.
///
/// Returns `None` for a container left in `Created`. The start never ran, so
/// the container says nothing about the extension: `Failed` here would match
/// the activation-failure exception and hold the release at an image that was
/// never deployed, while `None` lets the create job run again.
fn derive_overlay_status(
    state: &ContainerState,
    boot_time: SystemTime,
    claim: Option<&str>,
    running_abi: Option<&str>,
) -> Option<OverlayStatus> {
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

    // The root overlay is composed once, at boot, so a container staged after it
    // is waiting for the reboot that will splice it in, whatever kernel booted.
    //
    // Strict `>` is deliberate: `/proc/stat` btime has 1-second granularity, so
    // an overlay staged in the same second as boot is treated as pre-boot
    // (Active). Do NOT relax to `>=`.
    let created = state.created.as_system_time();
    if created > boot_time {
        return Some(OverlayStatus::Deployed);
    }

    match claim {
        Some(abi) if running_abi != Some(abi) => Some(OverlayStatus::Stale),
        // No claim, or a claim the running kernel honoured.
        _ => Some(OverlayStatus::Active),
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
    running_abi: Option<&str>,
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
        status: derive_overlay_status(
            &container.state,
            boot_time,
            kernel_claim(labels),
            running_abi,
        )?,
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
    use crate::oci::Health;
    use std::time::{Duration, SystemTime};

    fn state(status: ContainerStatus, exit: Option<i64>, created: SystemTime) -> ContainerState {
        ContainerState {
            status,
            // an activation container declares no healthcheck
            health: Health::None,
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
            derive_overlay_status(&st, boot, None, None),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn derives_deployed_when_staged_after_boot() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500); // after boot
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, boot, None, None),
            Some(OverlayStatus::Deployed)
        );
    }

    #[test]
    fn derives_failed_on_nonzero_exit() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, Some(1), created);
        assert_eq!(
            derive_overlay_status(&st, boot, None, None),
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
            derive_overlay_status(&st, boot, None, None),
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
            derive_overlay_status(&st, boot, None, None),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn derives_failed_without_an_exit_code() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, None, created);
        assert_eq!(
            derive_overlay_status(&st, boot, None, None),
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
        assert_eq!(derive_overlay_status(&st, boot, None, None), None);
    }

    #[test]
    fn a_created_container_with_an_engine_error_is_still_no_overlay() {
        // A start the engine could not carry out leaves an error on the
        // container, but says nothing about the extension: a refused
        // activation never lands here, because the runtime reports it as a
        // started container that exited non-zero. Reading this as Failed
        // would abandon a host update over a full disk or an unlucky restart.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500);
        let mut st = state(ContainerStatus::Created, None, created);
        st.error = Some(
            "failed to create task for container: failed to create shim task: \
             OCI runtime create failed: no space left on device: unknown"
                .to_string(),
        );
        assert_eq!(derive_overlay_status(&st, boot, None, None), None);
    }

    #[test]
    fn reads_the_kernel_claim_from_the_container_labels() {
        let labels = HashMap::from([(KERNEL_ABI_ID_LABEL.to_string(), "329ceda170ac".to_string())]);
        assert_eq!(kernel_claim(&labels), Some("329ceda170ac"));
    }

    #[test]
    fn an_extension_without_the_label_claims_no_kernel() {
        let labels = HashMap::from([(SERVICE_NAME_LABEL.to_string(), "tracing".to_string())]);
        assert_eq!(kernel_claim(&labels), None);
    }

    #[test]
    fn an_empty_kernel_claim_is_no_claim() {
        // An empty label must never compare equal to an absent boot token, which
        // would read a stock boot as a satisfied claim.
        let labels = HashMap::from([(KERNEL_ABI_ID_LABEL.to_string(), String::new())]);
        assert_eq!(kernel_claim(&labels), None);
    }

    #[test]
    fn derives_active_when_the_running_kernel_came_from_the_overlay() {
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, boot, Some("abi_c"), Some("abi_c")),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn derives_stale_when_the_boot_did_not_pick_up_the_claimed_kernel() {
        // The overlay was staged before this boot and the kernel that booted
        // came from somewhere else, so its arming never took effect.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, boot, Some("abi_d"), Some("abi_b")),
            Some(OverlayStatus::Stale)
        );
    }

    #[test]
    fn derives_stale_on_a_stock_kernel_boot() {
        // No token at all: the device lost its override. Same remedy.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, boot, Some("abi_c"), None),
            Some(OverlayStatus::Stale)
        );
    }

    #[test]
    fn derives_deployed_when_the_claim_matches_but_the_container_postdates_the_boot() {
        // Token equality proves the running kernel came from this overlay's
        // claim, not that this container's layers are in the live root: the root
        // is composed once at boot. A redeploy of the same km lands here, and it
        // still needs the reboot that recomposes.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500);
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, boot, Some("abi_c"), Some("abi_c")),
            Some(OverlayStatus::Deployed)
        );
    }

    #[test]
    fn derives_deployed_when_a_differing_claim_was_staged_this_boot() {
        // Waiting for the reboot that will activate it: not stale.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500);
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, boot, Some("abi_d"), Some("abi_b")),
            Some(OverlayStatus::Deployed)
        );
    }

    #[test]
    fn a_claimless_overlay_is_never_stale() {
        // `tracing` claims no kernel, so it cannot diverge from one however the
        // device booted. Its status stays the timestamp derivation.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, Some(0), created);
        assert_eq!(
            derive_overlay_status(&st, boot, None, Some("abi_b")),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn a_failed_activation_outranks_a_stale_claim() {
        // The container never exited cleanly, so nothing was armed to go stale.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let st = state(ContainerStatus::Stopped, Some(1), created);
        assert_eq!(
            derive_overlay_status(&st, boot, Some("abi_d"), Some("abi_b")),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn derives_failed_when_the_engine_recorded_an_error() {
        // Exit 0 with an engine-recorded error is not a clean activation.
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        let mut st = state(ContainerStatus::Stopped, Some(0), created);
        st.error = Some("oci runtime error".to_string());
        assert_eq!(
            derive_overlay_status(&st, boot, None, None),
            Some(OverlayStatus::Failed)
        );
    }
}
