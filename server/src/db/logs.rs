use crate::error::{AppError, AppResult};
use crate::models::log::{ms_to_rfc3339, InsertLog, LogEntry, LogQuery};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

pub async fn insert_log(pool: &SqlitePool, log: &InsertLog) -> AppResult<i64> {
    let result = sqlx::query(
        r#"
        INSERT INTO logs (timestamp_ms, app, level, source, message, meta_json)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
    )
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
        let result = sqlx::query(
            r#"
            INSERT INTO logs (timestamp_ms, app, level, source, message, meta_json)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
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

    if let Some(app) = query.app.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND app = ");
        qb.push_bind(app);
    }

    if let Some(level) = query.level.as_ref().filter(|s| !s.is_empty()) {
        let level = level.to_lowercase();
        qb.push(" AND level = ");
        qb.push_bind(level);
    }

    if let Some(source) = query.source.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND source = ");
        qb.push_bind(source);
    }

    if let Some(search) = query.search.as_ref().filter(|s| !s.is_empty()) {
        let pattern = format!("%{search}%");
        qb.push(" AND message LIKE ");
        qb.push_bind(pattern);
    }

    if let Some(from) = &query.from {
        let ms = parse_bound(from)?;
        qb.push(" AND timestamp_ms >= ");
        qb.push_bind(ms);
    }

    if let Some(to) = &query.to {
        let ms = parse_bound(to)?;
        qb.push(" AND timestamp_ms <= ");
        qb.push_bind(ms);
    }

    qb.push(" ORDER BY timestamp_ms DESC, id DESC LIMIT ");
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
