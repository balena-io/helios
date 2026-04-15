use serde::{Deserialize, Serialize};

use mahler::state::State;

use crate::common_types::ImageUri;
use crate::oci::{ImageConfig, LocalImage};

/// An image reference is either a image URI or a content addressable image ID
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum ImageRef {
    /// A URI reference
    Uri(ImageUri),

    /// A content addressable image id
    Id(String),
}

impl State for ImageRef {
    type Target = Self;
}

impl ImageRef {
    /// Convenience method to get the digest of an image ref
    ///
    /// Returns None if the image is not a Uri or the Uri does not have a digest
    pub fn digest(&self) -> Option<&String> {
        if let Self::Uri(uri) = self {
            uri.digest()
        } else {
            None
        }
    }

    /// Get the reference as a &str
    pub fn as_str(&self) -> &str {
        match self {
            Self::Uri(uri) => uri.as_str(),
            Self::Id(id) => id.as_str(),
        }
    }

    /// Compare that both images refer to the same artifact regardless
    /// of the name
    ///
    /// Note that compares either by name or digest, it doesn't cover cases where `ubuntu:latest`
    /// is the same image as `ubuntu:24.04` unless they have digests. It also returns false if one
    /// image is an Id and the other an Uri, even though the Uri might have the content addressable
    /// id locally.
    pub fn is_same_artifact(&self, other: &ImageRef) -> bool {
        use ImageRef::*;
        match (self, other) {
            // if the image uris are not the same, they are the same artifact if they have the same digest
            (Uri(s_uri), Uri(o_uri)) => {
                s_uri == o_uri || (s_uri.digest().is_some() && s_uri.digest() == o_uri.digest())
            }
            (Id(s_id), Id(o_id)) => s_id == o_id,
            _ => false,
        }
    }
}

impl std::ops::Deref for ImageRef {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Uri(uri) => uri,
            Self::Id(id) => id,
        }
    }
}

impl<'de> Deserialize<'de> for ImageRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;
        if s.starts_with("sha256:") {
            return Ok(ImageRef::Id(s));
        }

        let uri: ImageUri = s.parse().map_err(serde::de::Error::custom)?;
        Ok(ImageRef::Uri(uri))
    }
}

impl From<ImageUri> for ImageRef {
    fn from(uri: ImageUri) -> Self {
        ImageRef::Uri(uri)
    }
}

/// Image state stored by the Worker. Differently from other models, Image doesn't
/// implement `mahler::State` because it is only used internally by the worker and image
/// pulls do not come via the target state.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Image {
    /// Container engine id
    pub oci_id: Option<String>,

    /// Image pull progress
    pub download_progress: u8,

    /// Image configuration
    #[serde(default)]
    pub config: ImageConfig,
}

impl From<LocalImage> for Image {
    fn from(img: LocalImage) -> Self {
        Self {
            oci_id: Some(img.id),
            config: img.config,
            download_progress: 100,
        }
    }
}
