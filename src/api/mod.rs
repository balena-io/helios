use crate::fallback::{proxy_legacy, FallbackState};
use crate::UpdateRequest;

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use tokio::net::TcpListener;
use tokio::sync::watch::Sender;
use tower_http::trace::TraceLayer;
use tracing::info;

/// Start the API
///
/// Receives a TCP listener already bound to the right address and port,
/// and a bunch of arguments to forward to request handlers.
pub async fn start(
    listener: TcpListener,
    update_request_tx: Sender<UpdateRequest>,
    fallback_state: FallbackState,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route(
            "/v1/update",
            post(move |body| trigger_update(update_request_tx, body)),
        )
        // Default to proxying requests if there is no handler
        .fallback(move |request| proxy_legacy(fallback_state, request))
        // Enable tracing
        .layer(TraceLayer::new_for_http());

    info!("API started");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Handle `/v1/update` requests
///
/// This will trigger a fetch and an update to the API
async fn trigger_update(update_request_tx: Sender<UpdateRequest>, body: Bytes) -> StatusCode {
    let request = if body.is_empty() {
        // Empty payload, use defaults
        UpdateRequest::default()
    } else {
        // Try to parse JSON
        serde_json::from_slice::<UpdateRequest>(&body).unwrap_or_default()
    };

    if update_request_tx.send(request).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }

    // XXX: should we return something else if unmanaged?
    StatusCode::ACCEPTED
}
