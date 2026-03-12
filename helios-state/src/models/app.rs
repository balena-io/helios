use mahler::state::{Map, State};

use crate::common_types::Uuid;
use crate::remote_model::UserApp as RemoteAppTarget;

use super::release::{Release, ReleaseTarget};

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

impl From<(&Uuid, RemoteAppTarget)> for AppTarget {
    fn from((app_uuid, tgt): (&Uuid, RemoteAppTarget)) -> Self {
        let RemoteAppTarget { id, name, .. } = tgt;

        let mut releases: Map<Uuid, ReleaseTarget> = Map::new();

        // Namespace services/networks/volumes under the app_uuid
        for (rel_uuid, rel) in tgt.releases {
            let rel = releases.entry(rel_uuid.clone()).or_insert(rel.into());
            for (svc_name, svc) in rel.services.iter_mut() {
                svc.container_name = Some(format!("{svc_name}_{rel_uuid}"));
            }

            for (net_key, net) in rel.networks.iter_mut() {
                net.network_name = format!("{app_uuid}_{net_key}");
            }

            for (vol_key, vol) in rel.volumes.iter_mut() {
                vol.volume_name = format!("{app_uuid}_{vol_key}");
            }
        }

        AppTarget {
            id,
            name: Some(name),
            releases,
        }
    }
}
