use hk_core::Event;
use serde_json::Value;

use crate::tool::{Risk, ToolOutput};

/// Policy / approval / arg-rewrite point. Interceptors may reject or rewrite `args`.
#[derive(Debug, Clone)]
pub struct ToolPreExecute {
    pub call_id: String,
    pub tool: String,
    pub args: Value,
    pub risk: Risk,
}
impl Event for ToolPreExecute {
    const NAME: &'static str = "tools/pre-execute";
}

/// Emitted with validated, final args right before the tool body runs.
#[derive(Debug, Clone)]
pub struct ToolExecute {
    pub call_id: String,
    pub tool: String,
    pub args: Value,
}
impl Event for ToolExecute {
    const NAME: &'static str = "tools/execute";
}

/// Sees the full raw output before truncation/spill: redaction hooks rewrite `output` here so
/// secrets never reach the spill file or the model.
#[derive(Debug, Clone)]
pub struct ToolPostExecute {
    pub call_id: String,
    pub tool: String,
    pub output: ToolOutput,
}
impl Event for ToolPostExecute {
    const NAME: &'static str = "tools/post-execute";
}
