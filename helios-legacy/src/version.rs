use serde::Deserialize;
use tracing::instrument;

use crate::util::http::Uri;
use crate::util::types::ApiKey;

use super::error::UpstreamError;

#[derive(Debug, Deserialize)]
struct VersionResponse {
    version: String,
}

/// Query the version of the legacy supervisor
#[instrument(skip_all, err)]
pub async fn version(
    legacy_api_endpoint: Uri,
    legacy_api_key: ApiKey,
) -> Result<String, UpstreamError> {
    let url = Uri::from_parts(legacy_api_endpoint, "/v2/version", None)?.to_string();

    let response = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {legacy_api_key}"))
        .send()
        .await?
        .error_for_status()?;

    let VersionResponse { version } = response.json().await?;
    Ok(version)
}
