use std::time::Duration;

use crate::fallback::{proxy_legacy, FallbackState};
use crate::target::{CurrentState, Device, UpdateRequest};

use axum::routing::get;
use axum::Json;
use axum::{
    body::{Body, Bytes},
    http::{Request, Response, StatusCode},
    routing::post,
    Router,
};
use tokio::net::TcpListener;
use tokio::sync::watch::Sender;
use tower_http::trace::TraceLayer;
use tracing::{
    debug_span,
    field::{display, Empty},
    info, instrument, Span,
};

/// Start the API
///
/// Receives a TCP listener already bound to the right address and port,
/// and a bunch of arguments to forward to request handlers.
#[instrument(name = "api", skip_all)]
pub async fn start(
    listener: TcpListener,
    update_request_tx: Sender<UpdateRequest>,
    current_state: CurrentState,
    fallback_state: FallbackState,
) {
    let api_span = Span::current();
    let app = Router::new()
        .route("/v3/ping", get(|| async { "OK" }))
        .route("/v3/device", get(move || get_state(current_state)))
        // Legacy routes
        .route(
            "/v1/update",
            post(move |body| trigger_update(update_request_tx, body)),
        )
        // Default to proxying requests if there is no handler
        .fallback(move |request| proxy_legacy(fallback_state, request))
        // Enable tracing
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(move |request: &Request<Body>| {
                    debug_span!(parent: &api_span, "request",
                        method = %request.method(),
                        uri = %request.uri().path(),
                        version = ?request.version(),
                        status = Empty,
                    )
                })
                .on_response(|response: &Response<Body>, _: Duration, span: &Span| {
                    span.record("status", display(response.status()));
                }),
        );

    info!("starting");

    // safe because `serve` will never return an error (or return at all).
    axum::serve(listener, app).await.unwrap();
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

    StatusCode::ACCEPTED
}

/// Handle `/v3/device` request
///
/// Returns the device state
async fn get_state(current_state: CurrentState) -> Json<Device> {
    let device = current_state.read().await;
    Json(device)
}
