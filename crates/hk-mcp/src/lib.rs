//! hk-mcp: MCP client over rmcp. Stdio (desktop/daemon only) + streamable HTTP (all targets).
//! MCP tools join the `hk-tools` registry as `mcp.<server>.<tool>`; resources/prompts are listed
//! for the chat `@` picker.
// port: design §3.7 (no dsh/opencode source ported)

mod client;
mod config;
mod convert;
mod error;
mod server;
mod tool;

pub use client::McpClient;
pub use config::{HttpConfig, McpServerConfig, McpTransportConfig, StdioConfig};
pub use convert::{PromptArgument, PromptInfo, PromptMessageInfo, ResourceInfo, ToolInfo};
pub use error::McpError;
pub use server::McpServer;
pub use tool::{McpTool, qualified_name};
