//! Ollama node (`hokkabaz.ollama`): generate, chat, embeddings, list models, pull a model.
//!
//! Talks to the local Ollama HTTP API (default `http://localhost:11434`) without streaming:
//! `stream: false` for generate/chat, and for pull the NDJSON progress stream is read to the
//! end with only the final status (or the first error line) kept.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit::{self, node_err};
use crate::template::display;

pub const NODE_TYPE: &str = "hokkabaz.ollama";
pub const CREDENTIAL: &str = "ollamaApi";
const DEFAULT_URL: &str = "http://localhost:11434";
/// Generation and model pulls are slow; the request timeout is generous.
const TIMEOUT: Duration = Duration::from_secs(600);
const ERROR_PATHS: &[&str] = &["error"];

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Ollama")
        .documentation("https://github.com/ollama/ollama/blob/main/docs/api.md")
        .property(Prop::string("baseUrl", "Base URL").default(DEFAULT_URL))
        .property(Prop::string("apiKey", "API Key").password().description("Optional bearer token, for instances behind an authenticating proxy"))
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "Ollama")
        .description("Run local models and manage them through Ollama")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:robot")
        .color("#000000")
        .credential(CREDENTIAL, true)
        .property(Prop::options(
            "operation",
            "Operation",
            vec![
                Choice::new("Generate", "generate").action("Generate text from a prompt"),
                Choice::new("Chat", "chat").action("Chat with a model"),
                Choice::new("Embeddings", "embeddings").action("Create embeddings for text"),
                Choice::new("List Models", "listModels").action("List the installed models"),
                Choice::new("Pull Model", "pullModel").action("Download a model"),
            ],
        ))
        .property(Prop::string("model", "Model").required().default("llama3.2").show("operation", &["generate", "chat", "embeddings", "pullModel"]))
        .property(Prop::string("prompt", "Prompt").required().type_option("rows", 4).show("operation", &["generate", "chat"]))
        .property(Prop::string("system", "System Message").type_option("rows", 2).show("operation", &["generate", "chat"]))
        .property(
            Prop::json("messages", "History")
                .default(json!([]))
                .description("Earlier messages as `[{role, content}]`, placed before the prompt")
                .show("operation", &["chat"]),
        )
        .property(Prop::string("input", "Input").required().description("Text to embed").show("operation", &["embeddings"]))
        .property(Prop::collection(
            "options",
            "Options",
            vec![
                Prop::number("temperature", "Temperature"),
                Prop::number("num_predict", "Max Tokens"),
                Prop::number("num_ctx", "Context Length"),
                Prop::number("top_p", "Top P"),
                Prop::number("seed", "Seed"),
                Prop::boolean("json", "JSON Output").description("Constrain the reply to valid JSON"),
                Prop::string("keep_alive", "Keep Alive").placeholder("5m"),
            ],
        ))
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

#[derive(Debug)]
struct OllamaNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

struct Api {
    http: reqwest::Client,
    base: String,
    key: String,
}

impl Api {
    fn rb(&self, node: &str, method: &str, path: &str) -> Result<reqwest::RequestBuilder, FlowError> {
        let url = kit::join_url(node, &self.base, &[ "api".into(), path.into()])?;
        let rb = if method == "GET" { self.http.get(url) } else { self.http.post(url) };
        Ok(if self.key.is_empty() { rb } else { rb.bearer_auth(&self.key) })
    }
}

impl OllamaNode {
    /// Sampling options plus `format`/`keep_alive`, merged into a request body.
    fn apply_options(&self, ctx: &NodeContext, i: usize, body: &mut Value) -> Result<(), FlowError> {
        let mut opts = Map::new();
        for k in ["temperature", "num_predict", "num_ctx", "top_p", "seed"] {
            if let Some(v) = kit::field(ctx, &format!("options.{k}"), i)? {
                let n = v.as_f64().or_else(|| display(&v).parse().ok());
                if let Some(n) = n {
                    // integral settings must stay integers for the Go side
                    opts.insert(k.into(), if n.fract() == 0.0 && k != "temperature" && k != "top_p" { json!(n as i64) } else { json!(n) });
                }
            }
        }
        if !opts.is_empty() {
            body["options"] = Value::Object(opts);
        }
        if matches!(kit::field(ctx, "options.json", i)?, Some(Value::Bool(true))) {
            body["format"] = "json".into();
        }
        if let Some(k) = kit::field_str(ctx, "options.keep_alive", i)? {
            body["keep_alive"] = k.into();
        }
        Ok(())
    }

