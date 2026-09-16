use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Anonymous,
    Login,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::Anonymous => "anonymous",
            AuthMode::Login => "login",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub logs_database: PathBuf,
    pub system_database: PathBuf,
    pub metrics_database: PathBuf,
    pub api_key: Option<String>,
    pub client_token: Option<String>,
    pub auth_mode: AuthMode,
    pub retention_days: i64,
    pub session_days: i64,
    pub cookie_secure: bool,
    pub max_body_mb: usize,
    pub max_batch: usize,
    pub cors_origin: Option<String>,
    pub web_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Self {
        let retention_days = env_i64("TINY_LOG_RETENTION_DAYS", 30).clamp(1, 3650);
        let session_days = env_i64("TINY_LOG_SESSION_DAYS", 7).max(1);
        let (logs_database, system_database, metrics_database) = resolve_database_paths();

        Self {
            host: env::var("TINY_LOG_HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env_u16("TINY_LOG_PORT", 8080),
            logs_database,
            system_database,
            metrics_database,
            api_key: env_nonempty("TINY_LOG_API_KEY"),
            client_token: env_nonempty("TINY_LOG_CLIENT_TOKEN"),
            auth_mode: resolve_auth_mode(),
            retention_days,
            session_days,
            cookie_secure: env_bool("TINY_LOG_COOKIE_SECURE", true),
            max_body_mb: env_usize("TINY_LOG_MAX_BODY_MB", 1).max(1),
            max_batch: env_usize("TINY_LOG_MAX_BATCH", 500).min(500).max(1),
            cors_origin: env_nonempty("TINY_LOG_CORS_ORIGIN"),
            web_dir: resolve_web_dir(),
        }
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn logs_database_url(&self) -> String {
        sqlite_url(&self.logs_database)
    }

    pub fn system_database_url(&self) -> String {
        sqlite_url(&self.system_database)
    }

    pub fn metrics_database_url(&self) -> String {
        sqlite_url(&self.metrics_database)
    }

    pub fn is_anonymous(&self) -> bool {
        self.auth_mode == AuthMode::Anonymous
    }
}

fn resolve_auth_mode() -> AuthMode {
    if let Ok(mode) = env::var("TINY_LOG_AUTH_MODE") {
        match mode.to_lowercase().as_str() {
            "anonymous" | "anon" | "none" => return AuthMode::Anonymous,
            "login" | "user" | "auth" => return AuthMode::Login,
            _ => {}
        }
    }
    // Backward compat
    if env::var("TINY_LOG_REQUIRE_AUTH").is_ok() {
        if env_bool("TINY_LOG_REQUIRE_AUTH", true) {
            AuthMode::Login
        } else {
            AuthMode::Anonymous
        }
    } else {
        AuthMode::Login
    }
}

fn resolve_web_dir() -> PathBuf {
    if let Some(dir) = env_nonempty("TINY_LOG_WEB_DIR") {
        return PathBuf::from(dir);
    }
    for candidate in ["./server/web", "./web"] {
        if Path::new(candidate).is_dir() {
            return PathBuf::from(candidate);
        }
    }
    PathBuf::from("./server/web")
}

fn sqlite_url(path: &Path) -> String {
    format!("sqlite:{}?mode=rwc", path.display())
}

fn resolve_database_paths() -> (PathBuf, PathBuf, PathBuf) {
    let logs = env_nonempty("TINY_LOG_LOGS_DATABASE").map(PathBuf::from);
    let system = env_nonempty("TINY_LOG_SYSTEM_DATABASE").map(PathBuf::from);
    let metrics = env_nonempty("TINY_LOG_METRICS_DATABASE").map(PathBuf::from);

    if let (Some(logs), Some(system), Some(metrics)) =
        (logs.clone(), system.clone(), metrics.clone())
    {
        return (logs, system, metrics);
    }

    let data_dir = if let Some(legacy) = env_nonempty("TINY_LOG_DATABASE") {
        let legacy_path = PathBuf::from(&legacy);
        legacy_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from("./data")
    };

    (
        logs.unwrap_or_else(|| data_dir.join("logs.db")),
        system.unwrap_or_else(|| data_dir.join("system.db")),
        metrics.unwrap_or_else(|| data_dir.join("metrics.db")),
    )
}

fn env_nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(v) => matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => default,
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
