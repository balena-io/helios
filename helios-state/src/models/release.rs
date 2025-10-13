use mahler::State;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::remote_types::ReleaseTarget as RemoteReleaseTarget;

use super::service::Service;

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct Release {
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
}

impl State for Release {
    type Target = Self;
}

pub type ReleaseTarget = Release;

impl From<RemoteReleaseTarget> for ReleaseTarget {
    fn from(tgt: RemoteReleaseTarget) -> Self {
        let RemoteReleaseTarget { services, .. } = tgt;
        ReleaseTarget {
            services: services
                .into_iter()
                .map(|(svc_name, svc)| (svc_name, svc.into()))
                .collect(),
        }
    }
}
