use crate::config::{AgentConfig, ServiceConfig};
use crate::load::{
    apply_es_recommend, looks_like_cluster_health_url, mem_pct, merge_hint, parse_es_health_hint,
    resource_load_hint, service_recommend, LoadHint, Recommend,
};
use anyhow::Result;
use futures::stream::{self, StreamExt};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize)]
pub struct ServiceCheckResult {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub message: Option<String>,
    /// Kind-specific numbers (`mysql_status`, `dns`, `host_io`); stored as is by the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_used_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_limit_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommend: Option<String>,
}

impl ServiceCheckResult {
    pub fn finalize_resources(mut self) -> Self {
        let res_hint = resource_load_hint(self.cpu_pct, mem_pct(self.mem_used_bytes, self.mem_limit_bytes));
        let existing = self
            .load_hint
            .as_deref()
            .and_then(|s| match s {
                "ok" => Some(LoadHint::Ok),
                "watch" => Some(LoadHint::Watch),
                "tight" => Some(LoadHint::Tight),
                _ => None,
            });
        let hint = merge_hint(existing, res_hint);
        let rec = service_recommend(&self.status, hint, self.latency_ms);
        self.load_hint = hint.map(|h| h.as_str().to_string());
        self.recommend = Some(rec.as_str().to_string());
        self
    }
}

pub async fn run_configured(cfg: &AgentConfig) -> Result<Vec<ServiceCheckResult>> {
    let services = cfg.services.clone();
    let results = stream::iter(services)
        .map(|svc| async move { check_one(svc).await.finalize_resources() })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    Ok(results)
}

async fn check_one(svc: ServiceConfig) -> ServiceCheckResult {
    let timeout_ms = svc.timeout_ms.unwrap_or(2000).min(2000);
    let kind = svc.kind.to_lowercase();
    match kind.as_str() {
        "http" => check_http(&svc, timeout_ms).await,
        "tcp" => check_tcp(&svc, timeout_ms).await,
        "process" => check_process(&svc).await,
        "mysql_status" => check_mysql(&svc, new_kind_timeout(&svc)).await,
        "dns" => check_dns(&svc, new_kind_timeout(&svc)).await,
        "host_io" => check_host_io(&svc).await,
        other => ServiceCheckResult {
            name: svc.name,
            kind: other.to_string(),
            status: "unknown".into(),
            latency_ms: None,
            message: Some(format!("unsupported kind: {other}")),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: Some(Recommend::Limit.as_str().into()),
        },
    }
}

async fn check_http(svc: &ServiceConfig, timeout_ms: u64) -> ServiceCheckResult {
    let Some(url) = svc.url.as_ref() else {
        return ServiceCheckResult {
            name: svc.name.clone(),
            kind: "http".into(),
            status: "unknown".into(),
            latency_ms: None,
            message: Some("url missing".into()),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: Some(Recommend::Limit.as_str().into()),
        };
    };

    http_probe(&svc.name, "http", url, timeout_ms).await
}

pub async fn http_probe(name: &str, kind: &str, url: &str, timeout_ms: u64) -> ServiceCheckResult {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            return ServiceCheckResult {
                name: name.into(),
                kind: kind.into(),
                status: "down".into(),
                latency_ms: None,
                message: Some(err.to_string()),
                meta: None,
                cpu_pct: None,
                mem_used_bytes: None,
                mem_limit_bytes: None,
                load_hint: None,
                recommend: Some(Recommend::Limit.as_str().into()),
            };
        }
    };

    let started = Instant::now();
    match client.get(url).send().await {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let ok = resp.status().is_success();
            let latency_ms = Some(started.elapsed().as_millis() as i64);
            let body = resp.text().await.unwrap_or_default();
            let mut es_hint = None;
            let mut message = status_code.to_string();
            if looks_like_cluster_health_url(url) || body.contains("\"number_of_pending_tasks\"") {
                if let Some(h) = parse_es_health_hint(&body) {
                    es_hint = Some(h);
                    message = format!("{status_code}; es_load={}", h.as_str());
                }
            }
            let status = if ok { "up" } else { "down" };
            let mut rec = service_recommend(status, es_hint, latency_ms);
            if looks_like_cluster_health_url(url) || es_hint.is_some() {
                rec = apply_es_recommend(es_hint, rec);
            }
            ServiceCheckResult {
                name: name.into(),
                kind: kind.into(),
                status: status.into(),
                latency_ms,
                message: Some(message),
                meta: None,
                cpu_pct: None,
                mem_used_bytes: None,
                mem_limit_bytes: None,
                load_hint: es_hint.map(|h| h.as_str().into()),
                recommend: Some(rec.as_str().into()),
            }
        }
        Err(err) => ServiceCheckResult {
            name: name.into(),
            kind: kind.into(),
            status: "down".into(),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: Some(err.to_string()),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: Some(Recommend::Limit.as_str().into()),
        },
    }
}

