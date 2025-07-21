use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{Request, Response, StatusCode},
    routing::{get, post},
    Json, Router,
};
use std::time::Duration;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch::Sender;
use tower_http::trace::TraceLayer;
use tracing::{
    debug_span,
    field::{display, Empty},
    info, instrument, Span,
};

use crate::fallback::{proxy_legacy, FallbackState};
use crate::remote::PollRequest;
use crate::state::models::{App, Device, TargetApp, TargetDevice, TargetStatus, Uuid};
use crate::state::{CurrentState, SeekRequest, UpdateOpts};

pub enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

/// Start the API
///
/// Receives a TCP listener already bound to the right address and port,
/// and a bunch of arguments to forward to request handlers.
#[instrument(name = "api", skip_all)]
pub async fn start(
    listener: Listener,
    seek_request_tx: Sender<SeekRequest>,
    poll_request_tx: Sender<PollRequest>,
    current_state: CurrentState,
    fallback_state: FallbackState,
) {
    let api_span = Span::current();
    let target_device_tx = seek_request_tx.clone();
    let target_app_tx = seek_request_tx.clone();
    let app = Router::new()
        .route("/v3/ping", get(|| async { "OK" }))
        .route("/v3/status", get(target_status))
        .route("/v3/device", get(get_device_cur_state))
        .route(
            "/v3/device",
            post(move |query, apps| set_device_tgt_state(target_device_tx, query, apps)),
        )
        .route("/v3/device/apps/{uuid}", get(get_app_cur_state))
        .route(
            "/v3/device/apps/{uuid}",
            post(move |state, path, query, apps| {
                set_app_tgt_state(target_app_tx, state, path, query, apps)
            }),
        )
        // Legacy routes
        .route(
            "/v1/update",
            post(move |body| trigger_poll(poll_request_tx, body)),
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
        )
        .with_state(current_state);

    info!("ready");

    // safe because `serve` will never return an error (or return at all).
    match listener {
        Listener::Tcp(listener) => axum::serve(listener, app).await,
        Listener::Unix(listener) => axum::serve(listener, app).await,
    }
    .unwrap()
}

/// Handle `/v1/update` requests
///
/// This will trigger a fetch and an update to the API
async fn trigger_poll(poll_request_tx: Sender<PollRequest>, body: Bytes) -> StatusCode {
    let request = if body.is_empty() {
        // Empty payload, use defaults
        PollRequest::default()
    } else {
        let opts = serde_json::from_slice::<UpdateOpts>(&body).unwrap_or_default();

        // Create a poll request with reemit: true to tell the main loop
        // to re-apply the target even if it was modified
        PollRequest { opts, reemit: true }
    };

    if poll_request_tx.send(request).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }

    StatusCode::ACCEPTED
}

/// Handle `Get /v3/status` request
///
/// The request only returns the status of the local engine ignoring the status
/// of the legacy supervisor
async fn target_status(State(current_state): State<CurrentState>) -> Json<TargetStatus> {
    let status = current_state.status().await;
    Json(status)
}

/// Handle `GET /v3/device` request
///
/// Returns the device state
async fn get_device_cur_state(State(current_state): State<CurrentState>) -> Json<Device> {
    let device = current_state.read().await;
    Json(device)
}

/// Handle `POST /v3/device` request
async fn set_device_tgt_state(
    seek_request_tx: Sender<SeekRequest>,
    Query(opts): Query<UpdateOpts>,
    Json(target): Json<TargetDevice>,
) -> StatusCode {
    if seek_request_tx
        .send(SeekRequest {
            target,
            opts,
            raw_target: None,
        })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE;
    }

    StatusCode::ACCEPTED
}

/// Handle `GET /v3/device/apps/{uuid}`
async fn get_app_cur_state(
    State(current_state): State<CurrentState>,
    Path(app_uuid): Path<Uuid>,
) -> Result<Json<App>, StatusCode> {
    let device = current_state.read().await;
    if let Some(app) = device.apps.get(&app_uuid) {
        return Ok(Json(app.clone()));
    }

    Err(StatusCode::NOT_FOUND)
}

/// Handle `POST /v3/device/apps/{uuid}` request
///
/// Sets the target state for the device apps
async fn set_app_tgt_state(
    seek_request_tx: Sender<SeekRequest>,
    State(current_state): State<CurrentState>,
    Path(app_uuid): Path<Uuid>,
    Query(opts): Query<UpdateOpts>,
    Json(app): Json<TargetApp>,
) -> StatusCode {
    // Every endpoint to interact with the device state will be written in the same way:
    // - read the current state
    // - convert it to a target
    // - replace the relevant part of the target with the input
    // - send the full target to the channel
    let device = current_state.read().await;
    let mut target: TargetDevice = device.into();
    target.apps.insert(app_uuid, app);

    if seek_request_tx
        .send(SeekRequest {
            target,
            opts,
            raw_target: None,
        })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE;
    }

    StatusCode::ACCEPTED
}
