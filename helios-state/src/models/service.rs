use std::convert::Infallible;
use std::fmt::Display;
use std::str::FromStr;

use mahler::state::State;
use serde::{Deserialize, Serialize};

use crate::common_types::{ImageUri, Uuid};
use crate::remote_model::Service as RemoteServiceTarget;

// We don't want to fail if the service is supervised but it doesn't have an app-uuid,
// this could mean the container was tampered with or it is leftover from an old version of the
// supervisor.
pub const UNKNOWN_APP_UUID: &str = "10c401";

// We don't want to fail if the service is supervised but it has the wrong
// container name, that just means that we need to rename it (or remove it)
// so we use a fake release uuid for this.
const UNKNOWN_RELEASE_UUID: &str = "10ca12e1ea5e";

pub struct ServiceContainerName {
    pub service_name: String,
    pub release_uuid: Uuid,
}

impl Display for ServiceContainerName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}_{}", self.service_name, self.release_uuid)
    }
}

impl From<(String, Uuid)> for ServiceContainerName {
    fn from((service_name, release_uuid): (String, Uuid)) -> Self {
        Self {
            service_name,
            release_uuid,
        }
    }
}

impl Serialize for ServiceContainerName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_string().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ServiceContainerName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;
        let name: ServiceContainerName = s.parse().map_err(serde::de::Error::custom)?;
        Ok(name)
    }
}

impl FromStr for ServiceContainerName {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // the container name can never be an empty string, so we
        // panic here instead of returning an error as this is more likely
        // to be a bug
        assert!(!s.is_empty(), "container name cannot be empty");

        // the last component of the string should be the release uuid
        let parts: Vec<&str> = s.rsplitn(2, '_').collect();

        if parts.len() < 2 {
            return Ok(ServiceContainerName {
                service_name: s.to_owned(),
                release_uuid: UNKNOWN_RELEASE_UUID.into(),
            });
        }

        let mut release_uuid = parts[0].to_owned();
        let mut service_name = parts[1].to_owned();

        if service_name.is_empty() || release_uuid.is_empty() {
            service_name = s.to_owned();
            release_uuid = UNKNOWN_RELEASE_UUID.to_owned()
        }

        Ok(ServiceContainerName {
            service_name,
            release_uuid: release_uuid.into(),
        })
    }
}

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

#[derive(State, Debug, Clone)]
#[mahler(derive(PartialEq, Eq))]
pub struct Service {
    /// Service ID on the remote backend
    pub id: u32,

    /// Service image URI
    pub image: ImageRef,
}

impl From<Service> for ServiceTarget {
    fn from(svc: Service) -> Self {
        let Service { id, image } = svc;
        ServiceTarget { id, image }
    }
}

impl From<RemoteServiceTarget> for ServiceTarget {
    fn from(service: RemoteServiceTarget) -> Self {
        let RemoteServiceTarget { id, image, .. } = service;
        ServiceTarget {
            id,
            image: image.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_container_name_basic_parsing() {
        let name: ServiceContainerName = "myservice_abc123".parse().unwrap();
        assert_eq!(name.service_name, "myservice");
        assert_eq!(name.release_uuid, "abc123".into());
    }

    #[test]
    fn test_service_container_name_underscores() {
        let name: ServiceContainerName = "myservice_with_underscores_abc123".parse().unwrap();
        assert_eq!(name.service_name, "myservice_with_underscores");
        assert_eq!(name.release_uuid, "abc123".into());
    }

    #[test]
    fn test_service_container_name_empty_service_name() {
        let name: ServiceContainerName = "_abc123".parse().unwrap();
        assert_eq!(name.service_name, "_abc123");
        assert_eq!(name.release_uuid, UNKNOWN_RELEASE_UUID.into());
    }

    #[test]
    fn test_service_container_name_empty_release_uuid() {
        let name: ServiceContainerName = "myservice_".parse().unwrap();
        assert_eq!(name.service_name, "myservice_");
        assert_eq!(name.release_uuid, UNKNOWN_RELEASE_UUID.into());
    }

    #[test]
    fn test_service_container_name_no_underscore() {
        let name: ServiceContainerName = "myservice".parse().unwrap();
        assert_eq!(name.service_name, "myservice");
        assert_eq!(name.release_uuid, UNKNOWN_RELEASE_UUID.into());
    }

    #[test]
    #[should_panic(expected = "container name cannot be empty")]
    fn test_service_container_name_empty_string() {
        let _: ServiceContainerName = "".parse().unwrap();
    }

    #[test]
    fn test_service_container_name_single_underscore() {
        let name: ServiceContainerName = "_".parse().unwrap();
        assert_eq!(name.service_name, "_");
        assert_eq!(name.release_uuid, UNKNOWN_RELEASE_UUID.into());
    }
}
