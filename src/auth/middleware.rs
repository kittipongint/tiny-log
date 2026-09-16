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
    pub admin: AdminUser,
    pub session_id: String,
}

pub async fn require_admin(state: &AppState, jar: &CookieJar) -> AppResult<AuthUser> {
    if !state.config.require_auth {
        if let Some(admin) = db::admin::get_admin(&state.pool).await? {
            return Ok(AuthUser {
                admin,
                session_id: String::new(),
            });
        }
        return Err(AppError::Unauthorized);
    }

    let token = jar
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_string())
        .filter(|v| !v.is_empty())
        .ok_or(AppError::Unauthorized)?;

    let session = db::sessions::get_session(&state.pool, &token)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let now = Utc::now().timestamp_millis();
    if session.expires_at < now {
        let _ = db::sessions::delete_session(&state.pool, &token).await;
        return Err(AppError::Unauthorized);
    }

    let admin = db::admin::get_admin(&state.pool)
        .await?
        .ok_or(AppError::Unauthorized)?;

    if admin.id != session.admin_id {
        return Err(AppError::Unauthorized);
    }

    let _ = db::sessions::touch_session(&state.pool, &token, now).await;

    Ok(AuthUser {
        admin,
        session_id: token,
    })
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
    let value = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
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
