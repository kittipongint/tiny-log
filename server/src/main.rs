mod api;
mod auth;
mod cli;
mod config;
mod db;
mod error;
mod models;
mod retention;
mod state;
mod web;

use clap::Parser;
use cli::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    if let Err(err) = cli::run(cli).await {
        tracing::error!(error = %err, "fatal");
        eprintln!("error: {err}");
        std::process::exit(1);
    }
    Ok(())
}
