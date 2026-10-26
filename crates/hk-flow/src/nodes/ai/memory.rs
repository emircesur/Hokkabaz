//! Window Buffer Memory (`memoryBufferWindow`, `ai_memory`): the last N exchanges of one
//! session, held in the executor's [`ChatMemoryStore`].

use std::sync::Arc;

use async_trait::async_trait;
use hk_llm::{Message, Role};
use serde_json::json;

use super::types::{ChatMemoryStore, Memory, MemoryEntry};
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::node::{FlowNode, NodeContext, NodeOutput, Supplied};
use crate::nodes::util::*;

/// Windowed history of one `(workflow, node, session)`.
#[derive(Debug)]
pub struct BufferWindowMemory {
    store: ChatMemoryStore,
    key: String,
    /// Exchanges (user + assistant pairs) kept.
    window: usize,
}

impl BufferWindowMemory {
    pub fn new(store: ChatMemoryStore, key: impl Into<String>, window: usize) -> Self {
        Self {
            store,
            key: key.into(),
            window,
        }
    }
}

#[async_trait]
impl Memory for BufferWindowMemory {
    async fn load(&self) -> Vec<Message> {
        let all = self.store.get(&self.key);
        let keep = self.window.saturating_mul(2);
        let from = all.len().saturating_sub(keep);
        all[from..]
            .iter()
            .map(|e| Message {
                images: Vec::new(),
                role: if e.role == "assistant" {
                    Role::Assistant
                } else {
                    Role::User
                },
                content: e.content.clone(),
                reasoning: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            })
            .collect()
    }

    async fn save(&self, user: &str, assistant: &str) {
        self.store.push(
            &self.key,
            [
                MemoryEntry {
                    role: "user".into(),
                    content: user.to_owned(),
                },
                MemoryEntry {
                    role: "assistant".into(),
                    content: assistant.to_owned(),
                },
            ],
        );
    }

    async fn clear(&self) {
        self.store.clear(&self.key);
    }
}

#[derive(Debug)]
pub struct MemoryBufferWindowNode(NodeTypeDescription);

impl Default for MemoryBufferWindowNode {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBufferWindowNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Window Buffer Memory",
            "name": "@n8n/n8n-nodes-langchain.memoryBufferWindow",
            "version": [1, 1.1, 1.2, 1.3],
            "defaultVersion": 1.3,
            "description": "Keep the last messages of a chat session in memory",
            "group": ["transform"],
            "defaults": {"name": "Window Buffer Memory"},
            "inputs": [],
            "outputs": ["ai_memory"],
            "outputNames": ["Memory"],
            "properties": [
                {"displayName": "Session ID", "name": "sessionIdType", "type": "options", "default": "fromInput",
                 "options": [
                    {"name": "Connected Chat Trigger Node", "value": "fromInput"},
                    {"name": "Define below", "value": "customKey"}]},
                {"displayName": "Key", "name": "sessionKey", "type": "string", "default": "chat_history",
                 "displayOptions": {"show": {"sessionIdType": ["customKey"]}}},
                {"displayName": "Context Window Length", "name": "contextWindowLength", "type": "number", "default": 5}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for MemoryBufferWindowNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Err(node_err(
            ctx,
            "a memory node feeds an AI node over an `ai_memory` connection and does not run on its own",
        ))
    }

    async fn supply_data(&self, ctx: &NodeContext, i: usize) -> Result<Supplied, FlowError> {
        let window = param_usize(ctx, "contextWindowLength", i, 5)?;
        let session = if param_str(ctx, "sessionIdType", i, "fromInput")? == "customKey" {
            param_str(ctx, "sessionKey", i, "chat_history")?
        } else {
            // chat trigger puts `sessionId` on the item; a plain webhook may too
            ctx.input
                .first()
                .and_then(|p| p.get(i))
                .and_then(|it| it.json.get("sessionId"))
                .map(to_display)
                .unwrap_or_else(|| "default".into())
        };
        let key = format!(
            "{}:{}:{session}",
            ctx.workflow_id.as_deref().unwrap_or(""),
            ctx.node.name
        );
        let mem: Arc<dyn Memory> = Arc::new(BufferWindowMemory::new(
            ctx.hooks.chat_memory.clone(),
            key,
            window,
        ));
        Ok(Supplied::new(mem))
    }
}
