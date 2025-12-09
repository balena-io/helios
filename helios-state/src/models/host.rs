use mahler::state::{Map, State};

use crate::common_types::{ImageUri, OperatingSystem, Uuid};
use crate::remote_types::HostAppTarget as RemoteHostAppTarget;

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
        } = app;

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
            },
        );

        HostTarget { releases }
    }
}

#[derive(State, Debug, Clone, PartialEq, Eq)]
pub enum HostReleaseStatus {
    Created,
    Installed,
    Running,
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

    /// How many installs have been attempted for this release
    #[mahler(internal)]
    pub install_attempts: usize,
}

impl From<HostRelease> for HostReleaseTarget {
    fn from(rel: HostRelease) -> Self {
        let HostRelease {
            app,
            image,
            build,
            updater,
            status,
            ..
        } = rel;
        HostReleaseTarget {
            app,
            image,
            build,
            updater,
            status,
        }
    }
}
