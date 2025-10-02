use mahler::State;
use serde::{Deserialize, Serialize};

use super::image::ImageUri;
use crate::remote_types::ServiceTarget as RemoteServiceTarget;

#[derive(State, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// Service ID on the remote backend
    pub id: u32,

    /// Service image URI
    pub image: ImageUri,
}

impl From<RemoteServiceTarget> for ServiceTarget {
    fn from(service: RemoteServiceTarget) -> Self {
        let RemoteServiceTarget { id, image, .. } = service;
        ServiceTarget {
            id,
            image: image.into(),
        }
    }
}
