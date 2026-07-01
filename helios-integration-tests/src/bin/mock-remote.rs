use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, put},
};
use serde_json::Value;
use tokio::sync::{RwLock, broadcast};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

struct MockState {
    target: Option<Value>,
    report_tx: broadcast::Sender<Value>,
    current_report: Value,
}

type AppState = Arc<RwLock<MockState>>;

#[tokio::main]
async fn main() {
    let (report_tx, _) = broadcast::channel(256);
    let state: AppState = Arc::new(RwLock::new(MockState {
        target: None,
        report_tx,
        current_report: Value::Object(serde_json::Map::new()),
    }));
    let app = Router::new()
        .route("/health", get(health))
        .route("/device/v3/{uuid}/state", get(get_state))
        .route("/device/v3/state", patch(state_patch))
        .route("/mock/state", put(put_state).delete(delete_target))
        .route("/mock/reports", get(get_reports).delete(delete_reports))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:80").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Merge a report diff into the accumulated state.
///
/// - If diff has no `apps` key for a device -> keep current apps unchanged
/// - If diff has `apps` key -> replace apps entirely (all-or-nothing)
fn merge_report(current: &mut Value, diff: &Value) {
    let Some(diff_obj) = diff.as_object() else {
        return;
    };

    for (device_uuid, device_diff) in diff_obj {
        let Some(device_diff_obj) = device_diff.as_object() else {
            continue;
        };

        let current_device = current
            .as_object_mut()
            .unwrap()
            .entry(device_uuid.clone())
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .unwrap();

        if let Some(apps) = device_diff_obj.get("apps") {
            current_device.insert("apps".to_string(), apps.clone());
        }
    }
}

fn to_ndjson_line(report: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(report).unwrap();
    line.push(b'\n');
    line
}

async fn state_patch(State(state): State<AppState>, Json(body): Json<Value>) {
    let mut state = state.write().await;
    merge_report(&mut state.current_report, &body);
    let _ = state.report_tx.send(state.current_report.clone());
}

async fn health() -> &'static str {
    "OK"
}

async fn get_state(
    Path(uuid): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let state = state.read().await;
    match state.target.as_ref() {
        Some(target) => {
            let mut map = serde_json::Map::new();
            map.insert(uuid, target.clone());
            Ok(Json(Value::Object(map)))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn put_state(State(state): State<AppState>, Json(body): Json<Value>) -> StatusCode {
    state.write().await.target = Some(body);
    StatusCode::OK
}

async fn delete_target(State(state): State<AppState>) -> StatusCode {
    state.write().await.target = None;
    StatusCode::NO_CONTENT
}

async fn get_reports(State(state): State<AppState>) -> impl IntoResponse {
    let state = state.read().await;
    let current = state.current_report.clone();
    let rx = state.report_tx.subscribe();
    drop(state);

    // Replay current accumulated state, then stream new reports as NDJSON
    let replay: Option<Result<Vec<u8>, std::io::Error>> = if current.as_object().unwrap().is_empty()
    {
        None
    } else {
        Some(Ok(to_ndjson_line(&current)))
    };

    let live = BroadcastStream::new(rx).filter_map(|r| r.ok().map(|v| Ok(to_ndjson_line(&v))));
    let stream = tokio_stream::iter(replay).chain(live);

    Body::from_stream(stream)
}

async fn delete_reports(State(state): State<AppState>) -> StatusCode {
    let mut state = state.write().await;
    state.current_report = Value::Object(serde_json::Map::new());
    StatusCode::NO_CONTENT
}
