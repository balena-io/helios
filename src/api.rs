use super::config::Config;

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info, instrument, Span};

type HttpClient = Client<HttpsConnector<HttpConnector>, Body>;

#[derive(Clone)]
struct ProxyState {
    config: Config,
    client: HttpClient,
}

impl ProxyState {
    pub fn new(config: Config) -> Self {
        let https = HttpsConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(https);

        Self { config, client }
    }
}

pub struct Api {
    config: Config,
}

impl Api {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let state = ProxyState::new(self.config.clone());
        let app = Router::new()
            // TODO: intercept /v1/update and call the local planner
            .route("/{*path}", any(proxy))
            .layer(TraceLayer::new_for_http())
            .with_state(state);

        let listener = TcpListener::bind(self.config.listen_addr).await?;
        info!("Listening on {}", self.config.listen_addr);
        info!("Target API URL: {}", self.config.balena_api_endpoint);
        info!(
            "Legacy Supervisor URL: {}",
            self.config.legacy_supervisor_url
        );

        axum::serve(listener, app).await?;
        Ok(())
    }
}

#[instrument(skip(state, request), fields(method, path, destination, source_type))]
async fn proxy(State(state): State<ProxyState>, request: Request) -> Result<Response, ProxyError> {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let headers = request.headers().clone();

    let path_and_query = uri
        .path_and_query()
        .map(|x| x.as_str())
        .unwrap_or("/")
        .to_string();

    let target_url = if let Some(user_agent) = headers.get("user-agent") {
        if let Ok(ua_str) = user_agent.to_str() {
            if ua_str.starts_with("Supervisor/") {
                &state.config.balena_api_endpoint
            } else {
                &state.config.legacy_supervisor_url
            }
        } else {
            &state.config.legacy_supervisor_url
        }
    } else {
        &state.config.legacy_supervisor_url
    };

    // Record the method, path, and routing information in the current span
    Span::current().record("method", method.as_str());
    Span::current().record("path", &path_and_query);
    Span::current().record("destination", target_url);

    debug!("Processing request");
    // Build target URI
    let target_uri = format!("{}{}", target_url, path_and_query)
        .parse::<hyper::Uri>()
        .map_err(|e| {
            error!("Invalid target URI: {}", e);
            ProxyError::InvalidUri
        })?;

    // Extract and potentially transform the request body
    let request_body = to_bytes(request.into_body(), 10 * 1024 * 1024)
        .await
        .map_err(|e| {
            error!("Failed to read request body: {}", e);
            ProxyError::BodyRead
        })?;

    // Build the proxied request
    let mut proxy_request = hyper::Request::builder()
        .method(&method)
        .uri(target_uri)
        .body(Body::from(request_body))
        .map_err(|e| {
            error!("Failed to build request: {}", e);
            ProxyError::RequestBuild
        })?;

    // Copy headers, excluding 'host'
    for (key, value) in &headers {
        if key != "host" {
            proxy_request.headers_mut().insert(key, value.clone());
        }
    }

    debug!("Sending request to target");
    // Send the request
    let response = state.client.request(proxy_request).await.map_err(|e| {
        error!("Failed to connect to target: {}", e);
        ProxyError::UpstreamConnection
    })?;

    let status = response.status();
    debug!("Received response with status: {}", status);
    debug!("Successfully proxied request");

    // Convert the response directly without buffering the body
    let (parts, body) = response.into_parts();
    let mut response_builder = Response::builder().status(parts.status);

    // Copy response headers
    for (key, value) in parts.headers {
        if let Some(name) = key {
            response_builder = response_builder.header(name, value);
        }
    }

    let response = response_builder.body(Body::new(body)).map_err(|e| {
        error!("Failed to build response: {}", e);
        ProxyError::ResponseBuild
    })?;

    Ok(response)
}

#[derive(Debug)]
enum ProxyError {
    InvalidUri,
    BodyRead,
    RequestBuild,
    UpstreamConnection,
    ResponseBuild,
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ProxyError::InvalidUri => (StatusCode::BAD_REQUEST, "Invalid target URI"),
            ProxyError::BodyRead => (StatusCode::BAD_REQUEST, "Failed to read request body"),
            ProxyError::RequestBuild => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Failed to build request")
            }
            ProxyError::UpstreamConnection => {
                (StatusCode::BAD_GATEWAY, "Failed to connect to target")
            }
            ProxyError::ResponseBuild => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build response",
            ),
        };

        (status, message).into_response()
    }
}
