use std::sync::Arc;

use anyhow::Result;
use axum::{
    body::Body,
    extract::Request,
    http::{uri::PathAndQuery, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use hyper::Uri;
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::{client::legacy::connect::HttpConnector, rt::TokioExecutor};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::{debug, field, instrument, Span};

pub(super) type HttpsClient = Client<HttpsConnector<HttpConnector>, Body>;

use crate::config::{deserialize_optional_uri, serialize_optional_uri};
use crate::UpdateRequest;

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
/// Fallback API configurations
pub struct FallbackConfig {
    #[serde(
        deserialize_with = "deserialize_optional_uri",
        serialize_with = "serialize_optional_uri",
        default
    )]
    pub address: Option<Uri>,
    pub api_key: Option<String>,
}

pub type FallbackTarget = serde_json::Value;

#[derive(Clone)]
pub struct FallbackState {
    pub uuid: String,
    pub remote_uri: Option<Uri>,
    pub fallback_uri: Option<Uri>,
    pub https_client: HttpsClient,
    pub target: Arc<RwLock<Option<FallbackTarget>>>,
}

impl FallbackState {
    pub fn new(uuid: String, remote_uri: Option<Uri>, fallback_uri: Option<Uri>) -> Self {
        let https = HttpsConnector::new();
        let https_client = Client::builder(TokioExecutor::new()).build(https);

        Self {
            uuid,
            remote_uri,
            fallback_uri,
            target: Arc::new(RwLock::new(None)),
            https_client,
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
}

#[derive(Debug)]
pub enum FallbackError {
    InvalidUri(axum::http::uri::InvalidUriParts),
    UpstreamConnection(hyper_util::client::legacy::Error),
    JsonSerialization(serde_json::Error),
}

impl IntoResponse for FallbackError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            FallbackError::InvalidUri(e) => {
                (StatusCode::BAD_REQUEST, format!("Invalid target URI: {e}"))
            }
            FallbackError::UpstreamConnection(e) => (
                StatusCode::BAD_GATEWAY,
                format!("Target connection failed: {e}"),
            ),
            FallbackError::JsonSerialization(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("JSON serialization failed: {e}"),
            ),
        };

        (status, message).into_response()
    }
}

/// Trigger an update on the legacy supervisor
#[instrument(skip_all, fields(force = request.force, cancel = request.cancel, response = field::Empty), err)]
pub async fn trigger_legacy_update(
    fallback_uri: Option<Uri>,
    fallback_key: Option<String>,
    request: &UpdateRequest,
) -> Result<()> {
    let client = reqwest::Client::new();
    let fallback_address = if let Some(addr) = fallback_uri {
        addr
    } else {
        Span::current().record("response", 404);
        return Ok(());
    };

    // Build the URI from the address parts
    let mut addr_parts = fallback_address.into_parts();
    addr_parts.path_and_query = if let Some(apikey) = fallback_key.clone() {
        Some(PathAndQuery::from_maybe_shared(format!(
            "/v1/update?apikey={apikey}",
        ))?)
    } else {
        Some(PathAndQuery::from_maybe_shared("/v1/update")?)
    };
    let url = Uri::from_parts(addr_parts)?.to_string();

    let payload = json!({
        "force": request.force,
        "cancel": request.cancel
    });

    let response = client.post(&url).json(&payload).send().await?;

    Span::current().record("response", response.status().as_u16());

    Ok(())
}

async fn handle_target_state_request(state: &FallbackState) -> Result<Response, FallbackError> {
    // Try to serve from local cache
    if let Some(target) = state.target_state().await {
        debug!("returning cached target state");
        // XXX:
        let response_body =
            serde_json::to_string(&target).map_err(FallbackError::JsonSerialization)?;

        let mut response = Response::new(Body::from(response_body));
        response
            .headers_mut()
            .insert("content-type", HeaderValue::from_static("application/json"));

        Ok(response)
    } else {
        // No target state available, return 503 with Retry-After header
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("15"));

        let response = (StatusCode::SERVICE_UNAVAILABLE, headers).into_response();

        Ok(response)
    }
}

/// Proxy requests to/from fallback supervisor to the relevant target
///
/// This is the fallback API behavior while the supervisor migration is ongoing
pub async fn proxy_legacy(
    fallback_state: FallbackState,
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
        let expected_path = format!("/device/v3/{}/state", fallback_state.uuid);

        if path == expected_path {
            return handle_target_state_request(&fallback_state).await;
        }
    }

    // Default proxy behavior for non-target-state requests
    let target_endpoint = if is_supervisor_ua {
        if let Some(ref remote_uri) = fallback_state.remote_uri {
            remote_uri
        } else {
            // No remote API available, return 503 with Retry-After header
            // tell the fallback to retry in 10 minutes
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_static("600"));
            return Ok((StatusCode::SERVICE_UNAVAILABLE, headers).into_response());
        }
    } else if let Some(ref fallback_uri) = fallback_state.fallback_uri {
        fallback_uri
    } else {
        // No fallback configured, return 404
        return Ok((StatusCode::NOT_FOUND).into_response());
    };

    // Record the target address for the request
    debug!(
        "target" = field::display(target_endpoint),
        "proxying request to"
    );

    // Build target URI
    let mut target_parts = target_endpoint.clone().into_parts();
    target_parts.path_and_query = request.uri().path_and_query().cloned();
    let target_uri = Uri::from_parts(target_parts).map_err(FallbackError::InvalidUri)?;

    // Use the request as is replacing the target uri and remove the host header
    let (mut parts, body) = request.into_parts();
    parts.uri = target_uri;
    parts.headers.remove("host");

    // Build the proxied request
    let proxy_request = hyper::Request::from_parts(parts, body);

    // Send the request
    let response = fallback_state
        .https_client
        .request(proxy_request)
        .await
        .map_err(FallbackError::UpstreamConnection)?;

    // Convert the response directly without buffering the body
    let (parts, body) = response.into_parts();
    let response = Response::from_parts(parts, Body::new(body));

    Ok(response)
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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
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

        let uri_error = FallbackError::InvalidUri(Uri::from_parts(parts).unwrap_err());
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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

        remote_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_supervisor_request_with_no_remote_uri() {
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state_with_none_uris(None, Some(fallback_uri), None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "600");
    }

    #[tokio::test]
    async fn test_non_supervisor_request_with_no_fallback_uri() {
        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let state = create_test_state_with_none_uris(Some(remote_uri), None, None).await;

        let request = create_test_request("/test", Some("CustomClient/1.0"));

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
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

        let result = proxy_legacy(state, request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "15");
    }

    #[tokio::test]
    async fn test_both_remote_and_fallback_uris_none() {
        let state = create_test_state_with_none_uris(None, None, None).await;

        let supervisor_request = create_test_request("/test", Some("Supervisor/1.0.0"));
        let result = proxy_legacy(state.clone(), supervisor_request).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "600");

        let non_supervisor_request = create_test_request("/test", Some("CustomClient/1.0"));
        let result = proxy_legacy(state, non_supervisor_request).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
