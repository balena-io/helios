use serde::Deserialize;
use std::collections::HashMap;

use helios_util::types::ImageUri;

/// Target app as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct Service {
    pub id: u32,
    pub image: ImageUri,

    #[serde(default)]
    pub labels: HashMap<String, String>,

    #[serde(default)]
    pub composition: ServiceComposition,
}

// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug, Default)]
pub struct ServiceComposition {
    #[serde(default)]
    pub labels: HashMap<String, String>,
}
