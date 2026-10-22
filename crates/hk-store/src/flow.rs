//! Workflow / execution / credential-metadata persistence for the n8n-shaped REST API.
//!
//! JSON columns are plain text here; shaping them into n8n responses is the server's job.
//! Credential *values* are not handled here at all: only metadata rows (see migration 0002).

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Store, StoreError};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRow {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub version_id: String,
    /// JSON object text: the workflow minus id/name/active.
    pub data: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRow {
    pub id: i64,
    pub workflow_id: String,
    pub mode: String,
    pub status: String,
    pub finished: bool,
    pub started_ms: i64,
    pub stopped_ms: Option<i64>,
    pub workflow_data: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CredentialRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// Filters for [`Store::list_executions`].
#[derive(Debug, Clone, Default)]
pub struct ExecutionFilter {
    pub workflow_id: Option<String>,
    pub status: Option<String>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn workflow_row(r: &SqliteRow) -> WorkflowRow {
    WorkflowRow {
        id: r.get("id"),
        name: r.get("name"),
        active: r.get::<i64, _>("active") != 0,
        version_id: r.get("version_id"),
        data: r.get("data"),
        created_ms: r.get("created_ms"),
        updated_ms: r.get("updated_ms"),
    }
}

fn execution_row(r: &SqliteRow) -> ExecutionRow {
    ExecutionRow {
        id: r.get("id"),
        workflow_id: r.get("workflow_id"),
        mode: r.get("mode"),
        status: r.get("status"),
        finished: r.get::<i64, _>("finished") != 0,
        started_ms: r.get("started_ms"),
        stopped_ms: r.get("stopped_ms"),
        workflow_data: r.get("workflow_data"),
        data: r.get("data"),
    }
}

fn credential_row(r: &SqliteRow) -> CredentialRow {
    CredentialRow {
        id: r.get("id"),
        name: r.get("name"),
        kind: r.get("type"),
        created_ms: r.get("created_ms"),
        updated_ms: r.get("updated_ms"),
    }
}

impl Store {
    // ------------------------------------------------------------- workflows

    pub async fn insert_workflow(
        &self,
        id: &str,
        name: &str,
        version_id: &str,
        data: &str,
    ) -> Result<WorkflowRow, StoreError> {
        let now = now_ms();
        sqlx::query(
            "INSERT INTO workflows (id, name, active, version_id, data, created_ms, updated_ms)
             VALUES (?, ?, 0, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(version_id)
        .bind(data)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(WorkflowRow {
            id: id.into(),
            name: name.into(),
            active: false,
            version_id: version_id.into(),
            data: data.into(),
            created_ms: now,
            updated_ms: now,
        })
    }

    pub async fn get_workflow(&self, id: &str) -> Result<Option<WorkflowRow>, StoreError> {
        let r = sqlx::query("SELECT * FROM workflows WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().map(workflow_row))
    }

    /// Newest-updated first. `name` is a case-insensitive substring match.
    pub async fn list_workflows(
        &self,
        active: Option<bool>,
        name: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<WorkflowRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM workflows
             WHERE (?1 IS NULL OR active = ?1)
               AND (?2 IS NULL OR instr(lower(name), lower(?2)) > 0)
             ORDER BY updated_ms DESC, id
             LIMIT ?3 OFFSET ?4",
        )
        .bind(active.map(i64::from))
        .bind(name)
        .bind(i64::from(limit))
        .bind(i64::from(offset))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(workflow_row).collect())
    }

    /// Replace name/data/version. Returns the updated row, `None` when `id` is unknown.
    pub async fn update_workflow(
        &self,
        id: &str,
        name: &str,
        version_id: &str,
        data: &str,
    ) -> Result<Option<WorkflowRow>, StoreError> {
        let r = sqlx::query(
            "UPDATE workflows SET name = ?, version_id = ?, data = ?, updated_ms = ? WHERE id = ?",
        )
        .bind(name)
        .bind(version_id)
        .bind(data)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_workflow(id).await
    }

    pub async fn set_workflow_active(
        &self,
        id: &str,
        active: bool,
    ) -> Result<Option<WorkflowRow>, StoreError> {
        let r = sqlx::query("UPDATE workflows SET active = ?, updated_ms = ? WHERE id = ?")
            .bind(i64::from(active))
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_workflow(id).await
    }

    /// Executions go with it (FK cascade).
    pub async fn delete_workflow(&self, id: &str) -> Result<bool, StoreError> {
        let r = sqlx::query("DELETE FROM workflows WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // ------------------------------------------------------------ executions

    pub async fn insert_execution(
        &self,
        workflow_id: &str,
        mode: &str,
        status: &str,
        workflow_data: Option<&str>,
    ) -> Result<ExecutionRow, StoreError> {
        let started = now_ms();
        let id = sqlx::query(
            "INSERT INTO executions (workflow_id, mode, status, finished, started_ms, workflow_data)
             VALUES (?, ?, ?, 0, ?, ?)",
        )
        .bind(workflow_id)
        .bind(mode)
        .bind(status)
        .bind(started)
        .bind(workflow_data)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();
        Ok(ExecutionRow {
            id,
            workflow_id: workflow_id.into(),
            mode: mode.into(),
            status: status.into(),
            finished: false,
            started_ms: started,
            stopped_ms: None,
            workflow_data: workflow_data.map(Into::into),
            data: None,
        })
    }

    pub async fn finish_execution(
        &self,
        id: i64,
        status: &str,
        finished: bool,
        data: Option<&str>,
    ) -> Result<bool, StoreError> {
        let r = sqlx::query(
            "UPDATE executions SET status = ?, finished = ?, stopped_ms = ?, data = ? WHERE id = ?",
        )
        .bind(status)
        .bind(i64::from(finished))
        .bind(now_ms())
        .bind(data)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn get_execution(&self, id: i64) -> Result<Option<ExecutionRow>, StoreError> {
        let r = sqlx::query("SELECT * FROM executions WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().map(execution_row))
    }

    /// Newest first; `before_id` is the keyset cursor (exclusive). Rows include `data`.
    pub async fn list_executions(
        &self,
        filter: &ExecutionFilter,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<ExecutionRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM executions
             WHERE (?1 IS NULL OR workflow_id = ?1)
               AND (?2 IS NULL OR status = ?2)
               AND (?3 IS NULL OR id < ?3)
             ORDER BY id DESC
             LIMIT ?4",
        )
        .bind(filter.workflow_id.as_deref())
        .bind(filter.status.as_deref())
        .bind(before_id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(execution_row).collect())
    }

    pub async fn delete_execution(&self, id: i64) -> Result<bool, StoreError> {
        let r = sqlx::query("DELETE FROM executions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // ----------------------------------------------------------- credentials

    pub async fn insert_credential(
        &self,
        id: &str,
        name: &str,
        kind: &str,
    ) -> Result<CredentialRow, StoreError> {
        let now = now_ms();
        sqlx::query("INSERT INTO credentials (id, name, type, created_ms, updated_ms) VALUES (?, ?, ?, ?, ?)")
            .bind(id)
            .bind(name)
            .bind(kind)
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(CredentialRow {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            created_ms: now,
            updated_ms: now,
        })
    }

    pub async fn get_credential(&self, id: &str) -> Result<Option<CredentialRow>, StoreError> {
        let r = sqlx::query("SELECT * FROM credentials WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().map(credential_row))
    }

    /// Credential metadata (never values), newest first; `kind` filters by credential type.
    pub async fn list_credentials(
        &self,
        kind: Option<&str>,
    ) -> Result<Vec<CredentialRow>, StoreError> {
        let rows = match kind {
            Some(k) => {
                sqlx::query("SELECT * FROM credentials WHERE type = ? ORDER BY updated_ms DESC")
                    .bind(k)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                sqlx::query("SELECT * FROM credentials ORDER BY updated_ms DESC")
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        Ok(rows.iter().map(credential_row).collect())
    }

    /// Rename and touch.
    pub async fn rename_credential(&self, id: &str, name: &str) -> Result<bool, StoreError> {
        let r = sqlx::query("UPDATE credentials SET name = ?, updated_ms = ? WHERE id = ?")
            .bind(name)
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn delete_credential(&self, id: &str) -> Result<bool, StoreError> {
        let r = sqlx::query("DELETE FROM credentials WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workflow_execution_credential_roundtrip() {
        let s = Store::open_in_memory().await.unwrap();
        let w = s
            .insert_workflow("w1", "Alpha", "v1", "{\"nodes\":[]}")
            .await
            .unwrap();
        assert!(!w.active);
        assert!(
            s.set_workflow_active("w1", true)
                .await
                .unwrap()
                .unwrap()
                .active
        );
        assert_eq!(
            s.list_workflows(Some(true), Some("alp"), 10, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            s.list_workflows(Some(false), None, 10, 0)
                .await
                .unwrap()
                .is_empty()
        );

        let e1 = s
            .insert_execution("w1", "webhook", "running", None)
            .await
            .unwrap();
        let e2 = s
            .insert_execution("w1", "webhook", "running", None)
            .await
            .unwrap();
        assert!(
            s.finish_execution(e1.id, "success", true, Some("{}"))
                .await
                .unwrap()
        );
        let page = s
            .list_executions(&ExecutionFilter::default(), None, 1)
            .await
            .unwrap();
        assert_eq!(page[0].id, e2.id);
        let ok = ExecutionFilter {
            status: Some("success".into()),
            ..Default::default()
        };
        assert_eq!(s.list_executions(&ok, None, 10).await.unwrap().len(), 1);

        // cascade
        assert!(s.delete_workflow("w1").await.unwrap());
        assert!(s.get_execution(e1.id).await.unwrap().is_none());

        let c = s.insert_credential("c1", "gh", "githubApi").await.unwrap();
        assert_eq!(s.get_credential("c1").await.unwrap().unwrap(), c);
        assert!(s.delete_credential("c1").await.unwrap());
    }
}
