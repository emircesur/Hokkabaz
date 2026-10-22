//! Flow credentials: metadata in SQLite, values in the secret store (`credential.<id>`).
//! Values never come back to Dart; flows reference credentials by id.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use hk_flow::{CredentialRef, CredentialResolver, FlowError};
use hk_store::{Secret, SecretStore, Store};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{HkError, HkErrorKind};

#[derive(Clone)]
pub struct Credentials {
    pub store: Store,
    pub secrets: Arc<dyn SecretStore>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials")
    }
}

#[derive(Debug, Serialize)]
pub struct CredentialInfo {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub updated_ms: i64,
}

fn secret_name(id: &str) -> String {
    format!("credential.{id}")
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("cred{ms:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
}

impl Credentials {
    pub async fn list(&self, kind: Option<&str>) -> Result<Vec<CredentialInfo>, HkError> {
        Ok(self
            .store
            .list_credentials(kind)
            .await?
            .into_iter()
            .map(|r| CredentialInfo {
                id: r.id,
                name: r.name,
                kind: r.kind,
                updated_ms: r.updated_ms,
            })
            .collect())
    }

    /// Create a credential of a known type. `data` is validated against the type's properties
    /// (unknown keys are rejected so typos do not silently store garbage).
    pub async fn create(
        &self,
        name: &str,
        kind: &str,
        data: Map<String, Value>,
    ) -> Result<CredentialInfo, HkError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 100 {
            return Err(HkError::invalid("credential name must be 1-100 characters"));
        }
        let env = crate::nodes::env();
        let ty = env
            .cred_types
            .resolved(kind)
            .ok_or_else(|| HkError::invalid(format!("unknown credential type `{kind}`")))?;
        let known: Vec<&str> = ty.properties.iter().map(|p| p.name.as_str()).collect();
        if let Some(bad) = data.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(HkError::invalid(format!(
                "`{bad}` is not a field of `{kind}`"
            )));
        }
        let id = new_id();
        self.store
            .put_secret(
                self.secrets.as_ref(),
                &secret_name(&id),
                Secret::new(Value::Object(data).to_string()),
            )
            .await?;
        let row = self.store.insert_credential(&id, name, kind).await?;
        Ok(CredentialInfo {
            id: row.id,
            name: row.name,
            kind: row.kind,
            updated_ms: row.updated_ms,
        })
    }

    /// Replace the values (full object) and optionally rename.
    pub async fn update(
        &self,
        id: &str,
        name: Option<&str>,
        data: Option<Map<String, Value>>,
    ) -> Result<(), HkError> {
        let row = self
            .store
            .get_credential(id)
            .await?
            .ok_or_else(|| HkError::new(HkErrorKind::NotFound, format!("credential {id}")))?;
        if let Some(d) = data {
            let env = crate::nodes::env();
            if let Some(ty) = env.cred_types.resolved(&row.kind) {
                let known: Vec<&str> = ty.properties.iter().map(|p| p.name.as_str()).collect();
                if let Some(bad) = d.keys().find(|k| !known.contains(&k.as_str())) {
                    return Err(HkError::invalid(format!(
                        "`{bad}` is not a field of `{}`",
                        row.kind
                    )));
                }
            }
            self.store
                .put_secret(
                    self.secrets.as_ref(),
                    &secret_name(id),
                    Secret::new(Value::Object(d).to_string()),
                )
                .await?;
        }
        if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
            self.store.rename_credential(id, n).await?;
        }
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), HkError> {
        if !self.store.delete_credential(id).await? {
            return Err(HkError::new(
                HkErrorKind::NotFound,
                format!("credential {id}"),
            ));
        }
        self.store
            .delete_secret(self.secrets.as_ref(), &secret_name(id))
            .await?;
        Ok(())
    }

    /// Which fields are set (never the values), for the edit form.
    pub async fn set_fields(&self, id: &str) -> Result<Vec<String>, HkError> {
        let blob = self
            .store
            .get_secret(self.secrets.as_ref(), &secret_name(id))?
            .ok_or_else(|| HkError::new(HkErrorKind::NotFound, format!("credential {id}")))?;
        match serde_json::from_str::<Value>(blob.expose()) {
            Ok(Value::Object(m)) => Ok(m
                .into_iter()
                .filter(|(_, v)| !matches!(v, Value::String(s) if s.is_empty()) && !v.is_null())
                .map(|(k, _)| k)
                .collect()),
            _ => Ok(Vec::new()),
        }
    }
}

