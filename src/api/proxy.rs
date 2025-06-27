use super::ApiState;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use hyper::Uri;
use tracing::info;

pub struct ProxyConfig {
    pub remote_uri: Uri,
    pub fallback_uri: Option<Uri>,
}

async fn handle_target_state_request(state: &ApiState) -> Result<Response, ProxyError> {
    // Try to serve from local cache
    if let Some(target) = state.get_target_state().await {
        info!("returning cached target state");
        let response_body =
            serde_json::to_string(&target).map_err(ProxyError::JsonSerialization)?;

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
/// This is the fallback API behavior while we the supervisor migration is ongoing
pub async fn proxy(
    State(state): State<ApiState>,
    request: Request,
) -> Result<Response, ProxyError> {
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
            return handle_target_state_request(&state).await;
        }
    }

    // Default proxy behavior for non-target-state requests
    let target_endpoint = if is_supervisor_ua {
        Some(&state.proxy.remote_uri)
    } else {
        state.proxy.fallback_uri.as_ref()
    };

    let Some(target_endpoint) = target_endpoint else {
        // No fallback configured, return 404
        return Ok((StatusCode::NOT_FOUND).into_response());
    };

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
    JsonSerialization(serde_json::Error),
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
            ProxyError::JsonSerialization(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("JSON serialization failed: {e}"),
            ),
        };

        (status, message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use hyper_tls::HttpsConnector;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;
    use mockito::Server;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn create_test_state(remote_uri: Uri, fallback_uri: Uri) -> ApiState {
        let https = HttpsConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(https);

        ApiState {
            proxy: Arc::new(ProxyConfig {
                remote_uri,
                fallback_uri: Some(fallback_uri),
            }),
            https_client: client,
            uuid: "test-device-uuid".to_string(),
            target_state: Arc::new(RwLock::new(None)),
        }
    }

    fn create_test_state_with_target(
        remote_uri: Uri,
        fallback_uri: Uri,
        target: Option<serde_json::Value>,
    ) -> ApiState {
        let https = HttpsConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(https);

        ApiState {
            proxy: Arc::new(ProxyConfig {
                remote_uri,
                fallback_uri: Some(fallback_uri),
            }),
            https_client: client,
            uuid: "test-device-uuid".to_string(),
            target_state: Arc::new(RwLock::new(target)),
        }
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
        let state = create_test_state(remote_uri, fallback_uri);

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let result = proxy(State(state), request).await;
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
        let state = create_test_state(remote_uri, fallback_uri);

        let request = create_test_request("/test", Some("CustomClient/1.0"));

        let result = proxy(State(state), request).await;
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
        let state = create_test_state(remote_uri, fallback_uri);

        let request = create_test_request("/test", None);

        let result = proxy(State(state), request).await;
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
        let state = create_test_state(remote_uri, fallback_uri);

        let request = create_test_request("/test", Some("MySupervisor/1.0"));

        let result = proxy(State(state), request).await;
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
        let state = create_test_state(remote_uri, fallback_uri);

        let request = create_test_request("/api/v1/test?param=value", Some("Supervisor/1.0.0"));

        let result = proxy(State(state), request).await;
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
        let state = create_test_state(remote_uri, fallback_uri);

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let result = proxy(State(state), request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_connection_error_handling() {
        let remote_uri: Uri = "http://localhost:1".parse().unwrap(); // Invalid port
        let fallback_uri: Uri = "http://localhost:2".parse().unwrap(); // Invalid port
        let state = create_test_state(remote_uri, fallback_uri);

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let result = proxy(State(state), request).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProxyError::UpstreamConnection(_)
        ));
    }

    #[test]
    fn test_proxy_error_into_response_invalid_uri() {
        let mut parts = Uri::builder().build().unwrap().into_parts();
        parts.scheme = Some("invalid-scheme".parse().unwrap());
        parts.authority = Some("invalid-authority".parse().unwrap());

        let uri_error = ProxyError::InvalidUri(Uri::from_parts(parts).unwrap_err());
        let response = uri_error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_target_state_interception_with_cached_state() {
        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let fallback_uri: Uri = "http://localhost:9999".parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let state =
            create_test_state_with_target(remote_uri, fallback_uri, Some(target_state.clone()));

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("Supervisor/1.0.0"),
        );

        let result = proxy(State(state), request).await;
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
        let state = create_test_state_with_target(remote_uri, fallback_uri, None);

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("Supervisor/1.0.0"),
        );

        let result = proxy(State(state), request).await;
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
        let state = create_test_state_with_target(remote_uri, fallback_uri, Some(target_state));

        let request = create_test_request("/device/v3/wrong-uuid/state", Some("Supervisor/1.0.0"));

        let result = proxy(State(state), request).await;
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
        let state = create_test_state_with_target(remote_uri, fallback_uri, Some(target_state));

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("CustomClient/1.0"),
        );

        let result = proxy(State(state), request).await;
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
        let state = create_test_state_with_target(remote_uri, fallback_uri, Some(target_state));

        let request =
            create_test_request("/device/v3/test-device-uuid/logs", Some("Supervisor/1.0.0"));

        let result = proxy(State(state), request).await;
        assert!(result.is_ok());

        remote_mock.assert_async().await;
    }
}
