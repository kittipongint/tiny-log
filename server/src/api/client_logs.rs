use crate::auth::middleware::require_client_token;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::log::{LogEntry, NewLog};
use crate::state::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

pub async fn create_client_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut body): Json<NewLog>,
) -> AppResult<Json<Value>> {
    require_client_token(&state, &headers)?;

    if body.source.as_deref().unwrap_or("").is_empty() {
        body.source = Some("browser".into());
    }

    let insert = body
        .validate_and_normalize()
        .map_err(AppError::bad_request)?;

    let id = db::logs::insert_log(&state.logs_db, &insert).await?;

    let entry = LogEntry {
        id,
        timestamp: crate::models::log::ms_to_rfc3339(insert.timestamp_ms),
        app: insert.app,
        level: insert.level,
        source: insert.source,
        message: insert.message,
        meta: insert
            .meta_json
            .as_ref()
            .and_then(|m| serde_json::from_str(m).ok()),
    };
    let _ = state.broadcaster.send(entry);

    Ok(Json(json!({ "success": true, "id": id })))
}
