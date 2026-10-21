//! Durable-wait persistence seam.
//!
//! A Wait node that cannot be held in-process makes the executor park the execution as a
//! [`WaitRecord`] (see `executor.rs`, "Durable waits"). Anything that can store these
//! survives a restart; the in-memory impl below does not. The SQLite impl belongs next to
//! the other stores (hk-store has no flow tables yet, and hk-flow does not depend on it):
//! one row per record, `execution_id` primary key, `resume_at` indexed, `record` = the
//! serde JSON of [`WaitRecord`].

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::FlowError;
use crate::executor::WaitSnapshot;

/// What ends the wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WaitTrigger {
    /// Timer only (`resume_at` is set).
    Time,
    /// A call to `<base>/webhook-waiting/<execution id>[/<suffix>]` with `http_method`.
    #[serde(rename_all = "camelCase")]
    Webhook {
        http_method: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<String>,
    },
    /// A form submission to the waiting form URL.
    Form,
}

/// What a Wait node asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitRequest {
    /// Unix ms. The resume time for `Time`; the give-up time for webhook/form waits
    /// (`limitWaitTime`); `None` = wait forever.
    #[serde(default)]
    pub resume_at: Option<i64>,
    pub on: WaitTrigger,
}

/// A parked execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitRecord {
    pub execution_id: String,
    #[serde(default)]
    pub workflow_id: Option<String>,
    pub request: WaitRequest,
    pub snapshot: WaitSnapshot,
}

impl WaitRecord {
    pub fn to_json(&self) -> Result<String, FlowError> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self, FlowError> {
        Ok(serde_json::from_str(s)?)
    }
}

#[async_trait]
pub trait WaitStore: Send + Sync + fmt::Debug {
    /// Insert or replace by `execution_id`.
    async fn put(&self, record: &WaitRecord) -> Result<(), FlowError>;
    async fn get(&self, execution_id: &str) -> Result<Option<WaitRecord>, FlowError>;
    /// Remove and return. This is the claim: of two concurrent callers only one gets `Some`.
    async fn take(&self, execution_id: &str) -> Result<Option<WaitRecord>, FlowError>;
    /// Records whose `resume_at <= now_ms`, oldest first.
    async fn due(&self, now_ms: i64) -> Result<Vec<WaitRecord>, FlowError>;
    /// Everything parked, insertion order.
    async fn list(&self) -> Result<Vec<WaitRecord>, FlowError>;
}

#[derive(Debug, Default)]
pub struct InMemoryWaitStore {
    inner: Mutex<IndexMap<String, WaitRecord>>,
}

impl InMemoryWaitStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, IndexMap<String, WaitRecord>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl WaitStore for InMemoryWaitStore {
    async fn put(&self, record: &WaitRecord) -> Result<(), FlowError> {
        self.lock()
            .insert(record.execution_id.clone(), record.clone());
        Ok(())
    }

    async fn get(&self, execution_id: &str) -> Result<Option<WaitRecord>, FlowError> {
        Ok(self.lock().get(execution_id).cloned())
    }

    async fn take(&self, execution_id: &str) -> Result<Option<WaitRecord>, FlowError> {
        Ok(self.lock().shift_remove(execution_id))
    }

    async fn due(&self, now_ms: i64) -> Result<Vec<WaitRecord>, FlowError> {
        let mut v: Vec<WaitRecord> = self
            .lock()
            .values()
            .filter(|r| r.request.resume_at.is_some_and(|t| t <= now_ms))
            .cloned()
            .collect();
        v.sort_by_key(|r| r.request.resume_at);
        Ok(v)
    }

    async fn list(&self) -> Result<Vec<WaitRecord>, FlowError> {
        Ok(self.lock().values().cloned().collect())
    }
}