    fn model(&self, ctx: &NodeContext, i: usize) -> Result<String, FlowError> {
        let m = kit::str_param(ctx, &self.desc, "model", i)?;
        if m.is_empty() {
            return Err(node_err(ctx, "Parameter 'Model' is required"));
        }
        Ok(m)
    }

    async fn run_item(&self, ctx: &NodeContext, api: &Api, i: usize) -> Result<Vec<Item>, FlowError> {
        let node = &ctx.node.name;
        match kit::str_param(ctx, &self.desc, "operation", i)?.as_str() {
            "generate" => {
                let mut body = json!({"model": self.model(ctx, i)?, "prompt": kit::str_param(ctx, &self.desc, "prompt", i)?, "stream": false});
                let system = kit::str_param(ctx, &self.desc, "system", i)?;
                if !system.is_empty() {
                    body["system"] = system.into();
                }
                self.apply_options(ctx, i, &mut body)?;
                let mut res = kit::call(node, kit::json_body(api.rb(node, "POST", "generate")?, &body), ERROR_PATHS).await?;
                // the token context array is large and useless outside a follow-up request
                if let Value::Object(o) = &mut res {
                    o.remove("context");
                }
                Ok(vec![kit::to_item(res)])
            }
            "chat" => {
                let mut messages: Vec<Value> = Vec::new();
                let system = kit::str_param(ctx, &self.desc, "system", i)?;
                if !system.is_empty() {
                    messages.push(json!({"role": "system", "content": system}));
                }
                let history = match kit::param(ctx, &self.desc, "messages", i)? {
                    Some(Value::String(s)) if !s.trim().is_empty() => serde_json::from_str(&s).map_err(|_| node_err(ctx, "History is not valid JSON"))?,
                    Some(v) => v,
                    None => Value::Null,
                };
                if let Value::Array(h) = history {
                    messages.extend(h);
                }
                messages.push(json!({"role": "user", "content": kit::str_param(ctx, &self.desc, "prompt", i)?}));
                let mut body = json!({"model": self.model(ctx, i)?, "messages": messages, "stream": false});
                self.apply_options(ctx, i, &mut body)?;
                let res = kit::call(node, kit::json_body(api.rb(node, "POST", "chat")?, &body), ERROR_PATHS).await?;
                Ok(vec![kit::to_item(res)])
            }
            "embeddings" => {
                let body = json!({"model": self.model(ctx, i)?, "input": kit::str_param(ctx, &self.desc, "input", i)?});
                let res = kit::call(node, kit::json_body(api.rb(node, "POST", "embed")?, &body), ERROR_PATHS).await?;
                Ok(vec![kit::to_item(res)])
            }
            "listModels" => {
                let res = kit::call(node, api.rb(node, "GET", "tags")?, ERROR_PATHS).await?;
                let models = res.get("models").and_then(Value::as_array).cloned().unwrap_or_default();
                Ok(models.into_iter().map(kit::to_item).collect())
            }
            "pullModel" => {
                let model = self.model(ctx, i)?;
                let body = json!({"model": model, "stream": true});
                let res = api
                    .rb(node, "POST", "pull")?
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .send()
                    .await
                    .map_err(|e| kit::err(node, format!("The request failed: {}", e.without_url())))?;
                let status = res.status();
                // progress lines are consumed and dropped; only the last status or an error matters
                let text = res.text().await.map_err(|e| kit::err(node, format!("Could not read the response: {}", e.without_url())))?;
                let lines: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
                if let Some(err) = lines.iter().find_map(|l| l.get("error").map(display)) {
                    return Err(kit::err(node, format!("Ollama: {}", err.chars().take(300).collect::<String>())));
                }
                if !status.is_success() {
                    return Err(kit::err(node, format!("Ollama returned {}", status.as_u16())));
                }
                let last = lines.last().and_then(|l| l.get("status")).map(display).unwrap_or_default();
                Ok(vec![Item::from_json(json!({"success": last == "success", "model": model, "status": last}))])
            }
            other => Err(node_err(ctx, format!("Unsupported operation `{other}`"))),
        }
    }
}

#[async_trait]
impl FlowNode for OllamaNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let base = kit::cred_str(&cred, "baseUrl");
        let api = Api {
            http: kit::client(&ctx.node.name, TIMEOUT)?,
            base: if base.trim().is_empty() { DEFAULT_URL.to_owned() } else { base },
            key: kit::cred_str(&cred, "apiKey"),
        };
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = self.run_item(ctx, &api, i).await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-ollama`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(OllamaNode { desc: description(), creds: creds.clone() }))
}
