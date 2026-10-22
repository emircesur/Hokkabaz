use hk_plugin_sdk::host::{self, Level};
use hk_plugin_sdk::{Description, NodeInput, NodeOutput, Plugin, PollOutput, Risk, ToolDecl, Value, export_plugin, json};

struct Hello;

impl Plugin for Hello {
    fn describe() -> Description {
        Description::new()
            .tool(ToolDecl::new(
                "hello_greet",
                "Greet someone by name.",
                json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}),
            ))
            .tool(
                ToolDecl::new(
                    "hello_fetch",
                    "Fetch a URL (only allowed hosts).",
                    json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}),
                )
                .risk(Risk::Ask),
            )
            .node(json!({
                "displayName": "Hello",
                "name": "hello.greet",
                "group": ["transform"],
                "version": 1,
                "description": "Adds a greeting to each item",
                "defaults": {"name": "Hello"},
                "inputs": ["main"],
                "outputs": ["main"],
                "properties": [
                    {"displayName": "Name", "name": "name", "type": "string", "default": "world"}
                ]
            }))
            .trigger("hello_tick", 60)
    }

    fn call_tool(name: &str, args: Value) -> Result<String, String> {
        match name {
            "hello_greet" => {
                let who = args["name"].as_str().ok_or("`name` is required")?;
                host::log(Level::Info, &format!("greeting {who}"));
                Ok(format!("Hello, {who}!"))
            }
            "hello_fetch" => {
                let url = args["url"].as_str().ok_or("`url` is required")?;
                let r = host::get(url)?;
                Ok(format!("{} {} bytes", r.status, r.body.len()))
            }
            _ => Err(format!("unknown tool `{name}`")),
        }
    }

    fn execute_node(node_type: &str, input: NodeInput) -> Result<NodeOutput, String> {
        if node_type != "hello.greet" {
            return Err(format!("unknown node `{node_type}`"));
        }
        let name = input.params["name"].as_str().unwrap_or("world");
        let items = input
            .items
            .into_iter()
            .map(|mut it| {
                it["json"]["greeting"] = json!(format!("Hello, {name}!"));
                it
            })
            .collect();
        Ok(NodeOutput::one(items))
    }

    fn poll_trigger(name: &str, state: Value) -> Result<PollOutput, String> {
        if name != "hello_tick" {
            return Err(format!("unknown trigger `{name}`"));
        }
        let n = state["n"].as_u64().unwrap_or(0) + 1;
        host::kv_set("last_tick", &n.to_string())?;
        Ok(PollOutput { events: vec![json!({"tick": n})], state: json!({"n": n}) })
    }
}

export_plugin!(Hello);
