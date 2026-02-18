use std::convert::Infallible;
use std::fmt::Display;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::common_types::Uuid;

use crate::models::defaults::UNKNOWN_RELEASE_UUID;

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
