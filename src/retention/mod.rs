use crate::db;
use crate::error::AppResult;
use chrono::Utc;
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time::interval;

pub async fn run_cleanup(pool: &SqlitePool) -> AppResult<u64> {
    let settings = db::settings::get_settings(pool).await?;
    let cutoff = Utc::now().timestamp_millis() - settings.retention_days * 24 * 60 * 60 * 1000;
    let deleted = db::logs::delete_older_than(pool, cutoff).await?;
    if deleted > 0 {
        db::logs::vacuum_incremental(pool).await?;
        tracing::info!(deleted, days = settings.retention_days, "retention_cleanup");
    }
    let _ = db::sessions::delete_expired(pool, Utc::now().timestamp_millis()).await;
    Ok(deleted)
}

pub fn spawn_worker(pool: SqlitePool) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(3600));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match run_cleanup(&pool).await {
                Ok(deleted) => {
                    if deleted > 0 {
                        tracing::info!(deleted, "retention_worker_ok");
                    }
                }
                Err(err) => {
                    tracing::error!(error = %err, "retention_worker_error");
                }
            }
        }
    });
}
