use crate::models::log::ms_to_rfc3339;
use crate::models::metrics::{
    HostSample, HostSampleInput, MetricsBatchRequest, MetricsHistoryQuery, ServiceCheck,
    ServiceCheckInput,
};
use crate::error::{AppError, AppResult};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

pub async fn insert_batch(
    pool: &SqlitePool,
    host: &str,
    timestamp_ms: i64,
    system: Option<&HostSampleInput>,
    services: &[ServiceCheckInput],
) -> AppResult<()> {
    if services.len() > 200 {
        return Err(AppError::PayloadTooLarge);
    }

    let mut tx = pool.begin().await?;

    if let Some(sys) = system {
        sqlx::query(
            r#"
            INSERT INTO host_samples (
                timestamp_ms, host, cpu_pct, mem_used_bytes, mem_total_bytes,
                disk_used_bytes, disk_total_bytes, load1
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(timestamp_ms)
        .bind(host)
        .bind(sys.cpu_pct)
        .bind(sys.mem_used_bytes)
        .bind(sys.mem_total_bytes)
        .bind(sys.disk_used_bytes)
        .bind(sys.disk_total_bytes)
        .bind(sys.load1)
        .execute(&mut *tx)
        .await?;
    }

    for svc in services {
        let name = svc.name.trim();
        if name.is_empty() {
            continue;
        }
        let kind = svc.kind.trim().to_lowercase();
        let status = svc.status.trim().to_lowercase();
        if !matches!(status.as_str(), "up" | "down" | "unknown") {
            return Err(AppError::bad_request(format!("invalid status: {status}")));
        }
        let meta_json = svc
            .meta
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| AppError::bad_request("invalid meta json"))?;

        sqlx::query(
            r#"
            INSERT INTO service_checks (
                timestamp_ms, host, service, kind, status, latency_ms, message, meta_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(timestamp_ms)
        .bind(host)
        .bind(name)
        .bind(kind)
        .bind(status)
        .bind(svc.latency_ms)
        .bind(&svc.message)
        .bind(meta_json)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

pub async fn list_hosts(pool: &SqlitePool) -> AppResult<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT host FROM (
            SELECT DISTINCT host FROM host_samples
            UNION
            SELECT DISTINCT host FROM service_checks
        )
        ORDER BY host ASC
        LIMIT 500
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(h,)| h).collect())
}

pub async fn latest_host_samples(pool: &SqlitePool) -> AppResult<Vec<HostSample>> {
    let rows = sqlx::query_as::<_, HostSampleRow>(
        r#"
        SELECT h.id, h.timestamp_ms, h.host, h.cpu_pct, h.mem_used_bytes, h.mem_total_bytes,
               h.disk_used_bytes, h.disk_total_bytes, h.load1
        FROM host_samples h
        INNER JOIN (
            SELECT host, MAX(timestamp_ms) AS max_ts
            FROM host_samples
            GROUP BY host
        ) latest ON h.host = latest.host AND h.timestamp_ms = latest.max_ts
        ORDER BY h.host ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(HostSampleRow::into_sample).collect())
}

pub async fn latest_service_checks(pool: &SqlitePool) -> AppResult<Vec<ServiceCheck>> {
    let rows = sqlx::query_as::<_, ServiceCheckRow>(
        r#"
        SELECT s.id, s.timestamp_ms, s.host, s.service, s.kind, s.status,
               s.latency_ms, s.message, s.meta_json
        FROM service_checks s
        INNER JOIN (
            SELECT host, service, MAX(timestamp_ms) AS max_ts
            FROM service_checks
            GROUP BY host, service
        ) latest
          ON s.host = latest.host
         AND s.service = latest.service
         AND s.timestamp_ms = latest.max_ts
        ORDER BY s.host ASC, s.service ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter().map(ServiceCheckRow::into_check).collect()
}

pub async fn history_hosts(
    pool: &SqlitePool,
    query: &MetricsHistoryQuery,
) -> AppResult<Vec<HostSample>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let host = query.host.as_deref().unwrap_or("");
    if host.is_empty() {
        return Err(AppError::bad_request("host is required for host history"));
    }

    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT id, timestamp_ms, host, cpu_pct, mem_used_bytes, mem_total_bytes, disk_used_bytes, disk_total_bytes, load1 FROM host_samples WHERE host = ",
    );
    qb.push_bind(host);
    if let Some(from) = &query.from {
        qb.push(" AND timestamp_ms >= ");
        qb.push_bind(parse_bound(from)?);
    }
    if let Some(to) = &query.to {
        qb.push(" AND timestamp_ms <= ");
        qb.push_bind(parse_bound(to)?);
    }
    qb.push(" ORDER BY timestamp_ms DESC LIMIT ");
    qb.push_bind(limit);

    let rows = qb.build_query_as::<HostSampleRow>().fetch_all(pool).await?;
    Ok(rows.into_iter().map(HostSampleRow::into_sample).collect())
}

pub async fn history_services(
    pool: &SqlitePool,
    query: &MetricsHistoryQuery,
) -> AppResult<Vec<ServiceCheck>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT id, timestamp_ms, host, service, kind, status, latency_ms, message, meta_json FROM service_checks WHERE 1=1",
    );
    if let Some(host) = query.host.as_ref().filter(|h| !h.is_empty()) {
        qb.push(" AND host = ");
        qb.push_bind(host);
    }
    if let Some(service) = query.service.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND service = ");
        qb.push_bind(service);
    }
    if let Some(from) = &query.from {
        qb.push(" AND timestamp_ms >= ");
        qb.push_bind(parse_bound(from)?);
    }
    if let Some(to) = &query.to {
        qb.push(" AND timestamp_ms <= ");
        qb.push_bind(parse_bound(to)?);
    }
    qb.push(" ORDER BY timestamp_ms DESC LIMIT ");
    qb.push_bind(limit);

    let rows = qb.build_query_as::<ServiceCheckRow>().fetch_all(pool).await?;
    rows.into_iter().map(ServiceCheckRow::into_check).collect()
}

