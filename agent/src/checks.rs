use crate::config::{AgentConfig, ServiceConfig};
use crate::load::{
    apply_es_recommend, looks_like_cluster_health_url, mem_pct, merge_hint, parse_es_health_hint,
    resource_load_hint, service_recommend, LoadHint, Recommend,
};
use anyhow::Result;
use futures::stream::{self, StreamExt};
use serde::Serialize;
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
        other => ServiceCheckResult {
            name: svc.name,
            kind: other.to_string(),
            status: "unknown".into(),
            latency_ms: None,
            message: Some(format!("unsupported kind: {other}")),
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
