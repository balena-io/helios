use mahler::State;
use serde::{Deserialize, Serialize};

use crate::oci::ImageUri;

#[derive(State, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// Service ID on the remote backend
    pub id: u32,

    /// Service image URI
    pub image: ImageUri,
}
