//! SQLite session index: a rebuildable cache over the JSONL logs.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use hk_llm::Usage;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::Result;
use crate::event::{EventKind, ParentRef, SessionEvent};

/// Per-session facts shown in lists. Pure fold of the log via [`Self::apply`].
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    /// Session id.
    pub id: String,
    /// Current title.
    pub title: Option<String>,
    /// Latest model id (from `session/start`, then `request/header`).
    pub model: Option<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Fork origin.
    pub parent: Option<ParentRef>,
    /// Timestamp of the first event.
    pub created_ms: i64,
    /// Timestamp of the latest event.
    pub updated_ms: i64,
    /// Events in the log.
    pub event_count: u64,
    /// Turns started.
    pub turn_count: u64,
    /// Highest `seq` seen.
    pub last_seq: u64,
    /// Summed step usage.
    pub usage: Usage,
}

impl SessionSummary {
    /// Empty summary for `id`, before any event is applied.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: None,
            model: None,
            cwd: None,
            parent: None,
            created_ms: 0,
            updated_ms: 0,
            event_count: 0,
            turn_count: 0,
            last_seq: 0,
            usage: Usage::default(),
        }
    }

    /// Fold one event in.
    pub fn apply(&mut self, ev: &SessionEvent) {
        if self.event_count == 0 {
            self.created_ms = ev.ts_ms;
        }
        self.event_count += 1;
        self.updated_ms = ev.ts_ms;
        self.last_seq = ev.seq;
        match &ev.kind {
            EventKind::SessionStart {
                cwd,
                model,
                title,
                parent,
                ..
            } => {
                self.cwd = cwd.clone();
                self.model = model.clone();
                self.title = title.clone();
                self.parent = parent.clone();
            }
            EventKind::SessionTitle { title } => self.title = Some(title.clone()),
            EventKind::TurnStart { .. } => self.turn_count += 1,
            EventKind::RequestHeader(h) => self.model = Some(h.model.clone()),
            EventKind::StepEnd { usage: Some(u), .. } => self.usage.accumulate(u),
            _ => {}
        }
    }
}

/// Handle to the index database. Cheap to clone; serialises access internally.
#[derive(Debug, Clone)]
pub struct SessionIndex {
    conn: Arc<Mutex<Connection>>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    title TEXT,
    model TEXT,
    cwd TEXT,
    parent_id TEXT,
    parent_seq INTEGER,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    event_count INTEGER NOT NULL,
    turn_count INTEGER NOT NULL,
    last_seq INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    reasoning_tokens INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_updated ON sessions(updated_ms DESC);
";

impl SessionIndex {
    /// Open (creating if needed) the index at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // WAL: index writes happen at turn end and must not block readers (UI lists).
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    /// In-memory index, for tests and throwaway stores.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock only means another thread panicked mid-statement;
        // sqlite state is still consistent, so keep going.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Insert or replace the row for `s.id`.
    pub fn upsert(&self, s: &SessionSummary) -> Result<()> {
        let u = &s.usage;
        self.lock().execute(
            "INSERT OR REPLACE INTO sessions (id, title, model, cwd, parent_id, parent_seq,
                created_ms, updated_ms, event_count, turn_count, last_seq,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                s.id,
                s.title,
                s.model,
                s.cwd,
                s.parent.as_ref().map(|p| p.session_id.as_str()),
                s.parent.as_ref().map(|p| p.seq as i64),
                s.created_ms,
                s.updated_ms,
                s.event_count as i64,
                s.turn_count as i64,
                s.last_seq as i64,
                u.input_tokens as i64,
                u.output_tokens as i64,
                u.cache_read_tokens as i64,
                u.cache_write_tokens as i64,
                u.reasoning_tokens as i64,
            ],
        )?;
        Ok(())
    }

    /// Row for `id`, if indexed.
    pub fn get(&self, id: &str) -> Result<Option<SessionSummary>> {
        let conn = self.lock();
        let row = conn
            .query_row("SELECT * FROM sessions WHERE id = ?1", [id], from_row)
            .optional()?;
        Ok(row)
    }

    /// Most recently updated sessions first.
    pub fn list(&self, limit: u32, offset: u32) -> Result<Vec<SessionSummary>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT * FROM sessions ORDER BY updated_ms DESC, id LIMIT ?1 OFFSET ?2")?;
        let rows = stmt
            .query_map([limit, offset], from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Drop the row for `id`.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.lock()
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        Ok(())
    }
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<SessionSummary> {
    let parent_id: Option<String> = r.get("parent_id")?;
    let parent_seq: Option<i64> = r.get("parent_seq")?;
    let n = |c: &str| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(c)? as u64) };
    Ok(SessionSummary {
        id: r.get("id")?,
        title: r.get("title")?,
        model: r.get("model")?,
        cwd: r.get("cwd")?,
        parent: parent_id.map(|session_id| ParentRef {
            session_id,
            seq: parent_seq.unwrap_or(0) as u64,
        }),
        created_ms: r.get("created_ms")?,
        updated_ms: r.get("updated_ms")?,
        event_count: n("event_count")?,
        turn_count: n("turn_count")?,
        last_seq: n("last_seq")?,
        usage: Usage {
            input_tokens: n("input_tokens")?,
            output_tokens: n("output_tokens")?,
            cache_read_tokens: n("cache_read_tokens")?,
            cache_write_tokens: n("cache_write_tokens")?,
            reasoning_tokens: n("reasoning_tokens")?,
        },
    })
}
