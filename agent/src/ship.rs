//! Bounded log queue + the shipper task that posts it to `POST /api/v1/logs/batch`.
//!
//! The queue drops the *oldest* lines when full and counts them; the shipper reports
//! that count as one `agent` line in the next batch. The shipper holds one batch at a
//! time and retries it (network / 5xx / 429) until the server answers, so memory stays
//! bounded by the queue plus one batch.

use crate::config::AgentConfig;
use crate::logs::LogLine;
use serde::Serialize;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{info, warn};

/// Flush when this many lines are queued…
pub const FLUSH_LINES: usize = 200;
/// …or this often, whichever comes first.
pub const FLUSH_EVERY: Duration = Duration::from_secs(2);
pub const QUEUE_MAX_LINES: usize = 10_000;
/// The compose example caps the agent at 32 MiB; keep the queue well under that.
pub const QUEUE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Server default (`TINY_LOG_MAX_BODY_MB=1`); lowered further if a 413 says so.
const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
/// After 401/403/404 the server will not take logs until someone fixes the config.
const FATAL_PAUSE: Duration = Duration::from_secs(60);

pub struct LogQueue {
    inner: Mutex<QueueInner>,
    notify: Notify,
    max_lines: usize,
    max_bytes: usize,
}

#[derive(Default)]
struct QueueInner {
    lines: VecDeque<LogLine>,
    bytes: usize,
    dropped: u64,
}

impl LogQueue {
    pub fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(QueueInner::default()),
            notify: Notify::new(),
            max_lines: max_lines.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueueInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append; drops the oldest lines while over either bound (the newest always stays).
    pub fn push(&self, line: LogLine) {
        let len = {
            let mut q = self.lock();
            q.bytes += line.weight();
            q.lines.push_back(line);
            while q.lines.len() > 1 && (q.lines.len() > self.max_lines || q.bytes > self.max_bytes)
            {
                if let Some(old) = q.lines.pop_front() {
                    q.bytes = q.bytes.saturating_sub(old.weight());
                    q.dropped += 1;
                }
            }
            q.lines.len()
        };
        if len >= FLUSH_LINES {
            self.notify.notify_one();
        }
    }

    /// Up to `max` oldest lines, plus the overflow count since the last take (reset).
    pub fn take(&self, max: usize) -> (Vec<LogLine>, u64) {
        let mut q = self.lock();
        let n = max.min(q.lines.len());
        let out: Vec<LogLine> = q.lines.drain(..n).collect();
        let freed: usize = out.iter().map(LogLine::weight).sum();
        q.bytes = q.bytes.saturating_sub(freed);
        (out, std::mem::take(&mut q.dropped))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().lines.len()
    }

    /// Until `FLUSH_LINES` are queued or `max` has passed.
    pub async fn wait(&self, max: Duration) {
        let _ = tokio::time::timeout(max, self.notify.notified()).await;
    }
}

/// What the server (or the network) said about one POST.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Status(u16, String),
    Network(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Verdict {
    Sent,
    /// Network error, 5xx, 408, 429: same batch again after a backoff.
    Retry(String),
    /// 413: split. Limits come from the server's JSON when it is ours (a proxy's 413 has none).
    TooLarge {
        max_batch: Option<usize>,
        max_body_bytes: Option<usize>,
    },
    /// 400 with `rejected:[{index,error}]`: drop those, resend the rest once.
    Rejected(Vec<usize>, String),
    /// Any other 4xx: we can't tell which line is bad, so halve.
    Halve(String),
    /// 401 / 403 / 404: nothing in the batch can fix it.
    Fatal(String),
}

fn classify(reply: &Reply) -> Verdict {
    let (status, body) = match reply {
        Reply::Network(err) => return Verdict::Retry(err.clone()),
        Reply::Status(s, b) => (*s, b.as_str()),
    };
    let short = || format!("{status} {}", body.chars().take(200).collect::<String>());
    match status {
        200..=299 => Verdict::Sent,
        401 | 403 | 404 => Verdict::Fatal(short()),
        408 | 429 | 500..=599 => Verdict::Retry(short()),
        413 => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let num = |k: &str| {
                v.get(k)
                    .and_then(|n| n.as_u64())
                    .map(|n| n as usize)
                    .filter(|n| *n > 0)
            };
            Verdict::TooLarge {
                max_batch: num("max_batch"),
                max_body_bytes: num("max_body_bytes"),
            }
        }
        400 => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            match v.get("rejected").and_then(|r| r.as_array()) {
                Some(list) if !list.is_empty() => {
                    let idx = list
                        .iter()
                        .filter_map(|r| r.get("index").and_then(|i| i.as_u64()))
                        .map(|i| i as usize)
                        .collect();
                    let first = list
                        .first()
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string();
                    Verdict::Rejected(idx, first)
                }
                _ => Verdict::Halve(short()),
            }
        }
        _ => Verdict::Halve(short()),
    }
}

