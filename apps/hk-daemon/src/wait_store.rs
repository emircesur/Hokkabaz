//! SQLite-backed [`WaitStore`] (`flow_waits`, hk-store migration 0003). One JSON row per
//! parked execution; `take` is a single `DELETE ... RETURNING`, so it is a real claim even
//! with several workers on the same DB.

use async_trait::async_trait;
use hk_flow::{FlowError, WaitRecord, WaitStore};
use hk_store::{Store, StoreError};

use crate::scrub::scrub_headers;

#[derive(Debug, Clone)]
pub struct SqliteWaitStore {
    store: Store,
}

impl SqliteWaitStore {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

fn db(e: StoreError) -> FlowError {
    // sqlx text can echo SQL; keep it out of node-visible errors
    tracing::error!(error = %e, "wait store");
    FlowError::Store("wait store unavailable".into())
}

fn parse(s: &str) -> Result<WaitRecord, FlowError> {
    WaitRecord::from_json(s)
}

#[async_trait]
impl WaitStore for SqliteWaitStore {
    async fn put(&self, record: &WaitRecord) -> Result<(), FlowError> {
        // A resumed run sees `[redacted]` where a caller's Authorization header was.
        let mut v = serde_json::to_value(record)?;
        scrub_headers(&mut v);
        self.store
            .put_wait(
                &record.execution_id,
                record.workflow_id.as_deref(),
                record.request.resume_at,
                &v.to_string(),
            )
            .await
            .map_err(db)
    }

    async fn get(&self, execution_id: &str) -> Result<Option<WaitRecord>, FlowError> {
        self.store
            .get_wait(execution_id)
            .await
            .map_err(db)?
            .as_deref()
            .map(parse)
            .transpose()
    }

    async fn take(&self, execution_id: &str) -> Result<Option<WaitRecord>, FlowError> {
        self.store
            .take_wait(execution_id)
            .await
            .map_err(db)?
            .as_deref()
            .map(parse)
            .transpose()
    }

    async fn due(&self, now_ms: i64) -> Result<Vec<WaitRecord>, FlowError> {
        self.store
            .due_waits(now_ms)
            .await
            .map_err(db)?
            .iter()
            .map(|s| parse(s))
            .collect()
    }

    async fn list(&self) -> Result<Vec<WaitRecord>, FlowError> {
        self.store
            .list_waits()
            .await
            .map_err(db)?
            .iter()
            .map(|s| parse(s))
            .collect()
    }
}
