//! The node registry the app runs and the canvas lists: core nodes + the built-in integrations
//! + nodes from loaded plugins. Rebuilt when plugins change.

use std::sync::{Arc, PoisonError, RwLock};

use hk_flow::{NodeRegistry, register_core_nodes};
use hk_integrations::CredentialTypes;

use crate::error::HkError;

impl std::fmt::Debug for NodeEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NodeEnv")
    }
}

pub struct NodeEnv {
    pub registry: Arc<NodeRegistry>,
    /// Shared across rebuilds: integrations and the credential forms see the same types.
    pub cred_types: CredentialTypes,
}

static ENV: RwLock<Option<Arc<NodeEnv>>> = RwLock::new(None);
static CRED_TYPES: std::sync::OnceLock<CredentialTypes> = std::sync::OnceLock::new();

fn cred_types() -> CredentialTypes {
    CRED_TYPES
        .get_or_init(CredentialTypes::with_builtins)
        .clone()
}

fn build(extra: impl FnOnce(&mut NodeRegistry)) -> Result<NodeEnv, HkError> {
    let mut reg = NodeRegistry::new();
    register_core_nodes(&mut reg).map_err(|e| HkError::internal(format!("flow registry: {e}")))?;
    let types = cred_types();
    hk_integrations::register_integrations(&mut reg, &types)
        .map_err(|e| HkError::internal(format!("integrations: {e}")))?;
    crate::studio_node::register(&mut reg);
    extra(&mut reg);
    Ok(NodeEnv {
        registry: Arc::new(reg),
        cred_types: types,
    })
}

/// Current environment (built on first use).
pub fn env() -> Arc<NodeEnv> {
    if let Some(e) = ENV.read().unwrap_or_else(PoisonError::into_inner).clone() {
        return e;
    }
    let mut w = ENV.write().unwrap_or_else(PoisonError::into_inner);
    if let Some(e) = w.clone() {
        return e;
    }
    // Building only fails on duplicate registrations, which the tests rule out.
    let e = Arc::new(build(|_| {}).unwrap_or_else(|e| {
        tracing::error!(error = %e, "node registry build failed; falling back to core nodes");
        let mut reg = NodeRegistry::new();
        let _ = register_core_nodes(&mut reg);
        NodeEnv {
            registry: Arc::new(reg),
            cred_types: cred_types(),
        }
    }));
    *w = Some(e.clone());
    e
}

pub fn registry() -> Arc<NodeRegistry> {
    env().registry.clone()
}

/// Rebuild with `extra` registering plugin nodes; running executions keep the old registry.
pub fn rebuild(extra: impl FnOnce(&mut NodeRegistry)) -> Result<(), HkError> {
    let e = Arc::new(build(extra)?);
    *ENV.write().unwrap_or_else(PoisonError::into_inner) = Some(e);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn core_and_integration_nodes_register_together() {
        let reg = super::registry();
        for ty in [
            "n8n-nodes-base.set",
            "n8n-nodes-base.httpRequest",
            "hokkabaz.ntfy",
        ] {
            assert!(reg.latest(ty).is_some(), "{ty} missing");
        }
        assert!(super::env().cred_types.get("ntfyApi").is_some());
        super::rebuild(|_| {}).unwrap();
        assert!(super::registry().latest("hokkabaz.ntfy").is_some());
    }
}
