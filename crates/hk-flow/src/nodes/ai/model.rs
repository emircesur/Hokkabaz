//! Chat-model sub-nodes (`ai_languageModel`) and the hk-llm bridge behind them.
//!
//! Every `lmChat*` node reads `model` + `options`, resolves its credential through the
//! [`CredentialResolver`](crate::hooks::CredentialResolver) hook, and asks the
//! [`ChatModelFactory`] hook for a model. Without a hook the default factory builds the
//! matching hk-llm provider directly from the credential.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use hk_llm::registry::{KeyRef, ProviderConfig, ProviderOptions, ProviderRegistry};
use hk_llm::{
    CancellationToken, ChatRequest, FinishReason, Message, Provider, StreamEvent, ToolCall,
    ToolChoice, ToolSpec, Usage,
};
use serde_json::{Value, json};

use super::types::{ChatModel, ChatModelFactory, ChatModelSpec, Completion, ai_err};
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::node::{FlowNode, NodeContext, NodeOutput, Supplied};
use crate::nodes::util::*;

// ------------------------------------------------------------- provider bridge

/// [`ChatModel`] over an hk-llm [`Provider`]: one streamed request, aggregated.
#[derive(Clone)]
pub struct ProviderChatModel {
    provider: Arc<dyn Provider>,
    model: String,
    temperature: Option<f32>,
    max_tokens: Option<u64>,
}

impl std::fmt::Debug for ProviderChatModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderChatModel")
            .field("provider", &self.provider.id())
            .field("model", &self.model)
            .finish()
    }
}

impl ProviderChatModel {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            temperature: None,
            max_tokens: None,
        }
    }

    #[must_use]
    pub fn temperature(mut self, t: Option<f32>) -> Self {
        self.temperature = t;
        self
    }

    #[must_use]
    pub fn max_tokens(mut self, n: Option<u64>) -> Self {
        self.max_tokens = n;
        self
    }
}

#[async_trait]
impl ChatModel for ProviderChatModel {
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
    ) -> Result<Completion, FlowError> {
        let mut req = ChatRequest::new(self.model.clone(), messages);
        req.tool_choice = ToolChoice::Auto;
        req.tools = tools;
        req.temperature = self.temperature;
        req.max_output_tokens = self.max_tokens;
        let stream = self
            .provider
            .stream(req, CancellationToken::new())
            .await
            .map_err(|e| ai_err(e.to_string()))?;
        collect(stream).await
    }
}

/// Fold a provider event stream into one [`Completion`].
pub(crate) async fn collect(mut stream: hk_llm::EventStream) -> Result<Completion, FlowError> {
    let mut text = String::new();
    let mut calls: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    let mut usage = Usage::default();
    let mut finish = FinishReason::Stop;
    while let Some(ev) = stream.next().await {
        match ev.map_err(|e| ai_err(e.to_string()))? {
            StreamEvent::TextDelta { text: t } => text.push_str(&t),
            StreamEvent::ReasoningDelta { .. } => {}
            StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let c = calls.entry(index).or_default();
                if let Some(id) = id {
                    c.0 = id;
                }
                if let Some(n) = name {
                    c.1 = n;
                }
                c.2.push_str(&arguments_delta);
            }
            StreamEvent::Usage(u) => usage = u,
            StreamEvent::Finish(f) => finish = f,
        }
    }
    let tool_calls = calls
        .into_iter()
        .map(|(i, (id, name, args))| ToolCall {
            id: if id.is_empty() { format!("call_{i}") } else { id },
            name,
            arguments: if args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&args).unwrap_or(Value::String(args))
            },
        })
        .collect();
    Ok(Completion {
        text,
        tool_calls,
        finish,
        usage,
    })
}

// ---------------------------------------------------------------- default factory

/// Builds hk-llm providers straight from the credential fields.
#[derive(Debug, Default, Clone, Copy)]
pub struct HkLlmChatModelFactory;

