use crate::checks::ServiceCheckResult;
use crate::config::AgentConfig;
use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use serde::Deserialize;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::net::UnixStream;

pub async fn collect_checks(cfg: &AgentConfig) -> Result<Vec<ServiceCheckResult>> {
    if !cfg.docker_sock.exists() {
        return Ok(Vec::new());
    }

    let containers = list_containers(&cfg.docker_sock).await?;
    let mut out = Vec::new();

    for c in containers {
        let labels = c.labels.unwrap_or_default();
        let monitor = labels
            .get("tiny-log.monitor")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        if !monitor {
            continue;
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

        if let Some(health_url) = labels.get("tiny-log.health") {
            out.push(http_check(&name, health_url).await);
            continue;
        }

        let state = c.state.unwrap_or_default().to_lowercase();
        let health = c.status.as_deref().unwrap_or("").to_lowercase();
        let (status, message) = if state == "running" {
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

        out.push(ServiceCheckResult {
            name,
            kind: "docker".into(),
            status: status.into(),
            latency_ms: None,
            message: Some(message.into()),
        });
    }

    Ok(out)
}

async fn http_check(name: &str, url: &str) -> ServiceCheckResult {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(2000))
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            return ServiceCheckResult {
                name: name.into(),
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
            let code = resp.status().as_u16();
            let ok = resp.status().is_success();
            ServiceCheckResult {
                name: name.into(),
                kind: "http".into(),
                status: if ok { "up" } else { "down" }.into(),
                latency_ms: Some(started.elapsed().as_millis() as i64),
                message: Some(code.to_string()),
            }
        }
        Err(err) => ServiceCheckResult {
            name: name.into(),
            kind: "http".into(),
            status: "down".into(),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: Some(err.to_string()),
        },
    }
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
        .uri("http://localhost/containers/json")
        .header("Host", "localhost")
        .body(Full::new(Bytes::new()))
        .context("build docker request")?;

    let response = sender
        .send_request(req)
        .await
        .context("docker list containers")?;
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
    serde_json::from_slice(&body).context("parse docker containers json")
}
