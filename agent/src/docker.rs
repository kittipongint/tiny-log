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
use crate::logs::{LineSplitter, SourceSink};
use serde::Deserialize;
use std::path::{Path, PathBuf};
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
            meta: None,
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
        match container_stats(sock, &c.id).await {
            Ok(stats) => {
                result.cpu_pct = stats.cpu_pct;
                result.mem_used_bytes = stats.mem_used_bytes;
                result.mem_limit_bytes = stats.mem_limit_bytes;
            }
            Err(err) => {
                tracing::warn!(container = %c.id, error = %err, "docker_stats_failed");
            }
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
    // One-shot stats often have empty precpu_stats; sample twice for a real CPU delta.
    let _ = docker_get(sock, &format!("/containers/{id}/stats?stream=false")).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
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
        .filter(|l| *l > 0)
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
    let response = docker_request(sock, path, Duration::from_secs(3)).await?;
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

/// One request on a fresh connection; `head_timeout` covers the response head only, so
/// the body can stream for as long as it likes (`logs?follow=1`).
async fn docker_request(
    sock: &Path,
    path: &str,
    head_timeout: Duration,
) -> Result<http::Response<hyper::body::Incoming>> {
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

    tokio::time::timeout(head_timeout, sender.send_request(req))
        .await
        .context("docker request timeout")?
        .context("docker request")
}

// ---------------------------------------------------------------------------------------
// Container logs (`[[logs]] container = …` and `tiny-log.logs=<app>` label discovery)
// ---------------------------------------------------------------------------------------

/// A running container labelled `tiny-log.logs=<app>`.
#[derive(Debug, Clone, PartialEq)]
pub struct LogLabeled {
    pub name: String,
    pub app: String,
    pub include: Option<String>,
    pub exclude: Option<String>,
    pub max_lines_per_min: Option<u32>,
}

pub async fn list_log_labeled(sock: &Path) -> Result<Vec<LogLabeled>> {
    let containers = list_containers(sock).await?;
    Ok(containers.into_iter().filter_map(log_labeled).collect())
}

fn log_labeled(c: DockerContainer) -> Option<LogLabeled> {
    let labels = c.labels?;
    let app = labels.get("tiny-log.logs")?.trim().to_string();
    if app.is_empty() || app.len() > 128 {
        return None;
    }
    let name = c
        .names
        .as_ref()
        .and_then(|n| n.first())
        .map(|n| n.trim_start_matches('/').to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| c.id.chars().take(12).collect());
    let get = |k: &str| {
        labels
            .get(k)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    Some(LogLabeled {
        name,
        app,
        include: get("tiny-log.logs.include"),
        exclude: get("tiny-log.logs.exclude"),
        max_lines_per_min: get("tiny-log.logs.max_lines_per_min").and_then(|v| v.parse().ok()),
    })
}

#[derive(Debug, Deserialize)]
struct ContainerInspect {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Config")]
    config: Option<InspectConfig>,
    #[serde(rename = "State")]
    state: Option<InspectState>,
}

#[derive(Debug, Deserialize)]
struct InspectConfig {
    #[serde(rename = "Tty", default)]
    tty: bool,
}

#[derive(Debug, Deserialize)]
struct InspectState {
    #[serde(rename = "Running", default)]
    running: bool,
    #[serde(rename = "StartedAt", default)]
    started_at: String,
}

/// `Ok(None)` = no such container (404).
async fn inspect(sock: &Path, name: &str) -> Result<Option<ContainerInspect>> {
    let response =
        docker_request(sock, &format!("/containers/{name}/json"), Duration::from_secs(3)).await?;
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .context("read docker body")?
        .to_bytes();
    if status == http::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        anyhow::bail!("docker inspect {status}: {}", String::from_utf8_lossy(&body));
    }
    serde_json::from_slice(&body)
        .map(Some)
        .context("parse docker inspect")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

impl LogStream {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Docker's multiplexed log stream (non-TTY containers): frames of
/// `[stream, 0, 0, 0, size_be32]` + payload. Frames may span reads.
/// A header that can't be one (bytes 1..4 not zero) switches to raw passthrough.
#[derive(Debug, Default)]
pub struct Demuxer {
    buf: Vec<u8>,
    raw: bool,
}

impl Demuxer {
    pub fn feed(&mut self, data: &[u8]) -> Vec<(LogStream, Vec<u8>)> {
        if self.raw {
            return vec![(LogStream::Stdout, data.to_vec())];
        }
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut pos = 0;
        while self.buf.len() - pos >= 8 {
            let h = &self.buf[pos..pos + 8];
            if h[0] > 3 || h[1] != 0 || h[2] != 0 || h[3] != 0 {
                self.raw = true;
                out.push((LogStream::Stdout, self.buf[pos..].to_vec()));
                pos = self.buf.len();
                break;
            }
            let size = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize;
            if self.buf.len() - pos - 8 < size {
                break;
            }
            // 0 stdin (never sent), 1 stdout, 2 stderr, 3 daemon error in band
            let stream = if h[0] >= 2 {
                LogStream::Stderr
            } else {
                LogStream::Stdout
            };
            out.push((stream, self.buf[pos + 8..pos + 8 + size].to_vec()));
            pos += 8 + size;
        }
        self.buf.drain(..pos);
        out
    }
}

type Ts = chrono::DateTime<chrono::Utc>;

fn unix_to_ts(unix: f64) -> Ts {
    let secs = unix.floor();
    let nanos = ((unix - secs) * 1e9).round().clamp(0.0, 999_999_999.0) as u32;
    chrono::DateTime::from_timestamp(secs as i64, nanos).unwrap_or_else(chrono::Utc::now)
}

/// Docker's `since=` takes `<secs>.<nanos>`.
fn fmt_since(ts: Ts) -> String {
    format!("{}.{:09}", ts.timestamp(), ts.timestamp_subsec_nanos())
}

/// Per-stream state that survives reconnects: where to resume, and what was already shipped.
struct LogCursor {
    /// Newest docker timestamp shipped.
    last_seen: Option<Ts>,
    /// After a reconnect `since` is inclusive: skip lines up to here, once.
    skip_until: Option<Ts>,
}

impl LogCursor {
    /// `Some((ts, rest))` when the line should be shipped.
    fn accept<'a>(&mut self, line: &'a str) -> Option<(Option<String>, &'a str)> {
        let Some((ts_raw, rest)) = crate::logs::split_leading_timestamp(line) else {
            return Some((None, line));
        };
        let ts = chrono::DateTime::parse_from_rfc3339(&ts_raw)
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc));
        if let (Some(ts), Some(skip)) = (ts, self.skip_until) {
            if ts <= skip {
                return None;
            }
            self.skip_until = None;
        }
        if let Some(ts) = ts {
            if self.last_seen.is_none_or(|l| ts > l) {
                self.last_seen = Some(ts);
            }
        }
        Some((Some(ts_raw), rest))
    }
}

