use serde::{Deserialize, Serialize};

use crate::types::ApiKey;
use crate::util::http::Uri;

/// Legacy Supervisor API configuration
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LegacyConfig {
    pub api_endpoint: Uri,
    pub api_key: ApiKey,
}
