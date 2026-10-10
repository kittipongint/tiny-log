//! `[[logs]] file = …` — poll-based tail that follows rotation and truncation.
//!
//! * starts at the end of a file that exists when the agent starts (no backlog flood);
//!   a file that shows up later (was NotFound) is read from the start (it is all new);
//! * rotation (path now points to another inode/dev): drain the old fd to EOF, then
//!   reopen the path from offset 0;
//! * truncation (size < offset, `copytruncate`): back to offset 0;
//! * missing / unreadable file: warn once, retry quietly every poll.

use crate::logs::{LineSplitter, SourceSink};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

const POLL_EVERY: Duration = Duration::from_secs(1);
/// Read at most this much per poll so a burst can't hold the (single) runtime thread.
const MAX_READ_PER_POLL: u64 = 4 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum TailEvent {
    Line(String),
    Opened { from_start: bool },
    Rotated,
    Truncated,
    /// First failure to open (later failures are quiet until it opens again).
    Unavailable(String),
}

pub struct FileTailer {
    path: PathBuf,
    file: Option<File>,
    id: Option<(u64, u64)>,
    offset: u64,
    splitter: LineSplitter,
    /// Nothing opened yet *and* nothing failed yet: the first open seeks to the end.
    pristine: bool,
    warned: bool,
    /// The last poll stopped at `MAX_READ_PER_POLL`; poll again without sleeping.
    pub more: bool,
}

impl FileTailer {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            id: None,
            offset: 0,
            splitter: LineSplitter::default(),
            pristine: true,
            warned: false,
            more: false,
        }
    }

    pub fn poll(&mut self) -> Vec<TailEvent> {
        let mut events = Vec::new();
        self.more = false;

        if self.file.is_none() && !self.open(&mut events) {
            return events;
        }

        // What the path points to *now*, before draining, so nothing written to the old
        // file before the rename is lost.
        let path_id = std::fs::metadata(&self.path).ok().and_then(|m| file_id(&m));

        // Truncation: same inode, shorter than where we are.
        if let Some(f) = self.file.as_ref() {
            if let Ok(meta) = f.metadata() {
                if meta.len() < self.offset {
                    events.push(TailEvent::Truncated);
                    self.offset = 0;
                    self.splitter.clear();
                    if let Some(f) = self.file.as_mut() {
                        let _ = f.seek(SeekFrom::Start(0));
                    }
                }
            }
        }

        self.drain(&mut events);

        let rotated = match (path_id, self.id) {
            (Some(now), Some(open)) => now != open,
            // Path gone (renamed, not recreated yet): keep the old fd until it reappears.
            _ => false,
        };
        if rotated && !self.more {
            if let Some(rest) = self.splitter.flush() {
                events.push(TailEvent::Line(rest));
            }
            events.push(TailEvent::Rotated);
            self.file = None;
            self.id = None;
            self.offset = 0;
            if self.open(&mut events) {
                self.drain(&mut events);
            }
        }
        events
    }

    fn open(&mut self, events: &mut Vec<TailEvent>) -> bool {
        match File::open(&self.path) {
            Ok(mut f) => {
                let meta = f.metadata().ok();
                let from_start = !self.pristine;
                self.offset = if from_start {
                    0
                } else {
                    meta.as_ref().map(|m| m.len()).unwrap_or(0)
                };
                if f.seek(SeekFrom::Start(self.offset)).is_err() {
                    self.offset = 0;
                }
                self.id = meta.as_ref().and_then(file_id);
                self.file = Some(f);
                self.pristine = false;
                self.warned = false;
                self.splitter.clear();
                events.push(TailEvent::Opened { from_start });
                true
            }
            Err(err) => {
                // A file created later is all new; one we couldn't read (permissions)
                // may hold years of backlog, so that one still starts at the end.
                if err.kind() == io::ErrorKind::NotFound {
                    self.pristine = false;
                }
                if !self.warned {
                    self.warned = true;
                    events.push(TailEvent::Unavailable(err.to_string()));
                }
                false
            }
        }
    }

    fn drain(&mut self, events: &mut Vec<TailEvent>) {
        let Some(f) = self.file.as_mut() else {
            return;
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut read_total = 0u64;
        loop {
            if read_total >= MAX_READ_PER_POLL {
                self.more = true;
                break;
            }
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    read_total += n as u64;
                    self.offset += n as u64;
                    for line in self.splitter.feed(&buf[..n]) {
                        events.push(TailEvent::Line(line));
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    warn!(path = %self.path.display(), error = %err, "log_file_read_failed");
                    self.file = None;
                    self.id = None;
                    break;
                }
            }
        }
    }
}

