use crate::auth::middleware::require_api_key;
use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::metrics::{MetricsBatchRequest, MetricsHistoryQuery};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

pub async fn ingest_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MetricsBatchRequest>,
) -> AppResult<Json<Value>> {
    require_api_key(&state, &headers)?;

    let host = body.host.trim();
    if host.is_empty() {
        return Err(AppError::bad_request("host is required"));
    }
    if body.system.is_none() && body.services.is_empty() {
        return Err(AppError::bad_request(
            "system or services is required",
        ));
    }

    let timestamp_ms = db::metrics::parse_batch_timestamp(&body)?;
    db::metrics::insert_batch(
        &state.metrics_db,
        host,
        timestamp_ms,
        body.system.as_ref(),
        &body.services,
    )
    .await?;

    Ok(Json(json!({
        "success": true,
        "host": host,
        "services": body.services.len()
    })))
}

pub async fn overview(
    State(state): State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Value>> {
    let hosts = db::metrics::latest_host_samples(&state.metrics_db).await?;
    let services = db::metrics::latest_service_checks(&state.metrics_db).await?;
    Ok(Json(json!({
        "hosts": hosts,
        "services": services
    })))
}

pub async fn list_hosts(
    State(state): State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Value>> {
    let hosts = db::metrics::list_hosts(&state.metrics_db).await?;
    Ok(Json(json!({ "hosts": hosts })))
}

pub async fn history(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(query): Query<MetricsHistoryQuery>,
) -> AppResult<Json<Value>> {
    let kind = query
        .kind
        .as_deref()
        .unwrap_or_else(|| {
            if query.service.as_ref().is_some_and(|s| !s.is_empty()) {
                "service"
            } else {
                "host"
            }
        })
        .to_lowercase();

    match kind.as_str() {
        "host" => {
            let samples = db::metrics::history_hosts(&state.metrics_db, &query).await?;
            Ok(Json(json!({ "kind": "host", "samples": samples })))
        }
        "service" => {
            let checks = db::metrics::history_services(&state.metrics_db, &query).await?;
            Ok(Json(json!({ "kind": "service", "checks": checks })))
        }
        _ => Err(AppError::bad_request("kind must be host or service")),
    }
}
