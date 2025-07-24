use axum::http::Uri;
use serde::{Deserialize, Serialize};

use crate::util::json::{deserialize_optional_uri, serialize_optional_uri};

/// Fallback API configurations
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FallbackConfig {
    #[serde(
        deserialize_with = "deserialize_optional_uri",
        serialize_with = "serialize_optional_uri",
        default
    )]
    pub address: Option<Uri>,
    pub api_key: Option<String>,
}
