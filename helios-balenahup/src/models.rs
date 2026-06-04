use std::collections::HashMap;

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
/// helios-private: the id of the boot the overlay's container was created in.
const BOOT_ID_LABEL: &str = "io.balena.private.boot-id";
/// helios-private: the runtime the composition asked for.
const RUNTIME_LABEL: &str = "io.balena.private.runtime";
/// `io.balena.image.kernel-abi-id` — the ABI id of the kernel an extension
/// ships.
const KERNEL_ABI_ID_LABEL: &str = "io.balena.image.kernel-abi-id";

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
}

impl Host {
    pub fn new(meta: OperatingSystem) -> Self {
        Host {
            meta,
            releases: Map::new(),
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
                        runtime: ov.runtime,
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

    /// The OCI runtime the composition names.
    pub runtime: String,
}

impl From<Overlay> for OverlayTarget {
    fn from(ov: Overlay) -> Self {
        let Overlay {
            image,
            status,
            runtime,
        } = ov;
        OverlayTarget {
            image,
            status,
            runtime,
        }
    }
}

/// The labels written on an overlay container at deploy time
pub(crate) fn overlay_labels(
    name: &str,
    image: &ImageUri,
    boot_id: &str,
    runtime: &str,
) -> HashMap<String, String> {
    HashMap::from([
        (CLASS_LABEL.to_string(), CLASS_OVERLAY.to_string()),
        (SERVICE_NAME_LABEL.to_string(), name.to_string()),
        (IMAGE_LABEL.to_string(), image.as_str().to_string()),
        (BOOT_ID_LABEL.to_string(), boot_id.to_string()),
        (RUNTIME_LABEL.to_string(), runtime.to_string()),
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

/// The boot an overlay's container was created in, if it was stamped.
fn deployed_boot_id(labels: &HashMap<String, String>) -> Option<&str> {
    labels
        .get(BOOT_ID_LABEL)
        .map(String::as_str)
        .filter(|id| !id.is_empty())
}

/// Derive an overlay's status from its container's runtime state, the boot it
/// was staged in, the running boot, the kernel ABI the overlay claims and the
/// one the running kernel was published under.
///
/// Returns `None` for a container the engine flagged `Dead`, and for one it
/// left in `Created` with no start behind it.
fn derive_overlay_status(
    state: &ContainerState,
    staged_boot_id: Option<&str>,
    current_boot_id: &str,
    claim: Option<&str>,
    running_abi: Option<&str>,
) -> Option<OverlayStatus> {
    // A Dead container is a removal that has done everything it can.
    if state.status == ContainerStatus::Dead {
        return None;
    }

    if state.status == ContainerStatus::Created {
        let start_failed = !state.error.as_deref().unwrap_or("").is_empty();
        // 127: command not found; 126: found but not executable. Both are the
        // activation image under a runtime that honours its command.
        if start_failed && matches!(state.exit_code, Some(126 | 127)) {
            return Some(OverlayStatus::Failed);
        }
        return None;
    }

    // Deployed requires a clean one-shot exit (legacy: Exited && code 0 && no error).
    let deployed = state.status == ContainerStatus::Stopped(0)
        && state.error.as_deref().unwrap_or("").is_empty();
    if !deployed {
        return Some(OverlayStatus::Failed);
    }

    // The root overlay is composed once, at boot, so a container created during
    // this boot is waiting for the reboot that will splice it in, whatever
    // kernel booted.
    if staged_boot_id == Some(current_boot_id) {
        return Some(OverlayStatus::Deployed);
    }

    match claim {
        Some(abi) if running_abi != Some(abi) => Some(OverlayStatus::Stale),
        // No claim, or a claim the running kernel honoured.
        _ => Some(OverlayStatus::Active),
    }
}

/// The runtime the composition asked for when the overlay was deployed, if it
/// asked for one. An empty label is no request, as with the boot id.
fn requested_runtime(labels: &HashMap<String, String>) -> Option<String> {
    labels
        .get(RUNTIME_LABEL)
        .filter(|name| !name.is_empty())
        .cloned()
}

/// Build the derived `Overlay` from an overlay container.
///
/// Returns `None` for anything that is not a helios-deployed overlay: a missing
/// service-name, image or runtime label, a name that resolves to no release, or
/// a container left behind by an interrupted deploy.
pub(crate) fn overlay_from_container(
    container: LocalContainer,
    current_boot_id: &str,
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
            deployed_boot_id(labels),
            current_boot_id,
            kernel_claim(labels),
            running_abi,
        )?,
        runtime: requested_runtime(labels)?,
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

    /// The boot the state read happens in.
    const THIS_BOOT: &str = "3f2b6c1a-8e4d-4a19-9c77-2f5a1b0d6e83";
    /// Any other boot: the device rebooted between the deploy and the read.
    const OTHER_BOOT: &str = "b7c1d0e2-5a63-4f18-8d20-91ac3e7b5f44";

    fn state(status: ContainerStatus, exit: Option<i64>) -> ContainerState {
        ContainerState {
            status,
            // an activation container declares no healthcheck
            health: Health::None,
            // The derivation does not read this, but the field has no
            // default, so the fixture still has to carry a value.
            created: crate::oci::DateTime::default(),
            error: None,
            exit_code: exit,
        }
    }

    #[test]
    fn derives_active_when_staged_in_an_earlier_boot() {
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(&st, Some(OTHER_BOOT), THIS_BOOT, None, None),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn derives_deployed_when_staged_in_this_boot() {
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), THIS_BOOT, None, None),
            Some(OverlayStatus::Deployed)
        );
    }

    #[test]
    fn an_unstamped_container_predates_this_boot() {
        // An older helios created it. helios ships in the rootfs, so a new
        // binary arrives with a host update and therefore a reboot.
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(&st, None, THIS_BOOT, None, None),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn an_unstamped_container_with_an_unhonoured_claim_is_stale() {
        // Predating this boot, the claim still has to hold against the kernel
        // that actually booted.
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(&st, None, THIS_BOOT, Some("abi_c"), None),
            Some(OverlayStatus::Stale)
        );
    }

    #[test]
    fn the_same_container_reads_deployed_then_active_across_a_boot() {
        // The transition the activation reboot performs. Nothing about the
        // container changes, only the boot it is read from, and no clock moves
        // in either direction.
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), THIS_BOOT, None, None),
            Some(OverlayStatus::Deployed)
        );
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), OTHER_BOOT, None, None),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn derives_failed_on_nonzero_exit() {
        let st = state(ContainerStatus::Stopped(1), None);
        assert_eq!(
            derive_overlay_status(&st, Some(OTHER_BOOT), THIS_BOOT, None, None),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn derives_failed_when_the_container_is_still_running() {
        // An overlay is a one-shot. Anything still running never reached a clean
        // exit, so it has not been staged.
        let st = state(ContainerStatus::Running, None);
        assert_eq!(
            derive_overlay_status(&st, Some(OTHER_BOOT), THIS_BOOT, None, None),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn reports_no_status_for_a_container_that_never_started() {
        // helios dying between the create and start calls leaves the container
        // sitting in Created. Nothing failed to activate, so the overlay must
        // read as absent and be deployed again rather than aborting the release.
        let st = state(ContainerStatus::Created, None);
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), THIS_BOOT, None, None),
            None
        );
    }

    #[test]
    fn reports_no_status_for_a_container_the_removal_left_dead() {
        // The engine flags Dead when it cannot release a layer the running root
        // pins, which is what a removal of a mounted extension gets. Reading it
        // as present would re-plan the removal on every seek until the reboot,
        // and reading it as Failed would freeze a release that re-enables the
        // same overlay.
        let st = state(ContainerStatus::Dead, Some(0));
        assert_eq!(
            derive_overlay_status(&st, Some(OTHER_BOOT), THIS_BOOT, None, None),
            None
        );
    }

    #[test]
    fn a_created_container_with_an_engine_error_is_still_no_overlay() {
        let mut st = state(ContainerStatus::Created, None);
        st.error = Some(
            "failed to create task for container: failed to create shim task: \
             OCI runtime create failed: no space left on device: unknown"
                .to_string(),
        );
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), THIS_BOOT, None, None),
            None
        );
    }

    #[test]
    fn an_interrupted_deploy_reports_no_status() {
        // Created and never started
        let st = state(ContainerStatus::Created, Some(0));
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), THIS_BOOT, None, None),
            None
        );
    }

    #[test]
    fn derives_failed_when_the_runtime_refused_the_start() {
        // An overlay image carries `CMD ["none"]`, inert under the extension
        // runtime and fatal under any runtime that honours it.
        let mut st = state(ContainerStatus::Created, Some(127));
        st.error = Some(
            "Error response from daemon: failed to create task for container: \
             failed to create shim task: OCI runtime create failed: runc create failed: \
             unable to start container process: error during container init: \
             exec: \"none\": executable file not found in $PATH: unknown"
                .to_string(),
        );
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), THIS_BOOT, None, None),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn derives_failed_when_the_runtime_could_not_run_the_command() {
        // The engine sets 126 for a command it found but could not execute,
        // which is as permanent a refusal as one it could not find.
        let mut st = state(ContainerStatus::Created, Some(126));
        st.error = Some(
            "Error response from daemon: failed to create task for container: \
             failed to create shim task: OCI runtime create failed: runc create failed: \
             unable to start container process: exec: \"none\": permission denied: unknown"
                .to_string(),
        );
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), THIS_BOOT, None, None),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn an_engine_side_start_failure_stays_retryable() {
        let mut st = state(ContainerStatus::Created, Some(1));
        st.error = Some(
            "failed to create task for container: failed to create shim task: \
             context deadline exceeded: unknown"
                .to_string(),
        );
        assert_eq!(
            derive_overlay_status(&st, Some(THIS_BOOT), THIS_BOOT, None, None),
            None
        );
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
    fn overlay_labels_stamp_the_boot_id() {
        assert_eq!(
            labels("extension").get(BOOT_ID_LABEL).map(String::as_str),
            Some(THIS_BOOT)
        );
    }

    #[test]
    fn reads_the_deploying_boot_from_the_container_labels() {
        let labels = HashMap::from([(BOOT_ID_LABEL.to_string(), THIS_BOOT.to_string())]);
        assert_eq!(deployed_boot_id(&labels), Some(THIS_BOOT));
    }

    #[test]
    fn a_container_without_the_label_names_no_boot() {
        let labels = HashMap::from([(SERVICE_NAME_LABEL.to_string(), "tracing".to_string())]);
        assert_eq!(deployed_boot_id(&labels), None);
    }

    #[test]
    fn an_empty_boot_id_label_names_no_boot() {
        // Must not compare equal to a current boot id that is somehow also
        // empty, which would read every overlay as staged this boot.
        let labels = HashMap::from([(BOOT_ID_LABEL.to_string(), String::new())]);
        assert_eq!(deployed_boot_id(&labels), None);
    }

    #[test]
    fn derives_active_when_the_running_kernel_came_from_the_overlay() {
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(
                &st,
                Some(OTHER_BOOT),
                THIS_BOOT,
                Some("abi_c"),
                Some("abi_c")
            ),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn derives_stale_when_the_boot_did_not_pick_up_the_claimed_kernel() {
        // The overlay was staged before this boot and the kernel that booted
        // came from somewhere else, so its arming never took effect.
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(
                &st,
                Some(OTHER_BOOT),
                THIS_BOOT,
                Some("abi_d"),
                Some("abi_b")
            ),
            Some(OverlayStatus::Stale)
        );
    }

    #[test]
    fn derives_stale_on_a_stock_kernel_boot() {
        // No token at all: the device lost its override. Same remedy.
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(&st, Some(OTHER_BOOT), THIS_BOOT, Some("abi_c"), None),
            Some(OverlayStatus::Stale)
        );
    }

    #[test]
    fn derives_deployed_when_the_claim_matches_but_the_container_was_staged_this_boot() {
        // Token equality proves the running kernel came from this overlay's
        // claim, not that this container's layers are in the live root: the root
        // is composed once at boot. A redeploy of the same km lands here, and it
        // still needs the reboot that recomposes.
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(
                &st,
                Some(THIS_BOOT),
                THIS_BOOT,
                Some("abi_c"),
                Some("abi_c")
            ),
            Some(OverlayStatus::Deployed)
        );
    }

    #[test]
    fn derives_deployed_when_a_differing_claim_was_staged_this_boot() {
        // Waiting for the reboot that will activate it: not stale.
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(
                &st,
                Some(THIS_BOOT),
                THIS_BOOT,
                Some("abi_d"),
                Some("abi_b")
            ),
            Some(OverlayStatus::Deployed)
        );
    }

    #[test]
    fn a_claimless_overlay_is_never_stale() {
        // `tracing` claims no kernel, so it cannot diverge from one however the
        // device booted. Its status stays the boot-epoch derivation.
        let st = state(ContainerStatus::Stopped(0), None);
        assert_eq!(
            derive_overlay_status(&st, Some(OTHER_BOOT), THIS_BOOT, None, Some("abi_b")),
            Some(OverlayStatus::Active)
        );
    }

    #[test]
    fn a_failed_activation_outranks_a_stale_claim() {
        // The container never exited cleanly, so nothing was armed to go stale.
        let st = state(ContainerStatus::Stopped(1), None);
        assert_eq!(
            derive_overlay_status(
                &st,
                Some(OTHER_BOOT),
                THIS_BOOT,
                Some("abi_d"),
                Some("abi_b")
            ),
            Some(OverlayStatus::Failed)
        );
    }

    #[test]
    fn derives_failed_when_the_engine_recorded_an_error() {
        // Exit 0 with an engine-recorded error is not a clean activation.
        let mut st = state(ContainerStatus::Stopped(0), None);
        st.error = Some("oci runtime error".to_string());
        assert_eq!(
            derive_overlay_status(&st, Some(OTHER_BOOT), THIS_BOOT, None, None),
            Some(OverlayStatus::Failed)
        );
    }

    fn labels(runtime: &str) -> HashMap<String, String> {
        overlay_labels(
            "kernel-modules",
            &ImageUri::from_static("registry2.balena-cloud.com/v2/abc123:latest"),
            THIS_BOOT,
            runtime,
        )
    }

    #[test]
    fn a_recorded_runtime_reads_back_as_requested() {
        // The engine's own report is never consulted, so a request for the
        // engine default is a request like any other.
        assert_eq!(requested_runtime(&labels("runc")), Some("runc".to_string()));
    }

    #[test]
    fn an_empty_runtime_label_reads_back_as_no_request() {
        // Same rule as the boot id: an empty label is not a claim.
        let mut labels = labels("extension");
        labels.insert(RUNTIME_LABEL.to_string(), String::new());
        assert_eq!(requested_runtime(&labels), None);
    }
}
