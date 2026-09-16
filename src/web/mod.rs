use crate::auth::{require_admin, SESSION_COOKIE};
use crate::error::AppResult;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
use axum_extra::extract::CookieJar;
use std::path::PathBuf;
use tokio::fs;

pub async fn root(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    match require_admin(&state, &jar).await {
        Ok(_) => serve_file(&state, "index.html").await,
        Err(_) => serve_file(&state, "login.html").await,
    }
}

pub async fn login_page(State(state): State<AppState>) -> impl IntoResponse {
    serve_file(&state, "login.html").await
}

pub async fn settings_page(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    match require_admin(&state, &jar).await {
        Ok(_) => serve_file(&state, "settings.html").await,
        Err(_) => RedirectLogin.into_response(),
    }
}

struct RedirectLogin;

impl IntoResponse for RedirectLogin {
    fn into_response(self) -> Response<Body> {
        Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/login")
            .body(Body::empty())
            .unwrap()
    }
}

async fn serve_file(state: &AppState, name: &str) -> Response<Body> {
    let path = state.config.web_dir.join(name);
    match read_asset(path).await {
        Ok((content_type, bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

pub async fn static_asset(
    State(state): State<AppState>,
    axum::extract::Path(file): axum::extract::Path<String>,
) -> impl IntoResponse {
    let allowed = matches!(
        file.as_str(),
        "app.js" | "login.js" | "settings.js" | "style.css"
    );
    if !allowed {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = state.config.web_dir.join(&file);
    match read_asset(path).await {
        Ok((content_type, bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn read_asset(path: PathBuf) -> AppResult<(HeaderValue, Vec<u8>)> {
    let bytes = fs::read(&path)
        .await
        .map_err(|_| crate::error::AppError::NotFound)?;
    let content_type = match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        _ => "application/octet-stream",
    };
    Ok((HeaderValue::from_static(content_type), bytes))
}

#[allow(dead_code)]
pub fn has_session_cookie(jar: &CookieJar) -> bool {
    jar.get(SESSION_COOKIE)
        .map(|c| !c.value().is_empty())
        .unwrap_or(false)
}
