use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::Uuid;

use super::service::{ServiceMap, TargetServiceMap};

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Release {
    #[serde(default)]
    pub services: ServiceMap,
}

pub type ReleaseMap = BTreeMap<Uuid, Release>;

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct TargetRelease {
    #[serde(default)]
    pub services: TargetServiceMap,
}

impl From<Release> for TargetRelease {
    fn from(r: Release) -> Self {
        let Release { services } = r;

        Self {
            services: services
                .into_iter()
                .map(|(name, svc)| (name, svc.into()))
                .collect(),
        }
    }
}

pub type TargetReleaseMap = BTreeMap<Uuid, TargetRelease>;
