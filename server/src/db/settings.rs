use crate::error::{AppError, AppResult};
use crate::models::settings::Settings;
use chrono::Utc;
use sqlx::SqlitePool;

const KEY_RETENTION: &str = "retention_days";
const KEY_SESSION: &str = "session_days";
const KEY_METRICS_RETENTION: &str = "metrics_retention_days";

pub async fn ensure_defaults(
    pool: &SqlitePool,
    retention_days: i64,
    session_days: i64,
    metrics_retention_days: i64,
) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    upsert_if_missing(pool, KEY_RETENTION, &retention_days.to_string(), now).await?;
    upsert_if_missing(pool, KEY_SESSION, &session_days.to_string(), now).await?;
    upsert_if_missing(
        pool,
        KEY_METRICS_RETENTION,
        &metrics_retention_days.to_string(),
        now,
    )
    .await?;
    Ok(())
}

async fn upsert_if_missing(
    pool: &SqlitePool,
    key: &str,
    value: &str,
    now_ms: i64,
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, updated_at)
        VALUES (?, ?, ?)
        ON CONFLICT(key) DO NOTHING
        "#,
    )
    .bind(key)
    .bind(value)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(())
}

async fn get_i64(pool: &SqlitePool, key: &str) -> AppResult<i64> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;

    match row {
        Some((value,)) => value
            .parse()
            .map_err(|_| AppError::internal(format!("invalid setting {key}"))),
        None => Err(AppError::internal(format!("missing setting {key}"))),
    }
}

pub async fn get_settings(pool: &SqlitePool) -> AppResult<Settings> {
    Ok(Settings {
        retention_days: get_i64(pool, KEY_RETENTION).await?,
        session_days: get_i64(pool, KEY_SESSION).await?,
        metrics_retention_days: get_i64(pool, KEY_METRICS_RETENTION).await.unwrap_or(14),
    })
}

pub async fn set_retention_days(pool: &SqlitePool, days: i64) -> AppResult<()> {
    set_value(pool, KEY_RETENTION, &days.to_string()).await
}

pub async fn set_session_days(pool: &SqlitePool, days: i64) -> AppResult<()> {
    set_value(pool, KEY_SESSION, &days.to_string()).await
}

pub async fn set_metrics_retention_days(pool: &SqlitePool, days: i64) -> AppResult<()> {
    set_value(pool, KEY_METRICS_RETENTION, &days.to_string()).await
}

async fn set_value(pool: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, updated_at)
        VALUES (?, ?, ?)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at
        "#,
    )
    .bind(key)
    .bind(value)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
