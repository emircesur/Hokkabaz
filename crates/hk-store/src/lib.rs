//! hk-store: SQLite (sqlx) migrations, settings table, secrets API.
//!
//! port: none (Hokkabaz-native; see DESIGN §3.12).

pub mod flow;
pub mod secrets;
pub mod waits;

use std::path::Path;
use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

pub use flow::{CredentialRow, ExecutionFilter, ExecutionRow, WorkflowRow};
pub use secrets::{MemorySecretStore, Secret, SecretError, SecretStore};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    Secret(#[from] SecretError),
}

/// Handle to the app database. Cheap to clone (pool inside).
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating if missing) the DB file and run embedded migrations.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        Self::from_pool(pool).await
    }

    /// In-memory DB. Single connection: each `:memory:` connection is its own DB.
    pub async fn open_in_memory() -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::from_pool(pool).await
    }

    async fn from_pool(pool: SqlitePool) -> Result<Self, StoreError> {
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Raw JSON text setting, or `None`.
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, unixepoch())
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_setting(&self, key: &str) -> Result<bool, StoreError> {
        let r = sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Store a secret in `backend` and record its name for listing.
    pub async fn put_secret(
        &self,
        backend: &dyn SecretStore,
        name: &str,
        value: Secret,
    ) -> Result<(), StoreError> {
        backend.set(name, &value)?;
        sqlx::query("INSERT OR IGNORE INTO secret_refs (name, created_at) VALUES (?, unixepoch())")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub fn get_secret(
        &self,
        backend: &dyn SecretStore,
        name: &str,
    ) -> Result<Option<Secret>, StoreError> {
        Ok(backend.get(name)?)
    }

    pub async fn delete_secret(
        &self,
        backend: &dyn SecretStore,
        name: &str,
    ) -> Result<(), StoreError> {
        backend.delete(name)?;
        sqlx::query("DELETE FROM secret_refs WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Names only; values stay in the backend.
    pub async fn list_secret_names(&self) -> Result<Vec<String>, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT name FROM secret_refs ORDER BY name")
                .fetch_all(&self.pool)
                .await?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_and_settings_roundtrip() {
        let s = Store::open_in_memory().await.unwrap();
        assert_eq!(s.get_setting("theme").await.unwrap(), None);
        s.set_setting("theme", "\"dark\"").await.unwrap();
        s.set_setting("theme", "\"light\"").await.unwrap();
        assert_eq!(
            s.get_setting("theme").await.unwrap().as_deref(),
            Some("\"light\"")
        );
        assert!(s.delete_setting("theme").await.unwrap());
        assert!(!s.delete_setting("theme").await.unwrap());
    }

    #[tokio::test]
    async fn migrations_idempotent_on_file() {
        let dir = std::env::temp_dir().join(format!("hk-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hk.db");
        let a = Store::open(&p).await.unwrap();
        a.set_setting("k", "1").await.unwrap();
        drop(a);
        let b = Store::open(&p).await.unwrap();
        assert_eq!(b.get_setting("k").await.unwrap().as_deref(), Some("1"));
        drop(b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn secrets_roundtrip_via_memory_backend() {
        let s = Store::open_in_memory().await.unwrap();
        let be = MemorySecretStore::default();
        s.put_secret(&be, "deepseek", Secret::new("sk-abc"))
            .await
            .unwrap();
        let got = s.get_secret(&be, "deepseek").unwrap().unwrap();
        assert_eq!(got.expose(), "sk-abc");
        assert_eq!(s.list_secret_names().await.unwrap(), vec!["deepseek"]);
        s.delete_secret(&be, "deepseek").await.unwrap();
        assert!(s.get_secret(&be, "deepseek").unwrap().is_none());
        assert!(s.list_secret_names().await.unwrap().is_empty());
    }
}
