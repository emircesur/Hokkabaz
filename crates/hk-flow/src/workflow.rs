//! n8n workflow JSON, lossless.

use std::collections::HashSet;

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};
use serde_json::{Map, Number, Value};

use crate::error::FlowError;
use crate::item::Item;
use crate::raw::{Out, take, take_required};
use crate::version::{NodeKey, Version};

/// Default connection type for ordinary data flow.
pub const MAIN: &str = "main";

fn into_object<E: serde::de::Error>(v: Value, what: &str) -> Result<Map<String, Value>, E> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(E::custom(format!("{what} must be a json object"))),
    }
}

// ---------------------------------------------------------------- workflow

/// A workflow in n8n export format. Hokkabaz-only data lives under `meta.hokkabaz`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Workflow {
    pub id: Option<String>,
    pub name: Option<String>,
    pub active: Option<bool>,
    pub nodes: Vec<Node>,
    pub connections: Connections,
    pub settings: Option<Settings>,
    pub pin_data: Option<PinData>,
    /// `{"global": {...}, "node:<Name>": {...}}`
    pub static_data: Option<Value>,
    pub meta: Option<Value>,
    pub version_id: Option<String>,
    /// Tag objects (`{id,name,..}`) or bare names; kept raw.
    pub tags: Option<Vec<Value>>,
    /// Unknown top-level fields (`createdAt`, `triggerCount`, `isArchived`, ...), plus
    /// explicit nulls of modeled fields.
    pub extra: Map<String, Value>,
}

impl<'de> Deserialize<'de> for Workflow {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = into_object(Value::deserialize(d)?, "workflow")?;
        let err = D::Error::custom;
        Ok(Self {
            id: take(&mut m, "id"),
            name: take(&mut m, "name"),
            active: take(&mut m, "active"),
            nodes: take_required(&mut m, "nodes").map_err(err)?,
            connections: take(&mut m, "connections").unwrap_or_default(),
            settings: take(&mut m, "settings"),
            pin_data: take(&mut m, "pinData"),
            static_data: take(&mut m, "staticData"),
            meta: take(&mut m, "meta"),
            version_id: take(&mut m, "versionId"),
            tags: take(&mut m, "tags"),
            extra: m,
        })
    }
}

impl Serialize for Workflow {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut o = Out::default();
        let r = (|| {
            o.put("id", &self.id)?;
            o.put("name", &self.name)?;
            o.put("active", &self.active)?;
            o.put_req("nodes", &self.nodes)?;
            o.put_req("connections", &self.connections)?;
            o.put("settings", &self.settings)?;
            o.put("pinData", &self.pin_data)?;
            o.put("staticData", &self.static_data)?;
            o.put("meta", &self.meta)?;
            o.put("versionId", &self.version_id)?;
            o.put("tags", &self.tags)
        })();
        r.map_err(S::Error::custom)?;
        o.finish(&self.extra).serialize(s)
    }
}

impl Workflow {
    pub fn from_json_str(s: &str) -> Result<Self, FlowError> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn from_json_value(v: Value) -> Result<Self, FlowError> {
        Ok(serde_json::from_value(v)?)
    }

    pub fn to_json_value(&self) -> Result<Value, FlowError> {
        Ok(serde_json::to_value(self)?)
    }

