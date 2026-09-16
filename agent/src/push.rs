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
        self.post(body).await
    }

    pub async fn send_services(&self, services: Vec<ServiceCheckResult>) -> Result<()> {
        let body = MetricsBatch {
            host: self.cfg.host_name.clone(),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
            system: None,
            services,
        };
        self.post(body).await
    }

    async fn post(&self, body: MetricsBatch) -> Result<()> {
        let url = format!("{}/api/v1/metrics/batch", self.cfg.url);
        let res = self
            .http
            .post(url)
            .bearer_auth(&self.cfg.api_key)
            .json(&body)
            .send()
            .await
            .context("metrics push request")?;
        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            anyhow::bail!("metrics push failed: {status} {text}");
        }
        Ok(())
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