async fn check_tcp(svc: &ServiceConfig, timeout_ms: u64) -> ServiceCheckResult {
    let Some(addr) = svc.addr.as_ref() else {
        return ServiceCheckResult {
            name: svc.name.clone(),
            kind: "tcp".into(),
            status: "unknown".into(),
            latency_ms: None,
            message: Some("addr missing".into()),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: Some(Recommend::Limit.as_str().into()),
        };
    };

    let started = Instant::now();
    let result = match timeout(
        Duration::from_millis(timeout_ms),
        TcpStream::connect(addr.as_str()),
    )
    .await
    {
        Ok(Ok(_stream)) => ServiceCheckResult {
            name: svc.name.clone(),
            kind: "tcp".into(),
            status: "up".into(),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: Some("connected".into()),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: None,
        },
        Ok(Err(err)) => ServiceCheckResult {
            name: svc.name.clone(),
            kind: "tcp".into(),
            status: "down".into(),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: Some(err.to_string()),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: Some(Recommend::Limit.as_str().into()),
        },
        Err(_) => ServiceCheckResult {
            name: svc.name.clone(),
            kind: "tcp".into(),
            status: "down".into(),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: Some("timeout".into()),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: Some(Recommend::Limit.as_str().into()),
        },
    };
    result
}

async fn check_process(svc: &ServiceConfig) -> ServiceCheckResult {
    let Some(needle) = svc.process_match.as_ref() else {
        return ServiceCheckResult {
            name: svc.name.clone(),
            kind: "process".into(),
            status: "unknown".into(),
            latency_ms: None,
            message: Some("match missing".into()),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: Some(Recommend::Limit.as_str().into()),
        };
    };

    #[cfg(target_os = "linux")]
    {
        let pids = find_pids(needle);
        if pids.is_empty() {
            return ServiceCheckResult {
                name: svc.name.clone(),
                kind: "process".into(),
                status: "down".into(),
                latency_ms: None,
                message: Some("process not found".into()),
                meta: None,
                cpu_pct: None,
                mem_used_bytes: None,
                mem_limit_bytes: None,
                load_hint: None,
                recommend: Some(Recommend::Limit.as_str().into()),
            };
        }

        let mut mem_used = 0i64;
        for pid in &pids {
            if let Some(rss) = read_rss_bytes(*pid) {
                mem_used = mem_used.saturating_add(rss);
            }
        }
        let mem_limit = host_mem_total();
        let cpu_pct = sample_cpu_pct(&pids).await;

        ServiceCheckResult {
            name: svc.name.clone(),
            kind: "process".into(),
            status: "up".into(),
            latency_ms: None,
            message: Some(format!("process found ({})", pids.len())),
            meta: None,
            cpu_pct,
            mem_used_bytes: if mem_used > 0 { Some(mem_used) } else { None },
            mem_limit_bytes: mem_limit,
            load_hint: None,
            recommend: None,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = needle;
        ServiceCheckResult {
            name: svc.name.clone(),
            kind: "process".into(),
            status: "unknown".into(),
            latency_ms: None,
            message: Some("process checks supported on Linux only".into()),
            meta: None,
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: None,
        }
    }
}

#[cfg(target_os = "linux")]
fn find_pids(needle: &str) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let pid = name.to_str()?;
            if !pid.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let cmdline = std::fs::read_to_string(e.path().join("cmdline")).ok()?;
            if cmdline.replace('\0', " ").contains(needle) {
                pid.parse().ok()
            } else {
                None
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn read_rss_bytes(pid: i32) -> Option<i64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: i64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn host_mem_total() -> Option<i64> {
    let raw = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: i64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_proc_cpu_jiffies(pid: i32) -> Option<u64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = raw.rfind(')')?;
    let rest = raw.get(close + 2..)?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    // utime=14th field after comm → index 11 in parts after ") "
    let utime: u64 = parts.get(11)?.parse().ok()?;
    let stime: u64 = parts.get(12)?.parse().ok()?;
    Some(utime.saturating_add(stime))
}

#[cfg(target_os = "linux")]
fn read_total_jiffies() -> Option<u64> {
    let raw = std::fs::read_to_string("/proc/stat").ok()?;
    let line = raw.lines().next()?;
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|p| p.parse().ok())
        .collect();
    Some(nums.iter().sum())
}

#[cfg(target_os = "linux")]
async fn sample_cpu_pct(pids: &[i32]) -> Option<f64> {
    let mut before_proc = 0u64;
    for pid in pids {
        before_proc = before_proc.saturating_add(read_proc_cpu_jiffies(*pid).unwrap_or(0));
    }
    let before_total = read_total_jiffies()?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut after_proc = 0u64;
    for pid in pids {
        after_proc = after_proc.saturating_add(read_proc_cpu_jiffies(*pid).unwrap_or(0));
    }
    let after_total = read_total_jiffies()?;
    let dp = after_proc.saturating_sub(before_proc) as f64;
    let dt = after_total.saturating_sub(before_total) as f64;
    if dt <= 0.0 {
        return None;
    }
    Some(((dp / dt) * 100.0).clamp(0.0, 100.0 * pids.len() as f64))
}

// ---------------------------------------------------------------------------------------
// mysql_status · dns · host_io
// ---------------------------------------------------------------------------------------

/// The new kinds may wait longer than the 2 s cap of http/tcp: a MySQL that takes 3 s to
/// accept a connection is exactly what we want to see (as latency, not as "down").
fn new_kind_timeout(svc: &ServiceConfig) -> u64 {
    svc.timeout_ms.unwrap_or(2000).clamp(100, 10_000)
}

fn result(name: &str, kind: &str, status: &str) -> ServiceCheckResult {
    ServiceCheckResult {
        name: name.into(),
        kind: kind.into(),
        status: status.into(),
        latency_ms: None,
        message: None,
        meta: None,
        cpu_pct: None,
        mem_used_bytes: None,
        mem_limit_bytes: None,
        load_hint: None,
        recommend: None,
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Counters shipped as per-interval deltas.
pub const MYSQL_DELTA_KEYS: &[&str] = &[
    "Connection_errors_max_connections",
    "Aborted_connects",
    "Aborted_clients",
    "Connections",
    "Slow_queries",
    "Innodb_row_lock_waits",
];
const MYSQL_GAUGE_KEYS: &[&str] = &["Threads_connected", "Threads_running"];

struct MysqlPrev {
    at: Instant,
    values: HashMap<String, u64>,
}

/// Previous counters per service name (process-wide; checks run in parallel).
static MYSQL_PREV: LazyLock<Mutex<HashMap<String, MysqlPrev>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, PartialEq)]
pub struct MysqlEval {
    pub meta: Value,
    pub hint: LoadHint,
    pub message: String,
}

/// `cur - prev`; a counter that went down (server restart) counts from zero.
fn counter_delta(cur: u64, prev: u64) -> u64 {
    if cur >= prev {
        cur - prev
    } else {
        cur
    }
}

pub fn mysql_eval(
    status: &HashMap<String, u64>,
    max_connections: Option<u64>,
    prev: Option<(&HashMap<String, u64>, f64)>,
) -> MysqlEval {
    let tc = status.get("Threads_connected").copied();
    let tr = status.get("Threads_running").copied();
    let conn_pct = match (tc, max_connections) {
        (Some(t), Some(m)) if m > 0 => Some(((t as f64 / m as f64) * 1000.0).round() / 10.0),
        _ => None,
    };

    let mut meta = Map::new();
    meta.insert("Threads_connected".into(), json!(tc));
    meta.insert("Threads_running".into(), json!(tr));
    meta.insert("max_connections".into(), json!(max_connections));
    meta.insert("conn_pct".into(), json!(conn_pct));
    meta.insert("interval_secs".into(), json!(prev.map(|(_, s)| (s * 10.0).round() / 10.0)));
    let mut max_conn_errors = None;
    for key in MYSQL_DELTA_KEYS {
        let d = match (status.get(*key), prev.and_then(|(p, _)| p.get(*key))) {
            (Some(cur), Some(p)) => Some(counter_delta(*cur, *p)),
            _ => None,
        };
        if *key == "Connection_errors_max_connections" {
            max_conn_errors = d;
        }
        meta.insert(format!("{key}_delta"), json!(d));
    }

    let refused = max_conn_errors.unwrap_or(0);
    let pct = conn_pct.unwrap_or(0.0);
    let hint = if pct >= 80.0 || refused > 0 {
        LoadHint::Tight
    } else if pct >= 60.0 {
        LoadHint::Watch
    } else {
        LoadHint::Ok
    };

    let fmt = |v: Option<u64>| v.map_or("?".to_string(), |n| n.to_string());
    let mut message = format!(
        "threads {}/{} ({}%), running {}",
        fmt(tc),
        fmt(max_connections),
        conn_pct.map_or("?".to_string(), |p| format!("{p:.0}")),
        fmt(tr)
    );
    if refused > 0 {
        message.push_str(&format!(", {refused} refused (max_connections)"));
    }
    MysqlEval {
        meta: Value::Object(meta),
        hint,
        message,
    }
}

/// Drop the password from anything that might echo the DSN back.
fn redact_dsn(msg: &str, dsn: &str) -> String {
    let Some(rest) = dsn.split_once("://").map(|(_, r)| r) else {
        return msg.to_string();
    };
    let Some((userinfo, _)) = rest.rsplit_once('@') else {
        return msg.to_string();
    };
    let Some((_, pass)) = userinfo.split_once(':') else {
        return msg.to_string();
    };
    if pass.is_empty() {
        return msg.to_string();
    }
    let mut out = msg.replace(pass, "***");
    if let Ok(decoded) = percent_decode(pass) {
        if !decoded.is_empty() {
            out = out.replace(&decoded, "***");
        }
    }
    out
}

fn percent_decode(s: &str) -> Result<String, ()> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).map_err(|_| ())?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| ())?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