#[async_trait]
impl CredentialResolver for Credentials {
    async fn resolve(
        &self,
        credential_type: &str,
        reference: &CredentialRef,
    ) -> Result<Map<String, Value>, FlowError> {
        let id = reference.id.as_deref().ok_or_else(|| {
            FlowError::Invalid(format!("credential of type {credential_type} has no id"))
        })?;
        match self
            .store
            .get_credential(id)
            .await
            .map_err(|_| FlowError::Store("credential lookup failed".into()))?
        {
            Some(r) if r.kind == credential_type => {}
            Some(_) => {
                return Err(FlowError::Invalid(format!(
                    "credential {id} is not of type {credential_type}"
                )));
            }
            None => return Err(FlowError::Invalid(format!("credential {id} not found"))),
        }
        let blob = self
            .store
            .get_secret(self.secrets.as_ref(), &secret_name(id))
            .map_err(|_| FlowError::Store("credential secret unavailable".into()))?
            .ok_or_else(|| FlowError::Invalid(format!("credential {id} has no stored value")))?;
        match serde_json::from_str::<Value>(blob.expose()) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(FlowError::Invalid(format!("credential {id} is corrupt"))),
        }
    }
}

static CREDS: std::sync::RwLock<Option<Arc<Credentials>>> = std::sync::RwLock::new(None);

pub fn install(c: Arc<Credentials>) {
    *CREDS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(c);
}

pub fn uninstall() {
    *CREDS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

pub fn global() -> Result<Arc<Credentials>, HkError> {
    CREDS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or_else(|| {
            HkError::new(
                HkErrorKind::NotInitialized,
                "credentials need the in-process core (core_init)",
            )
        })
}

#[cfg(test)]
mod tests {
    use hk_store::MemorySecretStore;
    use serde_json::json;

    use super::*;

    async fn creds() -> Credentials {
        Credentials {
            store: Store::open_in_memory().await.unwrap(),
            secrets: Arc::new(MemorySecretStore::default()),
        }
    }

    #[tokio::test]
    async fn create_resolve_update_delete() {
        let c = creds().await;
        let data = json!({"serverUrl": "https://ntfy.sh", "accessToken": "tk_secret"})
            .as_object()
            .unwrap()
            .clone();
        let info = c.create("My ntfy", "ntfyApi", data).await.unwrap();
        assert_eq!(c.list(Some("ntfyApi")).await.unwrap().len(), 1);
        assert!(c.list(Some("other")).await.unwrap().is_empty());
        assert!(
            !serde_json::to_string(&c.list(None).await.unwrap())
                .unwrap()
                .contains("tk_secret")
        );
        let r = c
            .resolve(
                "ntfyApi",
                &CredentialRef {
                    id: Some(info.id.clone()),
                    name: None,
                    extra: Map::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(r["accessToken"], "tk_secret");
        assert!(
            c.resolve(
                "githubApi",
                &CredentialRef {
                    id: Some(info.id.clone()),
                    name: None,
                    extra: Map::new()
                }
            )
            .await
            .is_err(),
            "type mismatch"
        );
        let mut fields = c.set_fields(&info.id).await.unwrap();
        fields.sort();
        assert_eq!(fields, ["accessToken", "serverUrl"]);

        c.update(
            &info.id,
            Some("Renamed"),
            Some(
                json!({"serverUrl": "https://x.example"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(c.list(None).await.unwrap()[0].name, "Renamed");
        let r = c
            .resolve(
                "ntfyApi",
                &CredentialRef {
                    id: Some(info.id.clone()),
                    name: None,
                    extra: Map::new(),
                },
            )
            .await
            .unwrap();
        assert!(r.get("accessToken").is_none());

        c.delete(&info.id).await.unwrap();
        assert!(c.delete(&info.id).await.is_err());
        assert!(
            c.resolve(
                "ntfyApi",
                &CredentialRef {
                    id: Some(info.id),
                    name: None,
                    extra: Map::new()
                }
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn rejects_unknown_type_field_and_empty_name() {
        let c = creds().await;
        assert!(c.create("x", "noSuchType", Map::new()).await.is_err());
        assert!(
            c.create(
                "x",
                "ntfyApi",
                json!({"bogus": 1}).as_object().unwrap().clone()
            )
            .await
            .is_err()
        );
        assert!(c.create("  ", "ntfyApi", Map::new()).await.is_err());
    }
}
