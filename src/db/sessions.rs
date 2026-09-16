use crate::error::AppResult;
use crate::models::session::Session;
use sqlx::SqlitePool;

pub async fn create_session(
    pool: &SqlitePool,
    id: &str,
    admin_id: i64,
    expires_at: i64,
    now_ms: i64,
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO sessions (id, admin_id, expires_at, created_at, last_seen_at)
        VALUES (?, ?, ?, ?, ?)
        "#,
    )
    .bind(id)
    .bind(admin_id)
    .bind(expires_at)
    .bind(now_ms)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_session(pool: &SqlitePool, id: &str) -> AppResult<Option<Session>> {
    let session = sqlx::query_as::<_, Session>(
        "SELECT id, admin_id, expires_at, created_at, last_seen_at FROM sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(session)
}

pub async fn touch_session(pool: &SqlitePool, id: &str, now_ms: i64) -> AppResult<()> {
    sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE id = ?")
        .bind(now_ms)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_session(pool: &SqlitePool, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_all_sessions(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query("DELETE FROM sessions").execute(pool).await?;
    Ok(())
}

pub async fn delete_expired(pool: &SqlitePool, now_ms: i64) -> AppResult<u64> {
    let result = sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
        .bind(now_ms)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
