use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use hk_core::Effect;
use jsonschema::Validator;
use serde_json::Value;

use crate::tool::{Tool, ToolError, ToolSpec};

struct Entry {
    tool: Arc<dyn Tool>,
    schema: Arc<Validator>,
}

/// Name -> tool map. Schemas compile once at registration. Cheap to clone.
#[derive(Clone, Default)]
pub struct ToolRegistry(Arc<RwLock<HashMap<String, Entry>>>);

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.names())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Dropping the returned `Effect` unregisters it (plugin unload).
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<Effect, ToolError> {
        let spec = tool.spec();
        let schema =
            jsonschema::validator_for(&spec.parameters).map_err(|e| ToolError::BadSchema {
                tool: spec.name.clone(),
                reason: e.to_string(),
            })?;
        let name = spec.name;
        {
            let mut map = self.0.write().unwrap_or_else(|p| p.into_inner());
            if map.contains_key(&name) {
                return Err(ToolError::Duplicate(name));
            }
            map.insert(
                name.clone(),
                Entry {
                    tool,
                    schema: Arc::new(schema),
                },
            );
        }
        let inner = self.0.clone();
        Ok(Effect::new(move || {
            inner
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&name);
        }))
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.read(name, |e| e.tool.clone())
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<_> = self
            .0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }

    /// Specs offered to the model, sorted by name for stable prompts.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut v: Vec<_> = self
            .0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .map(|e| e.tool.spec())
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Check `args` against the tool's JSON Schema.
    pub fn validate(&self, name: &str, args: &Value) -> Result<(), ToolError> {
        let schema = self
            .read(name, |e| e.schema.clone())
            .ok_or_else(|| ToolError::Unknown(name.to_owned()))?;
        let errors: Vec<String> = schema
            .iter_errors(args)
            .map(|e| match e.instance_path().to_string() {
                p if p.is_empty() => e.to_string(),
                p => format!("{p}: {e}"),
            })
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ToolError::InvalidArgs {
                tool: name.to_owned(),
                reason: errors.join("; "),
            })
        }
    }

    fn read<R>(&self, name: &str, f: impl FnOnce(&Entry) -> R) -> Option<R> {
        self.0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .map(f)
    }
}
