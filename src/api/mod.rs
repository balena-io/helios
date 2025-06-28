mod proxy;

use crate::config::Config;
use crate::link::{FetchOpts, UplinkService};
use proxy::{proxy, ProxyConfig};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::StatusCode,
    routing::post,
    Router,
};
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::info;

pub(super) type HttpsClient = Client<HttpsConnector<HttpConnector>, Body>;

#[derive(Deserialize)]
struct UpdateRequest {
    #[serde(default)]
    force: bool,
    #[serde(default)]
    cancel: bool,
}

// Handle /v1/update requests
//
// This will trigger a fetch to the API (unless unmanaged)
async fn handle_update(State(state): State<ApiState>, body: Bytes) -> StatusCode {
    if let Some(uplink) = state.uplink.as_ref() {
        let (force, cancel) = if body.is_empty() {
            // Empty payload, use defaults
            (false, false)
        } else {
            // Try to parse JSON
            match serde_json::from_slice::<UpdateRequest>(&body) {
                Ok(request) => (request.force, request.cancel),
                Err(_) => (false, false), // Invalid JSON, use defaults
            }
        };

        uplink.trigger_fetch(FetchOpts {
            reemit: true,
            force,
            cancel,
        });
        StatusCode::ACCEPTED
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

#[derive(Clone)]
struct ApiState {
    /// Proxy configuration
    proxy: Arc<ProxyConfig>,

    /// Shared https client for remote connections
    https_client: HttpsClient,

    /// Device UUID
    uuid: String,

    /// Uplink connection to the remote API
    uplink: Arc<Option<UplinkService>>,

    /// Cached target state from uplink service
    target_state: Arc<RwLock<Option<Value>>>,
}

impl ApiState {
    pub fn new(config: Config) -> Self {
        let https = HttpsConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(https);

        Self {
            proxy: Arc::new(ProxyConfig {
                fallback_uri: config.fallback.address,
                remote_uri: config.remote.api_endpoint,
            }),
            https_client: client,
            uuid: config.uuid,
            uplink: Arc::new(None),
            target_state: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn get_target_state(&self) -> Option<Value> {
        let state = self.target_state.read().await;
        state.clone()
    }
}

pub struct Api {
    config: Config,
    state: ApiState,
}

impl Api {
    pub fn new(config: Config) -> Self {
        let state = ApiState::new(config.clone());
        Self { config, state }
    }

    pub fn with_uplink(mut self, uplink: UplinkService) -> Self {
        self.state.uplink = Arc::new(Some(uplink));
        self
    }

    pub async fn set_target_state(&self, target: Value) {
        let mut state = self.state.target_state.write().await;
        *state = Some(target);
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/v1/update", post(handle_update))
            // Default to proxying requests if there is no handler
            .fallback(proxy)
            .layer(TraceLayer::new_for_http())
            .with_state(self.state.clone());

        let listen_addr: SocketAddr =
            format!("{}:{}", self.config.local.address, self.config.local.port).parse()?;

        // Try to bind to the local address
        let listener = TcpListener::bind(listen_addr).await?;
        info!("API Listening on {}", listen_addr);

        axum::serve(listener, app).await?;
        Ok(())
    }
}
