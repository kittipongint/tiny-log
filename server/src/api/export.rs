//! GET /api/v1/logs/export — every log matching the list filters, as NDJSON or CSV.
//!
//! Streams page by page (keyset, newest first), so a million-row export holds one
//! page in memory, not the file.

use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::log::{LogEntry, LogQuery};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Deserialize;
use sqlx::SqlitePool;

const PAGE: i64 = 1_000;
pub const EXPORT_MAX: i64 = 1_000_000;

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    pub format: Option<String>,
    pub app: Option<String>,
    pub level: Option<String>,
    pub source: Option<String>,
    pub search: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    /// Max rows (default and cap: EXPORT_MAX).
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    Ndjson,
    Csv,
}

impl Format {
    fn parse(raw: Option<&str>) -> AppResult<Self> {
        match raw.unwrap_or("ndjson").to_ascii_lowercase().as_str() {
            "ndjson" | "jsonl" => Ok(Self::Ndjson),
            "csv" => Ok(Self::Csv),
            other => Err(AppError::bad_request(format!(
                "invalid format: {other} (ndjson or csv)"
            ))),
        }
    }
    fn content_type(self) -> &'static str {
        match self {
            Self::Ndjson => "application/x-ndjson; charset=utf-8",
            Self::Csv => "text/csv; charset=utf-8",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Self::Ndjson => "ndjson",
            Self::Csv => "csv",
        }
    }
}

struct Cursor {
    pool: SqlitePool,
    filters: LogQuery,
    format: Format,
    first: Option<Vec<(i64, LogEntry)>>,
    before: Option<(i64, i64)>,
    remaining: i64,
    started: bool,
    done: bool,
}

pub async fn export_logs(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ExportQuery>,
) -> AppResult<Response> {
    let format = Format::parse(q.format.as_deref())?;
    let max = q.limit.unwrap_or(EXPORT_MAX).clamp(1, EXPORT_MAX);
    let app_label = q.app.clone().filter(|a| !a.is_empty());
    let filters = LogQuery {
        app: q.app,
        level: q.level,
        source: q.source,
        search: q.search,
        from: q.from,
        to: q.to,
        limit: None,
        offset: None,
    };

    // First page before the 200 goes out: a bad `from` or a DB error is a proper
    // JSON error instead of a download that stops half way.
    let first = db::logs::export_page(&state.logs_db, &filters, None, PAGE.min(max)).await?;

    let cursor = Cursor {
        pool: state.logs_db.clone(),
        filters,
        format,
        first: Some(first),
        before: None,
        remaining: max,
        started: false,
        done: false,
    };
    let stream = futures::stream::unfold(cursor, next_chunk);

    let filename = format!(
        "tiny-log-{}-{}.{}",
        app_label.as_deref().map(safe_name).unwrap_or_else(|| "all".into()),
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        format.ext()
    );
    let mut res = Body::from_stream(stream).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(format.content_type()));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

async fn next_chunk(mut c: Cursor) -> Option<(Result<Bytes, std::io::Error>, Cursor)> {
    if c.done {
        return None;
    }
    let mut buf = String::new();
    if !c.started {
        c.started = true;
        if c.format == Format::Csv {
            // BOM so Excel reads UTF-8 (Thai) instead of mojibake.
            buf.push('\u{feff}');
            buf.push_str("id,timestamp,app,level,source,message,meta\r\n");
        }
    }

    let want = PAGE.min(c.remaining);
    let rows = match c.first.take() {
        Some(rows) => rows,
        None if want <= 0 || c.before.is_none() => Vec::new(),
        None => match db::logs::export_page(&c.pool, &c.filters, c.before, want).await {
            Ok(rows) => rows,
            Err(err) => {
                // Abort the body: the client sees a failed download, not a silently short file.
                tracing::error!(error = %err, "export_failed");
                c.done = true;
                return Some((Err(std::io::Error::other("export failed")), c));
            }
        },
    };

    if (rows.len() as i64) < want || rows.is_empty() {
        c.done = true;
    }
    c.remaining -= rows.len() as i64;
    if c.remaining <= 0 {
        c.done = true;
    }
    if let Some((ts, e)) = rows.last() {
        c.before = Some((*ts, e.id));
    }

    for (_, e) in &rows {
        match c.format {
            Format::Ndjson => {
                if let Ok(line) = serde_json::to_string(e) {
                    buf.push_str(&line);
                    buf.push('\n');
                }
            }
            Format::Csv => write_csv_row(&mut buf, e),
        }
    }

    if buf.is_empty() {
        return None;
    }
    Some((Ok(Bytes::from(buf)), c))
}

fn write_csv_row(out: &mut String, e: &LogEntry) {
    let meta = e
        .meta
        .as_ref()
        .map(|m| m.to_string())
        .unwrap_or_default();
    let cells = [
        e.id.to_string(),
        e.timestamp.clone(),
        e.app.clone(),
        e.level.clone(),
        e.source.clone().unwrap_or_default(),
        e.message.clone(),
        meta,
    ];
    for (i, v) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        csv_cell(out, v);
    }
    out.push_str("\r\n");
}

/// RFC 4180 quoting, plus a leading ' on cells a spreadsheet would run as a formula.
pub(crate) fn csv_cell(out: &mut String, v: &str) {
    let formula = matches!(v.chars().next(), Some('=' | '+' | '-' | '@' | '\t' | '\r'));
    if !formula && !v.contains([',', '"', '\n', '\r']) {
        out.push_str(v);
        return;
    }
    out.push('"');
    if formula {
        out.push('\'');
    }
    for ch in v.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
}

fn safe_name(app: &str) -> String {
    let s: String = app
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .take(40)
        .collect();
    if s.is_empty() { "app".into() } else { s }
}
