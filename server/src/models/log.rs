use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VALID_LEVELS: &[&str] = &["debug", "info", "warn", "error", "fatal"];

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_META_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: i64,
    pub timestamp: String,
    pub app: String,
    pub level: String,
    pub source: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewLog {
    pub app: String,
    pub level: String,
    pub message: String,
    pub timestamp: Option<String>,
    pub source: Option<String>,
    pub meta: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct InsertLog {
    pub timestamp_ms: i64,
    pub app: String,
    pub level: String,
    pub source: Option<String>,
    pub message: String,
    pub meta_json: Option<String>,
}

impl NewLog {
    pub fn validate_and_normalize(self) -> Result<InsertLog, String> {
        let app = self.app.trim().to_string();
        if app.is_empty() {
            return Err("app is required".into());
        }
        if app.len() > 128 {
            return Err("app is too long".into());
        }

        let level = self.level.trim().to_lowercase();
        if !VALID_LEVELS.contains(&level.as_str()) {
            return Err(format!("invalid level: {}", self.level));
        }

        let message = self.message;
        if message.is_empty() {
            return Err("message is required".into());
        }
        if message.len() > MAX_MESSAGE_BYTES {
            return Err("message exceeds maximum size".into());
        }

        let meta_json = match self.meta {
            Some(meta) => {
                let encoded = serde_json::to_string(&meta)
                    .map_err(|_| "invalid meta json".to_string())?;
                if encoded.len() > MAX_META_BYTES {
                    return Err("metadata exceeds maximum size".into());
                }
                Some(encoded)
            }
            None => None,
        };

        let source = self
            .source
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(ref s) = source {
            if s.len() > 128 {
                return Err("source is too long".into());
            }
        }

        let now = Utc::now();
        let timestamp_ms = match self.timestamp {
            Some(ts) => parse_timestamp_ms(&ts)?,
            None => now.timestamp_millis(),
        };

        Ok(InsertLog {
            timestamp_ms,
            app,
            level,
            source,
            message,
            meta_json,
        })
    }
}

fn parse_timestamp_ms(raw: &str) -> Result<i64, String> {
    if let Ok(ms) = raw.parse::<i64>() {
        return Ok(ms);
    }

    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.timestamp_millis())
        .or_else(|_| {
            raw.parse::<DateTime<Utc>>()
                .map(|dt| dt.timestamp_millis())
        })
        .map_err(|_| format!("invalid timestamp: {raw}"))
}

pub fn ms_to_rfc3339(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap())
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[derive(Debug, Deserialize)]
pub struct BatchLogsRequest {
    pub logs: Vec<NewLog>,
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    pub app: Option<String>,
    pub level: Option<String>,
    pub source: Option<String>,
    pub search: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
