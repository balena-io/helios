use mahler::state::{Map, State};
use serde::{Deserialize, Serialize};

use crate::common_types::{ImageUri, OperatingSystem, Uuid};
use crate::remote_model::HostApp as RemoteHostAppTarget;

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

impl From<(Uuid, RemoteHostAppTarget)> for HostTarget {
    fn from((app_uuid, app): (Uuid, RemoteHostAppTarget)) -> Self {
        let RemoteHostAppTarget {
            release_uuid,
            image,
            board_rev,
            updater,
            overlays,
        } = app;

        let overlays = overlays
            .into_iter()
            .map(|ov| {
                (
                    ov.name,
                    OverlayTarget {
                        image: ov.image,
                        class: ov.class,
                        requires_reboot: ov.requires_reboot,
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
                image,
                build: board_rev,
                updater,
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
    Absent,
    Deployed,
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

    pub class: String,

    pub requires_reboot: bool,

    pub status: OverlayStatus,
}

impl From<Overlay> for OverlayTarget {
    fn from(ov: Overlay) -> Self {
        let Overlay {
            image,
            class,
            requires_reboot,
            status,
        } = ov;
        OverlayTarget {
            image,
            class,
            requires_reboot,
            status,
        }
    }
}

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct HostRelease {
    /// The host app uuid
    ///
    /// There can only be one hostOS app runnning at a time, but the uuid
    /// may change when moving between compatible device types or between
    /// non-esr and esr
    pub app: Uuid,

    /// The fileset image
    /// This is needed for reporting and will be stored on local storage
    pub image: ImageUri,

    /// Build identifier.
    ///
    /// Used to compare the current/target core instances to avoid unnecessary downloads
    pub build: String,

    /// The updater artifact
    pub updater: ImageUri,

    /// The release is running/should be running
    pub status: HostReleaseStatus,

    pub overlays: Map<String, Overlay>,

    /// How many installs have been attempted for this release
    #[mahler(internal)]
    pub install_attempts: usize,

    #[mahler(internal, default)]
    pub hup_in_progress: bool,
}

impl From<HostRelease> for HostReleaseTarget {
    fn from(rel: HostRelease) -> Self {
        let HostRelease {
            app,
            image,
            build,
            updater,
            status,
            overlays,
            ..
        } = rel;
        HostReleaseTarget {
            app,
            image,
            build,
            updater,
            status,
            overlays: overlays.into_iter().map(|(k, v)| (k, v.into())).collect(),
        }
    }
}
