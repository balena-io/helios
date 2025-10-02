use std::{collections::HashMap, ops::Deref};

use mahler::State;
use serde::{Deserialize, Serialize};

use crate::oci::LocalImage;

#[derive(State, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImageUri(crate::oci::ImageUri);

impl Deref for ImageUri {
    type Target = crate::oci::ImageUri;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<crate::oci::ImageUri> for ImageUri {
    fn from(img: crate::oci::ImageUri) -> Self {
        Self(img)
    }
}

impl From<ImageUri> for crate::oci::ImageUri {
    fn from(img: ImageUri) -> Self {
        img.0
    }
}

impl From<ImageUri> for String {
    fn from(uri: ImageUri) -> Self {
        uri.0.into()
    }
}

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
