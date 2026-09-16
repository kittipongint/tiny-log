use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub database: PathBuf,
    pub api_key: Option<String>,
    pub client_token: Option<String>,
    pub require_auth: bool,
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

        Self {
            host: env::var("TINY_LOG_HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env_u16("TINY_LOG_PORT", 8080),
            database: PathBuf::from(
                env::var("TINY_LOG_DATABASE").unwrap_or_else(|_| "./data/logs.db".into()),
            ),
            api_key: env_nonempty("TINY_LOG_API_KEY"),
            client_token: env_nonempty("TINY_LOG_CLIENT_TOKEN"),
            require_auth: env_bool("TINY_LOG_REQUIRE_AUTH", true),
            retention_days,
            session_days,
            cookie_secure: env_bool("TINY_LOG_COOKIE_SECURE", true),
            max_body_mb: env_usize("TINY_LOG_MAX_BODY_MB", 1).max(1),
            max_batch: env_usize("TINY_LOG_MAX_BATCH", 500).min(500).max(1),
            cors_origin: env_nonempty("TINY_LOG_CORS_ORIGIN"),
            web_dir: PathBuf::from(env::var("TINY_LOG_WEB_DIR").unwrap_or_else(|_| "./web".into())),
        }
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn database_url(&self) -> String {
        let path = self.database.display();
        format!("sqlite:{path}?mode=rwc")
    }
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
