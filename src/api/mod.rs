use crate::fallback::{proxy_legacy, FallbackState};
use crate::{FallbackTarget, UpdateRequest};

use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
use tokio::net::TcpListener;
use tokio::sync::watch::Sender;
use tower_http::trace::TraceLayer;
use tracing::info;

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
pub struct ApiState {
    // The fallback proxy state
    pub fallback_state: FallbackState,

    // Sender for target state requests
    pub target_state_tx: Sender<Option<FallbackTarget>>,

    // Sender for update requests
    pub update_request_tx: Sender<UpdateRequest>,
}

pub struct Api(ApiState);

impl Api {
    pub fn new(
        fallback_state: FallbackState,
        target_state_tx: Sender<Option<FallbackTarget>>,
        update_request_tx: Sender<UpdateRequest>,
    ) -> Self {
        Self(ApiState {
            fallback_state,
            target_state_tx,
            update_request_tx,
        })
    }

    /// Start the API
    ///
    /// Receives a TCP listener already bound to the right address and port
    pub async fn start(&self, listener: TcpListener) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/v1/update", post(trigger_update))
            // Default to proxying requests if there is no handler
            .fallback(proxy_legacy)
            .layer(TraceLayer::new_for_http())
            .with_state(self.0.clone());

        info!("API started");
        axum::serve(listener, app).await?;
        Ok(())
    }
}
