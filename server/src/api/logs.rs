use crate::auth::middleware::{require_api_key, AuthUser};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::log::{BatchLogsRequest, LogEntry, LogQuery, NewLog};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};

pub async fn create_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<NewLog>,
) -> AppResult<(StatusCode, Json<Value>)> {
    require_api_key(&state, &headers)?;

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

    Ok((StatusCode::OK, Json(json!({ "success": true, "id": id }))))
}

pub async fn create_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BatchLogsRequest>,
) -> AppResult<Json<Value>> {
    require_api_key(&state, &headers)?;

    if body.logs.is_empty() {
        return Err(AppError::bad_request("logs array is empty"));
    }
    if body.logs.len() > state.config.max_batch {
        return Err(AppError::PayloadTooLarge);
    }

    let mut inserts = Vec::with_capacity(body.logs.len());
    for log in body.logs {
        inserts.push(
            log.validate_and_normalize()
                .map_err(AppError::bad_request)?,
        );
    }

    let ids = db::logs::insert_logs_batch(&state.logs_db, &inserts).await?;

    for (id, insert) in ids.iter().zip(inserts.into_iter()) {
        let entry = LogEntry {
            id: *id,
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
    }

    Ok(Json(json!({
        "success": true,
        "ids": ids,
        "count": ids.len()
    })))
}

pub async fn list_logs(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(query): Query<LogQuery>,
) -> AppResult<Json<Value>> {
    let logs = db::logs::query_logs(&state.logs_db, &query).await?;
    Ok(Json(json!({ "logs": logs })))
}

pub async fn get_log(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<LogEntry>> {
    match db::logs::get_log(&state.logs_db, id).await? {
        Some(log) => Ok(Json(log)),
        None => Err(AppError::NotFound),
    }
}

pub async fn list_apps(
    State(state): State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Value>> {
    let apps = db::logs::list_apps(&state.logs_db).await?;
    Ok(Json(json!({ "apps": apps })))
}
