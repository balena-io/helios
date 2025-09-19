use serde::{Deserialize, Serialize};

use crate::util::http::Uri;
use crate::util::types::ApiKey;

/// Legacy Supervisor API configuration
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LegacyConfig {
    pub api_endpoint: Uri,
    pub api_key: ApiKey,
}
