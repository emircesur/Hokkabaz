//! LangChain-style AI nodes (`@n8n/n8n-nodes-langchain.*`) on hk-llm. Feature `ai`.
//!
//! Clean-room: behavior from the public n8n docs and exported workflow JSON.
//!
//! n8n wires AI nodes with typed sub-node connections stored source -> target:
//! `ai_languageModel`, `ai_memory`, `ai_tool`, `ai_outputParser`. Sub-nodes produce no
//! items; a root node pulls typed handles ([`ChatModel`], [`Memory`], [`AgentTool`],
//! [`OutputParser`]) through [`NodeContext::sub_nodes`](crate::node::NodeContext::sub_nodes),
//! which calls each sub-node's [`FlowNode::supply_data`].
//!
//! | node | type | versions |
//! |---|---|---|
//! | AI Agent (tools agent) | `agent` | 1 - 3 |
//! | Basic LLM Chain | `chainLlm` | 1 - 1.9 |
//! | Text Classifier | `textClassifier` | 1, 1.1 |
//! | Information Extractor | `informationExtractor` | 1 - 1.2 |
//! | OpenAI / Anthropic / Gemini / Ollama / DeepSeek / OpenRouter chat models | `lmChat*` | 1 - 1.3 |
//! | Window Buffer Memory | `memoryBufferWindow` | 1 - 1.3 (in-process) |
//! | Code / Call Workflow / HTTP Request tools | `toolCode` `toolWorkflow` `toolHttpRequest` | 1 - 1.3 |
//! | Structured / Item List output parsers | `outputParserStructured` `outputParserItemList` | 1 - 1.3 / 1 |
//! | Hokkabaz Agent | `hokkabaz.agent` | 1 (full hk-agent session via [`AgentRunner`]) |
//!
//! Not covered: streaming, parser auto-fix, vector stores/embeddings/retrievers, Python code
//! tool, chat-memory backends other than the in-process window.

use std::sync::Arc;

use crate::node::FlowNode;

pub mod agent;
pub mod chains;
pub mod fake;
pub(crate) mod from_ai;
pub mod memory;
pub mod model;
pub mod parsers;
pub(crate) mod schema;
pub mod tools;
pub mod types;

pub use agent::{AgentNode, HokkabazAgentNode};
pub use chains::{ChainLlmNode, InformationExtractorNode, TextClassifierNode};
pub use fake::{ScriptedChatModelFactory, SeenRequest};
pub use memory::{BufferWindowMemory, MemoryBufferWindowNode};
pub use model::{ChatModelNode, HkLlmChatModelFactory, ProviderChatModel};
pub use parsers::{
    ItemListOutputParserNode, ItemListParser, StructuredOutputParserNode, StructuredParser,
};
#[cfg(feature = "http")]
pub use tools::ToolHttpRequestNode;
pub use tools::{ToolCodeNode, ToolWorkflowNode};
pub use types::{
    AgentRunRequest, AgentRunResult, AgentRunner, AgentTool, ChatMemoryStore, ChatModel,
    ChatModelFactory, ChatModelSpec, Completion, Memory, MemoryEntry, OutputParser,
};

/// Every AI node implementation.
pub fn ai_nodes() -> Vec<Arc<dyn FlowNode>> {
    #[cfg_attr(not(feature = "http"), allow(unused_mut))]
    let mut v: Vec<Arc<dyn FlowNode>> = vec![
        Arc::new(AgentNode::new()),
        Arc::new(HokkabazAgentNode::new()),
        Arc::new(ChainLlmNode::new()),
        Arc::new(TextClassifierNode::new()),
        Arc::new(InformationExtractorNode::new()),
        Arc::new(MemoryBufferWindowNode::new()),
        Arc::new(ToolCodeNode::new()),
        Arc::new(ToolWorkflowNode::new()),
        Arc::new(StructuredOutputParserNode::new()),
        Arc::new(ItemListOutputParserNode::new()),
    ];
    #[cfg(feature = "http")]
    v.push(Arc::new(ToolHttpRequestNode::new()));
    v.extend(model::chat_model_nodes());
    v
}
