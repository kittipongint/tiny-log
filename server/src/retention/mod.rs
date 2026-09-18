use crate::db;
use crate::error::AppResult;
use chrono::Utc;
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time::interval;

pub struct CleanupResult {
    pub logs_deleted: u64,
    pub metrics_deleted: u64,
}

pub async fn run_cleanup(
    logs_db: &SqlitePool,
    system_db: &SqlitePool,
    metrics_db: &SqlitePool,
) -> AppResult<CleanupResult> {
    let settings = db::settings::get_settings(system_db).await?;
    let now = Utc::now().timestamp_millis();

    let logs_cutoff = now - settings.retention_days * 24 * 60 * 60 * 1000;
    let logs_deleted = db::logs::delete_older_than(logs_db, logs_cutoff).await?;
    if logs_deleted > 0 {
        db::logs::vacuum_incremental(logs_db).await?;
        tracing::info!(
            deleted = logs_deleted,
            days = settings.retention_days,
            "retention_cleanup"
        );
    }

    let metrics_cutoff = now - settings.metrics_retention_days * 24 * 60 * 60 * 1000;
    let metrics_deleted = db::metrics::delete_older_than(metrics_db, metrics_cutoff).await?;
    if metrics_deleted > 0 {
        db::metrics::vacuum_incremental(metrics_db).await?;
        tracing::info!(
            deleted = metrics_deleted,
            days = settings.metrics_retention_days,
            "metrics_retention_cleanup"
        );
    }

    let _ = db::sessions::delete_expired(system_db, now).await;

    Ok(CleanupResult {
        logs_deleted,
        metrics_deleted,
    })
}

pub fn spawn_worker(logs_db: SqlitePool, system_db: SqlitePool, metrics_db: SqlitePool) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(3600));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match run_cleanup(&logs_db, &system_db, &metrics_db).await {
                Ok(result) => {
                    if result.logs_deleted > 0 || result.metrics_deleted > 0 {
                        tracing::info!(
                            logs_deleted = result.logs_deleted,
                            metrics_deleted = result.metrics_deleted,
                            "retention_worker_ok"
                        );
                    }
                }
                Err(err) => {
                    tracing::error!(error = %err, "retention_worker_error");
                }
            }
            // Keep WAL files bounded even when nothing was deleted.
            crate::state::checkpoint_all(&logs_db, &system_db, &metrics_db).await;
        }
    });
}
