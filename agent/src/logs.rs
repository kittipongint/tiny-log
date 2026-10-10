//! `[[logs]]` sources: tail files / docker containers, filter, rate-limit, detect the
//! level, and hand lines to the shipper queue (ship.rs).

use crate::config::{AgentConfig, LogSource, LogTarget, DEFAULT_MAX_LINES_PER_MIN};
use crate::ship::LogQueue;
use anyhow::{Context, Result};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Longer lines are cut (server max is 64 KiB; long stack dumps aren't worth the queue).
pub const MAX_LINE_BYTES: usize = 8 * 1024;
/// A "line" with no newline after this many bytes is shipped as is.
pub const MAX_PARTIAL_BYTES: usize = 64 * 1024;

/// One row for `POST /api/v1/logs/batch` (server `NewLog`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LogLine {
    pub app: String,
    pub level: &'static str,
    pub message: String,
    pub timestamp: Option<String>,
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl LogLine {
    /// A line the agent itself writes (disk warnings, queue drops).
    pub fn agent(host: &str, level: &'static str, message: String, extra: Value) -> Self {
        let mut meta = Map::new();
        meta.insert("host".into(), json!(host));
        meta.insert("log_origin".into(), json!("agent"));
        if let Value::Object(m) = extra {
            meta.extend(m);
        }
        Self {
            app: "agent".into(),
            level,
            message,
            timestamp: Some(now_rfc3339()),
            source: Some(host.to_string()),
            meta: Some(Value::Object(meta)),
        }
    }

    /// Bytes this line costs in the queue (message dominates).
    pub fn weight(&self) -> usize {
        self.message.len() + self.app.len() + 128
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

#[derive(Debug, Clone, Default)]
pub struct LineFilter {
    include: Option<Regex>,
    exclude: Option<Regex>,
}

impl LineFilter {
    pub fn new(include: Option<&str>, exclude: Option<&str>) -> Result<Self> {
        let compile = |raw: Option<&str>, what: &str| -> Result<Option<Regex>> {
            match raw.map(str::trim).filter(|s| !s.is_empty()) {
                Some(p) => Ok(Some(
                    Regex::new(p).with_context(|| format!("invalid {what} regex"))?,
                )),
                None => Ok(None),
            }
        };
        Ok(Self {
            include: compile(include, "include")?,
            exclude: compile(exclude, "exclude")?,
        })
    }

    pub fn allows(&self, line: &str) -> bool {
        if let Some(re) = &self.include {
            if !re.is_match(line) {
                return false;
            }
        }
        !self.exclude.as_ref().is_some_and(|re| re.is_match(line))
    }
}

/// Fixed one-minute window per source. Dropped lines are reported once the window closes.
#[derive(Debug)]
pub struct RateLimiter {
    max_per_min: u32,
    window_start: Instant,
    count: u32,
    dropped: u64,
}

impl RateLimiter {
    pub fn new(max_per_min: u32, now: Instant) -> Self {
        Self {
            max_per_min,
            window_start: now,
            count: 0,
            dropped: 0,
        }
    }

    /// Start a new window if a minute has passed; returns lines dropped in the closed one.
    pub fn roll(&mut self, now: Instant) -> Option<u64> {
        if now.duration_since(self.window_start) < Duration::from_secs(60) {
            return None;
        }
        self.window_start = now;
        self.count = 0;
        let dropped = std::mem::take(&mut self.dropped);
        (dropped > 0).then_some(dropped)
    }

    pub fn admit(&mut self) -> bool {
        if self.max_per_min == 0 || self.count < self.max_per_min {
            self.count += 1;
            true
        } else {
            self.dropped += 1;
            false
        }
    }
}

/// Splits a byte stream into lines, keeping the unfinished tail for the next chunk.
#[derive(Debug, Default)]
pub struct LineSplitter {
    partial: Vec<u8>,
}

impl LineSplitter {
    pub fn feed(&mut self, data: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = data;
        while let Some(pos) = rest.iter().position(|b| *b == b'\n') {
            self.partial.extend_from_slice(&rest[..pos]);
            push_line(&mut out, &self.partial);
            self.partial.clear();
            rest = &rest[pos + 1..];
        }
        self.partial.extend_from_slice(rest);
        if self.partial.len() >= MAX_PARTIAL_BYTES {
            push_line(&mut out, &self.partial);
            self.partial.clear();
        }
        out
    }

    /// Ship whatever is buffered (the file was rotated or the stream ended).
    pub fn flush(&mut self) -> Option<String> {
        let mut out = Vec::new();
        push_line(&mut out, &self.partial);
        self.partial.clear();
        out.pop()
    }

    pub fn clear(&mut self) {
        self.partial.clear();
    }
}

fn push_line(out: &mut Vec<String>, raw: &[u8]) {
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    let line = String::from_utf8_lossy(raw);
    if !line.trim().is_empty() {
        out.push(line.into_owned());
    }
}

pub fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line.to_string();
    }
    let mut cut = MAX_LINE_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…[truncated {} bytes]", &line[..cut], line.len() - cut)
}

/// Leading RFC 3339 token (MySQL 8 error log, docker `timestamps=1`) → (timestamp, rest).
pub fn split_leading_timestamp(line: &str) -> Option<(String, &str)> {
    let (token, rest) = line.split_once(' ').unwrap_or((line, ""));
    // Cheap pre-check before chrono: "2026-10-09T…"
    let b = token.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[10] != b'T' {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(token).ok()?;
    Some((token.to_string(), rest))
}

/// error | warn | info. Explicit markers (JSON / logfmt level, `[Warning]`) win over
/// words in the text, so `level=info msg="0 errors"` stays info.
pub fn detect_level(line: &str) -> &'static str {
    let lower = line.to_ascii_lowercase();
    if let Some(level) = json_level(&lower)
        .or_else(|| logfmt_level(&lower))
        .or_else(|| bracket_level(&lower))
    {
        return level;
    }
    word_level(&lower)
}

fn map_level_word(word: &str) -> Option<&'static str> {
    let w = word.trim();
    if w.is_empty() {
        return None;
    }
    if let Ok(n) = w.parse::<u32>() {
        // pino / bunyan numeric levels
        return Some(match n {
            50.. => "error",
            40..=49 => "warn",
            _ => "info",
        });
    }
    const ERR: &[&str] = &[
        "err", "error", "fatal", "panic", "crit", "critical", "emerg", "emergency", "alert",
        "severe",
    ];
    const WARN: &[&str] = &["warn", "warning"];
    const INFO: &[&str] = &[
        "info", "information", "note", "notice", "system", "debug", "trace", "verbose",
    ];
    if ERR.contains(&w) {
        Some("error")
    } else if WARN.contains(&w) {
        Some("warn")
    } else if INFO.contains(&w) {
        Some("info")
    } else {
        None
    }
}

