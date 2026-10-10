use crate::logs::LineFilter;
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
    /// `[[logs]]` sources to tail and ship to `/api/v1/logs/batch`.
    pub logs: Vec<LogSource>,
    /// Ship a log line when a disk reaches this % used (0 = off).
    pub disk_warn_pct: u64,
}

#[derive(Debug, Clone)]
pub struct LogSource {
    pub app: String,
    pub target: LogTarget,
    pub filter: LineFilter,
    /// 0 = unlimited.
    pub max_lines_per_min: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogTarget {
    File(PathBuf),
    /// Docker container name (or id), read through `docker_sock`.
    Container(String),
}

pub const DEFAULT_MAX_LINES_PER_MIN: u32 = 300;

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
    /// `mysql_status`: env var holding the DSN (credentials never live in the TOML).
    pub dsn_env: Option<String>,
    /// `dns`: name to resolve (falls back to `name`).
    pub host: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LogFileConfig {
    app: String,
    file: Option<String>,
    container: Option<String>,
    include: Option<String>,
    exclude: Option<String>,
    max_lines_per_min: Option<u32>,
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
    #[serde(default)]
    logs: Vec<LogFileConfig>,
    disk_warn_pct: Option<u64>,
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
        let logs = resolve_logs(&file_cfg.logs)?;

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
        // Services may go down to 10 s: short MySQL/ES stalls hide between 60 s samples.
        let service_interval_secs = env_u64("TINY_LOG_SERVICE_INTERVAL_SECS")
            .or(file_cfg.service_interval_secs)
            .unwrap_or(60)
            .clamp(10, 300);

        let disk_warn_pct = env_u64("TINY_LOG_DISK_WARN_PCT")
            .or(file_cfg.disk_warn_pct)
            .unwrap_or(85)
            .min(100);

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
            logs,
            disk_warn_pct,
        })
    }
}

fn resolve_logs(raw: &[LogFileConfig]) -> Result<Vec<LogSource>> {
    let mut out = Vec::with_capacity(raw.len());
    for (i, l) in raw.iter().enumerate() {
        let app = l.app.trim();
        if app.is_empty() || app.len() > 128 {
            bail!("[[logs]] #{}: app must be 1..128 chars", i + 1);
        }
        let file = l.file.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let container = l.container.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let target = match (file, container) {
            (Some(f), None) => LogTarget::File(PathBuf::from(f)),
            (None, Some(c)) => LogTarget::Container(c.trim_start_matches('/').to_string()),
            _ => bail!("[[logs]] {app}: set exactly one of file or container"),
        };
        if out.iter().any(|s: &LogSource| s.target == target) {
            bail!("[[logs]] {app}: duplicate source {target:?}");
        }
        let filter = LineFilter::new(l.include.as_deref(), l.exclude.as_deref())
            .with_context(|| format!("[[logs]] {app}"))?;
        out.push(LogSource {
            app: app.to_string(),
            target,
            filter,
            max_lines_per_min: l.max_lines_per_min.unwrap_or(DEFAULT_MAX_LINES_PER_MIN),
        });
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shipped example must parse, and its regexes must compile.
    #[test]
    fn example_configs_parse() {
        for (name, raw) in [
            ("agent.example.toml", include_str!("../agent.example.toml")),
            ("bare-metal.example.toml", include_str!("../bare-metal.example.toml")),
            ("docker.example.toml", include_str!("../docker.example.toml")),
            ("systemd/agent.toml.example", include_str!("../systemd/agent.toml.example")),
        ] {
            let cfg: FileConfig =
                toml::from_str(raw).unwrap_or_else(|e| panic!("{name}: {e}"));
            resolve_logs(&cfg.logs).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        }
        let bm: FileConfig = toml::from_str(include_str!("../bare-metal.example.toml")).unwrap();
        let logs = resolve_logs(&bm.logs).unwrap();
        let mysql = &logs[0];
        assert!(mysql.filter.allows("2026-10-09T17:00:00.1Z 0 [ERROR] [MY-1] [Server] x"));
        assert!(mysql.filter.allows("2026-10-09T17:00:00.1Z 9 [Warning] [MY-2] [Server] x"));
        assert!(!mysql.filter.allows("2026-10-09T17:00:00.1Z 0 [System] [MY-3] [Server] ready"));
        let es = &logs[1];
        assert!(es.filter.allows("[2026-10-09T17:00:00,123][WARN ][o.e.c.r.a.DiskThresholdMonitor] [n1] high disk watermark"));
        assert!(!es.filter.allows("[2026-10-09T17:00:00,123][INFO ][o.e.n.Node] [n1] started"));
        let mysql_svc = bm.service.iter().find(|s| s.kind == "mysql_status").unwrap();
        assert_eq!(mysql_svc.dsn_env.as_deref(), Some("TINY_LOG_MYSQL_DSN"));
        assert_eq!(bm.disk_warn_pct, Some(85));
    }

    #[test]
    fn logs_need_exactly_one_target() {
        let bad: FileConfig = toml::from_str(
            "[[logs]]\napp = \"x\"\nfile = \"/a\"\ncontainer = \"b\"\n",
        )
        .unwrap();
        assert!(resolve_logs(&bad.logs).is_err());
        let dup: FileConfig = toml::from_str(
            "[[logs]]\napp = \"x\"\nfile = \"/a\"\n[[logs]]\napp = \"y\"\nfile = \"/a\"\n",
        )
        .unwrap();
        assert!(resolve_logs(&dup.logs).is_err());
        let ok: FileConfig =
            toml::from_str("[[logs]]\napp = \"x\"\ncontainer = \"/api\"\n").unwrap();
        let l = resolve_logs(&ok.logs).unwrap();
        assert_eq!(l[0].target, LogTarget::Container("api".into()));
        assert_eq!(l[0].max_lines_per_min, DEFAULT_MAX_LINES_PER_MIN);
    }
}
