use crate::error::AppResult;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn info(State(state): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(json!({
        "name": "tiny-log",
        "version": env!("CARGO_PKG_VERSION"),
        "auth_mode": state.config.auth_mode.as_str(),
    })))
}
