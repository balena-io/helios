use mahler::state::{Map, State};
use serde::{Deserialize, Serialize};

use crate::common_types::{ImageUri, OperatingSystem, Uuid};
use crate::remote_model::HostRelease as RemoteHostReleaseTarget;

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
        } = rel;

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
}

impl From<HostRelease> for HostReleaseTarget {
    fn from(rel: HostRelease) -> Self {
        let HostRelease {
            app,
            hostapp,
            status,
        } = rel;
        HostReleaseTarget {
            app,
            hostapp: hostapp.into(),
            status,
        }
    }
}
