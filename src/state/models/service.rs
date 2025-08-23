use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::util::docker::ImageUri;

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

pub type ServiceMap = HashMap<String, Service>;
pub type TargetServiceMap = HashMap<String, TargetService>;
