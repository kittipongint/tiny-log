//! First-run setup must not hand the server to whoever calls it first.

use crate::config::{AuthMode, Config};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use tower::ServiceExt;

pub(super) fn test_config(mode: AuthMode, setup_token: Option<&str>) -> Config {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "tiny-log-test-{}",
        crate::auth::session::generate_session_id()
    ));
    Config {
        host: "127.0.0.1".into(),
        port: 0,
        logs_database: dir.join("logs.db"),
        system_database: dir.join("system.db"),
        metrics_database: dir.join("metrics.db"),
        api_key: Some("test-api-key".into()),
        client_token: None,
        setup_token: setup_token.map(str::to_string),
        auth_mode: mode,
        retention_days: 30,
        session_days: 7,
        cookie_secure: false,
        max_body_mb: 1,
        max_batch: 500,
        cors_origin: None,
        web_dir: PathBuf::from("./web"),
    }
}

pub(super) async fn app(config: Config) -> (Router, AppState) {
    let state = AppState::new(config).await.unwrap();
    state.migrate().await.unwrap();
    state.prepare_setup_token().await.unwrap();
    let router = crate::api::router()
        .with_state(state.clone())
        .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 9], 4000))));
    (router, state)
}

pub(super) async fn call(router: &Router, req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, cookie, body)
}

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::get(path);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::empty()).unwrap()
}

fn setup_req(token: Option<&str>) -> Request<Body> {
    let mut body = json!({
        "username": "golf",
        "password": "correct horse battery",
        "confirm_password": "correct horse battery",
    });
    if let Some(t) = token {
        body["setup_token"] = json!(t);
    }
    Request::post("/api/auth/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn login_mode_before_setup_is_locked_and_needs_the_token() {
    let (router, state) = app(test_config(AuthMode::Login, Some("tok-123"))).await;

    // Before: "setup required" counted as logged in, so every read API was open.
    for path in ["/api/v1/apps", "/api/v1/logs", "/api/v1/admin/settings", "/api/v1/metrics/hosts"] {
        let (status, _, _) = call(&router, get(path, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} must be closed before setup");
    }

    let (status, _, me) = call(&router, get("/api/auth/me", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["setup_required"], json!(true));
    assert_eq!(me["authenticated"], json!(false));

    let (status, _, _) = call(&router, setup_req(None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token");
    let (status, _, _) = call(&router, setup_req(Some("wrong"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "wrong token");

    let (status, cookie, body) = call(&router, setup_req(Some(" tok-123 "))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cookie = cookie.expect("setup logs the new admin in");
    assert!(state.setup_token.lock().unwrap().is_none(), "token is spent");

    let (status, _, _) = call(&router, setup_req(Some("tok-123"))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _, _) = call(&router, get("/api/v1/apps", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&router, get("/api/v1/apps", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn generated_token_when_env_unset() {
    let (_router, state) = app(test_config(AuthMode::Login, None)).await;
    let token = state.setup_token.lock().unwrap().clone().expect("token generated");
    assert_eq!(token.len(), 64);
}

#[tokio::test]
async fn wrong_tokens_are_rate_limited() {
    let (router, _) = app(test_config(AuthMode::Login, Some("tok-123"))).await;
    for _ in 0..5 {
        let (status, _, _) = call(&router, setup_req(Some("guess"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, _) = call(&router, setup_req(Some("tok-123"))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn anonymous_mode_stays_open_but_setup_still_needs_the_token() {
    let (router, _) = app(test_config(AuthMode::Anonymous, Some("tok-123"))).await;
    let (status, _, _) = call(&router, get("/api/v1/apps", None)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&router, setup_req(Some("wrong"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn existing_admin_means_no_token() {
    let config = test_config(AuthMode::Login, Some("tok-123"));
    let (router, state) = app(config).await;
    let (status, _, _) = call(&router, setup_req(Some("tok-123"))).await;
    assert_eq!(status, StatusCode::OK);
    // Restart: an admin exists, so no token is armed.
    state.prepare_setup_token().await.unwrap();
    assert!(state.setup_token.lock().unwrap().is_none());
}
