//! AI Agent (`agent`, tools agent) and the native `hokkabaz.agent`.
//!
//! The n8n node runs a lean tool-calling loop over the connected chat model: ask, run the
//! requested `ai_tool`s, feed observations back, repeat until the model answers without a
//! tool call. `hokkabaz.agent` hands the whole turn to the host's hk-agent loop
//! ([`AgentRunner`]) so it gets the real tool pipeline, modes and approvals; with no runner
//! wired it falls back to the same lean loop over its sub-nodes.

use std::sync::Arc;

use async_trait::async_trait;
use hk_llm::{Message, Role, ToolSpec};
use serde_json::{Map, Value, json};

use super::types::{
    AgentRunRequest, AgentRunner, AgentTool, ChatModel, Memory, OutputParser, ai_err,
};
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput, Supplied};
use crate::nodes::util::*;

pub(crate) const LANGUAGE_MODEL: &str = "ai_languageModel";
pub(crate) const MEMORY: &str = "ai_memory";
pub(crate) const TOOL: &str = "ai_tool";
pub(crate) const OUTPUT_PARSER: &str = "ai_outputParser";

fn handle<T: Clone + 'static>(s: &Supplied) -> Option<T> {
    s.get::<T>().cloned()
}

/// Sub-nodes of one root node for one item.
pub(crate) struct SubInputs {
    pub model: Option<Arc<dyn ChatModel>>,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub memory: Option<Arc<dyn Memory>>,
    pub parser: Option<Arc<dyn OutputParser>>,
}

impl SubInputs {
    pub async fn resolve(ctx: &NodeContext, i: usize) -> Result<Self, FlowError> {
        let model = ctx
            .sub_node(LANGUAGE_MODEL, i)
            .await?
            .and_then(|s| handle::<Arc<dyn ChatModel>>(&s));
        let tools = ctx
            .sub_nodes(TOOL, i)
            .await?
            .iter()
            .filter_map(handle::<Arc<dyn AgentTool>>)
            .collect();
        let memory = ctx
            .sub_node(MEMORY, i)
            .await?
            .and_then(|s| handle::<Arc<dyn Memory>>(&s));
        let parser = ctx
            .sub_node(OUTPUT_PARSER, i)
            .await?
            .and_then(|s| handle::<Arc<dyn OutputParser>>(&s));
        Ok(Self {
            model,
            tools,
            memory,
            parser,
        })
    }

    pub fn require_model(&self, ctx: &NodeContext) -> Result<Arc<dyn ChatModel>, FlowError> {
        self.model.clone().ok_or_else(|| {
            node_err(
                ctx,
                "A Chat Model is required: connect one to the `ai_languageModel` input",
            )
        })
    }
}

pub(crate) struct AgentOutcome {
    /// Final answer: text, or the parsed value when an output parser is connected.
    pub output: Value,
    pub steps: Vec<Value>,
}

pub(crate) struct LoopSpec {
    pub system: Option<String>,
    pub prompt: String,
    pub max_iterations: usize,
}

/// Tool-calling loop. Tool failures go back to the model as observations.
pub(crate) async fn run_loop(
    model: &Arc<dyn ChatModel>,
    inputs: &SubInputs,
    spec: LoopSpec,
) -> Result<AgentOutcome, FlowError> {
    let mut by_name: Vec<(&str, &Arc<dyn AgentTool>)> = Vec::new();
    for t in &inputs.tools {
        if by_name.iter().any(|(n, _)| *n == t.name()) {
            return Err(ai_err(format!("Two tools are named `{}`", t.name())));
        }
        by_name.push((t.name(), t));
    }
    let specs: Vec<ToolSpec> = inputs
        .tools
        .iter()
        .map(|t| ToolSpec {
            name: t.name().to_owned(),
            description: t.description().to_owned(),
            parameters: t.schema(),
        })
        .collect();

    let mut system = spec.system.unwrap_or_default();
    if let Some(p) = &inputs.parser {
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str(&p.format_instructions());
    }
    let mut msgs = Vec::new();
    if !system.is_empty() {
        msgs.push(msg(Role::System, system));
    }
    if let Some(m) = &inputs.memory {
        msgs.extend(m.load().await);
    }
    msgs.push(msg(Role::User, spec.prompt.clone()));

    let mut steps = Vec::new();
    for _ in 0..spec.max_iterations.max(1) {
        let c = model.complete(msgs.clone(), specs.clone()).await?;
        if c.tool_calls.is_empty() {
            let output = match &inputs.parser {
                Some(p) => p.parse(&c.text)?,
                None => Value::String(c.text.clone()),
            };
            if let Some(m) = &inputs.memory {
                m.save(&spec.prompt, &c.text).await;
            }
            return Ok(AgentOutcome { output, steps });
        }
        let mut assistant = msg(Role::Assistant, c.text.clone());
        assistant.tool_calls = c.tool_calls.clone();
        msgs.push(assistant);
        for call in &c.tool_calls {
            let observation = match by_name.iter().find(|(n, _)| *n == call.name) {
                Some((_, t)) => match t.call(call.arguments.clone()).await {
                    Ok(s) => s,
                    Err(FlowError::Node { message, .. }) => format!("Error: {message}"),
                    Err(e) => format!("Error: {e}"),
                },
                None => format!("Error: there is no tool named `{}`", call.name),
            };
            steps.push(json!({
                "action": {
                    "tool": call.name,
                    "toolInput": call.arguments,
                    "log": c.text,
                    "toolCallId": call.id,
                    "type": "tool_call",
                },
                "observation": observation,
            }));
            let mut tool_msg = msg(Role::Tool, observation);
            tool_msg.tool_call_id = Some(call.id.clone());
            msgs.push(tool_msg);
        }
    }
    Err(ai_err("Agent stopped due to max iterations."))
}