async fn check_mysql(svc: &ServiceConfig, timeout_ms: u64) -> ServiceCheckResult {
    use mysql_async::prelude::Queryable;

    let mut r = result(&svc.name, "mysql_status", "unknown");
    let Some(env_name) = svc.dsn_env.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        r.message = Some("dsn_env missing".into());
        return r;
    };
    let dsn = match std::env::var(env_name) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            r.message = Some(format!("env {env_name} is not set"));
            return r;
        }
    };
    let opts = match mysql_async::Opts::from_url(&dsn) {
        Ok(o) => o,
        Err(err) => {
            r.message = Some(redact_dsn(&format!("invalid DSN in {env_name}: {err}"), &dsn));
            return r;
        }
    };
    // One plain connection: no pool, and no second hop to @@socket (that would double
    // the Connections counter we report).
    let opts = mysql_async::OptsBuilder::from_opts(opts).prefer_socket(false);

    let started = Instant::now();
    let work = async {
        let mut conn = mysql_async::Conn::new(opts).await?;
        let mut sql = String::from("SHOW GLOBAL STATUS WHERE Variable_name IN (");
        for (i, k) in MYSQL_GAUGE_KEYS.iter().chain(MYSQL_DELTA_KEYS).enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push_str(&format!("'{k}'"));
        }
        sql.push(')');
        let rows: Vec<(String, Option<String>)> = conn.query(sql).await?;
        let max_conn: Option<u64> = conn.query_first("SELECT @@max_connections").await?;
        let latency = started.elapsed();
        let _ = conn.disconnect().await;
        Ok::<_, mysql_async::Error>((rows, max_conn, latency))
    };
    let outcome = timeout(Duration::from_millis(timeout_ms), work).await;
    let (rows, max_conn, latency) = match outcome {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => {
            r.status = "down".into();
            r.latency_ms = Some(started.elapsed().as_millis() as i64);
            r.message = Some(redact_dsn(&err.to_string(), &dsn));
            return r;
        }
        Err(_) => {
            r.status = "down".into();
            r.latency_ms = Some(started.elapsed().as_millis() as i64);
            r.message = Some(format!("timeout after {timeout_ms} ms"));
            return r;
        }
    };

    let status: HashMap<String, u64> = rows
        .into_iter()
        .filter_map(|(k, v)| Some((k, v?.trim().parse().ok()?)))
        .collect();
    let now = Instant::now();
    let eval = {
        let mut prev_map = lock(&MYSQL_PREV);
        let eval = mysql_eval(
            &status,
            max_conn,
            prev_map
                .get(&svc.name)
                .map(|p| (&p.values, now.duration_since(p.at).as_secs_f64())),
        );
        prev_map.insert(
            svc.name.clone(),
            MysqlPrev {
                at: now,
                values: status,
            },
        );
        eval
    };
    r.status = "up".into();
    r.latency_ms = Some(latency.as_millis() as i64);
    r.message = Some(eval.message);
    r.meta = Some(eval.meta);
    r.load_hint = Some(eval.hint.as_str().into());
    r
}

