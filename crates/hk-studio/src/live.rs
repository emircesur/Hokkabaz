//! Live editing session: one project in memory, edits applied through
//! [`EditOp`], every change broadcast so the UI (and the agent) see it at once.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};

use crate::edit::{EditOp, History, apply};
use crate::error::Result;
use crate::pipeline::{PreviewPlan, preview};
use crate::project::{ProjectStore, StudioProject};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StudioEvent {
    /// A clip changed (edit, undo, redo). `rev` increases on every change.
    Changed { rev: u64, clips: Vec<String>, by: String },
    /// Clips added or removed.
    ClipsChanged { rev: u64 },
    Transcribed { rev: u64 },
    RenderProgress { clip: String, fraction: f64 },
    RenderDone { clip: String, path: String },
    RenderFailed { clip: String, error: String },
}

#[derive(Debug)]
struct Inner {
    project: StudioProject,
    history: History,
    rev: u64,
}

/// Cheap to clone; all clones share the session.
#[derive(Debug, Clone)]
pub struct LiveStudio {
    inner: Arc<Mutex<Inner>>,
    store: ProjectStore,
    tx: broadcast::Sender<StudioEvent>,
}

impl LiveStudio {
    pub fn new(store: ProjectStore, project: StudioProject) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { inner: Arc::new(Mutex::new(Inner { project, history: History::default(), rev: 0 })), store, tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StudioEvent> {
        self.tx.subscribe()
    }

    pub fn emit(&self, ev: StudioEvent) {
        let _ = self.tx.send(ev);
    }

    pub async fn snapshot(&self) -> StudioProject {
        self.inner.lock().await.project.clone()
    }

    pub async fn rev(&self) -> u64 {
        self.inner.lock().await.rev
    }

    /// Run a pipeline step that mutates the project (transcribe, analyze), then persist and notify.
    pub async fn with_project<T>(&self, f: impl AsyncFnOnce(&mut StudioProject) -> Result<T>) -> Result<T> {
        let mut g = self.inner.lock().await;
        {
            let Inner { project, history, .. } = &mut *g;
            history.record(project);
        }
        let out = f(&mut g.project).await?;
        g.rev += 1;
        let rev = g.rev;
        self.store.save(&g.project).await?;
        drop(g);
        self.emit(StudioEvent::ClipsChanged { rev });
        Ok(out)
    }

    /// Apply an edit. `by` names the actor ("ui", "agent").
    pub async fn edit(&self, op: EditOp, by: &str) -> Result<u64> {
        let mut g = self.inner.lock().await;
        let mut trial = g.project.clone();
        let touched = apply(&mut trial, &op)?;
        {
            let Inner { project, history, .. } = &mut *g;
            history.record(project);
        }
        g.project = trial;
        g.rev += 1;
        let rev = g.rev;
        self.store.save(&g.project).await?;
        drop(g);
        let structural = matches!(op, EditOp::AddClip { .. } | EditOp::RemoveClip { .. } | EditOp::SplitClip { .. });
        if structural {
            self.emit(StudioEvent::ClipsChanged { rev });
        } else {
            self.emit(StudioEvent::Changed { rev, clips: touched.into_iter().collect(), by: by.to_string() });
        }
        Ok(rev)
    }

    pub async fn undo(&self) -> Result<bool> {
        self.step(true).await
    }

    pub async fn redo(&self) -> Result<bool> {
        self.step(false).await
    }

    async fn step(&self, undo: bool) -> Result<bool> {
        let mut g = self.inner.lock().await;
        let Inner { project, history, rev } = &mut *g;
        let moved = if undo { history.undo(project) } else { history.redo(project) };
        if !moved {
            return Ok(false);
        }
        *rev += 1;
        let r = *rev;
        self.store.save(&g.project).await?;
        drop(g);
        self.emit(StudioEvent::ClipsChanged { rev: r });
        Ok(true)
    }

    pub async fn can_undo(&self) -> bool {
        self.inner.lock().await.history.can_undo()
    }

    pub async fn can_redo(&self) -> bool {
        self.inner.lock().await.history.can_redo()
    }

    /// Player-ready plan of the current state of one clip.
    pub async fn preview(&self, clip: &str) -> Result<PreviewPlan> {
        preview(&self.inner.lock().await.project, clip)
    }
}
