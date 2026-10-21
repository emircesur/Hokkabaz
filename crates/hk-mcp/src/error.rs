#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("invalid mcp server config: {0}")]
    Config(String),
    /// Transport not compiled in or not available on this target (e.g. stdio on android).
    #[error("mcp transport unsupported: {0}")]
    Unsupported(&'static str),
    #[error("mcp connect failed: {0}")]
    Connect(String),
    #[error("mcp request failed: {0}")]
    Request(String),
    #[error("mcp request timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("mcp tool registration failed: {0}")]
    Register(#[from] hk_tools::ToolError),
}