async fn check_dns(svc: &ServiceConfig, timeout_ms: u64) -> ServiceCheckResult {
    let host = svc
        .host
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&svc.name)
        .to_string();
    let lookup = {
        let host = host.clone();
        async move {
            tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map(|it| it.map(|a| a.ip()).collect::<Vec<_>>())
        }
    };
    dns_check_with(&svc.name, &host, timeout_ms, lookup).await
}

/// `dns` with the resolver injected (tests use a never-ending future for the timeout path).
pub async fn dns_check_with<F>(name: &str, host: &str, timeout_ms: u64, lookup: F) -> ServiceCheckResult
where
    F: std::future::Future<Output = std::io::Result<Vec<std::net::IpAddr>>>,
{
    let mut r = result(name, "dns", "down");
    let started = Instant::now();
    let outcome = timeout(Duration::from_millis(timeout_ms), lookup).await;
    let elapsed = started.elapsed().as_millis() as i64;
    r.latency_ms = Some(elapsed);
    match outcome {
        Ok(Ok(addrs)) if !addrs.is_empty() => {
            r.status = "up".into();
            let mut uniq: Vec<String> = Vec::new();
            for a in &addrs {
                let s = a.to_string();
                if !uniq.contains(&s) {
                    uniq.push(s);
                }
            }
            r.message = Some(format!("{host} → {}", uniq.join(", ")));
            r.meta = Some(json!({ "host": host, "addrs": uniq }));
            r.load_hint = Some(
                if elapsed > 500 {
                    LoadHint::Watch
                } else {
                    LoadHint::Ok
                }
                .as_str()
                .into(),
            );
        }
        Ok(Ok(_)) => r.message = Some(format!("{host}: no addresses")),
        Ok(Err(err)) => r.message = Some(format!("{host}: {err}")),
        Err(_) => r.message = Some(format!("{host}: timeout after {timeout_ms} ms")),
    }
    r
}