fn msg(role: Role, content: String) -> Message {
    Message {
        images: Vec::new(),
        role,
        content,
        reasoning: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

// ----------------------------------------------------------------- AI Agent node

#[derive(Debug)]
pub struct AgentNode(NodeTypeDescription);

impl Default for AgentNode {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "AI Agent",
            "name": "@n8n/n8n-nodes-langchain.agent",
            "version": [1, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7, 1.8, 1.9, 2, 2.1, 2.2, 3],
            "defaultVersion": 3,
            "description": "Generates an action plan and executes it. Can use external tools.",
            "group": ["transform"],
            "defaults": {"name": "AI Agent", "color": "#404040"},
            "inputs": [
                "main",
                {"type": "ai_languageModel", "displayName": "Chat Model", "required": true, "maxConnections": 1},
                {"type": "ai_memory", "displayName": "Memory", "maxConnections": 1},
                {"type": "ai_tool", "displayName": "Tool"},
                {"type": "ai_outputParser", "displayName": "Output Parser", "maxConnections": 1}
            ],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Source for Prompt (User Message)", "name": "promptType", "type": "options", "default": "auto",
                 "options": [
                    {"name": "Connected Chat Trigger Node", "value": "auto"},
                    {"name": "Define below", "value": "define"}]},
                {"displayName": "Prompt (User Message)", "name": "text", "type": "string", "default": "",
                 "displayOptions": {"show": {"promptType": ["define"]}}},
                {"displayName": "Require Specific Output Format", "name": "hasOutputParser", "type": "boolean", "default": false},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add Option",
                 "options": [
                    {"displayName": "System Message", "name": "systemMessage", "type": "string",
                     "default": "You are a helpful assistant"},
                    {"displayName": "Max Iterations", "name": "maxIterations", "type": "number", "default": 10},
                    {"displayName": "Return Intermediate Steps", "name": "returnIntermediateSteps", "type": "boolean", "default": false}]}
            ]
        })))
    }

    fn prompt(ctx: &NodeContext, item: &Item, i: usize) -> Result<String, FlowError> {
        let defined = match param_str(ctx, "promptType", i, "")?.as_str() {
            "define" => true,
            "auto" => false,
            // < 1.3 has no promptType: an explicit `text` wins
            _ => raw_param(ctx, "text").is_some(),
        };
        if defined {
            return param_str(ctx, "text", i, "");
        }
        ["chatInput", "input"]
            .iter()
            .find_map(|k| item.json.get(*k).map(to_display))
            .ok_or_else(|| {
                node_err(
                    ctx,
                    "No prompt specified: expected a field `chatInput` in the input item",
                )
            })
    }
}

#[async_trait]
impl FlowNode for AgentNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        if let Some(kind) = raw_param(ctx, "agent").and_then(Value::as_str)
            && matches!(kind, "sqlAgent" | "planAndExecuteAgent")
        {
            return Err(node_err(
                ctx,
                format!("Agent type `{kind}` is not supported; use the tools agent"),
            ));
        }
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let inputs = SubInputs::resolve(ctx, i).await?;
            let model = inputs.require_model(ctx)?;
            let prompt = Self::prompt(ctx, item, i)?;
            let system = param_str(
                ctx,
                "options.systemMessage",
                i,
                "You are a helpful assistant",
            )?;
            let max = param_usize(ctx, "options.maxIterations", i, 10)?;
            let r = run_loop(
                &model,
                &inputs,
                LoopSpec {
                    system: Some(system).filter(|s| !s.is_empty()),
                    prompt,
                    max_iterations: max,
                },
            )
            .await
            .map_err(|e| match e {
                FlowError::Node { message, .. } => node_err(ctx, message),
                other => other,
            })?;
            let mut m = Map::new();
            m.insert("output".into(), r.output);
            if param_bool(ctx, "options.returnIntermediateSteps", i, false)? {
                m.insert("intermediateSteps".into(), Value::Array(r.steps));
            }
            let mut o = Item::new(m);
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}

