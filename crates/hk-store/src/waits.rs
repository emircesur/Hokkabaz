//! Durable Wait snapshots (migration 0003). The record is an opaque JSON string here:
//! hk-store does not know hk-flow, the daemon's `WaitStore` impl does the (de)serializing.

use sqlx::Row;

use crate::{Store, StoreError};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Store {
    /// Insert or replace by `execution_id` (insertion order is kept on replace).
    pub async fn put_wait(
        &self,
        execution_id: &str,
        workflow_id: Option<&str>,
        resume_at: Option<i64>,
        record: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO flow_waits (execution_id, workflow_id, resume_at, record, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (execution_id) DO UPDATE
             SET workflow_id = ?2, resume_at = ?3, record = ?4",
        )
        .bind(execution_id)
        .bind(workflow_id)
        .bind(resume_at)
        .bind(record)
        .bind(now_ms())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn get_wait(&self, execution_id: &str) -> Result<Option<String>, StoreError> {
        let r = sqlx::query("SELECT record FROM flow_waits WHERE execution_id = ?")
            .bind(execution_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(r.map(|r| r.get("record")))
    }

    /// Delete and return in one statement: the claim. Of two concurrent callers one gets `Some`.
    pub async fn take_wait(&self, execution_id: &str) -> Result<Option<String>, StoreError> {
        let r = sqlx::query("DELETE FROM flow_waits WHERE execution_id = ? RETURNING record")
            .bind(execution_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(r.map(|r| r.get("record")))
    }

    /// Records with `resume_at <= now_ms`, earliest first.
    pub async fn due_waits(&self, now_ms: i64) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query(
            "SELECT record FROM flow_waits WHERE resume_at IS NOT NULL AND resume_at <= ? ORDER BY resume_at, rowid",
        )
        .bind(now_ms)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(|r| r.get("record")).collect())
    }

    /// Everything parked, insertion order.
    pub async fn list_waits(&self) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query("SELECT record FROM flow_waits ORDER BY rowid")
            .fetch_all(self.pool())
            .await?;
        Ok(rows.iter().map(|r| r.get("record")).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_rows_put_due_take() {
        let s = Store::open_in_memory().await.unwrap();
        s.put_wait("e1", Some("w"), Some(100), "a").await.unwrap();
        s.put_wait("e2", None, None, "b").await.unwrap();
        s.put_wait("e3", Some("w"), Some(50), "c").await.unwrap();
        s.put_wait("e1", Some("w"), Some(100), "a2").await.unwrap();
        assert_eq!(s.due_waits(99).await.unwrap(), ["c"]);
        assert_eq!(s.due_waits(100).await.unwrap(), ["c", "a2"]);
        assert_eq!(s.list_waits().await.unwrap(), ["a2", "b", "c"]);
        assert_eq!(s.take_wait("e1").await.unwrap().as_deref(), Some("a2"));
        assert!(s.take_wait("e1").await.unwrap().is_none());
        assert_eq!(s.get_wait("e2").await.unwrap().as_deref(), Some("b"));
    }
}
