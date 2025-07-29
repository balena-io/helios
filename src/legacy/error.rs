use thiserror::Error;

use crate::util::http::{ClientError, InvalidUriError};

#[derive(Debug, Error)]
pub enum UpstreamError {
    #[error("Invalid target URI: {0}")]
    Uri(#[from] InvalidUriError),

    #[error("Target connection failed: {0}")]
    Connection(#[from] ClientError),

    #[error("JSON de/serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}
