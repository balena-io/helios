use mahler::State;
use serde::{Deserialize, Serialize};

use crate::common_types::ImageUri;
use crate::remote_types::ServiceTarget as RemoteServiceTarget;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// Service ID on the remote backend
    pub id: u32,

    /// Service image URI
    pub image: ImageUri,
}

impl State for Service {
    type Target = Self;
}

pub type ServiceTarget = Service;

impl From<RemoteServiceTarget> for ServiceTarget {
    fn from(service: RemoteServiceTarget) -> Self {
        let RemoteServiceTarget { id, image, .. } = service;
        ServiceTarget { id, image }
    }
}
