use std::collections::HashMap;

use mahler::State;
use serde::{Deserialize, Serialize};

use crate::oci::LocalImage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Image {
    /// Container engine id
    pub engine_id: String,

    /// Image labels
    #[serde(default)]
    pub labels: HashMap<String, String>,
}

impl State for Image {
    type Target = Self;
}

impl From<LocalImage> for Image {
    fn from(img: LocalImage) -> Self {
        Self {
            engine_id: img.id,
            labels: img.labels,
        }
    }
}
