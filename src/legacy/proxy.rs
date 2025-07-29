use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{field, trace};

use crate::types::Uuid;
use crate::util::http::{Client, Uri};

use super::error::UpstreamError;

type LegacyTarget = serde_json::Value;

#[derive(Clone)]
pub struct ProxyConfig {
    pub legacy_uuid: Uuid,
    pub legacy_uri: Uri,
    pub remote_uri: Option<Uri>,

    https_client: Client,
}

impl ProxyConfig {
    pub fn new(legacy_uuid: Uuid, legacy_uri: Uri, remote_uri: Option<Uri>) -> Self {
        Self {
            legacy_uuid,
            legacy_uri,
            remote_uri,
            https_client: Client::default(),
        }
    }
}

/// The target state that the legacy Supervisor should receive
/// the next time it polls and we intercept/proxy the request.
///
/// This state is meant to be set by the main loop (see `/state/seek`),
/// in order to keep the legacy Supervisor up-to-date. `proxy()` only
/// reads this state.
#[derive(Clone)]
pub struct ProxyState(Arc<RwLock<Option<LegacyTarget>>>);

impl ProxyState {
    pub fn new(target: Option<LegacyTarget>) -> Self {
        Self(Arc::new(RwLock::new(target)))
    }

    pub async fn get(&self) -> Option<LegacyTarget> {
        let target = self.0.read().await;
        target.clone()
    }

    pub async fn set(&self, target_state: LegacyTarget) {
        let mut target = self.0.write().await;
        target.replace(target_state);
    }

