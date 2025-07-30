use axum::http::Uri;
use serde::{Deserialize, Serialize};

use crate::types::ApiKey;
use crate::util::json::{deserialize_uri, serialize_uri};

/// Legacy Supervisor API configuration
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LegacyConfig {
    #[serde(deserialize_with = "deserialize_uri", serialize_with = "serialize_uri")]
    pub api_endpoint: Uri,
    pub api_key: ApiKey,
}
