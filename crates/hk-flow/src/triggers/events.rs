//! Native triggers: in-app events and (desktop, feature `notify`) file watching.
//!
//! Types `hokkabaz.appEventTrigger` and `hokkabaz.fileWatchTrigger` are Hokkabaz
//! extensions; they are ordinary workflow nodes, so such workflows still export as n8n
//! JSON (an n8n instance shows them as unknown nodes).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::error::FlowError;
use crate::item::Item;
use crate::workflow::Node;

pub const APP_EVENT_TYPE: &str = "hokkabaz.appEventTrigger";
pub const FILE_WATCH_TYPE: &str = "hokkabaz.fileWatchTrigger";

/// An event published inside the app (`domain/action` names, e.g. `session/finished`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppEvent {
    pub name: String,
    #[serde(default)]
    pub payload: Value,
}

/// Fan-out bus the embedder publishes app events on; app-event triggers subscribe.
#[derive(Debug, Clone)]
pub struct AppEventBus {
    tx: broadcast::Sender<AppEvent>,
}

impl Default for AppEventBus {
    fn default() -> Self {
        Self::new(256)
    }
}

impl AppEventBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            tx: broadcast::channel(capacity.max(1)).0,
        }
    }

    /// Returns how many triggers received it. Slow subscribers lose the oldest events.
    pub fn publish(&self, event: AppEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEventSpec {
    /// Exact name, or a prefix ending in `*` (`session/*`); `*` alone matches everything.
    pub pattern: String,
}

impl AppEventSpec {
    pub fn from_node(node: &Node) -> Result<Self, FlowError> {
        let pattern = node
            .parameters
            .get("event")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                FlowError::Invalid(format!("app event trigger `{}` has no `event`", node.name))
            })?;
        Ok(Self {
            pattern: pattern.to_owned(),
        })
    }

    pub fn matches(&self, name: &str) -> bool {
        match self.pattern.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => self.pattern == name,
        }
    }
}

/// `{ event, payload, timestamp }`
pub fn app_event_item(ev: &AppEvent, now_iso: String) -> Item {
    let mut m = Map::new();
    m.insert("event".into(), json!(ev.name));
    m.insert("payload".into(), ev.payload.clone());
    m.insert("timestamp".into(), json!(now_iso));
    Item::new(m)
}

// ---------------------------------------------------------------- file watch

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWatchSpec {
    pub path: String,
    pub recursive: bool,
    /// Subset of `create`, `modify`, `delete`. Empty = all.
    pub events: Vec<String>,
}

impl FileWatchSpec {
    pub fn from_node(node: &Node) -> Result<Self, FlowError> {
        let p = &node.parameters;
        let path = p
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                FlowError::Invalid(format!("file watch trigger `{}` has no `path`", node.name))
            })?;
        let events = match p.get("events") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Some(Value::String(s)) => s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self {
            path: path.to_owned(),
            recursive: p.get("recursive").and_then(Value::as_bool).unwrap_or(false),
            events,
        })
    }

    pub fn wants(&self, kind: &str) -> bool {
        self.events.is_empty() || self.events.iter().any(|e| e == kind)
    }
}

/// `{ event, path, timestamp }`
pub fn file_event_item(kind: &str, path: &str, now_iso: String) -> Item {
    let mut m = Map::new();
    m.insert("event".into(), json!(kind));
    m.insert("path".into(), json!(path));
    m.insert("timestamp".into(), json!(now_iso));
    Item::new(m)
}

#[cfg(all(feature = "notify", not(target_os = "android")))]
pub(crate) mod watch {
    use std::path::Path;

    use notify::{EventKind, RecursiveMode, Watcher};
    use tokio::sync::mpsc;

    use super::FileWatchSpec;
    use crate::error::FlowError;

    /// Keeps the OS watcher alive; dropping it stops the watch.
    #[derive(Debug)]
    pub(crate) struct FileWatcher {
        _watcher: notify::RecommendedWatcher,
    }

    /// `(kind, path)` pairs the spec asked for, from a notify thread into async land.
    pub(crate) fn start(
        spec: &FileWatchSpec,
    ) -> Result<(FileWatcher, mpsc::UnboundedReceiver<(String, String)>), FlowError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let spec_c = spec.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            let kind = match ev.kind {
                EventKind::Create(_) => "create",
                EventKind::Modify(_) => "modify",
                EventKind::Remove(_) => "delete",
                _ => return,
            };
            if !spec_c.wants(kind) {
                return;
            }
            for p in ev.paths {
                // receiver gone = trigger deactivated
                let _ = tx.send((kind.to_owned(), p.to_string_lossy().into_owned()));
            }
        })
        .map_err(|e| FlowError::Invalid(format!("file watch: {e}")))?;
        let mode = if spec.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        watcher
            .watch(Path::new(&spec.path), mode)
            .map_err(|e| FlowError::Invalid(format!("file watch `{}`: {e}", spec.path)))?;
        Ok((FileWatcher { _watcher: watcher }, rx))
    }
}
