use std::path::PathBuf;

use mahler::state::{Map, State};

use crate::common_types::Uuid;
use crate::remote_model::UserApp as RemoteAppTarget;

use super::release::Release;

/// The internal state of the app
#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct App {
    /// App id on the remote backend. This only exists for legacy reasons
    /// and should be removed at some point.
    pub id: u32,

    /// The app name provided by the backend
    pub name: Option<String>,

    /// The app locking state
    #[mahler(default)]
    pub locked: bool,

    /// Filesystem lock paths currently held by the app
    #[mahler(default, internal)]
    pub lockfiles: Vec<PathBuf>,

    /// App releases
    pub releases: Map<Uuid, Release>,
}

impl From<App> for AppTarget {
    fn from(app: App) -> Self {
        let App {
            id,
            name,
            locked,
            releases,
            ..
        } = app;
        AppTarget {
            id,
            name,
            locked,
            releases: releases
                .into_iter()
                .map(|(uuid, rel)| (uuid, rel.into()))
                .collect(),
        }
    }
}

pub type AppMap = Map<Uuid, App>;

impl From<RemoteAppTarget> for AppTarget {
    fn from(tgt: RemoteAppTarget) -> Self {
        let RemoteAppTarget { id, name, releases } = tgt;

        AppTarget {
            id,
            name: Some(name),
            locked: false,
            releases: releases
                .into_iter()
                .map(|(r_uuid, rel)| (r_uuid, rel.into()))
                .collect(),
        }
    }
}
