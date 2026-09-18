pub mod admin;

use crate::cli::admin::AdminCommand;
use crate::config::Config;
use crate::state::AppState;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "tiny-log", version, about = "Lightweight centralized logging server")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Start the HTTP server
    Serve,
    /// Run database migrations
    Migrate,
    /// Admin account management
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let config = Config::from_env();
    match cli.command {
        Commands::Serve => serve(config).await,
        Commands::Migrate => migrate(config).await,
        Commands::Admin { command } => admin::run(config, command).await,
    }
}

async fn migrate(config: Config) -> anyhow::Result<()> {
    let state = AppState::new(config).await?;
    state.migrate().await?;
    tracing::info!("migrate_ok");
    println!("Migration completed.");
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    use crate::api;
    use crate::retention;
    use crate::web;
    use axum::extract::DefaultBodyLimit;
    use axum::http::{header, HeaderValue};
    use axum::routing::get;
    use axum::Router;
    use std::net::SocketAddr;
    use tower_http::cors::{AllowOrigin, CorsLayer};
    use tower_http::set_header::SetResponseHeaderLayer;
    use tower_http::trace::TraceLayer;

    warn_prod_config(&config);

    let addr_str = config.bind_addr();
    let max_body = config.max_body_mb * 1024 * 1024;
    let cookie_secure = config.cookie_secure;
    let auth_mode = config.auth_mode.as_str().to_string();
    let cors_origin = config.cors_origin.clone();
    let logs_path = config.logs_database.display().to_string();
    let system_path = config.system_database.display().to_string();
    let metrics_path = config.metrics_database.display().to_string();
    let retention_days = config.retention_days;

    let state = AppState::new(config).await?;
    state.migrate().await?;

    tracing::info!(
        logs = %logs_path,
        system = %system_path,
        metrics = %metrics_path,
        "database_opened"
    );

    retention::spawn_worker(
        state.logs_db.clone(),
        state.system_db.clone(),
        state.metrics_db.clone(),
    );
    tracing::info!(days = retention_days, "retention_started");

    let mut app = Router::new()
        .merge(api::router())
        .route("/", get(web::root))
        .route("/login", get(web::login_page))
        .route("/setup", get(web::setup_page))
        .route("/settings", get(web::settings_page))
        .route("/monitor", get(web::monitor_page))
        .route("/swagger", get(web::swagger_page))
        .route("/clients", get(web::clients_page))
        .route("/openapi.json", get(web::openapi_spec))
        .route("/{file}", get(web::static_asset))
        .layer(DefaultBodyLimit::max(max_body))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    if let Some(origin) = cors_origin {
        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::exact(origin.parse()?))
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PUT,
            ])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
                axum::http::header::COOKIE,
            ])
            .allow_credentials(true);
        app = app.layer(cors);
    }

    let listener = tokio::net::TcpListener::bind(&addr_str).await?;
    tracing::info!(
        addr = %addr_str,
        cookie_secure,
        auth_mode = %auth_mode,
        "server_started"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    tracing::info!("server_stopped");
    Ok(())
}

fn warn_prod_config(config: &Config) {
    if config.api_key.is_none() {
        tracing::warn!("TINY_LOG_API_KEY unset — agent/log ingest will reject requests");
    }
    if config.client_token.is_none() {
        tracing::warn!("TINY_LOG_CLIENT_TOKEN unset — browser client ingest disabled");
    }
    if config.is_anonymous() {
        tracing::warn!("auth_mode=anonymous — UI has no login gate; use only on trusted networks");
    }
    if !config.cookie_secure && !config.is_anonymous() {
        tracing::warn!("TINY_LOG_COOKIE_SECURE=false — session cookies can leak over HTTP");
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "ctrl_c_handler_failed");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => tracing::error!(error = %err, "sigterm_handler_failed"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!(signal = "SIGINT", "shutdown_requested"),
        _ = terminate => tracing::info!(signal = "SIGTERM", "shutdown_requested"),
    }
}