const OPENAI_URL: &str = "https://api.openai.com/v1";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";

#[async_trait]
impl ChatModelFactory for HkLlmChatModelFactory {
    async fn build(&self, spec: ChatModelSpec) -> Result<Arc<dyn ChatModel>, FlowError> {
        let (id, kind, default_url) = match spec.provider.as_str() {
            "openai" => ("openai", "openai_compat", Some(OPENAI_URL)),
            "openrouter" => ("openrouter", "openai_compat", Some(OPENROUTER_URL)),
            "deepseek" => ("deepseek", "deepseek", None),
            "anthropic" => ("anthropic", "anthropic", None),
            "gemini" => ("gemini", "gemini", None),
            "ollama" => ("ollama", "ollama", None),
            other => return Err(ai_err(format!("unknown chat model provider `{other}`"))),
        };
        let cfg = ProviderConfig {
            id: id.into(),
            kind: kind.into(),
            base_url: spec
                .base_url
                .clone()
                .filter(|u| !u.is_empty())
                .or_else(|| default_url.map(str::to_owned)),
            api_key: spec.api_key.as_ref().map(|_| KeyRef::new("credential")),
            options: ProviderOptions::default(),
        };
        let key = spec.api_key.clone();
        let reg = ProviderRegistry::build(&[cfg], &move |_: &KeyRef| key.clone())
            .map_err(|e| ai_err(e.to_string()))?;
        let provider = reg
            .get(id)
            .cloned()
            .ok_or_else(|| ai_err("provider was not built"))?;
        Ok(Arc::new(
            ProviderChatModel::new(provider, spec.model)
                .temperature(spec.temperature)
                .max_tokens(spec.max_tokens),
        ))
    }
}

// -------------------------------------------------------------------- the nodes

/// `{__rl, value}` resource locator or bare string.
pub(crate) fn rl_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(o) => o.get("value").and_then(rl_value),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

struct Kind {
    node: &'static str,
    display: &'static str,
    provider: &'static str,
    credential: &'static str,
    default_model: &'static str,
    versions: &'static [f64],
}

const KINDS: &[Kind] = &[
    Kind {
        node: "lmChatOpenAi",
        display: "OpenAI Chat Model",
        provider: "openai",
        credential: "openAiApi",
        default_model: "gpt-4o-mini",
        versions: &[1.0, 1.1, 1.2, 1.3],
    },
    Kind {
        node: "lmChatAnthropic",
        display: "Anthropic Chat Model",
        provider: "anthropic",
        credential: "anthropicApi",
        default_model: "claude-sonnet-4-5",
        versions: &[1.0, 1.1, 1.2, 1.3],
    },
    Kind {
        node: "lmChatGoogleGemini",
        display: "Google Gemini Chat Model",
        provider: "gemini",
        credential: "googlePalmApi",
        default_model: "gemini-2.5-flash",
        versions: &[1.0],
    },
    Kind {
        node: "lmChatOllama",
        display: "Ollama Chat Model",
        provider: "ollama",
        credential: "ollamaApi",
        default_model: "llama3.2",
        versions: &[1.0, 1.1, 1.2],
    },
    Kind {
        node: "lmChatDeepSeek",
        display: "DeepSeek Chat Model",
        provider: "deepseek",
        credential: "deepSeekApi",
        default_model: "deepseek-chat",
        versions: &[1.0],
    },
    Kind {
        node: "lmChatOpenRouter",
        display: "OpenRouter Chat Model",
        provider: "openrouter",
        credential: "openRouterApi",
        default_model: "openai/gpt-4o-mini",
        versions: &[1.0],
    },
];

/// One `lmChat*` node.
#[derive(Debug)]
pub struct ChatModelNode {
    desc: NodeTypeDescription,
    provider: &'static str,
    credential: &'static str,
    default_model: &'static str,
}

