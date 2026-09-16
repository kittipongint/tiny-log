use crate::config::Config;
use crate::models::log::LogEntry;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex};

pub type LogBroadcast = broadcast::Sender<LogEntry>;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    pub log_tx: LogBroadcast,
    pub login_limiter: Arc<Mutex<LoginLimiter>>,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        if let Some(parent) = config.database.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        let options = SqliteConnectOptions::from_str(&config.database_url())?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_millis(5000))
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .min_connections(1)
            .connect_with(options)
            .await?;

        sqlx::query("PRAGMA auto_vacuum = INCREMENTAL;")
            .execute(&pool)
            .await?;

        let (log_tx, _) = broadcast::channel(1024);

        Ok(Self {
            pool,
            config: Arc::new(config),
            log_tx,
            login_limiter: Arc::new(Mutex::new(LoginLimiter::new())),
        })
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        crate::db::settings::ensure_defaults(
            &self.pool,
            self.config.retention_days,
            self.config.session_days,
        )
        .await?;
        Ok(())
    }
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
