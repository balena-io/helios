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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use hyper_util::client::legacy::Client;
    use hyper_tls::HttpsConnector;
    use hyper_util::rt::TokioExecutor;
    use mockito::Server;
    use std::sync::Arc;

    fn create_test_state(remote_uri: Uri, legacy_uri: Uri) -> ApiState {
        let https = HttpsConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(https);
        
        ApiState {
            proxy: Arc::new(ProxyConfig {
                remote_uri,
                legacy_uri,
            }),
            https_client: client,
        }
    }

    fn create_test_request(path: &str, user_agent: Option<&str>) -> Request {
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(path);
        
        if let Some(ua) = user_agent {
            builder = builder.header("user-agent", ua);
        }
        
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn test_supervisor_user_agent_routes_to_remote() {
        let mut server = Server::new_async().await;
        let remote_mock = server.mock("GET", "/test")
            .with_status(200)
            .with_body("remote response")
            .create_async()
            .await;

        let remote_uri: Uri = server.url().parse().unwrap();
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state(remote_uri, legacy_uri);

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));
        
        let result = proxy_legacy(State(state), request).await;
        assert!(result.is_ok());
        
        remote_mock.assert_async().await;
    }

    #[tokio::test] 
    async fn test_non_supervisor_user_agent_routes_to_legacy() {
        let mut server = Server::new_async().await;
        let legacy_mock = server.mock("GET", "/test")
            .with_status(200)
            .with_body("legacy response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = server.url().parse().unwrap();
        let state = create_test_state(remote_uri, legacy_uri);

        let request = create_test_request("/test", Some("CustomClient/1.0"));
        
        let result = proxy_legacy(State(state), request).await;
        assert!(result.is_ok());
        
        legacy_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_no_user_agent_routes_to_legacy() {
        let mut server = Server::new_async().await;
        let legacy_mock = server.mock("GET", "/test")
            .with_status(200)
            .with_body("legacy response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = server.url().parse().unwrap();
        let state = create_test_state(remote_uri, legacy_uri);

        let request = create_test_request("/test", None);
        
        let result = proxy_legacy(State(state), request).await;
        assert!(result.is_ok());
        
        legacy_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_partial_supervisor_user_agent_routes_to_legacy() {
        let mut server = Server::new_async().await;
        let legacy_mock = server.mock("GET", "/test")
            .with_status(200)
            .with_body("legacy response")
            .create_async()
            .await;

        let remote_uri: Uri = "http://localhost:9998".parse().unwrap();
        let legacy_uri: Uri = server.url().parse().unwrap();
        let state = create_test_state(remote_uri, legacy_uri);

        let request = create_test_request("/test", Some("MySupervisor/1.0"));
        
        let result = proxy_legacy(State(state), request).await;
        assert!(result.is_ok());
        
        legacy_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_path_and_query_forwarded() {
        let mut server = Server::new_async().await;
        let mock = server.mock("GET", "/api/v1/test?param=value")
            .with_status(200)
            .with_body("response")
            .create_async()
            .await;

        let remote_uri: Uri = server.url().parse().unwrap();
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state(remote_uri, legacy_uri);

        let request = create_test_request("/api/v1/test?param=value", Some("Supervisor/1.0.0"));
        
        let result = proxy_legacy(State(state), request).await;
        assert!(result.is_ok());
        
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_response_status_forwarded() {
        let mut server = Server::new_async().await;
        let mock = server.mock("GET", "/test")
            .with_status(404)
            .with_body("not found")
            .create_async()
            .await;

        let remote_uri: Uri = server.url().parse().unwrap();
        let legacy_uri: Uri = "http://localhost:9999".parse().unwrap();
        let state = create_test_state(remote_uri, legacy_uri);

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));
        
        let result = proxy_legacy(State(state), request).await;
        assert!(result.is_ok());
        
        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_connection_error_handling() {
        let remote_uri: Uri = "http://localhost:1".parse().unwrap(); // Invalid port
        let legacy_uri: Uri = "http://localhost:2".parse().unwrap(); // Invalid port
        let state = create_test_state(remote_uri, legacy_uri);

        let request = create_test_request("/test", Some("Supervisor/1.0.0"));
        
        let result = proxy_legacy(State(state), request).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProxyError::UpstreamConnection(_)));
    }

    #[test]
    fn test_proxy_error_into_response_invalid_uri() {
        let mut parts = Uri::builder().build().unwrap().into_parts();
        parts.scheme = Some("invalid-scheme".parse().unwrap());
        parts.authority = Some("invalid-authority".parse().unwrap());
        
        let uri_error = ProxyError::InvalidUri(
            Uri::from_parts(parts).unwrap_err()
        );
        let response = uri_error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
