use thiserror::Error;

use crate::util::uri::UriError;

#[derive(Debug, Error)]
pub enum UpstreamError {
    #[error("Invalid target URI: {0}")]
    Uri(#[from] UriError),

    #[error("Target connection failed: {0}")]
    Connection(#[from] reqwest::Error),

    #[error("JSON de/serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}
