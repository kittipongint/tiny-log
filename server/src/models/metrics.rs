use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct MetricsBatchRequest {
    pub host: String,
    pub timestamp: Option<String>,
    pub system: Option<HostSampleInput>,
    #[serde(default)]
    pub services: Vec<ServiceCheckInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiskSample {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub used_bytes: Option<i64>,
    pub total_bytes: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostSampleInput {
    pub cpu_pct: Option<f64>,
    pub mem_used_bytes: Option<i64>,
    pub mem_total_bytes: Option<i64>,
    pub disk_used_bytes: Option<i64>,
    pub disk_total_bytes: Option<i64>,
    #[serde(default)]
    pub disks: Vec<DiskSample>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub n_cpus: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceCheckInput {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub message: Option<String>,
    pub meta: Option<Value>,
    pub cpu_pct: Option<f64>,
    pub mem_used_bytes: Option<i64>,
    pub mem_limit_bytes: Option<i64>,
    pub load_hint: Option<String>,
    pub recommend: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostSample {
    pub id: i64,
    pub timestamp: String,
    pub host: String,
    pub cpu_pct: Option<f64>,
    pub mem_used_bytes: Option<i64>,
    pub mem_total_bytes: Option<i64>,
    pub disk_used_bytes: Option<i64>,
    pub disk_total_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<DiskSample>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub n_cpus: Option<i64>,
}

/// Latest host row for monitor overview (sample + derived capacity fields).
#[derive(Debug, Clone, Serialize)]
pub struct HostOverview {
    pub id: i64,
    pub timestamp: String,
    pub host: String,
    pub cpu_pct: Option<f64>,
    pub mem_used_bytes: Option<i64>,
    pub mem_total_bytes: Option<i64>,
    pub disk_used_bytes: Option<i64>,
    pub disk_total_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<DiskSample>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub n_cpus: Option<i64>,
    pub mem_pct: Option<f64>,
    pub disk_pct: Option<f64>,
    pub load_per_cpu: Option<f64>,
    pub headroom_pct: Option<f64>,
    pub recommend: String,
}

impl HostSample {
    pub fn into_overview(self) -> HostOverview {
        let mem_pct = pct(self.mem_used_bytes, self.mem_total_bytes);
        let disk_pct = worst_disk_pct(&self.disks)
            .or_else(|| pct(self.disk_used_bytes, self.disk_total_bytes));
        let load_per_cpu = match (self.load1, self.n_cpus) {
            (Some(l), Some(n)) if n > 0 => Some(l / n as f64),
            _ => None,
        };
        let headroom_pct = match (self.cpu_pct, mem_pct) {
            (Some(c), Some(m)) => Some((100.0 - c).min(100.0 - m).max(0.0)),
            (Some(c), None) => Some((100.0 - c).max(0.0)),
            (None, Some(m)) => Some((100.0 - m).max(0.0)),
            _ => None,
        };
        let recommend = host_recommend(self.cpu_pct, mem_pct, disk_pct, load_per_cpu);
        HostOverview {
            id: self.id,
            timestamp: self.timestamp,
            host: self.host,
            cpu_pct: self.cpu_pct,
            mem_used_bytes: self.mem_used_bytes,
            mem_total_bytes: self.mem_total_bytes,
            disk_used_bytes: self.disk_used_bytes,
            disk_total_bytes: self.disk_total_bytes,
            disks: self.disks,
            load1: self.load1,
            load5: self.load5,
            load15: self.load15,
            n_cpus: self.n_cpus,
            mem_pct,
            disk_pct,
            load_per_cpu,
            headroom_pct,
            recommend,
        }
    }
}

pub fn worst_disk_pct(disks: &[DiskSample]) -> Option<f64> {
    disks
        .iter()
        .filter_map(|d| pct(d.used_bytes, d.total_bytes))
        .fold(None, |acc: Option<f64>, p| match acc {
            Some(a) if a >= p => Some(a),
            _ => Some(p),
        })
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceCheck {
    pub id: i64,
    pub timestamp: String,
    pub host: String,
    pub service: String,
    pub kind: String,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    pub cpu_pct: Option<f64>,
    pub mem_used_bytes: Option<i64>,
    pub mem_limit_bytes: Option<i64>,
    pub load_hint: Option<String>,
    pub recommend: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MetricsHistoryQuery {
    pub host: Option<String>,
    pub service: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<i64>,
    /// "host" or "service" (default service if service filter set, else host)
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CapacityQuery {
    pub host: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapacityForecast {
    pub cpu_pct_1h: Option<f64>,
    pub cpu_pct_6h: Option<f64>,
    pub mem_pct_1h: Option<f64>,
    pub mem_pct_6h: Option<f64>,
    pub eta_hours_to_cpu_85: Option<f64>,
    pub eta_hours_to_mem_85: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostCapacity {
    pub host: String,
    pub timestamp: String,
    pub cpu_pct: Option<f64>,
    pub mem_pct: Option<f64>,
    pub disk_pct: Option<f64>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub n_cpus: Option<i64>,
    pub load_per_cpu: Option<f64>,
    pub headroom_pct: Option<f64>,
    pub recommend: String,
    pub forecast: Option<CapacityForecast>,
}

pub fn pct(used: Option<i64>, total: Option<i64>) -> Option<f64> {
    match (used, total) {
        (Some(u), Some(t)) if t > 0 => Some((u as f64 / t as f64) * 100.0),
        _ => None,
    }
}

pub fn host_recommend(
    cpu_pct: Option<f64>,
    mem_pct: Option<f64>,
    disk_pct: Option<f64>,
    load_per_cpu: Option<f64>,
) -> String {
    if disk_pct.is_some_and(|d| d >= 90.0) {
        return "limit".into();
    }
    let hot = |v: Option<f64>, hi: f64| v.is_some_and(|x| x >= hi);
    if hot(cpu_pct, 85.0) || hot(mem_pct, 85.0) || hot(load_per_cpu, 1.0) {
        return "scale_out".into();
    }
    let warn = |v: Option<f64>, lo: f64, hi: f64| v.is_some_and(|x| x >= lo && x < hi);
    if warn(cpu_pct, 70.0, 85.0)
        || warn(mem_pct, 70.0, 85.0)
        || load_per_cpu.is_some_and(|l| l >= 0.7 && l < 1.0)
    {
        return "watch".into();
    }
    "ok".into()
}

pub fn linear_forecast(points: &[(f64, f64)], hours_ahead: f64) -> Option<f64> {
    if points.len() < 8 {
        return None;
    }
    let n = points.len() as f64;
    let sum_x: f64 = points.iter().map(|(x, _)| x).sum();
    let sum_y: f64 = points.iter().map(|(_, y)| y).sum();
    let sum_xx: f64 = points.iter().map(|(x, _)| x * x).sum();
    let sum_xy: f64 = points.iter().map(|(x, y)| x * y).sum();
    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < f64::EPSILON {
        return None;
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denom;
    let intercept = (sum_y - slope * sum_x) / n;
    let x_last = points.iter().map(|(x, _)| *x).fold(f64::NEG_INFINITY, f64::max);
    let ms_ahead = hours_ahead * 3_600_000.0;
    Some(intercept + slope * (x_last + ms_ahead))
}

pub fn eta_hours_to_threshold(points: &[(f64, f64)], threshold: f64) -> Option<f64> {
    if points.len() < 8 {
        return None;
    }
    let n = points.len() as f64;
    let sum_x: f64 = points.iter().map(|(x, _)| x).sum();
    let sum_y: f64 = points.iter().map(|(_, y)| y).sum();
    let sum_xx: f64 = points.iter().map(|(x, _)| x * x).sum();
    let sum_xy: f64 = points.iter().map(|(x, y)| x * y).sum();
    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < f64::EPSILON {
        return None;
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denom;
    if slope <= 0.0 {
        return None;
    }
    let intercept = (sum_y - slope * sum_x) / n;
    let x_last = points.iter().map(|(x, _)| *x).fold(f64::NEG_INFINITY, f64::max);
    let y_last = intercept + slope * x_last;
    if y_last >= threshold {
        return Some(0.0);
    }
    let x_hit = (threshold - intercept) / slope;
    let hours = (x_hit - x_last) / 3_600_000.0;
    if hours.is_finite() && hours >= 0.0 {
        Some(hours)
    } else {
        None
    }
}