/// Aggregate `cpu` line of /proc/stat: (total, iowait, steal) in jiffies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub total: u64,
    pub iowait: u64,
    pub steal: u64,
}

pub fn parse_proc_stat_cpu(raw: &str) -> Option<CpuTimes> {
    let line = raw.lines().find(|l| l.starts_with("cpu "))?;
    let n: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|p| p.parse().ok())
        .collect();
    if n.len() < 5 {
        return None;
    }
    // user nice system idle iowait irq softirq steal (guest* are already in user/nice)
    Some(CpuTimes {
        total: n.iter().take(8).sum(),
        iowait: n[4],
        steal: n.get(7).copied().unwrap_or(0),
    })
}

/// (iowait_pct, steal_pct) between two samples.
pub fn cpu_window_pcts(a: CpuTimes, b: CpuTimes) -> Option<(f64, f64)> {
    let total = b.total.checked_sub(a.total)? as f64;
    if total <= 0.0 {
        return None;
    }
    let pct = |x: u64, y: u64| (y.saturating_sub(x) as f64 / total * 1000.0).round() / 10.0;
    Some((pct(a.iowait, b.iowait), pct(a.steal, b.steal)))
}

/// `some avg10` / `full avg10` of a /proc/pressure file.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Psi {
    pub some_avg10: Option<f64>,
    pub full_avg10: Option<f64>,
}

pub fn parse_psi(raw: &str) -> Psi {
    let mut psi = Psi::default();
    for line in raw.lines() {
        let mut parts = line.split_whitespace();
        let kind = parts.next();
        let avg10 = parts
            .find_map(|p| p.strip_prefix("avg10="))
            .and_then(|v| v.parse().ok());
        match kind {
            Some("some") => psi.some_avg10 = avg10,
            Some("full") => psi.full_avg10 = avg10,
            _ => {}
        }
    }
    psi
}

/// (OutSegs, RetransSegs) from /proc/net/snmp (header line + value line per protocol).
pub fn parse_snmp_tcp(raw: &str) -> Option<(u64, u64)> {
    let mut tcp = raw.lines().filter(|l| l.starts_with("Tcp:"));
    let header: Vec<&str> = tcp.next()?.split_whitespace().collect();
    let values: Vec<&str> = tcp.next()?.split_whitespace().collect();
    let get = |name: &str| -> Option<u64> {
        let i = header.iter().position(|h| *h == name)?;
        values.get(i)?.parse().ok()
    };
    Some((get("OutSegs")?, get("RetransSegs")?))
}

/// Retransmitted share of sent segments since the previous sample.
pub fn retrans_pct(prev: (u64, u64), cur: (u64, u64)) -> Option<f64> {
    let out = cur.0.checked_sub(prev.0)?;
    let retrans = cur.1.checked_sub(prev.1)?;
    if out == 0 {
        return Some(0.0);
    }
    Some((retrans as f64 / out as f64 * 10000.0).round() / 100.0)
}

