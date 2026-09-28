use crate::auth::middleware::require_client_token;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::log::NewLog;
use crate::api::json::JsonBody;
use crate::state::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

pub async fn create_client_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    JsonBody(mut body): JsonBody<NewLog>,
) -> AppResult<Json<Value>> {
    require_client_token(&state, &headers)?;

    if body.source.as_deref().unwrap_or("").is_empty() {
        body.source = Some("browser".into());
    }

    let insert = body
        .validate_and_normalize()
        .map_err(AppError::bad_request)?;

    let id = db::logs::insert_log(&state.logs_db, &insert).await?;
    crate::api::logs::broadcast_inserted(&state, [(id, insert)]);

    Ok(Json(json!({ "success": true, "id": id })))
}
