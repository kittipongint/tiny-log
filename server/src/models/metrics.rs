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

#[derive(Debug, Clone, Deserialize)]
pub struct HostSampleInput {
    pub cpu_pct: Option<f64>,
    pub mem_used_bytes: Option<i64>,
    pub mem_total_bytes: Option<i64>,
    pub disk_used_bytes: Option<i64>,
    pub disk_total_bytes: Option<i64>,
    pub load1: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceCheckInput {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub message: Option<String>,
    pub meta: Option<Value>,
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
    pub load1: Option<f64>,
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
