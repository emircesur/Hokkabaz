//! Session store: directory layout, create/open/fork, index maintenance.
//!
//! Layout: `<root>/index.sqlite`, `<root>/<session-id>/events.jsonl`.

use std::path::{Path, PathBuf};

use crate::error::{Result, SessionError};
use crate::event::{EventKind, ParentRef, SessionEvent};
use crate::index::{SessionIndex, SessionSummary};
use crate::log::{SessionWriter, io_err, read_log};

const LOG_FILE: &str = "events.jsonl";
const INDEX_FILE: &str = "index.sqlite";

/// Fresh time-ordered session id.
pub fn new_session_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

/// Metadata for a new session.
#[derive(Debug, Clone, Default)]
pub struct NewSession {
    /// Explicit id; generated when `None`.
    pub id: Option<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Initial model.
    pub model: Option<String>,
    /// Initial title.
    pub title: Option<String>,
}

/// Where to cut a source session when forking. Only closed turns qualify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkPoint {
    /// Keep everything through this turn's `turn/end`.
    AfterTurn(String),
    /// Keep everything before this turn's `turn/start` (edit-and-resubmit).
    BeforeTurn(String),
}

/// Handle to a sessions directory plus its index.
#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
    index: SessionIndex,
}

impl SessionStore {
    /// Open the store at `root`, creating directory and index if needed.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(io_err(&root))?;
        let index = SessionIndex::open(&root.join(INDEX_FILE))?;
        Ok(Self { root, index })
    }

    /// The listing index.
    pub fn index(&self) -> &SessionIndex {
        &self.index
    }

    /// Store root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, id: &str) -> Result<PathBuf> {
        // Ids become directory names; reject anything that could escape root.
        let ok = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ok {
            return Err(SessionError::InvalidId(id.to_owned()));
        }
        Ok(self.root.join(id))
    }

    fn log_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.dir(id)?.join(LOG_FILE))
    }

    /// Create a session and write its `session/start`.
    pub fn create(&self, new: NewSession) -> Result<SessionWriter> {
        self.create_with_parent(new, None)
    }

    fn create_with_parent(
        &self,
        new: NewSession,
        parent: Option<ParentRef>,
    ) -> Result<SessionWriter> {
        let id = new.id.unwrap_or_else(new_session_id);
        let dir = self.dir(&id)?;
        if dir.exists() {
            return Err(SessionError::AlreadyExists(id));
        }
        std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let mut w = SessionWriter::open(dir.join(LOG_FILE), &id, Some(self.index.clone()))?;
        w.append(EventKind::SessionStart {
            session_id: id,
            cwd: new.cwd,
            model: new.model,
            title: new.title,
            parent,
        })?;
        w.sync()?;
        Ok(w)
    }

    /// Reopen an existing session for appending.
    pub fn open_writer(&self, id: &str) -> Result<SessionWriter> {
        let path = self.log_path(id)?;
        if !path.exists() {
            return Err(SessionError::NotFound(id.to_owned()));
        }
        SessionWriter::open(path, id, Some(self.index.clone()))
    }

    /// Read a session's full log.
    pub fn read(&self, id: &str) -> Result<Vec<SessionEvent>> {
        let path = self.log_path(id)?;
        if !path.exists() {
            return Err(SessionError::NotFound(id.to_owned()));
        }
        read_log(&path)
    }

    /// New session seeded with the source prefix up to `point`.
    ///
    /// The copy keeps original `seq`/`ts_ms` so `through_seq` references in
    /// compaction events stay valid; the fork's own `session/start` (seq 0)
    /// records the parent. `overrides` fills metadata; unset title/model/cwd
    /// inherit from the source.
    pub fn fork(
        &self,
        source_id: &str,
        point: &ForkPoint,
        overrides: NewSession,
    ) -> Result<SessionWriter> {
        let events = self.read(source_id)?;
        let cut = cut_index(&events, point)?;
        let prefix: Vec<&SessionEvent> = events[..cut]
            .iter()
            .filter(|e| {
                !matches!(
                    e.kind,
                    EventKind::SessionStart { .. } | EventKind::SessionTitle { .. }
                )
            })
            .collect();

        let mut src = SessionSummary::new(source_id);
        events[..cut].iter().for_each(|e| src.apply(e));
        let parent = ParentRef {
            session_id: source_id.to_owned(),
            seq: prefix.last().map_or(0, |e| e.seq),
        };
        let meta = NewSession {
            id: overrides.id,
            cwd: overrides.cwd.or(src.cwd),
            model: overrides.model.or(src.model),
            title: overrides.title.or(src.title),
        };

        let mut w = self.create_with_parent(meta, Some(parent))?;
        for ev in prefix {
            w.append_raw(ev)?;
        }
        w.sync()?;
        Ok(w)
    }

    /// Recompute every index row from the logs. Logs that fail to parse are
    /// skipped with a warning so one bad session cannot hide the rest.
    pub fn rebuild_index(&self) -> Result<usize> {
        let mut n = 0;
        for entry in std::fs::read_dir(&self.root).map_err(io_err(&self.root))? {
            let entry = entry.map_err(io_err(&self.root))?;
            let path = entry.path().join(LOG_FILE);
            if !path.is_file() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            match read_log(&path) {
                Ok(events) => {
                    let mut s = SessionSummary::new(id);
                    events.iter().for_each(|e| s.apply(e));
                    self.index.upsert(&s)?;
                    n += 1;
                }
                Err(e) => tracing::warn!(session = %id, error = %e, "skipping unreadable log"),
            }
        }
        Ok(n)
    }

    /// Delete a session's log and index row.
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.dir(id)?;
        if !dir.exists() {
            return Err(SessionError::NotFound(id.to_owned()));
        }
        std::fs::remove_dir_all(&dir).map_err(io_err(&dir))?;
        self.index.remove(id)
    }
}

/// Index one past the last event to keep.
fn cut_index(events: &[SessionEvent], point: &ForkPoint) -> Result<usize> {
    let (ForkPoint::AfterTurn(turn) | ForkPoint::BeforeTurn(turn)) = point;
    let start = events
        .iter()
        .position(|e| matches!(&e.kind, EventKind::TurnStart { turn_id } if turn_id == turn))
        .ok_or_else(|| SessionError::TurnNotFound(turn.clone()))?;
    match point {
        ForkPoint::BeforeTurn(_) => Ok(start),
        ForkPoint::AfterTurn(_) => events[start..]
            .iter()
            .position(|e| matches!(&e.kind, EventKind::TurnEnd { turn_id, .. } if turn_id == turn))
            .map(|i| start + i + 1)
            .ok_or_else(|| SessionError::TurnOpen(turn.clone())),
    }
}
