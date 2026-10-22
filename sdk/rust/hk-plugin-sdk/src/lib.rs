//! SDK for Hokkabaz plugins.
//!
//! ```ignore
//! use hk_plugin_sdk::{export_plugin, Description, Plugin, ToolDecl};
//!
//! struct Hello;
//! impl Plugin for Hello {
//!     fn describe() -> Description {
//!         Description::new().tool(ToolDecl::new("hello", "Greets", serde_json::json!({"type":"object"})))
//!     }
//!     fn call_tool(name: &str, args: serde_json::Value) -> Result<String, String> {
//!         Ok(format!("hello {args}"))
//!     }
//! }
//! export_plugin!(Hello);
//! ```

pub use serde_json::{self, Value, json};

#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../../../wit/hokkabaz-plugin.wit",
        world: "plugin",
        pub_export_macro: true,
        export_macro_name: "__export_hk_plugin",
        default_bindings_module: "hk_plugin_sdk::bindings",
    });
}

use serde::{Deserialize, Serialize};

/// Risk class of a tool (decides whether the user is asked before it runs).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Safe,
    Ask,
    Dangerous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub risk: Risk,
}

impl ToolDecl {
    pub fn new(name: &str, description: &str, parameters: Value) -> Self {
        Self { name: name.into(), description: description.into(), parameters, risk: Risk::Safe }
    }

    pub fn risk(mut self, r: Risk) -> Self {
        self.risk = r;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerDecl {
    pub name: String,
    pub interval_secs: u64,
}

/// What `describe` returns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Description {
    pub tools: Vec<ToolDecl>,
    /// n8n node type descriptions (JSON), e.g. built with `json!`.
    pub nodes: Vec<Value>,
    pub triggers: Vec<TriggerDecl>,
    pub settings: Option<Value>,
}

impl Description {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tool(mut self, t: ToolDecl) -> Self {
        self.tools.push(t);
        self
    }

    pub fn node(mut self, n: Value) -> Self {
        self.nodes.push(n);
        self
    }

    pub fn trigger(mut self, name: &str, interval_secs: u64) -> Self {
        self.triggers.push(TriggerDecl { name: name.into(), interval_secs });
        self
    }

    pub fn settings(mut self, schema: Value) -> Self {
        self.settings = Some(schema);
        self
    }
}

/// Input of `execute_node`.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeInput {
    #[serde(default)]
    pub params: Value,
    /// Input items (`[{"json": {...}}]`).
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub settings: Value,
}

/// Output of `execute_node`: one list of items per output port.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NodeOutput {
    pub items: Vec<Vec<Value>>,
}

impl NodeOutput {
    /// A single output carrying `items`.
    pub fn one(items: Vec<Value>) -> Self {
        Self { items: vec![items] }
    }
}

/// Result of `poll_trigger`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PollOutput {
    pub events: Vec<Value>,
    pub state: Value,
}

/// Implement the parts you offer; the defaults say "not supported".
pub trait Plugin {
    fn describe() -> Description;

    fn call_tool(name: &str, _args: Value) -> Result<String, String> {
        Err(format!("unknown tool `{name}`"))
    }

    fn execute_node(node_type: &str, _input: NodeInput) -> Result<NodeOutput, String> {
        Err(format!("unknown node `{node_type}`"))
    }

    fn poll_trigger(name: &str, _state: Value) -> Result<PollOutput, String> {
        Err(format!("unknown trigger `{name}`"))
    }
}

/// Services of the host. Each call needs the matching capability in `plugin.toml`.
pub mod host {
    use super::bindings::hokkabaz::plugin::host as h;
    use serde_json::{Value, json};

    pub enum Level {
        Trace = 0,
        Debug = 1,
        Info = 2,
        Warn = 3,
        Error = 4,
    }

    pub fn log(level: Level, message: &str) {
        h::log(level as u8, message);
    }

    pub struct Response {
        pub status: u16,
        pub headers: Value,
        pub body: String,
    }

    pub fn http(method: &str, url: &str, headers: Value, body: Option<&str>) -> Result<Response, String> {
        let req = json!({"method": method, "url": url, "headers": headers, "body": body});
        let raw = h::http(&req.to_string())?;
        let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        Ok(Response {
            status: v["status"].as_u64().unwrap_or(0) as u16,
            headers: v["headers"].clone(),
            body: v["body"].as_str().unwrap_or_default().to_string(),
        })
    }

    pub fn get(url: &str) -> Result<Response, String> {
        http("GET", url, json!({}), None)
    }

    pub fn kv_get(key: &str) -> Result<Option<String>, String> {
        h::kv_get(key)
    }

    pub fn kv_set(key: &str, value: &str) -> Result<(), String> {
        h::kv_set(key, value)
    }

    pub fn kv_delete(key: &str) -> Result<(), String> {
        h::kv_delete(key)
    }

    pub fn secret(name: &str) -> Result<String, String> {
        h::secret(name)
    }

    pub fn emit(trigger: &str, event: &Value) -> Result<(), String> {
        h::emit(trigger, &event.to_string())
    }
}

#[doc(hidden)]
pub fn __describe<P: Plugin>() -> String {
    serde_json::to_string(&P::describe()).unwrap_or_else(|_| "{}".into())
}

#[doc(hidden)]
pub fn __node<P: Plugin>(node_type: &str, input: &str) -> Result<String, String> {
    let input: NodeInput = serde_json::from_str(input).map_err(|e| format!("bad node input: {e}"))?;
    let out = P::execute_node(node_type, input)?;
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

#[doc(hidden)]
pub fn __poll<P: Plugin>(name: &str, state: &str) -> Result<String, String> {
    let state: Value = serde_json::from_str(state).unwrap_or(Value::Null);
    let out = P::poll_trigger(name, state)?;
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

#[doc(hidden)]
pub fn __tool<P: Plugin>(name: &str, args: &str) -> Result<String, String> {
    let args: Value = serde_json::from_str(args).map_err(|e| format!("bad tool arguments: {e}"))?;
    P::call_tool(name, args)
}

/// Export `$ty: Plugin` as the component's implementation.
#[macro_export]
macro_rules! export_plugin {
    ($ty:ty) => {
        struct __HkAdapter;

        impl $crate::bindings::exports::hokkabaz::plugin::guest::Guest for __HkAdapter {
            fn describe() -> String {
                $crate::__describe::<$ty>()
            }

            fn call_tool(name: String, args: String) -> Result<String, String> {
                $crate::__tool::<$ty>(&name, &args)
            }

            fn execute_node(node_type: String, input: String) -> Result<String, String> {
                $crate::__node::<$ty>(&node_type, &input)
            }

            fn poll_trigger(name: String, state: String) -> Result<String, String> {
                $crate::__poll::<$ty>(&name, &state)
            }
        }

        $crate::bindings::__export_hk_plugin!(__HkAdapter with_types_in $crate::bindings);
    };
}