    /// Pretty JSON, the way n8n writes export files.
    pub fn to_json_string(&self) -> Result<String, FlowError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }

    pub fn node_mut(&mut self, name: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.name == name)
    }

    /// Targets wired to output `output` of `node` for connection type `conn_type`.
    pub fn destinations(&self, node: &str, conn_type: &str, output: usize) -> &[ConnectionTarget] {
        self.connections
            .0
            .get(node)
            .and_then(|c| c.0.get(conn_type))
            .and_then(|slots| slots.get(output))
            .and_then(|slot| slot.as_deref())
            .unwrap_or(&[])
    }

    /// Reverse lookup: every edge that ends at `node`.
    pub fn incoming(&self, node: &str) -> Vec<IncomingEdge<'_>> {
        let mut out = Vec::new();
        for (src, conns) in &self.connections.0 {
            for (ty, slots) in &conns.0 {
                for (output, slot) in slots.iter().enumerate() {
                    for t in slot.iter().flatten() {
                        if t.node == node {
                            out.push(IncomingEdge {
                                source: src,
                                conn_type: ty,
                                output,
                                input: t.index,
                                target_type: &t.conn_type,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// Pinned items for a node, normalized to [`Item`].
    pub fn pinned_items(&self, node: &str) -> Option<Vec<Item>> {
        let raw = self.pin_data.as_ref()?.0.get(node)?;
        Some(raw.iter().map(Item::from_pinned).collect())
    }

    /// `meta.hokkabaz`, the home for Hokkabaz-only data.
    pub fn hokkabaz_meta(&self) -> Option<&Value> {
        self.meta.as_ref()?.get("hokkabaz")
    }

    pub fn set_hokkabaz_meta(&mut self, v: Value) {
        let meta = self.meta.get_or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(m) = meta {
            m.insert("hokkabaz".into(), v);
        }
    }

    /// Structural checks run on import. Unknown node *types* are not errors (placeholders).
    pub fn validate(&self) -> Result<(), FlowError> {
        let mut names = HashSet::new();
        for n in &self.nodes {
            if !names.insert(n.name.as_str()) {
                return Err(FlowError::Invalid(format!(
                    "duplicate node name `{}`",
                    n.name
                )));
            }
        }
        for (src, conns) in &self.connections.0 {
            if !names.contains(src.as_str()) {
                return Err(FlowError::Invalid(format!(
                    "connection from unknown node `{src}`"
                )));
            }
            for t in conns.0.values().flatten().flatten().flatten() {
                if !names.contains(t.node.as_str()) {
                    return Err(FlowError::Invalid(format!(
                        "connection `{src}` -> unknown node `{}`",
                        t.node
                    )));
                }
            }
        }
        if let Some(p) = &self.pin_data {
            for k in p.0.keys() {
                if !names.contains(k.as_str()) {
                    return Err(FlowError::Invalid(format!(
                        "pinData for unknown node `{k}`"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// One inbound edge, see [`Workflow::incoming`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingEdge<'a> {
    pub source: &'a str,
    /// Connection type on the source side (`main`, `ai_tool`, ...).
    pub conn_type: &'a str,
    pub output: usize,
    pub input: u32,
    pub target_type: &'a str,
}

// ------------------------------------------------------------ connections

/// `{ "<source node>": { "<type>": [ [targets of output 0], [targets of output 1] ] } }`
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Connections(pub IndexMap<String, NodeConnections>);

/// Output slots per connection type. A slot may be `null` in the wild, kept as `None`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeConnections(pub IndexMap<String, Vec<Option<Vec<ConnectionTarget>>>>);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionTarget {
    pub node: String,
    #[serde(rename = "type")]
    pub conn_type: String,
    pub index: u32,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ConnectionTarget {
    pub fn main(node: impl Into<String>, index: u32) -> Self {
        Self {
            node: node.into(),
            conn_type: MAIN.into(),
            index,
            extra: Map::new(),
        }
    }
}

// --------------------------------------------------------------- settings

/// Workflow settings. Only keys the engine acts on are typed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    /// `"v1"` for current n8n execution order.
    pub execution_order: Option<String>,
    pub timezone: Option<String>,
    pub error_workflow: Option<String>,
    /// Seconds; `-1` = none.
    pub execution_timeout: Option<i64>,
    pub save_manual_executions: Option<bool>,
    pub save_data_error_execution: Option<String>,
    pub save_data_success_execution: Option<String>,
    pub caller_policy: Option<String>,
    pub extra: Map<String, Value>,
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = into_object(Value::deserialize(d)?, "settings")?;
        Ok(Self {
            execution_order: take(&mut m, "executionOrder"),
            timezone: take(&mut m, "timezone"),
            error_workflow: take(&mut m, "errorWorkflow"),
            execution_timeout: take(&mut m, "executionTimeout"),
            save_manual_executions: take(&mut m, "saveManualExecutions"),
            save_data_error_execution: take(&mut m, "saveDataErrorExecution"),
            save_data_success_execution: take(&mut m, "saveDataSuccessExecution"),
            caller_policy: take(&mut m, "callerPolicy"),
            extra: m,
        })
    }
}

impl Serialize for Settings {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut o = Out::default();
        let r = (|| {
            o.put("executionOrder", &self.execution_order)?;
            o.put("timezone", &self.timezone)?;
            o.put("errorWorkflow", &self.error_workflow)?;
            o.put("executionTimeout", &self.execution_timeout)?;
            o.put("saveManualExecutions", &self.save_manual_executions)?;
            o.put("saveDataErrorExecution", &self.save_data_error_execution)?;
            o.put(
                "saveDataSuccessExecution",
                &self.save_data_success_execution,
            )?;
            o.put("callerPolicy", &self.caller_policy)
        })();
        r.map_err(S::Error::custom)?;
        o.finish(&self.extra).serialize(s)
    }
}

// ---------------------------------------------------------------- pinData

/// `{ "<node name>": [ {json: {...}} | {...} ] }`. Raw values: both historical shapes
/// survive export; use [`Workflow::pinned_items`] for normalized items.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PinData(pub IndexMap<String, Vec<Value>>);

// ------------------------------------------------------------------ node

/// `onError` setting values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorMode {
    StopWorkflow,
    ContinueRegularOutput,
    ContinueErrorOutput,
    #[serde(untagged)]
    Other(String),
}

/// Credential reference on a node. Holds ids/names only, never secret values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CredentialRef {
    /// `null` in exports of unsaved credentials; that null lands in `extra`.
    pub id: Option<String>,
    pub name: Option<String>,
    pub extra: Map<String, Value>,
}

impl<'de> Deserialize<'de> for CredentialRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = into_object(Value::deserialize(d)?, "credential ref")?;
        Ok(Self {
            id: take(&mut m, "id"),
            name: take(&mut m, "name"),
            extra: m,
        })
    }
}

impl Serialize for CredentialRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut o = Out::default();
        let r = (|| {
            o.put("id", &self.id)?;
            o.put("name", &self.name)
        })();
        r.map_err(S::Error::custom)?;
        o.finish(&self.extra).serialize(s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: Option<String>,
    /// Unique within the workflow; connections refer to it.
    pub name: String,
    /// e.g. `n8n-nodes-base.httpRequest`
    pub node_type: String,
    /// Raw number so `1` vs `1.0` survives export.
    pub type_version: Number,
    pub position: [Number; 2],
    pub parameters: Map<String, Value>,
    /// credential type name -> ref
    pub credentials: Option<IndexMap<String, CredentialRef>>,
    pub disabled: Option<bool>,
    pub notes: Option<String>,
    pub notes_in_flow: Option<bool>,
    pub retry_on_fail: Option<bool>,
    pub max_tries: Option<u32>,
    pub wait_between_tries: Option<u64>,
    pub always_output_data: Option<bool>,
    pub execute_once: Option<bool>,
    /// Legacy; superseded by `onError`.
    pub continue_on_fail: Option<bool>,
    pub on_error: Option<ErrorMode>,
    pub webhook_id: Option<String>,
    pub color: Option<String>,
    pub extra: Map<String, Value>,
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = into_object(Value::deserialize(d)?, "node")?;
        let err = D::Error::custom;
        Ok(Self {
            id: take(&mut m, "id"),
            name: take_required(&mut m, "name").map_err(err)?,
            node_type: take_required(&mut m, "type").map_err(err)?,
            type_version: take_required(&mut m, "typeVersion").map_err(err)?,
            position: take_required(&mut m, "position").map_err(err)?,
            parameters: take(&mut m, "parameters").unwrap_or_default(),
            credentials: take(&mut m, "credentials"),
            disabled: take(&mut m, "disabled"),
            notes: take(&mut m, "notes"),
            notes_in_flow: take(&mut m, "notesInFlow"),
            retry_on_fail: take(&mut m, "retryOnFail"),
            max_tries: take(&mut m, "maxTries"),
            wait_between_tries: take(&mut m, "waitBetweenTries"),
            always_output_data: take(&mut m, "alwaysOutputData"),
            execute_once: take(&mut m, "executeOnce"),
            continue_on_fail: take(&mut m, "continueOnFail"),
            on_error: take(&mut m, "onError"),
            webhook_id: take(&mut m, "webhookId"),
            color: take(&mut m, "color"),
            extra: m,
        })
    }
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut o = Out::default();
        let r = (|| {
            o.put("id", &self.id)?;
            o.put_req("name", &self.name)?;
            o.put_req("type", &self.node_type)?;
            o.put_req("typeVersion", &self.type_version)?;
            o.put_req("position", &self.position)?;
            o.put_req("parameters", &self.parameters)?;
            o.put("credentials", &self.credentials)?;
            o.put("disabled", &self.disabled)?;
            o.put("notes", &self.notes)?;
            o.put("notesInFlow", &self.notes_in_flow)?;
            o.put("retryOnFail", &self.retry_on_fail)?;
            o.put("maxTries", &self.max_tries)?;
            o.put("waitBetweenTries", &self.wait_between_tries)?;
            o.put("alwaysOutputData", &self.always_output_data)?;
            o.put("executeOnce", &self.execute_once)?;
            o.put("continueOnFail", &self.continue_on_fail)?;
            o.put("onError", &self.on_error)?;
            o.put("webhookId", &self.webhook_id)?;
            o.put("color", &self.color)
        })();
        r.map_err(S::Error::custom)?;
        o.finish(&self.extra).serialize(s)
    }
}

impl Node {
    pub fn new(
        name: impl Into<String>,
        node_type: impl Into<String>,
        type_version: Number,
        position: [Number; 2],
    ) -> Self {
        Self {
            id: None,
            name: name.into(),
            node_type: node_type.into(),
            type_version,
            position,
            parameters: Map::new(),
            credentials: None,
            disabled: None,
            notes: None,
            notes_in_flow: None,
            retry_on_fail: None,
            max_tries: None,
            wait_between_tries: None,
            always_output_data: None,
            execute_once: None,
            continue_on_fail: None,
            on_error: None,
            webhook_id: None,
            color: None,
            extra: Map::new(),
        }
    }

    pub fn version(&self) -> Version {
        // JSON numbers are finite by construction
        Version::new(self.type_version.as_f64().unwrap_or(1.0))
            .unwrap_or_else(|| Version::new(1.0).expect("1.0 is finite"))
    }

    /// Registry key for this node's implementation.
    pub fn key(&self) -> NodeKey {
        NodeKey::new(self.node_type.clone(), self.version())
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled.unwrap_or(false)
    }

    /// Effective error mode, folding the legacy `continueOnFail` flag in.
    pub fn error_mode(&self) -> ErrorMode {
        match (&self.on_error, self.continue_on_fail) {
            (Some(m), _) => m.clone(),
            (None, Some(true)) => ErrorMode::ContinueRegularOutput,
            _ => ErrorMode::StopWorkflow,
        }
    }

    pub fn x(&self) -> f64 {
        self.position[0].as_f64().unwrap_or(0.0)
    }

    pub fn y(&self) -> f64 {
        self.position[1].as_f64().unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rt(v: Value) -> Value {
        let w: Workflow = serde_json::from_value(v).unwrap();
        serde_json::to_value(&w).unwrap()
    }

    #[test]
    fn unknown_and_null_fields_survive() {
        let v = json!({
            "name": "x", "nodes": [], "connections": {},
            "staticData": null, "id": null, "futureField": {"a": [1, 2.0]},
            "meta": null
        });
        assert_eq!(rt(v.clone()), v);
    }

    #[test]
    fn mistyped_known_field_kept_verbatim() {
        let node = json!({
            "name": "n", "type": "t", "typeVersion": 1.0, "position": [1.5, 2],
            "parameters": {}, "maxTries": "3", "credentials": {"a": {"id": null, "name": "c"}}
        });
        let v = json!({"nodes": [node], "connections": {}});
        assert_eq!(rt(v.clone()), v);
    }

    #[test]
    fn missing_node_name_is_error() {
        let v = json!({"nodes": [{"type": "t", "typeVersion": 1, "position": [0, 0]}], "connections": {}});
        assert!(serde_json::from_value::<Workflow>(v).is_err());
    }

    #[test]
    fn graph_lookups() {
        let w: Workflow = serde_json::from_value(json!({
            "nodes": [
                {"name": "A", "type": "t", "typeVersion": 1, "position": [0, 0], "parameters": {}},
                {"name": "B", "type": "t", "typeVersion": 1, "position": [0, 0], "parameters": {}}
            ],
            "connections": {"A": {"main": [[], [{"node": "B", "type": "main", "index": 1}]]}}
        }))
        .unwrap();
        assert!(w.destinations("A", MAIN, 0).is_empty());
        assert_eq!(w.destinations("A", MAIN, 1)[0].node, "B");
        let inc = w.incoming("B");
        assert_eq!((inc[0].source, inc[0].output, inc[0].input), ("A", 1, 1));
        w.validate().unwrap();
    }

    #[test]
    fn validate_rejects_dangling() {
        let w: Workflow = serde_json::from_value(json!({
            "nodes": [{"name": "A", "type": "t", "typeVersion": 1, "position": [0, 0], "parameters": {}}],
            "connections": {"A": {"main": [[{"node": "Z", "type": "main", "index": 0}]]}}
        }))
        .unwrap();
        assert!(w.validate().is_err());
    }

    #[test]
    fn error_mode_folds_legacy_flag() {
        let mut n = Node::new("n", "t", 1.into(), [0.into(), 0.into()]);
        assert_eq!(n.error_mode(), ErrorMode::StopWorkflow);
        n.continue_on_fail = Some(true);
        assert_eq!(n.error_mode(), ErrorMode::ContinueRegularOutput);
        n.on_error = Some(ErrorMode::ContinueErrorOutput);
        assert_eq!(n.error_mode(), ErrorMode::ContinueErrorOutput);
    }
}
