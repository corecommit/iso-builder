//! Error type shared by the library modules.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IsoBuilderError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot parse configuration: {0}")]
    Config(#[from] toml::de::Error),
    #[error("ISO extraction failed: {0}")]
    Extraction(String),
    #[error("script generation failed: {0}")]
    Script(String),
    #[error("required tool not found: {0}")]
    MissingTool(String),
}
