use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub url: String,
    pub api_key: String,
    pub host_name: String,
    pub system_interval_secs: u64,
    pub service_interval_secs: u64,
    pub docker_enabled: bool,
    pub docker_sock: PathBuf,
    pub disk_path: PathBuf,
    pub services: Vec<ServiceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub kind: String,
    pub url: Option<String>,
    pub addr: Option<String>,
    #[serde(rename = "match")]
    pub process_match: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    system_interval_secs: Option<u64>,
    service_interval_secs: Option<u64>,
    docker: Option<bool>,
    docker_sock: Option<String>,
    disk_path: Option<String>,
    host_name: Option<String>,
    #[serde(default)]
    service: Vec<ServiceConfig>,
}

impl AgentConfig {
    pub fn load() -> Result<Self> {
        let config_path = env::var("TINY_LOG_AGENT_CONFIG")
            .unwrap_or_else(|_| "./agent.toml".into());
        let file_cfg = if PathBuf::from(&config_path).is_file() {
            let raw = fs::read_to_string(&config_path)
                .with_context(|| format!("read {config_path}"))?;
            toml::from_str::<FileConfig>(&raw).context("parse agent.toml")?
        } else {
            FileConfig::default()
        };

        let url = env::var("TINY_LOG_URL").context("TINY_LOG_URL is required")?;
        let api_key = env::var("TINY_LOG_API_KEY").context("TINY_LOG_API_KEY is required")?;

        let host_name = env::var("TINY_LOG_HOST_NAME")
            .ok()
            .filter(|s| !s.is_empty())
            .or(file_cfg.host_name)
            .or_else(|| hostname::get().ok().and_then(|h| h.into_string().ok()))
            .unwrap_or_else(|| "unknown-host".into());

        let system_interval_secs = clamp_interval(
            env_u64("TINY_LOG_SYSTEM_INTERVAL_SECS")
                .or(file_cfg.system_interval_secs)
                .unwrap_or(60),
        );
        let service_interval_secs = clamp_interval(
            env_u64("TINY_LOG_SERVICE_INTERVAL_SECS")
                .or(file_cfg.service_interval_secs)
                .unwrap_or(60),
        );

        let docker_sock = PathBuf::from(
            env::var("TINY_LOG_DOCKER_SOCK")
                .ok()
                .or(file_cfg.docker_sock)
                .unwrap_or_else(|| "/var/run/docker.sock".into()),
        );

        let docker_enabled = match env::var("TINY_LOG_DOCKER") {
            Ok(v) => matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
            Err(_) => file_cfg.docker.unwrap_or_else(|| docker_sock.exists()),
        };

        let disk_path = PathBuf::from(
            env::var("TINY_LOG_DISK_PATH")
                .ok()
                .or(file_cfg.disk_path)
                .unwrap_or_else(|| "/".into()),
        );

        if url.trim().is_empty() || api_key.trim().is_empty() {
            bail!("TINY_LOG_URL and TINY_LOG_API_KEY must be non-empty");
        }

        Ok(Self {
            url: url.trim_end_matches('/').to_string(),
            api_key,
            host_name,
            system_interval_secs,
            service_interval_secs,
            docker_enabled,
            docker_sock,
            disk_path,
            services: file_cfg.service,
        })
    }
}

fn clamp_interval(v: u64) -> u64 {
    v.clamp(30, 300)
}

fn env_u64(key: &str) -> Option<u64> {
    env::var(key).ok().and_then(|v| v.parse().ok())
}
