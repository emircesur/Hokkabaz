#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("plugin runtime: {0}")]
    Runtime(String),
    /// The plugin ran and reported an error.
    #[error("plugin error: {0}")]
    Plugin(String),
    #[error("capability denied: {0}")]
    Denied(String),
    #[error("template: {0}")]
    Template(String),
    #[error("registry: {0}")]
    Registry(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, PluginError>;
