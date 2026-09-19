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
    /// Mounts to sample; at least one (defaults to `/` as `root`).
    pub disks: Vec<DiskMount>,
    pub services: Vec<ServiceConfig>,
}

#[derive(Debug, Clone)]
pub struct DiskMount {
    pub name: String,
    pub path: PathBuf,
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

#[derive(Debug, Clone, Deserialize)]
struct DiskFileConfig {
    name: Option<String>,
    path: String,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    system_interval_secs: Option<u64>,
    service_interval_secs: Option<u64>,
    docker: Option<bool>,
    docker_sock: Option<String>,
    /// Legacy single-disk path (used when `[[disk]]` is empty).
    disk_path: Option<String>,
    host_name: Option<String>,
    #[serde(default)]
    disk: Vec<DiskFileConfig>,
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

        let disks = resolve_disks(&file_cfg)?;

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
            disks,
            services: file_cfg.service,
        })
    }
}

fn resolve_disks(file_cfg: &FileConfig) -> Result<Vec<DiskMount>> {
    if !file_cfg.disk.is_empty() {
        let mut out = Vec::with_capacity(file_cfg.disk.len());
        let mut names = std::collections::HashSet::new();
        for d in &file_cfg.disk {
            let path = d.path.trim();
            if path.is_empty() {
                bail!("[[disk]] path must be non-empty");
            }
            let name = d
                .name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_disk_name(path));
            if !names.insert(name.clone()) {
                bail!("duplicate [[disk]] name: {name}");
            }
            out.push(DiskMount {
                name,
                path: PathBuf::from(path),
            });
        }
        return Ok(out);
    }

    let path = env::var("TINY_LOG_DISK_PATH")
        .ok()
        .or_else(|| file_cfg.disk_path.clone())
        .unwrap_or_else(|| "/".into());
    Ok(vec![DiskMount {
        name: default_disk_name(&path),
        path: PathBuf::from(path),
    }])
}

fn default_disk_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return "root".into();
    }
    PathBuf::from(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("disk")
        .to_string()
}

fn clamp_interval(v: u64) -> u64 {
    v.clamp(30, 300)
}

fn env_u64(key: &str) -> Option<u64> {
    env::var(key).ok().and_then(|v| v.parse().ok())
}
