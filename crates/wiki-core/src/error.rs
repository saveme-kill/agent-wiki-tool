//! Error types for wiki-core.

use std::path::PathBuf;

/// Convenience result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, WikiError>;

/// Errors that can arise while operating on a wiki workspace.
#[derive(Debug, thiserror::Error)]
pub enum WikiError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("I/O error: {0}")]
    IoPlain(#[from] std::io::Error),

    #[error("not a wiki workspace (missing AGENTS.md and wiki/): {0}")]
    NotAWorkspace(PathBuf),

    #[error("workspace already initialized: {0}")]
    AlreadyInitialized(PathBuf),

    #[error("page not found: {0}")]
    PageNotFound(String),

    #[error("raw source not found: {0}")]
    RawNotFound(String),

    #[error("invalid page path {path}: {reason}")]
    InvalidPagePath { path: String, reason: String },

    #[error("frontmatter error in {path}: {reason}")]
    Frontmatter { path: String, reason: String },

    #[error("heading '{heading}' not found in page {page}")]
    HeadingNotFound { page: String, heading: String },

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("search index error: {0}")]
    Search(String),

    #[error("{0}")]
    Other(String),
}

impl WikiError {
    /// Attach a path to a bare `io::Error` for a friendlier message.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        WikiError::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<tantivy::TantivyError> for WikiError {
    fn from(e: tantivy::TantivyError) -> Self {
        WikiError::Search(e.to_string())
    }
}

impl From<tantivy::directory::error::OpenDirectoryError> for WikiError {
    fn from(e: tantivy::directory::error::OpenDirectoryError) -> Self {
        WikiError::Search(e.to_string())
    }
}

impl From<tantivy::directory::error::OpenReadError> for WikiError {
    fn from(e: tantivy::directory::error::OpenReadError) -> Self {
        WikiError::Search(e.to_string())
    }
}