impl ChatModelNode {
    fn new(k: &Kind) -> Self {
        let latest = k.versions.last().copied().unwrap_or(1.0);
        Self {
            desc: describe(json!({
                "displayName": k.display,
                "name": format!("@n8n/n8n-nodes-langchain.{}", k.node),
                "version": k.versions,
                "defaultVersion": latest,
                "description": "Language model for AI chains and agents",
                "group": ["transform"],
                "defaults": {"name": k.display},
                "inputs": [],
                "outputs": ["ai_languageModel"],
                "outputNames": ["Model"],
                "credentials": [{"name": k.credential, "required": true}],
                "properties": [
                    {"displayName": "Model", "name": "model", "type": "string", "default": k.default_model},
                    {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                     "placeholder": "Add option",
                     "options": [
                        {"displayName": "Sampling Temperature", "name": "temperature", "type": "number", "default": 0.7},
                        {"displayName": "Maximum Number of Tokens", "name": "maxTokens", "type": "number", "default": -1},
                        {"displayName": "Base URL", "name": "baseURL", "type": "string", "default": ""}]}
                ]
            })),
            provider: k.provider,
            credential: k.credential,
            default_model: k.default_model,
        }
    }

    async fn spec(&self, ctx: &NodeContext, i: usize) -> Result<ChatModelSpec, FlowError> {
        let model = param(ctx, "model", i)?
            .as_ref()
            .and_then(rl_value)
            .unwrap_or_else(|| self.default_model.to_owned());
        let temperature = param_f64(ctx, "options.temperature", i, f64::NAN)?;
        // n8n uses -1 for "unset"
        let max_tokens = [
            "options.maxTokens",
            "options.maxTokensToSample",
            "options.maxOutputTokens",
        ]
        .iter()
        .find_map(|p| param_f64(ctx, p, i, -1.0).ok().filter(|n| *n > 0.0));
        let mut spec = ChatModelSpec {
            provider: self.provider.to_owned(),
            model,
            temperature: temperature.is_finite().then_some(temperature as f32),
            max_tokens: max_tokens.map(|n| n as u64),
            base_url: param(ctx, "options.baseURL", i)?
                .as_ref()
                .and_then(rl_value),
            api_key: None,
        };
        // Secrets are used to build the provider and dropped; none reach items or errors.
        if let (Some(r), Some(resolver)) = (
            ctx.node
                .credentials
                .as_ref()
                .and_then(|c| c.get(self.credential)),
            ctx.hooks.credentials.as_ref(),
        ) {
            let secret = resolver.resolve(self.credential, r).await?;
            let get = |k: &str| {
                secret
                    .get(k)
                    .filter(|v| !v.is_null())
                    .map(to_display)
                    .filter(|s| !s.is_empty())
            };
            spec.api_key = get("apiKey");
            if spec.base_url.is_none() && self.provider != "gemini" {
                spec.base_url = get("url").or_else(|| get("baseUrl"));
            }
        }
        Ok(spec)
    }
}

#[async_trait]
impl FlowNode for ChatModelNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Err(node_err(
            ctx,
            "a chat model node feeds an AI node over an `ai_languageModel` connection and does not run on its own",
        ))
    }

    async fn supply_data(&self, ctx: &NodeContext, i: usize) -> Result<Supplied, FlowError> {
        let spec = self.spec(ctx, i).await?;
        let model = match &ctx.hooks.chat_models {
            Some(f) => f.build(spec).await,
            None => HkLlmChatModelFactory.build(spec).await,
        }
        .map_err(|e| match e {
            FlowError::Node { message, .. } => node_err(ctx, message),
            other => other,
        })?;
        Ok(Supplied::new(model))
    }
}

pub(super) fn chat_model_nodes() -> Vec<Arc<dyn FlowNode>> {
    KINDS
        .iter()
        .map(|k| Arc::new(ChatModelNode::new(k)) as Arc<dyn FlowNode>)
        .collect()
}
