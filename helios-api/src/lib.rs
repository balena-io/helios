use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{Request, Response, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};
use std::net::{AddrParseError, IpAddr, Ipv4Addr, SocketAddr};
use std::path;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::watch::Sender;
use tokio::{
    net::{TcpListener, UnixListener},
    sync::watch::Receiver,
};
use tower_http::trace::TraceLayer;
use tracing::{
    Span, debug_span,
    field::{Empty, display},
    info, instrument,
};

use helios_legacy::{ProxyConfig, ProxyState, proxy};
use helios_remote::PollRequest;
use helios_state::models::{App, Device, TargetApp, TargetDevice};
use helios_state::{LocalState, SeekRequest, UpdateOpts, UpdateStatus};
use helios_util::types::Uuid;

pub enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

/// Local API listen address
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum LocalAddress {
    Tcp(SocketAddr),
    Unix(path::PathBuf),
}

/// Helios API configuration
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ApiConfig {
    pub local_address: LocalAddress,
}

impl Display for LocalAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalAddress::Tcp(socket_addr) => socket_addr.fmt(f),
            LocalAddress::Unix(path) => path.as_path().display().fmt(f),
        }
    }
}

impl FromStr for LocalAddress {
    type Err = AddrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<SocketAddr>()
            .map(LocalAddress::Tcp)
            .or_else(|_| Ok(LocalAddress::Unix(path::Path::new(s).to_path_buf())))
    }
}

impl Default for LocalAddress {
    fn default() -> Self {
        LocalAddress::Tcp(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            48484,
        ))
    }
}

type LocalStateRx = Receiver<LocalState>;

/// Start the API
///
/// Receives a TCP listener already bound to the right address and port,
/// and a bunch of arguments to forward to request handlers.
#[instrument(name = "api", skip_all)]
pub async fn start(
    listener: Listener,
    seek_request_tx: Sender<SeekRequest>,
    poll_request_tx: Sender<PollRequest>,
    state_rx: LocalStateRx,
    proxy_config: Option<ProxyConfig>,
    proxy_state: Option<ProxyState>,
) {
    let api_span = Span::current();
    let target_app_tx = seek_request_tx.clone();
    let app = Router::new()
        .route("/v3/ping", get(|| async { "OK" }))
        .route("/v3/status", get(update_status))
        .route("/v3/device", get(get_device_cur_state))
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
        );

    // Default to proxying requests if there is no handler
    let app = match (proxy_config, proxy_state) {
        (Some(proxy_config), Some(proxy_state)) => {
            app.fallback(move |request| proxy(proxy_config, proxy_state, request))
        }
        _ => app,
    };

    // Enable tracing
    let app = app.layer(
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

    // Assign state
    let app = app.with_state(state_rx);

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
async fn update_status(State(state_rx): State<LocalStateRx>) -> Json<UpdateStatus> {
    let state = state_rx.borrow();
    Json(state.status.clone())
}

/// Handle `GET /v3/device` request
///
/// Returns the device state
async fn get_device_cur_state(State(state_rx): State<LocalStateRx>) -> Json<Device> {
    let state = state_rx.borrow();
    Json(state.device.clone())
}

/// Handle `GET /v3/device/apps/{uuid}`
async fn get_app_cur_state(
    State(state_rx): State<LocalStateRx>,
    Path(app_uuid): Path<Uuid>,
) -> Result<Json<App>, StatusCode> {
    let state = state_rx.borrow();
    let device = state.device.clone();
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
    State(state_rx): State<LocalStateRx>,
    Path(app_uuid): Path<Uuid>,
    Query(opts): Query<UpdateOpts>,
    Json(app): Json<TargetApp>,
) -> StatusCode {
    // Every endpoint to interact with the device state will be written in the same way:
    // - read the current state
    // - convert it to a target
    // - replace the relevant part of the target with the input
    // - send the full target to the channel
    let state = state_rx.borrow();
    let device = state.device.clone();
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::sync::watch;

    async fn setup_test_server() -> (
        u16,
        watch::Receiver<PollRequest>,
        watch::Receiver<SeekRequest>,
    ) {
        let (seek_request_tx, seek_rx) = watch::channel(SeekRequest {
            target: TargetDevice::default(),
            opts: UpdateOpts::default(),
            raw_target: None,
        });
        let (poll_request_tx, poll_rx) = watch::channel(PollRequest::default());
        let device = Device::new(Uuid::default(), "balenaOS 6.3.1".parse().ok());
        let local_state = LocalState {
            device,
            status: UpdateStatus::default(),
        };
        let (_, state_rx) = watch::channel(local_state);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(start(
            Listener::Tcp(listener),
            seek_request_tx,
            poll_request_tx,
            state_rx,
            None,
            None,
        ));

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        (port, poll_rx, seek_rx)
    }

    #[tokio::test]
    async fn test_v1_update_empty_body() {
        let (port, mut poll_rx, _) = setup_test_server().await;
        let client = reqwest::Client::new();

        let response = client
            .post(format!("http://127.0.0.1:{port}/v1/update"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 202);

        assert!(poll_rx.changed().await.is_ok());
        let poll_request = poll_rx.borrow().clone();
        assert!(!poll_request.opts.force);
        assert!(poll_request.opts.cancel); // Default is true
        assert!(poll_request.reemit); // The state should be reemited by default
    }

    #[tokio::test]
    async fn test_v1_update_with_body() {
        let (port, mut poll_rx, _) = setup_test_server().await;
        let client = reqwest::Client::new();

        let body = json!({"force": true});
        let response = client
            .post(format!("http://127.0.0.1:{port}/v1/update"))
            .json(&body)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 202);

        assert!(poll_rx.changed().await.is_ok());
        let poll_request = poll_rx.borrow().clone();
        assert!(poll_request.opts.force);
        assert!(!poll_request.opts.cancel); // API default via serde is false
        assert!(poll_request.reemit);
    }

    #[tokio::test]
    async fn test_set_app_without_query_params() {
        let (port, _, mut seek_rx) = setup_test_server().await;
        let client = reqwest::Client::new();

        let app_uuid = Uuid::default();
        let target_app = TargetApp::default();
        let response = client
            .post(format!("http://127.0.0.1:{port}/v3/device/apps/{app_uuid}",))
            .json(&target_app)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 202);

        assert!(seek_rx.changed().await.is_ok());
        let seek_request = seek_rx.borrow().clone();
        assert!(!seek_request.opts.force);
        assert!(!seek_request.opts.cancel); // API default via serde is false
        assert!(seek_request.target.apps.contains_key(&app_uuid));
    }

    #[tokio::test]
    async fn test_set_app_with_query_params() {
        let (port, _, mut seek_rx) = setup_test_server().await;
        let client = reqwest::Client::new();

        let app_uuid = Uuid::default();
        let target_app = TargetApp::default();
        let response = client
            .post(format!(
                "http://127.0.0.1:{port}/v3/device/apps/{app_uuid}?force=true",
            ))
            .json(&target_app)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 202);

        assert!(seek_rx.changed().await.is_ok());
        let seek_request = seek_rx.borrow().clone();
        assert!(seek_request.opts.force);
        assert!(seek_request.target.apps.contains_key(&app_uuid));
    }
}
