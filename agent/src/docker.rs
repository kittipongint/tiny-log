use crate::checks::{http_probe, ServiceCheckResult};
use crate::config::AgentConfig;
use crate::load::{
    apply_es_recommend, looks_like_cluster_health_url, mem_pct, merge_hint, resource_load_hint,
    service_recommend, LoadHint,
};
use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

pub async fn collect_checks(cfg: &AgentConfig) -> Result<Vec<ServiceCheckResult>> {
    if !cfg.docker_sock.exists() {
        return Ok(Vec::new());
    }

    let containers = list_containers(&cfg.docker_sock).await?;
    let sock = cfg.docker_sock.clone();

    let results = stream::iter(containers)
        .map(|c| {
            let sock = sock.clone();
            async move { check_container(&sock, c).await }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;

    Ok(results.into_iter().flatten().collect())
}

async fn check_container(sock: &Path, c: DockerContainer) -> Option<ServiceCheckResult> {
    let labels = c.labels.unwrap_or_default();
    let monitor = labels
        .get("tiny-log.monitor")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if !monitor {
        return None;
    }

    let name = labels
        .get("tiny-log.service")
        .cloned()
        .or_else(|| {
            c.names
                .as_ref()
                .and_then(|n| n.first())
                .map(|n| n.trim_start_matches('/').to_string())
        })
        .unwrap_or_else(|| c.id.chars().take(12).collect());

    let state = c.state.unwrap_or_default().to_lowercase();
    let running = state == "running";

    let health_url = labels.get("tiny-log.health").cloned();
    let load_url = labels.get("tiny-log.load").cloned();

    let mut result = if let Some(ref url) = health_url {
        http_probe(&name, "http", url, 2000).await
    } else {
        let health = c.status.as_deref().unwrap_or("").to_lowercase();
        let (status, message) = if running {
            if health.contains("(healthy)") {
                ("up", "docker healthy")
            } else if health.contains("(unhealthy)") {
                ("down", "docker unhealthy")
            } else {
                ("up", "docker running")
            }
        } else {
            ("down", "docker not running")
        };
        ServiceCheckResult {
            name: name.clone(),
            kind: "docker".into(),
            status: status.into(),
            latency_ms: None,
            message: Some(message.into()),
            cpu_pct: None,
            mem_used_bytes: None,
            mem_limit_bytes: None,
            load_hint: None,
            recommend: None,
        }
    };

    if let Some(ref url) = load_url {
        if Some(url.as_str()) != health_url.as_deref() {
            let load_probe = http_probe(&name, "http", url, 2000).await;
            let load_hint = load_probe
                .load_hint
                .as_deref()
                .and_then(|s| match s {
                    "ok" => Some(LoadHint::Ok),
                    "watch" => Some(LoadHint::Watch),
                    "tight" => Some(LoadHint::Tight),
                    _ => None,
                })
                .or_else(|| {
                    // Re-parse from message path via fresh GET body already done in probe
                    if looks_like_cluster_health_url(url) {
                        load_probe.load_hint.as_deref().and_then(|s| match s {
                            "ok" => Some(LoadHint::Ok),
                            "watch" => Some(LoadHint::Watch),
                            "tight" => Some(LoadHint::Tight),
                            _ => None,
                        })
                    } else {
                        None
                    }
                });
            let existing = result.load_hint.as_deref().and_then(|s| match s {
                "ok" => Some(LoadHint::Ok),
                "watch" => Some(LoadHint::Watch),
                "tight" => Some(LoadHint::Tight),
                _ => None,
            });
            let merged = merge_hint(existing, load_hint);
            let mut rec = service_recommend(&result.status, merged, result.latency_ms);
            if looks_like_cluster_health_url(url) {
                rec = apply_es_recommend(merged, rec);
            }
            result.load_hint = merged.map(|h| h.as_str().into());
            result.recommend = Some(rec.as_str().into());
            if let Some(msg) = load_probe.message {
                result.message = Some(format!(
                    "{}; load={}",
                    result.message.unwrap_or_default(),
                    msg
                ));
            }
        }
    }

    if running {
        if let Ok(stats) = container_stats(sock, &c.id).await {
            result.cpu_pct = stats.cpu_pct;
            result.mem_used_bytes = stats.mem_used_bytes;
            result.mem_limit_bytes = stats.mem_limit_bytes;
        }
    }

    let res_hint = resource_load_hint(
        result.cpu_pct,
        mem_pct(result.mem_used_bytes, result.mem_limit_bytes),
    );
    let existing = result.load_hint.as_deref().and_then(|s| match s {
        "ok" => Some(LoadHint::Ok),
        "watch" => Some(LoadHint::Watch),
        "tight" => Some(LoadHint::Tight),
        _ => None,
    });
    let hint = merge_hint(existing, res_hint);
    let mut rec = service_recommend(&result.status, hint, result.latency_ms);
    if health_url
        .as_deref()
        .is_some_and(looks_like_cluster_health_url)
        || load_url.as_deref().is_some_and(looks_like_cluster_health_url)
    {
        rec = apply_es_recommend(hint, rec);
    }
    result.load_hint = hint.map(|h| h.as_str().into());
    result.recommend = Some(rec.as_str().into());

    Some(result)
}

struct ContainerResources {
    cpu_pct: Option<f64>,
    mem_used_bytes: Option<i64>,
    mem_limit_bytes: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct DockerStats {
    cpu_stats: Option<CpuStats>,
    precpu_stats: Option<CpuStats>,
    memory_stats: Option<MemoryStats>,
}

#[derive(Debug, Deserialize)]
struct CpuStats {
    cpu_usage: Option<CpuUsage>,
    system_cpu_usage: Option<u64>,
    online_cpus: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CpuUsage {
    total_usage: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct MemoryStats {
    usage: Option<u64>,
    limit: Option<u64>,
}

async fn container_stats(sock: &Path, id: &str) -> Result<ContainerResources> {
    let body = docker_get(sock, &format!("/containers/{id}/stats?stream=false")).await?;
    let stats: DockerStats = serde_json::from_slice(&body).context("parse docker stats")?;

    let cpu_pct = match (
        stats.cpu_stats.as_ref(),
        stats.precpu_stats.as_ref(),
    ) {
        (Some(cur), Some(pre)) => {
            let cpu_delta = cur
                .cpu_usage
                .as_ref()
                .and_then(|u| u.total_usage)
                .unwrap_or(0)
                .saturating_sub(
                    pre.cpu_usage
                        .as_ref()
                        .and_then(|u| u.total_usage)
                        .unwrap_or(0),
                ) as f64;
            let system_delta = cur
                .system_cpu_usage
                .unwrap_or(0)
                .saturating_sub(pre.system_cpu_usage.unwrap_or(0)) as f64;
            let ncpus = cur.online_cpus.unwrap_or(1).max(1) as f64;
            if system_delta > 0.0 && cpu_delta >= 0.0 {
                Some(((cpu_delta / system_delta) * ncpus * 100.0).clamp(0.0, ncpus * 100.0))
            } else {
                None
            }
        }
        _ => None,
    };

    let mem_used_bytes = stats
        .memory_stats
        .as_ref()
        .and_then(|m| m.usage)
        .map(|u| u as i64);
    let mem_limit_bytes = stats
        .memory_stats
        .as_ref()
        .and_then(|m| m.limit)
        .map(|u| u as i64);

    Ok(ContainerResources {
        cpu_pct,
        mem_used_bytes,
        mem_limit_bytes,
    })
}

#[derive(Debug, Deserialize)]
struct DockerContainer {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Names")]
    names: Option<Vec<String>>,
    #[serde(rename = "Labels")]
    labels: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "State")]
    state: Option<String>,
    #[serde(rename = "Status")]
    status: Option<String>,
}

async fn list_containers(sock: &Path) -> Result<Vec<DockerContainer>> {
    let body = docker_get(sock, "/containers/json").await?;
    serde_json::from_slice(&body).context("parse docker containers json")
}

async fn docker_get(sock: &Path, path: &str) -> Result<Bytes> {
    let stream = UnixStream::connect(sock)
        .await
        .with_context(|| format!("connect {}", sock.display()))?;
    let io = hyper_util::rt::TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .context("docker handshake")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = http::Request::builder()
        .method("GET")
        .uri(format!("http://localhost{path}"))
        .header("Host", "localhost")
        .body(Full::new(Bytes::new()))
        .context("build docker request")?;

    let response = tokio::time::timeout(Duration::from_secs(3), sender.send_request(req))
        .await
        .context("docker request timeout")?
        .context("docker request")?;
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .context("read docker body")?
        .to_bytes();
    if !status.is_success() {
        anyhow::bail!(
            "docker API status {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(body)
}
