use crate::config::Config;
use crate::models::log::LogEntry;
use sqlx::sqlite::{
    SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
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
    /// One-time token that POST /api/auth/setup must present. None once an admin exists.
    pub setup_token: Arc<std::sync::Mutex<Option<String>>>,
    /// Serialises setup so two callers can't both pass the "no admin yet" check.
    pub setup_lock: Arc<Mutex<()>>,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        ensure_db_path(&config.logs_database).await?;
        ensure_db_path(&config.system_database).await?;
        ensure_db_path(&config.metrics_database).await?;

        let logs_db = connect_pool(&config.logs_database_url(), 5)
            .await
            .map_err(|e| db_open_err("logs", &config.logs_database, e))?;
        let system_db = connect_pool(&config.system_database_url(), 2)
            .await
            .map_err(|e| db_open_err("system", &config.system_database, e))?;
        let metrics_db = connect_pool(&config.metrics_database_url(), 2)
            .await
            .map_err(|e| db_open_err("metrics", &config.metrics_database, e))?;

        harden_pool(&logs_db, "logs").await?;
        harden_pool(&system_db, "system").await?;
        harden_pool(&metrics_db, "metrics").await?;

        let (broadcaster, _) = broadcast::channel(1024);

        Ok(Self {
            logs_db,
            system_db,
            metrics_db,
            broadcaster,
            config: Arc::new(config),
            login_limiter: Arc::new(Mutex::new(LoginLimiter::new())),
            setup_token: Arc::new(std::sync::Mutex::new(None)),
            setup_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Arm first-run setup: while no admin exists, only a caller holding this token may
    /// create one. Uses TINY_LOG_SETUP_TOKEN, or generates a token and logs it once.
    pub async fn prepare_setup_token(&self) -> anyhow::Result<()> {
        if crate::db::admin::get_admin(&self.system_db).await?.is_some() {
            *self.setup_token.lock().unwrap() = None;
            return Ok(());
        }
        let token = match &self.config.setup_token {
            Some(t) => {
                tracing::warn!(
                    "setup_required — no admin yet; open /setup and enter TINY_LOG_SETUP_TOKEN, or run `tiny-log admin create`"
                );
                t.clone()
            }
            None => {
                let t = crate::auth::session::generate_session_id();
                tracing::warn!(
                    "setup_required — no admin yet; open /setup#token={t} (one-time token, valid until setup completes), or run `tiny-log admin create`"
                );
                t
            }
        };
        *self.setup_token.lock().unwrap() = Some(token);
        Ok(())
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

async fn ensure_db_path(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                anyhow::anyhow!(
                    "cannot create database directory {}: {err}",
                    parent.display()
                )
            })?;

            // Probe writability early — SQLite code 14 is otherwise opaque.
            let probe = parent.join(".tiny-log-write-test");
            match tokio::fs::write(&probe, b"ok").await {
                Ok(()) => {
                    let _ = tokio::fs::remove_file(&probe).await;
                }
                Err(err) => {
                    anyhow::bail!(
                        "cannot write to {} ({err}). \
                         If using Docker, ensure the volume is writable by uid 10001 \
                         (entrypoint should chown /data automatically).",
                        parent.display()
                    );
                }
            }
        }
    }
    Ok(())
}

fn db_open_err(name: &str, path: &Path, err: anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "unable to open {name} database at {}: {err}",
        path.display()
    )
}

/// Every pooled connection gets the same settings: sqlx runs these PRAGMAs on each new
/// connection, whereas a one-off `query("PRAGMA …").execute(pool)` only reaches whichever
/// connection the pool hands out. auto_vacuum only takes effect on a brand-new file
/// (sqlx sets it before journal_mode); an existing file needs one full VACUUM to switch.
pub(crate) fn connect_options(url: &str) -> anyhow::Result<SqliteConnectOptions> {
    Ok(SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .auto_vacuum(SqliteAutoVacuum::Incremental)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(10))
        .foreign_keys(true)
        // keep temp tables/sorts off disk
        .pragma("temp_store", "MEMORY")
        // 64 MiB WAL cap after checkpoint reset
        .pragma("journal_size_limit", "67108864")
        // ~20 MiB page cache per connection (negative = KiB)
        .pragma("cache_size", "-20000")
        .pragma("wal_autocheckpoint", "1000"))
}

async fn connect_pool(url: &str, max_connections: u32) -> anyhow::Result<SqlitePool> {
    Ok(SqlitePoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(15))
        .idle_timeout(Some(Duration::from_secs(600)))
        .connect_with(connect_options(url)?)
        .await?)
}

/// Fail fast on corruption; say so when an old file can't reclaim space incrementally.
async fn harden_pool(pool: &SqlitePool, name: &str) -> anyhow::Result<()> {
    let auto_vacuum: i64 = sqlx::query_scalar("PRAGMA auto_vacuum;")
        .fetch_one(pool)
        .await?;
    if auto_vacuum != 2 {
        tracing::warn!(
            db = name,
            auto_vacuum,
            "auto_vacuum is not INCREMENTAL on this existing file — deleted space is reused but the file never shrinks; run `VACUUM` once while the server is stopped to switch"
        );
    }

    let check: String = sqlx::query_scalar("PRAGMA quick_check;")
        .fetch_one(pool)
        .await?;
    if check != "ok" {
        anyhow::bail!("{name} database failed quick_check: {check}");
    }
    tracing::info!(db = name, "sqlite_ready");
    Ok(())
}

pub async fn checkpoint_all(
    logs_db: &SqlitePool,
    system_db: &SqlitePool,
    metrics_db: &SqlitePool,
) {
    for (name, pool) in [
        ("logs", logs_db),
        ("system", system_db),
        ("metrics", metrics_db),
    ] {
        match sqlx::query("PRAGMA wal_checkpoint(PASSIVE);")
            .execute(pool)
            .await
        {
            Ok(_) => {}
            Err(err) => tracing::warn!(db = name, error = %err, "wal_checkpoint_failed"),
        }
    }
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
