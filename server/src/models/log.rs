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

        let now_ms = Utc::now().timestamp_millis();
        let timestamp_ms = match self.timestamp {
            Some(ts) => clamp_timestamp_ms(parse_timestamp_ms(&ts)?, now_ms),
            None => now_ms,
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

/// Numbers below this are unix seconds, not milliseconds (as ms they'd be before 1973-03).
const SECONDS_BELOW: f64 = 100_000_000_000.0;
/// Clocks this far ahead are wrong; such rows would otherwise sit on top of every view.
const MAX_FUTURE_MS: i64 = 24 * 60 * 60 * 1000;

/// Accepts RFC 3339, unix milliseconds, or unix seconds (10 digits, optionally fractional).
pub fn parse_timestamp_ms(raw: &str) -> Result<i64, String> {
    let raw = raw.trim();
    if let Ok(n) = raw.parse::<f64>() {
        if !n.is_finite() {
            return Err(format!("invalid timestamp: {raw}"));
        }
        let ms = if n.abs() < SECONDS_BELOW { n * 1000.0 } else { n };
        return Ok(ms.round() as i64);
    }

    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.timestamp_millis())
        .or_else(|_| {
            raw.parse::<DateTime<Utc>>()
                .map(|dt| dt.timestamp_millis())
        })
        .map_err(|_| format!("invalid timestamp: {raw}"))
}

/// Before 1970 or more than a day ahead means the sender's clock or format is broken:
/// store receive time instead so the row is neither swept by retention nor pinned on top.
pub fn clamp_timestamp_ms(ts_ms: i64, now_ms: i64) -> i64 {
    if ts_ms < 0 || ts_ms > now_ms + MAX_FUTURE_MS {
        now_ms
    } else {
        ts_ms
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000_000; // 2026-09-21

    #[test]
    fn unix_seconds_are_not_1970() {
        assert_eq!(parse_timestamp_ms("1790000000").unwrap(), 1_790_000_000_000);
        assert_eq!(parse_timestamp_ms("1790000000.25").unwrap(), 1_790_000_000_250);
    }

    #[test]
    fn unix_millis_and_rfc3339_unchanged() {
        assert_eq!(parse_timestamp_ms("1790000000123").unwrap(), 1_790_000_000_123);
        assert_eq!(
            parse_timestamp_ms("2026-09-15T15:20:31.123Z").unwrap(),
            DateTime::parse_from_rfc3339("2026-09-15T15:20:31.123Z")
                .unwrap()
                .timestamp_millis()
        );
        assert!(parse_timestamp_ms("yesterday").is_err());
        assert!(parse_timestamp_ms("NaN").is_err());
    }

    #[test]
    fn clamp_keeps_sane_and_replaces_broken() {
        assert_eq!(clamp_timestamp_ms(NOW - 5_000, NOW), NOW - 5_000);
        assert_eq!(clamp_timestamp_ms(NOW + 60_000, NOW), NOW + 60_000);
        assert_eq!(clamp_timestamp_ms(NOW + 2 * MAX_FUTURE_MS, NOW), NOW);
        assert_eq!(clamp_timestamp_ms(-1, NOW), NOW);
        // old-but-real rows stay as sent; retention decides their fate
        assert_eq!(clamp_timestamp_ms(0, NOW), 0);
    }
}
