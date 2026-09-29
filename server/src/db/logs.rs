use crate::error::{AppError, AppResult};
use crate::models::log::{ms_to_rfc3339, InsertLog, LogEntry, LogQuery};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

/// Insert with the line's correlation id read from its meta (?6). Same expression as the backfill
/// in migrations/logs/0002_trace_id.sql.
const INSERT_SQL: &str = "INSERT INTO logs (timestamp_ms, app, level, source, message, meta_json, trace_id) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULLIF(substr(CAST(COALESCE(\
    json_extract(?6, '$.request_id'), \
    json_extract(?6, '$.correlation_id'), \
    json_extract(?6, '$.trace_id')) AS TEXT), 1, 128), ''))";

pub async fn insert_log(pool: &SqlitePool, log: &InsertLog) -> AppResult<i64> {
    let result = sqlx::query(INSERT_SQL)
    .bind(log.timestamp_ms)
    .bind(&log.app)
    .bind(&log.level)
    .bind(&log.source)
    .bind(&log.message)
    .bind(&log.meta_json)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

pub async fn insert_logs_batch(pool: &SqlitePool, logs: &[InsertLog]) -> AppResult<Vec<i64>> {
    let mut tx = pool.begin().await?;
    let mut ids = Vec::with_capacity(logs.len());

    for log in logs {
        let result = sqlx::query(INSERT_SQL)
        .bind(log.timestamp_ms)
        .bind(&log.app)
        .bind(&log.level)
        .bind(&log.source)
        .bind(&log.message)
        .bind(&log.meta_json)
        .execute(&mut *tx)
        .await?;

        ids.push(result.last_insert_rowid());
    }

    tx.commit().await?;
    Ok(ids)
}

#[derive(Debug, sqlx::FromRow)]
struct LogRow {
    id: i64,
    timestamp_ms: i64,
    app: String,
    level: String,
    source: Option<String>,
    message: String,
    meta_json: Option<String>,
}

impl LogRow {
    fn into_entry(self) -> AppResult<LogEntry> {
        let meta = match self.meta_json {
            Some(raw) if !raw.is_empty() => Some(
                serde_json::from_str(&raw)
                    .map_err(|e| AppError::internal(format!("corrupt meta_json: {e}")))?,
            ),
            _ => None,
        };

        Ok(LogEntry {
            id: self.id,
            timestamp: ms_to_rfc3339(self.timestamp_ms),
            app: self.app,
            level: self.level,
            source: self.source,
            message: self.message,
            meta,
        })
    }
}

pub async fn get_log(pool: &SqlitePool, id: i64) -> AppResult<Option<LogEntry>> {
    let row = sqlx::query_as::<_, LogRow>(
        r#"
        SELECT id, timestamp_ms, app, level, source, message, meta_json
        FROM logs
        WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    match row {
        Some(r) => Ok(Some(r.into_entry()?)),
        None => Ok(None),
    }
}

pub async fn query_logs(pool: &SqlitePool, query: &LogQuery) -> AppResult<Vec<LogEntry>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);

    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT id, timestamp_ms, app, level, source, message, meta_json FROM logs WHERE 1=1",
    );
    push_filters(&mut qb, query)?;
    if let Some(id) = query.before {
        qb.push(" AND (timestamp_ms, id) < (SELECT timestamp_ms, id FROM logs WHERE id = ");
        qb.push_bind(id);
        qb.push(")");
    }

    // A trace reads top to bottom in the order it happened.
    if query.order.as_deref() == Some("asc") {
        qb.push(" ORDER BY timestamp_ms ASC, id ASC LIMIT ");
    } else {
        qb.push(" ORDER BY timestamp_ms DESC, id DESC LIMIT ");
    }
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);

    let rows = qb.build_query_as::<LogRow>().fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(row.into_entry()?);
    }
    Ok(out)
}

/// One export page, newest first, strictly older than `before` = (timestamp_ms, id).
/// Keyset paging: every page costs the same however deep the export goes (OFFSET doesn't).
pub async fn export_page(
    pool: &SqlitePool,
    query: &LogQuery,
    before: Option<(i64, i64)>,
    limit: i64,
) -> AppResult<Vec<(i64, LogEntry)>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT id, timestamp_ms, app, level, source, message, meta_json FROM logs WHERE 1=1",
    );
    push_filters(&mut qb, query)?;
    if let Some((ts, id)) = before {
        qb.push(" AND (timestamp_ms < ");
        qb.push_bind(ts);
        qb.push(" OR (timestamp_ms = ");
        qb.push_bind(ts);
        qb.push(" AND id < ");
        qb.push_bind(id);
        qb.push("))");
    }
    qb.push(" ORDER BY timestamp_ms DESC, id DESC LIMIT ");
    qb.push_bind(limit);

    let rows = qb.build_query_as::<LogRow>().fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let ts = row.timestamp_ms;
        out.push((ts, row.into_entry()?));
    }
    Ok(out)
}

