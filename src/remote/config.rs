use axum::http::Uri;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::state::models::Uuid;
use crate::util::json::{
    deserialize_duration_from_ms, deserialize_uri, serialize_duration_to_ms, serialize_uri,
};

/// Remote API configuration
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteConfig {
    #[serde(deserialize_with = "deserialize_uri", serialize_with = "serialize_uri")]
    pub api_endpoint: Uri,

    pub api_key: String,
    pub uuid: Uuid,

    pub request: RequestConfig,

    pub provisioning: Option<ProvisioningConfig>,
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
    pub provisioning_key: String,
    pub device_type: String,
    pub fleet: u32, // FIXME: this should be fleet_uuid

    pub supervisor_version: Option<String>,
    pub os_name: Option<String>,
    pub os_variant: Option<String>,
    pub mac_address: Option<String>,
}