    pub async fn clear(&self) {
        let mut target = self.0.write().await;
        target.take();
    }
}

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error(transparent)]
    Upstream(#[from] UpstreamError),

    #[error("Failed to stream target response: {0}")]
    Http(#[from] axum::http::Error),
}

impl ProxyError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Upstream(e) => match e {
                UpstreamError::Uri(_) => StatusCode::BAD_REQUEST,
                UpstreamError::Connection(_) => StatusCode::BAD_GATEWAY,
                UpstreamError::Json(_) => StatusCode::INTERNAL_SERVER_ERROR,
            },
            Self::Http(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn from_upstream<T>(err: T) -> Self
    where
        T: Into<UpstreamError>,
    {
        Self::Upstream(err.into())
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        (self.status_code(), self.to_string()).into_response()
    }
}

/// Proxy requests to/from legacy Supervisor to the relevant target
///
/// This is the legacy API behavior while the Supervisor migration is ongoing
pub async fn proxy(
    config: ProxyConfig,
    state: ProxyState,
    request: Request,
) -> Result<Response, ProxyError> {
    // Check if this is a target state request from the Supervisor to the API
    let is_supervisor_ua = request
        .headers()
        .get("user-agent")
        .and_then(|ua| ua.to_str().ok())
        .filter(|ua| ua.starts_with("Supervisor/"))
        .is_some();

    // Check if this is a request to the target state endpoint
    if is_supervisor_ua {
        let path = request.uri().path();
        let expected_path = format!("/device/v3/{}/state", config.legacy_uuid);

        if path == expected_path {
            // Try to serve from local cache
            let response = if let Some(target) = state.get().await {
                trace!("returning cached target state");
                // XXX:
                let response_body =
                    serde_json::to_string(&target).map_err(ProxyError::from_upstream)?;

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
        if let Some(ref remote_uri) = config.remote_uri {
            remote_uri
        } else {
            // No remote API available, return 503 with Retry-After header
            // tell the legacy Supervisor to retry in 10 minutes
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_static("600"));
            return Ok((StatusCode::SERVICE_UNAVAILABLE, headers).into_response());
        }
    } else {
        &config.legacy_uri
    };

    // Record the target address for the request
    trace!(
        "target" = field::display(target_endpoint),
        "proxying request"
    );

    // Build target URI
    let target_uri = Uri::from_parts(
        target_endpoint.clone(),
        request.uri().path(),
        request.uri().query(),
    )
    .map_err(ProxyError::from_upstream)?;

    // Use the request as-is, replacing the target uri and removing the host header
    let (mut parts, body) = request.into_parts();
    parts.uri = target_uri.into();
    parts.headers.remove("host");

    // Send the request
    let res = config
        .https_client
        .request(parts.method, &parts.uri.into(), |req| {
            Ok(req
                .headers(parts.headers)
                .body(reqwest::Body::wrap_stream(body.into_data_stream())))
        })
        .await
        .map_err(ProxyError::from_upstream)?;

    // Convert the response directly without buffering the body
    let mut response = Response::builder().status(res.status());
    *response.headers_mut().unwrap() = res.headers().clone();
    response
        .body(Body::from_stream(res.stream()))
        .map_err(ProxyError::Http)
}

#[cfg(test)]
mod tests {
    use axum::http;
    use mockito::Server;
    use serde_json::json;

    use super::*;

    type LegacyTarget = serde_json::Value;

    async fn create_test_state(
        legacy_uri: Uri,
        remote_uri: Uri,
        target: Option<LegacyTarget>,
    ) -> (ProxyConfig, ProxyState) {
        create_test_state_with_none_uris(legacy_uri, Some(remote_uri), target).await
    }

    async fn create_test_state_with_none_uris(
        legacy_uri: Uri,
        remote_uri: Option<Uri>,
        target: Option<LegacyTarget>,
    ) -> (ProxyConfig, ProxyState) {
        let config = ProxyConfig::new(
            "test-device-uuid".to_string().into(),
            legacy_uri,
            remote_uri,
        );
        let state = ProxyState::new(target);
        (config, state)
    }

    fn create_test_request(path: &str, user_agent: Option<&str>) -> Request {
        let mut builder = Request::builder().method(http::Method::GET).uri(path);

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
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let (config, state) = create_test_state(legacy_uri, remote_uri, None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        proxy(config, state, request).await.unwrap();

        remote_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_non_supervisor_user_agent_routes_to_legacy() {
        let mut server = Server::new_async().await;
        let legacy_mock = server
            .mock("GET", "/test")
            .with_status(200)
            .with_body("legacy response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = server.url().parse().unwrap();
        let (config, state) = create_test_state(legacy_uri, remote_uri, None).await;

        let request = create_test_request("/test", Some("CustomClient/1.0"));

        proxy(config, state, request).await.unwrap();

        legacy_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_no_user_agent_routes_to_legacy() {
        let mut server = Server::new_async().await;
        let legacy_mock = server
            .mock("GET", "/test")
            .with_status(200)
            .with_body("legacy response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = server.url().parse().unwrap();
        let (config, state) = create_test_state(legacy_uri, remote_uri, None).await;

        let request = create_test_request("/test", None);

        proxy(config, state, request).await.unwrap();

        legacy_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_partial_supervisor_user_agent_routes_to_legacy() {
        let mut server = Server::new_async().await;
        let legacy_mock = server
            .mock("GET", "/test")
            .with_status(200)
            .with_body("legacy response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = server.url().parse().unwrap();
        let (config, state) = create_test_state(legacy_uri, remote_uri, None).await;

        let request = create_test_request("/test", Some("MySupervisor/1.0"));

        proxy(config, state, request).await.unwrap();

        legacy_mock.assert_async().await;
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
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let (config, state) = create_test_state(legacy_uri, remote_uri, None).await;

        let request = create_test_request("/api/v1/test?param=value", Some("Supervisor/1.0.0"));

        proxy(config, state, request).await.unwrap();

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
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let (config, state) = create_test_state(legacy_uri, remote_uri, None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let response = proxy(config, state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_connection_error_handling() {
        let remote_uri: Uri = "http://localhost:1".parse().unwrap(); // Invalid port
        let legacy_uri: Uri = "http://localhost:2".parse().unwrap(); // Invalid port
        let (config, state) = create_test_state(legacy_uri, remote_uri, None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let result = proxy(config, state, request).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProxyError::Upstream(UpstreamError::Connection(_))
        ));
    }

    #[test]
    fn test_proxy_error_into_response_invalid_uri() {
        let mut parts = http::Uri::builder().build().unwrap().into_parts();
        parts.scheme = Some("invalid-scheme".parse().unwrap());
        parts.authority = Some("invalid-authority".parse().unwrap());
        let err = http::Uri::from_parts(parts).unwrap_err();

        let uri_error = ProxyError::Upstream(UpstreamError::Uri(err.into()));
        let response = uri_error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_target_state_interception_with_cached_state() {
        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let (config, state) =
            create_test_state(legacy_uri, remote_uri, Some(target_state.clone())).await;

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("Supervisor/1.0.0"),
        );

        let response = proxy(config, state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[tokio::test]
    async fn test_target_state_interception_without_cached_state() {
        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let (config, state) = create_test_state(legacy_uri, remote_uri, None).await;

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("Supervisor/1.0.0"),
        );

        let response = proxy(config, state, request).await.unwrap();
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
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let (config, state) = create_test_state(legacy_uri, remote_uri, Some(target_state)).await;

        let request = create_test_request("/device/v3/wrong-uuid/state", Some("Supervisor/1.0.0"));

        proxy(config, state, request).await.unwrap();

        remote_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_target_state_interception_non_supervisor_user_agent() {
        let mut server = Server::new_async().await;
        let legacy_mock = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .with_status(200)
            .with_body("legacy response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = server.url().parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let (config, state) = create_test_state(legacy_uri, remote_uri, Some(target_state)).await;

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("CustomClient/1.0"),
        );

        proxy(config, state, request).await.unwrap();

        legacy_mock.assert_async().await;
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
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let target_state = json!({"apps": {"test-app": {"status": "running"}}});
        let (config, state) = create_test_state(legacy_uri, remote_uri, Some(target_state)).await;

        let request =
            create_test_request("/device/v3/test-device-uuid/logs", Some("Supervisor/1.0.0"));

        proxy(config, state, request).await.unwrap();

        remote_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_supervisor_request_with_no_remote_uri() {
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let (config, state) = create_test_state_with_none_uris(legacy_uri, None, None).await;

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));

        let response = proxy(config, state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "600");
    }

    #[tokio::test]
    async fn test_target_state_interception_with_no_remote_uri() {
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let (config, state) = create_test_state_with_none_uris(legacy_uri, None, None).await;

        let request = create_test_request(
            "/device/v3/test-device-uuid/state",
            Some("Supervisor/1.0.0"),
        );

        let response = proxy(config, state, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "15");
    }
}
