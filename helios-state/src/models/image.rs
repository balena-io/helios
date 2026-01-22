use serde::{Deserialize, Serialize};

use mahler::state::Map;

use crate::oci::LocalImage;

/// Image state stored by the Worker. Differently from other models, Image doesn't
/// implement `mahler::State` because it is only used internally by the worker and image
/// pulls do not come via the target state.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Image {
    /// Container engine id
    pub engine_id: Option<String>,

    /// Image pull progress
    pub download_progress: i64,

    /// Image labels
    #[serde(default)]
    pub labels: Map<String, String>,
}

impl From<LocalImage> for Image {
    fn from(img: LocalImage) -> Self {
        Self {
            engine_id: Some(img.id),
            labels: img.labels.into_iter().collect(),
            download_progress: 100,
        }
    }
}
