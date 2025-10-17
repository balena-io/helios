use std::collections::BTreeMap;

use mahler::State;
use serde::{Deserialize, Serialize};

use crate::common_types::{ImageUri, Uuid};
use crate::remote_types::HostAppTarget as RemoteHostAppTarget;

#[derive(State, Serialize, Deserialize, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Host {
    /// The host app uuid
    ///
    /// The value will not be available on first boot (it's not provided by the host OS)
    /// so it will be read from local storage
    pub uuid: Uuid,

    /// The hostapp releases. While only one release is expected on the target state, the
    /// device may be in-between releases, in which case there may still be clean-up steps to
    /// perform.
    pub releases: BTreeMap<Uuid, HostRelease>,
}

impl From<Host> for HostTarget {
    fn from(app: Host) -> Self {
        let Host { uuid, releases, .. } = app;
        HostTarget {
            uuid,
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

        let mut releases = BTreeMap::new();
        releases.insert(
            release_uuid,
            HostReleaseTarget {
                root: RootOSTarget {
                    image,
                    build: board_rev,
                    updater,
                },
            },
        );

        HostTarget {
            uuid: app_uuid,
            releases,
        }
    }
}

#[derive(State, Serialize, Deserialize, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct HostRelease {
    root: RootOS,
    // NOTE: here we'll add host extensions and perhaps other system services
}

impl From<HostRelease> for HostReleaseTarget {
    fn from(rel: HostRelease) -> Self {
        let HostRelease { root } = rel;
        HostReleaseTarget { root: root.into() }
    }
}

/// Root fileset for the host: kernel, essential drivers and services. Updated via HUP
#[derive(State, Serialize, Deserialize, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct RootOS {
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
    #[serde(default)]
    #[mahler(internal)]
    pub install_attempts: usize,
}

impl From<RootOS> for RootOSTarget {
    fn from(rel: RootOS) -> Self {
        let RootOS {
            image,
            build,
            updater,
            ..
        } = rel;
        RootOSTarget {
            image,
            build,
            updater,
        }
    }
}
