use crate::auth::require_admin;
use crate::db;
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
    let admin = db::admin::get_admin(&state.system_db).await.ok().flatten();
    if admin.is_none() {
        return serve_file(&state, "setup.html").await;
    }

    match require_admin(&state, &jar).await {
        Ok(_) => serve_file(&state, "index.html").await,
        Err(_) => serve_file(&state, "login.html").await,
    }
}

pub async fn login_page(State(state): State<AppState>) -> impl IntoResponse {
    let admin = db::admin::get_admin(&state.system_db).await.ok().flatten();
    if admin.is_none() {
        return redirect("/setup");
    }
    if state.config.is_anonymous() {
        return redirect("/");
    }
    serve_file(&state, "login.html").await
}

pub async fn setup_page(State(state): State<AppState>) -> impl IntoResponse {
    let admin = db::admin::get_admin(&state.system_db).await.ok().flatten();
    if admin.is_some() {
        return redirect("/");
    }
    serve_file(&state, "setup.html").await
}

pub async fn settings_page(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    let admin = db::admin::get_admin(&state.system_db).await.ok().flatten();
    if admin.is_none() {
        return redirect("/setup");
    }
    match require_admin(&state, &jar).await {
        Ok(_) => serve_file(&state, "settings.html").await,
        Err(_) => redirect("/login"),
    }
}

pub async fn monitor_page(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    let admin = db::admin::get_admin(&state.system_db).await.ok().flatten();
    if admin.is_none() {
        return redirect("/setup");
    }
    match require_admin(&state, &jar).await {
        Ok(_) => serve_file(&state, "monitor.html").await,
        Err(_) => redirect("/login"),
    }
}

pub async fn swagger_page(State(state): State<AppState>) -> impl IntoResponse {
    serve_file(&state, "swagger.html").await
}

pub async fn openapi_spec(State(state): State<AppState>) -> impl IntoResponse {
    serve_file(&state, "openapi.json").await
}

fn redirect(to: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, to)
        .body(Body::empty())
        .unwrap()
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
        "app.js"
            | "login.js"
            | "settings.js"
            | "setup.js"
            | "monitor.js"
            | "style.css"
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
        Some("json") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    };
    Ok((HeaderValue::from_static(content_type), bytes))
}
