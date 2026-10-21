//! n8n items: `{ json, binary, pairedItem }`.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One unit of data flowing between nodes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Item {
    #[serde(default)]
    pub json: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<IndexMap<String, BinaryData>>,
    #[serde(
        default,
        rename = "pairedItem",
        skip_serializing_if = "Option::is_none"
    )]
    pub paired_item: Option<PairedItem>,
    /// Per-item error (continue-on-fail output); shape is node specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Item {
    pub fn new(json: Map<String, Value>) -> Self {
        Self {
            json,
            ..Self::default()
        }
    }

    /// Build from a `json!({...})` literal; non-objects become `{}`.
    pub fn from_json(v: Value) -> Self {
        match v {
            Value::Object(m) => Self::new(m),
            _ => Self::default(),
        }
    }

    /// Pinned data comes in two shapes: `{"json": {...}}` (current) and bare
    /// objects (older exports). Normalize both.
    pub fn from_pinned(v: &Value) -> Self {
        if let Value::Object(m) = v
            && matches!(m.get("json"), Some(Value::Object(_)))
            && let Ok(item) = serde_json::from_value::<Item>(v.clone())
        {
            return item;
        }
        match v {
            Value::Object(m) => Self::new(m.clone()),
            _ => Self::default(),
        }
    }

    pub fn with_paired(mut self, p: PairedItem) -> Self {
        self.paired_item = Some(p);
        self
    }
}

/// Binary payload descriptor. `data` is base64 or a storage-mode marker
/// (`filesystem-v2`); the engine resolves it via a binary store, never inline in run data.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BinaryData {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// Human string in n8n (`"1.2 kB"`), so kept as raw JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Link from an output item back to the input item(s) it was derived from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PairedItem {
    Index(usize),
    Source(PairedSource),
    Many(Vec<PairedSource>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairedSource {
    pub item: usize,
    /// Input port index; absent means 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<usize>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn paired_item_shapes() {
        for raw in [
            json!(3),
            json!({"item": 1}),
            json!({"item": 1, "input": 2}),
            json!([{"item": 0}, {"item": 4, "input": 1}]),
        ] {
            let p: PairedItem = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(serde_json::to_value(&p).unwrap(), raw);
        }
    }

    #[test]
    fn item_roundtrip_keeps_unknown() {
        let raw = json!({
            "json": {"a": 1},
            "binary": {"data": {"data": "aGk=", "mimeType": "text/plain", "fileName": "a.txt", "fileSize": "2 B", "x": 1}},
            "pairedItem": {"item": 0},
            "evaluateExpression": true
        });
        let it: Item = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&it).unwrap(), raw);
    }

    #[test]
    fn pinned_both_shapes() {
        let a = Item::from_pinned(&json!({"json": {"k": 1}}));
        let b = Item::from_pinned(&json!({"k": 1}));
        assert_eq!(a, b);
        // a bare object that merely has a `json` scalar key stays bare
        let c = Item::from_pinned(&json!({"json": "x"}));
        assert_eq!(c.json.get("json"), Some(&json!("x")));
    }
}
