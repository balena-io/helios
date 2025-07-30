use axum::http::Uri;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::types::ApiKey;
use crate::util::json::{
    deserialize_duration_from_ms, deserialize_uri, serialize_duration_to_ms, serialize_uri,
};

/// Remote API configuration
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteConfig {
    #[serde(deserialize_with = "deserialize_uri", serialize_with = "serialize_uri")]
    pub api_endpoint: Uri,
    pub api_key: Option<ApiKey>,
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
