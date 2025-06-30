mod fallback;

use crate::config::Config;
use crate::{GlobalState, TargetState, UpdateRequest};
use fallback::proxy_legacy;

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
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::watch::Sender;
use tower_http::trace::TraceLayer;
use tracing::info;

pub(super) type HttpsClient = Client<HttpsConnector<HttpConnector>, Body>;

// Handle /v1/update requests
//
// This will trigger a fetch and an update to the API
async fn trigger_update(State(state): State<ApiState>, body: Bytes) -> StatusCode {
    let request = if body.is_empty() {
        // Empty payload, use defaults
        UpdateRequest::default()
    } else {
        // Try to parse JSON
        serde_json::from_slice::<UpdateRequest>(&body).unwrap_or_default()
    };

    if state.update_request_tx.send(request).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }

    // XXX: should we return something else if unmanaged?
    StatusCode::ACCEPTED
}

#[derive(Clone)]
#[allow(unused)]
struct ApiState {
    // Global config
    pub config: Config,

    // Shared Supervisor state
    pub global: GlobalState,

    // Sender for target state requests
    pub target_state_tx: Sender<Option<TargetState>>,

    // Sender for update requests
    pub update_request_tx: Sender<UpdateRequest>,

    /// Shared https client for remote connections
    pub https_client: HttpsClient,
}

pub struct Api(ApiState);

impl Api {
    pub fn new(
        config: Config,
        state: GlobalState,
        target_state_tx: Sender<Option<TargetState>>,
        update_request_tx: Sender<UpdateRequest>,
    ) -> Self {
        let https = HttpsConnector::new();
        let https_client = Client::builder(TokioExecutor::new()).build(https);

        Self(ApiState {
            config,
            https_client,
            global: state,
            target_state_tx,
            update_request_tx,
        })
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/v1/update", post(trigger_update))
            // Default to proxying requests if there is no handler
            .fallback(proxy_legacy)
            .layer(TraceLayer::new_for_http())
            .with_state(self.0.clone());

        let listen_addr: SocketAddr = format!(
            "{}:{}",
            self.0.config.local.address, self.0.config.local.port
        )
        .parse()?;

        // Try to bind to the local address
        let listener = TcpListener::bind(listen_addr).await?;
        info!("API Listening on {}", listen_addr);

        axum::serve(listener, app).await?;
        Ok(())
    }
}