/// Split `sizes` (encoded bytes per line) into consecutive ranges that fit both limits.
/// A single line over `max_body` still gets its own range (the server decides).
fn chunk_ranges(sizes: &[usize], max_lines: usize, max_body: usize) -> Vec<std::ops::Range<usize>> {
    // `{"logs":[` + `]}` and a comma per line.
    const ENVELOPE: usize = 11;
    let max_lines = max_lines.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    let mut bytes = ENVELOPE;
    for (i, size) in sizes.iter().enumerate() {
        let n = i - start;
        if n > 0 && (n >= max_lines || bytes + size + 1 > max_body) {
            out.push(start..i);
            start = i;
            bytes = ENVELOPE;
        }
        bytes += size + 1;
    }
    if start < sizes.len() {
        out.push(start..sizes.len());
    }
    out
}

#[derive(Serialize)]
struct BatchBody<'a> {
    logs: &'a [LogLine],
}

fn encode(lines: &[LogLine]) -> Vec<u8> {
    serde_json::to_vec(&BatchBody { logs: lines }).unwrap_or_default()
}

/// The one network call, behind a trait so the batch logic is testable without HTTP.
pub(crate) trait Transport {
    fn post(&self, body: Vec<u8>) -> impl std::future::Future<Output = Reply>;
}

struct HttpTransport {
    url: String,
    api_key: String,
    http: reqwest::Client,
}

impl HttpTransport {
    fn new(cfg: &AgentConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(1)
            .build()
            .expect("http client");
        Self {
            url: format!("{}/api/v1/logs/batch", cfg.url),
            api_key: cfg.api_key.clone(),
            http,
        }
    }
}

