use crate::auth::{clear_session_cookie, password, session_cookie, AuthUser};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::settings::SetupRequest;
use crate::state::AppState;
use axum::extract::ConnectInfo;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use axum_extra::extract::CookieJar;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> AppResult<(CookieJar, Json<Value>)> {
    let ip = client_ip(&headers, addr);

    {
        let mut limiter = state.login_limiter.lock().await;
        if !limiter.check_allowed(&ip) {
            return Err(AppError::TooManyRequests);
        }
    }

    let admin = match db::admin::get_admin(&state.system_db).await? {
        Some(a) => a,
        None => {
            state.login_limiter.lock().await.record_failure(&ip);
            return Err(AppError::Unauthorized);
        }
    };

    let username_ok = constant_time_eq(admin.username.as_bytes(), body.username.as_bytes());
    let password_ok = password::verify_password(&body.password, &admin.password_hash)?;

    if !(username_ok && password_ok) {
        state.login_limiter.lock().await.record_failure(&ip);
        tracing::warn!(ip = %ip, "unauthorized_request path=/api/auth/login");
        return Err(AppError::Unauthorized);
    }

    state.login_limiter.lock().await.clear(&ip);

    let token = crate::auth::session::create_session(&state, admin.id).await?;
    let settings = db::settings::get_settings(&state.system_db).await?;
    let cookie = session_cookie(&token, state.config.cookie_secure, settings.session_days);

    Ok((
        jar.add(cookie),
        Json(json!({
            "success": true,
            "username": admin.username,
            "role": "admin"
        })),
    ))
}

pub async fn setup(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<SetupRequest>,
) -> AppResult<(CookieJar, Json<Value>)> {
    if db::admin::get_admin(&state.system_db).await?.is_some() {
        return Err(AppError::Conflict("admin user already exists".into()));
    }

    let username = body.username.trim();
    if username.is_empty() {
        return Err(AppError::bad_request("username is required"));
    }
    if body.password != body.confirm_password {
        return Err(AppError::bad_request("passwords do not match"));
    }

    let hash = password::hash_password(&body.password)?;
    let now = Utc::now().timestamp_millis();
    db::admin::create_admin(&state.system_db, username, &hash, now).await?;

    let admin = db::admin::get_admin(&state.system_db)
        .await?
        .ok_or_else(|| AppError::internal("admin missing after create"))?;

    let token = crate::auth::session::create_session(&state, admin.id).await?;
    let settings = db::settings::get_settings(&state.system_db).await?;
    let cookie = session_cookie(&token, state.config.cookie_secure, settings.session_days);

    Ok((
        jar.add(cookie),
        Json(json!({
            "success": true,
            "username": admin.username,
            "role": "admin",
            "auth_mode": state.config.auth_mode.as_str()
        })),
    ))
}

pub async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
    user: AuthUser,
) -> AppResult<(CookieJar, Json<Value>)> {
    if !user.session_id.is_empty() {
        db::sessions::delete_session(&state.system_db, &user.session_id).await?;
    }

    let jar = jar.add(clear_session_cookie(state.config.cookie_secure));
    Ok((jar, Json(json!({ "success": true }))))
}

pub async fn me(State(state): State<AppState>, jar: CookieJar) -> AppResult<Json<Value>> {
    let admin = db::admin::get_admin(&state.system_db).await?;
    let setup_required = admin.is_none();

    let session_user = crate::auth::require_admin(&state, &jar).await.ok();
    let authenticated = session_user
        .as_ref()
        .map(|u| !u.setup_required && (state.config.is_anonymous() || !u.session_id.is_empty()))
        .unwrap_or(false);

    Ok(Json(json!({
        "auth_mode": state.config.auth_mode.as_str(),
        "authenticated": authenticated || (state.config.is_anonymous() && !setup_required),
        "setup_required": setup_required,
        "username": session_user.as_ref().and_then(|u| u.username()).or_else(|| admin.as_ref().map(|a| a.username.as_str())),
        "role": if authenticated || state.config.is_anonymous() { Some("admin") } else { None },
    })))
}

fn client_ip(headers: &HeaderMap, addr: SocketAddr) -> String {
    if let Some(forwarded) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return forwarded.to_string();
    }
    addr.ip().to_string()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