fn emit(sink: &mut SourceSink, cursor: &mut LogCursor, stream: Option<LogStream>, line: &str) {
    if let Some((ts, rest)) = cursor.accept(line) {
        // `<ts> ` alone (an empty log line): the server refuses empty messages.
        if !rest.trim().is_empty() {
            sink.line(rest, ts, stream.map(|s| ("stream", s.as_str())));
        }
    }
}

/// Follow one container's logs forever (configured) or until it is removed (discovered).
pub async fn run_container_logs(
    sock: PathBuf,
    name: String,
    mut sink: SourceSink,
    since_unix: f64,
    discovered: bool,
) {
    const BACKOFF_MIN: Duration = Duration::from_secs(1);
    const BACKOFF_MAX: Duration = Duration::from_secs(30);
    let start_from = unix_to_ts(since_unix);
    let mut cursor = LogCursor {
        last_seen: None,
        skip_until: None,
    };
    let mut known: Option<(String, String)> = None;
    let mut backoff = BACKOFF_MIN;
    let mut warned = false;

    loop {
        sink.tick();
        let info = match inspect(&sock, &name).await {
            Ok(Some(info)) => info,
            Ok(None) if discovered => {
                tracing::info!(container = %name, "docker_log_container_gone");
                return;
            }
            Ok(None) => {
                if !warned {
                    tracing::warn!(container = %name, "docker_log_container_not_found_retrying");
                    warned = true;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
            Err(err) => {
                if !warned {
                    tracing::warn!(container = %name, error = format!("{err:#}"), "docker_log_inspect_failed");
                    warned = true;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };
        warned = false;

        let state = info.state.as_ref();
        let running = state.is_some_and(|s| s.running);
        let ident = (
            info.id.clone(),
            state.map(|s| s.started_at.clone()).unwrap_or_default(),
        );
        if known.as_ref().is_some_and(|k| *k != ident) {
            let short: String = info.id.chars().take(12).collect();
            sink.notice("info", format!("container restarted ({name}, id {short})"));
        }
        known = Some(ident);

        let tty = info.config.as_ref().is_some_and(|c| c.tty);
        let from = cursor.last_seen.unwrap_or(start_from);
        cursor.skip_until = cursor.last_seen;
        let path = format!(
            "/containers/{}/logs?follow=1&stdout=1&stderr=1&timestamps=1&since={}",
            info.id,
            fmt_since(from)
        );
        let started = std::time::Instant::now();
        if let Err(err) = stream_logs(&sock, &path, tty, &mut sink, &mut cursor).await {
            tracing::warn!(container = %name, error = format!("{err:#}"), "docker_log_stream_failed");
        }
        // A stream that ran a while was healthy; a stopped container re-checks slowly.
        if running && started.elapsed() >= Duration::from_secs(30) {
            backoff = BACKOFF_MIN;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn stream_logs(
    sock: &Path,
    path: &str,
    tty: bool,
    sink: &mut SourceSink,
    cursor: &mut LogCursor,
) -> Result<()> {
    let response = docker_request(sock, path, Duration::from_secs(10)).await?;
    let status = response.status();
    let mut body = response.into_body();
    if !status.is_success() {
        let text = body.collect().await.map(|b| b.to_bytes()).unwrap_or_default();
        anyhow::bail!("docker logs {status}: {}", String::from_utf8_lossy(&text));
    }
    let mut demux = Demuxer::default();
    let mut out = LineSplitter::default();
    let mut err = LineSplitter::default();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            frame = body.frame() => {
                let Some(frame) = frame else { break };
                let frame = frame.context("read docker log stream")?;
                let Ok(data) = frame.into_data() else { continue };
                if tty {
                    for line in out.feed(&data) {
                        emit(sink, cursor, None, &line);
                    }
                    continue;
                }
                for (stream, payload) in demux.feed(&data) {
                    let splitter = match stream {
                        LogStream::Stdout => &mut out,
                        LogStream::Stderr => &mut err,
                    };
                    for line in splitter.feed(&payload) {
                        emit(sink, cursor, Some(stream), &line);
                    }
                }
            }
            _ = tick.tick() => sink.tick(),
        }
    }
    let tag = |s| if tty { None } else { Some(s) };
    if let Some(line) = out.flush() {
        emit(sink, cursor, tag(LogStream::Stdout), &line);
    }
    if let Some(line) = err.flush() {
        emit(sink, cursor, tag(LogStream::Stderr), &line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::LineFilter;
    use crate::ship::LogQueue;
    use std::sync::Arc;

    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![stream, 0, 0, 0];
        f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn demux_multiplexed_frames_across_reads() {
        let mut data = frame(1, b"2026-10-09T17:00:00.000000001Z out one\n");
        data.extend(frame(2, b"2026-10-09T17:00:00.000000002Z err one\n"));
        data.extend(frame(1, b""));
        let mut d = Demuxer::default();
        // split in the middle of the second header and the payload
        let (a, b) = data.split_at(50);
        let first = d.feed(a);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, LogStream::Stdout);
        let rest = d.feed(b);
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].0, LogStream::Stderr);
        assert_eq!(rest[0].1, b"2026-10-09T17:00:00.000000002Z err one\n");
        assert!(rest[1].1.is_empty());
        // byte by byte
        let mut d = Demuxer::default();
        let n: usize = data.iter().map(|b| d.feed(&[*b]).len()).sum();
        assert_eq!(n, 3);
    }

    #[test]
    fn demux_falls_back_to_raw_on_tty_bytes() {
        let mut d = Demuxer::default();
        let got = d.feed(b"2026-10-09T17:00:00Z hello from a tty\n");
        assert_eq!(got, vec![(LogStream::Stdout, b"2026-10-09T17:00:00Z hello from a tty\n".to_vec())]);
        assert_eq!(d.feed(b"more"), vec![(LogStream::Stdout, b"more".to_vec())]);
    }

    #[test]
    fn since_and_cursor_dedupe_after_reconnect() {
        let t = unix_to_ts(1_790_000_000.5);
        assert_eq!(fmt_since(t), "1790000000.500000000");
        let mut c = LogCursor { last_seen: None, skip_until: None };
        let (ts, rest) = c.accept("2026-10-09T17:00:01.000000000Z a").unwrap();
        assert_eq!((ts.as_deref(), rest), (Some("2026-10-09T17:00:01.000000000Z"), "a"));
        c.accept("2026-10-09T17:00:02.000000000Z b").unwrap();
        // reconnect: since is inclusive, docker re-sends b
        c.skip_until = c.last_seen;
        assert!(c.accept("2026-10-09T17:00:01.500000000Z a2").is_none());
        assert!(c.accept("2026-10-09T17:00:02.000000000Z b").is_none());
        assert!(c.accept("2026-10-09T17:00:03.000000000Z c").is_some());
        // after the first new line, older timestamps (stdout/stderr interleave) pass
        assert!(c.accept("2026-10-09T17:00:02.900000000Z d").is_some());
        // a line without a timestamp passes through untouched
        assert_eq!(c.accept("no ts").unwrap(), (None, "no ts"));
    }

    #[test]
    fn label_discovery_reads_labels() {
        let c: DockerContainer = serde_json::from_str(
            r#"{"Id":"abcdef0123456789","Names":["/searchapi"],"State":"running","Status":"Up",
                "Labels":{"tiny-log.logs":"searchapi","tiny-log.logs.include":"(?i)error|warn",
                          "tiny-log.logs.max_lines_per_min":"120"}}"#,
        )
        .unwrap();
        assert_eq!(
            log_labeled(c),
            Some(LogLabeled {
                name: "searchapi".into(),
                app: "searchapi".into(),
                include: Some("(?i)error|warn".into()),
                exclude: None,
                max_lines_per_min: Some(120),
            })
        );
        let unlabeled: DockerContainer =
            serde_json::from_str(r#"{"Id":"x","Labels":{"tiny-log.monitor":"true"}}"#).unwrap();
        assert_eq!(log_labeled(unlabeled), None);
    }

    #[test]
    fn emit_tags_stream_and_uses_docker_timestamp() {
        let q = Arc::new(LogQueue::new(100, 1 << 20));
        let mut sink = SourceSink::new(
            "vm1",
            "searchapi",
            LineFilter::default(),
            0,
            serde_json::json!({"source": "docker", "container": "searchapi"}),
            q.clone(),
        );
        let mut c = LogCursor { last_seen: None, skip_until: None };
        emit(&mut sink, &mut c, Some(LogStream::Stderr), "2026-10-09T17:00:00.123456789Z level=error msg=boom");
        emit(&mut sink, &mut c, None, "2026-10-09T17:00:01.000000000Z tty line");
        emit(&mut sink, &mut c, None, "2026-10-09T17:00:02.000000000Z   ");
        let (lines, _) = q.take(10);
        assert_eq!(lines.len(), 2, "timestamp-only line skipped");
        assert_eq!(lines[0].message, "level=error msg=boom");
        assert_eq!(lines[0].level, "error");
        assert_eq!(lines[0].timestamp.as_deref(), Some("2026-10-09T17:00:00.123456789Z"));
        assert_eq!(lines[0].meta.as_ref().unwrap()["stream"], "stderr");
        assert!(lines[1].meta.as_ref().unwrap().get("stream").is_none());
    }
}
