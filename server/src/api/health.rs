use crate::error::AppResult;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

/// Liveness: process is up (cheap).
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Readiness for load balancers / compose depends_on: SQLite pools accept queries.
pub async fn ready_or_503(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    for (name, pool) in [
        ("logs", &state.logs_db),
        ("system", &state.system_db),
        ("metrics", &state.metrics_db),
    ] {
        if let Err(err) = sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(pool)
            .await
        {
            tracing::error!(db = name, error = %err, "ready_check_failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "not_ready", "db": name })),
            );
        }
    }
    (StatusCode::OK, Json(json!({ "status": "ready" })))
}

pub async fn info(State(state): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(json!({
        "name": "tiny-log",
        "version": env!("CARGO_PKG_VERSION"),
        "auth_mode": state.config.auth_mode.as_str(),
    })))
}
