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
    use axum::routing::get;
    use axum::Router;
    use std::net::SocketAddr;
    use tower_http::cors::{AllowOrigin, CorsLayer};
    use tower_http::trace::TraceLayer;

    let addr_str = config.bind_addr();
    let max_body = config.max_body_mb * 1024 * 1024;
    let cookie_secure = config.cookie_secure;
    let cors_origin = config.cors_origin.clone();
    let db_path = config.database.display().to_string();
    let retention_days = config.retention_days;

    let state = AppState::new(config).await?;
    state.migrate().await?;

    tracing::info!(path = %db_path, "database_opened");

    retention::spawn_worker(state.pool.clone());
    tracing::info!(days = retention_days, "retention_started");

    let mut app = Router::new()
        .merge(api::router())
        .route("/", get(web::root))
        .route("/login", get(web::login_page))
        .route("/settings", get(web::settings_page))
        .route("/{file}", get(web::static_asset))
        .layer(DefaultBodyLimit::max(max_body))
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
    tracing::info!(addr = %addr_str, cookie_secure, "server_started");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
