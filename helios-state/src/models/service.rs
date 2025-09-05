use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt::Display;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::oci::ImageUri;
use crate::util::types::Uuid;

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
        let s = s.trim_start_matches('/');

        assert!(!s.is_empty(), "container name cannot be empty");
        let parts: Vec<&str> = s.split('_').collect();

        if parts.len() < 2 {
            return Ok(ServiceContainerName {
                service_name: s.to_owned(),
                release_uuid: UNKNOWN_RELEASE_UUID.into(),
            });
        }

        let mut service_name = parts[0].to_owned();
        let mut release_uuid = parts[parts.len() - 1].to_owned();

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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Service {
    pub id: u32,
    pub image: ImageUri,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TargetService {
    /// Service ID on the remote backend
    #[serde(default)]
    pub id: u32,

    /// Service image URI
    pub image: ImageUri,
}

impl From<Service> for TargetService {
    fn from(s: Service) -> Self {
        let Service { id, image } = s;
        Self { id, image }
    }
}

pub type ServiceMap = BTreeMap<String, Service>;
pub type TargetServiceMap = BTreeMap<String, TargetService>;

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
    fn test_service_container_name_with_variables() {
        let name: ServiceContainerName = "myservice_var1_var2_abc123".parse().unwrap();
        assert_eq!(name.service_name, "myservice");
        assert_eq!(name.release_uuid, "abc123".into());
    }

    #[test]
    fn test_service_container_name_with_many_variables() {
        let name: ServiceContainerName =
            "myservice_var1_var2_var3_var4_var5_abc123".parse().unwrap();
        assert_eq!(name.service_name, "myservice");
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

    #[test]
    fn test_service_container_name_with_leading_slash() {
        let name: ServiceContainerName = "/myservice_abc123".parse().unwrap();
        assert_eq!(name.service_name, "myservice");
        assert_eq!(name.release_uuid, "abc123".into());
    }

    #[test]
    fn test_service_container_name_with_leading_slash_fallback() {
        let name: ServiceContainerName = "/myservice".parse().unwrap();
        assert_eq!(name.service_name, "myservice");
        assert_eq!(name.release_uuid, UNKNOWN_RELEASE_UUID.into());
    }
}