/// `"level":"error"`, `"log.level": "WARN"` (ES 8), `"severity":"ERROR"`, `"level":50`.
fn json_level(lower: &str) -> Option<&'static str> {
    if !lower.contains('{') {
        return None;
    }
    for key in ["\"level\"", "\"log.level\"", "\"severity\"", "\"lvl\""] {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(key) {
            let after = &lower[from + pos + key.len()..];
            from += pos + key.len();
            let Some(v) = after.trim_start().strip_prefix(':') else {
                continue;
            };
            let v = v.trim_start();
            let value = if let Some(q) = v.strip_prefix('"') {
                q.split('"').next().unwrap_or("")
            } else {
                v.split(|c: char| !c.is_ascii_digit()).next().unwrap_or("")
            };
            if let Some(level) = map_level_word(value) {
                return Some(level);
            }
        }
    }
    None
}

/// Go logfmt / slog / logrus: `level=error`, `lvl=warn`, `level="info"`.
fn logfmt_level(lower: &str) -> Option<&'static str> {
    for key in ["level=", "lvl="] {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(key) {
            let at = from + pos;
            from = at + key.len();
            if at > 0 && !lower.as_bytes()[at - 1].is_ascii_whitespace() {
                continue;
            }
            let v = lower[from..].trim_start_matches('"');
            let value = v
                .split(|c: char| c.is_whitespace() || c == '"')
                .next()
                .unwrap_or("");
            if let Some(level) = map_level_word(value) {
                return Some(level);
            }
        }
    }
    None
}