#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Tail `path` forever into `sink`.
pub async fn run_file(path: PathBuf, mut sink: SourceSink) {
    let mut tailer = FileTailer::new(path.clone());
    loop {
        for ev in tailer.poll() {
            match ev {
                TailEvent::Line(l) => sink.line(&l, None, None),
                TailEvent::Opened { from_start } => {
                    info!(path = %path.display(), from_start, "log_file_opened")
                }
                TailEvent::Rotated => info!(path = %path.display(), "log_file_rotated"),
                TailEvent::Truncated => info!(path = %path.display(), "log_file_truncated"),
                TailEvent::Unavailable(err) => warn!(
                    path = %path.display(),
                    error = %err,
                    "log_file_unavailable_retrying"
                ),
            }
        }
        sink.tick();
        if tailer.more {
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(POLL_EVERY).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "tinylog-tail-{tag}-{}-{}",
                std::process::id(),
                crate::logs::unix_now()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn lines(ev: &[TailEvent]) -> Vec<&str> {
        ev.iter()
            .filter_map(|e| match e {
                TailEvent::Line(l) => Some(l.as_str()),
                _ => None,
            })
            .collect()
    }

    fn append(path: &std::path::Path, data: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(data.as_bytes()).unwrap();
    }

    #[test]
    fn starts_at_end_and_keeps_partial_lines() {
        let dir = TempDir::new("end");
        let p = dir.0.join("app.log");
        append(&p, "old 1\nold 2\n");
        let mut t = FileTailer::new(p.clone());
        let ev = t.poll();
        assert_eq!(ev, vec![TailEvent::Opened { from_start: false }]);
        append(&p, "new 1\nnew ");
        assert_eq!(lines(&t.poll()), vec!["new 1"]);
        append(&p, "2\n");
        assert_eq!(lines(&t.poll()), vec!["new 2"]);
        assert!(t.poll().is_empty());
    }

    #[test]
    fn follows_rename_rotation() {
        let dir = TempDir::new("rot");
        let p = dir.0.join("error.log");
        append(&p, "");
        let mut t = FileTailer::new(p.clone());
        t.poll();
        append(&p, "a\n");
        // logrotate: rename, the writer adds one more line to the old fd, new file created
        std::fs::rename(&p, dir.0.join("error.log.1")).unwrap();
        append(&dir.0.join("error.log.1"), "b\npartial");
        // path missing for a moment: keep reading the old fd
        assert_eq!(lines(&t.poll()), vec!["a", "b"]);
        append(&p, "c\n");
        let ev = t.poll();
        assert_eq!(lines(&ev), vec!["partial", "c"]);
        assert!(ev.contains(&TailEvent::Rotated));
        assert!(ev.contains(&TailEvent::Opened { from_start: true }));
        append(&p, "d\n");
        assert_eq!(lines(&t.poll()), vec!["d"]);
    }

    #[test]
    fn detects_copytruncate() {
        let dir = TempDir::new("trunc");
        let p = dir.0.join("app.log");
        append(&p, "x\n");
        let mut t = FileTailer::new(p.clone());
        t.poll();
        append(&p, "before truncate\n");
        assert_eq!(lines(&t.poll()), vec!["before truncate"]);
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&p)
            .unwrap();
        append(&p, "y\n");
        let ev = t.poll();
        assert!(ev.contains(&TailEvent::Truncated));
        assert_eq!(lines(&ev), vec!["y"]);
    }

    #[test]
    fn missing_file_warns_once_then_reads_from_start() {
        let dir = TempDir::new("missing");
        let p = dir.0.join("later.log");
        let mut t = FileTailer::new(p.clone());
        assert!(matches!(t.poll().as_slice(), [TailEvent::Unavailable(_)]));
        assert!(t.poll().is_empty(), "quiet after the first warning");
        append(&p, "first\n");
        let ev = t.poll();
        assert!(ev.contains(&TailEvent::Opened { from_start: true }));
        assert_eq!(lines(&ev), vec!["first"]);
    }
}
