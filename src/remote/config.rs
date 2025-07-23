use axum::http::Uri;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::util::json::{
    deserialize_duration_from_ms, deserialize_optional_uri, serialize_duration_to_ms,
    serialize_optional_uri,
};

/// Remote API configurations
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteConfig {
    #[serde(
        deserialize_with = "deserialize_optional_uri",
        serialize_with = "serialize_optional_uri",
        default
    )]
    pub api_endpoint: Option<Uri>,
    pub api_key: Option<String>,
    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub poll_interval: Duration,
    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub request_timeout: Duration,
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

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            api_endpoint: None,
            api_key: None,
            poll_interval: Duration::from_millis(900000),
            request_timeout: Duration::from_millis(59000),
            min_interval: Duration::from_millis(10000),
            max_poll_jitter: Duration::from_millis(60000),
        }
    }
}