/// `[Warning]` (MySQL 8), `[ERROR]`, `[WARN ]` (ES 7), `[error]` (nginx) — first 256 bytes.
fn bracket_level(lower: &str) -> Option<&'static str> {
    let mut end = lower.len().min(256);
    while !lower.is_char_boundary(end) {
        end -= 1;
    }
    let mut rest = &lower[..end];
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            break;
        };
        if let Some(level) = map_level_word(&after[..close]) {
            return Some(level);
        }
        rest = &after[close + 1..];
    }
    None
}

/// Plain words: `panic: …`, `ERROR 1040`, redis `# Warning: …`. `_` joins words, so
/// `Connection_errors_max_connections` is not an error.
fn word_level(lower: &str) -> &'static str {
    let mut warn = false;
    for word in lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        match word {
            "error" | "errors" | "err" | "fatal" | "panic" | "panicked" | "critical"
            | "crit" | "emerg" | "severe" | "exception" => return "error",
            "warn" | "warning" | "warnings" => warn = true,
            _ => {}
        }
    }
    if warn {
        "warn"
    } else {
        "info"
    }
}

/// Per-source state between the reader and the queue.
pub struct SourceSink {
    app: String,
    host: String,
    filter: LineFilter,
    limiter: RateLimiter,
    meta: Map<String, Value>,
    queue: Arc<LogQueue>,
}

impl SourceSink {
    pub fn new(
        host: &str,
        app: &str,
        filter: LineFilter,
        max_lines_per_min: u32,
        meta: Value,
        queue: Arc<LogQueue>,
    ) -> Self {
        let mut base = Map::new();
        base.insert("host".into(), json!(host));
        base.insert("log_origin".into(), json!("agent"));
        if let Value::Object(m) = meta {
            base.extend(m);
        }
        Self {
            app: app.to_string(),
            host: host.to_string(),
            filter,
            limiter: RateLimiter::new(max_lines_per_min, Instant::now()),
            meta: base,
            queue,
        }
    }

    /// A raw line from the source. `ts` is the reader's timestamp (docker), else parsed
    /// from the line or now. `extra` lands in meta (docker `stream`).
    pub fn line(&mut self, raw: &str, ts: Option<String>, extra: Option<(&str, &str)>) {
        self.tick();
        if !self.filter.allows(raw) {
            return;
        }
        if !self.limiter.admit() {
            return;
        }
        let timestamp = ts
            .or_else(|| split_leading_timestamp(raw).map(|(t, _)| t))
            .unwrap_or_else(now_rfc3339);
        let mut meta = self.meta.clone();
        if let Some((k, v)) = extra {
            meta.insert(k.into(), json!(v));
        }
        self.queue.push(LogLine {
            app: self.app.clone(),
            level: detect_level(raw),
            message: truncate_line(raw),
            timestamp: Some(timestamp),
            source: Some(self.host.clone()),
            meta: Some(Value::Object(meta)),
        });
    }

    /// Agent-written line for this source (bypasses filter and rate limit).
    pub fn notice(&mut self, level: &'static str, message: String) {
        self.queue.push(LogLine {
            app: self.app.clone(),
            level,
            message,
            timestamp: Some(now_rfc3339()),
            source: Some(self.host.clone()),
            meta: Some(Value::Object(self.meta.clone())),
        });
    }

