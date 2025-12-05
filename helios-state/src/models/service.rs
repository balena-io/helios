use mahler::state::State;

use crate::common_types::ImageUri;
use crate::remote_types::ServiceTarget as RemoteServiceTarget;

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Service {
    /// Service ID on the remote backend
    pub id: u32,

    /// Service image URI
    pub image: ImageUri,
}

impl From<Service> for ServiceTarget {
    fn from(svc: Service) -> Self {
        let Service { id, image } = svc;
        ServiceTarget { id, image }
    }
}

impl From<RemoteServiceTarget> for ServiceTarget {
    fn from(service: RemoteServiceTarget) -> Self {
        let RemoteServiceTarget { id, image, .. } = service;
        ServiceTarget { id, image }
    }
}
