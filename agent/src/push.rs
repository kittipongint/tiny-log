use crate::checks::ServiceCheckResult;
use crate::config::AgentConfig;
use crate::host::SystemSample;
use anyhow::{Context, Result};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct PushClient {
    cfg: Arc<AgentConfig>,
    http: reqwest::Client,
}

impl PushClient {
    pub fn new(cfg: Arc<AgentConfig>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(2)
            .build()
            .expect("http client");
        Self { cfg, http }
    }

    pub async fn send_system(&self, system: SystemSample) -> Result<()> {
        let body = MetricsBatch {
            host: self.cfg.host_name.clone(),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
            system: Some(system),
            services: Vec::new(),
        };
        self.post_with_retry(body).await
    }

    pub async fn send_services(&self, services: Vec<ServiceCheckResult>) -> Result<()> {
        let body = MetricsBatch {
            host: self.cfg.host_name.clone(),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
            system: None,
            services,
        };
        self.post_with_retry(body).await
    }

    async fn post_with_retry(&self, body: MetricsBatch) -> Result<()> {
        let mut last_err = None;
        for attempt in 0..3u32 {
            match self.post(&body).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let msg = err.to_string();
                    if msg.contains("push rejected") {
                        return Err(err);
                    }
                    last_err = Some(err);
                    if attempt < 2 {
                        let backoff = Duration::from_millis(200 * 2u64.pow(attempt));
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }
        Err(last_err.expect("retry loop"))
    }

    async fn post(&self, body: &MetricsBatch) -> Result<()> {
        let url = format!("{}/api/v1/metrics/batch", self.cfg.url);
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.api_key)
            .json(body)
            .send()
            .await
            .with_context(|| format!("metrics push request to {url}"))?;
        if res.status().is_success() {
            return Ok(());
        }
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if matches!(status.as_u16(), 400 | 401 | 403 | 413) {
            anyhow::bail!("metrics push rejected: {status} {text}");
        }
        anyhow::bail!("metrics push failed: {status} {text}");
    }
}

#[derive(Debug, Serialize)]
struct MetricsBatch {
    host: String,
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<SystemSample>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    services: Vec<ServiceCheckResult>,
}
