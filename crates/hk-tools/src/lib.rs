//! hk-tools: tool registry + guarded execution pipeline.
// port: dsh packages/core/tools (pipeline shape: pre-execute -> execute -> post-execute)

pub mod builtin;
mod events;
mod output;
mod pipeline;
mod registry;
mod tool;

pub use events::{ToolExecute, ToolPostExecute, ToolPreExecute};
pub use output::{OutputLimits, Spilled};
pub use pipeline::ToolPipeline;
pub use registry::ToolRegistry;
pub use tool::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};
