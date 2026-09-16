use crate::config::{AgentConfig, ServiceConfig};
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
}

pub async fn run_configured(cfg: &AgentConfig) -> Result<Vec<ServiceCheckResult>> {
    let services = cfg.services.clone();
    let results = stream::iter(services)
        .map(|svc| async move { check_one(svc).await })
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
        "process" => check_process(&svc),
        other => ServiceCheckResult {
            name: svc.name,
            kind: other.to_string(),
            status: "unknown".into(),
            latency_ms: None,
            message: Some(format!("unsupported kind: {other}")),
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
        };
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            return ServiceCheckResult {
                name: svc.name.clone(),
                kind: "http".into(),
                status: "down".into(),
                latency_ms: None,
                message: Some(err.to_string()),
            };
        }
    };

    let started = Instant::now();
    match client.get(url).send().await {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let ok = resp.status().is_success();
            ServiceCheckResult {
                name: svc.name.clone(),
                kind: "http".into(),
                status: if ok { "up" } else { "down" }.into(),
                latency_ms: Some(started.elapsed().as_millis() as i64),
                message: Some(status_code.to_string()),
            }
        }
        Err(err) => ServiceCheckResult {
            name: svc.name.clone(),
            kind: "http".into(),
            status: "down".into(),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: Some(err.to_string()),
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
        };
    };

    let started = Instant::now();
    match timeout(
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
        },
        Ok(Err(err)) => ServiceCheckResult {
            name: svc.name.clone(),
            kind: "tcp".into(),
            status: "down".into(),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: Some(err.to_string()),
        },
        Err(_) => ServiceCheckResult {
            name: svc.name.clone(),
            kind: "tcp".into(),
            status: "down".into(),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: Some("timeout".into()),
        },
    }
}

fn check_process(svc: &ServiceConfig) -> ServiceCheckResult {
    let Some(needle) = svc.process_match.as_ref() else {
        return ServiceCheckResult {
            name: svc.name.clone(),
            kind: "process".into(),
            status: "unknown".into(),
            latency_ms: None,
            message: Some("match missing".into()),
        };
    };

    #[cfg(target_os = "linux")]
    {
        let found = match std::fs::read_dir("/proc") {
            Ok(entries) => entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name();
                    let pid = name.to_str()?;
                    if !pid.chars().all(|c| c.is_ascii_digit()) {
                        return None;
                    }
                    std::fs::read_to_string(e.path().join("cmdline")).ok()
                })
                .any(|cmdline| cmdline.replace('\0', " ").contains(needle.as_str())),
            Err(err) => {
                return ServiceCheckResult {
                    name: svc.name.clone(),
                    kind: "process".into(),
                    status: "unknown".into(),
                    latency_ms: None,
                    message: Some(err.to_string()),
                };
            }
        };

        ServiceCheckResult {
            name: svc.name.clone(),
            kind: "process".into(),
            status: if found { "up" } else { "down" }.into(),
            latency_ms: None,
            message: Some(if found {
                "process found".into()
            } else {
                "process not found".into()
            }),
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
        }
    }
}