    /// Close the rate window when due and report what it dropped. Call ~1/s.
    pub fn tick(&mut self) {
        if let Some(dropped) = self.limiter.roll(Instant::now()) {
            let max = self.limiter.max_per_min;
            self.notice(
                "warn",
                format!("dropped {dropped} lines (over {max} lines/min)"),
            );
        }
    }
}

/// Start one task per `[[logs]]` source plus docker label discovery. Returns at once.
pub fn spawn_all(cfg: &Arc<AgentConfig>, queue: &Arc<LogQueue>) {
    let started_unix = unix_now();
    let configured: HashSet<String> = cfg
        .logs
        .iter()
        .filter_map(|s| match &s.target {
            LogTarget::Container(c) => Some(c.clone()),
            LogTarget::File(_) => None,
        })
        .collect();

    for src in cfg.logs.clone() {
        spawn_source(cfg, queue, src, started_unix, false, None);
    }

    if cfg.docker_enabled {
        let cfg = cfg.clone();
        let queue = queue.clone();
        let tailed = Arc::new(Mutex::new(configured));
        tokio::spawn(async move {
            discover_loop(cfg, queue, tailed, started_unix).await;
        });
    }
}

fn spawn_source(
    cfg: &Arc<AgentConfig>,
    queue: &Arc<LogQueue>,
    src: LogSource,
    since_unix: f64,
    discovered: bool,
    tailed: Option<Arc<Mutex<HashSet<String>>>>,
) {
    let host = cfg.host_name.clone();
    let sock = cfg.docker_sock.clone();
    let queue = queue.clone();
    info!(app = %src.app, target = ?src.target, discovered, "log_source_started");
    tokio::spawn(async move {
        match src.target.clone() {
            LogTarget::File(path) => {
                let meta = json!({ "source": "file", "path": path.display().to_string() });
                let sink = SourceSink::new(
                    &host,
                    &src.app,
                    src.filter,
                    src.max_lines_per_min,
                    meta,
                    queue,
                );
                crate::tail::run_file(path, sink).await;
            }
            LogTarget::Container(name) => {
                let meta = json!({ "source": "docker", "container": name });
                let sink = SourceSink::new(
                    &host,
                    &src.app,
                    src.filter,
                    src.max_lines_per_min,
                    meta,
                    queue,
                );
                crate::docker::run_container_logs(sock, name.clone(), sink, since_unix, discovered)
                    .await;
                if let Some(set) = tailed {
                    if let Ok(mut set) = set.lock() {
                        set.remove(&name);
                    }
                }
            }
        }
    });
}

/// Containers labelled `tiny-log.logs=<app>` (optional `tiny-log.logs.include`,
/// `tiny-log.logs.exclude`, `tiny-log.logs.max_lines_per_min`).
async fn discover_loop(
    cfg: Arc<AgentConfig>,
    queue: Arc<LogQueue>,
    tailed: Arc<Mutex<HashSet<String>>>,
    since_unix: f64,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    let mut bad: HashSet<String> = HashSet::new();
    loop {
        ticker.tick().await;
        if !cfg.docker_sock.exists() {
            continue;
        }
        let found = match crate::docker::list_log_labeled(&cfg.docker_sock).await {
            Ok(f) => f,
            Err(err) => {
                warn!(error = %err, "docker_log_discover_failed");
                continue;
            }
        };
        for c in found {
            let is_new = tailed.lock().map(|s| !s.contains(&c.name)).unwrap_or(false);
            if !is_new || bad.contains(&c.name) {
                continue;
            }
            let filter = match LineFilter::new(c.include.as_deref(), c.exclude.as_deref()) {
                Ok(f) => f,
                Err(err) => {
                    warn!(container = %c.name, error = format!("{err:#}"), "docker_log_label_invalid");
                    bad.insert(c.name);
                    continue;
                }
            };
            if let Ok(mut set) = tailed.lock() {
                set.insert(c.name.clone());
            }
            let src = LogSource {
                app: c.app,
                target: LogTarget::Container(c.name),
                filter,
                max_lines_per_min: c.max_lines_per_min.unwrap_or(DEFAULT_MAX_LINES_PER_MIN),
            };
            spawn_source(&cfg, &queue, src, since_unix, true, Some(tailed.clone()));
        }
    }
}

pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_mysql8_error_log() {
        let w = "2026-10-09T17:00:00.123456Z 123 [Warning] [MY-010055] [Server] IP address '10.0.0.5' could not be resolved";
        assert_eq!(detect_level(w), "warn");
        let e = "2026-10-09T17:00:01.000000Z 0 [ERROR] [MY-012574] [InnoDB] Unable to lock ./ibdata1 error: 11";
        assert_eq!(detect_level(e), "error");
        let s = "2026-10-09T17:00:02.000000Z 0 [System] [MY-010931] [Server] /usr/sbin/mysqld: ready for connections.";
        assert_eq!(detect_level(s), "info");
        // [Note] wins over the word "error" later in the line
        let n = "2026-10-09T17:00:03.000000Z 8 [Note] [MY-010914] [Server] Aborted connection 8 to db: 'x' (Got an error reading communication packets).";
        assert_eq!(detect_level(n), "info");
    }

    #[test]
    fn level_elasticsearch() {
        assert_eq!(
            detect_level("[2026-10-09T17:00:00,123][WARN ][o.e.c.r.a.DiskThresholdMonitor] [node-1] high disk watermark [90%] exceeded"),
            "warn"
        );
        assert_eq!(
            detect_level("[2026-10-09T17:00:00,123][ERROR][o.e.b.ElasticsearchUncaughtExceptionHandler] fatal error in thread"),
            "error"
        );
        assert_eq!(
            detect_level(r#"{"@timestamp":"2026-10-09T17:00:00.123Z", "log.level": "WARN", "message":"GC overhead"}"#),
            "warn"
        );
        assert_eq!(
            detect_level(r#"{"@timestamp":"2026-10-09T17:00:00.123Z", "log.level":"INFO", "message":"no errors found"}"#),
            "info"
        );
    }

    #[test]
    fn level_go_and_json() {
        assert_eq!(detect_level(r#"time=2026-10-09T17:00:00Z level=ERROR msg="search failed" err="i/o timeout""#), "error");
        assert_eq!(detect_level(r#"time=2026-10-09T17:00:00Z level=info msg="0 errors""#), "info");
        assert_eq!(detect_level(r#"level="warn" msg=slow"#), "warn");
        assert_eq!(detect_level(r#"{"level":"error","msg":"db down"}"#), "error");
        assert_eq!(detect_level(r#"{"level": "warn","msg":"x"}"#), "warn");
        assert_eq!(detect_level(r#"{"level":50,"msg":"pino"}"#), "error");
        assert_eq!(detect_level(r#"{"level":30,"msg":"pino error word"}"#), "info");
        assert_eq!(detect_level("panic: runtime error: invalid memory address"), "error");
        assert_eq!(detect_level("2026/10/09 17:00:00 [error] 12#12: *1 upstream timed out"), "error");
        assert_eq!(detect_level("1:M 09 Oct 2026 17:00:00.000 # WARNING overcommit_memory is set to 0"), "warn");
        assert_eq!(detect_level("GET /health 200 3ms"), "info");
        assert_eq!(detect_level("Connection_errors_max_connections 0"), "info");
        assert_eq!(detect_level("ERROR 1040 (08004): Too many connections"), "error");
        // level= inside another key is not a level marker
        assert_eq!(detect_level("loglevel=debugging ok"), "info");
    }

    #[test]
    fn filter_include_exclude() {
        let f = LineFilter::new(
            Some("(?i)error|warn|timeout|too many|abort|deadlock"),
            Some("statcount flushed"),
        )
        .unwrap();
        assert!(f.allows("[ERROR] boom"));
        assert!(f.allows("Too Many connections"));
        assert!(!f.allows("ready for connections"));
        assert!(!f.allows("[Warning] statcount flushed"));
        let all = LineFilter::new(None, Some("")).unwrap();
        assert!(all.allows("anything"));
        assert!(LineFilter::new(Some("(unclosed"), None).is_err());
    }

    #[test]
    fn rate_limiter_drops_and_reports_once() {
        let t0 = Instant::now();
        let mut rl = RateLimiter::new(3, t0);
        assert!(rl.admit() && rl.admit() && rl.admit());
        assert!(!rl.admit());
        assert!(!rl.admit());
        assert_eq!(rl.roll(t0 + Duration::from_secs(30)), None);
        assert_eq!(rl.roll(t0 + Duration::from_secs(60)), Some(2));
        assert!(rl.admit());
        // nothing dropped in this window → no report
        assert_eq!(rl.roll(t0 + Duration::from_secs(125)), None);
        let mut unlimited = RateLimiter::new(0, t0);
        assert!((0..10_000).all(|_| unlimited.admit()));
    }

    #[test]
    fn splitter_keeps_partial_lines() {
        let mut s = LineSplitter::default();
        assert_eq!(s.feed(b"one\ntw"), vec!["one"]);
        assert_eq!(s.feed(b"o\r\n\n  \nthree"), vec!["two"]);
        assert_eq!(s.flush().as_deref(), Some("three"));
        assert_eq!(s.flush(), None);
        let big = vec![b'x'; MAX_PARTIAL_BYTES];
        assert_eq!(s.feed(&big).len(), 1);
        // invalid utf-8 survives lossily
        assert_eq!(s.feed(b"a\xffb\n"), vec!["a\u{fffd}b"]);
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let s = "ก".repeat(MAX_LINE_BYTES); // 3 bytes each
        let t = truncate_line(&s);
        assert!(t.len() < MAX_LINE_BYTES + 64);
        assert!(t.contains("[truncated"));
        assert_eq!(truncate_line("short"), "short");
    }

    #[test]
    fn leading_timestamp() {
        let (ts, rest) =
            split_leading_timestamp("2026-10-09T17:00:00.123456789Z hello world").unwrap();
        assert_eq!(ts, "2026-10-09T17:00:00.123456789Z");
        assert_eq!(rest, "hello world");
        assert!(split_leading_timestamp("hello 2026-10-09T17:00:00Z").is_none());
        assert!(split_leading_timestamp("2026-10-09 17:00:00 nope").is_none());
    }

    #[test]
    fn sink_filters_limits_and_tags() {
        let q = Arc::new(LogQueue::new(100, 1 << 20));
        let mut sink = SourceSink::new(
            "hfc",
            "mysql",
            LineFilter::new(Some("(?i)error|warn"), None).unwrap(),
            2,
            json!({"source": "file", "path": "/var/log/mysql/error.log"}),
            q.clone(),
        );
        sink.line("2026-10-09T17:00:00.000001Z 1 [Warning] [MY-1] [Server] a", None, None);
        sink.line("ready", None, None);
        sink.line("[ERROR] b", None, Some(("stream", "stderr")));
        sink.line("[ERROR] c", None, None); // over the 2/min limit
        let (lines, dropped) = q.take(10);
        assert_eq!(dropped, 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].level, "warn");
        assert_eq!(lines[0].timestamp.as_deref(), Some("2026-10-09T17:00:00.000001Z"));
        assert_eq!(lines[0].source.as_deref(), Some("hfc"));
        let meta = lines[1].meta.as_ref().unwrap();
        assert_eq!(meta["host"], "hfc");
        assert_eq!(meta["source"], "file");
        assert_eq!(meta["log_origin"], "agent");
        assert_eq!(meta["stream"], "stderr");
        assert_eq!(lines[1].level, "error");
    }
}
