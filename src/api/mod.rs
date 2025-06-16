mod proxy;

use crate::config::Config;
use proxy::proxy_legacy;

use axum::{body::Body, Router};
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;

pub(super) type HttpsClient = Client<HttpsConnector<HttpConnector>, Body>;

#[derive(Clone)]
struct ApiState {
    /// Global supervisor configuration
    global_config: Config,

    /// Shared https client for remote connections
    https_client: HttpsClient,
}

impl ApiState {
    pub fn new(config: Config) -> Self {
        let https = HttpsConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(https);

        Self {
            global_config: config,
            https_client: client,
        }
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
        let state = ApiState::new(self.config.clone());
        let app = Router::new()
            // TODO: intercept /v1/update and call the local planner
            // Default to proxying requests if there is no handler
            .fallback(proxy_legacy)
            .layer(TraceLayer::new_for_http())
            .with_state(state);

        let listen_addr: SocketAddr = format!("0.0.0.0:{}", self.config.local_api_port).parse()?;

        // Try to bind to the local address
        let listener = TcpListener::bind(listen_addr).await?;
        info!("API Listening on {}", self.config.local_api_port);

        axum::serve(listener, app).await?;
        Ok(())
    }
}