impl Transport for HttpTransport {
    async fn post(&self, body: Vec<u8>) -> Reply {
        let res = self
            .http
            .post(&self.url)
            .bearer_auth(&self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        match res {
            Ok(res) => {
                let status = res.status().as_u16();
                let text = res.text().await.unwrap_or_default();
                Reply::Status(status, text)
            }
            Err(err) => Reply::Network(format!("{err:#}")),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ShipStats {
    pub sent: usize,
    /// Lines the server refused (invalid / too large / auth) and we gave up on.
    pub discarded: usize,
}

pub(crate) struct Shipper<T> {
    transport: T,
    max_batch: usize,
    max_body: usize,
    backoff_base: Duration,
    backoff_max: Duration,
    /// Set by a fatal reply; the run loop sleeps `FATAL_PAUSE` before the next batch.
    paused: bool,
}

impl<T: Transport> Shipper<T> {
    fn new(transport: T) -> Self {
        Self {
            transport,
            max_batch: FLUSH_LINES,
            max_body: DEFAULT_MAX_BODY_BYTES,
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            paused: false,
        }
    }

    fn chunk(&self, lines: Vec<LogLine>) -> Vec<Vec<LogLine>> {
        let sizes: Vec<usize> = lines
            .iter()
            .map(|l| serde_json::to_vec(l).map(|v| v.len()).unwrap_or(0))
            .collect();
        let ranges = chunk_ranges(&sizes, self.max_batch, self.max_body);
        let mut rest = lines;
        let mut out = Vec::with_capacity(ranges.len());
        for r in ranges.iter().rev() {
            out.push(rest.split_off(r.start));
        }
        out.reverse();
        out
    }

    /// POST until the server gives an answer other than "try again".
    async fn post_until_answered(&self, batch: &[LogLine]) -> Verdict {
        let body = encode(batch);
        let mut attempt: u32 = 0;
        loop {
            let verdict = classify(&self.transport.post(body.clone()).await);
            let Verdict::Retry(err) = verdict else {
                if attempt >= 3 {
                    info!(attempts = attempt + 1, "log_ship_recovered");
                }
                return verdict;
            };
            if attempt == 0 || attempt % 10 == 9 {
                warn!(error = %err, lines = batch.len(), attempt = attempt + 1, "log_ship_retry");
            }
            let backoff = self
                .backoff_base
                .saturating_mul(2u32.saturating_pow(attempt.min(16)))
                .min(self.backoff_max);
            tokio::time::sleep(backoff).await;
            attempt = attempt.saturating_add(1);
        }
    }

    /// Ship `lines`, splitting / dropping as the server asks. Returns once every line
    /// was either stored or given up on.
    async fn ship(&mut self, lines: Vec<LogLine>) -> ShipStats {
        let mut stats = ShipStats::default();
        // Stack of (batch, already resent after `rejected`); pop() takes the oldest first.
        let mut work: Vec<(Vec<LogLine>, bool)> =
            self.chunk(lines).into_iter().rev().map(|b| (b, false)).collect();

        while let Some((batch, resent)) = work.pop() {
            if batch.is_empty() {
                continue;
            }
            let n = batch.len();
            match self.post_until_answered(&batch).await {
                Verdict::Sent => stats.sent += n,
                Verdict::Retry(_) => unreachable!("post_until_answered retries"),
                Verdict::Fatal(err) => {
                    warn!(error = %err, lines = n, "log_ship_refused");
                    stats.discarded += n;
                    self.paused = true;
                }
                Verdict::TooLarge {
                    max_batch,
                    max_body_bytes,
                } => {
                    if let Some(m) = max_batch {
                        self.max_batch = self.max_batch.min(m);
                    }
                    if let Some(m) = max_body_bytes {
                        self.max_body = m;
                    }
                    if n == 1 {
                        warn!(app = %batch[0].app, "log_line_too_large_dropped");
                        stats.discarded += 1;
                        continue;
                    }
                    let mut parts = self.chunk(batch);
                    if parts.len() == 1 {
                        // Our limits say it fits (e.g. a proxy's smaller limit): halve.
                        let mut a = parts.pop().unwrap_or_default();
                        let b = a.split_off(a.len() / 2);
                        parts = vec![a, b];
                    }
                    for p in parts.into_iter().rev() {
                        work.push((p, resent));
                    }
                }
                Verdict::Rejected(idx, first_error) => {
                    let bad: std::collections::HashSet<usize> =
                        idx.into_iter().filter(|i| *i < n).collect();
                    if resent || bad.is_empty() {
                        // Second refusal (or indexes that don't match): halve instead of
                        // looping; a single line is dropped.
                        if n == 1 || resent {
                            warn!(error = %first_error, lines = n, "log_batch_rejected_dropped");
                            stats.discarded += n;
                        } else {
                            let mut a = batch;
                            let b = a.split_off(n / 2);
                            work.push((b, false));
                            work.push((a, false));
                        }
                        continue;
                    }
                    warn!(error = %first_error, dropped = bad.len(), "log_lines_rejected");
                    stats.discarded += bad.len();
                    let rest: Vec<LogLine> = batch
                        .into_iter()
                        .enumerate()
                        .filter(|(i, _)| !bad.contains(i))
                        .map(|(_, l)| l)
                        .collect();
                    work.push((rest, true));
                }
                Verdict::Halve(err) => {
                    if n == 1 {
                        warn!(error = %err, app = %batch[0].app, "log_line_refused_dropped");
                        stats.discarded += 1;
                    } else {
                        let mut a = batch;
                        let b = a.split_off(n / 2);
                        work.push((b, resent));
                        work.push((a, resent));
                    }
                }
            }
        }
        stats
    }
}

/// The shipper task. Never returns.
pub async fn run(cfg: Arc<AgentConfig>, queue: Arc<LogQueue>) {
    let mut shipper = Shipper::new(HttpTransport::new(&cfg));
    drain_forever(&mut shipper, &queue, &cfg.host_name, FLUSH_EVERY).await;
}

async fn drain_forever<T: Transport>(
    shipper: &mut Shipper<T>,
    queue: &LogQueue,
    host: &str,
    every: Duration,
) {
    loop {
        queue.wait(every).await;
        loop {
            let take = shipper.max_batch.min(FLUSH_LINES);
            let (mut lines, dropped) = queue.take(take);
            if dropped > 0 {
                warn!(dropped, "log_queue_overflow");
                lines.insert(
                    0,
                    LogLine::agent(
                        host,
                        "warn",
                        format!("log queue full: dropped {dropped} oldest lines"),
                        json!({ "dropped": dropped }),
                    ),
                );
            }
            if lines.is_empty() {
                break;
            }
            let full = lines.len() >= take;
            shipper.ship(lines).await;
            if shipper.paused {
                shipper.paused = false;
                tokio::time::sleep(FATAL_PAUSE).await;
            }
            if !full {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn line(i: usize) -> LogLine {
        LogLine {
            app: "app".into(),
            level: "info",
            message: format!("line {i}"),
            timestamp: None,
            source: None,
            meta: None,
        }
    }

    fn messages(body: &[u8]) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_slice(body).unwrap();
        v["logs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["message"].as_str().unwrap().to_string())
            .collect()
    }

    /// Scripted server: answers `replies` in order (then 200), records every body.
    struct Fake {
        replies: RefCell<VecDeque<Reply>>,
        seen: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn new(replies: Vec<Reply>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl Transport for &Fake {
        async fn post(&self, body: Vec<u8>) -> Reply {
            self.seen.borrow_mut().push(messages(&body));
            self.replies
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Reply::Status(200, r#"{"success":true}"#.into()))
        }
    }

    fn shipper(fake: &Fake) -> Shipper<&Fake> {
        let mut s = Shipper::new(fake);
        s.backoff_base = Duration::from_millis(1);
        s.backoff_max = Duration::from_millis(2);
        s
    }

    fn lines(n: usize) -> Vec<LogLine> {
        (0..n).map(line).collect()
    }

    #[test]
    fn queue_drops_oldest_and_counts() {
        let q = LogQueue::new(3, 1 << 20);
        for i in 0..5 {
            q.push(line(i));
        }
        assert_eq!(q.len(), 3);
        let (got, dropped) = q.take(10);
        assert_eq!(dropped, 2);
        assert_eq!(got[0].message, "line 2");
        let (_, dropped) = q.take(10);
        assert_eq!(dropped, 0, "drop count resets on take");

        // byte bound: each line weighs ~140; 300 bytes keeps two
        let q = LogQueue::new(100, 300);
        for i in 0..4 {
            q.push(line(i));
        }
        let (got, dropped) = q.take(1);
        assert_eq!((got[0].message.as_str(), dropped), ("line 2", 2));
        assert_eq!(q.len(), 1);
        // the newest line always stays even if it alone is over the bound
        let q = LogQueue::new(10, 1);
        q.push(line(0));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn classify_replies() {
        assert_eq!(classify(&Reply::Status(200, "{}".into())), Verdict::Sent);
        assert!(matches!(classify(&Reply::Network("refused".into())), Verdict::Retry(_)));
        assert!(matches!(classify(&Reply::Status(502, "".into())), Verdict::Retry(_)));
        assert!(matches!(classify(&Reply::Status(429, "".into())), Verdict::Retry(_)));
        assert!(matches!(classify(&Reply::Status(401, "".into())), Verdict::Fatal(_)));
        assert_eq!(
            classify(&Reply::Status(
                413,
                r#"{"error":"payload too large","max_batch":500,"max_body_bytes":1048576}"#.into()
            )),
            Verdict::TooLarge {
                max_batch: Some(500),
                max_body_bytes: Some(1_048_576)
            }
        );
        // nginx's HTML 413
        assert_eq!(
            classify(&Reply::Status(413, "<html>413</html>".into())),
            Verdict::TooLarge {
                max_batch: None,
                max_body_bytes: None
            }
        );
        assert_eq!(
            classify(&Reply::Status(
                400,
                r#"{"error":"invalid level: x","rejected":[{"index":1,"error":"invalid level: x"},{"index":3,"error":"y"}]}"#.into()
            )),
            Verdict::Rejected(vec![1, 3], "invalid level: x".into())
        );
        assert!(matches!(
            classify(&Reply::Status(400, r#"{"error":"logs array is empty"}"#.into())),
            Verdict::Halve(_)
        ));
        assert!(matches!(classify(&Reply::Status(422, "bad".into())), Verdict::Halve(_)));
    }

    #[test]
    fn chunking_respects_lines_and_bytes() {
        assert_eq!(chunk_ranges(&[10; 5], 2, 1 << 20), vec![0..2, 2..4, 4..5]);
        // 11 envelope + 3 × (30 + 1) = 104 > 100 → split after two
        assert_eq!(chunk_ranges(&[30; 4], 100, 100), vec![0..2, 2..4]);
        // an oversized single line still gets its own chunk
        assert_eq!(chunk_ranges(&[10, 500, 10], 100, 100), vec![0..1, 1..2, 2..3]);
        assert!(chunk_ranges(&[], 10, 100).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retries_network_and_5xx_then_sends() {
        let fake = Fake::new(vec![
            Reply::Network("connection refused".into()),
            Reply::Status(503, "".into()),
        ]);
        let mut s = shipper(&fake);
        let stats = s.ship(lines(3)).await;
        assert_eq!(stats, ShipStats { sent: 3, discarded: 0 });
        assert_eq!(fake.seen.borrow().len(), 3);
        assert!(fake.seen.borrow().iter().all(|b| b.len() == 3));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn splits_on_413_using_server_limits() {
        let fake = Fake::new(vec![Reply::Status(
            413,
            r#"{"error":"payload too large","max_batch":2,"max_body_bytes":1048576}"#.into(),
        )]);
        let mut s = shipper(&fake);
        let stats = s.ship(lines(5)).await;
        assert_eq!(stats.sent, 5);
        let seen = fake.seen.borrow();
        let sizes: Vec<usize> = seen.iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![5, 2, 2, 1]);
        assert_eq!(seen[1], vec!["line 0", "line 1"], "order kept");
        assert_eq!(s.max_batch, 2, "limit remembered for later batches");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn proxy_413_without_limits_halves_and_drops_single() {
        let html = || Reply::Status(413, "<html>too large</html>".into());
        let fake = Fake::new(vec![html(), html(), html()]);
        let mut s = shipper(&fake);
        let stats = s.ship(lines(4)).await;
        // 4 → [0,1] refused → [0] refused (dropped) → [1] ok → [2,3] ok
        assert_eq!(stats, ShipStats { sent: 3, discarded: 1 });
        let sizes: Vec<usize> = fake.seen.borrow().iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![4, 2, 1, 1, 2]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejected_indexes_dropped_and_rest_resent_once() {
        let fake = Fake::new(vec![Reply::Status(
            400,
            r#"{"error":"invalid level","rejected":[{"index":1,"error":"invalid level"},{"index":9,"error":"x"}]}"#.into(),
        )]);
        let mut s = shipper(&fake);
        let stats = s.ship(lines(3)).await;
        assert_eq!(stats, ShipStats { sent: 2, discarded: 1 });
        assert_eq!(fake.seen.borrow()[1], vec!["line 0", "line 2"]);

        // refused again after the resend → give up on that batch, no loop
        let rej = || {
            Reply::Status(
                400,
                r#"{"error":"e","rejected":[{"index":0,"error":"e"}]}"#.into(),
            )
        };
        let fake = Fake::new(vec![rej(), rej()]);
        let mut s = shipper(&fake);
        let stats = s.ship(lines(3)).await;
        assert_eq!(stats, ShipStats { sent: 0, discarded: 3 });
        assert_eq!(fake.seen.borrow().len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plain_400_halves_until_the_bad_line() {
        let bad = || Reply::Status(400, r#"{"error":"bad"}"#.into());
        // 4 → halves [0,1] bad → [0] bad (drop) → [1] ok → [2,3] ok
        let fake = Fake::new(vec![bad(), bad(), bad()]);
        let mut s = shipper(&fake);
        let stats = s.ship(lines(4)).await;
        assert_eq!(stats, ShipStats { sent: 3, discarded: 1 });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fatal_drops_and_pauses() {
        let fake = Fake::new(vec![Reply::Status(401, r#"{"error":"unauthorized"}"#.into())]);
        let mut s = shipper(&fake);
        let stats = s.ship(lines(2)).await;
        assert_eq!(stats, ShipStats { sent: 0, discarded: 2 });
        assert!(s.paused);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drain_reports_overflow_as_one_agent_line() {
        let q = LogQueue::new(2, 1 << 20);
        for i in 0..5 {
            q.push(line(i));
        }
        let fake = Fake::new(vec![]);
        let mut s = shipper(&fake);
        // one drain pass: run the loop body until the queue is empty
        let _ = tokio::time::timeout(
            Duration::from_millis(50),
            drain_forever(&mut s, &q, "hfc", Duration::from_millis(1)),
        )
        .await;
        let seen = fake.seen.borrow();
        assert_eq!(
            seen[0],
            vec!["log queue full: dropped 3 oldest lines", "line 3", "line 4"]
        );
    }
}
