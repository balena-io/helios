use serde::Deserialize;
use std::collections::HashMap;

use crate::common_types::ImageUri;

use super::command::Command;
use super::labels::Labels;

/// Target service as defined by the remote backend
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
    pub command: Option<Command>,

    #[serde(default)]
    pub labels: Labels,
}