/// WHERE clauses shared by the log list and the export, so both show the same rows.
fn push_filters(qb: &mut QueryBuilder<Sqlite>, query: &LogQuery) -> AppResult<()> {
    if let Some(app) = query.app.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND app = ");
        qb.push_bind(app.clone());
    }

    if let Some(level) = query.level.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND level = ");
        qb.push_bind(level.to_lowercase());
    }

    if let Some(source) = query.source.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND source = ");
        qb.push_bind(source.clone());
    }

    if let Some(trace) = query.trace.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        qb.push(" AND trace_id = ");
        qb.push_bind(trace.to_string());
    }

    if let Some(search) = query.search.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND message LIKE ");
        qb.push_bind(format!("%{search}%"));
    }

    if let Some(from) = &query.from {
        qb.push(" AND timestamp_ms >= ");
        qb.push_bind(parse_bound(from)?);
    }

    if let Some(to) = &query.to {
        qb.push(" AND timestamp_ms <= ");
        qb.push_bind(parse_bound(to)?);
    }
    Ok(())
}

/// One request (or any group of lines sharing a correlation id) as a summary row.
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct TraceSummary {
    pub trace_id: String,
    pub first_ms: i64,
    pub last_ms: i64,
    pub lines: i64,
    /// Worst level in the trace: debug < info < warn < error < fatal.
    pub level: String,
    pub apps: Option<String>,
    pub sources: Option<String>,
    /// Message of the trace's first line (usually what started it).
    pub first_message: Option<String>,
}

/// Traces that have at least one line matching the filters, newest activity first. The counts
/// cover the whole trace, not only the matching lines. Without `from`, looks at the last 24 h so
/// the GROUP BY stays cheap on a big table.
pub async fn query_traces(pool: &SqlitePool, query: &LogQuery) -> AppResult<Vec<TraceSummary>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);

    let mut q = LogQuery { ..query.clone() };
    if q.from.as_deref().map_or(true, str::is_empty) {
        q.from = Some((chrono::Utc::now().timestamp_millis() - 24 * 60 * 60 * 1000).to_string());
    }

    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "WITH hit AS (SELECT trace_id, MAX(timestamp_ms) AS last_hit FROM logs \
         WHERE trace_id IS NOT NULL",
    );
    push_filters(&mut qb, &q)?;
    qb.push(" GROUP BY trace_id ORDER BY last_hit DESC LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);
    qb.push(
        ") SELECT l.trace_id AS trace_id, MIN(l.timestamp_ms) AS first_ms, \
         MAX(l.timestamp_ms) AS last_ms, COUNT(*) AS lines, \
         CASE MAX(CASE l.level WHEN 'fatal' THEN 4 WHEN 'error' THEN 3 WHEN 'warn' THEN 2 \
              WHEN 'info' THEN 1 ELSE 0 END) \
              WHEN 4 THEN 'fatal' WHEN 3 THEN 'error' WHEN 2 THEN 'warn' WHEN 1 THEN 'info' \
              ELSE 'debug' END AS level, \
         GROUP_CONCAT(DISTINCT l.app) AS apps, GROUP_CONCAT(DISTINCT l.source) AS sources, \
         (SELECT f.message FROM logs f WHERE f.trace_id = l.trace_id \
          ORDER BY f.timestamp_ms ASC, f.id ASC LIMIT 1) AS first_message \
         FROM logs l JOIN hit h ON h.trace_id = l.trace_id \
         GROUP BY l.trace_id ORDER BY MAX(h.last_hit) DESC",
    );
    Ok(qb.build_query_as::<TraceSummary>().fetch_all(pool).await?)
}

pub async fn list_apps(pool: &SqlitePool) -> AppResult<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT app FROM logs ORDER BY app ASC LIMIT 500",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(a,)| a).collect())
}

/// Rows per retention DELETE. Each chunk is its own short write transaction, so ingest
/// waiting on busy_timeout (10s) gets the lock between chunks instead of 500ing.
pub const DELETE_CHUNK: i64 = 5_000;

pub async fn delete_older_than(pool: &SqlitePool, cutoff_ms: i64) -> AppResult<u64> {
    delete_in_chunks(pool, "logs", cutoff_ms, DELETE_CHUNK).await
}

/// Tables with a `timestamp_ms` column that retention trims.
pub async fn delete_in_chunks(
    pool: &SqlitePool,
    table: &str,
    cutoff_ms: i64,
    chunk: i64,
) -> AppResult<u64> {
    let sql = match table {
        "logs" => "DELETE FROM logs WHERE rowid IN \
                   (SELECT rowid FROM logs WHERE timestamp_ms < ? LIMIT ?)",
        "host_samples" => "DELETE FROM host_samples WHERE rowid IN \
                   (SELECT rowid FROM host_samples WHERE timestamp_ms < ? LIMIT ?)",
        "service_checks" => "DELETE FROM service_checks WHERE rowid IN \
                   (SELECT rowid FROM service_checks WHERE timestamp_ms < ? LIMIT ?)",
        other => return Err(AppError::internal(format!("no retention for table {other}"))),
    };
    let mut total = 0u64;
    loop {
        let n = sqlx::query(sql)
            .bind(cutoff_ms)
            .bind(chunk)
            .execute(pool)
            .await?
            .rows_affected();
        total += n;
        if n < chunk as u64 {
            return Ok(total);
        }
        tokio::task::yield_now().await;
    }
}

pub async fn vacuum_incremental(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE);")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA incremental_vacuum;")
        .execute(pool)
        .await?;
    Ok(())
}

fn parse_bound(raw: &str) -> AppResult<i64> {
    if let Ok(ms) = raw.parse::<i64>() {
        return Ok(ms);
    }

    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.timestamp_millis())
        .map_err(|_| AppError::bad_request(format!("invalid time bound: {raw}")))
}
