//! hk-session: append-only session event log, SQLite index, message derivation.
//!
//! The JSONL log is the source of truth. The model's history is always
//! [`derive_messages`] over that log, the SQLite index is a rebuildable cache
//! for listing, and UI state comes from incremental [`Projection`] folds.
// port: dsh packages/core/session (event vocabulary, derive-from-log, fork at turn boundary)

mod derive;
mod error;
mod event;
mod index;
mod log;
mod projection;
mod store;

pub use derive::derive_messages;
pub use error::{Result, SessionError};
pub use event::{EventKind, ParentRef, RequestHeader, SessionEvent, TurnEndReason};
pub use index::{SessionIndex, SessionSummary};
pub use log::{SessionWriter, read_log};
pub use projection::{Projection, UsageMeter, fold};
pub use store::{ForkPoint, NewSession, SessionStore, new_session_id};
