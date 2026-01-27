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

    /// App releases
    pub releases: Map<Uuid, Release>,
}

impl From<App> for AppTarget {
    fn from(app: App) -> Self {
        let App { id, name, releases } = app;
        AppTarget {
            id,
            name,
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
        let RemoteAppTarget {
            id, name, releases, ..
        } = tgt;

        AppTarget {
            id,
            name: Some(name),
            releases: releases
                .into_iter()
                .map(|(uuid, rel)| (uuid, rel.into()))
                .collect(),
        }
    }
}
