use mahler::state::{Map, State};

use crate::oci::LocalImage;

#[derive(State, Debug, Clone)]
pub struct Image {
    /// Container engine id
    pub engine_id: String,

    /// Image labels
    pub labels: Map<String, String>,
}

impl From<ImageTarget> for Image {
    fn from(tgt: ImageTarget) -> Self {
        let ImageTarget { engine_id, labels } = tgt;
        Self { engine_id, labels }
    }
}

impl From<LocalImage> for Image {
    fn from(img: LocalImage) -> Self {
        Self {
            engine_id: img.id,
            labels: img.labels.into_iter().collect(),
        }
    }
}
