use crate::config::AgentConfig;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct SystemSample {
    pub cpu_pct: Option<f64>,
    pub mem_used_bytes: Option<i64>,
    pub mem_total_bytes: Option<i64>,
    pub disk_used_bytes: Option<i64>,
    pub disk_total_bytes: Option<i64>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub n_cpus: Option<i64>,
}

pub async fn sample(cfg: &AgentConfig) -> Result<SystemSample> {
    let (mem_used, mem_total) = memory_bytes().unwrap_or((None, None));
    let (disk_used, disk_total) = disk_bytes(&cfg.disk_path).unwrap_or((None, None));
    let (load1, load5, load15) = loadavg();
    Ok(SystemSample {
        cpu_pct: cpu_pct().await,
        mem_used_bytes: mem_used,
        mem_total_bytes: mem_total,
        disk_used_bytes: disk_used,
        disk_total_bytes: disk_total,
        load1,
        load5,
        load15,
        n_cpus: n_cpus(),
    })
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
        let mut available_kb = None;
        for line in raw.lines() {
            if let Some(v) = line.strip_prefix("MemTotal:") {
                total_kb = parse_kb(v);
            } else if let Some(v) = line.strip_prefix("MemAvailable:") {
                available_kb = parse_kb(v);
            }
        }
        let total = total_kb.map(|v| v * 1024);
        let used = match (total_kb, available_kb) {
            (Some(t), Some(a)) => Some((t.saturating_sub(a)) * 1024),
            _ => None,
        };
        Ok((used, total))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = Path::new("/");
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
