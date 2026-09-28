use crate::auth::middleware::{require_api_key, AuthUser};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::log::{BatchLogsRequest, InsertLog, LogEntry, LogQuery, NewLog};
use crate::api::json::JsonBody;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};

pub async fn create_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<NewLog>,
) -> AppResult<(StatusCode, Json<Value>)> {
    require_api_key(&state, &headers)?;

    let insert = body
        .validate_and_normalize()
        .map_err(AppError::bad_request)?;

    let id = db::logs::insert_log(&state.logs_db, &insert).await?;
    broadcast_inserted(&state, [(id, insert)]);

    Ok((StatusCode::OK, Json(json!({ "success": true, "id": id }))))
}

/// All-or-nothing: one invalid entry rejects the batch (400) and `rejected` lists every
/// bad index, so a client can drop exactly those and resend the rest.
pub async fn create_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<BatchLogsRequest>,
) -> AppResult<Json<Value>> {
    require_api_key(&state, &headers)?;

    if body.logs.is_empty() {
        return Err(AppError::bad_request("logs array is empty"));
    }
    if body.logs.len() > state.config.max_batch {
        return Err(state.config.payload_too_large());
    }

    let mut inserts = Vec::with_capacity(body.logs.len());
    let mut rejected = Vec::new();
    for (index, log) in body.logs.into_iter().enumerate() {
        match log.validate_and_normalize() {
            Ok(insert) => inserts.push(insert),
            Err(error) => rejected.push((index, error)),
        }
    }
    if !rejected.is_empty() {
        return Err(AppError::Rejected(rejected));
    }

    let ids = db::logs::insert_logs_batch(&state.logs_db, &inserts).await?;
    let count = ids.len();
    broadcast_inserted(&state, ids.iter().copied().zip(inserts));

    Ok(Json(json!({
        "success": true,
        "ids": ids,
        "count": count
    })))
}

/// Feed live viewers. With nobody on /stream this costs nothing — no LogEntry is built
/// and meta_json isn't re-parsed.
pub(crate) fn broadcast_inserted(
    state: &AppState,
    rows: impl IntoIterator<Item = (i64, InsertLog)>,
) {
    if state.broadcaster.receiver_count() == 0 {
        return;
    }
    for (id, insert) in rows {
        let entry = LogEntry {
            id,
            timestamp: crate::models::log::ms_to_rfc3339(insert.timestamp_ms),
            app: insert.app,
            level: insert.level,
            source: insert.source,
            message: insert.message,
            meta: insert
                .meta_json
                .as_deref()
                .and_then(|m| serde_json::from_str(m).ok()),
        };
        let _ = state.broadcaster.send(entry);
    }
}

pub async fn list_logs(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(query): Query<LogQuery>,
) -> AppResult<Json<Value>> {
    let logs = db::logs::query_logs(&state.logs_db, &query).await?;
    Ok(Json(json!({ "logs": logs })))
}

pub async fn list_traces(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(query): Query<LogQuery>,
) -> AppResult<Json<Value>> {
    let traces = db::logs::query_traces(&state.logs_db, &query).await?;
    Ok(Json(json!({ "traces": traces })))
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
