use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::oci::ImageUri;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Service {
    pub id: u32,
    pub image: ImageUri,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TargetService {
    /// Service ID on the remote backend
    #[serde(default)]
    pub id: u32,

    /// Service image URI
    pub image: ImageUri,
}

impl From<Service> for TargetService {
    fn from(s: Service) -> Self {
        let Service { id, image } = s;
        Self { id, image }
    }
}

pub type ServiceMap = BTreeMap<String, Service>;
pub type TargetServiceMap = BTreeMap<String, TargetService>;
