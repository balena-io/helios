use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, field, instrument, trace, warn};

use crate::util::uri::{make_uri, UriError};

#[derive(Debug, Deserialize)]
struct StateStatusResponse {
    #[serde(rename = "appState")]
    app_state: String,
}

pub type FallbackTarget = serde_json::Value;

#[derive(Clone)]
pub struct FallbackState {
    pub uuid: String,
    pub remote_uri: Option<Uri>,
    pub fallback_uri: Option<Uri>,
    pub https_client: reqwest::Client,
    pub target: Arc<RwLock<Option<FallbackTarget>>>,
}

impl FallbackState {
    pub fn new(uuid: String, remote_uri: Option<Uri>, fallback_uri: Option<Uri>) -> Self {
        Self {
            uuid,
            remote_uri,
            fallback_uri,
            target: Arc::new(RwLock::new(None)),
            https_client: reqwest::Client::new(),
        }
    }

    pub async fn target_state(&self) -> Option<FallbackTarget> {
        let target = self.target.read().await;
        target.clone()
    }

    pub async fn set_target_state(&self, target_state: FallbackTarget) {
        let mut target = self.target.write().await;
        target.replace(target_state);
    }

    pub async fn clear_target_state(&self) {
        let mut target = self.target.write().await;
        target.take();
    }
}

