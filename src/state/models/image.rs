use std::collections::HashMap;

use bollard::secret::ImageInspect;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Image {
    /// Container engine id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_id: Option<String>,

    /// Image labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<HashMap<String, String>>,
}

impl From<ImageInspect> for Image {
    fn from(img: ImageInspect) -> Self {
        let ImageInspect { id, config, .. } = img;
        Self {
            engine_id: id,
            labels: config.and_then(|config| config.labels),
        }
    }
}