/// tight: io full avg10 > 10, steal > 20 % or iowait > 30 %; watch at half of each.
pub fn host_io_hint(iowait_pct: Option<f64>, steal_pct: Option<f64>, io_full_avg10: Option<f64>) -> LoadHint {
    let mut hint = LoadHint::Ok;
    for (v, tight) in [(io_full_avg10, 10.0), (steal_pct, 20.0), (iowait_pct, 30.0)] {
        let Some(v) = v else { continue };
        let h = if v > tight {
            LoadHint::Tight
        } else if v > tight / 2.0 {
            LoadHint::Watch
        } else {
            LoadHint::Ok
        };
        hint = hint.max(h);
    }
    hint
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
static TCP_PREV: LazyLock<Mutex<HashMap<String, (u64, u64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

async fn check_host_io(svc: &ServiceConfig) -> ServiceCheckResult {
    #[cfg(target_os = "linux")]
    {
        let read = |p: &str| std::fs::read_to_string(p).ok();
        let a = read("/proc/stat").as_deref().and_then(parse_proc_stat_cpu);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let b = read("/proc/stat").as_deref().and_then(parse_proc_stat_cpu);
        let (iowait, steal) = match (a, b) {
            (Some(a), Some(b)) => cpu_window_pcts(a, b).unzip(),
            _ => (None, None),
        };
        let psi = |name: &str| read(&format!("/proc/pressure/{name}")).map(|r| parse_psi(&r));
        let (cpu, io, mem) = (psi("cpu"), psi("io"), psi("memory"));

        let tcp = read("/proc/net/snmp").as_deref().and_then(parse_snmp_tcp);
        let retrans = match tcp {
            Some(cur) => {
                let mut prev = lock(&TCP_PREV);
                let r = prev.get(&svc.name).and_then(|p| retrans_pct(*p, cur));
                prev.insert(svc.name.clone(), cur);
                r
            }
            None => None,
        };

        let io_full = io.and_then(|p| p.full_avg10);
        let hint = host_io_hint(iowait, steal, io_full);
        let meta = json!({
            "iowait_pct": iowait,
            "steal_pct": steal,
            "psi_cpu_some_avg10": cpu.and_then(|p| p.some_avg10),
            "psi_io_some_avg10": io.and_then(|p| p.some_avg10),
            "psi_io_full_avg10": io_full,
            "psi_memory_some_avg10": mem.and_then(|p| p.some_avg10),
            "psi_memory_full_avg10": mem.and_then(|p| p.full_avg10),
            "retrans_pct": retrans,
        });
        let f = |v: Option<f64>| v.map_or("?".to_string(), |x| format!("{x:.1}"));
        let mut r = result(&svc.name, "host_io", "up");
        r.message = Some(format!(
            "iowait {}% steal {}% io_full {} retrans {}%",
            f(iowait),
            f(steal),
            f(io_full),
            f(retrans)
        ));
        r.meta = Some(meta);
        r.load_hint = Some(hint.as_str().into());
        r
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut r = result(&svc.name, "host_io", "unknown");
        r.message = Some("host_io supported on Linux only".into());
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn mysql_first_run_has_null_deltas() {
        let s = status(&[("Threads_connected", 30), ("Threads_running", 2), ("Connections", 1000)]);
        let e = mysql_eval(&s, Some(151), None);
        assert_eq!(e.hint, LoadHint::Ok);
        assert_eq!(e.meta["conn_pct"], 19.9);
        assert_eq!(e.meta["max_connections"], 151);
        assert!(e.meta["Connections_delta"].is_null());
        assert!(e.meta["interval_secs"].is_null());
        assert_eq!(e.message, "threads 30/151 (20%), running 2");
    }

    #[test]
    fn mysql_deltas_and_hints() {
        let prev = status(&[
            ("Connections", 1000),
            ("Connection_errors_max_connections", 5),
            ("Aborted_connects", 7),
            ("Slow_queries", 100),
        ]);
        let cur = status(&[
            ("Threads_connected", 95),
            ("Threads_running", 4),
            ("Connections", 1150),
            ("Connection_errors_max_connections", 5),
            ("Aborted_connects", 9),
            ("Slow_queries", 3), // server restarted: counts from zero
        ]);
        let e = mysql_eval(&cur, Some(151), Some((&prev, 60.04)));
        assert_eq!(e.meta["Connections_delta"], 150);
        assert_eq!(e.meta["Aborted_connects_delta"], 2);
        assert_eq!(e.meta["Slow_queries_delta"], 3);
        assert_eq!(e.meta["Connection_errors_max_connections_delta"], 0);
        assert!(e.meta["Innodb_row_lock_waits_delta"].is_null());
        assert_eq!(e.meta["interval_secs"], 60.0);
        assert_eq!(e.hint, LoadHint::Watch, "62.9% connections");

        let tight = status(&[("Threads_connected", 121)]);
        assert_eq!(mysql_eval(&tight, Some(151), None).hint, LoadHint::Tight);

        // a refused connection since the last run is tight even at low usage
        let p = status(&[("Connection_errors_max_connections", 5)]);
        let c = status(&[("Threads_connected", 10), ("Connection_errors_max_connections", 8)]);
        let e = mysql_eval(&c, Some(151), Some((&p, 60.0)));
        assert_eq!(e.hint, LoadHint::Tight);
        assert!(e.message.ends_with("3 refused (max_connections)"));

        // recommend stays consistent with load.rs
        let mut r = result("mysql", "mysql_status", "up");
        r.load_hint = Some(e.hint.as_str().into());
        r.latency_ms = Some(12);
        assert_eq!(r.finalize_resources().recommend.as_deref(), Some("scale_out"));
        let down = result("mysql", "mysql_status", "down").finalize_resources();
        assert_eq!(down.recommend.as_deref(), Some("limit"));
    }

    #[test]
    fn dsn_password_is_redacted() {
        let dsn = "mysql://mon:s3cr%40t@127.0.0.1:3306/";
        assert_eq!(
            redact_dsn("Access denied (password s3cr@t / s3cr%40t)", dsn),
            "Access denied (password *** / ***)"
        );
        assert_eq!(redact_dsn("x", "mysql://mon@localhost/"), "x");
        assert!(mysql_async::Opts::from_url(
            "mysql://mon:pw@localhost/?socket=/run/mysqld/mysqld.sock"
        )
        .unwrap()
        .socket()
        .is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mysql_missing_env_is_unknown() {
        let svc = ServiceConfig {
            name: "mysql".into(),
            kind: "mysql_status".into(),
            url: None,
            addr: None,
            process_match: None,
            timeout_ms: None,
            dsn_env: Some("TINY_LOG_TEST_DSN_NOT_SET".into()),
            host: None,
        };
        let r = check_one(svc).await;
        assert_eq!(r.status, "unknown");
        assert_eq!(r.message.as_deref(), Some("env TINY_LOG_TEST_DSN_NOT_SET is not set"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dns_timeout_and_success_paths() {
        let r = dns_check_with("resolver", "db.internal", 20, std::future::pending()).await;
        assert_eq!(r.status, "down");
        assert_eq!(r.message.as_deref(), Some("db.internal: timeout after 20 ms"));

        let ok = dns_check_with("resolver", "db.internal", 1000, async {
            Ok(vec!["10.0.0.5".parse().unwrap(), "10.0.0.5".parse().unwrap()])
        })
        .await;
        assert_eq!(ok.status, "up");
        assert_eq!(ok.load_hint.as_deref(), Some("ok"));
        assert_eq!(ok.meta.as_ref().unwrap()["addrs"], json!(["10.0.0.5"]));

        let err = dns_check_with("resolver", "nope.invalid", 1000, async {
            Err(std::io::Error::other("failed to lookup address information"))
        })
        .await;
        assert_eq!(err.status, "down");

        let slow = dns_check_with("resolver", "slow", 2000, async {
            tokio::time::sleep(Duration::from_millis(520)).await;
            Ok(vec!["10.0.0.6".parse().unwrap()])
        })
        .await;
        assert_eq!(slow.load_hint.as_deref(), Some("watch"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dns_resolves_localhost() {
        let svc = ServiceConfig {
            name: "localhost".into(),
            kind: "dns".into(),
            url: None,
            addr: None,
            process_match: None,
            timeout_ms: Some(2000),
            dsn_env: None,
            host: None,
        };
        let r = check_one(svc).await;
        assert_eq!(r.status, "up", "{:?}", r.message);
    }

    /// `cargo test -- --ignored mysql_live` with TINY_LOG_IT_MYSQL_DSN (monitor user,
    /// no privileges) and TINY_LOG_IT_MYSQL_ROOT_DSN (to provoke "Too many connections").
    /// Optional TINY_LOG_IT_MYSQL_SOCKET_DSN checks the unix-socket form too.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "needs a live MySQL"]
    async fn mysql_live() {
        use mysql_async::prelude::Queryable;
        let svc = |name: &str, env: &str| ServiceConfig {
            name: name.into(),
            kind: "mysql_status".into(),
            url: None,
            addr: None,
            process_match: None,
            timeout_ms: Some(5000),
            dsn_env: Some(env.into()),
            host: None,
        };
        let tcp = svc("it-mysql", "TINY_LOG_IT_MYSQL_DSN");
        let r = check_one(tcp.clone()).await;
        println!("first: {}", serde_json::to_string(&r).unwrap());
        assert_eq!(r.status, "up", "{:?}", r.message);
        let meta = r.meta.unwrap();
        assert!(meta["Threads_connected"].as_u64().unwrap() >= 1);
        assert!(meta["Connections_delta"].is_null());

        let r = check_one(tcp.clone()).await;
        println!("second: {}", serde_json::to_string(&r).unwrap());
        assert_eq!(r.meta.as_ref().unwrap()["Connections_delta"], 1, "one connection per check");

        if std::env::var("TINY_LOG_IT_MYSQL_SOCKET_DSN").is_ok() {
            let r = check_one(svc("it-mysql-sock", "TINY_LOG_IT_MYSQL_SOCKET_DSN")).await;
            println!("socket: {}", serde_json::to_string(&r).unwrap());
            assert_eq!(r.status, "up", "{:?}", r.message);
        }

        let root_dsn = std::env::var("TINY_LOG_IT_MYSQL_ROOT_DSN").unwrap();
        let mut root = mysql_async::Conn::new(mysql_async::Opts::from_url(&root_dsn).unwrap())
            .await
            .unwrap();
        root.query_drop("SET GLOBAL max_connections = 1").await.unwrap();
        let r = check_one(tcp.clone()).await.finalize_resources();
        println!("full: {}", serde_json::to_string(&r).unwrap());
        root.query_drop("SET GLOBAL max_connections = 151").await.unwrap();
        assert_eq!(r.status, "down");
        assert!(r.message.as_deref().unwrap().contains("Too many connections"));
        assert_eq!(r.recommend.as_deref(), Some("limit"));

        let r = check_one(tcp).await.finalize_resources();
        println!("after: {}", serde_json::to_string(&r).unwrap());
        assert_eq!(r.status, "up");
        assert_eq!(r.load_hint.as_deref(), Some("tight"), "refused connection since last run");
        assert!(r.meta.unwrap()["Connection_errors_max_connections_delta"].as_u64().unwrap() >= 1);
        let _ = root.disconnect().await;
    }

    const STAT_A: &str = "cpu  1000 0 500 8000 100 0 0 50 0 0\ncpu0 500 0 250 4000 50 0 0 25 0 0\nintr 1\n";
    const STAT_B: &str = "cpu  1100 0 550 8200 200 0 0 100 0 0\ncpu0 1 1 1 1 1 0 0 0 0 0\n";

    #[test]
    fn proc_stat_window() {
        let a = parse_proc_stat_cpu(STAT_A).unwrap();
        let b = parse_proc_stat_cpu(STAT_B).unwrap();
        assert_eq!(a, CpuTimes { total: 9650, iowait: 100, steal: 50 });
        // Δtotal = 100+50+200+100+50 = 500
        assert_eq!(cpu_window_pcts(a, b), Some((20.0, 10.0)));
        assert_eq!(cpu_window_pcts(a, a), None);
        assert!(parse_proc_stat_cpu("intr 1\n").is_none());
    }

    #[test]
    fn psi_parse() {
        let io = "some avg10=12.50 avg60=3.00 avg300=1.00 total=123\nfull avg10=11.25 avg60=2.00 avg300=0.50 total=99\n";
        assert_eq!(parse_psi(io), Psi { some_avg10: Some(12.5), full_avg10: Some(11.25) });
        // older kernels: cpu has no "full" line
        let cpu = "some avg10=0.40 avg60=0.20 avg300=0.10 total=5\n";
        assert_eq!(parse_psi(cpu), Psi { some_avg10: Some(0.4), full_avg10: None });
    }

    #[test]
    fn snmp_retrans() {
        let snmp = "Ip: Forwarding DefaultTTL\nIp: 1 64\nTcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors\nTcp: 1 200 120000 -1 10 20 0 0 5 1000 2000 30 0 1 0\nUdp: InDatagrams\nUdp: 1\n";
        assert_eq!(parse_snmp_tcp(snmp), Some((2000, 30)));
        assert_eq!(retrans_pct((2000, 30), (4000, 50)), Some(1.0));
        assert_eq!(retrans_pct((2000, 30), (2000, 30)), Some(0.0));
        assert_eq!(retrans_pct((2000, 30), (100, 1)), None, "counter reset");
    }

    #[test]
    fn host_io_hints() {
        assert_eq!(host_io_hint(Some(1.0), Some(0.0), Some(0.1)), LoadHint::Ok);
        assert_eq!(host_io_hint(Some(16.0), Some(0.0), None), LoadHint::Watch);
        assert_eq!(host_io_hint(None, Some(10.5), None), LoadHint::Watch);
        assert_eq!(host_io_hint(None, None, Some(10.1)), LoadHint::Tight);
        assert_eq!(host_io_hint(Some(31.0), None, None), LoadHint::Tight);
        assert_eq!(host_io_hint(None, Some(21.0), Some(0.0)), LoadHint::Tight);
        assert_eq!(host_io_hint(None, None, None), LoadHint::Ok);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    async fn host_io_reads_proc() {
        let svc = ServiceConfig {
            name: "hfc-io".into(),
            kind: "host_io".into(),
            url: None,
            addr: None,
            process_match: None,
            timeout_ms: None,
            dsn_env: None,
            host: None,
        };
        let r = check_one(svc.clone()).await;
        assert_eq!(r.status, "up");
        let meta = r.meta.unwrap();
        assert!(meta["iowait_pct"].is_number());
        assert!(meta["retrans_pct"].is_null(), "first run has no TCP delta");
        let r2 = check_one(svc).await;
        assert!(r2.meta.unwrap().get("retrans_pct").is_some());
    }
}
