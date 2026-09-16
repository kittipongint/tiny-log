use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::admin::AdminUser;
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use chrono::Utc;

use super::session::SESSION_COOKIE;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub admin: Option<AdminUser>,
    pub session_id: String,
    pub setup_required: bool,
}

impl AuthUser {
    pub fn username(&self) -> Option<&str> {
        self.admin.as_ref().map(|a| a.username.as_str())
    }
}

/// Allow UI/read access when anonymous, first-run setup, or valid session.
pub async fn require_admin(state: &AppState, jar: &CookieJar) -> AppResult<AuthUser> {
    let admin = db::admin::get_admin(&state.system_db).await?;
    let setup_required = admin.is_none();

    if setup_required {
        return Ok(AuthUser {
            admin: None,
            session_id: String::new(),
            setup_required: true,
        });
    }

    if state.config.is_anonymous() {
        return Ok(AuthUser {
            admin,
            session_id: String::new(),
            setup_required: false,
        });
    }

    let token = jar
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_string())
        .filter(|v| !v.is_empty())
        .ok_or(AppError::Unauthorized)?;

    let session = db::sessions::get_session(&state.system_db, &token)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let now = Utc::now().timestamp_millis();
    if session.expires_at < now {
        let _ = db::sessions::delete_session(&state.system_db, &token).await;
        return Err(AppError::Unauthorized);
    }

    let admin = admin.ok_or(AppError::Unauthorized)?;
    if admin.id != session.admin_id {
        return Err(AppError::Unauthorized);
    }

    let _ = db::sessions::touch_session(&state.system_db, &token, now).await;

    Ok(AuthUser {
        admin: Some(admin),
        session_id: token,
        setup_required: false,
    })
}

/// Operations that need a real admin account (password change, etc.)
#[allow(dead_code)]
pub async fn require_logged_in_admin(state: &AppState, jar: &CookieJar) -> AppResult<AuthUser> {
    let user = require_admin(state, jar).await?;
    if user.setup_required {
        return Err(AppError::Unauthorized);
    }
    if state.config.is_anonymous() {
        if user.admin.is_some() {
            return Ok(user);
        }
        return Err(AppError::Unauthorized);
    }
    if user.session_id.is_empty() || user.admin.is_none() {
        return Err(AppError::Unauthorized);
    }
    Ok(user)
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        require_admin(state, &jar).await
    }
}

pub fn extract_bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = value.strip_prefix("Bearer ")?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

pub fn require_api_key(state: &AppState, headers: &axum::http::HeaderMap) -> AppResult<()> {
    let configured = state
        .config
        .api_key
        .as_ref()
        .ok_or(AppError::Unauthorized)?;
    let provided = extract_bearer(headers).ok_or(AppError::Unauthorized)?;
    if provided != *configured {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

pub fn require_client_token(state: &AppState, headers: &axum::http::HeaderMap) -> AppResult<()> {
    let configured = state
        .config
        .client_token
        .as_ref()
        .ok_or(AppError::Unauthorized)?;
    let provided = extract_bearer(headers).ok_or(AppError::Unauthorized)?;
    if provided != *configured {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}
