mod proxy;

use crate::config::Config;
use proxy::{proxy, ProxyConfig};

use axum::{body::Body, Router};
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::info;

pub(super) type HttpsClient = Client<HttpsConnector<HttpConnector>, Body>;

#[derive(Clone)]
pub struct ApiState {
    /// Proxy configuration
    proxy: Arc<ProxyConfig>,

    /// Shared https client for remote connections
    https_client: HttpsClient,

    /// Device UUID
    uuid: String,

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
            target_state: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn set_target_state(&self, target: Value) {
        let mut state = self.target_state.write().await;
        *state = Some(target);
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

    pub fn get_state(&self) -> ApiState {
        self.state.clone()
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let app = Router::new()
            // TODO: intercept /v1/update and call the local planner
            // Default to proxying requests if there is no handler
            .fallback(proxy)
            .layer(TraceLayer::new_for_http())
            .with_state(self.state.clone());

        let listen_addr: SocketAddr = format!("0.0.0.0:{}", self.config.local.port).parse()?;

        // Try to bind to the local address
        let listener = TcpListener::bind(listen_addr).await?;
        info!("API Listening on {}", self.config.local.port);

        axum::serve(listener, app).await?;
        Ok(())
    }
}
