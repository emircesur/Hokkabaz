//! Link to the user's own n8n instance: pull / push workflows and (optionally) run node types
//! this engine lacks there. The instance is an `n8nApi` credential (API key + base URL), so the
//! key lives in the secret store like every other credential.

use std::sync::{Arc, PoisonError, RwLock};

use hk_flow::{CredentialRef, CredentialResolver, RemoteNodeRunner};
use hk_integrations::n8n_remote::{LocalCred, N8nClient, N8nRemote, map_credentials};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::credentials::Credentials;
use crate::error::{HkError, HkErrorKind};

const SETTINGS_KEY: &str = "n8n.link";
const CRED_TYPE: &str = "n8nApi";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Link {
    pub credential_id: Option<String>,
    /// Run unsupported node types on the instance instead of failing them.
    pub delegate: bool,
}

pub struct Hub {
    creds: Arc<Credentials>,
    link: RwLock<Link>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("n8n_sync::Hub")
    }
}

static GLOBAL: RwLock<Option<Arc<Hub>>> = RwLock::new(None);

pub async fn install(creds: Arc<Credentials>) -> Result<Arc<Hub>, HkError> {
    let link = creds.store.get_setting(SETTINGS_KEY).await?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let s = Arc::new(Hub { creds, link: RwLock::new(link) });
    *GLOBAL.write().unwrap_or_else(PoisonError::into_inner) = Some(s.clone());
    Ok(s)
}

pub fn uninstall() {
    *GLOBAL.write().unwrap_or_else(PoisonError::into_inner) = None;
}

pub fn get() -> Result<Arc<Hub>, HkError> {
    GLOBAL
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| HkError::new(HkErrorKind::NotInitialized, "n8n link needs the in-process core (core_init)"))
}

fn remote_err(e: String) -> HkError {
    HkError::new(HkErrorKind::Unavailable, e)
}

impl Hub {
    pub fn link(&self) -> Link {
        self.link.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub async fn configure(&self, credential_id: Option<String>, delegate: bool) -> Result<(), HkError> {
        if let Some(id) = &credential_id {
            let row = self.creds.store.get_credential(id).await?.ok_or_else(|| HkError::new(HkErrorKind::NotFound, format!("credential {id}")))?;
            if row.kind != CRED_TYPE {
                return Err(HkError::invalid(format!("credential `{}` is not an n8n API credential", row.name)));
            }
        }
        let link = Link { delegate: delegate && credential_id.is_some(), credential_id };
        self.creds.store.set_setting(SETTINGS_KEY, &serde_json::to_string(&link)?).await?;
        *self.link.write().unwrap_or_else(PoisonError::into_inner) = link;
        Ok(())
    }

    async fn client(&self) -> Result<N8nClient, HkError> {
        let id = self.link().credential_id.ok_or_else(|| HkError::invalid("choose an n8n API credential first"))?;
        let data = self
            .creds
            .resolve(CRED_TYPE, &CredentialRef { id: Some(id), name: None, extra: Default::default() })
            .await
            .map_err(|e| HkError::invalid(e.to_string()))?;
        let key = data.get("apiKey").and_then(Value::as_str).unwrap_or_default();
        let url = data.get("baseUrl").and_then(Value::as_str).unwrap_or_default();
        N8nClient::new(url, key).map_err(HkError::invalid)
    }

    pub async fn test(&self) -> Result<(), HkError> {
        self.client().await?.ping().await.map_err(remote_err)
    }

    pub async fn list_workflows(&self) -> Result<Value, HkError> {
        let all = self.client().await?.list_workflows().await.map_err(remote_err)?;
        Ok(Value::Array(
            all.iter()
                .map(|w| json!({"id": w["id"], "name": w["name"], "active": w["active"], "updated_at": w["updatedAt"], "nodes": w["nodes"].as_array().map(Vec::len)}))
                .collect(),
        ))
    }

    /// A workflow ready to open in the canvas, with credential references re-pointed at local
    /// credentials where type and name match.
    pub async fn pull(&self, id: &str) -> Result<Value, HkError> {
        let mut wf = self.client().await?.get_workflow(id).await.map_err(remote_err)?;
        let local: Vec<LocalCred> = self.creds.list(None).await?.into_iter().map(|c| LocalCred { id: c.id, name: c.name, kind: c.kind }).collect();
        let missing = map_credentials(&mut wf, &local);
        Ok(json!({
            "workflow": wf,
            "unmapped": missing.iter().map(|m| json!({"node": m.node, "type": m.kind, "name": m.name})).collect::<Vec<_>>(),
        }))
    }

    /// Create (or with `remote_id`, replace) a workflow on the instance. Returns its id.
    pub async fn push(&self, workflow: &Value, remote_id: Option<&str>) -> Result<String, HkError> {
        let c = self.client().await?;
        let res = match remote_id {
            Some(id) => c.update_workflow(id, workflow).await,
            None => c.create_workflow(workflow).await,
        }
        .map_err(remote_err)?;
        Ok(res["id"].as_str().map(str::to_owned).or_else(|| remote_id.map(str::to_owned)).unwrap_or_default())
    }

    /// Runner for unsupported nodes, when delegation is on and the credential still resolves.
    pub async fn delegate(&self) -> Option<Arc<dyn RemoteNodeRunner>> {
        if !self.link().delegate {
            return None;
        }
        match self.client().await {
            Ok(c) => Some(Arc::new(N8nRemote::new(c))),
            Err(e) => {
                tracing::warn!(error = %e, "n8n delegation is on but the instance is not usable");
                None
            }
        }
    }
}
