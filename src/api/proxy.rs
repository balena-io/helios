use super::ApiState;

use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use hyper::Uri;
use tracing::info;

pub struct ProxyConfig {
    pub remote_uri: Uri,
    pub legacy_uri: Uri,
}

/// Proxy requests to/from legacy supervisor to the relevant target
///
/// This is the fallback API behavior while we the supervisor migration is ongoing
pub async fn proxy_legacy(
    State(state): State<ApiState>,
    request: Request,
) -> Result<Response, ProxyError> {
    // Figure out the target URI, assuming that
    // requests with `Supervisor/<version>` user-agent are going to the API
    // TODO: we should figure out a more robust mechanism as this could cause issues if the
    // user app uses the same user agent prefix
    let target_endpoint = request
        .headers()
        .get("user-agent")
        .and_then(|ua| ua.to_str().ok())
        .filter(|ua| ua.starts_with("Supervisor/"))
        .map(|_| &state.proxy.remote_uri)
        .unwrap_or(&state.proxy.legacy_uri);

    // Record the target address for the request
    info!(
        "target" = target_endpoint.to_string(),
        "proxying request to"
    );

    // Build target URI
    let mut target_parts = target_endpoint.clone().into_parts();
    target_parts.path_and_query = request.uri().path_and_query().cloned();
    let target_uri = Uri::from_parts(target_parts).map_err(ProxyError::InvalidUri)?;

    // Use the request as is replacing the target uri and remove the host header
    let (mut parts, body) = request.into_parts();
    parts.uri = target_uri;
    parts.headers.remove("host");

    // Build the proxied request
    let proxy_request = hyper::Request::from_parts(parts, body);

    // Send the request
    let response = state
        .https_client
        .request(proxy_request)
        .await
        .map_err(ProxyError::UpstreamConnection)?;

    // Convert the response directly without buffering the body
    let (parts, body) = response.into_parts();
    let response = Response::from_parts(parts, Body::new(body));

    Ok(response)
}

#[derive(Debug)]
pub enum ProxyError {
    InvalidUri(axum::http::uri::InvalidUriParts),
    UpstreamConnection(hyper_util::client::legacy::Error),
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ProxyError::InvalidUri(e) => {
                (StatusCode::BAD_REQUEST, format!("Invalid target URI: {e}"))
            }
            ProxyError::UpstreamConnection(e) => (
                StatusCode::BAD_GATEWAY,
                format!("Target connection failed: {e}"),
            ),
        };

        (status, message).into_response()
    }
}
