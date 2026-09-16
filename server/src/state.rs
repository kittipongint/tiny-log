use crate::config::Config;
use crate::models::log::LogEntry;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex};

pub type LogBroadcast = broadcast::Sender<LogEntry>;

#[derive(Clone)]
pub struct AppState {
    pub logs_db: SqlitePool,
    pub system_db: SqlitePool,
    pub metrics_db: SqlitePool,
    pub broadcaster: LogBroadcast,
    pub config: Arc<Config>,
    pub login_limiter: Arc<Mutex<LoginLimiter>>,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        ensure_parent_dir(&config.logs_database).await?;
        ensure_parent_dir(&config.system_database).await?;
        ensure_parent_dir(&config.metrics_database).await?;

        let logs_db = connect_pool(&config.logs_database_url(), 5).await?;
        let system_db = connect_pool(&config.system_database_url(), 2).await?;
        let metrics_db = connect_pool(&config.metrics_database_url(), 2).await?;

        sqlx::query("PRAGMA auto_vacuum = INCREMENTAL;")
            .execute(&logs_db)
            .await?;
        sqlx::query("PRAGMA auto_vacuum = INCREMENTAL;")
            .execute(&metrics_db)
            .await?;

        let (broadcaster, _) = broadcast::channel(1024);

        Ok(Self {
            logs_db,
            system_db,
            metrics_db,
            broadcaster,
            config: Arc::new(config),
            login_limiter: Arc::new(Mutex::new(LoginLimiter::new())),
        })
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        maybe_split_legacy_database(
            &self.config.logs_database,
            &self.logs_db,
            &self.system_db,
        )
        .await?;

        sqlx::migrate!("./migrations/logs")
            .run(&self.logs_db)
            .await?;
        sqlx::migrate!("./migrations/system")
            .run(&self.system_db)
            .await?;
        sqlx::migrate!("./migrations/metrics")
            .run(&self.metrics_db)
            .await?;

        let _ = sqlx::query("ALTER TABLE logs DROP COLUMN created_at;")
            .execute(&self.logs_db)
            .await;

        crate::db::settings::ensure_defaults(
            &self.system_db,
            self.config.retention_days,
            self.config.session_days,
            14,
        )
        .await?;
        Ok(())
    }
}

async fn ensure_parent_dir(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    Ok(())
}

async fn connect_pool(url: &str, max_connections: u32) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_millis(5000))
        .foreign_keys(true);

    Ok(SqlitePoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .connect_with(options)
        .await?)
}

/// If an older single-file DB still holds auth tables, copy them into system.db
/// once, then drop those tables from logs.db.
async fn maybe_split_legacy_database(
    logs_path: &Path,
    logs_db: &SqlitePool,
    system_db: &SqlitePool,
) -> anyhow::Result<()> {
    if !logs_path.exists() {
        return Ok(());
    }

    let has_admin: bool = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'admin_user' LIMIT 1",
    )
    .fetch_optional(logs_db)
    .await?
    .is_some();

    if !has_admin {
        return Ok(());
    }

    let system_admin_count: i64 = match sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM admin_user",
    )
    .fetch_one(system_db)
    .await
    {
        Ok(n) => n,
        Err(_) => 0,
    };

    if system_admin_count > 0 {
        tracing::info!("legacy_split_skipped reason=system_db_already_initialized");
        return Ok(());
    }

    tracing::info!(logs = %logs_path.display(), "legacy_split_start");

    sqlx::migrate!("./migrations/system").run(system_db).await?;

    let admins: Vec<(i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, username, password_hash, created_at, updated_at FROM admin_user",
    )
    .fetch_all(logs_db)
    .await?;

    for (id, username, password_hash, created_at, updated_at) in &admins {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO admin_user
                (id, username, password_hash, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(id)
        .bind(username)
        .bind(password_hash)
        .bind(created_at)
        .bind(updated_at)
        .execute(system_db)
        .await?;
    }

    let sessions: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT id, admin_id, expires_at, created_at, last_seen_at FROM sessions",
    )
    .fetch_all(logs_db)
    .await
    .unwrap_or_default();

    for (id, admin_id, expires_at, created_at, last_seen_at) in &sessions {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO sessions
                (id, admin_id, expires_at, created_at, last_seen_at)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(id)
        .bind(admin_id)
        .bind(expires_at)
        .bind(created_at)
        .bind(last_seen_at)
        .execute(system_db)
        .await?;
    }

    let settings: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT key, value, updated_at FROM settings")
            .fetch_all(logs_db)
            .await
            .unwrap_or_default();

    for (key, value, updated_at) in &settings {
        sqlx::query(
            r#"
            INSERT INTO settings (key, value, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(key)
        .bind(value)
        .bind(updated_at)
        .execute(system_db)
        .await?;
    }

    sqlx::query("DROP TABLE IF EXISTS sessions;")
        .execute(logs_db)
        .await?;
    sqlx::query("DROP TABLE IF EXISTS admin_user;")
        .execute(logs_db)
        .await?;
    sqlx::query("DROP TABLE IF EXISTS settings;")
        .execute(logs_db)
        .await?;
    sqlx::query("DROP TABLE IF EXISTS _sqlx_migrations;")
        .execute(logs_db)
        .await?;

    tracing::info!("legacy_split_ok");
    Ok(())
}

#[derive(Debug)]
pub struct LoginLimiter {
    attempts: HashMap<String, AttemptWindow>,
}

#[derive(Debug)]
struct AttemptWindow {
    count: u32,
    window_start: Instant,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self {
            attempts: HashMap::new(),
        }
    }

    pub fn check_allowed(&mut self, ip: &str) -> bool {
        self.cleanup();
        match self.attempts.get(ip) {
            Some(window)
                if window.window_start.elapsed() < Duration::from_secs(300)
                    && window.count >= 5 =>
            {
                false
            }
            _ => true,
        }
    }

    pub fn record_failure(&mut self, ip: &str) {
        self.cleanup();
        let entry = self
            .attempts
            .entry(ip.to_string())
            .or_insert_with(|| AttemptWindow {
                count: 0,
                window_start: Instant::now(),
            });

        if entry.window_start.elapsed() >= Duration::from_secs(300) {
            entry.count = 0;
            entry.window_start = Instant::now();
        }

        entry.count = entry.count.saturating_add(1);
    }

    pub fn clear(&mut self, ip: &str) {
        self.attempts.remove(ip);
    }

    fn cleanup(&mut self) {
        self.attempts
            .retain(|_, w| w.window_start.elapsed() < Duration::from_secs(600));
    }
}