pub async fn delete_older_than(pool: &SqlitePool, cutoff_ms: i64) -> AppResult<u64> {
    let hosts = sqlx::query("DELETE FROM host_samples WHERE timestamp_ms < ?")
        .bind(cutoff_ms)
        .execute(pool)
        .await?
        .rows_affected();
    let services = sqlx::query("DELETE FROM service_checks WHERE timestamp_ms < ?")
        .bind(cutoff_ms)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(hosts + services)
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

pub fn parse_batch_timestamp(batch: &MetricsBatchRequest) -> AppResult<i64> {
    match &batch.timestamp {
        Some(raw) => {
            if let Ok(ms) = raw.parse::<i64>() {
                return Ok(ms);
            }
            chrono::DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.timestamp_millis())
                .map_err(|_| AppError::bad_request(format!("invalid timestamp: {raw}")))
        }
        None => Ok(chrono::Utc::now().timestamp_millis()),
    }
}

fn parse_bound(raw: &str) -> AppResult<i64> {
    if let Ok(ms) = raw.parse::<i64>() {
        return Ok(ms);
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.timestamp_millis())
        .map_err(|_| AppError::bad_request(format!("invalid time bound: {raw}")))
}

#[derive(Debug, sqlx::FromRow)]
struct HostSampleRow {
    id: i64,
    timestamp_ms: i64,
    host: String,
    cpu_pct: Option<f64>,
    mem_used_bytes: Option<i64>,
    mem_total_bytes: Option<i64>,
    disk_used_bytes: Option<i64>,
    disk_total_bytes: Option<i64>,
    load1: Option<f64>,
}

impl HostSampleRow {
    fn into_sample(self) -> HostSample {
        HostSample {
            id: self.id,
            timestamp: ms_to_rfc3339(self.timestamp_ms),
            host: self.host,
            cpu_pct: self.cpu_pct,
            mem_used_bytes: self.mem_used_bytes,
            mem_total_bytes: self.mem_total_bytes,
            disk_used_bytes: self.disk_used_bytes,
            disk_total_bytes: self.disk_total_bytes,
            load1: self.load1,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ServiceCheckRow {
    id: i64,
    timestamp_ms: i64,
    host: String,
    service: String,
    kind: String,
    status: String,
    latency_ms: Option<i64>,
    message: Option<String>,
    meta_json: Option<String>,
}

impl ServiceCheckRow {
    fn into_check(self) -> AppResult<ServiceCheck> {
        let meta = match self.meta_json {
            Some(raw) if !raw.is_empty() => Some(
                serde_json::from_str(&raw)
                    .map_err(|e| AppError::internal(format!("corrupt meta_json: {e}")))?,
            ),
            _ => None,
        };
        Ok(ServiceCheck {
            id: self.id,
            timestamp: ms_to_rfc3339(self.timestamp_ms),
            host: self.host,
            service: self.service,
            kind: self.kind,
            status: self.status,
            latency_ms: self.latency_ms,
            message: self.message,
            meta,
        })
    }
}