// ------------------------------------------------------------- hokkabaz.agent

#[derive(Debug)]
pub struct HokkabazAgentNode(NodeTypeDescription);

impl Default for HokkabazAgentNode {
    fn default() -> Self {
        Self::new()
    }
}

impl HokkabazAgentNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Hokkabaz Agent",
            "name": "hokkabaz.agent",
            "version": 1,
            "description": "Run a full Hokkabaz agent session (tools, modes, approvals) and return its answer",
            "group": ["transform"],
            "defaults": {"name": "Hokkabaz Agent", "color": "#7c3aed"},
            "inputs": [
                "main",
                {"type": "ai_languageModel", "displayName": "Chat Model", "maxConnections": 1},
                {"type": "ai_memory", "displayName": "Memory", "maxConnections": 1},
                {"type": "ai_tool", "displayName": "Tool"}
            ],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Prompt", "name": "prompt", "type": "string", "default": "={{ $json.chatInput }}"},
                {"displayName": "System Message", "name": "systemMessage", "type": "string", "default": ""},
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "build",
                 "options": [{"name": "Build", "value": "build"}, {"name": "Plan", "value": "plan"}]},
                {"displayName": "Model", "name": "model", "type": "string", "default": "",
                 "description": "provider/model; empty = default model"},
                {"displayName": "Session ID", "name": "sessionId", "type": "string", "default": "",
                 "description": "Continue this agent session; empty = new session"},
                {"displayName": "Tools", "name": "tools", "type": "string", "default": "",
                 "description": "Comma separated tool allow-list; empty = the mode's defaults"},
                {"displayName": "Max Steps", "name": "maxSteps", "type": "number", "default": 25},
                {"displayName": "Return Steps", "name": "returnSteps", "type": "boolean", "default": false}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for HokkabazAgentNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut out = Vec::new();
        for (i, _) in port(ctx, 0).iter().enumerate() {
            let prompt = param_str(ctx, "prompt", i, "")?;
            if prompt.trim().is_empty() {
                return Err(node_err(ctx, "Prompt is empty"));
            }
            let system = param_str(ctx, "systemMessage", i, "")?;
            let steps_wanted = param_bool(ctx, "returnSteps", i, false)?;
            let mut m = Map::new();
            let runner: Option<&Arc<dyn AgentRunner>> = ctx.hooks.agent_runner.as_ref();
            if let Some(runner) = runner {
                let max_steps = param_usize(ctx, "maxSteps", i, 25)?;
                let r = runner
                    .run(AgentRunRequest {
                        prompt,
                        system: Some(system).filter(|s| !s.is_empty()),
                        session_id: Some(param_str(ctx, "sessionId", i, "")?)
                            .filter(|s| !s.is_empty()),
                        mode: Some(param_str(ctx, "mode", i, "build")?),
                        model: Some(param_str(ctx, "model", i, "")?).filter(|s| !s.is_empty()),
                        tools: param_list(ctx, "tools", i)?,
                        max_steps: u32::try_from(max_steps).ok(),
                        workflow_id: ctx.workflow_id.clone(),
                        execution_id: ctx.execution_id.clone(),
                    })
                    .await
                    .map_err(|e| match e {
                        FlowError::Node { message, .. } => node_err(ctx, message),
                        other => other,
                    })?;
                m.insert("output".into(), Value::String(r.output));
                if let Some(s) = r.session_id {
                    m.insert("sessionId".into(), Value::String(s));
                }
                if steps_wanted {
                    m.insert("steps".into(), Value::Array(r.steps));
                }
                m.extend(r.extra);
            } else {
                let inputs = SubInputs::resolve(ctx, i).await?;
                let Some(model) = inputs.model.clone() else {
                    return Err(node_err(
                        ctx,
                        "No agent runtime is configured; connect a Chat Model to run a lightweight agent",
                    ));
                };
                let r = run_loop(
                    &model,
                    &inputs,
                    LoopSpec {
                        system: Some(system).filter(|s| !s.is_empty()),
                        prompt,
                        max_iterations: param_usize(ctx, "maxSteps", i, 25)?,
                    },
                )
                .await
                .map_err(|e| match e {
                    FlowError::Node { message, .. } => node_err(ctx, message),
                    other => other,
                })?;
                m.insert("output".into(), r.output);
                if steps_wanted {
                    m.insert("steps".into(), Value::Array(r.steps));
                }
            }
            let mut o = Item::new(m);
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}
