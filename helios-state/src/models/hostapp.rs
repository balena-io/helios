use std::collections::BTreeMap;

use mahler::State;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::common_types::{ImageUri, Uuid};
use crate::remote_types::HostAppTarget as RemoteHostAppTarget;

#[derive(State, Serialize, Deserialize, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct HostApp {
    /// The host app uuid
    ///
    /// The value will not be available on first boot (it's not provided by the host OS)
    /// so it will be read from local storage
    pub uuid: Uuid,

    /// The hostapp releases. While only one release is expected on the target state, the
    /// device may be in-between releases, in which case there may still be clean-up steps to
    /// perform.
    pub releases: BTreeMap<Uuid, HostAppRelease>,
}

impl From<HostApp> for HostAppTarget {
    fn from(app: HostApp) -> Self {
        let HostApp { uuid, releases, .. } = app;
        HostAppTarget {
            uuid,
            releases: releases.into_iter().map(|(u, r)| (u, r.into())).collect(),
        }
    }
}

#[derive(State, Serialize, Deserialize, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct HostAppRelease {
    /// The host app image
    ///
    /// This is needed for reporting and will be stored on local storage
    pub image: ImageUri,

    /// The board revision
    ///
    /// This is used for comparing with the target hostapp as the image digest cannot be used
    pub board_rev: String,

    /// How many installs have been attempted for this release
    #[serde(default)]
    #[mahler(internal)]
    pub install_attempts: usize,
}

#[derive(Error, Debug)]
#[error("invalid hostapp: ${0}")]
pub struct InvalidHostApp(&'static str);

impl From<(Uuid, RemoteHostAppTarget)> for HostAppTarget {
    fn from((app_uuid, app): (Uuid, RemoteHostAppTarget)) -> Self {
        let RemoteHostAppTarget {
            release_uuid,
            image,
            board_rev,
        } = app;

        let mut releases = BTreeMap::new();
        releases.insert(release_uuid, HostAppReleaseTarget { image, board_rev });

        HostAppTarget {
            uuid: app_uuid,
            releases,
        }
    }
}

impl From<HostAppRelease> for HostAppReleaseTarget {
    fn from(rel: HostAppRelease) -> Self {
        let HostAppRelease {
            image, board_rev, ..
        } = rel;
        HostAppReleaseTarget { image, board_rev }
    }
}
