use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, put},
};
use serde_json::Value;
use tokio::sync::RwLock;

struct MockState {
    target: Option<Value>,
    reports: Vec<Value>,
}

type AppState = Arc<RwLock<MockState>>;

#[tokio::main]
async fn main() {
    let state: AppState = Arc::new(RwLock::new(MockState {
        target: None,
        reports: Vec::new(),
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

async fn state_patch(State(state): State<AppState>, Json(body): Json<Value>) {
    state.write().await.reports.push(body);
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

async fn get_reports(State(state): State<AppState>) -> Json<Value> {
    let reports = state.read().await.reports.clone();
    Json(Value::Array(reports))
}

async fn delete_reports(State(state): State<AppState>) -> StatusCode {
    state.write().await.reports.clear();
    StatusCode::NO_CONTENT
}
