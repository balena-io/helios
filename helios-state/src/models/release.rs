use mahler::state::{Map, State};

use crate::remote_model::Release as RemoteReleaseTarget;

use super::service::Service;

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Release {
    pub services: Map<String, Service>,
}

impl From<Release> for ReleaseTarget {
    fn from(rel: Release) -> Self {
        let Release { services } = rel;
        ReleaseTarget {
            services: services
                .into_iter()
                .map(|(svc_name, svc)| (svc_name, svc.into()))
                .collect(),
        }
    }
}

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
