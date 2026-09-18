use crate::error::{AppError, AppResult};
use crate::models::log::ms_to_rfc3339;
use crate::models::metrics::{
    eta_hours_to_threshold, host_recommend, linear_forecast, pct, CapacityForecast, HostCapacity,
    HostSample, HostSampleInput, MetricsBatchRequest, MetricsHistoryQuery, ServiceCheck,
    ServiceCheckInput,
};
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

    let mut conn = pool.acquire().await?;
    // Write lock first so busy_timeout applies instead of mid-tx SQLITE_BUSY.
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;

    if let Err(err) =
        insert_batch_tx(&mut conn, host, timestamp_ms, system, services).await
    {
        let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
        return Err(err);
    }

    sqlx::query("COMMIT").execute(&mut *conn).await?;
    Ok(())
}

async fn insert_batch_tx(
    conn: &mut sqlx::sqlite::SqliteConnection,
    host: &str,
    timestamp_ms: i64,
    system: Option<&HostSampleInput>,
    services: &[ServiceCheckInput],
) -> AppResult<()> {
    if let Some(sys) = system {
        sqlx::query(
            r#"
            INSERT INTO host_samples (
                timestamp_ms, host, cpu_pct, mem_used_bytes, mem_total_bytes,
                disk_used_bytes, disk_total_bytes, load1, load5, load15, n_cpus
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(sys.load5)
        .bind(sys.load15)
        .bind(sys.n_cpus)
        .execute(&mut *conn)
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
        let load_hint = normalize_load_hint(svc.load_hint.as_deref());
        let recommend = normalize_recommend(svc.recommend.as_deref());
        let meta_json = svc
            .meta
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| AppError::bad_request("invalid meta json"))?;

        sqlx::query(
            r#"
            INSERT INTO service_checks (
                timestamp_ms, host, service, kind, status, latency_ms, message, meta_json,
                cpu_pct, mem_used_bytes, mem_limit_bytes, load_hint, recommend
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(svc.cpu_pct)
        .bind(svc.mem_used_bytes)
        .bind(svc.mem_limit_bytes)
        .bind(load_hint)
        .bind(recommend)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

fn normalize_load_hint(raw: Option<&str>) -> Option<String> {
    let v = raw?.trim().to_lowercase();
    matches!(v.as_str(), "ok" | "watch" | "tight").then_some(v)
}

fn normalize_recommend(raw: Option<&str>) -> Option<String> {
    let v = raw?.trim().to_lowercase();
    matches!(v.as_str(), "ok" | "watch" | "scale_out" | "limit").then_some(v)
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
               h.disk_used_bytes, h.disk_total_bytes, h.load1, h.load5, h.load15, h.n_cpus
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
               s.latency_ms, s.message, s.meta_json,
               s.cpu_pct, s.mem_used_bytes, s.mem_limit_bytes, s.load_hint, s.recommend
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
        "SELECT id, timestamp_ms, host, cpu_pct, mem_used_bytes, mem_total_bytes, disk_used_bytes, disk_total_bytes, load1, load5, load15, n_cpus FROM host_samples WHERE host = ",
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
        "SELECT id, timestamp_ms, host, service, kind, status, latency_ms, message, meta_json, cpu_pct, mem_used_bytes, mem_limit_bytes, load_hint, recommend FROM service_checks WHERE 1=1",
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

pub async fn host_capacity(pool: &SqlitePool, host: &str) -> AppResult<HostCapacity> {
    let host = host.trim();
    if host.is_empty() {
        return Err(AppError::bad_request("host is required"));
    }

    let latest = sqlx::query_as::<_, HostSampleRow>(
        "SELECT id, timestamp_ms, host, cpu_pct, mem_used_bytes, mem_total_bytes, disk_used_bytes, disk_total_bytes, load1, load5, load15, n_cpus FROM host_samples WHERE host = ? ORDER BY timestamp_ms DESC LIMIT 1",
    )
    .bind(host)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let history = sqlx::query_as::<_, HostSampleRow>(
        "SELECT id, timestamp_ms, host, cpu_pct, mem_used_bytes, mem_total_bytes, disk_used_bytes, disk_total_bytes, load1, load5, load15, n_cpus FROM host_samples WHERE host = ? ORDER BY timestamp_ms DESC LIMIT 120",
    )
    .bind(host)
    .fetch_all(pool)
    .await?;

    let sample = latest.into_sample();
    let mem_pct = pct(sample.mem_used_bytes, sample.mem_total_bytes);
    let disk_pct = pct(sample.disk_used_bytes, sample.disk_total_bytes);
    let load_per_cpu = match (sample.load1, sample.n_cpus) {
        (Some(l), Some(n)) if n > 0 => Some(l / n as f64),
        _ => None,
    };
    let headroom_pct = match (sample.cpu_pct, mem_pct) {
        (Some(c), Some(m)) => Some((100.0 - c).min(100.0 - m).max(0.0)),
        (Some(c), None) => Some((100.0 - c).max(0.0)),
        (None, Some(m)) => Some((100.0 - m).max(0.0)),
        _ => None,
    };
    let recommend = host_recommend(sample.cpu_pct, mem_pct, disk_pct, load_per_cpu);

    let mut cpu_points = Vec::new();
    let mut mem_points = Vec::new();
    for row in history.iter().rev() {
        let t = row.timestamp_ms as f64;
        if let Some(c) = row.cpu_pct {
            cpu_points.push((t, c));
        }
        if let Some(m) = pct(row.mem_used_bytes, row.mem_total_bytes) {
            mem_points.push((t, m));
        }
    }

    let forecast = if cpu_points.len() >= 8 || mem_points.len() >= 8 {
        Some(CapacityForecast {
            cpu_pct_1h: linear_forecast(&cpu_points, 1.0),
            cpu_pct_6h: linear_forecast(&cpu_points, 6.0),
            mem_pct_1h: linear_forecast(&mem_points, 1.0),
            mem_pct_6h: linear_forecast(&mem_points, 6.0),
            eta_hours_to_cpu_85: eta_hours_to_threshold(&cpu_points, 85.0),
            eta_hours_to_mem_85: eta_hours_to_threshold(&mem_points, 85.0),
        })
    } else {
        None
    };

    Ok(HostCapacity {
        host: sample.host,
        timestamp: sample.timestamp,
        cpu_pct: sample.cpu_pct,
        mem_pct,
        disk_pct,
        load1: sample.load1,
        load5: sample.load5,
        load15: sample.load15,
        n_cpus: sample.n_cpus,
        load_per_cpu,
        headroom_pct,
        recommend,
        forecast,
    })
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
    load5: Option<f64>,
    load15: Option<f64>,
    n_cpus: Option<i64>,
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
            load5: self.load5,
            load15: self.load15,
            n_cpus: self.n_cpus,
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
    cpu_pct: Option<f64>,
    mem_used_bytes: Option<i64>,
    mem_limit_bytes: Option<i64>,
    load_hint: Option<String>,
    recommend: Option<String>,
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
            cpu_pct: self.cpu_pct,
            mem_used_bytes: self.mem_used_bytes,
            mem_limit_bytes: self.mem_limit_bytes,
            load_hint: self.load_hint,
            recommend: self.recommend,
        })
    }
}
