use crate::config::{AgentConfig, DiskMount};
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct DiskSample {
    pub name: String,
    pub path: String,
    pub used_bytes: Option<i64>,
    pub total_bytes: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SystemSample {
    pub cpu_pct: Option<f64>,
    pub mem_used_bytes: Option<i64>,
    pub mem_total_bytes: Option<i64>,
    /// Primary / fullest disk (backward-compatible single-disk fields).
    pub disk_used_bytes: Option<i64>,
    pub disk_total_bytes: Option<i64>,
    pub disks: Vec<DiskSample>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub n_cpus: Option<i64>,
}

pub async fn sample(cfg: &AgentConfig) -> Result<SystemSample> {
    let (mem_used, mem_total) = memory_bytes().unwrap_or((None, None));
    let disks = sample_disks(&cfg.disks);
    let (disk_used, disk_total) = primary_disk(&disks);
    let (load1, load5, load15) = loadavg();
    Ok(SystemSample {
        cpu_pct: cpu_pct().await,
        mem_used_bytes: mem_used,
        mem_total_bytes: mem_total,
        disk_used_bytes: disk_used,
        disk_total_bytes: disk_total,
        disks,
        load1,
        load5,
        load15,
        n_cpus: n_cpus(),
    })
}

fn sample_disks(mounts: &[DiskMount]) -> Vec<DiskSample> {
    mounts
        .iter()
        .map(|m| {
            let (used, total) = disk_bytes(&m.path).unwrap_or((None, None));
            DiskSample {
                name: m.name.clone(),
                path: m.path.display().to_string(),
                used_bytes: used,
                total_bytes: total,
            }
        })
        .collect()
}

/// Prefer the fullest mount for host-level disk_pct / recommend.
fn primary_disk(disks: &[DiskSample]) -> (Option<i64>, Option<i64>) {
    let mut best: Option<&DiskSample> = None;
    let mut best_pct = -1.0_f64;
    for d in disks {
        let pct = match (d.used_bytes, d.total_bytes) {
            (Some(u), Some(t)) if t > 0 => (u as f64 / t as f64) * 100.0,
            _ => continue,
        };
        if pct >= best_pct {
            best_pct = pct;
            best = Some(d);
        }
    }
    match best.or_else(|| disks.first()) {
        Some(d) => (d.used_bytes, d.total_bytes),
        None => (None, None),
    }
}

fn loadavg() -> (Option<f64>, Option<f64>, Option<f64>) {
    #[cfg(target_os = "linux")]
    {
        let Ok(raw) = std::fs::read_to_string("/proc/loadavg") else {
            return (None, None, None);
        };
        let mut parts = raw.split_whitespace();
        let load1 = parts.next().and_then(|s| s.parse().ok());
        let load5 = parts.next().and_then(|s| s.parse().ok());
        let load15 = parts.next().and_then(|s| s.parse().ok());
        (load1, load5, load15)
    }
    #[cfg(not(target_os = "linux"))]
    {
        (None, None, None)
    }
}

fn n_cpus() -> Option<i64> {
    #[cfg(target_os = "linux")]
    {
        let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        if n > 0 {
            Some(n as i64)
        } else {
            None
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn memory_bytes() -> Result<(Option<i64>, Option<i64>)> {
    #[cfg(target_os = "linux")]
    {
        let raw = std::fs::read_to_string("/proc/meminfo")?;
        let mut total_kb = None;
        let mut avail_kb = None;
        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                total_kb = parse_kb(rest);
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                avail_kb = parse_kb(rest);
            }
        }
        let total = total_kb.map(|k| k.saturating_mul(1024));
        let used = match (total_kb, avail_kb) {
            (Some(t), Some(a)) => Some(t.saturating_sub(a).saturating_mul(1024)),
            _ => None,
        };
        Ok((used, total))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok((None, None))
    }
}

#[cfg(target_os = "linux")]
fn parse_kb(raw: &str) -> Option<i64> {
    raw.split_whitespace().next()?.parse().ok()
}

fn disk_bytes(path: &Path) -> Result<(Option<i64>, Option<i64>)> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        let c_path = CString::new(path.to_string_lossy().as_bytes())?;
        unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
                return Ok((None, None));
            }
            let total = (stat.f_blocks as i64).saturating_mul(stat.f_frsize as i64);
            let free = (stat.f_bavail as i64).saturating_mul(stat.f_frsize as i64);
            let used = total.saturating_sub(free);
            Ok((Some(used), Some(total)))
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Ok((None, None))
    }
}

async fn cpu_pct() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let a = read_cpu_times()?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let b = read_cpu_times()?;
        let idle = b.0.saturating_sub(a.0) as f64;
        let total = b.1.saturating_sub(a.1) as f64;
        if total <= 0.0 {
            return None;
        }
        Some(((total - idle) / total) * 100.0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn read_cpu_times() -> Option<(u64, u64)> {
    let raw = std::fs::read_to_string("/proc/stat").ok()?;
    let line = raw.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }
    let nums: Vec<u64> = parts.filter_map(|p| p.parse().ok()).collect();
    if nums.len() < 4 {
        return None;
    }
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    let total: u64 = nums.iter().sum();
    Some((idle, total))
}
