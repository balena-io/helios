use axum::http::Uri;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::config::StaticConfig;
use crate::types::{ApiKey, Uuid};
use crate::util::json::{
    deserialize_duration_from_ms, deserialize_uri, serialize_duration_to_ms, serialize_uri,
};

// IMPORTANT: be VERY careful making changes to these structs,
// namely ProvisioningConfig, RemoteConfig and RequestConfig.
// These structs are persisted to disk and failure to deserialize
// them will cause the device *to lose identity* and at best try to
// reprovision as a new device, or worse become invisible to remote.
// When making changes always consider how you'll migrate from an
// older version of this struct.

/// Remote API configuration
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteConfig {
    #[serde(deserialize_with = "deserialize_uri", serialize_with = "serialize_uri")]
    pub api_endpoint: Uri,
    pub api_key: ApiKey,
    pub request: RequestConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestConfig {
    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub timeout: Duration,

    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub poll_interval: Duration,

    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub poll_min_interval: Duration,

    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub poll_max_jitter: Duration,
}

impl Default for RequestConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(59_000),
            poll_interval: Duration::from_millis(900_000),
            poll_min_interval: Duration::from_millis(10_000),
            poll_max_jitter: Duration::from_millis(60_000),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProvisioningConfig {
    pub uuid: Uuid,

    pub fleet: u32, // FIXME: should be fleet_uuid
    pub device_type: String,

    pub supervisor_version: Option<String>,

    pub os_version: Option<String>,
    pub os_variant: Option<String>,
    pub mac_address: Option<String>,

    pub remote: RemoteConfig,
}

impl StaticConfig for ProvisioningConfig {
    fn name() -> String {
        "provisioning".to_owned()
    }
}

impl From<ProvisioningConfig> for RemoteConfig {
    fn from(value: ProvisioningConfig) -> Self {
        value.remote
    }
}
