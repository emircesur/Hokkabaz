//! JSONL log: crash-tolerant reader and append-only writer.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Result, SessionError};
use crate::event::{EventKind, SessionEvent};
use crate::index::{SessionIndex, SessionSummary};

pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> SessionError + '_ {
    move |source| SessionError::Io {
        path: path.to_owned(),
        source,
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Result of scanning a log file.
pub(crate) struct Scan {
    pub events: Vec<SessionEvent>,
    /// Byte length of the prefix ending at the last valid event.
    pub good_len: u64,
    /// Last valid event has no trailing newline (crash between write and newline).
    pub missing_newline: bool,
    /// File length on disk.
    pub file_len: u64,
}

/// Parse a log, tolerating one torn (crash-truncated) final line. A bad line
/// anywhere else is corruption and an error.
pub(crate) fn scan(path: &Path) -> Result<Scan> {
    let bytes = std::fs::read(path).map_err(io_err(path))?;
    let file_len = bytes.len() as u64;
    let mut events: Vec<SessionEvent> = Vec::new();
    let mut good_len = 0u64;
    let mut missing_newline = false;
    let mut offset = 0usize;
    let mut line_no = 0usize;

    while offset < bytes.len() {
        line_no += 1;
        let (line, next, terminated) = match bytes[offset..].iter().position(|&b| b == b'\n') {
            Some(i) => (&bytes[offset..offset + i], offset + i + 1, true),
            None => (&bytes[offset..], bytes.len(), false),
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            offset = next;
            continue;
        }
        match serde_json::from_slice::<SessionEvent>(line) {
            Ok(ev) => {
                if let Some(prev) = events.last()
                    && ev.seq <= prev.seq
                {
                    return Err(SessionError::Corrupt {
                        path: path.to_owned(),
                        line: line_no,
                        msg: format!("seq {} does not follow {}", ev.seq, prev.seq),
                    });
                }
                events.push(ev);
                good_len = next as u64;
                missing_newline = !terminated;
            }
            Err(_) if !terminated => {
                tracing::warn!(path = %path.display(), line = line_no, "dropping torn final log line");
                break;
            }
            Err(e) => {
                return Err(SessionError::Corrupt {
                    path: path.to_owned(),
                    line: line_no,
                    msg: e.to_string(),
                });
            }
        }
        offset = next;
    }
    Ok(Scan {
        events,
        good_len,
        missing_newline,
        file_len,
    })
}

/// Read all events from a log file.
pub fn read_log(path: &Path) -> Result<Vec<SessionEvent>> {
    Ok(scan(path)?.events)
}

/// Append-only writer for one session log.
///
/// Every event is one `write_all` of a full line. The log is `fsync`ed and the
/// index row refreshed on `turn/end` (and on [`Self::sync`]); between those
/// points a crash can lose the tail of the open turn, never earlier turns.
#[derive(Debug)]
pub struct SessionWriter {
    path: PathBuf,
    file: File,
    next_seq: u64,
    summary: SessionSummary,
    index: Option<SessionIndex>,
    dirty: bool,
}

impl SessionWriter {
    /// Open `path` for appending, creating it if missing. Repairs a torn tail
    /// left by a crash so the next line starts clean.
    pub(crate) fn open(
        path: PathBuf,
        session_id: &str,
        index: Option<SessionIndex>,
    ) -> Result<Self> {
        let mut summary = SessionSummary::new(session_id);
        let mut next_seq = 0;
        let mut needs_newline = false;

        if path.exists() {
            let s = scan(&path)?;
            for ev in &s.events {
                summary.apply(ev);
            }
            next_seq = s.events.last().map_or(0, |e| e.seq + 1);
            if s.good_len < s.file_len {
                truncate(&path, s.good_len)?;
            }
            needs_newline = s.missing_newline;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io_err(&path))?;
        if needs_newline {
            file.write_all(b"\n").map_err(io_err(&path))?;
        }
        Ok(Self {
            path,
            file,
            next_seq,
            summary,
            index,
            dirty: false,
        })
    }

    /// Session id.
    pub fn session_id(&self) -> &str {
        &self.summary.id
    }

    /// Log path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `seq` the next appended event will get.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Running summary of everything written so far.
    pub fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    /// Append an event stamped now. `turn/end` fsyncs.
    pub fn append(&mut self, kind: EventKind) -> Result<SessionEvent> {
        let ev = SessionEvent {
            seq: self.next_seq,
            ts_ms: now_ms(),
            kind,
        };
        self.append_raw(&ev)?;
        Ok(ev)
    }

    /// Append a pre-built event (fork prefix copy); `seq` must increase.
    pub(crate) fn append_raw(&mut self, ev: &SessionEvent) -> Result<()> {
        if ev.seq < self.next_seq {
            return Err(SessionError::Corrupt {
                path: self.path.clone(),
                line: 0,
                msg: format!("append seq {} < next {}", ev.seq, self.next_seq),
            });
        }
        let mut line = serde_json::to_vec(ev)?;
        line.push(b'\n');
        self.file.write_all(&line).map_err(io_err(&self.path))?;
        self.next_seq = ev.seq + 1;
        self.summary.apply(ev);
        self.dirty = true;
        if matches!(ev.kind, EventKind::TurnEnd { .. }) {
            self.sync()?;
        }
        Ok(())
    }

    /// Flush the log to disk and refresh the index row.
    pub fn sync(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.file.sync_data().map_err(io_err(&self.path))?;
        if let Some(index) = &self.index {
            index.upsert(&self.summary)?;
        }
        self.dirty = false;
        Ok(())
    }
}

fn truncate(path: &Path, len: u64) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.set_len(len))
        .map_err(io_err(path))
}
