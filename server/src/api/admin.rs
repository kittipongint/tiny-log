use crate::auth::password;
use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::settings::{ChangePasswordRequest, UpdateSettings};
use crate::retention;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use chrono::Utc;
use serde_json::{json, Value};

pub async fn get_settings(
    State(state): State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Value>> {
    let settings = db::settings::get_settings(&state.system_db).await?;
    Ok(Json(json!({
        "retention_days": settings.retention_days,
        "session_days": settings.session_days,
        "metrics_retention_days": settings.metrics_retention_days,
        "auth_mode": state.config.auth_mode.as_str()
    })))
}

pub async fn update_settings(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<UpdateSettings>,
) -> AppResult<Json<Value>> {
    let mut settings = db::settings::get_settings(&state.system_db).await?;

    if let Some(days) = body.retention_days {
        if !(1..=3650).contains(&days) {
            return Err(AppError::bad_request(
                "retention_days must be between 1 and 3650",
            ));
        }
        db::settings::set_retention_days(&state.system_db, days).await?;
        settings.retention_days = days;
    }

    if let Some(days) = body.session_days {
        if days < 1 {
            return Err(AppError::bad_request("session_days must be at least 1"));
        }
        db::settings::set_session_days(&state.system_db, days).await?;
        settings.session_days = days;
    }

    if let Some(days) = body.metrics_retention_days {
        if !(1..=3650).contains(&days) {
            return Err(AppError::bad_request(
                "metrics_retention_days must be between 1 and 3650",
            ));
        }
        db::settings::set_metrics_retention_days(&state.system_db, days).await?;
        settings.metrics_retention_days = days;
    }

    Ok(Json(json!({
        "success": true,
        "retention_days": settings.retention_days,
        "session_days": settings.session_days,
        "metrics_retention_days": settings.metrics_retention_days
    })))
}

pub async fn run_retention(
    State(state): State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Value>> {
    let result = retention::run_cleanup(
        &state.logs_db,
        &state.system_db,
        &state.metrics_db,
    )
    .await?;
    Ok(Json(json!({
        "deleted": result.logs_deleted,
        "metrics_deleted": result.metrics_deleted
    })))
}

pub async fn change_password(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ChangePasswordRequest>,
) -> AppResult<Json<Value>> {
    let admin = user
        .admin
        .as_ref()
        .ok_or(AppError::Unauthorized)?;

    if user.setup_required {
        return Err(AppError::Unauthorized);
    }

    // In login mode, require a real session for password change.
    if !state.config.is_anonymous() && user.session_id.is_empty() {
        return Err(AppError::Unauthorized);
    }

    if body.new_password != body.confirm_password {
        return Err(AppError::bad_request("passwords do not match"));
    }

    let ok = password::verify_password(&body.current_password, &admin.password_hash)?;
    if !ok {
        return Err(AppError::Unauthorized);
    }

    let hash = password::hash_password(&body.new_password)?;
    let now = Utc::now().timestamp_millis();
    db::admin::update_password(&state.system_db, &hash, now).await?;
    db::sessions::delete_all_sessions(&state.system_db).await?;

    Ok(Json(json!({ "success": true })))
}
