use axum::http::Uri;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::util::json::{
    deserialize_duration_from_ms, deserialize_uri, serialize_duration_to_ms, serialize_uri,
};

/// Remote API configuration
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteConfig {
    #[serde(deserialize_with = "deserialize_uri", serialize_with = "serialize_uri")]
    pub api_endpoint: Uri,

    pub api_key: String,

    #[serde(default)]
    pub connection: ConnectionConfig,

    pub provisioning: ProvisioningConfig,
}

/// Initial values at the time of provisioning.
///
/// These should be updated if the device moves between fleets,
/// performs Supervisor or OS updates, or networking changes.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProvisioningConfig {
    pub fleet: u32, // FIXME: this should be fleet_uuid

    pub device_type: String,

    pub supervisor_version: Option<String>,

    pub os_name: Option<String>,
    pub os_variant: Option<String>,

    pub mac_address: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnectionConfig {
    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub request_timeout: Duration,

    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub poll_interval: Duration,

    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub min_interval: Duration,

    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub max_poll_jitter: Duration,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_millis(59_000),
            poll_interval: Duration::from_millis(900_000),
            min_interval: Duration::from_millis(10_000),
            max_poll_jitter: Duration::from_millis(60_000),
        }
    }
}
