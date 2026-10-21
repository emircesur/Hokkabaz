//! Node type description schema (n8n `INodeTypeDescription` shape).
//!
//! Descriptions drive the params forms on the canvas, credential pickers and port
//! layout. They are generated from n8n node description JSON, so unmodeled keys are
//! kept in `extra` and survive.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::version::{NodeKey, NodeVersions, Version};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeTypeDescription {
    pub display_name: String,
    /// Type name without package prefix is NOT used; this is the full `type` string
    /// as it appears in workflows (`n8n-nodes-base.if`).
    pub name: String,
    pub version: NodeVersions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_version: Option<Version>,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// `trigger`, `input`, `output`, `transform`, `organization`, ...
    #[serde(default)]
    pub group: Vec<String>,
    /// string, or `{light, dark}`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<Value>,
    #[serde(default)]
    pub defaults: NodeDefaults,
    #[serde(default = "main_io")]
    pub inputs: IoDecl,
    #[serde(default = "main_io")]
    pub outputs: IoDecl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_names: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<CredentialDescription>,
    #[serde(default)]
    pub properties: Vec<NodeProperty>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhooks: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usable_as_tool: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn main_io() -> IoDecl {
    IoDecl::List(vec![IoSpec::Simple("main".into())])
}

impl NodeTypeDescription {
    pub fn key(&self, version: Version) -> NodeKey {
        NodeKey::new(self.name.clone(), version)
    }

    pub fn latest_version(&self) -> Option<Version> {
        self.default_version.or_else(|| self.version.latest())
    }

    pub fn is_trigger(&self) -> bool {
        self.group.iter().any(|g| g == "trigger")
    }

    pub fn property(&self, name: &str) -> Option<&NodeProperty> {
        self.properties.iter().find(|p| p.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeDefaults {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Inputs/outputs: a static list, or a `={{ ... }}` expression resolved against the
/// node parameters (e.g. Switch output count).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IoDecl {
    Expression(String),
    List(Vec<IoSpec>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IoSpec {
    /// `"main"`, `"ai_languageModel"`, ...
    Simple(String),
    Detailed(IoDetail),
}

impl IoSpec {
    pub fn conn_type(&self) -> &str {
        match self {
            Self::Simple(s) => s,
            Self::Detailed(d) => &d.io_type,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IoDetail {
    #[serde(rename = "type")]
    pub io_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u32>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialDescription {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_options: Option<DisplayOptions>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `show`/`hide`: parameter name -> list of values (or `_cnd` condition objects).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DisplayOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<IndexMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide: Option<IndexMap<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeProperty {
    pub display_name: String,
    pub name: String,
    #[serde(rename = "type")]
    pub prop_type: PropertyType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_data_expression: Option<bool>,
    /// Choices for options/multiOptions, nested properties for collection/fixedCollection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_options: Option<DisplayOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_options: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Parameter widget kinds. Unknown kinds (new n8n releases, community nodes) land in `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PropertyType {
    String,
    Number,
    Boolean,
    Options,
    MultiOptions,
    Collection,
    FixedCollection,
    Json,
    DateTime,
    Color,
    Hidden,
    Notice,
    Button,
    CredentialsSelect,
    ResourceLocator,
    ResourceMapper,
    Filter,
    AssignmentCollection,
    WorkflowSelector,
    CurlImport,
    #[serde(untagged)]
    Other(String),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn description_parses_and_keeps_unknown() {
        let raw = json!({
            "displayName": "If", "name": "n8n-nodes-base.if",
            "version": [1, 2, 2.1, 2.2], "defaultVersion": 2.2,
            "group": ["transform"], "description": "Route items",
            "defaults": {"name": "If", "color": "#408000"},
            "inputs": ["main"],
            "outputs": ["main", {"type": "main", "displayName": "False", "maxConnections": 1}],
            "outputNames": ["true", "false"],
            "credentials": [{"name": "httpBasicAuth", "required": true, "displayOptions": {"show": {"auth": ["basic"]}}}],
            "properties": [
                {"displayName": "Conditions", "name": "conditions", "type": "filter", "default": {}, "typeOptions": {"filter": {}}},
                {"displayName": "Mode", "name": "mode", "type": "futureWidget", "default": "a", "options": [{"name": "A", "value": "a"}], "somethingNew": 1}
            ],
            "iconColor": "green"
        });
        let d: NodeTypeDescription = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(d.latest_version().unwrap().get(), 2.2);
        assert_eq!(d.properties[0].prop_type, PropertyType::Filter);
        assert_eq!(
            d.properties[1].prop_type,
            PropertyType::Other("futureWidget".into())
        );
        assert_eq!(
            d.outputs,
            IoDecl::List(vec![
                IoSpec::Simple("main".into()),
                IoSpec::Detailed(IoDetail {
                    io_type: "main".into(),
                    display_name: Some("False".into()),
                    required: None,
                    max_connections: Some(1),
                    extra: Map::new(),
                }),
            ])
        );
        assert_eq!(serde_json::to_value(&d).unwrap(), raw);
    }

    #[test]
    fn io_expression() {
        let d: IoDecl =
            serde_json::from_value(json!("={{ $parameter.rules.values.length }}")).unwrap();
        assert!(matches!(d, IoDecl::Expression(_)));
    }
}
