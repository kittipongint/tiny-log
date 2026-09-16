use crate::error::{AppError, AppResult};
use crate::models::admin::AdminUser;
use sqlx::SqlitePool;

pub async fn get_admin(pool: &SqlitePool) -> AppResult<Option<AdminUser>> {
    let admin = sqlx::query_as::<_, AdminUser>(
        "SELECT id, username, password_hash, created_at, updated_at FROM admin_user WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(admin)
}

pub async fn create_admin(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
    now_ms: i64,
) -> AppResult<()> {
    if get_admin(pool).await?.is_some() {
        return Err(AppError::Conflict("admin user already exists".into()));
    }

    sqlx::query(
        r#"
        INSERT INTO admin_user (id, username, password_hash, created_at, updated_at)
        VALUES (1, ?, ?, ?, ?)
        "#,
    )
    .bind(username)
    .bind(password_hash)
    .bind(now_ms)
    .bind(now_ms)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn update_password(
    pool: &SqlitePool,
    password_hash: &str,
    now_ms: i64,
) -> AppResult<()> {
    let result = sqlx::query(
        "UPDATE admin_user SET password_hash = ?, updated_at = ? WHERE id = 1",
    )
    .bind(password_hash)
    .bind(now_ms)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}
