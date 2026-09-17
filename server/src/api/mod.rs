pub mod admin;
pub mod auth;
pub mod client_logs;
pub mod health;
pub mod logs;
pub mod metrics;
pub mod stream;

use crate::state::AppState;
use axum::routing::{get, post};
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health))
        .route("/api/v1/info", get(health::info))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/setup", post(auth::setup))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/me", get(auth::me))
        .route("/api/v1/logs", post(logs::create_log).get(logs::list_logs))
        .route("/api/v1/logs/batch", post(logs::create_batch))
        .route("/api/v1/logs/stream", get(stream::stream_logs))
        .route("/api/v1/logs/{id}", get(logs::get_log))
        .route("/api/v1/client/logs", post(client_logs::create_client_log))
        .route("/api/v1/metrics/batch", post(metrics::ingest_batch))
        .route("/api/v1/metrics/overview", get(metrics::overview))
        .route("/api/v1/metrics/hosts", get(metrics::list_hosts))
        .route("/api/v1/metrics/history", get(metrics::history))
        .route("/api/v1/metrics/capacity", get(metrics::capacity))
        .route(
            "/api/v1/admin/settings",
            get(admin::get_settings).put(admin::update_settings),
        )
        .route("/api/v1/admin/retention/run", post(admin::run_retention))
        .route("/api/v1/admin/password", post(admin::change_password))
        .route("/api/v1/apps", get(logs::list_apps))
}
