use crate::db;
use crate::error::AppResult;
use crate::state::AppState;
use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::Utc;
use rand::RngCore;
use time::Duration as TimeDuration;

pub const SESSION_COOKIE: &str = "tiny_log_session";

pub fn generate_session_id() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub async fn create_session(state: &AppState, admin_id: i64) -> AppResult<String> {
    let settings = db::settings::get_settings(&state.pool).await?;
    let now = Utc::now().timestamp_millis();
    let expires_at = now + settings.session_days * 24 * 60 * 60 * 1000;
    let id = generate_session_id();

    db::sessions::create_session(&state.pool, &id, admin_id, expires_at, now).await?;
    Ok(id)
}

pub fn session_cookie(token: &str, secure: bool, max_age_days: i64) -> Cookie<'static> {
    let mut cookie = Cookie::build((SESSION_COOKIE, token.to_string()))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(TimeDuration::days(max_age_days))
        .build();

    if secure {
        cookie.set_secure(true);
    }

    cookie
}

pub fn clear_session_cookie(secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::build((SESSION_COOKIE, ""))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(TimeDuration::seconds(0))
        .build();

    if secure {
        cookie.set_secure(true);
    }

    cookie
}
