use crate::config::{AgentConfig, DiskMount};
use crate::logs::LogLine;
use anyhow::Result;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

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

/// Repeat a still-full disk at most this often.
pub const DISK_WARN_REPEAT: Duration = Duration::from_secs(30 * 60);
/// At or above this a disk line is `error` (MySQL stops writing well before 100 %).
pub const DISK_ERROR_PCT: f64 = 95.0;
/// Back below `warn_pct - this` counts as recovered (no flapping around the threshold).
const DISK_RECOVER_MARGIN: f64 = 1.0;

/// Turns system samples into `agent` log lines when a disk crosses `disk_warn_pct`.
pub struct DiskWarner {
    warn_pct: f64,
    state: HashMap<String, DiskState>,
}

struct DiskState {
    last_sent: Instant,
    last_level: &'static str,
}

impl DiskWarner {
    pub fn new(warn_pct: u64) -> Self {
        Self {
            warn_pct: warn_pct as f64,
            state: HashMap::new(),
        }
    }

    pub fn check(&mut self, host: &str, disks: &[DiskSample], now: Instant) -> Vec<LogLine> {
        let mut out = Vec::new();
        for d in disks {
            let pct = match (d.used_bytes, d.total_bytes) {
                (Some(u), Some(t)) if t > 0 => u as f64 / t as f64 * 100.0,
                _ => continue,
            };
            let shown = pct.floor() as i64;
            let extra = json!({
                "disk": d.name,
                "path": d.path,
                "used_pct": (pct * 10.0).round() / 10.0,
                "used_bytes": d.used_bytes,
                "total_bytes": d.total_bytes,
            });
            if pct >= self.warn_pct {
                let level = if pct >= DISK_ERROR_PCT { "error" } else { "warn" };
                let due = match self.state.get(&d.name) {
                    None => true,
                    Some(s) => {
                        now.duration_since(s.last_sent) >= DISK_WARN_REPEAT
                            // escalation to error is news; don't sit on it for 30 min
                            || (level == "error" && s.last_level != "error")
                    }
                };
                if due {
                    out.push(LogLine::agent(
                        host,
                        level,
                        format!("disk {} ({}) {shown}% used on {host}", d.name, d.path),
                        extra,
                    ));
                    self.state.insert(
                        d.name.clone(),
                        DiskState {
                            last_sent: now,
                            last_level: level,
                        },
                    );
                }
            } else if pct < self.warn_pct - DISK_RECOVER_MARGIN
                && self.state.remove(&d.name).is_some()
            {
                out.push(LogLine::agent(
                    host,
                    "info",
                    format!("disk {} ({}) back to {shown}% used on {host}", d.name, d.path),
                    extra,
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(pct: f64) -> Vec<DiskSample> {
        vec![DiskSample {
            name: "root".into(),
            path: "/".into(),
            used_bytes: Some((pct * 10.0) as i64),
            total_bytes: Some(1000),
        }]
    }

    fn msgs(lines: &[LogLine]) -> Vec<(&str, &str)> {
        lines.iter().map(|l| (l.level, l.message.as_str())).collect()
    }

    #[test]
    fn disk_warning_state_machine() {
        let t0 = Instant::now();
        let min = |m: u64| t0 + Duration::from_secs(m * 60);
        let mut w = DiskWarner::new(85);
        assert!(w.check("hfc", &disk(80.0), t0).is_empty());
        let l = w.check("hfc", &disk(91.2), min(1));
        assert_eq!(msgs(&l), vec![("warn", "disk root (/) 91% used on hfc")]);
        assert_eq!(l[0].app, "agent");
        assert_eq!(l[0].meta.as_ref().unwrap()["used_pct"], 91.2);
        // still above: quiet until 30 min passed
        assert!(w.check("hfc", &disk(92.0), min(10)).is_empty());
        assert_eq!(w.check("hfc", &disk(92.0), min(31)).len(), 1);
        // escalation to error is reported at once, then rate-limited again
        let l = w.check("hfc", &disk(96.0), min(33));
        assert_eq!(msgs(&l), vec![("error", "disk root (/) 96% used on hfc")]);
        assert!(w.check("hfc", &disk(97.0), min(40)).is_empty());
        assert!(w.check("hfc", &disk(90.0), min(41)).is_empty(), "de-escalation is quiet");
        // inside the 1-point margin: not recovered yet
        assert!(w.check("hfc", &disk(84.5), min(42)).is_empty());
        let l = w.check("hfc", &disk(70.0), min(43));
        assert_eq!(msgs(&l), vec![("info", "disk root (/) back to 70% used on hfc")]);
        assert!(w.check("hfc", &disk(70.0), min(44)).is_empty());
        // crossing again is a fresh warning
        assert_eq!(w.check("hfc", &disk(86.0), min(45)).len(), 1);
        // unknown sizes are skipped
        let unknown = vec![DiskSample {
            name: "data".into(),
            path: "/data".into(),
            used_bytes: None,
            total_bytes: None,
        }];
        assert!(w.check("hfc", &unknown, min(46)).is_empty());
    }
}
