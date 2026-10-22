//! WASM plugins on a headless instance: everything installed under `<data>/plugins` is loaded
//! at start. Tools join the agent, nodes join the flow registry, and each declared trigger
//! becomes a polling trigger node (`<plugin id>.<trigger>`), armed when a workflow using it is
//! activated. Install with `hk plugin install` (same data dir) and restart.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, NodeRegistry};
use hk_integrations::poll::{PollBatch, PollContext, PolledItem};
use hk_integrations::{PollRegistry, PollingTrigger};
use hk_plugin::host::LoadedPlugin;
use hk_plugin::store::PluginStore;
use hk_plugin::{HostServices, PluginHost, SecretLookup};
use hk_store::SecretStore;
use serde_json::{Value, json};

struct Secrets(Arc<dyn SecretStore>);

impl SecretLookup for Secrets {
    fn get(&self, name: &str) -> Option<String> {
        // Plugins see only secrets stored under the `plugin/` namespace.
        self.0
            .get(&format!("plugin/{name}"))
            .ok()
            .flatten()
            .map(|s| s.expose().to_owned())
    }
}

#[derive(Debug)]
struct PluginPoll {
    plugin: Arc<LoadedPlugin>,
    trigger: String,
    desc: NodeTypeDescription,
}

#[async_trait]
impl PollingTrigger for PluginPoll {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn poll(
        &self,
        _cx: &PollContext,
        cursor: Option<&Value>,
    ) -> Result<PollBatch, FlowError> {
        let (plugin, name, state) = (
            self.plugin.clone(),
            self.trigger.clone(),
            cursor.cloned().unwrap_or(Value::Null),
        );
        let (events, new_state) =
            tokio::task::spawn_blocking(move || plugin.poll_trigger(&name, &state))
                .await
                .map_err(|e| FlowError::Store(e.to_string()))?
                .map_err(|e| FlowError::Store(format!("plugin trigger `{}`: {e}", self.trigger)))?;
        let items = events
            .into_iter()
            .map(|e| {
                // Events with an `id` dedupe across polls; others are identified by content.
                let id = e
                    .get("id")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| e.to_string());
                PolledItem::new(id, e)
            })
            .collect();
        Ok(PollBatch {
            items,
            cursor: Some(new_state).filter(|s| !s.is_null()),
        })
    }
}

fn trigger_description(
    plugin_id: &str,
    plugin_name: &str,
    trigger: &str,
    interval_secs: u64,
) -> Result<NodeTypeDescription, FlowError> {
    serde_json::from_value(json!({
        "displayName": format!("{plugin_name}: {trigger}"),
        "name": format!("{plugin_id}.{trigger}"),
        "version": [1],
        "description": format!("Polling trigger of the {plugin_name} plugin"),
        "group": ["trigger"],
        "polling": true,
        "defaults": {"name": trigger},
        "inputs": [],
        "outputs": ["main"],
        "properties": [{
            "displayName": "Poll Times", "name": "pollTimes", "type": "fixedCollection", "default": {},
            "typeOptions": {"multipleValues": true},
            "options": [{"displayName": "Item", "name": "item", "values": [
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "everyX",
                 "options": [{"name": "Every X", "value": "everyX"}]},
                {"displayName": "Value", "name": "value", "type": "number", "default": interval_secs},
                {"displayName": "Unit", "name": "unit", "type": "options", "default": "seconds",
                 "options": [{"name": "Seconds", "value": "seconds"}, {"name": "Minutes", "value": "minutes"}]}]}]
        }]
    }))
    .map_err(|e| FlowError::Store(format!("plugin trigger description: {e}")))
}

/// What stays alive for the runtime's lifetime: the engine (dropping it stops the timeout
/// ticker) and the agent tools the plugins provide.
pub struct Loaded {
    pub tools: Vec<Arc<dyn hk_tools::Tool>>,
    _host: Option<PluginHost>,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

/// Load every enabled plugin under `<data>/plugins`. A plugin that fails is logged and skipped.
pub fn load(
    data_dir: &Path,
    secrets: Arc<dyn SecretStore>,
    nodes: &mut NodeRegistry,
    polls: &mut PollRegistry,
) -> Loaded {
    let none = Loaded {
        tools: Vec::new(),
        _host: None,
    };
    let store = PluginStore::new(data_dir.join("plugins"));
    let Ok(list) = store.list() else { return none };
    if list.is_empty() {
        return none;
    }
    let host = match PluginHost::new(HostServices {
        secrets: Arc::new(Secrets(secrets)),
        emit: Arc::new(
            |e| tracing::info!(plugin = %e.plugin_id, trigger = %e.trigger, "plugin event (use polling triggers to start flows)"),
        ),
        data_dir: data_dir.join("plugin-data"),
    }) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "plugin runtime unavailable");
            return none;
        }
    };
    let mut tools = Vec::new();
    for p in list.into_iter().filter(|p| p.enabled) {
        let id = p.manifest.id.clone();
        let loaded = match host.load(&p) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(plugin = %id, error = %e, "cannot load plugin");
                continue;
            }
        };
        let n = hk_plugin::node::register_plugin_nodes(nodes, &loaded);
        tools.extend(hk_plugin::tool::plugin_tools(&loaded));
        let mut triggers = 0;
        for t in &loaded.description.triggers {
            match trigger_description(&id, &p.manifest.name, &t.name, t.interval_secs) {
                Ok(desc) => {
                    let trig = Arc::new(PluginPoll {
                        plugin: loaded.clone(),
                        trigger: t.name.clone(),
                        desc,
                    });
                    match polls.register(nodes, trig) {
                        Ok(()) => triggers += 1,
                        Err(e) => {
                            tracing::warn!(plugin = %id, trigger = %t.name, error = %e, "trigger not registered")
                        }
                    }
                }
                Err(e) => tracing::warn!(plugin = %id, error = %e, "bad trigger description"),
            }
        }
        tracing::info!(plugin = %id, nodes = n, triggers, "plugin loaded");
    }
    Loaded {
        tools,
        _host: Some(host),
    }
}
