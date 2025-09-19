use std::collections::HashMap;

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

impl From<LocalImage> for Image {
    fn from(img: LocalImage) -> Self {
        Self {
            engine_id: img.id,
            labels: img.labels,
        }
    }
}