#[derive(Debug, Error)]
pub enum FallbackError {
    #[error("Invalid target URI: {0}")]
    InvalidUri(#[from] UriError),

    #[error("Target connection failed: {0}")]
    UpstreamConnection(#[from] reqwest::Error),

    #[error("Failed to stream target response: {0}")]
    UpstreamProxy(#[from] axum::http::Error),

    #[error("JSON serialization failed: {0}")]
    JsonSerialization(#[from] serde_json::Error),
}

impl FallbackError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidUri(_) => StatusCode::BAD_REQUEST,
            Self::UpstreamConnection(_) => StatusCode::BAD_GATEWAY,
            Self::UpstreamProxy(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::JsonSerialization(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for FallbackError {
    fn into_response(self) -> Response {
        (self.status_code(), self.to_string()).into_response()
    }
}

impl From<axum::http::uri::InvalidUri> for FallbackError {
    fn from(err: axum::http::uri::InvalidUri) -> Self {
        Self::InvalidUri(err.into())
    }
}

impl From<axum::http::uri::InvalidUriParts> for FallbackError {
    fn from(err: axum::http::uri::InvalidUriParts) -> Self {
        Self::InvalidUri(err.into())
    }
}

/// Trigger an update on the legacy supervisor
#[instrument(skip_all, err)]
pub async fn legacy_update(
    fallback_uri: Uri,
    fallback_key: Option<String>,
    force: bool,
    cancel: bool,
) -> Result<(), FallbackError> {
    let client = reqwest::Client::new();

    // Build the URI from the address parts
    let update_url = if let Some(apikey) = &fallback_key {
        make_uri(
            fallback_uri.clone(),
            "/v1/update",
            Some(format!("apikey={apikey}").as_str()),
        )
    } else {
        make_uri(fallback_uri.clone(), "/v1/update", None)
    };
    let update_url = update_url?.to_string();

    let payload = json!({
        "force": force,
        "cancel": cancel
    });

    debug!("calling fallback");
    let response = loop {
        match client.post(&update_url).json(&payload).send().await {
            Ok(res) if res.status().is_success() => break res,
            Ok(res) => warn!(
                response = field::display(res.status()),
                "received error response"
            ),
            Err(e) => warn!("failed: {e}, retrying in 5s"),
        };

        // Back-off for a bit in case the supervisor is restarting
        tokio::time::sleep(Duration::from_secs(5)).await;
    };
    debug!(response = field::display(response.status()), "success");

    // Build the status check URI
    let status_url = if let Some(apikey) = &fallback_key {
        make_uri(
            fallback_uri,
            "/v2/state/status",
            Some(&format!("apikey={apikey}")),
        )
    } else {
        make_uri(fallback_uri, "/v2/state/status", None)
    };
    let status_url = status_url?.to_string();

    // Poll the status endpoint until appState is 'applied'
    loop {
        trace!("waiting for the state to settle");
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        let status_response = client.get(&status_url).send().await?;

        if status_response.status().is_success() {
            let status: StateStatusResponse = status_response.json().await?;
            if status.app_state == "applied" {
                break;
            }
        }
    }

    Ok(())
}

/// Proxy requests to/from fallback supervisor to the relevant target
///
/// This is the fallback API behavior while the supervisor migration is ongoing
pub async fn proxy_legacy(
    state: FallbackState,
    request: Request,
) -> Result<Response, FallbackError> {
    // Check if this is a target state request from the supervisor to the API
    let is_supervisor_ua = request
        .headers()
        .get("user-agent")
        .and_then(|ua| ua.to_str().ok())
        .filter(|ua| ua.starts_with("Supervisor/"))
        .is_some();

    // Check if this is a request to the target state endpoint
    if is_supervisor_ua {
        let path = request.uri().path();
        let expected_path = format!("/device/v3/{}/state", state.uuid);

        if path == expected_path {
            // Try to serve from local cache
            let response = if let Some(target) = state.target_state().await {
                trace!("returning cached target state");
                // XXX:
                let response_body = serde_json::to_string(&target)?;

                let mut response = Response::new(Body::from(response_body));
                response
                    .headers_mut()
                    .insert("content-type", HeaderValue::from_static("application/json"));

                Ok(response)
            } else {
                trace!("no cached target state yet");
                // No target state available, return 503 with Retry-After header
                let mut headers = HeaderMap::new();
                headers.insert("retry-after", HeaderValue::from_static("15"));

                Ok((StatusCode::SERVICE_UNAVAILABLE, headers).into_response())
            };

            return response;
        }
    }

    // Default proxy behavior for non-target-state requests
    let target_endpoint = if is_supervisor_ua {
        if let Some(ref remote_uri) = state.remote_uri {
            remote_uri
        } else {
            // No remote API available, return 503 with Retry-After header
            // tell the fallback to retry in 10 minutes
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_static("600"));
            return Ok((StatusCode::SERVICE_UNAVAILABLE, headers).into_response());
        }
    } else if let Some(ref fallback_uri) = state.fallback_uri {
        fallback_uri
    } else {
        // No fallback configured, return 404
        return Ok((StatusCode::NOT_FOUND).into_response());
    };

    // Record the target address for the request
    trace!(
        "target" = field::display(target_endpoint),
        "proxying request"
    );

    // Build target URI
    let target_uri = make_uri(
        target_endpoint.clone(),
        request.uri().path(),
        request.uri().query(),
    )?;

    // Use the request as-is, replacing the target uri and removing the host header
    let (mut parts, body) = request.into_parts();
    parts.uri = target_uri;
    parts.headers.remove("host");

    // Send the request
    let res = state
        .https_client
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await?;

    // Convert the response directly without buffering the body
    let mut response = Response::builder().status(res.status());
    *response.headers_mut().unwrap() = res.headers().clone();
    response
        .body(Body::from_stream(res.bytes_stream()))
        .map_err(FallbackError::UpstreamProxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use mockito::Server;
    use serde_json::json;

    async fn create_test_state(
        remote_uri: Uri,
        fallback_uri: Uri,
        target: Option<FallbackTarget>,
    ) -> FallbackState {
        create_test_state_with_none_uris(Some(remote_uri), Some(fallback_uri), target).await
    }

    async fn create_test_state_with_none_uris(
        remote_uri: Option<Uri>,
        fallback_uri: Option<Uri>,
        target: Option<FallbackTarget>,
    ) -> FallbackState {
        let fallback_state =
            FallbackState::new("test-device-uuid".to_string(), remote_uri, fallback_uri);

        if let Some(tgt) = target {
            fallback_state.set_target_state(tgt).await;
        }

        fallback_state
    }

    fn create_test_request(path: &str, user_agent: Option<&str>) -> Request {
        let mut builder = Request::builder().method(Method::GET).uri(path);

        if let Some(ua) = user_agent {
            builder = builder.header("user-agent", ua);
        }

        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn test_supervisor_user_agent_routes_to_remote() {
        let mut server = Server::new_async().await;
        let remote_mock = server
            .mock("GET", "/test")
            .with_status(200)
            .with_body("remote response")
            .create_async()
            .await;

        let remote_uri: Uri = server.url().parse().unwrap();
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state(remote_uri, fallback_uri, None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        proxy_legacy(state, request).await.unwrap();

        remote_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_non_supervisor_user_agent_routes_to_fallback() {
        let mut server = Server::new_async().await;
        let fallback_mock = server
            .mock("GET", "/test")
            .with_status(200)
            .with_body("fallback response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let fallback_uri: Uri = server.url().parse().unwrap();
        let state = create_test_state(remote_uri, fallback_uri, None).await;

        let request = create_test_request("/test", Some("CustomClient/1.0"));

        proxy_legacy(state, request).await.unwrap();

        fallback_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_no_user_agent_routes_to_fallback() {
        let mut server = Server::new_async().await;
        let fallback_mock = server
            .mock("GET", "/test")
            .with_status(200)
            .with_body("fallback response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let fallback_uri: Uri = server.url().parse().unwrap();
        let state = create_test_state(remote_uri, fallback_uri, None).await;

        let request = create_test_request("/test", None);

        proxy_legacy(state, request).await.unwrap();

        fallback_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_partial_supervisor_user_agent_routes_to_fallback() {
        let mut server = Server::new_async().await;
        let fallback_mock = server
            .mock("GET", "/test")
            .with_status(200)
            .with_body("fallback response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let fallback_uri: Uri = server.url().parse().unwrap();
        let state = create_test_state(remote_uri, fallback_uri, None).await;

        let request = create_test_request("/test", Some("MySupervisor/1.0"));

        proxy_legacy(state, request).await.unwrap();

        fallback_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_path_and_query_forwarded() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/test?param=value")
            .with_status(200)
            .with_body("response")
            .create_async()
            .await;

        let remote_uri: Uri = server.url().parse().unwrap();
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state(remote_uri, fallback_uri, None).await;

        let request = create_test_request("/api/v1/test?param=value", Some("Supervisor/1.0.0"));

        proxy_legacy(state, request).await.unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_response_status_forwarded() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/test")
            .with_status(404)
            .with_body("not found")
            .create_async()
            .await;

        let remote_uri: Uri = server.url().parse().unwrap();
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state(remote_uri, fallback_uri, None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let response = proxy_legacy(state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_connection_error_handling() {
        let remote_uri: Uri = "http://localhost:1".parse().unwrap(); // Invalid port
        let fallback_uri: Uri = "http://localhost:2".parse().unwrap(); // Invalid port
        let state = create_test_state(remote_uri, fallback_uri, None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let result = proxy_legacy(state, request).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            FallbackError::UpstreamConnection(_)
        ));
    }

    #[test]
    fn test_proxy_error_into_response_invalid_uri() {
        let mut parts = Uri::builder().build().unwrap().into_parts();
        parts.scheme = Some("invalid-scheme".parse().unwrap());
        parts.authority = Some("invalid-authority".parse().unwrap());

        let uri_error = FallbackError::InvalidUri(Uri::from_parts(parts).unwrap_err().into());
        let response = uri_error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_target_state_interception_with_cached_state() {
        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let state = create_test_state(remote_uri, fallback_uri, Some(target_state.clone())).await;

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("Supervisor/1.0.0"),
        );

        let response = proxy_legacy(state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[tokio::test]
    async fn test_target_state_interception_without_cached_state() {
        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state(remote_uri, fallback_uri, None).await;

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("Supervisor/1.0.0"),
        );

        let response = proxy_legacy(state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "15");
    }

    #[tokio::test]
    async fn test_target_state_interception_wrong_uuid() {
        let mut server = Server::new_async().await;
        let remote_mock = server
            .mock("GET", "/device/v3/wrong-uuid/state")
            .with_status(200)
            .with_body("remote response")
            .create_async()
            .await;

        let remote_uri: Uri = server.url().parse().unwrap();
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let state = create_test_state(remote_uri, fallback_uri, Some(target_state)).await;

        let request = create_test_request("/device/v3/wrong-uuid/state", Some("Supervisor/1.0.0"));

        proxy_legacy(state, request).await.unwrap();

        remote_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_target_state_interception_non_supervisor_user_agent() {
        let mut server = Server::new_async().await;
        let fallback_mock = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .with_status(200)
            .with_body("fallback response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let fallback_uri: Uri = server.url().parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let state = create_test_state(remote_uri, fallback_uri, Some(target_state)).await;

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("CustomClient/1.0"),
        );

        proxy_legacy(state, request).await.unwrap();

        fallback_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_target_state_interception_different_endpoint() {
        let mut server = Server::new_async().await;
        let remote_mock = server
            .mock("GET", "/device/v3/test-device-uuid/logs")
            .with_status(200)
            .with_body("remote response")
            .create_async()
            .await;

        let remote_uri: Uri = server.url().parse().unwrap();
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let state = create_test_state(remote_uri, fallback_uri, Some(target_state)).await;

        let request =
            create_test_request("/device/v3/test-device-uuid/logs", Some("Supervisor/1.0.0"));

        proxy_legacy(state, request).await.unwrap();

        remote_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_supervisor_request_with_no_remote_uri() {
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state_with_none_uris(None, Some(fallback_uri), None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let response = proxy_legacy(state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "600");
    }

    #[tokio::test]
    async fn test_non_supervisor_request_with_no_fallback_uri() {
        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let state = create_test_state_with_none_uris(Some(remote_uri), None, None).await;

        let request = create_test_request("/test", Some("CustomClient/1.0"));

        let response = proxy_legacy(state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_target_state_interception_with_no_remote_uri() {
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state_with_none_uris(None, Some(fallback_uri), None).await;

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("Supervisor/1.0.0"),
        );

        let response = proxy_legacy(state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "15");
    }

    #[tokio::test]
    async fn test_both_remote_and_fallback_uris_none() {
        let state = create_test_state_with_none_uris(None, None, None).await;

        let supervisor_request = create_test_request("/test", Some("Supervisor/1.0.0"));
        let response = proxy_legacy(state.clone(), supervisor_request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "600");

        let non_supervisor_request = create_test_request("/test", Some("CustomClient/1.0"));
        let response = proxy_legacy(state, non_supervisor_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
