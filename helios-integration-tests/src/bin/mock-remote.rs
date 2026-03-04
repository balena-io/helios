use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, put},
};
use serde_json::Value;
use tokio::sync::RwLock;

type AppState = Arc<RwLock<Option<Value>>>;

#[tokio::main]
async fn main() {
    let state: AppState = Arc::new(RwLock::new(None));
    let app = Router::new()
        .route("/health", get(health))
        .route("/device/v3/{uuid}/state", get(get_state))
        .route("/device/v3/state", patch(state_patch))
        .route("/mock/state", put(put_state).delete(delete_state))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:80").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// return 200 to any state patch
async fn state_patch() {}

async fn health() -> &'static str {
    "OK"
}

async fn get_state(
    Path(uuid): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let state = state.read().await;
    match state.as_ref() {
        Some(target) => {
            let mut map = serde_json::Map::new();
            map.insert(uuid, target.clone());
            Ok(Json(Value::Object(map)))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn put_state(State(state): State<AppState>, Json(body): Json<Value>) -> StatusCode {
    *state.write().await = Some(body);
    StatusCode::OK
}

async fn delete_state(State(state): State<AppState>) -> StatusCode {
    *state.write().await = None;
    StatusCode::NO_CONTENT
}
